//! `monitor.sock`: the server-push `RawTelemetryEvent` stream (Architecture.md
//! §5.2). Every connected client (C3) gets its own [`EventSubscriber`], so a
//! slow client only drops its own backlog (see `bus.rs`'s doc comment).
use std::path::PathBuf;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, write_envelope};
use neuroos_proto::v1::{Envelope, envelope};
use tokio::net::UnixStream;

use crate::bus::EventBus;

pub async fn serve(path: PathBuf, allowed_uids: Vec<u32>, bus: EventBus) {
    let server = match UdsServer::bind(UdsServerConfig::new(path, allowed_uids)) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to bind monitor.sock");
            return;
        }
    };
    loop {
        match server.accept().await {
            Ok(Some((stream, cred))) => {
                tracing::debug!(uid = cred.uid, "monitor.sock subscriber connected");
                tokio::spawn(handle_subscriber(stream, bus.subscribe()));
            }
            Ok(None) => continue, // rejected peer; keep serving
            Err(e) => tracing::warn!(error = %e, "monitor.sock accept failed"),
        }
    }
}

async fn handle_subscriber(mut stream: UnixStream, mut sub: crate::bus::EventSubscriber) {
    loop {
        let event = sub.recv().await;
        let env = Envelope {
            schema_version: 1,
            trace_id: String::new(),
            request_id: 0, // push event, not a request/response pair
            sent_at_ns: neuroos_common::now_ns(),
            body: Some(envelope::Body::Telemetry(event)),
        };
        if write_envelope(&mut stream, &env, DEFAULT_MAX_FRAME)
            .await
            .is_err()
        {
            return; // subscriber gone; its EventSubscriber drops here
        }
    }
}
