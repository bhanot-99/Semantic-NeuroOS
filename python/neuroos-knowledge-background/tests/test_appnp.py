import numpy as np
import pytest

from neuroos_bg import appnp


def test_normalized_adjacency_is_symmetric() -> None:
    a = appnp.normalized_adjacency(3, [(0, 1, 1.0), (1, 2, 1.0)])
    dense = a.toarray()
    assert np.allclose(dense, dense.T)


def test_normalized_adjacency_empty_graph() -> None:
    a = appnp.normalized_adjacency(0, [])
    assert a.shape == (0, 0)


def test_propagate_relevance_decays_with_graph_distance() -> None:
    # Path graph 0-1-2-3; seed only node 0.
    edges = [(0, 1, 1.0), (1, 2, 1.0), (2, 3, 1.0)]
    a = appnp.normalized_adjacency(4, edges)
    seed = np.array([1.0, 0.0, 0.0, 0.0])
    relevance = appnp.propagate(seed, a)

    assert relevance[0] > relevance[1] > relevance[2] > relevance[3]
    assert relevance[3] >= 0.0


def test_propagate_isolated_node_only_reflects_its_own_seed() -> None:
    # Node 2 has no edges to anything (self-loop only) -- APPNP can't
    # propagate relevance onto it from elsewhere.
    edges = [(0, 1, 1.0)]
    a = appnp.normalized_adjacency(3, edges)
    seed = np.array([1.0, 0.0, 0.0])
    relevance = appnp.propagate(seed, a)

    assert relevance[2] == pytest.approx(0.0, abs=1e-9)
    assert relevance[0] > relevance[1]


def test_propagate_converges_without_blowing_up() -> None:
    # A cycle graph with a uniform seed should stay bounded across the
    # fixed 10 iterations (FR-KNO-10) -- no unbounded growth from repeated
    # sparse matmuls.
    n = 20
    edges = [(i, (i + 1) % n, 1.0) for i in range(n)]
    a = appnp.normalized_adjacency(n, edges)
    seed = np.ones(n)
    relevance = appnp.propagate(seed, a)
    assert np.all(np.isfinite(relevance))
    assert np.max(relevance) < 10 * np.max(seed)


def test_appnp_iteration_count_matches_fr_kno_10() -> None:
    assert appnp.ITERATIONS == 10
