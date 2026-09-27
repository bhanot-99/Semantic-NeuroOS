//! P5-S03 / FR-KNO-03: evidence retrieval for a question
//! (`QueryHybridVectorText`, top_k = 5 default). The merge-by-distance
//! across every domain family happens server-side in C3
//! (`LanceStore::query_all_families`, P4-S03); this only calls it.
use neuroos_proto::v1::ChunkMatch;

use crate::storage_client::{StorageClient, StorageClientError};

/// FR-KNO-03: "Retrieve evidence from C3 (top_k = 5 default)."
pub const DEFAULT_TOP_K: u32 = 5;

pub async fn retrieve(
    client: &StorageClient,
    question: &str,
) -> Result<Vec<ChunkMatch>, StorageClientError> {
    client.query_hybrid(question, DEFAULT_TOP_K).await
}
