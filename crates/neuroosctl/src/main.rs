//! neuroosctl entry point (Architecture.md §6.5, PRD FR-CLI-01).
use std::time::Duration;

use clap::{Parser, Subcommand};
use neuroos_ipc::{DEFAULT_MAX_FRAME, connect, read_envelope_deadline, write_envelope_deadline};
use neuroos_proto::v1::{
    AggregateStatusRequest, ComponentStatus, Envelope, MonitorPauseRequest, MonitorStatusRequest,
    Status, envelope,
};
use serde::Serialize;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Parser)]
#[command(name = "neuroosctl", about = "NeuroOS operator CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show every component's aggregate health (P1-S04).
    Status {
        /// Emit machine-readable JSON instead of a text table.
        #[arg(long)]
        json: bool,
    },
    /// Stop all C1 sensing until resumed (FR-PRV-01, P3-S05).
    Pause {
        /// Pause for this many seconds, then auto-resume. Omit to pause
        /// indefinitely (until an explicit `neuroosctl resume`).
        duration: Option<u64>,
    },
    /// Resume C1 sensing after a `pause` (FR-PRV-01, P3-S05).
    Resume,
    /// Show C1's pause state, exclusion list and event-bus counters.
    MonitorStatus,
}

#[derive(Serialize, Debug)]
struct StatusRow {
    name: String,
    status: &'static str,
    rss_bytes: u64,
    budget_bytes: u64,
    uptime_s: u64,
    p50_ms: Option<f64>,
    p99_ms: Option<f64>,
    error_count: u64,
    last_error_at_ns: u64,
}

#[derive(Serialize, Debug)]
struct StatusReport {
    generated_at_ns: u64,
    components: Vec<StatusRow>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let exit_code = match cli.command {
        Commands::Status { json } => run_status(json).await,
        Commands::Pause { duration } => run_pause(duration, false).await,
        Commands::Resume => run_pause(None, true).await,
        Commands::MonitorStatus => run_monitor_status().await,
    };
    std::process::exit(exit_code);
}

async fn monitor_control_request(body: envelope::Body) -> Result<Envelope, String> {
    let socket_path = neuroos_common::paths::monitor_control_sock();
    let mut stream = connect(&socket_path, REQUEST_TIMEOUT)
        .await
        .map_err(|e| format!("connect {}: {e}", socket_path.display()))?;
    let request = Envelope {
        schema_version: 1,
        trace_id: String::new(),
        request_id: 0,
        sent_at_ns: neuroos_common::now_ns(),
        body: Some(body),
    };
    write_envelope_deadline(&mut stream, &request, DEFAULT_MAX_FRAME, REQUEST_TIMEOUT)
        .await
        .map_err(|e| format!("write request: {e}"))?;
    read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, REQUEST_TIMEOUT)
        .await
        .map_err(|e| format!("read response: {e}"))?
        .ok_or_else(|| "neuroos-monitor closed the connection with no response".to_string())
}

async fn run_pause(duration: Option<u64>, resume: bool) -> i32 {
    let response =
        monitor_control_request(envelope::Body::MonitorPauseRequest(MonitorPauseRequest {
            duration_s: duration.unwrap_or(0),
            resume,
        }))
        .await;
    match response {
        Ok(Envelope {
            body: Some(envelope::Body::MonitorPauseResponse(r)),
            ..
        }) => {
            if r.paused {
                match r.paused_until_ns {
                    u64::MAX => println!("monitor paused indefinitely"),
                    ns => println!(
                        "monitor paused for {}s",
                        ns.saturating_sub(neuroos_common::now_ns()) / 1_000_000_000
                    ),
                }
            } else {
                println!("monitor resumed");
            }
            0
        }
        Ok(other) => {
            eprintln!("neuroosctl: unexpected response: {other:?}");
            1
        }
        Err(e) => {
            eprintln!("neuroosctl: could not reach neuroos-monitor: {e}");
            1
        }
    }
}

