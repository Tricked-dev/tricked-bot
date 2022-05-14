#![allow(deprecated)]
use futures::stream::StreamExt;
use qrcodegen::{QrCode, QrCodeEcc};
use rand::{
    prelude::{IteratorRandom, SliceRandom, ThreadRng},
    Rng,
};
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, MutexGuard};
use twilight_bucket::{Bucket, Limit};
use twilight_embed_builder::EmbedBuilder;
use twilight_gateway::{Event, EventTypeFlags, Shard};
use twilight_http::{request::channel::reaction::RequestReactionType, Client as HttpClient};
use twilight_model::channel::{embed::Embed, message::AllowedMentions};
use twilight_model::gateway::payload::incoming::InviteCreate;
use twilight_model::gateway::payload::incoming::MessageCreate;
use twilight_model::gateway::payload::outgoing::update_presence::UpdatePresencePayload;
use twilight_model::gateway::presence::{ActivityType, MinimalActivity, Status};
use twilight_model::{gateway::Intents, id::Id, invite::Invite};

struct State {
    last_redesc: Instant,
    rng: ThreadRng,
    client: Client,
    user_bucket: Bucket,
    channel_bucket: Bucket,
    db: Connection,
    invites: Vec<BotInvite>,
}

impl State {
    fn new(rng: ThreadRng, client: Client, db: Connection) -> Self {
        let user_bucket = Bucket::new(Limit::new(Duration::from_secs(30), 10));
        let channel_bucket = Bucket::new(Limit::new(Duration::from_secs(60), 120));
        Self {
            db,
            rng,
            client,
            last_redesc: Instant::now(),
            user_bucket,
            channel_bucket,
            invites: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct Config {
    token: String,
    discord: u64,
    join_channel: u64,
    rename_channels: Vec<u64>,
    invites: HashMap<String, String>,
    shit_reddits: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt().init();
    let config = Arc::new(init_config());

    let client: Client = Client::builder().user_agent("tricked-bot/1.0").build()?;

    let (shard, mut events) = Shard::builder(
        config.token.to_owned(),
        Intents::GUILD_INVITES
            | Intents::GUILD_MESSAGES
            | Intents::GUILD_MEMBERS
            | Intents::GUILDS
            | Intents::MESSAGE_CONTENT,
    )
    .event_types(
        EventTypeFlags::INVITE_CREATE
            | EventTypeFlags::MESSAGE_CREATE
            | EventTypeFlags::MEMBER_ADD
            | EventTypeFlags::READY
            | EventTypeFlags::GUILD_CREATE,
    )
    .presence(
        UpdatePresencePayload::new(
            vec![MinimalActivity {
                kind: ActivityType::Competing,
                name: "The perfect bot to ruin your Discord server.".to_string(),
                url: None,
            }
            .into()],
            false,
            None,
            Status::Idle,
        )
        .unwrap(),
    )
    .build();
    let shard = Arc::new(shard);
    shard.start().await?;

    // HTTP is separate from the gateway, so create a new client.
    let http = Arc::new(
        HttpClient::builder()
            .token(config.token.to_owned())
            .default_allowed_mentions(AllowedMentions::builder().build())
            .build(),
    );
    let conn = Connection::open(".trickedbot/database.sqlite")?;

    let state = Arc::new(Mutex::new(State::new(rand::thread_rng(), client, conn)));

    while let Some(event) = events.next().await {
        let res = handle_event(
            event,
            Arc::clone(&http),
            Arc::clone(&shard),
            Arc::clone(&state),
            Arc::clone(&config),
        )
        .await;
        if let Err(res) = res {
            tracing::error!("{}", res);
        }
    }
    Ok(())
}
#[derive(PartialEq, Clone)]
enum Command {
    Text(String),
    React(char),
    Reply(String),
    Embed(Embed),
    Nothing,
}
/// This struct is needed to deal with the invite create event.
#[derive(Clone)]
struct BotInvite {
    code: String,
    uses: Option<u64>,
}

impl From<Invite> for BotInvite {
    fn from(invite: Invite) -> Self {
        Self {
            code: invite.code.to_owned(),
            uses: invite.uses,
        }
    }
}

impl From<Box<InviteCreate>> for BotInvite {
    fn from(invite: Box<InviteCreate>) -> Self {
        Self {
            code: invite.code.to_owned(),
            uses: Some(invite.uses as u64),
        }
    }
}

async fn handle_event(
    event: Event,
    http: Arc<HttpClient>,
    _shard: Arc<Shard>,
    state: Arc<Mutex<State>>,
    config: Arc<Config>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut locked_state = state.lock().await;
    match event {
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
                                Some(key.to_owned())
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
                                    invite.code.to_owned()
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
                    code: invite.code.to_owned(),
                    uses: invite.uses,
                })
                .collect()
        }
        Event::MessageCreate(msg) => {
            tracing::info!("Message received {}", &msg.content,);

            if msg.guild_id.is_none() || msg.author.bot {
                return Ok(());
            }
            if let Some(guild_id) = msg.guild_id {
                if guild_id != Id::new(config.discord) {
                    http.leave_guild(guild_id).exec().await?;
                }
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
            }

            let r = handle_message(&msg, locked_state, &config, &http).await;

            let locked_state = state.lock().await;
            if let Ok(res) = r {
                if res != Command::Nothing {
                    locked_state.channel_bucket.register(msg.channel_id.get());
                    locked_state.user_bucket.register(msg.author.id.get());
                }

                match res {
                    Command::Text(text) => {
                        http.create_message(msg.channel_id)
                            .content(&text)?
                            .exec()
                            .await?;
                    }
                    Command::Reply(text) => {
                        http.create_message(msg.channel_id)
                            .content(&text)?
                            .reply(msg.id)
                            .exec()
                            .await?;
                    }
                    Command::React(emoji) => {
                        http.create_reaction(
                            msg.channel_id,
                            msg.id,
                            &RequestReactionType::Unicode {
                                name: &emoji.to_string(),
                            },
                        )
                        .exec()
                        .await?;
                    }
                    Command::Embed(embed) => {
                        http.create_message(msg.channel_id)
                            .embeds(&[embed])?
                            .exec()
                            .await?;
                    }
                    _ => {}
                }
            }
        }
        Event::Ready(_) => {
            tracing::info!("Connected",);
        }
        Event::GuildCreate(guild) => {
            tracing::info!("Active in guild {}", guild.name);
            let invites_response = http.guild_invites(guild.id).exec().await?;
            locked_state.invites = invites_response
                .models()
                .await?
                .into_iter()
                .map(|invite| BotInvite {
                    code: invite.code.to_owned(),
                    uses: invite.uses,
                })
                .collect()
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
            res.push_str(&format!("{0}{0}", c));
        }
        res.push_str("\n");
    }
    res.push_str("\n");
    res
}

