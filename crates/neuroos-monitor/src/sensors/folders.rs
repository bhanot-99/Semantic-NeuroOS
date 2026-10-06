//! Watched-folders sensor (FR-MON-09, P3-S07, P1): inotify via `notify` for
//! configured git repos, the notes vault and ICS calendar files.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};

use crate::bus::EventBus;
use crate::privacy::PrivacyState;
use crate::sensor_health::{SensorHealth, supervise};
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::{FileActivityEvent, FileActivityKind, RawTelemetryEvent};

#[derive(Debug, thiserror::Error)]
pub enum FolderSensorError {
    #[error("failed to set up the folder watcher: {0}")]
    Notify(#[from] notify::Error),
}

#[derive(Debug, Clone)]
pub struct Watch {
    pub label: String,
    pub path: PathBuf,
}

/// Runs the folder sensor forever, reconnecting with backoff if the watcher
/// itself fails (AB-10). A single missing configured path is logged and
/// skipped rather than failing the whole sensor.
pub async fn run_forever(
    watches: Vec<Watch>,
    bus: EventBus,
    privacy: PrivacyState,
    health: SensorHealth,
) {
    if watches.is_empty() {
        // Configured off, not failed: it never reports to `health` at all.
        return;
    }
    let stop = Arc::new(AtomicBool::new(false)); // production: never asked to stop
    supervise("folders", health, move || {
        let watches = watches.clone();
        let bus = bus.clone();
        let privacy = privacy.clone();
        let stop = Arc::clone(&stop);
        async move {
            match tokio::task::spawn_blocking(move || {
                run_with_stop(&watches, &bus, &privacy, &stop)
            })
            .await
            {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(e) => Err(format!("sensor thread panicked: {e}")),
            }
        }
    })
    .await;
}

/// Polling period for the cooperative-cancellation check in
/// [`run_with_stop`] — bounds both the shutdown latency and (in production,
/// where `stop` is never set) how promptly a dead watcher's channel closure
/// is noticed.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// L11: inotify events pending between the watcher callback and the drain
/// loop below. This used to be `mpsc::channel()`, which is unbounded: a
/// burst that outruns the consumer -- a `git clone` or a build landing
/// thousands of events into a watched tree in a few milliseconds -- grew
/// the queue with no ceiling, inside C1's 25 MiB RSS budget (PRD §6.2).
/// The drain loop is tight, so a thousand pending events is already far
/// more headroom than a healthy burst needs.
const EVENT_QUEUE_CAPACITY: usize = 1024;

/// L11: how many dropped events between `warn!` lines, so a sustained
/// flood reports itself without becoming the flood. The message carries a
/// count and nothing else -- never a path (rules.md R0-6).
const DROP_LOG_EVERY: u64 = 256;

/// `Ok(())` only when `stop` is observed set (cooperative cancellation, used
/// by tests so the blocking OS thread this runs on actually exits — without
/// it, tokio's multi-thread `Runtime::drop` blocks forever waiting for a
/// `spawn_blocking` task that never returns; see ADR-0009). In production,
/// `stop` is never set and this behaves exactly like an unconditional loop.
fn run_with_stop(
    watches: &[Watch],
    bus: &EventBus,
    privacy: &PrivacyState,
    stop: &AtomicBool,
) -> Result<(), FolderSensorError> {
    // L11: bounded, and the watcher callback drops rather than blocks.
    // Blocking it would stall the thread reading the inotify fd, which
    // makes the *kernel* queue overflow instead -- losing the same events
    // with less control and no count. Dropping the newest event under
    // flood is the same choice `bus.rs` makes for a slow subscriber.
    let (tx, rx) = mpsc::sync_channel::<notify::Result<Event>>(EVENT_QUEUE_CAPACITY);
    // `AtomicU64`, not a plain counter: `notify` takes an `Fn` callback, so
    // the count has to be mutable through a shared reference.
    let dropped = AtomicU64::new(0);
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        // `notify`'s handler returns (); the bool says whether the event
        // was queued, which only the test needs.
        let _queued = offer_event(&tx, res, &dropped);
    })?;
    for w in watches {
        if let Err(e) = watcher.watch(&w.path, RecursiveMode::Recursive) {
            tracing::warn!(path = %w.path.display(), error = %e, "could not watch configured folder");
        }
    }

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let res = match rx.recv_timeout(POLL_INTERVAL) {
            Ok(res) => res,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            // Every sender (the watcher) was dropped, which doesn't happen
            // while `watcher` stays in scope above — the watcher itself died.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(FolderSensorError::Notify(notify::Error::generic(
                    "folder watcher channel closed unexpectedly",
                )));
            }
        };
        let Ok(event) = res else { continue };
        let Some(kind) = classify(&event.kind) else {
            continue;
        };
        for path in &event.paths {
            let Some(label) = label_for(watches, path) else {
                continue;
            };
            if !privacy.allows(None, neuroos_common::now_ns()) {
                continue;
            }
            bus.publish(RawTelemetryEvent {
                observed_at_ns: neuroos_common::now_ns(),
                source: "folders".into(),
                payload: Some(Payload::FileActivity(FileActivityEvent {
                    path: path.display().to_string(),
                    kind: kind as i32,
                    watch_label: label,
                })),
            });
        }
    }
}

