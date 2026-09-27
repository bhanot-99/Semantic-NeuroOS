-- Architecture.md §7.2: meta.sqlite3, WAL mode, single writer (C3).
-- taint columns store neuroos_taint::TaintFlags::bits() (u32).

CREATE TABLE focus_history (
    id          INTEGER PRIMARY KEY,
    app_id      TEXT NOT NULL,
    title       TEXT NOT NULL,
    pid         INTEGER NOT NULL,
    root_pid    INTEGER NOT NULL,
    t_start_ns  INTEGER NOT NULL,
    t_end_ns    INTEGER NOT NULL,
    dwell_ms    INTEGER NOT NULL
);
CREATE INDEX idx_focus_history_span ON focus_history(t_start_ns, t_end_ns);
CREATE INDEX idx_focus_history_app_id ON focus_history(app_id);

CREATE TABLE event_counters (
    domain          TEXT NOT NULL,
    key             TEXT NOT NULL,
    count           INTEGER NOT NULL DEFAULT 0,
    first_ns        INTEGER NOT NULL,
    last_ns         INTEGER NOT NULL,
    total_dwell_ms  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (domain, key)
);

CREATE TABLE aggregates_daily (
    day       TEXT NOT NULL,     -- YYYY-MM-DD, UTC
    domain    TEXT NOT NULL,
    key       TEXT NOT NULL,
    count     INTEGER NOT NULL DEFAULT 0,
    dwell_ms  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, domain, key)
);

CREATE TABLE entities (
    id           INTEGER PRIMARY KEY,
    domain       TEXT NOT NULL,
    kind         TEXT NOT NULL,
    label        TEXT NOT NULL,
    taint        INTEGER NOT NULL DEFAULT 0,
    created_ns   INTEGER NOT NULL,
    last_seen_ns INTEGER NOT NULL,
    permanent    INTEGER NOT NULL DEFAULT 0 CHECK (permanent IN (0, 1))
);
CREATE INDEX idx_entities_domain_label ON entities(domain, label);
CREATE INDEX idx_entities_last_seen ON entities(last_seen_ns);

CREATE TABLE edges (
    src           INTEGER NOT NULL REFERENCES entities(id),
    dst           INTEGER NOT NULL REFERENCES entities(id),
    kind          TEXT NOT NULL,
    weight        REAL NOT NULL DEFAULT 1.0,
    reinforced_ns INTEGER NOT NULL,
    hypothesis    INTEGER NOT NULL DEFAULT 0 CHECK (hypothesis IN (0, 1)),
    PRIMARY KEY (src, dst, kind)
);
CREATE INDEX idx_edges_dst ON edges(dst);

CREATE TABLE chunks_meta (
    chunk_id    TEXT PRIMARY KEY,
    entity_id   INTEGER NOT NULL REFERENCES entities(id),
    source      TEXT NOT NULL,
    taint       INTEGER NOT NULL DEFAULT 0,
    token_count INTEGER NOT NULL,
    created_ns  INTEGER NOT NULL
);
CREATE INDEX idx_chunks_meta_entity ON chunks_meta(entity_id);

CREATE TABLE index_meta (
    collection         TEXT PRIMARY KEY,
    embedding_model_id  TEXT NOT NULL,
    dim                INTEGER NOT NULL,
    index_kind         TEXT NOT NULL, -- 'flat' | 'hnsw'
    p99_ms             REAL NOT NULL DEFAULT 0,
    updated_ns         INTEGER NOT NULL
);
