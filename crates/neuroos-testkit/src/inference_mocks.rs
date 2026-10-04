//! Mock `inference.sock` server. C4 is real C++ (`cpp/neuroos-inference`),
//! so the live tests that need real tokens talk to the real binary; this
//! mock exists for the control-plane behaviour a client must get right
//! *when C4 misbehaves* -- here, a generation that is accepted and then
//! never produces a token, which is what makes a client's read time out.
use std::path::Path;
use std::sync::{Arc, Mutex};

use neuroos_ipc::{
    DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope,
    write_envelope_with_fd,
};
use neuroos_proto::v1::{
    AttachRingResponse, CancelResponse, DistillResponse, Envelope, GenerateResponse, envelope,
};
use neuroos_shm::Ring;

/// The `generation_id`s a client asked this server to cancel, in arrival
/// order. Readable at any point after the exchange under test.
pub type CancelledGenerations = Arc<Mutex<Vec<u64>>>;

/// Accepts `AttachRing` (handing back a real memfd ring), accepts every
/// `Generate`/`Distill` with `generation_id`, then writes *nothing* into
/// the ring -- the client must give up on its own deadline. Records every
/// `CancelRequest` it receives.
pub fn spawn_stalling_generator(
    sock_path: impl AsRef<Path>,
    allowed_uid: u32,
    generation_id: u64,
) -> CancelledGenerations {
    let cancelled: CancelledGenerations = Arc::new(Mutex::new(Vec::new()));
    let handle = Arc::clone(&cancelled);
    let sock_path = sock_path.as_ref().to_path_buf();
    tokio::spawn(async move {
        let Ok(server) = UdsServer::bind(UdsServerConfig::new(sock_path, vec![allowed_uid])) else {
            return;
        };
        // Held for the life of the server so every attach hands out the
        // same ring, exactly as C4's `RingRegistry` does.
        let Ok(ring) = Ring::create("testkit-stalling", 64, 256) else {
            return;
        };
        let ring = Arc::new(ring);
        loop {
            let Ok(Some((stream, _cred))) = server.accept().await else {
                continue;
            };
            // One task per connection: a real client holds its `Generate`
            // connection open while it reads the ring, and opens a second
            // one to cancel -- a server that served connections strictly
            // in sequence would deadlock against exactly the behaviour
            // under test.
            let handle = Arc::clone(&handle);
            let ring = Arc::clone(&ring);
            tokio::spawn(async move {
                let mut stream = stream;
                while let Ok(Some(env)) = read_envelope(&mut stream, DEFAULT_MAX_FRAME).await {
                    let reply = |body| Envelope {
                        schema_version: 1,
                        trace_id: String::new(),
                        request_id: env.request_id,
                        sent_at_ns: 0,
                        body: Some(body),
                    };
                    match env.body {
                        Some(envelope::Body::AttachRingRequest(_)) => {
                            let resp =
                                reply(envelope::Body::AttachRingResponse(AttachRingResponse {
                                    ok: true,
                                    error: String::new(),
                                }));
                            if write_envelope_with_fd(&stream, &resp, DEFAULT_MAX_FRAME, ring.fd())
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Some(envelope::Body::GenerateRequest(_)) => {
                            let resp = reply(envelope::Body::GenerateResponse(GenerateResponse {
                                generation_id,
                                accepted: true,
                                error: String::new(),
                            }));
                            if write_envelope(&mut stream, &resp, DEFAULT_MAX_FRAME)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Some(envelope::Body::DistillRequest(_)) => {
                            let resp = reply(envelope::Body::DistillResponse(DistillResponse {
                                generation_id,
                                accepted: true,
                                error: String::new(),
                            }));
                            if write_envelope(&mut stream, &resp, DEFAULT_MAX_FRAME)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Some(envelope::Body::CancelRequest(c)) => {
                            handle
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push(c.generation_id);
                            let resp = reply(envelope::Body::CancelResponse(CancelResponse {
                                cancelled: true,
                            }));
                            if write_envelope(&mut stream, &resp, DEFAULT_MAX_FRAME)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            });
        }
    });
    cancelled
}
