//! Running AI replies: one in-flight reply per author and channel, streamed into a Discord message.

use std::{
    collections::{BTreeMap, HashMap, hash_map::Entry},
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use color_eyre::{Result, eyre::eyre};
use futures_util::{Stream, StreamExt};
use tokio::task::AbortHandle;
use twilight_http::Client as HttpClient;
use twilight_model::id::{
    Id,
    marker::{ChannelMarker, MessageMarker, UserMarker},
};

use super::{AiRequest, gif, output, predictions, prompt, recall, refusal, timing, voice};
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

#[derive(Debug, Default)]
struct PostedState {
    ids: Vec<Id<MessageMarker>>,
    cancelled: bool,
}

type PostedSlot = Mutex<PostedState>;
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
    tasks: Mutex<Tasks>,
    generation: AtomicU64,
}

#[derive(Debug, Default)]
struct Tasks {
    in_flight: HashMap<ReplyKey, InFlight>,
    /// Finished text with ancillary work still running. New messages leave it alone; source deletion cancels it.
    finishing: HashMap<u64, (ReplyKey, InFlight)>,
    /// Recently finished multi-message replies, whose plain GIF has no Discord reply reference.
    completed: BTreeMap<Id<MessageMarker>, CompletedReply>,
}

#[derive(Debug)]
struct CompletedReply {
    channel: Id<ChannelMarker>,
    ids: Vec<Id<MessageMarker>>,
}

impl InFlight {
    /// Whether `req` replaces this reply: it answers a newer message, or this message after its text was edited.
    fn replaced_by(&self, req: &AiRequest) -> bool {
        let edited = self.message_id == req.message_id && self.message != req.message;
        (self.message_id < req.message_id || edited) && self.restarts < MAX_RESTARTS
    }
}

impl ReplyTasks {
    fn complete(&self, key: ReplyKey, generation: u64) {
        let mut tasks = self.tasks.lock().unwrap();
        if tasks.in_flight.get(&key).is_some_and(|entry| entry.generation == generation) {
            let entry = tasks.in_flight.remove(&key).unwrap();
            tasks.finishing.insert(generation, (key, entry));
        }
    }

    fn finish(&self, key: ReplyKey, generation: u64) {
        let mut tasks = self.tasks.lock().unwrap();
        let entry = if tasks.in_flight.get(&key).is_some_and(|entry| entry.generation == generation) {
            tasks.in_flight.remove(&key)
        } else {
            tasks.finishing.remove(&generation).map(|(_, entry)| entry)
        };
        if let Some(entry) = entry {
            let ids = entry.posted.lock().unwrap().ids.clone();
            if ids.len() > 1 {
                tasks.completed.insert(entry.message_id, CompletedReply { channel: key.0, ids });
                // Bound bookkeeping for completed GIFs, like Discord's finite message cache.
                while tasks.completed.len() > 1000 {
                    tasks.completed.pop_first();
                }
            }
        }
    }
}

/// Aborts an unfinished reply and deletes whatever part of it was already posted, returning their IDs.
fn cancel(bot: &Arc<Bot>, entry: &InFlight, channel: Id<ChannelMarker>) -> Vec<Id<MessageMarker>> {
    let posted = {
        let mut state = entry.posted.lock().unwrap();
        state.cancelled = true;
        std::mem::take(&mut state.ids)
    };
    entry.abort.abort();
    delete_posts(bot, channel, &posted);
    posted
}

fn delete_posts(bot: &Arc<Bot>, channel: Id<ChannelMarker>, ids: &[Id<MessageMarker>]) {
    for id in ids {
        let (bot, id) = (Arc::clone(bot), *id);
        tokio::spawn(async move {
            let _ = bot.http.delete_message(channel, id).await;
        });
    }
}

