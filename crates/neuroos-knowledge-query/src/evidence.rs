//! P5-S03 / FR-KNO-03: evidence retrieval for a question
//! (`QueryHybridVectorText`, top_k = 5 default). The merge-by-distance
//! across every domain family happens server-side in C3
//! (`LanceStore::query_all_families`, P4-S03); this calls it, then drops
//! duplicate chunk text and chunks too far from the question to be
//! evidence (KPI-1 diagnosis 2026-09-29: repeated window-focus events
//! filled top_k with identical titles, and irrelevant chunks at distance
//! 0.8-0.9 made the model ramble instead of saying "I don't know").
use std::collections::HashSet;

use neuroos_proto::v1::ChunkMatch;

use crate::storage_client::{StorageClient, StorageClientError};

/// FR-KNO-03: "Retrieve evidence from C3 (top_k = 5 default)."
pub const DEFAULT_TOP_K: u32 = 5;

/// Candidates fetched from C3 per returned chunk, so dropping duplicates
/// still leaves `DEFAULT_TOP_K` distinct chunks when they exist.
const OVERFETCH_FACTOR: u32 = 4;

/// Chunks farther than this from the question are not evidence.
pub const MAX_EVIDENCE_DISTANCE: f32 = 0.775;

pub async fn retrieve(
    client: &StorageClient,
    question: &str,
) -> Result<Vec<ChunkMatch>, StorageClientError> {
    let candidates = client
        .query_hybrid(question, DEFAULT_TOP_K * OVERFETCH_FACTOR)
        .await?;
    Ok(select_evidence(candidates, DEFAULT_TOP_K as usize))
}

/// Input is distance-sorted (C3 contract). Keeps the first (closest) chunk
/// per distinct text, drops chunks beyond [`MAX_EVIDENCE_DISTANCE`], and
/// caps the result at `top_k`.
fn select_evidence(candidates: Vec<ChunkMatch>, top_k: usize) -> Vec<ChunkMatch> {
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|c| c.distance <= MAX_EVIDENCE_DISTANCE)
        .filter(|c| seen.insert(c.text.clone()))
        .take(top_k)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn chunk(text: &str, distance: f32) -> ChunkMatch {
        ChunkMatch {
            chunk_id: String::new(),
            entity_id: 0,
            text: text.to_string(),
            taint: None,
            t_ns: 0,
            domain: "window_focus".to_string(),
            distance,
        }
    }

    #[test]
    fn duplicates_are_dropped_keeping_the_closest() {
        let out = select_evidence(vec![chunk("a", 0.1), chunk("a", 0.2), chunk("b", 0.3)], 5);
        let texts: Vec<_> = out.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["a", "b"]);
        assert!((out[0].distance - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn chunks_beyond_the_distance_cutoff_are_dropped() {
        let out = select_evidence(vec![chunk("near", 0.5), chunk("far", 0.9)], 5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "near");
    }

    #[test]
    fn result_is_capped_at_top_k() {
        let cands = (0..10).map(|i| chunk(&format!("c{i}"), 0.1)).collect();
        assert_eq!(select_evidence(cands, 5).len(), 5);
    }

    #[test]
    fn all_far_yields_empty() {
        assert!(select_evidence(vec![chunk("x", 0.95)], 5).is_empty());
    }
}
