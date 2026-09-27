//! P5-S02/S03/S05 / FR-KNO-02/03/04/06/07/08: the hot-path orchestration
//! (Architecture.md §6.1) -- preamble first, then deictic snap + evidence
//! retrieval in parallel, prompt assembly with taint wrapping, a C6
//! capability check, and finally a real C4 generation.
use neuroos_taint::TaintFlags;

use crate::assemble;
use crate::deictic::{self, WindowContext};
use crate::evidence;
use crate::inference_client::{InferenceClient, InferenceClientError};
use crate::kernel_client::{KernelClient, KernelClientError};
use crate::storage_client::{StorageClient, StorageClientError};
use crate::voice_client::{VoiceClient, VoiceClientError};

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

#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("preamble request failed: {0}")]
    Preamble(#[from] VoiceClientError),
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
    voice.request_preamble(DEFAULT_PREAMBLE).await?;
    deictic::snap(storage, t_speech_start_ns)
        .await
        .map_err(AskError::FocusHistory)
}

#[derive(Debug, Clone, PartialEq)]
pub struct AskResult {
    pub answer: String,
    /// FR-KNO-07: union of every evidence chunk's taint that went into
    /// this answer.
    pub taint: TaintFlags,
    /// Set when C6 denied the capability -- a fail-soft apology rather
    /// than a grounded answer (rules.md §5.6), not an error, since a
    /// denial is an expected, correctly-handled outcome, not a failure.
    pub degraded: bool,
}

/// The full hot path: preamble, parallel deictic snap + evidence
/// retrieval, prompt assembly (with taint union + wrapping), a C6
/// capability check, and a real C4 generation.
pub async fn ask(
    voice: &VoiceClient,
    storage: &StorageClient,
    inference: &InferenceClient,
    kernel: &KernelClient,
    question: &str,
    t_speech_start_ns: u64,
) -> Result<AskResult, AskError> {
    voice.request_preamble(DEFAULT_PREAMBLE).await?;

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

    let prompt = assemble::assemble(inference, question, window.as_ref(), &chunks).await?;

    let approved = kernel
        .evaluate_capability(CAPABILITY_GENERATE, prompt.taint)
        .await?;
    if !approved {
        return Ok(AskResult {
            answer: "I can't do that right now.".to_string(),
            taint: prompt.taint,
            degraded: true,
        });
    }

    let answer = inference
        .generate(TEXT_RING_NAME, &prompt.text, GENERATE_MAX_TOKENS)
        .await?;
    Ok(AskResult {
        answer,
        taint: prompt.taint,
        degraded: false,
    })
}
