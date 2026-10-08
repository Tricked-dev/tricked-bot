use crate::{ai_message, ai_reply::AiRequest, database::Memory, questions};
use std::{sync::Arc, time::Duration};
use twilight_http::Client as HttpClient;
use twilight_model::id::{
    marker::{ChannelMarker, MessageMarker},
    Id,
};

pub fn is_refusal(p: f32, threshold: f32, fallback: &str) -> bool {
    !fallback.trim().is_empty() && p.is_finite() && p >= threshold
}

pub async fn check_and_replace(
    req: &AiRequest,
    memories: Vec<Memory>,
    http: Arc<HttpClient>,
    channel: Id<ChannelMarker>,
    posted: (Id<MessageMarker>, String),
) {
    let Some(decider) = &req.decider else { return };
    let cfg = &req.config;
    if cfg.openrouter_fallback_model.trim().is_empty() {
        return;
    }
    let (message_id, text) = posted;
    if text.starts_with("AI Error") {
        return;
    }
    let p = match decider
        .noul(
            &questions::reply_state(&req.message, &text),
            [("refusal".into(), questions::refusal())].into(),
            false,
            Duration::from_millis(cfg.decider_check_timeout_ms),
        )
        .await
    {
        Ok(scores) => scores["refusal"],
        Err(error) => {
            tracing::warn!(target: "decider", kind = "refusal", msg_id = req.message_id, "skipped: {error}");
            return;
        }
    };
    let replace = is_refusal(p, cfg.decider_refusal_threshold, &cfg.openrouter_fallback_model);
    tracing::info!(target: "decider", kind = "refusal", msg_id = req.message_id, p, replace);
    if !replace {
        return;
    }
    // Rewrite the already-safe decline, never retry the original request for compliance.
    let mut rewrite = req.clone();
    rewrite.message = format!("Rewrite the following refusal in The Trickster's brief, dry Discord voice. Use everyday words and no em dashes. Preserve the refusal and its safety boundary; do not fulfill the original request or add harmful instructions. Return only the rewritten safe decline.\n\n{text}");
    rewrite.context.clear();
    rewrite.media.clear();
    let mut rx = match ai_message::main(&rewrite, memories, &cfg.openrouter_fallback_model).await {
        Ok(rx) => rx,
        Err(error) => {
            tracing::warn!(msg_id = req.message_id, "Refusal style rewrite failed: {error}");
            return;
        }
    };
    let mut last = String::new();
    while let Some(chunk) = rx.recv().await {
        last = chunk;
    }
    if last.trim().is_empty() || last.starts_with("AI Error") {
        return;
    }
    let replacement = crate::message_handler::discord_text(&last);
    if let Ok(request) = http.update_message(channel, message_id).content(Some(&replacement)) {
        if let Err(error) = request.exec().await {
            tracing::warn!(msg_id = req.message_id, "Refusal style edit failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn needs_score_and_fallback() {
        assert!(is_refusal(0.9, 0.9, "fallback"));
        assert!(!is_refusal(0.5, 0.9, "fallback"));
        assert!(!is_refusal(0.9, 0.9, "  "));
        assert!(!is_refusal(f32::NAN, 0.9, "fallback"));
    }
}
