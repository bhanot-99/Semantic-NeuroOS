//! LanceDB: vectors + chunks, one table per domain family (Architecture.md
//! §7.2: `attention`, `work`, `knowledge`, `system`, `external`), columns
//! `chunk_id, entity_id, text, vector[384], taint, t_ns, domain`.
use std::path::Path;
use std::sync::Arc;

use arrow_array::types::Float32Type;
use arrow_array::{
    Array, FixedSizeListArray, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
    UInt32Array, UInt64Array,
};
use arrow_schema::{ArrowError, DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{Connection, Table};

/// `bge-small-en-v1.5`'s output width (matches `embed::EMBEDDING_DIM`; kept
/// separate so this module has no compile-time dependency on `embed`).
pub const EMBEDDING_DIM: i32 = 384;

/// Architecture.md §7.2's 5 domain families.
pub const FAMILIES: &[&str] = &["attention", "work", "knowledge", "system", "external"];

const DISTANCE_COLUMN: &str = "_distance";

#[derive(Debug, thiserror::Error)]
pub enum LanceError {
    #[error("lancedb error: {0}")]
    Lance(#[from] lancedb::Error),
    #[error("arrow error: {0}")]
    Arrow(#[from] ArrowError),
    #[error("unknown domain family {0:?} (expected one of {FAMILIES:?})")]
    UnknownFamily(String),
    #[error("storage path is not valid UTF-8: {0:?}")]
    InvalidPath(std::path::PathBuf),
    #[error("malformed record batch: missing or mistyped column {0:?}")]
    MalformedBatch(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkRecord {
    pub chunk_id: String,
    pub entity_id: i64,
    pub text: String,
    pub vector: Vec<f32>,
    pub taint: u32,
    pub t_ns: u64,
    pub domain: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkMatch {
    pub chunk_id: String,
    pub entity_id: i64,
    pub text: String,
    pub taint: u32,
    pub t_ns: u64,
    pub domain: String,
    pub distance: f32,
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("entity_id", DataType::Int64, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                EMBEDDING_DIM,
            ),
            false,
        ),
        Field::new("taint", DataType::UInt32, false),
        Field::new("t_ns", DataType::UInt64, false),
        Field::new("domain", DataType::Utf8, false),
    ]))
}

fn empty_reader() -> RecordBatchIterator<std::vec::IntoIter<Result<RecordBatch, ArrowError>>> {
    RecordBatchIterator::new(Vec::new().into_iter(), schema())
}

fn records_to_batch(chunks: &[ChunkRecord]) -> Result<RecordBatch, LanceError> {
    let vector = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        chunks
            .iter()
            .map(|c| Some(c.vector.iter().copied().map(Some).collect::<Vec<_>>())),
        EMBEDDING_DIM,
    );
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from_iter_values(
                chunks.iter().map(|c| c.chunk_id.as_str()),
            )),
            Arc::new(Int64Array::from_iter_values(
                chunks.iter().map(|c| c.entity_id),
            )),
            Arc::new(StringArray::from_iter_values(
                chunks.iter().map(|c| c.text.as_str()),
            )),
            Arc::new(vector),
            Arc::new(UInt32Array::from_iter_values(
                chunks.iter().map(|c| c.taint),
            )),
            Arc::new(UInt64Array::from_iter_values(chunks.iter().map(|c| c.t_ns))),
            Arc::new(StringArray::from_iter_values(
                chunks.iter().map(|c| c.domain.as_str()),
            )),
        ],
    )?;
    Ok(batch)
}

fn column<'a, T: Array + 'static>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a T, LanceError> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<T>())
        .ok_or(LanceError::MalformedBatch(name))
}

fn decode_batch(batch: &RecordBatch, has_distance: bool) -> Result<Vec<ChunkMatch>, LanceError> {
    let chunk_id = column::<StringArray>(batch, "chunk_id")?;
    let entity_id = column::<Int64Array>(batch, "entity_id")?;
    let text = column::<StringArray>(batch, "text")?;
    let taint = column::<UInt32Array>(batch, "taint")?;
    let t_ns = column::<UInt64Array>(batch, "t_ns")?;
    let domain = column::<StringArray>(batch, "domain")?;
    let distance = has_distance
        .then(|| column::<arrow_array::Float32Array>(batch, DISTANCE_COLUMN))
        .transpose()?;

    Ok((0..batch.num_rows())
        .map(|i| ChunkMatch {
            chunk_id: chunk_id.value(i).to_string(),
            entity_id: entity_id.value(i),
            text: text.value(i).to_string(),
            taint: taint.value(i),
            t_ns: t_ns.value(i),
            domain: domain.value(i).to_string(),
            distance: distance.map(|d| d.value(i)).unwrap_or(0.0),
        })
        .collect())
}

pub struct LanceStore {
    conn: Connection,
}

impl LanceStore {
    /// Opens (creating if needed) the LanceDB database at `path` and
    /// ensures every domain-family table exists (empty tables are cheap and
    /// this keeps `insert`/`query` from needing a "does the table exist yet"
    /// branch on every call).
    pub async fn open(path: &Path) -> Result<Self, LanceError> {
        let uri = path
            .to_str()
            .ok_or_else(|| LanceError::InvalidPath(path.to_path_buf()))?;
        let conn = lancedb::connect(uri).execute().await?;
        let store = Self { conn };
        for family in FAMILIES {
            store.ensure_table(family).await?;
        }
        Ok(store)
    }

