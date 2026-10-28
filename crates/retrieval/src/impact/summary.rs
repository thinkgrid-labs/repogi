//! File projection of the existing graph; never resolves or upgrades an edge.
use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Result, ensure};
use serde::Serialize;

use super::{ImpactReport, Request};

#[derive(Clone, Default, Serialize)]
pub struct File {
    pub path: String,
    /// Counts of evidence entries, not distinct runtime effects.
    pub categories: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub declarations: Vec<String>,
}

// Independent completeness axes are deliberately explicit in the wire format.
#[allow(clippy::struct_excessive_bools)]
#[derive(Serialize)]
pub struct Summary {
    pub schema_version: u32,
    pub base_digest: String,
    pub head_digest: String,
    pub depth: usize,
    pub direct_files_complete: bool,
    pub traversal_complete: bool,
    pub snapshots_fresh: bool,
    pub possible_unverified: bool,
    pub unanalysable_files: usize,
    pub coverage_diagnostics: usize,
    pub analysis_limited: bool,
    pub complete: bool,
    pub truncated: bool,
    pub total_files: usize,
    pub omitted_files: usize,
    /// Category counts are distinct files; a file can have multiple categories.
    pub total_by_category: BTreeMap<String, usize>,
    pub omitted_by_category: BTreeMap<String, usize>,
    pub unsupported: Vec<String>,
    pub files: Vec<File>,
}

fn row<'a>(rows: &'a mut BTreeMap<String, File>, path: &str, category: &str) -> &'a mut File {
    let file = rows.entry(path.into()).or_insert_with(|| File {
        path: path.into(),
        ..File::default()
    });
    *file.categories.entry(category.into()).or_default() += 1;
    file
}

fn files(report: &ImpactReport) -> Vec<File> {
    let mut rows = BTreeMap::new();
    for file in &report.files {
        row(&mut rows, &file.path, "changed");
    }
    for declaration in &report.declarations {
        row(&mut rows, &declaration.path, "declaration")
            .declarations
            .push(format!(
                "{} {} {}{}",
                declaration.change,
                declaration.kind,
                declaration.name,
                if declaration.uncertain {
                    " (uncertain identity)"
                } else {
                    ""
                }
            ));
    }
    for impact in &report.impacts {
        for site in &impact.direct {
            row(
                &mut rows,
                &site.path,
                if site.jsx {
                    "direct_jsx"
                } else {
                    match site.kind.as_str() {
                        "CALLS" => "direct_call",
                        "EXTENDS" => "direct_extends",
                        _ => "direct_reference",
                    }
                },
            );
        }
        for site in &impact.import_only {
            row(&mut rows, &site.path, "import_only");
        }
        for site in &impact.re_exported_by {
            row(&mut rows, &site.path, "re_export");
        }
        for site in &impact.transitive {
            row(&mut rows, &site.path, "transitive");
        }
        for site in &impact.possible {
            row(&mut rows, &site.path, "possible");
        }
    }
    for module in &report.modules {
        for path in &module.importers {
            row(&mut rows, path, "module_importer");
        }
    }
    for binding in &report.rebound {
        row(
            &mut rows,
            &binding.path,
            &format!("binding_{}", binding.effect),
        );
    }
    let coverage = [&report.coverage.base, &report.coverage.head];
    for side in coverage {
        for path in &side.syntax_error_files {
            row(&mut rows, path, "unanalysable");
        }
    }
    rows.into_values().collect()
}

#[must_use]
pub fn project(report: &ImpactReport, depth: usize) -> Summary {
    let coverage = [&report.coverage.base, &report.coverage.head];
    // Baseline/head counts deliberately include both snapshots. Skipped samples
    // are not promoted to an exhaustive list of unknown files.
    let unanalysable_files = coverage
        .iter()
        .map(|s| s.syntax_error_files.len() + s.skipped_files)
        .sum();
    let coverage_diagnostics = coverage.iter().map(|s| s.diagnostics.len()).sum();
    let analysis_limited = report.truncated
        || report.declarations_not_analysed > 0
        || report.impacts.iter().any(|i| i.limit_reached)
        || report.modules.iter().any(|m| m.importers_omitted > 0);
    // Limitations include freshness failures and unverifiable source. Be
    // conservative rather than inferring freshness from a cache hit.
    let snapshots_fresh = report.limitations.is_empty();
    let known = !analysis_limited
        && snapshots_fresh
        && unanalysable_files == 0
        && coverage_diagnostics == 0;
    let possible_unverified = report.impacts.iter().any(|i| !i.possible.is_empty())
        || report.rebound.iter().any(|b| b.effect == "lost");
    let files = files(report);
    let mut total_by_category = BTreeMap::new();
    for file in &files {
        for category in file.categories.keys() {
            *total_by_category.entry(category.clone()).or_default() += 1;
        }
        if file.categories.keys().any(|c| c.starts_with("direct_")) {
            *total_by_category.entry("direct".into()).or_default() += 1;
        }
    }
    Summary {
        schema_version: 1,
        base_digest: report.base.digest.clone(),
        head_digest: report.head.digest.clone(),
        depth,
        direct_files_complete: known,
        traversal_complete: !analysis_limited,
        snapshots_fresh,
        possible_unverified,
        unanalysable_files,
        coverage_diagnostics,
        analysis_limited,
        complete: known && !possible_unverified,
        truncated: false,
        total_files: files.len(),
        omitted_files: 0,
        total_by_category,
        omitted_by_category: BTreeMap::new(),
        unsupported: report.coverage.not_analysed.clone(),
        files,
    }
}

