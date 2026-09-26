use crate::{
    ai_reply::{self, AiRequest},
    config::ReplyMode,
    memory_creator, questions,
    structs::State,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use twilight_http::Client as HttpClient;

pub fn spawn(state: Arc<Mutex<State>>, http: Arc<HttpClient>, req: AiRequest, ask_reply: bool) {
    tokio::spawn(async move {
        let Some(decider) = req.decider.clone() else { return };
        let cfg = req.config.clone();
        let mut qs = HashMap::from([("durable".to_string(), questions::durable(&req.user_name))]);
        if ask_reply {
            qs.insert("reply".into(), questions::reply());
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
        let wants = reply.is_some_and(|p| p >= cfg.decider_reply_threshold);
        let fire = wants
            && cfg.decider_reply_mode == ReplyMode::Live
            && state
                .lock()
                .await
                .reply_gate
                .try_claim(req.channel_id, cfg.decider_reply_cooldown);
        tracing::info!(target: "decider", kind = "check", channel = req.channel_id, msg_id = req.message_id,
            user = req.user_id, durable, reply = reply.unwrap_or(-1.0), wants, fire, mode = ?cfg.decider_reply_mode,
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
