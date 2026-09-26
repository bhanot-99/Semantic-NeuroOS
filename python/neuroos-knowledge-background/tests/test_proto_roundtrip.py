# round-trips an Envelope through the generated neuroos.v1 python bindings
from neuroos.v1 import common_pb2, envelope_pb2


def test_envelope_error_roundtrip() -> None:
    env = envelope_pb2.Envelope(
        schema_version=1,
        trace_id="0123456789abcdef0123456789abcdef",
        request_id=42,
        sent_at_ns=1_700_000_000_000_000_000,
        error=envelope_pb2.Error(
            code=common_pb2.ERROR_CODE_INTERNAL,
            message="boom",
            retryable=True,
        ),
    )

    encoded = env.SerializeToString(deterministic=True)

    decoded = envelope_pb2.Envelope()
    decoded.ParseFromString(encoded)
    assert decoded == env

    re_encoded = decoded.SerializeToString(deterministic=True)
    assert encoded == re_encoded
