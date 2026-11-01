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
        Event::MessageUpdate(msg) if !msg.author.bot => message_edit(&bot, &msg).await,
        Event::MessageDelete(deleted) => messages_deleted(&bot, deleted.channel_id, &[deleted.id]).await,
        Event::MessageDeleteBulk(deleted) => messages_deleted(&bot, deleted.channel_id, &deleted.ids).await,
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
    let req = ai::voice::prepare(bot, msg).await?;
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
    let req = ai::voice::prepare(bot, msg).await?;
    // A newer message from someone still waiting on a reply restarts it, whether or not it pings the bot.
    let restarted = ai::reply::submit(bot, &req, false);
    let response = ai::timing::measure(msg.id, "responders", messages::handle(bot, msg, &req, direct)).await;
    // Dispatch before sending a responder result, so even failed sends retain the durable check.
    let answered = restarted || direct || response.as_ref().map_or(true, |response| *response != Response::Nothing);
    let ask_reply = !answered && bot.ai.is_some() && bot.config.decider_reply_mode != ReplyMode::Off;
    check::spawn(Arc::clone(bot), req, ask_reply);
    send(bot, msg, response?).await
}

/// Editing the text of a message that is still being answered restarts the reply. Finished replies stay as they are.
async fn message_edit(bot: &Arc<Bot>, msg: &Message) -> Result<()> {
    let req = ai::voice::prepare(bot, msg).await?;
    ai::reply::submit(bot, &req, false);
    Ok(())
}

/// Deleting a message takes the bot's replies to it along: an unfinished one is cancelled and finished ones deleted.
async fn messages_deleted(bot: &Arc<Bot>, channel: Id<ChannelMarker>, deleted: &[Id<MessageMarker>]) -> Result<()> {
    let cancelled = ai::reply::cancel_answering(bot, channel, deleted);
    for reply in replies_to(bot, channel, deleted).into_iter().filter(|reply| !cancelled.contains(reply)) {
        if let Err(error) = bot.http.delete_message(channel, reply).await {
            tracing::warn!(channel = %channel, msg_id = %reply, "Deleting a reply to a deleted message failed: {error}");
        }
    }
    Ok(())
}

/// The bot's cached messages in `channel` that reply to one of `targets`.
fn replies_to(bot: &Bot, channel: Id<ChannelMarker>, targets: &[Id<MessageMarker>]) -> Vec<Id<MessageMarker>> {
    let (Some(ids), Some(oldest)) = (bot.cache.channel_messages(channel), targets.iter().min()) else {
        return Vec::new();
    };
    // Snowflakes are chronological, so only messages newer than a target can reply to it.
    ids.iter()
        .copied()
        .filter(|id| id > oldest)
        .filter(|id| {
            bot.cache.message(*id).is_some_and(|message| {
                message.author() == bot.id
                    && message
                        .reference()
                        .and_then(|reference| reference.message_id)
                        .is_some_and(|target| targets.contains(&target))
            })
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::tests::{message, test_bot};
    use twilight_model::{
        channel::message::{MessageReference, MessageReferenceType},
        gateway::payload::incoming::MessageCreate,
    };

    fn reply(id: u64, author: u64, to: u64) -> Message {
        let mut reply = message(id, author, "someone", "reply");
        reply.reference = Some(MessageReference {
            channel_id: Some(Id::new(1)),
            guild_id: None,
            kind: MessageReferenceType::Default,
            message_id: Some(Id::new(to)),
            fail_if_not_exists: None,
        });
        reply
    }

    #[tokio::test]
    async fn only_the_bots_replies_to_a_deleted_message_are_found() {
        let bot = test_bot();
        let cached = vec![
            message(10, 2, "alice", "question"),
            reply(11, 3, 10),
            message(12, 3, "bot", "unrelated"),
            reply(13, 2, 10),
            reply(14, 3, 12),
            reply(15, 3, 10),
        ];
        for msg in cached {
            bot.cache.update(&Event::MessageCreate(Box::new(MessageCreate(msg))));
        }
        let mut found = replies_to(&bot, Id::new(1), &[Id::new(10)]);
        found.sort_unstable();
        assert_eq!(found, [Id::new(11), Id::new(15)]);
        assert!(replies_to(&bot, Id::new(2), &[Id::new(10)]).is_empty(), "other channels are not searched");
    }
}
