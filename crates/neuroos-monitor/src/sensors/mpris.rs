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
use crate::sensor_health::{SensorHealth, supervise};
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::{MprisEvent, RawTelemetryEvent};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";

#[derive(Debug, thiserror::Error)]
pub enum MprisSensorError {
    #[error("D-Bus session bus connection failed: {0}")]
    Zbus(#[from] zbus::Error),
    /// H10: the bus connection went away (session bus restarted or
    /// crashed), which ends the `NameOwnerChanged` stream.
    #[error("D-Bus session bus connection closed")]
    Disconnected,
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
pub async fn run_forever(bus: EventBus, privacy: PrivacyState, health: SensorHealth) {
    supervise("mpris", health, move || {
        let bus = bus.clone();
        let privacy = privacy.clone();
        async move { Err(run_once(&bus, &privacy).await) }
    })
    .await;
}

/// Only ever returns once the sensor has stopped, so the return value is
/// always the reason why (H10: there is no success case to mistake for
/// "unreachable").
async fn run_once(bus: &EventBus, privacy: &PrivacyState) -> MprisSensorError {
    match Connection::session().await {
        Ok(conn) => run_once_on(conn, bus, privacy).await,
        Err(e) => e.into(),
    }
}

async fn run_once_on(conn: Connection, bus: &EventBus, privacy: &PrivacyState) -> MprisSensorError {
    match watch_bus(conn, bus, privacy).await {
        Ok(()) => MprisSensorError::Disconnected,
        Err(e) => e,
    }
}

/// M7: what one `NameOwnerChanged` means for the watcher set.
///
/// The old code spawned a watcher on *every* MPRIS owner change and relied
/// on "its first property fetch fails immediately and the task exits" to
/// clean up. That is not what happens: `receive_properties_changed()`'s
/// stream does not close when the player goes away, so a watcher for a
/// dead player blocks on `changes.next()` forever instead of returning.
/// Worse, a player that was merely *replaced* (a new owner for a name we
/// already watch) got a second watcher while the first was still live, so
/// every property change published duplicate telemetry events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerChange {
    /// A player appeared on a name nothing is watching yet.
    Start,
    /// The name lost its owner: abort the watcher we have for it.
    Stop,
    /// Not an MPRIS name, or a name already watched by a live task.
    Ignore,
}

fn classify_owner_change(name: &str, has_new_owner: bool, already_watching: bool) -> OwnerChange {
    if !name.starts_with(MPRIS_PREFIX) {
        return OwnerChange::Ignore;
    }
    match (has_new_owner, already_watching) {
        (false, _) => OwnerChange::Stop,
        (true, false) => OwnerChange::Start,
        (true, true) => OwnerChange::Ignore,
    }
}

/// Watches every player until the owner-change stream ends (`Ok`) or a
/// bus call fails (`Err`).
///
/// Holds exactly one watcher task per player bus name (M7). Dropping the
/// `JoinSet` on return aborts every watcher, so a reconnect starts clean.
async fn watch_bus(
    conn: Connection,
    bus: &EventBus,
    privacy: &PrivacyState,
) -> Result<(), MprisSensorError> {
    let dbus = DBusProxy::new(&conn).await?;

    // Watch for players appearing/disappearing.
    let mut owner_changes = dbus.receive_name_owner_changed().await?;

    // Start watching every player already running.
    let names = dbus.list_names().await.map_err(zbus::Error::from)?;
    let mut tasks = tokio::task::JoinSet::new();
    let mut watchers: HashMap<String, tokio::task::AbortHandle> = HashMap::new();
    for name in names {
        let name = name.to_string();
        if name.starts_with(MPRIS_PREFIX) {
            spawn_watcher(&mut tasks, &mut watchers, &conn, name, bus, privacy);
        }
    }

    while let Some(signal) = owner_changes.next().await {
        // Reap watchers that ended by themselves, so neither the `JoinSet`
        // nor `watchers` grows without bound.
        reap_finished(&mut tasks, &mut watchers);

        let Ok(args) = signal.args() else { continue };
        let name = args.name().to_string();
        let has_new_owner = args.new_owner().as_ref().is_some();
        match classify_owner_change(&name, has_new_owner, watchers.contains_key(&name)) {
            OwnerChange::Start => {
                spawn_watcher(&mut tasks, &mut watchers, &conn, name, bus, privacy);
            }
            OwnerChange::Stop => {
                // The player is gone and its property stream will never
                // close on its own, so the task has to be cancelled.
                if let Some(handle) = watchers.remove(&name) {
                    handle.abort();
                }
            }
            OwnerChange::Ignore => {}
        }
    }
    Ok(())
}

/// Spawns one watcher for `name` and records its abort handle. The task
/// yields its own bus name so [`reap_finished`] knows which entry to drop.
fn spawn_watcher(
    tasks: &mut tokio::task::JoinSet<String>,
    watchers: &mut HashMap<String, tokio::task::AbortHandle>,
    conn: &Connection,
    name: String,
    bus: &EventBus,
    privacy: &PrivacyState,
) {
    let conn = conn.clone();
    let bus = bus.clone();
    let privacy = privacy.clone();
    let key = name.clone();
    let handle = tasks.spawn(async move {
        watch_player(conn, name.clone(), bus, privacy).await;
        name
    });
    watchers.insert(key, handle);
}

/// Drops the bookkeeping for every watcher that has already returned.
/// Aborted tasks were removed from `watchers` before the abort, so their
/// `JoinError` needs no further handling.
fn reap_finished(
    tasks: &mut tokio::task::JoinSet<String>,
    watchers: &mut HashMap<String, tokio::task::AbortHandle>,
) {
    while let Some(result) = tasks.try_join_next() {
        if let Ok(finished) = result {
            watchers.remove(&finished);
        }
    }
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

    /// M7: before the fix every MPRIS owner change spawned another
    /// watcher, so a replaced player ran two live watchers publishing
    /// duplicate events, and a vanished player's watcher was never
    /// cancelled at all.
    #[test]
    fn an_appearing_player_is_watched_once_and_only_once() {
        let name = "org.mpris.MediaPlayer2.vlc";
        assert_eq!(
            classify_owner_change(name, true, false),
            OwnerChange::Start,
            "a new player must be watched"
        );
        assert_eq!(
            classify_owner_change(name, true, true),
            OwnerChange::Ignore,
            "a name we already watch must not get a second watcher"
        );
    }

    #[test]
    fn a_vanishing_player_stops_its_watcher() {
        // No new owner = the name was released; the watcher's property
        // stream will never close by itself, so it must be aborted.
        assert_eq!(
            classify_owner_change("org.mpris.MediaPlayer2.vlc", false, true),
            OwnerChange::Stop
        );
        // Stop is still correct when we have no handle: `watchers.remove`
        // simply finds nothing.
        assert_eq!(
            classify_owner_change("org.mpris.MediaPlayer2.vlc", false, false),
            OwnerChange::Stop
        );
    }

    #[test]
    fn non_mpris_names_are_ignored_entirely() {
        for (has_owner, watching) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(
                classify_owner_change("org.freedesktop.Notifications", has_owner, watching),
                OwnerChange::Ignore
            );
        }
    }

