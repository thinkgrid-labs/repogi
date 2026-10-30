//! Compact agent-facing rendering. Every byte counts toward the same estimated
//! UTF-8 budget as JSON/text output; IDs are kept so follow-up calls can select.
use std::{collections::BTreeMap, fmt::Write};

use anyhow::Result;
use code_intel_core::{ContextResponse, Edge};

/// One status phrase shared by every agent-format header. A complete result
/// says so explicitly; truncation always names what was left out.
pub(crate) fn completeness(truncated: bool, omitted: usize, limited: bool) -> String {
    if !truncated {
        return "complete".into();
    }
    let mut phrase = String::from("truncated");
    if omitted > 0 {
        write!(phrase, ": {omitted} omitted").expect("write to string");
    }
    if limited {
        phrase.push_str(if omitted > 0 {
            ", work limit reached"
        } else {
            ": work limit reached"
        });
    }
    phrase
}

/// `used/max bytes`: exact UTF-8 size of this text against the requested cap.
/// It is the existing estimated-token budget, not a model token count.
pub(crate) fn budget(used: usize, max: usize) -> String {
    format!("{used}/{max} bytes")
}

pub(crate) fn context(response: &ContextResponse) -> Result<String> {
    let mut output = format!(
        "atlas context r{} · {} · {} item{} · {}",
        response.index_revision,
        response.detail.as_deref().unwrap_or("detailed"),
        response.items.len(),
        if response.items.len() == 1 { "" } else { "s" },
        completeness(
            response.truncated,
            response.omitted_items,
            response.candidate_limit_reached
        ),
    );
    if response.syntax_error_files > 0 {
        write!(
            output,
            " · {} file(s) with syntax errors",
            response.syntax_error_files
        )?;
    }
    writeln!(
        output,
        " · {}",
        budget(response.estimated_tokens, response.budget.max_tokens)
    )?;
    if let Some(evidence) = &response.navigation {
        output.push_str(&crate::signal::text(evidence));
    }
    output.push_str(&crate::knowledge::context_notice(response));
    if response.items.is_empty() && response.navigation.is_some() {
        output.push_str("no indexed matches; other repository languages were not searched; use native Read/Grep\n");
    }
    for (position, item) in response.items.iter().enumerate() {
        write!(output, "\n## [{}] ", position + 1)?;
        // File items are named by their path, which the ID already carries.
        if let Some(name) = item.name.as_ref().filter(|_| item.kind != "file") {
            write!(output, "{name} · ")?;
        }
        write!(
            output,
            "{} · L{}",
            item.id,
            lines(item.start_line, item.end_line)
        )?;
        let why: Vec<&str> = item
            .why
            .iter()
            .map(String::as_str)
            .filter(|why| *why != "selected ID")
            .collect();
        if !why.is_empty() {
            write!(output, " · {}", why.join("; "))?;
        }
        if item.confidence != "syntax" {
            write!(output, " · {}", item.confidence)?;
        }
        match item.detail.as_deref() {
            Some("signature_preview") => output.push_str(" · signature preview"),
            Some("source") | None if item.end_line < item.provenance.end_line => write!(
                output,
                " · partial of L{}",
                lines(item.provenance.start_line, item.provenance.end_line)
            )?,
            _ => {}
        }
        output.push('\n');
        if !item.snippet.is_empty() {
            output.push_str(&item.snippet);
            if !item.snippet.ends_with('\n') {
                output.push('\n');
            }
        }
    }
    if !response.relationships.is_empty() {
        let labels: BTreeMap<&str, usize> = response
            .items
            .iter()
            .enumerate()
            .map(|(position, item)| (item.id.as_str(), position + 1))
            .collect();
        let label = |id: &str| {
            labels
                .get(id)
                .map_or_else(|| id.to_owned(), |position| format!("[{position}]"))
        };
        output.push_str("\nrelationships:\n");
        for group in groups(&response.relationships) {
            write!(
                output,
                "{} -{}-> {} {}",
                label(group.source),
                group.kinds.join("+"),
                group.target.map_or_else(|| "?".into(), label),
                group.status
            )?;
            if !group.sites.is_empty() {
                write!(output, " @{}", group.sites.join(", "))?;
            }
            output.push('\n');
        }
    }
    Ok(output)
}

struct Group<'a> {
    source: &'a str,
    target: Option<&'a str>,
    status: String,
    kinds: Vec<&'a str>,
    sites: Vec<String>,
}

/// One line per (source, target, status): the binder records an identifier
/// REFERENCES edge beside each CALLS/EXTENDS edge, and repeated call sites of
/// one target would otherwise repeat the same IDs. JSON keeps every edge.
fn groups(edges: &[Edge]) -> Vec<Group<'_>> {
    let mut groups: Vec<Group<'_>> = Vec::new();
    for edge in edges {
        let status = edge.resolution.map_or_else(
            || edge.provenance.confidence.clone(),
            |status| {
                serde_json::to_value(status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            },
        );
        // Heuristic edges carry the source declaration's range, already shown.
        let site = edge.location.as_ref().map(|location| {
            format!(
                "{}:{}",
                location.path,
                lines(location.start_line, location.end_line)
            )
        });
        let target = edge.target_id.as_deref();
        let position = groups.iter().position(|group| {
            group.source == edge.source_id && group.target == target && group.status == status
        });
        let group = if let Some(position) = position {
            &mut groups[position]
        } else {
            groups.push(Group {
                source: &edge.source_id,
                target,
                status,
                kinds: Vec::new(),
                sites: Vec::new(),
            });
            groups.last_mut().expect("just pushed")
        };
        if !group.kinds.contains(&edge.kind.as_str()) {
            group.kinds.push(&edge.kind);
        }
        if let Some(site) = site.filter(|site| !group.sites.contains(site)) {
            group.sites.push(site);
        }
    }
    groups
}

pub(crate) fn lines(start: usize, end: usize) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}
