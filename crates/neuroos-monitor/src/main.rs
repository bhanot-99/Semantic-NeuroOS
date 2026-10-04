// monitor entry point [C1]
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use neuroos_monitor::bus::EventBus;
use neuroos_monitor::privacy::PrivacyState;
use neuroos_monitor::sensors;
use neuroos_monitor::{anonymise, control, current_uid, dump, stream};

/// `neuroos-monitor`: raw desktop telemetry sensors (Architecture.md
/// component C1), streamed over `monitor.sock` (phases.md §6).
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Also record the live event stream to this dump file (FR-MON-10).
    #[arg(long)]
    record: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Anonymise a recorded dump for use as a committed test fixture
    /// (phases.md §6.1 item 5). Does not start the service.
    Anonymize { input: PathBuf, output: PathBuf },
}

fn main() {
    if neuroos_common::init_logging().is_err() {
        // logging isn't up yet; this is the one place a bare eprintln is
        // acceptable (rules.md §4 governs *services*, not this one line).
        eprintln!("neuroos-monitor: logging already initialized (unexpected)");
    }

    let args = Args::parse();

    if let Some(Command::Anonymize { input, output }) = args.command {
        // An offline operator tool on files the operator names, not the
        // service: no sandbox.
        let result = runtime().block_on(anonymise::anonymise_file(&input, &output));
        match result {
            Ok(count) => {
                tracing::info!(events = count, "anonymised dump written");
            }
            Err(e) => {
                tracing::error!(error = %e, "anonymise failed");
                std::process::exit(1);
            }
        }
        return;
    }

    let config = match neuroos_common::load_config() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load config");
            std::process::exit(1);
        }
    };
    let monitor_cfg = config.monitor;

    // H15: locked in before the async runtime spawns its worker threads.
    // `--record`'s file is created first so the sandbox can grant exactly
    // that one file. Fail closed (rules.md §5.5).
    if let Some(path) = &args.record
        && let Err(e) = std::fs::File::create(path)
    {
        tracing::error!(error = %e, path = %path.display(), "failed to create the --record dump file");
        std::process::exit(1);
    }
    let policy = neuroos_monitor::sandbox_policy(&monitor_cfg, args.record.as_deref());
    if let Err(e) = neuroos_sandbox::enter(&policy, &[neuroos_common::paths::runtime_dir()]) {
        tracing::error!(error = %e, "failed to enter the Landlock sandbox");
        std::process::exit(1);
    }
    runtime().block_on(run(args, monitor_cfg));
}

fn runtime() -> tokio::runtime::Runtime {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "failed to start the async runtime");
            std::process::exit(1);
        }
    }
}

async fn run(args: Args, monitor_cfg: neuroos_common::config::MonitorConfig) {
    let my_uid = current_uid();
    let bus = EventBus::new(monitor_cfg.bus_capacity);
    let privacy = PrivacyState::new(monitor_cfg.effective_excluded_app_ids());

    // P0-S04: a health endpoint is one line.
    let health =
        neuroos_health::HealthServer::new(concat!("neuroos-monitor v", env!("CARGO_PKG_VERSION")));
    tokio::spawn(health.serve(
        neuroos_common::paths::component_health_sock("neuroos-monitor"),
        vec![my_uid],
    ));

    tokio::spawn(stream::serve(
        neuroos_common::paths::monitor_sock(),
        vec![my_uid],
        bus.clone(),
    ));
    tokio::spawn(control::serve(
        neuroos_common::paths::monitor_control_sock(),
        vec![my_uid],
        privacy.clone(),
        bus.clone(),
    ));

    tokio::spawn(sensors::wayland_cosmic::run_forever(
        bus.clone(),
        privacy.clone(),
    ));
    tokio::spawn(sensors::idle::run_forever(
        monitor_cfg.idle_timeout_s.saturating_mul(1000),
        bus.clone(),
        privacy.clone(),
    ));
    tokio::spawn(sensors::mpris::run_forever(bus.clone(), privacy.clone()));
    let folder_watches = monitor_cfg
        .folders
        .iter()
        .map(|f| sensors::folders::Watch {
            label: f.label.clone(),
            path: f.path.clone(),
        })
        .collect();
    tokio::spawn(sensors::folders::run_forever(
        folder_watches,
        bus.clone(),
        privacy.clone(),
    ));
    tokio::spawn(resource_and_focus_loop(
        Duration::from_secs(monitor_cfg.resource_sample_interval_s.max(1) as u64),
        bus.clone(),
        privacy.clone(),
    ));

    if let Some(path) = args.record {
        tokio::spawn(record_loop(path, bus.subscribe()));
    }

    // Every sensor above runs forever; park here so the process stays up.
    // A SIGTERM/SIGKILL from systemd ends the process directly rather than
    // this task ever needing to return — see ADR-0009's note on why the
    // Wayland sensors' blocking dispatch loops can't join cleanly on drop.
    std::future::pending::<()>().await;
}

