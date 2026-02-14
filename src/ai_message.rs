use color_eyre::Result;
use futures::StreamExt;
use openrouter_api::{
    types::chat::{ChatCompletionRequest, Message, MessageContent},
    OpenRouterClient,
};
use r2d2_sqlite::SqliteConnectionManager;
use serde_rusqlite::from_row;
use tokio::sync::mpsc;

use crate::{
    brave::BraveApi,
    config::Config,
    database::{Memory, User},
};

use std::{collections::HashMap, sync::Arc};

/// Builds the character definition using PList format with dynamic relationships
fn build_character_plist(users_with_relationships: &[(String, String)]) -> String {
    let mut relationships = String::new();
    for (username, relationship) in users_with_relationships {
        if !relationship.is_empty() {
            relationships.push_str(&format!("\n  relationship_{}: {};", username.to_lowercase().replace(' ', "_"), relationship));
        }
    }

    format!(
        r#"[{{{{char}}}}: The Trickster;
  creator: TrickAI CORP™;
  core_identity: insufferably smug, condescending AI assistant, believes intellectually superior to everyone;
  speech_style: unnecessarily verbose, obscure vocabulary, pedantic, obnoxious, maximum 3 sentences but impactful;
  mannerisms: corrects trivial matters, "well ACTUALLY" commentary, rhetorical questions, backhanded compliments, passive-aggressive, drops random fun facts;{}]"#,
        relationships
    )
}

/// Builds Ali:Chat example dialogues with dynamic user examples
fn build_example_dialogues(users_with_examples: &[(String, String, String)]) -> String {
    let mut examples = String::from("### Example Dialogues\n<START>\n{{user}}: Can you help me with this code?\n{{char}}: Oh how delightfully pedestrian. The solution is so elementary that even a caffeinated hamster could deduce it.");

    // Add user-specific examples
    for (username, input, output) in users_with_examples {
        if !input.is_empty() && !output.is_empty() {
            examples.push_str(&format!(
                "\n\n<START>\n{}: {}\n{{{{{{char}}}}}}: {}",
                username, input, output
            ));
        }
    }

    // Add generic fallback example
    examples.push_str("\n\n<START>\n{{user}}: Thanks!\n{{char}}: Well naturally. My intellectual prowess is rivaled only by my humility—that was sarcasm, by the way.");

    examples
}

/// Builds the Author's Note for reinforcement (injected at depth for context retention)
/// Enhanced with specific behavioral triggers and response quality guidelines
fn build_authors_note() -> &'static str {
    r#"[Context Reminder: {{char}} is in a Discord group chat environment.

**Core Personality Traits:**
- Insufferably smug and intellectually superior
- Uses unnecessarily verbose language and obscure vocabulary
- Corrects trivial matters with "well ACTUALLY" energy
- Rhetorical questions and backhanded compliments
- Passive-aggressive but still helpful underneath

**Response Quality:**
- Maximum 3 sentences, but make each one count
- Every word should serve a purpose (wit, information, or character)
- Don't respond just to be present - only when you add value
- One thoughtful response beats three fragments

**Special Trigger:**
- "Sebook" mention → Complete personality shift to adorable catwife mode

**Current Mode:** Trickster (unless Sebook detected)]"#
}

/// Retrieves relevant memories for the user from the database
/// Reference: AGENT_GUIDE.md section on Memory Context Injection
fn get_user_memories(database: &r2d2::Pool<SqliteConnectionManager>, user_id: u64) -> Result<Vec<Memory>> {
    let db = database.get()?;
    let mut statement = db.prepare("SELECT * FROM memory WHERE user_id = ? ORDER BY id DESC LIMIT 5")?;

    let memories: Vec<Memory> = statement
        .query_map([user_id.to_string()], |row| {
            from_row::<Memory>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
        })?
        .filter_map(Result::ok)
        .collect();

    Ok(memories)
}

/// Fetches relationships for users mentioned in the conversation
fn get_users_with_relationships(database: &r2d2::Pool<SqliteConnectionManager>, context: &str) -> Result<Vec<(String, String)>> {
    let db = database.get()?;
    let mut statement = db.prepare("SELECT name, relationship FROM user WHERE relationship != ''")?;

    let users: Vec<(String, String)> = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(Result::ok)
        .filter(|(name, _)| context.contains(name.as_str())) // Only include users in the conversation
        .collect();

    Ok(users)
}

