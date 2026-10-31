//! JSON is the full contract; text is a compact projection of the same ledger.
//! Repository strings are escaped, so they cannot inject lines or controls.
use std::fmt::Write;

use anyhow::Result;

use super::{Next, Response};
use crate::Format;

#[allow(clippy::too_many_lines)] // Header, steps and evidence ledger in one stable layout.
pub fn render(response: &Response, format: Format) -> Result<String> {
    if format == Format::Json {
        return Ok(format!("{}\n", serde_json::to_string(response)?));
    }
    let mut text = String::new();
    let completeness = &response.completeness;
    writeln!(
        text,
        "Investigation r{} · {} · {}",
        response.index_revision,
        response
            .repository_root
            .as_deref()
            .unwrap_or("unknown root")
            .escape_debug(),
        response.ranking_label()
    )?;
    writeln!(
        text,
        "task: \"{}\" · {} · cues [{}] · search terms [{}] · selectors [{}]",
        response.task.original.escape_debug(),
        response.task.classification,
        response.task.cues.join(", "),
        response.task.search_terms.join(", ").escape_debug(),
        response.task.selectors.join(", ").escape_debug()
    )?;
    writeln!(text, "{}", response.notice)?;
    writeln!(
        text,
        "{} / {} bytes · {} of {} step(s) · depth {} · complete within limits: {}",
        response.budget.used_bytes,
        response.budget.max_bytes,
        response.steps.len(),
        response.steps.len() + completeness.omitted_steps,
        response.budget.depth,
        completeness.complete_within_limits
    )?;
    writeln!(
        text,
        "limits: byte truncated {}; step limit {}; work limit {} ({} / {} rows); candidate limit {}; depth frontier {}; unresolved findings {}; omitted lexical candidates {}",
        completeness.byte_truncated,
        completeness.step_limit_reached,
        completeness.work_limit_reached,
        response.budget.work_used,
        response.budget.work_limit,
        completeness.candidate_limit_reached,
        completeness.depth_frontier,
        completeness.unresolved_findings,
        completeness.omitted_lexical_candidates
    )?;
    if !completeness.omitted_categories.is_empty() {
        let categories: Vec<String> = completeness
            .omitted_categories
            .iter()
            .map(super::label)
            .collect();
        writeln!(text, "omitted categories: {}", categories.join(", "))?;
    }
    for unsupported in &completeness.unsupported {
        writeln!(text, "unsupported: {unsupported}")?;
    }
    let freshness = &response.freshness;
    writeln!(
        text,
        "freshness: indexed snapshot; selected sources {} ({} checked, {} unchecked); graph and inventory freshness unverified",
        freshness.selected_sources, freshness.checked_files, freshness.unchecked_files
    )?;
    if !freshness.stale_files.is_empty() || !freshness.missing_files.is_empty() {
        writeln!(
            text,
            "stale indexed evidence: changed {:?}; missing {:?}; read current files or explicitly re-index",
            freshness.stale_files, freshness.missing_files
        )?;
    }
    for item in &response.uncertainty {
        writeln!(text, "! {}: {}", item.code, item.detail.escape_debug())?;
    }
    for step in &response.steps {
        writeln!(
            text,
            "\n{}. {} [{}; tier {}] {}",
            step.order,
            super::label(&step.action),
            super::label(&step.category),
            step.priority.tier,
            step.objective.escape_debug()
        )?;
        for id in &step.targets {
            if let Some(target) = response.targets.iter().find(|t| &t.id == id) {
                writeln!(
                    text,
                    "   target: {} · {} · {}:{}-{} [{}]{}",
                    target.name.escape_debug(),
                    target.id.escape_debug(),
                    target.location.path.escape_debug(),
                    target.location.start_line,
                    target.location.end_line,
                    target.freshness,
                    target
                        .decision_status
                        .as_ref()
                        .map_or_else(String::new, |s| format!(" status={}", s.escape_debug()))
                )?;
            }
        }
        if step.omitted_targets > 0 {
            writeln!(
                text,
                "   … {} more target(s) omitted from this step",
                step.omitted_targets
            )?;
        }
        writeln!(text, "   evidence: {}", step.evidence.join(" "))?;
        for item in &step.uncertainty {
            writeln!(
                text,
                "   uncertain: {}: {}",
                item.code,
                item.detail.escape_debug()
            )?;
        }
        let next: Vec<String> = step.next.iter().map(next_text).collect();
        writeln!(text, "   next: {}", next.join(" | "))?;
        writeln!(
            text,
            "   priority: {}",
            step.priority.rationale.escape_debug()
        )?;
    }
    if !response.evidence.is_empty() {
        text.push_str("\nevidence:\n");
    }
    for evidence in &response.evidence {
        write!(
            text,
            "{} {} · {}:{}-{} [{}] · {}",
            evidence.id,
            super::label(&evidence.class),
            evidence.citation.path.escape_debug(),
            evidence.citation.start_line,
            evidence.citation.end_line,
            evidence.freshness,
            evidence.rule.escape_debug()
        )?;
        if let Some(relation) = &evidence.relationship {
            write!(
                text,
                " · {} --{}--> {} ({})",
                relation.source_id.escape_debug(),
                relation.kind,
                relation
                    .target_id
                    .as_deref()
                    .unwrap_or(&relation.target_text)
                    .escape_debug(),
                relation.status
            )?;
        }
        if let Some(depth) = evidence.depth {
            write!(text, " · depth {depth}")?;
        }
        text.push('\n');
    }
    Ok(text)
}

fn next_text(next: &Next) -> String {
    if next.tool == "read" {
        let path = next
            .path
            .as_deref()
            .unwrap_or_default()
            .escape_debug()
            .to_string();
        return match (next.start_line, next.end_line) {
            (Some(start), Some(end)) => format!("read {path}:{start}-{end}"),
            _ => format!("read {path} (whole file; indexed range is stale)"),
        };
    }
    let mut arguments = serde_json::Map::new();
    if let Some(id) = &next.id {
        arguments.insert("id".into(), id.clone().into());
    }
    if let Some(selector) = &next.selector {
        arguments.insert("selector".into(), selector.clone().into());
    }
    if let Some(direction) = next.direction {
        arguments.insert("direction".into(), super::label(&direction).into());
    }
    if let Some(depth) = next.depth {
        arguments.insert("depth".into(), depth.into());
    }
    format!("{} {}", next.tool, serde_json::Value::Object(arguments))
}

impl Response {
    fn ranking_label(&self) -> &'static str {
        match self.ranking {
            super::Ranking::Balanced => "balanced ranking",
            super::Ranking::Tiered => "tiered ranking",
        }
    }
}
