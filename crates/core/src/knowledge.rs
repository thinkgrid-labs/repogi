//! Repository-scoped document evidence. Ranges are inclusive, one based.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentReport {
    pub scanned: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSection {
    /// Identity is scoped to the database's recorded repository root.
    pub id: String,
    pub heading: String,
    pub hierarchy: Vec<String>,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub id: String,
    pub path: String,
    pub hash: String,
    pub content: String,
    pub sections: Vec<DocumentSection>,
}

/// A section hit, with its original snapshot and selected-source freshness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentHit {
    pub section: DocumentSection,
    pub path: String,
    pub content_hash: String,
    pub index_revision: u64,
    /// SQLite BM25; lower values rank first, ties use path and source order.
    pub relevance: f64,
    pub source_end_line: usize,
    pub freshness: String,
    pub truncated: bool,
}
