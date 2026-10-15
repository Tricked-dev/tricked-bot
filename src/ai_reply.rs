use crate::{
    ai_message, brave::BraveApi, config::Config, decider::Decider, memory_creator, message_handler, questions, recall,
};
use deadpool_postgres::Pool;
use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
};
use tokio::task::AbortHandle;
use twilight_http::Client as HttpClient;
use twilight_model::id::{marker::MessageMarker, Id};

#[derive(Clone)]
pub struct AiRequest {
    pub db: Pool,
    pub config: Arc<Config>,
    pub brave: BraveApi,
    pub decider: Option<Decider>,
    pub recall_decider: Option<Decider>,
    pub user_id: u64,
    pub user_name: String,
    pub channel_id: u64,
    pub message_id: u64,
    pub message: String,
    pub media: Vec<crate::media::MediaSource>,
    pub context: String,
    pub user_mentions: HashMap<String, u64>,
    pub ask_durable: bool,
}

/// Both DM authors and users intercepted by responders must have a stable DB identity.
pub async fn ensure_author(req: &AiRequest) -> color_eyre::Result<()> {
    crate::db::insert_user(
        &req.db,
        &crate::database::User {
            id: req.user_id as i64,
            name: req.user_name.clone(),
            level: 0,
            xp: 0,
            social_credit: 0,
            relationship: String::new(),
            example_input: String::new(),
            example_output: String::new(),
        },
    )
    .await
}

/// A newer message from the same person replaces their unfinished reply, at most this many times per chain.
const MAX_RESTARTS: u8 = 2;

struct InFlight {
    message_id: u64,
    generation: u64,
    restarts: u8,
    abort: AbortHandle,
    posted: Arc<StdMutex<Option<Id<MessageMarker>>>>,
}

static IN_FLIGHT: Lazy<StdMutex<HashMap<(u64, u64), InFlight>>> = Lazy::new(Default::default);
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn cancel(entry: InFlight, channel: u64, http: &Arc<HttpClient>) {
    entry.abort.abort();
    if let Some(posted) = *entry.posted.lock().unwrap() {
        let http = Arc::clone(http);
        tokio::spawn(async move {
            let _ = http.delete_message(Id::new(channel), posted).exec().await;
        });
    }
    tracing::info!(target: "decider", kind = "restart", superseded = entry.message_id, restarts = entry.restarts + 1);
}

/// Restart the author's unfinished reply with their newer message. Returns whether it did.
pub fn restart_pending(req: AiRequest, http: &Arc<HttpClient>) -> bool {
    let key = (req.channel_id, req.user_id);
    let mut in_flight = IN_FLIGHT.lock().unwrap();
    let restarts = match in_flight.get(&key) {
        Some(entry) if entry.message_id != req.message_id && entry.restarts < MAX_RESTARTS => entry.restarts + 1,
        _ => return false,
    };
    cancel(in_flight.remove(&key).unwrap(), req.channel_id, http);
    start(&mut in_flight, req, Arc::clone(http), restarts);
    true
}

pub fn spawn_ai_reply(req: AiRequest, http: Arc<HttpClient>) {
    let key = (req.channel_id, req.user_id);
    let mut in_flight = IN_FLIGHT.lock().unwrap();
    let restarts = match in_flight.get(&key) {
        None => 0,
        Some(entry) if entry.message_id == req.message_id => return,
        Some(entry) if entry.restarts < MAX_RESTARTS => {
            let restarts = entry.restarts + 1;
            cancel(in_flight.remove(&key).unwrap(), req.channel_id, &http);
            restarts
        }
        Some(_) => MAX_RESTARTS,
    };
    start(&mut in_flight, req, http, restarts);
}

fn start(in_flight: &mut HashMap<(u64, u64), InFlight>, req: AiRequest, http: Arc<HttpClient>, restarts: u8) {
    let key = (req.channel_id, req.user_id);
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    let posted = Arc::new(StdMutex::new(None));
    let message_id = req.message_id;
    let task = run(req, http, Arc::clone(&posted), move || finish(key, generation));
    let abort = tokio::spawn(task).abort_handle();
    in_flight.insert(key, InFlight { message_id, generation, restarts, abort, posted });
}

fn finish(key: (u64, u64), generation: u64) {
    let mut in_flight = IN_FLIGHT.lock().unwrap();
    if in_flight.get(&key).is_some_and(|entry| entry.generation == generation) {
        in_flight.remove(&key);
    }
}

async fn run(
    mut req: AiRequest,
    http: Arc<HttpClient>,
    posted_slot: Arc<StdMutex<Option<Id<MessageMarker>>>>,
    done: impl Fn(),
) {
    let channel = Id::new(req.channel_id);
    let reply_to = Id::new(req.message_id);
    let result = async {
        ensure_author(&req).await?;
        let recalled = recall::select(&req).await?;
        if recalled.durable {
            tokio::spawn(memory_creator::write_for_author(
                req.db.clone(),
                req.config.clone(),
                req.user_id,
                req.message_id,
                req.message.clone(),
                questions::short_context(&req.context),
            ));
        }
        // Link previews can arrive after MessageCreate. Refresh outside the State lock.
        if req.media.is_empty() && req.message.contains("http") {
            if let Ok(response) = http.message(channel, reply_to).exec().await {
                if let Ok(message) = response.model().await {
                    req.media = crate::media::sources(&message);
                }
            }
        }
        let fallback_memories = recalled.memories.clone();
        let rx = ai_message::main(&req, recalled.memories, &req.config.openrouter_model).await?;
        let posted =
            message_handler::handle_streaming_response(rx, channel, reply_to, http.clone(), Some(&posted_slot)).await;
        // A complete reply stays; newer messages no longer cancel it.
        done();
        if let Some(posted) = posted {
            crate::refusal::check_and_replace(&req, fallback_memories, http.clone(), channel, posted).await;
        }
        Ok::<(), color_eyre::Report>(())
    }
    .await;
    done();
    if let Err(error) = result {
        tracing::error!(target: "decider", kind = "reply", msg_id = req.message_id, "AI reply failed: {error}");
        if let Ok(request) = http
            .create_message(channel)
            .content("AI Error: reply or memory recall unavailable.")
        {
            let _ = request.reply(reply_to).exec().await;
        }
    }
}
