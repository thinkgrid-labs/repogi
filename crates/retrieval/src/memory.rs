//! Reviewed project memory: explicit proposals, digest-bound review,
//! evidence verification, search and structured conflicts.
//!
//! A memory is a durable, developer-reviewed claim with immutable evidence
//! snapshots. Nothing here creates or approves a memory implicitly: proposals,
//! reviews, supersession and retirement are separate explicit write commands,
//! and every read (list, show, history, search, verify, conflicts) is
//! read-only and computes verification live without persisting it.
mod capture;
mod conflicts;
mod render;
mod verify;

use std::path::Path;

use anyhow::Result;
use code_intel_core::memory::{
    Assertion, EvidenceRole, EvidenceSnapshot, MAX_ACTOR_BYTES, MAX_CLAIM_BYTES, MAX_EVIDENCE,
    MAX_NOTE_BYTES, MAX_TITLE_BYTES, MEMORY_NOTICE, Memory, MemoryEvent, MemoryStatus, MemoryType,
    Revision, RevisionState, utc_now,
};
use code_intel_store::{Act, Decision, MemoryError, MemoryFilter, NewRevision, Store};
use serde::Serialize;

pub use capture::Selector;
pub use conflicts::{Conflict, ConflictEvidence, Conflicts, conflicts};
pub use render::{Fit, receipt as render_receipt, render};
pub use verify::{EvidenceCheck, MemoryCheck, State};

pub(crate) use crate::investigate::{clean, edge_range, fact_range, label};

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 100;
pub const MAX_HISTORY: usize = 200;
/// FTS candidates read per search, before verification and output fitting.
pub const MAX_SEARCH_CANDIDATES: usize = 64;
/// Working-tree files read per request for verification.
pub const MAX_VERIFY_FILES: usize = 64;
/// Approved memories verified per conflict scan.
pub const MAX_CONFLICT_SCAN: usize = 200;
pub const MAX_QUERY_BYTES: usize = 1024;
pub const MAX_SELECTOR_BYTES: usize = 1024;
pub const DEFAULT_BYTES: usize = 12_000;
pub const MAX_BYTES: usize = 8_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub code: &'static str,
    pub detail: String,
}

fn fail<T>(code: &'static str, message: impl Into<String>) -> Result<T> {
    Err(MemoryError {
        code,
        message: message.into(),
    }
    .into())
}

/// The indexed whole-file hash used for a path: documents when the knowledge
/// layer is indexed, otherwise source files (as in selected-source checks).
fn indexed_hash(store: &Store, path: &str) -> Result<Option<String>> {
    if code_intel_index::supported_document(Path::new(path)) && store.knowledge_available()? {
        store.document_hash(path)
    } else {
        store.file_hash(path)
    }
}

/// BLAKE3 over the claim fields and every evidence snapshot: approving a
/// digest approves exactly that claim and that evidence.
#[must_use]
pub fn revision_digest(
    memory_type: MemoryType,
    title: &str,
    claim: &str,
    assertion: Option<&Assertion>,
    evidence: &[EvidenceSnapshot],
) -> String {
    let canonical = serde_json::to_vec(&(memory_type, title, claim, assertion, evidence))
        .expect("memory revision serializes");
    blake3::hash(&canonical).to_hex().to_string()
}

/// The content of a proposal or correction, before evidence capture.
#[derive(Debug, Clone)]
pub struct Draft {
    pub memory_type: MemoryType,
    pub title: String,
    pub claim: String,
    /// `SUBJECT=status`, for deterministic conflict detection only.
    pub assertion: Option<String>,
    /// Evidence that must be able to back the claim.
    pub evidence: Vec<Selector>,
    /// Contextual evidence; never backs a claim.
    pub context: Vec<Selector>,
}

fn text(field: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return fail(
            "invalid_arguments",
            format!(
                "{field} must be 1..={max} UTF-8 bytes without control characters (newlines included)"
            ),
        );
    }
    Ok(())
}

fn optional_text(field: &str, value: Option<&str>) -> Result<()> {
    value.map_or(Ok(()), |v| text(field, v, MAX_NOTE_BYTES))
}

/// Actor names are self-reported labels for the history, not authentication.
pub fn validate_actor(actor: &str) -> Result<()> {
    if actor.is_empty()
        || actor.len() > MAX_ACTOR_BYTES
        || !actor
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '@' | '-' | '+'))
    {
        return fail(
            "invalid_actor",
            format!(
                "actor names are 1..={MAX_ACTOR_BYTES} bytes of letters, digits, space and ._@-+"
            ),
        );
    }
    Ok(())
}

