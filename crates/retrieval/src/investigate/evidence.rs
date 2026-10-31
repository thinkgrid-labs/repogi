//! The shared evidence ledger: content-addressed evidence, the targets it is
//! about, per-target support and uncertainty, and selected-file freshness.
//! Evidence classes are recorded separately and never folded into a score.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use code_intel_core::{
    Fact,
    relationships::{EntityKind, KnowledgeEntity, KnowledgeRelationship, SourceRange},
    semantic::SemanticEdge,
};
use code_intel_store::Store;
use serde::Serialize;

use super::{
    Evidence, EvidenceClass, Freshness, MAX_EVIDENCE, NAME_BYTES, Relation, Role, Step, Target,
    Uncertainty,
};

pub(crate) fn label<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Untrusted names are kept as data: controls removed, length capped.
pub(crate) fn clean(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if out.len() > NAME_BYTES {
        let mut end = NAME_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out
}

pub(super) fn short_hash(parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex()[..12].to_owned()
}

pub(crate) fn fact_range(fact: &Fact) -> SourceRange {
    SourceRange {
        path: fact.path.clone(),
        start_byte: fact.start_byte,
        end_byte: fact.end_byte,
        start_line: fact.start_line,
        end_line: fact.end_line,
    }
}

pub(crate) fn edge_range(edge: &SemanticEdge) -> SourceRange {
    SourceRange {
        path: edge.location.path.clone(),
        start_byte: edge.location.start_byte,
        end_byte: edge.location.end_byte,
        start_line: edge.location.start_line,
        end_line: edge.location.end_line,
    }
}

pub(super) fn claim_relation(edge: &KnowledgeRelationship) -> Relation {
    Relation {
        id: Some(edge.id.clone()),
        kind: label(&edge.kind),
        source_id: edge.source_id.clone(),
        target_id: edge.target_id.clone(),
        target_text: clean(&edge.target),
        status: label(&edge.status),
    }
}

/// Evidence and targets of one request. Steps refer to both by ID only.
#[derive(Default)]
pub(super) struct Ledger {
    pub evidence: BTreeMap<String, Evidence>,
    pub targets: BTreeMap<String, Target>,
    /// Target -> supporting evidence, strongest-first where it matters.
    pub support: BTreeMap<String, Vec<String>>,
    /// Target -> uncertainty that travels with every step naming it.
    pub notes: BTreeMap<String, Vec<Uncertainty>>,
    /// Request-level uncertainty.
    pub global: Vec<Uncertainty>,
    /// `MAX_EVIDENCE` refused at least one new item.
    pub full: bool,
}

impl Ledger {
    pub fn global(&mut self, code: &'static str, detail: String, evidence: Vec<String>) {
        self.global.push(Uncertainty {
            code,
            detail,
            evidence,
        });
    }

    pub fn note(
        &mut self,
        target: &str,
        code: &'static str,
        detail: String,
        evidence: Vec<String>,
    ) {
        let notes = self.notes.entry(target.to_owned()).or_default();
        if !notes.iter().any(|n| n.code == code && n.detail == detail) {
            notes.push(Uncertainty {
                code,
                detail,
                evidence,
            });
        }
    }

    /// Record evidence under its content-derived ID; `None` once the ledger is full.
    pub fn add(
        &mut self,
        class: EvidenceClass,
        subject: &str,
        rule: String,
        citation: SourceRange,
        relationship: Option<Relation>,
        depth: Option<usize>,
    ) -> Option<String> {
        let relation_key = relationship.as_ref().map_or_else(String::new, |r| {
            format!(
                "{}|{}|{}|{}|{}",
                r.id.as_deref().unwrap_or(""),
                r.kind,
                r.source_id,
                r.target_id.as_deref().unwrap_or(""),
                r.status
            )
        });
        let id = format!(
            "ev-{}",
            short_hash(&[
                &label(&class),
                subject,
                &rule,
                &citation.path,
                &citation.start_byte.to_string(),
                &citation.end_byte.to_string(),
                &relation_key,
            ])
        );
        if self.evidence.contains_key(&id) {
            return Some(id);
        }
        if self.evidence.len() >= MAX_EVIDENCE {
            self.full = true;
            return None;
        }
        self.evidence.insert(
            id.clone(),
            Evidence {
                id: id.clone(),
                class,
                subject: subject.to_owned(),
                rule,
                citation,
                relationship,
                depth,
                freshness: "unverified".into(),
            },
        );
        Some(id)
    }

    pub fn support(&mut self, target: &str, evidence: Option<String>) {
        if let Some(evidence) = evidence {
            let list = self.support.entry(target.to_owned()).or_default();
            if !list.contains(&evidence) {
                list.push(evidence);
            }
        }
    }

    /// A code target keeps its strongest (lowest) role across discoveries.
    pub fn fact_target(&mut self, fact: &Fact, role: Role) {
        let target = self
            .targets
            .entry(fact.id.clone())
            .or_insert_with(|| Target {
                id: fact.id.clone(),
                kind: fact.kind.clone(),
                role,
                name: clean(&fact.name),
                explicit_id: None,
                decision_status: None,
                location: fact_range(fact),
                freshness: "unverified".into(),
            });
        target.role = target.role.min(role);
    }

    pub fn entity_target(&mut self, entity: &KnowledgeEntity) -> Role {
        let (kind, role) = match entity.kind {
            EntityKind::Requirement => ("requirement", Role::Requirement),
            EntityKind::ArchitectureDecision => ("architecture_decision", Role::Decision),
            EntityKind::Document => ("document", Role::Document),
            EntityKind::DocumentSection => ("document_section", Role::Document),
            EntityKind::CodeFile => ("file", Role::Implementation),
            EntityKind::CodeSymbol => ("code_symbol", Role::Implementation),
        };
        self.targets
            .entry(entity.id.clone())
            .or_insert_with(|| Target {
                id: entity.id.clone(),
                kind: kind.into(),
                role,
                name: clean(&entity.title),
                explicit_id: entity.explicit_id.clone(),
                decision_status: entity.decision_status.as_deref().map(clean),
                location: entity.source.clone(),
                freshness: "unverified".into(),
            });
        role
    }

    /// The strongest tier among `evidence`; 7 when none is recorded.
    pub fn tier(&self, evidence: &[String]) -> u8 {
        evidence
            .iter()
            .filter_map(|id| self.evidence.get(id))
            .map(|e| e.class.tier())
            .min()
            .unwrap_or(7)
    }

    pub fn strongest(&self, evidence: &[String]) -> String {
        evidence
            .iter()
            .filter_map(|id| self.evidence.get(id))
            .map(|e| e.class)
            .min()
            .map_or_else(|| "no evidence".into(), |c| label(&c).replace('_', " "))
    }
}

/// Check the files cited by the returned steps (the 32-file cap is spent on
/// steps in order), then propagate the result to targets, evidence and steps.
pub(super) fn refresh(store: &Store, ledger: &mut Ledger, steps: &mut [Step]) -> Result<Freshness> {
    let mut paths = Vec::new();
    for step in steps.iter() {
        for t in &step.targets {
            paths.push(ledger.targets[t].location.path.clone());
        }
        for e in &step.evidence {
            paths.push(ledger.evidence[e].citation.path.clone());
        }
    }
    let inspection = crate::signal::Inspection::new(store, paths.clone())?;
    let mut checked = BTreeSet::new();
    let mut unchecked = BTreeSet::new();
    let mut stale = BTreeSet::new();
    let mut missing = BTreeSet::new();
    for path in &paths {
        match inspection.source_state(path) {
            "matched_at_read" => checked.insert(path.clone()),
            "stale" => {
                stale.insert(path.clone());
                checked.insert(path.clone())
            }
            "missing" => {
                missing.insert(path.clone());
                checked.insert(path.clone())
            }
            _ => unchecked.insert(path.clone()),
        };
    }
    for target in ledger.targets.values_mut() {
        target.freshness = inspection.source_state(&target.location.path).into();
    }
    for evidence in ledger.evidence.values_mut() {
        let source = inspection.source_state(&evidence.citation.path);
        let target = evidence
            .relationship
            .as_ref()
            .and_then(|r| r.target_id.as_ref())
            .and_then(|t| ledger.targets.get(t))
            .map(|t| inspection.source_state(&t.location.path));
        evidence.freshness = if matches!(source, "stale" | "missing")
            || target.is_some_and(|s| matches!(s, "stale" | "missing"))
            || evidence.class == EvidenceClass::StaleReference
        {
            "stale"
        } else if source == "matched_at_read" && target.is_none_or(|s| s == "matched_at_read") {
            "matched_at_read"
        } else {
            "unverified"
        }
        .into();
    }
    for step in steps.iter_mut() {
        flag_stale_targets(&ledger.targets, step);
    }
    Ok(Freshness {
        evidence_source: "indexed_snapshot",
        selected_sources: if !stale.is_empty() || !missing.is_empty() {
            "stale"
        } else if !checked.is_empty() && unchecked.is_empty() {
            "matched_at_read"
        } else {
            "unverified"
        },
        checked_files: checked.len(),
        unchecked_files: unchecked.len(),
        stale_files: stale.into_iter().collect(),
        missing_files: missing.into_iter().collect(),
        graph: "unverified_other_files_and_resolution",
        inventory: "unverified_added_removed_or_other_files",
    })
}

/// Stale targets add `stale_source` and drop their read ranges; unchecked
/// targets add `unverified_source`.
fn flag_stale_targets(targets: &BTreeMap<String, Target>, step: &mut Step) {
    let stale_targets: Vec<&Target> = step
        .targets
        .iter()
        .map(|t| &targets[t])
        .filter(|t| matches!(t.freshness.as_str(), "stale" | "missing"))
        .collect();
    if !stale_targets.is_empty() {
        let names: Vec<String> = stale_targets
            .iter()
            .map(|t| format!("{} ({})", t.location.path, t.freshness))
            .collect();
        step.uncertainty.push(Uncertainty {
            code: "stale_source",
            detail: format!(
                "indexed locations changed or vanished since indexing: {}; read the current file and run `repogi index` before relying on these ranges",
                names.join(", ")
            ),
            evidence: vec![],
        });
        for next in &mut step.next {
            let changed = next
                .path
                .as_ref()
                .is_some_and(|p| stale_targets.iter().any(|t| &t.location.path == p));
            if next.tool == "read" && changed {
                next.start_line = None;
                next.end_line = None;
            }
        }
    }
    if step
        .targets
        .iter()
        .any(|t| targets[t].freshness == "unverified")
    {
        step.uncertainty.push(Uncertainty {
            code: "unverified_source",
            detail: "some target bytes were not checked against the working tree (selected-file check cap or unreadable)".into(),
            evidence: vec![],
        });
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    fn range(path: &str) -> SourceRange {
        SourceRange {
            path: path.into(),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
        }
    }

    #[test]
    fn untrusted_names_are_control_free_and_capped() {
        let name = clean(&format!("a\u{1b}[31m{}", "x".repeat(400)));
        assert!(!name.chars().any(char::is_control));
        assert!(name.len() <= NAME_BYTES + '…'.len_utf8());
    }

    #[test]
    fn evidence_identity_is_content_derived() {
        assert_eq!(short_hash(&["a", "b"]), short_hash(&["a", "b"]));
        assert_ne!(short_hash(&["ab", ""]), short_hash(&["a", "b"]));
    }

    #[test]
    fn ledger_deduplicates_by_content_and_reports_capacity() {
        let mut ledger = Ledger::default();
        let rule = || "rule".to_owned();
        let first = ledger.add(
            EvidenceClass::LexicalCandidate,
            "s",
            rule(),
            range("a.ts"),
            None,
            None,
        );
        let again = ledger.add(
            EvidenceClass::LexicalCandidate,
            "s",
            rule(),
            range("a.ts"),
            None,
            None,
        );
        assert_eq!(first, again);
        let other = ledger.add(
            EvidenceClass::RecordedIntent,
            "s",
            rule(),
            range("a.ts"),
            None,
            None,
        );
        assert_ne!(first, other, "the class is part of the identity");
        for n in 0..MAX_EVIDENCE {
            ledger.add(
                EvidenceClass::LexicalCandidate,
                &n.to_string(),
                rule(),
                range("b.ts"),
                None,
                None,
            );
        }
        assert_eq!(ledger.evidence.len(), MAX_EVIDENCE);
        assert!(ledger.full);
        assert_eq!(
            ledger.add(
                EvidenceClass::LexicalCandidate,
                "s",
                rule(),
                range("a.ts"),
                None,
                None
            ),
            first,
            "known evidence is still returned when full"
        );
        assert_eq!(ledger.tier(&[other.unwrap()]), 4);
        assert_eq!(ledger.tier(&[]), 7);
    }
}
