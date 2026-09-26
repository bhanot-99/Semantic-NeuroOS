// monitor entry point
use std::path::PathBuf;

mod sensors;

fn runtime_socket_dir() -> PathBuf {
    // Architecture.md §5.2: $XDG_RUNTIME_DIR/neuroos/ = /run/user/$UID/neuroos/
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("neuroos")
}

#[tokio::main]
async fn main() {
    let healthd_uid = unsafe { libc_getuid() }; // TODO(Phase 1): real healthd UID from config

    // P0-S04: a health endpoint is one line.
    let health =
        neuroos_health::HealthServer::new(concat!("neuroos-monitor v", env!("CARGO_PKG_VERSION")));
    tokio::spawn(health.serve(
        runtime_socket_dir().join("monitor.health.sock"),
        vec![healthd_uid],
    ));

    // Spike S-03 (P0-S08) proof, run at startup for now; Phase 3 replaces
    // this one-shot snapshot with a push-event subscription feeding
    // monitor.sock.
    match sensors::wayland_cosmic::list_toplevels() {
        Ok(toplevels) => {
            for t in &toplevels {
                tracing::debug!(app_id = ?t.app_id, title = ?t.title, activated = t.is_activated(), "toplevel");
            }
        }
        Err(e) => tracing::warn!(error = %e, "zcosmic_toplevel_info_v1 unavailable"),
    }
}

/// # Safety
/// `getuid()` takes no arguments and cannot fail.
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}
