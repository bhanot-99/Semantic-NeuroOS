// rules.md §5 exempts these lints for one-off diagnostic tools: crashing loudly
// on unexpected input IS the correct behavior for a spike/interop CLI tool,
// unlike a long-running service.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// P0-S07 cross-language interop tool: opens a ring by path (a /proc/<pid>/fd/<n>
// magic symlink announced by the writer), reads `count` messages, verifies
// each is the expected "token-N" content in order, prints OK/FAIL.
use std::fs::OpenOptions;
use std::os::fd::OwnedFd;

use neuroos_shm::Ring;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let count: u32 = args[2].parse().unwrap();

    // read+write, not just read: the mapping below is PROT_READ|PROT_WRITE
    // (the ring doesn't distinguish reader/writer mmap permissions; real
    // production fd handoff is SCM_RIGHTS of the original read-write memfd,
    // not a re-open through /proc, so this quirk is specific to this
    // same-host interop test's fd-sharing trick).
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open the writer's announced fd path");
    let fd: OwnedFd = file.into();
    let ring = Ring::open(fd).expect("open ring");
    let mut reader = ring.reader();

    let mut received = 0u32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while received < count {
        match reader.try_read() {
            Some(piece) => {
                let expected = format!("token-{received}");
                if piece.token_id != received || piece.payload != expected.as_bytes() {
                    println!(
                        "FAIL expected token_id={received} payload={expected:?}, got token_id={} payload={:?}",
                        piece.token_id,
                        String::from_utf8_lossy(&piece.payload)
                    );
                    std::process::exit(1);
                }
                received += 1;
            }
            None => {
                if std::time::Instant::now() > deadline {
                    println!("FAIL timed out after {received}/{count} messages");
                    std::process::exit(1);
                }
                std::hint::spin_loop();
            }
        }
    }
    println!("OK {received}");
}
