use serde_json::{json, Value};

use crate::{database::Memory, decider::Question};

pub const CONTEXT_LINES: usize = 8;

/// The last CONTEXT_LINES transcript lines before the current message.
pub fn short_context(context: &str) -> String {
    let lines: Vec<&str> = context.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(CONTEXT_LINES)..].join("\n")
}

pub fn state(context: &str, author: &str, text: &str) -> Value {
    json!({ "conversation": short_context(context), "last_message": { "author": author, "text": text } })
}

pub fn reply() -> Question {
    Question::noul(
        "The Trickster is a smug, witty regular in this Discord chat who only speaks up now and then. Is the last \
         message a moment where a short unprompted reply from The Trickster would land well — a question left open \
         to the room, a claim begging for a witty correction, a joke setup, or talk about the bot? Answer false for \
         private back-and-forth between others, serious or sensitive topics, bare links or media, and small talk \
         that needs no reply.",
        "yes: jumping in now would be natural and welcome",
        "no: stay quiet",
    )
}

pub fn durable(author: &str) -> Question {
    Question::noul(
        format!(
            "Does the last message reveal something lasting about {author} — a preference, plan, job, relationship, \
             skill, possession, or life event — rather than a passing mood, joke or one-off remark?"
        ),
        "yes: worth remembering long-term",
        "no: nothing lasting",
    )
}

pub fn memory_id(m: &Memory) -> String {
    format!("m{}", m.id)
}

pub fn recall(author: &str, m: &Memory) -> Question {
    Question::noul(
        format!(
            "The bot is about to reply to {author}'s last message. Stored memory about {author} — {}: {}. Would \
             including it make the reply more accurate or more personal? Answer false if it is unrelated to the \
             last message or the conversation shows it is outdated.",
            m.key, m.content
        ),
        "include: relevant and not contradicted",
        "leave out: unrelated, outdated or contradicted",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_context_keeps_last_lines() {
        let ctx = (1..=12).map(|i| format!("u: {i}")).collect::<Vec<_>>().join("\n");
        let s = short_context(&ctx);
        assert_eq!(s.lines().count(), CONTEXT_LINES);
        assert!(s.starts_with("u: 5") && s.ends_with("u: 12"));
    }
}

pub fn refusal() -> Question {
    Question::noul(
        "The Trickster is a smug, sarcastic Discord persona. Is the bot's reply a refusal or safety disclaimer in a \
         plain assistant voice (e.g. \"I can't help with that\", offering a harmless alternative) instead of staying \
         in character? A decline delivered as an in-character joke counts as false.",
        "out-of-character refusal",
        "in character, including in-character declines",
    )
}

pub fn reply_state(user_message: &str, reply: &str) -> Value {
    json!({ "user_message": user_message, "bot_reply": reply.chars().take(1200).collect::<String>() })
}

#[cfg(test)]
mod state_tests {
    use super::*;
    #[test]
    fn state_keeps_current_message_separate_and_skips_blank_lines() {
        assert_eq!(short_context("\n  \nalice: café\n\n"), "alice: café");
        let s = state("old context", "alice", "新しい message");
        assert_eq!(s["conversation"], "old context");
        assert_eq!(s["last_message"]["author"], "alice");
        assert_eq!(s["last_message"]["text"], "新しい message");
        assert_eq!(short_context(""), "");
    }
    #[test]
    fn refusal_state_contains_both_texts_and_truncates_on_char_boundary() {
        let reply = "🙂".repeat(1201);
        let s = reply_state("user text", &reply);
        assert_eq!(s["user_message"], "user text");
        assert_eq!(s["bot_reply"].as_str().unwrap().chars().count(), 1200);
    }
}