async fn run_monitor_status() -> i32 {
    let response = monitor_control_request(envelope::Body::MonitorStatusRequest(
        MonitorStatusRequest {},
    ))
    .await;
    match response {
        Ok(Envelope {
            body: Some(envelope::Body::MonitorStatusResponse(r)),
            ..
        }) => {
            println!("paused: {}", r.paused);
            if r.paused {
                match r.paused_until_ns {
                    u64::MAX => println!("paused_until: indefinite"),
                    ns => println!("paused_until_ns: {ns}"),
                }
            }
            println!("excluded_app_ids: {}", r.excluded_app_ids.join(", "));
            println!("subscribers: {}", r.subscriber_count);
            println!("events_dropped_total: {}", r.events_dropped_total);
            0
        }
        Ok(other) => {
            eprintln!("neuroosctl: unexpected response: {other:?}");
            1
        }
        Err(e) => {
            eprintln!("neuroosctl: could not reach neuroos-monitor: {e}");
            1
        }
    }
}

async fn run_status(json: bool) -> i32 {
    let report = match fetch_status().await {
        Ok(report) => report,
        Err(e) => {
            eprintln!("neuroosctl: could not reach healthd: {e}");
            return 1;
        }
    };

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("neuroosctl: failed to encode JSON: {e}");
                return 1;
            }
        }
    } else {
        print_table(&report);
    }
    0
}

async fn fetch_status() -> Result<StatusReport, String> {
    let socket_path = neuroos_common::paths::healthd_sock();
    let mut stream = connect(&socket_path, REQUEST_TIMEOUT)
        .await
        .map_err(|e| format!("connect {}: {e}", socket_path.display()))?;

    let request = Envelope {
        schema_version: 1,
        trace_id: String::new(),
        request_id: 0,
        sent_at_ns: neuroos_common::now_ns(),
        body: Some(envelope::Body::AggregateStatusRequest(
            AggregateStatusRequest {},
        )),
    };
    write_envelope_deadline(&mut stream, &request, DEFAULT_MAX_FRAME, REQUEST_TIMEOUT)
        .await
        .map_err(|e| format!("write request: {e}"))?;

    let response = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, REQUEST_TIMEOUT)
        .await
        .map_err(|e| format!("read response: {e}"))?
        .ok_or_else(|| "healthd closed the connection with no response".to_string())?;

    match response.body {
        Some(envelope::Body::AggregateStatusResponse(resp)) => Ok(StatusReport {
            generated_at_ns: resp.generated_at_ns,
            components: resp.components.iter().map(to_row).collect(),
        }),
        other => Err(format!("unexpected response body: {other:?}")),
    }
}

fn to_row(c: &ComponentStatus) -> StatusRow {
    // Deterministic pick among possibly-several named histograms: the one
    // sorted first by name, so repeated calls against the same snapshot
    // always report the same figure.
    let primary_hist = c
        .latency_histograms
        .iter()
        .min_by_key(|(name, _)| name.as_str())
        .map(|(_, h)| h);
    let p50_ms = primary_hist
        .and_then(neuroos_health::p50_ns)
        .map(|ns| ns as f64 / 1_000_000.0);
    let p99_ms = primary_hist
        .and_then(neuroos_health::p99_ns)
        .map(|ns| ns as f64 / 1_000_000.0);

    StatusRow {
        name: c.name.clone(),
        status: status_label(c.status),
        rss_bytes: c.rss_bytes,
        budget_bytes: c.budget_bytes,
        uptime_s: c.uptime_s,
        p50_ms,
        p99_ms,
        error_count: c.error_counters.values().sum(),
        last_error_at_ns: c.last_error_at_ns,
    }
}

fn status_label(status: i32) -> &'static str {
    match Status::try_from(status).unwrap_or(Status::Unknown) {
        Status::Ok => "OK",
        Status::Degraded => "DEGRADED",
        Status::Down => "DOWN",
        Status::Unknown | Status::Unspecified => "UNKNOWN",
    }
}

