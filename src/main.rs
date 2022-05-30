#![allow(deprecated, clippy::upper_case_acronyms)]
#![warn(
    clippy::all,
    future_incompatible,
    nonstandard_style,
    rust_2018_idioms,
    missing_debug_implementations
)]
#![forbid(unsafe_code, anonymous_parameters)]
///! This project uses the infamous monofile approach
///! This bot is and shall stay a monofile
///! Its cooler and makes stuff easier!
use crate::structs::*;
use argh::FromArgs;
use chrono::Utc;
use feed_rs::parser;
use futures::stream::StreamExt;
use lazy_static::lazy_static;
use log::error;
use mdcat::{push_tty, Environment, ResourceAccess, Settings, TerminalCapabilities, TerminalSize};
use pulldown_cmark::{Options, Parser};
use qrcodegen::{QrCode, QrCodeEcc};
use rand::prelude::{IteratorRandom, SliceRandom, ThreadRng};
use rand::Rng;
use reqwest::Client;
use rusqlite::{params, Connection};
use select::document::Document;
use select::predicate::Class;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use syntect::parsing::SyntaxSet;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time;
use twilight_embed_builder::{EmbedAuthorBuilder, EmbedBuilder, ImageSource};
use twilight_gateway::{Event, EventTypeFlags, Shard};
use twilight_http::{request::channel::reaction::RequestReactionType, Client as HttpClient};
use twilight_model::channel::embed::{Embed, EmbedAuthor, EmbedFooter};
use twilight_model::channel::message::AllowedMentions;
use twilight_model::gateway::payload::incoming::MessageCreate;
use twilight_model::gateway::payload::outgoing::update_presence::UpdatePresencePayload;
use twilight_model::gateway::presence::{ActivityType, MinimalActivity, Status};
use twilight_model::http::attachment::Attachment;
use twilight_model::util::Timestamp;
use twilight_model::{gateway::Intents, id::Id};

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

