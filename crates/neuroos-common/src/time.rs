//! UTC epoch nanoseconds, matching `Envelope.sent_at_ns` (Architecture.md §5.3).
use jiff::Timestamp;

/// Current UTC time as epoch nanoseconds (`rules.md` §4: `jiff`, not
/// `std::time::SystemTime` arithmetic).
pub fn now_ns() -> u64 {
    let ts = Timestamp::now();
    // as_nanosecond() is i128 (jiff's range spans well before/after the u64
    // epoch-ns range); any real "now" fits in u64 until the year 2554, and a
    // clock that isn't is a system fault this code can't meaningfully handle.
    ts.as_nanosecond().clamp(0, u64::MAX as i128) as u64
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn now_ns_is_a_recent_2026_timestamp() {
        let ns = now_ns();
        // 2026-01-01T00:00:00Z in epoch ns, as a sanity floor.
        let year_2026_ns: u64 = 1_767_225_600 * 1_000_000_000;
        assert!(ns > year_2026_ns, "now_ns() = {ns} looks wrong (before 2026)");
    }

    #[test]
    fn now_ns_increases_monotonically_enough() {
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
    }
}
