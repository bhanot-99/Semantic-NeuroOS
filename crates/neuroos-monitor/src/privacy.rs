//! Privacy layer (rules.md AB-5, PRD FR-MON-08/FR-PRV-01/FR-PRV-02): the
//! *only* filtering C1 is allowed to do. Pause state and the app exclusion
//! list are checked here, before an event ever reaches the bus.
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Shared, cheaply-cloned handle. `0` in `paused_until_ns` means "not paused".
#[derive(Clone)]
pub struct PrivacyState {
    paused_until_ns: Arc<AtomicU64>,
    excluded_app_ids: Arc<HashSet<String>>,
}

impl PrivacyState {
    pub fn new(excluded_app_ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            paused_until_ns: Arc::new(AtomicU64::new(0)),
            excluded_app_ids: Arc::new(excluded_app_ids.into_iter().collect()),
        }
    }

    /// FR-PRV-01: pause for `duration_s` seconds, or indefinitely if `None`
    /// (represented internally as `u64::MAX`, resumed only by an explicit
    /// `resume()`).
    pub fn pause(&self, duration_s: Option<u64>, now_ns: u64) {
        let until = match duration_s {
            Some(secs) => now_ns.saturating_add(secs.saturating_mul(1_000_000_000)),
            None => u64::MAX,
        };
        self.paused_until_ns.store(until, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused_until_ns.store(0, Ordering::SeqCst);
    }

    pub fn is_paused(&self, now_ns: u64) -> bool {
        let until = self.paused_until_ns.load(Ordering::SeqCst);
        until != 0 && (until == u64::MAX || now_ns < until)
    }

    /// `0` when not paused, `u64::MAX` when paused indefinitely, otherwise
    /// the UTC-ns instant the pause ends.
    pub fn paused_until_ns(&self) -> u64 {
        self.paused_until_ns.load(Ordering::SeqCst)
    }

    pub fn is_excluded(&self, app_id: &str) -> bool {
        self.excluded_app_ids.contains(app_id)
    }

    /// The single gate every sensor event passes through before publication
    /// (FR-MON-08): excluded apps and paused periods never leave C1.
    pub fn allows(&self, app_id: Option<&str>, now_ns: u64) -> bool {
        if self.is_paused(now_ns) {
            return false;
        }
        match app_id {
            Some(id) => !self.is_excluded(id),
            None => true,
        }
    }
}

pub fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn not_paused_by_default() {
        let p = PrivacyState::new(Vec::new());
        assert!(!p.is_paused(1_000));
        assert!(p.allows(Some("org.mozilla.firefox"), 1_000));
    }

    #[test]
    fn pause_with_duration_expires() {
        let p = PrivacyState::new(Vec::new());
        p.pause(Some(10), 1_000_000_000);
        assert!(p.is_paused(1_000_000_000 + 5_000_000_000));
        assert!(!p.is_paused(1_000_000_000 + 11_000_000_000));
    }

    #[test]
    fn pause_indefinite_until_resumed() {
        let p = PrivacyState::new(Vec::new());
        p.pause(None, 0);
        assert!(p.is_paused(u64::MAX - 1));
        p.resume();
        assert!(!p.is_paused(u64::MAX - 1));
    }

    #[test]
    fn excluded_app_never_allowed_even_when_not_paused() {
        let p = PrivacyState::new(["org.keepassxc.KeePassXC".to_string()]);
        assert!(!p.allows(Some("org.keepassxc.KeePassXC"), 0));
        assert!(p.allows(Some("org.mozilla.firefox"), 0));
    }

    #[test]
    fn events_with_no_app_id_are_still_paused_by_pause_state() {
        let p = PrivacyState::new(Vec::new());
        p.pause(None, 0);
        assert!(!p.allows(None, 0));
    }

    // SC (property test): no random sequence of pause/resume/exclusion-list
    // membership ever lets an excluded app_id through (phases.md §6.3 SC row).
    #[cfg(test)]
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn excluded_app_id_is_never_allowed(
                excluded in proptest::collection::hash_set("[a-z]{1,8}", 1..5),
                candidate in "[a-z]{1,8}",
                paused in any::<bool>(),
            ) {
                let p = PrivacyState::new(excluded.iter().cloned());
                if paused {
                    p.pause(None, 0);
                }
                if excluded.contains(&candidate) {
                    prop_assert!(!p.allows(Some(&candidate), 0));
                }
            }
        }
    }
}
