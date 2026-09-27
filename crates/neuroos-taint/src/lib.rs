//! `TaintFlags` + propagation helpers (Architecture.md §7.4). Shared by
//! every component that reads or writes tainted data (C3 storage, C5
//! prompt assembly, C6 tier evaluation) — one definition, not per-crate
//! copies, so the bit layout can never drift between them.
use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct TaintFlags: u32 {
        /// Came from C7 (web fetch spool).
        const EXTERNAL_UNTRUSTED = 1 << 0;
        /// Typed/spoken by the user (informational only).
        const USER_PROVIDED = 1 << 1;
        /// Produced by C4 (the LLM).
        const MODEL_GENERATED = 1 << 2;
        /// Embedded with an embedding model id that's since changed.
        const STALE_INDEX = 1 << 3;
    }
}

impl TaintFlags {
    /// Architecture.md §7.4: `taint(output) = union(taint(inputs))`. Taint
    /// is never cleared (rules.md R0-3) — this is the only way flags
    /// combine; there is deliberately no inverse "subtract" operation.
    pub fn propagate(inputs: impl IntoIterator<Item = TaintFlags>) -> TaintFlags {
        inputs
            .into_iter()
            .fold(TaintFlags::empty(), |acc, t| acc | t)
    }

    /// §8.3's tier-evaluation rule: only `EXTERNAL_UNTRUSTED` changes tier
    /// decisions in v1.0.
    pub fn escalates_tier(self) -> bool {
        self.contains(TaintFlags::EXTERNAL_UNTRUSTED)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn propagate_unions_every_input() {
        let t = TaintFlags::propagate([TaintFlags::USER_PROVIDED, TaintFlags::MODEL_GENERATED]);
        assert!(t.contains(TaintFlags::USER_PROVIDED));
        assert!(t.contains(TaintFlags::MODEL_GENERATED));
        assert!(!t.contains(TaintFlags::EXTERNAL_UNTRUSTED));
    }

    #[test]
    fn propagate_of_nothing_is_empty() {
        assert_eq!(TaintFlags::propagate([]), TaintFlags::empty());
    }

    #[test]
    fn only_external_untrusted_escalates_tier() {
        assert!(TaintFlags::EXTERNAL_UNTRUSTED.escalates_tier());
        assert!(!TaintFlags::USER_PROVIDED.escalates_tier());
        assert!(!TaintFlags::MODEL_GENERATED.escalates_tier());
        assert!(!TaintFlags::STALE_INDEX.escalates_tier());
        let mixed = TaintFlags::USER_PROVIDED | TaintFlags::EXTERNAL_UNTRUSTED;
        assert!(mixed.escalates_tier());
    }

    #[test]
    fn taint_is_never_lowered_by_union_with_empty() {
        // R0-3: no code path may remove a flag; unioning with empty must be
        // a no-op, never a reset.
        let t = TaintFlags::EXTERNAL_UNTRUSTED;
        assert_eq!(t | TaintFlags::empty(), t);
    }
}
