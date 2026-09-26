// integration test: rules.md §5's no-unwrap rule is scoped to non-test code;
// clippy's restriction lints don't auto-exempt files under tests/, so this is explicit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FI (phases.md §4.3): kill a mock mid-scrape; healthd keeps running and
//! reports DOWN within one cycle.
use std::sync::Arc;
use std::time::Duration;

use neuroos_healthd::aggregate::Aggregate;
use neuroos_healthd::targets::Target;
use neuroos_health::HealthServer;
use neuroos_proto::v1::Status;

fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[tokio::test]
async fn killed_mock_is_reported_down_next_cycle_without_crashing_healthd() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("victim.sock");
    let uid = current_uid();

    let health = HealthServer::new("victim v1");
    let handle = tokio::spawn(health.serve(sock.clone(), vec![uid]));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let target = Target { name: "victim".into(), socket: sock, budget_bytes: 100 * 1024 * 1024, cgroup_path: None };
    let targets = vec![target];
    let aggregate = Arc::new(Aggregate::new(&targets));

    // Cycle 1: the mock is alive and healthy.
    neuroos_healthd::scrape_cycle(&targets, &aggregate, None, Duration::from_millis(300), true).await;
    assert_eq!(aggregate.snapshot()[0].status, Status::Ok);

    // Kill it mid-flight, as a crash would.
    handle.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Cycle 2: healthd itself must not panic or hang, and must reclassify
    // the target as DOWN within this one cycle.
    neuroos_healthd::scrape_cycle(&targets, &aggregate, None, Duration::from_millis(300), false).await;
    assert_eq!(aggregate.snapshot()[0].status, Status::Down);

    // And healthd keeps working afterward — not stuck in a bad state.
    neuroos_healthd::scrape_cycle(&targets, &aggregate, None, Duration::from_millis(300), false).await;
    assert_eq!(aggregate.snapshot()[0].status, Status::Down);
}