/// L11: queues one watcher event, or drops it when the bounded queue is
/// full. Returns whether it was queued. A full queue is a flood -- the
/// event is dropped and counted, never blocked on, because blocking the
/// watcher callback stalls the thread reading the inotify fd and makes the
/// *kernel* queue overflow instead: the same events lost, with no count
/// and no control. `Disconnected` means the receiver is gone, which only
/// happens as [`run_with_stop`] returns, and needs no counting.
fn offer_event(
    tx: &mpsc::SyncSender<notify::Result<Event>>,
    res: notify::Result<Event>,
    dropped: &AtomicU64,
) -> bool {
    match tx.try_send(res) {
        Ok(()) => true,
        Err(mpsc::TrySendError::Full(_)) => {
            let n = dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n % DROP_LOG_EVERY == 1 {
                tracing::warn!(
                    dropped_total = n,
                    capacity = EVENT_QUEUE_CAPACITY,
                    "folder event queue is full; dropping file-activity events"
                );
            }
            false
        }
        Err(mpsc::TrySendError::Disconnected(_)) => false,
    }
}

fn classify(kind: &EventKind) -> Option<FileActivityKind> {
    match kind {
        EventKind::Create(_) => Some(FileActivityKind::Created),
        EventKind::Modify(ModifyKind::Name(_)) => Some(FileActivityKind::Renamed),
        EventKind::Modify(_) => Some(FileActivityKind::Modified),
        EventKind::Remove(_) => Some(FileActivityKind::Deleted),
        _ => None,
    }
}

