//! P5-S02 / FR-KNO-02: preamble request must precede retrieval work
//! (Architecture.md §6.1's hot-path sequence). Uses mock `voice.sock` +
//! mock `storage.sock` (`neuroos_testkit`, C2/C6 mocked per phases.md
//! §8.3), so this needs no real models and isn't `#[ignore]`d.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
use std::time::Duration;

use neuroos_knowledge_query::orchestrate::{AskError, ask_context};
use neuroos_knowledge_query::storage_client::StorageClient;
use neuroos_knowledge_query::voice_client::VoiceClient;
use neuroos_testkit::{storage_mocks, voice_mocks};

fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[tokio::test]
async fn preamble_is_requested_before_focus_history_is_queried() {
    let sock_dir = tempfile::tempdir().unwrap();
    let voice_sock = sock_dir.path().join("voice.sock");
    let storage_sock = sock_dir.path().join("storage.sock");
    let my_uid = current_uid();

    let preamble_at = voice_mocks::spawn_preamble_recorder(&voice_sock, my_uid);
    let retrieval_at = storage_mocks::spawn_focus_history_recorder(&storage_sock, my_uid);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let voice = VoiceClient::new(voice_sock);
    let storage = StorageClient::new(storage_sock);
    ask_context(&voice, &storage, 1_000_000_000).await.unwrap();

    let preamble_ts = preamble_at
        .lock()
        .unwrap()
        .expect("preamble should have been requested");
    let retrieval_ts = retrieval_at
        .lock()
        .unwrap()
        .expect("focus history should have been queried");
    assert!(
        preamble_ts < retrieval_ts,
        "preamble must be requested before retrieval begins"
    );
}

#[tokio::test]
async fn preamble_failure_is_reported_not_silently_swallowed() {
    // Neither socket is bound: proves ask_context surfaces a clear error
    // when C2 is unreachable rather than hanging or silently continuing.
    let sock_dir = tempfile::tempdir().unwrap();
    let voice = VoiceClient::new(sock_dir.path().join("nonexistent-voice.sock"));
    let storage = StorageClient::new(sock_dir.path().join("nonexistent-storage.sock"));
    let err = ask_context(&voice, &storage, 0).await.unwrap_err();
    assert!(matches!(err, AskError::Preamble(_)));
}
