//! P5-S04 / FR-KNO-05: when raw evidence exceeds 1,000 tokens, spawn an
//! asynchronous distillation job on C4's background lane that warms a
//! cache for follow-up questions -- never blocking the current answer
//! (rules.md AB-11: "hot paths must not block on slow work").
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use neuroos_proto::v1::ChunkMatch;
use neuroos_taint::TaintFlags;
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
/// AB-11).
const MAX_CONCURRENT_DISTILLATIONS: usize = 1;

/// M10: how many jobs may be in flight *or waiting* on the permit above.
///
/// BUG-006 bounded how many distillations run at once, but not how many
/// queue up behind the permit: every over-threshold question still
/// spawned a detached task that sat on `limiter.acquire()`, so a long
/// question burst left an unbounded pile of waiting tasks, each holding a
/// clone of its defused evidence. Past this cap the job is dropped
/// instead of queued -- a warm cache is an optimisation (FR-KNO-05's own
/// "it never blocks the current answer"), so shedding one is always
/// better than growing C5 without limit against its 75 MiB budget
/// (PRD §6.2).
const MAX_PENDING_DISTILLATIONS: usize = 2;

/// M10: entries kept before the oldest is evicted.
///
/// The cache was an unbounded `HashMap` keyed by question text, never
/// evicted, in a long-running service: every distinct over-threshold
/// question added an entry (plus up to `DISTILL_MAX_TOKENS` of text) that
/// nothing ever removed.
const MAX_CACHE_ENTRIES: usize = 32;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One cached distillation.
#[derive(Debug, Clone, PartialEq)]
pub struct Distillation {
    /// C4's condensed summary of the evidence.
    pub text: String,
    /// M9's rule applied to the cache: the union of the taint of every
    /// chunk this was distilled from, plus `MODEL_GENERATED`, because C4
    /// wrote the text. A reader must not have to rediscover this.
    pub taint: TaintFlags,
    /// When it was stored, so a reader can decide it is too old to use.
    /// design.md §9.2's follow-up window is 8 s; enforcing it belongs to
    /// the C2 turn that reads this, not here.
    pub stored_at_ns: u64,
}

struct Inner {
    entries: HashMap<String, Distillation>,
    /// Insertion order, for FIFO eviction at `MAX_CACHE_ENTRIES`.
    order: VecDeque<String>,
}

/// Keyed by question text -- a real conversation-window keying scheme
/// (per-focus-window, per-session) is Phase 6's concern (the 8 s
/// follow-up window, design.md §9.2); this phase's job is only to warm
/// the cache, not to define how a later turn looks it up.
#[derive(Clone)]
pub struct DistillationCache {
    inner: Arc<Mutex<Inner>>,
    /// BUG-006: bounds how many `distill()` jobs may run at once (see
    /// `MAX_CONCURRENT_DISTILLATIONS`'s doc comment).
    limiter: Arc<Semaphore>,
    /// M10: in-flight *plus* waiting jobs, bounded by
    /// `MAX_PENDING_DISTILLATIONS`.
    pending: Arc<AtomicUsize>,
}

impl Default for DistillationCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                entries: HashMap::new(),
                order: VecDeque::new(),
            })),
            limiter: Arc::new(Semaphore::new(MAX_CONCURRENT_DISTILLATIONS)),
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl DistillationCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks up a warmed distillation.
    ///
    /// Phase 6 is the consumer: C2's follow-up turn (design.md §9.2) is
    /// what decides which question key a later turn maps to and how old
    /// an entry may be, which is why this phase deliberately only warms
    /// the cache and does not read it on the `ask()` hot path. Wiring a
    /// lookup in here would invent that contract a phase early (R0-8).
    pub fn get(&self, question: &str) -> Option<Distillation> {
        lock(&self.inner).entries.get(question).cloned()
    }

    fn insert(&self, question: String, distilled: Distillation) {
        let mut inner = lock(&self.inner);
        if inner.entries.insert(question.clone(), distilled).is_none() {
            inner.order.push_back(question);
            // M10: evict oldest-first past the cap. The loop (not an `if`)
            // keeps this correct even if the cap is ever lowered.
            while inner.order.len() > MAX_CACHE_ENTRIES {
                if let Some(oldest) = inner.order.pop_front() {
                    inner.entries.remove(&oldest);
                }
            }
        }
    }

    /// Claims one of `MAX_PENDING_DISTILLATIONS` slots, or `None` when
    /// they are all taken. The returned guard releases it on drop, on
    /// every path out of the job.
    fn claim_pending(&self) -> Option<PendingSlot> {
        let claimed = self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_PENDING_DISTILLATIONS).then_some(n + 1)
            });
        claimed.ok().map(|_| PendingSlot(Arc::clone(&self.pending)))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        lock(&self.inner).entries.len()
    }

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.limiter.available_permits()
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
}

/// Releases its `pending` slot however the job ends.
struct PendingSlot(Arc<AtomicUsize>);

