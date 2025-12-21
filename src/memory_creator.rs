use color_eyre::Result;
use openrouter_api::{
    types::chat::{ChatCompletionRequest, Message, MessageContent},
    OpenRouterClient,
};
use r2d2_sqlite::SqliteConnectionManager;
use serde::{Deserialize, Serialize};
use serde_rusqlite::from_row;
use std::{collections::HashMap, sync::Arc};
use wb_sqlite::InsertSync;

use crate::{
    config::Config,
    database::{Memory, User},
};

/// JSON response structure for memory creation
#[derive(Debug, Serialize, Deserialize)]
struct MemoryCreationResponse {
    memories: Vec<MemoryEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MemoryEntry {
    username: String,
    key: String,
    content: String,
}

/// Build the system prompt for memory creation
fn build_memory_prompt(context: &str, participants: &str) -> String {
    format!(
        r#"You are a memory creation system for a Discord bot. Your job is to extract important facts, preferences, and information about users from conversations.

Analyze the following conversation and create memories for each participant. Focus on:
- Personal preferences and interests
- Facts about their life, work, or hobbies
- Relationships with other users
- Behaviors and patterns
- Important events or milestones

Participants in this conversation: {participants}

Conversation:
{context}

Respond ONLY with valid JSON in this exact format:
{{
  "memories": [
    {{
      "username": "exact_username_from_conversation",
      "key": "category_or_topic",
      "content": "the actual memory content"
    }}
  ]
}}

IMPORTANT GUIDELINES:
- Only create memories if there's meaningful information from THIS conversation
- Each user should have AT MOST ONE memory entry per unique "key" category
- The "key" should be a broad category like "preferences", "hobbies", "work", "personality", "relationships", "recent_activity"
- The "content" should combine ALL related facts for that category into ONE comprehensive entry
- Use exact usernames as they appear in the conversation
- If there's nothing meaningful to remember, return an empty memories array

EXAMPLE - CORRECT (combining multiple facts under one key):
{{
  "memories": [
    {{
      "username": "tricked.",
      "key": "preferences",
      "content": "Likes cats, dislikes insects"
    }}
  ]
}}

EXAMPLE - WRONG (duplicate keys for same user):
{{
  "memories": [
    {{"username": "tricked.", "key": "preferences", "content": "Likes cats"}},
    {{"username": "tricked.", "key": "preferences", "content": "Dislikes insects"}}
  ]
}}

Remember: Output ONLY valid JSON, nothing else. Combine related information under the same key."#,
        context = context,
        participants = participants
    )
}

/// Resolve usernames to user IDs using the database
fn resolve_username_to_id(
    database: &r2d2::Pool<SqliteConnectionManager>,
    username: &str,
) -> Option<u64> {
    let db = database.get().ok()?;
    let mut stmt = db
        .prepare("SELECT * FROM user WHERE name = ? COLLATE NOCASE")
        .ok()?;

    stmt.query_one([username], |row| {
        from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
    })
    .ok()
    .map(|user| user.id)
}

/// Insert a new memory in the database
/// If a memory with the same user_id and key exists, delete it first then insert the new one
/// This ensures the new memory has the latest timestamp and will be in the top 5 most recent
fn insert_memory(
    database: &r2d2::Pool<SqliteConnectionManager>,
    user_id: u64,
    key: &str,
    content: &str,
) -> Result<()> {
    let db = database.get()?;

    // Delete any existing memory with the same user_id and key
    db.execute(
        "DELETE FROM memory WHERE user_id = ? AND key = ?",
        rusqlite::params![user_id.to_string(), key],
    )?;

    // Insert the new memory (will get a new ID and timestamp)
    let memory = Memory {
        id: 0, // Will be auto-generated
        user_id: user_id.to_string(),
        content: content.to_string(),
        key: key.to_string(),
    };
    memory.insert_sync(&db)?;

    Ok(())
}

/// Process memory creation response and store in database
fn process_memory_response(
    database: &r2d2::Pool<SqliteConnectionManager>,
    response_text: &str,
) -> Result<usize> {
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
        if let Some(user_id) = resolve_username_to_id(database, &entry.username) {
            match insert_memory(database, user_id, &entry.key, &entry.content) {
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

    Ok(created_count)
}

/// Main function to create memories in the background
pub async fn create_memories_background(
    database: r2d2::Pool<SqliteConnectionManager>,
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

    // Determine which model to use (memory model or default to main model)
    let model = config
        .openrouter_memory_model
        .clone()
        .unwrap_or_else(|| config.openrouter_model.clone());

    log::info!("Creating memories using model: {}", model);

    // Create client
    let client = match OpenRouterClient::new()
        .skip_url_configuration()
        .with_retries(3, 1000)
        .with_timeout_secs(120)
        .configure(
            &api_key,
            config.openrouter_site_url.as_deref(),
            config.openrouter_site_name.as_deref(),
        ) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to create OpenRouter client for memory creation: {}", e);
            return;
        }
    };

    // Build list of participants
    let participants: Vec<String> = user_mentions
        .iter()
        .filter_map(|(_mention, user_id)| {
            let db = database.get().ok()?;
            let mut stmt = db.prepare("SELECT * FROM user WHERE id = ?").ok()?;
            stmt.query_one([user_id.to_string()], |row| {
                from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
            })
            .ok()
            .map(|user| user.name)
        })
        .collect();

    let participants_str = participants.join(", ");

    // Build the memory creation prompt
    let system_prompt = build_memory_prompt(&context, &participants_str);

    log::debug!("Memory creation prompt: {}", system_prompt);

    // Build request (non-streaming)
    let request = ChatCompletionRequest {
        model,
        messages: vec![Message {
            role: "user".to_string(),
            content: MessageContent::Text(system_prompt),
            ..Default::default()
        }],
        max_tokens: Some(2048),
        stream: Some(false), // Disable streaming for memory creation
        ..Default::default()
    };

    // Create chat client
    let chat_client = match client.chat() {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to create chat client for memory creation: {}", e);
            return;
        }
    };

    // Get the complete response (non-streaming)
    let response = match chat_client.chat_completion(request).await {
        Ok(r) => r,
        Err(e) => {
            log::error!("Error getting memory creation response: {:?}", e);
            return;
        }
    };

    // Extract the content from the response
    let response_text = match response.choices.first() {
        Some(choice) => match &choice.message.content {
            MessageContent::Text(text) => text.clone(),
            _ => {
                log::error!("No text content in memory creation response");
                return;
            }
        },
        None => {
            log::error!("No choices in memory creation response");
            return;
        }
    };

    // Process the response
    match process_memory_response(&database, &response_text) {
        Ok(count) => {
            log::info!("Successfully created {} memories", count);
        }
        Err(e) => {
            log::error!("Failed to process memory response: {}", e);
        }
    }
}
