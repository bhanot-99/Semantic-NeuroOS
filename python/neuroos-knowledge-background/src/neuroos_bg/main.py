"""Background knowledge worker entry point (C5b, FR-KNO-10, P5-S07).

The job loop: read entities + edges from `storage.sock`, score
co-occurrence candidates (APPNP relevance x Hawkes-decayed recency), write
them back as hypothesis edges, then prune whatever hypothesis edges have
gone 72h unreinforced. AB-1: never touches `meta.sqlite3` directly -- every
read and write goes through `neuroos_bg.ipc.StorageClient`.
"""

from __future__ import annotations

import argparse
import sys
import time
from collections.abc import Sequence

from neuroos_bg import graph, hawkes, pruning, sandbox
from neuroos_bg.ipc import Edge, StorageClient, runtime_dir
from neuroos_bg.ipc import now_ns as now_ns_fn

# 10 minutes between job runs -- frequent enough that a session's
# co-occurrences get picked up same-day, infrequent enough to stay well
# inside the 45 MiB / low-CPU budget a background job gets (phases.md §8.3).
DEFAULT_INTERVAL_S = 600.0

# Only entities active in the last 7 days become propagation-graph nodes --
# bounds the graph to a personal-scale, still-relevant working set rather
# than growing unboundedly over the life of an install.
LOOKBACK_NS = 7 * 24 * 3_600_000_000_000

CO_OCCURRENCE_KIND = "co_occurs"


def run_once(client: StorageClient, now_ns: int | None = None) -> tuple[int, int]:
    """One job iteration. Returns `(edges_written, edges_pruned)`.

    H14: each co-occurrence is one Hawkes event, counted once. A pair's
    event time is the later of its two `last_seen_ns`; it reinforces the
    edge only if it is newer than the edge's last reinforcement, and the
    edge is stamped with that event time (not the job's run time), so an
    edge with no new co-occurrence decays and is pruned after 72 h. A pair
    whose event is already past the TTL is history, not a new hypothesis.
    """
    now = now_ns if now_ns is not None else now_ns_fn()
    entities = client.list_entities(since_ns=now - LOOKBACK_NS)
    existing_edges = client.list_edges()
    result = graph.build_and_score(entities, existing_edges, now)
    last_seen = {e.id: e.last_seen_ns for e in entities}

    existing_by_pair = {(e.src, e.dst): e for e in existing_edges if e.kind == CO_OCCURRENCE_KIND}
    written = 0
    for a, b, score in result.scored_pairs:
        event_ns = max(last_seen[a], last_seen[b])
        prior = existing_by_pair.get((a, b)) or existing_by_pair.get((b, a))
        if prior is not None and event_ns <= prior.reinforced_ns:
            continue  # already counted
        if prior is None and now - event_ns > pruning.HYPOTHESIS_TTL_NS:
            continue  # too old to be a live hypothesis
        elapsed = hawkes.elapsed_hours(event_ns, prior.reinforced_ns) if prior else 0.0
        prior_weight = prior.weight if prior else 0.0
        weight = hawkes.excite(prior_weight, elapsed, jump=score)
        client.upsert_edge(
            Edge(
                src=prior.src if prior else a,
                dst=prior.dst if prior else b,
                kind=CO_OCCURRENCE_KIND,
                weight=weight,
                reinforced_ns=event_ns,
                hypothesis=True,
            )
        )
        written += 1

    pruned = client.prune_edges(pruning.cutoff_ns(now))
    return written, pruned


def enter_sandbox() -> None:
    """H15 / Architecture.md §8.2's C5b row: the Python installation
    (read) and the runtime dir; stored data is only ever reached over
    `storage.sock`."""
    sandbox.restrict_self(sandbox.python_read_only_paths(), [str(runtime_dir())])


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="NeuroOS background knowledge worker (C5b)")
    parser.add_argument("--once", action="store_true", help="run a single job iteration and exit")
    parser.add_argument(
        "--interval-s",
        type=float,
        default=DEFAULT_INTERVAL_S,
        help="seconds between job iterations (ignored with --once)",
    )
    args = parser.parse_args(argv)

    try:
        enter_sandbox()
    except sandbox.SandboxError as e:
        # rules.md §5.5: fail closed -- never run unsandboxed.
        print(f"neuroos-knowledge-background: {e}", file=sys.stderr)
        return 1

    client = StorageClient()
    while True:
        try:
            written, pruned = run_once(client)
            print(
                f"neuroos-knowledge-background: wrote {written} edges, pruned {pruned}",
                file=sys.stderr,
            )
        except Exception as e:  # noqa: BLE001 -- rules.md §5.9, see below
            # rules.md §5.6/§5.9: fail soft -- a job iteration failing for
            # any reason (C3 down or slow, a reset connection, a timeout,
            # a malformed frame, a bug) must not crash the worker, just
            # skip to the next scheduled attempt. Only the exception type
            # is logged: messages can carry entity labels (R0-6).
            print(f"neuroos-knowledge-background: job failed: {type(e).__name__}", file=sys.stderr)
        if args.once:
            return 0
        time.sleep(args.interval_s)


if __name__ == "__main__":
    raise SystemExit(main())
