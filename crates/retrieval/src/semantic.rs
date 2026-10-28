//! Additive semantic retrieval. Frozen optimized/graph/lexical strategies are unchanged.
use crate::optimized::Candidate;
use anyhow::Result;
use code_intel_core::{
    Edge, Provenance,
    semantic::{Relationship, Resolution},
};
use code_intel_store::Store;
use std::collections::{BTreeMap, BTreeSet};

pub fn discover(
    store: &Store,
    terms: &[String],
    candidates: &mut BTreeMap<String, Candidate>,
) -> Result<bool> {
    let expression = terms
        .iter()
        .map(|term| format!("\"{term}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let hits = store.semantic_search(&expression, 129)?;
    let mut limited = hits.len() > 128;
    for fact in hits.into_iter().take(128) {
        if candidates.len() >= 128 {
            limited = true;
            break;
        }
        candidates.entry(fact.id.clone()).or_insert(Candidate {
            fact,
            score: 20.0,
            why: "normalized identifier component".into(),
            forced: false,
        });
    }
    Ok(limited)
}

pub fn dependencies(
    store: &Store,
    candidates: &mut BTreeMap<String, Candidate>,
    relationships: &mut Vec<Edge>,
) -> Result<bool> {
    let mut frontier: Vec<_> = candidates
        .values()
        .map(|candidate| (candidate.fact.id.clone(), candidate.score))
        .collect();
    frontier.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    frontier.truncate(16);
    let mut seen = BTreeSet::new();
    let mut limited = false;
    for _ in 0..2 {
        let mut next = Vec::new();
        for (id, rank) in frontier {
            if !seen.insert(id.clone()) {
                continue;
            }
            let edges = store.semantic_dependencies(&id, 129)?;
            limited |= edges.len() > 128;
            for edge in edges.into_iter().take(128).filter(|edge| {
                edge.status == Resolution::Resolved
                    && matches!(
                        edge.kind,
                        Relationship::Calls | Relationship::References | Relationship::Extends
                    )
            }) {
                let Some(target_id) = &edge.target_id else {
                    continue;
                };
                let Some(fact) = store.fact(target_id)? else {
                    continue;
                };
                if fact.kind == "file" {
                    continue;
                }
                let weight = match edge.kind {
                    Relationship::Calls => 0.5,
                    Relationship::Extends => 0.45,
                    _ => 0.3,
                };
                let relevance = rank * weight;
                if candidates.len() < 128 && !candidates.contains_key(target_id) {
                    next.push((target_id.clone(), relevance));
                    candidates.insert(
                        target_id.clone(),
                        Candidate {
                            fact,
                            score: relevance,
                            why: format!("verified static {:?} binding", edge.kind),
                            forced: false,
                        },
                    );
                } else if candidates.len() >= 128 {
                    limited = true;
                }
                if relationships.len() >= 128 {
                    limited = true;
                    continue;
                }
                relationships.push(Edge {
                    source_id: edge.source_id,
                    target_id: edge.target_id,
                    target_text: Some(edge.target_text),
                    kind: serde_json::to_value(edge.kind)?
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                    provenance: Provenance {
                        source_path: edge.location.path.clone(),
                        start_line: edge.location.start_line,
                        end_line: edge.location.end_line,
                        extraction_method: "typescript_static_binder_v1".into(),
                        confidence: "verified_static".into(),
                    },
                    resolution: Some(edge.status),
                    evidence: Some(edge.evidence),
                    location: Some(edge.location),
                });
            }
        }
        next.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        next.truncate(16);
        frontier = next;
    }
    Ok(limited)
}
