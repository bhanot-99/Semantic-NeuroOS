//! P5-S02/S03/S05 / FR-KNO-02/03/04/06/07/08: the hot-path orchestration
//! (Architecture.md §6.1) -- preamble first, then deictic snap + evidence
//! retrieval in parallel, prompt assembly with taint wrapping, a C6
//! capability check, and finally a real C4 generation.
use std::time::Duration;

use neuroos_taint::TaintFlags;

use crate::assemble;
use crate::deictic::{self, WindowContext};
use crate::distill::{self, DistillationCache};
use crate::evidence;
use crate::inference_client::{InferenceClient, InferenceClientError};
use crate::kernel_client::{KernelClient, KernelClientError};
use crate::storage_client::{StorageClient, StorageClientError};
use crate::voice_client::VoiceClient;

/// design.md §9.3's "Lookup / summarise" category -- matches
/// Architecture.md §6.1's own sequence-diagram example verbatim. Rotating
/// among the catalogue's alternatives is deferred; a fixed phrase is
/// correct, just not yet varied.
pub const DEFAULT_PREAMBLE: &str = "Checking your files…";

/// FR-KNO-08's capability name for a generation call.
pub const CAPABILITY_GENERATE: &str = "llm.generate";

/// Named ring for the text path (`neuroosctl ask`, P5-S06). The voice path
/// (Phase 6) will use C2's own attached ring name instead.
pub const TEXT_RING_NAME: &str = "knowledge-text";

/// FR-INF-02's default `max_context_tokens` is 512 -- the *same* number as
/// FR-KNO-06's prompt cap, which leaves no guaranteed headroom for the
/// completion if a prompt genuinely fills all 320 evidence tokens. This is
/// a real, unresolved tension between the two requirements (see memory.md
/// tech debt); 128 is a conservative generation budget chosen to leave
/// room in the common case (a full 512-token prompt is the worst case, not
/// the typical one), not a value derived from either FR.
pub const GENERATE_MAX_TOKENS: u32 = 128;

/// Answer when retrieval finds no relevant evidence and there is no
/// deictic window to anchor on (C4 is not called).
pub const NO_EVIDENCE_ANSWER: &str = "I don't know. Nothing in your recorded activity covers that.";