async fn handle_message(
    msg: &MessageCreate,
    mut locked_state: MutexGuard<'_, State>,
    config: &Arc<Config>,
    http: &Arc<HttpClient>,
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
                Commands::QR(qr) => {
                    let qr = QrCode::encode_text(&qr.text.join(" "), QrCodeEcc::Low)?;
                    let res = print_qr(&qr);
                    let embed = EmbedBuilder::new()
                        .description(format!("```ansi\n{res}\n```"))
                        .build()?;
                    Ok(Command::embed(embed))
                }

                Commands::MD(md) => {
                    let env = &Environment::for_local_directory(&"/")?;
                    let settings = &Settings {
                        resource_access: ResourceAccess::LocalOnly,
                        syntax_set: SyntaxSet::load_defaults_newlines(),
                        terminal_capabilities: TerminalCapabilities::ansi(),
                        terminal_size: TerminalSize::default(),
                    };

                    let mut buf = Vec::new();
                    let text = md.text.join(" ");
                    let parser = Parser::new_ext(
                        &text,
                        Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH,
                    );
                    push_tty(settings, env, &mut buf, parser)?;
                    let res = String::from_utf8(buf.clone())?;
                    let size = res.len();
                    if size > 4050 {
                        Ok(
                            Command::text("Message exceeded discord limit send attachment!")
                                .attachments(vec![Attachment::from_bytes(
                                    "message.ansi".to_string(),
                                    buf.clone(),
                                    125,
                                )]),
                        )
                    } else if size > 2000 {
                        let embed = EmbedBuilder::new()
                            .description(format!("```ansi\n{res}\n```"))
                            .build()?;
                        Ok(Command::embed(embed))
                    } else {
                        Ok(Command::text(format!("```ansi\n{res}\n```")))
                    }
                }
                Commands::Zip(_) => {
                    let size = msg.attachments.iter().map(|x| x.size).sum::<u64>();
                    if size > 6000000 {
                        return Ok(Command::text("Too big".to_string()));
                    }
                    let mut buf = Vec::new();

                    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));

                    let options = zip::write::FileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored);
                    for attachment in &msg.attachments {
                        zip.start_file(attachment.filename.clone(), options)?;
                        zip.write_all(
                            &locked_state
                                .client
                                .get(&attachment.url)
                                .send()
                                .await?
                                .bytes()
                                .await?,
                        )?;
                    }

                    let res = zip.finish()?;

                    Ok(Command::text("Zip files arrived").attachments(vec![
                        Attachment::from_bytes(
                            format!("files-{}.zip", msg.id.get()),
                            (*res.get_ref()).clone(),
                            125,
                        ),
                    ]))
                }
                Commands::Search(Search { query }) => {
                    let res = locked_state
                        .client
                        .get(format!(
                            "https://duckduckgo.com/html/?q={}",
                            query.join("-")
                        ))
                        .send()
                        .await?
                        .text()
                        .await?;
                    let document = Document::from(res.as_str());
                    let embeds =
                        document
                            .find(Class("result__body"))
                            .take(5)
                            .map(|node| -> Option<Embed> {
                                let url_node = node.find(Class("result__a")).next()?;
                                let url = url_node
                                    .attr("href")?
                                    .replace("//duckduckgo.com", "https://duckduckgo.com");
                                let title = url_node.inner_html();
                                let snippet = node
                                    .find(Class("result__snippet"))
                                    .next()?
                                    .inner_html()
                                    .replace("<b>", "**")
                                    .replace("</b>", "**")
                                    .split_whitespace()
                                    .collect::<Vec<&str>>()
                                    .join(" ");

                                let icon = node
                                    .find(Class("result__icon__img"))
                                    .next()?
                                    .attr("src")?
                                    .replace(
                                        "//external-content.duckduckgo.com",
                                        "https://external-content.duckduckgo.com",
                                    );

                                let preview_url =
                                    node.find(Class("result__url")).next()?.inner_html();
                                EmbedBuilder::new()
                                    .title(title)
                                    .url(url)
                                    .color(0x179e87)
                                    .description(snippet)
                                    .author(
                                        EmbedAuthorBuilder::new(preview_url)
                                            .icon_url(ImageSource::url(icon).ok()?),
                                    )
                                    .build()
                                    .ok()
                            });
                    let embeds: Vec<Embed> = embeds.flatten().collect();
                    Ok(if embeds.is_empty() {
                        Command::text("Nothing found (or i am blocked)")
                    } else {
                        Command::embeds(embeds)
                    })
                }
            };
        } else {
            return Ok(Command::text(format!(
                "```\n{}\n```",
                commands.err().unwrap().output
            )));
        }
    }

    match msg.content.to_lowercase().as_str() {
        content
            if locked_state.last_redesc.elapsed() > std::time::Duration::from_secs(150)
                && config
                    .rename_channels
                    .clone()
                    .contains(&msg.channel_id.get())
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
        x if x.contains("im") => {
            let text = x.split("im").last().unwrap().trim();
            if text.is_empty() {
                return Ok(Command::nothing());
            }

            Ok(Command::text(format!("Hi {text} i'm Tricked-bot")).reply())
        }
        x if locked_state.rng.gen_range(0..45) == 2 => {
            let content = zalgify_text(locked_state.rng.clone(), x.to_owned());
            Ok(Command::text(content).reply())
        }
        x if locked_state.rng.gen_range(0..35) == 2 => {
            let mut text = x.split(' ').collect::<Vec<&str>>();
            text.shuffle(&mut locked_state.rng.clone());
            Ok(Command::text(text.join(" ")).reply())
        }
        _ if locked_state.rng.gen_range(0..20) == 2 => {
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
        _ => Ok(Command::nothing()),
    }
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

use zalgos::*;
pub(crate) fn zalgify_text(mut rng: ThreadRng, s: String) -> String {
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

mod zalgos {
    pub(crate) const ZALGO_UP: [char; 50] = [
        '\u{030e}', /*    ̎    */ '\u{0304}',
        /*    ̄    */ '\u{0305}', /*    ̅    */
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
        '\u{0346}', /*    ͆    */ '\u{031a}',
        /*    ̚    */ '\u{030d}', /*    ̍    */
    ];

    pub(crate) const ZALGO_DOWN: [char; 40] = [
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

    pub(crate) const ZALGO_MID: [char; 23] = [
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
}

mod structs {
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
    pub(crate) struct TrickedCommands {
        #[argh(subcommand)]
        pub(crate) nested: Commands,
    }

    #[derive(FromArgs, PartialEq, Eq, Debug)]
    #[argh(subcommand)]
    pub(crate) enum Commands {
        QR(QR),
        MD(MD),
        Zip(Zip),
        Search(Search),
    }

    #[derive(FromArgs, PartialEq, Eq, Debug)]
    #[argh(subcommand, name = "qr")]
    /// create a qrcode from text!
    pub(crate) struct QR {
        #[argh(positional)]
        /// the text to be qr-d
        pub(crate) text: Vec<String>,
    }
    #[derive(FromArgs, PartialEq, Eq, Debug)]
    #[argh(subcommand, name = "md")]
    /// turn text into a markdown ansiL
    pub(crate) struct MD {
        #[argh(positional)]
        /// the text to be marked!
        pub(crate) text: Vec<String>,
    }

    #[derive(FromArgs, PartialEq, Eq, Debug)]
    #[argh(subcommand, name = "search")]
    /// search for things on ddg
    pub(crate) struct Search {
        #[argh(positional)]
        /// query to be searched
        pub(crate) query: Vec<String>,
    }

    #[derive(FromArgs, PartialEq, Eq, Debug)]
    #[argh(subcommand, name = "zip")]
    /// zip some files they must be attachments!
    pub(crate) struct Zip {}

    #[derive(PartialEq, Eq, Default, Clone)]
    pub(crate) struct Command {
        pub(crate) embeds: Vec<Embed>,
        pub(crate) text: Option<String>,
        pub(crate) reply: bool,
        pub(crate) reaction: Option<char>,
        pub(crate) attachments: Vec<Attachment>,
        pub(crate) skip: bool,
    }

    impl Command {
        pub(crate) fn embed(embed: Embed) -> Self {
            Self {
                embeds: vec![embed],
                ..Self::default()
            }
        }
        pub(crate) fn embeds(embeds: Vec<Embed>) -> Self {
            Self {
                embeds,
                ..Self::default()
            }
        }
        pub(crate) fn text<T: Into<String>>(text: T) -> Self {
            Self {
                text: Some(text.into()),
                ..Self::default()
            }
        }
        pub(crate) fn react(reaction: char) -> Self {
            Self {
                reaction: Some(reaction),
                ..Self::default()
            }
        }
        pub(crate) fn nothing() -> Self {
            Self {
                skip: true,
                ..Self::default()
            }
        }
        pub(crate) fn reply(mut self) -> Self {
            self.reply = true;
            self
        }

        pub(crate) fn attachments(mut self, attachments: Vec<Attachment>) -> Self {
            self.attachments = attachments;
            self
        }
    }

    /// This pub(crate)struct is needed to deal with the invite create event.
    #[derive(Clone)]
    pub(crate) struct BotInvite {
        pub(crate) code: String,
        pub(crate) uses: Option<u64>,
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
    pub(crate) struct State {
        pub(crate) last_redesc: Instant,
        pub(crate) rng: ThreadRng,
        pub(crate) client: Client,
        pub(crate) user_bucket: Bucket,
        pub(crate) channel_bucket: Bucket,
        pub(crate) db: Connection,
        pub(crate) invites: Vec<BotInvite>,
    }

    impl State {
        pub(crate) fn new(rng: ThreadRng, client: Client, db: Connection) -> Self {
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
    pub(crate) struct Config {
        pub(crate) token: String,
        pub(crate) discord: u64,
        pub(crate) join_channel: u64,
        pub(crate) rename_channels: Vec<u64>,
        pub(crate) invites: HashMap<String, String>,
        pub(crate) shit_reddits: Vec<String>,
        pub(crate) rss_feeds: Vec<String>,
        pub(crate) status: String,
    }
    #[derive(Clone, Debug, Serialize, Deserialize, Default)]

    pub(crate) struct Responder {
        pub(crate) message: Option<String>,
        pub(crate) react: Option<String>,
    }

    #[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct List {
        pub(crate) data: Data,
    }

    #[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Data {
        pub(crate) children: Vec<Children>,
    }

    #[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Children {
        pub(crate) data: Data2,
    }

    #[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Data2 {
        #[serde(rename = "url_overridden_by_dest")]
        pub(crate) url_overridden_by_dest: String,
        #[serde(rename = "over_18")]
        pub(crate) over_18: bool,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use select::{document::Document, predicate::Class};

    #[test]
    fn test_scrapping_ddg() {
        let document = Document::from(include_str!("../tests/ddg.html"));
        for node in document.find(Class("result__body")).take(10) {
            let _url = node
                .find(Class("result__a"))
                .next()
                .unwrap()
                .attr("href")
                .unwrap()
                .replace("//duckduckgo.com", "https://duckduckgo.com");
            let _snippet = node
                .find(Class("result__snippet"))
                .next()
                .unwrap()
                .inner_html()
                .replace("<b>", "**")
                .replace("</b>", "**")
                .split_whitespace()
                .collect::<Vec<&str>>()
                .join(" ");
            let icon = node
                .find(Class("result__icon__img"))
                .next()
                .unwrap()
                .attr("src")
                .unwrap()
                .replace(
                    "//external-content.duckduckgo.com",
                    "https://external-content.duckduckgo.com",
                );

            let url = node.find(Class("result__url")).next().unwrap().inner_html();
            println!("{icon} {url}");
        }
    }
}
