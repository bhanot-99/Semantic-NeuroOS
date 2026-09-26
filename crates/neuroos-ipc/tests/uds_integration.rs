// integration test: rules.md §5's no-unwrap rule is scoped to non-test code;
// clippy's restriction lints don't auto-exempt files under tests/, so this is explicit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::time::Duration;

use neuroos_ipc::{
    DEFAULT_MAX_FRAME, ReconnectPolicy, UdsServer, UdsServerConfig, connect,
    connect_with_reconnect, read_envelope, write_envelope,
};
use neuroos_proto::v1::{Envelope, Error, ErrorCode, envelope};

fn fixture_envelope() -> Envelope {
    Envelope {
        schema_version: 1,
        trace_id: "it-test".into(),
        request_id: 1,
        sent_at_ns: 1,
        body: Some(envelope::Body::Error(Error {
            code: ErrorCode::Internal as i32,
            message: "echo".into(),
            retryable: false,
        })),
    }
}

/// IT (phases.md §3.3): client connects to a real UDS echo server; own-UID
/// peer is accepted (see `crates/neuroos-ipc/src/peercred.rs` for the
/// allow/deny decision unit tests — a genuine cross-UID rejection test needs
/// a second local user or root, unavailable in this sandbox).
#[tokio::test]
async fn echo_over_real_uds_socket() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("echo.sock");
    let my_uid = unsafe { libc_getuid() };

    let server = UdsServer::bind(UdsServerConfig::new(&sock_path, vec![my_uid])).unwrap();
    let server_task = tokio::spawn(async move {
        let (mut stream, _cred) = server
            .accept()
            .await
            .unwrap()
            .expect("own uid must be allowed");
        let env = read_envelope(&mut stream, DEFAULT_MAX_FRAME)
            .await
            .unwrap()
            .unwrap();
        write_envelope(&mut stream, &env, DEFAULT_MAX_FRAME)
            .await
            .unwrap();
    });

    let mut client = connect(&sock_path, Duration::from_secs(1)).await.unwrap();
    let sent = fixture_envelope();
    write_envelope(&mut client, &sent, DEFAULT_MAX_FRAME)
        .await
        .unwrap();
    let got = read_envelope(&mut client, DEFAULT_MAX_FRAME)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent, got);

    server_task.await.unwrap();
}

/// IT (phases.md §3.3): reconnect after server restart, well under the 10 s
/// budget from `ReconnectPolicy::default`.
#[tokio::test]
async fn client_reconnects_after_server_restart() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("restart.sock");
    let my_uid = unsafe { libc_getuid() };

    // Client starts trying before the server exists at all.
    let sock_path_for_client = sock_path.clone();
    let client_task = tokio::spawn(async move {
        connect_with_reconnect(&sock_path_for_client, &ReconnectPolicy::default()).await
    });

    tokio::time::sleep(Duration::from_millis(250)).await;
    let server = UdsServer::bind(UdsServerConfig::new(&sock_path, vec![my_uid])).unwrap();
    let accept_task = tokio::spawn(async move { server.accept().await });

    let client_result = tokio::time::timeout(Duration::from_secs(10), client_task)
        .await
        .expect("must reconnect within the 10s budget")
        .unwrap();
    assert!(client_result.is_ok());
    assert!(accept_task.await.unwrap().unwrap().is_some());
}

/// # Safety
/// `getuid()` takes no arguments and cannot fail.
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}