    fn check_family(family: &str) -> Result<(), LanceError> {
        if FAMILIES.contains(&family) {
            Ok(())
        } else {
            Err(LanceError::UnknownFamily(family.to_string()))
        }
    }

    async fn ensure_table(&self, family: &str) -> Result<Table, LanceError> {
        Self::check_family(family)?;
        let names = self.conn.table_names().execute().await?;
        if names.iter().any(|n| n == family) {
            Ok(self.conn.open_table(family).execute().await?)
        } else {
            Ok(self
                .conn
                .create_table(family, Box::new(empty_reader()))
                .execute()
                .await?)
        }
    }

    /// FR-STO-04: batched insert (the ingest path's batching requirement —
    /// call with every chunk from one ingest cycle, not one row at a time).
    pub async fn insert(&self, family: &str, chunks: &[ChunkRecord]) -> Result<(), LanceError> {
        if chunks.is_empty() {
            return Ok(());
        }
        let table = self.ensure_table(family).await?;
        let batch = records_to_batch(chunks)?;
        let reader = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema());
        table.add(Box::new(reader)).execute().await?;
        Ok(())
    }

    /// FR-STO-05: exact (flat) nearest-neighbor search within one family.
    /// No index is created here — that only happens once FR-STO-07's HNSW
    /// promotion threshold fires (P4-S05).
    pub async fn query(
        &self,
        family: &str,
        vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ChunkMatch>, LanceError> {
        let table = self.ensure_table(family).await?;
        let batches = table
            .query()
            .nearest_to(vector)?
            .limit(top_k)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        batches
            .iter()
            .map(|b| decode_batch(b, true))
            .collect::<Result<Vec<_>, _>>()
            .map(|nested| nested.into_iter().flatten().collect())
    }

    /// FR-STO-05's `QueryHybridVectorText`: search every family (a question
    /// isn't scoped to one domain) and merge by distance.
    pub async fn query_all_families(
        &self,
        vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ChunkMatch>, LanceError> {
        let mut all = Vec::new();
        for family in FAMILIES {
            all.extend(self.query(family, vector, top_k).await?);
        }
        all.sort_by(|a, b| a.distance.total_cmp(&b.distance));
        all.truncate(top_k);
        Ok(all)
    }

    /// FR-STO-12 ("forget"): deletes every row matching `predicate` (a
    /// LanceDB SQL-like filter, e.g. `"entity_id IN (1,2,3)"`) from one
    /// family.
    pub async fn delete(&self, family: &str, predicate: &str) -> Result<(), LanceError> {
        let table = self.ensure_table(family).await?;
        table.delete(predicate).await?;
        Ok(())
    }

    /// Same, across every family — "forget" isn't scoped to one domain.
    pub async fn delete_all_families(&self, predicate: &str) -> Result<(), LanceError> {
        for family in FAMILIES {
            self.delete(family, predicate).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn chunk(id: &str, vector: Vec<f32>) -> ChunkRecord {
        ChunkRecord {
            chunk_id: id.to_string(),
            entity_id: 1,
            text: format!("text for {id}"),
            vector,
            taint: 0,
            t_ns: 1,
            domain: "window_focus".into(),
        }
    }

    #[tokio::test]
    async fn insert_then_query_finds_the_nearest_vector() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        store
            .insert(
                "attention",
                &[
                    chunk("near", vec![1.0; EMBEDDING_DIM as usize]),
                    chunk("far", vec![-1.0; EMBEDDING_DIM as usize]),
                ],
            )
            .await
            .unwrap();

        let results = store
            .query("attention", &[1.0; EMBEDDING_DIM as usize], 1)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, "near");
    }

    #[tokio::test]
    async fn query_all_families_merges_across_families_by_distance() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        store
            .insert(
                "attention",
                &[chunk("a", vec![1.0; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap();
        store
            .insert(
                "knowledge",
                &[chunk("b", vec![0.99; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap();

        let results = store
            .query_all_families(&[1.0; EMBEDDING_DIM as usize], 2)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].chunk_id, "a"); // exact match, closest
        assert!(results[0].distance <= results[1].distance);
    }

    #[tokio::test]
    async fn empty_family_returns_no_matches_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let results = store
            .query("work", &[0.0; EMBEDDING_DIM as usize], 5)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn delete_removes_matching_rows_by_predicate() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let mut a = chunk("a", vec![1.0; EMBEDDING_DIM as usize]);
        a.entity_id = 1;
        let mut b = chunk("b", vec![1.0; EMBEDDING_DIM as usize]);
        b.entity_id = 2;
        store.insert("attention", &[a, b]).await.unwrap();

        store.delete("attention", "entity_id = 1").await.unwrap();
        let remaining = store
            .query("attention", &[1.0; EMBEDDING_DIM as usize], 10)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].chunk_id, "b");
    }

    #[tokio::test]
    async fn unknown_family_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let err = store
            .insert(
                "not_a_real_family",
                &[chunk("x", vec![0.0; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LanceError::UnknownFamily(_)));
    }
}
