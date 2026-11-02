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
    channel::{Message, message::Embed},
    id::{
        Id,
        marker::{ChannelMarker, MessageMarker, UserMarker},
    },
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
    /// Display names for Discord mentions in this conversation.
    pub user_mentions: HashMap<String, Id<UserMarker>>,
}

impl AiRequest {
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

impl Bot {
    /// Text visible to the models, including voice transcripts and Discord embed metadata.
    fn message_content(&self, id: Id<MessageMarker>, content: &str, embeds: &[Embed]) -> String {
        let mut text = self.voice.content(id.get(), content);
        for embed in embeds {
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
        recent
            .iter()
            .filter_map(|id| self.cache.message(*id))
            .map(|message| {
                let author = message.author();
                let name = if author == self.id {
                    PERSONA.to_owned()
                } else if let Some(user) = self.cache.user(author) {
                    mentions.insert(user.name.clone(), author);
                    user.name.clone()
                } else {
                    author.to_string()
                };
                let line =
                    transcript_line(&name, &self.message_content(message.id(), message.content(), message.embeds()));
                (message.id(), line)
            })
            .unzip()
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
            transcript_line(name, &self.message_content(reference.id, &reference.content, &reference.embeds))
        });
        AiRequest {
            user_id: msg.author.id,
            user_name: msg.author.name.clone(),
            channel_id: msg.channel_id,
            message_id: msg.id,
            message: self.message_content(msg.id, &msg.content, &msg.embeds),
            source_text: msg.content.clone(),
            media: media::sources(msg),
            conversation,
            conversation_ids,
            reply_target,
            reply_target_id: msg.referenced_message.as_ref().map(|reference| reference.id),
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
