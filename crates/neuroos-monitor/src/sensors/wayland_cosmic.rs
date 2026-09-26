//! COSMIC compositor sensor: `zcosmic_toplevel_info_v1` (spike S-03, P0-S08).
//!
//! Proven live against the reference machine's real COSMIC session
//! (docs/adr/0006-cosmic-toplevel-and-bitnet-spikes.md): lists every
//! toplevel window with its `app_id`, title and activation state. This is
//! the spike-level proof the protocol is reachable and gives the data C1
//! needs; wiring it into `monitor.sock`'s push-event stream is Phase 3.
use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1,
};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};

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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

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
}
