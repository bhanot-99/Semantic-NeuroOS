-- L13: `sqlite::upsert_entity` is a SELECT-then-INSERT-or-UPDATE keyed on
-- (domain, label), but nothing in the schema enforced that pair to be
-- unique -- only a plain (non-unique) `idx_entities_domain_label`. Two
-- writers cannot race here (C3 is the single writer, Architecture.md §7.2),
-- but the SELECT used to swallow real errors as "not found" (the `.ok()`
-- this migration ships alongside), and every such error inserted a
-- duplicate that then split one entity's edges, chunks and taint across
-- two ids. The index makes that impossible rather than unlikely.

-- Any duplicates an older build already created are merged onto the lowest
-- id for the pair, or the UNIQUE index below could not be created.
CREATE TEMP TABLE entity_merge AS
SELECT e.id AS old_id, m.keep_id AS keep_id
FROM entities e
JOIN (
    SELECT domain, label, MIN(id) AS keep_id
    FROM entities
    GROUP BY domain, label
    HAVING COUNT(*) > 1
) m ON m.domain = e.domain AND m.label = e.label
WHERE e.id <> m.keep_id;

-- The survivor absorbs the others: taint is unioned (never lowered, R0-3),
-- the span widens, and `permanent` is sticky.
UPDATE entities SET
    taint = taint | COALESCE((
        SELECT MAX(d.taint) FROM entities d
        JOIN entity_merge em ON em.old_id = d.id
        WHERE em.keep_id = entities.id
    ), 0),
    created_ns = MIN(created_ns, COALESCE((
        SELECT MIN(d.created_ns) FROM entities d
        JOIN entity_merge em ON em.old_id = d.id
        WHERE em.keep_id = entities.id
    ), created_ns)),
    last_seen_ns = MAX(last_seen_ns, COALESCE((
        SELECT MAX(d.last_seen_ns) FROM entities d
        JOIN entity_merge em ON em.old_id = d.id
        WHERE em.keep_id = entities.id
    ), last_seen_ns)),
    permanent = MAX(permanent, COALESCE((
        SELECT MAX(d.permanent) FROM entities d
        JOIN entity_merge em ON em.old_id = d.id
        WHERE em.keep_id = entities.id
    ), 0))
WHERE id IN (SELECT keep_id FROM entity_merge);

-- Repoint references. Edges are keyed (src, dst, kind), so a repoint can
-- collide with an edge the survivor already has: insert what does not
-- collide, then drop every edge still pointing at a merged id (including
-- the self-edges a merge can produce).
INSERT OR IGNORE INTO edges (src, dst, kind, weight, reinforced_ns, hypothesis)
SELECT
    COALESCE((SELECT keep_id FROM entity_merge WHERE old_id = e.src), e.src),
    COALESCE((SELECT keep_id FROM entity_merge WHERE old_id = e.dst), e.dst),
    e.kind, e.weight, e.reinforced_ns, e.hypothesis
FROM edges e
WHERE e.src IN (SELECT old_id FROM entity_merge)
   OR e.dst IN (SELECT old_id FROM entity_merge);

DELETE FROM edges
WHERE src IN (SELECT old_id FROM entity_merge)
   OR dst IN (SELECT old_id FROM entity_merge)
   OR src = dst;

UPDATE chunks_meta SET entity_id = (
    SELECT keep_id FROM entity_merge WHERE old_id = chunks_meta.entity_id
) WHERE entity_id IN (SELECT old_id FROM entity_merge);

UPDATE chunks_fts SET entity_id = (
    SELECT keep_id FROM entity_merge WHERE old_id = chunks_fts.entity_id
) WHERE entity_id IN (SELECT old_id FROM entity_merge);

DELETE FROM entities WHERE id IN (SELECT old_id FROM entity_merge);

DROP TABLE entity_merge;

-- The actual fix. The old non-unique index served the same lookups, so it
-- is redundant once this exists.
DROP INDEX IF EXISTS idx_entities_domain_label;
CREATE UNIQUE INDEX idx_entities_domain_label ON entities(domain, label);
