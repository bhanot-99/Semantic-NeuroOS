import math

from hypothesis import given
from hypothesis import strategies as st

from neuroos_bg import hawkes


def test_decay_weight_at_zero_elapsed_is_unchanged() -> None:
    assert hawkes.decay_weight(2.0, 0.0) == 2.0


def test_decay_weight_matches_the_closed_form_exponential() -> None:
    weight = hawkes.decay_weight(1.0, 72.0)
    assert weight == math.exp(-hawkes.DECAY_PER_HOUR * 72.0)
    # FR-KNO-10's 72h hypothesis TTL should correspond to real decay, not a
    # rounding error -- confirms the "0.05/hour" reading is a meaningfully
    # decaying kernel over the pruning window it's paired with.
    assert weight < 0.03


def test_decay_weight_never_increases() -> None:
    assert hawkes.decay_weight(5.0, 1.0) < 5.0
    assert hawkes.decay_weight(5.0, 100.0) < hawkes.decay_weight(5.0, 1.0)


def test_excite_adds_a_full_jump_at_zero_elapsed() -> None:
    assert hawkes.excite(0.0, 0.0, jump=1.0) == 1.0
    assert hawkes.excite(2.0, 0.0, jump=1.0) == 3.0


def test_elapsed_hours_never_negative() -> None:
    assert hawkes.elapsed_hours(now_ns=100, then_ns=200) == 0.0
    assert hawkes.elapsed_hours(now_ns=hawkes.NS_PER_HOUR * 3, then_ns=0) == 3.0


@given(
    weight=st.floats(min_value=0.0, max_value=1000.0, allow_nan=False),
    elapsed=st.floats(min_value=0.0, max_value=10_000.0, allow_nan=False),
)
def test_decay_weight_is_always_nonnegative_and_bounded(weight: float, elapsed: float) -> None:
    decayed = hawkes.decay_weight(weight, elapsed)
    assert 0.0 <= decayed <= weight + 1e-9
