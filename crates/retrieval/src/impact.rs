//! Change impact between two indexed snapshots (Phase 4.1).
//!
//! Inputs are two read-only stores (base and head), the directories holding
//! their source text, and the file-level changes between them. Symbol IDs are
//! snapshot-specific and are never compared across snapshots: declarations are
//! matched by (path, kind, name, order among equal names). Consumers of changed
//! declarations come from the base graph (previous consumers, including those of
//! removed declarations) and the head graph (current consumers). Only resolved
//! semantic edges are confirmed; everything else is reported as possible
//! impact.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write as _,
    fs,
    path::Path,
};

use anyhow::{Result, ensure};
use code_intel_core::{Fact, semantic::SemanticEdge};
use code_intel_index::snapshot::{FileChange, SnapshotInfo};
use code_intel_store::{Declaration, Store};
use serde::Serialize;

pub mod summary;

pub const MAX_DEPTH: usize = 4;
/// Skipped sources listed by name in coverage (the rest are counted).
const SKIPPED_SAMPLE: usize = 10;
pub const MAX_RESULTS: usize = 1_000;
/// Incoming sites read per declaration and snapshot before `limit_reached`.
const PER_NODE: usize = 5_000;
const CONSUMING: [&str; 5] = ["CALLS", "REFERENCES", "EXTENDS", "IMPORTS", "RE_EXPORTS"];
const USES: [&str; 3] = ["CALLS", "REFERENCES", "EXTENDS"];

/// One analysed state: its read-only store, provenance and source directory.
pub struct Side<'a> {
    pub store: &'a Store,
    pub info: &'a SnapshotInfo,
    pub source: &'a Path,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Upstream hops from a changed declaration; 1 = direct consumers only.
    pub depth: usize,
    /// Cap on every result list (consumers, dependents, possible sites, …).
    pub max_results: usize,
    /// Changed declarations whose consumers are analysed; the rest are listed only.
    pub max_declarations: usize,
    /// Keep binding chains on every site.
    pub detail: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            depth: 2,
            max_results: 200,
            max_declarations: 100,
            detail: false,
        }
    }
}

/// Snapshot-independent declaration identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct DeclKey {
    pub path: String,
    pub kind: String,
    pub name: String,
    /// Position among same-kind, same-name declarations in the file (overloads).
    #[serde(skip_serializing_if = "is_zero")]
    pub ordinal: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// A declaration in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeclRef {
    pub id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeclarationChange {
    pub name: String,
    pub kind: String,
    pub path: String,
    /// `added`, `removed`, `modified`, or `unknown` (the declaration is missing
    /// from one snapshot whose file has syntax errors).
    pub change: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<DeclRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<DeclRef>,
    /// The file was renamed (Git evidence) from this path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    /// Header before the body differs; `None` when the AST gives no header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_changed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_changed: Option<bool>,
    /// Exported names differ between the snapshots.
    pub export_changed: bool,
    /// Text differs only in whitespace.
    pub whitespace_only: bool,
    pub exported_as: Vec<String>,
    /// How a modified declaration was matched across snapshots: `unique` (the
    /// only one of its kind and name in the file), `text` (identical text),
    /// `remaining` (the single unmatched one on each side) or `position`
    /// (several unmatched, paired in source order: uncertain).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// The match is a guess (`identity` = `position`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub uncertain: bool,
}

/// Changes outside every declaration of one file.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleChange {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    /// `added`, `removed` or `modified`.
    pub change: String,
    pub imports_changed: bool,
    /// Export clauses, re-exports and `export default <expression>`.
    pub exports_changed: bool,
    /// Other top-level code (statements, declaration keywords).
    pub statements_changed: bool,
    /// Files that import this module (any confirmed `IMPORTS`/`RE_EXPORTS` edge into it).
    pub importers: Vec<String>,
    pub importers_omitted: usize,
}

/// A use site of a changed declaration.
#[derive(Debug, Clone, Serialize)]
pub struct Site {
    /// `base`, `head` or `both` (same site in an unchanged file).
    pub snapshot: String,
    pub path: String,
    pub line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub kind: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub jsx: bool,
    pub status: String,
    pub rule: String,
    pub target_text: String,
    /// Enclosing declaration; absent at module scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumer: Option<Consumer>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub binding_chain: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Consumer {
    pub name: String,
    pub kind: String,
    pub id: String,
}

/// A declaration reached upstream from a direct consumer.
#[derive(Debug, Clone, Serialize)]
pub struct Dependent {
    pub snapshot: String,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: usize,
    pub depth: usize,
    /// Evidence path: the changed declaration, then each consumer, ending here.
    pub via: Vec<String>,
}

/// Possible impact: never a confirmed dependency.
#[derive(Debug, Clone, Serialize)]
pub struct Possible {
    /// `unconfirmed_same_name` or `unanalysed_file`.
    pub reason: String,
    pub snapshot: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub jsx: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_text: Option<String>,
    /// For unanalysed files: the tokens found in the file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
}

/// Impact of one changed declaration.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolImpact {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub change: String,
    pub direct: Vec<Site>,
    pub import_only: Vec<Site>,
    pub re_exported_by: Vec<Site>,
    pub transitive: Vec<Dependent>,
    pub possible: Vec<Possible>,
    /// Same-name non-resolved sites left out of `possible` because they bind to
    /// a parameter or nested local (their binding chain proves it).
    pub local_same_name_excluded: usize,
    /// Sites read per declaration exceeded the work limit, or the transitive
    /// traversal hit `max_results`.
    pub limit_reached: bool,
    /// Entries dropped from each list to fit `max_results` or the byte budget.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub omitted: BTreeMap<String, usize>,
    /// No truncation, no work limit and no possible impact.
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Binding {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<DeclKey>,
    pub rule: String,
}

