//! P5-S04 / FR-KNO-05: when raw evidence exceeds 1,000 tokens, spawn an
//! asynchronous distillation job on C4's background lane that warms a
//! cache for follow-up questions -- never blocking the current answer
//! (rules.md AB-11: "hot paths must not block on slow work").
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use neuroos_proto::v1::ChunkMatch;
use tokio::sync::Semaphore;

use crate::inference_client::InferenceClient;

/// FR-KNO-05: "when raw evidence > 1,000 tokens".
pub const DISTILL_THRESHOLD_TOKENS: u32 = 1_000;
pub const DISTILL_MAX_TOKENS: u32 = 256;
pub const DISTILL_RING_NAME: &str = "knowledge-distill";

/// BUG-006: `maybe_spawn_distillation` used to fire an unbounded
/// `tokio::spawn` per over-threshold question -- on a real ~8h recording,
/// almost every early KPI-1 question exceeded the threshold, so a real
/// backlog of concurrent background BitNet generations (each several
/// seconds of real CPU-bound decode) piled up and starved this machine's
/// CPU enough to push `neuroos-storage`'s own query latency past its
/// 100ms deadline (~170-190ms measured, `storage.sock` `read deadline
/// exceeded` on ~20/51 questions). Capping concurrent distillation jobs
/// at 1 keeps the background lane's real CPU footprint bounded to "at
/// most one decode running" instead of an unbounded pile-up, without
/// making the hot `ask()` path await distillation itself (rules.md
/// AB-11) -- extra over-threshold questions just queue on this permit
/// inside their own already-spawned, already-detached task.
const MAX_CONCURRENT_DISTILLATIONS: usize = 1;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keyed by question text -- a real conversation-window keying scheme
/// (per-focus-window, per-session) is Phase 6's concern (the 8 s
/// follow-up window, design.md §9.2); this phase's job is only to warm
/// the cache, not to define how a later turn looks it up.
#[derive(Clone)]
pub struct DistillationCache {
    inner: Arc<Mutex<HashMap<String, String>>>,
    /// BUG-006: bounds how many `distill()` jobs may run at once (see
    /// `MAX_CONCURRENT_DISTILLATIONS`'s doc comment).
    limiter: Arc<Semaphore>,
}

impl Default for DistillationCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            limiter: Arc::new(Semaphore::new(MAX_CONCURRENT_DISTILLATIONS)),
        }
    }
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

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.limiter.available_permits()
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
    let limiter = cache.limiter.clone();
    tokio::spawn(async move {
        // BUG-006: acquired inside the already-detached task, not before
        // `tokio::spawn` -- the caller (the hot `ask()` path) still
        // returns immediately regardless of how many distillations are
        // already queued on this permit.
        let Ok(_permit) = limiter.acquire().await else {
            return; // semaphore closed (process shutting down)
        };
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

    // BUG-006: the cache's distillation limiter starts with exactly
    // `MAX_CONCURRENT_DISTILLATIONS` permits, and acquiring/releasing one
    // moves the count as expected -- proves the semaphore is wired with
    // the intended cap. A full test of real concurrent `distill()` calls
    // actually serializing needs a fake `inference.sock` server (see
    // `cpp_inference_smoke`'s style of test infra), out of scope here.
    #[tokio::test]
    async fn distillation_limiter_starts_at_the_configured_cap() {
        let cache = DistillationCache::new();
        assert_eq!(cache.available_permits(), MAX_CONCURRENT_DISTILLATIONS);

        let permit = cache.limiter.clone().acquire_owned().await.unwrap();
        assert_eq!(cache.available_permits(), MAX_CONCURRENT_DISTILLATIONS - 1);

        drop(permit);
        assert_eq!(cache.available_permits(), MAX_CONCURRENT_DISTILLATIONS);
    }
}
