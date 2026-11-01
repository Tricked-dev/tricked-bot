//! Picks the stored memories that would help answer a message.

use std::{cmp::Reverse, collections::HashMap, time::Duration};

use color_eyre::{Result, eyre::eyre};

use super::{AiRequest, embeddings, timing};
use crate::{
    bot::Bot,
    db::{self, Memory},
    decider::questions,
};

/// Most memories kept per user. With the no-memory option this fills the decider's limit of 255 choice options.
pub const LIMIT: usize = 254;
/// Above this many memories, the decider sees only those closest to the message by embedding.
const SHORTLIST: usize = 24;
/// Characters of content the decider sees per memory.
const SNIPPET: usize = 160;
/// Per-attempt timeout for embedding the message, so recall never waits long on it.
const EMBED_TIMEOUT: Duration = Duration::from_secs(5);

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

/// The start of `content`, cut on a char boundary.
fn snippet(content: &str) -> String {
    match content.char_indices().nth(SNIPPET) {
        Some((end, _)) => format!("{}…", &content[..end]),
        None => content.to_owned(),
    }
}

/// Every memory key with a content snippet as a choice option, plus a no-memory option under a name no key uses.
fn choice_options(memories: &[Memory]) -> (HashMap<String, String>, String) {
    let mut options: HashMap<_, _> = memories.iter().map(|m| (m.key.clone(), snippet(&m.content))).collect();
    let mut none = "NONE".to_owned();
    while options.contains_key(&none) {
        none.push('_');
    }
    options.insert(none.clone(), "No stored memory is needed".into());
    (options, none)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut a_norm, mut b_norm) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        a_norm += x * x;
        b_norm += y * y;
    }
    let norm = (a_norm * b_norm).sqrt();
    if norm > 0.0 { dot / norm } else { 0.0 }
}

/// Cosine similarity of each stored embedding to the query. Vectors of another size can't be compared and are left out.
fn similarities(query: &[f32], stored: Vec<(i64, Vec<f32>)>) -> HashMap<i64, f32> {
    stored
        .into_iter()
        .filter(|(_, embedding)| embedding.len() == query.len())
        .map(|(id, embedding)| (id, cosine(query, &embedding)))
        .collect()
}

/// The `SHORTLIST` memories most similar to the message, then up to as many not embedded yet so fresh memories stay
/// visible. `memories` must be sorted newest first, which also breaks similarity ties.
fn shortlist(memories: Vec<Memory>, similarity: &HashMap<i64, f32>) -> Vec<Memory> {
    let (mut embedded, unembedded): (Vec<_>, Vec<_>) =
        memories.into_iter().partition(|m| similarity.contains_key(&m.id));
    embedded.sort_by(|a, b| similarity[&b.id].total_cmp(&similarity[&a.id]));
    embedded.truncate(SHORTLIST);
    embedded.extend(unembedded.into_iter().take(SHORTLIST));
    embedded
}

/// Similarity of each of the author's embedded memories to the message and its recent context.
async fn similarity(bot: &Bot, req: &AiRequest) -> Result<HashMap<i64, f32>> {
    let ai = embeddings::client(bot).ok_or_else(|| eyre!("embeddings not configured"))?;
    let model = &bot.config.openrouter_embedding_model;
    let query = format!("{}\n{}: {}", questions::recent(req).join("\n"), req.user_name, req.message);
    let input = [query.trim_start().to_owned()];
    let (query, stored) = tokio::try_join!(
        ai.embed(model, &input, embeddings::DIMENSIONS, EMBED_TIMEOUT),
        db::memory_embeddings(&bot.db, req.user_id.get(), model),
    )?;
    Ok(similarities(&query[0], stored))
}

/// Shortlists memories by embedding similarity, then has the decider rank their names and content snippets. Full
/// content is used only in the final reply prompt.
pub async fn select(bot: &Bot, req: &AiRequest) -> Result<Vec<Memory>> {
    let mut memories = timing::measure(req.message_id, "memory_db", db::memories(&bot.db, req.user_id.get())).await?;
    if memories.is_empty() {
        return Ok(memories);
    }
    let candidates = memories.len();
    let started = std::time::Instant::now();
    memories.sort_by_key(|m| Reverse(m.id));
    if memories.len() > SHORTLIST {
        memories = match timing::measure(req.message_id, "memory_shortlist", similarity(bot, req)).await {
            Ok(similarity) => shortlist(memories, &similarity),
            Err(error) => {
                tracing::warn!(target: "decider", kind = "recall", msg_id = %req.message_id,
                    "Ranking memories newest first: {error}");
                // The background worker trims overflow. Keep requests valid while it is running.
                memories.truncate(LIMIT);
                memories
            }
        };
    }
    let ranked = memories.len();
    let decider =
        bot.recall_decider.as_ref().or(bot.decider.as_ref()).ok_or_else(|| eyre!("decider not configured"))?;
    let (options, none) = choice_options(&memories);
    let probabilities = decider
        .choice(
            &questions::state(req),
            "Which stored memory would help answer the last message? Each option is a memory name described by the \
             start of its content. Choose the no-memory option if no memory would help.",
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
        assert!(all.iter().all(|m| options[&m.key] == "fact"));
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

    #[test]
    fn snippets_cut_long_content_on_a_char_boundary() {
        assert_eq!(snippet("short"), "short");
        assert_eq!(snippet(&"é".repeat(SNIPPET)), "é".repeat(SNIPPET));
        assert_eq!(snippet(&"🙂".repeat(SNIPPET + 1)), format!("{}…", "🙂".repeat(SNIPPET)));
    }

    #[test]
    fn cosine_compares_direction_not_length() {
        assert!((cosine(&[1.0, 2.0], &[2.0, 4.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 3.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        assert!(cosine(&[0.0, 0.0], &[1.0, 0.0]).abs() < 1e-6);
    }

    #[test]
    fn similarities_skip_embeddings_of_another_size() {
        let stored = vec![(1, vec![1.0, 0.0]), (2, vec![0.6, 0.8]), (3, vec![1.0, 0.0, 0.0])];
        let similarity = similarities(&[1.0, 0.0], stored);
        assert_eq!(similarity.len(), 2);
        assert!(similarity[&1] > similarity[&2]);
    }

    #[test]
    #[expect(clippy::cast_precision_loss, reason = "small test ids")]
    fn shortlist_keeps_the_most_similar_and_the_newest_unembedded() {
        // Newest first, as `select` sorts them. Even ids are embedded, and higher ids are more similar.
        let memories: Vec<_> = (1..=100).rev().map(mem).collect();
        let similarity: HashMap<_, _> = (1..=100).filter(|id| id % 2 == 0).map(|id| (id, id as f32)).collect();
        let ids: Vec<_> = shortlist(memories, &similarity).iter().map(|m| m.id).collect();
        let expected: Vec<_> = (54..=100).rev().step_by(2).chain((53..=99).rev().step_by(2)).collect();
        assert_eq!(ids, expected);
        assert_eq!(ids.len(), 2 * SHORTLIST);
    }

    #[test]
    fn shortlist_offers_unembedded_memories_before_the_worker_catches_up() {
        let memories: Vec<_> = (1..=30).rev().map(mem).collect();
        let ids: Vec<_> = shortlist(memories, &HashMap::new()).iter().map(|m| m.id).collect();
        assert_eq!(ids, (7..=30).rev().collect::<Vec<_>>());
    }
}
