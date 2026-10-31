//! Step planning: one drafted step per finding group, in a fixed category
//! template, then bounded selection. Drafts are pushed in category order, and
//! ties in the plan sort keep that order, so the push order is part of the
//! output contract.
use std::collections::{BTreeMap, BTreeSet};

use super::{
    Action, Category, Code, EvidenceClass, Findings, Next, Priority, Ranking, Role, STEP_TARGETS,
    Step, Uncertainty,
    evidence::{Ledger, label, short_hash},
    recall::Recalled,
    task::{Cues, category_order},
};
use crate::navigate::Direction;

pub(super) struct Draft {
    action: Action,
    category: Category,
    tier: u8,
    rank: usize,
    objective: String,
    targets: Vec<String>,
    omitted_targets: usize,
    evidence: Vec<String>,
    uncertainty: Vec<Uncertainty>,
    next: Vec<Next>,
    rationale: String,
    /// Selected only after every category's first step, like lexical steps.
    fill_last: bool,
}

/// Draft every step the findings support, before any limit is applied.
pub(super) fn drafts(
    ledger: &Ledger,
    found: &Findings,
    knowledge: bool,
    depth: usize,
) -> Vec<Draft> {
    let mut planner = Planner {
        ledger,
        found,
        knowledge,
        drafts: Vec::new(),
        stepped: BTreeSet::new(),
    };
    planner.selected();
    planner.implementation();
    planner.knowledge();
    planner.memory();
    planner.consumers(depth);
    planner.dependencies();
    planner.tests();
    planner.uncertainty();
    planner.lexical();
    planner.drafts
}

struct Planner<'a> {
    ledger: &'a Ledger,
    found: &'a Findings,
    knowledge: bool,
    drafts: Vec<Draft>,
    /// Targets that already have a selected, implementation or knowledge step.
    stepped: BTreeSet<String>,
}

