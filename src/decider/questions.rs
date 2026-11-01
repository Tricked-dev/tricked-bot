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
         that needs no reply. Also stay quiet for acknowledgements, closing reactions, requests to stop, \
         and people discussing the bot with each other. Talking about the bot alone is not a reason to interrupt.",
        "yes: jumping in now would be natural and welcome",
        "no: stay quiet",
    )
}

pub fn followup() -> Question {
    Question::noul(
        "Does the last human message warrant a response from The Trickster, the server bot? \
         TRUE: a question or request addressed to the bot, a correction it should address, clarification \
         ('??', 'huh', 'what do you mean'), an answer to a question it just asked, or playful banter inviting \
         a comeback. Mentions and reply links are optional; infer the addressee from the exchange. \
         FALSE: acknowledgements ('true', 'peak', 'that's funny', 'thanks', 'looks good enough'), closing \
         remarks, complaints that it talks too much, requests to stop, and conversation between humans. \
         An answer to another human's question is FALSE even when the topic is the bot. A person adding to \
         their own account ('I added caching' then 'or at least it said it did') is also FALSE. \
         Merely referring to the bot does not warrant a reply. Discussion of other AI tools, coding agents, \
         prompts or implementations is not an exchange with The Trickster unless the human addresses it. \
         Crucially, a short clarification such as '??' IS a request for an answer, even after a joke. \
         A one-word answer such as 'nixos' to the bot's 'which distro?' IS a continuation, not an acknowledgement. \
         A correction directed at the bot IS worth addressing, even if other AI tools were discussed earlier.",
        "yes: a question, request, clarification, answer to the bot's question, correction, or invited banter",
        "no: acknowledgement, closing remark, or conversation not addressed to the bot",
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

pub fn poll() -> Question {
    Question::noul(
        "Would a short poll help the room choose between clear alternatives raised in the last message or recent \
         conversation? An open question about preferences, a group decision, or an unresolved opinion disagreement \
         can qualify; an argument is NOT required. For example, 'miku or teto, who is better?' or 'which game should \
         we play tonight, minecraft or terraria?' can be put to a vote. Answer false for factual questions with a \
         correct answer, private exchanges, jokes with no real choice, bare links, acknowledgements, serious personal \
         matters, or topics the room has already settled. Do not invent a topic just to post a poll.",
        "yes: an open choice that the room could usefully vote on",
        "no: no useful unresolved choice for a poll",
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
