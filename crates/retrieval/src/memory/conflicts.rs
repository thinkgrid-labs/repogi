//! Deterministic, structured conflicts only: explicit assertions that clash,
//! approved memories citing superseded decisions, and approved memories whose
//! backing evidence is missing, unresolved or changed. No natural-language
//! contradiction detection, and no conflict is ever resolved automatically.
use anyhow::Result;
use code_intel_core::{
    memory::{EvidenceRole, MemoryStatus},
    relationships::{RelationshipKind, ResolutionStatus},
};
use code_intel_store::{MemoryFilter, Store};
use serde::Serialize;

use super::{Header, MAX_CONFLICT_SCAN, Note, State, clean, header, revision, verify};

#[derive(Debug, Clone, Serialize)]
pub struct ConflictEvidence {
    pub memory: String,
    pub ordinal: usize,
    pub subject_id: String,
    pub path: String,
    pub state: State,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Note>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conflict {
    /// `conflicting_assertions`, `superseded_decision_reference`,
    /// `evidence_missing`, `evidence_unresolved` or `evidence_changed`.
    pub kind: &'static str,
    pub memories: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub detail: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ConflictEvidence>,
    /// Always true: a reviewer decides; Repogi never picks a winner.
    pub review_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conflicts {
    #[serde(flatten)]
    pub header: Header,
    pub conflicts: Vec<Conflict>,
    pub scanned_memories: usize,
    /// More approved memories exist past `next_after`; pass it to continue.
    pub scan_limit_reached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
    pub check_limit_reached: bool,
    pub truncated: bool,
    pub max_bytes: usize,
    pub used_bytes: usize,
}

/// Scan up to `limit` approved memories (in ID order, after `after`).
pub fn conflicts(store: &Store, after: Option<String>, limit: usize) -> Result<Conflicts> {
    if let Some(after) = &after {
        super::validate_id(after)?;
    }
    let limit = limit.clamp(1, MAX_CONFLICT_SCAN);
    let mut found = assertion_conflicts(store)?;
    let rows = store.memories(
        &MemoryFilter {
            statuses: vec![MemoryStatus::Approved],
            memory_type: None,
            after,
        },
        u32::try_from(limit + 1)?,
    )?;
    let more = rows.len() > limit;
    let mut checker = verify::Checker::new(store)?;
    let mut scanned = 0;
    let mut last = None;
    for memory in rows.into_iter().take(limit) {
        store.check_cancelled()?;
        scanned += 1;
        last = Some(memory.id.clone());
        let Some(number) = memory.approved_revision else {
            continue;
        };
        let approved = revision(store, &memory.id, number)?;
        let check = checker.revision(&approved)?;
        for item in &check.evidence {
            let snapshot = &approved.evidence[item.ordinal];
            if snapshot.role != EvidenceRole::Backing {
                continue;
            }
            let evidence = || {
                vec![ConflictEvidence {
                    memory: memory.id.clone(),
                    ordinal: item.ordinal + 1,
                    subject_id: snapshot.subject_id.clone(),
                    path: snapshot.citation.path.clone(),
                    state: item.state,
                    changes: item.changes.clone(),
                }]
            };
            if snapshot.kind == "architecture_decision"
                && let Some(detail) = superseded_decision(
                    store,
                    &snapshot.subject_id,
                    snapshot.explicit_id.as_deref(),
                )?
            {
                found.push(Conflict {
                    kind: "superseded_decision_reference",
                    memories: vec![memory.id.clone()],
                    subject: snapshot.explicit_id.clone(),
                    detail,
                    evidence: evidence(),
                    review_required: true,
                });
            }
            let kind = match item.state {
                State::Missing => "evidence_missing",
                State::Unresolved => "evidence_unresolved",
                State::Stale => "evidence_changed",
                State::Current | State::Unverified => continue,
            };
            found.push(Conflict {
                kind,
                memories: vec![memory.id.clone()],
                subject: snapshot.explicit_id.clone(),
                detail: format!(
                    "backing evidence #{} ({}) is {} since approval",
                    item.ordinal + 1,
                    clean(&snapshot.selector),
                    item.state.label()
                ),
                evidence: evidence(),
                review_required: true,
            });
        }
    }
    Ok(Conflicts {
        header: header(store, "memory_conflicts")?,
        conflicts: found,
        scanned_memories: scanned,
        scan_limit_reached: more,
        next_after: more.then_some(last).flatten(),
        check_limit_reached: checker.limit_reached,
        truncated: false,
        max_bytes: 0,
        used_bytes: 0,
    })
}

/// Approved memories asserting different statuses for one subject.
fn assertion_conflicts(store: &Store) -> Result<Vec<Conflict>> {
    let mut found = Vec::new();
    for clash in store.memory_assertion_clashes(64)? {
        found.push(Conflict {
            kind: "conflicting_assertions",
            memories: clash.claims.iter().map(|(id, _, _)| id.clone()).collect(),
            detail: clash
                .claims
                .iter()
                .map(|(id, revision, status)| format!("{id} r{revision} asserts {status}"))
                .collect::<Vec<_>>()
                .join("; "),
            subject: Some(clean(&clash.subject)),
            evidence: Vec::new(),
            review_required: true,
        });
    }
    Ok(found)
}

/// A cited decision whose current recorded status is historical, with its
/// explicit successors (validated SUPERSEDES claims), if any.
fn superseded_decision(store: &Store, id: &str, explicit: Option<&str>) -> Result<Option<String>> {
    if !(store.knowledge_available()? && store.relationships_available()?) {
        return Ok(None);
    }
    let Some(entity) = store
        .knowledge_entities(id, 2)?
        .into_iter()
        .find(|e| e.id == id)
    else {
        return Ok(None);
    };
    let status = entity.decision_status.as_deref().map(clean);
    let mut successors = Vec::new();
    for edge in store.knowledge_neighbors(id, 17)?.into_iter().take(16) {
        if edge.kind == RelationshipKind::Supersedes
            && edge.status == ResolutionStatus::Validated
            && edge.target_id.as_deref() == Some(id)
            && let Some(successor) = store
                .knowledge_entities(&edge.source_id, 1)?
                .into_iter()
                .next()
        {
            successors.push(clean(
                successor.explicit_id.as_deref().unwrap_or(&successor.id),
            ));
        }
    }
    let historical = matches!(
        status.as_deref(),
        Some("superseded" | "deprecated" | "rejected")
    );
    if !historical && successors.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "{} has recorded status {}{}",
        clean(explicit.unwrap_or(id)),
        status.as_deref().unwrap_or("undeclared"),
        if successors.is_empty() {
            String::new()
        } else {
            format!("; superseded by {}", successors.join(", "))
        }
    )))
}