/// The label of the longest configured watch root that contains `path`
/// (handles overlapping watches deterministically).
fn label_for(watches: &[Watch], path: &Path) -> Option<String> {
    watches
        .iter()
        .filter(|w| path.starts_with(&w.path))
        .max_by_key(|w| w.path.as_os_str().len())
        .map(|w| w.label.clone())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    /// L11: the queue used to be `mpsc::channel()` -- unbounded -- so a
    /// burst of inotify events grew it without limit inside C1's 25 MiB
    /// budget. It is now `sync_channel(EVENT_QUEUE_CAPACITY)` and the
    /// surplus is dropped and counted rather than queued or blocked on.
    #[test]
    fn a_flood_of_events_is_dropped_rather_than_queued_without_limit() {
        let (tx, rx) = mpsc::sync_channel::<notify::Result<Event>>(EVENT_QUEUE_CAPACITY);
        let dropped = AtomicU64::new(0);
        let event = || {
            Ok(Event {
                kind: EventKind::Create(notify::event::CreateKind::File),
                paths: vec![PathBuf::from("/home/u/proj/f")],
                attrs: Default::default(),
            })
        };

        // Exactly the capacity is accepted...
        for i in 0..EVENT_QUEUE_CAPACITY {
            assert!(offer_event(&tx, event(), &dropped), "event {i} must queue");
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 0);

        // ...and everything past it is dropped, not queued and not blocked
        // on (this call returning at all is the no-blocking proof).
        for _ in 0..5_000 {
            assert!(!offer_event(&tx, event(), &dropped));
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 5_000);

        // The queue itself never grew past the bound.
        let mut drained = 0;
        while rx.try_recv().is_ok() {
            drained += 1;
        }
        assert_eq!(drained, EVENT_QUEUE_CAPACITY);
    }

    fn watches() -> Vec<Watch> {
        vec![
            Watch {
                label: "git".into(),
                path: PathBuf::from("/home/u/proj"),
            },
            Watch {
                label: "notes".into(),
                path: PathBuf::from("/home/u/Notes"),
            },
        ]
    }

    #[test]
    fn label_for_matches_the_containing_watch() {
        assert_eq!(
            label_for(&watches(), Path::new("/home/u/proj/src/main.rs")),
            Some("git".to_string())
        );
        assert_eq!(
            label_for(&watches(), Path::new("/home/u/Notes/todo.md")),
            Some("notes".to_string())
        );
    }

    #[test]
    fn label_for_unwatched_path_is_none() {
        assert_eq!(label_for(&watches(), Path::new("/tmp/other")), None);
    }

    #[test]
    fn label_for_overlapping_watches_prefers_the_longer_root() {
        let nested = vec![
            Watch {
                label: "outer".into(),
                path: PathBuf::from("/home/u"),
            },
            Watch {
                label: "inner".into(),
                path: PathBuf::from("/home/u/proj"),
            },
        ];
        assert_eq!(
            label_for(&nested, Path::new("/home/u/proj/f")),
            Some("inner".to_string())
        );
    }

    #[test]
    fn classify_maps_create_modify_remove_rename() {
        assert_eq!(
            classify(&EventKind::Create(notify::event::CreateKind::File)),
            Some(FileActivityKind::Created)
        );
        assert_eq!(
            classify(&EventKind::Remove(notify::event::RemoveKind::File)),
            Some(FileActivityKind::Deleted)
        );
        assert_eq!(
            classify(&EventKind::Modify(ModifyKind::Name(
                notify::event::RenameMode::Any
            ))),
            Some(FileActivityKind::Renamed)
        );
        assert_eq!(
            classify(&EventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Content
            ))),
            Some(FileActivityKind::Modified)
        );
        assert_eq!(
            classify(&EventKind::Access(notify::event::AccessKind::Read)),
            None
        );
    }

    /// Live proof (real inotify, real filesystem event, real bus): touches a
    /// file inside a tempdir watch and observes the FileActivityEvent.
    #[tokio::test]
    async fn a_real_file_create_produces_a_file_activity_event() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(8);
        let mut sub = bus.subscribe();
        let watches = vec![Watch {
            label: "test".into(),
            path: dir.path().to_path_buf(),
        }];
        let privacy = PrivacyState::new(Vec::new());
        let stop = Arc::new(AtomicBool::new(false));
        let handle = tokio::task::spawn_blocking({
            let watches = watches.clone();
            let bus = bus.clone();
            let privacy = privacy.clone();
            let stop = Arc::clone(&stop);
            move || run_with_stop(&watches, &bus, &privacy, &stop)
        });
        // give the watcher a moment to register before writing
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        std::fs::write(dir.path().join("new_file.txt"), b"hello").unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let e = sub.recv().await;
                if let Some(Payload::FileActivity(fa)) = e.payload {
                    return fa;
                }
            }
        })
        .await
        .expect("expected a FileActivityEvent within 5s");
        assert_eq!(event.watch_label, "test");
        assert!(event.path.ends_with("new_file.txt"));

        // Ask the blocking thread to stop and wait for it, rather than
        // letting the test's runtime drop while it's still parked in
        // `recv_timeout` — see run_with_stop's doc comment / ADR-0009.
        stop.store(true, Ordering::Relaxed);
        handle
            .await
            .expect("sensor thread panicked")
            .expect("sensor should stop cleanly, not error, when asked");
    }
}
