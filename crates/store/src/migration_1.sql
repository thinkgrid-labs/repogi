CREATE TABLE metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE files(
    path TEXT PRIMARY KEY,
    language TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    syntax_error INTEGER NOT NULL
);
CREATE TABLE facts(
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    data TEXT NOT NULL
);
CREATE TABLE edges(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    target_id TEXT REFERENCES facts(id) ON DELETE CASCADE,
    target_text TEXT,
    kind TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX edges_source ON edges(source_id);
CREATE INDEX edges_target ON edges(target_id);
CREATE VIRTUAL TABLE search USING fts5(id UNINDEXED,name,path,snippet, tokenize='unicode61');
CREATE TRIGGER facts_delete AFTER DELETE ON facts BEGIN
    DELETE FROM search WHERE id=old.id;
END;
CREATE TABLE index_runs(id INTEGER PRIMARY KEY, revision INTEGER NOT NULL, report TEXT NOT NULL);
