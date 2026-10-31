//! What the bot does with an ordinary guild message, short of the AI deciding to reply.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use color_eyre::Result;
use rand::{
    random_range, random_ratio,
    seq::{IndexedRandom, IteratorRandom, SliceRandom},
};
use serde::Deserialize;
use twilight_model::{channel::Message, http::attachment::Attachment};

use crate::{
    ai::{self, AiRequest},
    bot::Bot,
    db, qalc, quiz, ratewaifu, zalgo,
};

/// What to send back in response to a message.
#[derive(Debug, Default, PartialEq)]
pub enum Response {
    #[default]
    Nothing,
    React(&'static str),
    Message(Outgoing),
}

#[derive(Debug, Default, PartialEq)]
pub struct Outgoing {
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// Send as a Discord reply to the message.
    pub reply: bool,
    /// Allow the author to be pinged by mentions in the text.
    pub mention_author: bool,
}

impl Response {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Message(Outgoing { text: text.into(), ..Outgoing::default() })
    }

    fn map_outgoing(mut self, update: impl FnOnce(&mut Outgoing)) -> Self {
        if let Self::Message(outgoing) = &mut self {
            update(outgoing);
        }
        self
    }

    pub fn reply(self) -> Self {
        self.map_outgoing(|outgoing| outgoing.reply = true)
    }

    pub fn mention_author(self) -> Self {
        self.map_outgoing(|outgoing| outgoing.mention_author = true)
    }

    pub fn attachments(self, attachments: Vec<Attachment>) -> Self {
        self.map_outgoing(|outgoing| outgoing.attachments = attachments)
    }
}

/// Canned answers to messages that consist of exactly one of these words.
fn responder(content: &str) -> Option<Response> {
    if content.len() > 5 {
        return None;
    }
    Some(match content.to_uppercase().as_str() {
        "L" => Response::text("+ ratio"),
        "DN" => Response::text("Digital Native"),
        "GN" => Response::text("https://www.youtube.com/watch?v=ykLDTsfnE5A"),
        "CLOWN" => Response::text("You are a clown"),
        "F" => Response::React("🇫"),
        "SKULL" => Response::React("💀"),
        "RUST" => Response::React("🦀"),
        "JAVA" => Response::React("🤮"),
        _ => return None,
    })
}

/// The outcome of handling a message.
pub struct Handled {
    pub response: Response,
    /// Whether an AI reply was started, which makes the background reply check redundant.
    pub ai_started: bool,
}

impl From<Response> for Handled {
    fn from(response: Response) -> Self {
        Self { response, ai_started: false }
    }
}

pub async fn handle(bot: &Arc<Bot>, msg: &Message, req: &AiRequest) -> Result<Handled> {
    if let Some(response) = responder(&msg.content) {
        return Ok(response.into());
    }
    if let Some(response) = quiz::answer(bot, msg).await {
        return Ok(response.into());
    }
    if let Some(response) = quiz::maybe_start(bot, msg).await {
        return Ok(response.into());
    }

    // Some extra XP for every attachment.
    let xp = random_range(5..20) + (0..msg.attachments.len()).map(|_| random_range(2..7)).sum::<i32>();
    if let Some(level) = db::award_xp(&bot.db, msg.author.id.get(), &msg.author.name, xp, false).await? {
        tokio::time::sleep(Duration::from_millis(random_range(3000..8000))).await;
        let congrats = format!("Congrats <@{}>! You are now level {level}!", msg.author.id);
        return Ok(Response::text(congrats).reply().mention_author().into());
    }

    if let Some(candidate) = ratewaifu::parse_command(&msg.content) {
        return Ok(rate_waifu(bot, candidate).await.reply().into());
    }

    let lower = msg.content.to_lowercase();
    if bot.config.rename_channels.contains(&msg.channel_id.get())
        && bot.last_topic_change.lock().unwrap().elapsed() > Duration::from_secs(150)
        && random_ratio(1, 10)
    {
        if lower.contains("uwu") || lower.contains("owo") {
            return Ok(Response::text("No furry shit!!!!!").into());
        }
        bot.http.update_channel(msg.channel_id).topic(&msg.content).await?;
        *bot.last_topic_change.lock().unwrap() = Instant::now();
        tracing::info!("Channel topic changed");
        return Ok(Response::Nothing.into());
    }
    if let Some(response) = dad_joke(&lower, &msg.content) {
        return Ok(response.into());
    }
    if bot.ai.is_some() && ai::is_direct(msg, bot.id) {
        ai::reply::spawn(bot, req.clone());
        return Ok(Handled { response: Response::Nothing, ai_started: true });
    }
    if random_ratio(1, 75) {
        return Ok(Response::text(zalgo::zalgify(&msg.content)).reply().into());
    }
    if random_ratio(1, 500) {
        steal_nickname(bot, msg).await?;
        return Ok(Response::Nothing.into());
    }
    if random_ratio(1, 55) {
        let mut words: Vec<_> = msg.content.split(' ').collect();
        words.shuffle(&mut rand::rng());
        return Ok(Response::text(words.join(" ")).reply().into());
    }
    if !bot.config.shit_reddits.is_empty() && random_ratio(1, 80) {
        return Ok(reddit_image(bot).await?.map_or(Response::Nothing, Response::text).into());
    }
    if let Some(expr) = lower.strip_prefix("qalc ").or_else(|| lower.strip_prefix("calc ")) {
        let text = qalc::eval(expr).await.unwrap_or_else(|error| format!("Error: {error}"));
        return Ok(Response::text(text).reply().into());
    }
    Ok(Response::Nothing.into())
}

