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
/// 4 (`JavaScript` only): callable facts for function-valued assignments,
/// object-literal members and class expressions; object and prototype tables;
/// alias and `new` origins of bindings; chained `exports = module.exports`.
/// 5: `new C()` origins in TypeScript too (also `new ns.C()`), receiver escape
/// reasons of constructed and aliased bindings, constructors that return a
/// value, statically identified symbol keys (`this[Symbol.iterator]`,
/// `this[util.inspect.custom]`) and canonical integer keys (`x[0]`).
pub const SEMANTIC_IR_VERSION: u32 = 5;

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
    /// Top-level bindings initialized with an object literal (`JavaScript`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectTable>,
    /// `F.prototype.m = …` tables of top-level function declarations (`JavaScript`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prototypes: Vec<ObjectTable>,
}

/// The statically known properties of one object: an object literal held by a
/// top-level binding, or the prototype of a top-level function declaration.
/// Only unconditional top-level statements contribute.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObjectTable {
    /// The binding (object) or function name (prototype).
    pub name: String,
    /// Fact ID of the declaration that owns the table: the variable declarator
    /// of the object, or the function declaration of the prototype.
    pub owner: String,
    pub properties: Vec<ObjectProperty>,
    /// Byte offsets of the statements' receivers (`X` in `X.m = …`,
    /// `F` in `F.prototype.m = …`) that built the table: these writes are
    /// the table itself, not later mutations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statements: Vec<usize>,
    /// Why the table cannot be trusted (escape, computed or nested writes,
    /// prototype replacement); then no property resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectProperty {
    pub name: String,
    /// The callable or class fact the value is (inline function, method, class expression).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<String>,
    /// A local identifier holding the value (`{ validate }`, `X.m = validate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// `local.member` (`X.m = other.n`) or `require("spec").member`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// `require("spec")` holding the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// A getter or setter: the property is not a callable value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accessor: bool,
    /// Defined more than once (literal and assignment, or two assignments):
    /// which value a call sees depends on execution order.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redefined: bool,
    pub location: Location,
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
    /// A `constructor` that returns a value other than `this`: `new C()` may
    /// produce another object (IR 5).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub constructor_returns: bool,
    /// Computed own-property keys that name a Node.js built-in symbol through
    /// a module binding (`this[util.inspect.custom] = …`). They cannot collide
    /// with string-named members only if the specifier is the built-in module,
    /// which the binder checks; otherwise the class has dynamic members (IR 5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub builtin_symbol_keys: Vec<BuiltinSymbolKey>,
}

/// A computed key `util.inspect.custom` (or `inspect.custom`) whose binding is
/// a `require()`/`import` of `specifier` (`util` or `node:util`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuiltinSymbolKey {
    pub expression: String,
    pub specifier: String,
    /// The property the expression reads last (`custom`): a write of it on an
    /// untracked receiver could change the key.
    pub property: String,
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

/// Dynamic `CommonJS` reasons that leave the `module.exports` value itself
/// known: only its members are not statically known.
pub const MEMBER_ONLY_EXPORT_REASONS: [&str; 4] = [
    "computed_export_key",
    "spread_or_computed_export",
    "exports_object_escapes",
    "nested_or_conditional_export_member_write",
];

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
    /// `require("spec").member`, or `local.member` when `local` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The callable or class fact of an inline value (`exports.f = function () {}`,
    /// `module.exports = { m() {} }`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<String>,
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
    /// `const v = w` / `const v = w.p` (`JavaScript`): the value's origin, read
    /// once at the declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<AliasOf>,
    /// `const x = new F(…)`: the constructor identifier, or `ns.F`
    /// (`JavaScript`; TypeScript too since IR 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constructed: Option<String>,
    /// For constructed and identifier-aliased bindings (IR 5): why the value
    /// may reach code Repogi does not follow (passed, returned, stored,
    /// exported), including through a `const b = x` alias. `None` when every
    /// use is a member access, a call, a comparison or such an alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escapes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AliasOf {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
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
    /// The expression form of a callable or class fact that is not a
    /// declaration statement: `function_expression`, `arrow_function`,
    /// `object_method`, `class_expression`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<String>,
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
    /// Entry-point fields of each `package.json` that declares any, by directory.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub packages: BTreeMap<String, PackageEntry>,
    /// Workspace packages declared by the repository root (IR-independent;
    /// part of the resolution fingerprint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Workspace>,
}

/// Local workspace packages declared at the repository root by
/// `package.json` `workspaces` and/or `pnpm-workspace.yaml`, read as data.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    /// The declaring files (`package.json`, `pnpm-workspace.yaml`).
    pub declared_in: Vec<String>,
    /// Member packages with a `name`, sorted by directory.
    pub packages: Vec<WorkspacePackage>,
    /// Why member identities cannot be trusted (unsupported pattern, unreadable
    /// member manifest, symlinked member, conflicting declarations); then no
    /// workspace import resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Dependency declarations on member names, by the declaring
    /// `package.json` directory: name -> version specifier.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspacePackage {
    pub name: String,
    /// Repository-relative package directory.
    pub dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `types` (else `typings`): TypeScript's entry when `exports` is not used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<String>,
    /// `typesVersions` is present: TypeScript may redirect any path.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub types_versions: bool,
}

/// The entry-point fields of a local `package.json`, read as data.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    /// `browser` or `module` is present: bundlers may pick another entry.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alternate_entry: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exports: Option<ExportsTarget>,
    /// The file is unreadable or an entry field has an unsupported shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A `package.json` `exports` value: a subpath map, a condition map, a path,
/// `null`, or a shape Repogi does not follow (arrays).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportsTarget {
    Path(String),
    Null,
    /// Keys in declaration order.
    Subpaths(Vec<(String, ExportsTarget)>),
    Conditions(Vec<(String, ExportsTarget)>),
    Unsupported(String),
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
