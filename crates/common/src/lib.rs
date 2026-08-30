/// Converts an absolute Unix-millisecond timestamp into a monotonic `Instant` this process's
/// clock can compare against. A target already in the past collapses to "right now" (the
/// delta saturates to zero), so an already-elapsed expiry takes effect on the very next
/// passive-expiry check rather than needing special-casing.
pub fn instant_from_unix_ms(target_unix_ms: i64) -> std::time::Instant {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let target = UNIX_EPOCH + Duration::from_millis(target_unix_ms.max(0) as u64);
    let delta = target
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    std::time::Instant::now() + delta
}

/// The inverse of `instant_from_unix_ms`: converts a monotonic `Instant` (which has no defined
/// relationship to wall-clock time on its own) into an absolute Unix-millisecond timestamp, by
/// measuring `at`'s offset from *now* on both clocks and applying that offset to the wall clock.
/// Uses `saturating_duration_since` on both sides (never panics, unlike plain `Instant` subtraction
/// on some historical Rust versions) so a caller never needs to reason about which `Instant` is
/// later.
pub fn unix_ms_from_instant(at: std::time::Instant) -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_instant = std::time::Instant::now();
    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis() as i64;
    if at >= now_instant {
        now_unix_ms + at.saturating_duration_since(now_instant).as_millis() as i64
    } else {
        now_unix_ms - now_instant.saturating_duration_since(at).as_millis() as i64
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum EngineError {
    #[error("WRONGTYPE Operation against a key holding the wrong kind of value")]
    WrongType,
    #[error("value is not an integer or out of range")]
    NotAnInteger,
    #[error("no such key")]
    NoSuchKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_such_key_has_the_expected_display_text() {
        assert_eq!(EngineError::NoSuchKey.to_string(), "no such key");
    }

    #[test]
    fn instant_from_unix_ms_of_a_future_timestamp_is_in_the_future() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let future_ms = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            + 60_000;
        assert!(instant_from_unix_ms(future_ms) > std::time::Instant::now());
    }

    #[test]
    fn unix_ms_from_instant_round_trips_through_instant_from_unix_ms() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let target_ms = now_ms + 5_000;
        let at = instant_from_unix_ms(target_ms);
        let round_tripped = unix_ms_from_instant(at);
        // millisecond rounding through two clock reads on each side can drift a few ms
        assert!(
            (round_tripped - target_ms).abs() < 50,
            "round-tripped to {round_tripped}, expected near {target_ms}"
        );
    }

    #[test]
    fn unix_ms_from_instant_of_a_past_instant_is_less_than_now() {
        let past = std::time::Instant::now() - std::time::Duration::from_secs(10);
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(unix_ms_from_instant(past) < now_ms);
    }
}
