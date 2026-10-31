//! The durable project-memory store, `.repogi/memory.db`.
//!
//! It is authoritative for memories and independent of the rebuildable code
//! index: indexing never creates, migrates or writes it, reads never create or
//! write anything, and deleting `.code-intel/` leaves it intact. It is versioned
//! separately (`user_version` is the memory schema, `application_id` marks the
//! file type) and records the repository root it belongs to.
//!
//! Journal mode is SQLite's rollback journal (`DELETE`) with `synchronous=FULL`:
//! between writes the database is one self-contained file, so a quiescent copy
//! is a consistent backup and no `-wal`/`-shm` files appear. Writers serialize
//! through `IMMEDIATE` transactions with a busy timeout; readers see one
//! snapshot per command.
//!
//! Schema-5 indexes may still hold memories (`legacy`). They are migrated by an
//! explicit write (`repogi memory migrate`, any memory write, or `repogi
//! index`), copied row for row into a temporary database that is validated and
//! then installed with a no-clobber link, so the store path only ever holds a
//! complete, verified database. The legacy rows are never deleted; they are
//! frozen against further writes by older binaries.
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use code_intel_core::memory::{
    StoredEvent, StoredEvidence, StoredMemory, StoredRevision, StoredSearch, stored_digest,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde_json::json;

use crate::{MIGRATION_5, memory::fail, validate_state_path};

/// The memory store schema, independent of the code index schema.
pub const MEMORY_SCHEMA_VERSION: u32 = 1;
/// `PRAGMA application_id` of a memory store ("RPGM").
pub const MEMORY_APPLICATION_ID: i32 = 0x5250_474D;
pub const MEMORY_DIR: &str = ".repogi";
pub const MEMORY_FILE: &str = ".repogi/memory.db";
const STORE_KIND: &str = "repogi-memory";
const STORE_SCHEMA: &str = include_str!("memory_store.sql");
const LEGACY_TABLES: [&str; 5] = [
    "memories",
    "memory_revisions",
    "memory_evidence",
    "memory_events",
    "memory_search",
];
/// Interrupted migration databases older than this are removed.
const STALE_TEMPORARY: Duration = Duration::from_secs(600);
const GITIGNORE: &str = "# Repogi durable project memory: local by default. Share memories with\n# `repogi memory export`, not by committing these database files.\n*\n";
/// Written into frozen legacy tables; older binaries report it on a write.
const MOVED: &str =
    "project memory moved to .repogi/memory.db; this repogi binary is too old to change it";

/// Where a store's memories come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySource {
    /// `.repogi/memory.db`: authoritative.
    Durable,
    /// Unmigrated schema-5 memory tables in `.code-intel/index.db`, read-only.
    Legacy,
    /// Nothing recorded yet: an empty in-memory view (nothing on disk).
    Empty,
}

impl MemorySource {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::Legacy => "legacy_index",
            Self::Empty => "none",
        }
    }
}

pub struct MemoryStore {
    pub(crate) connection: Connection,
    pub(crate) cancelled: Arc<AtomicBool>,
    source: MemorySource,
    root: String,
    repository_id: Option<String>,
    notes: Vec<(&'static str, String)>,
}

/// Row counts of one memory dataset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Counts {
    pub memories: u64,
    pub revisions: u64,
    pub evidence: u64,
    pub events: u64,
    pub max_sequence: u64,
}

/// What a migration did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MigrationReport {
    /// `migrated` (new store), `merged` (into an existing store) or
    /// `already_migrated` (nothing to do).
    pub outcome: &'static str,
    pub source: &'static str,
    pub counts: Counts,
    /// BLAKE3 over the legacy rows exactly as stored.
    pub source_digest: String,
    /// Memories newly written or advanced by this migration.
    pub written: usize,
    /// Older binaries can no longer write the legacy tables.
    pub legacy_frozen: bool,
    pub notes: Vec<String>,
}

/// The result of reconciling one incoming memory with the destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    Absent,
    Identical,
    /// The destination holds a strict prefix of the incoming history.
    FastForward,
    /// The incoming copy is a strict prefix of the destination's (older).
    Behind,
    /// Both sides changed; never merged automatically.
    Diverged(String),
}

/// One memory imported for local review from another memory lineage.
pub struct ReviewImport {
    pub source_memory: String,
    /// The complete source history, kept verbatim.
    pub archive: String,
    /// A pending local memory to create (its ID is allocated on insert), or
    /// `None` when only the history is archived.
    pub local: Option<StoredMemory>,
    /// Normalized claim of `local`, for duplicate detection.
    pub claim_key: Option<String>,
}

/// Per-memory outcome of an import.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImportedMemory {
    pub source_memory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_memory: Option<String>,
    /// `created`, `unchanged`, `fast_forwarded`, `pending_local_review`,
    /// `archived_only`, `duplicate_claim` or `already_imported`.
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub struct ImportRecord<'a> {
    pub export_digest: &'a str,
    pub source_repository: Option<&'a str>,
    pub mode: &'static str,
    pub at: &'a str,
    pub actor: &'a str,
}

/// Store identity and health, for `repogi memory status`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoreStatus {
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub counts: Counts,
    /// Store-level history: creation, migrations, imports, rebinds.
    pub events: Vec<StoreEvent>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StoreEvent {
    pub sequence: u64,
    pub action: String,
    pub at: String,
    pub data: serde_json::Value,
}

impl MemoryStore {
    fn new(
        connection: Connection,
        source: MemorySource,
        root: &str,
        repository_id: Option<String>,
    ) -> Self {
        Self {
            connection,
            cancelled: Arc::default(),
            source,
            root: root.to_owned(),
            repository_id,
            notes: Vec::new(),
        }
    }

    #[must_use]
    pub const fn source(&self) -> MemorySource {
        self.source
    }

    /// Canonical repository root this store belongs to.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    #[must_use]
    pub fn repository_id(&self) -> Option<&str> {
        self.repository_id.as_deref()
    }

