//! Opt-in project memory recall. Approved memories become their own evidence
//! class (`approved_project_memory`): reviewed claims with their snapshots'
//! citations and live verification, never static or validated relationships.
//! Read-only: no memory is created, approved, updated or recorded here.
use anyhow::Result;
use code_intel_core::memory::EvidenceRole;

use super::{EvidenceClass, Investigation, MAX_MEMORIES, Role, Target, evidence::clean};
use crate::memory::State;

/// Backing citations recorded per recalled memory.
const MEMORY_CITATIONS: usize = 4;
/// Plan identities offered for exact evidence lookups.
const MAX_SUBJECTS: usize = 64;

pub(super) struct Recalled {
    pub id: String,
    pub memory_type: &'static str,
    pub revision: u32,
    pub state: State,
    pub rule: &'static str,
    pub evidence: Vec<String>,
    /// Backing citations that are no longer current, as `path [state]`.
    pub changed: Vec<String>,
    /// Plan targets the memory's evidence cites by identity.
    pub cites: Vec<String>,
}

impl Investigation<'_> {
    /// Identities a memory may cite to be linked to this plan, in plan
    /// priority order: the explicit selection when there is one, otherwise
    /// the ranked code targets and then the knowledge targets by rank.
    fn plan_subjects(&self) -> Vec<String> {
        let mut ordered: Vec<&String> = Vec::new();
        if self.found.selected.is_empty() {
            ordered.extend(&self.found.primary);
            let mut knowledge: Vec<(&String, &(u8, usize))> =
                self.found.knowledge_rank.iter().collect();
            knowledge.sort_by(|a, b| {
                (std::cmp::Reverse(a.1.0), a.1.1, a.0).cmp(&(std::cmp::Reverse(b.1.0), b.1.1, b.0))
            });
            ordered.extend(knowledge.into_iter().map(|(id, _)| id));
        } else {
            ordered.extend(&self.found.selected);
        }
        let mut subjects: Vec<String> = Vec::new();
        for id in ordered {
            let explicit = self
                .ledger
                .targets
                .get(id)
                .and_then(|t| t.explicit_id.as_ref());
            for subject in [Some(id), explicit].into_iter().flatten() {
                if !subjects.contains(subject) && subjects.len() < MAX_SUBJECTS {
                    subjects.push(subject.clone());
                }
            }
        }
        subjects
    }

    pub(super) fn recall(&mut self, terms: &[String]) -> Result<()> {
        // No memory store means no memory has been recorded: nothing to recall.
        let Some(memory) = self.memory else {
            return Ok(());
        };
        let subjects = self.plan_subjects();
        let recalled = crate::memory::relevant(
            crate::memory::Stores {
                memory,
                index: Some(self.store),
            },
            &crate::memory::Relevance {
                subjects: &subjects,
                selected: !self.found.selected.is_empty(),
                terms,
            },
            MAX_MEMORIES + 1,
        )?;
        self.spend(recalled.len());
        self.work.candidates_limited |= recalled.len() > MAX_MEMORIES;
        for crate::memory::Recall {
            memory,
            revision,
            check,
            rule,
        } in recalled.into_iter().take(MAX_MEMORIES)
        {
            let backing: Vec<usize> = (0..revision.evidence.len())
                .filter(|&i| revision.evidence[i].role == EvidenceRole::Backing)
                .collect();
            let Some(&first) = backing.first() else {
                continue;
            };
            self.ledger
                .targets
                .entry(memory.id.clone())
                .or_insert_with(|| Target {
                    id: memory.id.clone(),
                    kind: "project_memory".into(),
                    role: Role::Memory,
                    name: clean(&revision.title),
                    explicit_id: None,
                    decision_status: None,
                    location: revision.evidence[first].citation.clone(),
                    freshness: "unverified".into(),
                });
            let mut evidence = Vec::new();
            let mut changed = Vec::new();
            let mut cites = Vec::new();
            for &i in backing.iter().take(MEMORY_CITATIONS) {
                let snapshot = &revision.evidence[i];
                let state = check.evidence[i].state;
                if state != State::Current {
                    changed.push(format!("{} [{}]", snapshot.citation.path, state.label()));
                }
                let in_plan = subjects.contains(&snapshot.subject_id)
                    || snapshot
                        .explicit_id
                        .as_ref()
                        .is_some_and(|e| subjects.contains(e));
                let name = snapshot
                    .explicit_id
                    .as_ref()
                    .unwrap_or(&snapshot.subject_id);
                if in_plan && !cites.contains(name) {
                    cites.push(name.clone());
                }
                if let Some(id) = self.ledger.add(
                    EvidenceClass::ApprovedProjectMemory,
                    &memory.id,
                    format!(
                        "{rule}; approved revision {}; backing #{} {} {} recorded at index r{}; evidence {} now; reviewed claim, not runtime proof",
                        revision.revision,
                        i + 1,
                        crate::investigate::label(&snapshot.class),
                        clean(snapshot.explicit_id.as_deref().unwrap_or(&snapshot.selector)),
                        snapshot.index_revision,
                        state.label()
                    ),
                    snapshot.citation.clone(),
                    None,
                    None,
                ) {
                    evidence.push(id);
                }
            }
            self.found.memories.push(Recalled {
                id: memory.id,
                memory_type: memory.memory_type.label(),
                revision: revision.revision,
                state: check.state,
                rule,
                evidence,
                changed,
                cites,
            });
        }
        Ok(())
    }
}
