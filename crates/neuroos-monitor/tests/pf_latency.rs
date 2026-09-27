#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code; this whole file is a test
//! PF (phases.md §6.3): capture latency p99 < 1.5 ms, "compositor event
//! timestamp → socket write".
//!
//! No Wayland protocol gives us the compositor's own event timestamp
//! (`observed_at_ns` is stamped by our own dispatch handler at the instant
//! we see the event), so this measures the part of that budget C1 actually
//! controls end to end: from `EventBus::publish` through a real subscriber
//! task to a completed `write_frame` on a real socketpair — bus dispatch,
//! protobuf encoding and the length-prefixed write. Kernel/compositor
//! scheduling latency before our process observes the event isn't ours to
//! benchmark deterministically (same honest scoping healthd_pf.sh uses for
//! its unreachable 9th target).
use neuroos_ipc::framing::{DEFAULT_MAX_FRAME, write_frame};
use neuroos_monitor::bus::EventBus;
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::{IdleEvent, RawTelemetryEvent};
use prost::Message;

const SAMPLES: usize = 2_000;

#[tokio::test(flavor = "multi_thread")]
async fn capture_latency_p99_is_under_1_5ms() {
    let bus = EventBus::new(8);
    let mut sub = bus.subscribe();

    // Real subscriber pipeline: decode timestamp back out, encode+frame it
    // to a real socketpair, exactly like stream.rs's handle_subscriber.
    let (mut a, mut b) = tokio::net::UnixStream::pair().expect("socketpair");
    let drainer = tokio::spawn(async move {
        let mut buf = [0u8; 65536];
        loop {
            use tokio::io::AsyncReadExt;
            if b.read(&mut buf).await.unwrap_or(0) == 0 {
                return;
            }
        }
    });

    // One event at a time, end to end, rather than a tight-loop burst: a
    // burst measures queueing delay under sustained load, not per-event
    // capture latency, and real desktop events (focus changes, MPRIS
    // updates) are sporadic, not bursty.
    let mut latencies_ns = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = neuroos_common::now_ns();
        bus.publish(RawTelemetryEvent {
            observed_at_ns: t0,
            source: "pf_bench".into(),
            payload: Some(Payload::Idle(IdleEvent { idle: true })),
        });
        let event = sub.recv().await;
        let buf = event.encode_to_vec();
        write_frame(&mut a, &buf, DEFAULT_MAX_FRAME)
            .await
            .expect("write_frame");
        latencies_ns.push(neuroos_common::now_ns().saturating_sub(t0));
    }
    drainer.abort();

    latencies_ns.sort_unstable();
    let p50 = latencies_ns[latencies_ns.len() / 2];
    let p99 = latencies_ns[latencies_ns.len() * 99 / 100];
    eprintln!(
        "capture pipeline latency over {SAMPLES} samples: p50={:.3}ms p99={:.3}ms",
        p50 as f64 / 1_000_000.0,
        p99 as f64 / 1_000_000.0
    );
    assert!(
        p99 < 1_500_000,
        "p99 pipeline latency {:.3}ms exceeds the 1.5ms budget",
        p99 as f64 / 1_000_000.0
    );
}
