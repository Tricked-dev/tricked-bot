//! Timings for the work before and during a reply, without logging prompt contents.

use std::{
    future::Future,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use twilight_model::id::{Id, marker::MessageMarker};

pub async fn measure<T>(message_id: Id<MessageMarker>, stage: &'static str, work: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let result = work.await;
    record(message_id, stage, started);
    result
}

pub fn record(message_id: Id<MessageMarker>, stage: &'static str, started: Instant) {
    // Discord IDs encode the creation time in milliseconds above their low 22 bits.
    let source_ms = u128::from((message_id.get() >> 22) + 1_420_070_400_000);
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    tracing::info!(target: "ai_timing", msg_id = %message_id, stage,
        latency_ms = started.elapsed().as_millis(), source_age_ms = now_ms.saturating_sub(source_ms));
}
