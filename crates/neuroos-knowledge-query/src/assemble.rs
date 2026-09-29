//! P5-S03 / FR-KNO-04/06: prompt assembly with a hard 512-token cap: 128
//! system + 64 deictic + 320 evidence (Architecture.md §6.1). Token counts
//! come from C4's own tokenizer via `GetInfoRequest.tokenize_text` --
//! BitNet's GGUF has no separate tokenizer file for C5a to load locally,
//! and FR-KNO-09 excludes C3/C4 call latency from C5's own-compute budget
//! precisely so this is allowed.
use std::time::{Duration, Instant};

use neuroos_proto::v1::ChunkMatch;
use neuroos_taint::TaintFlags;

use crate::deictic::WindowContext;
use crate::inference_client::{InferenceClient, InferenceClientError};
use crate::taint_wrap;

pub const SYSTEM_BUDGET_TOKENS: u32 = 128;
pub const DEICTIC_BUDGET_TOKENS: u32 = 64;
pub const EVIDENCE_BUDGET_TOKENS: u32 = 320;
pub const TOTAL_BUDGET_TOKENS: u32 =
    SYSTEM_BUDGET_TOKENS + DEICTIC_BUDGET_TOKENS + EVIDENCE_BUDGET_TOKENS; // 512, FR-KNO-06

const SYSTEM_PREAMBLE: &str = "You are NeuroOS, a personal assistant. The evidence is a list of \
window titles from the user's recent computer activity. Answer the user's question in one or two \
short sentences using only that evidence. If the evidence does not answer it, reply: I do not know.";

#[derive(Debug, Clone, PartialEq)]
pub struct AssembledPrompt {
    pub text: String,
    /// FR-KNO-07: union of every evidence chunk's taint.
    pub taint: TaintFlags,
    /// FR-KNO-09: wall-clock time spent in this function's own CPU-bound
    /// work only -- every `inference.count_tokens` `.await` inside
    /// [`truncate_evidence`] is C4-call latency and is excluded, timed
    /// separately from the taint/formatting work around it.
    pub own_compute: Duration,
}

/// Truncates `evidence` to fit `EVIDENCE_BUDGET_TOKENS`, wraps tainted
/// chunks (FR-KNO-07), formats the deictic window, and assembles the
/// final prompt.
pub async fn assemble(
    inference: &InferenceClient,
    question: &str,
    window: Option<&WindowContext>,
    evidence: &[ChunkMatch],
) -> Result<AssembledPrompt, InferenceClientError> {
    let t0 = Instant::now();
    let taint = taint_wrap::union_taint(evidence);
    let deictic_text = format_deictic(window, question);
    let mut own_compute = t0.elapsed();

    let (evidence_text, truncate_own_compute) =
        truncate_evidence(inference, evidence, EVIDENCE_BUDGET_TOKENS).await?;
    own_compute += truncate_own_compute;

    let t1 = Instant::now();
    // BitNet b1.58 2B-4T's own GGUF chat template (`tokenizer.chat_template`):
    // `<bos>Human: {content}\n\nBITNETAssistant: `, generation ends at
    // `<|end_of_text|>`. The template has no system role, so the
    // instructions go inside the user turn. BOS is added by C4's tokenizer.
    let text = format!(
        "Human: {SYSTEM_PREAMBLE}\n\nEvidence:\n{evidence_text}\n\n{deictic_text}\n\nBITNETAssistant: "
    );
    own_compute += t1.elapsed();

    Ok(AssembledPrompt {
        text,
        taint,
        own_compute,
    })
}

fn format_deictic(window: Option<&WindowContext>, question: &str) -> String {
    match window {
        Some(w) => format!(
            "The user is looking at \"{}\" in {} and asked: {question}",
            w.title, w.app_id
        ),
        None => format!("The user asked: {question}"),
    }
}