impl Drop for PendingSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// FR-KNO-05: spawns a background distillation job for `raw_evidence` and
/// returns immediately.
///
/// M10: the token count used to be measured by the *caller*, on the hot
/// `ask()` path, with an awaited `GetInfoRequest` round trip to C4 -- in
/// direct contradiction of this module's own "never blocking the current
/// answer". Everything except claiming a queue slot now happens inside
/// the detached task: the threshold check, the control-token defusing and
/// the text join included.
pub fn maybe_spawn_distillation(
    inference: InferenceClient,
    cache: DistillationCache,
    question: String,
    raw_evidence: &[ChunkMatch],
) {
    if raw_evidence.is_empty() {
        return;
    }
    // M10: a sound, C4-free lower bound. Every token is at least one
    // character, so evidence of N characters can never tokenize to more
    // than N tokens -- if N is already within the threshold, no token
    // count can put it over, and there is nothing to distil. This cannot
    // produce a false negative, and it keeps both the `count_tokens`
    // round trip and a queue slot for the cases that might really qualify.
    let total_chars: usize = raw_evidence.iter().map(|c| c.text.len()).sum();
    if total_chars <= DISTILL_THRESHOLD_TOKENS as usize {
        return;
    }
    // M10: shed rather than queue once the cap is reached (see
    // `MAX_PENDING_DISTILLATIONS`).
    let Some(slot) = cache.claim_pending() else {
        tracing::debug!("distillation queue full; skipping this one");
        return;
    };

    // Cloned, not defused, on the hot path: the CPU work moves into the
    // task below. This is the only unavoidable hot-path cost, and it is a
    // handful of small string clones.
    let texts: Vec<String> = raw_evidence.iter().map(|c| c.text.clone()).collect();
    let taint = TaintFlags::propagate(
        raw_evidence
            .iter()
            .map(|c| TaintFlags::from_bits_truncate(c.taint.map(|t| t.flags).unwrap_or(0))),
    );
    let limiter = Arc::clone(&cache.limiter);

    tokio::spawn(async move {
        let _slot = slot;
        // H6: C4 builds the distillation prompt straight from these, with
        // special-token parsing on -- same defusing as the answer prompt.
        let chunks: Vec<String> = texts
            .iter()
            .map(|t| crate::assemble::defuse_control_tokens(t))
            .collect();
        let raw_text = chunks.join("\n---\n");

        // C4 call, now off the hot path entirely.
        let Ok(raw_tokens) = inference.count_tokens(&raw_text).await else {
            return;
        };
        if raw_tokens <= DISTILL_THRESHOLD_TOKENS {
            return;
        }

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
            Ok(distilled) => cache.insert(
                question,
                Distillation {
                    text: distilled,
                    // M9: the summary is C4's output, over this evidence.
                    taint: taint | TaintFlags::MODEL_GENERATED,
                    stored_at_ns: neuroos_common::now_ns(),
                },
            ),
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

    fn distillation(text: &str) -> Distillation {
        Distillation {
            text: text.to_string(),
            taint: TaintFlags::MODEL_GENERATED,
            stored_at_ns: 1,
        }
    }

    fn chunk(text: &str, taint_flags: u32) -> ChunkMatch {
        ChunkMatch {
            chunk_id: String::new(),
            entity_id: 0,
            text: text.to_string(),
            taint: Some(neuroos_proto::v1::Taint { flags: taint_flags }),
            t_ns: 0,
            domain: "window_focus".to_string(),
            distance: 0.1,
            keyword_score: 0.0,
        }
    }

    #[test]
    fn cache_starts_empty_and_round_trips_a_manual_insert() {
        let cache = DistillationCache::new();
        assert_eq!(cache.get("q"), None);
        cache.insert("q".to_string(), distillation("d"));
        assert_eq!(cache.get("q").map(|d| d.text), Some("d".to_string()));
    }

    /// M10: the cache was an unbounded `HashMap` in a long-running
    /// service -- every distinct over-threshold question added an entry
    /// that nothing ever removed.
    #[test]
    fn the_cache_evicts_oldest_first_past_its_cap() {
        let cache = DistillationCache::new();
        for i in 0..MAX_CACHE_ENTRIES + 5 {
            cache.insert(format!("q{i}"), distillation(&format!("d{i}")));
        }
        assert_eq!(cache.len(), MAX_CACHE_ENTRIES, "cache must stay bounded");
        assert_eq!(cache.get("q0"), None, "the oldest entries are evicted");
        assert!(
            cache.get(&format!("q{}", MAX_CACHE_ENTRIES + 4)).is_some(),
            "the newest entry must survive"
        );
    }

    /// Re-distilling the same question must refresh it, not grow the
    /// eviction queue with a duplicate key.
    #[test]
    fn reinserting_one_question_does_not_grow_the_cache() {
        let cache = DistillationCache::new();
        for n in 0..10 {
            cache.insert("q".to_string(), distillation(&format!("d{n}")));
        }
        assert_eq!(cache.len(), 1);
        assert_eq!(lock(&cache.inner).order.len(), 1);
        assert_eq!(cache.get("q").map(|d| d.text), Some("d9".to_string()));
    }

    /// M9's rule reaches the cache: a summary is C4's output over its
    /// evidence, so it carries that evidence's taint plus MODEL_GENERATED.
    #[test]
    fn a_cached_distillation_carries_evidence_taint_plus_model_generated() {
        let cache = DistillationCache::new();
        cache.insert(
            "q".to_string(),
            Distillation {
                text: "summary".to_string(),
                taint: TaintFlags::EXTERNAL_UNTRUSTED | TaintFlags::MODEL_GENERATED,
                stored_at_ns: 7,
            },
        );
        let got = cache.get("q").unwrap();
        assert!(got.taint.contains(TaintFlags::EXTERNAL_UNTRUSTED));
        assert!(got.taint.contains(TaintFlags::MODEL_GENERATED));
        assert_eq!(got.stored_at_ns, 7, "a reader needs the age for §9.2");
    }

    /// M10: evidence too short to possibly exceed the threshold must not
    /// cost a C4 round trip or a queue slot, so an obviously-qualifying
    /// question is never shed in favour of a trivial one.
    #[tokio::test]
    async fn evidence_too_short_to_qualify_never_reaches_c4() {
        let inference = InferenceClient::new(PathBuf::from("/tmp/nonexistent-inference.sock"));
        let cache = DistillationCache::new();
        // Well under DISTILL_THRESHOLD_TOKENS characters, so it cannot
        // tokenize to more than DISTILL_THRESHOLD_TOKENS tokens.
        let evidence = vec![chunk("firefox: a short window title", 0)];
        maybe_spawn_distillation(inference, cache.clone(), "q".to_string(), &evidence);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(cache.pending(), 0, "no slot should have been claimed");
        assert_eq!(cache.len(), 0);
    }

    /// The bound is a lower bound only: evidence long enough to *possibly*
    /// qualify still goes to C4 for the real count.
    #[tokio::test]
    async fn evidence_long_enough_to_possibly_qualify_is_handed_to_the_task() {
        let inference = InferenceClient::new(PathBuf::from("/tmp/nonexistent-inference.sock"));
        let cache = DistillationCache::new();
        let long = "x".repeat(DISTILL_THRESHOLD_TOKENS as usize + 1);
        let evidence = vec![chunk(&long, 0)];
        maybe_spawn_distillation(inference, cache.clone(), "q".to_string(), &evidence);
        // The task has the slot until its C4 call fails.
        assert_eq!(cache.pending(), 1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(cache.pending(), 0, "the failed job released its slot");
    }

    #[tokio::test]
    async fn empty_evidence_never_spawns_a_distillation_job() {
        let inference = InferenceClient::new(PathBuf::from("/tmp/nonexistent-inference.sock"));
        let cache = DistillationCache::new();
        maybe_spawn_distillation(inference, cache.clone(), "q".to_string(), &[]);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.pending(), 0, "no slot should have been claimed");
    }

    /// M10: over-threshold questions used to each spawn a detached task
    /// that queued on the 1-permit semaphore without limit, every one
    /// holding a clone of its evidence. Past the cap a job is shed.
    #[tokio::test]
    async fn pending_distillations_are_capped_rather_than_queued() {
        let cache = DistillationCache::new();
        let mut slots = Vec::new();
        for _ in 0..MAX_PENDING_DISTILLATIONS {
            slots.push(
                cache
                    .claim_pending()
                    .expect("slots up to the cap must be available"),
            );
        }
        assert_eq!(cache.pending(), MAX_PENDING_DISTILLATIONS);
        assert!(
            cache.claim_pending().is_none(),
            "past the cap a job must be shed, not queued"
        );

        // A job ending frees its slot however it ended.
        drop(slots.pop());
        assert_eq!(cache.pending(), MAX_PENDING_DISTILLATIONS - 1);
        assert!(cache.claim_pending().is_some());
    }

    /// M10: `maybe_spawn_distillation` must not await anything. The C4
    /// socket here does not exist, so a hot-path `count_tokens` would
    /// have to wait for a connect failure before returning; the call must
    /// return immediately regardless.
    #[tokio::test]
    async fn spawning_a_distillation_does_not_block_the_caller() {
        let inference = InferenceClient::new(PathBuf::from("/tmp/nonexistent-inference.sock"));
        let cache = DistillationCache::new();
        let long = "x".repeat(DISTILL_THRESHOLD_TOKENS as usize + 1);
        let evidence = vec![chunk(&long, 0)];

        let started = std::time::Instant::now();
        maybe_spawn_distillation(inference, cache.clone(), "q".to_string(), &evidence);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(5),
            "the hot path must not await C4: took {elapsed:?}"
        );
        // The job itself fails (no C4), leaving the cache cold and its
        // slot released.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.pending(), 0, "the shed job released its slot");
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
