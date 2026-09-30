"""APPNP graph propagation (FR-KNO-10): sparse power iteration, 10
iterations, over the co-occurrence graph.

Deliberately vector-valued, not the textbook APPNP's full `n x k` feature
matrix: the cold worker's RSS budget is 45 MiB (phases.md §8.3 PF), and this
worker only ever needs one score per entity -- "how relevant is this entity
to what's active right now" -- not a per-class prediction. A `n x n` dense
feature matrix (the naive "propagate an identity matrix" approach) would be
`O(n^2)` memory and blow that budget past a few hundred entities; this stays
`O(n)` for the score vector and `O(nnz)` for the sparse adjacency, so it
scales to a real personal-scale graph (thousands of entities).
"""

from __future__ import annotations

import numpy as np
from scipy import sparse

# FR-KNO-10: "APPNP sparse power iteration (10 iterations)".
ITERATIONS = 10
# APPNP's restart probability (Klicpera et al. 2018's own default; PRD/
# Architecture name the iteration count and the Hawkes decay explicitly but
# not this -- 0.1 is the paper's default, kept rather than invented.
ALPHA = 0.1


def normalized_adjacency(n: int, edges: list[tuple[int, int, float]]) -> sparse.csr_matrix:
    """Symmetric-normalized adjacency with self-loops: `D^-1/2 (A + I) D^-1/2`.

    `edges` are `(row_index, row_index, weight)` triples over dense `0..n`
    row indices (the caller maps real entity ids to row indices first) --
    each is added both directions, since co-occurrence is symmetric.
    """
    if n == 0:
        return sparse.csr_matrix((0, 0))
    rows: list[int] = []
    cols: list[int] = []
    vals: list[float] = []
    for i, j, w in edges:
        rows.append(i)
        cols.append(j)
        vals.append(w)
        if i != j:
            rows.append(j)
            cols.append(i)
            vals.append(w)
    for i in range(n):
        rows.append(i)
        cols.append(i)
        vals.append(1.0)
    a = sparse.csr_matrix((vals, (rows, cols)), shape=(n, n))
    degree = np.asarray(a.sum(axis=1)).flatten()
    inv_sqrt = np.zeros_like(degree)
    nonzero = degree > 0
    inv_sqrt[nonzero] = 1.0 / np.sqrt(degree[nonzero])
    d_inv_sqrt = sparse.diags(inv_sqrt)
    return (d_inv_sqrt @ a @ d_inv_sqrt).tocsr()


def propagate(
    seed: np.ndarray,
    adjacency: sparse.csr_matrix,
    alpha: float = ALPHA,
    iterations: int = ITERATIONS,
) -> np.ndarray:
    """`Z^(k+1) = (1 - alpha) * A_hat @ Z^(k) + alpha * Z^(0)`, `iterations`
    times. `seed` is the personalization vector (e.g. 1.0 for entities
    active right now, else 0.0); the result is a relevance score per entity,
    highest for the seeds themselves and decaying with graph distance from
    them.
    """
    z = seed.astype(np.float64, copy=True)
    z0 = z.copy()
    for _ in range(iterations):
        z = (1 - alpha) * (adjacency @ z) + alpha * z0
    return z
