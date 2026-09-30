//! `knowledge.sock`: serves `AskRequest` (FR-CLI-01 `neuroosctl ask`,
//! P5-S06; also the future voice-triggered entry point once Phase 6's C2
//! calls in). Wraps [`crate::orchestrate::ask`] behind a real IPC server,
//! same shape as `neuroos-storage::server`.
use std::path::PathBuf;
use std::sync::Arc;

use neuroos_health::HealthServer;
use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{
    AskResponse, Envelope, Error, ErrorCode, RenderGraphViewResponse, Taint, envelope,
};
use tokio::net::UnixStream;

use crate::distill::DistillationCache;
use crate::graph_view;
use crate::inference_client::InferenceClient;
use crate::kernel_client::KernelClient;
use crate::orchestrate;
use crate::storage_client::StorageClient;
use crate::voice_client::VoiceClient;

/// FR-KNO-09's own-compute measurement lives under this name in `ask`'s
/// `HealthResponse.latency_histograms` -- `neuroosctl status`-style
/// tooling (or a direct `HealthRequest`) reads `p99_ns` off it the same
/// way it already does for every other component's histograms.
pub const OWN_COMPUTE_HISTOGRAM: &str = "ask_own_compute_ns";

/// Every downstream client `ask()` needs, bundled so `serve()` has one
/// cheap-to-clone value per connection instead of five. Each client is
/// just a socket `PathBuf` wrapper -- cloning opens no new connection.
#[derive(Clone)]
pub struct Clients {
    pub voice: VoiceClient,
    pub storage: StorageClient,
    pub inference: InferenceClient,
    pub kernel: KernelClient,
    pub distill_cache: DistillationCache,
    /// Shared with the `component_health_sock` endpoint (`main.rs`) so
    /// FR-KNO-09's own-compute figure is visible the same way every other
    /// component's latency histograms already are.
    pub health: Arc<HealthServer>,
}

pub async fn serve(clients: Clients, path: PathBuf, allowed_uids: Vec<u32>) {
    let server = match UdsServer::bind(UdsServerConfig::new(path, allowed_uids)) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to bind knowledge.sock");
            return;
        }
    };
    loop {
        match server.accept().await {
            Ok(Some((stream, _cred))) => {
                tokio::spawn(handle_conn(stream, clients.clone()));
            }
            Ok(None) => continue, // rejected peer (SO_PEERCRED not in allowlist); keep serving
            Err(e) => {
                tracing::warn!(error = %e, "knowledge.sock accept failed");
            }
        }
    }
}

async fn handle_conn(mut stream: UnixStream, clients: Clients) {
    loop {
        let env = match read_envelope(&mut stream, DEFAULT_MAX_FRAME).await {
            Ok(Some(env)) => env,
            Ok(None) => return, // peer closed cleanly
            Err(e) => {
                tracing::debug!(error = %e, "knowledge.sock read failed");
                return;
            }
        };
        let response = handle_request(env, &clients).await;
        if write_envelope(&mut stream, &response, DEFAULT_MAX_FRAME)
            .await
            .is_err()
        {
            return;
        }
    }
}

/// FI (phases.md §8.3): a hard failure anywhere in `ask()` (C3/C4/voice
/// down or timed out) is a fail-soft degraded apology, not an `Error`
/// envelope (rules.md §5.6) -- the caller still gets a well-formed
/// `AskResponse`, just flagged `degraded`.
fn degraded_response() -> envelope::Body {
    envelope::Body::AskResponse(AskResponse {
        answer: "I can't answer that right now.".to_string(),
        taint: Some(Taint { flags: 0 }),
        degraded: true,
    })
}

