//! Wall-clock time.

use std::time::{Duration, SystemTime};

/// Time since the Unix epoch, or zero for a clock set before it.
pub fn unix_now() -> Duration {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default()
}
