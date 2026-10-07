//! Shared synchronisation helpers.

use std::sync::{Mutex, MutexGuard};

/// Locks `m`, recovering rather than panicking if a prior holder panicked
/// while holding it.
///
/// rules.md §5 forbids `unwrap()`/`expect()`/`panic!` in non-test code, and
/// `Mutex::lock`'s only error is poisoning. Every caller in this workspace
/// guards state with no invariant a partial update could leave meaningfully
/// broken — histogram counters, fragment-insert tallies, a distillation
/// cache, per-sensor health — so taking the poisoned guard is the right
/// recovery, and it is strictly better than propagating a panic into an
/// unrelated request.
///
/// C2: this was copy-pasted into five modules across four crates
/// (`neuroos-health` twice, `neuroos-monitor`, `neuroos-storage`,
/// `neuroos-knowledge-query`). If a caller ever does guard an invariant that
/// a panic could break, it must handle `PoisonError` itself rather than reach
/// for this.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use std::sync::Arc;

    #[test]
    fn recovers_the_guard_after_a_holder_panicked() {
        let m = Arc::new(Mutex::new(41));
        let poisoner = Arc::clone(&m);
        // Really poison it: take the lock, then panic while holding it.
        let joined = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poisoning on purpose");
        })
        .join();
        assert!(joined.is_err(), "the thread must have panicked");
        assert!(m.is_poisoned(), "the mutex must be poisoned");

        // The plain `lock()` would error here; ours returns the value.
        assert!(m.lock().is_err());
        *lock(&m) += 1;
        assert_eq!(*lock(&m), 42);
    }
}
