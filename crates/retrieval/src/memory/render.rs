//! JSON is the full contract; text is a compact projection. Every stored or
//! repository string is escaped, so claims cannot inject lines or controls.
//! Responses fit a byte budget by dropping trailing items and saying so.
use std::fmt::Write;

use anyhow::Result;
use serde::Serialize;

use super::{
    Conflicts, Detail, EvidenceView, Header, History, Listing, Note, Receipt, RevisionView, Search,
    Summary, Verification, label,
};
use crate::Format;

/// A response that can shrink to fit a byte budget.
pub trait Fit: Serialize {
    fn text(&self) -> Result<String>;
    /// Drop the least important trailing item; false when nothing is left to drop.
    fn shrink(&mut self) -> bool;
    fn budget(&mut self, max_bytes: usize, used_bytes: usize, truncated: bool);
    fn used(&self) -> usize;
}

/// Render within `max_bytes`, dropping trailing items first.
pub fn render<T: Fit>(response: &mut T, format: Format, max_bytes: usize) -> Result<String> {
    let mut truncated = false;
    loop {
        let mut output = String::new();
        for _ in 0..8 {
            output = if format == Format::Json {
                format!("{}\n", serde_json::to_string(response)?)
            } else {
                response.text()?
            };
            if output.len() == response.used() {
                break;
            }
            response.budget(max_bytes, output.len(), truncated);
        }
        if output.len() <= max_bytes {
            return Ok(output);
        }
        if !response.shrink() {
            return Err(crate::BudgetTooSmall {
                minimum: output.len(),
            }
            .into());
        }
        truncated = true;
        response.budget(max_bytes, 0, truncated);
    }
}

/// Write receipts are small and never truncated.
pub fn receipt(receipt: &Receipt, format: Format) -> Result<String> {
    if format == Format::Json {
        return Ok(format!("{}\n", serde_json::to_string(receipt)?));
    }
    let mut text = format!(
        "memory {} {} · status {}",
        receipt.id,
        receipt.action,
        receipt.status.label()
    );
    if let Some(revision) = receipt.revision {
        write!(text, " · revision {revision}")?;
    }
    if let Some(state) = receipt.verification {
        write!(text, " · evidence {}", state.label())?;
    }
    text.push('\n');
    if let Some(digest) = &receipt.digest {
        writeln!(text, "digest: {digest}")?;
    }
    notes(&mut text, "", &receipt.notes)?;
    Ok(text)
}

fn head(text: &mut String, header: &Header, title: &str) -> Result<()> {
    writeln!(
        text,
        "{title} · r{} · {}",
        header.index_revision,
        header
            .repository_root
            .as_deref()
            .unwrap_or("unknown root")
            .escape_debug()
    )?;
    writeln!(text, "{}", header.notice)?;
    Ok(())
}

fn notes(text: &mut String, indent: &str, notes: &[Note]) -> Result<()> {
    for note in notes {
        writeln!(
            text,
            "{indent}! {}: {}",
            note.code,
            note.detail.escape_debug()
        )?;
    }
    Ok(())
}

fn summary_line(text: &mut String, prefix: &str, memory: &Summary) -> Result<()> {
    writeln!(
        text,
        "{prefix}{} [{} r{}{}] {} · \"{}\"",
        memory.id,
        memory.status.label(),
        memory.revision,
        if memory.authoritative {
            " · authoritative"
        } else {
            " · not authoritative"
        },
        memory.memory_type.label(),
        memory.title.escape_debug()
    )?;
    writeln!(text, "   claim: \"{}\"", memory.claim.escape_debug())?;
    if let Some(assertion) = &memory.assertion {
        writeln!(
            text,
            "   asserts: {} = {}",
            assertion.subject.escape_debug(),
            assertion.status
        )?;
    }
    Ok(())
}

fn evidence_line(text: &mut String, evidence: &EvidenceView) -> Result<()> {
    let snapshot = &evidence.snapshot;
    writeln!(
        text,
        "   #{} {} {} · {} · {}:{}-{} [{}] · {}",
        evidence.ordinal,
        label(&snapshot.role),
        label(&snapshot.class),
        snapshot
            .explicit_id
            .as_deref()
            .unwrap_or(&snapshot.subject_id)
            .escape_debug(),
        snapshot.citation.path.escape_debug(),
        snapshot.citation.start_line,
        snapshot.citation.end_line,
        evidence.state.label(),
        snapshot.title.escape_debug()
    )?;
    for change in &evidence.changes {
        writeln!(
            text,
            "      changed: {}: {}",
            change.code,
            change.detail.escape_debug()
        )?;
    }
    Ok(())
}

fn revision_block(text: &mut String, name: &str, revision: &RevisionView) -> Result<()> {
    writeln!(
        text,
        "\n{name} revision {} ({}) · evidence {} · captured at index r{} by {} on {}",
        revision.revision,
        revision.state.label(),
        revision.verification.label(),
        revision.index_revision,
        revision.proposed_by.escape_debug(),
        revision.created_at
    )?;
    writeln!(text, "   digest: {}", revision.digest)?;
    writeln!(text, "   title: \"{}\"", revision.title.escape_debug())?;
    writeln!(text, "   claim: \"{}\"", revision.claim.escape_debug())?;
    if let Some(reason) = &revision.reason {
        writeln!(text, "   reason: \"{}\"", reason.escape_debug())?;
    }
    for evidence in &revision.evidence {
        evidence_line(text, evidence)?;
    }
    Ok(())
}

