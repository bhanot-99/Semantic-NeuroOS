from __future__ import annotations

import pytest

from neuroos_bg import main
from neuroos_bg.ipc import Edge, Entity


@pytest.fixture(autouse=True)
def no_sandbox(monkeypatch: pytest.MonkeyPatch) -> None:
    """`main()` sandboxes the whole process (test_sandbox.py covers that in
    a subprocess); here it would lock pytest itself out of the filesystem."""
    monkeypatch.setattr(main, "enter_sandbox", lambda: None)


def test_main_fails_closed_when_the_sandbox_cannot_be_entered(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def refuse() -> None:
        raise main.sandbox.SandboxError("Landlock unavailable")

    monkeypatch.setattr(main, "enter_sandbox", refuse)
    monkeypatch.setattr(main, "StorageClient", lambda: pytest.fail("must not run unsandboxed"))
    assert main.main(["--once"]) == 1


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


HOUR = 3_600_000_000_000
T0 = 1_790_000_000_000_000_000  # a realistic 2026 UTC-ns instant


def test_run_once_writes_scored_edges_and_prunes() -> None:
    entities = [entity(1, T0), entity(2, T0 + 1_000_000_000)]  # 1s apart, co-occurring
    client = FakeStorageClient(entities, [])

    written, pruned = main.run_once(client, now_ns=T0 + HOUR)  # type: ignore[arg-type]

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


def test_main_survives_any_job_exception(monkeypatch: pytest.MonkeyPatch) -> None:
    """H13 / rules.md §5.9: a socket timeout, a reset connection, a bad
    protobuf frame -- or any other bug in one job run -- is logged and
    retried next cycle; it never kills the worker loop."""
    for exc in (TimeoutError("recv timed out"), ConnectionResetError(), ValueError("bad")):

        class BrokenClient:
            def list_entities(self, since_ns: int = 0, _exc: Exception = exc) -> list[Entity]:
                raise _exc

        monkeypatch.setattr(main, "StorageClient", lambda: BrokenClient())
        assert main.main(["--once"]) == 0


def test_a_pair_is_not_reinforced_again_without_a_new_co_occurrence() -> None:
    """H14: re-running the job must not count the same co-occurrence again."""
    client = FakeStorageClient([entity(1, T0), entity(2, T0 + 1_000_000_000)], [])
    main.run_once(client, now_ns=T0 + HOUR)  # type: ignore[arg-type]
    assert len(client.upserted) == 1
    first = client.upserted[0]
    assert first.reinforced_ns == T0 + 1_000_000_000, "stamped with the event, not the run"

    rerun = FakeStorageClient(client._entities, [first])
    written, _ = main.run_once(rerun, now_ns=T0 + 2 * HOUR)  # type: ignore[arg-type]
    assert written == 0
    assert rerun.upserted == []


def test_a_new_co_occurrence_reinforces_from_its_own_time() -> None:
    prior = Edge(
        src=1,
        dst=2,
        kind=main.CO_OCCURRENCE_KIND,
        weight=1.0,
        reinforced_ns=T0,
        hypothesis=True,
    )
    t1 = T0 + 10 * HOUR
    client = FakeStorageClient([entity(1, t1), entity(2, t1)], [prior])
    written, _ = main.run_once(client, now_ns=t1 + HOUR)  # type: ignore[arg-type]
    assert written == 1
    edge = client.upserted[0]
    assert edge.reinforced_ns == t1
    assert 1.0 < edge.weight < 1.0 + 1.0 + 1e-9  # decayed prior (<1) plus one jump (<=1)


def test_co_occurrences_older_than_the_ttl_are_not_recreated() -> None:
    """A pair last seen together 4 days ago (past the 72 h hypothesis TTL)
    is history: it must not come back as a fresh edge each run."""
    old = T0 - 4 * 24 * HOUR
    client = FakeStorageClient([entity(1, old), entity(2, old)], [])
    written, _ = main.run_once(client, now_ns=T0)  # type: ignore[arg-type]
    assert written == 0
