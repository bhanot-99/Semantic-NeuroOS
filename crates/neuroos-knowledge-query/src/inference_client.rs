//! Thin client to `inference.sock` (Architecture.md §5.2). C5a is one of
//! its two clients (the other, C2, lands in Phase 6): `GetInfo` (token
//! counting -- BitNet's GGUF embeds its own tokenizer, so there is no
//! separate tokenizer file for C5a to load locally; FR-KNO-09 excludes
//! C3/C4 call latency from C5's "<5ms own compute" budget precisely for
//! cases like this) and `AttachRing` + `Generate` (FR-KNO-03's grounded
//! answer, streamed through the memfd ring C4 sends back via `SCM_RIGHTS`).
use std::path::PathBuf;
use std::time::{Duration, Instant};

use neuroos_ipc::{
    DEFAULT_MAX_FRAME, connect, read_envelope_deadline, read_envelope_with_fd_deadline,
    write_envelope_deadline,
};
use neuroos_proto::v1::{
    AttachRingRequest, DistillRequest, Envelope, GenerateRequest, GetInfoRequest, Lane, envelope,
};
use neuroos_shm::{FLAG_EOS, Ring};
use tokio::net::UnixStream;

/// rules.md §5.7: control calls (GetInfo, AttachRing) get the default
/// 250ms; C4 generation gets 30s.
pub const CONTROL_DEADLINE: Duration = Duration::from_millis(250);
pub const GENERATE_DEADLINE: Duration = Duration::from_secs(30);
/// BUG-005: pure greedy decoding (the old hardcoded `0.0`) reliably
/// degenerates into a repeated-token loop on real, longer BitNet 2B
/// prompts. Non-zero but still low: FR-KNO-03's answers should stay close
/// to the grounded evidence, not get creative.
pub const DEFAULT_TEMPERATURE: f32 = 0.7;
/// BUG-005: llama.cpp's own CLI default (`--repeat-penalty`) for the same
/// reason -- 1.0 disables it; the old hardcoded call passed no penalty at
/// all.
pub const DEFAULT_REPETITION_PENALTY: f32 = 1.1;
/// FR-KNO-05: distillation has a "≈6.0 s budget"; this leaves slack above
/// that rather than matching it exactly.
pub const DISTILL_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum InferenceClientError {
    #[error("failed to connect to inference.sock at {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("inference.sock request failed: {0}")]
    Io(#[from] neuroos_ipc::FramingError),
    #[error("inference.sock closed the connection with no response")]
    NoResponse,
    #[error("inference.sock returned an error: {0}")]
    Remote(String),
    #[error("inference.sock sent an unexpected response type")]
    UnexpectedResponse,
    #[error("AttachRing response carried no fd")]
    NoRingFd,
    #[error("ring error: {0}")]
    Ring(#[from] neuroos_shm::RingError),
    #[error("generation did not finish before the deadline")]
    GenerationTimedOut,
}

fn envelope_req(body: envelope::Body) -> Envelope {
    Envelope {
        schema_version: 1,
        trace_id: String::new(),
        request_id: 1,
        sent_at_ns: neuroos_common::now_ns(),
        body: Some(body),
    }
}

#[derive(Clone)]
pub struct InferenceClient {
    socket_path: PathBuf,
}

impl InferenceClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    async fn connect(&self) -> Result<UnixStream, InferenceClientError> {
        connect(&self.socket_path, CONTROL_DEADLINE)
            .await
            .map_err(|source| InferenceClientError::Connect {
                path: self.socket_path.clone(),
                source,
            })
    }

    /// FR-KNO-04/06: counts tokens for `text` using C4's own tokenizer.
    pub async fn count_tokens(&self, text: &str) -> Result<u32, InferenceClientError> {
        let mut stream = self.connect().await?;
        let req = envelope_req(envelope::Body::GetInfoRequest(GetInfoRequest {
            tokenize_text: text.to_string(),
        }));
        write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, CONTROL_DEADLINE).await?;
        let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, CONTROL_DEADLINE)
            .await?
            .ok_or(InferenceClientError::NoResponse)?;
        match resp.body {
            Some(envelope::Body::GetInfoResponse(r)) => Ok(r.token_count),
            Some(envelope::Body::Error(e)) => Err(InferenceClientError::Remote(e.message)),
            _ => Err(InferenceClientError::UnexpectedResponse),
        }
    }

    /// Attaches a fresh named ring (created on first use, per
    /// `AttachRingRequest`'s doc comment) and receives its memfd via
    /// `SCM_RIGHTS`.
    async fn attach_ring(&self, ring_name: &str) -> Result<Ring, InferenceClientError> {
        let mut stream = self.connect().await?;
        let req = envelope_req(envelope::Body::AttachRingRequest(AttachRingRequest {
            ring_name: ring_name.to_string(),
            capacity_slots: 0, // 0 = engine default
            slot_size: 0,      // 0 = engine default
        }));
        write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, CONTROL_DEADLINE).await?;
        let (resp, fd) =
            read_envelope_with_fd_deadline(&stream, DEFAULT_MAX_FRAME, CONTROL_DEADLINE)
                .await?
                .ok_or(InferenceClientError::NoResponse)?;
        match resp.body {
            Some(envelope::Body::AttachRingResponse(r)) if r.ok => {
                let fd = fd.ok_or(InferenceClientError::NoRingFd)?;
                Ok(Ring::open(fd)?)
            }
            Some(envelope::Body::AttachRingResponse(r)) => {
                Err(InferenceClientError::Remote(r.error))
            }
            Some(envelope::Body::Error(e)) => Err(InferenceClientError::Remote(e.message)),
            _ => Err(InferenceClientError::UnexpectedResponse),
        }
    }

    /// FR-KNO-03: attaches `ring_name`, starts generation on the
    /// interactive lane, and collects every token piece into one string
    /// once the stream signals `FLAG_EOS` -- P5's first cut is
    /// request/response, not the token-by-token progress stream
    /// `knowledge.sock`'s socket-map entry ultimately allows for; that's a
    /// fast-follow once a streaming CLI consumer exists.
    pub async fn generate(
        &self,
        ring_name: &str,
        prompt: &str,
        max_tokens: u32,
    ) -> Result<String, InferenceClientError> {
        self.generate_with_sampling(
            ring_name,
            prompt,
            max_tokens,
            DEFAULT_TEMPERATURE,
            DEFAULT_REPETITION_PENALTY,
        )
        .await
    }

    /// Same as [`Self::generate`], but with `temperature`/`repetition_penalty`
    /// configurable per call (BUG-005) instead of `generate`'s sane BitNet
    /// 2B defaults.
    pub async fn generate_with_sampling(
        &self,
        ring_name: &str,
        prompt: &str,
        max_tokens: u32,
        temperature: f32,
        repetition_penalty: f32,
    ) -> Result<String, InferenceClientError> {
        let ring = self.attach_ring(ring_name).await?;
        let mut stream = self.connect().await?;
        let req = envelope_req(envelope::Body::GenerateRequest(GenerateRequest {
            prompt: prompt.to_string(),
            max_tokens,
            lane: Lane::Interactive as i32,
            ring_name: ring_name.to_string(),
            temperature,
            seed: 0,
            grammar_gbnf: String::new(),
            repetition_penalty,
        }));
        write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, CONTROL_DEADLINE).await?;
        let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, CONTROL_DEADLINE)
            .await?
            .ok_or(InferenceClientError::NoResponse)?;
        match resp.body {
            Some(envelope::Body::GenerateResponse(r)) if r.accepted => {}
            Some(envelope::Body::GenerateResponse(r)) => {
                return Err(InferenceClientError::Remote(r.error));
            }
            Some(envelope::Body::Error(e)) => return Err(InferenceClientError::Remote(e.message)),
            _ => return Err(InferenceClientError::UnexpectedResponse),
        }
        read_all_tokens(ring, max_tokens, GENERATE_DEADLINE).await
    }

    /// FR-KNO-05: distills `chunks` on C4's background lane (implicit in
    /// `DistillRequest` -- unlike `Generate`, it has no `lane` field: it
    /// always runs on `LANE_BACKGROUND` per the proto's own doc comment).
    /// Callers that must not block on this (the hot path never should,
    /// rules.md AB-11) call it inside their own `tokio::spawn`, not here.
    pub async fn distill(
        &self,
        ring_name: &str,
        chunks: Vec<String>,
        max_tokens: u32,
    ) -> Result<String, InferenceClientError> {
        let ring = self.attach_ring(ring_name).await?;
        let mut stream = self.connect().await?;
        let req = envelope_req(envelope::Body::DistillRequest(DistillRequest {
            chunks,
            max_tokens,
            ring_name: ring_name.to_string(),
        }));
        write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, CONTROL_DEADLINE).await?;
        let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, CONTROL_DEADLINE)
            .await?
            .ok_or(InferenceClientError::NoResponse)?;
        match resp.body {
            Some(envelope::Body::DistillResponse(r)) if r.accepted => {}
            Some(envelope::Body::DistillResponse(r)) => {
                return Err(InferenceClientError::Remote(r.error));
            }
            Some(envelope::Body::Error(e)) => return Err(InferenceClientError::Remote(e.message)),
            _ => return Err(InferenceClientError::UnexpectedResponse),
        }
        read_all_tokens(ring, max_tokens, DISTILL_DEADLINE).await
    }
}