    /// Storage caveats for read responses: unmigrated or partially migrated
    /// legacy memories. Empty for a fully migrated durable store.
    #[must_use]
    pub fn notes(&self) -> &[(&'static str, String)] {
        &self.notes
    }

    /// Open for reading without creating, migrating or writing anything: the
    /// durable store when it exists, else memories still held by a schema-5
    /// index (read-only), else an empty in-memory view.
    pub fn open_read(root: &Path) -> Result<Self> {
        validate_state_path(root)?;
        let root_text = root_text(root)?;
        let database = root.join(MEMORY_FILE);
        if exists(&database)? {
            let connection = Connection::open_with_flags(
                &database,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .with_context(|| format!("open memory store {}", database.display()))?;
            connection.busy_timeout(Duration::from_secs(5))?;
            if let Some(id) = identity(&connection, root_text)? {
                connection.execute_batch("BEGIN DEFERRED")?;
                let mut store = Self::new(connection, MemorySource::Durable, root_text, Some(id));
                store.notes = legacy_notes(root, &store.connection)?;
                return Ok(store);
            }
        }
        if let Some(connection) = legacy(root, root_text)? {
            let count = Counts::of(&connection)?.memories;
            let mut store = Self::new(connection, MemorySource::Legacy, root_text, None);
            store.notes.push((
                "memory_not_migrated",
                format!(
                    "{count} memories are still stored in the rebuildable .code-intel/index.db; run `repogi memory migrate` before deleting .code-intel"
                ),
            ));
            return Ok(store);
        }
        Self::empty(root_text)
    }

    /// An in-memory store with the full schema and no rows.
    fn empty(root: &str) -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(STORE_SCHEMA)?;
        connection.execute_batch(MIGRATION_5)?;
        Ok(Self::new(connection, MemorySource::Empty, root, None))
    }

    /// Open the durable store for an explicit write, creating it when absent
    /// and migrating legacy index memories first. Never touches the index
    /// beyond reading legacy rows and freezing them after a migration.
    pub fn open_write(root: &Path, at: &str) -> Result<(Self, Option<MigrationReport>)> {
        validate_state_path(root)?;
        let migration = migrate(root, at)?;
        Ok((Self::open_or_create(root, at, None)?, migration))
    }

    /// Open the existing durable store for writing; `None` when it does not
    /// exist (nothing is created).
    pub fn open_existing(root: &Path) -> Result<Option<Self>> {
        validate_state_path(root)?;
        let root_text = root_text(root)?;
        let database = root.join(MEMORY_FILE);
        if !exists(&database)? {
            return Ok(None);
        }
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| format!("open memory store {}", database.display()))?;
        configure_write(&connection)?;
        Ok(identity(&connection, root_text)?
            .map(|id| Self::new(connection, MemorySource::Durable, root_text, Some(id))))
    }