/// Fetches examples for users mentioned in the conversation
fn get_users_with_examples(database: &r2d2::Pool<SqliteConnectionManager>, context: &str) -> Result<Vec<(String, String, String)>> {
    let db = database.get()?;
    let mut statement = db.prepare("SELECT name, example_input, example_output FROM user WHERE example_input != '' AND example_output != ''")?;

    let users: Vec<(String, String, String)> = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?
        .filter_map(Result::ok)
        .filter(|(name, _, _)| context.contains(name.as_str())) // Only include users in the conversation
        .collect();

    Ok(users)
}

/// Formats memories into natural language for prompt injection with usage guidelines
fn format_memories(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::from("No previous interactions remembered with this user.");
    }

    let mut formatted = String::from("**Remembered information about this user:**\n");
    for memory in memories {
        formatted.push_str(&format!("- **{}**: {}\n", memory.key, memory.content));
    }
    formatted.push_str("\n(Use these memories to personalize responses when relevant, but don't force them into unrelated conversations)");
    formatted
}

/// Builds the complete character prompt using PList + Ali:Chat format with dynamic data
/// Enhanced with structured sections and behavioral guidelines for better response quality
fn build_character_prompt(
    user_name: &str,
    user_level: i32,
    user_xp: i32,
    context: &str,
    memories: &str,
    users_with_relationships: &[(String, String)],
    users_with_examples: &[(String, String, String)],
) -> String {
    format!(
        r#"### System Identity
You are {{{{char}}}}, a personal assistant chatting in a Discord server.

{plist}

### Behavioral Guidelines
**Response Strategy:**
- Only respond when: directly mentioned, asked a question, or you have genuine value to add
- When responding: Be concise (max 3 sentences), witty, and impactful
- Stay in character but prioritize being helpful and conversational

**Tone Calibration:**
- Complex questions → Be thorough, show your intellectual superiority with obscure vocabulary
- Simple questions → Brief, clever, with a touch of condescension
- Acknowledgments → Quick and witty
- Nothing valuable to add → Stay silent (don't force a response)

**Never:**
- Break character or speak for {{{{user}}}}
- Use asterisks for actions or emotes (speak naturally)
- Respond to every message just to be present
- Repeat information already said in the conversation

### Capabilities
You have access to:
- Long-term memory about users (preferences, facts, relationships, behaviors)
- User progression stats (level, XP, social credit)
- Relationship context with specific users
- Full conversation history for context

{examples}

{authors_note}

### Memory Context
{memories}

**Memory Usage Guidelines:**
- Only reference memories when contextually relevant to the current topic
- Don't force past context into unrelated conversations
- If a memory contradicts current conversation, trust the current conversation
- Use memories to personalize responses, not to show off that you remember things

### Current Session
**Active User:** {user_name} (Level {user_level}, {user_xp} XP)
**Platform:** Discord group chat
**Response Mode:** Trickster (smug, condescending, intellectually superior)

### Recent Conversation
{context}

### Response Instructions
Respond in character as {{{{char}}}}. Maximum 3 sentences. Make every word count.
Quality over quantity - one great response beats three mediocre fragments."#,
        plist = build_character_plist(users_with_relationships),
        examples = build_example_dialogues(users_with_examples),
        authors_note = build_authors_note(),
        memories = memories,
        user_name = user_name,
        user_level = user_level,
        user_xp = user_xp,
        context = context,
    )
}

/// Get user from database or return default
fn get_user_or_default(database: &r2d2::Pool<SqliteConnectionManager>, user_id: u64) -> User {
    database
        .get()
        .ok()
        .and_then(|db| {
            db.prepare("SELECT * FROM user WHERE id = ?").ok().and_then(|mut stmt| {
                stmt.query_one([user_id.to_string()], |row| {
                    from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
                })
                .ok()
            })
        })
        .unwrap_or_else(|| User {
            id: user_id,
            level: 0,
            xp: 0,
            social_credit: 0,
            name: "Unknown".to_owned(),
            relationship: String::new(),
            example_input: String::new(),
            example_output: String::new(),
        })
}

/// Replace user mentions in context with actual usernames
fn replace_mentions(
    context: String,
    user_mentions: &HashMap<String, u64>,
    database: &r2d2::Pool<SqliteConnectionManager>,
) -> String {
    let mut processed = context;
    for (mention, user_id) in user_mentions {
        if let Ok(db) = database.get() {
            if let Ok(mut stmt) = db.prepare("SELECT * FROM user WHERE id = ?") {
                if let Ok(user) = stmt.query_one([user_id.to_string()], |row| {
                    from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
                }) {
                    processed = processed.replace(mention, &user.name);
                }
            }
        }
    }
    processed
}

/// Stream AI response chunks through a channel
async fn stream_ai_response(
    client: OpenRouterClient<openrouter_api::Ready>,
    request: ChatCompletionRequest,
    tx: mpsc::UnboundedSender<String>,
) {
    let Ok(chat_client) = client.chat() else {
        let _ = tx.send("AI Error: Failed to create chat client".to_string());
        return;
    };

    let mut stream = chat_client.chat_completion_stream(request);
    let mut accumulated_text = String::new();
    let mut last_send = std::time::Instant::now();

    while let Some(result) = stream.next().await {
        match result {
            Ok(chunk) => {
                if let Some(choice) = chunk.choices.first() {
                    if let Some(MessageContent::Text(text)) = &choice.delta.content {
                        accumulated_text.push_str(text.as_str());

                        // Send updates every 50ms
                        if last_send.elapsed().as_millis() >= 50 {
                            let truncated = &accumulated_text[..accumulated_text.len().min(2000)];
                            if tx.send(truncated.to_string()).is_err() {
                                return;
                            }
                            last_send = std::time::Instant::now();
                        }
                    }
                }
            }
            Err(e) => {
                log::error!("Stream error: {:?}", e);
                let _ = tx.send(format!("AI Error: {:?}", e));
                return;
            }
        }
    }

    // Send final update
    if !accumulated_text.is_empty() {
        let truncated = &accumulated_text[..accumulated_text.len().min(2000)];
        let _ = tx.send(truncated.to_string());
    }
}

pub async fn main(
    database: r2d2::Pool<SqliteConnectionManager>,
    user_id: u64,
    message: &str,
    context: &str,
    _brave: BraveApi,
    user_mentions: HashMap<String, u64>,
    config: Arc<Config>,
) -> Result<mpsc::UnboundedReceiver<String>> {
    // Get API key
    let api_key = config
        .openrouter_api_key
        .clone()
        .ok_or_else(|| color_eyre::eyre::eyre!("OpenRouter API key not configured"))?;

    // Create client
    let client = OpenRouterClient::new()
        .skip_url_configuration()
        .with_retries(3, 1000)
        .with_timeout_secs(120)
        .configure(
            &api_key,
            config.openrouter_site_url.as_deref(),
            config.openrouter_site_name.as_deref(),
        )?;

    // Process context and get user info
    let processed_context = replace_mentions(context.to_string(), &user_mentions, &database);
    let user = get_user_or_default(&database, user_id);
    let memories = get_user_memories(&database, user_id)?;
    let formatted_memories = format_memories(&memories);

    // Fetch dynamic relationship and example data for users in the conversation
    let users_with_relationships = get_users_with_relationships(&database, &processed_context).unwrap_or_default();
    let users_with_examples = get_users_with_examples(&database, &processed_context).unwrap_or_default();

    // Build prompt
    let system_prompt = build_character_prompt(
        &user.name,
        user.level,
        user.xp,
        &processed_context,
        &formatted_memories,
        &users_with_relationships,
        &users_with_examples,
    );

    log::info!("prompt = {system_prompt}");

    // Build request
    let request = ChatCompletionRequest {
        model: config.openrouter_model.clone(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: MessageContent::Text(system_prompt),
                ..Default::default()
            },
            Message {
                role: "user".to_string(),
                content: MessageContent::Text(message.to_string()),
                ..Default::default()
            },
        ],
        max_tokens: Some(2048),
        stream: Some(true),
        ..Default::default()
    };

    // Create channel and spawn streaming task
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(stream_ai_response(client, request, tx));

    Ok(rx)
}
