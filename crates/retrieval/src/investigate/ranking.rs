//! Code target ranking. Explicit selection, then claim origin, then how many
//! independent sources corroborate a target; lexical score breaks ties only.
use std::collections::BTreeSet;

use code_intel_core::Fact;

use super::{Code, EvidenceClass, Investigation, MAX_CODE_TARGETS, Role};

/// How many search terms prefix a camel-case or snake-case component of `name`.
fn name_terms(name: &str, terms: &[String]) -> usize {
    let mut components = Vec::new();
    for word in name.split(|c: char| !c.is_alphanumeric()) {
        let mut start = 0;
        for (offset, c) in word.char_indices() {
            if offset > start
                && c.is_uppercase()
                && !word[start..offset].chars().all(char::is_uppercase)
            {
                components.push(word[start..offset].to_lowercase());
                start = offset;
            }
        }
        components.push(word[start..].to_lowercase());
    }
    terms
        .iter()
        .filter(|t| components.iter().any(|c| c.starts_with(t.as_str())))
        .count()
}

impl Investigation<'_> {
    pub(super) fn rank_code(&mut self, terms: &[String]) {
        // Explicit selection, then claim origin, then how many independent
        // sources (validated claims, a lexical match) corroborate the target.
        let support = |code: &Code| {
            let mut claims = BTreeSet::new();
            let mut lexical = 0;
            for e in code
                .evidence
                .iter()
                .filter_map(|e| self.ledger.evidence.get(e))
            {
                match e.class {
                    EvidenceClass::ValidatedKnowledgeReference => {
                        claims.insert(e.subject.as_str());
                    }
                    EvidenceClass::LexicalCandidate => lexical = 1,
                    _ => {}
                }
            }
            claims.len() + lexical
        };
        let mut ranked: Vec<&Code> = self
            .found
            .code
            .values()
            .filter(|c| c.test.is_none())
            .collect();
        ranked.sort_by(|a, b| {
            (
                b.explicit,
                b.claim,
                support(b),
                name_terms(&b.fact.name, terms),
            )
                .cmp(&(
                    a.explicit,
                    a.claim,
                    support(a),
                    name_terms(&a.fact.name, terms),
                ))
                .then(b.score.total_cmp(&a.score))
                .then_with(|| a.fact.id.cmp(&b.fact.id))
        });
        let ids: Vec<String> = ranked.iter().map(|c| c.fact.id.clone()).collect();
        let tests: Vec<(Fact, Vec<String>)> = self
            .found
            .code
            .values()
            .filter(|c| c.test.is_some())
            .map(|c| (c.fact.clone(), c.evidence.clone()))
            .collect();
        for (position, id) in ids.into_iter().enumerate() {
            let fact = self.found.code[&id].fact.clone();
            self.ledger.fact_target(&fact, Role::Implementation);
            if position < MAX_CODE_TARGETS {
                self.found.primary.push(id);
            } else {
                self.found.extra.push(id);
            }
        }
        for (fact, evidence) in tests {
            self.ledger.fact_target(&fact, Role::Test);
            for evidence in evidence {
                self.found.tests.push((fact.id.clone(), evidence));
            }
        }
    }
}
