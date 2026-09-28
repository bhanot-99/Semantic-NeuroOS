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

from neuroos_bg import graph, hawkes, pruning
from neuroos_bg.ipc import Edge, IpcError, StorageClient, now_ns

# 10 minutes between job runs -- frequent enough that a session's
# co-occurrences get picked up same-day, infrequent enough to stay well
# inside the 45 MiB / low-CPU budget a background job gets (phases.md §8.3).
DEFAULT_INTERVAL_S = 600.0

# Only entities active in the last 7 days become propagation-graph nodes --
# bounds the graph to a personal-scale, still-relevant working set rather
# than growing unboundedly over the life of an install.
LOOKBACK_NS = 7 * 24 * 3_600_000_000_000

CO_OCCURRENCE_KIND = "co_occurs"


def run_once(client: StorageClient) -> tuple[int, int]:
    """One job iteration. Returns `(edges_written, edges_pruned)`."""
    now = now_ns()
    entities = client.list_entities(since_ns=now - LOOKBACK_NS)
    existing_edges = client.list_edges()
    result = graph.build_and_score(entities, existing_edges, now)

    existing_by_pair = {(e.src, e.dst): e for e in existing_edges if e.kind == CO_OCCURRENCE_KIND}
    written = 0
    for a, b, score in result.scored_pairs:
        prior = existing_by_pair.get((a, b)) or existing_by_pair.get((b, a))
        elapsed = hawkes.elapsed_hours(now, prior.reinforced_ns) if prior else 0.0
        prior_weight = prior.weight if prior else 0.0
        weight = hawkes.excite(prior_weight, elapsed, jump=score)
        client.upsert_edge(
            Edge(
                src=a,
                dst=b,
                kind=CO_OCCURRENCE_KIND,
                weight=weight,
                reinforced_ns=now,
                hypothesis=True,
            )
        )
        written += 1

    pruned = client.prune_edges(pruning.cutoff_ns(now))
    return written, pruned


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

    client = StorageClient()
    while True:
        try:
            written, pruned = run_once(client)
            print(
                f"neuroos-knowledge-background: wrote {written} edges, pruned {pruned}",
                file=sys.stderr,
            )
        except IpcError as e:
            # rules.md §5.6: fail soft -- a job iteration failing (C3 down
            # or slow) must not crash the worker, just skip to the next
            # scheduled attempt.
            print(f"neuroos-knowledge-background: job failed: {e}", file=sys.stderr)
        if args.once:
            return 0
        time.sleep(args.interval_s)


if __name__ == "__main__":
    raise SystemExit(main())
