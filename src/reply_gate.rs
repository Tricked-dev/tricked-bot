use std::collections::HashMap;

/// Per-channel count of guild messages since the last unprompted reply.
#[derive(Default)]
pub struct ReplyGate {
    since_reply: HashMap<u64, u32>,
}

impl ReplyGate {
    pub fn count(&mut self, channel: u64) {
        let n = self.since_reply.entry(channel).or_insert(u32::MAX - 1);
        *n = n.saturating_add(1);
    }

    pub fn try_claim(&mut self, channel: u64, cooldown: u32) -> bool {
        let open = self.since_reply.get(&channel).is_some_and(|n| *n >= cooldown);
        if open {
            self.since_reply.insert(channel, 0);
        }
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_claim_is_open_then_cooldown_applies() {
        let mut g = ReplyGate::default();
        g.count(1);
        assert!(g.try_claim(1, 3));
        assert!(!g.try_claim(1, 3)); // a burst: second claim right after is refused
        g.count(1);
        g.count(1);
        assert!(!g.try_claim(1, 3));
        g.count(1);
        assert!(g.try_claim(1, 3));
        g.count(2);
        assert!(g.try_claim(2, 3)); // channels are independent
    }
}
