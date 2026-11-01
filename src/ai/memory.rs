//! Extracts lasting facts about an author from their message into per-user memories.

use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, Mutex},
    time::Duration,
};

use color_eyre::{Result, eyre::eyre};
use serde::Deserialize;
use serde_json::json;
use twilight_model::id::{Id, marker::MessageMarker};

use super::{
    AiRequest,
    openrouter::{ChatRequest, Message, Role},
};
use crate::{bot::Bot, db, decider::questions};

#[derive(Debug, Deserialize)]
struct MemoryCreationResponse {
    #[serde(default)]
    memories: Vec<MemoryEntry>,
}

#[derive(Debug, Deserialize)]
struct MemoryEntry {
    username: String,
    key: String,
    content: String,
}

/// Instructions for the memory model. Static, so providers can cache them.
const SYSTEM: &str = include_str!("prompts/memory.md");

/// The data the memory model works on: the author's message, the conversation around it and what is already stored.
fn memory_input(author: &str, req: &AiRequest, existing: &str) -> String {
    let mut input = format!("<author>{author}</author>\n<message>\n{}\n</message>\n", req.message);
    let recent = questions::recent(req);
    if !recent.is_empty() || req.reply_target.is_some() {
        input.push_str("<conversation note=\"context only\">\n");
        for line in recent {
            input.push_str(line);
            input.push('\n');
        }
        if let Some(target) = &req.reply_target {
            let _ = writeln!(input, "(the message replies to) {target}");
        }
        input.push_str("</conversation>\n");
    }
    let _ = write!(input, "<existing_memories>\n{existing}\n</existing_memories>");
    input
}

/// Parses the outermost JSON object, tolerating code fences or prose around it.
fn parse_response(text: &str) -> Result<MemoryCreationResponse> {
    let start = text.find('{').ok_or_else(|| eyre!("Memory response contains no JSON object"))?;
    let end = text
        .rfind('}')
        .filter(|end| *end >= start)
        .ok_or_else(|| eyre!("Memory response contains no complete JSON object"))?;
    serde_json::from_str(&text[start..=end]).map_err(|e| eyre!("Failed to parse memory JSON: {e}"))
}

/// Each author retains its high-water mark for the process lifetime. Keeping it with the write lock prevents a
/// delayed decider result from reverting newer facts.
#[derive(Debug, Default)]
pub struct AuthorWriteState {
    latest_success: Option<Id<MessageMarker>>,
}

impl AuthorWriteState {
    fn is_stale(&self, message: Id<MessageMarker>) -> bool {
        self.latest_success.is_some_and(|latest| message <= latest)
    }
}

/// Serializes memory writes and cleanup per author.
#[derive(Debug, Default)]
pub struct AuthorLocks(Mutex<HashMap<u64, Arc<tokio::sync::Mutex<AuthorWriteState>>>>);

impl AuthorLocks {
    pub fn get(&self, author: u64) -> Arc<tokio::sync::Mutex<AuthorWriteState>> {
        Arc::clone(self.0.lock().unwrap().entry(author).or_default())
    }
}

pub async fn write_for_author(bot: Arc<Bot>, req: AiRequest) {
    let lock = bot.memory_locks.get(req.user_id.get());
    let mut state = lock.lock().await;
    if state.is_stale(req.message_id) {
        tracing::info!(user = %req.user_id, msg_id = %req.message_id, "Skipping stale memory event");
        return;
    }
    match write(&bot, &req).await {
        Ok(count) => {
            state.latest_success = Some(req.message_id);
            tracing::info!(user = %req.user_id, count, "Stored memories");
        }
        Err(error) => tracing::warn!(user = %req.user_id, msg_id = %req.message_id, "Memory write failed: {error}"),
    }
}

async fn write(bot: &Bot, req: &AiRequest) -> Result<usize> {
    let ai = bot.ai.as_ref().ok_or_else(|| eyre!("OpenRouter API key not configured"))?;
    let author = req.user_id.get();
    db::ensure_user(&bot.db, author, &req.user_name).await?;
    let (user, memories) = tokio::try_join!(db::user(&bot.db, author), db::memories(&bot.db, author))?;
    let user = user.ok_or_else(|| eyre!("author disappeared"))?;
    let existing: Vec<_> = memories.into_iter().map(|m| json!({"key": m.key, "content": m.content})).collect();
    let input = memory_input(&user.name, req, &serde_json::Value::from(existing).to_string());
    let request = ChatRequest {
        model: bot.config.openrouter_memory_model.clone(),
        messages: vec![Message::new(Role::System, SYSTEM), Message::new(Role::User, input)],
        max_tokens: 1024,
        response_format: Some(json!({ "type": "json_object" })),
        reasoning: Some(json!({ "effort": "none" })),
        ..Default::default()
    };
    let response = parse_response(&ai.complete(&request, Duration::from_secs(120)).await?)?;
    // Names are exact: the model must not write memories about other people in the conversation.
    let own: Vec<_> = response
        .memories
        .iter()
        .filter(|entry| entry.username == user.name)
        .map(|entry| (entry.key.as_str(), entry.content.as_str()))
        .collect();
    if own.len() < response.memories.len() {
        tracing::warn!(
            user = author,
            ignored = response.memories.len() - own.len(),
            "Ignoring memories for another author"
        );
    }
    db::upsert_memories(&bot.db, author, &own).await?;
    Ok(own.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_holds_the_message_conversation_and_existing_memories() {
        let mut req = crate::ai::tests::request("I hate cilantro");
        let existing = r#"[{"key":"preferences","content":"Uses Linux."}]"#;
        assert_eq!(
            memory_input("alice", &req, existing),
            format!(
                "<author>alice</author>\n<message>\nI hate cilantro\n</message>\n<existing_memories>\n{existing}\n</existing_memories>"
            )
        );
        req.conversation = vec!["bob: lol".into()];
        req.reply_target = Some("bob: what do you hate".into());
        assert!(memory_input("alice", &req, "[]").contains(
            "<conversation note=\"context only\">\nbob: lol\n(the message replies to) bob: what do you hate\n</conversation>"
        ));
        assert!(SYSTEM.contains("complete replacement"));
    }

    #[tokio::test]
    async fn concurrent_author_writers_share_a_lock() {
        let locks = AuthorLocks::default();
        let (first, second, other) = (locks.get(123), locks.get(123), locks.get(456));
        let guard = first.lock().await;
        assert!(second.try_lock().is_err());
        assert!(other.try_lock().is_ok());
        drop(guard);
        assert!(second.try_lock().is_ok());
    }

    #[tokio::test]
    async fn newer_completion_blocks_older_arrival_even_after_lock_handles_drop() {
        let locks = AuthorLocks::default();
        let newer = locks.get(987_654);
        {
            let mut state = newer.lock().await;
            assert!(!state.is_stale(Id::new(200)));
            state.latest_success = Some(Id::new(200));
        }
        drop(newer);
        let delayed = locks.get(987_654);
        let state = delayed.lock().await;
        assert!(state.is_stale(Id::new(100)));
        assert!(state.is_stale(Id::new(200)));
        assert!(!state.is_stale(Id::new(201)));
        assert!(!locks.get(987_655).lock().await.is_stale(Id::new(100)));
    }

    #[test]
    fn malformed_response_is_an_error_not_a_panic() {
        assert!(parse_response("} malformed {").is_err());
        assert!(parse_response("no JSON").is_err());
        assert!(parse_response("```json\n{\"memories\": []}\n```").is_ok());
    }
}