pub fn validate_id(id: &str) -> Result<()> {
    let digits = id.strip_prefix("mem-").unwrap_or_default();
    if id.len() > 32 || digits.len() < 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return fail(
            "invalid_memory_id",
            format!("{:?} is not a memory ID (mem-0001)", clean(id)),
        );
    }
    Ok(())
}

fn assertion(value: Option<&str>) -> Result<Option<Assertion>> {
    let Some(value) = value else { return Ok(None) };
    let Some((subject, status)) = value.split_once('=') else {
        return fail("invalid_arguments", "assertions are SUBJECT=status");
    };
    if subject.is_empty()
        || subject.len() > 128
        || subject.chars().any(|c| c.is_control() || c == '=')
    {
        return fail(
            "invalid_arguments",
            "an assertion subject is 1..=128 bytes without controls",
        );
    }
    if status.is_empty()
        || status.len() > 32
        || !status.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        return fail(
            "invalid_arguments",
            "an assertion status is 1..=32 lowercase letters or underscores",
        );
    }
    Ok(Some(Assertion {
        subject: subject.to_owned(),
        status: status.to_owned(),
    }))
}

/// Capture every selector by value against the current index and working tree.
fn capture_all(store: &Store, draft: &Draft) -> Result<(Option<Assertion>, Vec<EvidenceSnapshot>)> {
    text("title", &draft.title, MAX_TITLE_BYTES)?;
    text("claim", &draft.claim, MAX_CLAIM_BYTES)?;
    let assertion = assertion(draft.assertion.as_deref())?;
    if draft.evidence.is_empty() {
        return fail(
            "evidence_required",
            "a memory needs at least one backing --evidence (requirement, decision, document, validated claim or static edge)",
        );
    }
    if draft.evidence.len() + draft.context.len() > MAX_EVIDENCE {
        return fail(
            "invalid_arguments",
            format!("at most {MAX_EVIDENCE} evidence items per revision"),
        );
    }
    let root = store.root()?.map(std::path::PathBuf::from);
    let Some(root) = root else {
        return fail("index_unreadable", "the index records no repository root");
    };
    let mut evidence: Vec<EvidenceSnapshot> = Vec::new();
    for (selector, role) in draft
        .evidence
        .iter()
        .map(|s| (s, EvidenceRole::Backing))
        .chain(draft.context.iter().map(|s| (s, EvidenceRole::Context)))
    {
        let snapshot = capture::capture(store, &root, selector, role)?;
        if evidence
            .iter()
            .any(|e| e.subject_id == snapshot.subject_id && e.citation == snapshot.citation)
        {
            continue;
        }
        evidence.push(snapshot);
    }
    Ok((assertion, evidence))
}

/// The result of an explicit write.
#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub schema_version: u32,
    pub kind: &'static str,
    pub action: &'static str,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
    pub status: MemoryStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<State>,
    pub notes: Vec<Note>,
}

fn receipt(
    store: &Store,
    action: &'static str,
    id: &str,
    revision: Option<u32>,
    digest: Option<String>,
) -> Result<Receipt> {
    let memory = store
        .memory(id)?
        .ok_or_else(|| anyhow::anyhow!("memory vanished after write"))?;
    Ok(Receipt {
        schema_version: SCHEMA_VERSION,
        kind: "memory_write",
        action,
        id: id.to_owned(),
        revision,
        status: memory.status,
        digest,
        verification: None,
        notes: Vec::new(),
    })
}

/// Create a pending memory. It is not authoritative until explicitly approved.
pub fn propose(store: &mut Store, draft: &Draft, author: &str) -> Result<Receipt> {
    validate_actor(author)?;
    let (assertion, evidence) = capture_all(store, draft)?;
    let digest = revision_digest(
        draft.memory_type,
        &draft.title,
        &draft.claim,
        assertion.as_ref(),
        &evidence,
    );
    let mut notes = Vec::new();
    for (id, status, live) in store.memory_duplicates(draft.memory_type, &draft.claim)? {
        if !live || !matches!(status, MemoryStatus::PendingReview | MemoryStatus::Approved) {
            notes.push(Note {
                code: "similar_claim_exists",
                detail: format!(
                    "{id} ({}) recorded the same claim in a revision that is not its live claim",
                    status.label()
                ),
            });
        }
    }
    let index_revision = store.revision()?;
    let at = utc_now();
    let id = store.memory_propose(
        &NewRevision {
            memory_type: draft.memory_type,
            title: &draft.title,
            claim: &draft.claim,
            assertion: assertion.as_ref(),
            evidence: &evidence,
            digest: &digest,
            index_revision,
            reason: None,
        },
        &Act {
            actor: author,
            at: &at,
            note: None,
            detail: None,
        },
    )?;
    let mut receipt = receipt(store, "proposed", &id, Some(1), Some(digest))?;
    receipt.notes = notes;
    receipt.notes.push(Note {
        code: "review_required",
        detail: format!("pending review; approve only after reading it: repogi memory show {id}"),
    });
    Ok(receipt)
}