macro_rules! budgeted {
    () => {
        fn budget(&mut self, max_bytes: usize, used_bytes: usize, truncated: bool) {
            self.max_bytes = max_bytes;
            self.used_bytes = used_bytes;
            self.truncated = truncated;
        }
        fn used(&self) -> usize {
            self.used_bytes
        }
    };
}

fn footer(text: &mut String, max: usize, used: usize, truncated: bool) -> Result<()> {
    writeln!(text, "{used} / {max} bytes · truncated: {truncated}")?;
    Ok(())
}

impl Fit for Listing {
    fn text(&self) -> Result<String> {
        let mut text = String::new();
        head(&mut text, &self.header, "Project memories")?;
        let statuses: Vec<&str> = self.statuses.iter().map(|s| s.label()).collect();
        writeln!(
            text,
            "{} shown · status [{}] · type {}{}",
            self.memories.len(),
            if statuses.is_empty() {
                "any".into()
            } else {
                statuses.join(", ")
            },
            self.memory_type.map_or("any", |t| t.label()),
            self.next_after
                .as_ref()
                .map_or_else(String::new, |a| format!(" · more: --after {a}"))
        )?;
        for memory in &self.memories {
            summary_line(&mut text, "", memory)?;
        }
        footer(&mut text, self.max_bytes, self.used_bytes, self.truncated)?;
        Ok(text)
    }
    fn shrink(&mut self) -> bool {
        let dropped = self.memories.pop();
        if let Some(last) = self.memories.last() {
            self.next_after = Some(last.id.clone());
        }
        dropped.is_some()
    }
    budgeted!();
}

impl Fit for Detail {
    fn text(&self) -> Result<String> {
        let mut text = String::new();
        head(&mut text, &self.header, "Project memory")?;
        summary_line(&mut text, "", &self.memory)?;
        if let Some(by) = &self.memory.superseded_by {
            writeln!(text, "   superseded by: {by}")?;
        }
        notes(&mut text, "", &self.warnings)?;
        if let Some(approved) = &self.approved {
            revision_block(&mut text, "approved", approved)?;
        }
        if let Some(reviewed) = &self.reviewed {
            revision_block(&mut text, "reviewed", reviewed)?;
        }
        if let Some(pending) = &self.pending {
            revision_block(&mut text, "pending", pending)?;
            writeln!(
                text,
                "   review: repogi memory approve|reject {} --digest {} --reviewer <name>",
                self.memory.id,
                &pending.digest[..16]
            )?;
        }
        if self.check_limit_reached {
            writeln!(
                text,
                "! check_limit_reached: some evidence files were not read"
            )?;
        }
        footer(&mut text, self.max_bytes, self.used_bytes, self.truncated)?;
        Ok(text)
    }
    fn shrink(&mut self) -> bool {
        for revision in [&mut self.pending, &mut self.approved, &mut self.reviewed]
            .into_iter()
            .flatten()
        {
            if revision.evidence.pop().is_some() {
                return true;
            }
        }
        false
    }
    budgeted!();
}

impl Fit for History {
    fn text(&self) -> Result<String> {
        let mut text = String::new();
        head(&mut text, &self.header, "Memory history")?;
        writeln!(
            text,
            "{} · status {} · {} revision(s) shown, {} omitted · {} event(s) shown, {} omitted",
            self.id,
            self.status.label(),
            self.revisions.len(),
            self.omitted_revisions,
            self.events.len(),
            self.omitted_events
        )?;
        for revision in &self.revisions {
            writeln!(
                text,
                "r{} {} · {} evidence · by {} on {} · digest {}",
                revision.revision,
                revision.state.label(),
                revision.evidence,
                revision.proposed_by.escape_debug(),
                revision.created_at,
                &revision.digest[..16]
            )?;
            writeln!(
                text,
                "   \"{}\": \"{}\"",
                revision.title.escape_debug(),
                revision.claim.escape_debug()
            )?;
            if let Some(reason) = &revision.reason {
                writeln!(text, "   reason: \"{}\"", reason.escape_debug())?;
            }
        }
        for event in &self.events {
            write!(
                text,
                "event {} {}{} by {} on {}",
                event.sequence,
                event.action,
                event
                    .revision
                    .map_or_else(String::new, |r| format!(" r{r}")),
                event.actor.escape_debug(),
                event.at
            )?;
            if let Some(note) = &event.note {
                write!(text, " · \"{}\"", note.escape_debug())?;
            }
            if let Some(detail) = &event.detail {
                for (key, value) in detail {
                    write!(text, " · {key}={}", value.escape_debug())?;
                }
            }
            text.push('\n');
        }
        footer(&mut text, self.max_bytes, self.used_bytes, self.truncated)?;
        Ok(text)
    }
    fn shrink(&mut self) -> bool {
        if self.events.pop().is_some() {
            self.omitted_events += 1;
            return true;
        }
        if self.revisions.pop().is_some() {
            self.omitted_revisions += 1;
            return true;
        }
        false
    }
    budgeted!();
}

