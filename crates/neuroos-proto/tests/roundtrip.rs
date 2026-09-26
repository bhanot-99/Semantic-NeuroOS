use neuroos_proto::v1::{Envelope, Error, ErrorCode, envelope};
use prost::Message;

#[test]
fn envelope_error_roundtrip() {
    let env = Envelope {
        schema_version: 1,
        trace_id: "0123456789abcdef0123456789abcdef".into(),
        request_id: 42,
        sent_at_ns: 1_700_000_000_000_000_000,
        body: Some(envelope::Body::Error(Error {
            code: ErrorCode::Internal as i32,
            message: "boom".into(),
            retryable: true,
        })),
    };

    let mut buf = Vec::new();
    env.encode(&mut buf).unwrap();

    let decoded = Envelope::decode(buf.as_slice()).unwrap();
    assert_eq!(env, decoded);

    let mut buf2 = Vec::new();
    decoded.encode(&mut buf2).unwrap();
    assert_eq!(
        buf, buf2,
        "re-encoding a decoded message must be byte-identical"
    );
}