    fn open_or_create(root: &Path, at: &str, adopt: Option<&str>) -> Result<Self> {
        let root_text = root_text(root)?;
        ensure_dir(root)?;
        let database = root.join(MEMORY_FILE);
        let created = !exists(&database)?;
        let mut connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| format!("open memory store {}", database.display()))?;
        configure_write(&connection)?;
        if created {
            restrict(&database);
        }
        let id = match identity(&connection, root_text)? {
            Some(id) => id,
            None => initialize(&mut connection, root_text, at, adopt)?,
        };
        Ok(Self::new(
            connection,
            MemorySource::Durable,
            root_text,
            Some(id),
        ))
    }

    /// Open (or create, adopting `repository_id`) the durable store for a
    /// restore import.
    pub fn open_for_restore(
        root: &Path,
        at: &str,
        repository_id: Option<&str>,
    ) -> Result<(Self, Option<MigrationReport>)> {
        validate_state_path(root)?;
        let migration = migrate(root, at)?;
        Ok((Self::open_or_create(root, at, repository_id)?, migration))
    }

    pub(crate) fn require_durable(&self) -> Result<()> {
        if self.source != MemorySource::Durable {
            return fail(
                "read_only",
                "this memory view is read-only; writes open .repogi/memory.db",
            );
        }
        Ok(())
    }

    /// Every memory exactly as stored, ordered by ID, from one snapshot.
    pub fn stored_memories(&self) -> Result<Vec<StoredMemory>> {
        self.check_cancelled()?;
        read_all(&self.connection)
    }

    pub fn counts(&self) -> Result<Counts> {
        Counts::of(&self.connection)
    }

    /// Store identity, counts and store-level history (bounded).
    pub fn status(&self, limit: u32) -> Result<StoreStatus> {
        let durable = self.source == MemorySource::Durable;
        let meta = |key: &str| -> Result<Option<String>> {
            if !durable {
                return Ok(None);
            }
            Ok(self
                .connection
                .query_row(
                    "SELECT value FROM store_metadata WHERE key=?1",
                    [key],
                    |r| r.get(0),
                )
                .optional()?)
        };
        let mut events = Vec::new();
        if durable {
            let mut query = self.connection.prepare(
                "SELECT sequence,action,at,data FROM store_events ORDER BY sequence DESC LIMIT ?1",
            )?;
            let rows = query.query_map([limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (sequence, action, at, data) = row?;
                events.push(StoreEvent {
                    sequence: u64::try_from(sequence)?,
                    action,
                    at,
                    data: serde_json::from_str(&data)?,
                });
            }
            events.reverse();
        }
        Ok(StoreStatus {
            source: self.source.label(),
            schema_version: durable.then_some(MEMORY_SCHEMA_VERSION),
            repository_id: self.repository_id.clone(),
            created_at: meta("created_at")?,
            counts: self.counts()?,
            events,
        })
    }

    /// Full SQLite, foreign-key and FTS integrity checks. Read-only.
    pub fn integrity(&self) -> Result<Vec<String>> {
        integrity(&self.connection)
    }

    /// Reconcile and write restored memories in one transaction. Any
    /// divergence refuses the whole import; nothing is written then.
    #[allow(clippy::too_many_lines)] // Reconcile, apply and validate in one transaction.
    pub fn import_restore(
        &mut self,
        incoming: &[StoredMemory],
        adopt: Option<&str>,
        record: &ImportRecord<'_>,
        dry_run: bool,
    ) -> Result<Vec<ImportedMemory>> {
        self.require_durable()?;
        self.check_cancelled()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.pragma_update(None, "defer_foreign_keys", true)?;
        if let Some(adopt) = adopt {
            let current: String = tx.query_row(
                "SELECT value FROM store_metadata WHERE key='repository_id'",
                [],
                |r| r.get(0),
            )?;
            if current != adopt {
                if tx.query_row("SELECT count(*) FROM memories", [], |r| r.get::<_, i64>(0))? > 0 {
                    return fail(
                        "repository_mismatch",
                        "this memory store belongs to a different memory lineage and already holds memories; import without --restore to bring the claims in for local review, or move .repogi aside to restore",
                    );
                }
                tx.execute(
                    "UPDATE store_metadata SET value=?1 WHERE key='repository_id'",
                    [adopt],
                )?;
            }
        }
        let existing: BTreeMap<String, StoredMemory> = read_all(&tx)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        let mut outcomes = Vec::new();
        let mut conflicts = Vec::new();
        let mut plans = Vec::new();
        for memory in incoming {
            let found = existing.get(&memory.id);
            match reconcile(found, memory) {
                Reconciled::Diverged(reason) => conflicts.push(format!("{}: {reason}", memory.id)),
                plan => plans.push((memory, found, plan)),
            }
        }
        if !conflicts.is_empty() {
            return fail(
                "import_conflict",
                format!(
                    "{} memories conflict with this store and nothing was imported: {}; export this store first, then resolve by moving .repogi aside or importing without --restore",
                    conflicts.len(),
                    conflicts.join("; ")
                ),
            );
        }
        for (memory, found, plan) in plans {
            let outcome = match plan {
                Reconciled::Absent => {
                    // Exports carry no store-local event sequences.
                    insert_stored(&tx, memory, false)?;
                    "created"
                }
                Reconciled::Identical => "unchanged",
                Reconciled::Behind => "store_newer",
                Reconciled::FastForward => {
                    fast_forward(&tx, found.expect("fast-forward target"), memory)?;
                    "fast_forwarded"
                }
                Reconciled::Diverged(_) => unreachable!("conflicts refused above"),
            };
            outcomes.push(ImportedMemory {
                source_memory: memory.id.clone(),
                local_memory: Some(memory.id.clone()),
                outcome,
                detail: None,
            });
        }
        dangling_supersession(&tx)?;
        // The transaction must now hold every incoming memory exactly, or a
        // newer continuation of it.
        let after: BTreeMap<String, StoredMemory> = read_all(&tx)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        for memory in incoming {
            if !holds(&after, memory) {
                anyhow::bail!(
                    "import validation failed for {}; nothing was imported",
                    memory.id
                );
            }
        }
        store_event(
            &tx,
            "imported",
            record.at,
            &json!({
                "mode": record.mode,
                "export_digest": record.export_digest,
                "source_repository": record.source_repository,
                "actor": record.actor,
                "outcomes": tally(&outcomes),
            }),
        )?;
        crate::memory::fault("import_before_commit")?;
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()
                .context("commit memory import; nothing was imported on failure")?;
        }
        Ok(outcomes)
    }

    /// Import memories from another lineage as pending local proposals, with
    /// their complete source histories archived. One transaction; repeating an
    /// import of the same export changes nothing.
    pub fn import_review(
        &mut self,
        incoming: Vec<ReviewImport>,
        record: &ImportRecord<'_>,
        dry_run: bool,
    ) -> Result<Vec<ImportedMemory>> {
        self.require_durable()?;
        self.check_cancelled()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut outcomes = Vec::new();
        for item in incoming {
            let archived: Option<Option<String>> = tx
                .query_row(
                    "SELECT local_memory FROM memory_imports WHERE export_digest=?1 AND source_memory=?2",
                    params![record.export_digest, item.source_memory],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(local) = archived {
                outcomes.push(ImportedMemory {
                    source_memory: item.source_memory,
                    local_memory: local,
                    outcome: "already_imported",
                    detail: None,
                });
                continue;
            }
            let mut local_id = None;
            let mut outcome = "archived_only";
            let mut detail = None;
            if let (Some(mut local), Some(key)) = (item.local, item.claim_key) {
                let duplicate: Option<String> = tx
                    .query_row(
                        "SELECT m.id FROM memory_revisions r JOIN memories m ON m.id=r.memory_id
                         WHERE r.claim_key=?1 AND m.status IN ('pending_review','approved')
                           AND (r.revision IS m.approved_revision OR r.revision IS m.pending_revision)
                         ORDER BY m.id LIMIT 1",
                        [&key],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = duplicate {
                    outcome = "duplicate_claim";
                    detail = Some(format!("{existing} already records this claim"));
                } else {
                    let next: i64 = tx.query_row(
                        "SELECT coalesce(max(CAST(substr(id,5) AS INTEGER)),0)+1 FROM memories",
                        [],
                        |r| r.get(0),
                    )?;
                    local.id = format!("mem-{next:04}");
                    insert_stored(&tx, &local, false)?;
                    outcome = "pending_local_review";
                    local_id = Some(local.id);
                }
            }
            tx.execute(
                "INSERT INTO memory_imports(export_digest,source_repository,source_memory,local_memory,imported_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    record.export_digest,
                    record.source_repository,
                    item.source_memory,
                    local_id,
                    record.at,
                    item.archive
                ],
            )?;
            outcomes.push(ImportedMemory {
                source_memory: item.source_memory,
                local_memory: local_id,
                outcome,
                detail,
            });
        }
        store_event(
            &tx,
            "imported",
            record.at,
            &json!({
                "mode": record.mode,
                "export_digest": record.export_digest,
                "source_repository": record.source_repository,
                "actor": record.actor,
                "outcomes": tally(&outcomes),
            }),
        )?;
        crate::memory::fault("import_before_commit")?;
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()
                .context("commit memory import; nothing was imported on failure")?;
        }
        Ok(outcomes)
    }

    /// The archived source history of a memory imported for review, if any.
    pub fn imported_history(&self, local: &str) -> Result<Option<(String, String)>> {
        self.check_cancelled()?;
        if self.source != MemorySource::Durable {
            return Ok(None);
        }
        Ok(self
            .connection
            .query_row(
                "SELECT export_digest,data FROM memory_imports WHERE local_memory=?1 ORDER BY sequence LIMIT 1",
                [local],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
}

/// Rebind a moved repository's store to its new root. `from` must equal the
/// recorded root exactly, and the old location must not still hold this
/// store (a copy is restored through export/import instead).
pub fn rebind(root: &Path, from: &str, at: &str) -> Result<String> {
    validate_state_path(root)?;
    let root_text = root_text(root)?;
    let database = root.join(MEMORY_FILE);
    if !exists(&database)? {
        return fail("memory_store_missing", "no .repogi/memory.db to rebind");
    }
    let mut connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure_write(&connection)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (recorded, id) = recorded_identity(&tx)?;
    if recorded == root_text {
        return fail(
            "invalid_arguments",
            "the memory store already belongs to this root",
        );
    }
    if recorded != from {
        return fail(
            "memory_root_mismatch",
            "--from must name the recorded root exactly (see the error from any memory command)",
        );
    }
    let original = Path::new(from).join(MEMORY_FILE);
    if original.exists() {
        let still = Connection::open_with_flags(
            &original,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .ok()
        .and_then(|c| recorded_identity(&c).ok());
        if still.is_some_and(|(_, other)| other == id) {
            return fail(
                "memory_store_copied",
                format!(
                    "{from} still holds this memory store, so this is a copy, not a move; run `repogi memory export` there and `repogi memory import --restore` here (after moving this .repogi aside)"
                ),
            );
        }
    }
    tx.execute(
        "UPDATE store_metadata SET value=?1 WHERE key='root'",
        [root_text],
    )?;
    store_event(&tx, "rebound", at, &json!({"from": from, "to": root_text}))?;
    tx.commit()?;
    Ok(id)
}

/// Migrate memories from a schema-5 index into the durable store. A no-op
/// (`None`) when the index holds none; idempotent and safe to retry.
pub fn migrate(root: &Path, at: &str) -> Result<Option<MigrationReport>> {
    validate_state_path(root)?;
    let root_text = root_text(root)?;
    let Some(legacy_connection) = legacy(root, root_text)? else {
        return Ok(None);
    };
    // One read snapshot of the source.
    let memories = read_all(&legacy_connection)?;
    let counts = Counts::of(&legacy_connection)?;
    drop(legacy_connection);
    let digest = stored_digest(&memories, false);
    let database = root.join(MEMORY_FILE);
    let initialized = exists(&database)? && {
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        identity(&connection, root_text)?.is_some()
    };
    let mut report = if initialized {
        merge(root, at, &memories, counts, &digest)?
    } else {
        match install(root, root_text, at, &memories, counts, &digest)? {
            Some(report) => report,
            // Another process installed a store first: reconcile with it.
            None => merge(root, at, &memories, counts, &digest)?,
        }
    };
    crate::memory::fault("migrate_before_freeze")?;
    match freeze_index(root, &digest) {
        Ok(()) => report.legacy_frozen = true,
        Err(error) => report.notes.push(format!(
            "legacy memory tables in .code-intel/index.db were not frozen ({error:#}); older binaries could still write them"
        )),
    }
    Ok(Some(report))
}

/// Build, validate and install a new store holding the legacy rows. `None`
/// when a store appeared at the path meanwhile (nothing installed then).
fn install(
    root: &Path,
    root_text: &str,
    at: &str,
    memories: &[StoredMemory],
    counts: Counts,
    digest: &str,
) -> Result<Option<MigrationReport>> {
    let dir = ensure_dir(root)?;
    remove_stale_temporaries(&dir);
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let temporary = dir.join(format!(
        "memory.db.migrating-{}-{nonce}",
        std::process::id()
    ));
    let built = (|| -> Result<()> {
        let mut connection = Connection::open_with_flags(
            &temporary,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure_write(&connection)?;
        restrict(&temporary);
        initialize(&mut connection, root_text, at, None)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.pragma_update(None, "defer_foreign_keys", true)?;
        for memory in memories {
            insert_stored(&tx, memory, true)?;
        }
        record_migration(&tx, at, "migrated", counts, digest)?;
        crate::memory::fault("migrate_before_commit")?;
        tx.commit()?;
        drop(connection);
        // Validate the closed file, as installed: integrity, then every row.
        let check = Connection::open_with_flags(
            &temporary,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let problems = integrity(&check)?;
        anyhow::ensure!(
            problems.is_empty(),
            "migrated store failed integrity checks: {}",
            problems.join("; ")
        );
        let copied = read_all(&check)?;
        anyhow::ensure!(
            copied == memories && stored_digest(&copied, false) == digest,
            "migrated store does not match the index rows; nothing was installed"
        );
        anyhow::ensure!(
            Counts::of(&check)? == counts,
            "migrated store row counts differ from the index"
        );
        crate::memory::fault("migrate_before_install")?;
        Ok(())
    })();
    if let Err(error) = built {
        remove_database(&temporary);
        return Err(error.context(
            "memory migration failed; the index memories are unchanged and the migration can be retried",
        ));
    }
    // A hard link never replaces an existing store, unlike a rename.
    let installed = hard_link(&temporary, &root.join(MEMORY_FILE));
    remove_database(&temporary);
    match installed {
        Ok(()) => {
            sync_dir(&dir);
            crate::memory::fault("migrate_after_install")?;
            Ok(Some(MigrationReport {
                outcome: "migrated",
                source: ".code-intel/index.db",
                counts,
                source_digest: digest.to_owned(),
                written: memories.len(),
                legacy_frozen: false,
                notes: Vec::new(),
            }))
        }
        // Usually another migrator won; an uninitialized leftover file is
        // filled in place instead of being merged with new event sequences.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            install_in_place(root, root_text, at, memories, counts, digest)
        }
        // No hard links on this filesystem (FAT, exFAT, some network and FUSE
        // mounts): build the store at its final path in one transaction.
        Err(error) => {
            let report = install_in_place(root, root_text, at, memories, counts, digest)
                .with_context(|| {
                    format!(
                        "install .repogi/memory.db without a hard link ({error}); the index memories are unchanged"
                    )
                })?;
            Ok(report.map(|mut report| {
                report.notes.push(format!(
                    "hard links are unavailable here ({error}); the store was built in place in one transaction"
                ));
                report
            }))
        }
    }
}

/// Install without publishing a temporary file: create the store at its final
/// path and fill it in one `IMMEDIATE` transaction that is validated before it
/// commits. SQLite never truncates an existing file, the identity check runs
/// under the write lock, and a crash rolls back to an empty file that the next
/// migration fills. `None` when the path already holds an initialized store.
fn install_in_place(
    root: &Path,
    root_text: &str,
    at: &str,
    memories: &[StoredMemory],
    counts: Counts,
    digest: &str,
) -> Result<Option<MigrationReport>> {
    let database = root.join(MEMORY_FILE);
    let created = !exists(&database)?;
    let mut connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("open memory store {}", database.display()))?;
    configure_write(&connection)?;
    if created {
        restrict(&database);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if identity(&tx, root_text)?.is_some() {
        return Ok(None);
    }
    create_identity(&tx, root_text, at, None)?;
    tx.pragma_update(None, "defer_foreign_keys", true)?;
    for memory in memories {
        insert_stored(&tx, memory, true)?;
    }
    record_migration(&tx, at, "migrated", counts, digest)?;
    let problems = integrity(&tx)?;
    anyhow::ensure!(
        problems.is_empty(),
        "migrated store failed integrity checks: {}; nothing was installed",
        problems.join("; ")
    );
    let copied = read_all(&tx)?;
    anyhow::ensure!(
        copied == memories && stored_digest(&copied, false) == digest && Counts::of(&tx)? == counts,
        "migrated store does not match the index rows; nothing was installed"
    );
    crate::memory::fault("migrate_in_place_before_commit")?;
    tx.commit()
        .context("commit the migrated store; nothing was installed on failure")?;
    Ok(Some(MigrationReport {
        outcome: "migrated",
        source: ".code-intel/index.db",
        counts,
        source_digest: digest.to_owned(),
        written: memories.len(),
        legacy_frozen: false,
        notes: Vec::new(),
    }))
}

/// Reconcile legacy rows into an existing store in one transaction.
fn merge(
    root: &Path,
    at: &str,
    memories: &[StoredMemory],
    counts: Counts,
    digest: &str,
) -> Result<MigrationReport> {
    let mut store = MemoryStore::open_or_create(root, at, None)?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let recorded: i64 = tx.query_row(
        "SELECT count(*) FROM store_events WHERE action='migrated_from_index' AND json_extract(data,'$.source_digest')=?1",
        [digest],
        |r| r.get(0),
    )?;
    if recorded > 0 {
        tx.commit()?;
        return Ok(MigrationReport {
            outcome: "already_migrated",
            source: ".code-intel/index.db",
            counts,
            source_digest: digest.to_owned(),
            written: 0,
            legacy_frozen: false,
            notes: Vec::new(),
        });
    }
    tx.pragma_update(None, "defer_foreign_keys", true)?;
    let existing: BTreeMap<String, StoredMemory> = read_all(&tx)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    let mut conflicts = Vec::new();
    let mut written = 0;
    for memory in memories {
        match reconcile(existing.get(&memory.id), memory) {
            Reconciled::Absent => {
                insert_stored(&tx, memory, false)?;
                written += 1;
            }
            Reconciled::Identical | Reconciled::Behind => {}
            Reconciled::FastForward => {
                fast_forward(&tx, &existing[&memory.id], memory)?;
                written += 1;
            }
            Reconciled::Diverged(reason) => conflicts.push(format!("{}: {reason}", memory.id)),
        }
    }
    if !conflicts.is_empty() {
        return fail(
            "migration_conflict",
            format!(
                "the index holds memories that conflict with .repogi/memory.db and nothing was migrated: {}; both copies are unchanged; export the store and resolve explicitly",
                conflicts.join("; ")
            ),
        );
    }
    dangling_supersession(&tx)?;
    let after: BTreeMap<String, StoredMemory> = read_all(&tx)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    for memory in memories {
        anyhow::ensure!(
            holds(&after, memory),
            "migration validation failed for {}; nothing was migrated",
            memory.id
        );
    }
    record_migration(&tx, at, "merged", counts, digest)?;
    crate::memory::fault("migrate_before_commit")?;
    tx.commit()
        .context("commit memory migration; nothing was migrated on failure")?;
    Ok(MigrationReport {
        outcome: if written == 0 {
            "already_migrated"
        } else {
            "merged"
        },
        source: ".code-intel/index.db",
        counts,
        source_digest: digest.to_owned(),
        written,
        legacy_frozen: false,
        notes: Vec::new(),
    })
}

/// The store holds `memory` exactly or a newer continuation of it.
fn holds(store: &BTreeMap<String, StoredMemory>, memory: &StoredMemory) -> bool {
    matches!(
        reconcile(store.get(&memory.id), memory),
        Reconciled::Identical | Reconciled::Behind
    )
}

fn record_migration(
    tx: &Transaction<'_>,
    at: &str,
    outcome: &str,
    counts: Counts,
    digest: &str,
) -> Result<()> {
    store_event(
        tx,
        "migrated_from_index",
        at,
        &json!({
            "source": ".code-intel/index.db",
            "outcome": outcome,
            "source_digest": digest,
            "counts": counts,
        }),
    )
}

fn store_event(tx: &Connection, action: &str, at: &str, data: &serde_json::Value) -> Result<()> {
    tx.execute(
        "INSERT INTO store_events(action,at,data) VALUES(?1,?2,?3)",
        params![action, at, serde_json::to_string(data)?],
    )?;
    Ok(())
}

fn tally(outcomes: &[ImportedMemory]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for outcome in outcomes {
        *counts.entry(outcome.outcome).or_insert(0) += 1;
    }
    counts
}

fn dangling_supersession(tx: &Connection) -> Result<()> {
    let dangling: Option<String> = tx
        .query_row(
            "SELECT id FROM memories WHERE superseded_by IS NOT NULL AND superseded_by NOT IN (SELECT id FROM memories) ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = dangling {
        return fail(
            "import_invalid",
            format!("{id} is superseded by a memory that is neither imported nor present"),
        );
    }
    Ok(())
}

/// Compare an incoming memory with the destination's copy.
#[must_use]
pub fn reconcile(existing: Option<&StoredMemory>, incoming: &StoredMemory) -> Reconciled {
    let Some(existing) = existing else {
        return Reconciled::Absent;
    };
    if existing.portable() == incoming.portable() {
        return Reconciled::Identical;
    }
    if existing.memory_type != incoming.memory_type || existing.created_at != incoming.created_at {
        return Reconciled::Diverged("a different memory has this ID".into());
    }
    match (extends(existing, incoming), extends(incoming, existing)) {
        (Ok(()), _) => Reconciled::FastForward,
        (_, Ok(())) => Reconciled::Behind,
        (Err(reason), _) => Reconciled::Diverged(reason),
    }
}

/// Whether `longer` continues `base`: the same revisions (a proposed revision
/// may since have been reviewed), then more; the same events, then more.
fn extends(base: &StoredMemory, longer: &StoredMemory) -> std::result::Result<(), String> {
    if base.revisions.len() > longer.revisions.len()
        || base.events.len() > longer.events.len()
        || base.search.len() > longer.search.len()
        || base.version > longer.version
    {
        return Err("each copy has history the other lacks".into());
    }
    for (have, take) in base.revisions.iter().zip(&longer.revisions) {
        let reviewed_since = have.state == "proposed"
            && matches!(take.state.as_str(), "approved" | "rejected")
            && StoredRevision {
                state: take.state.clone(),
                ..have.clone()
            } == *take;
        if have != take && !reviewed_since {
            return Err(format!(
                "revision {} differs between the copies",
                have.revision
            ));
        }
    }
    for (have, take) in base.events.iter().zip(&longer.events) {
        if (have.revision, &have.action, &have.data) != (take.revision, &take.action, &take.data) {
            return Err("the review histories differ".into());
        }
    }
    if longer.search[..base.search.len()] != base.search[..] {
        return Err("search rows differ".into());
    }
    Ok(())
}

/// Advance `existing` to `incoming` (a validated fast-forward).
fn fast_forward(
    tx: &Transaction<'_>,
    existing: &StoredMemory,
    incoming: &StoredMemory,
) -> Result<()> {
    for (have, take) in existing.revisions.iter().zip(&incoming.revisions) {
        if have.state != take.state {
            tx.execute(
                "UPDATE memory_revisions SET state=?3 WHERE memory_id=?1 AND revision=?2",
                params![incoming.id, take.revision, take.state],
            )?;
        }
    }
    for revision in &incoming.revisions[existing.revisions.len()..] {
        insert_revision(tx, &incoming.id, revision)?;
    }
    for search in &incoming.search[existing.search.len()..] {
        insert_search(tx, &incoming.id, search)?;
    }
    for event in &incoming.events[existing.events.len()..] {
        insert_event(tx, &incoming.id, event, false)?;
    }
    tx.execute(
        "UPDATE memories SET status=?2, latest_revision=?3, approved_revision=?4, pending_revision=?5, superseded_by=?6, updated_at=?7, version=?8 WHERE id=?1",
        params![
            incoming.id,
            incoming.status,
            incoming.latest_revision,
            incoming.approved_revision,
            incoming.pending_revision,
            incoming.superseded_by,
            incoming.updated_at,
            i64::try_from(incoming.version)?
        ],
    )?;
    Ok(())
}

/// Insert one memory row for row. Callers defer foreign keys so supersession
/// targets may follow.
pub(crate) fn insert_stored(
    tx: &Connection,
    memory: &StoredMemory,
    keep_sequences: bool,
) -> Result<()> {
    tx.execute(
        "INSERT INTO memories(id,type,status,latest_revision,approved_revision,pending_revision,superseded_by,created_at,updated_at,version) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            memory.id,
            memory.memory_type,
            memory.status,
            memory.latest_revision,
            memory.approved_revision,
            memory.pending_revision,
            memory.superseded_by,
            memory.created_at,
            memory.updated_at,
            i64::try_from(memory.version)?
        ],
    )?;
    for revision in &memory.revisions {
        insert_revision(tx, &memory.id, revision)?;
    }
    for search in &memory.search {
        insert_search(tx, &memory.id, search)?;
    }
    for event in &memory.events {
        insert_event(tx, &memory.id, event, keep_sequences)?;
    }
    Ok(())
}

fn insert_revision(tx: &Connection, id: &str, revision: &StoredRevision) -> Result<()> {
    tx.execute(
        "INSERT INTO memory_revisions(memory_id,revision,state,digest,claim_key,assertion_subject,assertion_status,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            id,
            revision.revision,
            revision.state,
            revision.digest,
            revision.claim_key,
            revision.assertion_subject,
            revision.assertion_status,
            revision.data
        ],
    )?;
    for evidence in &revision.evidence {
        tx.execute(
            "INSERT INTO memory_evidence(memory_id,revision,ordinal,role,class,subject_id,explicit_id,target_id,path,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                id,
                revision.revision,
                evidence.ordinal,
                evidence.role,
                evidence.class,
                evidence.subject_id,
                evidence.explicit_id,
                evidence.target_id,
                evidence.path,
                evidence.data
            ],
        )?;
    }
    Ok(())
}

fn insert_search(tx: &Connection, id: &str, search: &StoredSearch) -> Result<()> {
    tx.execute(
        "INSERT INTO memory_search(memory_id,revision,title,claim,subjects) VALUES(?1,?2,?3,?4,?5)",
        params![
            id,
            search.revision,
            search.title,
            search.claim,
            search.subjects
        ],
    )?;
    Ok(())
}

fn insert_event(tx: &Connection, id: &str, event: &StoredEvent, keep_sequence: bool) -> Result<()> {
    tx.execute(
        "INSERT INTO memory_events(sequence,memory_id,revision,action,data) VALUES(?1,?2,?3,?4,?5)",
        params![
            keep_sequence
                .then(|| i64::try_from(event.sequence))
                .transpose()?,
            id,
            event.revision,
            event.action,
            event.data
        ],
    )?;
    Ok(())
}

/// Every memory with all of its rows, ordered by ID; one pass per table.
#[allow(clippy::too_many_lines)] // One pass per table, in schema order.
pub(crate) fn read_all(connection: &Connection) -> Result<Vec<StoredMemory>> {
    let mut memories: BTreeMap<String, StoredMemory> = BTreeMap::new();
    {
        let mut query = connection.prepare(
            "SELECT id,type,status,latest_revision,approved_revision,pending_revision,superseded_by,created_at,updated_at,version FROM memories ORDER BY id",
        )?;
        let rows = query.query_map([], |r| {
            Ok(StoredMemory {
                id: r.get(0)?,
                memory_type: r.get(1)?,
                status: r.get(2)?,
                latest_revision: r.get(3)?,
                approved_revision: r.get(4)?,
                pending_revision: r.get(5)?,
                superseded_by: r.get(6)?,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
                version: u64::try_from(r.get::<_, i64>(9)?).unwrap_or_default(),
                revisions: Vec::new(),
                events: Vec::new(),
                search: Vec::new(),
            })
        })?;
        for row in rows {
            let memory = row?;
            memories.insert(memory.id.clone(), memory);
        }
    }
    let orphan = |table: &str, id: &str| anyhow::anyhow!("{table} row for unknown memory {id:?}");
    {
        let mut query = connection.prepare(
            "SELECT memory_id,revision,state,digest,claim_key,assertion_subject,assertion_status,data FROM memory_revisions ORDER BY memory_id,revision",
        )?;
        let mut rows = query.query([])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            memories
                .get_mut(&id)
                .ok_or_else(|| orphan("memory_revisions", &id))?
                .revisions
                .push(StoredRevision {
                    revision: r.get(1)?,
                    state: r.get(2)?,
                    digest: r.get(3)?,
                    claim_key: r.get(4)?,
                    assertion_subject: r.get(5)?,
                    assertion_status: r.get(6)?,
                    data: r.get(7)?,
                    evidence: Vec::new(),
                });
        }
    }
    {
        let mut query = connection.prepare(
            "SELECT memory_id,revision,ordinal,role,class,subject_id,explicit_id,target_id,path,data FROM memory_evidence ORDER BY memory_id,revision,ordinal",
        )?;
        let mut rows = query.query([])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            let number: u32 = r.get(1)?;
            let revision = memories
                .get_mut(&id)
                .and_then(|m| m.revisions.iter_mut().find(|v| v.revision == number))
                .ok_or_else(|| orphan("memory_evidence", &id))?;
            revision.evidence.push(StoredEvidence {
                ordinal: r.get(2)?,
                role: r.get(3)?,
                class: r.get(4)?,
                subject_id: r.get(5)?,
                explicit_id: r.get(6)?,
                target_id: r.get(7)?,
                path: r.get(8)?,
                data: r.get(9)?,
            });
        }
    }
    {
        let mut query = connection.prepare(
            "SELECT memory_id,sequence,revision,action,data FROM memory_events ORDER BY memory_id,sequence",
        )?;
        let mut rows = query.query([])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            memories
                .get_mut(&id)
                .ok_or_else(|| orphan("memory_events", &id))?
                .events
                .push(StoredEvent {
                    sequence: u64::try_from(r.get::<_, i64>(1)?)?,
                    revision: r.get(2)?,
                    action: r.get(3)?,
                    data: r.get(4)?,
                });
        }
    }
    {
        // One scan of the FTS table (memory_id is UNINDEXED), sorted here.
        let mut query = connection
            .prepare("SELECT memory_id,revision,title,claim,subjects FROM memory_search")?;
        let mut rows = query.query([])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            memories
                .get_mut(&id)
                .ok_or_else(|| orphan("memory_search", &id))?
                .search
                .push(StoredSearch {
                    revision: r.get(1)?,
                    title: r.get(2)?,
                    claim: r.get(3)?,
                    subjects: r.get(4)?,
                });
        }
        for memory in memories.values_mut() {
            memory.search.sort_by(|a, b| {
                (a.revision, &a.title, &a.claim, &a.subjects).cmp(&(
                    b.revision,
                    &b.title,
                    &b.claim,
                    &b.subjects,
                ))
            });
        }
    }
    Ok(memories.into_values().collect())
}

