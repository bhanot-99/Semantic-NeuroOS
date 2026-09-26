// integration test: rules.md §5's no-unwrap rule is scoped to non-test code;
// clippy's restriction lints don't auto-exempt files under tests/, so this is explicit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! IT (phases.md §4.3): healthd against 8 mock components from testkit —
//! healthy, slow (> timeout), crashing, returning malformed frames (2 of
//! each) — proving DOWN/OK classification end to end, not just per-function.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use neuroos_healthd::aggregate::Aggregate;
use neuroos_healthd::targets::Target;
use neuroos_proto::v1::Status;
use neuroos_testkit::health_mocks;

fn target(dir: &std::path::Path, name: &str) -> Target {
    Target {
        name: name.to_string(),
        socket: dir.join(format!("{name}.sock")),
        budget_bytes: 100 * 1024 * 1024,
        cgroup_path: None,
    }
}

fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[tokio::test]
async fn healthd_classifies_all_8_mock_components_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let uid = current_uid();

    let healthy_a = target(dir.path(), "healthy-a");
    let healthy_b = target(dir.path(), "healthy-b");
    let slow_a = target(dir.path(), "slow-a");
    let slow_b = target(dir.path(), "slow-b");
    let crashing_a = target(dir.path(), "crashing-a");
    let crashing_b = target(dir.path(), "crashing-b");
    let malformed_a = target(dir.path(), "malformed-a");
    let malformed_b = target(dir.path(), "malformed-b");

    health_mocks::spawn_healthy(&healthy_a.socket, uid, "a v1");
    health_mocks::spawn_healthy(&healthy_b.socket, uid, "b v1");
    health_mocks::spawn_slow(&slow_a.socket, Duration::from_secs(10));
    health_mocks::spawn_slow(&slow_b.socket, Duration::from_secs(10));
    health_mocks::spawn_crashing(&crashing_a.socket);
    health_mocks::spawn_crashing(&crashing_b.socket);
    health_mocks::spawn_malformed(&malformed_a.socket);
    health_mocks::spawn_malformed(&malformed_b.socket);
    // give every mock a moment to bind before scraping
    tokio::time::sleep(Duration::from_millis(100)).await;

    let targets: Vec<Target> = vec![
        healthy_a, healthy_b, slow_a, slow_b, crashing_a, crashing_b, malformed_a, malformed_b,
    ];
    let aggregate = Arc::new(Aggregate::new(&targets));

    // per-target timeout well under the "slow" mocks' 10s hang, so they
    // classify as DOWN within this one cycle rather than blocking it.
    neuroos_healthd::scrape_cycle(&targets, &aggregate, None, Duration::from_millis(300), true).await;

    let snapshot = aggregate.snapshot();
    assert_eq!(snapshot.len(), 8);

    let status_of = |name: &str| snapshot.iter().find(|r| r.name == name).unwrap().status;

    assert_eq!(status_of("healthy-a"), Status::Ok);
    assert_eq!(status_of("healthy-b"), Status::Ok);
    assert_eq!(status_of("slow-a"), Status::Down);
    assert_eq!(status_of("slow-b"), Status::Down);
    assert_eq!(status_of("crashing-a"), Status::Down);
    assert_eq!(status_of("crashing-b"), Status::Down);
    assert_eq!(status_of("malformed-a"), Status::Down);
    assert_eq!(status_of("malformed-b"), Status::Down);
}
