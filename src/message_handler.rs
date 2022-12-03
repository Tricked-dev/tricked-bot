use argh::FromArgs;
use rand::{
    prelude::{IteratorRandom, SliceRandom},
    Rng,
};
use tokio::sync::MutexGuard;
use twilight_embed_builder::EmbedBuilder;
use twilight_http::Client as HttpClient;
use twilight_model::gateway::payload::incoming::MessageCreate;

use std::error::Error;
use std::{sync::Arc, time::Instant};

use crate::{
    config::Config,
    structs::{Command, Commands, InviteStats, List, State, TrickedCommands},
    zalgos::zalgify_text,
    RESPONDERS,
};

pub async fn handle_message(
    msg: &MessageCreate,
    mut locked_state: MutexGuard<'_, State>,
    config: &Arc<Config>,
    http: &Arc<HttpClient>,
    name: &'_ str,
) -> Result<Command, Box<dyn Error + Send + Sync>> {
    if let Some(responder) = RESPONDERS.get(msg.content.to_uppercase().as_str()) {
        if let Some(msg) = &responder.message {
            return Ok(Command::text(msg));
        }
        if let Some(reaction) = &responder.react {
            return Ok(Command::react(reaction.chars().next().unwrap()));
        }
    }
    if msg.content.to_lowercase().starts_with("l+") {
        let join = msg.content.split('+').skip(1).collect::<Vec<_>>().join("+");
        let args = join.trim().split(' ');
        let two = &args.collect::<Vec<&str>>()[..];
        let commands = TrickedCommands::from_args(&["L+"], two);
        if let Ok(command) = commands {
            return match command.nested {
                Commands::InviteStats(InviteStats {}) => {
                    let mut stmt = locked_state
                        .db
                        .prepare("select invite_used, count(invite_used) from users group by invite_used")
                        .unwrap();
                    let mut res = stmt
                        .query_map([], |row| {
                            let key: String = row.get(0)?;
                            let value: i32 = row.get(1)?;
                            Ok((key, value))
                        })?
                        .flatten()
                        .collect::<Vec<(String, i32)>>();

                    res.sort_by(|a, b| b.1.cmp(&a.1));

                    let data = res
                        .into_iter()
                        .map(|(k, v)| {
                            format!(
                                "{k} ({}): {v}",
                                config
                                    .invites
                                    .clone()
                                    .into_iter()
                                    .find_map(|(key, val)| if val == k { Some(key) } else { None })
                                    .unwrap_or_else(|| "None".to_owned())
                            )
                        })
                        .collect::<Vec<String>>();
                    let embed = EmbedBuilder::new().description(data.join("\n")).build()?;
                    Ok(Command::embed(embed))
                }
            };
        } else {
            return Ok(Command::text(format!("```\n{}\n```", commands.err().unwrap().output)));
        }
    }

    match msg.content.to_lowercase().as_str() {
        content
            if locked_state.last_redesc.elapsed() > std::time::Duration::from_secs(150)
                && config.rename_channels.clone().contains(&msg.channel_id.get())
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
        _x if locked_state.rng.gen_range(0..60) == 2 => {
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
                    config.shit_reddits.choose(&mut rand::thread_rng()).unwrap()
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
