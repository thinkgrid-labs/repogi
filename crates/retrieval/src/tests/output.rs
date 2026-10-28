use std::{collections::BTreeMap, fmt::Write as _};

use anyhow::{Result, ensure};

use super::{Candidate, Options, Report};
use crate::impact::Request;

fn candidate(out: &mut String, c: &Candidate, detail: bool) {
    let _ = write!(out, "  {} [{}]", c.path, c.priority);
    // A file observed in one snapshot only is never called added or deleted.
    match (
        c.snapshots.contains_key("base"),
        c.snapshots.contains_key("head"),
    ) {
        (true, false) => out.push_str(" (baseline only; not observed in the changed state)"),
        (false, true) => out.push_str(" (changed state only)"),
        _ => {}
    }
    if let Some(e) = &c.evidence {
        let _ = write!(
            out,
            " d{} {}:{} {} -> {}#{} via {}",
            e.depth,
            e.snapshot,
            e.line,
            e.relationship,
            e.target.path,
            e.target.name,
            e.via.join(" <- ")
        );
        if detail {
            let _ = write!(out, " ({})", e.rule);
            if !e.binding_chain.is_empty() {
                let _ = write!(out, " [{}]", e.binding_chain.join(" -> "));
            }
        }
    }
    // The default view gives a confirmed row its linkage reasons only; merged
    // heuristic hints remain in JSON and detailed text.
    let confirmed = c.priority != "possible";
    let reasons: Vec<_> = c
        .reasons
        .iter()
        .filter(|r| detail || !confirmed || !super::evidence::hint(r))
        .map(String::as_str)
        .collect();
    let weak = if c.priority == "possible" && super::evidence::strength(c) >= 2 {
        "weak heuristic: "
    } else {
        ""
    };
    let _ = writeln!(out, "\n    {weak}{}", reasons.join("; "));
}

/// A text projection only. Counts describe represented candidates, not dropped
/// rows; the original candidates, classifications and reasons stay in Report.
fn possible(out: &mut String, candidates: &[Candidate], individual: bool) {
    let mut colocated = BTreeMap::<&str, usize>::new();
    let mut basename = 0;
    let mut basename_directories = std::collections::BTreeSet::new();
    for c in candidates {
        let strength = super::evidence::strength(c);
        if individual || strength < 2 {
            candidate(out, c, individual);
            continue;
        }
        let directory = c.path.rsplit_once('/').map_or(".", |(parent, _)| parent);
        if strength == 2 {
            basename += 1;
            basename_directories.insert(directory);
        } else {
            *colocated.entry(directory).or_default() += 1;
        }
    }
    if basename > 0 {
        let _ = writeln!(
            out,
            "  weak heuristic: basename only (may include colocation): {basename} possible candidates across {} directories; collisions possible; no target inferred",
            basename_directories.len()
        );
    }
    for (directory, count) in &colocated {
        let _ = writeln!(
            out,
            "  weak heuristic: colocation only: {directory}/ ({count} possible candidates; proximity does not establish usage)"
        );
    }
    let colocation: usize = colocated.values().sum();
    if basename + colocation > 0 {
        let _ = writeln!(
            out,
            "  summarized possible candidates: weak={} (basename={basename} colocation={colocation}); individual paths and reasons: --detail or --format json (MCP: detail=true or format=json)",
            basename + colocation
        );
    }
}

fn omitted(out: &mut String, report: &Report, key: &str) {
    if let Some(count) = report.omitted.get(key) {
        let _ = writeln!(out, "  … {count} omitted");
    }
}

