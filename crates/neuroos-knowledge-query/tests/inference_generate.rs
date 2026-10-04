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

async fn ready_client(proc: &InferenceProcess) -> InferenceClient {
    let sock = sock_path(proc);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !sock.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "inference.sock never appeared (model load took too long or failed)"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    InferenceClient::new(sock)
}

const APPLE: &str = "System: Repeat the user's word many times.<|eot_id|>User: apple<|eot_id|>Assistant: apple apple";
const ZEBRA: &str = "System: Repeat the user's word many times.<|eot_id|>User: zebra<|eot_id|>Assistant: zebra zebra";

/// H7: two generations on the same (reused) ring name must each get only
/// their own tokens. The ring's generation used to be bumped at submit
/// and stamped onto slots at write time, so a request submitted while
/// another was still decoding received the first one's remaining tokens.
#[tokio::test]
#[ignore = "needs the real BitNet model + built cpp/neuroos-inference binary; see doc comment"]
async fn concurrent_generations_on_one_ring_do_not_mix_tokens() {
    let Some(proc) = spawn_real_inference() else {
        return;
    };
    let client = ready_client(&proc).await;
    let solo_apple = client.generate("shared", APPLE, 24).await.unwrap();
    let solo_zebra = client.generate("shared", ZEBRA, 24).await.unwrap();
    assert!(
        solo_apple.contains("apple") && !solo_apple.contains("zebra"),
        "precondition (model behaviour): {solo_apple:?}"
    );
    assert!(
        solo_zebra.contains("zebra") && !solo_zebra.contains("apple"),
        "precondition (model behaviour): {solo_zebra:?}"
    );

    let (a, z) = (client.clone(), client.clone());
    let apple = tokio::spawn(async move { a.generate("shared", APPLE, 24).await });
    tokio::time::sleep(Duration::from_millis(300)).await; // apple is decoding
    let zebra = tokio::spawn(async move { z.generate("shared", ZEBRA, 24).await });
    let apple = apple.await.unwrap().unwrap();
    let zebra = zebra.await.unwrap().unwrap();

    assert!(
        !apple.contains("zebra"),
        "apple got zebra's tokens: {apple:?}"
    );
    assert!(
        !zebra.contains("apple"),
        "zebra got apple's tokens: {zebra:?}"
    );
    assert!(apple.contains("apple"), "{apple:?}");
    assert!(zebra.contains("zebra"), "{zebra:?}");
}

/// H8: a prompt that leaves less room in the 512-token context than
/// `max_tokens` must end cleanly at the context limit (with an
/// end-of-stream slot), not fail mid-decode and leave the client waiting
/// for its 30 s generation deadline.
#[tokio::test]
#[ignore = "needs the real BitNet model + built cpp/neuroos-inference binary; see doc comment"]
async fn a_prompt_near_the_context_limit_ends_cleanly() {
    let Some(proc) = spawn_real_inference() else {
        return;
    };
    let client = ready_client(&proc).await;
    let mut prompt = String::from("Notes:");
    while client.count_tokens(&prompt).await.unwrap() < 495 {
        prompt.push_str(" the quick brown fox");
    }
    assert!(client.count_tokens(&prompt).await.unwrap() <= 512);

    let started = std::time::Instant::now();
    let answer = client.generate("near-limit", &prompt, 64).await;

    assert!(answer.is_ok(), "{answer:?}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "must end at the context limit, not at the client deadline ({:?})",
        started.elapsed()
    );
}
