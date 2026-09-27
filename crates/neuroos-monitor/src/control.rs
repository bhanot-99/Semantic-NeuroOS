//! `monitor.control.sock`: request/response control channel for
//! `neuroosctl pause`/`resume`/`status` (FR-PRV-01), kept separate from
//! `monitor.sock`'s push-event stream (Architecture.md §5.2).
use std::path::PathBuf;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{
    Envelope, Error, ErrorCode, MonitorPauseResponse, MonitorStatusResponse, envelope,
};
use tokio::net::UnixStream;

use crate::bus::EventBus;
use crate::privacy::PrivacyState;

pub async fn serve(path: PathBuf, allowed_uids: Vec<u32>, privacy: PrivacyState, bus: EventBus) {
    let server = match UdsServer::bind(UdsServerConfig::new(path, allowed_uids)) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to bind monitor.control.sock");
            return;
        }
    };
    loop {
        match server.accept().await {
            Ok(Some((stream, _cred))) => {
                tokio::spawn(handle_conn(stream, privacy.clone(), bus.clone()));
            }
            Ok(None) => continue, // rejected peer (SO_PEERCRED not in allowlist); keep serving
            Err(e) => {
                tracing::warn!(error = %e, "monitor.control.sock accept failed");
            }
        }
    }
}

async fn handle_conn(mut stream: UnixStream, privacy: PrivacyState, bus: EventBus) {
    loop {
        let env = match read_envelope(&mut stream, DEFAULT_MAX_FRAME).await {
            Ok(Some(env)) => env,
            Ok(None) => return, // peer closed cleanly
            Err(e) => {
                tracing::debug!(error = %e, "monitor.control.sock read failed");
                return;
            }
        };
        let response = handle_request(env, &privacy, &bus);
        if write_envelope(&mut stream, &response, DEFAULT_MAX_FRAME)
            .await
            .is_err()
        {
            return;
        }
    }
}

fn handle_request(env: Envelope, privacy: &PrivacyState, bus: &EventBus) -> Envelope {
    let now = neuroos_common::now_ns();
    let body = match env.body {
        Some(envelope::Body::MonitorPauseRequest(req)) => {
            if req.resume {
                privacy.resume();
            } else {
                let duration = (req.duration_s > 0).then_some(req.duration_s);
                privacy.pause(duration, now);
            }
            envelope::Body::MonitorPauseResponse(MonitorPauseResponse {
                paused: privacy.is_paused(now),
                paused_until_ns: privacy.paused_until_ns(),
            })
        }
        Some(envelope::Body::MonitorStatusRequest(_)) => {
            envelope::Body::MonitorStatusResponse(MonitorStatusResponse {
                paused: privacy.is_paused(now),
                paused_until_ns: privacy.paused_until_ns(),
                excluded_app_ids: privacy.excluded_app_ids(),
                subscriber_count: bus.subscriber_count() as u64,
                events_dropped_total: bus.total_dropped(),
            })
        }
        _ => envelope::Body::Error(Error {
            code: ErrorCode::InvalidArgument as i32,
            message: "unsupported request on monitor.control.sock".into(),
            retryable: false,
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_ipc::read_envelope;
    use neuroos_proto::v1::{MonitorPauseRequest, MonitorStatusRequest};

    #[tokio::test]
    async fn a_real_client_pauses_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("monitor.control.sock");
        let privacy = PrivacyState::new(Vec::new());
        let bus = EventBus::new(4);
        tokio::spawn(serve(
            path.clone(),
            vec![crate::current_uid()],
            privacy.clone(),
            bus,
        ));

        let mut client = wait_for_connect(&path).await;
        let req = request(envelope::Body::MonitorPauseRequest(MonitorPauseRequest {
            duration_s: 0,
            resume: false,
        }));
        write_envelope(&mut client, &req, DEFAULT_MAX_FRAME)
            .await
            .unwrap();
        let resp = read_envelope(&mut client, DEFAULT_MAX_FRAME)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            resp.body,
            Some(envelope::Body::MonitorPauseResponse(_))
        ));
        assert!(privacy.is_paused(neuroos_common::now_ns()));
    }

    async fn wait_for_connect(path: &std::path::Path) -> UnixStream {
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(path).await {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("monitor.control.sock never became connectable");
    }

    fn request(body: envelope::Body) -> Envelope {
        Envelope {
            schema_version: 1,
            trace_id: "t".into(),
            request_id: 1,
            sent_at_ns: 0,
            body: Some(body),
        }
    }

    #[test]
    fn pause_request_pauses_and_echoes_state() {
        let privacy = PrivacyState::new(Vec::new());
        let bus = EventBus::new(4);
        let resp = handle_request(
            request(envelope::Body::MonitorPauseRequest(MonitorPauseRequest {
                duration_s: 0,
                resume: false,
            })),
            &privacy,
            &bus,
        );
        match resp.body {
            Some(envelope::Body::MonitorPauseResponse(r)) => {
                assert!(r.paused);
                assert_eq!(r.paused_until_ns, u64::MAX);
            }
            other => panic!("expected MonitorPauseResponse, got {other:?}"),
        }
    }

    #[test]
    fn resume_request_clears_pause() {
        let privacy = PrivacyState::new(Vec::new());
        privacy.pause(None, 0);
        let bus = EventBus::new(4);
        let resp = handle_request(
            request(envelope::Body::MonitorPauseRequest(MonitorPauseRequest {
                duration_s: 0,
                resume: true,
            })),
            &privacy,
            &bus,
        );
        match resp.body {
            Some(envelope::Body::MonitorPauseResponse(r)) => assert!(!r.paused),
            other => panic!("expected MonitorPauseResponse, got {other:?}"),
        }
    }

    #[test]
    fn status_request_reports_exclusion_list_and_bus_counters() {
        let privacy = PrivacyState::new(["org.keepassxc.KeePassXC".to_string()]);
        let bus = EventBus::new(4);
        let _sub = bus.subscribe();
        let resp = handle_request(
            request(envelope::Body::MonitorStatusRequest(
                MonitorStatusRequest {},
            )),
            &privacy,
            &bus,
        );
        match resp.body {
            Some(envelope::Body::MonitorStatusResponse(r)) => {
                assert!(!r.paused);
                assert_eq!(
                    r.excluded_app_ids,
                    vec!["org.keepassxc.KeePassXC".to_string()]
                );
                assert_eq!(r.subscriber_count, 1);
            }
            other => panic!("expected MonitorStatusResponse, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_body_yields_an_error_response() {
        let privacy = PrivacyState::new(Vec::new());
        let bus = EventBus::new(4);
        let resp = handle_request(
            request(envelope::Body::Error(Error {
                code: ErrorCode::Internal as i32,
                message: "x".into(),
                retryable: false,
            })),
            &privacy,
            &bus,
        );
        assert!(matches!(resp.body, Some(envelope::Body::Error(_))));
    }
}