/// Propose a corrected revision of an approved memory. The approved revision
/// stays authoritative, and its evidence snapshot unchanged, until review.
pub fn correct(
    store: &mut Store,
    id: &str,
    draft: &Draft,
    reason: &str,
    author: &str,
    expected_version: Option<u64>,
) -> Result<Receipt> {
    validate_id(id)?;
    validate_actor(author)?;
    text("reason", reason, MAX_NOTE_BYTES)?;
    let Some(memory) = store.memory(id)? else {
        return fail(
            "memory_not_found",
            format!("no memory {id} in this repository"),
        );
    };
    if draft.memory_type != memory.memory_type {
        return fail(
            "invalid_arguments",
            format!(
                "{id} is a {}; a correction keeps the type",
                memory.memory_type.label()
            ),
        );
    }
    let (assertion, evidence) = capture_all(store, draft)?;
    let digest = revision_digest(
        draft.memory_type,
        &draft.title,
        &draft.claim,
        assertion.as_ref(),
        &evidence,
    );
    let index_revision = store.revision()?;
    let at = utc_now();
    let revision = store.memory_correct(
        id,
        expected_version,
        &NewRevision {
            memory_type: draft.memory_type,
            title: &draft.title,
            claim: &draft.claim,
            assertion: assertion.as_ref(),
            evidence: &evidence,
            digest: &digest,
            index_revision,
            reason: Some(reason),
        },
        &Act {
            actor: author,
            at: &at,
            note: Some(reason),
            detail: None,
        },
    )?;
    let mut receipt = receipt(
        store,
        "correction_proposed",
        id,
        Some(revision),
        Some(digest),
    )?;
    receipt.notes.push(Note {
        code: "review_required",
        detail: format!("revision {revision} is pending; the approved revision stays authoritative until it is reviewed"),
    });
    Ok(receipt)
}

/// Approve or reject the pending revision named by its digest. Approval also
/// requires every backing evidence item to verify `current` right now.
pub fn review(
    store: &mut Store,
    id: &str,
    digest: &str,
    decision: Decision,
    reviewer: &str,
    note: Option<&str>,
) -> Result<Receipt> {
    validate_id(id)?;
    validate_actor(reviewer)?;
    optional_text("note", note)?;
    if digest.len() < 12
        || digest.len() > 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return fail(
            "digest_mismatch",
            "pass at least 12 lowercase hex characters of the pending revision digest",
        );
    }
    let Some(memory) = store.memory(id)? else {
        return fail(
            "memory_not_found",
            format!("no memory {id} in this repository"),
        );
    };
    let mut state = None;
    let mut detail = std::collections::BTreeMap::new();
    if decision == Decision::Approve {
        let Some(pending) = memory.pending_revision else {
            return fail(
                "nothing_to_review",
                format!("{id} has no revision awaiting review"),
            );
        };
        let check = approval_check(store, id, pending)?;
        detail.insert("verification".to_owned(), check.state.label().to_owned());
        detail.insert(
            "index_revision".to_owned(),
            check.index_revision.to_string(),
        );
        state = Some(check.state);
    }
    let at = utc_now();
    let revision = store.memory_review(
        id,
        digest,
        decision,
        &Act {
            actor: reviewer,
            at: &at,
            note,
            detail: (!detail.is_empty()).then_some(detail),
        },
    )?;
    let full = store.memory_revision(id, revision)?.map(|r| r.digest);
    let mut receipt = receipt(
        store,
        match decision {
            Decision::Approve => "approved",
            Decision::Reject => "rejected",
        },
        id,
        Some(revision),
        full,
    )?;
    receipt.verification = state;
    Ok(receipt)
}

/// Approval re-verifies the pending revision: it needs backing evidence, and
/// every backing item must still be current.
fn approval_check(store: &Store, id: &str, pending: u32) -> Result<MemoryCheck> {
    let revision = store
        .memory_revision(id, pending)?
        .ok_or_else(|| anyhow::anyhow!("pending revision missing"))?;
    if !revision
        .evidence
        .iter()
        .any(|e| e.role == EvidenceRole::Backing && e.class.can_back())
    {
        return fail(
            "evidence_required",
            format!("{id} revision {pending} has no backing evidence; it cannot be approved"),
        );
    }
    let mut checker = verify::Checker::new(store)?;
    let check = checker.revision(&revision)?;
    let changed: Vec<String> = check
        .evidence
        .iter()
        .filter(|c| {
            revision.evidence[c.ordinal].role == EvidenceRole::Backing && c.state != State::Current
        })
        .map(|c| {
            format!(
                "#{} {} ({})",
                c.ordinal + 1,
                clean(&revision.evidence[c.ordinal].selector),
                c.state.label()
            )
        })
        .collect();
    if !changed.is_empty() {
        return fail(
            "evidence_not_current",
            format!(
                "backing evidence changed since it was captured: {}; reject this revision or propose a correction with current evidence",
                changed.join(", ")
            ),
        );
    }
    Ok(check)
}

