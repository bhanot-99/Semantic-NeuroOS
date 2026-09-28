from hypothesis import given
from hypothesis import strategies as st

from neuroos_bg import pruning
from neuroos_bg.ipc import Edge


def make_edge(*, reinforced_ns: int, hypothesis: bool) -> Edge:
    return Edge(
        src=1,
        dst=2,
        kind="co_occurs",
        weight=1.0,
        reinforced_ns=reinforced_ns,
        hypothesis=hypothesis,
    )


def test_cutoff_ns_is_72_hours_before_now() -> None:
    now = 100 * pruning.NS_PER_HOUR
    assert pruning.cutoff_ns(now) == now - 72 * pruning.NS_PER_HOUR


def test_cutoff_ns_never_goes_negative() -> None:
    assert pruning.cutoff_ns(0) == 0


def test_confirmed_edge_is_never_stale_regardless_of_age() -> None:
    edge = make_edge(reinforced_ns=0, hypothesis=False)
    assert not pruning.is_stale(edge, now_ns=1000 * pruning.NS_PER_HOUR)


def test_hypothesis_edge_within_ttl_is_not_stale() -> None:
    now = 100 * pruning.NS_PER_HOUR
    edge = make_edge(reinforced_ns=now - 71 * pruning.NS_PER_HOUR, hypothesis=True)
    assert not pruning.is_stale(edge, now_ns=now)


def test_hypothesis_edge_past_ttl_is_stale() -> None:
    now = 100 * pruning.NS_PER_HOUR
    edge = make_edge(reinforced_ns=now - 73 * pruning.NS_PER_HOUR, hypothesis=True)
    assert pruning.is_stale(edge, now_ns=now)


@given(
    reinforced_ns=st.integers(min_value=0, max_value=10_000 * pruning.NS_PER_HOUR),
    now_ns=st.integers(min_value=0, max_value=10_000 * pruning.NS_PER_HOUR),
)
def test_confirmed_edges_are_always_kept(reinforced_ns: int, now_ns: int) -> None:
    edge = make_edge(reinforced_ns=reinforced_ns, hypothesis=False)
    assert not pruning.is_stale(edge, now_ns)


@given(
    reinforced_ns=st.integers(min_value=0, max_value=10_000 * pruning.NS_PER_HOUR),
    now_ns=st.integers(min_value=0, max_value=10_000 * pruning.NS_PER_HOUR),
)
def test_staleness_matches_the_72h_cutoff_exactly(reinforced_ns: int, now_ns: int) -> None:
    edge = make_edge(reinforced_ns=reinforced_ns, hypothesis=True)
    assert pruning.is_stale(edge, now_ns) == (reinforced_ns < pruning.cutoff_ns(now_ns))