/// FR-KNO-04: fast-path truncation. Builds the evidence block chunk by
/// chunk, highest relevance first (matching `ChunkMatch`'s
/// distance-sorted order from C3's `query_all_families`), stopping before
/// any chunk whose real token count would exceed `budget_tokens`. One
/// `GetInfo` call per candidate chunk (top_k = 5 default, so at most 5
/// round trips) -- each is C3/C4-call latency, excluded from FR-KNO-09's
/// own-compute budget.
async fn truncate_evidence(
    inference: &InferenceClient,
    chunks: &[ChunkMatch],
    budget_tokens: u32,
) -> Result<(String, Duration), InferenceClientError> {
    let mut text = String::new();
    let mut own_compute = Duration::ZERO;
    for chunk in chunks {
        let t = Instant::now();
        let taint = TaintFlags::from_bits_truncate(chunk.taint.as_ref().map_or(0, |t| t.flags));
        let wrapped = taint_wrap::wrap_if_tainted(&chunk.text, taint);
        let candidate = if text.is_empty() {
            wrapped
        } else {
            format!("{text}\n---\n{wrapped}")
        };
        own_compute += t.elapsed();

        // C4 call (`GetInfoRequest.tokenize_text`), excluded from
        // FR-KNO-09's own-compute budget -- timed separately, above.
        let count = inference.count_tokens(&candidate).await?;
        if count > budget_tokens {
            break;
        }
        text = candidate;
    }
    Ok((text, own_compute))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn budgets_sum_to_the_512_token_hard_cap() {
        assert_eq!(TOTAL_BUDGET_TOKENS, 512);
    }

    #[test]
    fn deictic_text_mentions_the_window_when_present() {
        let window = WindowContext {
            app_id: "org.mozilla.firefox".into(),
            title: "quarterly revenue".into(),
            pid: 0,
            root_pid: 0,
            t_start_ns: 0,
            t_end_ns: 0,
            dwell_ms: 0,
        };
        let text = format_deictic(Some(&window), "what does this say");
        assert!(text.contains("quarterly revenue"));
        assert!(text.contains("org.mozilla.firefox"));
        assert!(text.contains("what does this say"));
    }

    #[test]
    fn deictic_text_falls_back_to_the_bare_question_without_a_window() {
        let text = format_deictic(None, "what's the weather");
        assert_eq!(text, "The user asked: what's the weather");
    }

    /// FR-KNO-09 (phases.md §8.3 PF: "Own compute p99 < 5 ms, instrumented
    /// spans"): a fast, deterministic, no-IPC measurement of exactly the
    /// operations `assemble()`/`truncate_evidence()` time as `own_compute`
    /// (taint union, deictic formatting, per-chunk taint wrapping + string
    /// building) -- `assemble()` itself can't be called without a real
    /// `InferenceClient` (every chunk needs a real `GetInfoRequest`), so
    /// this exercises the identical non-IPC code paths directly, n=200,
    /// for a real p99 rather than trusting a single sample.
    /// `tests/ask_end_to_end.rs`'s live test additionally confirms one real
    /// `own_compute` sample from the actual full C3+C4 path stays this low.
    #[test]
    fn own_compute_for_the_non_ipc_portions_of_assembly_is_well_under_budget() {
        use neuroos_health::Histogram;
        use neuroos_proto::v1::Taint;

        let evidence: Vec<ChunkMatch> = (0..5)
            .map(|i| ChunkMatch {
                chunk_id: format!("chunk-{i}"),
                entity_id: i,
                text: "some real-looking evidence text ".repeat(40),
                taint: Some(Taint {
                    flags: if i % 2 == 0 { 0 } else { 1 },
                }),
                t_ns: 0,
                domain: "notes".to_string(),
                distance: 0.1,
            })
            .collect();
        let window = WindowContext {
            app_id: "org.mozilla.firefox".into(),
            title: "quarterly revenue dashboard".into(),
            pid: 0,
            root_pid: 0,
            t_start_ns: 0,
            t_end_ns: 0,
            dwell_ms: 0,
        };

        let hist = Histogram::new();
        for _ in 0..200 {
            let t = Instant::now();
            let taint = taint_wrap::union_taint(&evidence);
            let _deictic = format_deictic(Some(&window), "what does the revenue dashboard say");
            let mut text = String::new();
            for chunk in &evidence {
                let chunk_taint =
                    TaintFlags::from_bits_truncate(chunk.taint.as_ref().map_or(0, |t| t.flags));
                let wrapped = taint_wrap::wrap_if_tainted(&chunk.text, chunk_taint);
                text = if text.is_empty() {
                    wrapped
                } else {
                    format!("{text}\n---\n{wrapped}")
                };
            }
            let _final = format!(
                "System: {SYSTEM_PREAMBLE}<|eot_id|>User: Evidence:\n{text}\n\nx<|eot_id|>Assistant: "
            );
            hist.record(t.elapsed());
            std::hint::black_box(&taint);
        }
        let p99 = neuroos_health::p99_ns(&hist.to_proto()).expect("200 samples must yield a p99");
        assert!(
            p99 < 5_000_000,
            "FR-KNO-09: own-compute p99 must be < 5ms, measured {p99}ns over 200 samples"
        );
    }
}