async fn handle_message(
    msg: &Box<MessageCreate>,
    mut locked_state: MutexGuard<'_, State>,
    config: &Arc<Config>,
    http: &Arc<HttpClient>,
) -> Result<Command, Box<dyn Error + Send + Sync>> {
    match msg.content.to_lowercase().as_str() {
        "l" => Ok(Command::Text("+ ratio".to_string())),
        "f" => Ok(Command::React('🇫')),
        "dn" => Ok(Command::Text("Digital Native".to_string())),
        "gn" => Ok(Command::Text(
            "https://www.youtube.com/watch?v=ykLDTsfnE5A".into(),
        )),
        x if x.contains("--qr") => {
            let qr = QrCode::encode_text(x.replace("--qr", "").trim(), QrCodeEcc::Low)?;
            let res = print_qr(&qr);
            let embed = EmbedBuilder::new()
                .description(format!("```ansi\n{res}\n```"))
                .build()?;
            Ok(Command::Embed(embed))
        }

        x if x.contains("skull") => Ok(Command::React('💀')),
        content
            if locked_state.last_redesc.elapsed() > std::time::Duration::from_secs(150)
                && config
                    .rename_channels
                    .to_vec()
                    .contains(&msg.channel_id.get())
                && locked_state.rng.gen_range(0..10) == 2 =>
        {
            if content.to_lowercase().contains("uwu") || content.to_lowercase().contains("owo") {
                Ok(Command::Text("No furry shit!!!!!".into()))
            } else {
                tracing::info!("Channel renamed");
                match http.update_channel(msg.channel_id).topic(content) {
                    Ok(req) => {
                        req.exec().await?;
                        locked_state.last_redesc = Instant::now();
                    }
                    Err(err) => tracing::error!("{:?}", err),
                }
                Ok(Command::Nothing)
            }
        }
        x if locked_state.rng.gen_range(0..45) == 2 => {
            let content = zalgify_text(locked_state.rng.clone(), x.to_owned());
            Ok(Command::Reply(content))
        }
        _ if locked_state.rng.gen_range(0..20) == 2 => {
            let res = (&locked_state)
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
                Ok(Command::Text(pic))
            } else {
                Ok(Command::Nothing)
            }
        }
        _ => Ok(Command::Nothing),
    }
}

