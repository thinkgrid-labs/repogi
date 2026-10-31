//! The request and response contract (`schema_version` 1). Field order is the
//! JSON order; every type here is serialized as part of the public output.
use code_intel_core::relationships::SourceRange;
use serde::Serialize;

use super::{DEFAULT_BYTES, DEFAULT_DEPTH, DEFAULT_STEPS};
use crate::navigate::Direction;

/// How steps compete for `max_steps`. `Balanced` keeps one step per evidence
/// category (in plan order) before filling; `Tiered` ranks by evidence strength only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ranking {
    #[default]
    Balanced,
    Tiered,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub task: String,
    /// Explicit entity IDs, author IDs (REQ-/ADR-) or exact fact IDs.
    pub selectors: Vec<String>,
    pub depth: usize,
    pub max_steps: usize,
    pub max_bytes: usize,
    pub ranking: Ranking,
    /// Opt-in: add approved project memories as separately classed evidence.
    pub include_memory: bool,
}

impl Request {
    #[must_use]
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            selectors: Vec::new(),
            depth: DEFAULT_DEPTH,
            max_steps: DEFAULT_STEPS,
            max_bytes: DEFAULT_BYTES,
            ranking: Ranking::Balanced,
            include_memory: false,
        }
    }
}

/// Evidence classes are reported separately; they are never folded into one score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// The caller named this exact indexed identity.
    ExplicitSelector,
    /// A resolved typed edge from the persisted TypeScript graph.
    VerifiedStaticRelationship,
    /// An explicit document claim whose target identity validated in the index.
    ValidatedKnowledgeReference,
    /// An author-declared requirement or decision; intent, not implementation.
    RecordedIntent,
    /// A developer-approved project memory: review status, not runtime proof.
    ApprovedProjectMemory,
    /// A resolved static consumer in a file matching test conventions.
    AdvisoryTestCandidate,
    /// FTS5 discovery only; no relationship supports it.
    LexicalCandidate,
    /// Unresolved, ambiguous, invalid or unsupported references and unconfirmed sites.
    UnresolvedReference,
    /// A claim whose indexed target was deleted or changed.
    StaleReference,
}

impl EvidenceClass {
    pub(super) const fn tier(self) -> u8 {
        match self {
            Self::ExplicitSelector => 1,
            Self::VerifiedStaticRelationship => 2,
            Self::ValidatedKnowledgeReference => 3,
            Self::RecordedIntent | Self::ApprovedProjectMemory => 4,
            Self::AdvisoryTestCandidate => 5,
            Self::LexicalCandidate => 6,
            Self::UnresolvedReference | Self::StaleReference => 7,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Relation {
    /// Knowledge relationship ID; static graph edges have no persisted ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: String,
    pub source_id: String,
    pub target_id: Option<String>,
    pub target_text: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    /// Content-derived: identical evidence keeps its ID across runs and budgets.
    pub id: String,
    pub class: EvidenceClass,
    /// The entity or fact this evidence is about.
    pub subject: String,
    pub rule: String,
    pub citation: SourceRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship: Option<Relation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    pub freshness: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Implementation,
    Requirement,
    Decision,
    Document,
    Consumer,
    Dependency,
    Test,
    Memory,
}

#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub id: String,
    pub kind: String,
    pub role: Role,
    /// Identifier or heading text: untrusted data, capped and control-free.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_status: Option<String>,
    pub location: SourceRange,
    pub freshness: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    InspectImplementation,
    ReviewRequirement,
    ReviewDecision,
    ReviewDocument,
    InspectConsumers,
    InspectDependencies,
    ReviewTestCandidates,
    ResolveUncertainty,
    ReviewLexicalCandidates,
    ReviewMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Selected,
    Implementation,
    Knowledge,
    Memory,
    Consumers,
    Dependencies,
    Tests,
    Uncertainty,
    Lexical,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Uncertainty {
    pub code: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

/// A bounded follow-up built only from indexed identities and locations.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Next {
    pub tool: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<&'static str>,
}

impl Next {
    pub(super) const fn new(tool: &'static str) -> Self {
        Self {
            tool,
            path: None,
            start_line: None,
            end_line: None,
            id: None,
            selector: None,
            direction: None,
            depth: None,
            operation: None,
        }
    }
    pub(super) fn read(location: &SourceRange) -> Self {
        Self {
            path: Some(location.path.clone()),
            start_line: Some(location.start_line),
            end_line: Some(location.end_line),
            ..Self::new("read")
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Priority {
    pub tier: u8,
    pub rank: usize,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub id: String,
    pub order: usize,
    pub action: Action,
    pub category: Category,
    pub objective: String,
    pub targets: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted_targets: usize,
    pub evidence: Vec<String>,
    pub uncertainty: Vec<Uncertainty>,
    pub next: Vec<Next>,
    pub priority: Priority,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip predicate signature
const fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub original: String,
    /// `supported`, `ambiguous_selector`, `no_matching_evidence` or `unsupported`.
    pub classification: &'static str,
    /// Deterministic keyword cues that reorder plan phases; not semantic intent.
    pub cues: Vec<&'static str>,
    pub terms: Vec<String>,
    pub search_terms: Vec<String>,
    pub selectors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Freshness {
    pub evidence_source: &'static str,
    pub selected_sources: &'static str,
    pub checked_files: usize,
    pub unchecked_files: usize,
    pub stale_files: Vec<String>,
    pub missing_files: Vec<String>,
    pub graph: &'static str,
    pub inventory: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Budget {
    pub max_bytes: usize,
    pub used_bytes: usize,
    pub method: &'static str,
    pub max_steps: usize,
    pub depth: usize,
    pub max_evidence: usize,
    pub max_graph_nodes: usize,
    pub max_graph_edges: usize,
    pub max_checked_files: usize,
    pub work_limit: usize,
    pub work_used: usize,
    /// Present only when project memory was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_memories: Option<usize>,
}

// Independent completeness axes, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize)]
pub struct Completeness {
    /// Traversal, claim enumeration and step/byte fitting finished within the
    /// requested limits. Lexical discovery is never exhaustive and unresolved
    /// findings are reported separately; neither is implied by this flag.
    pub complete_within_limits: bool,
    pub byte_truncated: bool,
    pub step_limit_reached: bool,
    pub omitted_steps: usize,
    pub work_limit_reached: bool,
    pub candidate_limit_reached: bool,
    /// An unvisited incident edge was observed past `depth`, or frontier checks were capped.
    pub depth_frontier: bool,
    pub unresolved_findings: usize,
    pub omitted_categories: Vec<Category>,
    pub omitted_lexical_candidates: usize,
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub schema_version: u32,
    pub kind: &'static str,
    pub repository_root: Option<String>,
    pub index_revision: u64,
    pub notice: &'static str,
    pub task: Task,
    pub ranking: Ranking,
    pub steps: Vec<Step>,
    pub targets: Vec<Target>,
    pub evidence: Vec<Evidence>,
    pub uncertainty: Vec<Uncertainty>,
    pub freshness: Freshness,
    pub budget: Budget,
    pub completeness: Completeness,
}
