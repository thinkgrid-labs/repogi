//! Candidate discovery: explicit selectors, lexical code and document matches,
//! one explicit-claim hop from requirement/decision seeds, governing claims
//! onto lexical code, and supersession successors. Only validated claims give
//! a target identity; everything else is recorded as uncertainty.
use std::collections::BTreeSet;

use anyhow::Result;
use code_intel_core::{
    Fact, file_id,
    relationships::{
        EntityKind, KnowledgeEntity, KnowledgeRelationship, RelationshipKind, ResolutionStatus,
        SourceRange,
    },
};

use super::{
    EvidenceClass, FILE_DECLARATIONS, Investigation, MAX_DOCUMENT_HITS, MAX_LEXICAL_CODE,
    MAX_SEEDS, MAX_SELECTORS, Role, SEED_EDGES, Seed, Target,
    evidence::{claim_relation, clean, fact_range, label},
};

fn declaration(fact: &Fact) -> bool {
    !matches!(fact.kind.as_str(), "file" | "import" | "export")
}

impl Investigation<'_> {
    /// Record an explicit requirement/decision as intent, with its declaration as evidence.
    fn knowledge_target(&mut self, entity: &KnowledgeEntity, strength: u8, rank: usize) {
        self.ledger.entity_target(entity);
        let rule = match entity.kind {
            EntityKind::Requirement => "explicit_requirement_heading",
            EntityKind::ArchitectureDecision => "adr_front_matter_id; author-declared status",
            _ => "indexed_markdown_document",
        };
        let intent = self.ledger.add(
            EvidenceClass::RecordedIntent,
            &entity.id,
            rule.into(),
            entity.source.clone(),
            None,
            None,
        );
        let support = self.ledger.support.entry(entity.id.clone()).or_default();
        if let Some(intent) = intent
            && !support.contains(&intent)
        {
            support.insert(0, intent);
        }
        let entry = self
            .found
            .knowledge_rank
            .entry(entity.id.clone())
            .or_insert((strength, rank));
        if (strength, std::cmp::Reverse(rank)) > (entry.0, std::cmp::Reverse(entry.1)) {
            *entry = (strength, rank);
        }
        if entity.kind == EntityKind::ArchitectureDecision
            && let Some(status) = entity.decision_status.as_deref()
            && matches!(status, "superseded" | "deprecated" | "rejected")
        {
            self.ledger.note(
                &entity.id,
                "historical_decision",
                format!(
                    "recorded status is {}; check for a superseding decision before relying on it",
                    clean(status)
                ),
                vec![],
            );
        }
    }

    /// Declarations of an explicitly referenced file (bounded), or the file itself.
    fn file_declarations(&mut self, path: &str) -> Result<Vec<Fact>> {
        let facts = self
            .store
            .file_facts(path, u32::try_from(FILE_DECLARATIONS * 4 + 1)?)?;
        self.spend(facts.len());
        let mut declarations: Vec<Fact> =
            facts.iter().filter(|f| declaration(f)).cloned().collect();
        if declarations.len() > FILE_DECLARATIONS || facts.len() > FILE_DECLARATIONS * 4 {
            self.work.candidates_limited = true;
        }
        declarations.truncate(FILE_DECLARATIONS);
        if declarations.is_empty() {
            declarations.extend(facts.into_iter().filter(|f| f.kind == "file"));
        }
        Ok(declarations)
    }

    pub(super) fn select(&mut self, selectors: &[String]) -> Result<()> {
        for selector in selectors {
            let matches: Vec<KnowledgeEntity> = if self.knowledge {
                self.store.knowledge_entities(selector, 5)?
            } else {
                self.store
                    .fact(selector)?
                    .map(|fact| KnowledgeEntity {
                        id: fact.id.clone(),
                        explicit_id: None,
                        kind: if fact.kind == "file" {
                            EntityKind::CodeFile
                        } else {
                            EntityKind::CodeSymbol
                        },
                        title: fact.name.clone(),
                        source: fact_range(&fact),
                        content_hash: String::new(),
                        decision_status: None,
                        acceptance_criteria: vec![],
                        diagnostics: vec![],
                    })
                    .into_iter()
                    .collect()
            };
            self.spend(matches.len() + 1);
            match matches.len() {
                0 => self.ledger.global(
                    "selector_not_found",
                    format!("selector {:?} matches no indexed entity, author ID or exact fact ID; names are never resolved by similarity", clean(selector)),
                    vec![],
                ),
                1 => {}
                n => {
                    self.found.ambiguous = true;
                    self.ledger.global(
                        "ambiguous_selector",
                        format!(
                            "selector {:?} matches {}{} indexed entities; each is shown, none is chosen",
                            clean(selector),
                            n.min(4),
                            if n > 4 { "+" } else { "" }
                        ),
                        vec![],
                    );
                }
            }
            for entity in matches.into_iter().take(4) {
                if self.found.selected.contains(&entity.id) {
                    continue;
                }
                self.found.selected.push(entity.id.clone());
                let explicit = self.ledger.add(
                    EvidenceClass::ExplicitSelector,
                    &entity.id,
                    format!(
                        "caller selector {:?} matched this exact indexed identity",
                        clean(selector)
                    ),
                    entity.source.clone(),
                    None,
                    None,
                );
                match entity.kind {
                    EntityKind::CodeSymbol | EntityKind::CodeFile => {
                        if let Some(fact) = self.fact(&entity.id)? {
                            if entity.kind == EntityKind::CodeFile {
                                for decl in self.file_declarations(&fact.path)? {
                                    self.found
                                        .code_candidate(decl, explicit.clone(), true, 2, 0.0);
                                }
                            } else {
                                self.found
                                    .code_candidate(fact, explicit.clone(), true, 2, 0.0);
                            }
                        }
                        self.ledger.entity_target(&entity);
                        self.ledger.support(&entity.id, explicit);
                    }
                    _ => {
                        let rank = self.found.seeds.len();
                        self.found
                            .seeds
                            .insert(entity.id.clone(), Seed { strength: 2, rank });
                        self.knowledge_target(&entity, 2, rank);
                        self.ledger.support(&entity.id, explicit);
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn discover_code(&mut self, terms: &[String]) -> Result<()> {
        if terms.is_empty() {
            return Ok(());
        }
        let (found, _) = crate::optimized::lexical(self.store, terms)?;
        self.spend(found.len());
        let mut ranked: Vec<_> = found.into_values().collect();
        ranked.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.fact.id.cmp(&b.fact.id))
        });
        self.found.lexical_omitted += ranked.len().saturating_sub(MAX_LEXICAL_CODE);
        for candidate in ranked.into_iter().take(MAX_LEXICAL_CODE) {
            let rule = if candidate.why == "identifier match" {
                "fts5_identifier_match"
            } else {
                "fts5_source_text_match"
            };
            let evidence = self.ledger.add(
                EvidenceClass::LexicalCandidate,
                &candidate.fact.id,
                rule.into(),
                fact_range(&candidate.fact),
                None,
                None,
            );
            self.found
                .code_candidate(candidate.fact, evidence, false, 0, candidate.score);
        }
        Ok(())
    }

    pub(super) fn discover_documents(&mut self, terms: &[String]) -> Result<()> {
        if !self.knowledge || terms.is_empty() {
            return Ok(());
        }
        let hits = crate::knowledge::ranked_documents(
            self.store,
            &terms.join(" "),
            u32::try_from(MAX_DOCUMENT_HITS * 2 + 1)?,
        )?;
        self.spend(hits.len());
        // Partial-term sections are a fallback only when no section covers every
        // term, and must still match at least two thirds of the terms (minimum 2).
        let all_terms = hits.iter().any(|h| h.match_mode == "all_terms");
        let chosen: Vec<_> =
            hits.into_iter()
                .filter(|h| {
                    if h.match_mode == "all_terms" {
                        return true;
                    }
                    let matched = terms
                        .iter()
                        .filter(|term| {
                            h.matches.iter().any(|range| {
                                h.section.content.get(range.clone()).is_some_and(|word| {
                                    word.to_lowercase().starts_with(term.as_str())
                                })
                            })
                        })
                        .count();
                    !all_terms && matched >= 2 && matched * 3 >= terms.len() * 2
                })
                .collect();
        self.found.lexical_omitted += chosen.len().saturating_sub(MAX_DOCUMENT_HITS);
        for (rank, hit) in chosen.into_iter().take(MAX_DOCUMENT_HITS).enumerate() {
            let citation = SourceRange {
                path: hit.path.clone(),
                start_byte: hit.section.start_byte,
                end_byte: hit.section.end_byte,
                start_line: hit.section.start_line,
                end_line: hit.section.end_line,
            };
            let mode = if hit.match_mode == "all_terms" {
                "all task terms"
            } else {
                "some task terms"
            };
            let owner = self
                .store
                .knowledge_section_owner(&hit.path, hit.section.start_byte)?;
            let entity = match owner {
                Some(owner) => self.entity(&owner)?,
                None => None,
            };
            if let Some(entity) = entity {
                let evidence = self.ledger.add(
                    EvidenceClass::LexicalCandidate,
                    &entity.id,
                    format!("fts5_section_match_within_entity ({mode})"),
                    citation,
                    None,
                    None,
                );
                if !self.found.seeds.contains_key(&entity.id) {
                    let rank = MAX_SELECTORS + rank;
                    self.found
                        .seeds
                        .insert(entity.id.clone(), Seed { strength: 1, rank });
                }
                self.knowledge_target(&entity, 1, MAX_SELECTORS + rank);
                self.ledger.support(&entity.id, evidence);
            } else {
                let id = hit.section.id.clone();
                self.ledger
                    .targets
                    .entry(id.clone())
                    .or_insert_with(|| Target {
                        id: id.clone(),
                        kind: "document_section".into(),
                        role: Role::Document,
                        name: clean(&hit.section.hierarchy.join(" / ")),
                        explicit_id: None,
                        decision_status: None,
                        location: citation.clone(),
                        freshness: "unverified".into(),
                    });
                if let Some(evidence) = self.ledger.add(
                    EvidenceClass::LexicalCandidate,
                    &id,
                    format!(
                        "fts5_section_match ({mode}); outside any declared requirement or decision"
                    ),
                    citation,
                    None,
                    None,
                ) {
                    self.found.documents.push((id, evidence));
                }
            }
        }
        Ok(())
    }

    /// One explicit-claim hop from each seed. Only validated targets gain identity.
    pub(super) fn expand_seeds(&mut self) -> Result<()> {
        let mut seeds: Vec<(String, Seed)> = self.found.seeds.clone().into_iter().collect();
        seeds.sort_by(|a, b| {
            (std::cmp::Reverse(a.1.strength), a.1.rank, &a.0).cmp(&(
                std::cmp::Reverse(b.1.strength),
                b.1.rank,
                &b.0,
            ))
        });
        if seeds.len() > MAX_SEEDS {
            self.work.candidates_limited = true;
        }
        for (seed, info) in seeds.into_iter().take(MAX_SEEDS) {
            if self.work.limited {
                break;
            }
            let edges = self
                .store
                .knowledge_neighbors(&seed, u32::try_from(SEED_EDGES + 1)?)?;
            self.spend(edges.len());
            if edges.len() > SEED_EDGES {
                self.work.candidates_limited = true;
            }
            for edge in edges.into_iter().take(SEED_EDGES) {
                if edge.kind == RelationshipKind::Contains {
                    continue;
                }
                if edge.status == ResolutionStatus::Validated {
                    let other = if edge.source_id == seed {
                        edge.target_id.clone()
                    } else {
                        Some(edge.source_id.clone())
                    };
                    let Some(other) = other else { continue };
                    self.claim(&seed, &edge, &other, info.strength, info.rank)?;
                } else if edge.source_id == seed {
                    self.unresolved_claim(&seed, &edge);
                }
            }
        }
        Ok(())
    }

    fn claim(
        &mut self,
        seed: &str,
        edge: &KnowledgeRelationship,
        other: &str,
        strength: u8,
        rank: usize,
    ) -> Result<()> {
        let rule = format!(
            "{}; explicit {} claim; target identity only",
            edge.rule,
            label(&edge.kind)
        );
        let evidence = self.ledger.add(
            EvidenceClass::ValidatedKnowledgeReference,
            &edge.source_id,
            rule,
            edge.evidence.clone(),
            Some(claim_relation(edge)),
            None,
        );
        self.ledger.support(seed, evidence.clone());
        let Some(entity) = self.entity(other)? else {
            return Ok(());
        };
        match entity.kind {
            EntityKind::CodeSymbol => {
                if let Some(fact) = self.fact(&entity.id)? {
                    self.found
                        .code_candidate(fact, evidence, false, strength, 0.0);
                }
            }
            EntityKind::CodeFile => {
                for decl in self.file_declarations(&entity.source.path)? {
                    self.found
                        .code_candidate(decl, evidence.clone(), false, strength, 0.0);
                }
            }
            _ => {
                // A related knowledge entity ranks just after its seed.
                self.knowledge_target(&entity, strength.saturating_sub(1).max(1), rank + 1);
                self.ledger.support(&entity.id, evidence);
            }
        }
        Ok(())
    }

    fn unresolved_claim(&mut self, seed: &str, edge: &KnowledgeRelationship) {
        let (class, code) = match edge.status {
            ResolutionStatus::Stale => (EvidenceClass::StaleReference, "stale_reference"),
            ResolutionStatus::Ambiguous => {
                (EvidenceClass::UnresolvedReference, "ambiguous_reference")
            }
            ResolutionStatus::Invalid => (EvidenceClass::UnresolvedReference, "invalid_reference"),
            ResolutionStatus::Unsupported => {
                (EvidenceClass::UnresolvedReference, "unsupported_reference")
            }
            _ => (EvidenceClass::UnresolvedReference, "unresolved_reference"),
        };
        let rule = format!(
            "{}: {}{}",
            label(&edge.status),
            edge.rule,
            edge.reason
                .as_deref()
                .map_or_else(String::new, |r| format!("; {}", clean(r)))
        );
        if let Some(evidence) = self.ledger.add(
            class,
            seed,
            rule,
            edge.evidence.clone(),
            Some(claim_relation(edge)),
            None,
        ) {
            self.ledger.note(
                seed,
                code,
                format!(
                    "explicit {} claim to {:?} has no validated target",
                    label(&edge.kind),
                    clean(&edge.target)
                ),
                vec![evidence.clone()],
            );
            self.found.unresolved.push((seed.to_owned(), evidence));
        }
    }

    /// Governing claims that explicitly target lexically discovered code.
    pub(super) fn reverse_claims(&mut self) -> Result<()> {
        if !self.knowledge {
            return Ok(());
        }
        let mut lexical: Vec<(String, String)> = self
            .found
            .code
            .values()
            .filter(|c| !c.explicit && c.claim == 0)
            .map(|c| (c.fact.id.clone(), c.fact.path.clone()))
            .collect();
        lexical.sort();
        let mut files = BTreeSet::new();
        for (id, path) in lexical.into_iter().take(MAX_LEXICAL_CODE) {
            let mut subjects = vec![id.clone()];
            if files.insert(path.clone()) {
                subjects.push(file_id(&path));
            }
            for subject in subjects {
                if self.work.limited {
                    return Ok(());
                }
                let edges = self.store.knowledge_neighbors(&subject, 17)?;
                self.spend(edges.len());
                self.work.candidates_limited |= edges.len() > 16;
                for edge in edges.into_iter().take(16) {
                    if edge.status != ResolutionStatus::Validated
                        || edge.kind == RelationshipKind::Contains
                        || edge.target_id.as_deref() != Some(subject.as_str())
                    {
                        continue;
                    }
                    let Some(owner) = self.entity(&edge.source_id)? else {
                        continue;
                    };
                    if !matches!(
                        owner.kind,
                        EntityKind::Requirement | EntityKind::ArchitectureDecision
                    ) {
                        continue;
                    }
                    let rule = format!(
                        "{}; explicit {} claim to {}; target identity only",
                        edge.rule,
                        label(&edge.kind),
                        if subject.starts_with("file:") {
                            "the containing file"
                        } else {
                            "this declaration"
                        }
                    );
                    let evidence = self.ledger.add(
                        EvidenceClass::ValidatedKnowledgeReference,
                        &edge.source_id,
                        rule,
                        edge.evidence.clone(),
                        Some(claim_relation(&edge)),
                        None,
                    );
                    let rank = MAX_SELECTORS + MAX_DOCUMENT_HITS + self.found.knowledge_rank.len();
                    self.knowledge_target(&owner, 1, rank);
                    self.ledger.support(&owner.id, evidence.clone());
                    // Corroboration only: the governing entity did not match the task.
                    if let Some(code) = self.found.code.get_mut(&id)
                        && let Some(evidence) = evidence
                        && !code.evidence.contains(&evidence)
                    {
                        code.evidence.push(evidence);
                    }
                }
            }
        }
        Ok(())
    }

    /// Superseded decisions point at their explicit successors (one bounded hop).
    pub(super) fn supersession(&mut self) -> Result<()> {
        let historical: Vec<String> = self
            .ledger
            .targets
            .values()
            .filter(|t| {
                t.role == Role::Decision && t.decision_status.as_deref() == Some("superseded")
            })
            .map(|t| t.id.clone())
            .collect();
        for id in historical {
            if self.work.limited {
                break;
            }
            let edges = self.store.knowledge_neighbors(&id, 17)?;
            self.spend(edges.len());
            for edge in edges.into_iter().take(16) {
                if edge.kind != RelationshipKind::Supersedes
                    || edge.status != ResolutionStatus::Validated
                    || edge.target_id.as_deref() != Some(id.as_str())
                {
                    continue;
                }
                let Some(successor) = self.entity(&edge.source_id)? else {
                    continue;
                };
                let rank = self.found.knowledge_rank.get(&id).map_or(0, |r| r.1);
                let evidence = self.ledger.add(
                    EvidenceClass::ValidatedKnowledgeReference,
                    &edge.source_id,
                    format!(
                        "{}; explicit SUPERSEDES claim; lineage not reconciled",
                        edge.rule
                    ),
                    edge.evidence.clone(),
                    Some(claim_relation(&edge)),
                    None,
                );
                self.knowledge_target(&successor, 1, rank + 1);
                self.ledger.support(&successor.id, evidence.clone());
                self.ledger.support(&id, evidence.clone());
                self.ledger.note(
                    &id,
                    "superseded_decision",
                    format!(
                        "superseded by {}",
                        successor.explicit_id.as_deref().unwrap_or(&successor.id)
                    ),
                    evidence.into_iter().collect(),
                );
            }
        }
        Ok(())
    }
}
