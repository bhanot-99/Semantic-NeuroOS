"""Real-socket proof of `ipc.py`'s framing + protobuf wire format -- the
same u32-LE length-prefixed `Envelope` scheme `neuroos-storage`'s real
`storage.sock` server speaks (Rust side: `crates/neuroos-storage/src/
server.rs`). This drives `StorageClient` against a small fake server
speaking that exact wire format over a real `AF_UNIX` socket, proving the
framing/encoding round-trips correctly without needing a running,
model-loaded `neuroos-storage` process.
"""

from __future__ import annotations

import socket
import threading
from collections.abc import Callable
from pathlib import Path

import pytest

from neuroos.v1 import envelope_pb2
from neuroos_bg import ipc
from neuroos_bg.ipc import Edge, IpcError, StorageClient


def _serve_one(
    sock_path: Path,
    respond: Callable[[envelope_pb2.Envelope], envelope_pb2.Envelope],
) -> threading.Thread:
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(sock_path))
    server.listen(1)

    def _run() -> None:
        conn, _ = server.accept()
        try:
            req = ipc.read_envelope(conn)
            ipc.write_envelope(conn, respond(req))
        finally:
            conn.close()
            server.close()

    thread = threading.Thread(target=_run, daemon=True)
    thread.start()
    return thread


def test_list_entities_round_trips_over_a_real_unix_socket(tmp_path: Path) -> None:
    sock_path = tmp_path / "storage.sock"

    def respond(req: envelope_pb2.Envelope) -> envelope_pb2.Envelope:
        assert req.WhichOneof("body") == "list_entities_request"
        assert req.list_entities_request.since_ns == 42
        resp = envelope_pb2.Envelope(schema_version=1, request_id=req.request_id)
        resp.list_entities_response.entities.add(
            id=7,
            domain="window_focus",
            kind="window",
            label="firefox",
            taint=0,
            created_ns=1,
            last_seen_ns=2,
            permanent=False,
        )
        return resp

    thread = _serve_one(sock_path, respond)
    client = StorageClient(socket_path=sock_path, timeout_s=2.0)
    entities = client.list_entities(since_ns=42)
    thread.join(timeout=2.0)

    assert len(entities) == 1
    assert entities[0].id == 7
    assert entities[0].label == "firefox"


def test_list_edges_round_trips_over_a_real_unix_socket(tmp_path: Path) -> None:
    sock_path = tmp_path / "storage.sock"

    def respond(req: envelope_pb2.Envelope) -> envelope_pb2.Envelope:
        assert req.WhichOneof("body") == "list_edges_request"
        resp = envelope_pb2.Envelope(schema_version=1, request_id=req.request_id)
        resp.list_edges_response.edges.add(
            src=1, dst=2, kind="co_occurs", weight=1.5, reinforced_ns=99, hypothesis=True
        )
        return resp

    thread = _serve_one(sock_path, respond)
    client = StorageClient(socket_path=sock_path, timeout_s=2.0)
    edges = client.list_edges()
    thread.join(timeout=2.0)

    assert edges == [
        Edge(
            src=1,
            dst=2,
            kind="co_occurs",
            weight=pytest.approx(1.5),
            reinforced_ns=99,
            hypothesis=True,
        )
    ]


def test_upsert_edge_sends_every_field(tmp_path: Path) -> None:
    sock_path = tmp_path / "storage.sock"
    sent_edge = Edge(src=3, dst=4, kind="co_occurs", weight=2.5, reinforced_ns=123, hypothesis=True)

    def respond(req: envelope_pb2.Envelope) -> envelope_pb2.Envelope:
        assert req.WhichOneof("body") == "upsert_edge_request"
        edge = req.upsert_edge_request.edge
        assert (edge.src, edge.dst, edge.kind, edge.reinforced_ns, edge.hypothesis) == (
            3,
            4,
            "co_occurs",
            123,
            True,
        )
        assert edge.weight == pytest.approx(2.5)
        resp = envelope_pb2.Envelope(schema_version=1, request_id=req.request_id)
        resp.upsert_edge_response.ok = True
        return resp

    thread = _serve_one(sock_path, respond)
    client = StorageClient(socket_path=sock_path, timeout_s=2.0)
    client.upsert_edge(sent_edge)  # must not raise
    thread.join(timeout=2.0)


def test_prune_edges_returns_the_pruned_count(tmp_path: Path) -> None:
    sock_path = tmp_path / "storage.sock"

    def respond(req: envelope_pb2.Envelope) -> envelope_pb2.Envelope:
        assert req.WhichOneof("body") == "prune_edges_request"
        assert req.prune_edges_request.older_than_ns == 55
        resp = envelope_pb2.Envelope(schema_version=1, request_id=req.request_id)
        resp.prune_edges_response.pruned = 3
        return resp

    thread = _serve_one(sock_path, respond)
    client = StorageClient(socket_path=sock_path, timeout_s=2.0)
    pruned = client.prune_edges(older_than_ns=55)
    thread.join(timeout=2.0)

    assert pruned == 3


def test_connect_failure_raises_a_clear_ipc_error(tmp_path: Path) -> None:
    client = StorageClient(socket_path=tmp_path / "nonexistent.sock", timeout_s=1.0)
    with pytest.raises(IpcError, match="failed to connect"):
        client.list_entities()


def test_unexpected_response_type_raises_a_clear_ipc_error(tmp_path: Path) -> None:
    sock_path = tmp_path / "storage.sock"

    def respond(req: envelope_pb2.Envelope) -> envelope_pb2.Envelope:
        resp = envelope_pb2.Envelope(schema_version=1, request_id=req.request_id)
        resp.list_edges_response.SetInParent()  # wrong response type on purpose
        return resp

    thread = _serve_one(sock_path, respond)
    client = StorageClient(socket_path=sock_path, timeout_s=2.0)
    with pytest.raises(IpcError, match="unexpected response"):
        client.list_entities()
    thread.join(timeout=2.0)


def test_runtime_dir_respects_xdg_runtime_dir(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/tmp/example")
    assert ipc.runtime_dir() == Path("/tmp/example/neuroos")
    assert ipc.storage_sock_path() == Path("/tmp/example/neuroos/storage.sock")
