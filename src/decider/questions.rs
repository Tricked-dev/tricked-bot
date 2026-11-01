use serde_json::{Value, json};

use super::Question;
use crate::{ai::AiRequest, db::Memory};

pub const CONTEXT_LINES: usize = 8;

/// The last [`CONTEXT_LINES`] transcript lines before the current message.
pub fn recent(req: &AiRequest) -> &[String] {
    &req.conversation[req.conversation.len().saturating_sub(CONTEXT_LINES)..]
}

/// What the decider sees of a message: the recent conversation and the message itself.
pub fn state(req: &AiRequest) -> Value {
    let mut last = json!({ "author": req.user_name, "text": req.message });
    if let Some(target) = &req.reply_target {
        last["replying_to"] = target.as_str().into();
    }
    json!({ "conversation": recent(req).join("\n"), "last_message": last })
}

pub fn reply_state(user_message: &str, reply: &str) -> Value {
    json!({ "user_message": user_message, "bot_reply": reply.chars().take(1200).collect::<String>() })
}

pub fn memory_id(m: &Memory) -> String {
    format!("m{}", m.id)
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

pub fn followup() -> Question {
    Question::noul(
        "The Trickster is the bot speaking in this chat. Does the last human message continue an exchange \
         with the bot? Focus on its most recent remark and the messages after it. A short reaction or request \
         for clarification immediately after the bot's remark, such as '??', 'huh', or 'what do you mean', \
         is a response even without a mention or reply link. Answers, thanks, disagreement, and criticism \
         of the bot also count. 'You' or 'the bot' can refer to The Trickster. A different participant can \
         join the exchange. Answer false when no bot remark appears, the topic has moved on, or the message \
         answers another human. Do not require a complete sentence or an explicit bot name.",
        "yes: this is a reaction or response to the bot",
        "no: this is unrelated or addressed to another human",
    )
}

pub fn wrong_claim() -> Question {
    Question::noul(
        "Does the last message confidently state something as fact that is clearly and verifiably false: a wrong \
         date, number, definition, scientific or historical fact? Answer false for jokes, sarcasm, hyperbole, bits, \
         opinions, predictions, personal experiences, questions, hedged guesses, and anything that is disputed or \
         that you are not sure is wrong.",
        "yes: a confident, checkable claim that is actually false",
        "no: true, unverifiable, opinion, joke, or unsure",
    )
}

pub fn argument() -> Question {
    Question::noul(
        "Across the recent conversation and the last message, are two or more people having a sustained back-and-forth \
         disagreement, each pushing their own position over several messages? Answer false for one-off \
         disagreements, friendly banter or roasting with no real dispute, people agreeing, a single person ranting, \
         or a conversation with The Trickster, the bot.",
        "yes: an ongoing argument between people",
        "no: no real ongoing dispute",
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

pub fn refusal() -> Question {
    Question::noul(
        "The Trickster is a smug, sarcastic Discord persona. Is the bot's reply a refusal or safety disclaimer in a \
         plain assistant voice (e.g. \"I can't help with that\", offering a harmless alternative) instead of staying \
         in character? A decline delivered as an in-character joke counts as false.",
        "out-of-character refusal",
        "in character, including in-character declines",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_keeps_the_last_lines_and_the_current_message_separate() {
        let mut req = crate::ai::tests::request("新しい message");
        req.conversation = (1..=12).map(|i| format!("u: {i}")).collect();
        let s = state(&req);
        assert_eq!(s["conversation"], (5..=12).map(|i| format!("u: {i}")).collect::<Vec<_>>().join("\n"));
        assert_eq!(s["last_message"], json!({"author": "alice", "text": "新しい message"}));
        req.reply_target = Some("bob: hi".into());
        assert_eq!(state(&req)["last_message"]["replying_to"], "bob: hi");
    }

    #[test]
    fn refusal_state_contains_both_texts_and_truncates_on_char_boundary() {
        let reply = "🙂".repeat(1201);
        let s = reply_state("user text", &reply);
        assert_eq!(s["user_message"], "user text");
        assert_eq!(s["bot_reply"].as_str().unwrap().chars().count(), 1200);
    }
}