impl Planner<'_> {
    fn knowledge_next(&self, id: &str) -> Vec<Next> {
        let target = &self.ledger.targets[id];
        let mut next = vec![Next::read(&target.location)];
        if self.knowledge && matches!(target.role, Role::Requirement | Role::Decision) {
            next.push(Next {
                selector: Some(
                    target
                        .explicit_id
                        .clone()
                        .unwrap_or_else(|| target.id.clone()),
                ),
                depth: Some(1),
                ..Next::new("atlas_knowledge_trace")
            });
        }
        next
    }

    fn code_next(&self, id: &str) -> Vec<Next> {
        let target = &self.ledger.targets[id];
        let mut next = vec![Next::read(&target.location)];
        if target.kind != "file" {
            next.push(Next {
                id: Some(id.to_owned()),
                ..Next::new("atlas_references")
            });
        }
        next
    }

    fn selected(&mut self) {
        for (rank, id) in self.found.selected.iter().enumerate() {
            let Some(target) = self.ledger.targets.get(id) else {
                continue;
            };
            self.stepped.insert(id.clone());
            let evidence = self.ledger.support.get(id).cloned().unwrap_or_default();
            let (action, objective, next) = match target.role {
                Role::Requirement => (
                    Action::ReviewRequirement,
                    format!(
                        "Review selected requirement {} (recorded intent)",
                        target.explicit_id.as_deref().unwrap_or(id)
                    ),
                    self.knowledge_next(id),
                ),
                Role::Decision => (
                    Action::ReviewDecision,
                    format!(
                        "Review selected decision {} (recorded status: {})",
                        target.explicit_id.as_deref().unwrap_or(id),
                        target.decision_status.as_deref().unwrap_or("undeclared")
                    ),
                    self.knowledge_next(id),
                ),
                Role::Document => (
                    Action::ReviewDocument,
                    "Review the selected document".into(),
                    self.knowledge_next(id),
                ),
                _ => (
                    Action::InspectImplementation,
                    format!("Inspect selected {} `{}`", target.kind, target.name),
                    self.code_next(id),
                ),
            };
            let mut evidence = evidence;
            if let Some(code) = self.found.code.get(id) {
                for e in &code.evidence {
                    if !evidence.contains(e) {
                        evidence.push(e.clone());
                    }
                }
            }
            self.drafts.push(Draft {
                action,
                category: Category::Selected,
                fill_last: false,
                tier: 1,
                rank,
                objective,
                targets: vec![id.clone()],
                omitted_targets: 0,
                uncertainty: self.ledger.notes.get(id).cloned().unwrap_or_default(),
                rationale: format!(
                    "explicit selector {}; strongest evidence: explicit selector",
                    rank + 1
                ),
                evidence,
                next,
            });
        }
    }

    fn implementation(&mut self) {
        for (rank, id) in self.found.primary.iter().enumerate() {
            if !self.stepped.insert(id.clone()) {
                continue;
            }
            let code = &self.found.code[id];
            let target = &self.ledger.targets[id];
            let tier = self.ledger.tier(&code.evidence);
            self.drafts.push(Draft {
                action: Action::InspectImplementation,
                category: Category::Implementation,
                fill_last: false,
                tier,
                rank,
                objective: format!(
                    "Inspect {} `{}` as a candidate implementation location",
                    target.kind, target.name
                ),
                targets: vec![id.clone()],
                omitted_targets: 0,
                evidence: code.evidence.clone(),
                uncertainty: code_uncertainty(code),
                next: self.code_next(id),
                rationale: format!(
                    "code target rank {}; strongest evidence: {}",
                    rank + 1,
                    self.ledger.strongest(&code.evidence)
                ),
            });
        }
    }

    fn knowledge(&mut self) {
        let mut knowledge: Vec<(&String, &(u8, usize))> =
            self.found.knowledge_rank.iter().collect();
        knowledge.sort_by(|a, b| {
            (std::cmp::Reverse(a.1.0), a.1.1, a.0).cmp(&(std::cmp::Reverse(b.1.0), b.1.1, b.0))
        });
        for (rank, (id, _)) in knowledge.into_iter().enumerate() {
            if !self.stepped.insert((*id).clone()) {
                continue;
            }
            let target = &self.ledger.targets[id.as_str()];
            let evidence = self
                .ledger
                .support
                .get(id.as_str())
                .cloned()
                .unwrap_or_default();
            let label = target.explicit_id.as_deref().unwrap_or(id);
            let (action, objective) = match target.role {
                Role::Requirement => (
                    Action::ReviewRequirement,
                    format!("Review requirement {label} (recorded intent)"),
                ),
                Role::Decision => (
                    Action::ReviewDecision,
                    format!(
                        "Review decision {label} (recorded status: {})",
                        target.decision_status.as_deref().unwrap_or("undeclared")
                    ),
                ),
                _ => (Action::ReviewDocument, format!("Review document {label}")),
            };
            let mut uncertainty = self
                .ledger
                .notes
                .get(id.as_str())
                .cloned()
                .unwrap_or_default();
            if evidence.iter().all(|e| {
                self.ledger.evidence.get(e).is_none_or(|e| {
                    matches!(
                        e.class,
                        EvidenceClass::RecordedIntent | EvidenceClass::LexicalCandidate
                    )
                })
            }) {
                uncertainty.push(Uncertainty {
                    code: "lexical_only",
                    detail: "matched by document text only; no explicit claim links it to the code targets".into(),
                    evidence: vec![],
                });
            }
            self.drafts.push(Draft {
                action,
                category: Category::Knowledge,
                fill_last: false,
                tier: self.ledger.tier(&evidence),
                rank,
                objective,
                targets: vec![(*id).clone()],
                omitted_targets: 0,
                uncertainty,
                next: self.knowledge_next(id),
                rationale: format!(
                    "knowledge rank {}; strongest evidence: {}",
                    rank + 1,
                    self.ledger.strongest(&evidence)
                ),
                evidence,
            });
        }
    }

    /// One step for approved memories whose backing evidence is current, one
    /// fill-last step for the rest, which never outranks other evidence.
    fn memory(&mut self) {
        self.current_memories();
        self.changed_memories();
    }

    fn current_memories(&mut self) {
        let current: Vec<&Recalled> = self
            .found
            .memories
            .iter()
            .filter(|m| m.state == crate::memory::State::Current)
            .collect();
        if !current.is_empty() {
            let mut rationale: Vec<String> = Vec::new();
            for memory in &current {
                rationale.push(format!(
                    "{} r{} {} by {}{}",
                    memory.id,
                    memory.revision,
                    memory.memory_type,
                    memory.rule,
                    if memory.cites.is_empty() {
                        String::new()
                    } else {
                        format!(" (cites {})", memory.cites.join(", "))
                    }
                ));
            }
            let evidence: Vec<String> = current.iter().flat_map(|m| m.evidence.clone()).collect();
            let mut next: Vec<Next> = current.iter().map(|m| memory_next(&m.id)).collect();
            next.push(Next::read(&self.ledger.targets[&current[0].id].location));
            self.drafts.push(Draft {
                action: Action::ReviewMemory,
                category: Category::Memory,
                fill_last: false,
                tier: self.ledger.tier(&evidence),
                rank: 0,
                objective: format!(
                    "Review {} approved project memory(ies) whose backing evidence is current",
                    current.len()
                ),
                targets: current.iter().map(|m| m.id.clone()).collect(),
                omitted_targets: 0,
                evidence,
                uncertainty: vec![review_only()],
                next,
                rationale: format!(
                    "approved memories in recall order: {}; strongest evidence: approved project memory",
                    rationale.join("; ")
                ),
            });
        }
    }

    fn changed_memories(&mut self) {
        let changed: Vec<&Recalled> = self
            .found
            .memories
            .iter()
            .filter(|m| m.state != crate::memory::State::Current)
            .collect();
        if changed.is_empty() {
            return;
        }
        let mut uncertainty = vec![review_only()];
        for memory in &changed {
            uncertainty.push(Uncertainty {
                code: if memory.state == crate::memory::State::Unverified {
                    "unverified_memory"
                } else {
                    "stale_memory"
                },
                detail: format!(
                    "{} backing evidence is {} since approval: {}; re-check with the memory's history before relying on it",
                    memory.id,
                    memory.state.label(),
                    memory.changed.join(", ")
                ),
                evidence: memory.evidence.clone(),
            });
        }
        let mut targets: Vec<String> = changed.iter().map(|m| m.id.clone()).collect();
        let total = targets.len();
        targets.truncate(STEP_TARGETS);
        self.drafts.push(Draft {
            action: Action::ReviewMemory,
            category: Category::Memory,
            fill_last: true,
            tier: 7,
            rank: 1,
            objective: format!(
                "Re-check {total} approved memory(ies) whose evidence changed or could not be checked; do not rely on them over current sources"
            ),
            omitted_targets: total - targets.len(),
            next: targets.iter().take(4).map(|id| memory_next(id)).collect(),
            targets,
            evidence: changed.iter().flat_map(|m| m.evidence.clone()).collect(),
            uncertainty,
            rationale: "approved memories with non-current evidence; selected only after every other category".into(),
        });
    }

    fn consumers(&mut self, depth: usize) {
        for (rank, id) in self.found.primary.iter().enumerate() {
            let Some(found) = self.found.consumers.get(id) else {
                continue;
            };
            let mut targets: Vec<String> = Vec::new();
            for (consumer, _) in found {
                if !targets.contains(consumer) {
                    targets.push(consumer.clone());
                }
            }
            let evidence: Vec<String> = found.iter().map(|(_, e)| e.clone()).collect();
            let mut uncertainty = vec![Uncertainty {
                code: "static_scope_only",
                detail: "dynamic, runtime, HTTP and non-TypeScript consumers are outside the static graph".into(),
                evidence: vec![],
            }];
            if let Some(sites) = self.found.unconfirmed.get(id) {
                uncertainty.push(Uncertainty {
                    code: "unconfirmed_sites",
                    detail: format!("{} same-name site(s) have no resolved target and are not counted as consumers", sites.len()),
                    evidence: sites.clone(),
                });
            }
            let name = &self.ledger.targets[id].name;
            let total = targets.len();
            targets.truncate(STEP_TARGETS);
            self.drafts.push(Draft {
                action: Action::InspectConsumers,
                category: Category::Consumers,
                fill_last: false,
                tier: self.ledger.tier(&evidence),
                rank,
                objective: format!("Inspect {total} confirmed static consumer(s) of `{name}` within depth {depth}"),
                omitted_targets: total - targets.len(),
                targets,
                evidence,
                uncertainty,
                next: vec![
                    Next {
                        id: Some(id.clone()),
                        ..Next::new("atlas_references")
                    },
                    Next {
                        id: Some(id.clone()),
                        direction: Some(Direction::Upstream),
                        depth: Some(depth),
                        ..Next::new("atlas_dependencies")
                    },
                ],
                rationale: format!("consumers of code target rank {}; strongest evidence: verified static relationship", rank + 1),
            });
        }
    }

    fn dependencies(&mut self) {
        for (id, found) in &self.found.dependencies {
            let targets: Vec<String> = found.iter().map(|(t, _)| t.clone()).collect();
            let evidence: Vec<String> = found.iter().map(|(_, e)| e.clone()).collect();
            self.drafts.push(Draft {
                action: Action::InspectDependencies,
                category: Category::Dependencies,
                fill_last: false,
                tier: self.ledger.tier(&evidence),
                rank: 0,
                objective: format!("Inspect {} confirmed direct static dependenc(ies) of `{}`", targets.len(), self.ledger.targets[id].name),
                targets,
                omitted_targets: 0,
                evidence,
                uncertainty: vec![],
                next: vec![Next {
                    id: Some(id.clone()),
                    direction: Some(Direction::Downstream),
                    depth: Some(1),
                    ..Next::new("atlas_dependencies")
                }],
                rationale: "direct dependencies of the first code target; strongest evidence: verified static relationship".into(),
            });
        }
    }

    fn tests(&mut self) {
        if self.found.tests.is_empty() {
            return;
        }
        let mut targets: Vec<String> = Vec::new();
        for (test, _) in &self.found.tests {
            if !targets.contains(test) {
                targets.push(test.clone());
            }
        }
        let evidence: Vec<String> = self.found.tests.iter().map(|(_, e)| e.clone()).collect();
        let mut uncertainty = vec![Uncertainty {
            code: "tests_not_executed",
            detail: "static links only; Repogi did not run tests, and pass/fail and coverage are unknown; runner configuration is not evaluated".into(),
            evidence: vec![],
        }];
        let lexical: Vec<String> = evidence
            .iter()
            .filter(|e| {
                self.ledger
                    .evidence
                    .get(*e)
                    .is_some_and(|e| e.class == EvidenceClass::LexicalCandidate)
            })
            .cloned()
            .collect();
        if !lexical.is_empty() {
            uncertainty.push(Uncertainty {
                code: "lexical_only",
                detail: "some test candidates matched by name/text only, without a static link"
                    .into(),
                evidence: lexical,
            });
        }
        let total = targets.len();
        targets.truncate(STEP_TARGETS);
        let next = targets
            .iter()
            .take(4)
            .map(|t| Next::read(&self.ledger.targets[t].location))
            .collect();
        let rationale = format!(
            "test candidates; strongest evidence: {}",
            self.ledger.strongest(&evidence)
        );
        self.drafts.push(Draft {
            action: Action::ReviewTestCandidates,
            category: Category::Tests,
            fill_last: false,
            tier: self.ledger.tier(&evidence),
            rank: 0,
            objective: format!(
                "Review {total} test candidate(s) linked to the targets; Repogi did not run them"
            ),
            omitted_targets: total - targets.len(),
            targets,
            evidence,
            uncertainty,
            next,
            rationale,
        });
    }

    fn uncertainty(&mut self) {
        if self.found.unresolved.is_empty() {
            return;
        }
        let mut targets: Vec<String> = Vec::new();
        for (subject, _) in &self.found.unresolved {
            if !targets.contains(subject) {
                targets.push(subject.clone());
            }
        }
        let mut evidence: Vec<String> = self
            .found
            .unresolved
            .iter()
            .map(|(_, e)| e.clone())
            .collect();
        evidence.sort_by(|a, b| {
            let (a, b) = (
                &self.ledger.evidence[a].citation,
                &self.ledger.evidence[b].citation,
            );
            (&a.path, a.start_byte).cmp(&(&b.path, b.start_byte))
        });
        evidence.dedup();
        let mut codes = BTreeMap::<&'static str, Vec<String>>::new();
        for id in &evidence {
            codes
                .entry(unresolved_code(&self.ledger.evidence[id]))
                .or_default()
                .push(id.clone());
        }
        let uncertainty = codes
            .into_iter()
            .map(|(code, evidence)| Uncertainty {
                code,
                detail: format!(
                    "{} finding(s); none is treated as a relationship",
                    evidence.len()
                ),
                evidence,
            })
            .collect();
        let next = evidence
            .iter()
            .take(4)
            .map(|e| Next::read(&self.ledger.evidence[e].citation))
            .collect();
        let total = targets.len();
        targets.truncate(STEP_TARGETS);
        self.drafts.push(Draft {
            action: Action::ResolveUncertainty,
            category: Category::Uncertainty,
            fill_last: false,
            tier: 7,
            rank: 0,
            objective: format!("Resolve {} unresolved, ambiguous, invalid, stale or unconfirmed finding(s) before relying on them", evidence.len()),
            omitted_targets: total - targets.len(),
            targets,
            evidence,
            uncertainty,
            next,
            rationale: "unresolved findings; reported, never promoted to relationships".into(),
        });
    }

    fn lexical(&mut self) {
        let mut lexical: Vec<(String, Vec<String>)> = self
            .found
            .documents
            .iter()
            .map(|(t, e)| (t.clone(), vec![e.clone()]))
            .collect();
        for id in &self.found.extra {
            if !self.stepped.contains(id) {
                lexical.push((id.clone(), self.found.code[id].evidence.clone()));
            }
        }
        if lexical.is_empty() {
            return;
        }
        let evidence: Vec<String> = lexical.iter().flat_map(|(_, e)| e.clone()).collect();
        let mut targets: Vec<String> = lexical.into_iter().map(|(t, _)| t).collect();
        let total = targets.len();
        targets.truncate(STEP_TARGETS);
        let next = targets
            .iter()
            .take(4)
            .map(|t| Next::read(&self.ledger.targets[t].location))
            .collect();
        let tier = self.ledger.tier(&evidence);
        let rationale = format!(
            "lower-ranked candidates; strongest evidence: {}",
            self.ledger.strongest(&evidence)
        );
        self.drafts.push(Draft {
            action: Action::ReviewLexicalCandidates,
            category: Category::Lexical,
            fill_last: false,
            tier,
            rank: 0,
            objective: format!(
                "Optionally check {total} additional candidate(s) beyond the ranked targets"
            ),
            omitted_targets: total - targets.len(),
            uncertainty: if tier >= 6 {
                vec![Uncertainty {
                    code: "lexical_only",
                    detail: "text/name similarity only; no relationship supports these candidates"
                        .into(),
                    evidence: vec![],
                }]
            } else {
                vec![]
            },
            targets,
            evidence,
            next,
            rationale,
        });
    }
}

