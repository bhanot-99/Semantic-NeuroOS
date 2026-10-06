//! `monitor.sock`: the server-push `RawTelemetryEvent` stream (Architecture.md
//! §5.2). Every connected client (C3) gets its own [`EventSubscriber`], so a
//! slow client only drops its own backlog (see `bus.rs`'s doc comment).
use std::path::PathBuf;

use neuroos_ipc::{
    ConnectionPermit, DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, write_envelope,
};
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
            Ok(Some((stream, cred, permit))) => {
                tracing::debug!(uid = cred.uid, "monitor.sock subscriber connected");
                tokio::spawn(handle_subscriber(stream, bus.subscribe(), permit));
            }
            Ok(None) => continue, // this one connection failed; keep serving
            Err(e) => {
                // M2: `accept` reports only an unusable listening socket as
                // an error now, so retrying would spin at full CPU forever.
                tracing::error!(error = %e, "monitor.sock listener is unusable; stopped serving");
                return;
            }
        }
    }
}

/// L2: this socket is server-push -- a subscriber never sends a request,
/// so there is no idle *read* to time out and the connection cap is the
/// whole of its back pressure. The `_permit` is dropped when the
/// subscriber goes away, freeing its slot.
async fn handle_subscriber(
    mut stream: UnixStream,
    mut sub: crate::bus::EventSubscriber,
    _permit: ConnectionPermit,
) {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_ipc::read_envelope;
    use neuroos_proto::v1::raw_telemetry_event::Payload;
    use neuroos_proto::v1::{IdleEvent, RawTelemetryEvent};

    #[tokio::test]
    async fn a_connected_client_receives_a_published_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("monitor.sock");
        let bus = EventBus::new(8);
        tokio::spawn(serve(path.clone(), vec![current_test_uid()], bus.clone()));

        let mut client = wait_for_connect(&path).await;
        // let the accept loop actually register the subscriber before
        // publishing, or the event is published before anyone's listening.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        bus.publish(RawTelemetryEvent {
            observed_at_ns: 42,
            source: "test".into(),
            payload: Some(Payload::Idle(IdleEvent { idle: true })),
        });

        let env = read_envelope(&mut client, DEFAULT_MAX_FRAME)
            .await
            .unwrap()
            .unwrap();
        match env.body {
            Some(envelope::Body::Telemetry(t)) => assert_eq!(t.observed_at_ns, 42),
            other => panic!("expected Telemetry, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn two_clients_each_get_their_own_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("monitor.sock");
        let bus = EventBus::new(8);
        tokio::spawn(serve(path.clone(), vec![current_test_uid()], bus.clone()));

        let mut a = wait_for_connect(&path).await;
        let mut b = wait_for_connect(&path).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        bus.publish(RawTelemetryEvent {
            observed_at_ns: 7,
            source: "test".into(),
            payload: Some(Payload::Idle(IdleEvent { idle: false })),
        });

        for client in [&mut a, &mut b] {
            let env = read_envelope(client, DEFAULT_MAX_FRAME)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(env.body, Some(envelope::Body::Telemetry(_))));
        }
    }

    async fn wait_for_connect(path: &std::path::Path) -> UnixStream {
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(path).await {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("monitor.sock never became connectable");
    }

    fn current_test_uid() -> u32 {
        crate::current_uid()
    }
}