fn init_config() -> Config {
    let config_str = fs::read_to_string(fs::canonicalize("trickedbot.toml").unwrap()).unwrap();
    toml::from_str(&config_str).unwrap_or_default()
}

const ZALGO_UP: [char; 50] = [
    '\u{030e}', /*    ̎    */ '\u{0304}', /*    ̄    */ '\u{0305}', /*    ̅    */
    '\u{033f}', /*    ̿    */ '\u{0311}', /*    ̑    */ '\u{0306}',
    /*    ̆    */ '\u{0310}', /*    ̐    */
    '\u{0352}', /*    ͒    */ '\u{0357}', /*    ͗    */ '\u{0351}',
    /*    ͑    */ '\u{0307}', /*    ̇    */
    '\u{0308}', /*    ̈    */ '\u{030a}', /*    ̊    */ '\u{0342}',
    /*    ͂    */ '\u{0343}', /*    ̓    */
    '\u{0344}', /*    ̈́    */ '\u{034a}', /*    ͊    */ '\u{034b}',
    /*    ͋    */ '\u{034c}', /*    ͌    */
    '\u{0303}', /*    ̃    */ '\u{0302}', /*    ̂    */ '\u{030c}',
    /*    ̌    */ '\u{0350}', /*    ͐    */
    '\u{0300}', /*    ̀    */ '\u{0301}', /*    ́    */ '\u{030b}',
    /*    ̋    */ '\u{030f}', /*    ̏    */
    '\u{0312}', /*    ̒    */ '\u{0313}', /*    ̓    */ '\u{0314}',
    /*    ̔    */ '\u{033d}', /*    ̽    */
    '\u{0309}', /*    ̉    */ '\u{0363}', /*    ͣ    */ '\u{0364}',
    /*    ͤ    */ '\u{0365}', /*    ͥ    */
    '\u{0366}', /*    ͦ    */ '\u{0367}', /*    ͧ    */ '\u{0368}',
    /*    ͨ    */ '\u{0369}', /*    ͩ    */
    '\u{036a}', /*    ͪ    */ '\u{036b}', /*    ͫ    */ '\u{036c}',
    /*    ͬ    */ '\u{036d}', /*    ͭ    */
    '\u{036e}', /*    ͮ    */ '\u{036f}', /*    ͯ    */ '\u{033e}',
    /*    ̾    */ '\u{035b}', /*    ͛    */
    '\u{0346}', /*    ͆    */ '\u{031a}', /*    ̚    */ '\u{030d}', /*    ̍    */
];

