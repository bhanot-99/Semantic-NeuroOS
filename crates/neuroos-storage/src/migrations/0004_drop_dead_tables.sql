-- D3: drop two tables that have existed since 0001 and were never written.
--
-- `aggregates_daily` (Architecture.md §7.2's 90-day rollup) and `chunks_meta`
-- (per-chunk provenance) were both part of the Phase 4 schema sketch. Nothing
-- ever inserted into either one: chunk provenance ended up denormalised into
-- `chunks_fts` and the LanceDB row (both of which carry `chunk_id`,
-- `entity_id`, `taint` and the timestamp), and the daily rollup was never
-- built because `event_counters` answers the same questions directly. Every
-- `DELETE FROM chunks_meta` in sqlite.rs was therefore dead, and the table's
-- foreign key into `entities` made deletes marginally slower for no benefit.
--
-- Safe to drop: no data to migrate, by definition. Migration 0003 still
-- repoints `chunks_meta.entity_id` when it runs against a pre-0003 database,
-- and it runs before this file, so the ordering is intact.

DROP INDEX IF EXISTS idx_chunks_meta_entity;
DROP TABLE IF EXISTS chunks_meta;
DROP TABLE IF EXISTS aggregates_daily;