fn print_table(report: &StatusReport) {
    println!(
        "{:<28} {:<9} {:>10} {:>10} {:>8} {:>8} {:>8} {:>7}",
        "COMPONENT", "STATUS", "RSS_MIB", "BUDGET_MIB", "UPTIME_S", "P50_MS", "P99_MS", "ERRORS"
    );
    for row in &report.components {
        let rss_mib = row.rss_bytes as f64 / (1024.0 * 1024.0);
        let budget_mib = row.budget_bytes as f64 / (1024.0 * 1024.0);
        let p50 = row
            .p50_ms
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "-".to_string());
        let p99 = row
            .p99_ms
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<28} {:<9} {:>10.1} {:>10.1} {:>8} {:>8} {:>8} {:>7}",
            row.name, row.status, rss_mib, budget_mib, row.uptime_s, p50, p99, row.error_count
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use std::collections::HashMap;
    use std::sync::Arc;

    use neuroos_healthd::aggregate::Aggregate;
    use neuroos_healthd::targets::Target;
    use neuroos_proto::v1::LatencyHistogram;

    use super::*;

    fn target(name: &str) -> Target {
        Target {
            name: name.to_string(),
            socket: std::path::PathBuf::from("/tmp/does-not-matter.sock"),
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

    #[test]
    fn status_label_covers_every_status() {
        assert_eq!(status_label(Status::Ok as i32), "OK");
        assert_eq!(status_label(Status::Degraded as i32), "DEGRADED");
        assert_eq!(status_label(Status::Down as i32), "DOWN");
        assert_eq!(status_label(Status::Unknown as i32), "UNKNOWN");
        assert_eq!(status_label(999), "UNKNOWN");
    }

    #[test]
    fn to_row_picks_the_lexicographically_first_histogram_deterministically() {
        let mut histograms = HashMap::new();
        histograms.insert(
            "z_op".to_string(),
            LatencyHistogram {
                bucket_upper_bound_ns: vec![1_000_000],
                bucket_counts: vec![1],
                count: 1,
                sum_ns: 1_000_000,
            },
        );
        histograms.insert(
            "a_op".to_string(),
            LatencyHistogram {
                bucket_upper_bound_ns: vec![2_000_000],
                bucket_counts: vec![1],
                count: 1,
                sum_ns: 2_000_000,
            },
        );
        let status = ComponentStatus {
            name: "comp".into(),
            status: Status::Ok as i32,
            rss_bytes: 1024,
            budget_bytes: 2048,
            uptime_s: 10,
            latency_histograms: histograms,
            error_counters: HashMap::new(),
            last_error_at_ns: 0,
            build_info: String::new(),
        };
        let row = to_row(&status);
        // "a_op" sorts before "z_op": its 2ms bucket must be the one picked.
        assert_eq!(row.p50_ms, Some(2.0));
    }

    #[test]
    fn to_row_sums_error_counters() {
        let mut errors = HashMap::new();
        errors.insert("timeout".to_string(), 3u64);
        errors.insert("refused".to_string(), 4u64);
        let status = ComponentStatus {
            name: "comp".into(),
            status: Status::Ok as i32,
            rss_bytes: 0,
            budget_bytes: 0,
            uptime_s: 0,
            latency_histograms: HashMap::new(),
            error_counters: errors,
            last_error_at_ns: 0,
            build_info: String::new(),
        };
        assert_eq!(to_row(&status).error_count, 7);
    }

    #[tokio::test]
    async fn fetch_status_round_trips_against_a_real_healthd_sock_server() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test process (nextest runs each test in
        // its own process); no other thread reads env vars concurrently.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", dir.path());
        }
        let sock = neuroos_common::paths::healthd_sock();
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

        let targets = vec![target("neuroos-monitor")];
        let aggregate = Arc::new(Aggregate::new(&targets));
        let my_uid = current_uid();
        tokio::spawn(neuroos_healthd::server::serve(
            aggregate,
            sock,
            vec![my_uid],
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let report = fetch_status().await.unwrap();
        assert_eq!(report.components.len(), 1);
        assert_eq!(report.components[0].name, "neuroos-monitor");
        assert_eq!(report.components[0].status, "UNKNOWN");
    }

    #[tokio::test]
    async fn fetch_status_reports_a_clear_error_when_healthd_is_unreachable() {
        // SAFETY: single-threaded test process.
        unsafe {
            std::env::set_var(
                "XDG_RUNTIME_DIR",
                "/tmp/neuroosctl-test-nonexistent-runtime-dir",
            );
        }
        let err = fetch_status().await.unwrap_err();
        assert!(err.contains("connect"));
    }
}
