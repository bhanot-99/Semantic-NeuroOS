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

#[tokio::main]
async fn main() {
    if neuroos_common::init_logging().is_err() {
        // logging isn't up yet; this is the one place a bare eprintln is
        // acceptable (rules.md §4 governs *services*, not this one line).
        eprintln!("neuroos-monitor: logging already initialized (unexpected)");
    }

    let args = Args::parse();

    if let Some(Command::Anonymize { input, output }) = args.command {
        match anonymise::anonymise_file(&input, &output).await {
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

        if let Ok(Some(sample)) = tokio::task::spawn_blocking(move || sampler.sample())
            .await
            .unwrap_or(Ok(None))
        {
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
        // sampler was moved into the closure above; recreate it for the
        // next tick (cheap: it only holds one prior tick's counters).
        sampler = sensors::proc::ResourceSampler::new();

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
