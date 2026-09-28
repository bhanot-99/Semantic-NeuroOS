//! Thin client to `storage.sock` (Architecture.md §5.2). AB-1 forbids C5a
//! linking `neuroos-storage` directly, so this is the only way C5a reaches
//! stored data: `QueryFocusHistory` (deictic snap, P5-S01) and
//! `QueryHybridVectorText` (evidence retrieval, P5-S03).
use std::path::PathBuf;
use std::time::Duration;

use neuroos_ipc::{DEFAULT_MAX_FRAME, connect, read_envelope_deadline, write_envelope_deadline};
use neuroos_proto::v1::{
    ChunkMatch, EdgeRow, EntityRow, Envelope, FocusHistoryRow, ListEdgesRequest,
    ListEntitiesRequest, QueryFocusHistoryRequest, QueryHybridRequest, envelope,
};

/// rules.md §5.7: every C3 query gets a 100 ms deadline.
pub const STORAGE_QUERY_DEADLINE: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum StorageClientError {
    #[error("failed to connect to storage.sock at {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("storage.sock request failed: {0}")]
    Io(#[from] neuroos_ipc::FramingError),
    #[error("storage.sock closed the connection with no response")]
    NoResponse,
    #[error("storage.sock returned an error: {0}")]
    Remote(String),
    #[error("storage.sock sent an unexpected response type")]
    UnexpectedResponse,
}

#[derive(Clone)]
pub struct StorageClient {
    socket_path: PathBuf,
}

impl StorageClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    async fn round_trip(&self, body: envelope::Body) -> Result<envelope::Body, StorageClientError> {
        let mut stream = connect(&self.socket_path, STORAGE_QUERY_DEADLINE)
            .await
            .map_err(|source| StorageClientError::Connect {
                path: self.socket_path.clone(),
                source,
            })?;
        let request = Envelope {
            schema_version: 1,
            trace_id: String::new(),
            request_id: 1,
            sent_at_ns: neuroos_common::now_ns(),
            body: Some(body),
        };
        write_envelope_deadline(
            &mut stream,
            &request,
            DEFAULT_MAX_FRAME,
            STORAGE_QUERY_DEADLINE,
        )
        .await?;
        let response =
            read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, STORAGE_QUERY_DEADLINE)
                .await?
                .ok_or(StorageClientError::NoResponse)?;
        match response.body {
            Some(envelope::Body::Error(e)) => Err(StorageClientError::Remote(e.message)),
            Some(body) => Ok(body),
            None => Err(StorageClientError::NoResponse),
        }
    }

    /// FR-KNO-01: the deictic-snap query ("what was I looking at when I
    /// said this"). `None` iff nothing overlapped `t_ns ± window_ns`.
    pub async fn query_focus_history(
        &self,
        t_ns: u64,
        window_ns: u64,
    ) -> Result<Option<FocusHistoryRow>, StorageClientError> {
        let body = self
            .round_trip(envelope::Body::QueryFocusHistoryRequest(
                QueryFocusHistoryRequest { t_ns, window_ns },
            ))
            .await?;
        match body {
            envelope::Body::QueryFocusHistoryResponse(r) => Ok(r.row),
            _ => Err(StorageClientError::UnexpectedResponse),
        }
    }

    /// FR-KNO-03: evidence retrieval for a question, merged across every
    /// domain family and sorted by distance (`LanceStore::query_all_families`
    /// does the merge server-side).
    pub async fn query_hybrid(
        &self,
        text: &str,
        top_k: u32,
    ) -> Result<Vec<ChunkMatch>, StorageClientError> {
        let body = self
            .round_trip(envelope::Body::QueryHybridRequest(QueryHybridRequest {
                text: text.to_string(),
                top_k,
            }))
            .await?;
        match body {
            envelope::Body::QueryHybridResponse(r) => Ok(r.matches),
            _ => Err(StorageClientError::UnexpectedResponse),
        }
    }

    /// FR-KNO-11: every entity, for the graph view (P5-S08).
    pub async fn list_entities(&self, since_ns: u64) -> Result<Vec<EntityRow>, StorageClientError> {
        let body = self
            .round_trip(envelope::Body::ListEntitiesRequest(ListEntitiesRequest {
                since_ns,
            }))
            .await?;
        match body {
            envelope::Body::ListEntitiesResponse(r) => Ok(r.entities),
            _ => Err(StorageClientError::UnexpectedResponse),
        }
    }

    /// FR-KNO-11: every edge, for the graph view (P5-S08).
    pub async fn list_edges(&self) -> Result<Vec<EdgeRow>, StorageClientError> {
        let body = self
            .round_trip(envelope::Body::ListEdgesRequest(ListEdgesRequest {}))
            .await?;
        match body {
            envelope::Body::ListEdgesResponse(r) => Ok(r.edges),
            _ => Err(StorageClientError::UnexpectedResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[tokio::test]
    async fn connect_failure_is_a_clear_error_not_a_hang() {
        let client = StorageClient::new(PathBuf::from(
            "/tmp/neuroos-knowledge-query-test-nonexistent.sock",
        ));
        let err = client.query_focus_history(0, 0).await.unwrap_err();
        assert!(matches!(err, StorageClientError::Connect { .. }));
    }
}
