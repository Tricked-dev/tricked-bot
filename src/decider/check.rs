//! Background check on ordinary messages: should a memory be written, and should the bot chime in unprompted?

use std::{collections::HashMap, sync::Arc, time::Duration};

use super::{questions, reply_gate::ReplyGate};
use crate::{
    ai::{AiRequest, PERSONA},
    bot::Bot,
    config::{Config, ReplyMode},
};

#[derive(Debug)]
struct ReplyDecision {
    wants: bool,
    responding: bool,
    fire: bool,
}

fn decide_reply(
    cfg: &Config,
    gate: &mut ReplyGate,
    channel: u64,
    reply: Option<f32>,
    followup: Option<f32>,
    bot_in_context: bool,
) -> ReplyDecision {
    let wants = reply.is_some_and(|p| p >= cfg.decider_reply_threshold);
    let responding = bot_in_context && followup.is_some_and(|p| p >= cfg.decider_followup_threshold);
    let fire = cfg.decider_reply_mode == ReplyMode::Live
        && (responding || (wants && gate.try_claim(channel, cfg.decider_reply_cooldown)));
    ReplyDecision { wants, responding, fire }
}

pub fn spawn(bot: Arc<Bot>, req: AiRequest, ask_reply: bool) {
    let Some(decider) = bot.decider.clone() else { return };
    tokio::spawn(async move {
        let cfg = &bot.config;
        let mut qs = HashMap::from([("durable".to_owned(), questions::durable(&req.user_name))]);
        if ask_reply {
            qs.insert("reply".into(), questions::reply());
            qs.insert("followup".into(), questions::followup());
        }
        let started = std::time::Instant::now();
        let state = questions::state(&req.context, &req.user_name, &req.message);
        let timeout = Duration::from_millis(cfg.decider_check_timeout_ms);
        let scores = match decider.noul(&state, qs, false, timeout).await {
            Ok(scores) => scores,
            Err(error) => {
                tracing::warn!(target: "decider", kind = "check", channel = %req.channel_id, msg_id = %req.message_id, "skipped: {error}");
                return;
            }
        };
        let durable = scores["durable"];
        let reply = scores.get("reply").copied();
        let followup = scores.get("followup").copied();
        let speaker = format!("{PERSONA}: ");
        let bot_in_context = questions::short_context(&req.context).lines().any(|line| line.starts_with(&speaker));
        let ReplyDecision { wants, responding, fire } = decide_reply(
            cfg,
            &mut bot.reply_gate.lock().unwrap(),
            req.channel_id.get(),
            reply,
            followup,
            bot_in_context,
        );
        tracing::info!(target: "decider", kind = "check", channel = %req.channel_id, msg_id = %req.message_id,
            user = %req.user_id, durable, reply = reply.unwrap_or(-1.0), followup = followup.unwrap_or(-1.0),
            wants, responding, fire, bot_in_context, mode = ?cfg.decider_reply_mode,
            latency_ms = started.elapsed().as_millis() as u64);
        if durable >= cfg.decider_durable_threshold {
            tokio::spawn(crate::ai::memory::write_for_author(Arc::clone(&bot), req.clone()));
        }
        if fire {
            crate::ai::reply::spawn(bot, AiRequest { ask_durable: false, ..req });
        }
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
            ..Config::default()
        }
    }

    #[test]
    fn logged_followup_bypasses_cooldown_without_resetting_it() {
        let cfg = live_config();
        let mut gate = ReplyGate::default();
        gate.count(1);
        assert!(decide_reply(&cfg, &mut gate, 1, Some(0.95), None, true).fire);
        for _ in 0..14 {
            gate.count(1);
        }
        // Actual scores for "Your cooking sucks" after the bot's meal joke.
        let decision = decide_reply(&cfg, &mut gate, 1, Some(0.2464), Some(0.614), true);
        assert!(decision.responding && decision.fire);
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(0.95), None, true).fire);
        gate.count(1);
        assert!(decide_reply(&cfg, &mut gate, 1, Some(0.95), None, true).fire);
    }

    #[test]
    fn unrelated_message_and_disabled_mode_stay_quiet() {
        let mut cfg = live_config();
        let mut gate = ReplyGate::default();
        gate.count(1);
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(0.2237), Some(0.2496), true).fire);
        cfg.decider_reply_mode = ReplyMode::Off;
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(1.0), Some(1.0), true).fire);
    }

    #[test]
    fn classifier_cannot_follow_up_when_the_bot_has_not_spoken() {
        let cfg = live_config();
        let mut gate = ReplyGate::default();
        let decision = decide_reply(&cfg, &mut gate, 1, None, Some(1.0), false);
        assert!(!decision.responding && !decision.fire);
    }
}
