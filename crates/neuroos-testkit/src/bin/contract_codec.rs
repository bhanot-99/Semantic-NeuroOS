// tests/contract/ cross-language proto round-trip helper (Rust side).
// `encode-fixture`: writes the canonical test Envelope to stdout.
// `roundtrip`: reads an Envelope from stdin, decodes it, re-encodes it to stdout.
use std::io::{Read, Write};

use neuroos_proto::v1::{Envelope, Error, ErrorCode, envelope};
use prost::Message;

fn fixture() -> Envelope {
    Envelope {
        schema_version: 1,
        trace_id: "0123456789abcdef0123456789abcdef".into(),
        request_id: 42,
        sent_at_ns: 1_700_000_000_000_000_000,
        body: Some(envelope::Body::Error(Error {
            code: ErrorCode::Internal as i32,
            message: "boom".into(),
            retryable: true,
        })),
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let mut stdout = std::io::stdout();
    match mode.as_str() {
        "encode-fixture" => {
            let mut buf = Vec::new();
            fixture().encode(&mut buf).expect("encode fixture");
            stdout.write_all(&buf).expect("write stdout");
        }
        "roundtrip" => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf).expect("read stdin");
            let env = Envelope::decode(buf.as_slice()).expect("decode envelope");
            let mut out = Vec::new();
            env.encode(&mut out).expect("re-encode envelope");
            stdout.write_all(&out).expect("write stdout");
        }
        other => {
            eprintln!("usage: contract_codec <encode-fixture|roundtrip>, got {other:?}");
            std::process::exit(2);
        }
    }
}
