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

/// `ns` (UTC epoch nanoseconds) as wall-clock `HH:MM` in `tz`. Used to
/// put times on evidence shown to the model ("at 22:15"), so it can answer
/// "last"/"before" questions about the user's own day.
pub fn hhmm_in(ns: u64, tz: &jiff::tz::TimeZone) -> String {
    let ts = Timestamp::from_nanosecond(ns as i128).unwrap_or(Timestamp::UNIX_EPOCH);
    ts.to_zoned(tz.clone()).strftime("%H:%M").to_string()
}

/// `ns` as a sortable UTC label, `YYYYMMDDTHHMMSSZ` (backup directory
/// names, Architecture.md §7.1's `backups/<UTC timestamp>/`).
pub fn utc_label(ns: u64) -> String {
    let ts = Timestamp::from_nanosecond(ns as i128).unwrap_or(Timestamp::UNIX_EPOCH);
    ts.strftime("%Y%m%dT%H%M%SZ").to_string()
}

/// [`hhmm_in`] the system's local time zone (UTC if it can't be found).
pub fn local_hhmm(ns: u64) -> String {
    hhmm_in(ns, &jiff::tz::TimeZone::system())
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
        assert!(
            ns > year_2026_ns,
            "now_ns() = {ns} looks wrong (before 2026)"
        );
    }

    #[test]
    fn now_ns_increases_monotonically_enough() {
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
    }

    #[test]
    fn utc_label_is_sortable_utc() {
        // 2026-09-29T16:48:08Z
        assert_eq!(
            utc_label(1_790_700_488u64 * 1_000_000_000),
            "20260929T164808Z"
        );
    }

    #[test]
    fn hhmm_formats_in_the_given_zone() {
        // 2026-09-29T16:48:08Z
        let ns = 1_790_700_488u64 * 1_000_000_000;
        assert_eq!(hhmm_in(ns, &jiff::tz::TimeZone::UTC), "16:48");
        let ist = jiff::tz::TimeZone::get("Asia/Kolkata").unwrap();
        assert_eq!(hhmm_in(ns, &ist), "22:18");
    }
}
