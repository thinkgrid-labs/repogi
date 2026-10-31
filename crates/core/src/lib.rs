//! Serializable Phase 0 facts and command responses. Line ranges are inclusive, one based.
use serde::{Deserialize, Serialize};

pub mod knowledge;
pub mod memory;
pub mod relationships;
pub mod semantic;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    pub snippet: String,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source_id: String,
    pub target_id: Option<String>,
    pub target_text: Option<String>,
    pub kind: String,
    pub provenance: Provenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<semantic::Resolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<semantic::Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<semantic::Location>,
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
    pub semantic: semantic::Module,
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
    /// The module-resolution settings (tsconfig/jsconfig) differed from the
    /// indexed ones, so every cached module was rebound without reparsing.
    #[serde(default)]
    pub resolution_changed: bool,
    /// Markdown counters are separate from the existing code inventory.
    #[serde(default)]
    pub documents: knowledge::DocumentReport,
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

/// Read-only comparison of eligible source files against an indexed snapshot.
/// Paths are sorted; lists are capped by the caller-supplied sample limit.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Freshness {
    pub fresh: bool,
    pub files_checked: usize,
    pub changed: usize,
    pub added: usize,
    pub removed: usize,
    pub skipped: usize,
    pub sample: Vec<String>,
    /// Module-resolution settings differ from the indexed ones (graph is stale
    /// even when every source file is unchanged).
    #[serde(default)]
    pub resolution_changed: bool,
    /// The cached syntax IR predates the current extractor (re-index reparses it).
    #[serde(default)]
    pub semantic_ir_outdated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    /// Explicit document-excerpt completeness; omitted for code compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_byte: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_byte: Option<usize>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub max_tokens: usize,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    /// Present only when explicitly retrieving document intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_skipped_files: Option<usize>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<Edge>,
    /// Additive agent-facing evidence contract. Frozen ranking controls may
    /// omit it; CLI/MCP navigation adapters attach it before final budgeting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation: Option<NavigationEvidence>,
}

/// Point-in-time checks of returned source bytes, never whole-graph freshness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavigationEvidence {
    pub source: String,
    pub current_source: String,
    pub checked_files: usize,
    pub unchecked_files: usize,
    pub changed_files: Vec<String>,
    pub graph_freshness: String,
    pub supported_extensions: Vec<String>,
}

#[must_use]
pub fn file_id(path: &str) -> String {
    format!("file:{path}")
}
