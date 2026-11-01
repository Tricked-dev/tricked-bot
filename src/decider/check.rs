//! Background check on ordinary messages: should a memory be written, and should the bot chime in unprompted?

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{cooldown::Cooldown, questions, reply_gate::ReplyGate};
use crate::{
    ai::{AiRequest, PERSONA, interject},
    bot::Bot,
    config::{Config, ReplyMode},
};

#[derive(Debug)]
struct ReplyDecision {
    wants: bool,
    responding: bool,
    fire: bool,
}

/// `interjecting` suppresses the reply without claiming the reply gate.
fn decide_reply(
    cfg: &Config,
    gate: &mut ReplyGate,
    channel: u64,
    reply: Option<f32>,
    followup: Option<f32>,
    bot_in_context: bool,
    interjecting: bool,
) -> ReplyDecision {
    let wants = reply.is_some_and(|p| p >= cfg.decider_reply_threshold);
    let responding = bot_in_context && followup.is_some_and(|p| p >= cfg.decider_followup_threshold);
    let fire = !interjecting
        && cfg.decider_reply_mode == ReplyMode::Live
        && (responding || (wants && gate.try_claim(channel, cfg.decider_reply_cooldown)));
    ReplyDecision { wants, responding, fire }
}

/// An unprompted post that takes the place of a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interjection {
    Note,
    Ruling,
    Poll,
}

fn note_period(cfg: &Config, channel: u64) -> Duration {
    Duration::from_secs(cfg.interjections(channel).note_cooldown_secs)
}

fn ruling_period(cfg: &Config, channel: u64) -> Duration {
    Duration::from_secs(cfg.interjections(channel).ruling_cooldown_secs)
}

#[derive(Clone, Copy, Default)]
struct InterjectionScores {
    wrong_claim: Option<f32>,
    argument: Option<f32>,
    poll: Option<f32>,
}

/// A Community Note beats a ruling: the claim is about this message, the argument can wait for the next one.
fn decide_interjection(
    cfg: &Config,
    notes: &mut Cooldown,
    rulings: &mut Cooldown,
    channel: u64,
    now: Instant,
    scores: InterjectionScores,
) -> Option<Interjection> {
    if cfg.decider_reply_mode != ReplyMode::Live {
        return None;
    }
    let policy = cfg.interjections(channel);
    if scores.wrong_claim.is_some_and(|p| p >= policy.wrong_claim_threshold)
        && notes.try_claim(channel, note_period(cfg, channel), now)
    {
        return Some(Interjection::Note);
    }
    let kind = if scores.argument.is_some_and(|p| p >= policy.argument_threshold) {
        Interjection::Ruling
    } else if scores.poll.is_some_and(|p| p >= policy.poll_threshold) {
        Interjection::Poll
    } else {
        return None;
    };
    rulings.try_claim(channel, ruling_period(cfg, channel), now).then_some(kind)
}

/// Whether at least two people other than the bot speak in the recent conversation, counting the message itself.
fn several_speakers(req: &AiRequest) -> bool {
    let speaker = format!("{PERSONA}: ");
    questions::recent(req)
        .iter()
        .filter(|line| !line.starts_with(&speaker))
        .filter_map(|line| line.split_once(": ").map(|(name, _)| name))
        .any(|name| name != req.user_name)
}