impl Counts {
    fn of(connection: &Connection) -> Result<Self> {
        let count = |sql: &str| -> Result<u64> {
            Ok(u64::try_from(
                connection.query_row(sql, [], |r| r.get::<_, i64>(0))?,
            )?)
        };
        Ok(Self {
            memories: count("SELECT count(*) FROM memories")?,
            revisions: count("SELECT count(*) FROM memory_revisions")?,
            evidence: count("SELECT count(*) FROM memory_evidence")?,
            events: count("SELECT count(*) FROM memory_events")?,
            max_sequence: count("SELECT coalesce(max(sequence),0) FROM memory_events")?,
        })
    }
}

fn integrity(connection: &Connection) -> Result<Vec<String>> {
    let mut problems: Vec<String> = Vec::new();
    {
        let mut query = connection.prepare("PRAGMA integrity_check")?;
        let rows = query.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let row = row?;
            if row != "ok" {
                problems.push(format!("integrity: {row}"));
            }
        }
    }
    {
        let mut query = connection.prepare("PRAGMA foreign_key_check")?;
        let rows = query.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            problems.push(format!("foreign key violation in {}", row?));
        }
    }
    // FTS5's own consistency check is a special INSERT, so it needs a
    // writable connection; it changes nothing.
    if !connection.is_readonly(rusqlite::MAIN_DB)?
        && let Err(error) = connection.execute(
            "INSERT INTO memory_search(memory_search) VALUES('integrity-check')",
            [],
        )
    {
        problems.push(format!("search index: {error}"));
    }
    let rows: i64 = connection.query_row("SELECT count(*) FROM memory_search", [], |r| r.get(0))?;
    let revisions: i64 =
        connection.query_row("SELECT count(*) FROM memory_revisions", [], |r| r.get(0))?;
    if rows != revisions {
        problems.push(format!(
            "search index has {rows} rows for {revisions} revisions"
        ));
    }
    Ok(problems)
}