async fn rate_waifu(bot: &Bot, candidate: &str) -> Response {
    if candidate.is_empty() {
        return Response::text("Usage: `t!ratewaifu <text>`");
    }
    let score = ratewaifu::score(candidate);
    let explanation = match &bot.ai {
        Some(ai) => ratewaifu::explanation(ai, &bot.config.openrouter_model, candidate, score)
            .await
            .inspect_err(|error| tracing::warn!("Waifu rating explanation failed: {error:?}"))
            .ok(),
        None => None,
    };
    let explanation = explanation.unwrap_or_else(|| ratewaifu::fallback_explanation(score).to_owned());
    Response::text(format!("**Waifu rating: {score}/10**\n{explanation}"))
}

/// "im tired" → "Hi tired i'm Tricked-bot", for short messages.
fn dad_joke(lower: &str, content: &str) -> Option<Response> {
    let separator = if lower.contains("im") {
        "im"
    } else if lower.contains("i am") {
        "i am"
    } else {
        return None;
    };
    if lower.split(' ').count() >= 4 || lower.contains("https://") {
        return None;
    }
    // Split the original text so the name keeps its casing; mixed-case separators fall back to the whole message.
    let name = content.rsplit(separator).next()?.trim();
    (!name.is_empty()).then(|| Response::text(format!("Hi {name} i'm Tricked-bot")).reply())
}

/// Renames the author after a random cached member of the guild.
async fn steal_nickname(bot: &Bot, msg: &Message) -> Result<()> {
    let Some(guild) = msg.guild_id else { return Ok(()) };
    let name = {
        let Some(members) = bot.cache.guild_members(guild) else {
            return Ok(());
        };
        let Some(&victim) = members.iter().choose(&mut rand::rng()) else {
            return Ok(());
        };
        let nick = bot.cache.member(guild, victim).and_then(|member| member.nick().map(str::to_owned));
        let Some(name) = nick.or_else(|| bot.cache.user(victim).map(|user| user.name.clone())) else {
            return Ok(());
        };
        name
    };
    bot.http.update_guild_member(guild, msg.author.id).nick(Some(&name)).await?;
    Ok(())
}

#[derive(Deserialize)]
struct Listing {
    data: ListingData,
}

#[derive(Deserialize)]
struct ListingData {
    children: Vec<Post>,
}

#[derive(Deserialize)]
struct Post {
    data: PostData,
}

#[derive(Deserialize)]
struct PostData {
    url_overridden_by_dest: Option<String>,
    over_18: bool,
}

/// A random safe-for-work image post from one of the configured subreddits.
async fn reddit_image(bot: &Bot) -> Result<Option<String>> {
    let Some(subreddit) = bot.config.shit_reddits.choose(&mut rand::rng()) else {
        return Ok(None);
    };
    let listing: Listing =
        bot.web.get(format!("https://www.reddit.com/r/{subreddit}/.json")).send().await?.json().await?;
    Ok(listing
        .data
        .children
        .into_iter()
        .filter(|post| !post.data.over_18)
        .filter_map(|post| post.data.url_overridden_by_dest)
        .filter(|url| url.contains("i."))
        .choose(&mut rand::rng()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responders_match_whole_messages_case_insensitively() {
        assert_eq!(responder("l"), Some(Response::text("+ ratio")));
        assert_eq!(responder("Skull"), Some(Response::React("💀")));
        assert_eq!(responder("lol"), None);
        assert_eq!(responder("clowns"), None);
    }

    #[test]
    fn dad_joke_only_answers_short_introductions() {
        let joke = |text: &str| dad_joke(&text.to_lowercase(), text);
        assert_eq!(joke("im Hungry"), Some(Response::text("Hi Hungry i'm Tricked-bot").reply()));
        assert_eq!(joke("i am tired"), Some(Response::text("Hi tired i'm Tricked-bot").reply()));
        assert_eq!(joke("im"), None);
        assert_eq!(joke("im going to the shop"), None);
        assert_eq!(joke("im https://example.com"), None);
        assert_eq!(joke("hello there"), None);
    }

    #[test]
    fn builder_only_touches_messages() {
        assert_eq!(Response::Nothing.reply(), Response::Nothing);
        let Response::Message(outgoing) = Response::text("hi").reply().mention_author() else { panic!() };
        assert!(outgoing.reply && outgoing.mention_author);
    }
}
