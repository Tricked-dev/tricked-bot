#![allow(deprecated, clippy::upper_case_acronyms)]
#![warn(
    clippy::all,
    future_incompatible,
    nonstandard_style,
    rust_2018_idioms,
    missing_debug_implementations
)]
#![forbid(anonymous_parameters)]

use crate::structs::*;

use clap::Parser;
use config::Config;
use futures::stream::StreamExt;
use once_cell::sync::Lazy;
use r2d2_sqlite::SqliteConnectionManager;
use reqwest::Client;
use tokio::sync::Mutex;
use twilight_gateway::{
    stream::{self, ShardEventStream},
    Config as TLConfig,
};
use twilight_http::Client as HttpClient;
use twilight_model::{
    channel::message::AllowedMentions,
    gateway::{
        payload::outgoing::update_presence::UpdatePresencePayload,
        presence::{ActivityType, MinimalActivity, Status},
        Intents,
    },
    id::Id,
};
use vesper::prelude::*;

use std::{collections::HashMap, env, sync::Arc};

pub mod ai_message;
pub mod brave;
mod color_quiz;
mod commands;
mod config;
mod database;
mod event_handler;
mod math_test;
mod message_handler;
mod quiz_handler;
mod structs;
pub mod utils;
mod zalgos;

static RESPONDERS: Lazy<HashMap<String, Responder>> =
    Lazy::new(|| toml::from_str(include_str!("../responders.toml")).unwrap());

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    dotenv::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::new("debug,h2::codec::framed_read=off")
        )
        .init();

    let mut cfg = Config::parse();
    if cfg.id == 0 {
        cfg.id =
            String::from_utf8_lossy(&base64::decode(cfg.token.split_once('.').unwrap().0).unwrap()).parse::<u64>()?;
    }

    if std::fs::metadata(&cfg.database_file).is_err() {
        std::fs::write(&cfg.database_file, [])?;
    }

    let manager = SqliteConnectionManager::file(&cfg.database_file);
    let pool = r2d2::Pool::new(manager).unwrap();
    let rusqlite = pool.get().unwrap();

    if rusqlite.table_exists(None, "User")? {
        rusqlite.execute("ALTER TABLE User RENAME TO user1", [])?;
        rusqlite.execute("ALTER TABLE user1 RENAME TO user", [])?;
    } else if !rusqlite.table_exists(None, "user")? {
        rusqlite.execute(database::User::CREATE_TABLE_SQL, [])?;
    }
    if !rusqlite.column_exists(None, "user", "name")? {
        rusqlite.execute("ALTER TABLE user ADD COLUMN name TEXT DEFAULT ''", [])?;
    }

    rusqlite.execute(database::Memory::CREATE_TABLE_SQL, [])?;
    rusqlite.execute(database::MathQuestion::CREATE_TABLE_SQL, [])?;

    let config = Arc::new(cfg);

    let client: Client = Client::builder()
        .user_agent(format!(
            "tricked-bot/{} ({}; {})",
            VERSION,
            env::consts::OS,
            env::consts::ARCH
        ))
        .build()?;

    // HTTP is separate from the gateway, so create a new client.
    let http = Arc::new(
        HttpClient::builder()
            .token(config.token.clone())
            .default_allowed_mentions(AllowedMentions::default())
            .build(),
    );

    let tl_config = TLConfig::new(
        config.token.clone(),
        Intents::GUILD_INVITES
            | Intents::GUILD_MESSAGES
            | Intents::GUILD_MEMBERS
            | Intents::GUILDS
            | Intents::GUILD_PRESENCES
            | Intents::MESSAGE_CONTENT
            | Intents::GUILD_MESSAGE_TYPING,
    );
    let mut shards = stream::create_recommended(&http, tl_config, |_, builder| {
        builder
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
    })
    .await?
    .collect::<Vec<_>>();
    let mut shard_stream = ShardEventStream::new(shards.iter_mut());

    let state = Arc::new(Mutex::new(State::new(
        rand::thread_rng(),
        client,
        pool,
        Arc::clone(&config),
    )));

    let framework = Arc::new(
        Framework::builder(Arc::clone(&http), Id::new(config.id), Arc::clone(&state))
            .command(commands::level::level)
            .build(),
    );

    framework.register_guild_commands(Id::new(config.discord)).await?;

    while let Some(event) = shard_stream.next().await {
        let ev = match event.1 {
            Ok(v) => v,
            Err(_) => continue,
        };
        {
            state.lock().await.cache.update(&ev);
        }
        let res = event_handler::handle_event(ev, &http, &state, Arc::clone(&framework)).await;
        if let Err(res) = res {
            tracing::error!("{:?}", res);
        }
    }
    Ok(())
}
