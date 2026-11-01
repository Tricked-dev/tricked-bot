//! Running AI replies: one in-flight reply per author and channel, streamed into a Discord message.

use std::{
    collections::{HashMap, hash_map::Entry},
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use color_eyre::{Result, eyre::eyre};
use futures_util::{Stream, StreamExt};
use tokio::task::AbortHandle;
use twilight_http::Client as HttpClient;
use twilight_model::id::{
    Id,
    marker::{ChannelMarker, MessageMarker, UserMarker},
};

use super::{AiRequest, output, prompt, recall, refusal};
use crate::{bot::Bot, db};

/// A newer message from the same person replaces their unfinished reply, at most this many times per chain.
/// An edit to the text being answered also counts as a restart.
const MAX_RESTARTS: u8 = 2;
/// How often a streaming reply is edited with newly generated text.
const EDIT_INTERVAL: Duration = Duration::from_millis(1500);
/// How long the model may take to finish a reply.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(120);
/// Prefix of the messages that stand in for a reply that failed.
const AI_ERROR: &str = "AI Error";

type PostedSlot = Mutex<Option<Id<MessageMarker>>>;
type ReplyKey = (Id<ChannelMarker>, Id<UserMarker>);

#[derive(Debug)]
struct InFlight {
    message_id: Id<MessageMarker>,
    /// The text being answered, so an edit that only resolves link previews leaves the reply alone.
    message: String,
    generation: u64,
    restarts: u8,
    abort: AbortHandle,
    posted: Arc<PostedSlot>,
}

/// Unfinished replies, keyed by channel and author.
#[derive(Debug, Default)]
pub struct ReplyTasks {
    in_flight: Mutex<HashMap<ReplyKey, InFlight>>,
    generation: AtomicU64,
}

impl InFlight {
    /// Whether `req` replaces this reply: it answers a newer message, or this message after its text was edited.
    fn replaced_by(&self, req: &AiRequest) -> bool {
        let edited = self.message_id == req.message_id && self.message != req.message;
        (self.message_id < req.message_id || edited) && self.restarts < MAX_RESTARTS
    }
}

impl ReplyTasks {
    fn finish(&self, key: ReplyKey, generation: u64) {
        let mut in_flight = self.in_flight.lock().unwrap();
        if in_flight.get(&key).is_some_and(|entry| entry.generation == generation) {
            in_flight.remove(&key);
        }
    }
}

/// Aborts an unfinished reply and deletes whatever part of it was already posted, returning that part.
fn cancel(bot: &Arc<Bot>, entry: &InFlight, channel: Id<ChannelMarker>) -> Option<Id<MessageMarker>> {
    entry.abort.abort();
    let posted = *entry.posted.lock().unwrap();
    if let Some(posted) = posted {
        let bot = Arc::clone(bot);
        tokio::spawn(async move {
            let _ = bot.http.delete_message(channel, posted).await;
        });
    }
    posted
}

/// Cancels the unfinished replies to any of the `deleted` messages in `channel`. Returns the partial replies that are
/// being deleted along with them.
pub fn cancel_answering(
    bot: &Arc<Bot>,
    channel: Id<ChannelMarker>,
    deleted: &[Id<MessageMarker>],
) -> Vec<Id<MessageMarker>> {
    let mut in_flight = bot.replies.in_flight.lock().unwrap();
    let mut posted = Vec::new();
    in_flight.retain(|&(entry_channel, _), entry| {
        if entry_channel != channel || !deleted.contains(&entry.message_id) {
            return true;
        }
        posted.extend(cancel(bot, entry, channel));
        tracing::info!(target: "decider", kind = "cancel", deleted = %entry.message_id);
        false
    });
    posted
}

/// Answers `req`, replacing the author's unfinished reply to an older message, or to the same message before its text
/// was edited. Without an unfinished reply, one only starts when `start_new` is set. Returns whether a reply to `req`
/// started.
///
/// Events are handled concurrently, so an older message can arrive here after a newer one. Snowflakes are
/// chronological, which makes "newer" a plain ID comparison.
pub fn submit(bot: &Arc<Bot>, req: &AiRequest, start_new: bool) -> bool {
    let key = (req.channel_id, req.user_id);
    let mut in_flight = bot.replies.in_flight.lock().unwrap();
    let restarts = match in_flight.entry(key) {
        Entry::Vacant(_) if start_new => 0,
        Entry::Occupied(entry) if entry.get().replaced_by(req) => {
            let superseded = entry.remove();
            cancel(bot, &superseded, req.channel_id);
            let restarts = superseded.restarts + 1;
            tracing::info!(target: "decider", kind = "restart", superseded = %superseded.message_id, restarts);
            restarts
        }
        _ => return false,
    };
    let generation = bot.replies.generation.fetch_add(1, Ordering::Relaxed);
    let posted = Arc::default();
    let abort = tokio::spawn(run(Arc::clone(bot), req.clone(), Arc::clone(&posted), generation)).abort_handle();
    let entry =
        InFlight { message_id: req.message_id, message: req.message.clone(), generation, restarts, abort, posted };
    in_flight.insert(key, entry);
    true
}

async fn run(bot: Arc<Bot>, req: AiRequest, posted: Arc<PostedSlot>, generation: u64) {
    let key = (req.channel_id, req.user_id);
    let result = reply(&bot, &req, &posted, generation).await;
    bot.replies.finish(key, generation);
    if let Err(error) = result {
        tracing::error!(target: "decider", kind = "reply", msg_id = %req.message_id, "AI reply failed: {error}");
        let _ = bot
            .http
            .create_message(req.channel_id)
            .content(&format!("{AI_ERROR}: reply or memory recall unavailable."))
            .reply(req.message_id)
            .await;
    }
}

async fn reply(bot: &Bot, req: &AiRequest, posted: &PostedSlot, generation: u64) -> Result<()> {
    let ai = bot.ai.as_ref().ok_or_else(|| eyre!("OpenRouter API key not configured"))?;
    // Memory recall waits on the decider, so the profile lookup and image downloads run alongside it.
    let user = async {
        db::ensure_user(&bot.db, req.user_id.get(), &req.user_name).await?;
        db::user(&bot.db, req.user_id.get()).await
    };
    let (memories, user, turn) = tokio::try_join!(recall::select(bot, req), user, prompt::user_turn(bot, req))?;
    let request =
        prompt::build_request(&bot.config, req, user.as_ref(), &memories, turn, &bot.config.openrouter_model)?;
    let deltas = ai.stream(&request, REPLY_TIMEOUT).await?;
    let posted = stream_to_discord(&bot.http, deltas, req.channel_id, req.message_id, posted).await;
    // A complete reply stays; newer messages no longer cancel it.
    bot.replies.finish((req.channel_id, req.user_id), generation);
    if let Some(posted) = posted {
        refusal::check_and_replace(bot, req, user.as_ref(), &memories, posted).await;
    }
    Ok(())
}

/// Posts the reply as soon as it has presentable text, then edits it at most every [`EDIT_INTERVAL`] and once more at
/// the end. A stream error replaces the text with the error.
///
/// Returns the posted message and the last text Discord accepted, or `None` when nothing was posted or the stream
/// failed.
pub async fn stream_to_discord(
    http: &HttpClient,
    deltas: impl Stream<Item = Result<String>>,
    channel_id: Id<ChannelMarker>,
    reply_to: Id<MessageMarker>,
    posted_slot: &PostedSlot,
) -> Option<(Id<MessageMarker>, String)> {
    let mut deltas = pin!(deltas);
    let mut raw = String::new();
    let mut failure = None;
    let mut changed = false;
    let mut message_id = None;
    let mut last_sent = String::new();
    let mut tick = tokio::time::interval(EDIT_INTERVAL);
    loop {
        let (finished, edit_due) = tokio::select! {
            next = deltas.next() => match next {
                Some(Ok(delta)) => {
                    raw.push_str(&delta);
                    changed = true;
                    (false, false)
                }
                Some(Err(error)) => {
                    tracing::error!("AI stream failed: {error}");
                    failure = Some(output::discord_text(&format!("{AI_ERROR}: {error}")));
                    changed = true;
                    (true, false)
                }
                None => (true, false),
            },
            _ = tick.tick() => (false, true),
        };
        // Cleaning up the text is linear in its length, so it only happens when the result is about to be sent.
        if changed && (finished || edit_due || message_id.is_none()) {
            changed = false;
            let text = failure.clone().or_else(|| output::presentable(&raw)).unwrap_or_default();
            if !text.trim().is_empty() && text != last_sent {
                if let Some(id) = message_id {
                    match http.update_message(channel_id, id).content(Some(&text)).await {
                        Ok(_) => last_sent = text,
                        Err(error) => {
                            tracing::warn!("AI message update failed: {error}");
                            // Retry on the next tick or the final flush, even if no more text arrives.
                            changed = true;
                        }
                    }
                } else {
                    let id = post(http, channel_id, reply_to, &text).await?;
                    message_id = Some(id);
                    *posted_slot.lock().unwrap() = Some(id);
                    last_sent = text;
                }
            }
        }
        if finished {
            return message_id.filter(|_| failure.is_none()).map(|id| (id, last_sent));
        }
    }
}

/// Posts `text` as a reply to `reply_to`, returning the new message.
async fn post(
    http: &HttpClient,
    channel_id: Id<ChannelMarker>,
    reply_to: Id<MessageMarker>,
    text: &str,
) -> Option<Id<MessageMarker>> {
    let response = http
        .create_message(channel_id)
        .content(text)
        .reply(reply_to)
        .await
        .inspect_err(|error| tracing::warn!("AI initial message failed: {error}"))
        .ok()?;
    let message =
        response.model().await.inspect_err(|error| tracing::warn!("AI posted message decode failed: {error}"));
    message.ok().map(|message| message.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::tests::{message, test_bot};
    use axum::{Json, Router, extract::State, http::Method, http::StatusCode, routing::any};
    use futures_util::stream;
    use serde_json::{Value, json};

    type Seen = Arc<tokio::sync::Mutex<Vec<String>>>;

    async fn mock_discord(
        State(seen): State<Seen>,
        method: Method,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let content = body["content"].as_str().unwrap().to_owned();
        seen.lock().await.push(content.clone());
        if method == Method::PATCH && content.ends_with("failure") {
            return (StatusCode::BAD_REQUEST, Json(json!({"code": 50035, "message": "invalid"})));
        }
        (
            StatusCode::OK,
            Json(json!({
                "id": "42", "channel_id": "1", "content": content, "timestamp": "2026-09-26T00:00:00.000000+00:00",
                "author": {"id": "3", "username": "bot", "discriminator": "0001", "avatar": null},
                "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
                "embeds": [], "pinned": false, "type": 0
            })),
        )
    }

    async fn run_stream(deltas: Vec<Result<String>>) -> (Option<(Id<MessageMarker>, String)>, Vec<String>) {
        let seen = Seen::default();
        let app = Router::new().fallback(any(mock_discord)).with_state(Arc::clone(&seen));
        let address = crate::test_util::serve(app).await;
        let http = HttpClient::builder()
            .token("test-token".to_owned())
            .proxy(address.to_string(), true)
            .ratelimiter(None)
            .build();
        let result = stream_to_discord(&http, stream::iter(deltas), Id::new(1), Id::new(2), &Mutex::default()).await;
        let requests = seen.lock().await.clone();
        (result, requests)
    }

    fn deltas(texts: &[&str]) -> Vec<Result<String>> {
        texts.iter().map(|text| Ok((*text).to_owned())).collect()
    }

    fn in_flight(message_id: u64, message: &str, restarts: u8) -> InFlight {
        InFlight {
            message_id: Id::new(message_id),
            message: message.to_owned(),
            generation: 0,
            restarts,
            abort: tokio::spawn(async {}).abort_handle(),
            posted: Arc::default(),
        }
    }

    #[tokio::test]
    async fn newer_messages_and_text_edits_replace_a_reply() {
        let bot = test_bot();
        let entry = in_flight(10, "hello", 0);
        assert!(entry.replaced_by(&bot.snapshot(&message(11, 2, "alice", "more"))));
        assert!(entry.replaced_by(&bot.snapshot(&message(10, 2, "alice", "hello there"))));
        assert!(!entry.replaced_by(&bot.snapshot(&message(10, 2, "alice", "hello"))), "only a link preview resolved");
        assert!(!entry.replaced_by(&bot.snapshot(&message(9, 2, "alice", "older"))));
        assert!(!in_flight(10, "hello", MAX_RESTARTS).replaced_by(&bot.snapshot(&message(10, 2, "alice", "edited"))));
    }

    #[tokio::test]
    async fn completed_short_stream_is_posted_presentably() {
        let (result, requests) = run_stream(deltas(&["The Trickster: Hi."])).await;
        assert_eq!(result.unwrap().1, "hi");
        assert_eq!(requests, ["hi"]);
    }

    #[tokio::test]
    async fn completed_stream_flushes_final_text() {
        let (result, requests) = run_stream(deltas(&["Hi", " there"])).await;
        assert_eq!(result.unwrap().1, "hi there");
        assert_eq!(requests, ["hi", "hi there"]);
    }

    #[tokio::test]
    async fn failed_final_edit_returns_last_successfully_posted_text() {
        let (result, _) = run_stream(deltas(&["Hi", " final failure"])).await;
        assert_eq!(result.unwrap().1, "hi");
    }

    #[tokio::test]
    async fn stream_error_replaces_the_reply_and_reports_no_reply() {
        let mut stream = deltas(&["Hi"]);
        stream.push(Err(eyre!("overloaded")));
        let (result, requests) = run_stream(stream).await;
        assert!(result.is_none());
        assert_eq!(requests, ["hi", "AI Error: overloaded"]);
    }
}
