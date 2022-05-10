use futures::stream::StreamExt;
use rand::{
    prelude::{IteratorRandom, ThreadRng},
    Rng,
};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fs::{canonicalize, read_to_string},
    sync::Arc,
    time::{Duration, Instant},
};
use surf::{Client, Config as SurfConfig};
use tokio::sync::Mutex;
use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_gateway::{Event, Shard};
use twilight_http::{request::channel::reaction::RequestReactionType, Client as HttpClient};
use twilight_model::{channel::message::AllowedMentions, gateway::Intents, id::Id};

#[derive(Clone, Debug)]
struct State {
    last_redesc: Instant,
    rng: ThreadRng,
    client: Client,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct Config {
    token: String,
    discord: u64,
    rename_channels: Vec<u64>,
}

impl State {
    fn new(rng: ThreadRng, client: Client) -> Self {
        Self {
            rng,
            client,
            ..Self::default()
        }
    }
}

impl Default for State {
    fn default() -> Self {
        let rng = rand::thread_rng();
        Self {
            last_redesc: Instant::now(),
            client: Client::default(),
            rng,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt().init();
    let config = Arc::new(init_config());

    let client: Client = SurfConfig::new()
        .add_header("user-agent", "tricked-bot/1.0")?
        .set_timeout(Some(Duration::from_secs(5)))
        .try_into()?;

    let (shard, mut events) = Shard::builder(config.token.to_owned(), Intents::all()).build();
    let shard = Arc::new(shard);
    shard.start().await?;

    // HTTP is separate from the gateway, so create a new client.
    let http = Arc::new(
        HttpClient::builder()
            .token(config.token.to_owned())
            .default_allowed_mentions(AllowedMentions::builder().build())
            .build(),
    );

    let state = Arc::new(Mutex::new(State::new(rand::thread_rng(), client)));

    let cache = InMemoryCache::builder()
        .resource_types(ResourceType::MESSAGE | ResourceType::PRESENCE | ResourceType::MEMBER)
        .build();

    while let Some(event) = events.next().await {
        cache.update(&event);
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
    log::error!("Reached end of events ?");

    Ok(())
}

async fn handle_event(
    event: Event,
    http: Arc<HttpClient>,
    _shard: Arc<Shard>,
    state: Arc<Mutex<State>>,
    config: Arc<Config>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match event {
        Event::MessageCreate(msg) => {
            log::info!("Message received {}", &msg.content,);

            if msg.guild_id.is_none() || msg.author.bot {
                return Ok(());
            }
            if let Some(guild_id) = msg.guild_id {
                if guild_id != Id::new(config.discord) {
                    http.leave_guild(guild_id).exec().await?;
                }
            }

            let mut locked_state = state.lock().await;
            if locked_state.last_redesc.elapsed() > std::time::Duration::from_secs(300)
                && config
                    .rename_channels
                    .to_vec()
                    .contains(&msg.channel_id.get())
            {
                log::info!("Channel renamed");
                match http.update_channel(msg.channel_id).topic(&msg.content) {
                    Ok(req) => {
                        req.exec().await?;

                        locked_state.last_redesc = Instant::now();
                    }
                    Err(err) => log::error!("{:?}", err),
                }
            }
            if locked_state.rng.gen_range(0..45) == 2 {
                let content = zalgify_text(locked_state.rng.clone(), msg.content.to_owned());
                match http
                    .create_message(msg.channel_id)
                    .reply(msg.id)
                    .content(&content)?
                    .exec()
                    .await
                {
                    Ok(_) => {}
                    Err(e) => log::error!("Failed to send message {e:?}"),
                }
            }

            if locked_state.rng.gen_range(0..20) == 2 {
                let res = locked_state
                    .client
                    .get("https://www.reddit.com/r/shitposting/.json")
                    .await?
                    .body_json::<List>()
                    .await?
                    .data
                    .children
                    .into_iter()
                    .filter(|x| !x.data.over_18)
                    .filter(|x| x.data.url_overridden_by_dest.contains("i."))
                    .choose(&mut locked_state.rng)
                    .map(|x| x.data.url_overridden_by_dest);
                if let Some(pic) = res {
                    http.create_message(msg.channel_id)
                        .content(&pic)?
                        .exec()
                        .await?;
                }
            }
            if msg.content.to_lowercase() == "l" {
                http.create_message(msg.channel_id)
                    .content("+ ratio")?
                    .exec()
                    .await?;
            }
            if msg.content.to_lowercase().contains("skull") {
                http.create_reaction(
                    msg.channel_id,
                    msg.id,
                    &RequestReactionType::Unicode { name: "💀" },
                )
                .exec()
                .await?;
            }
        }
        Event::Ready(_) => {
            log::info!("Connected",);
        }
        // #[cfg(feature = "lol-trolling")]
        // Event::GuildCreate(guild) => {
        //     use twilight_model::gateway::payload::outgoing::RequestGuildMembers;
        //     if guild.id == Id::new(config.discord) {
        //         shard
        //             .command(
        //                 &RequestGuildMembers::builder(guild.id)
        //                     .presences(true)
        //                     .query("", None),
        //             )
        //             .await?;
        //     }
        // }
        // #[cfg(feature = "lol-trolling")]
        // Event::PresenceUpdate(_) => {
        //     use chrono::prelude::*;
        //     cache.iter().presences().for_each(|presence| {
        //         if presence.guild_id() != Id::new(config.discord) {
        //             return;
        //         }
        //         presence.activities().iter().for_each(|activity| {
        //             if let Some(timestamps) = &activity.created_at {
        //                 if activity.name == "League of Legends" {
        //                     let timestamp: i64 = (*timestamps).try_into().unwrap();
        //                     let ts = DateTime::<Utc>::from_utc(
        //                         NaiveDateTime::from_timestamp(timestamp / 1000, 0),
        //                         Utc,
        //                     );
        //                     let time = Utc::now().signed_duration_since(ts);
        //                     if time.num_seconds() > 1800 {
        //                         log::info!(
        //                             "{} has been playing LoL for over 30 minutes",
        //                             presence.user_id()
        //                         );
        //                     }
        //                 }
        //                 return;
        //             }
        //         })
        //     });
        // }
        _ => {}
    }
    Ok(())
}

fn init_config() -> Config {
    let config_str = read_to_string(canonicalize("trickedbot.toml").unwrap()).unwrap();
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
