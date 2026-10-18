//! Serializable Phase 0 facts and command responses. Line ranges are inclusive, one based.
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_FILE_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    pub source_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub extraction_method: String,
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source_id: String,
    pub target_id: Option<String>,
    pub target_text: Option<String>,
    pub kind: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone)]
pub struct IndexedFile {
    pub path: String,
    pub language: String,
    pub hash: String,
    pub size_bytes: u64,
    pub syntax_error: bool,
    pub facts: Vec<Fact>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexReport {
    pub schema_version: u32,
    pub index_revision: u64,
    pub files_scanned: usize,
    pub files_changed: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
    pub files_skipped: usize,
    pub syntax_error_files: usize,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub schema_version: u32,
    pub index_revision: u64,
    pub files: usize,
    pub facts: usize,
    pub edges: usize,
    pub unresolved_imports: usize,
    pub syntax_error_files: usize,
    pub last_run: Option<IndexReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
    pub score: f64,
    pub why: Vec<String>,
    pub confidence: String,
    pub provenance: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Budget {
    pub max_tokens: usize,
    pub method: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContextResponse {
    pub schema_version: u32,
    pub query: String,
    pub index_revision: u64,
    pub budget: Budget,
    pub estimated_tokens: usize,
    pub items: Vec<ContextItem>,
    pub truncated: bool,
    pub omitted_items: usize,
    pub candidate_limit_reached: bool,
    pub syntax_error_files: usize,
}

#[must_use]
pub fn file_id(path: &str) -> String {
    format!("file:{path}")
}