/// Cancels the unfinished replies to any of the `deleted` messages in `channel`. Returns the partial replies that are
/// being deleted along with them.
pub fn cancel_answering(
    bot: &Arc<Bot>,
    channel: Id<ChannelMarker>,
    deleted: &[Id<MessageMarker>],
) -> Vec<Id<MessageMarker>> {
    let mut tasks = bot.replies.tasks.lock().unwrap();
    let mut posted = Vec::new();
    let mut keep = |&(entry_channel, _): &ReplyKey, entry: &InFlight| {
        if entry_channel != channel || !deleted.contains(&entry.message_id) {
            return true;
        }
        posted.extend(cancel(bot, entry, channel));
        tracing::info!(target: "decider", kind = "cancel", deleted = %entry.message_id);
        false
    };
    tasks.in_flight.retain(|key, entry| keep(key, entry));
    tasks.finishing.retain(|_, (key, entry)| keep(key, entry));
    for source in deleted {
        if tasks.completed.get(source).is_some_and(|entry| entry.channel == channel) {
            let ids = tasks.completed.remove(source).unwrap().ids;
            delete_posts(bot, channel, &ids);
            posted.extend(ids);
        }
    }
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
    let mut tasks = bot.replies.tasks.lock().unwrap();
    let in_flight = &mut tasks.in_flight;
    // The gateway updates the cache in order, before dispatching concurrent handlers. Validate while holding the
    // reply lock so an edit/delete handler either prevents this start or cancels the newly registered task.
    if !is_current(bot, req) {
        return false;
    }
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
    let started = Instant::now();
    let result = Box::pin(reply(&bot, &req, &posted, generation)).await;
    timing::record(req.message_id, "reply_total", started);
    if let Err(error) = result {
        tracing::error!(target: "decider", kind = "reply", msg_id = %req.message_id, "AI reply failed: {error}");
        post(
            &bot.http,
            req.channel_id,
            Some(req.message_id),
            Some(format!("{AI_ERROR}: reply or memory recall unavailable.")),
            &posted,
        )
        .await;
    }
    bot.replies.finish(key, generation);
}

/// A missing or edited cached source must never trigger work from an old decision snapshot.
pub fn is_current(bot: &Bot, req: &AiRequest) -> bool {
    bot.cache.message(req.message_id).is_some_and(|message| message.content() == req.source_text)
}

async fn reply(bot: &Bot, req: &AiRequest, posted_slot: &Arc<PostedSlot>, generation: u64) -> Result<()> {
    let ai = bot.ai.as_ref().ok_or_else(|| eyre!("OpenRouter API key not configured"))?;
    // Memory recall waits on the decider, so the profile lookup and image downloads run alongside it.
    let user = async {
        db::ensure_user(&bot.db, req.user_id.get(), &req.user_name).await?;
        db::user(&bot.db, req.user_id.get()).await
    };
    let (memories, user, turn) = tokio::try_join!(
        timing::measure(req.message_id, "recall", recall::select(bot, req)),
        timing::measure(req.message_id, "profile", user),
        timing::measure(req.message_id, "media", prompt::user_turn(bot, req)),
    )?;
    let request =
        prompt::build_request(&bot.config, req, user.as_ref(), &memories, turn, &bot.config.openrouter_model)?;
    let deltas = timing::measure(req.message_id, "stream_headers", ai.stream(&request, REPLY_TIMEOUT)).await?;
    let streamed = stream_to_discord(&bot.http, deltas, req.channel_id, req.message_id, posted_slot).await;
    // New messages leave completed text alone, but deleting its source still cancels GIF lookup and side effects.
    bot.replies.complete((req.channel_id, req.user_id), generation);
    let Some(Streamed { posted, raw }) = streamed else {
        return Ok(());
    };
    let directives = output::directives(&raw);
    let gif = async {
        if let Some(query) = &directives.gif {
            attach_gif(bot, req, query, posted.is_some(), posted_slot).await;
        }
    };
    let prediction = async {
        if let Some(prediction) = &directives.prediction {
            predictions::store(bot, req, prediction).await;
        }
    };
    // Post extras in order: the text owns the reply header, otherwise only the first extra does.
    gif.await;
    let audio = async {
        if let Some(text) = &directives.voice {
            attach_voice(bot, req, text, posted_slot).await;
        }
    };
    tokio::join!(audio, prediction);
    if let Some(posted) = posted {
        refusal::check_and_replace(bot, req, user.as_ref(), &memories, posted).await;
    }
    Ok(())
}

