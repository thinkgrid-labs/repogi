//! SQLite persistence, versioned schema, FTS5, and bounded graph reads.
use std::{collections::BTreeMap, path::Path, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use code_intel_core::{Edge, Fact, IndexReport, IndexedFile, SCHEMA_VERSION, Status, file_id};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

const MIGRATION_1: &str = include_str!("migration_1.sql");

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open_index(database: &Path, root: &Path) -> Result<Self> {
        let mut connection = Connection::open(database).with_context(|| {
            format!(
                "open index {}; check directory permissions",
                database.display()
            )
        })?;
        configure(&connection)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version <= SCHEMA_VERSION,
            "index schema {version} is newer than supported schema {SCHEMA_VERSION}; use a compatible code-intel binary"
        );
        if version == 0 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(MIGRATION_1)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            transaction.commit()?;
        }
        let stored_root: Option<String> = connection
            .query_row("SELECT value FROM metadata WHERE key='root'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let root_text = root
            .to_str()
            .context("repository root must be valid UTF-8")?;
        if let Some(stored_root) = stored_root {
            ensure!(
                stored_root == root_text,
                "index belongs to a different repository root; move .code-intel aside and re-index"
            );
        } else {
            connection.execute(
                "INSERT INTO metadata(key,value) VALUES('root',?1),('revision','0')",
                [root_text],
            )?;
        }
        Ok(Self { connection })
    }

    /// A read transaction keeps revision, FTS, and graph results in one snapshot.
    pub fn open_read(database: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| {
                format!(
                    "open index {}; run `code-intel index <path>` first",
                    database.display()
                )
            })?;
        configure(&connection)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version == SCHEMA_VERSION,
            "unsupported index schema {version}; move .code-intel aside and re-index"
        );
        connection.execute_batch("BEGIN DEFERRED")?;
        Ok(Self { connection })
    }

    pub fn revision(&self) -> Result<u64> {
        let text: String = self.connection.query_row(
            "SELECT value FROM metadata WHERE key='revision'",
            [],
            |row| row.get(0),
        )?;
        text.parse()
            .context("invalid index revision; rebuild the index")
    }

    pub fn hashes(&self) -> Result<BTreeMap<String, String>> {
        let mut query = self
            .connection
            .prepare("SELECT path,content_hash FROM files ORDER BY path")?;
        Ok(query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Replace changed files and purge stale facts, FTS rows and edges in one transaction.
    pub fn apply(
        &mut self,
        files: &[IndexedFile],
        retained: &[String],
        mut report: IndexReport,
        expected_revision: u64,
    ) -> Result<IndexReport> {
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
            "index changed during scan; retry `code-intel index <path>`"
        );
        transaction.execute_batch("CREATE TEMP TABLE retained(path TEXT PRIMARY KEY)")?;
        for path in retained {
            transaction.execute("INSERT INTO retained VALUES(?1)", [path])?;
        }
        report.files_removed = transaction.execute(
            "DELETE FROM files WHERE path NOT IN (SELECT path FROM retained)",
            [],
        )?;
        for file in files {
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
        report.index_revision = expected_revision
            + u64::from(!files.is_empty() || report.files_removed > 0 || expected_revision == 0);
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
        transaction
            .commit()
            .context("commit index update; previous index is preserved on failure")?;
        Ok(report)
    }

    pub fn status(&self) -> Result<Status> {
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
        let data: Option<String> = self
            .connection
            .query_row("SELECT data FROM facts WHERE id=?1", [id], |row| row.get(0))
            .optional()?;
        data.map(|text| Ok(serde_json::from_str(&text)?))
            .transpose()
    }

    /// Undirected one-hop neighborhood, deterministically capped by the caller.
    pub fn neighbors(&self, id: &str, limit: u32) -> Result<Vec<Edge>> {
        let mut query = self.connection.prepare("SELECT data FROM edges WHERE (source_id=?1 OR target_id=?1) AND target_id IS NOT NULL ORDER BY kind,source_id,target_id LIMIT ?2")?;
        let rows = query.query_map(params![id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|data| Ok(serde_json::from_str(&data?)?)).collect()
    }
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
