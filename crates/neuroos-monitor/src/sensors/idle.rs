//! Idle/active sensor: `ext_idle_notify_v1` (FR-MON-02, P3-S02).
//!
//! Same connect-and-dispatch shape as [`super::wayland_cosmic`]'s push-event
//! loop: runs on a dedicated OS thread (wayland-client's event queue isn't
//! `Send` across an `.await`), publishing directly to the [`crate::bus`]
//! since `broadcast::Sender::send` is synchronous.
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};

use crate::bus::EventBus;
use crate::privacy::PrivacyState;
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::{IdleEvent, RawTelemetryEvent};

#[derive(Debug, thiserror::Error)]
pub enum IdleSensorError {
    #[error("could not connect to the Wayland compositor: {0}")]
    Connect(#[from] wayland_client::ConnectError),
    #[error("Wayland dispatch failed: {0}")]
    Dispatch(#[from] wayland_client::DispatchError),
    #[error("compositor does not support ext_idle_notify_v1")]
    ProtocolUnsupported,
    #[error("compositor announced no wl_seat")]
    NoSeat,
}

/// Runs the idle sensor forever, reconnecting with backoff on any error
/// (AB-10; FI: "compositor restart"). Never returns under normal operation.
pub async fn run_forever(timeout_ms: u32, bus: EventBus, privacy: PrivacyState) {
    let mut backoff_ms = 100u64;
    loop {
        let bus = bus.clone();
        let privacy = privacy.clone();
        let result =
            tokio::task::spawn_blocking(move || run_once(timeout_ms, &bus, &privacy)).await;
        match result {
            Ok(Ok(())) => unreachable!("run_once only returns on error"),
            Ok(Err(e)) => tracing::warn!(error = %e, "idle sensor stopped; reconnecting"),
            Err(e) => tracing::warn!(error = %e, "idle sensor thread panicked; reconnecting"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(10_000);
    }
}

fn run_once(
    timeout_ms: u32,
    bus: &EventBus,
    privacy: &PrivacyState,
) -> Result<(), IdleSensorError> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let mut state = IdleState::default();
    event_queue.roundtrip(&mut state)?;

    let notifier = state
        .notifier
        .as_ref()
        .ok_or(IdleSensorError::ProtocolUnsupported)?;
    let seat = state.seat.as_ref().ok_or(IdleSensorError::NoSeat)?;
    let _notification = notifier.get_idle_notification(timeout_ms, seat, &qh, ());

    state.bus = Some(bus.clone());
    state.privacy = Some(privacy.clone());
    loop {
        event_queue.blocking_dispatch(&mut state)?;
    }
}

#[derive(Default)]
struct IdleState {
    notifier: Option<ext_idle_notifier_v1::ExtIdleNotifierV1>,
    seat: Option<wl_seat::WlSeat>,
    bus: Option<EventBus>,
    privacy: Option<PrivacyState>,
}

impl IdleState {
    fn publish(&self, idle: bool) {
        let Some(bus) = &self.bus else { return };
        if let Some(privacy) = &self.privacy
            && !privacy.allows(None, neuroos_common::now_ns())
        {
            return;
        }
        bus.publish(RawTelemetryEvent {
            observed_at_ns: neuroos_common::now_ns(),
            source: "idle".into(),
            payload: Some(Payload::Idle(IdleEvent { idle })),
        });
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for IdleState {
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
        {
            match interface.as_str() {
                "ext_idle_notifier_v1" => state.notifier = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for IdleState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ext_idle_notifier_v1::ExtIdleNotifierV1, ()> for IdleState {
    fn event(
        _: &mut Self,
        _: &ext_idle_notifier_v1::ExtIdleNotifierV1,
        _: ext_idle_notifier_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, ()> for IdleState {
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.publish(true),
            ext_idle_notification_v1::Event::Resumed => state.publish(false),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[tokio::test]
    async fn idle_state_publishes_through_the_bus() {
        let bus = EventBus::new(4);
        let mut sub = bus.subscribe();
        let state = IdleState {
            bus: Some(bus),
            privacy: Some(PrivacyState::new(Vec::new())),
            ..Default::default()
        };
        state.publish(true);
        let got = sub.recv().await;
        match got.payload {
            Some(Payload::Idle(IdleEvent { idle })) => assert!(idle),
            other => panic!("expected Idle payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn idle_state_respects_pause() {
        let bus = EventBus::new(4);
        let mut sub = bus.subscribe();
        let privacy = PrivacyState::new(Vec::new());
        privacy.pause(None, 0);
        let state = IdleState {
            bus: Some(bus),
            privacy: Some(privacy),
            ..Default::default()
        };
        state.publish(true);
        let timed_out = tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv())
            .await
            .is_err();
        assert!(timed_out, "paused sensor must not publish");
    }
}