/// The ring's `try_read()` is a raw, non-blocking poll (Architecture.md
/// §5.5: a seqlock, not a waker-based channel) -- exactly how
/// `neuroos-shm`'s own `bin/shm_reader.rs` interop tool consumes it. Run
/// off the async runtime via `spawn_blocking` so the tight poll loop can't
/// starve other tasks; a short sleep between empty polls (token generation
/// takes tens of ms per Phase 2's own measurements, so 1ms polling is
/// plenty responsive without truly spin-looping a whole thread).
///
/// `max_tokens` is also a stop condition, not just `FLAG_EOS`: C4's own
/// `engine.cpp` only sets `FLAG_EOS` when the model's sampler naturally
/// hits its end-of-generation token (`llama_vocab_is_eog`) -- reaching
/// `max_tokens` first writes no end-of-stream signal of any kind, so a
/// reader that trusted `FLAG_EOS` alone would hang until `deadline` on
/// every capped-length generation. `tests/contract/cpp_inference_smoke.cpp`
/// already works around this the same way (`pieces >= 8`); see memory.md's
/// tech debt register for the real fix (C4 should write FLAG_EOS when
/// max_tokens is reached too).
async fn read_all_tokens(
    ring: Ring,
    max_tokens: u32,
    deadline: Duration,
) -> Result<String, InferenceClientError> {
    tokio::task::spawn_blocking(move || {
        let mut reader = ring.reader();
        let mut text = String::new();
        let mut pieces = 0u32;
        let start = Instant::now();
        loop {
            match reader.try_read() {
                Some(piece) => {
                    text.push_str(&String::from_utf8_lossy(&piece.payload));
                    pieces += 1;
                    if piece.flags & FLAG_EOS != 0 || pieces >= max_tokens {
                        return Ok(text);
                    }
                }
                None => {
                    if start.elapsed() > deadline {
                        return Err(InferenceClientError::GenerationTimedOut);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    })
    .await
    .map_err(|_| InferenceClientError::GenerationTimedOut)?
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[tokio::test]
    async fn connect_failure_is_a_clear_error_not_a_hang() {
        let client = InferenceClient::new(PathBuf::from(
            "/tmp/neuroos-knowledge-query-test-nonexistent-inference.sock",
        ));
        let err = client.count_tokens("hello").await.unwrap_err();
        assert!(matches!(err, InferenceClientError::Connect { .. }));
    }
}