fn memory_next(id: &str) -> Next {
    Next {
        id: Some(id.to_owned()),
        operation: Some("get"),
        ..Next::new("atlas_memory")
    }
}

fn review_only() -> Uncertainty {
    Uncertainty {
        code: "memory_review_only",
        detail: "approved by developer review; approval is not runtime proof, and current code and documents take precedence".into(),
        evidence: vec![],
    }
}

fn code_uncertainty(code: &Code) -> Vec<Uncertainty> {
    let mut list = Vec::new();
    if code.explicit || code.claim > 0 {
        if code.claim > 0 {
            list.push(Uncertainty {
                code: "behavior_unverified",
                detail: "an explicit claim validates this location's identity, not that it implements the requirement correctly".into(),
                evidence: vec![],
            });
        }
    } else {
        list.push(Uncertainty {
            code: "lexical_only",
            detail:
                "found by name/text similarity only; no static or explicit relationship supports it"
                    .into(),
            evidence: code.evidence.clone(),
        });
    }
    list
}

fn unresolved_code(evidence: &super::Evidence) -> &'static str {
    match (
        evidence.class,
        evidence.relationship.as_ref().map(|r| r.status.as_str()),
    ) {
        (EvidenceClass::StaleReference, _) => "stale_reference",
        _ if evidence
            .relationship
            .as_ref()
            .is_some_and(|r| r.id.is_none()) =>
        {
            "unconfirmed_sites"
        }
        (_, Some("ambiguous")) => "ambiguous_reference",
        (_, Some("invalid")) => "invalid_reference",
        (_, Some("unsupported")) => "unsupported_reference",
        _ => "unresolved_reference",
    }
}

