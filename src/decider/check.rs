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
}

fn note_period(cfg: &Config) -> Duration {
    Duration::from_secs(cfg.decider_note_cooldown_secs)
}

fn ruling_period(cfg: &Config) -> Duration {
    Duration::from_secs(cfg.decider_ruling_cooldown_secs)
}

/// A Community Note beats a ruling: the claim is about this message, the argument can wait for the next one.
fn decide_interjection(
    cfg: &Config,
    notes: &mut Cooldown,
    rulings: &mut Cooldown,
    channel: u64,
    now: Instant,
    wrong_claim: Option<f32>,
    argument: Option<f32>,
) -> Option<Interjection> {
    if cfg.decider_reply_mode != ReplyMode::Live {
        return None;
    }
    if wrong_claim.is_some_and(|p| p >= cfg.decider_wrong_claim_threshold)
        && notes.try_claim(channel, note_period(cfg), now)
    {
        return Some(Interjection::Note);
    }
    (argument.is_some_and(|p| p >= cfg.decider_argument_threshold)
        && rulings.try_claim(channel, ruling_period(cfg), now))
    .then_some(Interjection::Ruling)
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
                && bot.note_cooldown.lock().unwrap().is_open(channel, note_period(cfg), now)
            {
                qs.insert("wrong_claim".into(), questions::wrong_claim());
            }
            if several_speakers(&req) && bot.ruling_cooldown.lock().unwrap().is_open(channel, ruling_period(cfg), now) {
                qs.insert("argument".into(), questions::argument());
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
        let reply = scores.get("reply").copied();
        let followup = scores.get("followup").copied();
        let wrong_claim = scores.get("wrong_claim").copied();
        let argument = scores.get("argument").copied();
        let interjection = decide_interjection(
            cfg,
            &mut bot.note_cooldown.lock().unwrap(),
            &mut bot.ruling_cooldown.lock().unwrap(),
            channel,
            Instant::now(),
            wrong_claim,
            argument,
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
            wrong_claim = wrong_claim.unwrap_or(-1.0), argument = argument.unwrap_or(-1.0), ?interjection,
            wants, responding, fire, bot_in_context, mode = ?cfg.decider_reply_mode,
            latency_ms = started.elapsed().as_millis());
        if fire {
            crate::ai::reply::submit(&bot, &req, true);
        }
        let interjecting = async {
            match interjection {
                Some(Interjection::Note) => interject::note(&bot, &req).await,
                Some(Interjection::Ruling) => interject::ruling(&bot, &req).await,
                None => {}
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
        decide_interjection(cfg, notes, rulings, 1, now, wrong_claim, argument)
    }

    #[test]
    fn notes_beat_rulings_and_each_waits_out_its_cooldown() {
        let cfg = live_config();
        let (mut notes, mut rulings) = (Cooldown::default(), Cooldown::default());
        let start = Instant::now();
        let mut at = |secs, wrong_claim, argument| {
            interjection(&cfg, (&mut notes, &mut rulings), start + Duration::from_secs(secs), wrong_claim, argument)
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
}