async fn handle_request(env: Envelope, clients: &Clients) -> Envelope {
    let now = neuroos_common::now_ns();
    let body = match env.body {
        Some(envelope::Body::AskRequest(req)) => {
            // AskRequest.t_ns == 0 means "use now" (knowledge.proto's own
            // doc comment) -- the text CLI path has no real utterance
            // start time the way a voice command does.
            let t_ns = if req.t_ns == 0 { now } else { req.t_ns };
            match orchestrate::ask(
                &clients.voice,
                &clients.storage,
                &clients.inference,
                &clients.kernel,
                &clients.distill_cache,
                &req.question,
                t_ns,
            )
            .await
            {
                Ok(result) => {
                    // FR-KNO-09: recorded regardless of degraded/approved
                    // outcome -- own-compute is spent either way.
                    clients
                        .health
                        .record_latency(OWN_COMPUTE_HISTOGRAM, result.own_compute);
                    envelope::Body::AskResponse(AskResponse {
                        answer: result.answer,
                        taint: Some(Taint {
                            flags: result.taint.bits(),
                        }),
                        degraded: result.degraded,
                    })
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ask() failed, answering degraded");
                    degraded_response()
                }
            }
        }
        Some(envelope::Body::RenderGraphViewRequest(_)) => {
            match render_and_write_graph_view(&clients.storage, now).await {
                Ok(path) => envelope::Body::RenderGraphViewResponse(RenderGraphViewResponse {
                    path: path.display().to_string(),
                }),
                Err(e) => envelope::Body::Error(Error {
                    code: ErrorCode::Internal as i32,
                    message: format!("RenderGraphView failed: {e}"),
                    retryable: true,
                }),
            }
        }
        _ => envelope::Body::Error(Error {
            code: ErrorCode::Internal as i32,
            message: "unsupported request on knowledge.sock".to_string(),
            retryable: true,
        }),
    };
    Envelope {
        schema_version: 1,
        trace_id: env.trace_id,
        request_id: env.request_id,
        sent_at_ns: now,
        body: Some(body),
    }
}

#[derive(Debug, thiserror::Error)]
enum RenderGraphViewError {
    #[error("failed to list entities: {0}")]
    ListEntities(crate::storage_client::StorageClientError),
    #[error("failed to list edges: {0}")]
    ListEdges(crate::storage_client::StorageClientError),
    #[error("failed to create {0}: {1}")]
    CreateDir(PathBuf, std::io::Error),
    #[error("failed to write {0}: {1}")]
    Write(PathBuf, std::io::Error),
}