/// Which drafts become steps.
pub(super) struct Selection {
    /// Draft indices in plan (output) order.
    pub chosen: Vec<usize>,
    /// The same indices in the order selection picked them; fitting drops from the end.
    pub order: Vec<usize>,
}

/// Plan order is category phase (cue-adjusted), then tier, rank and action.
/// `Balanced` takes the k-th step of every category before any (k+1)-th, with
/// lexical-only steps last; `Tiered` takes the strongest evidence first.
pub(super) fn select(
    drafts: &[Draft],
    cues: Cues,
    ranking: Ranking,
    max_steps: usize,
) -> Selection {
    let order = category_order(cues);
    let position = |c: Category| order.iter().position(|o| *o == c).unwrap_or(order.len());
    let mut plan: Vec<usize> = (0..drafts.len()).collect();
    plan.sort_by_key(|&i| {
        (
            position(drafts[i].category),
            drafts[i].tier,
            drafts[i].rank,
            drafts[i].action,
        )
    });
    let mut chosen: Vec<usize> = Vec::new();
    match ranking {
        Ranking::Balanced => {
            // Fill levels: 0 ordinary, 1 lexical-only, 2 stale memory (never
            // ahead of anything else).
            let level = |i: usize| {
                if drafts[i].fill_last {
                    2u8
                } else {
                    u8::from(drafts[i].category == Category::Lexical)
                }
            };
            let mut passes: BTreeMap<(usize, u8), Vec<usize>> = BTreeMap::new();
            let mut seen: BTreeMap<Category, usize> = BTreeMap::new();
            for &i in &plan {
                let k = seen.entry(drafts[i].category).or_default();
                passes.entry((*k, level(i))).or_default().push(i);
                *k += 1;
            }
            let mut ordered: Vec<(usize, u8, usize)> = Vec::new();
            for ((k, level), indices) in passes {
                ordered.extend(indices.into_iter().map(|i| (k, level, i)));
            }
            ordered.sort_by_key(|(k, level, i)| (*level, *k, plan.iter().position(|p| p == i)));
            chosen.extend(ordered.into_iter().map(|(_, _, i)| i).take(max_steps));
        }
        Ranking::Tiered => {
            let mut strength = plan.clone();
            strength.sort_by_key(|&i| {
                (
                    drafts[i].tier,
                    position(drafts[i].category),
                    drafts[i].rank,
                    drafts[i].action,
                )
            });
            chosen.extend(strength.into_iter().take(max_steps));
        }
    }
    let order = chosen.clone();
    chosen.sort_by_key(|i| plan.iter().position(|p| p == i));
    Selection { chosen, order }
}

