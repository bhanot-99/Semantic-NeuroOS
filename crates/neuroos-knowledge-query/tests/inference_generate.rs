//! P5-S03 prereq: a real `Generate` call against the real
//! `neuroos-inference` binary (C4) over `inference.sock`, with tokens read
//! back through a real memfd ring received via `SCM_RIGHTS`. Mirrors
//! `tests/contract/inference_smoke.sh`'s own env-var setup for spawning
//! the real binary against the real BitNet model.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use neuroos_knowledge_query::inference_client::InferenceClient;

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

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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

fn sock_path(proc: &InferenceProcess) -> PathBuf {
    proc.tmp.path().join("run/neuroos/inference.sock")
}

/// Live proof: needs the real model fetched (`just fetch-models`) and
/// `cpp/neuroos-inference` built (`just build` / cmake), so `#[ignore]`d
/// like this workspace's other real-download-dependent tests (`cargo test
/// -p neuroos-knowledge-query --test inference_generate -- --ignored`).
#[tokio::test]
#[ignore = "needs the real BitNet model + built cpp/neuroos-inference binary; see doc comment"]
async fn generate_produces_real_text_through_a_real_memfd_ring() {
    let Some(proc) = spawn_real_inference() else {
        return;
    };
    let sock = sock_path(&proc);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !sock.exists() {
        if std::time::Instant::now() > deadline {
            panic!("inference.sock never appeared (model load took too long or failed)");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let client = InferenceClient::new(sock);
    let count = client.count_tokens("hello world").await.unwrap();
    assert!(count > 0, "should have tokenized a non-empty string");

    let answer = client
        .generate("p5-s03-test", "Q: What is 2+2?\nA:", 8)
        .await
        .unwrap();
    assert!(
        !answer.trim().is_empty(),
        "generation should produce some text: {answer:?}"
    );
}