impl Fit for Search {
    fn text(&self) -> Result<String> {
        let mut text = String::new();
        head(&mut text, &self.header, "Memory search")?;
        let statuses: Vec<&str> = self.statuses.iter().map(|s| s.label()).collect();
        writeln!(
            text,
            "query \"{}\" · status [{}]{} · {} result(s), {} omitted{}",
            self.query.escape_debug(),
            statuses.join(", "),
            if self.include_unreviewed {
                " + unreviewed"
            } else {
                ""
            },
            self.results.len(),
            self.omitted_results,
            if self.candidate_limit_reached {
                " · candidate limit reached"
            } else {
                ""
            }
        )?;
        for hit in &self.results {
            summary_line(&mut text, &format!("{}. ", hit.rank), &hit.summary)?;
            writeln!(
                text,
                "   evidence {} · {}{}",
                hit.verification.label(),
                hit.match_mode,
                if hit.explicit_ids.is_empty() {
                    String::new()
                } else {
                    format!(" · cites {}", hit.explicit_ids.join(", ").escape_debug())
                }
            )?;
            for citation in &hit.citations {
                writeln!(
                    text,
                    "   - {} {} {}:{}-{} [{}]",
                    label(&citation.role),
                    label(&citation.class),
                    citation.path.escape_debug(),
                    citation.start_line,
                    citation.end_line,
                    citation.state.label()
                )?;
            }
            notes(&mut text, "   ", &hit.warnings)?;
        }
        footer(&mut text, self.max_bytes, self.used_bytes, self.truncated)?;
        Ok(text)
    }
    fn shrink(&mut self) -> bool {
        if self.results.pop().is_some() {
            self.omitted_results += 1;
            return true;
        }
        false
    }
    budgeted!();
}

impl Fit for Verification {
    fn text(&self) -> Result<String> {
        let mut text = String::new();
        head(&mut text, &self.header, "Memory verification")?;
        writeln!(
            text,
            "{} memory(ies) · {} omitted · recorded: {}{}",
            self.results.len(),
            self.omitted,
            self.recorded,
            if self.check_limit_reached {
                " · check limit reached"
            } else {
                ""
            }
        )?;
        writeln!(
            text,
            "Matching bytes prove only that cited sources are unchanged, not that claims are correct."
        )?;
        for result in &self.results {
            writeln!(
                text,
                "{} [{} r{}] evidence {}",
                result.id,
                result.status.label(),
                result.revision,
                result.verification.label()
            )?;
            for check in &result.evidence {
                writeln!(text, "   #{} {}", check.ordinal + 1, check.state.label())?;
                for change in &check.changes {
                    writeln!(
                        text,
                        "      changed: {}: {}",
                        change.code,
                        change.detail.escape_debug()
                    )?;
                }
            }
        }
        footer(&mut text, self.max_bytes, self.used_bytes, self.truncated)?;
        Ok(text)
    }
    fn shrink(&mut self) -> bool {
        if self.results.pop().is_some() {
            self.omitted += 1;
            return true;
        }
        false
    }
    budgeted!();
}

impl Fit for Conflicts {
    fn text(&self) -> Result<String> {
        let mut text = String::new();
        head(&mut text, &self.header, "Memory conflicts")?;
        writeln!(
            text,
            "{} conflict(s) · {} approved memory(ies) scanned{}{}",
            self.conflicts.len(),
            self.scanned_memories,
            self.next_after
                .as_ref()
                .map_or_else(String::new, |a| format!(" · more: --after {a}")),
            if self.check_limit_reached {
                " · check limit reached"
            } else {
                ""
            }
        )?;
        writeln!(
            text,
            "Structured conflicts only; Repogi never decides which memory is correct."
        )?;
        for conflict in &self.conflicts {
            writeln!(
                text,
                "{} · {}{} · {}",
                conflict.kind,
                conflict.memories.join(", "),
                conflict
                    .subject
                    .as_ref()
                    .map_or_else(String::new, |s| format!(" · subject {}", s.escape_debug())),
                conflict.detail.escape_debug()
            )?;
            for evidence in &conflict.evidence {
                writeln!(
                    text,
                    "   {} #{} {} [{}]",
                    evidence.memory,
                    evidence.ordinal,
                    evidence.path.escape_debug(),
                    evidence.state.label()
                )?;
                for change in &evidence.changes {
                    writeln!(
                        text,
                        "      changed: {}: {}",
                        change.code,
                        change.detail.escape_debug()
                    )?;
                }
            }
        }
        footer(&mut text, self.max_bytes, self.used_bytes, self.truncated)?;
        Ok(text)
    }
    fn shrink(&mut self) -> bool {
        self.conflicts.pop().is_some()
    }
    budgeted!();
}
