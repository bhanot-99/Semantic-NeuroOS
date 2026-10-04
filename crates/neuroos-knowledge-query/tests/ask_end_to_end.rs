//! P5-S03/S05 end-to-end: **Real C3 + real C4 + mock C2/C6**, matching
//! phases.md §8.3's IT requirement verbatim -- a real focus session and a
//! real embedded chunk in a real `StorageEngine`, a real running
//! `neuroos-inference` generating a real completion through a real memfd
//! ring, and mock `voice.sock`/`kernel.sock` servers standing in for C2/C6
//! (both land in later phases).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use neuroos_knowledge_query::inference_client::InferenceClient;
use neuroos_knowledge_query::kernel_client::KernelClient;
use neuroos_knowledge_query::orchestrate;
use neuroos_knowledge_query::storage_client::StorageClient;
use neuroos_knowledge_query::voice_client::VoiceClient;
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::window_event::Kind;
use neuroos_proto::v1::{
    RawTelemetryEvent, ToplevelState, WindowEvent, WindowOpened, WindowStateChanged,
};
use neuroos_storage::engine::StorageEngine;
use neuroos_testkit::{kernel_mocks, voice_mocks};
use tokio::sync::Mutex;

fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn dev_models_dir() -> PathBuf {
    repo_root().join(".dev-cache/models")
}

fn dev_onnxruntime_dylib() -> PathBuf {
    // M16: a test is its own `main`, so it pins the ONNX Runtime
    // dylib the way `main` does, before `Embedder::load` can be
    // reached; harmless to repeat, an error only on a conflict.
    let path =
        dev_models_dir().join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so");
    let _ = neuroos_storage::embed::Embedder::set_dylib_path(&path);
    path
}

struct InferenceProcess {
    child: Child,
    tmp: tempfile::TempDir,
}

impl Drop for InferenceProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_real_inference() -> Option<InferenceProcess> {
    let model_path = repo_root().join(".dev-cache/models/bitnet-b1.58-2B-4T/ggml-model-i2_s.gguf");
    let bin = repo_root().join("cpp/build/neuroos-inference/neuroos-inference");
    if !model_path.exists() || !bin.exists() {
        eprintln!(
            "skipping: model ({}) or binary ({}) not found",
            model_path.display(),
            bin.display()
        );
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("run");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[inference]\nmodel_path = \"{}\"\nthreads = 8\nmax_context_tokens = 512\n",
            model_path.display()
        ),
    )
    .unwrap();
    let child = Command::new(&bin)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("NEUROOS_CONFIG", &config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn real neuroos-inference");
    Some(InferenceProcess { child, tmp })
}

fn window_event(toplevel_id: u64, at_ns: u64, kind: Kind) -> RawTelemetryEvent {
    RawTelemetryEvent {
        observed_at_ns: at_ns,
        source: "wayland_cosmic".into(),
        payload: Some(Payload::Window(WindowEvent {
            toplevel_id,
            kind: Some(kind),
        })),
    }
}