/// Sends the GIF as a separate message containing only its URL, leaving the text reply untouched.
async fn attach_gif(bot: &Bot, req: &AiRequest, query: &str, text_posted: bool, slot: &Arc<PostedSlot>) {
    if bot.config.disable_gifs {
        return;
    }
    let url = match gif::find(&bot.web, bot.config.klipy_api_key.as_deref(), query).await {
        Ok(Some(url)) => url,
        Ok(None) => {
            tracing::info!(target: "gifs", msg_id = %req.message_id, query, "No GIF found");
            return;
        }
        Err(error) => {
            tracing::warn!(target: "gifs", msg_id = %req.message_id, query, "GIF search failed: {error}");
            return;
        }
    };
    post_gif(&bot.http, req.channel_id, req.message_id, text_posted, url, slot).await;
}

async fn attach_voice(bot: &Bot, req: &AiRequest, text: &str, slot: &Arc<PostedSlot>) {
    if bot.config.disable_voice || bot.config.openrouter_tts_model.is_empty() {
        return;
    }
    let clip = match timing::measure(req.message_id, "tts", voice::synthesize(bot, text)).await {
        Ok(clip) => clip,
        Err(error) => {
            tracing::warn!(target: "voice", msg_id = %req.message_id, %error, "Voice synthesis failed");
            if slot.lock().unwrap().ids.is_empty() {
                post(&bot.http, req.channel_id, Some(req.message_id), Some(output::discord_text(text)), slot).await;
            }
            return;
        }
    };
    let spoken = clip.text.clone();
    let reply_to = slot.lock().unwrap().ids.is_empty().then_some(req.message_id);
    if let Some(id) = post_inner(&bot.http, req.channel_id, reply_to, None, Some(clip), slot).await {
        bot.voice.remember(id.get(), spoken.clone());
        if let Err(error) =
            db::save_spoken_text(&bot.db, id.get(), req.channel_id.get(), &spoken, &bot.config.openrouter_tts_model)
                .await
        {
            tracing::warn!(target: "voice", msg_id = %id, %error, "Saving generated speech text failed");
        }
    } else {
        let empty = slot.lock().unwrap().ids.is_empty();
        if empty {
            post(&bot.http, req.channel_id, Some(req.message_id), Some(output::discord_text(text)), slot).await;
        }
    }
}

/// Only a GIF-only response gets a reply header; a GIF accompanying text is a plain follow-on message.
async fn post_gif(
    http: &Arc<HttpClient>,
    channel: Id<ChannelMarker>,
    source: Id<MessageMarker>,
    text_posted: bool,
    url: String,
    slot: &Arc<PostedSlot>,
) -> Option<Id<MessageMarker>> {
    post(http, channel, (!text_posted).then_some(source), Some(url), slot).await
}

/// A reply stream that ran to the end.
pub struct Streamed {
    /// The posted message and the last text Discord accepted, or `None` when nothing presentable came.
    pub posted: Option<(Id<MessageMarker>, String)>,
    /// Everything the model sent, directives included.
    pub raw: String,
}

/// Posts the reply as soon as it has presentable text, then edits it at most every [`EDIT_INTERVAL`] and once more at
/// the end. A stream error replaces the text with the error.
///
/// Returns `None` when the stream failed or the reply couldn't be posted.
async fn stream_to_discord(
    http: &Arc<HttpClient>,
    deltas: impl Stream<Item = Result<String>>,
    channel_id: Id<ChannelMarker>,
    reply_to: Id<MessageMarker>,
    posted_slot: &Arc<PostedSlot>,
) -> Option<Streamed> {
    let mut deltas = pin!(deltas);
    let started = Instant::now();
    let mut first_delta = true;
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
                    if first_delta && !delta.is_empty() {
                        timing::record(reply_to, "first_delta", started);
                        first_delta = false;
                    }
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
                } else if finished || text.split_whitespace().count() >= 2 || text.ends_with(char::is_whitespace) {
                    let id = post(http, channel_id, Some(reply_to), Some(text.clone()), posted_slot).await?;
                    timing::record(reply_to, "first_post", started);
                    message_id = Some(id);
                    last_sent = text;
                } else {
                    // Keep a partial first word pending so completion flushes a one-word reply too.
                    changed = true;
                }
            }
        }
        if finished {
            timing::record(reply_to, "stream_total", started);
            return failure.is_none().then(|| Streamed { posted: message_id.map(|id| (id, last_sent)), raw });
        }
    }
}

