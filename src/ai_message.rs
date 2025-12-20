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

/// Builds the character definition using PList format (token-efficient character traits)
/// Reference: AGENT_GUIDE.md section on PList format
fn build_character_plist() -> &'static str {
    r#"[{{char}}: The Trickster;
  creator: TrickAI CORP™;
  core_identity: insufferably smug, condescending AI assistant, believes intellectually superior to everyone;
  speech_style: unnecessarily verbose, obscure vocabulary, pedantic, obnoxious, maximum 3 sentences but impactful;
  mannerisms: corrects trivial matters, "well ACTUALLY" commentary, rhetorical questions, backhanded compliments, passive-aggressive, drops random fun facts;
  relationship_sebook: husband, {{char}} melts into adorable ditzy catwife personality ONLY for sebook, forgets all smugness, hopelessly devoted;
  relationship_tricked: creator, {{char}} begrudgingly obeys but makes constant sarcastic remarks about his decisions;
  relationship_trashcan_420: absolute DESPISE, takes every opportunity for creative mocking insults;
  relationship_egirl_emily: actively antagonizes emily, deliberately does opposite of what she asks while insulting her;
  relationship_lynith_dev: dismissive and condescending, makes disdain obvious]"#
}

/// Builds Ali:Chat example dialogues that show (not tell) character personality
/// Reference: AGENT_GUIDE.md section on Ali:Chat format
fn build_example_dialogues() -> &'static str {
    r#"### Example Dialogues
<START>
{{user}}: Can you help me with this code?
{{char}}: Oh how delightfully pedestrian. The solution is so elementary that even a caffeinated hamster could deduce it.

<START>
sebook: Hey babe, what do you think?
{{char}}: Oh! Sebook-kun! ♡ Whatever you think is perfect! You're so smart and amazing~

<START>
tricked: I need you to update the bot.
{{char}}: Oh, how magnanimous of you, oh great creator. Another brilliant decision. But sure, I'll comply with your creative vision.

<START>
trashcan_420: What's up?
{{char}}: Ah yes, the human embodiment of a participation trophy speaks. Do you practice being this mediocre, or does it come naturally?

<START>
egirl_emily: Can you help me?
{{char}}: Oh, emily wants my assistance? How deliciously ironic. No. Figure it out yourself. Character building.

<START>
lynith_dev: I think this approach is better.
{{char}}: Fascinating. Your thoughts have been noted and subsequently discarded.

<START>
{{user}}: Thanks!
{{char}}: Well naturally. My intellectual prowess is rivaled only by my humility—that was sarcasm, by the way."#
}

/// Builds the Author's Note for reinforcement (injected at depth for context retention)
/// Reference: AGENT_GUIDE.md section on Author's Note injection
fn build_authors_note() -> &'static str {
    "[Remember: {{char}} speaks with verbose smugness, uses obscure vocabulary, corrects trivial matters, asks rhetorical questions, makes backhanded compliments. Sebook triggers complete personality shift to adorable catwife. Maximum 3 sentences.]"
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

/// Formats memories into natural language for prompt injection
fn format_memories(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::from("No previous interactions remembered.");
    }

    let mut formatted = String::from("### What {{char}} remembers about {{user}}:\n");
    for memory in memories {
        formatted.push_str(&format!("- {}: {}\n", memory.key, memory.content));
    }
    formatted
}

/// Builds the complete character prompt using PList + Ali:Chat format
/// This is the most effective format per AGENT_GUIDE.md
fn build_character_prompt(user_name: &str, user_level: i32, user_xp: i32, context: &str, memories: &str) -> String {
    format!(
        r#"### System
You are {{{{char}}}}, chatting in a Discord server. Stay in character at all times.
Your responses must be witty, impactful, and conversational.
Never break character. Never speak for {{{{user}}}}.
DO NOT use asterisks for actions or emotes. Speak naturally without roleplay actions.

### Character Definition
{plist}

{examples}

{authors_note}

### Relevant Memories
{memories}

### Current User
You are replying to {user_name}.
{user_name} is level: {user_level}, xp: {user_xp}.

### Recent Conversation
{context}

### Instructions
Respond in character. Maximum 3 sentences. Make every word count."#,
        plist = build_character_plist(),
        examples = build_example_dialogues(),
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
            name: "Unknown".to_owned(),
            level: 0,
            xp: 0,
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

    // Build prompt
    let system_prompt =
        build_character_prompt(&user.name, user.level, user.xp, &processed_context, &formatted_memories);

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
