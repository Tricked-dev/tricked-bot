use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use deadpool_postgres::Pool;
use twilight_cache_inmemory::{DefaultInMemoryCache, ResourceType};
use twilight_http::Client as HttpClient;
use twilight_model::id::{
    Id,
    marker::{ApplicationMarker, ChannelMarker, UserMarker},
};

use crate::{
    ai::{memory::AuthorLocks, openrouter::OpenRouter, reply::ReplyTasks, voice::VoiceState},
    config::Config,
    currency::CurrencyRates,
    decider::{Decider, cooldown::Cooldown, reply_gate::ReplyGate},
    events::TypingIndicator,
    quiz::PendingQuiz,
};

/// Shared, mostly immutable bot context. Mutable parts sit behind short-lived locks that are never held across an
/// `.await`, so every event can be handled concurrently. Deliberately not `Debug`: it holds the bot token and API
/// keys.
pub struct Bot {
    pub config: Config,
    pub id: Id<UserMarker>,
    pub application_id: Id<ApplicationMarker>,
    pub http: Arc<HttpClient>,
    pub cache: DefaultInMemoryCache,
    pub db: Pool,
    /// General-purpose HTTP client with a shared connection pool.
    pub web: reqwest::Client,
    /// Client for untrusted media URLs: no redirects and a short timeout.
    pub media: reqwest::Client,
    pub ai: Option<OpenRouter>,
    pub decider: Option<Decider>,
    /// Optional faster decider used only to rank memory names.
    pub recall_decider: Option<Decider>,
    pub user_limit: RateLimit,
    pub channel_limit: RateLimit,
    pub dm_limit: RateLimit,
    pub allowed_bot_limit: RateLimit,
    pub reply_gate: Mutex<ReplyGate>,
    /// When each channel last got a Community Note.
    pub note_cooldown: Mutex<Cooldown>,
    /// When each channel last got a ruling or poll.
    pub ruling_cooldown: Mutex<Cooldown>,
    pub replies: ReplyTasks,
    pub voice: VoiceState,
    pub memory_locks: AuthorLocks,
    pub quizzes: Mutex<HashMap<Id<ChannelMarker>, PendingQuiz>>,
    /// Minimum time between two channel topic changes.
    pub topic_limit: RateLimit,
    pub typing: Mutex<TypingIndicator>,
    pub currency: RwLock<CurrencyRates>,
}

impl Bot {
    pub fn new(
        config: Config,
        http: HttpClient,
        id: Id<UserMarker>,
        application_id: Id<ApplicationMarker>,
        db: Pool,
    ) -> color_eyre::Result<Self> {
        let web = reqwest::Client::builder().user_agent(concat!("tricked-bot/", env!("CARGO_PKG_VERSION"))).build()?;
        let media = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?;
        let decider = Decider::from_parts(&web, config.decider_url.clone(), config.decider_api_key.clone());
        let recall_decider =
            Decider::from_parts(&web, config.decider_recall_url.clone(), config.decider_api_key.clone());
        tracing::info!("decider: {}", if decider.is_some() { "configured" } else { "disabled" });
        Ok(Self {
            ai: OpenRouter::from_config(web.clone(), &config),
            id,
            application_id,
            http: Arc::new(http),
            cache: DefaultInMemoryCache::builder()
                .resource_types(ResourceType::MESSAGE | ResourceType::MEMBER | ResourceType::USER)
                .build(),
            db,
            web,
            media,
            decider,
            recall_decider,
            user_limit: RateLimit::new(Duration::from_secs(30), 10),
            channel_limit: RateLimit::new(Duration::from_secs(60), 120),
            dm_limit: RateLimit::new(Duration::from_secs(3600), 30),
            allowed_bot_limit: RateLimit::new(BOT_LIMIT.0, BOT_LIMIT.1),
            reply_gate: Mutex::default(),
            note_cooldown: Mutex::default(),
            ruling_cooldown: Mutex::default(),
            replies: ReplyTasks::default(),
            voice: VoiceState::default(),
            memory_locks: AuthorLocks::default(),
            quizzes: Mutex::default(),
            topic_limit: RateLimit::new(Duration::from_secs(150), 1),
            typing: Mutex::default(),
            currency: RwLock::default(),
            config,
        })
    }
}

/// Fixed-window rate limiter: each key may be hit `limit` times per `window`.
/// Messages handled per allowed bot per window, so two bots can't keep each other talking.
pub const BOT_LIMIT: (Duration, u32) = (Duration::from_secs(600), 10);

#[derive(Debug)]
pub struct RateLimit {
    window: Duration,
    limit: u32,
    windows: Mutex<HashMap<u64, (Instant, u32)>>,
}

impl RateLimit {
    pub fn new(window: Duration, limit: u32) -> Self {
        Self { window, limit, windows: Mutex::default() }
    }

    /// Records a hit for `key`. Returns how long until the key may be used again when it's over the limit.
    pub fn hit(&self, key: u64) -> Option<Duration> {
        let now = Instant::now();
        let mut windows = self.windows.lock().unwrap();
        if windows.len() > 4096 {
            windows.retain(|_, (start, _)| now - *start < self.window);
        }
        let (start, hits) = windows.entry(key).or_insert((now, 0));
        if now - *start >= self.window {
            (*start, *hits) = (now, 0);
        }
        *hits += 1;
        (*hits > self.limit).then(|| self.window.checked_sub(now - *start).unwrap())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A bot that never touches the network: the pool connects lazily and the HTTP client is never called. Needs a
    /// Tokio runtime for the HTTP client's ratelimiter task.
    pub fn test_bot() -> Bot {
        let manager = deadpool_postgres::Manager::new(tokio_postgres::Config::new(), tokio_postgres::NoTls);
        let pool = Pool::builder(manager).build().unwrap();
        Bot::new(Config::default(), HttpClient::builder().ratelimiter(None).build(), Id::new(3), Id::new(3), pool)
            .unwrap()
    }

    pub fn message(id: u64, author: u64, name: &str, text: &str) -> twilight_model::channel::Message {
        serde_json::from_value(serde_json::json!({
            "id": id.to_string(), "channel_id": "1", "content": text, "timestamp": "2026-10-08T00:00:00+00:00",
            "author": {"id": author.to_string(), "username": name, "discriminator": "0001", "avatar": null},
            "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
            "embeds": [], "pinned": false, "type": 0
        }))
        .unwrap()
    }

    #[test]
    fn rate_limit_blocks_after_the_limit_until_the_window_ends() {
        let limit = RateLimit::new(Duration::from_secs(60), 2);
        assert!(limit.hit(1).is_none());
        assert!(limit.hit(1).is_none());
        let wait = limit.hit(1).unwrap();
        assert!(wait <= Duration::from_secs(60) && wait > Duration::from_secs(59));
        assert!(limit.hit(2).is_none(), "keys are independent");

        let expired = RateLimit::new(Duration::ZERO, 1);
        assert!(expired.hit(1).is_none());
        assert!(expired.hit(1).is_none(), "a new window starts once the old one ends");
    }
}
