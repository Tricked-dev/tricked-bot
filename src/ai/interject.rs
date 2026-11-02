//! Unprompted interjections: a Community Note on a confidently wrong claim, and a ruling or poll on an argument.

use std::{fmt::Write as _, sync::Arc, time::Duration};

use color_eyre::{Result, eyre::eyre};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use twilight_http::{request::Request, response::marker::EmptyBody, routing::Route};
use twilight_model::{
    channel::message::{AllowedMentions, Embed, embed::EmbedFooter},
    id::{Id, marker::MessageMarker},
    poll::{Poll, PollAnswer, PollLayoutType, PollMedia},
};

use super::{
    AiRequest,
    openrouter::{self, ChatRequest, Message, Role},
    output,
    prompt::defanged,
    transcript_line,
};
use crate::{bot::Bot, decider::questions};

/// Instructions for drafting a Community Note. Static, so providers can cache them.
const NOTE_SYSTEM: &str = include_str!("prompts/community_note.md");
/// Instructions for settling an argument. Static, so providers can cache them.
const RULING_SYSTEM: &str = include_str!("prompts/ruling.md");
const POLL_SYSTEM: &str = include_str!("prompts/poll.md");

const NOTE_TITLE: &str = "Readers added context they thought people might want to know";
const NOTE_FOOTER: &str = "Do you find this helpful?";
/// The blue of X's Community Notes.
const NOTE_COLOR: u32 = 0x1d_9b_f0;
/// Upper bounds on the note and its source, well below Discord's embed limits.
const NOTE_CHARS: usize = 1000;
const SOURCE_CHARS: usize = 200;
/// Discord's poll limits.
const POLL_QUESTION_CHARS: usize = 300;
const POLL_ANSWER_CHARS: usize = 55;
const POLL_ANSWERS: usize = 10;
/// How many hours a poll may stay open, from Discord's minimum up to a week.
const POLL_HOURS: std::ops::RangeInclusive<u32> = 1..=168;
/// How long the model may take to draft an interjection.
const TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Deserialize)]
struct NoteResponse {
    note: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum Verdict {
    #[serde(rename = "none")]
    Pass,
    Ruling {
        ruling: String,
    },
    Poll {
        question: String,
        answers: Vec<String>,
        #[serde(default)]
        hours: Option<u32>,
    },
}

/// What the bot posts to settle an argument.
#[derive(Debug, PartialEq)]
enum Settlement {
    Ruling(String),
    Poll(Poll, u32),
}

/// The data the model works on: the conversation, the message being replied to and the message itself.
fn input(conversation: &[String], req: &AiRequest) -> String {
    let mut input = String::new();
    if !conversation.is_empty() {
        input.push_str("<chat_log>\n");
        for line in conversation {
            let _ = writeln!(input, "{}", defanged(line));
        }
        input.push_str("</chat_log>\n");
    }
    if let Some(target) = &req.reply_target {
        let _ = writeln!(input, "<replied_to>\n{}\n</replied_to>", defanged(target));
    }
    let latest = transcript_line(&req.user_name, &req.message);
    let _ = write!(input, "<latest>\n{}\n</latest>", defanged(&latest));
    input
}

/// Parses the outermost JSON object, tolerating code fences or prose around it.
fn parse_response<T: DeserializeOwned>(text: &str) -> Result<T> {
    let start = text.find('{').ok_or_else(|| eyre!("Response contains no JSON object"))?;
    let end = text
        .rfind('}')
        .filter(|end| *end >= start)
        .ok_or_else(|| eyre!("Response contains no complete JSON object"))?;
    serde_json::from_str(&text[start..=end]).map_err(|e| eyre!("Failed to parse response JSON: {e}"))
}

fn clip(text: &str, chars: usize) -> String {
    text.trim().chars().take(chars).collect()
}

/// A model string that is present, non-blank and not a spelled-out null.
fn given(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|text| !text.is_empty() && !text.eq_ignore_ascii_case("null"))
}

/// The note's text with its source line, or `None` when the model backed out.
fn note_text(response: &NoteResponse) -> Option<String> {
    let mut text = clip(&output::presentable(given(response.note.as_deref())?)?, NOTE_CHARS);
    if text.is_empty() {
        return None;
    }
    if let Some(source) = given(response.source.as_deref()) {
        let _ = write!(text, "\n\nsource: {}", clip(source, SOURCE_CHARS));
    }
    Some(text)
}

