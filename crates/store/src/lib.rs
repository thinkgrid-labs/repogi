//! SQLite persistence, versioned schema, FTS5, and bounded graph reads.
mod knowledge;
mod relationships;
mod semantic;

pub use knowledge::DocumentUpdate;

pub use semantic::profile;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use code_intel_core::semantic::{ResolutionContext, SemanticEdge};
use code_intel_core::{Edge, Fact, IndexReport, IndexedFile, SCHEMA_VERSION, Status, file_id};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

const MIGRATION_1: &str = include_str!("migration_1.sql");
const DATABASE_VERSION: u32 = 4;
const MIGRATION_4: &str = include_str!("migration_4.sql");
const MIGRATION_2: &str = include_str!("migration_2.sql");
const MIGRATION_3: &str = include_str!("migration_3.sql");

/// A declaration's identity fields, without its source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub id: String,
    pub path: String,
    pub kind: String,
    pub name: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

pub struct Store {
    connection: Connection,
    cancelled: Arc<AtomicBool>,
}

/// Cross-thread SQLite interrupt; a no-op when no statement is running.
pub struct Interrupt {
    sqlite: rusqlite::InterruptHandle,
    cancelled: Arc<AtomicBool>,
}

impl Interrupt {
    pub fn interrupt(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.sqlite.interrupt();
    }
}

impl Store {
    pub fn open_index(database: &Path, root: &Path) -> Result<Self> {
        let mut connection = Connection::open_with_flags(
            database,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| {
            format!(
                "open index {}; check directory permissions",
                database.display()
            )
        })?;
        configure(&connection)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version <= DATABASE_VERSION,
            "index schema {version} is newer than supported schema {DATABASE_VERSION}; use a compatible repogi binary"
        );
        if version > 0 {
            let stored: Option<String> = connection
                .query_row("SELECT value FROM metadata WHERE key='root'", [], |row| {
                    row.get(0)
                })
                .optional()?;
            // Another first opener can have committed schema creation and not
            // yet registered ownership. The atomic insert/check below handles
            // that case; an established owner is checked before any migration.
            ensure!(
                stored
                    .as_deref()
                    .is_none_or(|stored| Some(stored) == root.to_str()),
                "index belongs to a different repository root"
            );
        }
        if version < DATABASE_VERSION {
            // Write-locked, and the version re-read inside: concurrent first
            // opens of one database migrate it exactly once.
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let version: u32 =
                transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if version == 0 {
                transaction.execute_batch(MIGRATION_1)?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            if version < 2 {
                transaction.execute_batch(MIGRATION_2)?;
            }
            if version < 3 {
                transaction.execute_batch(MIGRATION_3)?;
            }
            if version < 4 {
                transaction.execute_batch(MIGRATION_4)?;
            }
            transaction.pragma_update(None, "user_version", DATABASE_VERSION)?;
            transaction.commit()?;
        }
        // Additive lookup indexes (no data change; older binaries ignore them):
        // replacing many files otherwise scans these tables once per file.
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS facts_path ON facts(path); CREATE INDEX IF NOT EXISTS edges_path ON edges(path);",
        )?;
        let stored_root: Option<String> = connection
            .query_row("SELECT value FROM metadata WHERE key='root'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let root_text = root
            .to_str()
            .context("repository root must be valid UTF-8")?;
        if stored_root.is_none() {
            // Another process may register the root at the same time.
            connection.execute(
                "INSERT OR IGNORE INTO metadata(key,value) VALUES('root',?1),('revision','0')",
                [root_text],
            )?;
        }
        let stored_root: String =
            connection.query_row("SELECT value FROM metadata WHERE key='root'", [], |row| {
                row.get(0)
            })?;
        ensure!(
            stored_root == root_text,
            "index belongs to a different repository root; move .code-intel aside and re-index"
        );
        Ok(Self {
            connection,
            cancelled: Arc::default(),
        })
    }

