-- Additive document index. Code graph and search semantics are unchanged.
CREATE TABLE IF NOT EXISTS documents (
    path TEXT PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    content_hash TEXT NOT NULL,
    content TEXT NOT NULL,
    index_revision INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS document_sections (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL REFERENCES documents(path) ON DELETE CASCADE,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS document_sections_path ON document_sections(path);
CREATE VIRTUAL TABLE IF NOT EXISTS document_search USING fts5(
    id UNINDEXED, path, heading, content, tokenize='unicode61'
);
CREATE TRIGGER IF NOT EXISTS document_sections_delete AFTER DELETE ON document_sections BEGIN
    DELETE FROM document_search WHERE id=old.id;
END;