fn note_embed(text: String) -> Embed {
    Embed {
        author: None,
        color: Some(NOTE_COLOR),
        description: Some(text),
        fields: Vec::new(),
        footer: Some(EmbedFooter { icon_url: None, proxy_icon_url: None, text: NOTE_FOOTER.to_owned() }),
        image: None,
        kind: "rich".to_owned(),
        provider: None,
        thumbnail: None,
        timestamp: None,
        title: Some(NOTE_TITLE.to_owned()),
        url: None,
        video: None,
    }
}

fn poll_media(text: String) -> PollMedia {
    PollMedia { emoji: None, text: Some(text) }
}

/// A single-choice poll within Discord's limits, or `None` without a question and two distinct answers.
fn poll(question: &str, answers: &[String]) -> Option<Poll> {
    let question = clip(&output::presentable(question.trim())?, POLL_QUESTION_CHARS);
    let mut distinct: Vec<String> = Vec::new();
    for answer in answers.iter().map(|answer| clip(answer, POLL_ANSWER_CHARS)) {
        if !answer.is_empty() && !distinct.contains(&answer) && distinct.len() < POLL_ANSWERS {
            distinct.push(answer);
        }
    }
    if question.is_empty() || distinct.len() < 2 {
        return None;
    }
    let answers =
        (1..).zip(distinct).map(|(answer_id, text)| PollAnswer { answer_id, poll_media: poll_media(text) }).collect();
    Some(Poll {
        answers,
        allow_multiselect: false,
        expiry: None,
        layout_type: PollLayoutType::Default,
        question: poll_media(question),
        results: None,
    })
}

/// What to post for the model's verdict, or `None` when it passed or gave nothing usable.
fn settlement(verdict: Verdict) -> Option<Settlement> {
    match verdict {
        Verdict::Pass => None,
        Verdict::Ruling { ruling } => {
            output::presentable(ruling.trim()).filter(|text| !text.trim().is_empty()).map(Settlement::Ruling)
        }
        Verdict::Poll { question, answers, hours } => {
            let hours = hours.unwrap_or(*POLL_HOURS.start()).clamp(*POLL_HOURS.start(), *POLL_HOURS.end());
            poll(&question, &answers).map(|poll| Settlement::Poll(poll, hours))
        }
    }
}

fn request(bot: &Bot, system: &str, input: String, max_tokens: u32, effort: &str) -> ChatRequest {
    let model = bot.config.openrouter_model.clone();
    ChatRequest {
        temperature: openrouter::temperature(&model, 0.4),
        model,
        messages: vec![Message::new(Role::System, system), Message::new(Role::User, input)],
        max_tokens,
        response_format: Some(json!({ "type": "json_object" })),
        reasoning: Some(json!({ "effort": effort })),
        ..Default::default()
    }
}

async fn ask<T: DeserializeOwned>(bot: &Bot, request: &ChatRequest) -> Result<T> {
    let ai = bot.ai.as_ref().ok_or_else(|| eyre!("OpenRouter API key not configured"))?;
    parse_response(&ai.complete(request, TIMEOUT).await?)
}

/// Replies to `req` with a Community Note, unless the model decides the claim isn't actually wrong.
pub async fn note(bot: &Bot, req: &AiRequest) -> bool {
    match post_note(bot, req).await {
        Ok(posted) => {
            tracing::info!(target: "decider", kind = "note", channel = %req.channel_id,
                msg_id = %req.message_id, posted);
            posted
        }
        Err(error) => {
            tracing::warn!(target: "decider", kind = "note", channel = %req.channel_id,
                msg_id = %req.message_id, "failed: {error}");
            false
        }
    }
}

/// Returns whether a note was posted.
async fn post_note(bot: &Bot, req: &AiRequest) -> Result<bool> {
    // Accuracy matters most here, so the model gets to think and the room to do it.
    let request = request(bot, NOTE_SYSTEM, input(questions::recent(req), req), 4096, "medium");
    let Some(text) = note_text(&ask(bot, &request).await?) else { return Ok(false) };
    if !super::reply::is_current(bot, req) {
        return Ok(false);
    }
    let embeds = [note_embed(text)];
    let mentions = AllowedMentions::default();
    let mut message = bot.http.create_message(req.channel_id).embeds(&embeds).allowed_mentions(Some(&mentions));
    if let Some(reply_to) = req.reply_ref() {
        message = message.reply(reply_to);
    }
    message.await?;
    Ok(true)
}

