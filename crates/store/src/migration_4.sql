-- Rebuildable source knowledge, separate from the static dependency graph.
CREATE TABLE IF NOT EXISTS knowledge_entities (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL REFERENCES documents(path) ON DELETE CASCADE,
    explicit_id TEXT,
    data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS knowledge_entities_path ON knowledge_entities(path);
CREATE INDEX IF NOT EXISTS knowledge_entities_explicit ON knowledge_entities(explicit_id,id);
CREATE TABLE IF NOT EXISTS knowledge_relationships (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL REFERENCES documents(path) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES knowledge_entities(id) ON DELETE CASCADE,
    target_id TEXT,
    data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS knowledge_relationships_source ON knowledge_relationships(source_id,id);
CREATE INDEX IF NOT EXISTS knowledge_relationships_target ON knowledge_relationships(target_id,id);

DELETE FROM metadata WHERE key='relationships_indexed';
