"""Graph pruning (FR-KNO-10): "prune unreinforced [hypothesis] edges after
72h" -- Architecture.md §7.2's `edges.hypothesis` TTL. The actual `DELETE`
happens in C3 (`storage.sock`'s `PruneEdges`, AB-1: this worker never
touches SQLite directly); this module is the pure, IPC-free half -- the
cutoff computation and the predicate -- so it's unit-testable without a
running `neuroos-storage`.
"""

from __future__ import annotations

# FR-KNO-10.
HYPOTHESIS_TTL_HOURS = 72
NS_PER_HOUR = 3_600_000_000_000
HYPOTHESIS_TTL_NS = HYPOTHESIS_TTL_HOURS * NS_PER_HOUR


def cutoff_ns(now_ns: int, ttl_ns: int = HYPOTHESIS_TTL_NS) -> int:
    """Edges reinforced before this instant are stale. `storage.sock`'s
    `PruneEdges(older_than_ns=cutoff_ns(now))` deletes exactly those that
    are also still `hypothesis` (a confirmed edge is never pruned)."""
    return max(0, now_ns - ttl_ns)
