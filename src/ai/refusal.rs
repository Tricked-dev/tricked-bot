//! Rewrites out-of-character assistant refusals into an in-character dodge.

use std::time::Duration;

use twilight_model::id::{Id, marker::MessageMarker};

use super::{AiRequest, openrouter::Content, output, prompt, reply::REPLY_TIMEOUT};
use crate::{
    bot::Bot,
    db::{Memory, User},
    decider::questions,
};

pub async fn check_and_replace(
    bot: &Bot,
    req: &AiRequest,
    user: Option<&User>,
    memories: &[Memory],
    (message_id, text): (Id<MessageMarker>, String),
) {
    let (Some(decider), Some(ai)) = (&bot.decider, &bot.ai) else {
        return;
    };
    let cfg = &bot.config;
    let Some(fallback_model) = cfg.openrouter_fallback_model.as_deref().filter(|model| !model.trim().is_empty()) else {
        return;
    };
    let p = match decider
        .noul(
            &questions::reply_state(&req.message, &text),
            [("refusal".into(), questions::refusal())].into(),
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
    let replace = p >= cfg.decider_refusal_threshold;
    tracing::info!(target: "decider", kind = "refusal", msg_id = %req.message_id, p, replace);
    if !replace {
        return;
    }
    // Rewrite the already-safe decline, never retry the original request for compliance.
    let rewrite = AiRequest {
        message: format!(
            "Rewrite this refusal as a one-line in-character dodge in your usual voice: no lecture, no explanation, \
             no em dashes. Keep declining; don't fulfill the original request or add harmful instructions. Reply \
             with only the dodge.\n<refusal>\n{text}\n</refusal>"
        ),
        conversation: Vec::new(),
        reply_target: None,
        media: Vec::new(),
        ..req.clone()
    };
    let turn = Content::Text(prompt::text_turn(&rewrite));
    let rewritten = async {
        let request = prompt::build_request(cfg, &rewrite, user, memories, turn, fallback_model)?;
        ai.complete(&request, REPLY_TIMEOUT).await
    };
    let replacement = match rewritten.await.map(|text| output::presentable(&text)) {
        Ok(Some(text)) if !text.trim().is_empty() => text,
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(msg_id = %req.message_id, "Refusal style rewrite failed: {error}");
            return;
        }
    };
    if let Err(error) = bot.http.update_message(req.channel_id, message_id).content(Some(&replacement)).await {
        tracing::warn!(msg_id = %req.message_id, "Refusal style edit failed: {error}");
    }
}
