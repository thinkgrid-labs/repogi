-- Durable project memory (.repogi/memory.db), schema 1. Versioned
-- independently of the rebuildable code index (PRAGMA user_version is the
-- memory schema; PRAGMA application_id identifies the file type). The memory
-- tables themselves are created from migration_5.sql, so their logical
-- contracts are exactly those of index schema 5.
CREATE TABLE store_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Only the recorded root (an explicit rebind) and, while the store holds no
-- memories, the repository identity (an explicit restore) ever change.
CREATE TRIGGER store_metadata_guard BEFORE UPDATE ON store_metadata
WHEN NEW.key IS NOT OLD.key
    OR OLD.key NOT IN ('root','repository_id')
    OR (OLD.key = 'repository_id' AND EXISTS (SELECT 1 FROM memories))
BEGIN SELECT RAISE(ABORT, 'memory store identity is immutable'); END;
CREATE TRIGGER store_metadata_no_delete BEFORE DELETE ON store_metadata
BEGIN SELECT RAISE(ABORT, 'memory store identity is immutable'); END;

-- Store-level audit log: creation, migrations, imports and rebinds.
CREATE TABLE store_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    action TEXT NOT NULL,
    at TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX store_events_action ON store_events(action,sequence);
CREATE TRIGGER store_events_no_update BEFORE UPDATE ON store_events
BEGIN SELECT RAISE(ABORT, 'store history is append-only'); END;
CREATE TRIGGER store_events_no_delete BEFORE DELETE ON store_events
BEGIN SELECT RAISE(ABORT, 'store history is append-only'); END;

-- Complete source histories of memories imported for local review from
-- another memory lineage, kept verbatim (export JSON) and never edited.
CREATE TABLE memory_imports (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    export_digest TEXT NOT NULL,
    source_repository TEXT,
    source_memory TEXT NOT NULL,
    local_memory TEXT REFERENCES memories(id),
    imported_at TEXT NOT NULL,
    data TEXT NOT NULL,
    UNIQUE (export_digest, source_memory)
);
CREATE INDEX memory_imports_local ON memory_imports(local_memory,sequence);
CREATE TRIGGER memory_imports_no_update BEFORE UPDATE ON memory_imports
BEGIN SELECT RAISE(ABORT, 'imported history is immutable'); END;
CREATE TRIGGER memory_imports_no_delete BEFORE DELETE ON memory_imports
BEGIN SELECT RAISE(ABORT, 'imported history is immutable'); END;