#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("focus history query failed: {0}")]
    FocusHistory(StorageClientError),
    #[error("evidence retrieval failed: {0}")]
    Evidence(StorageClientError),
    #[error("inference call failed: {0}")]
    Inference(#[from] InferenceClientError),
    #[error("capability check failed: {0}")]
    Capability(#[from] KernelClientError),
}

/// FR-KNO-02: requests the preamble first and only *then* resolves deictic
/// context. Ordering matters -- a preamble that starts after retrieval (or
/// after the model has already begun replying) isn't a preamble; it's
/// dead air. Kept as its own small function (not inlined into [`ask`]) so
/// P5-S02's ordering proof (`tests/preamble_ordering.rs`) stays a fast,
/// mock-only test that doesn't need real inference/kernel clients.
pub async fn ask_context(
    voice: &VoiceClient,
    storage: &StorageClient,
    t_speech_start_ns: u64,
) -> Result<Option<WindowContext>, AskError> {
    request_preamble(voice).await;
    deictic::snap(storage, t_speech_start_ns)
        .await
        .map_err(AskError::FocusHistory)
}

/// H9 / rules.md §5.6 (fail soft): the preamble only masks latency, so a
/// C2 that is down or slow must not stop the answer itself. The failure is
/// logged (no user content in it) and the hot path carries on.
async fn request_preamble(voice: &VoiceClient) {
    if let Err(e) = voice.request_preamble(DEFAULT_PREAMBLE).await {
        tracing::warn!(error = %e, "preamble request failed; answering without it");
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AskResult {
    pub answer: String,
    /// FR-KNO-07 / Architecture.md §7.4 (`taint(output) =
    /// union(taint(inputs))`): the union of every evidence chunk's taint
    /// that went into this answer, plus `MODEL_GENERATED` when the answer
    /// text is C4's output rather than a fixed string composed by C5 (M9).
    pub taint: TaintFlags,
    /// Set when C6 denied the capability -- a fail-soft apology rather
    /// than a grounded answer (rules.md §5.6), not an error, since a
    /// denial is an expected, correctly-handled outcome, not a failure.
    pub degraded: bool,
    /// FR-KNO-09: total C5-owned compute for this call, excluding every
    /// C3/C4/C6 IPC round trip (`voice`/`storage`/`kernel`/`inference`
    /// `.await`s are never included) -- see [`assemble::AssembledPrompt`]'s
    /// own doc comment for how `assemble`'s share of this is measured.
    pub own_compute: Duration,
}

/// The full hot path: preamble, parallel deictic snap + evidence
/// retrieval, prompt assembly (with taint union + wrapping), a C6
/// capability check, and a real C4 generation. `distill_cache` warms
/// (FR-KNO-05) whenever the raw evidence is large -- see
/// [`crate::distill`] -- and is never awaited by this function itself.
pub async fn ask(
    voice: &VoiceClient,
    storage: &StorageClient,
    inference: &InferenceClient,
    kernel: &KernelClient,
    distill_cache: &DistillationCache,
    question: &str,
    t_speech_start_ns: u64,
) -> Result<AskResult, AskError> {
    request_preamble(voice).await;

    let (window, chunks) = tokio::try_join!(
        async {
            deictic::snap(storage, t_speech_start_ns)
                .await
                .map_err(AskError::FocusHistory)
        },
        async {
            evidence::retrieve(storage, question)
                .await
                .map_err(AskError::Evidence)
        },
    )?;

    // M10: the evidence join that used to be measured here moved into
    // the distillation task, so nothing accumulates into `own_compute`
    // before prompt assembly any more.
    let mut own_compute = Duration::ZERO;

    // FR-KNO-05: fire-and-forget, never on the critical path to the
    // answer below (rules.md AB-11).
    //
    // M10: this used to `await inference.count_tokens(..)` right here --
    // a real C4 round trip on the hot path, in a block whose own comment
    // says it never blocks the current answer. The token count, the
    // control-token defusing and the text join all happen inside the
    // detached task now, so the only cost left here is cloning the chunk
    // texts.
    distill::maybe_spawn_distillation(
        inference.clone(),
        distill_cache.clone(),
        question.to_string(),
        &chunks,
    );

    // KPI-1 diagnosis: with nothing relevant retrieved and no deictic
    // anchor, the small model rambles instead of declining. Decline
    // without calling C4.
    if chunks.is_empty() && window.is_none() {
        return Ok(AskResult {
            answer: NO_EVIDENCE_ANSWER.to_string(),
            taint: TaintFlags::empty(),
            degraded: false,
            own_compute,
        });
    }

    let prompt = assemble::assemble(inference, question, window.as_ref(), &chunks).await?;
    own_compute += prompt.own_compute;

    // C6 call, excluded from FR-KNO-09's own-compute budget.
    let approved = kernel
        .evaluate_capability(CAPABILITY_GENERATE, prompt.taint)
        .await?;
    if !approved {
        return Ok(AskResult {
            answer: "I can't do that right now.".to_string(),
            taint: prompt.taint,
            degraded: true,
            own_compute,
        });
    }

    // C4 call, excluded from FR-KNO-09's own-compute budget.
    let answer = inference
        .generate(TEXT_RING_NAME, &prompt.text, GENERATE_MAX_TOKENS)
        .await?;
    Ok(AskResult {
        answer,
        // M9: this string is literally the model's output, so it carries
        // `MODEL_GENERATED` on top of its evidence's taint. The two early
        // returns above do *not*: `NO_EVIDENCE_ANSWER` and the
        // capability-denied apology are fixed strings C5 wrote, and
        // claiming the model produced them would be false provenance.
        taint: prompt.taint | TaintFlags::MODEL_GENERATED,
        degraded: false,
        own_compute,
    })
}
