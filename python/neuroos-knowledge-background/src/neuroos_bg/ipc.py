"""IPC client to `storage.sock` (Architecture.md §5.1/§5.2).

AB-1 forbids this worker linking `neuroos-storage` (or touching
`meta.sqlite3` directly) -- every read/write onto the graph goes over this
socket, same u32-LE length-prefixed protobuf `Envelope` framing every other
component uses (`neuroos-ipc::framing`, mirrored here by hand since Python
has no shared IPC crate).
"""

from __future__ import annotations

import os
import socket
import time
from dataclasses import dataclass
from pathlib import Path

from neuroos.v1 import envelope_pb2, storage_pb2

DEFAULT_MAX_FRAME = 4 * 1024 * 1024  # Architecture.md §5.1


class IpcError(RuntimeError):
    """Raised on a connect/framing/protocol failure talking to storage.sock."""


def runtime_dir() -> Path:
    """`$XDG_RUNTIME_DIR/neuroos/`, matching `neuroos_common::paths::runtime_dir`."""
    base = os.environ.get("XDG_RUNTIME_DIR")
    if base:
        return Path(base) / "neuroos"
    return Path(f"/run/user/{os.getuid()}/neuroos")


def storage_sock_path() -> Path:
    return runtime_dir() / "storage.sock"


def write_envelope(sock: socket.socket, env: envelope_pb2.Envelope) -> None:
    payload = env.SerializeToString(deterministic=True)
    if len(payload) > DEFAULT_MAX_FRAME:
        raise IpcError(f"frame of {len(payload)} bytes exceeds max frame size {DEFAULT_MAX_FRAME}")
    sock.sendall(len(payload).to_bytes(4, "little") + payload)


def _recv_exact(sock: socket.socket, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise IpcError("connection closed mid-frame")
        buf.extend(chunk)
    return bytes(buf)


def read_envelope(sock: socket.socket) -> envelope_pb2.Envelope:
    len_bytes = _recv_exact(sock, 4)
    length = int.from_bytes(len_bytes, "little")
    if length > DEFAULT_MAX_FRAME:
        raise IpcError(f"frame of {length} bytes exceeds max frame size {DEFAULT_MAX_FRAME}")
    payload = _recv_exact(sock, length)
    env = envelope_pb2.Envelope()
    env.ParseFromString(payload)
    return env


def now_ns() -> int:
    return time.time_ns()


@dataclass(frozen=True)
class Entity:
    id: int
    domain: str
    kind: str
    label: str
    taint: int
    created_ns: int
    last_seen_ns: int
    permanent: bool


@dataclass(frozen=True)
class Edge:
    src: int
    dst: int
    kind: str
    weight: float
    reinforced_ns: int
    hypothesis: bool


class StorageClient:
    """One TCP-like request/response round trip per call, over a fresh
    connection each time -- the cold worker's job loop runs every few
    minutes, not a hot path, so there's no reason to hold a socket open
    between jobs (mirrors the simplicity of the Rust clients' own
    connect-per-call style, e.g. `kernel_client.rs`)."""

    def __init__(self, socket_path: Path | None = None, timeout_s: float = 5.0) -> None:
        self.socket_path = socket_path or storage_sock_path()
        self.timeout_s = timeout_s

    def _connect(self) -> socket.socket:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(self.timeout_s)
        try:
            sock.connect(str(self.socket_path))
        except OSError as e:
            sock.close()
            raise IpcError(f"failed to connect to storage.sock at {self.socket_path}: {e}") from e
        return sock

    def _request(self, body_field: str, message: object) -> envelope_pb2.Envelope:
        req = envelope_pb2.Envelope(schema_version=1, request_id=1, sent_at_ns=now_ns())
        getattr(req, body_field).CopyFrom(message)
        sock = self._connect()
        try:
            write_envelope(sock, req)
            return read_envelope(sock)
        finally:
            sock.close()

    def list_entities(self, since_ns: int = 0) -> list[Entity]:
        resp = self._request(
            "list_entities_request", storage_pb2.ListEntitiesRequest(since_ns=since_ns)
        )
        if resp.WhichOneof("body") != "list_entities_response":
            raise IpcError(f"unexpected response to ListEntities: {resp.WhichOneof('body')}")
        return [
            Entity(
                id=e.id,
                domain=e.domain,
                kind=e.kind,
                label=e.label,
                taint=e.taint,
                created_ns=e.created_ns,
                last_seen_ns=e.last_seen_ns,
                permanent=e.permanent,
            )
            for e in resp.list_entities_response.entities
        ]

    def list_edges(self) -> list[Edge]:
        resp = self._request("list_edges_request", storage_pb2.ListEdgesRequest())
        if resp.WhichOneof("body") != "list_edges_response":
            raise IpcError(f"unexpected response to ListEdges: {resp.WhichOneof('body')}")
        return [
            Edge(
                src=e.src,
                dst=e.dst,
                kind=e.kind,
                weight=e.weight,
                reinforced_ns=e.reinforced_ns,
                hypothesis=e.hypothesis,
            )
            for e in resp.list_edges_response.edges
        ]

    def upsert_edge(self, edge: Edge) -> None:
        req = storage_pb2.UpsertEdgeRequest(
            edge=storage_pb2.EdgeRow(
                src=edge.src,
                dst=edge.dst,
                kind=edge.kind,
                weight=edge.weight,
                reinforced_ns=edge.reinforced_ns,
                hypothesis=edge.hypothesis,
            )
        )
        resp = self._request("upsert_edge_request", req)
        if resp.WhichOneof("body") != "upsert_edge_response":
            raise IpcError(f"unexpected response to UpsertEdge: {resp.WhichOneof('body')}")

    def prune_edges(self, older_than_ns: int) -> int:
        resp = self._request(
            "prune_edges_request", storage_pb2.PruneEdgesRequest(older_than_ns=older_than_ns)
        )
        if resp.WhichOneof("body") != "prune_edges_response":
            raise IpcError(f"unexpected response to PruneEdges: {resp.WhichOneof('body')}")
        return int(resp.prune_edges_response.pruned)
