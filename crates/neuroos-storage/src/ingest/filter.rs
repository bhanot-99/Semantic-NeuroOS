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

/// Strips leading status glyphs from a window title: terminals animate a
/// spinner (`◐`/`◑`/`✳`) and editors prefix unsaved files with `*`, which
/// otherwise turn one window into several near-duplicate chunks (BUG-007).
/// Keeps leading path and bracket characters, which carry meaning.
pub fn normalize_title(title: &str) -> &str {
    title
        .trim_start_matches(|c: char| !(c.is_alphanumeric() || "~/.([\"'#@".contains(c)))
        .trim_end()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOutcome {
    /// Stage 2: this is neuroos observing itself. Never counted, never
    /// promoted, not even transiently.
    Excluded,
    /// Stage 3 gate not yet met: counted in memory, nothing persisted yet.
    NotYetPromoted { key: String },
    /// Ready for a domain adapter to persist as (or update) an entity.
    /// `segment` is `Some((t_start_ns, t_end_ns))` when this touch was a
    /// window dwell segment ending (a `StateChanged` deactivate) — the one
    /// case a domain adapter needs more than `key` and the triggering
    /// event's own timestamp to write `focus_history`.
    Promoted {
        key: String,
        segment: Option<(u64, u64)>,
    },
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
    /// toplevel_id -> last-known title, tracked the same way (`Opened` /
    /// `TitleChanged` to `Closed`) so a completed dwell segment can be
    /// labeled with whatever was on screen, not just the app_id.
    toplevel_titles: HashMap<u64, String>,
    /// toplevel_id -> the UTC-ns instant its current focus segment began,
    /// present only while currently activated.
    active_since_ns: HashMap<u64, u64>,
    /// The title on screen during the segment the last `Promoted` /
    /// `NotYetPromoted` outcome closed, read by the window adapter right
    /// after `process` (the triggering event may already carry the *next*
    /// title).
    last_segment_title: Option<String>,
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

    /// The last-known title for a still-open toplevel (`None` if it was
    /// never opened, was self-observation-excluded, or has since closed).
    /// A domain adapter reads this after a dwell segment completes, since
    /// that event (`StateChanged`) carries no title of its own.
    pub fn title_for(&self, toplevel_id: u64) -> Option<&str> {
        self.toplevel_titles.get(&toplevel_id).map(String::as_str)
    }

    /// The title shown during the dwell segment the most recent `process`
    /// call closed.
    pub fn last_segment_title(&self) -> Option<&str> {
        self.last_segment_title.as_deref()
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
                self.touch(
                    format!("proc:{}", snapshot.root_pid),
                    true,
                    Duration::ZERO,
                    None,
                )
            }
            Some(Payload::Window(w)) => self.process_window(w, event.observed_at_ns),
            Some(Payload::Mpris(_)) => FilterOutcome::Demoted,
            Some(Payload::FileActivity(f)) => self.touch(
                format!("file:{}", f.watch_label),
                true,
                Duration::ZERO,
                None,
            ),
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
                self.toplevel_titles.insert(w.toplevel_id, o.title.clone());
                FilterOutcome::NotApplicable // opening isn't necessarily focusing
            }
            Some(Kind::AppIdChanged(a)) => {
                if self.toplevel_app_ids.contains_key(&w.toplevel_id) {
                    self.toplevel_app_ids
                        .insert(w.toplevel_id, a.app_id.clone());
                }
                FilterOutcome::NotApplicable
            }
            Some(Kind::TitleChanged(t)) => {
                let Some(app_id) = self.toplevel_app_ids.get(&w.toplevel_id).cloned() else {
                    return FilterOutcome::NotApplicable;
                };
                let old = self
                    .toplevel_titles
                    .insert(w.toplevel_id, t.title.clone())
                    .unwrap_or_default();
                // BUG-007: while focused, a real title change (a new page in
                // a browser tab, not a spinner glyph) ends the old title's
                // dwell segment and starts the new one's.
                let Some(started_ns) = self.active_since_ns.get(&w.toplevel_id).copied() else {
                    return FilterOutcome::NotApplicable;
                };
                if normalize_title(&old) == normalize_title(&t.title) {
                    return FilterOutcome::NotApplicable;
                }
                self.active_since_ns.insert(w.toplevel_id, observed_at_ns);
                self.last_segment_title = Some(old);
                let dwell = Duration::from_nanos(observed_at_ns.saturating_sub(started_ns));
                self.touch(app_id, false, dwell, Some((started_ns, observed_at_ns)))
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
                    self.touch(app_id, true, Duration::ZERO, None)
                } else if let Some(started_ns) = self.active_since_ns.remove(&w.toplevel_id) {
                    self.last_segment_title = self.toplevel_titles.get(&w.toplevel_id).cloned();
                    let dwell = Duration::from_nanos(observed_at_ns.saturating_sub(started_ns));
                    self.touch(app_id, false, dwell, Some((started_ns, observed_at_ns)))
                } else {
                    FilterOutcome::NotApplicable
                }
            }
            Some(Kind::Closed(_)) => {
                let app_id = self.toplevel_app_ids.remove(&w.toplevel_id);
                let title = self.toplevel_titles.remove(&w.toplevel_id);
                let started = self.active_since_ns.remove(&w.toplevel_id);
                // BUG-007: most windows are closed while still focused,
                // without a deactivate first; that close ends the segment.
                match (app_id, started) {
                    (Some(app_id), Some(started_ns)) => {
                        self.last_segment_title = title;
                        let dwell = Duration::from_nanos(observed_at_ns.saturating_sub(started_ns));
                        self.touch(app_id, false, dwell, Some((started_ns, observed_at_ns)))
                    }
                    _ => FilterOutcome::NotApplicable,
                }
            }
            _ => FilterOutcome::NotApplicable,
        }
    }

    /// Updates (or creates) `key`'s transient counter and returns the
    /// resulting outcome. `add_occurrence` counts one focus/observation
    /// session; `extra_dwell` adds to cumulative active dwell. A key that
    /// already crossed the gate stays `Promoted` on every later touch.
    fn touch(
        &mut self,
        key: String,
        add_occurrence: bool,
        extra_dwell: Duration,
        segment: Option<(u64, u64)>,
    ) -> FilterOutcome {
        let counter = self.counters.entry(key.clone()).or_default();
        if add_occurrence {
            counter.occurrences += 1;
        }
        counter.dwell += extra_dwell;
        if counter.promoted {
            return FilterOutcome::Promoted { key, segment };
        }
        if counter.gate() {
            counter.promoted = true;
            FilterOutcome::Promoted { key, segment }
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

    fn retitled(toplevel_id: u64, at_ns: u64, title: &str) -> RawTelemetryEvent {
        window_event(
            toplevel_id,
            at_ns,
            Kind::TitleChanged(neuroos_proto::v1::WindowTitleChanged {
                title: title.to_string(),
            }),
        )
    }

    /// BUG-007: a browser tab navigated within one focus session used to
    /// leave only its final page title; every page in between was lost.
    #[test]
    fn a_title_change_while_focused_closes_the_previous_titles_segment() {
        const S: u64 = 1_000_000_000;
        let mut f = IngestFilter::new();
        f.process(&opened(1, "brave-browser"));
        f.process(&activated(1, 0, true));
        f.process(&retitled(1, S, "linux jarvis theme packs - Google Search"));
        // a spinner glyph is the same page, not a new segment
        assert_eq!(
            f.process(&retitled(
                1,
                2 * S,
                "◐ linux jarvis theme packs - Google Search"
            )),
            FilterOutcome::NotApplicable
        );
        assert_eq!(
            f.process(&retitled(
                1,
                7 * S,
                "J.A.R.V.I.S Animated Theme : r/omarchy"
            )),
            FilterOutcome::Promoted {
                key: "brave-browser".into(),
                segment: Some((S, 7 * S)),
            }
        );
        assert_eq!(
            f.last_segment_title(),
            Some("◐ linux jarvis theme packs - Google Search")
        );
        assert_eq!(
            f.process(&activated(1, 10 * S, false)),
            FilterOutcome::Promoted {
                key: "brave-browser".into(),
                segment: Some((7 * S, 10 * S)),
            }
        );
        assert_eq!(
            f.last_segment_title(),
            Some("J.A.R.V.I.S Animated Theme : r/omarchy")
        );
    }

    /// BUG-007: 45 of 62 window closes in the KPI-1 recording happened
    /// while the window was still focused (no deactivate first), and every
    /// one of those final focus segments was silently dropped.
    #[test]
    fn closing_a_focused_window_ends_its_focus_segment() {
        const S: u64 = 1_000_000_000;
        let mut f = IngestFilter::new();
        f.process(&opened(1, "gedit"));
        f.process(&retitled(1, 0, "MASTER_PLAN.md (~/Project Custom) - gedit"));
        f.process(&activated(1, 0, true));
        assert_eq!(
            f.process(&closed(1, 6 * S)),
            FilterOutcome::Promoted {
                key: "gedit".into(),
                segment: Some((0, 6 * S)),
            }
        );
        assert_eq!(
            f.last_segment_title(),
            Some("MASTER_PLAN.md (~/Project Custom) - gedit")
        );
        assert_eq!(f.title_for(1), None);
    }

    #[test]
    fn a_title_change_while_unfocused_only_updates_the_title() {
        let mut f = IngestFilter::new();
        f.process(&opened(1, "brave-browser"));
        assert_eq!(
            f.process(&retitled(1, 5, "Background tab")),
            FilterOutcome::NotApplicable
        );
        assert_eq!(f.title_for(1), Some("Background tab"));
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
                key: "org.mozilla.firefox".into(),
                segment: None, // this touch is an activate, not a completed dwell segment
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
                key: "cosmic-term".into(),
                segment: Some((0, 5_100_000_000)),
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
                key: "cosmic-term".into(),
                segment: None, // an activate, not a completed dwell segment
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
                key: "org.mozilla.firefox".into(),
                segment: Some((0, 6_000_000_000)),
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
                key: "file:git".into(),
                segment: None,
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
