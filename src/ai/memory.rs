//! Extracts lasting facts about an author from their message into per-user memories.

use std::{
    collections::HashMap,
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

/// Build the system prompt for memory creation with quality guidelines.
fn build_memory_prompt(author: &str, message: &str, context: &str, existing: &str) -> String {
    format!(
        r#"You are a memory creation system for a Discord bot. Your job is to extract lasting information about **{author}** from their message below.

**What makes a GOOD memory:**
- Persistent facts (hobbies, preferences, job, relationships, personality traits)
- Important life events or milestones
- Recurring patterns or behaviors
- Strong opinions or beliefs
- Personal context that helps future interactions

**What makes a BAD memory:**
- Temporary status ("is busy today", "feeling tired")
- One-off jokes or comments with no lasting relevance
- Information already implied by context
- Vague or generic statements
- Duplicates of existing information

**Author:** {author}
**Their message (the reason you were called):**
{message}
**Recent conversation (context only; do not write memories about other people):**
{context}
**Existing memories (your output REPLACES the whole row for that key):**
{existing}

**Output Format** - Respond ONLY with valid JSON:
{{
  "memories": [
    {{
      "username": "exact_username_from_conversation",
      "key": "category_or_topic",
      "content": "comprehensive memory content"
    }}
  ]
}}

**Critical Guidelines:**
1. **Quality over quantity** - Only create memories for meaningful, lasting information
2. **One entry per category** - Combine ALL related facts into ONE comprehensive entry per "key"
3. **Broad categories** - Use keys like: "preferences", "hobbies", "work", "personality", "relationships", "technical_skills", "life_context", "communication_style"
4. **Exact usernames** - Must match exactly as they appear in the conversation
5. **Merge, never drop** - For a key that already exists, output the complete replacement: keep every established fact unless the new message clearly contradicts it, then add the new facts. Omit keys that did not change.
6. **Empty when appropriate** - If there's nothing worth remembering long-term, return {{"memories": []}}

**Examples:**

GOOD:
{{"username": "Alice", "key": "hobbies", "content": "Passionate about rock climbing and photography. Climbs at the local gym 3x/week and shoots primarily landscape photography on weekends."}}

BAD:
{{"username": "Alice", "key": "today", "content": "went climbing"}}

GOOD:
{{"username": "Bob", "key": "work", "content": "Senior software engineer at a fintech startup. Specializes in backend systems and distributed databases. Currently working on migrating to microservices architecture."}}

BAD:
{{"username": "Bob", "key": "current_task", "content": "debugging code"}}

Remember: Output ONLY valid JSON, nothing else. Focus on persistent, meaningful information."#
    )
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
    let user = db::user(&bot.db, author).await?.ok_or_else(|| eyre!("author disappeared"))?;
    let existing: Vec<_> =
        db::memories(&bot.db, author).await?.into_iter().map(|m| json!({"key": m.key, "content": m.content})).collect();
    let prompt = build_memory_prompt(
        &user.name,
        &req.message,
        &questions::short_context(&req.context),
        &serde_json::Value::from(existing).to_string(),
    );
    let request = ChatRequest {
        model: bot.config.openrouter_memory_model.clone(),
        messages: vec![Message::new(Role::User, prompt)],
        max_tokens: 1024,
        response_format: Some(json!({ "type": "json_object" })),
        reasoning: Some(json!({ "effort": "none" })),
        ..Default::default()
    };
    let response = parse_response(&ai.complete(&request, Duration::from_secs(120)).await?)?;
    let mut stored = 0;
    for entry in response.memories {
        // Names are exact: the model must not write memories about other people in the conversation.
        if entry.username != user.name {
            tracing::warn!(user = author, "Ignoring memory for another author");
            continue;
        }
        db::upsert_memory(&bot.db, author, &entry.key, &entry.content).await?;
        stored += 1;
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_includes_existing_memories_and_replace_rule() {
        let existing = r#"[{"key":"preferences","content":"Uses Linux."}]"#;
        let p = build_memory_prompt("alice", "alice: I hate cilantro", "bob: lol", existing);
        assert!(p.contains("Uses Linux."));
        assert!(p.contains("**Existing memories"));
        assert!(p.contains("complete replacement"));
        assert!(p.contains("alice: I hate cilantro"));
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
