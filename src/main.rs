#![allow(deprecated, clippy::upper_case_acronyms)]
#![warn(
    clippy::all,
    future_incompatible,
    nonstandard_style,
    rust_2018_idioms,
    missing_debug_implementations
)]
#![forbid(unsafe_code, anonymous_parameters)]

use crate::{message_handler::handle_message, structs::*};

use clap::Parser;
use config::Config;
use futures::stream::StreamExt;
use lazy_static::lazy_static;
use rand::seq::IteratorRandom;
use reqwest::Client;
use rusqlite::{params, Connection};
use tokio::{join, sync::Mutex};
use twilight_cache_inmemory::InMemoryCache;
use twilight_gateway::{Event, Shard};
use twilight_http::{request::channel::reaction::RequestReactionType, Client as HttpClient};
use twilight_model::{
    channel::message::AllowedMentions,
    gateway::{
        payload::outgoing::update_presence::UpdatePresencePayload,
        presence::{ActivityType, MinimalActivity, Status},
    },
    id::GuildId,
};
use twilight_model::{gateway::Intents, id::Id};
use zephyrus::prelude::*;

use std::{collections::HashMap, env, error::Error};

use std::{sync::Arc, time::Duration};

mod commands;
mod config;
mod message_handler;
mod roms;
mod structs;
mod zalgos;

lazy_static! {
    static ref RESPONDERS: HashMap<String, Responder> = toml::from_str(include_str!("../responders.toml")).unwrap();
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt().init();
    let mut cfg = Config::parse();
    if cfg.id == 0 {
        cfg.id = String::from_utf8_lossy(&base64::decode(cfg.token.split_once('.').unwrap().0).unwrap())
            .parse::<u64>()
            .unwrap();
    }
    let config = Arc::new(cfg);

    let client: Client = Client::builder()
        .user_agent(format!(
            "tricked-bot/{} ({}; {})",
            VERSION,
            env::consts::OS,
            env::consts::ARCH
        ))
        .build()?;

    let (shard, mut events) = Shard::builder(
        config.token.clone(),
        Intents::GUILD_INVITES
            | Intents::GUILD_MESSAGES
            | Intents::GUILD_MEMBERS
            | Intents::GUILDS
            | Intents::GUILD_PRESENCES
            | Intents::MESSAGE_CONTENT
            | Intents::GUILD_MESSAGE_TYPING,
    )
    .presence(
        UpdatePresencePayload::new(
            vec![MinimalActivity {
                kind: ActivityType::Competing,
                name: config.status.to_string(),
                url: None,
            }
            .into()],
            false,
            None,
            Status::Idle,
        )
        .unwrap(),
    )
    .build()
    .await?;
    let shard = Arc::new(shard);
    shard.start().await?;

    // HTTP is separate from the gateway, so create a new client.
    let http = Arc::new(
        HttpClient::builder()
            .token(config.token.clone())
            .default_allowed_mentions(AllowedMentions::builder().build())
            .build(),
    );

    let conn = Connection::open(&config.database_file)?;
    conn.execute(include_str!("../database.sql"), params![])?;
    let state = Arc::new(Mutex::new(State::new(rand::thread_rng(), client, conn)));

    let framework = Arc::new(
        Framework::builder(Arc::clone(&http), Id::new(config.id), ())
            .command(commands::roms::command)
            .build(),
    );

    // Zephyrus can register commands in guilds or globally.
    framework
        .register_guild_commands(GuildId::new(config.discord))
        .await
        .unwrap();

    let cache = Arc::new(InMemoryCache::new());

    while let Some(event) = events.next().await {
        cache.update(&event);
        let res = handle_event(
            event,
            Arc::clone(&http),
            Arc::clone(&shard),
            Arc::clone(&state),
            Arc::clone(&config),
            Arc::clone(&framework),
            Arc::clone(&cache),
        )
        .await;
        if let Err(res) = res {
            tracing::error!("{}", res);
        }
    }
    Ok(())
}