    /// M7: the `JoinSet` was never reaped, so finished tasks accumulated
    /// for the life of the bus connection.
    #[tokio::test]
    async fn finished_watchers_are_reaped_from_both_the_joinset_and_the_map() {
        let mut tasks: tokio::task::JoinSet<String> = tokio::task::JoinSet::new();
        let mut watchers: HashMap<String, tokio::task::AbortHandle> = HashMap::new();

        for name in ["a", "b"] {
            let owned = name.to_string();
            let handle = tasks.spawn(async move { owned });
            watchers.insert(name.to_string(), handle);
        }
        // Let both tasks finish.
        tokio::task::yield_now().await;
        while !tasks.is_empty() {
            reap_finished(&mut tasks, &mut watchers);
            if !tasks.is_empty() {
                tokio::task::yield_now().await;
            }
        }
        assert!(watchers.is_empty(), "{watchers:?} should have been reaped");
        assert_eq!(tasks.len(), 0);
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

    /// H10: when the bus connection drops, the owner-change stream ends.
    /// That must surface as an error (so `run_forever` reconnects), never
    /// as `Ok(())` -- which `run_forever` used to treat as unreachable and
    /// panic on, killing the sensor for good. Uses a private, throwaway
    /// `dbus-daemon` so the test can kill the bus deterministically.
    #[tokio::test]
    async fn a_dropped_bus_connection_is_an_error_not_a_panic() {
        use std::io::BufRead;
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("dbus-daemon must be installed for this test");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let conn = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();

        let bus = EventBus::new(4);
        let privacy = PrivacyState::new(Vec::new());
        let sensor = tokio::spawn(async move { run_once_on(conn, &bus, &privacy).await });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        daemon.kill().unwrap();
        daemon.wait().unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), sensor)
            .await
            .expect("the sensor must notice the bus going away")
            .expect("the sensor task must not panic");
        assert!(
            matches!(result, MprisSensorError::Disconnected),
            "a dropped bus must be reported as a disconnect, got {result}"
        );
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
        if let Ok(e) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            run_once(&bus, &privacy),
        )
        .await
        {
            panic!("run_once stopped early: {e}");
        }
    }
}
