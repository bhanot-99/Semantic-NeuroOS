// rules.md §5 exempts these lints for one-off diagnostic tools: crashing loudly
// on unexpected input IS the correct behavior for a spike/interop CLI tool,
// unlike a long-running service.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// PF test support (phases.md §4.3): binds a healthy HealthServer at
// <runtime_dir>/<name>.health.sock for every name given on argv, then blocks
// forever so a real neuroos-healthd process can scrape them.
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let runtime_dir = PathBuf::from(args.next().expect("usage: health_mock_farm <runtime_dir> <name>..."));
    let names: Vec<String> = args.collect();
    assert!(!names.is_empty(), "usage: health_mock_farm <runtime_dir> <name>...");

    let my_uid = current_uid();
    for name in &names {
        let health = neuroos_health::HealthServer::new(format!("{name} mock"));
        let sock = runtime_dir.join(format!("{name}.health.sock"));
        tokio::spawn(health.serve(sock, vec![my_uid]));
    }

    println!("READY");
    use std::io::Write;
    std::io::stdout().flush().unwrap();

    std::future::pending::<()>().await;
}

unsafe fn libc_getuid_impl() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe { libc_getuid_impl() }
}