impl Summary {
    fn retain(&mut self, count: usize) {
        for file in self.files.drain(count..) {
            self.omitted_files += 1;
            if file.categories.keys().any(|c| c.starts_with("direct_")) {
                *self.omitted_by_category.entry("direct".into()).or_default() += 1;
            }
            for category in file.categories.keys() {
                *self
                    .omitted_by_category
                    .entry(category.clone())
                    .or_default() += 1;
            }
        }
        self.truncated = self.omitted_files > 0;
        if self.truncated {
            self.complete = false;
            self.direct_files_complete = false;
        }
    }

    #[must_use]
    pub fn text(&self) -> String {
        let mut out = format!(
            "atlas impact summary · {}{}\n",
            if self.complete {
                "complete within supported static profile"
            } else {
                "incomplete"
            },
            if self.truncated { " · truncated" } else { "" }
        );
        let _ = writeln!(
            out,
            "snapshots: {} -> {}",
            self.base_digest, self.head_digest
        );
        let _ = writeln!(
            out,
            "direct_files_complete={} traversal_complete={} depth={} snapshots_fresh={}",
            self.direct_files_complete, self.traversal_complete, self.depth, self.snapshots_fresh
        );
        let _ = writeln!(
            out,
            "possible_unverified={} unanalysable_files={} coverage_diagnostics={} analysis_limited={}",
            self.possible_unverified,
            self.unanalysable_files,
            self.coverage_diagnostics,
            self.analysis_limited
        );
        let _ = writeln!(
            out,
            "files: shown={} total={} omitted={}",
            self.files.len(),
            self.total_files,
            self.omitted_files
        );
        let counts = |values: &BTreeMap<String, usize>| {
            values
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let _ = writeln!(
            out,
            "total_by_category: {}",
            counts(&self.total_by_category)
        );
        let _ = writeln!(
            out,
            "omitted_by_category: {}",
            counts(&self.omitted_by_category)
        );
        for file in &self.files {
            let _ = writeln!(
                out,
                "{} | {}{}",
                file.path,
                counts(&file.categories),
                if file.declarations.is_empty() {
                    String::new()
                } else {
                    format!(" | {}", file.declarations.join("; "))
                }
            );
        }
        let _ = writeln!(out, "unsupported: {}", self.unsupported.join("; "));
        out.push_str("Static dependencies do not establish runtime safety or compilation errors. Possible sites and lost bindings need verification.\nEvidence: repeat with summary=false, detail=true, evidence_file=<path> (CLI: --detail --evidence-file <path>); JSON: format=json.\n");
        out
    }
}

/// Shared CLI/MCP path. Defaults continue through the original renderer/fitter.
pub fn respond(
    request: &Request,
    summary: bool,
    evidence_file: Option<&str>,
    json: bool,
    max_bytes: usize,
) -> Result<String> {
    ensure!(
        !(summary && evidence_file.is_some()),
        "summary and evidence_file cannot be combined"
    );
    if let Some(path) = evidence_file {
        ensure!(
            !path.is_empty()
                && !path.contains('\\')
                && !path.contains('\n')
                && !std::path::Path::new(path).is_absolute()
                && std::path::Path::new(path)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "evidence_file must be a repository-relative file path"
        );
    }
    let mut report = if summary || evidence_file.is_some() {
        super::compare_for_view(request)?
    } else {
        super::compare(request)?
    };
    let render_started = std::time::Instant::now();
    if summary {
        let mut view = project(&report, request.options.depth);
        view.retain(view.files.len().min(request.options.max_results));
        loop {
            let text = if json {
                format!("{}\n", serde_json::to_string(&view)?)
            } else {
                view.text()
            };
            if text.len() <= max_bytes {
                code_intel_store::profile(&[("impact_render", render_started.elapsed())]);
                return Ok(text);
            }
            ensure!(
                !view.files.is_empty(),
                "summary metadata exceeds max_bytes; increase the budget"
            );
            view.retain(view.files.len() - 1);
        }
    }
    if let Some(path) = evidence_file {
        // Select before fitting, so unrelated sites cannot consume the budget.
        // Preserve global coverage/limitations and explicitly label the scope.
        for impact in &mut report.impacts {
            impact.direct.retain(|s| s.path == path);
            impact.import_only.retain(|s| s.path == path);
            impact.re_exported_by.retain(|s| s.path == path);
            impact.transitive.retain(|s| s.path == path);
            impact.possible.retain(|s| s.path == path);
        }
        report.impacts.retain(|i| {
            i.path == path
                || !i.direct.is_empty()
                || !i.import_only.is_empty()
                || !i.re_exported_by.is_empty()
                || !i.transitive.is_empty()
                || !i.possible.is_empty()
        });
        report.declarations.retain(|d| {
            d.path == path
                || report
                    .impacts
                    .iter()
                    .any(|i| i.path == d.path && i.name == d.name && i.kind == d.kind)
        });
        report
            .files
            .retain(|f| f.path == path || report.declarations.iter().any(|d| d.path == f.path));
        report.rebound.retain(|s| s.path == path);
        for module in &mut report.modules {
            module.importers.retain(|p| p == path);
        }
        report.affected_files.retain(|f| f.path == path);
        // Global completeness must never be inferred from the selected subset.
        report.limitations.push(format!("evidence selection only: {path}; other files intentionally excluded; request summary for file totals"));
        super::cap(&mut report, request.options.max_results);
        super::finish(&mut report);
    }
    let (_, text) = super::fit(&report, max_bytes, |view| {
        if json {
            format!(
                "{}\n",
                serde_json::to_string(view).expect("serializable impact report")
            )
        } else {
            super::render_text(view, request.options.detail || evidence_file.is_some())
        }
    });
    code_intel_store::profile(&[("impact_render", render_started.elapsed())]);
    Ok(text)
}
