-- Keep the Phase 1 heuristic graph/FTS unchanged for reproducible controls.
CREATE TABLE semantic_modules (
    path TEXT PRIMARY KEY REFERENCES files(path) ON DELETE CASCADE,
    data TEXT NOT NULL
);
CREATE TABLE semantic_edges (
    ordinal INTEGER PRIMARY KEY,
    path TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    target_id TEXT REFERENCES facts(id) ON DELETE CASCADE,
    data TEXT NOT NULL
);
CREATE INDEX semantic_source ON semantic_edges(source_id);
CREATE INDEX semantic_target ON semantic_edges(target_id);
CREATE VIRTUAL TABLE semantic_search USING fts5(id UNINDEXED, terms, tokenize='unicode61');
CREATE TRIGGER semantic_search_delete AFTER DELETE ON facts BEGIN
    DELETE FROM semantic_search WHERE id=old.id;
END;
