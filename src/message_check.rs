use crate::{
    ai_reply::{self, AiRequest},
    config::{Config, ReplyMode},
    memory_creator, questions,
    reply_gate::ReplyGate,
    structs::State,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use twilight_http::Client as HttpClient;

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
) -> ReplyDecision {
    let wants = reply.is_some_and(|p| p >= cfg.decider_reply_threshold);
    let responding = followup.is_some_and(|p| p >= cfg.decider_followup_threshold);
    let fire = cfg.decider_reply_mode == ReplyMode::Live
        && (responding || (wants && gate.try_claim(channel, cfg.decider_reply_cooldown)));
    ReplyDecision {
        wants,
        responding,
        fire,
    }
}

pub fn spawn(state: Arc<Mutex<State>>, http: Arc<HttpClient>, req: AiRequest, ask_reply: bool) {
    tokio::spawn(async move {
        let Some(decider) = req.decider.clone() else { return };
        let cfg = req.config.clone();
        let mut qs = HashMap::from([("durable".to_string(), questions::durable(&req.user_name))]);
        if ask_reply {
            qs.insert("reply".into(), questions::reply());
            qs.insert("followup".into(), questions::followup());
        }
        let started = std::time::Instant::now();
        let scores = match decider
            .noul(
                &questions::state(&req.context, &req.user_name, &req.message),
                qs,
                false,
                Duration::from_millis(cfg.decider_check_timeout_ms),
            )
            .await
        {
            Ok(scores) => scores,
            Err(error) => {
                tracing::warn!(target: "decider", kind = "check", channel = req.channel_id, msg_id = req.message_id, "skipped: {error}");
                return;
            }
        };
        let durable = scores["durable"];
        let reply = scores.get("reply").copied();
        let followup = scores.get("followup").copied();
        let ReplyDecision {
            wants,
            responding,
            fire,
        } = decide_reply(
            &cfg,
            &mut state.lock().await.reply_gate,
            req.channel_id,
            reply,
            followup,
        );
        tracing::info!(target: "decider", kind = "check", channel = req.channel_id, msg_id = req.message_id,
            user = req.user_id, durable, reply = reply.unwrap_or(-1.0), followup = followup.unwrap_or(-1.0),
            wants, responding, fire, mode = ?cfg.decider_reply_mode,
            latency_ms = started.elapsed().as_millis() as u64);
        if durable >= cfg.decider_durable_threshold {
            let write_req = req.clone();
            tokio::spawn(async move {
                if let Err(error) = ai_reply::ensure_author(&write_req).await {
                    tracing::warn!(
                        msg_id = write_req.message_id,
                        "Memory author initialization failed: {error}"
                    );
                    return;
                }
                memory_creator::write_for_author(
                    write_req.db,
                    write_req.config,
                    write_req.user_id,
                    write_req.message_id,
                    write_req.message,
                    questions::short_context(&write_req.context),
                )
                .await;
            });
        }
        if fire {
            ai_reply::spawn_ai_reply(
                AiRequest {
                    ask_durable: false,
                    ..req
                },
                http,
            );
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
        assert!(decide_reply(&cfg, &mut gate, 1, Some(0.95), None).fire);
        for _ in 0..14 {
            gate.count(1);
        }
        // Actual scores for "Your cooking sucks" after the bot's meal joke.
        let decision = decide_reply(&cfg, &mut gate, 1, Some(0.2464), Some(0.614));
        assert!(decision.responding && decision.fire);
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(0.95), None).fire);
        gate.count(1);
        assert!(decide_reply(&cfg, &mut gate, 1, Some(0.95), None).fire);
    }

    #[test]
    fn unrelated_message_and_disabled_mode_stay_quiet() {
        let mut cfg = live_config();
        let mut gate = ReplyGate::default();
        gate.count(1);
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(0.2237), Some(0.2496)).fire);
        cfg.decider_reply_mode = ReplyMode::Off;
        assert!(!decide_reply(&cfg, &mut gate, 1, Some(1.0), Some(1.0)).fire);
    }
}