pub fn supersede(
    store: &mut Store,
    old: &str,
    new: &str,
    reviewer: &str,
    reason: &str,
) -> Result<Receipt> {
    validate_id(old)?;
    validate_id(new)?;
    validate_actor(reviewer)?;
    text("reason", reason, MAX_NOTE_BYTES)?;
    let at = utc_now();
    store.memory_supersede(
        old,
        new,
        &Act {
            actor: reviewer,
            at: &at,
            note: Some(reason),
            detail: None,
        },
    )?;
    let mut receipt = receipt(store, "superseded", old, None, None)?;
    receipt.notes.push(Note {
        code: "superseded_by",
        detail: format!(
            "{old} is kept with its history and is no longer authoritative; {new} replaces it"
        ),
    });
    Ok(receipt)
}

pub fn retire(store: &mut Store, id: &str, reviewer: &str, reason: &str) -> Result<Receipt> {
    validate_id(id)?;
    validate_actor(reviewer)?;
    text("reason", reason, MAX_NOTE_BYTES)?;
    let at = utc_now();
    store.memory_retire(
        id,
        &Act {
            actor: reviewer,
            at: &at,
            note: Some(reason),
            detail: None,
        },
    )?;
    receipt(store, "retired", id, None, None)
}

/// Shared response header.
#[derive(Debug, Clone, Serialize)]
pub struct Header {
    pub schema_version: u32,
    pub kind: &'static str,
    pub repository_root: Option<String>,
    pub index_revision: u64,
    pub notice: &'static str,
}

fn header(store: &Store, kind: &'static str) -> Result<Header> {
    Ok(Header {
        schema_version: SCHEMA_VERSION,
        kind,
        repository_root: store.root()?,
        index_revision: store.revision()?,
        notice: MEMORY_NOTICE,
    })
}

/// A memory as seen through one revision: the authoritative (approved)
/// revision when there is one, otherwise the pending proposal.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub id: String,
    pub memory_type: MemoryType,
    pub status: MemoryStatus,
    /// Approved, not superseded or retired, shown at its approved revision.
    pub authoritative: bool,
    pub revision: u32,
    pub revision_state: RevisionState,
    pub title: String,
    pub claim: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion: Option<Assertion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_revision: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    pub updated_at: String,
    pub version: u64,
}

fn summary(memory: &Memory, revision: &Revision) -> Summary {
    Summary {
        id: memory.id.clone(),
        memory_type: memory.memory_type,
        status: memory.status,
        authoritative: memory.status == MemoryStatus::Approved
            && memory.approved_revision == Some(revision.revision),
        revision: revision.revision,
        revision_state: revision.state,
        title: revision.title.clone(),
        claim: revision.claim.clone(),
        assertion: revision.assertion.clone(),
        pending_revision: memory.pending_revision,
        superseded_by: memory.superseded_by.clone(),
        updated_at: memory.updated_at.clone(),
        version: memory.version,
    }
}

/// The revision a summary shows: approved if any, else pending, else latest.
fn shown_revision(memory: &Memory) -> u32 {
    memory
        .approved_revision
        .or(memory.pending_revision)
        .unwrap_or(memory.latest_revision)
}

fn load(store: &Store, id: &str) -> Result<Memory> {
    validate_id(id)?;
    store.memory(id)?.map_or_else(
        || {
            fail(
                "memory_not_found",
                format!("no memory {id} in this repository"),
            )
        },
        Ok,
    )
}

fn revision(store: &Store, id: &str, number: u32) -> Result<Revision> {
    store
        .memory_revision(id, number)?
        .ok_or_else(|| anyhow::anyhow!("memory {id} revision {number} is missing"))
}

