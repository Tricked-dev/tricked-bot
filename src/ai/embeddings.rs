//! Embeds memory text in the background so recall can shortlist memories by content.

use std::{sync::Arc, time::Duration};

use color_eyre::Result;
use deadpool_postgres::Pool;

use super::openrouter::OpenRouter;
use crate::{
    bot::Bot,
    db::{self, Memory},
};

/// Components per embedding. Fixed so stored vectors stay comparable with query vectors.
pub const DIMENSIONS: u32 = 512;
/// Memories embedded per request.
const BATCH: usize = 64;

/// The chat client, when embeddings are configured.
pub fn client(bot: &Bot) -> Option<&OpenRouter> {
    bot.ai.as_ref().filter(|_| !bot.config.openrouter_embedding_model.is_empty())
}

/// The embedded text, so a memory is found by its name as well as its content.
fn text(memory: &Memory) -> String {
    format!("{}: {}", memory.key, memory.content)
}

/// Embeds every memory without a current embedding and returns how many were stored.
async fn embed_pending(db: &Pool, ai: &OpenRouter, model: &str) -> Result<u64> {
    let mut stored = 0;
    loop {
        let pending = db::unembedded_memories(db, model, BATCH).await?;
        if pending.is_empty() {
            return Ok(stored);
        }
        let texts: Vec<_> = pending.iter().map(text).collect();
        let embeddings = ai.embed(model, &texts, DIMENSIONS, Duration::from_secs(60)).await?;
        let batch = db::store_embeddings(db, model, &pending, &embeddings).await?;
        stored += batch;
        // A batch lost entirely to concurrent edits is retried on the next sweep instead of spinning here.
        if pending.len() < BATCH || batch == 0 {
            return Ok(stored);
        }
    }
}

/// One periodic worker covers existing rows, model writes, web edits and embedding model changes.
pub fn spawn(bot: Arc<Bot>) {
    let Some(ai) = client(&bot).cloned() else { return };
    tokio::spawn(async move {
        let model = &bot.config.openrouter_embedding_model;
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match embed_pending(&bot.db, &ai, model).await {
                Ok(0) => {}
                Ok(stored) => tracing::info!(kind = "memory_embeddings", stored, "Embedded memories"),
                Err(error) => {
                    tracing::warn!(kind = "memory_embeddings", %error, "Embedding deferred until next sweep");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_text_includes_the_name() {
        let memory = Memory { id: 1, user_id: 1, key: "work".into(), content: "builds bots".into() };
        assert_eq!(text(&memory), "work: builds bots");
    }
}
