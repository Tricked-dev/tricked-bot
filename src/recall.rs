use std::{collections::HashMap, time::Duration};

use color_eyre::{eyre::eyre, Result};

use crate::{ai_reply::AiRequest, database::Memory, db, questions};

pub struct Recalled {
    pub memories: Vec<Memory>,
    pub durable: bool,
}

pub fn pick(memories: &[Memory], scores: &HashMap<String, f32>, threshold: f32) -> Vec<Memory> {
    memories
        .iter()
        .filter(|memory| {
            scores
                .get(&questions::memory_id(memory))
                .is_some_and(|p| p.is_finite() && *p >= threshold)
        })
        .cloned()
        .collect()
}

/// Wait for relevant memories and, for direct triggers, durability. Never use a recency fallback.
pub async fn select(req: &AiRequest) -> Result<Recalled> {
    let memories = db::get_all_memories(&req.db, req.user_id).await?;
    let mut qs: HashMap<_, _> = memories
        .iter()
        .map(|m| (questions::memory_id(m), questions::recall(&req.user_name, m)))
        .collect();
    if req.ask_durable {
        qs.insert("durable".into(), questions::durable(&req.user_name));
    }
    if qs.is_empty() {
        return Ok(Recalled {
            memories,
            durable: false,
        });
    }
    let decider = req.decider.as_ref().ok_or_else(|| eyre!("decider not configured"))?;
    let started = std::time::Instant::now();
    let result = decider
        .noul(
            &questions::state(&req.context, &req.user_name, &req.message),
            qs,
            false,
            Duration::from_millis(req.config.decider_recall_timeout_ms),
        )
        .await;
    let scores = match result {
        Ok(scores) => scores,
        Err(error) => {
            tracing::warn!(target: "decider", kind = "recall", msg_id = req.message_id,
                latency_ms = started.elapsed().as_millis() as u64, "Recall failed: {error}");
            return Err(error);
        }
    };
    let chosen = pick(&memories, &scores, req.config.decider_recall_threshold);
    let durable = scores.get("durable").copied();
    tracing::info!(target: "decider", kind = "recall", user = req.user_id, msg_id = req.message_id,
        candidates = memories.len(), selected = chosen.len(), durable = durable.unwrap_or(-1.0),
        latency_ms = started.elapsed().as_millis() as u64);
    for memory in &memories {
        tracing::debug!(target: "decider", kind = "recall_item", msg_id = req.message_id,
            memory = memory.id, p = scores.get(&questions::memory_id(memory)).copied().unwrap_or(-1.0));
    }
    Ok(Recalled {
        memories: chosen,
        durable: durable.is_some_and(|p| p >= req.config.decider_durable_threshold),
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
    fn threshold_is_inclusive_and_preserves_candidate_order() {
        let ms = vec![mem(3), mem(2), mem(1)];
        let scores = HashMap::from([("m3".into(), 0.2), ("m2".into(), 0.8), ("m1".into(), 0.5)]);
        assert_eq!(
            pick(&ms, &scores, 0.5).iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![2, 1]
        );
    }
    #[test]
    fn missing_and_invalid_scores_are_not_selected() {
        assert!(pick(&[mem(9)], &HashMap::new(), 0.1).is_empty());
        assert!(pick(&[mem(9)], &HashMap::from([("m9".into(), f32::NAN)]), 0.1).is_empty());
    }
}
