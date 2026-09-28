//! KPI-1 (PRD.md §8, phases.md §8.3 AT): "Answer correctness on the
//! scripted evaluation set (>= 50 questions over recorded telemetry) >= 80%
//! judged correct." This is the harness: ingests every *raw* (not
//! anonymised) event from Phase 3's real recording
//! (`.dev-cache/telemetry-raw/*.bin`, gitignored, never committed -- the
//! whole point of this eval is asking real questions about real
//! remembered content, which the anonymised `tests/fixtures/telemetry/`
//! copies can't support since their titles are hashed placeholders), runs
//! >=50 real questions through the real full hot path (real C3 + real C4,
//! mock C2/C6, same as `ask_end_to_end.rs`), and writes a transcript for
//! grading.
//!
//! **KPI-1 explicitly requires "human-graded"** (PRD.md §8's own table
//! wording) -- this harness produces the real questions and real answers;
//! a human (the owner, or an assistant doing a documented first pass
//! pending the owner's own review) still has to judge each one. The
//! transcript is written outside the repo (a path given via
//! `NEUROOS_KPI1_TRANSCRIPT_PATH`, defaulting to a tmp file) rather than
//! committed, since the real answers can quote real personal browsing
//! history.
//!
//! **First real run, 2026-09-28 (0/51 usable answers -- KPI-1 measured,
//! not met)**: over this machine's real ~8h/17,000-event recording, every
//! single one of the 51 scripted questions either (a) errored with
//! `inference.sock returned an error: lane queue is full` -- C4's
//! interactive lane (`kMaxQueueDepth = 4`, `cpp/neuroos-inference/src/
//! lanes.hpp`) reported itself full under a merely-sequential, 500ms-apart
//! real workload, a real resource-accounting bug, not contention -- or
//! (b) when generation did run, produced a degenerate repeated-token loop
//! (e.g. `"---\nbrave-browser: 0\n"` over and over to the 128-token cap)
//! instead of a real answer. `InferenceClient::generate` hardcodes
//! `temperature: 0.0, seed: 0` (greedy decoding, `src/
//! inference_client.rs`) with no repetition penalty -- a well-known small-
//! model failure mode that this session's earlier, much shorter
//! synthetic-prompt tests never had enough context length to trigger. The
//! first run (before a diagnostic `STORAGE_QUERY_DEADLINE` bump, reverted
//! after) also hit real `storage.sock` timeouts on every question,
//! corroborating this session's other query-latency finding at real data
//! scale (see memory.md tech debt). None of the three are fixed here --
//! all are real bugs in already-shipped Phase 2/4 code, out of scope to
//! patch unilaterally mid-Phase-5-closure; recorded in memory.md.
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
use neuroos_monitor::dump::DumpReader;
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
    dev_models_dir().join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so")
}

fn raw_dump_dir() -> PathBuf {
    repo_root().join(".dev-cache/telemetry-raw")
}

