use crate::{message_handler::handle_message, structs::*};

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
            tracing::info!("Message received {}", &msg.content.replace('\n', "\\ "));

            if msg.guild_id.is_none() || msg.author.bot {
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