/// The store's repository ID after checking type, schema and root, or `None`
/// for an empty, uninitialized file.
fn identity(connection: &Connection, root: &str) -> Result<Option<String>> {
    let application: i32 = connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if application == 0 && version == 0 {
        let tables: i64 =
            connection.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
        if tables == 0 {
            return Ok(None);
        }
    }
    if application != MEMORY_APPLICATION_ID {
        return fail(
            "memory_store_invalid",
            ".repogi/memory.db is not a Repogi memory store; move it aside (never delete it before inspecting it)",
        );
    }
    if version > MEMORY_SCHEMA_VERSION {
        return fail(
            "memory_store_newer",
            format!(
                "memory store schema {version} is newer than supported schema {MEMORY_SCHEMA_VERSION}; use a compatible repogi binary (or export with the newer binary); nothing was changed"
            ),
        );
    }
    if version == 0 {
        return fail(
            "memory_store_invalid",
            ".repogi/memory.db has no memory schema version; move it aside and restore from an export",
        );
    }
    let (recorded, id) = recorded_identity(connection)?;
    if recorded != root {
        return fail(
            "memory_root_mismatch",
            format!(
                "the memory store belongs to {recorded:?}, not this repository; if the repository moved, run `repogi memory rebind --from {recorded:?}`; if it was copied, move .repogi aside and restore with `repogi memory import --restore`"
            ),
        );
    }
    Ok(Some(id))
}

