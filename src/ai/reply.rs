//! Running AI replies: one in-flight reply per author and channel, streamed into a Discord message.

use std::{
    collections::HashMap,
    future::ready,
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

use super::{
    AiRequest, memory,
    openrouter::{ChatRequest, OpenRouter},
    prompt, recall, refusal,
};
use crate::bot::Bot;

/// A newer message from the same person replaces their unfinished reply, at most this many times per chain.
const MAX_RESTARTS: u8 = 2;
/// How often a streaming reply is edited with newly generated text.
const EDIT_INTERVAL: Duration = Duration::from_millis(1500);

type PostedSlot = Arc<Mutex<Option<Id<MessageMarker>>>>;
type ReplyKey = (Id<ChannelMarker>, Id<UserMarker>);

#[derive(Debug)]
struct InFlight {
    message_id: Id<MessageMarker>,
    generation: u64,
    restarts: u8,
    abort: AbortHandle,
    posted: PostedSlot,
}

/// Unfinished replies, keyed by channel and author.
#[derive(Debug, Default)]
pub struct ReplyTasks {
    in_flight: Mutex<HashMap<ReplyKey, InFlight>>,
    generation: AtomicU64,
}

impl ReplyTasks {
    fn finish(&self, key: ReplyKey, generation: u64) {
        let mut in_flight = self.in_flight.lock().unwrap();
        if in_flight.get(&key).is_some_and(|entry| entry.generation == generation) {
            in_flight.remove(&key);
        }
    }
}

/// Aborts a superseded reply and deletes whatever part of it was already posted.
fn cancel(bot: &Arc<Bot>, entry: &InFlight, channel: Id<ChannelMarker>) {
    entry.abort.abort();
    if let Some(posted) = *entry.posted.lock().unwrap() {
        let bot = Arc::clone(bot);
        tokio::spawn(async move {
            let _ = bot.http.delete_message(channel, posted).await;
        });
    }
    tracing::info!(target: "decider", kind = "restart", superseded = %entry.message_id, restarts = entry.restarts + 1);
}

/// Restart the author's unfinished reply with their newer message. Returns whether it did.
///
/// Events are handled concurrently, so an older message can arrive here after a newer one. Snowflakes are
/// chronological, which makes "newer" a plain ID comparison.
pub fn restart_pending(bot: &Arc<Bot>, req: &AiRequest) -> bool {
    let key = (req.channel_id, req.user_id);
    let mut in_flight = bot.replies.in_flight.lock().unwrap();
    let restarts = match in_flight.get(&key) {
        Some(entry) if entry.message_id < req.message_id && entry.restarts < MAX_RESTARTS => entry.restarts + 1,
        _ => return false,
    };
    cancel(bot, &in_flight.remove(&key).unwrap(), req.channel_id);
    start(bot, &mut in_flight, req.clone(), restarts);
    true
}

pub fn spawn(bot: &Arc<Bot>, req: AiRequest) {
    let key = (req.channel_id, req.user_id);
    let mut in_flight = bot.replies.in_flight.lock().unwrap();
    let restarts = match in_flight.get(&key) {
        None => 0,
        // Already answering this message or a newer one.
        Some(entry) if entry.message_id >= req.message_id => return,
        Some(entry) if entry.restarts < MAX_RESTARTS => {
            let restarts = entry.restarts + 1;
            cancel(bot, &in_flight.remove(&key).unwrap(), req.channel_id);
            restarts
        }
        Some(_) => MAX_RESTARTS,
    };
    start(bot, &mut in_flight, req, restarts);
}

fn start(bot: &Arc<Bot>, in_flight: &mut HashMap<ReplyKey, InFlight>, req: AiRequest, restarts: u8) {
    let key = (req.channel_id, req.user_id);
    let generation = bot.replies.generation.fetch_add(1, Ordering::Relaxed);
    let posted = PostedSlot::default();
    let message_id = req.message_id;
    let abort = tokio::spawn(run(Arc::clone(bot), req, Arc::clone(&posted), generation)).abort_handle();
    in_flight.insert(key, InFlight { message_id, generation, restarts, abort, posted });
}

async fn run(bot: Arc<Bot>, mut req: AiRequest, posted_slot: PostedSlot, generation: u64) {
    let key = (req.channel_id, req.user_id);
    let result = async {
        crate::db::ensure_user(&bot.db, req.user_id.get(), &req.user_name).await?;
        let recalled = recall::select(&bot, &req).await?;
        if recalled.durable {
            tokio::spawn(memory::write_for_author(Arc::clone(&bot), req.clone()));
        }
        // Link previews can arrive after MessageCreate, so look again before giving up on media.
        if req.media.is_empty()
            && req.message.contains("http")
            && let Ok(response) = bot.http.message(req.channel_id, req.message_id).await
            && let Ok(message) = response.model().await
        {
            req.media = super::media::sources(&message);
        }
        let ai = bot.ai.as_ref().ok_or_else(|| eyre!("OpenRouter API key not configured"))?;
        let request = prompt::build_request(&bot, &req, &recalled.memories, &bot.config.openrouter_model).await?;
        let text = generate(ai, &request).await?;
        let posted = stream_to_discord(&bot.http, text, req.channel_id, req.message_id, Some(&posted_slot)).await;
        // A complete reply stays; newer messages no longer cancel it.
        bot.replies.finish(key, generation);
        if let Some(posted) = posted {
            refusal::check_and_replace(&bot, &req, &recalled.memories, posted).await;
        }
        Ok::<_, color_eyre::Report>(())
    }
    .await;
    bot.replies.finish(key, generation);
    if let Err(error) = result {
        tracing::error!(target: "decider", kind = "reply", msg_id = %req.message_id, "AI reply failed: {error}");
        let _ = bot
            .http
            .create_message(req.channel_id)
            .content("AI Error: reply or memory recall unavailable.")
            .reply(req.message_id)
            .await;
    }
}

/// Streams the reply as successive snapshots of the presentable text. A stream error replaces the text with the
/// error and ends the stream.
pub async fn generate(ai: &OpenRouter, request: &ChatRequest) -> Result<impl Stream<Item = String> + use<>> {
    let deltas = ai.stream(request, Duration::from_secs(120)).await?;
    Ok(deltas
        .scan((String::new(), false), |(text, failed), delta| {
            ready((!*failed).then(|| match delta {
                Ok(delta) => {
                    text.push_str(&delta);
                    prompt::presentable(text)
                }
                Err(error) => {
                    tracing::error!("AI stream failed: {error}");
                    *failed = true;
                    Some(format!("AI Error: {error}"))
                }
            }))
        })
        .filter_map(ready))
}

/// Posts the first text immediately, then edits it at most every [`EDIT_INTERVAL`] and once more at the end.
/// Returns the posted message and the last text Discord accepted.
pub async fn stream_to_discord(
    http: &HttpClient,
    snapshots: impl Stream<Item = String>,
    channel_id: Id<ChannelMarker>,
    reply_to: Id<MessageMarker>,
    posted_slot: Option<&Mutex<Option<Id<MessageMarker>>>>,
) -> Option<(Id<MessageMarker>, String)> {
    let mut snapshots = pin!(snapshots);
    let mut message_id = None;
    let mut last_sent = String::new();
    let mut pending = String::new();
    let mut tick = tokio::time::interval(EDIT_INTERVAL);
    loop {
        let (finished, edit_due) = tokio::select! {
            next = snapshots.next() => match next {
                Some(text) => {
                    pending = text;
                    (false, false)
                }
                None => (true, false),
            },
            _ = tick.tick() => (false, true),
        };
        let text = prompt::discord_text(&pending);
        if !text.trim().is_empty() && text != last_sent {
            match message_id {
                Some(id) if finished || edit_due => {
                    match http.update_message(channel_id, id).content(Some(&text)).await {
                        Ok(_) => last_sent = text,
                        Err(error) => tracing::warn!("AI message update failed: {error}"),
                    }
                }
                Some(_) => {}
                None => {
                    let response = http
                        .create_message(channel_id)
                        .content(&text)
                        .reply(reply_to)
                        .await
                        .inspect_err(|error| tracing::warn!("AI initial message failed: {error}"))
                        .ok()?;
                    let message = response
                        .model()
                        .await
                        .inspect_err(|error| tracing::warn!("AI posted message decode failed: {error}"))
                        .ok()?;
                    message_id = Some(message.id);
                    if let Some(slot) = posted_slot {
                        *slot.lock().unwrap() = Some(message.id);
                    }
                    last_sent = text;
                }
            }
        }
        if finished {
            return message_id.map(|id| (id, last_sent));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::Method, http::StatusCode, routing::any};
    use futures_util::stream;
    use serde_json::{Value, json};

    type Seen = Arc<tokio::sync::Mutex<Vec<String>>>;

    async fn mock_discord(
        State(seen): State<Seen>,
        method: Method,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        seen.lock().await.push(body["content"].as_str().unwrap().to_owned());
        if method == Method::PATCH && body["content"] == "final failure" {
            return (StatusCode::BAD_REQUEST, Json(json!({"code": 50035, "message": "invalid"})));
        }
        (
            StatusCode::OK,
            Json(json!({
                "id": "42", "channel_id": "1", "content": body["content"], "timestamp": "2026-09-26T00:00:00.000000+00:00",
                "author": {"id": "3", "username": "bot", "discriminator": "0001", "avatar": null},
                "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
                "embeds": [], "pinned": false, "type": 0
            })),
        )
    }

    async fn run_stream(chunks: &[&str]) -> (Option<(Id<MessageMarker>, String)>, Vec<String>) {
        let seen = Seen::default();
        let app = Router::new().fallback(any(mock_discord)).with_state(Arc::clone(&seen));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = HttpClient::builder()
            .token("test-token".to_owned())
            .proxy(address.to_string(), true)
            .ratelimiter(None)
            .build();
        let snapshots = stream::iter(chunks.iter().map(|text| (*text).to_owned()));
        let result = stream_to_discord(&http, snapshots, Id::new(1), Id::new(2), None).await;
        server.abort();
        let requests = seen.lock().await.clone();
        (result, requests)
    }

    #[tokio::test]
    async fn completed_short_stream_is_posted() {
        let (result, requests) = run_stream(&["Hi"]).await;
        assert_eq!(result.unwrap().1, "Hi");
        assert_eq!(requests, ["Hi"]);
    }

    #[tokio::test]
    async fn completed_stream_flushes_final_text() {
        let (result, requests) = run_stream(&["Hi", "Final reply"]).await;
        assert_eq!(result.unwrap().1, "Final reply");
        assert_eq!(requests.last().unwrap(), "Final reply");
    }

    #[tokio::test]
    async fn failed_final_edit_returns_last_successfully_posted_text() {
        let (result, _) = run_stream(&["Hi", "final failure"]).await;
        assert_eq!(result.unwrap().1, "Hi");
    }
}
