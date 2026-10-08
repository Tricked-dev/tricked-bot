use color_eyre::Result;
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use crate::{config::Config, database::User, db};

/// JSON response structure for memory creation
#[derive(Debug, Serialize, Deserialize)]
struct MemoryCreationResponse {
    #[serde(default)]
    memories: Vec<MemoryEntry>,
    #[serde(default)]
    profile_updates: Vec<ProfileUpdate>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MemoryEntry {
    username: String,
    key: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProfileUpdate {
    username: String,
    relationship: Option<String>,
    example_input: Option<String>,
    example_output: Option<String>,
}

/// Build the enhanced system prompt for memory creation with quality guidelines
fn build_memory_prompt(author: &str, message: &str, context: &str, profile: &str, existing: &str) -> String {
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
**Their current profile (evolve; never discard without evidence):**
{profile}
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
  ],
  "profile_updates": [
    {{
      "username": "exact_username_from_conversation",
      "relationship": "how this user and the bot currently relate, or null",
      "example_input": "a real representative user message from the transcript, or null",
      "example_output": "the bot reply paired with that message, or null"
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
7. **Profiles evolve slowly** - Update relationships only when the transcript contains clear evidence of a lasting change
8. **Real examples only** - Example input/output must be an actual adjacent user/bot exchange from the transcript; never invent one
9. **No destructive blanks** - Use null for fields that should remain unchanged

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

fn parse_response(response_text: &str) -> Result<MemoryCreationResponse> {
    let start = response_text
        .find('{')
        .ok_or_else(|| color_eyre::eyre::eyre!("Memory response contains no JSON object"))?;
    let end = response_text
        .rfind('}')
        .filter(|end| *end >= start)
        .ok_or_else(|| color_eyre::eyre::eyre!("Memory response contains no complete JSON object"))?;
    serde_json::from_str(&response_text[start..=end])
        .map_err(|e| color_eyre::eyre::eyre!("Failed to parse memory JSON: {}", e))
}

/// Process memory creation response and store in database
async fn process_memory_response(database: &Pool, user: &User, response_text: &str) -> Result<usize> {
    let memory_response = parse_response(response_text)?;

    let mut created_count = 0;

    for entry in memory_response.memories {
        if !belongs_to_author(&entry.username, user) {
            tracing::warn!(user = user.discord_id(), "Ignoring memory for another author");
            continue;
        }
        db::upsert_memory(database, user.discord_id(), &entry.key, &entry.content).await?;
        tracing::info!(user = user.discord_id(), "Created memory");
        created_count += 1;
    }

    for update in memory_response.profile_updates {
        if !belongs_to_author(&update.username, user) {
            tracing::warn!(user = user.discord_id(), "Ignoring profile for another author");
            continue;
        }
        let relationship = update.relationship.filter(|v| !v.trim().is_empty() && v.len() <= 1000);
        let example_input = update.example_input.filter(|v| !v.trim().is_empty() && v.len() <= 2000);
        let example_output = update
            .example_output
            .filter(|v| !v.trim().is_empty() && v.len() <= 2000);
        if let Some(relationship) = relationship.filter(|value| value != &user.relationship) {
            db::stage_profile_candidate(database, user.discord_id(), "relationship", &relationship).await?;
            created_count += 1;
        }
        if let (Some(input), Some(output)) = (example_input, example_output) {
            if input == user.example_input && output == user.example_output {
                continue;
            }
            let pair = serde_json::json!({ "input": input, "output": output }).to_string();
            db::stage_profile_candidate(database, user.discord_id(), "example", &pair).await?;
            created_count += 1;
        }
    }

    Ok(created_count)
}

fn belongs_to_author(username: &str, user: &User) -> bool {
    username == user.name
}

// Each author retains its high-water mark for this process lifetime. Keeping it
// with the write lock prevents a delayed decider result from reverting newer facts.
#[derive(Default)]
pub(crate) struct AuthorWriteState {
    latest_success: Option<u64>,
}

impl AuthorWriteState {
    fn is_stale(&self, message_id: u64) -> bool {
        self.latest_success.is_some_and(|latest| message_id <= latest)
    }
}

pub(crate) fn author_lock(author: u64) -> Arc<tokio::sync::Mutex<AuthorWriteState>> {
    static AUTHORS: OnceLock<std::sync::Mutex<HashMap<u64, Arc<tokio::sync::Mutex<AuthorWriteState>>>>> =
        OnceLock::new();
    let mut authors = AUTHORS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    authors.entry(author).or_default().clone()
}

pub async fn write_for_author(
    database: Pool,
    config: Arc<Config>,
    author_id: u64,
    source_message_id: u64,
    message: String,
    context: String,
) {
    let lock = author_lock(author_id);
    let mut guard = lock.lock().await;
    if guard.is_stale(source_message_id) {
        tracing::info!(
            user = author_id,
            msg_id = source_message_id,
            "Skipping stale memory event"
        );
        return;
    }
    // Get API key
    let api_key = match &config.openrouter_api_key {
        Some(key) => key.clone(),
        None => {
            log::warn!("OpenRouter API key not configured, skipping memory creation");
            return;
        }
    };

    // Profile extraction needs deterministic JSON, not the chat model's long
    // reasoning mode. It remains configurable for future model changes.
    let model = config
        .openrouter_memory_model
        .clone()
        .unwrap_or_else(|| "tencent/hy3-preview".to_string());

    log::info!("Creating memories using model: {}", model);

    let user = match db::get_user(&database, author_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(user = author_id, %error, "Memory writer could not load author");
            return;
        }
    };
    let existing = match db::get_all_memories(&database, author_id).await {
        Ok(memories) => memories
            .into_iter()
            .map(|m| serde_json::json!({"key": m.key, "content": m.content}))
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(user = author_id, %error, "Memory writer could not load existing facts");
            return;
        }
    };
    let profile = serde_json::json!({
        "relationship": user.relationship,
        "example_input": user.example_input,
        "example_output": user.example_output,
    });
    let system_prompt = build_memory_prompt(
        &user.name,
        &message,
        &context,
        &profile.to_string(),
        &serde_json::json!(existing).to_string(),
    );

    let request = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": system_prompt }],
        "max_tokens": 1024,
        "response_format": { "type": "json_object" },
        "reasoning": { "effort": "none" }
    });
    let http_client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            log::error!("Failed to create memory HTTP client: {}", e);
            return;
        }
    };
    let mut http_request = http_client
        .post(format!(
            "{}/chat/completions",
            config.openrouter_base_url.trim_end_matches('/')
        ))
        .bearer_auth(api_key)
        .json(&request);
    if let Some(url) = &config.openrouter_site_url {
        http_request = http_request.header("HTTP-Referer", url);
    }
    if let Some(name) = &config.openrouter_site_name {
        http_request = http_request.header("X-Title", name);
    }
    let response = match http_request.send().await {
        Ok(response) => response,
        Err(e) => {
            log::error!("Memory model request failed: {}", e);
            return;
        }
    };
    if !response.status().is_success() {
        log::error!("Memory model returned HTTP {}", response.status());
        return;
    }
    let response: serde_json::Value = match response.json().await {
        Ok(response) => response,
        Err(e) => {
            log::error!("Invalid memory model response: {}", e);
            return;
        }
    };
    let Some(response_text) = response.pointer("/choices/0/message/content").and_then(|v| v.as_str()) else {
        log::error!("Memory model returned no content");
        return;
    };

    // Process the response
    match process_memory_response(&database, &user, response_text).await {
        Ok(count) => {
            guard.latest_success = Some(source_message_id);
            log::info!("Successfully created {} memories", count);
        }
        Err(e) => {
            log::error!("Failed to process memory response: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_includes_existing_memories_and_replace_rule() {
        let existing = r#"[{"key":"preferences","content":"Uses Linux."}]"#;
        let p = build_memory_prompt("alice", "alice: I hate cilantro", "bob: lol", "{}", existing);
        assert!(p.contains("Uses Linux."));
        assert!(p.contains("**Existing memories"));
        assert!(p.contains("complete replacement"));
        assert!(p.contains("alice: I hate cilantro"));
    }
    #[tokio::test]
    async fn concurrent_author_writers_share_a_lock() {
        let first = author_lock(123);
        let second = author_lock(123);
        let other = author_lock(456);
        let guard = first.lock().await;
        assert!(second.try_lock().is_err());
        assert!(other.try_lock().is_ok());
        drop(guard);
        assert!(second.try_lock().is_ok());
    }
    #[tokio::test]
    async fn newer_completion_blocks_older_arrival_even_after_lock_handles_drop() {
        let newer = author_lock(987_654);
        {
            let mut state = newer.lock().await;
            assert!(!state.is_stale(200));
            state.latest_success = Some(200);
        }
        drop(newer);
        let delayed = author_lock(987_654);
        let state = delayed.lock().await;
        assert!(state.is_stale(100));
        assert!(state.is_stale(200));
        assert!(!state.is_stale(201));
        assert!(!author_lock(987_655).lock().await.is_stale(100));
    }

    #[test]
    fn malformed_response_is_an_error_not_a_panic() {
        assert!(parse_response("} malformed {").is_err());
        assert!(parse_response("no JSON").is_err());
        assert!(parse_response("```json\n{\"memories\": []}\n```").is_ok());
    }
    #[test]
    fn author_matching_is_exact() {
        let user = User {
            id: 1,
            name: "alice".into(),
            level: 0,
            xp: 0,
            social_credit: 0,
            relationship: String::new(),
            example_input: String::new(),
            example_output: String::new(),
        };
        assert!(belongs_to_author("alice", &user));
        assert!(!belongs_to_author("bob", &user));
        assert!(!belongs_to_author("Alice", &user));
    }
}