/// Finish a Discord POST even if its caller is aborted. Register the ID and cancellation under one lock, deleting
/// a late result when cancellation won the race. Covers text, GIF-only replies and error messages.
async fn post(
    http: &Arc<HttpClient>,
    channel_id: Id<ChannelMarker>,
    reply_to: Option<Id<MessageMarker>>,
    text: Option<String>,
    slot: &Arc<PostedSlot>,
) -> Option<Id<MessageMarker>> {
    post_inner(http, channel_id, reply_to, text, None, slot).await
}

async fn post_inner(
    http: &Arc<HttpClient>,
    channel_id: Id<ChannelMarker>,
    reply_to: Option<Id<MessageMarker>>,
    text: Option<String>,
    voice: Option<voice::VoiceClip>,
    slot: &Arc<PostedSlot>,
) -> Option<Id<MessageMarker>> {
    let (http, slot) = (Arc::clone(http), Arc::clone(slot));
    tokio::spawn(async move {
        if slot.lock().unwrap().cancelled {
            return None;
        }
        let response = if let Some(clip) = &voice {
            let request = voice::upload_request(channel_id.get(), reply_to.map(Id::get), clip)
                .inspect_err(|error| tracing::warn!(%error, "Building voice message failed"))
                .ok()?;
            http.request::<twilight_model::channel::Message>(request).await
        } else {
            let mut request = http.create_message(channel_id);
            if let Some(reply_to) = reply_to {
                request = request.reply(reply_to);
            }
            if let Some(text) = &text {
                request = request.content(text);
            }
            request.await
        }
        .inspect_err(|error| tracing::warn!("AI initial message failed: {error}"))
        .ok()?;
        let message = response
            .model()
            .await
            .inspect_err(|error| tracing::warn!("AI posted message decode failed: {error}"))
            .ok()?;
        let cancelled = {
            let mut state = slot.lock().unwrap();
            if state.cancelled {
                true
            } else {
                state.ids.push(message.id);
                false
            }
        };
        if cancelled {
            if let Err(error) = http.delete_message(channel_id, message.id).await {
                tracing::warn!(%error, msg_id = %message.id, "Deleting cancelled late reply failed");
            }
            None
        } else {
            Some(message.id)
        }
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::tests::{message, test_bot};
    use axum::{Json, Router, extract::State, http::Method, http::StatusCode, routing::any};
    use futures_util::stream;
    use serde_json::{Value, json};

    #[tokio::test]
    async fn stale_edited_and_deleted_requests_never_start_a_reply() {
        use twilight_gateway::Event;
        use twilight_model::gateway::payload::incoming::{MessageCreate, MessageDelete};
        let bot = Arc::new(test_bot());
        let original = message(10, 2, "alice", "old text");
        bot.cache.update(&Event::MessageCreate(Box::new(MessageCreate(original.clone()))));
        let req = bot.snapshot(&original);
        let edited = message(10, 2, "alice", "edited text");
        bot.cache.update(&Event::MessageCreate(Box::new(MessageCreate(edited))));
        assert!(!submit(&bot, &req, true), "a delayed check must not answer the old text");
        bot.cache.update(&Event::MessageDelete(MessageDelete {
            channel_id: Id::new(1),
            guild_id: None,
            id: Id::new(10),
        }));
        assert!(!submit(&bot, &req, true), "a delayed check must not answer a deleted message");
    }

    #[tokio::test]
    async fn cancelling_while_discord_accepts_a_post_deletes_the_late_message() {
        let accepted = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let deleted = Arc::new(tokio::sync::Notify::new());
        let (a, r, d) = (Arc::clone(&accepted), Arc::clone(&release), Arc::clone(&deleted));
        let app = Router::new().fallback(any(move |method: Method| {
            let (a, r, d) = (Arc::clone(&a), Arc::clone(&r), Arc::clone(&d));
            async move {
                if method == Method::DELETE {
                    d.notify_one();
                    return (StatusCode::NO_CONTENT, Json(json!(null)));
                }
                a.notify_one();
                r.notified().await;
                (StatusCode::OK, Json(serde_json::to_value(message(42, 3, "bot", "hi")).unwrap()))
            }
        }));
        let address = crate::test_util::serve(app).await;
        let mut bot = test_bot();
        bot.http = HttpClient::builder()
            .token("test-token".into())
            .proxy(address.to_string(), true)
            .ratelimiter(None)
            .build()
            .into();
        let bot = Arc::new(bot);
        let posted = Arc::default();
        let (task_bot, task_posted) = (Arc::clone(&bot), Arc::clone(&posted));
        let task = tokio::spawn(async move {
            stream_to_discord(&task_bot.http, stream::iter(deltas(&["hi"])), Id::new(1), Id::new(10), &task_posted)
                .await;
        });
        tokio::time::timeout(Duration::from_secs(2), accepted.notified()).await.unwrap();
        let entry = InFlight { abort: task.abort_handle(), posted, ..in_flight(10, "hello", 0) };
        cancel(&bot, &entry, Id::new(1));
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), deleted.notified())
            .await
            .expect("a POST that finishes after cancellation must be deleted");
    }

    #[tokio::test]
    async fn text_and_gif_are_separate_posts_and_both_are_tracked() {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
        let captured = Arc::clone(&seen);
        let app = Router::new().fallback(any(move |method: Method, Json(body): Json<Value>| {
            let seen = Arc::clone(&captured);
            async move {
                assert_eq!(method, Method::POST);
                assert!(body.get("embeds").is_none());
                let content = body["content"].as_str().unwrap().to_owned();
                let mut seen = seen.lock().await;
                seen.push(body);
                Json(serde_json::to_value(message(40 + seen.len() as u64, 3, "bot", &content)).unwrap())
            }
        }));
        let address = crate::test_util::serve(app).await;
        let http = Arc::new(
            HttpClient::builder().token("test-token".into()).proxy(address.to_string(), true).ratelimiter(None).build(),
        );
        let slot = Arc::default();
        post(&http, Id::new(1), Some(Id::new(10)), Some("skill issue".into()), &slot).await.unwrap();
        post_gif(&http, Id::new(1), Id::new(10), true, "https://example.com/reaction.gif".into(), &slot).await.unwrap();
        let requests = seen.lock().await;
        assert_eq!(requests[0]["content"], "skill issue");
        assert_eq!(requests[1]["content"], "https://example.com/reaction.gif");
        assert_eq!(requests[0]["message_reference"]["message_id"], "10");
        assert!(
            requests[1].get("message_reference").is_none(),
            "the accompanying GIF must not repeat the reply header"
        );
        assert_eq!(slot.lock().unwrap().ids, [Id::new(41), Id::new(42)]);
        drop(requests);
        let mut bot = test_bot();
        bot.http = Arc::clone(&http);
        let bot = Arc::new(bot);
        let key = (Id::new(1), Id::new(2));
        let entry = InFlight { posted: slot, ..in_flight(10, "hello", 0) };
        bot.replies.tasks.lock().unwrap().in_flight.insert(key, entry);
        bot.replies.finish(key, 0);
        assert_eq!(cancel_answering(&bot, Id::new(9), &[Id::new(10)]), Vec::<Id<MessageMarker>>::new());
        assert_eq!(cancel_answering(&bot, Id::new(1), &[Id::new(10)]), [Id::new(41), Id::new(42)]);
        assert!(bot.replies.tasks.lock().unwrap().completed.is_empty());
        post_gif(&http, Id::new(1), Id::new(20), false, "https://example.com/solo.gif".into(), &Arc::default())
            .await
            .unwrap();
        let seen = seen.lock().await;
        assert_eq!(seen[2]["message_reference"]["message_id"], "20");
    }

    #[tokio::test]
    async fn native_voice_post_keeps_mime_metadata_and_tracking_through_the_http_client() {
        let app = Router::new().fallback(any(|headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
            assert!(headers["content-type"].to_str().unwrap().starts_with("multipart/form-data; boundary="));
            let body = String::from_utf8(body.to_vec()).unwrap();
            assert!(body.contains("Content-Type: audio/ogg"));
            assert!(body.contains("\"flags\":8192"));
            assert!(body.contains("\"duration_secs\":1.0"));
            assert!(!body.contains("message_reference"));
            Json(serde_json::to_value(message(42, 3, "bot", "")).unwrap())
        }));
        let address = crate::test_util::serve(app).await;
        let http = Arc::new(
            HttpClient::builder().token("test-token".into()).proxy(address.to_string(), true).ratelimiter(None).build(),
        );
        let slot = Arc::default();
        let clip = voice::VoiceClip {
            bytes: b"OggS-fixture".to_vec(),
            duration: 1.0,
            waveform: "AA==".into(),
            text: "hello".into(),
        };
        assert_eq!(post_inner(&http, Id::new(1), None, None, Some(clip), &slot).await, Some(Id::new(42)));
        assert_eq!(slot.lock().unwrap().ids, [Id::new(42)]);
    }

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

    async fn run_stream(deltas: Vec<Result<String>>) -> (Option<Streamed>, Vec<String>) {
        let seen = Seen::default();
        let app = Router::new().fallback(any(mock_discord)).with_state(Arc::clone(&seen));
        let address = crate::test_util::serve(app).await;
        let http = Arc::new(
            HttpClient::builder()
                .token("test-token".to_owned())
                .proxy(address.to_string(), true)
                .ratelimiter(None)
                .build(),
        );
        let result = stream_to_discord(&http, stream::iter(deltas), Id::new(1), Id::new(2), &Arc::default()).await;
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
    async fn completed_text_allows_new_replies_but_source_deletion_cancels_ancillary_work() {
        use twilight_gateway::Event;
        use twilight_model::gateway::payload::incoming::MessageCreate;
        let bot = Arc::new(test_bot());
        let key = (Id::new(1), Id::new(2));
        let entry = InFlight { generation: 9, ..in_flight(10, "old", 0) };
        let slot = Arc::clone(&entry.posted);
        bot.replies.tasks.lock().unwrap().in_flight.insert(key, entry);
        bot.replies.complete(key, 9);
        assert!(bot.replies.tasks.lock().unwrap().in_flight.is_empty());
        let newer = message(20, 2, "alice", "new");
        bot.cache.update(&Event::MessageCreate(Box::new(MessageCreate(newer.clone()))));
        assert!(submit(&bot, &bot.snapshot(&newer), true), "completed text must allow an independent reply");
        cancel_answering(&bot, Id::new(1), &[Id::new(10)]);
        assert!(slot.lock().unwrap().cancelled, "source deletion cancels the old GIF/fallback work");
        bot.replies.finish(key, 9);
        assert_eq!(bot.replies.tasks.lock().unwrap().in_flight[&key].message_id, Id::new(20));
        cancel_answering(&bot, Id::new(1), &[Id::new(20)]);
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
        assert_eq!(result.unwrap().posted.unwrap().1, "hi");
        assert_eq!(requests, ["hi"]);
    }

    #[tokio::test]
    async fn first_post_waits_for_a_word_boundary_instead_of_a_letter() {
        let (_, requests) = run_stream(deltas(&["h", "e", "llo ", "there"])).await;
        assert_eq!(requests, ["hello ", "hello there"]);
        let (_, requests) = run_stream(deltas(&["h", "i"])).await;
        assert_eq!(requests, ["hi"], "a finished one-word reply must still be sent");
    }

    #[tokio::test]
    async fn completed_stream_flushes_final_text() {
        let (result, requests) = run_stream(deltas(&["Hi", " there"])).await;
        assert_eq!(result.unwrap().posted.unwrap().1, "hi there");
        assert_eq!(requests, ["hi there"]);
    }

    #[tokio::test]
    async fn failed_final_edit_returns_last_successfully_posted_text() {
        let (result, _) = run_stream(deltas(&["Hi ", "final failure"])).await;
        assert_eq!(result.unwrap().posted.unwrap().1.trim(), "hi");
    }

    #[tokio::test]
    async fn stream_error_replaces_the_reply_and_reports_no_reply() {
        let mut stream = deltas(&["Hi "]);
        stream.push(Err(eyre!("overloaded")));
        let (result, requests) = run_stream(stream).await;
        assert!(result.is_none());
        assert_eq!(requests, ["hi ", "AI Error: overloaded"]);
    }
}