/// Warnings that keep non-authoritative or changed memories from reading as fact.
fn warnings(memory: &Memory, shown: &Revision, check: Option<&MemoryCheck>) -> Vec<Note> {
    let mut notes = Vec::new();
    match memory.status {
        MemoryStatus::PendingReview => notes.push(Note {
            code: "unreviewed",
            detail: "pending review; not an approved project memory".into(),
        }),
        MemoryStatus::Rejected => notes.push(Note {
            code: "rejected",
            detail: "rejected by review; not project knowledge".into(),
        }),
        MemoryStatus::Superseded => notes.push(Note {
            code: "superseded",
            detail: format!(
                "superseded by {}; historical only",
                memory.superseded_by.as_deref().unwrap_or("another memory")
            ),
        }),
        MemoryStatus::Retired => notes.push(Note {
            code: "retired",
            detail: "retired by review; historical only".into(),
        }),
        MemoryStatus::Approved => {}
    }
    if shown.state == RevisionState::Proposed && memory.approved_revision.is_some() {
        notes.push(Note {
            code: "unreviewed",
            detail: "this is a pending correction, not the approved claim".into(),
        });
    } else if memory.pending_revision.is_some() && memory.status == MemoryStatus::Approved {
        notes.push(Note {
            code: "correction_pending",
            detail: format!(
                "revision {} is awaiting review",
                memory.pending_revision.unwrap_or_default()
            ),
        });
    }
    if let Some(check) = check {
        match check.state {
            State::Current => {}
            State::Unverified => notes.push(Note { code: "evidence_unverified", detail: "some backing evidence could not be checked; treat the claim as unconfirmed".into() }),
            state => notes.push(Note {
                code: "stale_evidence",
                detail: format!(
                    "backing evidence is {} since approval ({}); current code and documents take precedence until the memory is reviewed",
                    state.label(),
                    verify::changed_paths(check, shown).join(", ")
                ),
            }),
        }
    }
    notes
}

#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    #[serde(flatten)]
    pub header: Header,
    pub statuses: Vec<MemoryStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<MemoryType>,
    pub memories: Vec<Summary>,
    /// Pass as `--after` for the next page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
    pub truncated: bool,
    pub max_bytes: usize,
    pub used_bytes: usize,
}

