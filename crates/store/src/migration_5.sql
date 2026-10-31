-- Reviewed project memory. Unlike the rest of the index, these rows are not
-- rebuildable: indexing never writes them, and triggers keep evidence
-- snapshots, review history and reviewed revisions immutable.
CREATE TABLE IF NOT EXISTS memories (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending_review','approved','rejected','superseded','retired')),
    latest_revision INTEGER NOT NULL,
    approved_revision INTEGER,
    pending_revision INTEGER,
    superseded_by TEXT REFERENCES memories(id),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    version INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS memories_status ON memories(status,id);

CREATE TABLE IF NOT EXISTS memory_revisions (
    memory_id TEXT NOT NULL REFERENCES memories(id),
    revision INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('proposed','approved','rejected')),
    digest TEXT NOT NULL,
    claim_key TEXT NOT NULL,
    assertion_subject TEXT,
    assertion_status TEXT,
    data TEXT NOT NULL,
    PRIMARY KEY (memory_id, revision)
);
CREATE INDEX IF NOT EXISTS memory_revisions_claim ON memory_revisions(claim_key);
CREATE INDEX IF NOT EXISTS memory_revisions_assertion ON memory_revisions(assertion_subject,memory_id);

CREATE TABLE IF NOT EXISTS memory_evidence (
    memory_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('backing','context')),
    class TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    explicit_id TEXT,
    -- Target of a cited relationship or static edge, for identity lookups.
    target_id TEXT,
    path TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (memory_id, revision, ordinal),
    FOREIGN KEY (memory_id, revision) REFERENCES memory_revisions(memory_id, revision)
);
CREATE INDEX IF NOT EXISTS memory_evidence_subject ON memory_evidence(subject_id,memory_id);
CREATE INDEX IF NOT EXISTS memory_evidence_explicit ON memory_evidence(explicit_id,memory_id);
CREATE INDEX IF NOT EXISTS memory_evidence_path ON memory_evidence(path,memory_id);
CREATE INDEX IF NOT EXISTS memory_evidence_target ON memory_evidence(target_id,memory_id);

CREATE TABLE IF NOT EXISTS memory_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id TEXT NOT NULL REFERENCES memories(id),
    revision INTEGER,
    action TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS memory_events_memory ON memory_events(memory_id,sequence);

-- One row per revision, inserted once; searches join the authoritative revision.
CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
    memory_id UNINDEXED,
    revision UNINDEXED,
    title,
    claim,
    subjects,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS memory_evidence_no_update BEFORE UPDATE ON memory_evidence
BEGIN SELECT RAISE(ABORT, 'memory evidence snapshots are immutable'); END;
CREATE TRIGGER IF NOT EXISTS memory_evidence_no_delete BEFORE DELETE ON memory_evidence
BEGIN SELECT RAISE(ABORT, 'memory evidence snapshots are immutable'); END;
CREATE TRIGGER IF NOT EXISTS memory_events_no_update BEFORE UPDATE ON memory_events
BEGIN SELECT RAISE(ABORT, 'memory history is append-only'); END;
CREATE TRIGGER IF NOT EXISTS memory_events_no_delete BEFORE DELETE ON memory_events
BEGIN SELECT RAISE(ABORT, 'memory history is append-only'); END;
CREATE TRIGGER IF NOT EXISTS memory_revisions_no_delete BEFORE DELETE ON memory_revisions
BEGIN SELECT RAISE(ABORT, 'memory revisions are immutable'); END;
-- A proposed revision is reviewed once; its claim and evidence never change.
CREATE TRIGGER IF NOT EXISTS memory_revisions_review_once BEFORE UPDATE ON memory_revisions
WHEN OLD.state <> 'proposed'
    OR NEW.state NOT IN ('approved','rejected')
    OR NEW.memory_id IS NOT OLD.memory_id
    OR NEW.revision IS NOT OLD.revision
    OR NEW.digest IS NOT OLD.digest
    OR NEW.claim_key IS NOT OLD.claim_key
    OR NEW.assertion_subject IS NOT OLD.assertion_subject
    OR NEW.assertion_status IS NOT OLD.assertion_status
    OR NEW.data IS NOT OLD.data
BEGIN SELECT RAISE(ABORT, 'memory revisions are immutable once reviewed'); END;
CREATE TRIGGER IF NOT EXISTS memories_no_delete BEFORE DELETE ON memories
BEGIN SELECT RAISE(ABORT, 'memories are retired or superseded, never deleted'); END;
