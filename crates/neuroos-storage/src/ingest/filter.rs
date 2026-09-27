//! The synchronous 4-stage ingest filter (FR-STO-01, P4-S01, Architecture.md
//! §6.2): every `RawTelemetryEvent` from C1 passes through this before a
//! domain adapter ever touches persistent storage.
//!
//! 1. **PPID collapse** — a `ProcessTreeSnapshot`'s subprocesses (e.g. a
//!    compiler a build tool spawned) are never counted as their own entity;
//!    the whole subtree collapses onto its `root_pid`.
//! 2. **Self-observation exclusion** — neuroos's own windows/processes
//!    never become persistent nodes (design.md §7: `org.neuroos.Confirm`;
//!    generalized here to any `app_id`/`comm` this list names or any
//!    process comm starting with `neuroos-`).
//! 3. **Promotion gate** — an entity key needs N ≥ 3 occurrences or > 5 s
//!    cumulative active dwell before it's worth persisting; below that it's
//!    a transient, in-memory-only counter (phases.md §6.2: "not promoted ->
//!    Transient counters (memory only)"). An occurrence is counted once per
//!    focus session (a window becoming activated), not per raw event.
//! 4. **MPRIS demotion** — media-playback events are never promoted by this
//!    filter. C1 has no signal for "the user actually interacted with this
//!    player" (that comes from C5's `voice_interaction` domain instead, a
//!    different source entirely), so every `media_playback` observation
//!    from C1 stays demoted here, matching Architecture.md §7.3 domain 10's
//!    "demoted unless interacted" from the one signal this stage actually
//!    has.
use std::collections::HashMap;
use std::time::Duration;

use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::window_event::Kind;
use neuroos_proto::v1::{RawTelemetryEvent, ToplevelState, WindowEvent};

/// N >= 3 occurrences promotes (FR-STO-01).
const PROMOTION_OCCURRENCE_THRESHOLD: u32 = 3;
/// > 5s cumulative active dwell promotes (FR-STO-01).
const PROMOTION_DWELL_THRESHOLD: Duration = Duration::from_secs(5);

/// Self-observation exclusion list (stage 2). `org.neuroos.Confirm` is the
/// one named case (design.md §7); every other neuroos binary runs
/// headless, so `comm` names are matched by prefix instead of enumerated.
const SELF_APP_IDS: &[&str] = &["org.neuroos.Confirm"];
const SELF_COMM_PREFIX: &str = "neuroos-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOutcome {
    /// Stage 2: this is neuroos observing itself. Never counted, never
    /// promoted, not even transiently.
    Excluded,
    /// Stage 3 gate not yet met: counted in memory, nothing persisted yet.
    NotYetPromoted { key: String },
    /// Ready for a domain adapter to persist as (or update) an entity.
    Promoted { key: String },
    /// Stage 4: this event type is never promoted by this filter.
    Demoted,
    /// No entity key applies (e.g. a periodic `ResourceSample`, or a raw
    /// event this filter has nothing to do yet, like a window becoming
    /// activated — dwell is only known once it deactivates) — these bypass
    /// the promotion gate; a domain adapter may still aggregate them
    /// directly (they aren't "entities" in the N>=3 sense).
    NotApplicable,
}

#[derive(Default, Clone)]
struct Counter {
    occurrences: u32,
    dwell: Duration,
    promoted: bool,
}

impl Counter {
    fn gate(&self) -> bool {
        self.occurrences >= PROMOTION_OCCURRENCE_THRESHOLD || self.dwell > PROMOTION_DWELL_THRESHOLD
    }
}

/// Per-key transient counters live only in memory (never persisted) until a
/// key crosses the promotion gate — after which it stays promoted for the
/// life of this filter instance (matches phases.md's "not promoted ->
/// Transient counters (memory only)": there is no persisted memory of a
/// key that never got promoted).
#[derive(Default)]
pub struct IngestFilter {
    counters: HashMap<String, Counter>,
    /// Stage 1: subprocess pid -> root pid, refreshed by every
    /// `ProcessTreeSnapshot` (last one wins; a pid not present here is its
    /// own root).
    pid_roots: HashMap<u32, u32>,
    /// toplevel_id -> app_id, tracked from `Opened` to `Closed` so a bare
    /// `StateChanged` (which carries no app_id) can be attributed.
    toplevel_app_ids: HashMap<u64, String>,
    /// toplevel_id -> the UTC-ns instant it last became activated, present
    /// only while currently activated.
    active_since_ns: HashMap<u64, u64>,
}

