use std::{collections::HashMap, fmt::Write, sync::Arc, time::Duration};

use color_eyre::Result;
use twilight_gateway::Event;
use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_model::{
    channel::message::AllowedMentions,
    channel::{Attachment, Message, message::Embed},
    gateway::payload::incoming::TypingStart,
    id::{
        Id,
        marker::{ChannelMarker, MessageMarker, UserMarker},
    },
};

use crate::{
    ai,
    bot::Bot,
    config::ReplyMode,
    decider::check,
    messages::{self, Response},
};

/// Waiting out a user rate limit is preferred over dropping the message when it is about to lift.
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(5);

pub async fn handle(bot: Arc<Bot>, event: Event) {
    log_message_event(&event);
    let result = match event {
        Event::InteractionCreate(interaction) => crate::commands::handle(&bot, interaction.0).await,
        Event::MessageCreate(msg) if !msg.author.bot => match msg.guild_id {
            Some(_) => guild_message(&bot, &msg.0).await,
            None => direct_message(&bot, &msg.0).await,
        },
        Event::TypingStart(typing) => typing_start(&bot, &typing).await,
        Event::Ready(_) => {
            tracing::info!("Connected");
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(error) = result {
        tracing::error!("Error handling event: {error:?}");
    }
}

async fn direct_message(bot: &Arc<Bot>, msg: &Message) -> Result<()> {
    if let Some(wait) = bot.dm_limit.hit(msg.author.id.get()) {
        tracing::info!(user = %msg.author.id, "DM rate limit reached, {} seconds remaining", wait.as_secs());
        let notice = format!(
            "You've reached the DM rate limit. Please wait {} seconds before sending more messages.",
            wait.as_secs()
        );
        bot.http.create_message(msg.channel_id).content(&notice).reply(msg.id).await?;
        return Ok(());
    }
    let req = bot.snapshot(msg);
    if bot.ai.is_some() {
        ai::reply::submit(bot, &req, true);
    }
    check::spawn(Arc::clone(bot), req, false);
    Ok(())
}

async fn guild_message(bot: &Arc<Bot>, msg: &Message) -> Result<()> {
    if bot.config.today_i_channel == Some(msg.channel_id.get()) && !msg.content.to_lowercase().starts_with("today i") {
        bot.http.delete_message(msg.channel_id, msg.id).await?;
        return Ok(());
    }
    if let Some(wait) = bot.channel_limit.hit(msg.channel_id.get()) {
        tracing::info!(channel = %msg.channel_id, "Channel limit reached, {} seconds remaining", wait.as_secs());
        return Ok(());
    }
    // A message that waited competes for the next window like any other, so a queued burst can't all slip through.
    while let Some(wait) = bot.user_limit.hit(msg.author.id.get()) {
        tracing::info!(user = %msg.author.id, "User limit reached, {} seconds remaining", wait.as_secs());
        if wait > MAX_RATE_LIMIT_WAIT {
            return Ok(());
        }
        tokio::time::sleep(wait).await;
    }

    let direct = ai::is_direct(msg, bot.id);
    if !direct {
        bot.reply_gate.lock().unwrap().count(msg.channel_id.get());
        tracing::info!(target: "decider", kind = "count", channel = %msg.channel_id, msg_id = %msg.id);
    }
    let req = bot.snapshot(msg);
    // A newer message from someone still waiting on a reply restarts it, whether or not it pings the bot.
    let restarted = ai::reply::submit(bot, &req, false);
    let response = messages::handle(bot, msg, &req, direct).await;
    // Dispatch before sending a responder result, so even failed sends retain the durable check.
    let answered = restarted || direct || response.as_ref().map_or(true, |response| *response != Response::Nothing);
    let ask_reply = !answered && bot.ai.is_some() && bot.config.decider_reply_mode != ReplyMode::Off;
    check::spawn(Arc::clone(bot), req, ask_reply);
    send(bot, msg, response?).await
}

async fn send(bot: &Bot, msg: &Message, response: Response) -> Result<()> {
    match response {
        Response::Nothing => {}
        Response::React(emoji) => {
            bot.http.create_reaction(msg.channel_id, msg.id, &RequestReactionType::Unicode { name: emoji }).await?;
        }
        Response::Message(outgoing) => {
            let mentions = AllowedMentions { users: vec![msg.author.id], ..Default::default() };
            let mut request =
                bot.http.create_message(msg.channel_id).content(&outgoing.text).attachments(&outgoing.attachments);
            if outgoing.reply {
                request = request.reply(msg.id);
            }
            if outgoing.mention_author {
                request = request.allowed_mentions(Some(&mentions));
            }
            request.await?;
        }
    }
    Ok(())
}

/// The "<name> is typing" gag: the last indicator posted per channel, and who it was about.
#[derive(Debug, Default)]
pub struct TypingIndicator {
    last_typer: Option<Id<UserMarker>>,
    posted: HashMap<Id<ChannelMarker>, Id<MessageMarker>>,
}

async fn typing_start(bot: &Bot, event: &TypingStart) -> Result<()> {
    let Some(member) = &event.member else { return Ok(()) };
    if !rand::random_ratio(1, 100) {
        return Ok(());
    }
    // Claim the announcement before awaiting, so concurrent typing events can't both post one.
    let previous = {
        let mut typing = bot.typing.lock().unwrap();
        if typing.last_typer == Some(event.user_id) {
            return Ok(());
        }
        typing.last_typer = Some(event.user_id);
        typing.posted.remove(&event.channel_id)
    };
    let announce = format!("{} is typing", member.user.name);
    let (posted, ()) = tokio::join!(bot.http.create_message(event.channel_id).content(&announce), async {
        if let Some(previous) = previous {
            let _ = bot.http.delete_message(event.channel_id, previous).await;
        }
    });
    let posted = posted?.model().await?;
    bot.typing.lock().unwrap().posted.insert(event.channel_id, posted.id);
    Ok(())
}

fn log_message_event(event: &Event) {
    match event {
        Event::MessageCreate(msg) => {
            tracing::info!(target: "messages", channel = %msg.channel_id, msg_id = %msg.id,
                author = %msg.author.name, bot = msg.author.bot,
                "Message received {}", logged_content(&msg.content, &msg.attachments, &msg.embeds));
        }
        Event::MessageUpdate(msg) => {
            tracing::info!(target: "messages", channel = %msg.channel_id, msg_id = %msg.id,
                "Message edited {}", logged_content(&msg.content, &msg.attachments, &msg.embeds));
        }
        _ => {}
    }
}

fn logged_content(content: &str, attachments: &[Attachment], embeds: &[Embed]) -> String {
    let mut text = content.replace('\n', "\\ ");
    for attachment in attachments {
        let _ = write!(text, " [attachment: {} {}]", attachment.filename.replace('\n', " "), attachment.url);
    }
    for url in embeds.iter().filter_map(|embed| embed.url.as_deref()) {
        let _ = write!(text, " [embed: {url}]");
    }
    text
}
