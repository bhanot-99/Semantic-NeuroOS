// rules.md §5 exempts these lints for one-off diagnostic tools: crashing loudly
// on unexpected input IS the correct behavior for a spike/interop CLI tool,
// unlike a long-running service.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// P0-S07 cross-language interop tool: creates a ring, writes `count`
// sequential "token-N" messages, announces a /proc/self/fd/N path any
// process on this machine can open (no SCM_RIGHTS needed for this
// same-host spike proof; that's the real Phase 2 production mechanism),
// then blocks until the reader is done so the memfd stays alive.
use std::io::{BufRead, Write};
use std::os::fd::AsRawFd;

use neuroos_shm::Ring;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let capacity: u32 = args[1].parse().unwrap();
    let slot_size: u32 = args[2].parse().unwrap();
    let count: u32 = args[3].parse().unwrap();

    let ring = Ring::create("neuroos-shm-interop", capacity, slot_size).unwrap();
    let writer = ring.writer();
    for token_id in 0..count {
        let payload = format!("token-{token_id}");
        writer.write(token_id, 0, payload.as_bytes()).unwrap();
    }

    let pid = std::process::id();
    let fd = ring.fd().as_raw_fd();
    println!("READY /proc/{pid}/fd/{fd}");
    std::io::stdout().flush().unwrap();

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
}
