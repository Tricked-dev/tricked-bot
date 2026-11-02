//! Everything behind the bot's AI persona: prompting, streaming replies, memories and media.

pub mod budget;
pub mod cleanup;
pub mod embeddings;
pub mod gif;
pub mod interject;
pub mod media;
pub mod memory;
pub mod openrouter;
pub mod output;
pub mod predictions;
pub mod prompt;
pub mod recall;
pub mod refusal;
#[cfg(test)]
mod replay;
pub mod reply;
pub mod timing;
pub mod voice;

use std::{collections::HashMap, fmt::Write as _};

use twilight_model::{
    channel::{
        Message,
        message::{Embed, MessageType},
    },
    id::{
        Id,
        marker::{ChannelMarker, MessageMarker, UserMarker},
    },
    poll::Poll,
};

use crate::bot::Bot;

/// The name the persona uses for itself in transcripts and prompts.
pub const PERSONA: &str = "The Trickster";

/// Upper bound on characters taken from any single message.
const MESSAGE_CHARS: usize = 2400;
/// How many earlier channel messages are included as conversation context.
const CONTEXT_MESSAGES: usize = 25;

/// Everything a reply needs about one message, detached from the gateway cache.
#[derive(Clone, Debug)]
pub struct AiRequest {
    pub user_id: Id<UserMarker>,
    pub user_name: String,
    pub channel_id: Id<ChannelMarker>,
    pub message_id: Id<MessageMarker>,
    pub message: String,
    /// Original gateway content, independent of the transcript used by the models.
    pub source_text: String,
    pub media: Vec<media::MediaSource>,
    /// Earlier channel messages as `name: text` lines, oldest first.
    pub conversation: Vec<String>,
    /// The ID of each `conversation` line, for when it was sent. Lines past its end have no known time.
    pub conversation_ids: Vec<Id<MessageMarker>>,
    /// The message this one replies to, as a `name: text` line.
    pub reply_target: Option<String>,
    pub reply_target_id: Option<Id<MessageMarker>>,
    /// Whether Discord allows replying to the message; system messages like poll results can't be replied to.
    pub replyable: bool,
    /// Display names for Discord mentions in this conversation.
    pub user_mentions: HashMap<String, Id<UserMarker>>,
}

impl AiRequest {
    /// The message to reply to, or `None` when Discord doesn't allow it.
    pub fn reply_ref(&self) -> Option<Id<MessageMarker>> {
        self.replyable.then_some(self.message_id)
    }

    /// Who each mentioned ID is, for resolving `<@id>` mentions.
    pub fn mention_names(&self) -> HashMap<Id<UserMarker>, &str> {
        self.user_mentions.iter().map(|(name, id)| (*id, name.as_str())).collect()
    }
}

/// Whether the message pings or replies to the bot.
pub fn is_direct(msg: &Message, bot: Id<UserMarker>) -> bool {
    msg.mentions.iter().any(|user| user.id == bot)
        || msg.content.contains(&format!("<@{bot}>"))
        || msg.content.contains(&format!("<@!{bot}>"))
        || msg.referenced_message.as_ref().is_some_and(|reply| reply.author.id == bot)
}

fn transcript_line(name: &str, content: &str) -> String {
    let content: String = content.chars().take(MESSAGE_CHARS).collect();
    format!("{name}: {}", content.replace('\n', " "))
}

fn push_line(text: &mut String, line: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(line);
}

/// A poll as one line with its vote counts, like `[poll: game tonight? | minecraft 3, terraria 1 | 40m left]`.
fn poll_text(poll: &Poll, now_ms: i64) -> String {
    let answers: Vec<_> = poll
        .answers
        .iter()
        .map(|answer| {
            let text = answer.poll_media.text.as_deref().unwrap_or_default();
            // Discord leaves results out until someone votes.
            let count = poll.results.iter().flat_map(|results| &results.answer_counts);
            let count = count.filter(|count| count.id == answer.answer_id).map(|count| count.count).sum::<u64>();
            format!("{text} {count}")
        })
        .collect();
    let finalized = poll.results.as_ref().is_some_and(|results| results.is_finalized);
    let left_ms = poll.expiry.map(|expiry| expiry.as_micros() / 1000 - now_ms);
    let state = match left_ms {
        _ if finalized => " | closed".to_owned(),
        Some(left) if left <= 0 => " | closed".to_owned(),
        Some(left) => format!(" | {} left", prompt::short_age(u64::try_from(left / 60_000).unwrap_or_default())),
        None => String::new(),
    };
    format!("[poll: {} | {}{state}]", poll.question.text.as_deref().unwrap_or_default(), answers.join(", "))
}