impl IngestFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Maps a subprocess pid to its collapsed root pid (identity if never
    /// seen in a `ProcessTreeSnapshot`, or is itself a root).
    pub fn collapse_pid(&self, pid: u32) -> u32 {
        self.pid_roots.get(&pid).copied().unwrap_or(pid)
    }

    pub fn process(&mut self, event: &RawTelemetryEvent) -> FilterOutcome {
        match &event.payload {
            Some(Payload::ProcTree(snapshot)) => {
                // Stage 1: record the collapse mapping for later /proc-derived
                // events; the snapshot itself is keyed on its root pid.
                for p in &snapshot.processes {
                    self.pid_roots.insert(p.pid, snapshot.root_pid);
                }
                if !snapshot.root_pid_known {
                    return FilterOutcome::NotApplicable;
                }
                self.touch(format!("proc:{}", snapshot.root_pid), true, Duration::ZERO)
            }
            Some(Payload::Window(w)) => self.process_window(w, event.observed_at_ns),
            Some(Payload::Mpris(_)) => FilterOutcome::Demoted,
            Some(Payload::FileActivity(f)) => {
                self.touch(format!("file:{}", f.watch_label), true, Duration::ZERO)
            }
            Some(Payload::Idle(_)) | Some(Payload::Resource(_)) | None => {
                FilterOutcome::NotApplicable
            }
        }
    }

    fn process_window(&mut self, w: &WindowEvent, observed_at_ns: u64) -> FilterOutcome {
        match &w.kind {
            Some(Kind::Opened(o)) => {
                if is_self_observation(&o.app_id) {
                    return FilterOutcome::Excluded;
                }
                self.toplevel_app_ids
                    .insert(w.toplevel_id, o.app_id.clone());
                FilterOutcome::NotApplicable // opening isn't necessarily focusing
            }
            Some(Kind::AppIdChanged(a)) => {
                if self.toplevel_app_ids.contains_key(&w.toplevel_id) {
                    self.toplevel_app_ids
                        .insert(w.toplevel_id, a.app_id.clone());
                }
                FilterOutcome::NotApplicable
            }
            Some(Kind::StateChanged(s)) => {
                let Some(app_id) = self.toplevel_app_ids.get(&w.toplevel_id).cloned() else {
                    return FilterOutcome::NotApplicable; // unknown or excluded toplevel
                };
                let activated = s.states.contains(&(ToplevelState::Activated as i32));
                if activated {
                    self.active_since_ns.insert(w.toplevel_id, observed_at_ns);
                    // One occurrence per focus session (Architecture.md §6.2's
                    // "N >= 3 occurrences" — 3 separate times focused, not 3
                    // raw wire events).
                    self.touch(app_id, true, Duration::ZERO)
                } else if let Some(started_ns) = self.active_since_ns.remove(&w.toplevel_id) {
                    let dwell = Duration::from_nanos(observed_at_ns.saturating_sub(started_ns));
                    self.touch(app_id, false, dwell)
                } else {
                    FilterOutcome::NotApplicable
                }
            }
            Some(Kind::Closed(_)) => {
                self.toplevel_app_ids.remove(&w.toplevel_id);
                self.active_since_ns.remove(&w.toplevel_id);
                FilterOutcome::NotApplicable
            }
            _ => FilterOutcome::NotApplicable,
        }
    }

    /// Updates (or creates) `key`'s transient counter and returns the
    /// resulting outcome. `add_occurrence` counts one focus/observation
    /// session; `extra_dwell` adds to cumulative active dwell. A key that
    /// already crossed the gate stays `Promoted` on every later touch.
    fn touch(&mut self, key: String, add_occurrence: bool, extra_dwell: Duration) -> FilterOutcome {
        let counter = self.counters.entry(key.clone()).or_default();
        if add_occurrence {
            counter.occurrences += 1;
        }
        counter.dwell += extra_dwell;
        if counter.promoted {
            return FilterOutcome::Promoted { key };
        }
        if counter.gate() {
            counter.promoted = true;
            FilterOutcome::Promoted { key }
        } else {
            FilterOutcome::NotYetPromoted { key }
        }
    }
}

