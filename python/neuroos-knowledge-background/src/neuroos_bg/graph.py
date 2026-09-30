"""Builds the co-occurrence candidate graph from `entities` (FR-KNO-10).

Not part of the original Phase-0 scaffold's fixed module list
(`appnp.py`/`hawkes.py`/`ipc.py`/`main.py`/`pruning.py`) -- split out because
it's pure, IPC-free graph-construction logic distinct from all four of
those, same reasoning the Rust side used when it grew new files beyond its
own Phase-0 stub list.

`entities` carries only `last_seen_ns` per entity (no full session interval
the way `focus_history` has) -- two entities "co-occur" here when their
`last_seen_ns` values fall within `CO_OCCURRENCE_WINDOW_NS` of each other,
an approximation of "were both touched around the same time", not literal
temporal overlap. Good enough for a v1 hypothesis edge (rules.md AB-11: a
wrong hypothesis just fails to reinforce and gets pruned in 72h -- this
isn't a safety-relevant decision).
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from neuroos_bg import appnp
from neuroos_bg.ipc import Edge, Entity

CO_OCCURRENCE_WINDOW_NS = 5 * 60 * 1_000_000_000  # 5 minutes
ACTIVE_WINDOW_NS = 60 * 60 * 1_000_000_000  # 1 hour -- APPNP's seed/personalization set


@dataclass(frozen=True)
class GraphResult:
    """One job run's output: candidate co-occurrence pairs (by real entity
    id, not row index) scored by blending Hawkes-decayed recency with
    APPNP-propagated relevance."""

    scored_pairs: list[tuple[int, int, float]]  # (entity_id_a, entity_id_b, score)


def candidate_pairs(entities: list[Entity]) -> list[tuple[int, int]]:
    """Entities whose `last_seen_ns` are within `CO_OCCURRENCE_WINDOW_NS`
    of each other, sorted by time so the scan is O(n log n) rather than
    O(n^2) -- a personal-scale graph can have thousands of entities, and a
    45 MiB RSS budget (phases.md §8.3) leaves no room for a naive
    all-pairs comparison.
    """
    ordered = sorted(entities, key=lambda e: e.last_seen_ns)
    pairs: list[tuple[int, int]] = []
    for i, a in enumerate(ordered):
        for b in ordered[i + 1 :]:
            if b.last_seen_ns - a.last_seen_ns > CO_OCCURRENCE_WINDOW_NS:
                break
            if a.id != b.id:
                pairs.append((min(a.id, b.id), max(a.id, b.id)))
    return sorted(set(pairs))


def build_and_score(
    entities: list[Entity],
    existing_edges: list[Edge],
    now_ns: int,
) -> GraphResult:
    """FR-KNO-10's full "the graph learns relationships" step for one job
    run: candidate co-occurrence pairs from `entities`, an APPNP relevance
    propagation seeded from currently-active entities over `existing_edges`
    plus those candidates, and a per-pair score the caller upserts as a
    (hypothesis) edge.
    """
    if not entities:
        return GraphResult(scored_pairs=[])

    index_of = {e.id: i for i, e in enumerate(entities)}
    n = len(entities)

    candidates = [(index_of[a], index_of[b]) for a, b in candidate_pairs(entities)]
    existing = [
        (index_of[e.src], index_of[e.dst], e.weight)
        for e in existing_edges
        if e.src in index_of and e.dst in index_of
    ]
    edge_triples = existing + [(i, j, 1.0) for i, j in candidates]

    adjacency = appnp.normalized_adjacency(n, edge_triples)
    seed = np.array(
        [1.0 if (now_ns - e.last_seen_ns) <= ACTIVE_WINDOW_NS else 0.0 for e in entities]
    )
    relevance = appnp.propagate(seed, adjacency)

    scored_pairs = [
        (entities[i].id, entities[j].id, float((relevance[i] + relevance[j]) / 2.0))
        for i, j in candidates
    ]
    return GraphResult(scored_pairs=scored_pairs)
