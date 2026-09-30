from __future__ import annotations

import pytest

from neuroos_bg import main
from neuroos_bg.ipc import Edge, Entity


class FakeStorageClient:
    """Duck-types `ipc.StorageClient`'s surface -- `run_once`/`main` only
    ever call these four methods, so a fake avoids needing a real socket
    for this module's own logic (the wire format itself is `test_ipc.py`'s
    job)."""

    def __init__(self, entities: list[Entity], edges: list[Edge]) -> None:
        self._entities = entities
        self._edges = edges
        self.upserted: list[Edge] = []
        self.pruned_with: int | None = None

    def list_entities(self, since_ns: int = 0) -> list[Entity]:
        return self._entities

    def list_edges(self) -> list[Edge]:
        return self._edges

    def upsert_edge(self, edge: Edge) -> None:
        self.upserted.append(edge)

    def prune_edges(self, older_than_ns: int) -> int:
        self.pruned_with = older_than_ns
        return 2


def entity(id_: int, last_seen_ns: int) -> Entity:
    return Entity(
        id=id_,
        domain="window_focus",
        kind="window",
        label=f"app-{id_}",
        taint=0,
        created_ns=last_seen_ns,
        last_seen_ns=last_seen_ns,
        permanent=False,
    )


def test_run_once_writes_scored_edges_and_prunes() -> None:
    entities = [entity(1, 0), entity(2, 1_000_000_000)]  # 1s apart, co-occurring
    client = FakeStorageClient(entities, [])

    written, pruned = main.run_once(client)  # type: ignore[arg-type]

    assert written == 1
    assert pruned == 2
    assert len(client.upserted) == 1
    edge = client.upserted[0]
    assert edge.kind == main.CO_OCCURRENCE_KIND
    assert edge.hypothesis is True
    assert {edge.src, edge.dst} == {1, 2}


def test_run_once_with_no_entities_writes_nothing_but_still_prunes() -> None:
    client = FakeStorageClient([], [])
    written, pruned = main.run_once(client)  # type: ignore[arg-type]
    assert written == 0
    assert pruned == 2
    assert client.pruned_with is not None


def test_main_once_flag_runs_a_single_iteration_and_returns(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = FakeStorageClient([], [])
    monkeypatch.setattr(main, "StorageClient", lambda: client)
    exit_code = main.main(["--once"])
    assert exit_code == 0
    assert client.pruned_with is not None


def test_main_job_failure_is_caught_not_raised(monkeypatch: pytest.MonkeyPatch) -> None:
    from neuroos_bg.ipc import IpcError

    class BrokenClient:
        def list_entities(self, since_ns: int = 0) -> list[Entity]:
            raise IpcError("storage.sock unreachable")

    monkeypatch.setattr(main, "StorageClient", lambda: BrokenClient())
    exit_code = main.main(["--once"])  # must not raise
    assert exit_code == 0
