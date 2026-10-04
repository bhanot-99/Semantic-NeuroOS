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
    DEFAULT_MAX_FRAME, connect_retrying, read_envelope_deadline, read_envelope_with_fd_deadline,
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
/// Greedy. BUG-005 moved this off 0.0 because greedy decoding looped,
/// but those loops came from the broken bitnet.cpp fork (BUG-007(d)); on
/// the repinned C4, greedy plus the repetition penalty below ends cleanly.
/// Deterministic answers also make KPI-1 runs comparable: at 0.2, the same
/// question flipped between right and "I do not know" from run to run.
pub const DEFAULT_TEMPERATURE: f32 = 0.0;
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
    /// M11: the blocking ring-reader task died (panicked, or the runtime
    /// shut down under it). Distinct from [`Self::GenerationTimedOut`],
    /// which it used to be reported as: a timeout means C4 was too slow,
    /// this means our own reader never ran to completion, and the two call
    /// for different operator action.
    #[error("the ring reader task did not complete")]
    ReaderTaskFailed,
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
        connect_retrying(&self.socket_path, CONTROL_DEADLINE)
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
        self.generate_inner(
            ring_name,
            prompt,
            max_tokens,
            temperature,
            repetition_penalty,
            GENERATE_DEADLINE,
        )
        .await
    }

    /// Same as [`Self::generate`] with an explicit read deadline instead of
    /// [`GENERATE_DEADLINE`]. Exists for callers whose own budget is
    /// shorter than C4's 30s worst case (and for the tests that have to
    /// provoke a timeout without waiting 30s for one).
    pub async fn generate_with_deadline(
        &self,
        ring_name: &str,
        prompt: &str,
        max_tokens: u32,
        deadline: Duration,
    ) -> Result<String, InferenceClientError> {
        self.generate_inner(
            ring_name,
            prompt,
            max_tokens,
            DEFAULT_TEMPERATURE,
            DEFAULT_REPETITION_PENALTY,
            deadline,
        )
        .await
    }

    async fn generate_inner(
        &self,
        ring_name: &str,
        prompt: &str,
        max_tokens: u32,
        temperature: f32,
        repetition_penalty: f32,
        deadline: Duration,
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
        let generation_id = match resp.body {
            Some(envelope::Body::GenerateResponse(r)) if r.accepted => r.generation_id,
            Some(envelope::Body::GenerateResponse(r)) => {
                return Err(InferenceClientError::Remote(r.error));
            }
            Some(envelope::Body::Error(e)) => return Err(InferenceClientError::Remote(e.message)),
            _ => return Err(InferenceClientError::UnexpectedResponse),
        };
        self.read_or_cancel(ring, generation_id, max_tokens, deadline)
            .await
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
        let generation_id = match resp.body {
            Some(envelope::Body::DistillResponse(r)) if r.accepted => r.generation_id,
            Some(envelope::Body::DistillResponse(r)) => {
                return Err(InferenceClientError::Remote(r.error));
            }
            Some(envelope::Body::Error(e)) => return Err(InferenceClientError::Remote(e.message)),
            _ => return Err(InferenceClientError::UnexpectedResponse),
        };
        self.read_or_cancel(ring, generation_id, max_tokens, DISTILL_DEADLINE)
            .await
    }

    /// FR-INF-05 / M11: reads `generation_id`'s stream and, if the read
    /// ends in anything other than a completed answer, tells C4 to stop.
    /// Without this, a client that gave up left C4 decoding to the end of
    /// `max_tokens` -- holding the interactive lane and the ring against
    /// the *next* request (H7's territory) for an answer nobody will read.
    async fn read_or_cancel(
        &self,
        ring: Ring,
        generation_id: u64,
        max_tokens: u32,
        deadline: Duration,
    ) -> Result<String, InferenceClientError> {
        let result = read_all_tokens(ring, generation_id, max_tokens, deadline).await;
        if result.is_err() {
            // Best-effort: we are already returning the read's error, and
            // the cancel's own failure must not replace it (rules.md
            // §5.6). Logged by type only (R0-6).
            if let Err(err) = self.cancel(generation_id).await {
                tracing::warn!(
                    generation_id,
                    error = %err,
                    "failed to cancel an abandoned generation"
                );
            }
        }
        result
    }

    /// Asks C4 to stop `generation_id` (`CancelRequest`, FR-INF-05: it
    /// stops within one decode step). `false` means C4 did not know the id
    /// -- already finished, or never started.
    pub async fn cancel(&self, generation_id: u64) -> Result<bool, InferenceClientError> {
        let mut stream = self.connect().await?;
        let req = envelope_req(envelope::Body::CancelRequest(
            neuroos_proto::v1::CancelRequest { generation_id },
        ));
        write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, CONTROL_DEADLINE).await?;
        let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, CONTROL_DEADLINE)
            .await?
            .ok_or(InferenceClientError::NoResponse)?;
        match resp.body {
            Some(envelope::Body::CancelResponse(r)) => Ok(r.cancelled),
            Some(envelope::Body::Error(e)) => Err(InferenceClientError::Remote(e.message)),
            _ => Err(InferenceClientError::UnexpectedResponse),
        }
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
/// Reads only `generation_id`'s slots (H7: the ring name is shared, so
/// another request's tokens can be in it too). C4 ends every job's stream
/// with a `FLAG_EOS` slot (H8: also at max_tokens, the context limit, on
/// cancel or failure); `max_tokens` stays a stop condition too, as a
/// bound in its own right -- a slot is not exactly a token (M12's
/// `write_split` spreads an over-wide piece across slots), so that bound
/// is a backstop against a ring that never reports an end, not a token
/// count.
async fn read_all_tokens(
    ring: Ring,
    generation_id: u64,
    max_tokens: u32,
    deadline: Duration,
) -> Result<String, InferenceClientError> {
    tokio::task::spawn_blocking(move || {
        let mut reader = ring.reader_for_generation(generation_id);
        // M11: the pieces are concatenated as *bytes* and decoded once at
        // the end. A piece is a slice of C4's detokenized output, not a
        // character boundary, so a multi-byte character can straddle two
        // slots (C4 also splits a piece wider than one slot's payload);
        // decoding each piece on its own turned both halves into U+FFFD.
        let mut bytes: Vec<u8> = Vec::new();
        let mut pieces = 0u32;
        let start = Instant::now();
        loop {
            match reader.try_read() {
                Some(piece) => {
                    bytes.extend_from_slice(&piece.payload);
                    pieces += 1;
                    if piece.flags & FLAG_EOS != 0 || pieces >= max_tokens {
                        return Ok(String::from_utf8_lossy(&bytes).into_owned());
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
    .map_err(|_| InferenceClientError::ReaderTaskFailed)?
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn current_uid() -> u32 {
        // SAFETY: getuid() takes no arguments and cannot fail.
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        unsafe { getuid() }
    }

    /// M11: C4 writes one ring slot per token *piece*, and a piece is a
    /// slice of the detokenized bytes -- a multi-byte character can land
    /// across two slots. `from_utf8_lossy` per piece turned each half into
    /// U+FFFD, so "日" arrived as "??". The bytes are now joined first and
    /// decoded once.
    #[tokio::test]
    async fn a_multi_byte_character_split_across_pieces_is_not_corrupted() {
        let ring = Ring::create("m11-utf8", 8, 64).unwrap();
        {
            let writer = ring.writer();
            let bytes = "日本".as_bytes();
            writer.write_as(7, 1, 0, &bytes[..2]).unwrap();
            writer.write_as(7, 2, 0, &bytes[2..5]).unwrap();
            writer.write_as(7, 3, FLAG_EOS, &bytes[5..]).unwrap();
        }
        let text = read_all_tokens(ring, 7, 16, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(text, "日本");
    }

    #[tokio::test]
    async fn pieces_that_are_whole_characters_still_decode() {
        let ring = Ring::create("m11-ascii", 8, 64).unwrap();
        {
            let writer = ring.writer();
            writer.write_as(1, 1, 0, b"hel").unwrap();
            writer.write_as(1, 2, FLAG_EOS, b"lo").unwrap();
        }
        let text = read_all_tokens(ring, 1, 16, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(text, "hello");
    }

    /// M11: when the read gives up, C4 is still decoding -- holding the
    /// interactive lane and burning CPU for an answer nobody will read.
    /// The proto has had `CancelRequest` since Phase 2; the client never
    /// sent it.
    #[tokio::test]
    async fn a_generation_that_times_out_is_cancelled_on_c4() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("inference.sock");
        let cancelled =
            neuroos_testkit::inference_mocks::spawn_stalling_generator(&sock, current_uid(), 4242);
        // Give the mock's bind a moment; connect() fails fast otherwise.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = InferenceClient::new(sock);
        let err = client
            .generate_with_deadline("ring-a", "hello", 8, Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(
            matches!(err, InferenceClientError::GenerationTimedOut),
            "unexpected error: {err}"
        );
        assert_eq!(
            *cancelled.lock().unwrap(),
            vec![4242],
            "C4 must be told to stop generating"
        );
    }

    #[tokio::test]
    async fn connect_failure_is_a_clear_error_not_a_hang() {
        let client = InferenceClient::new(PathBuf::from(
            "/tmp/neuroos-knowledge-query-test-nonexistent-inference.sock",
        ));
        let err = client.count_tokens("hello").await.unwrap_err();
        assert!(matches!(err, InferenceClientError::Connect { .. }));
    }
}
