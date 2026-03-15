use color_eyre::Result;
use deadpool_postgres::Pool;
use openrouter_api::{
    types::chat::{ChatCompletionRequest, Message, MessageContent},
    OpenRouterClient,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

use crate::{config::Config, db};

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

/// Build the enhanced system prompt for memory creation with quality guidelines
fn build_memory_prompt(context: &str, participants: &str) -> String {
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
  ]
}}

**Critical Guidelines:**
1. **Quality over quantity** - Only create memories for meaningful, lasting information
2. **One entry per category** - Combine ALL related facts into ONE comprehensive entry per "key"
3. **Broad categories** - Use keys like: "preferences", "hobbies", "work", "personality", "relationships", "technical_skills", "life_context", "communication_style"
4. **Exact usernames** - Must match exactly as they appear in the conversation
5. **Combine and deduplicate** - If this conversation adds to an existing category, write a complete updated entry that includes both old and new info
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

Remember: Output ONLY valid JSON, nothing else. Focus on persistent, meaningful information."#,
        context = context,
        participants = participants
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
    let mut participants: Vec<String> = Vec::new();
    for (_mention, &user_id) in &user_mentions {
        if let Ok(Some(u)) = db::get_user(&database, user_id).await {
            participants.push(u.name);
        }
    }

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
    match process_memory_response(&database, &response_text).await {
        Ok(count) => {
            log::info!("Successfully created {} memories", count);
        }
        Err(e) => {
            log::error!("Failed to process memory response: {}", e);
        }
    }
}
