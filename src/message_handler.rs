use rand::{
    prelude::{IteratorRandom, SliceRandom},
    Rng,
};
use tokio::sync::MutexGuard;
use twilight_http::Client as HttpClient;
use twilight_model::gateway::payload::incoming::MessageCreate;

use std::error::Error;
use std::{sync::Arc, time::Instant};

use crate::{
    structs::{Command, List, State},
    zalgos::zalgify_text,
    RESPONDERS,
};

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

    match msg.content.to_lowercase().as_str() {
        content
            if locked_state.last_redesc.elapsed() > std::time::Duration::from_secs(150)
                && locked_state
                    .config
                    .rename_channels
                    .clone()
                    .contains(&msg.channel_id.get())
                && locked_state.rng.gen_range(0..10) == 2 =>
        {
            if content.to_lowercase().contains("uwu") || content.to_lowercase().contains("owo") {
                Ok(Command::text("No furry shit!!!!!"))
            } else {
                tracing::info!("Channel renamed");
                match http.update_channel(msg.channel_id).topic(content) {
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
        x if x.contains("im") && (x.split(' ').count() < 4) => {
            let text = msg.content.split("im").last().unwrap().trim();
            if text.is_empty() {
                return Ok(Command::nothing());
            }

            Ok(Command::text(format!("Hi {text} i'm Tricked-bot")).reply())
        }
        _x if locked_state.rng.gen_range(0..75) == 2 => {
            let content = zalgify_text(locked_state.rng.clone(), msg.content.to_owned());
            Ok(Command::text(content).reply())
        }
        _x if locked_state.rng.gen_range(0..500) == 2 => {
            let st = locked_state.cache.guild_members(msg.guild_id.unwrap()).unwrap().clone();
            let id = st.iter().choose(&mut rand::thread_rng()).unwrap();
            let member = locked_state.cache.member(msg.guild_id.unwrap(), *id).unwrap();
            let username = locked_state.cache.user(*id).unwrap().name.clone();
            let name = member.nick().unwrap_or(&username);

            http.update_guild_member(msg.guild_id.unwrap(), msg.author.id)
                .nick(Some(name))?
                .exec()
                .await?;

            Ok(Command::nothing())
        }
        _x if locked_state.rng.gen_range(0..55) == 2 => {
            let mut text = msg.content.split(' ').collect::<Vec<&str>>();
            text.shuffle(&mut locked_state.rng.clone());
            Ok(Command::text(text.join(" ")).reply())
        }
        _ if locked_state.rng.gen_range(0..40) == 2 => {
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
                .await?
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