/// A use site in an unchanged file whose binding differs between snapshots.
#[derive(Debug, Clone, Serialize)]
pub struct Rebound {
    pub path: String,
    pub line: usize,
    /// Byte span of the site (deduplication only).
    #[serde(skip)]
    pub span: (usize, usize),
    pub kind: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub jsx: bool,
    pub target_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumer: Option<String>,
    /// `retargeted` (resolved in both, to different declarations: a confirmed
    /// graph change), `gained` (newly resolved), or `lost` (no longer resolved:
    /// possible breakage, or only that Atlas can no longer confirm the binding,
    /// e.g. a barrel with an unresolvable `export *` source).
    pub effect: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Binding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Binding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AffectedFile {
    pub path: String,
    /// `changed`, `direct`, `import`, `transitive`, `rebound`, `importer`, `possible`.
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SideCoverage {
    pub indexed_files: usize,
    /// Files with Tree-sitter errors: no semantic edges, consumers unknown.
    pub syntax_error_files: Vec<String>,
    /// Sources the indexer skipped (generated, too large, non-UTF-8, symlinks).
    /// All of them are screened for mentions of changed declarations; only a
    /// sample is listed.
    pub skipped_files: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_sample: Vec<String>,
    /// Other indexing diagnostics (configuration errors, submodules, unsafe paths).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<code_intel_core::Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub base: SideCoverage,
    pub head: SideCoverage,
    /// Never analysed in this milestone.
    pub not_analysed: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    pub schema_version: u32,
    pub base: SnapshotInfo,
    pub head: SnapshotInfo,
    pub files: Vec<FileChange>,
    /// The module-resolution context differs between the snapshots: a config
    /// (`tsconfig`/`jsconfig`, their `extends`) or the set of module files
    /// (files added, removed or renamed).
    pub resolution_changed: bool,
    pub declarations: Vec<DeclarationChange>,
    /// Declarations in changed files whose text and exports are identical.
    pub unchanged_declarations: usize,
    pub modules: Vec<ModuleChange>,
    pub impacts: Vec<SymbolImpact>,
    /// Changed declarations listed but not analysed (`max_declarations`).
    pub declarations_not_analysed: usize,
    pub rebound: Vec<Rebound>,
    pub rebound_omitted: usize,
    pub affected_files: Vec<AffectedFile>,
    pub affected_files_omitted: usize,
    pub coverage: Coverage,
    pub warnings: Vec<String>,
    /// Conditions that make the analysis incomplete regardless of the result
    /// lists (source changed during analysis, unverifiable source text, content
    /// filters or line-ending conversion, submodules in the working tree).
    pub limitations: Vec<String>,
    /// Every reason `complete` is false; empty when it is true.
    pub incomplete_reasons: Vec<String>,
    /// Entries dropped from top-level lists to fit the byte budget
    /// (`files`, `declarations`, `modules`, `impacts`, coverage lists).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub omitted: BTreeMap<String, usize>,
    /// Some list was cut to fit `max_results` or the byte budget.
    pub truncated: bool,
    /// Every analysed declaration is complete, nothing was truncated, no
    /// unanalysable file could hide a consumer, and no warning applies.
    pub complete: bool,
    /// Actual file-comparison inputs, internal composition metadata only.
    #[serde(skip)]
    pub input_digests: Option<(String, String)>,
}

/// Everything the engine needs about one snapshot, loaded once. Edges are
/// not loaded: they are read through indexed lookups or one streamed scan.
struct Catalog<'a> {
    side: &'a Side<'a>,
    label: &'static str,
    declarations: Vec<Declaration>,
    by_id: HashMap<String, usize>,
    keys: Vec<DeclKey>,
    hashes: BTreeMap<String, String>,
    syntax_errors: Vec<String>,
    skipped: Vec<String>,
    /// Source files whose text could not be read or no longer matches the
    /// snapshot's content hash (completeness-blocking).
    problems: std::cell::RefCell<BTreeSet<String>>,
}

impl<'a> Catalog<'a> {
    fn load(side: &'a Side<'a>, label: &'static str) -> Result<Self> {
        let declarations = side.store.declarations()?;
        let mut by_id = HashMap::with_capacity(declarations.len());
        let mut keys = Vec::with_capacity(declarations.len());
        let mut seen: HashMap<(String, String, String), usize> = HashMap::new();
        for (index, declaration) in declarations.iter().enumerate() {
            by_id.insert(declaration.id.clone(), index);
            let counter = seen
                .entry((
                    declaration.path.clone(),
                    declaration.kind.clone(),
                    declaration.name.clone(),
                ))
                .or_insert(0);
            keys.push(DeclKey {
                path: declaration.path.clone(),
                kind: declaration.kind.clone(),
                name: declaration.name.clone(),
                ordinal: *counter,
            });
            *counter += 1;
        }
        let skipped = side
            .info
            .diagnostics
            .iter()
            .filter(|d| {
                d.message.contains("skipped")
                    || d.message.contains("exceeds")
                    || d.message.contains("not materialised")
            })
            .map(|d| d.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(Self {
            side,
            label,
            declarations,
            by_id,
            keys,
            hashes: side.store.hashes()?,
            syntax_errors: side.store.syntax_error_files()?,
            skipped,
            problems: std::cell::RefCell::default(),
        })
    }

    /// An indexed file's text, only if it still matches the snapshot.
    fn text(&self, path: &str) -> Option<String> {
        let expected = self.hashes.get(path)?;
        let text = fs::read_to_string(self.side.source.join(path)).ok();
        let verified = text.filter(|text| code_intel_index::content_hash(text) == *expected);
        if verified.is_none() {
            self.problems.borrow_mut().insert(format!(
                "{}: source text of {path} is unreadable or differs from the {} snapshot",
                self.label, self.label
            ));
        }
        verified
    }

    fn key(&self, id: &str) -> Option<&DeclKey> {
        self.by_id.get(id).map(|index| &self.keys[*index])
    }

    fn consumer(&self, id: &str) -> Option<Consumer> {
        self.by_id.get(id).map(|index| {
            let declaration = &self.declarations[*index];
            Consumer {
                name: declaration.name.clone(),
                kind: declaration.kind.clone(),
                id: declaration.id.clone(),
            }
        })
    }

    /// Names a declaration is exported under from its own file.
    fn export_names(&self, id: &str, path: &str) -> Result<Vec<String>> {
        let edges =
            self.side
                .store
                .semantic_incoming(id, &["EXPORTS"], u32::try_from(PER_NODE)?)?;
        let names: BTreeSet<String> = edges
            .into_iter()
            .filter(|edge| edge.location.path == path)
            .map(|edge| edge.target_text)
            .collect();
        Ok(names.into_iter().collect())
    }

    /// Barrels re-exporting a declaration, with the names they use.
    fn re_exports(&self, id: &str) -> Result<Vec<SemanticEdge>> {
        self.side
            .store
            .semantic_incoming(id, &["RE_EXPORTS"], u32::try_from(PER_NODE)?)
    }
}

/// Compare two snapshots. `files` are the file-level changes (see
/// `code_intel_index::snapshot::file_changes`).
pub fn analyze(
    base: &Side<'_>,
    head: &Side<'_>,
    files: Vec<FileChange>,
    options: &Options,
) -> Result<ImpactReport> {
    analyze_inner(base, head, files, options, true)
}

fn analyze_inner(
    base: &Side<'_>,
    head: &Side<'_>,
    files: Vec<FileChange>,
    options: &Options,
    cap_sites: bool,
) -> Result<ImpactReport> {
    ensure!(
        (1..=MAX_DEPTH).contains(&options.depth),
        "depth must be between 1 and {MAX_DEPTH}"
    );
    ensure!(
        (1..=MAX_RESULTS).contains(&options.max_results),
        "max_results must be between 1 and {MAX_RESULTS}"
    );
    let started = std::time::Instant::now();
    let before = Catalog::load(base, "base")?;
    let after = Catalog::load(head, "head")?;
    let loaded = started.elapsed();
    let renames: BTreeMap<String, String> = files
        .iter()
        .filter_map(|file| {
            file.previous_path
                .clone()
                .map(|from| (from, file.path.clone()))
        })
        .collect();
    let warnings = Vec::new();
    let changed_sources: Vec<&FileChange> =
        files.iter().filter(|f| f.category == "source").collect();
    let differing = differing_paths(&before, &after, &renames);
    let unexplained = unexplained_differences(&changed_sources, &differing);
    let (mut declarations, unchanged, modules) =
        diff_files(&before, &after, &changed_sources, options)?;
    declarations.sort_by(|a, b| {
        (&a.path, &a.name, &a.kind, &a.change).cmp(&(&b.path, &b.name, &b.kind, &b.change))
    });
    let resolution_changed =
        base.store.resolution_fingerprint()? != head.store.resolution_fingerprint()?;
    let diffed = started.elapsed();
    let analysed = declarations.len().min(options.max_declarations);
    let terms = declarations
        .iter()
        .take(analysed)
        .map(|change| search_terms(&before, &after, change))
        .collect::<Result<Vec<_>>>()?;
    let names: BTreeSet<String> = terms.iter().flat_map(|t| t.names.iter().cloned()).collect();
    let (rebound, candidates) = rebound_sites(&before, &after, &renames, &differing, &names)?;
    let rebound_done = started.elapsed();
    let impacts = declarations
        .iter()
        .zip(&terms)
        .map(|(change, terms)| impact_of(&before, &after, change, terms, &candidates, options))
        .collect::<Result<Vec<_>>>()?;
    let coverage = coverage(&before, &after);
    let mut report = ImpactReport {
        schema_version: 1,
        base: base.info.clone(),
        head: head.info.clone(),
        files,
        resolution_changed,
        declarations,
        unchanged_declarations: unchanged,
        modules,
        impacts,
        declarations_not_analysed: 0,
        rebound,
        rebound_omitted: 0,
        affected_files: Vec::new(),
        affected_files_omitted: 0,
        coverage,
        warnings,
        limitations: before
            .problems
            .borrow()
            .iter()
            .chain(after.problems.borrow().iter())
            .cloned()
            .chain(unexplained)
            .collect(),
        incomplete_reasons: Vec::new(),
        omitted: BTreeMap::new(),
        truncated: false,
        complete: false,
        input_digests: None,
    };
    report.declarations_not_analysed = report.declarations.len() - analysed;
    code_intel_store::profile(&[
        ("impact_load_catalogs", loaded),
        ("impact_diff_files", diffed.saturating_sub(loaded)),
        ("impact_rebound", rebound_done.saturating_sub(diffed)),
        (
            "impact_consumers",
            started.elapsed().saturating_sub(rebound_done),
        ),
    ]);
    notes(&mut report, options);
    if cap_sites {
        cap(&mut report, options.max_results);
    }
    finish(&mut report);
    Ok(report)
}

/// Indexed files that differ between the snapshots although no file-level
/// change lists them: the inputs were not stable during the analysis.
fn unexplained_differences(
    changed_sources: &[&FileChange],
    differing: &BTreeSet<String>,
) -> Vec<String> {
    let listed: BTreeSet<String> = changed_sources
        .iter()
        .flat_map(|f| [Some(f.path.clone()), f.previous_path.clone()])
        .flatten()
        .collect();
    let unlisted: Vec<&String> = differing.iter().filter(|p| !listed.contains(*p)).collect();
    unlisted
        .first()
        .map(|first| {
            format!(
                "{} indexed file(s) differ between the snapshots without a file-level change (first: {first}); the working tree may have changed during analysis",
                unlisted.len()
            )
        })
        .into_iter()
        .collect()
}

/// Indexed paths whose content differs between the snapshots (renames mapped).
fn differing_paths(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    renames: &BTreeMap<String, String>,
) -> BTreeSet<String> {
    let mut differing: BTreeSet<String> = BTreeSet::new();
    for (path, hash) in &before.hashes {
        let mapped = renames.get(path).unwrap_or(path);
        if after.hashes.get(mapped) != Some(hash) {
            differing.insert(path.clone());
        }
    }
    for (path, hash) in &after.hashes {
        let original = renames
            .iter()
            .find(|(_, to)| *to == path)
            .map_or(path, |(from, _)| from);
        if before.hashes.get(original) != Some(hash) {
            differing.insert(path.clone());
        }
    }
    differing
}

fn notes(report: &mut ImpactReport, options: &Options) {
    if report.declarations_not_analysed > 0 {
        report.warnings.push(format!(
            "{} changed declaration(s) not analysed (max_declarations {})",
            report.declarations_not_analysed, options.max_declarations
        ));
    }
    // The resolution fingerprint also covers the set of module files, so
    // adding, removing or renaming a file changes it; warn only for configs.
    let configs_changed = report
        .files
        .iter()
        .any(|f| matches!(f.category.as_str(), "config" | "package" | "json"));
    if report.resolution_changed && configs_changed {
        report.warnings.push(
            "module-resolution configuration changed: every binding was recomputed; see rebound sites"
                .into(),
        );
    }
}

type FileDiff = (Vec<DeclarationChange>, usize, Vec<ModuleChange>);

/// Declaration and module-level changes of every changed source file.
fn diff_files(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    changed_sources: &[&FileChange],
    options: &Options,
) -> Result<FileDiff> {
    let mut declarations = Vec::new();
    let mut unchanged = 0;
    let mut modules = Vec::new();
    for file in changed_sources {
        let base_path = (file.status != "added").then(|| {
            file.previous_path
                .clone()
                .unwrap_or_else(|| file.path.clone())
        });
        let head_path = (file.status != "removed").then(|| file.path.clone());
        let old = match &base_path {
            Some(path) if before.hashes.contains_key(path) => {
                before.side.store.file_facts(path, 100_000)?
            }
            _ => Vec::new(),
        };
        let new = match &head_path {
            Some(path) if after.hashes.contains_key(path) => {
                after.side.store.file_facts(path, 100_000)?
            }
            _ => Vec::new(),
        };
        let (changes, same) = diff_declarations(before, after, &old, &new, file)?;
        unchanged += same;
        declarations.extend(changes);
        if let Some(module) = module_change(
            before,
            after,
            file,
            base_path.as_deref(),
            head_path.as_deref(),
            &old,
            &new,
            options,
        )? {
            modules.push(module);
        }
    }
    Ok((declarations, unchanged, modules))
}

fn coverage(before: &Catalog<'_>, after: &Catalog<'_>) -> Coverage {
    Coverage {
        base: side_coverage(before),
        head: side_coverage(after),
        not_analysed: vec![
            "JavaScript sources (.js/.jsx/.mjs/.cjs) and .mts/.cts files".into(),
            "packages and node_modules; excluded directories (dist, build, coverage, .next, generated)".into(),
            "runtime behaviour: dynamic imports, instance dispatch, values passed through props or registries".into(),
        ],
    }
}

fn side_coverage(catalog: &Catalog<'_>) -> SideCoverage {
    let listed: BTreeSet<&String> = catalog
        .skipped
        .iter()
        .chain(&catalog.syntax_errors)
        .collect();
    SideCoverage {
        indexed_files: catalog.hashes.len(),
        syntax_error_files: catalog.syntax_errors.clone(),
        skipped_files: catalog.skipped.len(),
        skipped_sample: catalog
            .skipped
            .iter()
            .take(SKIPPED_SAMPLE)
            .cloned()
            .collect(),
        diagnostics: catalog
            .side
            .info
            .diagnostics
            .iter()
            .filter(|d| !listed.contains(&d.path))
            .cloned()
            .collect(),
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Header before the declaration body, from the stored signature or by
/// reparsing a function-valued variable.
fn header(fact: &Fact) -> Option<String> {
    match fact.kind.as_str() {
        "variable" => code_intel_index::variable_header(&fact.path, &fact.snippet),
        "function" | "class" | "method" => fact
            .signature
            .as_ref()
            .map(|signature| signature.snippet.clone()),
        _ => None,
    }
}

fn reference(fact: &Fact) -> DeclRef {
    DeclRef {
        id: fact.id.clone(),
        path: fact.path.clone(),
        start_line: fact.start_line,
        end_line: fact.end_line,
    }
}

/// Pair declarations of one file across snapshots: identical text first (so a
/// moved declaration keeps its identity), then the rest in source order.
fn diff_declarations(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    old: &[Fact],
    new: &[Fact],
    file: &FileChange,
) -> Result<(Vec<DeclarationChange>, usize)> {
    let declaration = |fact: &&Fact| !matches!(fact.kind.as_str(), "file" | "import" | "export");
    let mut groups: BTreeMap<(String, String), Sides<'_>> = BTreeMap::new();
    for fact in old.iter().filter(declaration) {
        groups
            .entry((fact.kind.clone(), fact.name.clone()))
            .or_default()
            .0
            .push(fact);
    }
    for fact in new.iter().filter(declaration) {
        groups
            .entry((fact.kind.clone(), fact.name.clone()))
            .or_default()
            .1
            .push(fact);
    }
    let mut changes = Vec::new();
    let mut same = 0;
    for ((kind, name), (olds, news)) in groups {
        for (old, new, identity) in pair_facts(&olds, &news) {
            let exported_before = match old {
                Some(f) => before.export_names(&f.id, &f.path)?,
                None => Vec::new(),
            };
            let exported_after = match new {
                Some(f) => after.export_names(&f.id, &f.path)?,
                None => Vec::new(),
            };
            let (change, signature, body, whitespace) = match (old, new) {
                // A declaration missing beside syntax errors is unknown, not gone.
                // Missing beside syntax errors, or in a file the indexer skipped
                // in that snapshot: unknown, not gone.
                (Some(_), None)
                    if file.git_state.as_deref() == Some("excluded")
                        || (file.status != "removed"
                            && (after.syntax_errors.contains(&file.path)
                                || !after.hashes.contains_key(&file.path))) =>
                {
                    ("unknown", None, None, false)
                }
                (None, Some(_))
                    if file.status != "added" && {
                        let base = file.previous_path.as_ref().unwrap_or(&file.path);
                        before.syntax_errors.contains(base) || !before.hashes.contains_key(base)
                    } =>
                {
                    ("unknown", None, None, false)
                }
                (Some(_), None) => ("removed", None, None, false),
                (None, Some(_)) => ("added", None, None, false),
                (Some(a), Some(b))
                    if a.snippet == b.snippet && exported_before == exported_after =>
                {
                    same += 1;
                    continue;
                }
                (Some(a), Some(b)) => {
                    let (signature, body, whitespace) = classify(a, b);
                    ("modified", signature, body, whitespace)
                }
                (None, None) => continue,
            };
            let mut exported_as: BTreeSet<String> = exported_before.iter().cloned().collect();
            exported_as.extend(exported_after.iter().cloned());
            changes.push(DeclarationChange {
                name: name.clone(),
                kind: kind.clone(),
                path: new.map_or_else(
                    || {
                        file.previous_path
                            .clone()
                            .unwrap_or_else(|| file.path.clone())
                    },
                    |f| f.path.clone(),
                ),
                change: change.into(),
                base: old.map(reference),
                head: new.map(reference),
                previous_path: file.previous_path.clone(),
                signature_changed: signature,
                body_changed: body,
                export_changed: old.is_some() && new.is_some() && exported_before != exported_after,
                whitespace_only: whitespace,
                exported_as: exported_as.into_iter().collect(),
                identity: (change == "modified").then(|| identity.to_owned()),
                uncertain: change == "modified" && identity == "position",
            });
        }
    }
    Ok((changes, same))
}

/// Same-kind, same-name declarations of one file across snapshots: identical
/// text first (a moved declaration keeps its identity), then the rest in order.
type Pair<'f> = (Option<&'f Fact>, Option<&'f Fact>, &'static str);
/// Base and head declarations of one (kind, name).
type Sides<'f> = (Vec<&'f Fact>, Vec<&'f Fact>);

fn pair_facts<'f>(olds: &[&'f Fact], news: &[&'f Fact]) -> Vec<Pair<'f>> {
    let mut paired_new = vec![false; news.len()];
    let mut pairs: Vec<Pair<'f>> = Vec::new();
    let mut leftover_old = Vec::new();
    let unique = olds.len() == 1 && news.len() == 1;
    for fact in olds {
        if let Some(position) =
            (0..news.len()).find(|i| !paired_new[*i] && news[*i].snippet == fact.snippet)
        {
            paired_new[position] = true;
            pairs.push((
                Some(fact),
                Some(news[position]),
                if unique { "unique" } else { "text" },
            ));
        } else {
            leftover_old.push(*fact);
        }
    }
    let leftover_new: Vec<&'f Fact> = news
        .iter()
        .enumerate()
        .filter(|(i, _)| !paired_new[*i])
        .map(|(_, f)| *f)
        .collect();
    // One leftover on each side is forced; several are paired by position,
    // which is a guess and is reported as such.
    let identity = if unique {
        "unique"
    } else if leftover_old.len() == 1 && leftover_new.len() == 1 {
        "remaining"
    } else {
        "position"
    };
    let mut leftover_new = leftover_new.into_iter();
    for fact in leftover_old {
        let new = leftover_new.next();
        pairs.push((Some(fact), new, if new.is_some() { identity } else { "" }));
    }
    pairs.extend(leftover_new.map(|fact| (None, Some(fact), "")));
    pairs
}

/// (signature changed, body changed, whitespace only) for a modified declaration.
/// Signature and body are `None` when the AST gives no header for either side.
fn classify(a: &Fact, b: &Fact) -> (Option<bool>, Option<bool>, bool) {
    let (signature, body) = match (header(a), header(b)) {
        (Some(ha), Some(hb)) if a.snippet.starts_with(&ha) && b.snippet.starts_with(&hb) => (
            Some(collapse(&ha) != collapse(&hb)),
            Some(collapse(&a.snippet[ha.len()..]) != collapse(&b.snippet[hb.len()..])),
        ),
        _ => (None, None),
    };
    let whitespace = a.snippet != b.snippet && collapse(&a.snippet) == collapse(&b.snippet);
    (signature, body, whitespace)
}

/// Text outside every fact span (declarations, imports, export statements).
fn residue(text: &str, facts: &[Fact]) -> String {
    let mut spans: Vec<(usize, usize)> = facts
        .iter()
        .filter(|f| f.kind != "file")
        .map(|f| (f.start_byte, f.end_byte))
        .collect();
    spans.sort_unstable();
    let mut rest = String::new();
    let mut position = 0;
    for (start, end) in spans {
        if start > position {
            rest.push_str(text.get(position..start).unwrap_or_default());
            rest.push(' ');
        }
        position = position.max(end);
    }
    rest.push_str(text.get(position.min(text.len())..).unwrap_or_default());
    collapse(&rest)
}

fn statements(facts: &[Fact], kind: &str) -> Vec<String> {
    let declarations: Vec<(usize, usize)> = facts
        .iter()
        .filter(|f| !matches!(f.kind.as_str(), "file" | "import" | "export"))
        .map(|f| (f.start_byte, f.end_byte))
        .collect();
    let mut texts: Vec<String> = facts
        .iter()
        .filter(|f| f.kind == kind)
        // Export statements that wrap a declaration are compared as that declaration.
        .filter(|f| {
            kind != "export"
                || !declarations
                    .iter()
                    .any(|(s, e)| *s >= f.start_byte && *e <= f.end_byte)
        })
        .map(|f| collapse(&f.snippet))
        .collect();
    texts.sort();
    texts
}

#[allow(clippy::too_many_arguments)]
fn module_change(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    file: &FileChange,
    base_path: Option<&str>,
    head_path: Option<&str>,
    old: &[Fact],
    new: &[Fact],
    options: &Options,
) -> Result<Option<ModuleChange>> {
    // Unverifiable text is reported (problems) and treated as changed.
    let read = |catalog: &Catalog<'_>, path: Option<&str>| -> Option<String> {
        path.filter(|p| catalog.hashes.contains_key(*p))
            .and_then(|p| catalog.text(p))
    };
    let (imports, exports, statements_changed) = match (base_path, head_path) {
        (Some(_), Some(_)) => {
            let statements_changed = match (read(before, base_path), read(after, head_path)) {
                (Some(a), Some(b)) => match (
                    code_intel_index::module_residue(base_path.unwrap_or_default(), &a),
                    code_intel_index::module_residue(head_path.unwrap_or_default(), &b),
                ) {
                    (Some(x), Some(y)) => x != y,
                    // A file that does not parse cleanly: compare raw residue text.
                    _ => residue(&a, old) != residue(&b, new),
                },
                // Text unavailable or not the indexed text: assume changed.
                _ => true,
            };
            (
                statements(old, "import") != statements(new, "import"),
                statements(old, "export") != statements(new, "export"),
                statements_changed,
            )
        }
        _ => (true, true, true),
    };
    if !(imports || exports || statements_changed) {
        return Ok(None);
    }
    let change = match (base_path, head_path) {
        (None, _) => "added",
        (_, None) => "removed",
        _ => "modified",
    };
    // Importers: confirmed IMPORTS/RE_EXPORTS into any declaration of the file or the file itself.
    let mut importers: BTreeSet<String> = BTreeSet::new();
    for (catalog, path) in [(before, base_path), (after, head_path)] {
        let Some(path) = path else { continue };
        let mut targets: Vec<String> = catalog
            .declarations
            .iter()
            .filter(|d| d.path == path)
            .map(|d| d.id.clone())
            .collect();
        targets.push(code_intel_core::file_id(path));
        for target in targets {
            for edge in catalog.side.store.semantic_incoming(
                &target,
                &["IMPORTS", "RE_EXPORTS"],
                u32::try_from(PER_NODE)?,
            )? {
                if edge.location.path != path {
                    importers.insert(edge.location.path);
                }
            }
        }
    }
    let total = importers.len();
    let importers: Vec<String> = importers.into_iter().take(options.max_results).collect();
    Ok(Some(ModuleChange {
        path: file.path.clone(),
        previous_path: file.previous_path.clone(),
        change: change.into(),
        imports_changed: imports,
        exports_changed: exports,
        statements_changed,
        importers_omitted: total - importers.len(),
        importers,
    }))
}

/// Binding of an edge, with its target as a snapshot-independent key.
fn binding(
    catalog: &Catalog<'_>,
    edge: &SemanticEdge,
    renames: &BTreeMap<String, String>,
) -> Binding {
    let target = edge.target_id.as_deref().and_then(|id| {
        if let Some(key) = catalog.key(id) {
            let mut key = key.clone();
            if let Some(to) = renames.get(&key.path) {
                key.path.clone_from(to);
            }
            Some(key)
        } else {
            id.strip_prefix("file:").map(|path| DeclKey {
                path: renames
                    .get(path)
                    .cloned()
                    .unwrap_or_else(|| path.to_owned()),
                kind: "file".into(),
                name: String::new(),
                ordinal: 0,
            })
        }
    });
    Binding {
        status: status_name(edge),
        target,
        rule: edge.evidence.rule.clone(),
    }
}

fn status_name(edge: &SemanticEdge) -> String {
    format!("{:?}", edge.status).to_lowercase()
}

/// A non-resolved edge naming a changed declaration, with its snapshot label.
type Candidate = (&'static str, SemanticEdge);
type Candidates = Vec<Candidate>;

/// One streamed pass over a snapshot's edges.
struct Scan {
    /// Order-sensitive hash and count of each unchanged file's raw edge rows.
    sequences: HashMap<String, (u64, usize)>,
    /// Non-resolved edges whose text names a changed declaration (pre-filtered
    /// by substring, then parsed).
    candidates: Vec<SemanticEdge>,
}

fn scan(
    catalog: &Catalog<'_>,
    unchanged: &dyn Fn(&str) -> bool,
    patterns: &[String],
) -> Result<Scan> {
    use std::hash::{Hash, Hasher};
    let mut hashers: HashMap<String, (std::collections::hash_map::DefaultHasher, usize)> =
        HashMap::new();
    let mut candidates = Vec::new();
    catalog.side.store.scan_edges(&mut |path, data| {
        if unchanged(path) {
            let entry = hashers.entry(path.to_owned()).or_default();
            data.hash(&mut entry.0);
            entry.1 += 1;
        }
        if data.contains("\"target_id\":null") && patterns.iter().any(|p| data.contains(p.as_str()))
        {
            candidates.push(serde_json::from_str(data)?);
        }
        Ok(())
    })?;
    Ok(Scan {
        sequences: hashers
            .into_iter()
            .map(|(path, (hasher, count))| (path, (hasher.finish(), count)))
            .collect(),
        candidates,
    })
}

/// Parsed edges of the given files.
fn edges_of(catalog: &Catalog<'_>, paths: &BTreeSet<String>) -> Result<Vec<SemanticEdge>> {
    let mut edges = Vec::new();
    catalog.side.store.scan_edges(&mut |path, data| {
        if paths.contains(path) {
            edges.push(serde_json::from_str(data)?);
        }
        Ok(())
    })?;
    Ok(edges)
}

/// Sites in files with identical content in both snapshots whose binding
/// (status or target declaration) differs: effects of barrel, export and
/// configuration changes on code that did not change. Also returns the
/// non-resolved candidate sites for `names` from both snapshots.
fn rebound_sites(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    renames: &BTreeMap<String, String>,
    differing: &BTreeSet<String>,
    names: &BTreeSet<String>,
) -> Result<(Vec<Rebound>, Candidates)> {
    type Key = (String, usize, usize, String, String);
    // Renamed files are file-level changes on both sides, never rebinding evidence.
    let moved: BTreeSet<&String> = renames.keys().chain(renames.values()).collect();
    let unchanged = |path: &str| {
        before.hashes.contains_key(path)
            && after.hashes.contains_key(path)
            && !differing.contains(path)
            && !moved.iter().any(|m| m.as_str() == path)
    };
    let patterns: Vec<String> = names
        .iter()
        .flat_map(|name| {
            [
                format!("\"target_text\":\"{name}\",\"kind\""),
                format!(".{name}\",\"kind\""),
            ]
        })
        .collect();
    let old = scan(before, &unchanged, &patterns)?;
    let new = scan(after, &unchanged, &patterns)?;
    // Same IR in an unchanged file yields the same edge rows unless a binding
    // moved; only files whose row sequences differ are parsed and compared.
    let mismatched: BTreeSet<String> = old
        .sequences
        .keys()
        .chain(new.sequences.keys())
        .filter(|path| old.sequences.get(*path) != new.sequences.get(*path))
        .cloned()
        .collect();
    let mut previous: HashMap<Key, SemanticEdge> = HashMap::new();
    let mut rebound = Vec::new();
    if !mismatched.is_empty() {
        let key = |edge: &SemanticEdge| -> Key {
            (
                edge.location.path.clone(),
                edge.location.start_byte,
                edge.location.end_byte,
                kind_name(edge),
                edge.target_text.clone(),
            )
        };
        for edge in edges_of(before, &mismatched)? {
            previous.insert(key(&edge), edge);
        }
        let identity = BTreeMap::new();
        for edge in edges_of(after, &mismatched)? {
            let now = binding(after, &edge, &identity);
            let was = previous
                .remove(&key(&edge))
                .map(|old| binding(before, &old, renames));
            if was
                .as_ref()
                .is_some_and(|p| p.status == now.status && p.target == now.target)
            {
                continue;
            }
            rebound.push(rebound_entry(after, &edge, was, Some(now)));
        }
        for (_, edge) in previous {
            let was = binding(before, &edge, renames);
            rebound.push(rebound_entry(before, &edge, Some(was), None));
        }
    }
    let mut rebound = one_per_location(
        rebound,
        |r| (r.path.clone(), r.span, r.target_text.clone()),
        |r| r.kind.clone(),
    );
    rebound.sort_by(|a, b| {
        (&a.path, a.line, &a.kind, &a.target_text).cmp(&(&b.path, b.line, &b.kind, &b.target_text))
    });
    let candidates = old
        .candidates
        .into_iter()
        .map(|edge| ("base", edge))
        .chain(new.candidates.into_iter().map(|edge| ("head", edge)))
        .collect();
    Ok((rebound, candidates))
}

fn rebound_entry(
    catalog: &Catalog<'_>,
    edge: &SemanticEdge,
    before: Option<Binding>,
    after: Option<Binding>,
) -> Rebound {
    Rebound {
        path: edge.location.path.clone(),
        line: edge.location.start_line,
        span: (edge.location.start_byte, edge.location.end_byte),
        kind: kind_name(edge),
        jsx: edge.evidence.syntax.ends_with("> JSX element"),
        target_text: edge.target_text.clone(),
        consumer: catalog.consumer(&edge.source_id).map(|c| c.name),
        effect: effect(before.as_ref(), after.as_ref()).into(),
        before,
        after,
    }
}

fn effect(before: Option<&Binding>, after: Option<&Binding>) -> &'static str {
    let resolved = |b: Option<&Binding>| b.is_some_and(|b| b.status == "resolved");
    match (resolved(before), resolved(after)) {
        (true, true) => "retargeted",
        (false, true) => "gained",
        (true, false) => "lost",
        // Unconfirmed in both (status or rule changed): nothing confirmed moved.
        (false, false) => "unconfirmed",
    }
}

fn site(catalog: &Catalog<'_>, edge: &SemanticEdge, snapshot: &str, detail: bool) -> Site {
    Site {
        snapshot: snapshot.into(),
        path: edge.location.path.clone(),
        line: edge.location.start_line,
        start_byte: edge.location.start_byte,
        end_byte: edge.location.end_byte,
        kind: kind_name(edge),
        jsx: edge.evidence.syntax.ends_with("> JSX element"),
        status: format!("{:?}", edge.status).to_lowercase(),
        rule: edge.evidence.rule.clone(),
        target_text: edge.target_text.clone(),
        consumer: catalog.consumer(&edge.source_id),
        binding_chain: if detail {
            edge.evidence.binding_chain.clone()
        } else {
            Vec::new()
        },
    }
}

fn kind_name(edge: &SemanticEdge) -> String {
    serde_json::to_value(edge.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Incoming confirmed sites of `id` in one snapshot, excluding the declaration's own body.
fn incoming(catalog: &Catalog<'_>, id: &str, kinds: &[&str]) -> Result<(Vec<SemanticEdge>, bool)> {
    let mut edges =
        catalog
            .side
            .store
            .semantic_incoming(id, kinds, u32::try_from(PER_NODE + 1)?)?;
    let limited = edges.len() > PER_NODE;
    edges.truncate(PER_NODE);
    edges.retain(|edge| edge.source_id != id);
    Ok((edges, limited))
}

fn impact_of(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    change: &DeclarationChange,
    terms: &Terms,
    candidates: &[Candidate],
    options: &Options,
) -> Result<SymbolImpact> {
    let mut limit_reached = false;
    // A site reported by both snapshots at the same location (its file is
    // unchanged) is one site, "both"; otherwise it belongs to one snapshot.
    let mut sites: BTreeMap<(String, usize, usize, String), Site> = BTreeMap::new();
    for (catalog, declaration) in [(before, &change.base), (after, &change.head)] {
        let Some(declaration) = declaration else {
            continue;
        };
        let (edges, limited) = incoming(catalog, &declaration.id, &CONSUMING)?;
        limit_reached |= limited;
        for edge in edges {
            let unchanged_file = before.hashes.contains_key(&edge.location.path)
                && before.hashes.get(&edge.location.path) == after.hashes.get(&edge.location.path);
            let key = (
                edge.location.path.clone(),
                edge.location.start_byte,
                edge.location.end_byte,
                format!(
                    "{}{}",
                    kind_name(&edge),
                    if unchanged_file { "" } else { catalog.label }
                ),
            );
            match sites.get_mut(&key) {
                Some(existing) => existing.snapshot = "both".into(),
                None => {
                    sites.insert(key, site(catalog, &edge, catalog.label, options.detail));
                }
            }
        }
    }
    let sites = pair_sites(one_per_location(
        sites.into_values().collect(),
        |s| {
            (
                s.snapshot.clone(),
                s.path.clone(),
                s.start_byte,
                s.end_byte,
                s.target_text.clone(),
            )
        },
        |s| s.kind.clone(),
    ));
    let mut direct = Vec::new();
    let mut imports = Vec::new();
    let mut re_exported_by = Vec::new();
    for site in sites {
        match site.kind.as_str() {
            "IMPORTS" => imports.push(site),
            "RE_EXPORTS" => re_exported_by.push(site),
            _ => direct.push(site),
        }
    }
    let used: BTreeSet<(String, String)> = direct
        .iter()
        .map(|s| (s.path.clone(), s.snapshot.clone()))
        .collect();
    let import_only: Vec<Site> = imports
        .into_iter()
        .filter(|s| !used.contains(&(s.path.clone(), s.snapshot.clone())))
        .collect();
    let order = |a: &Site, b: &Site| {
        (&a.path, a.line, a.start_byte, &a.kind, &a.snapshot).cmp(&(
            &b.path,
            b.line,
            b.start_byte,
            &b.kind,
            &b.snapshot,
        ))
    };
    direct.sort_by(order);
    let mut import_only = import_only;
    import_only.sort_by(order);
    re_exported_by.sort_by(order);
    let (transitive, walk_limited) = transitive(before, after, change, &direct, options)?;
    limit_reached |= walk_limited;
    let (possible, local_same_name_excluded) = possible(before, after, terms, candidates, &direct);
    Ok(SymbolImpact {
        name: change.name.clone(),
        kind: change.kind.clone(),
        path: change.path.clone(),
        change: change.change.clone(),
        direct,
        import_only,
        re_exported_by,
        transitive,
        possible,
        local_same_name_excluded,
        limit_reached,
        omitted: BTreeMap::new(),
        complete: false,
    })
}

/// Kind precedence when one source span carries several edges (a call is also
/// recorded as a REFERENCES of its callee): the most specific kind is kept.
fn kind_rank(kind: &str) -> usize {
    ["CALLS", "EXTENDS", "REFERENCES", "IMPORTS", "RE_EXPORTS"]
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(9)
}

/// Keep one item per location, preferring the most specific kind.
fn one_per_location<T, K: Ord>(
    items: Vec<T>,
    location: impl Fn(&T) -> K,
    kind: impl Fn(&T) -> String,
) -> Vec<T> {
    let mut best: BTreeMap<K, T> = BTreeMap::new();
    for item in items {
        let key = location(&item);
        let better = best
            .get(&key)
            .is_none_or(|existing| kind_rank(&kind(&item)) < kind_rank(&kind(existing)));
        if better {
            best.insert(key, item);
        }
    }
    best.into_values().collect()
}

/// In files that changed, the same use appears once per snapshot at possibly
/// shifted offsets. Pair base and head sites with the same path, kind, enclosing
/// declaration and text, in source order, into one `both` site (head location).
fn pair_sites(sites: Vec<Site>) -> Vec<Site> {
    type Group = (String, String, Option<(String, String)>, String);
    let group = |site: &Site| -> Group {
        (
            site.path.clone(),
            site.kind.clone(),
            site.consumer
                .as_ref()
                .map(|c| (c.kind.clone(), c.name.clone())),
            site.target_text.clone(),
        )
    };
    let mut bases: BTreeMap<Group, Vec<Site>> = BTreeMap::new();
    let mut paired = Vec::new();
    for site in sites.iter().filter(|s| s.snapshot == "base") {
        bases.entry(group(site)).or_default().push(site.clone());
    }
    let mut heads: Vec<Site> = Vec::new();
    for site in sites {
        match site.snapshot.as_str() {
            "head" => heads.push(site),
            "base" => {}
            _ => paired.push(site),
        }
    }
    heads.sort_by_key(|s| (s.path.clone(), s.start_byte));
    for list in bases.values_mut() {
        list.sort_by_key(|s| s.start_byte);
        list.reverse();
    }
    for mut site in heads {
        if bases.get_mut(&group(&site)).and_then(Vec::pop).is_some() {
            site.snapshot = "both".into();
        }
        paired.push(site);
    }
    paired.extend(bases.into_values().flatten());
    paired
}

/// Breadth-first upstream from the direct consumers' declarations over confirmed
/// CALLS/REFERENCES/EXTENDS, in the snapshot each consumer was found in.
fn transitive(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    change: &DeclarationChange,
    direct: &[Site],
    options: &Options,
) -> Result<(Vec<Dependent>, bool)> {
    let mut dependents = Vec::new();
    let mut limited = false;
    if options.depth < 2 {
        return Ok((dependents, limited));
    }
    let mut visited: BTreeSet<(&str, String)> = BTreeSet::new();
    for (label, declaration) in [("base", &change.base), ("head", &change.head)] {
        if let Some(declaration) = declaration {
            visited.insert((label, declaration.id.clone()));
        }
    }
    let mut frontier: Vec<(&Catalog<'_>, String, Vec<String>)> = Vec::new();
    for site in direct {
        let Some(consumer) = &site.consumer else {
            continue;
        };
        let catalog = if site.snapshot == "base" {
            before
        } else {
            after
        };
        if visited.insert((catalog.label, consumer.id.clone())) {
            frontier.push((
                catalog,
                consumer.id.clone(),
                vec![change.name.clone(), consumer.name.clone()],
            ));
        }
    }
    for depth in 2..=options.depth {
        let mut next = Vec::new();
        for (catalog, id, via) in &frontier {
            let (edges, edge_limited) = incoming(catalog, id, &USES)?;
            limited |= edge_limited;
            for edge in edges {
                let Some(consumer) = catalog.consumer(&edge.source_id) else {
                    continue;
                };
                if !visited.insert((catalog.label, consumer.id.clone())) {
                    continue;
                }
                if dependents.len() >= options.max_results {
                    limited = true;
                    continue;
                }
                let declaration = &catalog.declarations[catalog.by_id[&consumer.id]];
                let mut path = via.clone();
                path.push(consumer.name.clone());
                dependents.push(Dependent {
                    snapshot: catalog.label.into(),
                    name: consumer.name.clone(),
                    kind: consumer.kind.clone(),
                    path: declaration.path.clone(),
                    line: declaration.start_line,
                    depth,
                    via: path.clone(),
                });
                next.push((*catalog, consumer.id, path));
            }
        }
        frontier = next;
    }
    dependents.sort_by(|a, b| {
        (a.depth, &a.path, a.line, &a.name, &a.snapshot).cmp(&(
            b.depth,
            &b.path,
            b.line,
            &b.name,
            &b.snapshot,
        ))
    });
    Ok((dependents, limited))
}

fn identifier_tokens(text: &str) -> BTreeSet<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|token| !token.is_empty())
        .collect()
}

fn module_token(path: &str) -> Option<String> {
    let mut parts = path.rsplit('/');
    let file = parts.next()?;
    let stem = file.split('.').next()?;
    if stem == "index" {
        parts.next().map(str::to_owned)
    } else {
        Some(stem.to_owned())
    }
}

/// Unconfirmed same-name sites, and unanalysable files that mention the
/// declaration, an export alias or its module. Never confirmed.
/// Names a declaration can be referred to by (its own and its export aliases),
/// and the tokens that reveal a possible use in an unanalysable file (those
/// names, plus module names where it is reachable as a default export).
fn search_terms(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    change: &DeclarationChange,
) -> Result<Terms> {
    let short = change
        .name
        .rsplit('.')
        .next()
        .unwrap_or(&change.name)
        .to_owned();
    let mut names: BTreeSet<String> = BTreeSet::from([short]);
    names.extend(
        change
            .exported_as
            .iter()
            .filter(|name| name.as_str() != "default")
            .cloned(),
    );
    let mut tokens: BTreeSet<String> = names.clone();
    // Any import except a default import must spell the exported name, so a
    // module's name is a search token only where the declaration is reachable
    // as `default` (from its own module or a barrel re-exporting it as default).
    if change.exported_as.iter().any(|name| name == "default") {
        tokens.extend(module_token(&change.path));
        if let Some(previous) = &change.previous_path {
            tokens.extend(module_token(previous));
        }
    }
    for (catalog, declaration) in [(before, &change.base), (after, &change.head)] {
        let Some(declaration) = declaration else {
            continue;
        };
        for edge in catalog.re_exports(&declaration.id)? {
            if edge.target_text == "default" {
                tokens.extend(module_token(&edge.location.path));
            } else {
                names.insert(edge.target_text.clone());
                tokens.insert(edge.target_text.clone());
            }
        }
    }
    Ok(Terms { names, tokens })
}

/// What a changed declaration can be called, and what reveals it in text.
struct Terms {
    /// Its name and export aliases (same-name site search).
    names: BTreeSet<String>,
    /// Names plus module names where reachable as a default export
    /// (unanalysable-file screening).
    tokens: BTreeSet<String>,
}

/// Possible sites, and how many same-name sites were excluded because they bind
/// to a lexical local. `candidates` are the non-resolved edges of both
/// snapshots whose text names some changed declaration (from the edge scan).
fn possible(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    terms: &Terms,
    candidates: &[Candidate],
    direct: &[Site],
) -> (Vec<Possible>, usize) {
    let confirmed: BTreeSet<(String, usize)> = direct
        .iter()
        .map(|s| (s.path.clone(), s.start_byte))
        .collect();
    let mut found: BTreeMap<(String, usize, String), Possible> = BTreeMap::new();
    let mut shadowed: BTreeSet<(String, usize)> = BTreeSet::new();
    for (label, edge) in candidates {
        let kind = kind_name(edge);
        if !USES.contains(&kind.as_str()) {
            continue;
        }
        let Some(name) = terms.names.iter().find(|name| {
            edge.target_text == **name || edge.target_text.ends_with(&format!(".{name}"))
        }) else {
            continue;
        };
        let path = &edge.location.path;
        if confirmed.contains(&(path.clone(), edge.location.start_byte)) {
            continue;
        }
        // An identifier bound to a parameter or nested local is provably not
        // this top-level declaration. A member (`x.name`) with a local receiver
        // could still reach it at runtime, so it stays possible.
        let chain = &edge.evidence.binding_chain;
        let local = chain.len() == 1 && chain[0].ends_with(" => local ");
        if local && edge.target_text == *name {
            shadowed.insert((path.clone(), edge.location.start_byte));
            continue;
        }
        let unchanged =
            before.hashes.contains_key(path) && before.hashes.get(path) == after.hashes.get(path);
        let snapshot = if unchanged { "both" } else { label };
        found
            .entry((path.clone(), edge.location.start_byte, snapshot.to_owned()))
            .or_insert_with(|| Possible {
                reason: "unconfirmed_same_name".into(),
                snapshot: snapshot.to_owned(),
                path: path.clone(),
                line: Some(edge.location.start_line),
                kind: Some(kind.clone()),
                jsx: edge.evidence.syntax.ends_with("> JSX element"),
                status: Some(status_name(edge)),
                rule: Some(edge.evidence.rule.clone()),
                target_text: Some(edge.target_text.clone()),
                mentions: Vec::new(),
            });
    }
    for entry in unanalysed_mentions(before, after, &terms.tokens) {
        found.insert((entry.path.clone(), 0, entry.snapshot.clone()), entry);
    }
    (found.into_values().collect(), shadowed.len())
}

/// Files Atlas could not analyse (syntax errors, skipped sources) whose text
/// contains one of `tokens`. A file with identical bytes in both snapshots is
/// listed once (`both`).
fn unanalysed_mentions(
    before: &Catalog<'_>,
    after: &Catalog<'_>,
    tokens: &BTreeSet<String>,
) -> Vec<Possible> {
    let mut found: BTreeMap<(String, String), Possible> = BTreeMap::new();
    for catalog in [before, after] {
        for path in catalog.syntax_errors.iter().chain(&catalog.skipped) {
            let Ok(bytes) = fs::read(catalog.side.source.join(path)) else {
                catalog.problems.borrow_mut().insert(format!(
                    "{}: unanalysable file {path} could not be read for screening",
                    catalog.label
                ));
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let present = identifier_tokens(&text);
            let mentions: Vec<String> = tokens
                .iter()
                .filter(|t| present.contains(t.as_str()))
                .cloned()
                .collect();
            if mentions.is_empty() {
                continue;
            }
            let other = if catalog.label == "base" {
                after
            } else {
                before
            };
            let same = fs::read(other.side.source.join(path)).is_ok_and(|o| o == bytes);
            let snapshot = if same { "both" } else { catalog.label };
            found
                .entry((path.clone(), snapshot.to_owned()))
                .or_insert_with(|| Possible {
                    reason: "unanalysed_file".into(),
                    snapshot: snapshot.into(),
                    path: path.clone(),
                    line: None,
                    kind: None,
                    jsx: false,
                    status: None,
                    rule: Some(
                        if catalog.syntax_errors.contains(path) {
                            "syntax_error_file"
                        } else {
                            "skipped_file"
                        }
                        .into(),
                    ),
                    target_text: None,
                    mentions,
                });
        }
    }
    found.into_values().collect()
}

/// Cap every list at `limit`, recording what was dropped.
fn cap(report: &mut ImpactReport, limit: usize) {
    for impact in &mut report.impacts {
        let lengths = [
            ("direct", impact.direct.len()),
            ("import_only", impact.import_only.len()),
            ("re_exported_by", impact.re_exported_by.len()),
            ("transitive", impact.transitive.len()),
            ("possible", impact.possible.len()),
        ];
        for (name, length) in lengths {
            if length > limit {
                *impact.omitted.entry(name.into()).or_default() += length - limit;
            }
        }
        impact.direct.truncate(limit);
        impact.import_only.truncate(limit);
        impact.re_exported_by.truncate(limit);
        impact.transitive.truncate(limit);
        impact.possible.truncate(limit);
    }
    if report.rebound.len() > limit {
        report.rebound_omitted += report.rebound.len() - limit;
        report.rebound.truncate(limit);
    }
}

/// Files touched by the change or reached by its evidence, with the reasons.
fn affected_files(report: &ImpactReport) -> Vec<AffectedFile> {
    let mut affected: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for file in &report.files {
        if file.category == "source" {
            affected
                .entry(file.path.clone())
                .or_default()
                .insert("changed");
        }
    }
    for impact in &report.impacts {
        for site in &impact.direct {
            affected
                .entry(site.path.clone())
                .or_default()
                .insert("direct");
        }
        for site in &impact.import_only {
            affected
                .entry(site.path.clone())
                .or_default()
                .insert("import");
        }
        for dependent in &impact.transitive {
            affected
                .entry(dependent.path.clone())
                .or_default()
                .insert("transitive");
        }
        for possible in &impact.possible {
            affected
                .entry(possible.path.clone())
                .or_default()
                .insert("possible");
        }
    }
    for module in &report.modules {
        for importer in &module.importers {
            affected
                .entry(importer.clone())
                .or_default()
                .insert("importer");
        }
    }
    for site in &report.rebound {
        affected
            .entry(site.path.clone())
            .or_default()
            .insert("rebound");
    }
    affected
        .into_iter()
        .map(|(path, reasons)| AffectedFile {
            path,
            reasons: reasons.into_iter().map(str::to_owned).collect(),
        })
        .collect()
}

/// Why a report is incomplete, from truncation and per-declaration results.
fn incomplete_reasons(report: &ImpactReport) -> Vec<String> {
    let mut reasons: Vec<String> = Vec::new();
    if report.truncated {
        reasons.push("output truncated: see the omitted counts".into());
    }
    if report.declarations_not_analysed > 0 {
        reasons.push(format!(
            "{} changed declaration(s) not analysed",
            report.declarations_not_analysed
        ));
    }
    for impact in report.impacts.iter().filter(|i| !i.complete) {
        let mut why = Vec::new();
        if impact.limit_reached {
            why.push("work limit reached".to_owned());
        }
        let unconfirmed = impact
            .possible
            .iter()
            .filter(|p| p.reason == "unconfirmed_same_name")
            .count();
        let unanalysed = impact
            .possible
            .iter()
            .filter(|p| p.reason == "unanalysed_file")
            .count();
        if unconfirmed > 0 {
            why.push(format!("{unconfirmed} unconfirmed same-name site(s)"));
        }
        if unanalysed > 0 {
            why.push(format!("{unanalysed} unanalysable file(s) mention it"));
        }
        if !impact.omitted.is_empty() {
            why.push("lists capped".into());
        }
        reasons.push(format!(
            "{} {}: {}",
            impact.kind,
            impact.name,
            why.join(", ")
        ));
    }
    reasons
}

/// Affected files, completeness and truncation from the (possibly capped) lists.
pub fn finish(report: &mut ImpactReport) {
    report.affected_files = affected_files(report);
    for impact in &mut report.impacts {
        impact.complete =
            !impact.limit_reached && impact.omitted.is_empty() && impact.possible.is_empty();
    }
    report.truncated = !report.omitted.is_empty()
        || report.rebound_omitted > 0
        || report.affected_files_omitted > 0
        || report.declarations_not_analysed > 0
        || report.modules.iter().any(|m| m.importers_omitted > 0)
        || report.impacts.iter().any(|i| !i.omitted.is_empty());
    let mut reasons = incomplete_reasons(report);
    reasons.extend(report.limitations.iter().cloned());
    report.complete = reasons.is_empty();
    report.incomplete_reasons = reasons;
}

/// Keep the first `limit` entries of a top-level list, recording what was dropped.
fn shorten<T>(list: &mut Vec<T>, limit: usize, name: &str, omitted: &mut BTreeMap<String, usize>) {
    if list.len() > limit {
        *omitted.entry(name.to_owned()).or_default() += list.len() - limit;
        list.truncate(limit);
    }
}

/// Byte-budget stages, least important first: coverage detail, then secondary
/// impact lists, then the primary lists (changed files and declarations,
/// impacts and their direct consumers).
const STAGES: usize = 3;

fn longest(report: &ImpactReport, stage: usize) -> usize {
    let lengths: Vec<usize> = match stage {
        0 => [&report.coverage.base, &report.coverage.head]
            .iter()
            .flat_map(|c| {
                [
                    c.skipped_sample.len(),
                    c.syntax_error_files.len(),
                    c.diagnostics.len(),
                ]
            })
            .collect(),
        1 => report
            .impacts
            .iter()
            .flat_map(|i| {
                [
                    i.transitive.len(),
                    i.possible.len(),
                    i.import_only.len(),
                    i.re_exported_by.len(),
                ]
            })
            .chain(report.modules.iter().map(|m| m.importers.len()))
            .chain([report.rebound.len(), report.affected_files.len()])
            .collect(),
        _ => report
            .impacts
            .iter()
            .map(|i| i.direct.len())
            .chain([
                report.files.len(),
                report.declarations.len(),
                report.modules.len(),
                report.impacts.len(),
            ])
            .collect(),
    };
    lengths.into_iter().max().unwrap_or(0)
}

/// Cap the lists of one stage at `limit`, recording what was dropped.
fn shrink(view: &mut ImpactReport, stage: usize, limit: usize) {
    let omitted = &mut view.omitted;
    match stage {
        0 => {
            for (side, coverage) in [
                ("base", &mut view.coverage.base),
                ("head", &mut view.coverage.head),
            ] {
                shorten(
                    &mut coverage.skipped_sample,
                    limit,
                    &format!("coverage.{side}.skipped_sample"),
                    omitted,
                );
                shorten(
                    &mut coverage.syntax_error_files,
                    limit,
                    &format!("coverage.{side}.syntax_error_files"),
                    omitted,
                );
                shorten(
                    &mut coverage.diagnostics,
                    limit,
                    &format!("coverage.{side}.diagnostics"),
                    omitted,
                );
            }
        }
        1 => {
            for impact in &mut view.impacts {
                for (name, length) in [
                    ("import_only", impact.import_only.len()),
                    ("re_exported_by", impact.re_exported_by.len()),
                    ("transitive", impact.transitive.len()),
                    ("possible", impact.possible.len()),
                ] {
                    if length > limit {
                        *impact.omitted.entry(name.into()).or_default() += length - limit;
                    }
                }
                impact.import_only.truncate(limit);
                impact.re_exported_by.truncate(limit);
                impact.transitive.truncate(limit);
                impact.possible.truncate(limit);
            }
            for module in &mut view.modules {
                if module.importers.len() > limit {
                    module.importers_omitted += module.importers.len() - limit;
                    module.importers.truncate(limit);
                }
            }
            if view.rebound.len() > limit {
                view.rebound_omitted += view.rebound.len() - limit;
                view.rebound.truncate(limit);
            }
            if view.affected_files.len() > limit {
                view.affected_files_omitted += view.affected_files.len() - limit;
                view.affected_files.truncate(limit);
            }
        }
        _ => {
            for impact in &mut view.impacts {
                if impact.direct.len() > limit {
                    *impact.omitted.entry("direct".into()).or_default() +=
                        impact.direct.len() - limit;
                    impact.direct.truncate(limit);
                }
            }
            shorten(&mut view.files, limit, "files", omitted);
            shorten(&mut view.declarations, limit, "declarations", omitted);
            shorten(&mut view.modules, limit, "modules", omitted);
            shorten(&mut view.impacts, limit, "impacts", omitted);
        }
    }
}

/// Shrink lists until the rendering fits `max_bytes`, stage by stage (least
/// important first), recording what was dropped; a view that had to drop
/// anything is truncated and incomplete. Only fixed-size fields remain at the end.
pub fn fit(
    report: &ImpactReport,
    max_bytes: usize,
    render: impl Fn(&ImpactReport) -> String,
) -> (ImpactReport, String) {
    let text = render(report);
    if text.len() <= max_bytes {
        return (report.clone(), text);
    }
    let mut view = report.clone();
    let mut text = text;
    for stage in 0..STAGES {
        // Largest per-list limit for this stage that fits, by binary search.
        let (mut low, mut high) = (0, longest(&view, stage));
        let mut best: Option<(ImpactReport, String)> = None;
        while low < high {
            let limit = low + (high - low) / 2;
            let mut candidate = view.clone();
            shrink(&mut candidate, stage, limit);
            finish_keep_affected(&mut candidate);
            let rendered = render(&candidate);
            if rendered.len() <= max_bytes {
                best = Some((candidate, rendered));
                low = limit + 1;
            } else {
                high = limit;
            }
        }
        if let Some((candidate, rendered)) = best {
            return (candidate, rendered);
        }
        shrink(&mut view, stage, 0);
        finish_keep_affected(&mut view);
        text = render(&view);
    }
    (view, text)
}

/// Recompute completeness without rebuilding the (already capped) affected list.
fn finish_keep_affected(report: &mut ImpactReport) {
    let affected = std::mem::take(&mut report.affected_files);
    let omitted = report.affected_files_omitted;
    finish(report);
    report.affected_files = affected;
    report.affected_files_omitted = omitted;
    if omitted > 0 {
        report.truncated = true;
        report.complete = false;
    }
}

fn short(oid: Option<&str>) -> String {
    oid.map_or_else(String::new, |oid| oid[..oid.len().min(10)].to_owned())
}

fn side_label(info: &SnapshotInfo) -> String {
    match info.kind.as_str() {
        "worktree" => "working tree".into(),
        _ => format!(
            "{} ({})",
            info.spec.clone().unwrap_or_default(),
            short(info.commit.as_deref())
        ),
    }
}

fn site_line(site: &Site, detail: bool) -> String {
    let from = site.consumer.as_ref().map_or_else(
        || "at module scope".to_owned(),
        |c| format!("from {}", c.name),
    );
    let kind = if site.jsx {
        format!("{}(JSX)", site.kind)
    } else {
        site.kind.clone()
    };
    let snapshot = if site.snapshot == "both" {
        String::new()
    } else {
        format!(" [{}]", site.snapshot)
    };
    let mut line = format!("{}:{} {kind} {from}{snapshot}", site.path, site.line);
    if detail {
        let _ = write!(line, " · {}", site.rule);
        if !site.binding_chain.is_empty() {
            let _ = write!(line, " · {}", site.binding_chain.join(" → "));
        }
    }
    line
}

/// Concise text; `detail` adds transitive paths, import-only files, possible
/// sites, all rebound sites and evidence.
#[must_use]
pub fn render_text(report: &ImpactReport, detail: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "atlas impact · {} → {} · {} file(s), {} declaration(s) changed · {}{}",
        side_label(&report.base),
        side_label(&report.head),
        report.files.len(),
        report.declarations.len(),
        if report.complete {
            "complete within the analysed static graph"
        } else {
            "INCOMPLETE"
        },
        if report.truncated {
            " · truncated"
        } else {
            ""
        }
    );
    render_files(&mut out, report);
    render_declarations(&mut out, report);
    for impact in &report.impacts {
        render_impact(&mut out, impact, detail);
    }
    render_rebound(&mut out, report, detail);
    render_footer(&mut out, report);
    out
}

fn render_files(out: &mut String, report: &ImpactReport) {
    if report.files.is_empty() {
        return;
    }
    let _ = writeln!(out, "\nchanged files:");
    for file in &report.files {
        let status = match file.status.as_str() {
            "added" => "A",
            "removed" => "D",
            "renamed" => "R",
            _ => "M",
        };
        let from = file
            .previous_path
            .as_ref()
            .map(|p| format!(" ← {p}"))
            .unwrap_or_default();
        let state = file
            .git_state
            .as_ref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        let category = if file.category == "source" {
            String::new()
        } else {
            format!(" [{}]", file.category)
        };
        let _ = writeln!(out, "  {status} {}{from}{category}{state}", file.path);
    }
}

fn render_declarations(out: &mut String, report: &ImpactReport) {
    if !report.declarations.is_empty() {
        let _ = writeln!(out, "\nchanged declarations:");
    }
    for change in &report.declarations {
        let mut flags = Vec::new();
        if change.signature_changed == Some(true) {
            flags.push("signature");
        }
        if change.body_changed == Some(true) {
            flags.push("body");
        }
        if change.export_changed {
            flags.push("exports");
        }
        if change.whitespace_only {
            flags.push("whitespace only");
        }
        if change.uncertain {
            flags.push("identity uncertain: paired by position");
        }
        let line = change
            .head
            .as_ref()
            .or(change.base.as_ref())
            .map_or(0, |d| d.start_line);
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };
        let _ = writeln!(
            out,
            "  {:<8} {} {}  {}:{line}{flags}",
            change.change, change.kind, change.name, change.path
        );
    }
    for module in report.modules.iter().filter(|m| m.change == "modified") {
        let mut parts = Vec::new();
        if module.imports_changed {
            parts.push("imports");
        }
        if module.exports_changed {
            parts.push("export clauses");
        }
        if module.statements_changed {
            parts.push("top-level code");
        }
        let _ = writeln!(
            out,
            "  module   {}: {} changed · {} importer(s){}",
            module.path,
            parts.join(", "),
            module.importers.len() + module.importers_omitted,
            if module.importers_omitted > 0 {
                " (truncated)"
            } else {
                ""
            }
        );
    }
}

fn render_impact(out: &mut String, impact: &SymbolImpact, detail: bool) {
    let omitted = |name: &str| impact.omitted.get(name).copied().unwrap_or(0);
    let _ = writeln!(
        out,
        "\n{} {} ({}) — {} direct, {} import-only, {} transitive, {} possible · {}",
        impact.kind,
        impact.name,
        impact.change,
        impact.direct.len() + omitted("direct"),
        impact.import_only.len() + omitted("import_only"),
        impact.transitive.len() + omitted("transitive"),
        impact.possible.len() + omitted("possible"),
        if impact.complete {
            "complete"
        } else {
            "incomplete"
        }
    );
    for site in &impact.direct {
        let _ = writeln!(out, "  {}", site_line(site, detail));
    }
    if impact.direct.is_empty() && omitted("direct") == 0 && impact.change != "added" {
        let _ = writeln!(
            out,
            "  no confirmed consumers in the analysed graph (not proof of safety: JavaScript, packages and dynamic uses are not analysed)"
        );
    }
    if omitted("direct") > 0 {
        let _ = writeln!(out, "  … {} more direct site(s) omitted", omitted("direct"));
    }
    if detail {
        for site in &impact.import_only {
            let _ = writeln!(out, "  import only: {}", site_line(site, detail));
        }
        for site in &impact.re_exported_by {
            let _ = writeln!(out, "  re-exported: {}", site_line(site, detail));
        }
        for dependent in &impact.transitive {
            let _ = writeln!(
                out,
                "  transitive d{}: {} {}  {}:{}  via {}",
                dependent.depth,
                dependent.kind,
                dependent.name,
                dependent.path,
                dependent.line,
                dependent.via.join(" ← ")
            );
        }
    }
    let unconfirmed = impact
        .possible
        .iter()
        .filter(|p| p.reason == "unconfirmed_same_name")
        .count();
    for possible in &impact.possible {
        if detail || possible.reason != "unconfirmed_same_name" {
            let _ = writeln!(out, "  possible: {}", possible_line(possible));
        }
    }
    if !detail && unconfirmed > 0 {
        let _ = writeln!(
            out,
            "  possible: {unconfirmed} unconfirmed same-name site(s) (--detail lists them)"
        );
    }
    if impact.limit_reached {
        let _ = writeln!(out, "  work limit reached: results are partial");
    }
}

fn render_rebound(out: &mut String, report: &ImpactReport, detail: bool) {
    if report.rebound.is_empty() {
        return;
    }
    let total = report.rebound.len() + report.rebound_omitted;
    let _ = writeln!(
        out,
        "\nbinding changes in unchanged files (effect: retargeted, gained, lost = no longer confirmed, not necessarily broken): {total}"
    );
    let shown = if detail {
        report.rebound.len()
    } else {
        report.rebound.len().min(10)
    };
    for site in &report.rebound[..shown] {
        let _ = writeln!(
            out,
            "  {}:{} {}{} {} [{}]: {} → {}",
            site.path,
            site.line,
            site.kind,
            if site.jsx { "(JSX)" } else { "" },
            site.target_text,
            site.effect,
            binding_text(site.before.as_ref()),
            binding_text(site.after.as_ref())
        );
    }
    if shown < total {
        let _ = writeln!(out, "  … {} more (--detail lists them)", total - shown);
    }
}

fn render_footer(out: &mut String, report: &ImpactReport) {
    let _ = writeln!(
        out,
        "\naffected files: {}",
        report.affected_files.len() + report.affected_files_omitted
    );
    for file in &report.affected_files {
        let _ = writeln!(out, "  {} ({})", file.path, file.reasons.join(", "));
    }
    if report.affected_files_omitted > 0 {
        let _ = writeln!(out, "  … {} more omitted", report.affected_files_omitted);
    }
    let unanalysed: BTreeSet<&String> = report
        .coverage
        .head
        .syntax_error_files
        .iter()
        .chain(&report.coverage.base.syntax_error_files)
        .collect();
    let mut warnings = report.warnings.clone();
    if !unanalysed.is_empty() {
        warnings.push(format!(
            "{} file(s) with syntax errors have no semantic edges",
            unanalysed.len()
        ));
    }
    if !warnings.is_empty() {
        let _ = writeln!(out, "\nwarnings:");
        for warning in warnings {
            let _ = writeln!(out, "  {warning}");
        }
    }
    if !report.incomplete_reasons.is_empty() {
        let _ = writeln!(out, "\nincomplete because:");
        for reason in &report.incomplete_reasons {
            let _ = writeln!(out, "  {reason}");
        }
    }
    let _ = writeln!(
        out,
        "\nscope: static TypeScript/TSX graph; a dependency is not a break; JavaScript, packages and runtime behaviour are not analysed"
    );
}

fn binding_text(binding: Option<&Binding>) -> String {
    binding.map_or_else(
        || "absent".to_owned(),
        |b| match &b.target {
            Some(target) if target.kind == "file" => format!("{} {}", b.status, target.path),
            Some(target) => format!("{} {}#{}", b.status, target.path, target.name),
            None => format!("{} ({})", b.status, b.rule),
        },
    )
}

fn possible_line(possible: &Possible) -> String {
    match possible.reason.as_str() {
        "unanalysed_file" => format!(
            "{} could not be analysed ({}); mentions {}",
            possible.path,
            possible.rule.clone().unwrap_or_default(),
            possible.mentions.join(", ")
        ),
        _ => format!(
            "{}:{} {}{} \"{}\" {} ({})",
            possible.path,
            possible.line.unwrap_or(0),
            possible.kind.clone().unwrap_or_default(),
            if possible.jsx { "(JSX)" } else { "" },
            possible.target_text.clone().unwrap_or_default(),
            possible.status.clone().unwrap_or_default(),
            possible.rule.clone().unwrap_or_default()
        ),
    }
}

/// One comparison request: a Git repository (or a directory inside one), a
/// base revision and either a head revision or the working tree.
#[derive(Debug, Clone)]
pub struct Request {
    pub root: std::path::PathBuf,
    pub base: String,
    /// `None` compares against the working tree.
    pub head: Option<String>,
    pub cache_dir: Option<std::path::PathBuf>,
    pub options: Options,
    /// `root` must be the Git work-tree top level and repository discovery may
    /// not look above it (the MCP configured-root boundary).
    pub confined: bool,
}

/// Build or reuse both snapshots, find the file-level changes and analyse them.
/// Read-only for the repository: writes only the impact cache.
pub fn compare(request: &Request) -> Result<ImpactReport> {
    compare_inner(request, true, |_, _, _, _| Ok(())).map(|(report, ())| report)
}

/// Same analysis and traversal limits, retaining sites until file grouping or
/// evidence selection. Legacy reports still use their original per-list caps.
pub fn compare_for_view(request: &Request) -> Result<ImpactReport> {
    compare_inner(request, false, |_, _, _, _| Ok(())).map(|(report, ())| report)
}

/// Inspect the same stores/sources while they are open, before final working
/// tree freshness verification. No second snapshot build or symbol-ID transfer.
pub fn inspect_comparison<T>(
    request: &Request,
    inspect: impl FnOnce(&code_intel_index::git::Git, &Side<'_>, &Side<'_>, &ImpactReport) -> Result<T>,
) -> Result<(ImpactReport, T)> {
    compare_inner(request, false, inspect)
}

fn compare_inner<T>(
    request: &Request,
    cap_sites: bool,
    inspect: impl FnOnce(&code_intel_index::git::Git, &Side<'_>, &Side<'_>, &ImpactReport) -> Result<T>,
) -> Result<(ImpactReport, T)> {
    use code_intel_index::{
        git::Git,
        snapshot::{Cache, Head, ensure_inside, file_comparison},
    };
    let started = std::time::Instant::now();
    let git = if request.confined {
        Git::open_confined(&request.root)?
    } else {
        Git::open(&request.root)?
    };
    ensure_inside(&git)?;
    let cache = Cache::new(&git.root, request.cache_dir.as_deref())?;
    let base_revision = git.revision(&request.base)?;
    let user_index = git.root.join(".code-intel/index.db");
    let (base, head, head_revision) = match &request.head {
        None => {
            let head = cache.worktree(&git.root)?;
            let base = cache.commit(&git, &base_revision, Some(&head.database))?;
            (base, head, None)
        }
        Some(spec) => {
            let head_revision = git.revision(spec)?;
            let seed = user_index.is_file().then_some(user_index.as_path());
            let base = cache.commit(&git, &base_revision, seed)?;
            let head = cache.commit(&git, &head_revision, Some(&base.database))?;
            (base, head, Some(head_revision))
        }
    };
    let head_kind = head_revision.as_ref().map_or(Head::Worktree, Head::Commit);
    let snapshots = started.elapsed();
    let file_comparison =
        file_comparison(&git, &base_revision, &base.source, &head_kind, &head.source)?;
    code_intel_store::profile(&[
        ("impact_snapshots", snapshots),
        (
            "impact_file_changes",
            started.elapsed().saturating_sub(snapshots),
        ),
    ]);
    // Test hook: lets a test edit the working tree mid-analysis. The optional
    // marker file (outside the repository) tells the test the pause has begun.
    if let Some(pause) = std::env::var("CODE_INTEL_TEST_PAUSE_AFTER_SNAPSHOTS_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        if let Some(marker) = std::env::var_os("CODE_INTEL_TEST_PAUSE_MARKER") {
            let _ = std::fs::write(marker, b"");
        }
        std::thread::sleep(std::time::Duration::from_millis(pause.min(10_000)));
    }
    let base_store = Store::open_read(&base.database)?;
    let head_store = Store::open_read(&head.database)?;
    let mut report = analyze_inner(
        &Side {
            store: &base_store,
            info: &base.info,
            source: &base.source,
        },
        &Side {
            store: &head_store,
            info: &head.info,
            source: &head.source,
        },
        file_comparison.changes,
        &request.options,
        cap_sites,
    )?;
    report.input_digests = Some((
        file_comparison.base_files_digest,
        file_comparison.head_files_digest,
    ));
    let inspected = inspect(
        &git,
        &Side {
            store: &base_store,
            info: &base.info,
            source: &base.source,
        },
        &Side {
            store: &head_store,
            info: &head.info,
            source: &head.source,
        },
        &report,
    )?;
    if head_revision.is_none() {
        // The working tree is live: confirm the analysed snapshot still matches it.
        let now = code_intel_index::freshness(&git.root, &head_store.hashes()?, 3)?;
        if !now.fresh {
            report.limitations.push(format!(
                "the working tree changed during analysis ({}); results describe the indexed state",
                now.sample.join(", ")
            ));
        }
    }
    report.limitations.extend(git_limitations(
        &git,
        &report.files,
        head_revision.is_none(),
    )?);
    finish(&mut report);
    Ok((report, inspected))
}

/// Git conditions Atlas does not model, as completeness-blocking limitations:
/// content filters (clean/smudge, e.g. LFS) and line-ending conversion on
/// changed files, and submodules whose working-tree files the indexer reads
/// while commit snapshots omit them. Blobs and files are always compared raw.
fn git_limitations(
    git: &code_intel_index::git::Git,
    files: &[FileChange],
    worktree: bool,
) -> Result<Vec<String>> {
    let mut limitations = Vec::new();
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let autocrlf = git
        .config("core.autocrlf")
        .is_some_and(|value| matches!(value.as_str(), "true" | "input"));
    let attributes = git.attributes(&paths)?;
    let mut filtered = Vec::new();
    let mut converted = Vec::new();
    for file in files {
        let attributes = attributes
            .get(&file.path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if attributes.iter().any(|(name, _)| name == "filter") {
            filtered.push(file.path.clone());
            continue;
        }
        let conversion = autocrlf
            || attributes
                .iter()
                .any(|(name, value)| name == "eol" || (name == "text" && value != "unset"));
        // Only a working-tree file with CRLF can differ from its blob by conversion.
        let crlf = worktree
            && fs::read(git.root.join(&file.path))
                .is_ok_and(|bytes| bytes.windows(2).any(|w| w == b"\r\n"));
        if conversion && crlf {
            converted.push(file.path.clone());
        }
    }
    if !filtered.is_empty() {
        limitations.push(format!(
            "content filter attribute on {}: blobs and files are compared raw, without clean/smudge filters",
            filtered.join(", ")
        ));
    }
    if !converted.is_empty() {
        limitations.push(format!(
            "line-ending conversion applies to {}: files are compared raw, so they may differ only in line endings",
            converted.join(", ")
        ));
    }
    if worktree {
        let submodules: Vec<String> = git
            .index_entries()?
            .into_iter()
            .filter(|(_, entry)| entry.mode == "160000")
            .map(|(path, _)| path)
            .collect();
        if !submodules.is_empty() {
            limitations.push(format!(
                "submodule {}: the working-tree index reads its files but commit snapshots omit it",
                submodules.join(", ")
            ));
        }
    }
    Ok(limitations)
}
