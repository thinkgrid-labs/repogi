//! Evidence-backed static bindings. Separate from the frozen heuristic graph.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Relationship {
    Imports,
    Exports,
    ReExports,
    References,
    Calls,
    Extends,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Resolved,
    Unresolved,
    Ambiguous,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    pub syntax: String,
    pub rule: String,
    pub binding_chain: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticEdge {
    pub source_id: String,
    pub target_id: Option<String>,
    pub target_text: String,
    pub kind: Relationship,
    pub status: Resolution,
    pub location: Location,
    pub evidence: Evidence,
}

/// Version of the cached per-file syntax IR. Bump whenever the extractor records
/// something new, so files whose cached IR predates it are reparsed once
/// instead of silently missing the new facts. 2: JSX component references, and
/// object-pattern default values read as expressions instead of bindings.
pub const SEMANTIC_IR_VERSION: u32 = 2;

/// Cached syntax IR, not claims of resolved bindings. Scope ranges are half-open bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Module {
    pub complete: bool,
    pub bindings: Vec<Binding>,
    pub exports: Vec<Export>,
    pub uses: Vec<Usage>,
    pub symbols: Vec<Symbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub name: String,
    pub target: Option<String>,
    pub module: Option<String>,
    pub imported: String,
    pub scope_start: usize,
    pub scope_end: usize,
    pub top_level: bool,
    pub mutated: bool,
    pub type_only: bool,
    pub location: Location,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Export {
    pub type_only: bool,
    pub name: String,
    pub local: String,
    pub module: Option<String>,
    pub location: Location,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub syntax: String,
    pub source_id: String,
    pub name: String,
    pub member: Option<String>,
    pub kind: Relationship,
    pub location: Location,
    /// A JSX element's component name (`<Name>`/`<Ns.Name>`): a REFERENCES use,
    /// never a confirmed call.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub jsx: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub static_member: bool,
    pub callable: bool,
}

/// Project module-resolution settings, read from `tsconfig.json`/`jsconfig.json`
/// files inside the repository. Paths are repository-relative with `/`
/// separators; `""` is the repository root. Only the binder consumes this; it is
/// fingerprinted so a configuration change rebinds without reparsing sources.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolutionContext {
    /// One per directory that holds a config (tsconfig.json wins over jsconfig.json).
    pub projects: Vec<Project>,
    /// Non-indexed files a TypeScript resolver could select before an indexed one
    /// (JavaScript, `.mts`/`.cts`/`.d.mts`/`.d.cts`, `package.json`).
    pub other_files: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    /// The config file that governs sources in `dir` and below.
    pub config: String,
    pub dir: String,
    /// Effective `baseUrl`, resolved against the config that declared it.
    pub base_url: Option<String>,
    /// `baseUrl` points outside the repository: bare specifiers are unsupported
    /// because the compiler would look there first.
    pub base_url_outside_root: bool,
    /// Effective `paths`, in declaration order.
    pub paths: Vec<PathMapping>,
    /// Set when the project's resolution settings cannot be trusted (invalid
    /// JSONC, missing/cyclic/package `extends`, unsupported options). Then no
    /// non-relative specifier in the project resolves.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathMapping {
    pub pattern: String,
    /// Substitutions in order, each with at most one `*`, resolved against the
    /// paths base. `None` marks a substitution outside the repository root.
    pub substitutions: Vec<Option<String>>,
    /// The config file that declared this `paths` entry (evidence only).
    pub declared_in: String,
}
