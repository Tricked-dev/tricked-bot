use argh::FromArgs;
use rand::prelude::ThreadRng;
use reqwest::Client;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use std::{collections::HashMap, time::Duration};
use twilight_bucket::{Bucket, Limit};
use twilight_model::http::attachment::Attachment;
use twilight_model::invite::Invite;
use twilight_model::{channel::embed::Embed, gateway::payload::incoming::InviteCreate};

#[derive(FromArgs, PartialEq, Eq, Debug)]
/// Tricked Commands!!
pub struct TrickedCommands {
    #[argh(subcommand)]
    pub nested: Commands,
}

#[derive(FromArgs, PartialEq, Eq, Debug)]
#[argh(subcommand)]
pub enum Commands {
    QR(QR),
    MD(MD),
    Zip(Zip),
    Search(Search),
    InviteStats(InviteStats),
}

#[derive(FromArgs, PartialEq, Eq, Debug)]
#[argh(subcommand, name = "qr")]
/// create a qrcode from text!
pub struct QR {
    #[argh(positional)]
    /// the text to be qr-d
    pub text: Vec<String>,
}
#[derive(FromArgs, PartialEq, Eq, Debug)]
#[argh(subcommand, name = "md")]
/// turn text into a markdown ansiL
pub struct MD {
    #[argh(positional)]
    /// the text to be marked!
    pub text: Vec<String>,
}

#[derive(FromArgs, PartialEq, Eq, Debug)]
#[argh(subcommand, name = "search")]
/// search for things on ddg
pub struct Search {
    #[argh(positional)]
    /// query to be searched
    pub query: Vec<String>,
}

#[derive(FromArgs, PartialEq, Eq, Debug)]
#[argh(subcommand, name = "zip")]
/// zip some files they must be attachments!
pub struct Zip {}

#[derive(FromArgs, PartialEq, Eq, Debug)]
#[argh(subcommand, name = "invite_stats")]
/// Get Some invite stats!
pub struct InviteStats {}

#[derive(PartialEq, Eq, Default, Clone)]
pub struct Command {
    pub embeds: Vec<Embed>,
    pub text: Option<String>,
    pub reply: bool,
    pub reaction: Option<char>,
    pub attachments: Vec<Attachment>,
    pub skip: bool,
}

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
    pub uses: Option<u64>,
}

impl From<Invite> for BotInvite {
    fn from(invite: Invite) -> Self {
        Self {
            code: invite.code.clone(),
            uses: invite.uses,
        }
    }
}

impl From<Box<InviteCreate>> for BotInvite {
    fn from(invite: Box<InviteCreate>) -> Self {
        Self {
            code: invite.code.clone(),
            uses: Some(invite.uses as u64),
        }
    }
}
pub struct State {
    pub last_redesc: Instant,
    pub rng: ThreadRng,
    pub client: Client,
    pub user_bucket: Bucket,
    pub channel_bucket: Bucket,
    pub db: Connection,
    pub invites: Vec<BotInvite>,
    pub nick: String,
    pub nick_id: u64,
}

impl State {
    pub fn new(rng: ThreadRng, client: Client, db: Connection) -> Self {
        let user_bucket = Bucket::new(Limit::new(Duration::from_secs(30), 10));
        let channel_bucket = Bucket::new(Limit::new(Duration::from_secs(60), 120));
        Self {
            db,
            rng,
            client,
            last_redesc: Instant::now(),
            user_bucket,
            nick: "".to_owned(),
            nick_id: 0,
            channel_bucket,
            invites: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub token: String,
    pub discord: u64,
    pub join_channel: u64,
    pub id: u64,
    #[serde(default)]
    pub rename_channels: Vec<u64>,
    #[serde(default)]
    pub invites: HashMap<String, String>,
    #[serde(default)]
    pub shit_reddits: Vec<String>,
    #[serde(default)]
    pub rss_feeds: Vec<String>,
    #[serde(default = "default_status")]
    pub status: String,
}
fn default_status() -> String {
    "I am a bot".to_string()
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