const ZALGO_DOWN: [char; 40] = [
    '\u{0317}', /*     ̗     */ '\u{0318}',
    /*     ̘     */ '\u{0319}', /*     ̙     */
    '\u{031c}', /*     ̜     */ '\u{031d}', /*     ̝     */ '\u{031e}',
    /*     ̞     */ '\u{031f}', /*     ̟     */
    '\u{0320}', /*     ̠     */ '\u{0324}', /*     ̤     */ '\u{0325}',
    /*     ̥     */ '\u{0326}', /*     ̦     */
    '\u{0329}', /*     ̩     */ '\u{032a}', /*     ̪     */ '\u{032b}',
    /*     ̫     */ '\u{032c}', /*     ̬     */
    '\u{032d}', /*     ̭     */ '\u{032e}', /*     ̮     */ '\u{032f}',
    /*     ̯     */ '\u{0330}', /*     ̰     */
    '\u{0331}', /*     ̱     */ '\u{0332}', /*     ̲     */ '\u{0333}',
    /*     ̳     */ '\u{0339}', /*     ̹     */
    '\u{033a}', /*     ̺     */ '\u{033b}', /*     ̻     */ '\u{033c}',
    /*     ̼     */ '\u{0345}', /*     ͅ     */
    '\u{0347}', /*     ͇     */ '\u{0348}', /*     ͈     */ '\u{0349}',
    /*     ͉     */ '\u{034d}', /*     ͍     */
    '\u{034e}', /*     ͎     */ '\u{0353}', /*     ͓     */ '\u{0354}',
    /*     ͔     */ '\u{0355}', /*     ͕     */
    '\u{0356}', /*     ͖     */ '\u{0359}', /*     ͙     */ '\u{035a}',
    /*     ͚     */ '\u{0323}', /*     ̣     */
    '\u{0316}', /*     ̖     */
];

const ZALGO_MID: [char; 23] = [
    '\u{031b}', /*     ̛     */ '\u{0340}',
    /*     ̀     */ '\u{0341}', /*     ́     */
    '\u{0358}', /*     ͘     */ '\u{0321}', /*     ̡     */ '\u{0322}',
    /*     ̢     */ '\u{0327}', /*     ̧     */
    '\u{0328}', /*     ̨     */ '\u{0334}', /*     ̴     */ '\u{0335}',
    /*     ̵     */ '\u{0336}', /*     ̶     */
    '\u{034f}', /*     ͏     */ '\u{035c}', /*     ͜     */ '\u{035d}',
    /*     ͝     */ '\u{035e}', /*     ͞     */
    '\u{035f}', /*     ͟     */ '\u{0360}', /*     ͠     */ '\u{0362}',
    /*     ͢     */ '\u{0338}', /*     ̸     */
    '\u{0337}', /*     ̷     */ '\u{0361}', /*     ͡     */ '\u{0489}',
    /*     ҉_   */ '\u{0315}', /*     ̕     */
];

pub fn zalgify_text(mut rng: ThreadRng, s: String) -> String {
    let mut new_text = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        new_text.push(c);
        for _ in 0..rng.gen_range(0..8) / 2 + 1 {
            new_text.push(ZALGO_UP[rng.gen_range(0..ZALGO_UP.len())]);
        }
        for _ in 0..rng.gen_range(0..3) / 2 {
            new_text.push(ZALGO_MID[rng.gen_range(0..ZALGO_MID.len())]);
        }
        for _ in 0..rng.gen_range(0..4) / 2 + 1 {
            new_text.push(ZALGO_DOWN[rng.gen_range(0..ZALGO_DOWN.len())]);
        }
    }
    new_text
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct List {
    pub data: Data,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Data {
    pub children: Vec<Children>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Children {
    pub data: Data2,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Data2 {
    #[serde(rename = "url_overridden_by_dest")]
    pub url_overridden_by_dest: String,
    #[serde(rename = "over_18")]
    pub over_18: bool,
}