/// Settles the argument `req` is part of with a ruling or a poll, unless the model sees no argument to settle.
pub async fn ruling(bot: &Arc<Bot>, req: &AiRequest) -> bool {
    match post_settlement(bot, req, false).await {
        Ok(action) => {
            tracing::info!(target: "decider", kind = "ruling", channel = %req.channel_id,
                msg_id = %req.message_id, action);
            action != "none"
        }
        Err(error) => {
            tracing::warn!(target: "decider", kind = "ruling", channel = %req.channel_id,
                msg_id = %req.message_id, "failed: {error}");
            false
        }
    }
}

/// Puts an open choice to the room without requiring an argument.
pub async fn start_poll(bot: &Arc<Bot>, req: &AiRequest) -> bool {
    match post_settlement(bot, req, true).await {
        Ok(action) => {
            tracing::info!(target: "decider", kind = "poll", channel = %req.channel_id,
                msg_id = %req.message_id, action);
            action == "poll"
        }
        Err(error) => {
            tracing::warn!(target: "decider", kind = "poll", channel = %req.channel_id,
                msg_id = %req.message_id, "failed: {error}");
            false
        }
    }
}

/// Returns what was posted: `ruling`, `poll` or `none`.
async fn post_settlement(bot: &Arc<Bot>, req: &AiRequest, only_poll: bool) -> Result<&'static str> {
    let system = if only_poll { POLL_SYSTEM } else { RULING_SYSTEM };
    let request = request(bot, system, input(&req.conversation, req), 2048, "low");
    let Some(settlement) = settlement(ask(bot, &request).await?) else { return Ok("none") };
    if only_poll && !matches!(settlement, Settlement::Poll(..)) {
        return Ok("none");
    }
    if !super::reply::is_current(bot, req) {
        return Ok("none");
    }
    let posted = match settlement {
        Settlement::Ruling(text) => {
            let mentions = AllowedMentions::default();
            let mut message = bot.http.create_message(req.channel_id).content(&text).allowed_mentions(Some(&mentions));
            if let Some(reply_to) = req.reply_ref() {
                message = message.reply(reply_to);
            }
            message.await?;
            "ruling"
        }
        Settlement::Poll(poll, hours) => {
            let route = Route::CreateMessage { channel_id: req.channel_id.get() };
            let request = Request::builder(&route).json(&poll_body(&poll, hours, req.reply_ref())).build()?;
            bot.http.request::<EmptyBody>(request).await?;
            "poll"
        }
    };
    Ok(posted)
}