/// (root, repository ID) as recorded, after checking the store kind.
fn recorded_identity(connection: &Connection) -> Result<(String, String)> {
    let meta = |key: &str| -> Result<Option<String>> {
        Ok(connection
            .query_row(
                "SELECT value FROM store_metadata WHERE key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?)
    };
    if meta("store_kind")?.as_deref() != Some(STORE_KIND) {
        return fail(
            "memory_store_invalid",
            ".repogi/memory.db does not identify itself as a Repogi memory store",
        );
    }
    match (meta("root")?, meta("repository_id")?) {
        (Some(root), Some(id)) => Ok((root, id)),
        _ => fail(
            "memory_store_invalid",
            ".repogi/memory.db records no repository identity",
        ),
    }
}

/// Create the schema and identity of a new store; concurrent creators of one
/// file initialize it exactly once.
fn initialize(
    connection: &mut Connection,
    root: &str,
    at: &str,
    adopt: Option<&str>,
) -> Result<String> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(id) = identity(&tx, root)? {
        tx.commit()?;
        return Ok(id);
    }
    let id = create_identity(&tx, root, at, adopt)?;
    tx.commit().context("create .repogi/memory.db")?;
    Ok(id)
}

/// Schema, metadata and `created` event of a new store, inside the caller's
/// write transaction.
fn create_identity(tx: &Connection, root: &str, at: &str, adopt: Option<&str>) -> Result<String> {
    tx.execute_batch(STORE_SCHEMA)?;
    tx.execute_batch(MIGRATION_5)?;
    let id: String = match adopt {
        Some(id) => id.to_owned(),
        None => tx.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?,
    };
    tx.execute(
        "INSERT INTO store_metadata(key,value) VALUES('store_kind',?1),('repository_id',?2),('root',?3),('created_at',?4),('created_by',?5)",
        params![
            STORE_KIND,
            id,
            root,
            at,
            concat!("repogi ", env!("CARGO_PKG_VERSION"))
        ],
    )?;
    store_event(
        tx,
        "created",
        at,
        &json!({"schema": MEMORY_SCHEMA_VERSION, "adopted_repository_id": adopt.is_some()}),
    )?;
    tx.pragma_update(None, "application_id", MEMORY_APPLICATION_ID)?;
    tx.pragma_update(None, "user_version", MEMORY_SCHEMA_VERSION)?;
    Ok(id)
}

/// Read-only connection to memories in a schema-5 index of this root, when it
/// holds at least one. Refuses an index recorded for another root.
fn legacy(root: &Path, root_text: &str) -> Result<Option<Connection>> {
    let database = root.join(".code-intel/index.db");
    if !database.is_file() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("open index {}", database.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != 5 || !has_legacy_tables(&connection)? {
        return Ok(None);
    }
    connection.execute_batch("BEGIN DEFERRED")?;
    let recorded: Option<String> = connection
        .query_row("SELECT value FROM metadata WHERE key='root'", [], |r| {
            r.get(0)
        })
        .optional()?;
    if recorded.as_deref() != Some(root_text) {
        anyhow::bail!(
            "index belongs to a different repository root; memory is scoped to the repository that owns the index"
        );
    }
    let count: i64 = connection.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?;
    Ok((count > 0).then_some(connection))
}

fn has_legacy_tables(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT count(*)=5 FROM sqlite_master WHERE name IN (SELECT value FROM json_each(?1))",
        [serde_json::to_string(&LEGACY_TABLES)?],
        |r| r.get(0),
    )?)
}

