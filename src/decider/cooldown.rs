use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Debug)]
enum Slot {
    Pending(Instant),
    Posted(Instant),
}

/// Per-channel reservations and delivered interjections of one kind.
#[derive(Debug, Default)]
pub struct Cooldown {
    last: HashMap<u64, Slot>,
}

impl Cooldown {
    /// Whether `channel` has gone at least `period` without a claim.
    pub fn is_open(&self, channel: u64, period: Duration, now: Instant) -> bool {
        match self.last.get(&channel) {
            None => true,
            Some(Slot::Pending(_)) => false,
            Some(Slot::Posted(last)) => now.saturating_duration_since(*last) >= period,
        }
    }

    /// Starts a new cooldown in `channel` when the last one is over. Returns whether it did.
    pub fn try_claim(&mut self, channel: u64, period: Duration, now: Instant) -> bool {
        let open = self.is_open(channel, period, now);
        if open {
            self.last.insert(channel, Slot::Pending(now));
        }
        open
    }
    /// Finishes only the matching reservation. A skipped/failed post spends no cooldown; a successful one
    /// starts its period at delivery. An old completion cannot clear a newer reservation.
    pub fn finish(&mut self, channel: u64, claimed_at: Instant, posted: bool, now: Instant) {
        if !matches!(self.last.get(&channel), Some(Slot::Pending(at)) if *at == claimed_at) {
            return;
        }
        if posted {
            self.last.insert(channel, Slot::Posted(now));
        } else {
            self.last.remove(&channel);
        }
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
        cooldown.finish(1, start, true, start);
        assert!(!cooldown.is_open(1, period, start + Duration::from_secs(59)));
        assert!(!cooldown.try_claim(1, period, start + Duration::from_secs(59)));
        assert!(cooldown.try_claim(2, period, start), "channels are independent");
        assert!(cooldown.try_claim(1, period, start + period));
        assert!(!cooldown.try_claim(1, period, start + period), "a claim restarts the period");
    }
    #[test]
    fn completion_starts_cooldown_at_delivery_and_cannot_release_a_newer_claim() {
        let (mut cooldown, period, start) = (Cooldown::default(), Duration::from_secs(60), Instant::now());
        assert!(cooldown.try_claim(1, period, start));
        cooldown.finish(1, start, true, start + Duration::from_secs(10));
        assert!(!cooldown.is_open(1, period, start + period), "count from delivery, not drafting");
        let later = start + Duration::from_secs(70);
        assert!(cooldown.try_claim(1, period, later));
        cooldown.finish(1, start, false, later);
        assert!(!cooldown.is_open(1, period, later), "an older failure cannot release the new claim");
        cooldown.finish(1, later, false, later);
        assert!(cooldown.is_open(1, period, later));
    }
    #[test]
    fn in_flight_drafts_do_not_expire_when_cooldown_is_zero() {
        let (mut cooldown, start) = (Cooldown::default(), Instant::now());
        assert!(cooldown.try_claim(1, Duration::ZERO, start));
        assert!(!cooldown.try_claim(1, Duration::ZERO, start + Duration::from_secs(1)), "one draft at a time");
        cooldown.finish(1, start, false, start + Duration::from_secs(1));
        assert!(cooldown.try_claim(1, Duration::ZERO, start + Duration::from_secs(1)));
    }
}