/// Turn the chosen drafts into numbered steps with content-derived IDs. Also
/// returns every category that had a draft, for `omitted_categories`.
pub(super) fn materialize(
    drafts: Vec<Draft>,
    chosen: &[usize],
    ledger: &Ledger,
) -> (Vec<Step>, BTreeSet<Category>) {
    let mut drafts: Vec<Option<Draft>> = drafts.into_iter().map(Some).collect();
    let mut steps: Vec<Step> = Vec::new();
    for (order, index) in chosen.iter().enumerate() {
        let draft = drafts[*index].take().expect("each draft chosen once");
        let mut sorted = draft.targets.clone();
        sorted.sort();
        let id = format!(
            "step-{}",
            short_hash(&[
                &label(&draft.action),
                &label(&draft.category),
                &sorted.join("\n")
            ])
        );
        let mut evidence = Vec::new();
        for e in draft.evidence {
            if ledger.evidence.contains_key(&e) && !evidence.contains(&e) {
                evidence.push(e);
            }
        }
        steps.push(Step {
            id,
            order: order + 1,
            action: draft.action,
            category: draft.category,
            objective: draft.objective,
            targets: draft.targets,
            omitted_targets: draft.omitted_targets,
            evidence,
            uncertainty: draft.uncertainty,
            next: draft.next.into_iter().fold(Vec::new(), |mut next, item| {
                if !next.contains(&item) {
                    next.push(item);
                }
                next
            }),
            priority: Priority {
                tier: draft.tier,
                rank: draft.rank + 1,
                rationale: draft.rationale,
            },
        });
    }
    let present: BTreeSet<Category> = drafts
        .iter()
        .flatten()
        .map(|d| d.category)
        .chain(steps.iter().map(|s| s.category))
        .collect();
    (steps, present)
}