fn is_self_observation(app_id: &str) -> bool {
    SELF_APP_IDS.contains(&app_id) || app_id.starts_with(SELF_COMM_PREFIX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_proto::v1::{
        FileActivityEvent, IdleEvent, MprisEvent, ProcessInfo, ProcessTreeSnapshot,
        WindowAppIdChanged, WindowClosed, WindowOpened, WindowStateChanged,
    };

    fn opened(toplevel_id: u64, app_id: &str) -> RawTelemetryEvent {
        window_event(
            toplevel_id,
            0,
            Kind::Opened(WindowOpened {
                app_id: app_id.to_string(),
                title: String::new(),
                pid: 0,
                pid_known: false,
            }),
        )
    }

    fn activated(toplevel_id: u64, at_ns: u64, active: bool) -> RawTelemetryEvent {
        let states = if active {
            vec![ToplevelState::Activated as i32]
        } else {
            vec![]
        };
        window_event(
            toplevel_id,
            at_ns,
            Kind::StateChanged(WindowStateChanged { states }),
        )
    }

    fn closed(toplevel_id: u64, at_ns: u64) -> RawTelemetryEvent {
        window_event(toplevel_id, at_ns, Kind::Closed(WindowClosed {}))
    }

    fn window_event(toplevel_id: u64, at_ns: u64, kind: Kind) -> RawTelemetryEvent {
        RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "wayland_cosmic".into(),
            payload: Some(Payload::Window(WindowEvent {
                toplevel_id,
                kind: Some(kind),
            })),
        }
    }

    #[test]
    fn open_alone_is_not_applicable_not_a_focus_session() {
        let mut f = IngestFilter::new();
        assert_eq!(
            f.process(&opened(1, "org.mozilla.firefox")),
            FilterOutcome::NotApplicable
        );
    }

    #[test]
    fn self_observation_app_id_is_excluded() {
        let mut f = IngestFilter::new();
        assert_eq!(
            f.process(&opened(1, "org.neuroos.Confirm")),
            FilterOutcome::Excluded
        );
        // and never promotable even after many activations, since it was
        // never registered as a tracked toplevel:
        for i in 0..10 {
            assert_eq!(
                f.process(&activated(1, i * 2_000_000_000, true)),
                FilterOutcome::NotApplicable
            );
            assert_eq!(
                f.process(&activated(1, i * 2_000_000_000 + 1, false)),
                FilterOutcome::NotApplicable
            );
        }
    }

    #[test]
    fn self_observation_matches_any_neuroos_comm_prefixed_app_id() {
        let mut f = IngestFilter::new();
        assert_eq!(
            f.process(&opened(1, "neuroos-confirm")),
            FilterOutcome::Excluded
        );
    }

    #[test]
    fn promotion_gate_by_occurrence_at_exactly_n_equals_3() {
        let mut f = IngestFilter::new();
        f.process(&opened(1, "org.mozilla.firefox"));
        // 2 focus sessions: not yet promoted
        for session in 0..2u64 {
            let base = session * 1_000_000_000;
            assert_eq!(
                f.process(&activated(1, base, true)),
                FilterOutcome::NotYetPromoted {
                    key: "org.mozilla.firefox".into()
                }
            );
            f.process(&activated(1, base + 100_000_000, false)); // 100ms dwell, well under 5s
        }
        // 3rd focus session crosses N>=3 at the moment it activates
        assert_eq!(
            f.process(&activated(1, 3_000_000_000, true)),
            FilterOutcome::Promoted {
                key: "org.mozilla.firefox".into()
            }
        );
    }

    #[test]
    fn promotion_gate_by_dwell_just_over_5s_promotes_just_under_does_not() {
        let mut f = IngestFilter::new();
        f.process(&opened(1, "cosmic-term"));
        f.process(&activated(1, 0, true));
        // 4.9s dwell: not promoted (1 occurrence, dwell under threshold)
        assert_eq!(
            f.process(&activated(1, 4_900_000_000, false)),
            FilterOutcome::NotYetPromoted {
                key: "cosmic-term".into()
            }
        );

        let mut f2 = IngestFilter::new();
        f2.process(&opened(2, "cosmic-term"));
        f2.process(&activated(2, 0, true));
        // 5.1s dwell: promoted on this same first session
        assert_eq!(
            f2.process(&activated(2, 5_100_000_000, false)),
            FilterOutcome::Promoted {
                key: "cosmic-term".into()
            }
        );
    }

    #[test]
    fn once_promoted_a_key_stays_promoted_on_every_later_touch() {
        let mut f = IngestFilter::new();
        f.process(&opened(1, "cosmic-term"));
        f.process(&activated(1, 0, true));
        f.process(&activated(1, 6_000_000_000, false)); // promotes via dwell
        assert_eq!(
            f.process(&activated(1, 10_000_000_000, true)),
            FilterOutcome::Promoted {
                key: "cosmic-term".into()
            }
        );
    }

    #[test]
    fn closed_toplevel_state_changes_are_not_applicable() {
        let mut f = IngestFilter::new();
        f.process(&opened(1, "org.mozilla.firefox"));
        f.process(&closed(1, 0));
        assert_eq!(
            f.process(&activated(1, 1, true)),
            FilterOutcome::NotApplicable
        );
    }

    #[test]
    fn app_id_changed_retargets_the_tracked_toplevel() {
        let mut f = IngestFilter::new();
        f.process(&opened(1, "unknown"));
        f.process(&window_event(
            1,
            0,
            Kind::AppIdChanged(WindowAppIdChanged {
                app_id: "org.mozilla.firefox".into(),
            }),
        ));
        f.process(&activated(1, 0, true));
        assert_eq!(
            f.process(&activated(1, 6_000_000_000, false)),
            FilterOutcome::Promoted {
                key: "org.mozilla.firefox".into()
            }
        );
    }

    #[test]
    fn mpris_events_are_always_demoted() {
        let mut f = IngestFilter::new();
        let event = RawTelemetryEvent {
            observed_at_ns: 0,
            source: "mpris".into(),
            payload: Some(Payload::Mpris(MprisEvent {
                player_bus_name: "org.mpris.MediaPlayer2.vlc".into(),
                playback_status: "Playing".into(),
                track_id: String::new(),
                title: String::new(),
                artist: String::new(),
                album: String::new(),
                position_us: 0,
            })),
        };
        for _ in 0..10 {
            assert_eq!(f.process(&event), FilterOutcome::Demoted);
        }
    }

    #[test]
    fn idle_and_resource_events_are_not_applicable_bypass_the_gate() {
        let mut f = IngestFilter::new();
        assert_eq!(
            f.process(&RawTelemetryEvent {
                observed_at_ns: 0,
                source: "idle".into(),
                payload: Some(Payload::Idle(IdleEvent { idle: true })),
            }),
            FilterOutcome::NotApplicable
        );
    }

    #[test]
    fn process_tree_snapshot_collapses_children_onto_root_pid() {
        let mut f = IngestFilter::new();
        let snapshot = RawTelemetryEvent {
            observed_at_ns: 0,
            source: "proc".into(),
            payload: Some(Payload::ProcTree(ProcessTreeSnapshot {
                root_pid: 100,
                root_pid_known: true,
                processes: vec![
                    ProcessInfo {
                        pid: 100,
                        ppid: 1,
                        comm: "editor".into(),
                    },
                    ProcessInfo {
                        pid: 101,
                        ppid: 100,
                        comm: "cc1".into(),
                    },
                ],
            })),
        };
        f.process(&snapshot);
        assert_eq!(f.collapse_pid(101), 100);
        assert_eq!(f.collapse_pid(100), 100);
        assert_eq!(f.collapse_pid(999), 999); // never seen: identity
    }

    #[test]
    fn unknown_root_pid_snapshot_is_not_applicable() {
        let mut f = IngestFilter::new();
        let snapshot = RawTelemetryEvent {
            observed_at_ns: 0,
            source: "proc".into(),
            payload: Some(Payload::ProcTree(ProcessTreeSnapshot {
                root_pid: 0,
                root_pid_known: false,
                processes: vec![],
            })),
        };
        assert_eq!(f.process(&snapshot), FilterOutcome::NotApplicable);
    }

    #[test]
    fn file_activity_events_go_through_the_promotion_gate_too() {
        let mut f = IngestFilter::new();
        let event = |label: &str| RawTelemetryEvent {
            observed_at_ns: 0,
            source: "folders".into(),
            payload: Some(Payload::FileActivity(FileActivityEvent {
                path: "/x".into(),
                kind: 1,
                watch_label: label.into(),
            })),
        };
        assert_eq!(
            f.process(&event("git")),
            FilterOutcome::NotYetPromoted {
                key: "file:git".into()
            }
        );
        f.process(&event("git"));
        assert_eq!(
            f.process(&event("git")),
            FilterOutcome::Promoted {
                key: "file:git".into()
            }
        );
    }

    // PT (phases.md §7.3): the filter never promotes an excluded app_id,
    // no matter how many times it's activated or how long it dwells.
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn self_observation_is_never_promoted(
                sessions in 1..20u64,
                dwell_ns in 0u64..20_000_000_000,
            ) {
                let mut f = IngestFilter::new();
                let app_id = "org.neuroos.Confirm";
                f.process(&opened(1, app_id));
                for s in 0..sessions {
                    let base = s * (dwell_ns + 1_000_000_000);
                    let outcome_on = f.process(&activated(1, base, true));
                    prop_assert_eq!(&outcome_on, &FilterOutcome::NotApplicable);
                    let outcome_off = f.process(&activated(1, base + dwell_ns, false));
                    prop_assert_eq!(&outcome_off, &FilterOutcome::NotApplicable);
                }
            }
        }
    }
}
