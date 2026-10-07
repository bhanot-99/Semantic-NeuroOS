"""FR-KNO-10: hypothesis edges are pruned after 72 h unless reinforced.

D2: this file used to test a `pruning.is_stale` predicate as well. Nothing
called it -- the decision of *which* edges to delete belongs to C3, which
applies it in SQL (`storage.sock`'s `PruneEdges`, covered by
`sqlite::tests::prune_hypothesis_edges_only_removes_stale_hypotheses`), and
AB-1 forbids this worker from touching SQLite at all. All this module owns is
the cutoff instant it passes to C3, so that is all this file tests.
"""

from hypothesis import given
from hypothesis import strategies as st

from neuroos_bg import pruning


def test_cutoff_ns_is_72_hours_before_now() -> None:
    now = 100 * pruning.NS_PER_HOUR
    assert pruning.cutoff_ns(now) == now - 72 * pruning.NS_PER_HOUR


def test_cutoff_ns_never_goes_negative() -> None:
    assert pruning.cutoff_ns(0) == 0


def test_cutoff_ns_honours_an_explicit_ttl() -> None:
    now = 100 * pruning.NS_PER_HOUR
    assert pruning.cutoff_ns(now, ttl_ns=pruning.NS_PER_HOUR) == now - pruning.NS_PER_HOUR


@given(now_ns=st.integers(min_value=0, max_value=10_000 * pruning.NS_PER_HOUR))
def test_cutoff_is_never_in_the_future_and_never_negative(now_ns: int) -> None:
    cutoff = pruning.cutoff_ns(now_ns)
    assert 0 <= cutoff <= now_ns


@given(
    now_ns=st.integers(min_value=0, max_value=10_000 * pruning.NS_PER_HOUR),
    ttl_ns=st.integers(min_value=0, max_value=10_000 * pruning.NS_PER_HOUR),
)
def test_a_longer_ttl_never_prunes_more(now_ns: int, ttl_ns: int) -> None:
    # A longer TTL moves the cutoff earlier, so it can only keep more edges.
    longer = ttl_ns + pruning.NS_PER_HOUR
    assert pruning.cutoff_ns(now_ns, ttl_ns) >= pruning.cutoff_ns(now_ns, longer)
