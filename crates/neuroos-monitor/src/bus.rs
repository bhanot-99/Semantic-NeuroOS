//! Event bus: fans raw sensor events out to every `monitor.sock` subscriber.
//!
//! phases.md §6.1 item 2: "bounded buffer and drop counter". `tokio::sync::broadcast`
//! already gives both for free — a slow/absent subscriber that falls behind the
//! bounded channel capacity gets `RecvError::Lagged(n)` instead of blocking the
//! sender or growing memory, which is exactly the FI requirement in §6.3
//! ("subscriber slow/absent: bounded buffer, drop counter increments").
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use neuroos_proto::v1::RawTelemetryEvent;
use tokio::sync::broadcast;

/// Shared handle: cheap to clone, one per process.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<RawTelemetryEvent>,
    dropped: Arc<AtomicU64>,
}

pub struct EventSubscriber {
    rx: broadcast::Receiver<RawTelemetryEvent>,
    dropped: Arc<AtomicU64>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Publishes one event. `Err` only when there are zero subscribers left
    /// (nothing dropped — nobody was listening), which is not itself an error
    /// worth logging on every event on a system with an intermittent C3.
    pub fn publish(&self, event: RawTelemetryEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> EventSubscriber {
        EventSubscriber {
            rx: self.tx.subscribe(),
            dropped: Arc::clone(&self.dropped),
        }
    }

    /// Total events ever dropped across all subscribers combined (a subscriber
    /// falling behind by `n` increments this by `n` the next time it polls).
    pub fn total_dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl EventSubscriber {
    /// Waits for the next event, transparently skipping past any events this
    /// subscriber lagged behind on (after recording the drop count).
    pub async fn recv(&mut self) -> RawTelemetryEvent {
        loop {
            match self.rx.recv().await {
                Ok(event) => return event,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    self.dropped.fetch_add(n, Ordering::Relaxed);
                    tracing::warn!(dropped = n, "subscriber lagged; events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // The bus itself is never dropped while the process runs;
                    // park forever rather than spin.
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_proto::v1::raw_telemetry_event::Payload;
    use neuroos_proto::v1::{IdleEvent, RawTelemetryEvent};

    fn idle_event(idle: bool) -> RawTelemetryEvent {
        RawTelemetryEvent {
            observed_at_ns: 1,
            source: "idle".into(),
            payload: Some(Payload::Idle(IdleEvent { idle })),
        }
    }

    #[tokio::test]
    async fn subscriber_receives_published_events_in_order() {
        let bus = EventBus::new(8);
        let mut sub = bus.subscribe();
        bus.publish(idle_event(true));
        bus.publish(idle_event(false));
        assert_eq!(sub.recv().await, idle_event(true));
        assert_eq!(sub.recv().await, idle_event(false));
    }

    #[tokio::test]
    async fn slow_subscriber_drops_are_counted_not_blocking() {
        let bus = EventBus::new(2);
        let mut sub = bus.subscribe();
        for i in 0..10 {
            bus.publish(idle_event(i % 2 == 0));
        }
        assert_eq!(
            bus.total_dropped(),
            0,
            "not yet observed by the lagging receiver"
        );
        let _ = sub.recv().await; // triggers the Lagged catch-up
        assert!(bus.total_dropped() > 0);
    }

    #[tokio::test]
    async fn subscriber_count_tracks_subscribe_and_drop() {
        let bus = EventBus::new(4);
        assert_eq!(bus.subscriber_count(), 0);
        let sub = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        drop(sub);
        assert_eq!(bus.subscriber_count(), 0);
    }
}
