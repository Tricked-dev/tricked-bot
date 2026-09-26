use crate::{
    message_handler::{handle_message, is_direct, snapshot_ai_request},
    structs::*,
};

use rand::Rng;
use tokio::{join, sync::Mutex};
use twilight_gateway::Event;
use twilight_http::{request::channel::reaction::RequestReactionType, Client as HttpClient};
use twilight_model::{channel::message::AllowedMentions, id::Id};
use vesper::prelude::*;

use std::{sync::Arc, time::Duration};

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
            tracing::info!(target: "messages", channel = msg.channel_id.get(), msg_id = msg.id.get(), author = %msg.author.name, bot = msg.author.bot, "Message received {}", msg.content.replace('\n', "\\ "));

            if msg.author.bot {
                return Ok(());
            }

            // Check if this is a DM (no guild_id means it's a DM)
            let is_dm = msg.guild_id.is_none();

            if is_dm {
                // Handle DM with rate limiting
                if let Some(dm_limit_duration) = locked_state.dm_bucket.limit_duration(msg.author.id.get()) {
                    tracing::info!(
                        "DM rate limit reached for user {}, {} seconds remaining",
                        msg.author.id.get(),
                        dm_limit_duration.as_secs()
                    );

                    // Send a message to the user about the rate limit
                    let _ = http
                        .create_message(msg.channel_id)
                        .content(&format!(
                            "You've reached the DM rate limit. Please wait {} seconds before sending more messages.",
                            dm_limit_duration.as_secs()
                        ))?
                        .reply(msg.id)
                        .exec()
                        .await;
                    return Ok(());
                }

                let req = snapshot_ai_request(&locked_state, &msg);
                let enabled = locked_state.config.openrouter_api_key.is_some();
                drop(locked_state);
                if enabled {
                    crate::ai_reply::spawn_ai_reply(req, Arc::clone(http));
                } else {
                    crate::message_check::spawn(Arc::clone(state), Arc::clone(http), req, false);
                }
                return Ok(());
            }

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

            let direct = is_direct(&msg, locked_state.config.id);
            if !direct {
                locked_state.reply_gate.count(msg.channel_id.get());
                tracing::info!(target: "decider", kind = "count", channel = msg.channel_id.get(), msg_id = msg.id.get());
            }
            let req = snapshot_ai_request(&locked_state, &msg);
            let ask_reply = !direct
                && locked_state.config.openrouter_api_key.is_some()
                && locked_state.config.decider_reply_mode != crate::config::ReplyMode::Off;
            let mut ai_started = false;
            let r = handle_message(&msg, locked_state, http, &mut ai_started).await;
            // Dispatch before sending a responder result, so even failed sends retain the durable check.
            if !ai_started {
                let answered = r.as_ref().map(|res| !res.skip).unwrap_or(true);
                crate::message_check::spawn(Arc::clone(state), Arc::clone(http), req, ask_reply && !answered);
            }
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
                    } else if text.is_some() || !embeds.is_empty() || !attachments.is_empty() {
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
