//! P5-S02 / FR-KNO-02: on receiving a question, request an acoustic
//! preamble from C2 before any retrieval work begins (Architecture.md
//! §6.1's hot-path sequence). This module owns that ordering. P5-S03 will
//! extend `ask_context` to also run evidence retrieval (`QueryHybridVectorText`)
//! alongside the deictic snap, once this crate has a consumer for it.
use crate::deictic::{self, WindowContext};
use crate::storage_client::{StorageClient, StorageClientError};
use crate::voice_client::{VoiceClient, VoiceClientError};

/// design.md §9.3's "Lookup / summarise" category — matches
/// Architecture.md §6.1's own sequence-diagram example verbatim. Rotating
/// among the catalogue's alternatives is deferred; a fixed phrase is
/// correct, just not yet varied.
pub const DEFAULT_PREAMBLE: &str = "Checking your files…";

#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("preamble request failed: {0}")]
    Preamble(#[from] VoiceClientError),
    #[error("focus history query failed: {0}")]
    FocusHistory(#[from] StorageClientError),
}

/// FR-KNO-02: requests the preamble first and only *then* resolves deictic
/// context. Ordering matters -- a preamble that starts after retrieval (or
/// after the model has already begun replying) isn't a preamble; it's
/// dead air. `#[ignore]`-free ordering proof lives in `tests/` (mock C2 +
/// mock C3, no real models needed).
pub async fn ask_context(
    voice: &VoiceClient,
    storage: &StorageClient,
    t_speech_start_ns: u64,
) -> Result<Option<WindowContext>, AskError> {
    voice.request_preamble(DEFAULT_PREAMBLE).await?;
    Ok(deictic::snap(storage, t_speech_start_ns).await?)
}
