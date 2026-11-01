mod ai;
mod bot;
mod commands;
mod config;
mod currency;
mod db;
mod decider;
mod events;
mod ffmpeg;
mod levels;
mod messages;
mod pfp;
mod qalc;
mod quiz;
mod ratewaifu;
#[cfg(test)]
mod test_util;
mod time;
mod web;
mod zalgo;

use std::sync::Arc;

use clap::Parser;
use color_eyre::Result;
use twilight_gateway::{ConfigBuilder, EventTypeFlags, Intents, Shard, StreamExt as _};
use twilight_http::Client as HttpClient;
use twilight_model::{
    channel::message::AllowedMentions,
    gateway::{
        payload::outgoing::update_presence::UpdatePresencePayload,
        presence::{ActivityType, MinimalActivity, Status},
    },
};

use crate::{bot::Bot, config::Config};

const DEFAULT_LOG_FILTER: &str = "debug,h2::codec=info,twilight_gateway::shard=off,\
    twilight_http_ratelimiting::in_memory=info,rustls::client=info,hyper_util::client=info,tokio_postgres=info";

const INTENTS: Intents = Intents::GUILDS
    .union(Intents::GUILD_MEMBERS)
    .union(Intents::GUILD_MESSAGES)
    .union(Intents::GUILD_MESSAGE_TYPING)
    .union(Intents::DIRECT_MESSAGES)
    .union(Intents::MESSAGE_CONTENT);

/// Events the bot handles or the cache needs. Everything else is skipped before deserialization.
const EVENTS: EventTypeFlags = EventTypeFlags::READY
    .union(EventTypeFlags::GUILD_CREATE)
    .union(EventTypeFlags::GUILD_DELETE)
    .union(EventTypeFlags::INTERACTION_CREATE)
    .union(EventTypeFlags::MEMBER_ADD)
    .union(EventTypeFlags::MEMBER_CHUNK)
    .union(EventTypeFlags::MEMBER_REMOVE)
    .union(EventTypeFlags::MEMBER_UPDATE)
    .union(EventTypeFlags::MESSAGE_CREATE)
    .union(EventTypeFlags::MESSAGE_DELETE)
    .union(EventTypeFlags::MESSAGE_DELETE_BULK)
    .union(EventTypeFlags::MESSAGE_UPDATE)
    .union(EventTypeFlags::TYPING_START);

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER)),
        )
        .init();

    let config = Config::parse();
    let pool = deadpool_postgres::Pool::builder(deadpool_postgres::Manager::new(
        config.database_url.parse()?,
        tokio_postgres::NoTls,
    ))
    .max_size(16)
    .build()?;
    db::run_migrations(&pool).await?;

    let http =
        HttpClient::builder().token(config.token.clone()).default_allowed_mentions(AllowedMentions::default()).build();
    let (user, application) = tokio::try_join!(
        async { http.current_user().await?.model().await.map_err(color_eyre::Report::from) },
        async { http.current_user_application().await?.model().await.map_err(color_eyre::Report::from) },
    )?;
    tracing::info!(user = %user.name, "Logged in");

    let gateway = ConfigBuilder::new(config.token.clone(), INTENTS)
        .presence(UpdatePresencePayload::new(
            vec![MinimalActivity { kind: ActivityType::Competing, name: config.status.clone(), url: None }.into()],
            false,
            None,
            Status::Idle,
        )?)
        .build();
    let bot = Arc::new(Bot::new(config, http, user.id, application.id, pool)?);
    let shards = twilight_gateway::create_recommended(&bot.http, gateway, |_, builder| builder.build()).await?;

    commands::register(&bot).await?;
    tokio::spawn(qalc::update_rates());
    currency::spawn_refresh(Arc::clone(&bot));
    ai::cleanup::spawn(Arc::clone(&bot));
    ai::embeddings::spawn(Arc::clone(&bot));
    pfp::spawn_daily(Arc::clone(&bot));
    if let Some(port) = bot.config.web_port {
        let db = bot.db.clone();
        tokio::spawn(async move {
            if let Err(error) = web::serve(db, port).await {
                tracing::error!("Web server error: {error:?}");
            }
        });
    }

    let runners: Vec<_> = shards.map(|shard| tokio::spawn(run_shard(Arc::clone(&bot), shard))).collect();
    for runner in runners {
        runner.await?;
    }
    Ok(())
}

/// Updates the cache in gateway order, then handles each event on its own task so slow handlers never stall the
/// gateway or each other.
async fn run_shard(bot: Arc<Bot>, mut shard: Shard) {
    while let Some(event) = shard.next_event(EVENTS).await {
        match event {
            Ok(event) => {
                bot.cache.update(&event);
                tokio::spawn(events::handle(Arc::clone(&bot), event));
            }
            Err(error) => tracing::warn!(shard = %shard.id(), "Error receiving event: {error}"),
        }
    }
}
