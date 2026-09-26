// healthd entry point
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use neuroos_healthd::{aggregate::Aggregate, current_uid, run_forever, soak::SoakEngine, targets};

/// healthd: scrapes every component's health socket, aggregates status,
/// serves `healthd.sock` for `neuroosctl status` (Architecture.md §6.5).
#[derive(Parser)]
struct Args {
    /// Capture a soak baseline on the first scrape cycle, then flag RSS
    /// growth > 5% or p99 drift > 10% against it (PRD FR-HLT-04).
    #[arg(long)]
    soak: bool,
}

#[tokio::main]
async fn main() {
    if neuroos_common::init_logging().is_err() {
        // logging isn't up yet; this is the one place a bare eprintln is
        // acceptable (rules.md §4 governs *services*, not this one line).
        eprintln!("neuroos-healthd: logging already initialized (unexpected)");
    }

    let args = Args::parse();

    let config = match neuroos_common::load_config() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load config");
            std::process::exit(1);
        }
    };

    let all_targets = targets::merge_targets(targets::default_targets(), &config.healthd.extra_targets);
    let aggregate = Arc::new(Aggregate::new(&all_targets));

    let my_uid = current_uid();
    let socket_path = neuroos_common::paths::healthd_sock();
    tokio::spawn(neuroos_healthd::server::serve(aggregate.clone(), socket_path, vec![my_uid]));

    let soak_engine =
        args.soak.then(|| Arc::new(SoakEngine::new(neuroos_common::paths::soak_dir().join("healthd-soak.csv"))));

    let poll_interval = Duration::from_secs(config.healthd.poll_interval_s.max(1));
    let per_target_timeout = Duration::from_secs(config.healthd.per_target_timeout_s.max(1));

    run_forever(all_targets, aggregate, soak_engine, poll_interval, per_target_timeout).await;
}
