use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use code_intel_core::{
    IndexReport,
    knowledge::{Document, DocumentHit},
};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::Store;

#[derive(Clone, Copy)]
pub struct DocumentUpdate<'a> {
    pub changed: &'a [Document],
    pub retained: &'a [String],
}

impl Store {
    /// Skips in the last explicit project index make document coverage partial.
    pub fn document_skipped(&self) -> Result<usize> {
        self.check_cancelled()?;
        let skipped: i64 = self.connection.query_row(
            "SELECT coalesce(json_extract(report,'$.documents.skipped'),0) FROM index_runs ORDER BY id DESC LIMIT 1",
            [], |row| row.get(0),
        ).optional()?.unwrap_or(0);
        Ok(usize::try_from(skipped)?)
    }
    /// Old read-only indexes have no knowledge layer; this never migrates them.
    pub fn knowledge_available(&self) -> Result<bool> {
        self.check_cancelled()?;
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key='knowledge_indexed'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some())
    }

    pub fn document_hashes(&self) -> Result<BTreeMap<String, String>> {
        self.check_cancelled()?;
        let mut query = self
            .connection
            .prepare("SELECT path,content_hash FROM documents ORDER BY path")?;
        Ok(query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn document_hash(&self, path: &str) -> Result<Option<String>> {
        self.check_cancelled()?;
        Ok(self
            .connection
            .query_row(
                "SELECT content_hash FROM documents WHERE path=?1",
                [path],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn search_documents(&self, expression: &str, limit: u32) -> Result<Vec<DocumentHit>> {
        self.check_cancelled()?;
        ensure!(
            self.knowledge_available()?,
            "documentation index unavailable; run `repogi index <path>` first"
        );
        ensure!(limit <= 1001, "documentation candidate limit exceeds 1001");
        let mut query = self.connection.prepare(
            "SELECT s.data,d.path,d.content_hash,d.index_revision,bm25(document_search,0.0,1.0,4.0,1.0) FROM document_search f JOIN document_sections s ON s.id=f.id JOIN documents d ON d.path=s.path WHERE document_search MATCH ?1 ORDER BY bm25(document_search,0.0,1.0,4.0,1.0),d.path,s.start_line,s.id LIMIT ?2"
        ).context("documentation index unavailable; run `repogi index <path>` first")?;
        query
            .query_map(params![expression, limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })?
            .map(|row| {
                let (data, path, content_hash, index_revision, relevance) = row?;
                let section: code_intel_core::knowledge::DocumentSection =
                    serde_json::from_str(&data)?;
                Ok(DocumentHit {
                    source_end_line: section.end_line,
                    section,
                    path,
                    content_hash,
                    index_revision: u64::try_from(index_revision)?,
                    relevance,
                    freshness: "unverified".into(),
                    truncated: false,
                })
            })
            .collect()
    }
}

pub(super) fn apply(
    transaction: &Transaction<'_>,
    update: DocumentUpdate<'_>,
    report: &mut IndexReport,
    revision: u64,
) -> Result<bool> {
    transaction.execute_batch("CREATE TEMP TABLE retained_documents(path TEXT PRIMARY KEY)")?;
    for path in update.retained {
        transaction.execute("INSERT INTO retained_documents VALUES(?1)", [path])?;
    }
    report.documents.removed = transaction.execute(
        "DELETE FROM documents WHERE path NOT IN (SELECT path FROM retained_documents)",
        [],
    )?;
    for document in update.changed {
        transaction.execute("DELETE FROM documents WHERE path=?1", [&document.path])?;
        transaction.execute("INSERT INTO documents(path,id,content_hash,content,index_revision) VALUES(?1,?2,?3,?4,?5)", params![document.path,document.id,document.hash,document.content,i64::try_from(revision)?])?;
        for section in &document.sections {
            transaction.execute("INSERT INTO document_sections(id,path,start_line,end_line,data) VALUES(?1,?2,?3,?4,?5)", params![section.id,document.path,i64::try_from(section.start_line)?,i64::try_from(section.end_line)?,serde_json::to_string(section)?])?;
            transaction.execute(
                "INSERT INTO document_search(id,path,heading,content) VALUES(?1,?2,?3,?4)",
                params![
                    section.id,
                    document.path,
                    section.hierarchy.join(" / "),
                    section.content
                ],
            )?;
        }
    }
    let initialized = transaction.execute(
        "INSERT OR IGNORE INTO metadata(key,value) VALUES('knowledge_indexed','1')",
        [],
    )? > 0;
    transaction.execute_batch("DROP TABLE retained_documents")?;
    Ok(initialized || !update.changed.is_empty() || report.documents.removed > 0)
}
