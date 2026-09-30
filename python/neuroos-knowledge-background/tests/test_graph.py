from neuroos_bg import graph
from neuroos_bg.ipc import Edge, Entity


def entity(id_: int, last_seen_ns: int) -> Entity:
    return Entity(
        id=id_,
        domain="window_focus",
        kind="window",
        label=f"app-{id_}",
        taint=0,
        created_ns=last_seen_ns,
        last_seen_ns=last_seen_ns,
        permanent=False,
    )


def test_candidate_pairs_links_entities_seen_close_in_time() -> None:
    entities = [entity(1, 0), entity(2, 60_000_000_000)]  # 60s apart
    pairs = graph.candidate_pairs(entities)
    assert pairs == [(1, 2)]


def test_candidate_pairs_excludes_entities_far_apart_in_time() -> None:
    entities = [entity(1, 0), entity(2, graph.CO_OCCURRENCE_WINDOW_NS * 10)]
    assert graph.candidate_pairs(entities) == []


def test_candidate_pairs_is_deduplicated_and_ordered() -> None:
    entities = [entity(3, 0), entity(1, 1_000), entity(2, 2_000)]
    pairs = graph.candidate_pairs(entities)
    assert pairs == sorted(set(pairs))
    assert all(a < b for a, b in pairs)


def test_build_and_score_empty_entities_yields_no_pairs() -> None:
    result = graph.build_and_score([], [], now_ns=0)
    assert result.scored_pairs == []


def test_build_and_score_scores_active_pair_above_inactive_one() -> None:
    now = 10 * graph.ACTIVE_WINDOW_NS
    entities = [
        # a, b just co-occurred and are both "active" right now.
        entity(1, now - 60_000_000_000),
        entity(2, now - 30_000_000_000),
        # c, d co-occurred with each other but long before `now` -- not in
        # the APPNP seed set.
        entity(3, 0),
        entity(4, 30_000_000_000),
    ]
    result = graph.build_and_score(entities, [], now_ns=now)
    scores = {frozenset((a, b)): s for a, b, s in result.scored_pairs}

    assert frozenset((1, 2)) in scores
    assert frozenset((3, 4)) in scores
    assert scores[frozenset((1, 2))] > scores[frozenset((3, 4))]


def test_build_and_score_reuses_existing_edge_weights_as_appnp_input() -> None:
    now = 0
    entities = [entity(1, 0), entity(2, 0)]
    existing = [Edge(src=1, dst=2, kind="co_occurs", weight=5.0, reinforced_ns=0, hypothesis=True)]
    # Must not raise -- existing edges reference real entity ids, which
    # build_and_score maps back to row indices internally.
    result = graph.build_and_score(entities, existing, now_ns=now)
    assert len(result.scored_pairs) == 1
