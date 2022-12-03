use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use rand::prelude::ThreadRng;
use reqwest::Client;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use twilight_bucket::{Bucket, Limit};
use twilight_cache_inmemory::InMemoryCache;
use twilight_model::{
    channel::message::Embed, gateway::payload::incoming::InviteCreate, guild::invite::Invite,
    http::attachment::Attachment, id::Id,
};
use zephyrus::twilight_exports::ChannelMarker;

use crate::config::Config;

#[derive(PartialEq, Default, Eq, Clone)]
pub struct Command {
    pub embeds: Vec<Embed>,
    pub text: Option<String>,
    pub reply: bool,
    pub reaction: Option<char>,
    pub attachments: Vec<Attachment>,
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

    pub fn attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = attachments;
        self
    }
}

/// This pubstruct is needed to deal with the invite create event.
#[derive(Clone)]
pub struct BotInvite {
    pub code: String,
    pub uses: u64,
}

impl From<Invite> for BotInvite {
    fn from(invite: Invite) -> Self {
        Self {
            code: invite.code.clone(),
            uses: invite.uses.unwrap_or_default(),
        }
    }
}

impl From<Box<InviteCreate>> for BotInvite {
    fn from(invite: Box<InviteCreate>) -> Self {
        Self {
            code: invite.code.clone(),
            uses: invite.uses as u64,
        }
    }
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
    /// Sqlite database connection
    pub db: Connection,
    pub invites: Vec<BotInvite>,
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
}
// i hate fixing error
unsafe impl Send for State {}

impl State {
    pub fn new(rng: ThreadRng, client: Client, db: Connection, config: Arc<Config>) -> Self {
        let user_bucket = Bucket::new(Limit::new(Duration::from_secs(30), 10));
        let channel_bucket = Bucket::new(Limit::new(Duration::from_secs(60), 120));
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
            invites: Vec::new(),
            cache: InMemoryCache::new(),
            config,
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
    pub url_overridden_by_dest: String,
    #[serde(rename = "over_18")]
    pub over_18: bool,
}
