//! P5-S03 / FR-KNO-04/06: prompt assembly with a hard 512-token cap: 128
//! system + 64 deictic + 320 evidence (Architecture.md §6.1). Token counts
//! come from C4's own tokenizer via `GetInfoRequest.tokenize_text` --
//! BitNet's GGUF has no separate tokenizer file for C5a to load locally,
//! and FR-KNO-09 excludes C3/C4 call latency from C5's own-compute budget
//! precisely so this is allowed.
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

const SYSTEM_PREAMBLE: &str = "You are NeuroOS, a calm, competent assistant. Answer briefly \
and concretely, grounded only in the evidence given below. Say \"I don't know\" when the \
evidence doesn't cover the question.";

#[derive(Debug, Clone, PartialEq)]
pub struct AssembledPrompt {
    pub text: String,
    /// FR-KNO-07: union of every evidence chunk's taint.
    pub taint: TaintFlags,
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
    let taint = taint_wrap::union_taint(evidence);
    let deictic_text = format_deictic(window, question);
    let evidence_text = truncate_evidence(inference, evidence, EVIDENCE_BUDGET_TOKENS).await?;
    let text = format!("{SYSTEM_PREAMBLE}\n\n{deictic_text}\n\nEvidence:\n{evidence_text}");
    Ok(AssembledPrompt { text, taint })
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
) -> Result<String, InferenceClientError> {
    let mut text = String::new();
    for chunk in chunks {
        let taint = TaintFlags::from_bits_truncate(chunk.taint.as_ref().map_or(0, |t| t.flags));
        let wrapped = taint_wrap::wrap_if_tainted(&chunk.text, taint);
        let candidate = if text.is_empty() {
            wrapped
        } else {
            format!("{text}\n---\n{wrapped}")
        };
        let count = inference.count_tokens(&candidate).await?;
        if count > budget_tokens {
            break;
        }
        text = candidate;
    }
    Ok(text)
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
}
