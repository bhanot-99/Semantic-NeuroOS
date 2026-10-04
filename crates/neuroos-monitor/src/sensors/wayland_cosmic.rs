//! COSMIC compositor sensor: `zcosmic_toplevel_info_v1` (spike S-03, P0-S08;
//! push-event wiring P3-S01).
//!
//! Proven live against the reference machine's real COSMIC session
//! (docs/adr/0006-cosmic-toplevel-and-bitnet-spikes.md): lists every
//! toplevel window with its `app_id`, title and activation state.
//! [`run_forever`] is the real push-event subscription `monitor.sock`
//! streams from; [`list_toplevels`] remains as the original one-shot spike
//! proof and this module's non-live unit-test surface.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1,
};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};

use crate::bus::EventBus;
use crate::privacy::PrivacyState;
use crate::sensor_health::{SensorHealth, supervise};
use crate::sensors::proc;
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::{
    RawTelemetryEvent, ToplevelState as ProtoToplevelState, WindowAppIdChanged, WindowClosed,
    WindowEvent, WindowOpened, WindowStateChanged, WindowTitleChanged, window_event::Kind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToplevelState {
    Maximized,
    Minimized,
    Activated,
    Fullscreen,
}

impl TryFrom<u32> for ToplevelState {
    type Error = ();
    fn try_from(val: u32) -> Result<Self, ()> {
        match val {
            0 => Ok(Self::Maximized),
            1 => Ok(Self::Minimized),
            2 => Ok(Self::Activated),
            3 => Ok(Self::Fullscreen),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToplevelInfo {
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub states: Vec<ToplevelState>,
}

impl ToplevelInfo {
    pub fn is_activated(&self) -> bool {
        self.states.contains(&ToplevelState::Activated)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WaylandError {
    #[error("could not connect to the Wayland compositor: {0}")]
    Connect(#[from] wayland_client::ConnectError),
    #[error("Wayland dispatch/roundtrip failed: {0}")]
    Dispatch(#[from] wayland_client::DispatchError),
    #[error("compositor does not support zcosmic_toplevel_info_v1")]
    ProtocolUnsupported,
}

/// Connects to the compositor named by `$WAYLAND_DISPLAY`, binds
/// `zcosmic_toplevel_info_v1`, and returns every toplevel window currently
/// known to it. One-shot: opens a fresh connection each call. A push-event
/// version (subscribing to `Toplevel`/`Closed`/`State` events as they
/// happen, rather than snapshotting) is Phase 3 work.
pub fn list_toplevels() -> Result<Vec<ToplevelInfo>, WaylandError> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let mut state = AppState::default();
    // Three round-trips: (1) registry globals, (2) toplevel_info binds and
    // announces existing toplevels, (3) each toplevel's title/app_id/state
    // events, which the compositor sends right after announcing it.
    event_queue.roundtrip(&mut state)?;
    event_queue.roundtrip(&mut state)?;
    event_queue.roundtrip(&mut state)?;

    if state.toplevel_info.is_none() {
        return Err(WaylandError::ProtocolUnsupported);
    }
    Ok(state.toplevels.into_iter().map(|t| t.info).collect())
}

#[derive(Default)]
struct AppState {
    toplevel_info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    toplevels: Vec<TrackedToplevel>,
}

struct TrackedToplevel {
    handle: zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    info: ToplevelInfo,
}

impl Dispatch<wl_registry::WlRegistry, ()> for AppState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "zcosmic_toplevel_info_v1"
        {
            state.toplevel_info = Some(registry.bind(name, 1, qh, ()));
        }
    }
}

impl Dispatch<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, ()> for AppState {
    fn event(
        state: &mut Self,
        _info: &zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        event: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_toplevel_info_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(TrackedToplevel {
                handle: toplevel,
                info: ToplevelInfo::default(),
            });
        }
    }

    event_created_child!(
        AppState,
        zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        [
            zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()),
        ]
    );
}

impl Dispatch<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()> for AppState {
    fn event(
        state: &mut Self,
        toplevel: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(tracked) = state.toplevels.iter_mut().find(|t| &t.handle == toplevel) else {
            return;
        };
        match event {
            zcosmic_toplevel_handle_v1::Event::Title { title } => tracked.info.title = Some(title),
            zcosmic_toplevel_handle_v1::Event::AppId { app_id } => {
                tracked.info.app_id = Some(app_id)
            }
            zcosmic_toplevel_handle_v1::Event::State { state: raw } => {
                tracked.info.states = raw
                    .chunks_exact(4)
                    .filter_map(|c| <[u8; 4]>::try_from(c).ok())
                    .map(u32::from_ne_bytes)
                    .filter_map(|v| ToplevelState::try_from(v).ok())
                    .collect();
            }
            _ => {}
        }
    }
}

impl ToplevelState {
    fn to_proto(self) -> ProtoToplevelState {
        match self {
            Self::Maximized => ProtoToplevelState::Maximized,
            Self::Minimized => ProtoToplevelState::Minimized,
            Self::Activated => ProtoToplevelState::Activated,
            Self::Fullscreen => ProtoToplevelState::Fullscreen,
        }
    }
}

/// Runs the push-event toplevel sensor forever, reconnecting with backoff on
/// any error (AB-10; FI: "compositor restart").
pub async fn run_forever(bus: EventBus, privacy: PrivacyState, health: SensorHealth) {
    // M5: one id source for the whole process, so reconnects continue the
    // sequence instead of restarting it.
    let ids = ToplevelIdSource::new();
    supervise("wayland_cosmic", health, move || {
        let bus = bus.clone();
        let privacy = privacy.clone();
        let ids = ids.clone();
        async move {
            match tokio::task::spawn_blocking(move || run_once(&bus, &privacy, &ids)).await {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(e) => Err(format!("sensor thread panicked: {e}")),
            }
        }
    })
    .await;
}

fn run_once(
    bus: &EventBus,
    privacy: &PrivacyState,
    ids: &ToplevelIdSource,
) -> Result<(), WaylandError> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let mut state = PushState {
        bus: bus.clone(),
        privacy: privacy.clone(),
        ids: ids.clone(),
        ..Default::default()
    };
    event_queue.roundtrip(&mut state)?;
    if state.toplevel_info.is_none() {
        return Err(WaylandError::ProtocolUnsupported);
    }

    let outcome = loop {
        if let Err(e) = event_queue.blocking_dispatch(&mut state) {
            break Err(WaylandError::from(e));
        }
        if state.manager_finished {
            break Err(WaylandError::ProtocolUnsupported);
        }
    };
    // M5: the connection is going away and the compositor will never send
    // `Closed` for the windows that were still open on it, so C3 would
    // keep them open forever (and keep counting dwell). Close them out
    // here; the ids are never reused, so these are unambiguous.
    state.close_open_toplevels();
    outcome
}

#[derive(Default)]
struct PushToplevel {
    id: u64,
    handle: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    pending: ToplevelInfo,
    published: Option<ToplevelInfo>,
    app_id_for_pid: Option<String>,
}

/// Hands out `toplevel_id`s for the life of the *process*, not of one
/// Wayland connection.
///
/// M5: this counter used to live in `PushState`, which `run_once` rebuilds
/// on every reconnect, so after a compositor restart the sensor started
/// again at 1 and handed the ids of windows C3 still had open to brand new
/// windows. C3's ingest filter then paired a stale `active_since_ns` with
/// a new window and reported a dwell that never happened. Ids are
/// monotonic across reconnects now, so a new window can never be mistaken
/// for an old one.
#[derive(Clone, Default)]
pub struct ToplevelIdSource {
    next: Arc<AtomicU64>,
}

impl ToplevelIdSource {
    pub fn new() -> Self {
        Self {
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    fn next_id(&self) -> u64 {
        // Relaxed: the only invariant is uniqueness, and ordering against
        // other memory is established by the event queue itself.
        self.next.fetch_add(1, Ordering::Relaxed).max(1)
    }
}

struct PushState {
    toplevel_info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    toplevels: Vec<PushToplevel>,
    ids: ToplevelIdSource,
    manager_finished: bool,
    bus: EventBus,
    privacy: PrivacyState,
}

impl Default for PushState {
    fn default() -> Self {
        Self {
            toplevel_info: None,
            toplevels: Vec::new(),
            ids: ToplevelIdSource::new(),
            manager_finished: false,
            bus: EventBus::new(1),
            privacy: PrivacyState::new(Vec::new()),
        }
    }
}

impl PushState {
    /// Publishes `Closed` for every toplevel still open on this (now dead)
    /// connection -- see `run_once`'s call site.
    fn close_open_toplevels(&mut self) {
        for toplevel in std::mem::take(&mut self.toplevels) {
            // Only windows C3 was actually told about need closing.
            if toplevel.published.is_some() {
                self.publish(
                    toplevel.id,
                    Kind::Closed(WindowClosed {}),
                    toplevel.app_id_for_pid.as_deref(),
                );
            }
        }
    }

    fn publish(&self, toplevel_id: u64, kind: Kind, app_id_for_gate: Option<&str>) {
        if !self
            .privacy
            .allows(app_id_for_gate, neuroos_common::now_ns())
        {
            return;
        }
        self.bus.publish(RawTelemetryEvent {
            observed_at_ns: neuroos_common::now_ns(),
            source: "wayland_cosmic".into(),
            payload: Some(Payload::Window(WindowEvent {
                toplevel_id,
                kind: Some(kind),
            })),
        });
    }

    /// Called on a handle's `done` event: the first `done` after creation
    /// publishes `WindowOpened`; every later one diffs against the last
    /// published snapshot and emits only what actually changed.
    fn flush_toplevel(&mut self, handle: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1) {
        let Some(idx) = self
            .toplevels
            .iter()
            .position(|t| t.handle.as_ref() == Some(handle))
        else {
            return;
        };
        let id = self.toplevels[idx].id;
        let pending = self.toplevels[idx].pending.clone();
        let app_id = pending.app_id.clone().unwrap_or_default();

        match self.toplevels[idx].published.clone() {
            None => {
                let (pid, pid_known) = proc::find_pid_for_app_id(&app_id)
                    .ok()
                    .flatten()
                    .map(|p| (p, true))
                    .unwrap_or((0, false));
                self.publish(
                    id,
                    Kind::Opened(WindowOpened {
                        app_id: app_id.clone(),
                        title: pending.title.clone().unwrap_or_default(),
                        pid,
                        pid_known,
                    }),
                    Some(&app_id),
                );
            }
            Some(prev) => {
                if prev.title != pending.title {
                    self.publish(
                        id,
                        Kind::TitleChanged(WindowTitleChanged {
                            title: pending.title.clone().unwrap_or_default(),
                        }),
                        Some(&app_id),
                    );
                }
                if prev.app_id != pending.app_id {
                    self.publish(
                        id,
                        Kind::AppIdChanged(WindowAppIdChanged {
                            app_id: app_id.clone(),
                        }),
                        Some(&app_id),
                    );
                }
                if prev.states != pending.states {
                    self.publish(
                        id,
                        Kind::StateChanged(WindowStateChanged {
                            states: pending.states.iter().map(|s| s.to_proto() as i32).collect(),
                        }),
                        Some(&app_id),
                    );
                }
            }
        }
        self.toplevels[idx].app_id_for_pid = Some(app_id);
        self.toplevels[idx].published = Some(pending);
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for PushState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "zcosmic_toplevel_info_v1"
        {
            state.toplevel_info = Some(registry.bind(name, 1, qh, ()));
        }
    }
}

impl Dispatch<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, ()> for PushState {
    fn event(
        state: &mut Self,
        _info: &zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        event: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zcosmic_toplevel_info_v1::Event::Toplevel { toplevel } => {
                let id = state.ids.next_id();
                state.toplevels.push(PushToplevel {
                    id,
                    handle: Some(toplevel),
                    ..Default::default()
                });
            }
            zcosmic_toplevel_info_v1::Event::Finished => state.manager_finished = true,
            _ => {}
        }
    }

    event_created_child!(
        PushState,
        zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        [
            zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()),
        ]
    );
}

impl Dispatch<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()> for PushState {
    fn event(
        state: &mut Self,
        handle: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zcosmic_toplevel_handle_v1::Event::Title { title } => {
                if let Some(t) = state
                    .toplevels
                    .iter_mut()
                    .find(|t| t.handle.as_ref() == Some(handle))
                {
                    t.pending.title = Some(title);
                }
            }
            zcosmic_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(t) = state
                    .toplevels
                    .iter_mut()
                    .find(|t| t.handle.as_ref() == Some(handle))
                {
                    t.pending.app_id = Some(app_id);
                }
            }
            zcosmic_toplevel_handle_v1::Event::State { state: raw } => {
                if let Some(t) = state
                    .toplevels
                    .iter_mut()
                    .find(|t| t.handle.as_ref() == Some(handle))
                {
                    t.pending.states = raw
                        .chunks_exact(4)
                        .filter_map(|c| <[u8; 4]>::try_from(c).ok())
                        .map(u32::from_ne_bytes)
                        .filter_map(|v| ToplevelState::try_from(v).ok())
                        .collect();
                }
            }
            zcosmic_toplevel_handle_v1::Event::Done => state.flush_toplevel(handle),
            zcosmic_toplevel_handle_v1::Event::Closed => {
                if let Some(idx) = state
                    .toplevels
                    .iter()
                    .position(|t| t.handle.as_ref() == Some(handle))
                {
                    let id = state.toplevels[idx].id;
                    let app_id = state.toplevels[idx].app_id_for_pid.clone();
                    state.publish(id, Kind::Closed(WindowClosed {}), app_id.as_deref());
                    state.toplevels.remove(idx);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    /// M5: `next_id` lived in `PushState`, which `run_once` rebuilds on
    /// every reconnect, so ids restarted at 1 and the first window after a
    /// compositor restart inherited the id of a window C3 still had open.
    #[test]
    fn ids_continue_across_reconnects_instead_of_restarting() {
        let ids = ToplevelIdSource::new();
        // One connection's worth of windows.
        let first: Vec<u64> = (0..3).map(|_| ids.next_id()).collect();
        assert_eq!(first, vec![1, 2, 3]);

        // The compositor restarts: `run_once` builds a fresh `PushState`
        // from the same source.
        let after_reconnect: Vec<u64> = (0..3)
            .map(|_| {
                PushState {
                    ids: ids.clone(),
                    ..Default::default()
                }
                .ids
                .next_id()
            })
            .collect();
        assert_eq!(after_reconnect, vec![4, 5, 6]);
        assert!(
            after_reconnect.iter().all(|id| !first.contains(id)),
            "an id was reused: {first:?} then {after_reconnect:?}"
        );
    }

    /// M5: when the connection dies the compositor never sends `Closed`
    /// for the windows that were open on it, so C3 kept them open (and
    /// kept counting dwell) forever.
    #[tokio::test]
    async fn a_dying_connection_closes_the_windows_it_had_open() {
        let bus = EventBus::new(16);
        let mut sub = bus.subscribe();
        let mut state = PushState {
            bus,
            ..Default::default()
        };
        // Two windows C3 was told about, one that was never published.
        state.toplevels.push(PushToplevel {
            id: 7,
            published: Some(ToplevelInfo::default()),
            app_id_for_pid: Some("org.mozilla.firefox".into()),
            ..Default::default()
        });
        state.toplevels.push(PushToplevel {
            id: 8,
            published: Some(ToplevelInfo::default()),
            ..Default::default()
        });
        state.toplevels.push(PushToplevel {
            id: 9,
            published: None,
            ..Default::default()
        });

        state.close_open_toplevels();
        assert!(state.toplevels.is_empty());

        let mut closed = Vec::new();
        for _ in 0..2 {
            let event = sub.recv().await;
            if let Some(Payload::Window(WindowEvent {
                toplevel_id,
                kind: Some(Kind::Closed(_)),
            })) = event.payload
            {
                closed.push(toplevel_id);
            }
        }
        closed.sort_unstable();
        assert_eq!(
            closed,
            vec![7, 8],
            "only windows C3 had been told about need closing"
        );
    }

    #[test]
    fn state_try_from_matches_the_wire_protocol_values() {
        assert_eq!(ToplevelState::try_from(0), Ok(ToplevelState::Maximized));
        assert_eq!(ToplevelState::try_from(2), Ok(ToplevelState::Activated));
        assert_eq!(ToplevelState::try_from(99), Err(()));
    }

    #[test]
    fn is_activated_checks_the_states_list() {
        let mut info = ToplevelInfo::default();
        assert!(!info.is_activated());
        info.states.push(ToplevelState::Maximized);
        assert!(!info.is_activated());
        info.states.push(ToplevelState::Activated);
        assert!(info.is_activated());
    }

    /// Live integration proof (spike S-03): only runs with a real Wayland
    /// session available, which CI doesn't have — run manually with `cargo
    /// test -p neuroos-monitor -- --ignored wayland_cosmic`.
    #[test]
    #[ignore = "needs a real Wayland/COSMIC session; see doc comment"]
    fn list_toplevels_reaches_a_real_compositor() {
        let toplevels = list_toplevels().expect("zcosmic_toplevel_info_v1 should be reachable");
        assert!(
            !toplevels.is_empty(),
            "expected at least one toplevel window on a real desktop"
        );
    }

    /// Live integration proof (P3-S01): the real reference COSMIC session
    /// already has toplevels open (panel, launcher, ...), so subscribing
    /// must yield at least one real `WindowOpened` within a few seconds —
    /// run manually with `cargo test -p neuroos-monitor -- --ignored
    /// wayland_cosmic_push_event_stream`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real Wayland/COSMIC session; see doc comment"]
    async fn wayland_cosmic_push_event_stream_reports_real_open_windows() {
        let bus = EventBus::new(64);
        let privacy = PrivacyState::new(Vec::new());
        let mut sub = bus.subscribe();
        let health = SensorHealth::new(neuroos_health::HealthServer::new("test"));
        tokio::spawn(run_forever(bus, privacy, health));

        let opened = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let event = sub.recv().await;
                if let Some(Payload::Window(WindowEvent {
                    kind: Some(Kind::Opened(opened)),
                    ..
                })) = event.payload
                {
                    return opened;
                }
            }
        })
        .await
        .expect("expected at least one real WindowOpened within 5s");
        assert!(
            !opened.app_id.is_empty(),
            "a real toplevel should have an app_id"
        );
        // The sensor's blocking_dispatch loop runs forever by design (see
        // ADR-0009): tokio's multi-thread Runtime blocks on Drop until every
        // spawn_blocking task finishes, so a clean async return here would
        // hang the test process forever waiting on a loop that never exits.
        // Manually-run diagnostic test only; exiting the process directly is
        // the correct teardown, matching how main.rs itself never returns.
        std::process::exit(0);
    }
}