fn transcript_path() -> PathBuf {
    std::env::var_os("NEUROOS_KPI1_TRANSCRIPT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("neuroos-kpi1-transcript.md"))
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

/// 50 real questions about the real content in this machine's own
/// `.dev-cache/telemetry-raw/*.bin` (see that dump's real window titles --
/// hardware research, general browsing, this very project's own work
/// sessions, and media playback that the ingest filter's MPRIS demotion
/// should make correctly *un*-answerable). A `t_ns` of 0 means "now"
/// (no deictic anchor needed -- these are lookup questions, not "what's
/// this" pointing questions, which P5-S01's own fixture suite already
/// covers separately).
fn scripted_questions() -> Vec<&'static str> {
    vec![
        // Hardware research (transparent/small displays, robots)
        "what transparent display products was I researching",
        "what did I find about the 2.4-inch transparent SSD1309 display",
        "what was the StackChan robot I was looking at",
        "what M5Stack products did I look up",
        "what did I search for about Otto DIY robot",
        "what Waveshare display products did I view",
        "what did I find about the 1.51 inch transparent OLED display",
        "what small HDMI touchscreen displays was I comparing",
        "what did I look up about the Elecrow 5 inch touchscreen",
        "what hologram cube display product did I see",
        "what did I search for about a mini transparent display robot",
        "what CNC aluminum USB display did I research",
        "what AIDA64 sensor panel displays did I look at",
        "did I look up anything about a GeekMagic Hello Cube",
        "what DFRobot display product did I view",
        // General browsing
        "what did I search for about iPhone mini screen size",
        "did I look up a Bitcoin price tracker",
        "what phone did I look up on Amazon",
        "did I visit AliExpress",
        "what currency conversion did I search for",
        "what OnePlus phone specs did I look at",
        // This project's own work sessions
        "what was I doing with memory.md and phase 4",
        "what phase 5 work was I completing",
        "did I open a pull request",
        "what command did I run to install cloc",
        "was I using the COSMIC terminal",
        "what was in the file manager",
        // Notes / Gemini Notebook
        "what is in my Semantic Matrix notebook",
        "what autonomous data architecture notes do I have",
        // Media playback (expected: no evidence, since MPRIS is always demoted -- never becomes a persistent entity)
        "what show was I watching in VLC",
        "what episode of Bloodhounds did I watch last",
        "summarize what happened in the TV show I was watching",
        "what music was playing",
        "what was playing in my media player an hour ago",
        // Generic / no-evidence-expected sanity checks
        "what is the capital of France",
        "what did I have for lunch",
        "what is my bank account password",
        "did I email anyone today",
        "what meetings do I have tomorrow",
        // Broader lookups over the same real content, phrased differently
        "summarize what I was researching about small displays for a project",
        "what kind of desktop robot companion was I interested in",
        "give me a list of the display products I was comparing",
        "what was the last thing I searched for related to M5Stack",
        "was I comparing prices for any electronics",
        "what programming or development tools was I using",
        "what website was I browsing the most",
        "did I look at any Google search results about robots",
        "what did I learn about the CoreS3 module",
        "what secondary display options for a PC did I consider",
        "what was open in my browser related to hardware",
        "did I look up anything about a smart keychain robot",
    ]
}

/// Live proof (P5's own KPI-1 harness), needs the real BitNet model, built
/// `cpp/neuroos-inference`, fetched embedder models, AND this machine's own
/// real (non-anonymised, gitignored) `.dev-cache/telemetry-raw/*.bin` --
/// `#[ignore]`d and machine-local by nature (the raw recording only exists
/// on the machine that made it).
#[tokio::test]
#[ignore = "needs the real model, built cpp/neuroos-inference, and this machine's own .dev-cache/telemetry-raw/*.bin; see doc comment"]
async fn kpi1_scripted_questions_over_real_recorded_telemetry() {
    let dump_dir = raw_dump_dir();
    if !dump_dir.exists() {
        eprintln!(
            "skipping: {} not found (raw recording already cleaned up on this machine? this eval is machine-local by nature)",
            dump_dir.display()
        );
        return;
    }
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

    let mut dumps: Vec<PathBuf> = std::fs::read_dir(&dump_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bin"))
        .collect();
    dumps.sort();
    let mut total_events = 0u64;
    for dump in &dumps {
        let mut reader = DumpReader::open(dump).await.unwrap();
        while let Some(event) = reader.read_event().await.unwrap() {
            let _ = engine.ingest(&event).await;
            total_events += 1;
        }
    }
    eprintln!(
        "ingested {total_events} real events from {} raw dumps",
        dumps.len()
    );

    let storage_sock_dir = tempfile::tempdir().unwrap();
    let storage_sock = storage_sock_dir.path().join("storage.sock");
    let voice_sock_dir = tempfile::tempdir().unwrap();
    let voice_sock = voice_sock_dir.path().join("voice.sock");
    let kernel_sock_dir = tempfile::tempdir().unwrap();
    let kernel_sock = kernel_sock_dir.path().join("kernel.sock");
    let my_uid = current_uid();

    voice_mocks::spawn_preamble_recorder(&voice_sock, my_uid);
    kernel_mocks::spawn_always_approve(&kernel_sock, my_uid);

    let questions = scripted_questions();
    assert!(
        questions.len() >= 50,
        "KPI-1 needs >=50 scripted questions, have {}",
        questions.len()
    );

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

            let mut transcript = String::new();
            transcript.push_str("# KPI-1 evaluation transcript (real questions, real recorded telemetry)\n\n");
            transcript.push_str(&format!("Ingested {total_events} real events from {} raw dumps.\n\n", dumps.len()));
            transcript.push_str("Not committed to the repo -- may quote real personal browsing history.\n\n---\n\n");

            for (i, question) in questions.iter().enumerate() {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let result = orchestrate::ask(
                    &voice,
                    &storage,
                    &inference,
                    &kernel,
                    &distill_cache,
                    question,
                    0,
                )
                .await;
                let (answer, degraded) = match result {
                    Ok(r) => (r.answer, r.degraded),
                    Err(e) => (format!("[error: {e}]"), true),
                };
                eprintln!("[{}/{}] Q: {question}\nA: {answer}\n", i + 1, questions.len());
                transcript.push_str(&format!(
                    "## {}. {question}\n\n**Answer{}:** {answer}\n\n",
                    i + 1,
                    if degraded { " (degraded)" } else { "" },
                    answer = answer
                ));
            }

            std::fs::write(transcript_path(), &transcript).unwrap();
            eprintln!("\nTranscript written to {}", transcript_path().display());
        } => {}
    }
}
