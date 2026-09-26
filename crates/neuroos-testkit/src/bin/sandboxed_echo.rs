// rules.md §5 exempts these lints for one-off diagnostic tools: crashing loudly
// on unexpected input IS the correct behavior for a spike/interop CLI tool,
// unlike a long-running service.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Phase 0 exit criterion (phases.md §3.4): "a sandboxed echo unit proves:
// no network, UDS works, peer UID enforced." Meant to run inside a real
// systemd unit with PrivateNetwork=true (see tests/contract/sandboxed_echo.sh
// for the systemd-run invocation), not just as a plain test process.
//
// On startup: (1) proves the network really is unreachable from inside this
// sandbox (a raw TCP connect attempt to 1.1.1.1:443 must fail); (2) binds a
// UDS echo server and reports SO_PEERCRED for each connection, so an outside
// client can prove the socket still works and the peer UID is correct.
use std::time::Duration;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_frame, write_frame};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sock_path = &args[1];
    let allowed_uid: u32 = args[2].parse().unwrap();

    match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect("1.1.1.1:443"),
    )
    .await
    {
        Ok(Ok(_)) => {
            println!("NETWORK_REACHABLE (unexpected — sandbox is not isolating network)");
            std::process::exit(1);
        }
        _ => println!("NETWORK_BLOCKED"),
    }

    let server = UdsServer::bind(UdsServerConfig::new(sock_path, vec![allowed_uid])).unwrap();
    println!("READY");
    use std::io::Write;
    std::io::stdout().flush().unwrap();

    let Some((mut stream, cred)) = server.accept().await.unwrap() else {
        println!("PEER_REJECTED");
        return;
    };
    println!(
        "PEER_UID={} (allowed={})",
        cred.uid,
        cred.uid == allowed_uid
    );

    if let Some(frame) = read_frame(&mut stream, DEFAULT_MAX_FRAME).await.unwrap() {
        write_frame(&mut stream, &frame, DEFAULT_MAX_FRAME)
            .await
            .unwrap();
        println!("ECHO_OK");
    }
}
