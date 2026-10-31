//! Rewrites out-of-character assistant refusals into an in-character dodge.

use std::time::Duration;

use futures_util::StreamExt;
use twilight_model::id::{Id, marker::MessageMarker};

use super::{AiRequest, prompt, reply};
use crate::{bot::Bot, db::Memory, decider::questions};

fn is_refusal(p: f32, threshold: f32) -> bool {
    p.is_finite() && p >= threshold
}

pub async fn check_and_replace(bot: &Bot, req: &AiRequest, memories: &[Memory], posted: (Id<MessageMarker>, String)) {
    let (Some(decider), Some(ai)) = (&bot.decider, &bot.ai) else {
        return;
    };
    let cfg = &bot.config;
    let Some(fallback_model) = cfg.openrouter_fallback_model.as_deref().filter(|model| !model.trim().is_empty()) else {
        return;
    };
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
            tracing::warn!(target: "decider", kind = "refusal", msg_id = %req.message_id, "skipped: {error}");
            return;
        }
    };
    let replace = is_refusal(p, cfg.decider_refusal_threshold);
    tracing::info!(target: "decider", kind = "refusal", msg_id = %req.message_id, p, replace);
    if !replace {
        return;
    }
    // Rewrite the already-safe decline, never retry the original request for compliance.
    let rewrite = AiRequest {
        message: format!(
            "Rewrite the following refusal as a one-line in-character dodge in The Trickster's lowercase shitposter \
             voice: no lecture, no explanation, no em dashes. Keep declining; do not fulfill the original request or \
             add harmful instructions. Return only the rewritten dodge.\n\n{text}"
        ),
        context: String::new(),
        media: Vec::new(),
        ..req.clone()
    };
    let rewritten = async {
        let request = prompt::build_request(bot, &rewrite, memories, fallback_model).await?;
        let snapshots = reply::generate(ai, &request).await?;
        Ok::<_, color_eyre::Report>(snapshots.fold(String::new(), |_, text| std::future::ready(text)).await)
    };
    let last = match rewritten.await {
        Ok(text) if !text.trim().is_empty() && !text.starts_with("AI Error") => text,
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(msg_id = %req.message_id, "Refusal style rewrite failed: {error}");
            return;
        }
    };
    let replacement = prompt::discord_text(&last);
    if let Err(error) = bot.http.update_message(req.channel_id, message_id).content(Some(&replacement)).await {
        tracing::warn!(msg_id = %req.message_id, "Refusal style edit failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_a_finite_score_at_the_threshold() {
        assert!(is_refusal(0.9, 0.9));
        assert!(!is_refusal(0.5, 0.9));
        assert!(!is_refusal(f32::NAN, 0.9));
    }
}