/// A note when the index still holds memories no migration record covers.
fn legacy_notes(root: &Path, store: &Connection) -> Result<Vec<(&'static str, String)>> {
    let Some(legacy) = legacy(root, root_text(root)?).ok().flatten() else {
        return Ok(Vec::new());
    };
    let counts = Counts::of(&legacy)?;
    let covered: i64 = store.query_row(
        "SELECT count(*) FROM store_events WHERE action='migrated_from_index' AND json_extract(data,'$.counts')=json(?1)",
        // Stored records have sorted keys (serde_json::Value), as does this.
        [serde_json::to_value(counts)?.to_string()],
        |r| r.get(0),
    )?;
    if covered > 0 {
        return Ok(Vec::new());
    }
    Ok(vec![(
        "legacy_memory_unmigrated",
        format!(
            "the code index holds {} memories that no migration has copied into .repogi/memory.db; run `repogi memory migrate`",
            counts.memories
        ),
    )])
}

/// Freeze schema-5 memory tables against writes (older binaries write them
/// directly). Reads and migrations are unaffected. Requires the tables.
pub(crate) fn freeze_legacy(connection: &Connection) -> Result<()> {
    if !has_legacy_tables(connection)? {
        return Ok(());
    }
    let mut sql = String::new();
    for (table, operations) in [
        ("memories", &["INSERT", "UPDATE"][..]),
        ("memory_revisions", &["INSERT", "UPDATE"][..]),
        ("memory_evidence", &["INSERT"][..]),
        ("memory_events", &["INSERT"][..]),
    ] {
        for operation in operations {
            let _ = write!(
                sql,
                "CREATE TRIGGER IF NOT EXISTS memory_moved_{table}_{} BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT, '{MOVED}'); END;",
                operation.to_lowercase()
            );
        }
    }
    connection.execute_batch(&sql)?;
    Ok(())
}

