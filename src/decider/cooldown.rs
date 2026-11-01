use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Per-channel time of the last unprompted interjection of one kind.
#[derive(Debug, Default)]
pub struct Cooldown {
    last: HashMap<u64, Instant>,
}

impl Cooldown {
    /// Whether `channel` has gone at least `period` without a claim.
    pub fn is_open(&self, channel: u64, period: Duration, now: Instant) -> bool {
        self.last.get(&channel).is_none_or(|last| now.saturating_duration_since(*last) >= period)
    }

    /// Starts a new cooldown in `channel` when the last one is over. Returns whether it did.
    pub fn try_claim(&mut self, channel: u64, period: Duration, now: Instant) -> bool {
        let open = self.is_open(channel, period, now);
        if open {
            if self.last.len() > 4096 {
                self.last.retain(|_, last| now.saturating_duration_since(*last) < period);
            }
            self.last.insert(channel, now);
        }
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_reopen_after_the_period_per_channel() {
        let (mut cooldown, period, start) = (Cooldown::default(), Duration::from_secs(60), Instant::now());
        assert!(cooldown.is_open(1, period, start));
        assert!(cooldown.try_claim(1, period, start));
        assert!(!cooldown.is_open(1, period, start + Duration::from_secs(59)));
        assert!(!cooldown.try_claim(1, period, start + Duration::from_secs(59)));
        assert!(cooldown.try_claim(2, period, start), "channels are independent");
        assert!(cooldown.try_claim(1, period, start + period));
        assert!(!cooldown.try_claim(1, period, start + period), "a claim restarts the period");
    }
}
