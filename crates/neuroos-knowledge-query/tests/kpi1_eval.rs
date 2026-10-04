//! KPI-1 (PRD.md §8, phases.md §8.3 AT): "Answer correctness on the
//! scripted evaluation set (>= 50 questions over recorded telemetry) >= 80%
//! judged correct." This is the harness: ingests every *raw* (not
//! anonymised) event from Phase 3's real recording
//! (`.dev-cache/telemetry-raw/*.bin`, gitignored, never committed -- the
//! whole point of this eval is asking real questions about real
//! remembered content, which the anonymised `tests/fixtures/telemetry/`
//! copies can't support since their titles are hashed placeholders), runs
//! at least 50 real questions through the real full hot path (real C3 + real C4,
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
//! **Questions are machine-local too**: they are read one per line from
//! `.dev-cache/telemetry-raw/questions.txt` (blank lines and `#` comments
//! skipped), written by the owner against what this machine's own recording
//! actually contains, plus a few negative controls (nothing recorded, expect
//! a decline). They are not committed since they name real browsing history.
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

/// One scripted question plus its optional first-pass grading key.
struct Scripted {
    question: String,
    /// `None`: ungraded. `Some(empty)`: a negative control, expect a
    /// decline. Otherwise: correct if the text contains any alternative
    /// (case-insensitive).
    expect: Option<Vec<String>>,
    /// Retrieval-only key (`## evidence:`), for questions whose answer
    /// ("yes") never appears in the evidence itself.
    evidence: Option<Vec<String>>,
}

fn alternatives(s: &str) -> Vec<String> {
    s.split('|').map(|a| a.trim().to_lowercase()).collect()
}

/// Reads the machine-local scripted questions (see the file header). Each
/// line is `question [## expect: alt1 | alt2]`, or `... ## expect: DECLINE`.
fn scripted_questions() -> Vec<Scripted> {
    // NEUROOS_KPI1_QUESTIONS: an alternate question file (e.g. a held-out
    // set never used while tuning retrieval/prompting).
    let path = std::env::var_os("NEUROOS_KPI1_QUESTIONS")
        .map_or_else(|| raw_dump_dir().join("questions.txt"), PathBuf::from);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (l, evidence) = match l.split_once("## evidence:") {
                Some((rest, ev)) => (rest, Some(alternatives(ev))),
                None => (l, None),
            };
            let (question, expect) = match l.split_once("## expect:") {
                Some((q, e)) if e.trim() == "DECLINE" => (q, Some(Vec::new())),
                Some((q, e)) => (q, Some(alternatives(e))),
                None => (l, None),
            };
            Scripted {
                question: question.trim().to_owned(),
                expect,
                evidence,
            }
        })
        .collect()
}

/// First-pass auto-grade only; KPI-1 itself is human-graded.
fn auto_grade(expect: &Option<Vec<String>>, text: &str) -> Option<bool> {
    let lower = text.to_lowercase();
    let declined = lower.contains("i don't know") || lower.contains("i do not know");
    expect.as_ref().map(|alts| {
        if alts.is_empty() {
            declined
        } else {
            !declined && alts.iter().any(|a| lower.contains(a.as_str()))
        }
    })
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
    if std::env::var_os("NEUROOS_KPI1_QUESTIONS").is_none()
        && !dump_dir.join("questions.txt").exists()
    {
        eprintln!("skipping: {}/questions.txt not found", dump_dir.display());
        return;
    }
    // NEUROOS_KPI1_RETRIEVAL_ONLY=1: skip C4 and grade whether the
    // retrieved evidence contains the expected answer (seconds, not minutes).
    let retrieval_only = std::env::var_os("NEUROOS_KPI1_RETRIEVAL_ONLY").is_some();
    let inference_proc = if retrieval_only {
        None
    } else {
        let Some(p) = spawn_real_inference() else {
            return;
        };
        Some(p)
    };
    let inference_sock = inference_proc.as_ref().map_or_else(PathBuf::new, |p| {
        p.tmp.path().join("run/neuroos/inference.sock")
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while inference_proc.is_some() && !inference_sock.exists() {
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

    // Retrieval-only diagnostic: is each expected answer stored at all?
    // (A question whose answer never reached C3 bounds what retrieval can
    // achieve; e.g. windows too brief to pass the ingest promotion gate.)
    if std::env::var_os("NEUROOS_KPI1_RETRIEVAL_ONLY").is_some() {
        for item in scripted_questions() {
            let Some(alts) = item.evidence.as_ref().or(item.expect.as_ref()) else {
                continue;
            };
            if alts.is_empty() || alts.iter().any(|a| a == "yes") {
                continue;
            }
            let mut stored = false;
            for alt in alts {
                let hits = engine.query_hybrid(alt, 50).await.unwrap_or_default();
                if hits
                    .iter()
                    .any(|h| h.text.to_lowercase().contains(alt.as_str()))
                {
                    stored = true;
                    break;
                }
            }
            if !stored {
                eprintln!("UNREACHABLE (answer not in store): {}", item.question);
            }
        }
    }

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
        questions.len() >= 50 || std::env::var_os("NEUROOS_KPI1_QUESTIONS").is_some(),
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

            let (mut graded, mut correct) = (0u32, 0u32);
            for (i, item) in questions.iter().enumerate() {
                let question = &item.question;
                let (answer, degraded) = if retrieval_only {
                    match neuroos_knowledge_query::evidence::retrieve(&storage, question).await {
                        Ok(ev) if ev.is_empty() => ("I don't know. [no evidence]".to_owned(), false),
                        Ok(ev) => (
                            ev.iter()
                                .map(|c| format!("[d={:.3} k={:.1}] {}", c.distance, c.keyword_score, c.text))
                                .collect::<Vec<_>>()
                                .join(" || "),
                            false,
                        ),
                        Err(e) => (format!("[error: {e}]"), true),
                    }
                } else {
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
                    match result {
                        Ok(r) => (r.answer, r.degraded),
                        Err(e) => (format!("[error: {e}]"), true),
                    }
                };
                let key = if retrieval_only && item.evidence.is_some() {
                    &item.evidence
                } else {
                    &item.expect
                };
                let grade = auto_grade(key, &answer);
                if let Some(ok) = grade {
                    graded += 1;
                    correct += u32::from(ok);
                }
                let mark = match grade {
                    Some(true) => "PASS",
                    Some(false) => "FAIL",
                    None => "----",
                };
                eprintln!("[{}/{}] {mark} Q: {question}\nA: {answer}\n", i + 1, questions.len());
                transcript.push_str(&format!(
                    "## {}. {question}\n\n**Answer{}:** {answer}\n\n*Auto-grade (first pass): {mark}*\n\n",
                    i + 1,
                    if degraded { " (degraded)" } else { "" },
                    answer = answer
                ));
            }
            let summary = format!(
                "AUTO-GRADE{}: {correct}/{graded} first-pass correct",
                if retrieval_only { " (retrieval only)" } else { "" }
            );
            eprintln!("\n{summary}");
            transcript.push_str(&format!("---\n\n{summary}\n"));

            std::fs::write(transcript_path(), &transcript).unwrap();
            eprintln!("\nTranscript written to {}", transcript_path().display());
        } => {}
    }
}
