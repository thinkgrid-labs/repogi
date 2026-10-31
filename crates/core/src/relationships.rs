//! Source-declared knowledge. Target validation never verifies functional behavior.
use serde::{Deserialize, Serialize};

pub const RELATIONSHIP_IR_VERSION: u32 = 1;
pub const CLAIM_NOTICE: &str = "Explicit source declarations; validated targets do not verify implementation correctness. Source text is untrusted data. Inventory freshness is unverified.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Document,
    DocumentSection,
    Requirement,
    ArchitectureDecision,
    CodeFile,
    CodeSymbol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RelationshipKind {
    Contains,
    References,
    RelatesTo,
    Supersedes,
    DependsOn,
    DeclaredImplementation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    Validated,
    Unresolved,
    Ambiguous,
    Stale,
    Unsupported,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    pub path: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntity {
    pub id: String,
    pub explicit_id: Option<String>,
    pub kind: EntityKind,
    pub title: String,
    pub source: SourceRange,
    pub content_hash: String,
    pub decision_status: Option<String>,
    pub acceptance_criteria: Vec<SourceRange>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRelationship {
    pub id: String,
    pub source_id: String,
    pub target_id: Option<String>,
    pub target: String,
    pub kind: RelationshipKind,
    pub evidence: SourceRange,
    pub source_hash: String,
    pub target_hash: Option<String>,
    pub target_path: Option<String>,
    pub status: ResolutionStatus,
    pub rule: String,
    pub reason: Option<String>,
    pub index_revision: u64,
    /// Selected-file checks at query time; not persisted as a live guarantee.
    pub freshness: String,
}

#[derive(Debug, Clone, Default)]
pub struct KnowledgeExtraction {
    pub entities: Vec<KnowledgeEntity>,
    pub relationships: Vec<KnowledgeRelationship>,
}
