#![allow(deprecated, clippy::upper_case_acronyms)]
#![warn(
    clippy::all,
    future_incompatible,
    nonstandard_style,
    rust_2018_idioms,
    missing_debug_implementations
)]
#![forbid(unsafe_code, anonymous_parameters)]

use crate::message_handler::handle_message;
use crate::structs::*;

use chrono::Utc;
use feed_rs::parser;
use futures::stream::StreamExt;
use lazy_static::lazy_static;
use log::error;
use qrcodegen::QrCode;
use rand::seq::IteratorRandom;
use reqwest::Client;
use roms::{codename, format_device, search};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::{join, time};
use twilight_cache_inmemory::InMemoryCache;
use twilight_embed_builder::EmbedBuilder;
use twilight_gateway::{Event, Shard};
use twilight_http::{request::channel::reaction::RequestReactionType, Client as HttpClient};
use twilight_model::channel::embed::{EmbedAuthor, EmbedFooter};
use twilight_model::channel::message::AllowedMentions;
use twilight_model::gateway::payload::outgoing::update_presence::UpdatePresencePayload;
use twilight_model::gateway::presence::{ActivityType, MinimalActivity, Status};
use twilight_model::id::GuildId;
use twilight_model::util::Timestamp;
use twilight_model::{gateway::Intents, id::Id};
use zephyrus::prelude::*;
use zephyrus::twilight_exports::{
    CommandOptionChoice, InteractionResponse, InteractionResponseData, InteractionResponseType,
};

mod message_handler;
mod roms;
mod structs;
mod zalgos;

#[cfg(test)]
pub mod tests;

lazy_static! {
    static ref RESPONDERS: HashMap<String, Responder> =
        toml::from_str(include_str!("../responders.toml")).unwrap();
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt().init();
    let config = Arc::new(init_config());

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

    let conn = Connection::open(".trickedbot/database.sqlite")?;

    let state = Arc::new(Mutex::new(State::new(rand::thread_rng(), client, conn)));

    let mut interval = time::interval(Duration::from_secs(3600));
    let http_clone = Arc::clone(&http);
    let config_clone = Arc::clone(&config);
    tokio::spawn(async move {
        loop {
            let http = Arc::clone(&http_clone);
            let config = Arc::clone(&config_clone);
            tokio::spawn(async move {
                let res = update_rss_feed(http, config).await;
                if let Err(e) = res {
                    error!("Error updating RSS feed: {}", e);
                }
            });
            interval.tick().await;
        }
    });

    let framework = Arc::new(
        Framework::builder(Arc::clone(&http), Id::new(config.id), ())
            .command(roms)
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

#[command]
#[description = "Find the fucking rom!"]
async fn roms(
    ctx: &SlashContext<'_, ()>,
    #[autocomplete = "autocomplete_arg"]
    #[description = "Some description"]
    device: Option<String>,
    #[description = "Some description"] code: Option<String>,
) -> CommandResult {
    let cdn = device.map(Some).unwrap_or(code);
    let m = if let Some(device) = cdn {
        let device = codename(device).await;
        if let Some(device) = device {
            format_device(device)
        } else {
            "Phone not found".to_owned()
        }
    } else {
        "Please provide either a device or a codename".to_owned()
    };

    ctx.interaction_client
        .create_response(
            ctx.interaction.id,
            &ctx.interaction.token,
            &InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    content: Some(m),
                    ..Default::default()
                }),
            },
        )
        .exec()
        .await?;

    Ok(())
}

#[autocomplete]
async fn autocomplete_arg(ctx: AutocompleteContext<()>) -> Option<InteractionResponseData> {
    let r = search(ctx.user_input.unwrap()).await;
    Some(InteractionResponseData {
        choices: r.map(|x| {
            let devices = x.1;
            devices
                .into_iter()
                .map(|x| CommandOptionChoice::String {
                    value: x.codename.clone(),
                    name: format!("{} ({}): {}", x.name, x.codename, x.roms.len()),
                    name_localizations: None,
                })
                .collect::<Vec<CommandOptionChoice>>()
        }),
        ..Default::default()
    })
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

            if let Some(channel_limit_duration) = locked_state
                .channel_bucket
                .limit_duration(msg.channel_id.get())
            {
                tracing::info!("Channel limit reached {}", channel_limit_duration.as_secs());
                return Ok(());
            }
            if let Some(user_limit_duration) =
                locked_state.user_bucket.limit_duration(msg.author.id.get())
            {
                tracing::info!("User limit reached {}", user_limit_duration.as_secs());
                if Duration::from_secs(5) > user_limit_duration {
                    tokio::time::sleep(user_limit_duration).await;
                } else {
                    return Ok(());
                }
            };

            let st = cache.guild_members(msg.guild_id.unwrap()).unwrap().clone();
            let id = st.iter().choose(&mut rand::thread_rng()).unwrap();
            let member = cache.member(msg.guild_id.unwrap(), id.clone()).unwrap();
            let username = cache.user(id.clone()).unwrap().name.clone();
            let nick = member.nick().unwrap_or_else(|| &username);

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
fn print_qr(qr: &QrCode) -> String {
    let border: i32 = 1;
    let mut res = String::new();
    for y in -border..qr.size() + border {
        for x in -border..qr.size() + border {
            let c = if qr.get_module(x, y) { '█' } else { ' ' };

            let _ = write!(res, "{0}{0}", c);
        }
        res.push('\n');
    }
    res.push('\n');
    res
}

fn init_config() -> Config {
    let config_str = fs::read_to_string(fs::canonicalize("trickedbot.toml").unwrap()).unwrap();
    toml::from_str(&config_str).unwrap_or_default()
}

async fn update_rss_feed(
    http: Arc<HttpClient>,
    config: Arc<Config>,
) -> Result<(), Box<dyn std::error::Error>> {
    for page in &config.rss_feeds {
        let bytes = reqwest::get(page).await?.bytes().await?;
        let res = std::io::Cursor::new(bytes);
        let res = parser::parse(res)?;

        let icon = if let Some(icon) = res.icon {
            Some(icon.uri)
        } else if let Some(icon) = res.logo {
            Some(icon.uri)
        } else {
            None
        };

        for entry in res.entries {
            if entry.published.is_none() {
                continue;
            }

            if 3600 > (Utc::now().timestamp() - entry.published.unwrap().timestamp()) {
                if res.title.is_none()
                    || res.links.is_empty()
                    || entry.summary.is_none()
                    || entry.published.is_none()
                    || entry.authors.is_empty()
                {
                    continue;
                }
                let embed = EmbedBuilder::new()
                    .author(EmbedAuthor {
                        icon_url: icon.clone(),
                        name: res.title.clone().unwrap().content,
                        url: Some(res.links.get(0).unwrap().href.clone()),
                        proxy_icon_url: None,
                    })
                    .title(entry.title.unwrap().content)
                    .url(entry.links.get(0).as_ref().unwrap().href.clone())
                    .description(rhtml2md::parse_html(&entry.summary.unwrap().content))
                    .timestamp(Timestamp::from_secs(entry.published.unwrap().timestamp()).unwrap())
                    .footer(EmbedFooter {
                        text: entry.authors.get(0).unwrap().name.clone(),
                        icon_url: entry.authors.get(0).unwrap().uri.clone(),
                        proxy_icon_url: None,
                    })
                    .build()?;
                http.create_message(Id::new(config.join_channel))
                    .embeds(&[embed])?
                    .exec()
                    .await?;
            }
        }
    }
    Ok(())
}
