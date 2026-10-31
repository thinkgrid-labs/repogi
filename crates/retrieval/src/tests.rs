//! Read-only test discovery: project impact, then query terminal test edges.
//! No runner, configuration evaluation, independent resolver or persisted schema.
mod evidence;
mod inventory;
mod output;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use code_intel_index::snapshot::SnapshotInfo;
use serde::Serialize;

use crate::impact::{self, DeclKey, ImpactReport, Request};
pub use inventory::{Configuration, TestFile};

#[derive(Clone, Debug, Serialize)]
pub struct Evidence {
    pub snapshot: String,
    pub changed: DeclKey,
    pub relationship: String,
    pub depth: usize,
    pub line: usize,
    pub via: Vec<String>,
    /// Exact terminal target in the named snapshot, never an ID from another.
    pub target: DeclKey,
    pub rule: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub binding_chain: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub path: String,
    pub snapshots: BTreeMap<String, String>,
    pub priority: String,
    pub reasons: BTreeSet<String>,
    pub linked_changes: BTreeSet<DeclKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Gap {
    pub path: String,
    pub declaration: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Changed {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub change: String,
}

// These are independent coverage axes, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Serialize)]
pub struct Completeness {
    pub direct_files_complete: bool,
    pub traversal_complete: bool,
    pub snapshots_fresh: bool,
    pub inventory_complete: bool,
    pub static_links_complete: bool,
    pub possible_unverified: bool,
    pub source_coverage_unknown: bool,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub kind: String,
    pub base: SnapshotInfo,
    pub head: SnapshotInfo,
    pub base_inventory_digest: String,
    pub head_inventory_digest: String,
    pub depth: usize,
    pub scope: Option<String>,
    pub changed: Vec<Changed>,
    pub confirmed: Vec<Candidate>,
    pub possible: Vec<Candidate>,
    pub unknown: Vec<TestFile>,
    pub gaps: Vec<Gap>,
    pub inventory: Vec<TestFile>,
    pub inventory_included: bool,
    pub inventory_counts: BTreeMap<String, usize>,
    pub configurations: Vec<Configuration>,
    pub limitations: Vec<String>,
    pub completeness: Completeness,
    pub analysis_limited: bool,
    pub truncated: bool,
    pub totals: BTreeMap<String, usize>,
    pub omitted: BTreeMap<String, usize>,
    pub excluded_by_selection: BTreeMap<String, usize>,
}

/// The literal filename and `__tests__` conventions that discovery classifies.
/// No runner configuration is evaluated, so membership stays advisory.
pub(crate) fn convention(path: &str) -> Option<&'static str> {
    if inventory::named_test(path) {
        Some("test/spec filename convention")
    } else if path.split('/').any(|part| part == "__tests__") {
        Some("__tests__ directory convention; runner membership unverified")
    } else {
        None
    }
}

pub struct Options {
    /// Literal repository-relative directories, no executable config or globs.
    pub directories: Vec<String>,
}

/// One comparison, two catalog scans and one streamed test-edge pass per side.
pub fn compare(request: &Request, options: &Options) -> Result<Report> {
    compare_with_impact(request, options).map(|comparison| comparison.tests)
}

/// Both projections share one comparison and its final freshness verification.
pub(crate) struct Comparison {
    pub impact: ImpactReport,
    pub tests: Report,
    pub base_resolution: Option<String>,
    pub head_resolution: Option<String>,
}

pub(crate) fn compare_with_impact(request: &Request, options: &Options) -> Result<Comparison> {
    inventory::validate_directories(&options.directories)?;
    let (
        impact,
        (
            catalog,
            confirmed,
            possible,
            base_digest,
            head_digest,
            catalog_fresh,
            base_resolution,
            head_resolution,
        ),
    ) = impact::inspect_comparison(request, |git, base, head, report| {
        let started = std::time::Instant::now();
        let before = inventory::collect(git, base, "base", &options.directories)?;
        let after = inventory::collect(git, head, "head", &options.directories)?;
        let base_digest = before.digest.clone();
        let head_digest = after.digest.clone();
        let catalog = inventory::merge(before, after);
        let (confirmed, possible) =
            evidence::discover(base, head, report, &catalog.files, request.options.depth)?;
        let catalog_fresh = head.info.kind != "worktree"
            || inventory::collect(git, head, "head", &options.directories)?.digest == head_digest;
        code_intel_store::profile(&[("test_discovery", started.elapsed())]);
        Ok((
            catalog,
            confirmed,
            possible,
            base_digest,
            head_digest,
            catalog_fresh,
            base.store.resolution_fingerprint()?,
            head.store.resolution_fingerprint()?,
        ))
    })?;
    let tests = plan(
        &impact,
        catalog,
        confirmed,
        possible,
        base_digest,
        head_digest,
        catalog_fresh,
        request.options.depth,
    );
    Ok(Comparison {
        impact,
        tests,
        base_resolution,
        head_resolution,
    })
}

#[allow(clippy::too_many_arguments)]
fn plan(
    impact: &ImpactReport,
    catalog: inventory::Catalog,
    confirmed: Vec<Candidate>,
    possible: Vec<Candidate>,
    base_digest: String,
    head_digest: String,
    catalog_fresh: bool,
    depth: usize,
) -> Report {
    let summary = impact::summary::project(impact, depth);
    let inventory: Vec<_> = catalog.files.into_values().collect();
    let unknown: Vec<_> = inventory
        .iter()
        .filter(|f| {
            f.snapshots
                .values()
                .any(|c| c == "recognized_unindexed" || c == "unsupported_or_excluded")
        })
        .cloned()
        .collect();
    let mut inventory_counts = BTreeMap::new();
    for file in &inventory {
        for class in file.snapshots.values().collect::<BTreeSet<_>>() {
            *inventory_counts.entry(class.clone()).or_default() += 1;
        }
    }
    let gaps = gaps(impact, &confirmed);
    let inventory_complete = unknown.is_empty()
        && catalog_fresh
        && !catalog.limited
        && catalog.configurations.iter().all(|c| c.interpreted);
    let snapshots_fresh = summary.snapshots_fresh && catalog_fresh;
    let static_links_complete = summary.traversal_complete
        && snapshots_fresh
        && inventory_complete
        && !summary.analysis_limited
        && summary.coverage_diagnostics == 0
        && summary.unanalysable_files == 0;
    let completeness = Completeness {
        direct_files_complete: summary.direct_files_complete,
        traversal_complete: summary.traversal_complete,
        snapshots_fresh,
        inventory_complete,
        static_links_complete,
        possible_unverified: summary.possible_unverified || !possible.is_empty(),
        source_coverage_unknown: catalog.limited
            || summary.unanalysable_files > 0
            || !unknown.is_empty()
            || summary.coverage_diagnostics > 0,
        complete: static_links_complete && possible.is_empty() && !summary.possible_unverified,
    };
    let mut limitations: BTreeSet<_> = catalog.limitations;
    limitations.extend(impact.limitations.iter().cloned());
    if !catalog_fresh {
        limitations.insert("test inventory/configuration changed during discovery".into());
    }
    limitations.extend(impact.coverage.not_analysed.iter().cloned());
    limitations.insert("linked tests are advisory static dependencies; they may pass or fail for unrelated reasons; unlisted tests may still be necessary".into());
    let mut report = Report {
        schema_version: 1,
        kind: "affected_tests".into(),
        base: impact.base.clone(),
        head: impact.head.clone(),
        base_inventory_digest: base_digest,
        head_inventory_digest: head_digest,
        depth,
        scope: None,
        changed: impact
            .declarations
            .iter()
            .map(|d| Changed {
                path: d.path.clone(),
                name: d.name.clone(),
                kind: d.kind.clone(),
                change: d.change.clone(),
            })
            .collect(),
        confirmed,
        possible,
        unknown,
        gaps,
        inventory,
        inventory_included: true,
        inventory_counts,
        configurations: catalog.configurations,
        limitations: limitations.into_iter().collect(),
        completeness,
        analysis_limited: summary.analysis_limited,
        truncated: false,
        totals: BTreeMap::new(),
        omitted: BTreeMap::new(),
        excluded_by_selection: BTreeMap::new(),
    };
    report.totals = BTreeMap::from([
        ("confirmed".into(), report.confirmed.len()),
        ("possible".into(), report.possible.len()),
        ("unknown".into(), report.unknown.len()),
        ("gaps".into(), report.gaps.len()),
        ("changed".into(), report.changed.len()),
        ("inventory".into(), report.inventory.len()),
        ("configurations".into(), report.configurations.len()),
        ("limitations".into(), report.limitations.len()),
    ]);
    report
}

fn gaps(impact: &ImpactReport, confirmed: &[Candidate]) -> Vec<Gap> {
    let linked: BTreeSet<_> = confirmed
        .iter()
        .flat_map(|c| c.linked_changes.iter().map(|d| (&d.path, &d.name, &d.kind)))
        .collect();
    let mut gaps: Vec<Gap> = impact.declarations.iter().filter(|d| !linked.contains(&(&d.path, &d.name, &d.kind)))
        .map(|d| Gap { path: d.path.clone(), declaration: d.name.clone(),
            reason: "no confirmed test linkage found at the requested depth; behavioral coverage is unknown".into() }).collect();
    if confirmed.is_empty() {
        for file in &impact.files {
            if !impact.declarations.iter().any(|d| d.path == file.path)
                && (impact.modules.iter().any(|m| m.path == file.path)
                    || file.category == "config"
                    || file.status == "renamed")
            {
                gaps.push(Gap { path: file.path.clone(), declaration: String::new(), reason: "module/configuration change without confirmed test linkage; inspect binding and possible candidates".into() });
            }
        }
    }
    gaps
}

pub fn respond(
    request: &Request,
    options: &Options,
    selected: Option<&str>,
    json: bool,
    max_bytes: usize,
) -> Result<String> {
    output::respond(request, options, selected, json, max_bytes)
}