/// Discord's poll result embed as one line, like `[poll ended: game tonight? | winner minecraft, 3 of 4 votes]`.
fn poll_result_text(embed: &Embed) -> Option<String> {
    if embed.kind != "poll_result" {
        return None;
    }
    let field = |name: &str| embed.fields.iter().find(|field| field.name == name).map(|field| field.value.as_str());
    let question = field("poll_question_text").unwrap_or_default();
    let total = field("total_votes").unwrap_or("0");
    Some(match (field("victor_answer_text"), field("victor_answer_votes")) {
        (Some(winner), Some(votes)) => format!("[poll ended: {question} | winner {winner}, {votes} of {total} votes]"),
        _ => format!("[poll ended: {question} | no single winner, {total} votes]"),
    })
}

/// Whether Discord allows replying to `msg`, which it refuses for system messages.
pub fn replyable(msg: &Message) -> bool {
    matches!(
        msg.kind,
        MessageType::Regular | MessageType::Reply | MessageType::ChatInputCommand | MessageType::ContextMenuCommand
    )
}

/// Whether `msg` is Discord's announcement that a poll ended.
pub fn is_poll_result(msg: &Message) -> bool {
    msg.embeds.iter().any(|embed| embed.kind == "poll_result")
}

impl Bot {
    /// Text visible to the models, including voice transcripts, polls and Discord embed metadata.
    fn message_content(&self, id: Id<MessageMarker>, content: &str, embeds: &[Embed], poll: Option<&Poll>) -> String {
        let mut text = self.voice.content(id.get(), content);
        if let Some(poll) = poll {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |now| i64::try_from(now.as_millis()).unwrap_or(i64::MAX));
            push_line(&mut text, &poll_text(poll, now_ms));
        }
        for embed in embeds {
            if let Some(result) = poll_result_text(embed) {
                push_line(&mut text, &result);
                continue;
            }
            let mut parts = Vec::new();
            parts.extend(embed.author.as_ref().map(|author| author.name.as_str()));
            parts.extend(embed.title.as_deref());
            parts.extend(embed.url.as_deref());
            parts.extend(embed.description.as_deref());
            for field in &embed.fields {
                parts.push(&field.name);
                parts.push(&field.value);
            }
            parts.extend(embed.footer.as_ref().map(|footer| footer.text.as_str()));
            parts.retain(|part| !part.trim().is_empty());
            if !parts.is_empty() {
                let _ = write!(text, "\n[embed]\n{}", parts.join("\n"));
            }
        }
        text.chars().take(MESSAGE_CHARS).collect()
    }

    /// The last [`CONTEXT_MESSAGES`] cached messages in `channel` before `before`, as their IDs and transcript lines,
    /// recording the names of their authors in `mentions`.
    pub fn transcript(
        &self,
        channel: Id<ChannelMarker>,
        before: Id<MessageMarker>,
        mentions: &mut HashMap<String, Id<UserMarker>>,
    ) -> (Vec<Id<MessageMarker>>, Vec<String>) {
        let Some(ids) = self.cache.channel_messages(channel) else {
            return (Vec::new(), Vec::new());
        };
        let mut earlier: Vec<_> = ids.iter().copied().filter(|id| *id < before).collect();
        earlier.sort_unstable();
        let recent = &earlier[earlier.len().saturating_sub(CONTEXT_MESSAGES)..];
        let replied_to = |id: &Id<MessageMarker>| {
            self.cache.message(*id).and_then(|message| message.reference().and_then(|reference| reference.message_id))
        };
        // Replied-to lines in the window get a short tag, in order of appearance, for their replies to point at.
        let mut tags: Vec<_> = recent.iter().filter_map(replied_to).filter(|id| recent.contains(id)).collect();
        tags.sort_unstable();
        tags.dedup();
        let tag = |id: Id<MessageMarker>| tags.iter().position(|tagged| *tagged == id).map(|index| index + 1);
        recent
            .iter()
            .filter_map(|id| self.cache.message(*id))
            .map(|message| {
                let mut name = self.author_name(message.author(), mentions);
                if let Some(tag) = tag(message.id()) {
                    let _ = write!(name, " #{tag}");
                }
                if let Some(target) = replied_to(&message.id()) {
                    if let Some(tag) = tag(target) {
                        let _ = write!(name, "→#{tag}");
                    } else if let Some(author) = self.cache.message(target).map(|target| target.author()) {
                        let _ = write!(name, "→{}", self.author_name(author, mentions));
                    }
                }
                let content =
                    self.message_content(message.id(), message.content(), message.embeds(), message.poll().as_ref());
                (message.id(), transcript_line(&name, &content))
            })
            .unzip()
    }

    /// How `author` is named in transcripts, recording their name in `mentions`.
    fn author_name(&self, author: Id<UserMarker>, mentions: &mut HashMap<String, Id<UserMarker>>) -> String {
        if author == self.id {
            PERSONA.to_owned()
        } else if let Some(user) = self.cache.user(author) {
            mentions.insert(user.name.clone(), author);
            user.name.clone()
        } else {
            author.to_string()
        }
    }

    pub fn snapshot(&self, msg: &Message) -> AiRequest {
        let mut user_mentions = HashMap::from([(msg.author.name.clone(), msg.author.id)]);
        let (conversation_ids, conversation) = self.transcript(msg.channel_id, msg.id, &mut user_mentions);
        // A reply can target an older message outside the cached transcript. Keep
        // its identity and content explicit even when newer channel messages exist.
        let reply_target = msg.referenced_message.as_ref().map(|reference| {
            let name = if reference.author.id == self.id {
                PERSONA
            } else {
                user_mentions.insert(reference.author.name.clone(), reference.author.id);
                &reference.author.name
            };
            // Discord sends a replied-to poll without its votes; the cached copy has them counted.
            let poll =
                self.cache.message(reference.id).and_then(|cached| cached.poll()).or_else(|| reference.poll.clone());
            transcript_line(
                name,
                &self.message_content(reference.id, &reference.content, &reference.embeds, poll.as_ref()),
            )
        });
        AiRequest {
            user_id: msg.author.id,
            user_name: msg.author.name.clone(),
            channel_id: msg.channel_id,
            message_id: msg.id,
            message: self.message_content(msg.id, &msg.content, &msg.embeds, msg.poll.as_ref()),
            source_text: msg.content.clone(),
            media: media::sources(msg),
            conversation,
            conversation_ids,
            reply_target,
            reply_target_id: msg.referenced_message.as_ref().map(|reference| reference.id),
            replyable: replyable(msg),
            user_mentions,
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bot::tests::{message, test_bot};
    use twilight_model::gateway::{event::Event, payload::incoming::MessageCreate};

    /// A request from alice with no conversation around it.
    pub fn request(message: &str) -> AiRequest {
        AiRequest {
            user_id: Id::new(2),
            user_name: "alice".into(),
            channel_id: Id::new(1),
            message_id: Id::new(3),
            message: message.into(),
            source_text: message.into(),
            media: Vec::new(),
            conversation: Vec::new(),
            conversation_ids: Vec::new(),
            reply_target: None,
            reply_target_id: None,
            replyable: true,
            user_mentions: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn embeds_are_read_in_current_referenced_and_cached_messages() {
        let bot = test_bot();
        let embed: Embed = serde_json::from_value(serde_json::json!({
            "type": "rich", "author": {"name": "News author"},
            "title": "Link title", "url": "https://example.com/story",
            "description": "Story summary", "fields": [{"name": "Score", "value": "42"}],
            "footer": {"text": "Source footer"}
        }))
        .unwrap();
        let mut previous = message(40, 2, "alice", "");
        previous.embeds.push(embed.clone());
        bot.cache.update(&Event::MessageCreate(Box::new(MessageCreate(previous.clone()))));
        let mut current = message(42, 2, "alice", "Thoughts?");
        current.embeds.push(embed);
        current.referenced_message = Some(Box::new(previous));
        let req = bot.snapshot(&current);
        for text in [req.message.as_str(), req.reply_target.as_deref().unwrap(), &req.conversation[0]] {
            for expected in [
                "[embed]",
                "News author",
                "Link title",
                "https://example.com/story",
                "Story summary",
                "Score",
                "42",
                "Source footer",
            ] {
                assert!(text.contains(expected), "missing {expected:?} in {text:?}");
            }
        }
        assert_eq!(req.source_text, "Thoughts?");
    }

    #[tokio::test]
    async fn embed_text_keeps_voice_transcripts_and_character_limits() {
        let bot = test_bot();
        let mut current = message(42, 2, "alice", "caption");
        bot.voice.remember(current.id.get(), "spoken words".into());
        let plain = bot.snapshot(&current).message;
        current.embeds.push(serde_json::from_value(serde_json::json!({"type": "image"})).unwrap());
        assert_eq!(bot.snapshot(&current).message, plain, "empty metadata adds no marker");
        current.embeds.push(
            serde_json::from_value(serde_json::json!({
                "type": "rich", "description": "🦊".repeat(MESSAGE_CHARS)
            }))
            .unwrap(),
        );
        let text = bot.snapshot(&current).message;
        assert!(text.starts_with("caption\n[voice transcript] spoken words\n[embed]\n"));
        assert_eq!(text.chars().count(), MESSAGE_CHARS);
    }

    #[tokio::test]
    async fn explicit_reply_target_is_kept_when_channel_history_exists() {
        let bot = test_bot();
        bot.cache.update(&Event::MessageCreate(Box::new(MessageCreate(message(40, 2, "alice", "old bird topic")))));
        let mut current = message(42, 2, "alice", "Thoughts?");
        current.referenced_message = Some(Box::new(message(20, 3, "bot_account", "Level: 37, position: 12")));
        let req = bot.snapshot(&current);
        assert_eq!(req.reply_target.as_deref(), Some("The Trickster: Level: 37, position: 12"));
        assert_eq!(req.conversation, ["alice: old bird topic"]);
        assert_eq!(req.conversation_ids, [Id::new(40)]);
        assert_eq!(req.reply_target_id, Some(Id::new(20)));
    }

    #[tokio::test]
    async fn polls_and_poll_results_read_as_text() {
        let bot = test_bot();
        let poll: Poll = serde_json::from_value(serde_json::json!({
            "question": {"text": "game tonight?"},
            "answers": [
                {"answer_id": 1, "poll_media": {"text": "minecraft"}},
                {"answer_id": 2, "poll_media": {"text": "terraria"}},
            ],
            "expiry": null, "allow_multiselect": false, "layout_type": 1,
            "results": {"answer_counts": [{"id": 2, "count": 3, "me_voted": false}], "is_finalized": true},
        }))
        .unwrap();
        let mut asked = message(40, 3, "bot", "");
        asked.poll = Some(poll.clone());
        assert_eq!(bot.snapshot(&asked).message, "[poll: game tonight? | minecraft 0, terraria 3 | closed]");
        asked.poll.as_mut().unwrap().results = None;
        assert_eq!(bot.snapshot(&asked).message, "[poll: game tonight? | minecraft 0, terraria 0]");
        let mut open = poll.clone();
        open.results = None;
        open.expiry = Some(twilight_model::util::Timestamp::from_secs(1_800_000_000).unwrap());
        let now_ms = 1_800_000_000_000 - 40 * 60_000;
        assert_eq!(poll_text(&open, now_ms), "[poll: game tonight? | minecraft 0, terraria 0 | 40m left]");
        assert_eq!(poll_text(&open, 1_800_000_000_001), "[poll: game tonight? | minecraft 0, terraria 0 | closed]");

        let mut ended = message(42, 3, "bot", "");
        let fields = |extra: &[(&str, &str)]| -> Embed {
            let mut fields = vec![("poll_question_text", "game tonight?"), ("total_votes", "4")];
            fields.extend_from_slice(extra);
            let fields: Vec<_> =
                fields.iter().map(|(name, value)| serde_json::json!({"name": name, "value": value})).collect();
            serde_json::from_value(serde_json::json!({"type": "poll_result", "fields": fields})).unwrap()
        };
        ended.embeds.push(fields(&[("victor_answer_votes", "3"), ("victor_answer_text", "terraria")]));
        asked.poll = Some(poll);
        ended.referenced_message = Some(Box::new(asked));
        assert!(is_poll_result(&ended));
        ended.kind = MessageType::Unknown(46);
        assert!(!bot.snapshot(&ended).replyable);
        assert!(bot.snapshot(&message(46, 2, "alice", "hi")).replyable);
        let req = bot.snapshot(&ended);
        assert_eq!(req.message, "[poll ended: game tonight? | winner terraria, 3 of 4 votes]");
        assert_eq!(
            req.reply_target.as_deref(),
            Some("The Trickster: [poll: game tonight? | minecraft 0, terraria 3 | closed]")
        );
        ended.embeds = vec![fields(&[("victor_answer_votes", "2")])];
        assert_eq!(bot.snapshot(&ended).message, "[poll ended: game tonight? | no single winner, 4 votes]");
        assert!(!is_poll_result(&message(43, 2, "alice", "hi")));

        // A reply to a cached poll reads the votes counted on the cache, not Discord's empty copy.
        let mut cached = message(44, 3, "bot", "");
        cached.poll = ended.referenced_message.as_ref().unwrap().poll.clone();
        cached.poll.as_mut().unwrap().results = None;
        bot.cache.update(&Event::MessageCreate(Box::new(MessageCreate(cached.clone()))));
        bot.cache.message(cached.id).unwrap().vote(1, true);
        let mut reply = message(45, 2, "alice", "who's winning?");
        cached.poll.as_mut().unwrap().results =
            Some(twilight_model::poll::PollResults { answer_counts: Vec::new(), is_finalized: false });
        reply.referenced_message = Some(Box::new(cached));
        assert_eq!(
            bot.snapshot(&reply).reply_target.as_deref(),
            Some("The Trickster: [poll: game tonight? | minecraft 1, terraria 0]")
        );
    }

    #[tokio::test]
    async fn replies_in_the_transcript_point_at_short_tags() {
        let bot = test_bot();
        let add = |id: u64, author: u64, name: &str, target: Option<u64>| {
            let mut msg = message(id, author, name, "x");
            if let Some(target) = target {
                msg.kind = MessageType::Reply;
                msg.reference =
                    Some(serde_json::from_value(serde_json::json!({"message_id": target.to_string()})).unwrap());
            }
            bot.cache.update(&Event::MessageCreate(Box::new(MessageCreate(msg))));
        };
        add(1, 4, "bob", None);
        for id in 2..=20 {
            add(id, 5, "carol", None);
        }
        add(40, 4, "bob", None);
        add(41, 2, "alice", None);
        add(42, 2, "alice", Some(40));
        add(43, 4, "bob", Some(41));
        add(44, 4, "bob", Some(40));
        add(45, 2, "alice", Some(1));
        add(46, 2, "alice", Some(9_999));
        let req = bot.snapshot(&message(47, 2, "alice", "now"));
        let tail: Vec<_> =
            req.conversation.iter().rev().take(7).rev().map(|line| line.trim_end_matches(": x")).collect();
        assert_eq!(tail, ["bob #1", "alice #2", "alice→#1", "bob→#2", "bob→#1", "alice→bob", "alice"]);
    }

    #[test]
    fn direct_means_mention_or_reply_to_the_bot() {
        let bot = Id::new(3);
        assert!(is_direct(&message(1, 2, "alice", "hey <@3>"), bot));
        assert!(is_direct(&message(1, 2, "alice", "hey <@!3>"), bot));
        assert!(!is_direct(&message(1, 2, "alice", "hey <@33>"), bot));
        let mut reply = message(1, 2, "alice", "huh");
        reply.referenced_message = Some(Box::new(message(10, 3, "bot", "hi")));
        assert!(is_direct(&reply, bot));
    }
}
