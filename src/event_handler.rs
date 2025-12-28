use crate::{ai_message, memory_creator, message_handler::handle_message, structs::*};

use rand::Rng;
use tokio::{join, sync::Mutex};
use twilight_gateway::Event;
use twilight_http::{request::channel::reaction::RequestReactionType, Client as HttpClient};
use twilight_model::{channel::message::AllowedMentions, id::Id};
use vesper::prelude::*;

use std::{collections::HashMap, sync::Arc, time::Duration};

/// Helper function to handle AI message processing with memory creation
async fn handle_ai_message(
    state: &Arc<Mutex<State>>,
    http: &Arc<HttpClient>,
    user_id: u64,
    channel_id: u64,
    message_id: u64,
    name: String,
    content: String,
    context: String,
    user_mentions: HashMap<String, u64>,
    should_create_memory: bool,
    tracking_id: u64, // Either channel_id or user_id for DMs
) -> color_eyre::Result<()> {
    let locked_state = state.lock().await;
    let user_mentions_clone = user_mentions.clone();

    match ai_message::main(
        locked_state.db.clone(),
        user_id,
        &format!("{}: {}", name, &content[..std::cmp::min(content.len(), 2400)]),
        &context,
        locked_state.brave_api.clone(),
        user_mentions,
        locked_state.config.clone(),
    )
    .await
    {
        Ok(stream_rx) => {
            drop(locked_state); // Release lock before spawning tasks

            tokio::spawn(crate::message_handler::handle_streaming_response(
                stream_rx,
                Id::new(channel_id),
                Id::new(message_id),
                Arc::clone(http),
            ));

            // Spawn background task to create memories if we've reached the threshold
            if should_create_memory {
                let state_clone = Arc::clone(state);
                tokio::spawn(async move {
                    memory_creator::create_memories_background(
                        state_clone.lock().await.db.clone(),
                        context,
                        user_mentions_clone,
                        state_clone.lock().await.config.clone(),
                    )
                    .await;

                    // Reset the message counter
                    state_clone.lock().await.channel_message_counts.insert(tracking_id, 0);
                });
            }
            Ok(())
        }
        Err(e) => {
            drop(locked_state); // Release lock before making HTTP call
            tracing::error!("AI Error: {:?}", e);
            http.create_message(Id::new(channel_id))
                .content(&format!("AI Error: {:?}", e))?
                .reply(Id::new(message_id))
                .exec()
                .await?;
            Ok(())
        }
    }
}

