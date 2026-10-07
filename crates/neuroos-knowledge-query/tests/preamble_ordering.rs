//! P5-S02 / FR-KNO-02: preamble request must precede retrieval work
//! (Architecture.md §6.1's hot-path sequence). Uses mock `voice.sock` +
//! mock `storage.sock` (`neuroos_testkit`, C2/C6 mocked per phases.md
//! §8.3), so this needs no real models and isn't `#[ignore]`d.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
use neuroos_common::current_uid;
use std::time::Duration;

use neuroos_knowledge_query::distill::DistillationCache;
use neuroos_knowledge_query::inference_client::InferenceClient;
use neuroos_knowledge_query::kernel_client::KernelClient;
use neuroos_knowledge_query::orchestrate::{AskError, NO_EVIDENCE_ANSWER, ask};
use neuroos_knowledge_query::storage_client::StorageClient;
use neuroos_knowledge_query::voice_client::VoiceClient;
use neuroos_testkit::{storage_mocks, voice_mocks};

#[tokio::test]
async fn preamble_is_requested_before_focus_history_is_queried() {
    let sock_dir = tempfile::tempdir().unwrap();
    let voice_sock = sock_dir.path().join("voice.sock");
    let storage_sock = sock_dir.path().join("storage.sock");
    let my_uid = current_uid();

    let preamble_at = voice_mocks::spawn_preamble_recorder(&voice_sock, my_uid);
    let retrieval_at = storage_mocks::spawn_focus_history_recorder(&storage_sock, my_uid);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // D2: drives `ask`, the real hot path, rather than the `ask_context`
    // duplicate this test used to call -- which meant the ordering proof
    // never covered the code that actually runs. The mock C3 only answers
    // the focus-history RPC, so evidence retrieval fails and `ask` returns
    // an error; the recorded timestamps are what this test is about.
    let voice = VoiceClient::new(voice_sock);
    let storage = StorageClient::new(storage_sock);
    let _ = ask(
        &voice,
        &storage,
        &InferenceClient::new(sock_dir.path().join("nonexistent-inference.sock")),
        &KernelClient::new(sock_dir.path().join("nonexistent-kernel.sock")),
        &DistillationCache::new(),
        "what was that recipe?",
        1_000_000_000,
    )
    .await;

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

/// H9 / rules.md §5.6 (fail soft): an unreachable C2 must not stop
/// context resolution -- the preamble is best-effort, the answer isn't.
#[tokio::test]
async fn an_unreachable_voice_service_does_not_stop_context_resolution() {
    let sock_dir = tempfile::tempdir().unwrap();
    let storage_sock = sock_dir.path().join("storage.sock");
    let retrieval_at = storage_mocks::spawn_focus_history_recorder(&storage_sock, current_uid());
    tokio::time::sleep(Duration::from_millis(50)).await;

    let voice = VoiceClient::new(sock_dir.path().join("nonexistent-voice.sock"));
    let storage = StorageClient::new(storage_sock);
    let _ = ask(
        &voice,
        &storage,
        &InferenceClient::new(sock_dir.path().join("nonexistent-inference.sock")),
        &KernelClient::new(sock_dir.path().join("nonexistent-kernel.sock")),
        &DistillationCache::new(),
        "what was that recipe?",
        0,
    )
    .await;

    // The point: C2 being down did not stop C5 from resolving context.
    assert!(
        retrieval_at.lock().unwrap().is_some(),
        "focus history must still have been queried with the voice service down"
    );
}

/// H9: the full hot path with C2 down still answers (here: the
/// no-evidence decline, since the mock C3 has recorded nothing), and is
/// not reported as degraded.
#[tokio::test]
async fn ask_answers_even_when_the_voice_service_is_down() {
    let sock_dir = tempfile::tempdir().unwrap();
    let storage_sock = sock_dir.path().join("storage.sock");
    storage_mocks::spawn_empty(&storage_sock, current_uid());
    tokio::time::sleep(Duration::from_millis(50)).await;

    let result = ask(
        &VoiceClient::new(sock_dir.path().join("nonexistent-voice.sock")),
        &StorageClient::new(storage_sock),
        &InferenceClient::new(sock_dir.path().join("nonexistent-inference.sock")),
        &KernelClient::new(sock_dir.path().join("nonexistent-kernel.sock")),
        &DistillationCache::new(),
        "what was the name of that recipe?",
        1_000_000_000,
    )
    .await
    .unwrap();

    assert_eq!(result.answer, NO_EVIDENCE_ANSWER);
    assert!(!result.degraded);
    // M9: this answer is a fixed string C5 composed, not the model's
    // output, so claiming MODEL_GENERATED would be false provenance.
    assert!(
        !result
            .taint
            .contains(neuroos_taint::TaintFlags::MODEL_GENERATED),
        "the no-evidence decline never reaches C4: {:?}",
        result.taint
    );
}

/// A storage failure is still a real error (C3 is required, C2 isn't).
#[tokio::test]
async fn an_unreachable_storage_service_is_still_reported() {
    let sock_dir = tempfile::tempdir().unwrap();
    let voice = VoiceClient::new(sock_dir.path().join("nonexistent-voice.sock"));
    let storage = StorageClient::new(sock_dir.path().join("nonexistent-storage.sock"));
    let err = ask(
        &voice,
        &storage,
        &InferenceClient::new(sock_dir.path().join("nonexistent-inference.sock")),
        &KernelClient::new(sock_dir.path().join("nonexistent-kernel.sock")),
        &DistillationCache::new(),
        "what was that recipe?",
        0,
    )
    .await
    .unwrap_err();
    // `ask` runs the focus-history snap and evidence retrieval concurrently,
    // so with C3 unreachable either leg can be the one that reports first.
    assert!(
        matches!(err, AskError::FocusHistory(_) | AskError::Evidence(_)),
        "an unreachable C3 must surface as a storage-side error, got {err:?}"
    );
}
