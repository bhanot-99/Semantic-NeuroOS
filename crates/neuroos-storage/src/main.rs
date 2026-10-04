// storage engine entry point [C3]
use std::sync::Arc;

use tokio::sync::Mutex;

fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// `models_dir/onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so`
/// (models/manifest.toml's `onnxruntime-linux-x64` entry: `extract_to =
/// "onnxruntime"`, `verify_file` names this exact relative path).
fn onnxruntime_dylib_path(models_dir: &std::path::Path) -> std::path::PathBuf {
    models_dir.join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so")
}

fn main() {
    if neuroos_common::init_logging().is_err() {
        // logging isn't up yet; this is the one place a bare eprintln is
        // acceptable (rules.md §4 governs *services*, not this one line).
        eprintln!("neuroos-storage: logging already initialized (unexpected)");
    }

    let config = match neuroos_common::load_config() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load config");
            std::process::exit(1);
        }
    };

    // H15: locked in before the async runtime spawns its worker threads.
    // Fail closed (rules.md §5.5).
    let policy = neuroos_storage::sandbox_policy(&config.storage);
    // LanceDB's query spill files go to the process temp dir; keep them
    // inside the store (the sandbox doesn't grant /tmp, and they hold
    // user data anyway).
    let scratch = neuroos_storage::scratch_dir();
    if let Err(e) = tempfile::env::override_temp_dir(&scratch) {
        tracing::error!(path = %e.display(), "temp dir was already overridden");
        std::process::exit(1);
    }
    let dirs = [
        neuroos_common::paths::runtime_dir(),
        neuroos_common::paths::storage_dir(),
        neuroos_common::paths::backups_dir(),
        scratch,
    ];
    if let Err(e) = neuroos_sandbox::enter(&policy, &dirs) {
        tracing::error!(error = %e, "failed to enter the Landlock sandbox");
        std::process::exit(1);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "failed to start the async runtime");
            std::process::exit(1);
        }
    };
    runtime.block_on(run(config));
}

async fn run(config: neuroos_common::Config) {
    let my_uid = current_uid();
    let sqlite_path = neuroos_common::paths::storage_dir().join("meta.sqlite3");
    let lance_path = neuroos_common::paths::storage_dir().join("lance");
    let onnxruntime_dylib = onnxruntime_dylib_path(&config.storage.models_dir);

    let engine = match neuroos_storage::engine::StorageEngine::open(
        &sqlite_path,
        &lance_path,
        &config.storage.models_dir,
        &onnxruntime_dylib,
    )
    .await
    {
        Ok(e) => Arc::new(Mutex::new(e)),
        Err(e) => {
            // Fatal per rules.md §5.4: a bad model/config means C3 cannot
            // serve anything real, so systemd should restart or stop us
            // rather than run degraded with no data path.
            tracing::error!(error = %e, "failed to open StorageEngine");
            std::process::exit(1);
        }
    };

    // P0-S04 convention: a health endpoint is one line.
    let health =
        neuroos_health::HealthServer::new(concat!("neuroos-storage v", env!("CARGO_PKG_VERSION")));
    tokio::spawn(health.serve(
        neuroos_common::paths::component_health_sock("neuroos-storage"),
        vec![my_uid],
    ));

    neuroos_storage::server::run_service(
        engine,
        neuroos_storage::server::ServicePaths {
            storage_sock: neuroos_common::paths::storage_sock(),
            monitor_sock: neuroos_common::paths::monitor_sock(),
            spool_dir: config.storage.spool_dir.clone(),
            backups_dir: neuroos_common::paths::backups_dir(),
        },
        vec![my_uid],
    )
    .await;
}
