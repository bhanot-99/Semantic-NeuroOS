//! Thin client to `storage.sock` (Architecture.md §5.2). AB-1 forbids C5a
//! linking `neuroos-storage` directly, so this is the only way C5a reaches
//! stored data: `QueryFocusHistory` (deictic snap, P5-S01) and
//! `QueryHybridVectorText` (evidence retrieval, P5-S03).
use std::path::PathBuf;
use std::time::Duration;

use neuroos_ipc::{OnRestart, request_once};
use neuroos_proto::v1::{
    ChunkMatch, EdgeRow, EntityRow, FocusHistoryRow, ListEdgesRequest, ListEntitiesRequest,
    QueryActivityRequest, QueryActivityResponse, QueryFocusHistoryRequest, QueryHybridRequest,
    envelope,
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

/// C3: `request_once` reports the transport failures this enum already had
/// variants for, so the mapping is one-to-one and every existing
/// `StorageClientError::Connect { .. }` match keeps working.
impl From<neuroos_ipc::RequestError> for StorageClientError {
    fn from(e: neuroos_ipc::RequestError) -> Self {
        match e {
            neuroos_ipc::RequestError::Connect { path, source } => Self::Connect { path, source },
            neuroos_ipc::RequestError::Framing(e) => Self::Io(e),
            neuroos_ipc::RequestError::NoResponse => Self::NoResponse,
        }
    }
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
        // C3: connect/envelope/write/read lives in neuroos_ipc::request_once.
        let response = request_once(
            &self.socket_path,
            body,
            1,
            STORAGE_QUERY_DEADLINE,
            OnRestart::Retry,
        )
        .await?;
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

    /// BUG-007: structured activity (titles by dwell, media in order) in
    /// `[since_ns, until_ns]`, `until_ns = 0` meaning unbounded.
    pub async fn query_activity(
        &self,
        since_ns: u64,
        until_ns: u64,
        limit: u32,
    ) -> Result<QueryActivityResponse, StorageClientError> {
        let body = self
            .round_trip(envelope::Body::QueryActivityRequest(QueryActivityRequest {
                since_ns,
                until_ns,
                limit,
            }))
            .await?;
        match body {
            envelope::Body::QueryActivityResponse(r) => Ok(r),
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
    use neuroos_common::current_uid;

    /// M20 / AB-10: C3 restarting (systemd, a GC crash) left its socket
    /// file behind with nothing listening, and every C5a call in that
    /// window failed outright -- no client in the workspace retried a
    /// connect. The restart is now invisible as long as it fits inside
    /// the call's own deadline.
    #[tokio::test]
    async fn a_storage_restart_inside_the_deadline_is_invisible() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("storage.sock");
        // C3 has been up and is now down, socket file still in place.
        drop(tokio::net::UnixListener::bind(&sock).unwrap());

        let restart = sock.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            std::fs::remove_file(&restart).ok();
            neuroos_testkit::storage_mocks::spawn_empty(&restart, current_uid());
        });

        let client = StorageClient::new(sock);
        let rows = client
            .query_focus_history(0, 0)
            .await
            .expect("a restart within the 100ms deadline must not fail the query");
        assert!(rows.is_none(), "the restarted mock has no focus history");
    }

    #[tokio::test]
    async fn connect_failure_is_a_clear_error_not_a_hang() {
        let client = StorageClient::new(PathBuf::from(
            "/tmp/neuroos-knowledge-query-test-nonexistent.sock",
        ));
        let err = client.query_focus_history(0, 0).await.unwrap_err();
        assert!(matches!(err, StorageClientError::Connect { .. }));
    }
}