/// Asks whether the message is worth remembering, and with `ask_reply` whether the bot should answer it unprompted:
/// with a reply, a Community Note, or a ruling on an argument. Notes and rulings are only asked about while their
/// channel cooldown is over.
pub fn spawn(bot: Arc<Bot>, req: AiRequest, ask_reply: bool) {
    tokio::spawn(async move {
        let Some(decider) = &bot.decider else { return };
        let cfg = &bot.config;
        let channel = req.channel_id.get();
        let mut qs = HashMap::from([("durable".to_owned(), questions::durable(&req.user_name))]);
        if ask_reply {
            qs.insert("reply".into(), questions::reply());
            qs.insert("followup".into(), questions::followup());
            let now = Instant::now();
            if !req.message.trim().is_empty()
                && bot.note_cooldown.lock().unwrap().is_open(channel, note_period(cfg, channel), now)
            {
                qs.insert("wrong_claim".into(), questions::wrong_claim());
            }
            if bot.ruling_cooldown.lock().unwrap().is_open(channel, ruling_period(cfg, channel), now) {
                if several_speakers(&req) {
                    qs.insert("argument".into(), questions::argument());
                }
                if !req.message.trim().is_empty() {
                    qs.insert("poll".into(), questions::poll());
                }
            }
        }
        let started = std::time::Instant::now();
        let state = questions::state(&req);
        let timeout = Duration::from_millis(cfg.decider_check_timeout_ms);
        let scores = match decider.noul(&state, qs, timeout).await {
            Ok(scores) => scores,
            Err(error) => {
                tracing::warn!(target: "decider", kind = "check", channel = %req.channel_id, msg_id = %req.message_id, "skipped: {error}");
                return;
            }
        };
        let durable = scores["durable"];
        if !crate::ai::reply::is_current(&bot, &req) {
            tracing::info!(target: "decider", kind = "check", msg_id = %req.message_id, "Discarding stale check");
            return;
        }
        let reply = scores.get("reply").copied();
        let followup = scores.get("followup").copied();
        let wrong_claim = scores.get("wrong_claim").copied();
        let argument = scores.get("argument").copied();
        let claimed_at = Instant::now();
        let interjection = decide_interjection(
            cfg,
            &mut bot.note_cooldown.lock().unwrap(),
            &mut bot.ruling_cooldown.lock().unwrap(),
            channel,
            claimed_at,
            InterjectionScores { wrong_claim, argument, poll: scores.get("poll").copied() },
        );
        let speaker = format!("{PERSONA}: ");
        let bot_in_context =
            questions::recent(&req).iter().chain(&req.reply_target).any(|line| line.starts_with(&speaker));
        let ReplyDecision { wants, responding, fire } = decide_reply(
            cfg,
            &mut bot.reply_gate.lock().unwrap(),
            channel,
            reply,
            followup,
            bot_in_context,
            interjection.is_some(),
        );
        tracing::info!(target: "decider", kind = "check", channel = %req.channel_id, msg_id = %req.message_id,
            user = %req.user_id, durable, reply = reply.unwrap_or(-1.0), followup = followup.unwrap_or(-1.0),
            wrong_claim = wrong_claim.unwrap_or(-1.0), argument = argument.unwrap_or(-1.0),
            poll = scores.get("poll").copied().unwrap_or(-1.0), ?interjection,
            wants, responding, fire, bot_in_context, mode = ?cfg.decider_reply_mode,
            latency_ms = started.elapsed().as_millis());
        if fire {
            crate::ai::reply::submit(&bot, &req, true);
        }
        let interjecting = async {
            if let Some(kind) = interjection {
                let posted = deliver_interjection(&bot, &req, kind, claimed_at).await;
                if !posted && crate::ai::reply::is_current(&bot, &req) {
                    let fallback = decide_reply(
                        cfg,
                        &mut bot.reply_gate.lock().unwrap(),
                        channel,
                        reply,
                        followup,
                        bot_in_context,
                        false,
                    );
                    tracing::info!(target: "decider", kind = "interjection_fallback", channel,
                        msg_id = %req.message_id, fire = fallback.fire);
                    if fallback.fire {
                        crate::ai::reply::submit(&bot, &req, true);
                    }
                }
            }
        };
        let remembering = async {
            if durable >= cfg.decider_durable_threshold {
                crate::ai::memory::write_for_author(Arc::clone(&bot), req.clone()).await;
            }
        };
        tokio::join!(interjecting, remembering);
    });
}

