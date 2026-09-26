// integration test: rules.md §5's no-unwrap rule is scoped to non-test code;
// clippy's restriction lints don't auto-exempt files under tests/, so this is explicit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! P0-S07 spike S-02: proves the seqlock ring race-free under sustained
//! concurrent single-writer/single-reader access, in-process (two OS
//! threads sharing one `Ring`, same memory-safety guarantees a real
//! writer-process/reader-process pair gets over a shared memfd).
use std::sync::Arc;
use std::thread;

use neuroos_shm::Ring;

#[test]
fn basic_write_read_roundtrip() {
    let ring = Ring::create("neuroos-shm-test", 8, 64).unwrap();
    let writer = ring.writer();
    let mut reader = ring.reader();

    assert!(reader.try_read().is_none());
    writer.write(1, 0, b"hello").unwrap();
    let piece = reader.try_read().unwrap();
    assert_eq!(piece.token_id, 1);
    assert_eq!(piece.payload, b"hello");
    assert!(reader.try_read().is_none());
}

#[test]
fn payload_too_large_is_rejected() {
    let ring = Ring::create("neuroos-shm-test-toobig", 4, 40).unwrap();
    let err = ring.writer().write(1, 0, &[0u8; 64]).unwrap_err();
    assert!(matches!(
        err,
        neuroos_shm::RingError::PayloadTooLarge(64, _)
    ));
}

#[test]
fn cancel_makes_reader_skip_stale_generation_slots() {
    let ring = Ring::create("neuroos-shm-test-cancel", 8, 64).unwrap();
    let writer = ring.writer();
    let mut reader = ring.reader();

    writer.write(1, 0, b"old").unwrap();
    writer.cancel();
    writer.write(2, 0, b"new").unwrap();

    // the reader never sees the stale-generation "old" piece
    let piece = reader.try_read().unwrap();
    assert_eq!(piece.token_id, 2);
    assert_eq!(piece.payload, b"new");
    assert!(reader.try_read().is_none());
}

/// The core race-freedom proof: one writer thread, one reader thread, both
/// hammering the same ring concurrently for a large number of messages.
///
/// The ring is bounded and lossy under a slow reader by design (Architecture
/// .md §5.5: the reader resyncs rather than reading overwritten data), so a
/// fast writer racing a much slower reader (this test's reader does a
/// `format!` + `assert_eq!` per message) is *expected* to skip some
/// messages. What must never happen, no matter how the two threads
/// interleave: a corrupted payload, a `token_id` that goes backwards, or a
/// repeated `token_id` — any of those would mean the seqlock let a torn
/// read through.
#[test]
fn concurrent_writer_reader_no_torn_reads() {
    run_stress(if cfg!(debug_assertions) {
        200_000
    } else {
        2_000_000
    });
}

/// Full spike-S-02 scale (Architecture.md §5.5: "10 M messages"). Not run by
/// default (`just ci`/`just test`) — too slow for routine CI; run explicitly
/// with `cargo test -p neuroos-shm --release -- --ignored --nocapture`.
#[test]
#[ignore = "10M-message spike-scale run; see doc comment"]
fn concurrent_writer_reader_no_torn_reads_10m() {
    run_stress(10_000_000);
}

fn run_stress(messages: u32) {
    const CAPACITY: u32 = 256;
    const SLOT_SIZE: u32 = 128;

    let ring = Arc::new(Ring::create("neuroos-shm-stress", CAPACITY, SLOT_SIZE).unwrap());

    let writer_ring = ring.clone();
    let writer_handle = thread::spawn(move || {
        let writer = writer_ring.writer();
        for token_id in 0..messages {
            let payload = format!("token-{token_id}");
            writer
                .write(token_id, 0, payload.as_bytes())
                .expect("payload fits in a 128B slot");
        }
    });

    let reader_ring = ring.clone();
    let reader_handle = thread::spawn(move || {
        let mut reader = reader_ring.reader();
        let mut received: u32 = 0;
        let mut last_token_id: Option<u32> = None;
        let mut writer_done = false;
        loop {
            match reader.try_read() {
                Some(piece) => {
                    let expected = format!("token-{}", piece.token_id);
                    assert_eq!(
                        piece.payload,
                        expected.as_bytes(),
                        "corrupted payload for token_id {}",
                        piece.token_id
                    );
                    if let Some(last) = last_token_id {
                        assert!(
                            piece.token_id > last,
                            "token_id went backwards or repeated: {} after {}",
                            piece.token_id,
                            last
                        );
                    }
                    last_token_id = Some(piece.token_id);
                    received += 1;
                }
                // Caught up. If the writer has also finished, there is
                // nothing left to ever arrive; otherwise keep polling.
                None if writer_done => break,
                None => {
                    writer_done = WRITER_DONE.load(std::sync::atomic::Ordering::Acquire);
                    std::hint::spin_loop();
                }
            }
        }
        received
    });

    writer_handle.join().unwrap();
    WRITER_DONE.store(true, std::sync::atomic::Ordering::Release);
    let received = reader_handle.join().unwrap();

    assert!(received > 0, "reader observed zero messages");
    assert!(received <= messages);
    println!(
        "concurrent_writer_reader_no_torn_reads: {received}/{messages} messages observed intact"
    );
}

static WRITER_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
