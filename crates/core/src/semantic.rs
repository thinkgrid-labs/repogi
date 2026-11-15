//! Evidence-backed static bindings. Separate from the frozen heuristic graph.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    /// Set on instance-member edges (`this.m`, `super.m`): `static_declaration`
    /// means the target is the declaration the class hierarchy statically
    /// selects, not a proof of the runtime callee. Absent on every other edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<String>,
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
/// 3: class member context for `this`/`super`, `CommonJS` `require` bindings and
/// export tables, member-level (instead of whole-binding) write invalidation.
pub const SEMANTIC_IR_VERSION: u32 = 3;

/// Cached syntax IR, not claims of resolved bindings. Scope ranges are half-open bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Module {
    pub complete: bool,
    pub bindings: Vec<Binding>,
    pub exports: Vec<Export>,
    pub uses: Vec<Usage>,
    pub symbols: Vec<Symbol>,
    /// Top-level classes with facts, and what may shadow their methods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<ClassInfo>,
    /// Writes through a member (`x.m = v`, `x[k] = v`, `x.m++`), outside
    /// `this.m` writes inside classes (recorded on [`ClassInfo`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_writes: Vec<MemberWrite>,
    /// Identifiers whose `.prototype` is used anywhere (read or write).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prototype_uses: Vec<NameAt>,
    /// `__proto__`, `setPrototypeOf` or `.prototype` on a non-identifier: the
    /// prototype chains of this module's classes cannot be trusted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prototype_opaque: bool,
    /// `import`/`export` statements are present (ES module syntax).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub esm_syntax: bool,
    /// Static `CommonJS` exports, when the file writes `module.exports`/`exports`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commonjs: Option<CommonJs>,
}

/// A top-level class declaration (by fact ID) and its own property names.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClassInfo {
    pub id: String,
    /// Instance fields (`m = …`, `m;`) and `this.m` writes anywhere in the body:
    /// own properties that shadow prototype methods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instance_fields: Vec<String>,
    /// Static fields: own properties of the constructor.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub static_fields: Vec<String>,
    /// `this[k] = …`, `Object.assign(this, …)`, `Object.defineProperty(this, …)`
    /// or similar: any member name may be an own property.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dynamic_members: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberWrite {
    /// The identifier the written member hangs off (`x` in `x.a.b = v`);
    /// `None` for any other receiver (calls, `this.a.b`, literals).
    pub receiver: Option<String>,
    /// The written property name; `None` when computed.
    pub property: Option<String>,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameAt {
    pub name: String,
    pub offset: usize,
}

/// The lexical class context of a `this.m`/`super.m` use: the nearest
/// non-arrow function is a method of this class.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemberContext {
    /// Class fact ID.
    pub class: String,
    /// The method is `static`: `this` is the constructor, `super` its parent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub static_context: bool,
}

/// Static `CommonJS` exports: only unconditional top-level assignments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommonJs {
    pub exports: Vec<CommonJsExport>,
    /// Why the table cannot be trusted (conditional, computed or aliased
    /// writes, `Object.defineProperty`, shadowed `module`/`exports`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommonJsExport {
    /// The export name; `""` is the `module.exports` value itself.
    pub name: String,
    /// A local binding holding the value, when it is a plain identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// `require("spec")` (optionally `.member`) holding the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub location: Location,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // Serialized IR shape; flags are independent.
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
    /// Members written through this binding (`x.m = v`: `m`; `x[k] = v`: `*`).
    /// They invalidate member uses (`x.m()`), not the binding itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutated_members: Vec<String>,
    /// A `require("…")` binding: `module` is the literal specifier and
    /// `imported` is `*` (the whole `module.exports`) or a property name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub commonjs: bool,
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
    /// For `this.m`/`super.m`: the enclosing class method, when supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MemberContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub static_member: bool,
    pub callable: bool,
    /// A body-less member (abstract method, overload signature): never a
    /// runtime target of instance dispatch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub signature_only: bool,
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
    /// `package.json` `type` per directory that has one: `module`, `commonjs`
    /// (also when the field is absent) or `invalid` (unreadable or not a string).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub package_types: BTreeMap<String, String>,
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
