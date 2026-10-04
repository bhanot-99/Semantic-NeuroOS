-- BUG-007(b): keyword (BM25) index over every embedded chunk, fused with
-- LanceDB's vector search in StorageEngine::query_hybrid. Vector search
-- alone misses rare exact tokens (user/repo names, show titles) in short
-- window titles. Porter stemming so "installed" finds "apt install".
CREATE VIRTUAL TABLE chunks_fts USING fts5(
    text,
    -- extra search-only spellings of the text (see sqlite::search_aliases)
    aliases,
    chunk_id UNINDEXED,
    entity_id UNINDEXED,
    domain UNINDEXED,
    taint UNINDEXED,
    t_ns UNINDEXED,
    tokenize = 'porter unicode61'
);
