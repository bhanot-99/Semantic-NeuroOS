# tests/contract/ cross-language proto round-trip helper (Python side).
# `roundtrip`: reads an Envelope from stdin, decodes it, re-encodes it to stdout.
import sys

from neuroos.v1 import envelope_pb2


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] != "roundtrip":
        print("usage: py_codec.py roundtrip (reads Envelope bytes on stdin)", file=sys.stderr)
        return 2

    data = sys.stdin.buffer.read()
    env = envelope_pb2.Envelope()
    env.ParseFromString(data)

    sys.stdout.buffer.write(env.SerializeToString(deterministic=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
