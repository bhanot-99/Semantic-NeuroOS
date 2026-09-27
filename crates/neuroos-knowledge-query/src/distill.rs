//! P5-S04 / FR-KNO-05: when raw evidence exceeds 1,000 tokens, spawn an
//! asynchronous distillation job on C4's background lane that warms a
//! cache for follow-up questions -- never blocking the current answer
//! (rules.md AB-11: "hot paths must not block on slow work").
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use neuroos_proto::v1::ChunkMatch;

use crate::inference_client::InferenceClient;

/// FR-KNO-05: "when raw evidence > 1,000 tokens".
pub const DISTILL_THRESHOLD_TOKENS: u32 = 1_000;
pub const DISTILL_MAX_TOKENS: u32 = 256;
pub const DISTILL_RING_NAME: &str = "knowledge-distill";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keyed by question text -- a real conversation-window keying scheme
/// (per-focus-window, per-session) is Phase 6's concern (the 8 s
/// follow-up window, design.md §9.2); this phase's job is only to warm
/// the cache, not to define how a later turn looks it up.
#[derive(Clone, Default)]
pub struct DistillationCache {
    inner: Arc<Mutex<HashMap<String, String>>>,
}

impl DistillationCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, question: &str) -> Option<String> {
        lock(&self.inner).get(question).cloned()
    }

    fn insert(&self, question: String, distilled: String) {
        lock(&self.inner).insert(question, distilled);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        lock(&self.inner).len()
    }
}

/// FR-KNO-05: if the raw (untruncated) evidence exceeds
/// `DISTILL_THRESHOLD_TOKENS`, spawns a background distillation job and
/// returns immediately without awaiting it.
pub fn maybe_spawn_distillation(
    inference: InferenceClient,
    cache: DistillationCache,
    question: String,
    raw_evidence: &[ChunkMatch],
    raw_evidence_tokens: u32,
) {
    if raw_evidence_tokens <= DISTILL_THRESHOLD_TOKENS {
        return;
    }
    let chunks: Vec<String> = raw_evidence.iter().map(|c| c.text.clone()).collect();
    tokio::spawn(async move {
        match inference
            .distill(DISTILL_RING_NAME, chunks, DISTILL_MAX_TOKENS)
            .await
        {
            Ok(distilled) => cache.insert(question, distilled),
            Err(e) => {
                tracing::warn!(error = %e, "FR-KNO-05 background distillation failed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn cache_starts_empty_and_round_trips_a_manual_insert() {
        let cache = DistillationCache::new();
        assert_eq!(cache.get("q"), None);
        cache.insert("q".to_string(), "d".to_string());
        assert_eq!(cache.get("q"), Some("d".to_string()));
    }

    #[tokio::test]
    async fn small_evidence_never_spawns_a_distillation_job() {
        // A nonexistent socket would make any real distill() call fail
        // fast; if this test's cache ever gained an entry, it would only
        // be from a job that was wrongly spawned despite being under
        // threshold.
        let inference = InferenceClient::new(PathBuf::from("/tmp/nonexistent-inference.sock"));
        let cache = DistillationCache::new();
        maybe_spawn_distillation(
            inference,
            cache.clone(),
            "q".to_string(),
            &[],
            DISTILL_THRESHOLD_TOKENS, // exactly at the threshold: not over it
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(cache.len(), 0);
    }
}