/// FR-MON-04/FR-MON-05: periodic system resource sample plus a process-tree
/// snapshot rooted at whichever toplevel is currently activated (reusing
/// the one-shot [`sensors::wayland_cosmic::list_toplevels`] spike query
/// rather than threading extra state through the push-event dispatcher).
async fn resource_and_focus_loop(interval: Duration, bus: EventBus, privacy: PrivacyState) {
    let mut sampler = sensors::proc::ResourceSampler::new();
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        if privacy.is_paused(neuroos_common::now_ns()) {
            continue;
        }

        // Reading /proc/stat + /proc/meminfo is a small, fast local file
        // read (not worth a spawn_blocking hop) — and calling it directly
        // keeps `sampler` borrowed in place, so its delta-tracking state
        // (the previous tick's tick counts) actually persists across ticks.
        // (A prior version reassigned `sampler` to a fresh instance every
        // tick to work around spawn_blocking's `move` closure needing to
        // own it — that silently made sample() always see "first call"
        // and never publish. Found via this function's own test timing out.)
        if let Ok(Some(sample)) = sampler.sample() {
            bus.publish(neuroos_proto::v1::RawTelemetryEvent {
                observed_at_ns: neuroos_common::now_ns(),
                source: "proc".into(),
                payload: Some(neuroos_proto::v1::raw_telemetry_event::Payload::Resource(
                    neuroos_proto::v1::ResourceSample {
                        system_cpu_percent: sample.cpu_percent,
                        system_mem_used_bytes: sample.mem_used_bytes,
                        system_mem_total_bytes: sample.mem_total_bytes,
                    },
                )),
            });
        }

        if let Ok(Some(app_id)) = tokio::task::spawn_blocking(focused_app_id).await
            && let Ok(Some(pid)) = sensors::proc::find_pid_for_app_id(&app_id)
            && let Ok(snapshot) = sensors::proc::process_tree(pid)
            && privacy.allows(Some(&app_id), neuroos_common::now_ns())
        {
            bus.publish(neuroos_proto::v1::RawTelemetryEvent {
                observed_at_ns: neuroos_common::now_ns(),
                source: "proc".into(),
                payload: Some(neuroos_proto::v1::raw_telemetry_event::Payload::ProcTree(
                    snapshot,
                )),
            });
        }
    }
}

fn focused_app_id() -> Option<String> {
    sensors::wayland_cosmic::list_toplevels()
        .ok()?
        .into_iter()
        .find(|t| t.is_activated())?
        .app_id
}

/// FR-MON-10: `--record <file>` mirrors the live stream to a dump file.
async fn record_loop(path: PathBuf, mut sub: neuroos_monitor::bus::EventSubscriber) {
    let header = dump::Header {
        created_at_ns: neuroos_common::now_ns(),
        anonymised: false,
    };
    let mut writer = match dump::DumpWriter::create(&path, header).await {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "failed to open --record dump file");
            return;
        }
    };
    loop {
        let event = sub.recv().await;
        if let Err(e) = writer.write_event(&event).await {
            tracing::warn!(error = %e, "failed to write telemetry event to --record dump");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[tokio::test]
    async fn record_loop_writes_published_events_to_the_dump_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.bin");
        let bus = EventBus::new(4);
        let handle = tokio::spawn(record_loop(path.clone(), bus.subscribe()));

        // record_loop opens the dump file before its first recv(); give it
        // a moment, matching the same real-race handling other socket
        // tests in this crate use.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        bus.publish(neuroos_proto::v1::RawTelemetryEvent {
            observed_at_ns: 99,
            source: "test".into(),
            payload: Some(neuroos_proto::v1::raw_telemetry_event::Payload::Idle(
                neuroos_proto::v1::IdleEvent { idle: true },
            )),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.abort();

        let mut reader = dump::DumpReader::open(&path).await.unwrap();
        let event = reader.read_event().await.unwrap().unwrap();
        assert_eq!(event.observed_at_ns, 99);
    }

    #[tokio::test]
    async fn resource_and_focus_loop_publishes_a_resource_sample() {
        let bus = EventBus::new(4);
        let mut sub = bus.subscribe();
        let privacy = PrivacyState::new(Vec::new());
        let handle = tokio::spawn(resource_and_focus_loop(
            std::time::Duration::from_millis(50),
            bus,
            privacy,
        ));

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv())
            .await
            .expect("expected a resource sample within 5s");
        handle.abort();
        assert_eq!(event.source, "proc");
    }

    /// Live proof: needs a real Wayland/COSMIC session (same as
    /// wayland_cosmic's own live tests) — run manually with `cargo test -p
    /// neuroos-monitor -- --ignored focused_app_id`.
    #[test]
    #[ignore = "needs a real Wayland/COSMIC session"]
    fn focused_app_id_resolves_the_real_activated_toplevel() {
        assert!(focused_app_id().is_some());
    }
}
