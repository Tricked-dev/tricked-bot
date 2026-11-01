//! Everything behind the bot's AI persona: prompting, streaming replies, memories and media.

pub mod budget;
pub mod cleanup;
pub mod media;
pub mod memory;
pub mod openrouter;
pub mod output;
pub mod prompt;
pub mod recall;
pub mod refusal;
pub mod reply;

use std::collections::HashMap;

use twilight_model::{
    channel::Message,
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
    pub media: Vec<media::MediaSource>,
    /// Earlier channel messages as `name: text` lines, oldest first.
    pub conversation: Vec<String>,
    /// The message this one replies to, as a `name: text` line.
    pub reply_target: Option<String>,
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
    pub fn snapshot(&self, msg: &Message) -> AiRequest {
        let mut user_mentions = HashMap::from([(msg.author.name.clone(), msg.author.id)]);
        let mut conversation = Vec::new();
        if let Some(ids) = self.cache.channel_messages(msg.channel_id) {
            let mut earlier: Vec<_> = ids.iter().copied().filter(|id| *id < msg.id).collect();
            earlier.sort_unstable();
            for id in &earlier[earlier.len().saturating_sub(CONTEXT_MESSAGES)..] {
                let Some(message) = self.cache.message(*id) else {
                    continue;
                };
                let author = message.author();
                let name = if author == self.id {
                    PERSONA.to_owned()
                } else if let Some(user) = self.cache.user(author) {
                    user_mentions.insert(user.name.clone(), author);
                    user.name.clone()
                } else {
                    author.to_string()
                };
                conversation.push(transcript_line(&name, message.content()));
            }
        }
        // A reply can target an older message outside the cached transcript. Keep
        // its identity and content explicit even when newer channel messages exist.
        let reply_target = msg.referenced_message.as_ref().map(|reference| {
            let name = if reference.author.id == self.id {
                PERSONA
            } else {
                user_mentions.insert(reference.author.name.clone(), reference.author.id);
                &reference.author.name
            };
            transcript_line(name, &reference.content)
        });
        AiRequest {
            user_id: msg.author.id,
            user_name: msg.author.name.clone(),
            channel_id: msg.channel_id,
            message_id: msg.id,
            message: msg.content.chars().take(MESSAGE_CHARS).collect(),
            media: media::sources(msg),
            conversation,
            reply_target,
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
            media: Vec::new(),
            conversation: Vec::new(),
            reply_target: None,
            user_mentions: HashMap::new(),
        }
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
