use crate::{ai_reply::AiRequest, database::Memory, db, questions};
use color_eyre::{eyre::eyre, Result};
use std::{collections::HashMap, time::Duration};

pub struct Recalled {
    pub memories: Vec<Memory>,
    pub durable: bool,
}
pub const LIMIT: usize = 255;

/// Choice probabilities compete with each other: retain at most three strong options.
/// The old independent-noul threshold does not apply to this distribution.
pub fn pick(memories: &[Memory], scores: &HashMap<String, f32>) -> Vec<Memory> {
    let mut ranked: Vec<_> = memories
        .iter()
        .filter_map(|m| {
            scores
                .get(&questions::memory_id(m))
                .copied()
                .filter(|p| p.is_finite() && *p > 0.0)
                .map(|p| (m, p))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
    let top = ranked.first().map(|(_, p)| *p).unwrap_or(0.0);
    let floor = (top * 0.25).max(1.0 / scores.len().max(1) as f32);
    ranked
        .into_iter()
        .filter(|(_, p)| *p >= floor)
        .take(3)
        .map(|(m, _)| m.clone())
        .collect()
}

fn choice_options(memories: &[Memory]) -> (HashMap<String,String>,Option<String>) {
    let mut options:HashMap<_,_>=memories.iter().map(|m|(m.key.clone(),String::new())).collect();
    let none=if options.len()<LIMIT {
        let mut name="NONE".to_string();
        while options.contains_key(&name) { name.push('_'); }
        options.insert(name.clone(),"No stored memory is needed".into());
        Some(name)
    } else { None };
    (options,none)
}

/// One ranking over memory names. Full content is used only in the final reply prompt.
pub async fn select(req: &AiRequest) -> Result<Recalled> {
    let mut memories = db::get_all_memories(&req.db, req.user_id).await?;
    let total = memories.len();
    // The background worker trims overflow. Keep requests valid while it is running.
    memories.sort_by_key(|m| std::cmp::Reverse(m.id));
    memories.truncate(LIMIT);
    let state = questions::state(&req.context,&req.user_name,&req.message);
    let timeout=Duration::from_millis(req.config.decider_recall_timeout_ms);
    let started=std::time::Instant::now();
    if memories.is_empty() && !req.ask_durable { return Ok(Recalled { memories,durable:false }); }
    let decider=req.decider.as_ref().ok_or_else(||eyre!("decider not configured"))?;
    let ranker=req.recall_decider.as_ref().unwrap_or(decider);
    let durability=async {
        if !req.ask_durable { return Ok::<_,color_eyre::Report>(false); }
        let flags=decider.noul(&state,HashMap::from([("durable".into(),questions::durable(&req.user_name))]),false,timeout).await?;
        Ok(flags["durable"]>=req.config.decider_durable_threshold)
    };
    let ranking=async {
        if memories.is_empty() { return Ok::<_,color_eyre::Report>(Vec::new()); }
        let (options,none)=choice_options(&memories);
        let mut flags=HashMap::new();
        if none.is_none() {
            flags.insert("relevant".into(),crate::decider::Question::noul(
                "Would stored personal history help answer this message? Exclude greetings, thanks and general questions with enough context already provided.",
                "personal history helps","no personal history needed"));
        }
        let result=ranker.choice(&state,
            "Which stored memory name would help answer the last message? Choose the no-memory option if no memory would help.",
            options,flags,timeout).await?;
        let none_weight=none.as_ref().and_then(|name|result.probabilities.get(name)).copied().unwrap_or(0.0);
        let relevant=result.flags.get("relevant").copied().unwrap_or(1.0)>=0.15;
        let scores=memories.iter().map(|m| {
            let p=result.probabilities[&m.key];
            (questions::memory_id(m),if relevant && p>none_weight {p} else {0.0})
        }).collect();
        Ok(pick(&memories,&scores))
    };
    let (chosen,durable)=tokio::try_join!(ranking,durability)?;
    tracing::info!(target:"decider",kind="recall",user=req.user_id,msg_id=req.message_id,
        candidates=total,ranked=memories.len(),selected=chosen.len(),durable,
        latency_ms=started.elapsed().as_millis() as u64);
    Ok(Recalled {
        memories: chosen,
        durable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mem(id: i64) -> Memory {
        Memory {
            id,
            user_id: 1,
            key: format!("key{id}"),
            content: "fact".into(),
        }
    }
    #[test]
    fn name_options_preserve_255_cap_and_allow_abstention_below_it() {
        let all:Vec<_>=(0..255).map(mem).collect();
        let (options,none)=choice_options(&all);
        assert_eq!(options.len(),255);assert!(none.is_none());
        assert!(options.values().all(String::is_empty));
        let (options,none)=choice_options(&all[..254]);
        assert_eq!(options.len(),255);assert!(none.is_some());
        let mut collision=mem(0);collision.key="NONE".into();
        let (options,none)=choice_options(&[collision]);
        assert_eq!(none.as_deref(),Some("NONE_"));assert_eq!(options.len(),2);
    }
    #[test]
    fn choice_weights_rank_multiple_memories_without_old_half_threshold() {
        let ms = vec![mem(1), mem(2), mem(3), mem(4)];
        let scores = HashMap::from([
            ("m1".into(), 0.05),
            ("m2".into(), 0.45),
            ("m3".into(), 0.3),
            ("m4".into(), 0.2),
        ]);
        assert_eq!(pick(&ms, &scores).iter().map(|m| m.id).collect::<Vec<_>>(), vec![2, 3]);
    }
    #[test]
    fn absent_invalid_and_none_options_do_not_become_memories() {
        assert!(pick(&[mem(1)], &HashMap::from([("none".into(), 1.0)])).is_empty());
        assert!(pick(&[mem(1)], &HashMap::from([("m1".into(), f32::NAN)])).is_empty());
    }
}