fn header(report: &Report) -> String {
    let mut out = format!(
        "atlas affected tests · {}{}\n",
        if report.completeness.complete {
            "complete within supported static discovery profile"
        } else {
            "incomplete"
        },
        if report.truncated {
            " · truncated"
        } else {
            ""
        }
    );
    let _ = writeln!(
        out,
        "states: {} -> {}",
        report.base.spec.as_deref().unwrap_or("commit"),
        report.head.spec.as_deref().unwrap_or("working tree")
    );
    let _ = writeln!(
        out,
        "snapshots: {} -> {}",
        report.base.digest, report.head.digest
    );
    let _ = writeln!(
        out,
        "inventory snapshots: {} -> {}",
        report.base_inventory_digest, report.head_inventory_digest
    );
    let c = &report.completeness;
    let _ = writeln!(
        out,
        "depth={} direct_files_complete={} traversal_complete={} snapshots_fresh={} inventory_complete={} static_links_complete={}",
        report.depth,
        c.direct_files_complete,
        c.traversal_complete,
        c.snapshots_fresh,
        c.inventory_complete,
        c.static_links_complete
    );
    let _ = writeln!(
        out,
        "possible_unverified={} source_coverage_unknown={} analysis_limited={}",
        c.possible_unverified, c.source_coverage_unknown, report.analysis_limited
    );
    let counts = |m: &BTreeMap<String, usize>| {
        m.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let _ = writeln!(
        out,
        "totals: {}\nomitted: {}",
        counts(&report.totals),
        counts(&report.omitted)
    );
    let _ = writeln!(
        out,
        "inventory classes (distinct files per class): {}",
        counts(&report.inventory_counts)
    );
    if let Some(scope) = &report.scope {
        let _ = writeln!(
            out,
            "selected test: {scope}; other candidates intentionally excluded: {}",
            counts(&report.excluded_by_selection)
        );
    }
    out
}

fn text(report: &Report, detail: bool) -> String {
    let mut out = header(report);
    out.push_str("\nchanged declarations:\n");
    for change in &report.changed {
        let _ = writeln!(
            out,
            "  {} {} {}#{}",
            change.change, change.kind, change.path, change.name
        );
    }
    omitted(&mut out, report, "changed");
    out.push_str("\nconfirmed linked tests (direct before transitive):\n");
    for c in &report.confirmed {
        candidate(&mut out, c, detail);
    }
    omitted(&mut out, report, "confirmed");
    out.push_str("\npossible candidates (unverified, strongest signal first):\n");
    possible(&mut out, &report.possible, detail || report.scope.is_some());
    omitted(&mut out, report, "possible");
    out.push_str("\nareas without confirmed test linkage:\n");
    for gap in &report.gaps {
        let _ = writeln!(out, "  {}#{}: {}", gap.path, gap.declaration, gap.reason);
    }
    omitted(&mut out, report, "gaps");
    out.push_str("\nunknown test coverage:\n");
    for file in &report.unknown {
        let _ = writeln!(
            out,
            "  {} {:?}: {}",
            file.path,
            file.snapshots,
            file.reasons.iter().cloned().collect::<Vec<_>>().join("; ")
        );
    }
    omitted(&mut out, report, "unknown");
    if detail {
        out.push_str("\ntest inventory (probable tests; no runner membership claim):\n");
        for file in &report.inventory {
            let _ = writeln!(out, "  {} {:?}", file.path, file.snapshots);
        }
        omitted(&mut out, report, "inventory");
    }
    out.push_str("\nconfiguration data:\n");
    for config in &report.configurations {
        let _ = writeln!(
            out,
            "  {} {}: {}",
            config.snapshot,
            config.path,
            config.hints.join("; ")
        );
    }
    omitted(&mut out, report, "configurations");
    out.push_str("\ndiscovery limitations:\n");
    for limitation in &report.limitations {
        let _ = writeln!(out, "  {limitation}");
    }
    omitted(&mut out, report, "limitations");
    out.push_str("\nAdvisory selection only. Static linkage does not establish failure or behavioral coverage. Passing selected tests does not prove safety. No tests or project scripts were executed.\nEvidence for one test: evidence_file=<test-path> (CLI: --evidence-file <test-path>); structure: format=json (CLI: --format json).\n");
    out
}

fn cap<T>(values: &mut Vec<T>, limit: usize, key: &str, omitted: &mut BTreeMap<String, usize>) {
    if values.len() > limit {
        *omitted.entry(key.into()).or_default() += values.len() - limit;
        values.truncate(limit);
    }
}

fn flags(report: &mut Report) {
    report.truncated = !report.omitted.is_empty();
    if report.truncated || report.scope.is_some() {
        report.completeness.complete = false;
    }
    if report.omitted.get("confirmed").copied().unwrap_or(0) > 0 || report.scope.is_some() {
        report.completeness.static_links_complete = false;
    }
}

pub fn respond(
    request: &Request,
    options: &Options,
    selected: Option<&str>,
    json: bool,
    max_bytes: usize,
) -> Result<String> {
    if let Some(path) = selected {
        ensure!(
            super::inventory::safe_relative(path),
            "evidence-file must be a repository-relative test file"
        );
    }
    let mut report = super::compare(request, options)?;
    let started = std::time::Instant::now();
    if let Some(path) = selected {
        ensure!(
            report.inventory.iter().any(|f| f.path == path),
            "test file not observed in the configured inventory: {path}"
        );
        report.scope = Some(path.into());
        macro_rules! select {
            ($field:ident) => {{
                let count = report.$field.len();
                report.$field.retain(|row| row.path == path);
                report
                    .excluded_by_selection
                    .insert(stringify!($field).into(), count - report.$field.len());
            }};
        }
        select!(confirmed);
        select!(possible);
        select!(unknown);
        select!(inventory);
    }
    // Inventory is an explicit detailed projection, not a silently truncated
    // candidate list. Totals/classes remain present in the default plan.
    report.inventory_included = request.options.detail;
    if !request.options.detail {
        report.inventory.clear();
        // Binding chains are detailed evidence, as in the text view.
        for candidate in report.confirmed.iter_mut().chain(&mut report.possible) {
            if let Some(evidence) = &mut candidate.evidence {
                evidence.binding_chain.clear();
            }
        }
    }
    let limit = request.options.max_results;
    cap(
        &mut report.confirmed,
        limit,
        "confirmed",
        &mut report.omitted,
    );
    cap(&mut report.possible, limit, "possible", &mut report.omitted);
    cap(&mut report.unknown, limit, "unknown", &mut report.omitted);
    cap(&mut report.gaps, limit, "gaps", &mut report.omitted);
    cap(&mut report.changed, limit, "changed", &mut report.omitted);
    cap(
        &mut report.inventory,
        limit,
        "inventory",
        &mut report.omitted,
    );
    loop {
        flags(&mut report);
        let rendered = if json {
            format!("{}\n", serde_json::to_string(&report)?)
        } else {
            text(&report, request.options.detail)
        };
        if rendered.len() <= max_bytes {
            code_intel_store::profile(&[("test_render", started.elapsed())]);
            return Ok(rendered);
        }
        // Whole deterministic entries only. Candidate priority survives fitting.
        // Global limitations outlast per-file rows (possible rows drop weakest
        // first); confirmed rows go last.
        macro_rules! drop_last {
            ($field:ident) => {
                if report.$field.pop().is_some() {
                    *report.omitted.entry(stringify!($field).into()).or_default() += 1;
                    continue;
                }
            };
        }
        drop_last!(inventory);
        drop_last!(configurations);
        drop_last!(unknown);
        drop_last!(gaps);
        drop_last!(possible);
        drop_last!(limitations);
        drop_last!(changed);
        drop_last!(confirmed);
        anyhow::bail!("test discovery metadata exceeds max-bytes; increase the response budget");
    }
}
