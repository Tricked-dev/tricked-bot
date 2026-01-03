use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::time::Instant as TokioInstant;

use r2d2_sqlite::SqliteConnectionManager;
use rand::prelude::ThreadRng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use twilight_bucket::{Bucket, Limit};
use twilight_cache_inmemory::InMemoryCache;
use twilight_model::{channel::message::Embed, http::attachment::Attachment, id::Id};
use vesper::twilight_exports::ChannelMarker;

use crate::{brave::BraveApi, config::Config};

#[derive(PartialEq, Default, Eq, Clone)]
pub struct Command {
    pub embeds: Vec<Embed>,
    pub text: Option<String>,
    pub reply: bool,
    pub reaction: Option<char>,
    pub attachments: Vec<Attachment>,
    pub mention: bool,
    pub skip: bool,
}

#[allow(dead_code)]
impl Command {
    pub fn embed(embed: Embed) -> Self {
        Self {
            embeds: vec![embed],
            ..Self::default()
        }
    }
    pub fn embeds(embeds: Vec<Embed>) -> Self {
        Self {
            embeds,
            ..Self::default()
        }
    }
    pub fn text<T: Into<String>>(text: T) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }
    pub fn react(reaction: char) -> Self {
        Self {
            reaction: Some(reaction),
            ..Self::default()
        }
    }
    pub fn nothing() -> Self {
        Self {
            skip: true,
            ..Self::default()
        }
    }
    pub fn reply(mut self) -> Self {
        self.reply = true;
        self
    }
    pub fn mention(mut self) -> Self {
        self.mention = true;
        self
    }

    pub fn attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = attachments;
        self
    }
}

/// Tracks a pending math test for a user
#[derive(Debug, Clone)]
pub struct PendingMathTest {
    pub user_id: u64,
    pub channel_id: u64,
    pub question: String,
    pub answer: f64,
    pub started_at: TokioInstant,
}

/// Tracks a pending color test for a user
#[derive(Debug, Clone)]
pub struct PendingColorTest {
    pub user_id: u64,
    pub channel_id: u64,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub started_at: TokioInstant,
}

/// This struct is used to store the state of the bot.\
/// It is used to store the cache, the database connection, the config and the http client.
pub struct State {
    pub last_redesc: Instant,
    /// Rng
    pub rng: ThreadRng,
    /// Reqwest client
    pub client: Client,
    /// Bucket for user messages
    pub user_bucket: Bucket,
    /// Ratelimit for channel creation
    pub channel_bucket: Bucket,
    /// Ratelimit for DM messages (30 messages per hour)
    pub dm_bucket: Bucket,
    /// Sqlite database connection
    pub db: r2d2::Pool<SqliteConnectionManager>,
    pub nick: String,
    pub nick_id: u64,
    /// The id of the last user that typed to not resend the indicator
    pub last_typer: u64,
    /// This is a map of channel id to the last time a message was sent in that channel for message typing indicator.
    pub del: HashMap<Id<ChannelMarker>, u64>,
    /// cli args
    pub config: Arc<Config>,
    /// twilight cache
    pub cache: InMemoryCache,
    /// Brave API
    pub brave_api: BraveApi,
    /// Pending math tests
    pub pending_math_tests: HashMap<u64, PendingMathTest>,
    /// Pending color tests
    pub pending_color_tests: HashMap<u64, PendingColorTest>,
    /// Message count per channel/user since last memory creation (channel_id or user_id -> message count)
    pub channel_message_counts: HashMap<u64, i32>,
}
// i hate fixing error
unsafe impl Send for State {}

impl State {
    pub fn new(rng: ThreadRng, client: Client, db: r2d2::Pool<SqliteConnectionManager>, config: Arc<Config>) -> Self {
        let user_bucket = Bucket::new(Limit::new(Duration::from_secs(30), 10));
        let channel_bucket = Bucket::new(Limit::new(Duration::from_secs(60), 120));
        let dm_bucket = Bucket::new(Limit::new(Duration::from_secs(3600), 30)); // 30 messages per hour
        let client_clone = client.clone();
        Self {
            db,
            rng,
            client,
            last_redesc: Instant::now(),
            user_bucket,
            last_typer: 0,
            nick: "".to_owned(),
            nick_id: 0,
            del: HashMap::new(),
            channel_bucket,
            cache: InMemoryCache::new(),
            brave_api: BraveApi::new(client_clone, &config.brave_api.clone().unwrap_or_default()),
            config,
            pending_math_tests: HashMap::new(),
            pending_color_tests: HashMap::new(),
            channel_message_counts: HashMap::new(),
            dm_bucket,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]

pub struct Responder {
    pub message: Option<String>,
    pub react: Option<String>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct List {
    pub data: Data,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Data {
    pub children: Vec<Children>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Children {
    pub data: Data2,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Data2 {
    #[serde(rename = "url_overridden_by_dest")]
    pub url_overridden_by_dest: Option<String>,
    #[serde(rename = "over_18")]
    pub over_18: bool,
}