/// FR-KNO-11: fetches every entity/edge over `storage.sock`, renders
/// `graph_view.html` (`crate::graph_view::render`), and writes it to
/// `~/.local/share/neuroos/graph_view.html` (design.md §6).
async fn render_and_write_graph_view(
    storage: &StorageClient,
    generated_at_ns: u64,
) -> Result<PathBuf, RenderGraphViewError> {
    let entities = storage
        .list_entities(0)
        .await
        .map_err(RenderGraphViewError::ListEntities)?;
    let edges = storage
        .list_edges()
        .await
        .map_err(RenderGraphViewError::ListEdges)?;
    let html = graph_view::render(&entities, &edges, generated_at_ns);

    let path = neuroos_common::paths::graph_view_html_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| RenderGraphViewError::CreateDir(parent.to_path_buf(), e))?;
    }
    std::fs::write(&path, html).map_err(|e| RenderGraphViewError::Write(path.clone(), e))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use std::time::Duration;

    use neuroos_ipc::{connect, read_envelope_deadline, write_envelope_deadline};
    use neuroos_proto::v1::AskRequest;
    use neuroos_testkit::{kernel_mocks, voice_mocks};

    use super::*;

    fn current_uid() -> u32 {
        // SAFETY: getuid() takes no arguments and cannot fail.
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        unsafe { getuid() }
    }

    /// No real C3/C4 running -- `storage`/`inference` point at sockets that
    /// don't exist, so `ask()` must fail and this server must still answer
    /// with a well-formed degraded `AskResponse`, never a hang or an
    /// `Error` envelope (FI: "C3 slow -> deadline hit -> answer without
    /// evidence flagged" / "C4 down -> degraded apology").
    #[tokio::test]
    async fn ask_request_degrades_gracefully_when_downstream_services_are_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_sock = dir.path().join("knowledge.sock");
        let voice_sock = dir.path().join("voice.sock");
        let kernel_sock = dir.path().join("kernel.sock");
        let my_uid = current_uid();

        voice_mocks::spawn_preamble_recorder(&voice_sock, my_uid);
        kernel_mocks::spawn_always_approve(&kernel_sock, my_uid);

        let clients = Clients {
            voice: VoiceClient::new(voice_sock),
            storage: StorageClient::new(dir.path().join("storage-nonexistent.sock")),
            inference: InferenceClient::new(dir.path().join("inference-nonexistent.sock")),
            kernel: KernelClient::new(kernel_sock),
            distill_cache: DistillationCache::new(),
            health: neuroos_health::HealthServer::new("test"),
        };

        tokio::spawn(serve(clients, knowledge_sock.clone(), vec![my_uid]));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut stream = connect(&knowledge_sock, Duration::from_secs(1))
            .await
            .unwrap();
        let req = Envelope {
            schema_version: 1,
            trace_id: "test".into(),
            request_id: 1,
            sent_at_ns: neuroos_common::now_ns(),
            body: Some(envelope::Body::AskRequest(AskRequest {
                question: "what does this say".into(),
                t_ns: 0,
            })),
        };
        write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, Duration::from_secs(1))
            .await
            .unwrap();
        let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, Duration::from_secs(1))
            .await
            .unwrap()
            .unwrap();
        match resp.body {
            Some(envelope::Body::AskResponse(r)) => {
                assert!(r.degraded, "unreachable C3 must yield a degraded answer");
                assert!(!r.answer.trim().is_empty());
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn unsupported_request_gets_an_error_response() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_sock = dir.path().join("knowledge.sock");
        let my_uid = current_uid();
        let clients = Clients {
            voice: VoiceClient::new(dir.path().join("voice-nonexistent.sock")),
            storage: StorageClient::new(dir.path().join("storage-nonexistent.sock")),
            inference: InferenceClient::new(dir.path().join("inference-nonexistent.sock")),
            kernel: KernelClient::new(dir.path().join("kernel-nonexistent.sock")),
            distill_cache: DistillationCache::new(),
            health: neuroos_health::HealthServer::new("test"),
        };
        tokio::spawn(serve(clients, knowledge_sock.clone(), vec![my_uid]));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut stream = connect(&knowledge_sock, Duration::from_secs(1))
            .await
            .unwrap();
        let req = Envelope {
            schema_version: 1,
            trace_id: "test".into(),
            request_id: 1,
            sent_at_ns: neuroos_common::now_ns(),
            body: Some(envelope::Body::IngestRequest(
                neuroos_proto::v1::IngestRequest {
                    source: "x".into(),
                    payload: vec![],
                },
            )),
        };
        write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, Duration::from_secs(1))
            .await
            .unwrap();
        let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, Duration::from_secs(1))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(resp.body, Some(envelope::Body::Error(_))));
    }

    /// FR-KNO-11 (P5-S08): `RenderGraphViewRequest` fetches entities/edges
    /// over a (mocked) `storage.sock`, renders `graph_view.html`, and
    /// writes it to the real filesystem path `neuroosctl graph open`
    /// expects.
    #[tokio::test]
    async fn render_graph_view_writes_html_and_returns_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_sock = dir.path().join("knowledge.sock");
        let storage_sock = dir.path().join("storage.sock");
        let my_uid = current_uid();

        let data_dir = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test process (nextest runs each test in
        // its own process); no other thread reads env vars concurrently.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", data_dir.path());
        }

        neuroos_testkit::storage_mocks::spawn_fixed_graph(
            &storage_sock,
            my_uid,
            vec![neuroos_proto::v1::EntityRow {
                id: 1,
                domain: "window_focus".into(),
                kind: "window".into(),
                label: "firefox".into(),
                taint: 0,
                created_ns: 0,
                last_seen_ns: 0,
                permanent: false,
            }],
            vec![],
        );

        let clients = Clients {
            voice: VoiceClient::new(dir.path().join("voice-nonexistent.sock")),
            storage: StorageClient::new(storage_sock),
            inference: InferenceClient::new(dir.path().join("inference-nonexistent.sock")),
            kernel: KernelClient::new(dir.path().join("kernel-nonexistent.sock")),
            distill_cache: DistillationCache::new(),
            health: neuroos_health::HealthServer::new("test"),
        };
        tokio::spawn(serve(clients, knowledge_sock.clone(), vec![my_uid]));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut stream = connect(&knowledge_sock, Duration::from_secs(1))
            .await
            .unwrap();
        let req = Envelope {
            schema_version: 1,
            trace_id: "test".into(),
            request_id: 1,
            sent_at_ns: neuroos_common::now_ns(),
            body: Some(envelope::Body::RenderGraphViewRequest(
                neuroos_proto::v1::RenderGraphViewRequest {},
            )),
        };
        write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, Duration::from_secs(1))
            .await
            .unwrap();
        let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, Duration::from_secs(1))
            .await
            .unwrap()
            .unwrap();
        match resp.body {
            Some(envelope::Body::RenderGraphViewResponse(r)) => {
                let path = std::path::PathBuf::from(&r.path);
                assert_eq!(path, neuroos_common::paths::graph_view_html_file());
                let html = std::fs::read_to_string(&path).unwrap();
                assert!(html.contains("firefox"));
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }
}