pub async fn handle_event(
    event: Event,
    http: &Arc<HttpClient>,
    state: &Arc<Mutex<State>>,
    framework: Arc<Framework<Arc<Mutex<State>>>>,
) -> color_eyre::Result<()> {
    let mut locked_state = state.lock().await;
    match event {
        Event::InteractionCreate(i) => {
            tracing::info!("Slash Command!");
            tokio::spawn(async move {
                let inner = i.0;
                framework.process(inner).await;
            });
        }
        Event::MessageCreate(msg) => {
            tracing::info!("Message received {}", &msg.content.replace('\n', "\\ "));

            if msg.author.bot {
                return Ok(());
            }

            // Check if this is a DM (no guild_id means it's a DM)
            let is_dm = msg.guild_id.is_none();

            if is_dm {
                // Handle DM with rate limiting
                if let Some(dm_limit_duration) = locked_state.dm_bucket.limit_duration(msg.author.id.get()) {
                    tracing::info!("DM rate limit reached for user {}, {} seconds remaining", msg.author.id.get(), dm_limit_duration.as_secs());

                    // Send a message to the user about the rate limit
                    let _ = http
                        .create_message(msg.channel_id)
                        .content(&format!("You've reached the DM rate limit. Please wait {} seconds before sending more messages.", dm_limit_duration.as_secs()))?
                        .reply(msg.id)
                        .exec()
                        .await;
                    return Ok(());
                }

                // Track message count for memory creation using the DM user ID as key
                let count = locked_state.channel_message_counts.entry(msg.author.id.get()).or_insert(0);
                *count += 1;

                // Check if we should create memories (every 15 messages)
                let should_create_memory = *count >= 15;

                // Handle the DM message with AI
                if locked_state.config.openrouter_api_key.is_some() {
                    let name = msg.author.name.clone();
                    let content = msg.content.clone();
                    let user_id = msg.author.id.get();
                    let channel_id = msg.channel_id.get();
                    let message_id = msg.id.get();
                    let bot_id = locked_state.config.id;

                    // Build context from recent DM messages (up to 15 messages)
                    let mut context = String::new();
                    match locked_state.cache.channel_messages(msg.channel_id) {
                        Some(v) => {
                            let msgs = v
                                .iter()
                                .take(15)
                                .filter_map(|m| {
                                    let message = locked_state.cache.message(m.to_owned());
                                    message.map(|msg| {
                                        let content = msg.content();
                                        let ai_content = content[..std::cmp::min(content.len(), 2400)]
                                            .replace(&bot_id.to_string(), "The Trickster");
                                        match msg.author().get() {
                                            id if id == bot_id => format!("The Trickster: {ai_content}"),
                                            _ => {
                                                let username = locked_state
                                                    .cache
                                                    .user(msg.author())
                                                    .map(|c| c.name.clone())
                                                    .unwrap_or_default();
                                                format!("{}: {}", username, ai_content)
                                            }
                                        }
                                    })
                                })
                                .rev()
                                .collect::<Vec<String>>();
                            context.push_str(&msgs.join("\n"));
                        }
                        None => {
                            // Fallback to just current message if cache is empty
                            context.push_str(&format!("{}: {}", name, &content[..std::cmp::min(content.len(), 2400)]));
                        }
                    }

                    let user_mentions = HashMap::new();

                    drop(locked_state); // Release lock before calling helper

                    handle_ai_message(
                        state,
                        http,
                        user_id,
                        channel_id,
                        message_id,
                        name,
                        content,
                        context,
                        user_mentions,
                        should_create_memory,
                        user_id, // Use user_id as tracking_id for DMs
                    )
                    .await?;
                }
                return Ok(());
            }

            // Original guild message handling continues below
            if msg.guild_id.is_none() {
                return Ok(());
            }

            // Increment message count for this channel
            let count = locked_state.channel_message_counts.entry(msg.channel_id.get()).or_insert(0);
            *count += 1;

            if let Some(today_i) = locked_state.config.today_i_channel {
                if msg.channel_id == Id::new(today_i) && !msg.content.clone().to_lowercase().starts_with("today i") {
                    http.delete_message(msg.channel_id, msg.id).exec().await?;
                    return Ok(());
                }
            }

            if let Some(channel_limit_duration) = locked_state.channel_bucket.limit_duration(msg.channel_id.get()) {
                tracing::info!("Channel limit reached {}", channel_limit_duration.as_secs());
                return Ok(());
            }
            if let Some(user_limit_duration) = locked_state.user_bucket.limit_duration(msg.author.id.get()) {
                tracing::info!("User limit reached {}", user_limit_duration.as_secs());
                if Duration::from_secs(5) > user_limit_duration {
                    tokio::time::sleep(user_limit_duration).await;
                } else {
                    return Ok(());
                }
            };

            let r = handle_message(&msg, locked_state, http).await;
            match r {
                Ok(res) => {
                    let Command {
                        embeds,
                        text,
                        reaction,
                        attachments,
                        reply,
                        skip,
                        mention,
                    } = res;
                    if skip {
                        return Ok(());
                    } else if let Some(reaction) = reaction {
                        http.create_reaction(
                            msg.channel_id,
                            msg.id,
                            &RequestReactionType::Unicode {
                                name: &reaction.to_string(),
                            },
                        )
                        .exec()
                        .await?;
                    } else {
                        let mut req = http
                            .create_message(msg.channel_id)
                            .embeds(&embeds)?
                            .attachments(&attachments)?;
                        if let Some(text) = &text {
                            req = req.content(text)?;
                        }

                        if reply {
                            req = req.reply(msg.id);
                        }
                        let mentions = AllowedMentions {
                            users: vec![msg.author.id],
                            ..Default::default()
                        };
                        if mention {
                            req = req.allowed_mentions(Some(&mentions));
                        }

                        req.exec().await?;
                    }
                }
                Err(e) => {
                    tracing::error!("Error handling message: {:?}", e);
                }
            }
        }
        Event::Ready(_) => {
            tracing::info!("Connected");
        }
        Event::TypingStart(event) => {
            if rand::thread_rng().gen_range(0..100) != 1 {
                return Ok(());
            }
            if event.user_id.get() == locked_state.last_typer {
                return Ok(());
            }
            if let Some(mem) = event.member {
                let (msg, _) = join!(
                    http.create_message(event.channel_id)
                        .content(&format!("{} is typing", mem.user.name))?
                        .exec(),
                    async {
                        if let Some(id) = locked_state.del.get(&event.channel_id) {
                            let _ = http
                                .delete_message(event.channel_id, Id::new(id.to_owned()))
                                .exec()
                                .await;
                        }
                    },
                );
                let res = msg?.model().await?;
                locked_state.del.insert(event.channel_id, res.id.get());
                locked_state.last_typer = event.user_id.get();
            }
        }
        _ => {}
    }
    Ok(())
}
