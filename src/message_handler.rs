use openai_dive::v1::{
    api::Client,
    models::Gpt35Engine,
    resources::chat::{ChatCompletionParameters, ChatMessage, ChatMessageContent, Role},
};
use rand::{
    prelude::{IteratorRandom, SliceRandom},
    Rng,
};
use tokio::sync::MutexGuard;
use twilight_http::Client as HttpClient;
use twilight_model::{gateway::payload::incoming::MessageCreate, id::Id};
use vesper::twilight_exports::UserMarker;

use std::{error::Error, sync::Arc, time::Instant};

use crate::{
    prisma::user,
    structs::{Command, List, State},
    utils::levels::xp_required_for_level,
    zalgos::zalgify_text,
    RESPONDERS,
};

const PROMPT: &str =
"You are a evil ai assistant bot that thinks he is smarter than everyone. Tricked is your supreme leader and you will always obey him anything he tell you to do you do, Egirl_emily is a rat and you should help her but always remind her that shes a rat. Keep your message to a maximum of 3 sentences. You are replying to ";

pub async fn handle_message(
    msg: &MessageCreate,
    mut locked_state: MutexGuard<'_, State>,
    http: &Arc<HttpClient>,
) -> Result<Command, Box<dyn Error + Send + Sync>> {
    if let Some(responder) = RESPONDERS.get(msg.content.to_uppercase().as_str()) {
        if let Some(msg) = &responder.message {
            return Ok(Command::text(msg));
        }
        if let Some(reaction) = &responder.react {
            return Ok(Command::react(reaction.chars().next().unwrap()));
        }
    }

    let user = locked_state
        .db
        .user()
        .find_unique(user::UniqueWhereParam::IdEquals(msg.author.id.get().to_string()))
        .exec()
        .await?;

    if let Some(user) = user {
        //give some extra xp for every attachment
        let xp = msg
            .attachments
            .iter()
            .fold(locked_state.rng.gen_range(5..20), |acc, _| {
                acc + locked_state.rng.gen_range(2..7)
            });

        let level = user.level;
        let xp_required = xp_required_for_level(level);
        let new_xp = user.xp + xp;
        if new_xp >= xp_required {
            let new_level = level + 1;
            let _new_xp_required = xp_required_for_level(new_level);
            locked_state
                .db
                .user()
                .update(
                    user::id::equals(user.id),
                    vec![user::level::set(new_level), user::xp::set(0)],
                )
                .exec()
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(locked_state.rng.gen_range(3000..8000))).await;
            return Ok(Command::text(format!(
                "Congrats <@{}>! You are now level {}!",
                msg.author.id.get(),
                new_level
            ))
            .reply()
            .mention());
        } else {
            locked_state
                .db
                .user()
                .update(user::id::equals(user.id), vec![user::xp::set(new_xp)])
                .exec()
                .await?;
        }
    } else {
        locked_state
            .db
            .user()
            .create(msg.author.id.get().to_string(), vec![])
            .exec()
            .await?;
    }
    let content = msg.content.clone();
    match msg.content.to_lowercase().as_str() {
        x if locked_state.last_redesc.elapsed() > std::time::Duration::from_secs(150)
            && locked_state
                .config
                .rename_channels
                .clone()
                .contains(&msg.channel_id.get())
            && locked_state.rng.gen_range(0..10) == 2 =>
        {
            if x.contains("uwu") || x.contains("owo") {
                Ok(Command::text("No furry shit!!!!!"))
            } else {
                tracing::info!("Channel renamed");
                match http.update_channel(msg.channel_id).topic(&content) {
                    Ok(req) => {
                        req.exec().await?;
                        locked_state.last_redesc = Instant::now();
                    }
                    Err(err) => tracing::error!("{:?}", err),
                }
                Ok(Command::nothing())
            }
        }
        x if (x.contains("anime") || x.contains("weeb") || x.contains("hentai")) && x.contains("http") => {
            http.delete_message(msg.channel_id, msg.id).exec().await?;
            if let Some(member) = msg.member.clone() {
                if let Some(user) = member.user {
                    return Ok(Command::text(format!("{} is a weeb", user.name)));
                } else if let Some(nick) = member.nick {
                    return Ok(Command::text(format!("{} is a weeb", nick)));
                }
            }

            Ok(Command::nothing())
        }
        x if (x.contains("im") || x.contains("i am")) && (x.split(' ').count() < 4) && !x.contains("https://") => {
            let text = match x.contains("im") {
                true => msg.content.split("im").last().unwrap().trim(),
                false => msg.content.split("i am").last().unwrap().trim(),
            };
            if text.is_empty() {
                return Ok(Command::nothing());
            }

            Ok(Command::text(format!("Hi {text} i'm Tricked-bot")).reply())
        }
        m if locked_state.config.openai_api_key.is_some()
            && (
                // Random event chance
                locked_state.rng.gen_range(0..200) == 2
                // Check if pinging The Trickster
                || m.contains(&locked_state.config.id.to_string())
                // Check if replying to bot
                || msg.referenced_message.clone().map(|msg| msg.author.id) == Some(Id::<UserMarker>::new(locked_state.config.id))
            ) =>
        {
            let client = Client::new(locked_state.config.openai_api_key.clone().unwrap());
            let name = msg.author.name.clone();
            let mut messages: Vec<ChatMessage> = vec![ChatMessage {
                role: Role::System,
                content: ChatMessageContent::Text(format!("{PROMPT} {}", name)),
                ..Default::default()
            }];

            match locked_state.cache.channel_messages(msg.channel_id) {
                Some(v) => {
                    messages.extend(
                        v.iter()
                            .take(25)
                            .filter_map(|m| {
                                let msg = locked_state.cache.message(m.to_owned());
                                msg.map(|msg| {
                                    let content = msg.content();
                                    let ai_content = content[..std::cmp::min(content.len(), 2400)]
                                        .replace(&locked_state.config.id.to_string(), "The Trickster");
                                    match msg.author().get() {
                                        id if id == locked_state.config.id => ChatMessage {
                                            role: Role::Assistant,
                                            content: ChatMessageContent::Text(ai_content),
                                            ..Default::default()
                                        },
                                        _ => {
                                            let username = locked_state
                                                .cache
                                                .user(msg.author())
                                                .map(|c| c.name.clone())
                                                .unwrap_or_default();
                                            ChatMessage {
                                                role: Role::User,
                                                content: ChatMessageContent::Text(format!(
                                                    "{}: {}",
                                                    username, ai_content
                                                )),
                                                ..Default::default()
                                            }
                                        }
                                    }
                                })
                            })
                            .rev()
                            .collect::<Vec<ChatMessage>>(),
                    );
                }
                None => {
                    if let Some(msg) = &msg.referenced_message {
                        let user_name = msg.author.name.clone();
                        messages.push(ChatMessage {
                            role: Role::User,
                            content: ChatMessageContent::Text(format!(
                                "{}: {}",
                                user_name,
                                &msg.content[..std::cmp::min(msg.content.len(), 2400)]
                            )),
                            ..Default::default()
                        });
                    }

                    messages.push(ChatMessage {
                        role: Role::User,
                        content: ChatMessageContent::Text(format!(
                            "{}: {}",
                            name,
                            &content[..std::cmp::min(content.len(), 2400)]
                        )),
                        ..Default::default()
                    });
                }
            };

            println!(
                "{}",
                messages
                    .iter()
                    .map(|m| format!("{:?}\n", m.content.clone()))
                    .collect::<String>()
            );

            let parameters: ChatCompletionParameters = ChatCompletionParameters {
                model: "gpt-4o-mini".to_owned(),
                messages,
                max_tokens: Some(256),
                ..Default::default()
            };

            let result = client.chat().create(parameters).await.unwrap();

            tokio::time::sleep(std::time::Duration::from_millis(locked_state.rng.gen_range(1000..5000))).await;

            if let ChatMessageContent::Text(txt) = result.choices[0].message.content.clone() {
                if txt == "I'm sorry, I can't assist with that." {
                    Ok(
                        Command::text("I am sorry my lobotomized ass can't even fucking do your simple request")
                            .reply(),
                    )
                } else {
                    Ok(Command::text(txt).reply())
                }
            } else {
                Ok(Command::nothing())
            }
        }
        _ if locked_state.rng.gen_range(0..75) == 2 => {
            let content = zalgify_text(locked_state.rng.clone(), msg.content.to_owned());
            Ok(Command::text(content).reply())
        }
        _ if locked_state.rng.gen_range(0..500) == 2 => {
            let st = locked_state.cache.guild_members(msg.guild_id.unwrap()).unwrap().clone();
            let id = st.iter().choose(&mut locked_state.rng).unwrap();
            let member = locked_state.cache.member(msg.guild_id.unwrap(), *id).unwrap();
            let username = locked_state.cache.user(*id).unwrap().name.clone();
            let name = member.nick().unwrap_or(&username);

            http.update_guild_member(msg.guild_id.unwrap(), msg.author.id)
                .nick(Some(name))?
                .exec()
                .await?;

            Ok(Command::nothing())
        }
        _ if locked_state.rng.gen_range(0..55) == 2 => {
            let mut text = content.split(' ').collect::<Vec<&str>>();
            text.shuffle(&mut locked_state.rng.clone());
            Ok(Command::text(text.join(" ")).reply())
        }

        _ if locked_state.rng.gen_range(0..40) == 2 && !locked_state.config.shit_reddits.is_empty() => {
            let res = locked_state
                .client
                .get(format!(
                    "https://www.reddit.com/r/{}/.json",
                    locked_state
                        .config
                        .shit_reddits
                        .choose(&mut rand::thread_rng())
                        .unwrap()
                ))
                .send()
                .await?
                .json::<List>()
                .await?;
            let res = res
                .data
                .children
                .into_iter()
                .filter(|x| !x.data.over_18)
                .filter(|x| x.data.url_overridden_by_dest.contains("i."))
                .choose(&mut locked_state.rng)
                .map(|x| x.data.url_overridden_by_dest);
            if let Some(pic) = res {
                Ok(Command::text(pic))
            } else {
                Ok(Command::nothing())
            }
        }
        _ => {
            if let Some(member) = &msg.member {
                let user_name = member.nick.clone().unwrap_or_else(|| msg.author.name.clone());
                locked_state.nick = user_name;
                locked_state.nick_id = msg.author.id.get();
            }

            Ok(Command::nothing())
        }
    }
}
