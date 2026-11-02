//! Twilight's in-memory cache, with messages that keep their poll.

use std::{ops::Deref, sync::Mutex};

use twilight_cache_inmemory::{CacheableMessage, CacheableModels, InMemoryCache, model};
use twilight_model::{
    channel::{Channel, StageInstance, message::Message, message::Reaction},
    guild::{GuildIntegration, Role, scheduled_event::GuildScheduledEvent},
    poll::{AnswerCount, Poll, PollResults},
    user::{CurrentUser, User},
};

pub type Cache = InMemoryCache<Models>;

#[derive(Clone, Debug)]
pub struct Models;

impl CacheableModels for Models {
    type Channel = Channel;
    type CurrentUser = CurrentUser;
    type Emoji = model::CachedEmoji;
    type Guild = model::CachedGuild;
    type GuildIntegration = GuildIntegration;
    type Member = model::CachedMember;
    type Message = CachedMessage;
    type Presence = model::CachedPresence;
    type Role = Role;
    type StageInstance = StageInstance;
    type Sticker = model::CachedSticker;
    type User = User;
    type VoiceState = model::CachedVoiceState;
    type GuildScheduledEvent = GuildScheduledEvent;
}

/// Twilight's cached message plus its poll, which twilight keeps but doesn't expose. Votes arrive as their own
/// events, which twilight's cache ignores, so they're counted on the cached poll in place.
#[derive(Debug)]
pub struct CachedMessage {
    message: model::CachedMessage,
    poll: Option<Mutex<Poll>>,
}

impl CachedMessage {
    pub fn poll(&self) -> Option<Poll> {
        self.poll.as_ref().map(|poll| poll.lock().unwrap().clone())
    }

    /// Counts a vote added to, or removed from, `answer` on an open poll.
    pub fn vote(&self, answer: u8, added: bool) {
        let Some(poll) = &self.poll else { return };
        let mut poll = poll.lock().unwrap();
        let results =
            poll.results.get_or_insert_with(|| PollResults { answer_counts: Vec::new(), is_finalized: false });
        if results.is_finalized {
            return;
        }
        let counts = &mut results.answer_counts;
        let index = counts.iter().position(|count| count.id == answer).unwrap_or_else(|| {
            counts.push(AnswerCount { id: answer, count: 0, me_voted: false });
            counts.len() - 1
        });
        let count = &mut counts[index].count;
        *count = if added { *count + 1 } else { count.saturating_sub(1) };
    }
}

impl Clone for CachedMessage {
    fn clone(&self) -> Self {
        Self { message: self.message.clone(), poll: self.poll().map(Mutex::new) }
    }
}

impl PartialEq for CachedMessage {
    fn eq(&self, other: &Self) -> bool {
        self.message == other.message && self.poll() == other.poll()
    }
}

impl Deref for CachedMessage {
    type Target = model::CachedMessage;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl From<Message> for CachedMessage {
    fn from(mut message: Message) -> Self {
        let poll = message.poll.take().map(Mutex::new);
        Self { message: message.into(), poll }
    }
}

impl PartialEq<Message> for CachedMessage {
    fn eq(&self, other: &Message) -> bool {
        self.message == *other && self.poll() == other.poll
    }
}

impl CacheableMessage for CachedMessage {
    fn reactions(&self) -> &[Reaction] {
        self.message.reactions()
    }

    fn reactions_mut(&mut self) -> &mut [Reaction] {
        self.message.reactions_mut()
    }

    fn retain_reactions(&mut self, f: impl FnMut(&Reaction) -> bool) {
        self.message.retain_reactions(f);
    }

    fn clear_reactions(&mut self) {
        self.message.clear_reactions();
    }

    fn add_reaction(&mut self, reaction: Reaction) {
        self.message.add_reaction(reaction);
    }

    fn remove_reaction(&mut self, idx: usize) {
        self.message.remove_reaction(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::tests::message;
    use serde_json::json;

    #[test]
    fn votes_count_on_open_polls_only() {
        let mut msg = message(40, 2, "alice", "");
        msg.poll = Some(
            serde_json::from_value(json!({
                "question": {"text": "game tonight?"},
                "answers": [
                    {"answer_id": 1, "poll_media": {"text": "minecraft"}},
                    {"answer_id": 2, "poll_media": {"text": "terraria"}},
                ],
                "expiry": null, "allow_multiselect": false, "layout_type": 1,
            }))
            .unwrap(),
        );
        let cached = CachedMessage::from(msg.clone());
        cached.vote(1, true);
        cached.vote(1, true);
        cached.vote(2, true);
        cached.vote(2, false);
        cached.vote(2, false);
        let counts = |cached: &CachedMessage| -> Vec<(u8, u64)> {
            let results = cached.poll().unwrap().results.unwrap();
            results.answer_counts.iter().map(|count| (count.id, count.count)).collect()
        };
        assert_eq!(counts(&cached), [(1, 2), (2, 0)]);
        assert_eq!(cached.clone(), cached);

        msg.poll.as_mut().unwrap().results = Some(PollResults { answer_counts: Vec::new(), is_finalized: true });
        let closed = CachedMessage::from(msg);
        closed.vote(1, true);
        assert_eq!(counts(&closed), []);
        assert!(CachedMessage::from(message(41, 2, "alice", "hi")).poll().is_none());
    }
}
