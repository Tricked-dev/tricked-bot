use color_eyre::Result;
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

use crate::{config::Config, db};

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
fn build_memory_prompt(context: &str, participants: &str, profiles: &str) -> String {
    format!(
        r#"You are a memory creation system for a Discord bot. Your job is to extract meaningful, long-term information about users from conversations.

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

**Participants in this conversation:** {participants}

**Existing profiles (evolve these; do not discard established facts without evidence):**
{profiles}

**Conversation:**
{context}

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
5. **Combine and deduplicate** - If this conversation adds to an existing category, write a complete updated entry that includes both old and new info
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

Remember: Output ONLY valid JSON, nothing else. Focus on persistent, meaningful information."#,
        context = context,
        participants = participants,
        profiles = profiles
    )
}

/// Process memory creation response and store in database
async fn process_memory_response(database: &Pool, response_text: &str) -> Result<usize> {
    log::info!("Raw memory response: {}", response_text);

    // Try to extract JSON from the response (in case the model adds extra text)
    let json_start = response_text.find('{').unwrap_or(0);
    let json_end = response_text.rfind('}').map(|i| i + 1).unwrap_or(response_text.len());
    let json_text = &response_text[json_start..json_end];

    let memory_response: MemoryCreationResponse = serde_json::from_str(json_text)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to parse memory JSON: {} - Raw: {}", e, json_text))?;

    let mut created_count = 0;

    for entry in memory_response.memories {
        // Resolve username to user ID
        if let Ok(Some(user)) = db::get_user_by_name(database, &entry.username).await {
            let user_id = user.discord_id();
            match db::upsert_memory(database, user_id, &entry.key, &entry.content).await {
                Ok(_) => {
                    log::info!(
                        "Created memory for user {} ({}): {} = {}",
                        entry.username,
                        user_id,
                        entry.key,
                        entry.content
                    );
                    created_count += 1;
                }
                Err(e) => {
                    log::error!("Failed to insert memory for {}: {}", entry.username, e);
                }
            }
        } else {
            log::warn!(
                "Could not resolve username '{}' to user ID, skipping memory",
                entry.username
            );
        }
    }

    for update in memory_response.profile_updates {
        let Some(user) = db::get_user_by_name(database, &update.username).await? else {
            log::warn!("Could not resolve profile username '{}', skipping", update.username);
            continue;
        };
        let relationship = update.relationship.filter(|v| !v.trim().is_empty() && v.len() <= 1000);
        let example_input = update.example_input.filter(|v| !v.trim().is_empty() && v.len() <= 2000);
        let example_output = update.example_output.filter(|v| !v.trim().is_empty() && v.len() <= 2000);
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

/// Main function to create memories in the background
pub async fn create_memories_background(
    database: Pool,
    context: String,
    user_mentions: HashMap<String, u64>,
    config: Arc<Config>,
) {
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

    // Build list of participants
    let mut participants: Vec<String> = Vec::new();
    for (_mention, &user_id) in &user_mentions {
        if let Ok(Some(u)) = db::get_user(&database, user_id).await {
            participants.push(u.name);
        }
    }

    let participants_str = participants.join(", ");

    let mut profiles = Vec::new();
    for name in &participants {
        if let Ok(Some(user)) = db::get_user_by_name(&database, name).await {
            profiles.push(serde_json::json!({
                "username": user.name,
                "relationship": user.relationship,
                "example_input": user.example_input,
                "example_output": user.example_output,
            }));
        }
    }
    let profiles_json = serde_json::to_string(&profiles).unwrap_or_else(|_| "[]".to_string());

    // Build the memory creation prompt
    let system_prompt = build_memory_prompt(&context, &participants_str, &profiles_json);

    log::debug!("Memory creation prompt: {}", system_prompt);

    let request = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": system_prompt }],
        "max_tokens": 1024,
        "response_format": { "type": "json_object" },
        "reasoning": { "effort": "none" }
    });
    let http_client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build() {
        Ok(client) => client,
        Err(e) => {
            log::error!("Failed to create memory HTTP client: {}", e);
            return;
        }
    };
    let mut http_request = http_client
        .post(format!("{}/chat/completions", config.openrouter_base_url.trim_end_matches('/')))
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
    match process_memory_response(&database, response_text).await {
        Ok(count) => {
            log::info!("Successfully created {} memories", count);
        }
        Err(e) => {
            log::error!("Failed to process memory response: {}", e);
        }
    }
}
