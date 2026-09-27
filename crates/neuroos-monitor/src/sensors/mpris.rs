//! MPRIS media session sensor (FR-MON-03, P3-S03): D-Bus session bus, via
//! `zbus`. Watches every `org.mpris.MediaPlayer2.*` name for playback
//! status and metadata changes.
use std::collections::HashMap;

use futures_util::StreamExt;
use zbus::fdo::{DBusProxy, PropertiesProxy};
use zbus::zvariant::OwnedValue;
use zbus::{Connection, proxy};

use crate::bus::EventBus;
use crate::privacy::PrivacyState;
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::{MprisEvent, RawTelemetryEvent};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";

#[derive(Debug, thiserror::Error)]
pub enum MprisSensorError {
    #[error("D-Bus session bus connection failed: {0}")]
    Zbus(#[from] zbus::Error),
}

#[proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait MprisPlayer {
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    #[zbus(property)]
    fn position(&self) -> zbus::Result<i64>;
}

/// Runs the MPRIS sensor forever, reconnecting with backoff if the D-Bus
/// connection itself drops (AB-10; FI: "D-Bus player vanishing" is handled
/// per-player inside the loop, not by tearing this down).
pub async fn run_forever(bus: EventBus, privacy: PrivacyState) {
    let mut backoff_ms = 100u64;
    loop {
        match run_once(&bus, &privacy).await {
            Ok(()) => unreachable!("run_once only returns on error"),
            Err(e) => tracing::warn!(error = %e, "MPRIS sensor stopped; reconnecting"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(10_000);
    }
}

async fn run_once(bus: &EventBus, privacy: &PrivacyState) -> Result<(), MprisSensorError> {
    let conn = Connection::session().await?;
    let dbus = DBusProxy::new(&conn).await?;

    // Watch for players appearing/disappearing.
    let mut owner_changes = dbus.receive_name_owner_changed().await?;

    // Start watching every player already running.
    let names = dbus.list_names().await.map_err(zbus::Error::from)?;
    let mut tasks = tokio::task::JoinSet::new();
    for name in names {
        let name = name.to_string();
        if name.starts_with(MPRIS_PREFIX) {
            tasks.spawn(watch_player(
                conn.clone(),
                name,
                bus.clone(),
                privacy.clone(),
            ));
        }
    }

    while let Some(signal) = owner_changes.next().await {
        let Ok(args) = signal.args() else { continue };
        let name = args.name().to_string();
        if name.starts_with(MPRIS_PREFIX) {
            // Either a new player (spawn and watch) or one that just vanished
            // (spawn anyway; its first property fetch fails immediately and
            // the task exits — self-cleaning, no separate "is it a removal"
            // check needed).
            tasks.spawn(watch_player(
                conn.clone(),
                name,
                bus.clone(),
                privacy.clone(),
            ));
        }
    }
    Ok(())
}

/// Watches one player's `PlaybackStatus`/`Metadata`/`Position` until it goes
/// away (its proxy calls start failing, e.g. `NameHasNoOwner`), then returns.
async fn watch_player(conn: Connection, bus_name: String, bus: EventBus, privacy: PrivacyState) {
    if let Err(e) = watch_player_inner(conn, bus_name, bus, privacy).await {
        tracing::debug!(error = %e, "MPRIS player watch ended");
    }
}

async fn watch_player_inner(
    conn: Connection,
    bus_name: String,
    bus: EventBus,
    privacy: PrivacyState,
) -> zbus::Result<()> {
    let player = MprisPlayerProxy::builder(&conn)
        .destination(bus_name.clone())?
        .build()
        .await?;
    let props = PropertiesProxy::builder(&conn)
        .destination(bus_name.clone())?
        .path(PLAYER_PATH)?
        .build()
        .await?;
    let mut changes = props.receive_properties_changed().await?;

    publish_snapshot(&player, &bus_name, &bus, &privacy).await;
    while let Some(signal) = changes.next().await {
        let Ok(args) = signal.args() else { continue };
        if args.interface_name().as_str() != PLAYER_IFACE {
            continue;
        }
        publish_snapshot(&player, &bus_name, &bus, &privacy).await;
    }
    Ok(())
}

async fn publish_snapshot(
    player: &MprisPlayerProxy<'_>,
    bus_name: &str,
    bus: &EventBus,
    privacy: &PrivacyState,
) {
    if !privacy.allows(None, neuroos_common::now_ns()) {
        return;
    }
    let Ok(playback_status) = player.playback_status().await else {
        return;
    };
    let metadata = player.metadata().await.unwrap_or_default();
    let position_us = player.position().await.unwrap_or(0);

    bus.publish(RawTelemetryEvent {
        observed_at_ns: neuroos_common::now_ns(),
        source: "mpris".into(),
        payload: Some(Payload::Mpris(MprisEvent {
            player_bus_name: bus_name.to_string(),
            playback_status,
            track_id: metadata_str(&metadata, "mpris:trackid"),
            title: metadata_str(&metadata, "xesam:title"),
            artist: metadata_array_str(&metadata, "xesam:artist"),
            album: metadata_str(&metadata, "xesam:album"),
            position_us,
        })),
    });
}

fn metadata_str(metadata: &HashMap<String, OwnedValue>, key: &str) -> String {
    metadata
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default()
}

fn metadata_array_str(metadata: &HashMap<String, OwnedValue>, key: &str) -> String {
    metadata
        .get(key)
        .and_then(|v| <Vec<String>>::try_from(v.clone()).ok())
        .map(|v| v.join(", "))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn owned(value: zbus::zvariant::Value<'_>) -> OwnedValue {
        OwnedValue::try_from(value).unwrap()
    }

    #[test]
    fn metadata_str_reads_a_string_value() {
        let mut m = HashMap::new();
        m.insert(
            "xesam:title".to_string(),
            owned(zbus::zvariant::Value::from("Song Name")),
        );
        assert_eq!(metadata_str(&m, "xesam:title"), "Song Name");
        assert_eq!(metadata_str(&m, "missing"), "");
    }

    #[test]
    fn metadata_array_str_joins_an_artist_list() {
        let mut m = HashMap::new();
        m.insert(
            "xesam:artist".to_string(),
            owned(zbus::zvariant::Value::from(vec![
                "A".to_string(),
                "B".to_string(),
            ])),
        );
        assert_eq!(metadata_array_str(&m, "xesam:artist"), "A, B");
    }

    /// Live proof (P3-S03): connects to the real session bus, lists names
    /// and watches for owner changes, without erroring — needs a real
    /// D-Bus session, so `#[ignore]`d like the Wayland sensors' own live
    /// tests, but `run_once`'s loop here is a plain async stream (no
    /// blocking OS thread), so it aborts cleanly with no ADR-0009-style
    /// runtime-drop hang.
    #[tokio::test]
    #[ignore = "needs a real D-Bus session bus"]
    async fn mpris_sensor_connects_to_the_real_session_bus() {
        let bus = EventBus::new(4);
        let privacy = PrivacyState::new(Vec::new());
        // Timing out (dropping the future) means connect + list_names +
        // subscribe all succeeded and it's parked waiting for the next
        // owner-change signal; an early Err would mean a real failure.
        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            run_once(&bus, &privacy),
        )
        .await
        {
            Err(_) => {}
            Ok(Err(e)) => panic!("run_once errored: {e}"),
            Ok(Ok(())) => panic!("run_once returned Ok(()) unexpectedly"),
        }
    }
}