/// Live proof: needs the real BitNet model + built `cpp/neuroos-inference`
/// (as `tests/inference_generate.rs` does) *and* the real embedder models
/// fetched into `.dev-cache/models` (as `neuroos-storage`'s own live tests
/// do) -- `#[ignore]`d, `cargo test -p neuroos-knowledge-query --test
/// ask_end_to_end -- --ignored`.
#[tokio::test]
#[ignore = "needs the real BitNet model, fetched embedder models, and built cpp/neuroos-inference; see doc comment"]
async fn ask_end_to_end_real_c3_real_c4_mock_c2_c6() {
    let Some(inference_proc) = spawn_real_inference() else {
        return;
    };
    let inference_sock = inference_proc.tmp.path().join("run/neuroos/inference.sock");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !inference_sock.exists() {
        if std::time::Instant::now() > deadline {
            panic!("inference.sock never appeared");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let sqlite_dir = tempfile::tempdir().unwrap();
    let lance_dir = tempfile::tempdir().unwrap();
    let mut engine = StorageEngine::open(
        &sqlite_dir.path().join("meta.sqlite3"),
        lance_dir.path(),
        &dev_models_dir(),
        &dev_onnxruntime_dylib(),
    )
    .await
    .expect("real model + onnxruntime should load");

    engine
        .ingest(&window_event(
            1,
            0,
            Kind::Opened(WindowOpened {
                app_id: "org.mozilla.firefox".into(),
                title: "quarterly revenue dashboard".into(),
                pid: 0,
                pid_known: false,
            }),
        ))
        .await
        .unwrap();
    engine
        .ingest(&window_event(
            1,
            0,
            Kind::StateChanged(WindowStateChanged {
                states: vec![ToplevelState::Activated as i32],
            }),
        ))
        .await
        .unwrap();
    engine
        .ingest(&window_event(
            1,
            6_000_000_000,
            Kind::StateChanged(WindowStateChanged { states: vec![] }),
        ))
        .await
        .unwrap();

    let storage_sock_dir = tempfile::tempdir().unwrap();
    let storage_sock = storage_sock_dir.path().join("storage.sock");
    let voice_sock_dir = tempfile::tempdir().unwrap();
    let voice_sock = voice_sock_dir.path().join("voice.sock");
    let kernel_sock_dir = tempfile::tempdir().unwrap();
    let kernel_sock = kernel_sock_dir.path().join("kernel.sock");
    let my_uid = current_uid();

    voice_mocks::spawn_preamble_recorder(&voice_sock, my_uid);
    kernel_mocks::spawn_always_approve(&kernel_sock, my_uid);

    tokio::select! {
        _ = neuroos_storage::server::serve(Arc::new(Mutex::new(engine)), storage_sock.clone(), vec![my_uid]) => {
            panic!("storage.sock server exited unexpectedly");
        }
        _ = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let voice = VoiceClient::new(voice_sock);
            let storage = StorageClient::new(storage_sock);
            let inference = InferenceClient::new(inference_sock);
            let kernel = KernelClient::new(kernel_sock);
            let distill_cache = neuroos_knowledge_query::distill::DistillationCache::new();

            // Spoke at t=3s (mid-session) about "revenue" -- both the
            // deictic snap and the evidence retrieval should find the
            // real ingested/embedded firefox window.
            let result = orchestrate::ask(
                &voice,
                &storage,
                &inference,
                &kernel,
                &distill_cache,
                "what does the revenue dashboard say",
                3_000_000_000,
            )
            .await
            .unwrap();

            assert!(!result.degraded, "a SAFE-tier generate must not be degraded when C6 always approves");
            assert!(
                !result.answer.trim().is_empty(),
                "must have produced a real generated answer"
            );
            // M9 / Architecture.md §7.4: the answer text is C4's output,
            // so it carries MODEL_GENERATED on top of its evidence's
            // taint. Nothing used to set it, so C5's answers claimed a
            // provenance they did not have.
            assert!(
                result
                    .taint
                    .contains(neuroos_taint::TaintFlags::MODEL_GENERATED),
                "a real generated answer must be marked MODEL_GENERATED, got {:?}",
                result.taint
            );
            // FR-KNO-09: real, measured own-compute over the full real
            // C3+C4 path (not a mock) -- a single sample here plus
            // `assemble::tests::own_compute_...` (fast, no-IPC, n=200) for
            // the actual p99 statistic. Repeating *this* real call several
            // times in a loop was tried and reproducibly hit a separate,
            // real, pre-existing issue: `StorageEngine::query_hybrid`'s
            // `embed()` is synchronous CPU work called with no
            // `spawn_blocking` while every connection (including unrelated
            // `QueryFocusHistory` ones) shares one `LocalSet` (D-19), and a
            // repeated real embedding call pushed real total query latency
            // past the 100ms C3-query deadline (rules.md §5.7) on this
            // machine -- flagged in memory.md's tech debt as a genuine
            // Phase 4 gap, not fixed here (shipped, merged code; out of
            // scope to change unilaterally).
            assert!(
                result.own_compute < Duration::from_millis(5),
                "FR-KNO-09: own-compute should be well under budget, got {:?}",
                result.own_compute,
            );
        } => {}
    }
}