fn freeze_index(root: &Path, digest: &str) -> Result<()> {
    let database = root.join(".code-intel/index.db");
    let mut connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    freeze_legacy(&tx)?;
    tx.execute(
        "INSERT INTO metadata(key,value) VALUES('memory_migrated',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [digest],
    )?;
    tx.commit()?;
    Ok(())
}

/// Remove every memory row from an index copy (an impact snapshot), zeroing
/// the freed pages, and recreate the empty schema-5 memory tables.
pub(crate) fn scrub_memory(connection: &mut Connection) -> Result<()> {
    if !has_legacy_tables(connection)? {
        return Ok(());
    }
    let rows: i64 = connection.query_row(
        "SELECT (SELECT count(*) FROM memories)+(SELECT count(*) FROM memory_revisions)+(SELECT count(*) FROM memory_evidence)+(SELECT count(*) FROM memory_events)+(SELECT count(*) FROM memory_search)",
        [],
        |r| r.get(0),
    )?;
    if rows == 0 {
        return Ok(());
    }
    connection.pragma_update(None, "secure_delete", true)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "DROP TABLE memory_search; DROP TABLE memory_events; DROP TABLE memory_evidence; DROP TABLE memory_revisions; DROP TABLE memories;",
    )?;
    tx.execute_batch(MIGRATION_5)?;
    freeze_legacy(&tx)?;
    tx.commit()?;
    Ok(())
}

fn configure_write(connection: &Connection) -> Result<()> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let mode: String =
        connection.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))?;
    anyhow::ensure!(
        mode.eq_ignore_ascii_case("delete"),
        "memory store journal mode is {mode}, expected delete"
    );
    connection.pragma_update(None, "synchronous", "FULL")?;
    // On macOS, plain fsync can leave commits in the drive cache; F_FULLFSYNC
    // flushes them. A no-op on other platforms.
    connection.pragma_update(None, "fullfsync", "ON")?;
    Ok(())
}

/// Create `.repogi/` (owner-only, with a `.gitignore` excluding everything);
/// refuses a symlinked or non-directory entry.
fn ensure_dir(root: &Path) -> Result<PathBuf> {
    let dir = root.join(MEMORY_DIR);
    match fs::symlink_metadata(&dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&dir) {
                Ok(()) => {
                    let ignore = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(dir.join(".gitignore"));
                    if let Ok(mut file) = ignore {
                        std::io::Write::write_all(&mut file, GITIGNORE.as_bytes())?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(anyhow::Error::from(error)
                        .context("create .repogi; check repository permissions"));
                }
            }
        }
        Err(error) => return Err(anyhow::Error::from(error).context("inspect .repogi")),
    }
    validate_state_path(root)?;
    let meta = fs::symlink_metadata(&dir)?;
    anyhow::ensure!(
        meta.is_dir(),
        "{} is not a directory; move it aside",
        dir.display()
    );
    Ok(dir)
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(anyhow::Error::from(error).context(format!("inspect {}", path.display())))
        }
    }
}

fn root_text(root: &Path) -> Result<&str> {
    root.to_str().context("repository root must be valid UTF-8")
}

/// Owner-only permissions on a new database file (best effort).
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `fs::hard_link`, which debug builds can fail as an unsupported filesystem.
fn hard_link(from: &Path, to: &Path) -> std::io::Result<()> {
    if crate::memory::fault("migrate_hard_link").is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "injected: hard links are not supported",
        ));
    }
    fs::hard_link(from, to)
}

fn remove_database(path: &Path) {
    let _ = fs::remove_file(path);
    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    let _ = fs::remove_file(PathBuf::from(journal));
}

fn remove_stale_temporaries(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > STALE_TEMPORARY);
        if name
            .to_str()
            .is_some_and(|n| n.starts_with("memory.db.migrating-"))
            && stale
            && entry.file_type().is_ok_and(|t| t.is_file())
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}
