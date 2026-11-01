//! Picks the stored memories that would help answer a message.

use std::{cmp::Reverse, collections::HashMap, time::Duration};

use color_eyre::{Result, eyre::eyre};

use super::AiRequest;
use crate::{
    bot::Bot,
    db::{self, Memory},
    decider::questions,
};

/// Most memories kept per user. With the no-memory option this fills the decider's limit of 255 choice options.
pub const LIMIT: usize = 254;

/// Picks up to three memories from the decider's distribution over memory keys and the `none` option.
///
/// Choice probabilities compete with each other, so a memory has to beat the no-memory option, a uniform split over
/// all options, and a quarter of the strongest memory.
#[expect(clippy::cast_precision_loss, reason = "the decider accepts at most 255 options")]
pub fn pick(memories: Vec<Memory>, probabilities: &HashMap<String, f32>, none: &str) -> Vec<Memory> {
    let none = probabilities.get(none).copied().unwrap_or(0.0);
    let uniform = 1.0 / probabilities.len().max(1) as f32;
    let mut ranked: Vec<_> = memories
        .into_iter()
        .filter_map(|memory| {
            let p = probabilities.get(&memory.key).copied()?;
            (p > none && p >= uniform).then_some((memory, p))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
    let floor = ranked.first().map_or(0.0, |(_, top)| top * 0.25);
    ranked.into_iter().take_while(|(_, p)| *p >= floor).take(3).map(|(memory, _)| memory).collect()
}

/// Every memory key as a choice option, plus a no-memory option under a name no key uses.
fn choice_options(memories: &[Memory]) -> (HashMap<String, String>, String) {
    let mut options: HashMap<_, _> = memories.iter().map(|m| (m.key.clone(), String::new())).collect();
    let mut none = "NONE".to_owned();
    while options.contains_key(&none) {
        none.push('_');
    }
    options.insert(none.clone(), "No stored memory is needed".into());
    (options, none)
}

/// One ranking over memory names. Full content is used only in the final reply prompt.
pub async fn select(bot: &Bot, req: &AiRequest) -> Result<Vec<Memory>> {
    let mut memories = db::memories(&bot.db, req.user_id.get()).await?;
    if memories.is_empty() {
        return Ok(memories);
    }
    let candidates = memories.len();
    // The background worker trims overflow. Keep requests valid while it is running.
    memories.sort_by_key(|m| Reverse(m.id));
    memories.truncate(LIMIT);
    let ranked = memories.len();
    let decider =
        bot.recall_decider.as_ref().or(bot.decider.as_ref()).ok_or_else(|| eyre!("decider not configured"))?;
    let (options, none) = choice_options(&memories);
    let started = std::time::Instant::now();
    let probabilities = decider
        .choice(
            &questions::state(req),
            "Which stored memory name would help answer the last message? Choose the no-memory option if no memory \
             would help.",
            options,
            Duration::from_millis(bot.config.decider_recall_timeout_ms),
        )
        .await?;
    let chosen = pick(memories, &probabilities, &none);
    tracing::info!(target: "decider", kind = "recall", user = %req.user_id, msg_id = %req.message_id,
        candidates, ranked, selected = chosen.len(), latency_ms = started.elapsed().as_millis());
    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: i64) -> Memory {
        Memory { id, user_id: 1, key: format!("key{id}"), content: "fact".into() }
    }

    fn weights<const N: usize>(weights: [(&str, f32); N]) -> HashMap<String, f32> {
        weights.into_iter().map(|(key, p)| (key.to_owned(), p)).collect()
    }

    #[test]
    fn options_always_include_an_unused_no_memory_name() {
        let all: Vec<_> = (0..254).map(mem).collect();
        let (options, none) = choice_options(&all);
        assert_eq!(options.len(), 255);
        assert_eq!(none, "NONE");
        assert!(all.iter().all(|m| options[&m.key].is_empty()));
        let collision = Memory { key: "NONE".into(), ..mem(0) };
        let (options, none) = choice_options(&[collision]);
        assert_eq!(none, "NONE_");
        assert_eq!(options.len(), 2);
    }

    #[test]
    fn keeps_strong_memories_above_the_uniform_split() {
        let memories = vec![mem(1), mem(2), mem(3), mem(4)];
        let p = weights([("key1", 0.05), ("key2", 0.45), ("key3", 0.3), ("key4", 0.15), ("NONE", 0.05)]);
        assert_eq!(pick(memories, &p, "NONE").iter().map(|m| m.id).collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn a_single_memory_competes_with_the_no_memory_option() {
        assert_eq!(pick(vec![mem(1)], &weights([("key1", 0.99), ("NONE", 0.01)]), "NONE"), [mem(1)]);
        assert_eq!(pick(vec![mem(1)], &weights([("key1", 0.4), ("NONE", 0.6)]), "NONE"), []);
        assert_eq!(pick(vec![mem(1)], &weights([("NONE", 1.0)]), "NONE"), []);
    }
}