/// Releases the reservation even if drafting is cancelled. No lock is held across model or Discord work.
struct InterjectionClaim<'a> {
    cooldown: &'a std::sync::Mutex<Cooldown>,
    channel: u64,
    claimed_at: Instant,
    posted: bool,
}

impl Drop for InterjectionClaim<'_> {
    fn drop(&mut self) {
        self.cooldown.lock().unwrap().finish(self.channel, self.claimed_at, self.posted, Instant::now());
    }
}

/// Draft and deliver the selected interjection, spending the cooldown only when a message is posted.
async fn deliver_interjection(bot: &Arc<Bot>, req: &AiRequest, kind: Interjection, claimed_at: Instant) -> bool {
    let cooldown = match kind {
        Interjection::Note => &bot.note_cooldown,
        Interjection::Ruling | Interjection::Poll => &bot.ruling_cooldown,
    };
    let mut claim = InterjectionClaim { cooldown, channel: req.channel_id.get(), claimed_at, posted: false };
    claim.posted = match kind {
        Interjection::Note => interject::note(bot, req).await,
        Interjection::Ruling => interject::ruling(bot, req).await,
        Interjection::Poll => interject::start_poll(bot, req).await,
    };
    claim.posted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_config() -> Config {
        Config {
            decider_reply_mode: ReplyMode::Live,
            decider_reply_threshold: 0.9,
            decider_followup_threshold: crate::config::DEFAULT_FOLLOWUP_THRESHOLD,
            decider_reply_cooldown: 15,
            decider_wrong_claim_threshold: 0.9,
            decider_argument_threshold: 0.85,
            decider_poll_threshold: 0.85,
            decider_note_cooldown_secs: 1800,
            decider_ruling_cooldown_secs: 3600,
            ..Config::default()
        }
    }

    #[test]
    fn logged_followup_bypasses_cooldown_without_resetting_it() {
        let cfg = live_config();
        let mut gate = ReplyGate::default();
        gate.count(1);
        assert!(decide_reply(&cfg, &mut gate, 1, Some(0.95), None, true, false).fire);
        for _ in 0..14 {
            gate.count(1);
        }
        // Actual scores for "Your cooking sucks" after the bot's meal joke.
        let decision = decide_reply(&cfg, &mut gate, 1, Some(0.2464), Some(0.614), true, false);
        assert!(decision.responding && decision.fire);
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(0.95), None, true, false).fire);
        gate.count(1);
        assert!(decide_reply(&cfg, &mut gate, 1, Some(0.95), None, true, false).fire);
    }

    #[test]
    fn unrelated_message_and_disabled_mode_stay_quiet() {
        let mut cfg = live_config();
        let mut gate = ReplyGate::default();
        gate.count(1);
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(0.2237), Some(0.2496), true, false).fire);
        cfg.decider_reply_mode = ReplyMode::Off;
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(1.0), Some(1.0), true, false).fire);
    }

    #[test]
    fn classifier_cannot_follow_up_when_the_bot_has_not_spoken() {
        let cfg = live_config();
        let mut gate = ReplyGate::default();
        let decision = decide_reply(&cfg, &mut gate, 1, None, Some(1.0), false, false);
        assert!(!decision.responding && !decision.fire);
    }

    #[test]
    fn an_interjection_suppresses_the_reply_without_claiming_the_gate() {
        let cfg = live_config();
        let mut gate = ReplyGate::default();
        gate.count(1);
        let decision = decide_reply(&cfg, &mut gate, 1, Some(0.95), Some(1.0), true, true);
        assert!(decision.wants && decision.responding && !decision.fire);
        assert!(decide_reply(&cfg, &mut gate, 1, Some(0.95), None, false, false).fire);
    }

    fn interjection(
        cfg: &Config,
        (notes, rulings): (&mut Cooldown, &mut Cooldown),
        now: Instant,
        wrong_claim: Option<f32>,
        argument: Option<f32>,
    ) -> Option<Interjection> {
        decide_interjection(cfg, notes, rulings, 1, now, InterjectionScores { wrong_claim, argument, poll: None })
    }

    #[test]
    fn notes_beat_rulings_and_each_waits_out_its_cooldown() {
        let cfg = live_config();
        let (mut notes, mut rulings) = (Cooldown::default(), Cooldown::default());
        let start = Instant::now();
        let mut at = |secs, wrong_claim, argument| {
            let now = start + Duration::from_secs(secs);
            let result = interjection(&cfg, (&mut notes, &mut rulings), now, wrong_claim, argument);
            match result {
                Some(Interjection::Note) => notes.finish(1, now, true, now),
                Some(Interjection::Ruling | Interjection::Poll) => rulings.finish(1, now, true, now),
                None => {}
            }
            result
        };
        assert_eq!(at(0, Some(0.89), Some(0.84)), None, "below both thresholds");
        assert_eq!(at(0, Some(0.95), Some(0.95)), Some(Interjection::Note));
        assert_eq!(at(1, Some(0.95), Some(0.95)), Some(Interjection::Ruling), "the note is cooling down");
        assert_eq!(at(2, Some(0.95), Some(0.95)), None, "both are cooling down");
        assert_eq!(at(1800, Some(0.95), Some(0.95)), Some(Interjection::Note));
        assert_eq!(at(3600, None, Some(0.95)), None, "the ruling cooldown started at 1s");
        assert_eq!(at(3601, None, Some(0.95)), Some(Interjection::Ruling));
    }

    #[test]
    fn interjections_stay_off_outside_live_mode_and_without_scores() {
        let mut cfg = live_config();
        let (mut notes, mut rulings) = (Cooldown::default(), Cooldown::default());
        assert_eq!(interjection(&cfg, (&mut notes, &mut rulings), Instant::now(), None, None), None);
        cfg.decider_reply_mode = ReplyMode::Off;
        assert_eq!(interjection(&cfg, (&mut notes, &mut rulings), Instant::now(), Some(1.0), Some(1.0)), None);
        assert!(notes.is_open(1, Duration::MAX, Instant::now()), "nothing was claimed");
    }

    #[test]
    fn arguments_need_another_human_speaker() {
        let mut req = crate::ai::tests::request("no you're wrong");
        req.conversation = vec!["alice: tabs".into(), format!("{PERSONA}: spaces obviously")];
        assert!(!several_speakers(&req));
        req.conversation.push("bob: spaces".into());
        assert!(several_speakers(&req));
    }
    #[tokio::test]
    async fn declined_or_failed_drafts_do_not_spend_the_channel_cooldown() {
        use axum::{Json, Router, routing::post};
        use serde_json::json;

        let app = Router::new().route(
            "/chat/completions",
            post(|| async {
                Json(json!({"choices": [{"message": {"content": "{\"note\":null,\"action\":\"none\"}"}}]}))
            }),
        );
        let address = crate::test_util::serve(app).await;
        for model_available in [true, false] {
            for kind in [Interjection::Note, Interjection::Ruling, Interjection::Poll] {
                let mut bot = crate::bot::tests::test_bot();
                bot.config = live_config();
                bot.config.openrouter_base_url = format!("http://{address}");
                bot.config.openrouter_api_key = model_available.then(|| "test-key".into());
                bot.ai = crate::ai::openrouter::OpenRouter::from_config(bot.web.clone(), &bot.config);
                let bot = Arc::new(bot);
                let req = crate::ai::tests::request("a candidate the drafting model declines");
                let channel = req.channel_id.get();
                let now = Instant::now();
                let selected = decide_interjection(
                    &bot.config,
                    &mut bot.note_cooldown.lock().unwrap(),
                    &mut bot.ruling_cooldown.lock().unwrap(),
                    channel,
                    now,
                    InterjectionScores {
                        wrong_claim: (kind == Interjection::Note).then_some(1.0),
                        argument: (kind == Interjection::Ruling).then_some(1.0),
                        poll: (kind == Interjection::Poll).then_some(1.0),
                    },
                );
                assert_eq!(selected, Some(kind));
                deliver_interjection(&bot, &req, kind, now).await;
                let open = match kind {
                    Interjection::Note => bot.note_cooldown.lock().unwrap().is_open(
                        channel,
                        note_period(&bot.config, channel),
                        Instant::now(),
                    ),
                    Interjection::Ruling | Interjection::Poll => bot.ruling_cooldown.lock().unwrap().is_open(
                        channel,
                        ruling_period(&bot.config, channel),
                        Instant::now(),
                    ),
                };
                assert!(open, "{kind:?} must remain eligible after no post; model_available={model_available}");
            }
        }
    }
    #[tokio::test]
    async fn delivered_interjections_spend_the_channel_cooldown() {
        use axum::{
            Json, Router,
            routing::{any, post},
        };
        use serde_json::json;

        let app = Router::new()
            .route("/chat/completions", post(|| async {
                Json(json!({"choices": [{"message": {"content": "{\"note\":\"bats can see\",\"action\":\"poll\",\"question\":\"who wins?\",\"answers\":[\"alice\",\"bob\"]}"}}]}))
            }))
            .fallback(any(|| async { Json(crate::bot::tests::message(42, 3, "bot", "posted")) }));
        let address = crate::test_util::serve(app).await;
        for kind in [Interjection::Note, Interjection::Ruling, Interjection::Poll] {
            let mut bot = crate::bot::tests::test_bot();
            bot.config = live_config();
            bot.config.openrouter_base_url = format!("http://{address}");
            bot.config.openrouter_api_key = Some("test-key".into());
            bot.ai = crate::ai::openrouter::OpenRouter::from_config(bot.web.clone(), &bot.config);
            bot.http = twilight_http::Client::builder()
                .token("test-token".into())
                .proxy(address.to_string(), true)
                .ratelimiter(None)
                .build()
                .into();
            let bot = Arc::new(bot);
            let source = crate::bot::tests::message(10, 2, "alice", "miku or teto?");
            bot.cache.update(&twilight_gateway::Event::MessageCreate(Box::new(
                twilight_model::gateway::payload::incoming::MessageCreate(source.clone()),
            )));
            let req = bot.snapshot(&source);
            let channel = req.channel_id.get();
            let now = Instant::now();
            assert_eq!(
                decide_interjection(
                    &bot.config,
                    &mut bot.note_cooldown.lock().unwrap(),
                    &mut bot.ruling_cooldown.lock().unwrap(),
                    channel,
                    now,
                    InterjectionScores {
                        wrong_claim: (kind == Interjection::Note).then_some(1.0),
                        argument: (kind == Interjection::Ruling).then_some(1.0),
                        poll: (kind == Interjection::Poll).then_some(1.0),
                    },
                ),
                Some(kind)
            );
            deliver_interjection(&bot, &req, kind, now).await;
            let cooldown = match kind {
                Interjection::Note => &bot.note_cooldown,
                Interjection::Ruling | Interjection::Poll => &bot.ruling_cooldown,
            };
            assert!(!cooldown.lock().unwrap().is_open(channel, Duration::from_secs(1), Instant::now()));
        }
    }

    #[test]
    fn abandoned_interjection_releases_its_reservation() {
        let cooldown = std::sync::Mutex::new(Cooldown::default());
        let now = Instant::now();
        assert!(cooldown.lock().unwrap().try_claim(1, Duration::from_secs(3600), now));
        drop(InterjectionClaim { cooldown: &cooldown, channel: 1, claimed_at: now, posted: false });
        assert!(cooldown.lock().unwrap().is_open(1, Duration::from_secs(3600), now));
    }
    #[test]
    fn open_choices_use_channel_thresholds_and_share_the_ruling_cooldown() {
        let mut cfg = live_config();
        cfg.decider_channel_overrides = r#"{"2":{"poll_threshold":0.7,"ruling_cooldown_secs":60}}"#.parse().unwrap();
        let (mut notes, mut rulings) = (Cooldown::default(), Cooldown::default());
        let now = Instant::now();
        let scores = InterjectionScores { poll: Some(0.8), ..Default::default() };
        assert_eq!(decide_interjection(&cfg, &mut notes, &mut rulings, 1, now, scores), None);
        assert_eq!(decide_interjection(&cfg, &mut notes, &mut rulings, 2, now, scores), Some(Interjection::Poll));
        rulings.finish(2, now, true, now);
        let argument = InterjectionScores { argument: Some(1.0), ..Default::default() };
        assert_eq!(decide_interjection(&cfg, &mut notes, &mut rulings, 2, now, argument), None);
        assert_eq!(
            decide_interjection(&cfg, &mut notes, &mut rulings, 2, now + Duration::from_secs(60), argument),
            Some(Interjection::Ruling)
        );
    }

    #[tokio::test]
    async fn ordinary_open_choice_reaches_the_poll_delivery_pipeline_without_an_argument() {
        use axum::{
            Json, Router,
            routing::{any, post},
        };
        use serde_json::{Value, json};

        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let app = Router::new()
            .route(
                "/judge",
                post(|Json(body): Json<Value>| async move {
                    let questions = body["questions"].as_object().unwrap();
                    assert!(questions.contains_key("poll"), "a lone open question is eligible");
                    assert!(!questions.contains_key("argument"), "no second human is needed for a poll");
                    let answers: serde_json::Map<String, Value> = questions
                        .keys()
                        .map(|id| (id.clone(), json!({"noul": if id == "poll" { 1.0 } else { 0.0 }})))
                        .collect();
                    Json(json!({"answers":answers}))
                }),
            )
            .route(
                "/chat/completions",
                post(|| async {
                    Json(json!({"choices":[{"message":{"content":
                    "{\"action\":\"poll\",\"question\":\"best vocaloid?\",\"answers\":[\"miku\",\"teto\"]}"}}]}))
                }),
            )
            .fallback(any(move |Json(body): Json<Value>| {
                sent.send(body).unwrap();
                async { Json(crate::bot::tests::message(42, 3, "bot", "")) }
            }));
        let address = crate::test_util::serve(app).await;
        let mut bot = crate::bot::tests::test_bot();
        bot.config = live_config();
        bot.config.decider_check_timeout_ms = 2000;
        bot.config.openrouter_base_url = format!("http://{address}");
        bot.config.openrouter_api_key = Some("test-key".into());
        bot.ai = crate::ai::openrouter::OpenRouter::from_config(bot.web.clone(), &bot.config);
        bot.decider = Some(
            crate::decider::Decider::new(bot.web.clone(), format!("http://{address}/judge"), "test-key".into())
                .unwrap(),
        );
        bot.http = twilight_http::Client::builder()
            .token("test-token".into())
            .proxy(address.to_string(), true)
            .ratelimiter(None)
            .build()
            .into();
        let source = crate::bot::tests::message(10, 2, "alice", "miku or teto, who is better?");
        bot.cache.update(&twilight_gateway::Event::MessageCreate(Box::new(
            twilight_model::gateway::payload::incoming::MessageCreate(source.clone()),
        )));
        let req = bot.snapshot(&source);
        spawn(Arc::new(bot), req, true);
        let posted = tokio::time::timeout(Duration::from_secs(2), received.recv()).await.unwrap().unwrap();
        assert_eq!(posted["poll"]["question"]["text"], "best vocaloid?");
        assert_eq!(posted["poll"]["answers"].as_array().unwrap().len(), 2);
        assert_eq!(posted["message_reference"]["message_id"], "10");
    }
}
