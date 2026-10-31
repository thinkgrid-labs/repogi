//! Repository-scoped document evidence. Ranges are inclusive, one based.
use serde::{Deserialize, Serialize};

/// Extraction semantics changed to preserve standalone-CR source locations.
pub const DOCUMENT_IR_VERSION: u32 = 2;

/// Original Markdown line starts, preserving LF, CRLF and lone CR bytes.
#[must_use]
pub fn line_starts(source: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let mut starts = vec![0];
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' || (*byte == b'\r' && bytes.get(index + 1) != Some(&b'\n')) {
            starts.push(index + 1);
        }
    }
    starts
}

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
    pub knowledge: crate::relationships::KnowledgeExtraction,
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
    /// All query terms precede partial matches; BM25 orders each tier.
    #[serde(default)]
    pub match_mode: String,
    pub source_end_line: usize,
    #[serde(default)]
    pub source_start_line: usize,
    #[serde(default)]
    pub source_start_byte: usize,
    #[serde(default)]
    pub source_end_byte: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_locations_limited: Option<bool>,
    /// FTS5 token matches relative to the original section; never persisted.
    #[serde(skip)]
    pub matches: Vec<std::ops::Range<usize>>,
    pub freshness: String,
    pub truncated: bool,
}
