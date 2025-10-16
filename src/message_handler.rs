use rand::{
    prelude::{IteratorRandom, SliceRandom},
    seq::IndexedRandom,
    Rng,
};
use serde_rusqlite::from_row;
use std::{sync::Arc, time::Instant};
use tokio::sync::MutexGuard;
use twilight_http::Client as HttpClient;
use twilight_model::{gateway::payload::incoming::MessageCreate, id::Id};
use vesper::twilight_exports::UserMarker;

use crate::{
    ai_message,
    database::User,
    structs::{Command, List, State},
    utils::levels::xp_required_for_level,
    zalgos::zalgify_text,
    RESPONDERS,
};

pub async fn handle_message(
    msg: &MessageCreate,
    mut locked_state: MutexGuard<'_, State>,
    http: &Arc<HttpClient>,
) -> color_eyre::Result<Command> {
    if let Some(responder) = RESPONDERS.get(msg.content.to_uppercase().as_str()) {
        if let Some(msg) = &responder.message {
            return Ok(Command::text(msg));
        }
        if let Some(reaction) = &responder.react {
            return Ok(Command::react(reaction.chars().next().unwrap()));
        }
    }

    let user = {
        let db = locked_state.db.get()?;
        let mut statement = db.prepare("SELECT * FROM user WHERE id = ?").unwrap();
        statement
            .query_one([msg.author.id.get().to_string()], |row| {
                from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
            })
            .ok()
    };

    if let Some(mut user) = user {
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
        user.name = msg.author.name.clone();
        if new_xp >= xp_required {
            let new_level = level + 1;
            let _new_xp_required = xp_required_for_level(new_level);

            user.level = new_level;
            user.xp = 0;
            user.update_sync(&*locked_state.db.get()?)?;
            tokio::time::sleep(std::time::Duration::from_millis(locked_state.rng.gen_range(3000..8000))).await;
            return Ok(Command::text(format!(
                "Congrats <@{}>! You are now level {}!",
                msg.author.id.get(),
                new_level
            ))
            .reply()
            .mention());
        } else {
            user.xp = new_xp;
            user.update_sync(&*locked_state.db.get()?)?;
        }
    } else {
        let new_user = User {
            id: msg.author.id.get(),
            name: msg.author.name.clone(),
            level: 0,
            xp: 0,
        };
        new_user.insert_sync(&*locked_state.db.get()?)?;
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
            let name = msg.author.name.clone();

            let mut context = String::new();
            match locked_state.cache.channel_messages(msg.channel_id) {
                Some(v) => {
                    let msgs = v
                        .iter()
                        .take(25)
                        .filter_map(|m| {
                            let msg = locked_state.cache.message(m.to_owned());
                            msg.map(|msg| {
                                let content = msg.content();
                                let ai_content = content[..std::cmp::min(content.len(), 2400)]
                                    .replace(&locked_state.config.id.to_string(), "The Trickster");
                                match msg.author().get() {
                                    id if id == locked_state.config.id => format!("The Trickster: {ai_content}"),
                                    _ => {
                                        let username = locked_state
                                            .cache
                                            .user(msg.author())
                                            .map(|c| c.name.clone())
                                            .unwrap_or_default();
                                        format!("{}: {}\n", username, ai_content)
                                    }
                                }
                            })
                        })
                        .rev()
                        .collect::<Vec<String>>();
                    context.push_str(&msgs.join("\n"));
                }
                None => {
                    if let Some(msg) = &msg.referenced_message {
                        let user_name = msg.author.name.clone();
                        context.push_str(&format!(
                            "{}: {}\n",
                            user_name,
                            &msg.content[..std::cmp::min(msg.content.len(), 2400)]
                        ));
                    }
                }
            };

            println!("Context: {}", context);
            let mut user_mentions = std::collections::HashMap::new();
            // Extract user IDs from the cache for users that appear in context
            for line in context.lines() {
                if let Some(colon_pos) = line.find(':') {
                    let username = line[..colon_pos].trim();
                    if !username.is_empty() && username != "The Trickster" {
                        // Try to find the real user ID from cache
                        for user_ref in locked_state.cache.iter().users() {
                            if user_ref.name == username {
                                user_mentions.insert(username.to_string(), user_ref.id.get());
                                break;
                            }
                        }
                    }
                }
            }

            match ai_message::main(
                locked_state.db.clone(),
                msg.author.id.get(),
                &format!("{name}: {}", &content[..std::cmp::min(content.len(), 2400)]),
                &context,
                locked_state.brave_api.clone(),
                user_mentions,
            )
            .await
            {
                Ok(txt) => Ok(Command::text(txt).reply()),
                Err(e) => Ok(Command::text(format!("AI Error: {:?}", e)).reply()),
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