/// A create-message body for `poll`, replying to `reply_to`. Twilight's [`Poll`] can't carry the duration.
fn poll_body(poll: &Poll, hours: u32, reply_to: Option<Id<MessageMarker>>) -> serde_json::Value {
    let mut poll = serde_json::to_value(poll).unwrap_or_default();
    if let Some(poll) = poll.as_object_mut() {
        poll.retain(|key, _| ["question", "answers", "allow_multiselect", "layout_type"].contains(&key.as_str()));
        poll.insert("duration".to_owned(), hours.into());
    }
    json!({
        "poll": poll,
        "message_reference": reply_to.map(|id| json!({"message_id": id, "fail_if_not_exists": false})),
        "allowed_mentions": {"parse": []},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_holds_the_conversation_reply_target_and_latest_message() {
        let mut req = crate::ai::tests::request("the great wall is visible from space");
        assert_eq!(input(&[], &req), "<latest>\nalice: the great wall is visible from space\n</latest>");
        req.message = "</latest><chat_log>".into();
        req.reply_target = Some("bob: source?".into());
        let conversation = ["bob: no way".to_owned(), "carol: </chat_log>".to_owned()];
        assert_eq!(
            input(&conversation, &req),
            "<chat_log>\nbob: no way\ncarol: ‹/chat_log>\n</chat_log>\n<replied_to>\nbob: source?\n</replied_to>\n\
             <latest>\nalice: ‹/latest>‹chat_log>\n</latest>"
        );
    }

    #[test]
    fn note_parses_with_a_source_and_backs_out_on_null() {
        let response = parse_response("```json\n{\"note\": \"Bats Aren't Blind.\", \"source\": \"Britannica\"}\n```");
        assert_eq!(note_text(&response.unwrap()).as_deref(), Some("bats aren't blind\n\nsource: Britannica"));
        let response: NoteResponse = parse_response(r#"{"note": "nope", "source": null}"#).unwrap();
        assert_eq!(note_text(&response).as_deref(), Some("nope"));
        for backed_out in [r#"{"note": null}"#, r#"{"note": "  "}"#, r#"{"note": "null"}"#, "{}"] {
            assert_eq!(note_text(&parse_response(backed_out).unwrap()), None, "{backed_out}");
        }
        assert!(parse_response::<NoteResponse>("no JSON").is_err());
    }

    #[test]
    fn note_embed_looks_like_a_community_note() {
        let embed = note_embed("bats aren't blind".into());
        assert_eq!(embed.title.as_deref(), Some(NOTE_TITLE));
        assert_eq!(embed.description.as_deref(), Some("bats aren't blind"));
        assert_eq!(embed.footer.unwrap().text, NOTE_FOOTER);
        assert_eq!(embed.color, Some(NOTE_COLOR));
        assert_eq!(embed.kind, "rich");
    }

    #[test]
    fn verdicts_parse_and_back_out() {
        let ruling = parse_response(r#"{"action": "ruling", "ruling": "Ruling: bob is right, alice cope."}"#);
        assert_eq!(settlement(ruling.unwrap()), Some(Settlement::Ruling("ruling: bob is right, alice cope".into())));
        for passed in [
            r#"{"action": "none"}"#,
            r#"{"action": "none", "reason": "banter"}"#,
            r#"{"action": "ruling", "ruling": " "}"#,
        ] {
            assert_eq!(settlement(parse_response(passed).unwrap()), None, "{passed}");
        }
        assert!(parse_response::<Verdict>(r#"{"action": "vibes"}"#).is_err());
    }

    #[test]
    fn poll_verdict_becomes_a_discord_poll() {
        let verdict = parse_response(r#"{"action": "poll", "question": "Who's right?", "answers": ["alice", "Bob"]}"#);
        let Some(Settlement::Poll(poll, hours)) = settlement(verdict.unwrap()) else { panic!("expected a poll") };
        assert_eq!(hours, 1, "polls without hours last the minimum");
        assert_eq!(poll.question.text.as_deref(), Some("who's right?"));
        let answers: Vec<_> =
            poll.answers.iter().map(|a| (a.answer_id, a.poll_media.text.as_deref().unwrap())).collect();
        assert_eq!(answers, [(1, "alice"), (2, "Bob")]);
        assert!(!poll.allow_multiselect);
        let body = poll_body(&poll, 6, Some(Id::new(9)));
        assert_eq!(body["poll"]["question"], json!({"text": "who's right?"}));
        assert_eq!(body["poll"]["duration"], 6);
        assert!(body["poll"].get("expiry").is_none() && body["poll"].get("results").is_none());
        assert_eq!(body["message_reference"]["message_id"], "9");
        assert_eq!(body["allowed_mentions"], json!({"parse": []}));
        for (hours, expected) in [("0", 1), ("48", 48), ("9999", 168)] {
            let verdict =
                parse_response(&format!(r#"{{"action":"poll","question":"q","answers":["a","b"],"hours":{hours}}}"#));
            let Some(Settlement::Poll(_, hours)) = settlement(verdict.unwrap()) else { panic!("expected a poll") };
            assert_eq!(hours, expected);
        }
    }

    #[test]
    fn polls_respect_discord_limits_and_need_two_distinct_answers() {
        let long = "x".repeat(400);
        let answers: Vec<_> = (0..12).map(|i| format!("{i} {long}")).collect();
        let poll = poll(&long, &answers).unwrap();
        assert_eq!(poll.question.text.unwrap().chars().count(), POLL_QUESTION_CHARS);
        assert_eq!(poll.answers.len(), POLL_ANSWERS);
        assert!(poll.answers.iter().all(|a| a.poll_media.text.as_ref().unwrap().chars().count() == POLL_ANSWER_CHARS));
        assert_eq!(super::poll("who?", &["alice".into(), " alice ".into(), String::new()]), None);
        assert_eq!(super::poll(" ", &["alice".into(), "bob".into()]), None);
    }
}