/// Bounded listing without verification. Default: every status.
pub fn list(
    store: &Store,
    statuses: Vec<MemoryStatus>,
    memory_type: Option<MemoryType>,
    after: Option<String>,
    limit: usize,
) -> Result<Listing> {
    if let Some(after) = &after {
        validate_id(after)?;
    }
    let limit = limit.clamp(1, MAX_LIMIT);
    let mut rows = store.memories(
        &MemoryFilter {
            statuses: statuses.clone(),
            memory_type,
            after,
        },
        u32::try_from(limit + 1)?,
    )?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let mut memories = Vec::new();
    for memory in &rows {
        store.check_cancelled()?;
        let shown = revision(store, &memory.id, shown_revision(memory))?;
        memories.push(summary(memory, &shown));
    }
    Ok(Listing {
        header: header(store, "memory_list")?,
        statuses,
        memory_type,
        next_after: more.then(|| rows.last().map(|m| m.id.clone())).flatten(),
        memories,
        truncated: false,
        max_bytes: 0,
        used_bytes: 0,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceView {
    pub ordinal: usize,
    #[serde(flatten)]
    pub snapshot: EvidenceSnapshot,
    pub state: State,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Note>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<code_intel_core::relationships::SourceRange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevisionView {
    pub revision: u32,
    pub state: RevisionState,
    pub digest: String,
    pub title: String,
    pub claim: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion: Option<Assertion>,
    pub index_revision: u64,
    pub created_at: String,
    pub proposed_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Live, not persisted: worst backing state.
    pub verification: State,
    pub evidence: Vec<EvidenceView>,
}

fn revision_view(revision: &Revision, check: &MemoryCheck) -> RevisionView {
    RevisionView {
        revision: revision.revision,
        state: revision.state,
        digest: revision.digest.clone(),
        title: revision.title.clone(),
        claim: revision.claim.clone(),
        assertion: revision.assertion.clone(),
        index_revision: revision.index_revision,
        created_at: revision.created_at.clone(),
        proposed_by: revision.proposed_by.clone(),
        reason: revision.reason.clone(),
        verification: check.state,
        evidence: check
            .evidence
            .iter()
            .map(|c| EvidenceView {
                ordinal: c.ordinal + 1,
                snapshot: revision.evidence[c.ordinal].clone(),
                state: c.state,
                changes: c.changes.clone(),
                current: c.current.clone(),
            })
            .collect(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub header: Header,
    pub memory: Summary,
    pub warnings: Vec<Note>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved: Option<RevisionView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<RevisionView>,
    /// A rejected first proposal: reviewed, never approved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed: Option<RevisionView>,
    pub check_limit_reached: bool,
    pub truncated: bool,
    pub max_bytes: usize,
    pub used_bytes: usize,
}

/// One memory with its approved and pending revisions, verified live.
pub fn show(store: &Store, id: &str) -> Result<Detail> {
    let memory = load(store, id)?;
    let mut checker = verify::Checker::new(store)?;
    let shown = revision(store, id, shown_revision(&memory))?;
    let mut approved = None;
    let mut approved_check = None;
    if let Some(number) = memory.approved_revision {
        let revision = revision(store, id, number)?;
        let check = checker.revision(&revision)?;
        approved = Some(revision_view(&revision, &check));
        approved_check = Some(check);
    }
    let pending = match memory.pending_revision {
        Some(number) => {
            let revision = revision(store, id, number)?;
            let check = checker.revision(&revision)?;
            if approved_check.is_none() {
                approved_check = Some(check.clone());
            }
            Some(revision_view(&revision, &check))
        }
        None => None,
    };
    let mut reviewed = None;
    if approved.is_none() && pending.is_none() {
        // A rejected first proposal: the reviewed revision, verified live.
        let check = checker.revision(&shown)?;
        reviewed = Some(revision_view(&shown, &check));
        approved_check = Some(check);
    }
    Ok(Detail {
        header: header(store, "memory")?,
        warnings: warnings(&memory, &shown, approved_check.as_ref()),
        memory: summary(&memory, &shown),
        approved,
        pending,
        reviewed,
        check_limit_reached: checker.limit_reached,
        truncated: false,
        max_bytes: 0,
        used_bytes: 0,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct RevisionLine {
    pub revision: u32,
    pub state: RevisionState,
    pub digest: String,
    pub title: String,
    pub claim: String,
    pub created_at: String,
    pub proposed_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub evidence: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct History {
    #[serde(flatten)]
    pub header: Header,
    pub id: String,
    pub status: MemoryStatus,
    /// Newest first.
    pub revisions: Vec<RevisionLine>,
    /// Oldest first; append-only.
    pub events: Vec<MemoryEvent>,
    pub omitted_revisions: usize,
    pub omitted_events: usize,
    pub truncated: bool,
    pub max_bytes: usize,
    pub used_bytes: usize,
}

/// Every revision (claims are never rewritten) and the append-only review log.
pub fn history(store: &Store, id: &str, limit: usize) -> Result<History> {
    let memory = load(store, id)?;
    let limit = limit.clamp(1, MAX_HISTORY);
    let revisions = store.memory_revisions(id, u32::try_from(limit)?)?;
    let total_events = usize::try_from(store.memory_event_count(id)?)?;
    let events = store.memory_events(id, 0, u32::try_from(limit)?)?;
    Ok(History {
        header: header(store, "memory_history")?,
        id: memory.id.clone(),
        status: memory.status,
        omitted_revisions: usize::try_from(memory.latest_revision)?.saturating_sub(revisions.len()),
        omitted_events: total_events.saturating_sub(events.len()),
        revisions: revisions
            .into_iter()
            .map(|r| RevisionLine {
                revision: r.revision,
                state: r.state,
                digest: r.digest,
                title: r.title,
                claim: r.claim,
                created_at: r.created_at,
                proposed_by: r.proposed_by,
                reason: r.reason,
                evidence: r.evidence.len(),
            })
            .collect(),
        events,
        truncated: false,
        max_bytes: 0,
        used_bytes: 0,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Citation {
    pub class: code_intel_core::memory::MemoryEvidenceClass,
    pub role: EvidenceRole,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit_id: Option<String>,
    pub state: State,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub rank: usize,
    /// `all_terms`, or `partial_terms` when fewer memories cover every term.
    pub match_mode: &'static str,
    #[serde(flatten)]
    pub summary: Summary,
    pub verification: State,
    pub explicit_ids: Vec<String>,
    pub citations: Vec<Citation>,
    pub warnings: Vec<Note>,
}

// Independent result flags, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize)]
pub struct Search {
    #[serde(flatten)]
    pub header: Header,
    pub query: String,
    pub statuses: Vec<MemoryStatus>,
    pub include_unreviewed: bool,
    pub results: Vec<Hit>,
    pub omitted_results: usize,
    pub candidate_limit_reached: bool,
    pub check_limit_reached: bool,
    pub truncated: bool,
    pub max_bytes: usize,
    pub used_bytes: usize,
}

/// FTS5 over memory titles, claims and cited IDs/paths. Default: approved
/// memories only. Others are labelled and never authoritative.
pub fn search(
    store: &Store,
    query: &str,
    statuses: Vec<MemoryStatus>,
    include_unreviewed: bool,
    limit: usize,
) -> Result<Search> {
    if query.len() > MAX_QUERY_BYTES || query.chars().any(char::is_control) {
        return fail(
            "invalid_arguments",
            format!("query must be at most {MAX_QUERY_BYTES} bytes without control characters"),
        );
    }
    let statuses = if statuses.is_empty() {
        vec![MemoryStatus::Approved]
    } else {
        statuses
    };
    let limit = limit.clamp(1, MAX_LIMIT);
    let expression = crate::knowledge::expression(query).map_err(|error| MemoryError {
        code: "invalid_arguments",
        message: format!("{error:#}"),
    })?;
    let candidates = MAX_SEARCH_CANDIDATES.max(limit);
    // Memories covering every term first; partial matches only fill the rest.
    let conjunction = expression.replace(" OR ", " AND ");
    let mut hits: Vec<(code_intel_store::MemoryHit, &'static str)> = store
        .memory_search(
            &conjunction,
            &statuses,
            include_unreviewed,
            u32::try_from(candidates + 1)?,
        )?
        .into_iter()
        .map(|hit| (hit, "all_terms"))
        .collect();
    if hits.len() <= candidates && expression != conjunction {
        let partial = format!("({expression}) NOT ({conjunction})");
        let remaining = candidates + 1 - hits.len();
        hits.extend(
            store
                .memory_search(
                    &partial,
                    &statuses,
                    include_unreviewed,
                    u32::try_from(remaining)?,
                )?
                .into_iter()
                .map(|hit| (hit, "partial_terms")),
        );
    }
    let candidate_limit_reached = hits.len() > candidates;
    hits.truncate(candidates);
    let total = hits.len();
    let mut checker = verify::Checker::new(store)?;
    let mut results = Vec::new();
    for (hit, mode) in hits.into_iter().take(limit) {
        store.check_cancelled()?;
        let memory = load(store, &hit.memory_id)?;
        let shown = revision(store, &hit.memory_id, hit.revision)?;
        let check = checker.revision(&shown)?;
        let mut view = hit_view(results.len() + 1, &memory, &shown, &check);
        view.match_mode = mode;
        results.push(view);
    }
    Ok(Search {
        header: header(store, "memory_search")?,
        query: query.to_owned(),
        statuses,
        include_unreviewed,
        omitted_results: total - results.len(),
        results,
        candidate_limit_reached,
        check_limit_reached: checker.limit_reached,
        truncated: false,
        max_bytes: 0,
        used_bytes: 0,
    })
}

fn hit_view(rank: usize, memory: &Memory, shown: &Revision, check: &MemoryCheck) -> Hit {
    let mut explicit_ids: Vec<String> = Vec::new();
    for evidence in &shown.evidence {
        if let Some(id) = &evidence.explicit_id
            && !explicit_ids.contains(id)
        {
            explicit_ids.push(id.clone());
        }
    }
    Hit {
        rank,
        match_mode: "all_terms",
        summary: summary(memory, shown),
        verification: check.state,
        explicit_ids,
        citations: check
            .evidence
            .iter()
            .map(|c| {
                let e = &shown.evidence[c.ordinal];
                Citation {
                    class: e.class,
                    role: e.role,
                    path: e.citation.path.clone(),
                    start_line: e.citation.start_line,
                    end_line: e.citation.end_line,
                    explicit_id: e.explicit_id.clone(),
                    state: c.state,
                }
            })
            .collect(),
        warnings: warnings(memory, shown, Some(check)),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifiedMemory {
    pub id: String,
    pub status: MemoryStatus,
    pub revision: u32,
    pub verification: State,
    pub evidence: Vec<EvidenceCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verification {
    #[serde(flatten)]
    pub header: Header,
    /// True only for `--record`: a `verified` history event was appended.
    pub recorded: bool,
    pub results: Vec<VerifiedMemory>,
    pub omitted: usize,
    pub check_limit_reached: bool,
    pub truncated: bool,
    pub max_bytes: usize,
    pub used_bytes: usize,
}

/// Verify the authoritative (else pending) revision of each memory. Read-only.
pub fn verify(
    store: &Store,
    ids: &[String],
    all_approved: bool,
    limit: usize,
) -> Result<Verification> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let mut memories = Vec::new();
    let mut omitted = 0;
    if all_approved {
        let rows = store.memories(
            &MemoryFilter {
                statuses: vec![MemoryStatus::Approved],
                ..MemoryFilter::default()
            },
            u32::try_from(limit + 1)?,
        )?;
        omitted = rows.len().saturating_sub(limit);
        memories.extend(rows.into_iter().take(limit));
    }
    for id in ids.iter().take(limit) {
        memories.push(load(store, id)?);
    }
    omitted += ids.len().saturating_sub(limit);
    let mut checker = verify::Checker::new(store)?;
    let mut results = Vec::new();
    for memory in memories {
        store.check_cancelled()?;
        let shown = revision(store, &memory.id, shown_revision(&memory))?;
        let check = checker.revision(&shown)?;
        results.push(VerifiedMemory {
            id: memory.id,
            status: memory.status,
            revision: shown.revision,
            verification: check.state,
            evidence: check.evidence,
        });
    }
    Ok(Verification {
        header: header(store, "memory_verification")?,
        recorded: false,
        results,
        omitted,
        check_limit_reached: checker.limit_reached,
        truncated: false,
        max_bytes: 0,
        used_bytes: 0,
    })
}

/// Append each result to its memory's history as a `verified` event. This is
/// an explicit write; it never changes a claim, status or evidence snapshot.
pub fn record_verification(
    store: &mut Store,
    verification: &mut Verification,
    actor: &str,
) -> Result<()> {
    validate_actor(actor)?;
    let at = utc_now();
    for result in &verification.results {
        let mut detail = std::collections::BTreeMap::new();
        detail.insert(
            "verification".to_owned(),
            result.verification.label().to_owned(),
        );
        detail.insert("revision".to_owned(), result.revision.to_string());
        detail.insert(
            "index_revision".to_owned(),
            verification.header.index_revision.to_string(),
        );
        let changed: Vec<String> = result
            .evidence
            .iter()
            .filter(|c| c.state != State::Current)
            .map(|c| format!("#{}:{}", c.ordinal + 1, c.state.label()))
            .collect();
        if !changed.is_empty() {
            detail.insert("changed_evidence".to_owned(), changed.join(" "));
        }
        store.memory_record_verification(
            &result.id,
            &Act {
                actor,
                at: &at,
                note: None,
                detail: Some(detail),
            },
        )?;
    }
    verification.recorded = true;
    Ok(())
}

/// What an investigation offers memory recall.
pub(crate) struct Relevance<'a> {
    /// Plan identities (entity, fact and author IDs) in plan priority order.
    pub subjects: &'a [String],
    /// The task named identities explicitly; only those are offered then.
    pub selected: bool,
    pub terms: &'a [String],
}

/// A recalled memory and why it was recalled.
pub(crate) struct Recall {
    pub memory: Memory,
    pub revision: Revision,
    pub check: MemoryCheck,
    pub rule: &'static str,
}

/// Approved memories ranked for an investigation. A memory qualifies by
/// citing a plan identity together with at least one task term (no term is
/// needed for explicitly selected identities), or by matching every task term.
/// Identity and text together rank first, then identity alone by plan
/// priority, then text alone. Bounded candidates; read-only.
pub(crate) fn relevant(
    store: &Store,
    relevance: &Relevance<'_>,
    limit: usize,
) -> Result<Vec<Recall>> {
    const CANDIDATES: u32 = 32;
    let mut ids: Vec<String> = store.memories_citing(relevance.subjects, CANDIDATES)?;
    if !relevance.terms.is_empty() {
        let any = relevance
            .terms
            .iter()
            .map(|t| format!("\"{t}\"*"))
            .collect::<Vec<_>>()
            .join(" OR ");
        for hit in store.memory_search(&any, &[MemoryStatus::Approved], false, CANDIDATES)? {
            if !ids.contains(&hit.memory_id) {
                ids.push(hit.memory_id);
            }
        }
    }
    let mut ranked = Vec::new();
    for id in ids {
        store.check_cancelled()?;
        let memory = load(store, &id)?;
        let Some(number) = memory.approved_revision else {
            continue;
        };
        let shown = revision(store, &id, number)?;
        let position = shown
            .evidence
            .iter()
            .flat_map(|e| {
                [
                    Some(&e.subject_id),
                    e.explicit_id.as_ref(),
                    e.relationship.as_ref().and_then(|r| r.target_id.as_ref()),
                ]
            })
            .flatten()
            .filter_map(|s| relevance.subjects.iter().position(|p| p == s))
            .min();
        let words: Vec<String> = format!("{} {}", shown.title, shown.claim)
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        let matched = relevance
            .terms
            .iter()
            .filter(|t| words.iter().any(|w| w.starts_with(t.as_str())))
            .count();
        let all = !relevance.terms.is_empty() && matched == relevance.terms.len();
        let (tier, rule) = match position {
            Some(_) if all => (0, "evidence_cites_plan_identity; all task terms"),
            Some(_) if relevance.selected || relevance.terms.is_empty() || matched > 0 => {
                (1, "evidence_cites_plan_identity")
            }
            _ if all => (2, "all_task_terms_without_identity_link"),
            _ => continue,
        };
        ranked.push((
            tier,
            position.unwrap_or(usize::MAX),
            std::cmp::Reverse(matched),
            memory,
            shown,
            rule,
        ));
    }
    ranked.sort_by(|a, b| (a.0, a.1, a.2, &a.3.id).cmp(&(b.0, b.1, b.2, &b.3.id)));
    let mut checker = verify::Checker::new(store)?;
    let mut found = Vec::new();
    for (_, _, _, memory, revision, rule) in ranked.into_iter().take(limit) {
        let check = checker.revision(&revision)?;
        found.push(Recall {
            memory,
            revision,
            check,
            rule,
        });
    }
    Ok(found)
}