    /// A read transaction keeps revision, FTS, and graph results in one snapshot.
    pub fn open_read(database: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| {
            format!(
                "open index {}; run `repogi index <path>` first",
                database.display()
            )
        })?;
        configure(&connection)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            (1..=DATABASE_VERSION).contains(&version),
            "unsupported index schema {version}; run `repogi index <path>` with a compatible binary"
        );
        connection.execute_batch("BEGIN DEFERRED")?;
        Ok(Self {
            connection,
            cancelled: Arc::default(),
        })
    }

    pub fn revision(&self) -> Result<u64> {
        self.check_cancelled()?;
        let text: String = self.connection.query_row(
            "SELECT value FROM metadata WHERE key='revision'",
            [],
            |row| row.get(0),
        )?;
        text.parse()
            .context("invalid index revision; rebuild the index")
    }

    pub fn hashes(&self) -> Result<BTreeMap<String, String>> {
        self.check_cancelled()?;
        let mut query = self
            .connection
            .prepare("SELECT path,content_hash FROM files ORDER BY path")?;
        Ok(query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// One indexed source digest, without enumerating the repository inventory.
    pub fn file_hash(&self, path: &str) -> Result<Option<String>> {
        self.check_cancelled()?;
        Ok(self
            .connection
            .query_row(
                "SELECT content_hash FROM files WHERE path=?1",
                [path],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Selector suggestions backed by resolved use sites, never name inference.
    /// The caller must keep its original not-found/ambiguous result explicit.
    pub fn targets_named(&self, name: &str, path: Option<&str>, limit: u32) -> Result<Vec<Fact>> {
        self.check_cancelled()?;
        // Check the join's non-null constraint before JSON decoding. This skips
        // unrelated unbound edges even when SQLite chooses a table scan.
        let mut query = self.connection.prepare("SELECT DISTINCT f.data FROM semantic_edges e JOIN facts f ON f.id=e.target_id WHERE e.target_id IS NOT NULL AND json_extract(e.data,'$.status')='resolved' AND json_extract(e.data,'$.target_text')=?1 AND (?2 IS NULL OR f.path=?2) ORDER BY f.path,f.id LIMIT ?3")?;
        query
            .query_map(params![name, path, limit], |row| row.get::<_, String>(0))?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    pub fn semantic_missing(&self) -> Result<BTreeSet<String>> {
        let mut query = self.connection.prepare("SELECT path FROM files WHERE path NOT IN (SELECT path FROM semantic_modules) ORDER BY path")?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Stable source-order semantic edges. A NULL target is never a confirmed binding.
    pub fn semantic_edges(
        &self,
        path: Option<&str>,
        source: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SemanticEdge>> {
        let mut query = self.connection.prepare("SELECT data FROM semantic_edges WHERE (?1 IS NULL OR path=?1) AND (?2 IS NULL OR source_id=?2) ORDER BY ordinal LIMIT ?3").context("semantic graph unavailable; run `repogi index <path>` to migrate and populate it")?;
        query
            .query_map(params![path, source, limit], |row| row.get::<_, String>(0))?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    /// Component-normalized symbol search; deliberately separate from frozen Phase 1 FTS.
    pub fn semantic_search(&self, expression: &str, limit: u32) -> Result<Vec<Fact>> {
        let mut query = self.connection.prepare("SELECT f.data FROM semantic_search s JOIN facts f ON f.id=s.id WHERE semantic_search MATCH ?1 ORDER BY bm25(semantic_search),f.id LIMIT ?2").context("semantic search unavailable; re-index this repository")?;
        query
            .query_map(params![expression, limit], |row| row.get::<_, String>(0))?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    /// Cap verified dependency sites rather than unrelated unresolved properties.
    pub fn semantic_dependencies(&self, source: &str, limit: u32) -> Result<Vec<SemanticEdge>> {
        let mut query = self.connection.prepare("SELECT data FROM semantic_edges WHERE source_id=?1 AND json_extract(data,'$.status')='resolved' AND json_extract(data,'$.kind') IN ('CALLS','REFERENCES','EXTENDS') ORDER BY ordinal LIMIT ?2").context("semantic graph unavailable; re-index this repository")?;
        query
            .query_map(params![source, limit], |row| row.get::<_, String>(0))?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    /// Repository root recorded when the index was created.
    /// Version of the cached syntax IR (absent before IR 2).
    pub fn semantic_ir_version(&self) -> Result<Option<u32>> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key='semantic_ir'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| value.parse().ok()))
    }

    /// The build (`indexer_build`) that last wrote this database, when recorded.
    pub fn indexer_build(&self) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key='indexer_build'",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Record the build that wrote this database (after a successful update).
    pub fn record_build(&self, build: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO metadata(key,value) VALUES('indexer_build',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [build],
        )?;
        Ok(())
    }

    /// Edge count, for snapshot validation.
    pub fn semantic_edge_count(&self) -> Result<usize> {
        let count: i64 =
            self.connection
                .query_row("SELECT count(*) FROM semantic_edges", [], |row| row.get(0))?;
        Ok(usize::try_from(count)?)
    }

    /// Fingerprint of the module-resolution settings the graph was bound with.
    pub fn resolution_fingerprint(&self) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key='module_resolution'",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn root(&self) -> Result<Option<String>> {
        self.check_cancelled()?;
        Ok(self
            .connection
            .query_row("SELECT value FROM metadata WHERE key='root'", [], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// False for schema-1 snapshots that predate the typed graph.
    pub fn has_semantic(&self) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT count(*)=2 FROM sqlite_master WHERE type='table' AND name IN ('semantic_edges','semantic_modules')",
            [],
            |row| row.get(0),
        )?)
    }

    /// Typed edge counts by resolution status, ordered by status.
    pub fn semantic_status_counts(&self) -> Result<BTreeMap<String, usize>> {
        let mut query = self.connection.prepare("SELECT json_extract(data,'$.status'),count(*) FROM semantic_edges GROUP BY 1 ORDER BY 1")?;
        query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .map(|row| {
                let (status, count) = row?;
                Ok((status, usize::try_from(count)?))
            })
            .collect()
    }

    /// Resolved edges whose canonical target is `target`, in stable source order.
    pub fn semantic_incoming(
        &self,
        target: &str,
        kinds: &[&str],
        limit: u32,
    ) -> Result<Vec<SemanticEdge>> {
        let mut query = self.connection.prepare("SELECT data FROM semantic_edges WHERE target_id=?1 AND json_extract(data,'$.status')='resolved' AND json_extract(data,'$.kind') IN (SELECT value FROM json_each(?2)) ORDER BY ordinal LIMIT ?3").context("semantic graph unavailable; re-index this repository")?;
        query
            .query_map(
                params![target, serde_json::to_string(kinds)?, limit],
                |row| row.get::<_, String>(0),
            )?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    pub fn semantic_incoming_count(&self, target: &str, kinds: &[&str]) -> Result<usize> {
        self.check_cancelled()?;
        let count:i64=self.connection.query_row("SELECT count(*) FROM semantic_edges WHERE target_id=?1 AND json_extract(data,'$.status')='resolved' AND json_extract(data,'$.kind') IN (SELECT value FROM json_each(?2))",params![target,serde_json::to_string(kinds)?],|row|row.get(0))?;
        Ok(usize::try_from(count)?)
    }

    pub fn semantic_unresolved_count(&self, name: &str, kinds: &[&str]) -> Result<usize> {
        self.check_cancelled()?;
        let count:i64=self.connection.query_row("SELECT count(*) FROM semantic_edges WHERE target_id IS NULL AND json_extract(data,'$.kind') IN (SELECT value FROM json_each(?3)) AND (json_extract(data,'$.target_text')=?1 OR substr(json_extract(data,'$.target_text'),-length(?2))=?2)",params![name,format!(".{name}"),serde_json::to_string(kinds)?],|row|row.get(0))?;
        Ok(usize::try_from(count)?)
    }

    /// Outgoing typed edges of any status from `source`, in stable source order.
    pub fn semantic_outgoing(
        &self,
        source: &str,
        kinds: &[&str],
        limit: u32,
    ) -> Result<Vec<SemanticEdge>> {
        let mut query = self.connection.prepare("SELECT data FROM semantic_edges WHERE source_id=?1 AND json_extract(data,'$.kind') IN (SELECT value FROM json_each(?2)) ORDER BY ordinal LIMIT ?3").context("semantic graph unavailable; re-index this repository")?;
        query
            .query_map(
                params![source, serde_json::to_string(kinds)?, limit],
                |row| row.get::<_, String>(0),
            )?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    /// Non-resolved sites whose expression is `name` or ends in `.name`. These are
    /// textual matches of unconfirmed sites, never claimed bindings.
    pub fn semantic_unresolved_named(
        &self,
        name: &str,
        kinds: &[&str],
        limit: u32,
    ) -> Result<Vec<SemanticEdge>> {
        let mut query = self.connection.prepare("SELECT data FROM semantic_edges WHERE target_id IS NULL AND json_extract(data,'$.kind') IN (SELECT value FROM json_each(?3)) AND (json_extract(data,'$.target_text')=?1 OR substr(json_extract(data,'$.target_text'),-length(?2))=?2) ORDER BY ordinal LIMIT ?4").context("semantic graph unavailable; re-index this repository")?;
        query
            .query_map(
                params![
                    name,
                    format!(".{name}"),
                    serde_json::to_string(kinds)?,
                    limit
                ],
                |row| row.get::<_, String>(0),
            )?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    /// Declarations named exactly `name` or qualified as `Owner.name`; exact names first.
    pub fn facts_named(&self, name: &str, path: Option<&str>, limit: u32) -> Result<Vec<Fact>> {
        let mut query = self.connection.prepare("SELECT data FROM facts WHERE json_extract(data,'$.kind') NOT IN ('file','import','export') AND (?3 IS NULL OR path=?3) AND (json_extract(data,'$.name')=?1 OR substr(json_extract(data,'$.name'),-length(?2))=?2) ORDER BY json_extract(data,'$.name')<>?1,path,json_extract(data,'$.start_byte'),id LIMIT ?4")?;
        query
            .query_map(params![name, format!(".{name}"), path, limit], |row| {
                row.get::<_, String>(0)
            })?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    /// Every declaration (not file/import/export facts), lightweight, in path then source order.
    pub fn declarations(&self) -> Result<Vec<Declaration>> {
        self.check_cancelled()?;
        let mut query = self.connection.prepare("SELECT id,path,json_extract(data,'$.kind'),json_extract(data,'$.name'),json_extract(data,'$.start_byte'),json_extract(data,'$.end_byte'),json_extract(data,'$.start_line'),json_extract(data,'$.end_line') FROM facts WHERE json_extract(data,'$.kind') NOT IN ('file','import','export') ORDER BY path,json_extract(data,'$.start_byte'),id")?;
        query
            .query_map([], |row| {
                Ok(Declaration {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    kind: row.get(2)?,
                    name: row.get(3)?,
                    start_byte: size(row, 4)?,
                    end_byte: size(row, 5)?,
                    start_line: size(row, 6)?,
                    end_line: size(row, 7)?,
                })
            })?
            .map(|row| Ok(row?))
            .collect()
    }

    /// Stream every typed edge as (path, raw JSON) in ordinal order, which
    /// keeps each file's edges together and in source order.
    pub fn scan_edges(&self, visit: &mut dyn FnMut(&str, &str) -> Result<()>) -> Result<()> {
        self.check_cancelled()?;
        let mut query = self
            .connection
            .prepare("SELECT path,data FROM semantic_edges ORDER BY ordinal")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let path: &str = row.get_ref(0)?.as_str()?;
            let data: &str = row.get_ref(1)?.as_str()?;
            visit(path, data)?;
        }
        Ok(())
    }

    /// Indexed files Tree-sitter reported syntax errors for (no semantic edges).
    pub fn syntax_error_files(&self) -> Result<Vec<String>> {
        let mut query = self
            .connection
            .prepare("SELECT path FROM files WHERE syntax_error=1 ORDER BY path")?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Lets another thread abort this connection's running statement on cancellation.
    #[must_use]
    pub fn interrupt_handle(&self) -> Interrupt {
        Interrupt {
            sqlite: self.connection.get_interrupt_handle(),
            cancelled: Arc::clone(&self.cancelled),
        }
    }

    /// Cancellation persists between SQL statements and during filesystem work.
    pub fn check_cancelled(&self) -> Result<()> {
        ensure!(!self.cancelled.load(Ordering::Relaxed), "request cancelled");
        Ok(())
    }

    /// Replace changed files and purge stale facts, FTS rows and edges in one transaction.
    pub fn apply(
        &mut self,
        files: &[IndexedFile],
        retained: &[String],
        report: IndexReport,
        expected_revision: u64,
        resolution: &ResolutionContext,
        resolution_fingerprint: &str,
    ) -> Result<IndexReport> {
        self.apply_with_documents(
            (files, retained),
            None,
            report,
            expected_revision,
            resolution,
            resolution_fingerprint,
        )
    }

    /// Commit code and documentation changes under one optimistic revision lock.
    pub fn apply_with_documents(
        &mut self,
        code: (&[IndexedFile], &[String]),
        documents: Option<DocumentUpdate<'_>>,
        report: IndexReport,
        expected_revision: u64,
        resolution: &ResolutionContext,
        resolution_fingerprint: &str,
    ) -> Result<IndexReport> {
        self.apply_index(
            code,
            documents,
            report,
            expected_revision,
            resolution,
            (resolution_fingerprint, None),
        )
    }

    /// Persist build identity in the same transaction as inventory and revision.
    pub fn apply_index(
        &mut self,
        code: (&[IndexedFile], &[String]),
        documents: Option<DocumentUpdate<'_>>,
        mut report: IndexReport,
        expected_revision: u64,
        resolution: &ResolutionContext,
        metadata: (&str, Option<&str>),
    ) -> Result<IndexReport> {
        let (resolution_fingerprint, build) = metadata;
        let (files, retained) = code;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: String = transaction.query_row(
            "SELECT value FROM metadata WHERE key='revision'",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            current == expected_revision.to_string(),
            "index changed during scan; retry `repogi index <path>`"
        );
        transaction.execute_batch("CREATE TEMP TABLE retained(path TEXT PRIMARY KEY)")?;
        for path in retained {
            transaction.execute("INSERT INTO retained VALUES(?1)", [path])?;
        }
        purge(&transaction, files)?;
        report.files_removed = transaction.execute(
            "DELETE FROM files WHERE path NOT IN (SELECT path FROM retained)",
            [],
        )?;
        for file in files {
            insert_file(&transaction, file)?;
        }
        // Module-resolution settings change bindings without changing sources:
        // rebind every cached module (no reparsing) when their fingerprint moves.
        let resolution_moved = record_resolution(&transaction, resolution_fingerprint)?;
        report.resolution_changed = resolution_moved && expected_revision > 0;
        let rebind = !files.is_empty() || report.files_removed > 0 || resolution_moved;
        // Every file was reparsed by the current extractor when its IR was outdated.
        transaction.execute(
            "INSERT INTO metadata(key,value) VALUES('semantic_ir',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [code_intel_core::semantic::SEMANTIC_IR_VERSION.to_string()],
        )?;
        if rebind {
            semantic::persist(&transaction, resolution)?;
        }
        // Recompute only heuristic file links. No TypeScript symbol resolution is claimed.
        transaction.execute("DELETE FROM edges WHERE kind='module_candidate'", [])?;
        let paths: BTreeMap<String, String> = {
            let mut query =
                transaction.prepare("SELECT path,content_hash FROM files ORDER BY path")?;
            query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let imports: Vec<String> = {
            let mut query = transaction.prepare(
                "SELECT data FROM edges WHERE target_text IS NOT NULL ORDER BY path,source_id",
            )?;
            query
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for data in imports {
            let mut edge: Edge = serde_json::from_str(&data)?;
            if let Some(target) = edge
                .target_text
                .as_deref()
                .and_then(|module| module_candidate(&edge.provenance.source_path, module, &paths))
            {
                edge.target_id = Some(file_id(&target));
                edge.kind = "module_candidate".into();
                edge.provenance.confidence = "path_heuristic".into();
                edge.provenance.extraction_method = "unique_relative_module_candidate".into();
                transaction.execute("INSERT INTO edges(path,source_id,target_id,target_text,kind,data) VALUES(?1,?2,?3,?4,?5,?6)", params![edge.provenance.source_path,edge.source_id,edge.target_id,edge.target_text,edge.kind,serde_json::to_string(&edge)?])?;
            }
        }
        let documents_changed = if let Some(documents) = documents {
            let changed =
                knowledge::apply(&transaction, documents, &mut report, expected_revision + 1)?;
            relationships::resolve(&transaction, rebind || changed, expected_revision + 1)?
                || changed
        } else {
            false
        };
        report.index_revision =
            expected_revision + u64::from(rebind || documents_changed || expected_revision == 0);
        report.syntax_error_files = usize::try_from(transaction.query_row(
            "SELECT count(*) FROM files WHERE syntax_error=1",
            [],
            |row| row.get::<_, i64>(0),
        )?)?;
        transaction.execute(
            "UPDATE metadata SET value=?1 WHERE key='revision'",
            [report.index_revision.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO index_runs(revision,report) VALUES(?1,?2)",
            params![
                i64::try_from(report.index_revision)?,
                serde_json::to_string(&report)?
            ],
        )?;
        transaction.execute_batch("DROP TABLE retained")?;
        if let Some(build) = build {
            transaction.execute("INSERT INTO metadata(key,value) VALUES('indexer_build',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [build])?;
        }
        transaction
            .commit()
            .context("commit index update; previous index is preserved on failure")?;
        Ok(report)
    }

    pub fn status(&self) -> Result<Status> {
        self.check_cancelled()?;
        let count = |sql| -> Result<usize> {
            Ok(usize::try_from(self.connection.query_row(
                sql,
                [],
                |row| row.get::<_, i64>(0),
            )?)?)
        };
        let report: Option<String> = self
            .connection
            .query_row(
                "SELECT report FROM index_runs ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(Status {
            schema_version: SCHEMA_VERSION,
            index_revision: self.revision()?,
            files: count("SELECT count(*) FROM files")?,
            facts: count("SELECT count(*) FROM facts")?,
            edges: count("SELECT count(*) FROM edges")?,
            unresolved_imports: count(
                "SELECT count(*) FROM edges e WHERE e.kind='imports' AND NOT EXISTS(SELECT 1 FROM edges c WHERE c.kind='module_candidate' AND c.source_id=e.source_id)",
            )?,
            syntax_error_files: count("SELECT count(*) FROM files WHERE syntax_error=1")?,
            last_run: report.map(|text| serde_json::from_str(&text)).transpose()?,
        })
    }

    pub fn search(&self, expression: &str, limit: u32) -> Result<Vec<Fact>> {
        if expression.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = self.connection.prepare("SELECT f.data FROM search JOIN facts f ON f.id=search.id WHERE search MATCH ?1 ORDER BY bm25(search,0,5,2,1),f.id LIMIT ?2")?;
        let rows = query.query_map(params![expression, limit], |row| row.get::<_, String>(0))?;
        rows.map(|data| Ok(serde_json::from_str(&data?)?)).collect()
    }

    pub fn fact(&self, id: &str) -> Result<Option<Fact>> {
        self.check_cancelled()?;
        let data: Option<String> = self
            .connection
            .query_row("SELECT data FROM facts WHERE id=?1", [id], |row| row.get(0))
            .optional()?;
        data.map(|text| Ok(serde_json::from_str(&text)?))
            .transpose()
    }

    /// Source-ordered, bounded declarations for lexical dependency candidates.
    pub fn file_facts(&self, path: &str, limit: u32) -> Result<Vec<Fact>> {
        let mut query = self.connection.prepare("SELECT data FROM facts WHERE path=?1 ORDER BY json_extract(data,'$.start_byte'),id LIMIT ?2")?;
        query
            .query_map(params![path, limit], |row| row.get::<_, String>(0))?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    /// Outgoing heuristic module candidates only; never semantic symbol bindings.
    pub fn module_edges(&self, path: &str, limit: u32) -> Result<Vec<Edge>> {
        let mut query = self.connection.prepare("SELECT data FROM edges WHERE path=?1 AND kind='module_candidate' ORDER BY source_id,target_id LIMIT ?2")?;
        query
            .query_map(params![path, limit], |row| row.get::<_, String>(0))?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect()
    }

    /// Undirected one-hop neighborhood, deterministically capped by the caller.
    pub fn neighbors(&self, id: &str, limit: u32) -> Result<Vec<Edge>> {
        let mut query = self.connection.prepare("SELECT data FROM edges WHERE (source_id=?1 OR target_id=?1) AND target_id IS NOT NULL ORDER BY kind,source_id,target_id LIMIT ?2")?;
        let rows = query.query_map(params![id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|data| Ok(serde_json::from_str(&data?)?)).collect()
    }
}

fn store_syntax(transaction: &rusqlite::Transaction<'_>, file: &IndexedFile) -> Result<()> {
    transaction.execute(
        "INSERT INTO semantic_modules(path,data) VALUES(?1,?2)",
        params![file.path, serde_json::to_string(&file.semantic)?],
    )?;
    for fact in &file.facts {
        if matches!(fact.kind.as_str(), "file" | "import" | "export") {
            continue;
        }
        transaction.execute(
            "INSERT INTO semantic_search(id,terms) VALUES(?1,?2)",
            params![fact.id, normalized(&fact.name)],
        )?;
    }
    Ok(())
}

fn normalized(name: &str) -> String {
    let mut result = String::new();
    for character in name.chars() {
        if character.is_uppercase() || !character.is_alphanumeric() {
            result.push(' ');
        }
        result.extend(character.to_lowercase());
    }
    format!("{name} {result}")
}

/// Replace one file's row, facts, FTS rows, heuristic edges and cached syntax IR.
fn insert_file(transaction: &rusqlite::Transaction<'_>, file: &IndexedFile) -> Result<()> {
    transaction.execute("DELETE FROM files WHERE path=?1", [&file.path])?;
    transaction.execute("INSERT INTO files(path,language,content_hash,size_bytes,syntax_error) VALUES(?1,?2,?3,?4,?5)", params![file.path,file.language,file.hash,i64::try_from(file.size_bytes)?,file.syntax_error])?;
    for fact in &file.facts {
        transaction.execute(
            "INSERT INTO facts(id,path,data) VALUES(?1,?2,?3)",
            params![fact.id, file.path, serde_json::to_string(fact)?],
        )?;
        transaction.execute(
            "INSERT INTO search(id,name,path,snippet) VALUES(?1,?2,?3,?4)",
            params![fact.id, fact.name, fact.path, fact.snippet],
        )?;
    }
    for edge in &file.edges {
        transaction.execute("INSERT INTO edges(path,source_id,target_id,target_text,kind,data) VALUES(?1,?2,?3,?4,?5,?6)", params![file.path,edge.source_id,edge.target_id,edge.target_text,edge.kind,serde_json::to_string(edge)?])?;
    }
    store_syntax(transaction, file)
}

/// Set-based removal of everything that belongs to changed or removed files,
/// before their `files` rows go. Row-by-row cascades and the per-fact FTS delete
/// triggers scan whole tables once per file, which made bulk changes (many
/// files at once, an IR upgrade, a branch switch) quadratic. The triggers are
/// suspended only for this statement set and recreated from their stored SQL in
/// the same transaction, so the committed schema is unchanged.
fn purge(transaction: &rusqlite::Transaction<'_>, files: &[IndexedFile]) -> Result<()> {
    transaction.execute_batch(
        "CREATE TEMP TABLE doomed(path TEXT PRIMARY KEY);
         INSERT INTO doomed SELECT path FROM files WHERE path NOT IN (SELECT path FROM retained);",
    )?;
    for file in files {
        transaction.execute("INSERT OR IGNORE INTO doomed VALUES(?1)", [&file.path])?;
    }
    let doomed: i64 = transaction.query_row("SELECT count(*) FROM doomed", [], |row| row.get(0))?;
    if doomed > 0 {
        // Any changed or removed file rebinds the whole graph in this transaction.
        transaction.execute("DELETE FROM semantic_edges", [])?;
        let triggers: Vec<(String, String)> = {
            let mut query = transaction.prepare(
                "SELECT name, sql FROM sqlite_master WHERE type='trigger' AND tbl_name='facts' ORDER BY name",
            )?;
            query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (name, _) in &triggers {
            transaction.execute_batch(&format!("DROP TRIGGER \"{name}\""))?;
        }
        transaction.execute_batch(
            "DELETE FROM search WHERE id IN (SELECT id FROM facts WHERE path IN (SELECT path FROM doomed));
             DELETE FROM semantic_search WHERE id IN (SELECT id FROM facts WHERE path IN (SELECT path FROM doomed));
             DELETE FROM edges WHERE path IN (SELECT path FROM doomed);
             DELETE FROM facts WHERE path IN (SELECT path FROM doomed);",
        )?;
        for (_, sql) in &triggers {
            transaction.execute_batch(sql)?;
        }
    }
    transaction.execute_batch("DROP TABLE doomed")?;
    Ok(())
}

/// Store the module-resolution fingerprint; true when it differs from the indexed one.
fn record_resolution(transaction: &rusqlite::Transaction<'_>, fingerprint: &str) -> Result<bool> {
    let indexed: Option<String> = transaction
        .query_row(
            "SELECT value FROM metadata WHERE key='module_resolution'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    transaction.execute(
        "INSERT INTO metadata(key,value) VALUES('module_resolution',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [fingerprint],
    )?;
    Ok(indexed.as_deref() != Some(fingerprint))
}

fn configure(connection: &Connection) -> Result<()> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn module_candidate(
    source: &str,
    module: &str,
    paths: &BTreeMap<String, String>,
) -> Option<String> {
    if module.contains('\\') || (!module.starts_with("./") && !module.starts_with("../")) {
        return None;
    }
    let mut components: Vec<&str> = source.split('/').collect();
    components.pop();
    for part in module.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                components.pop()?;
            }
            _ => components.push(part),
        }
    }
    let base = components.join("/");
    let candidates = if matches!(
        Path::new(&base)
            .extension()
            .and_then(|value| value.to_str()),
        Some("ts" | "tsx")
    ) {
        vec![base]
    } else if let Some(stem) = base
        .strip_suffix(".js")
        .or_else(|| base.strip_suffix(".jsx"))
    {
        vec![format!("{stem}.ts"), format!("{stem}.tsx")]
    } else {
        vec![
            format!("{base}.ts"),
            format!("{base}.tsx"),
            format!("{base}/index.ts"),
            format!("{base}/index.tsx"),
        ]
    };
    let mut found = candidates
        .into_iter()
        .filter(|candidate| paths.contains_key(candidate));
    let result = found.next()?;
    if found.next().is_some() {
        return None;
    }
    Some(result)
}

/// A non-negative SQLite integer as `usize`.
fn size(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<usize> {
    let value: i64 = row.get(index)?;
    usize::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

/// A transactionally consistent copy of an index database (`VACUUM INTO`),
/// used to seed another snapshot. The source is opened read-only.
pub fn copy_database(source: &Path, target: &Path) -> Result<()> {
    ensure!(!target.exists(), "copy target {} exists", target.display());
    let connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("open index {}", source.display()))?;
    configure(&connection)?;
    let target = target.to_str().context("index path must be valid UTF-8")?;
    connection
        .execute("VACUUM INTO ?1", [target])
        .context("copy index database")?;
    Ok(())
}

/// Reject state paths redirected through symlinks, including an existing database.
pub fn validate_state_path(root: &Path) -> Result<()> {
    for path in [
        root.join(".code-intel"),
        root.join(".code-intel/index.db"),
        root.join(".code-intel/index.db-journal"),
        root.join(".code-intel/index.db-wal"),
        root.join(".code-intel/index.db-shm"),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => bail!(
                "refusing symlinked index state {}; use a local .code-intel directory",
                path.display()
            ),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect index state {}", path.display()));
            }
        }
    }
    Ok(())
}