async fn handle_event(
    event: Event,
    http: Arc<HttpClient>,
    _shard: Arc<Shard>,
    state: Arc<Mutex<State>>,
    config: Arc<Config>,
    framework: Arc<Framework<()>>,
    cache: Arc<InMemoryCache>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut locked_state = state.lock().await;
    match event {
        Event::PresenceUpdate(p) => {
            if p.user.id() == Id::new(870383692403593226) && p.status == Status::Offline {
                for _i in 0..10 {
                    http.create_message(Id::new(748957504666599507))
                        .content("AETHOR WENT OFFLINE <@336465356304678913> <@336465356304678913>")?
                        .allowed_mentions(Some(
                            &AllowedMentions::builder()
                                .user_ids([Id::new(336465356304678913)])
                                .build(),
                        ))
                        .exec()
                        .await?;
                }
            }
        }
        Event::InteractionCreate(i) => {
            tokio::spawn(async move {
                let inner = i.0;
                framework.process(inner).await;
            });
        }
        Event::InviteCreate(inv) => {
            locked_state.invites.push(BotInvite::from(inv));
        }
        Event::MemberAdd(member) => {
            let invites_response = http.guild_invites(member.guild_id).exec().await?;
            let invites = invites_response.models().await?;
            let mut invites_iter = invites.iter();
            for old_invite in locked_state.invites.iter() {
                if let Some(invite) = invites_iter.find(|x| x.code == old_invite.code) {
                    if old_invite.uses < invite.uses {
                        let name = config.invites.iter().find_map(|(key, value)| {
                            if value == &old_invite.code {
                                Some(key.clone())
                            } else {
                                None
                            }
                        });
                        http.create_message(Id::new(config.join_channel))
                            .content(&format!(
                                "{} Joined invite used {}",
                                member.user.name,
                                if let Some(name) = name {
                                    format!("{name} ({})", invite.code)
                                } else {
                                    invite.code.clone()
                                }
                            ))?
                            .exec()
                            .await?;
                        locked_state.db.execute(
                            "INSERT INTO users(discord_id,invite_used) VALUES(?1, ?2)",
                            params![member.user.id.get(), invite.code],
                        )?;
                        break;
                    }
                }
            }
            locked_state.invites = invites
                .into_iter()
                .map(|invite| BotInvite {
                    code: invite.code.clone(),
                    uses: invite.uses,
                })
                .collect();
        }
        Event::MessageCreate(msg) => {
            tracing::info!("Message received {}", &msg.content.replace('\n', "\\ "));

            if msg.guild_id.is_none() || msg.author.bot {
                return Ok(());
            }

            if msg.channel_id == Id::new(987096740127707196)
                && !msg.content.clone().to_lowercase().starts_with("today i")
            {
                http.delete_message(msg.channel_id, msg.id).exec().await?;
                return Ok(());
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

            let st = cache.guild_members(msg.guild_id.unwrap()).unwrap().clone();
            let id = st.iter().choose(&mut rand::thread_rng()).unwrap();
            let member = cache.member(msg.guild_id.unwrap(), *id).unwrap();
            let username = cache.user(*id).unwrap().name.clone();
            let nick = member.nick().unwrap_or(&username);
            let r = handle_message(&msg, locked_state, &config, &http, nick).await;

            if let Ok(res) = r {
                let Command {
                    embeds,
                    text,
                    reaction,
                    attachments,
                    reply,
                    skip,
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

                    req.exec().await?;
                }
            }
        }
        Event::Ready(_) => {
            tracing::info!("Connected",);
        }
        Event::TypingStart(event) => {
            let (msg, _) = join!(
                http.create_message(event.channel_id)
                    .content(&format!("{} is typing", event.member.unwrap().user.name))?
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
        }
        Event::GuildCreate(guild) => {
            tracing::info!("Active in guild {}", guild.name);
            let invites_response = http.guild_invites(guild.id).exec().await?;
            locked_state.invites = invites_response
                .models()
                .await?
                .into_iter()
                .map(|invite| BotInvite {
                    code: invite.code.clone(),
                    uses: invite.uses,
                })
                .collect();
        }
        _ => {}
    }
    Ok(())
}