#[cfg(test)]
mod unit {
    use super::*;

    fn draft(category: Category, tier: u8, rank: usize) -> Draft {
        Draft {
            action: Action::InspectImplementation,
            category,
            fill_last: false,
            tier,
            rank,
            objective: String::new(),
            targets: vec![],
            omitted_targets: 0,
            evidence: vec![],
            uncertainty: vec![],
            next: vec![],
            rationale: String::new(),
        }
    }

    #[test]
    fn balanced_selection_covers_categories_before_depth_and_lexical_last() {
        let drafts = vec![
            draft(Category::Implementation, 6, 0),
            draft(Category::Implementation, 6, 1),
            draft(Category::Lexical, 6, 0),
            draft(Category::Tests, 5, 0),
            draft(Category::Knowledge, 4, 0),
        ];
        let selection = select(&drafts, Cues::default(), Ranking::Balanced, 3);
        assert_eq!(
            selection.order,
            [0, 4, 3],
            "one per category first, lexical last"
        );
        assert_eq!(selection.chosen, [0, 4, 3], "output keeps plan order");
        let tiered = select(&drafts, Cues::default(), Ranking::Tiered, 3);
        assert_eq!(tiered.order, [4, 3, 0], "strongest evidence first");
        assert_eq!(tiered.chosen, [0, 4, 3]);
    }
}
