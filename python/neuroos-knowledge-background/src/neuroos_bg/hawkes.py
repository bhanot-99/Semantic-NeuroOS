"""Parametric exponential-kernel Hawkes process (FR-KNO-10, OQ-04).

R-09 / ADR-0005: a neural ("Neural Hawkes") model needs PyTorch, which
breaks the 75 MiB voice-service budget referenced there and this worker's
own 45 MiB budget (phases.md §8.3 PF) many times over. A parametric
exponential-kernel Hawkes process needs only NumPy: each past co-occurrence
is one self-exciting "jump" whose contribution decays exponentially, so an
edge that keeps recurring stays hot and one that stopped recurring cools
off -- exactly OQ-04's "rhythm model" without a neural network.
"""

from __future__ import annotations

import math

# FR-KNO-10: "Hawkes excitation (decay 0.05)". Units aren't named in
# PRD/Architecture; chosen as per-hour here since it composes directly with
# FR-KNO-10's other number, the 72h hypothesis TTL (this file's own tests
# check that a weight from 72h ago has decayed to a small fraction of its
# original value, not to a meaningless one).
DECAY_PER_HOUR = 0.05

NS_PER_HOUR = 3_600_000_000_000


def decay_weight(weight: float, elapsed_hours: float, decay: float = DECAY_PER_HOUR) -> float:
    """Exponential decay of an edge's weight with no new reinforcement."""
    if elapsed_hours < 0:
        elapsed_hours = 0.0
    return weight * math.exp(-decay * elapsed_hours)


def excite(
    weight: float,
    elapsed_hours: float,
    jump: float = 1.0,
    decay: float = DECAY_PER_HOUR,
) -> float:
    """A new co-occurrence observation: decay whatever weight the edge
    already had, then add one self-exciting jump. Matches a Hawkes
    process's own intensity update -- each event's contribution decays
    exponentially, and a new event's contribution starts at full strength
    on top of what's left of the old ones.
    """
    return decay_weight(weight, elapsed_hours, decay) + jump


def elapsed_hours(now_ns: int, then_ns: int) -> float:
    return max(0, now_ns - then_ns) / NS_PER_HOUR
