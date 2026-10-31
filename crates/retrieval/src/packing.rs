use anyhow::Result;
use code_intel_core::{ContextItem, ContextResponse, Edge};

use crate::{
    Detail, Format, legacy,
    optimized::{Candidate, contains},
};

pub(super) fn pack(
    mut response: ContextResponse,
    candidates: &[Candidate],
    edges: &[Edge],
    format: Format,
    detail: Detail,
) -> Result<ContextResponse> {
    legacy::measure(&mut response, format)?;
    let budget = response.budget.max_tokens;
    if response.estimated_tokens > budget {
        return Err(crate::BudgetTooSmall {
            minimum: response.estimated_tokens,
        }
        .into());
    }
    // Code-only retrieval retains its original packing path.
    let mut balanced;
    let (candidates, mut selected) = if response.knowledge_scope.as_deref() == Some("code,docs") {
        balanced = candidates.to_vec();
        let selected = seed_scopes(&mut balanced, &response, edges, format, detail)?;
        (balanced.as_slice(), selected)
    } else {
        (candidates, Vec::new())
    };
    if !selected.is_empty() {
        response = build(&response, &selected, candidates, edges, detail);
        legacy::measure(&mut response, format)?;
    }
    loop {
        let mut best: Option<(f64, String, Vec<usize>, ContextResponse)> = None;
        for (index, candidate) in candidates.iter().enumerate() {
            if selected.contains(&index) || covered(index, &selected, candidates, detail) {
                continue;
            }
            let mut trial: Vec<_> = selected
                .iter()
                .copied()
                .filter(|old| {
                    detail != Detail::Detailed || !contains(&candidate.fact, &candidates[*old].fact)
                })
                .collect();
            trial.push(index);
            let mut proposal = build(&response, &trial, candidates, edges, detail);
            legacy::measure(&mut proposal, format)?;
            if proposal.estimated_tokens > budget {
                continue;
            }
            let marginal = marginal_value(index, &selected, candidates, detail);
            let cost = proposal
                .estimated_tokens
                .saturating_sub(response.estimated_tokens)
                .max(1);
            let value = marginal / f64::from(u32::try_from(cost)?)
                + if candidate.forced { 1000.0 } else { 0.0 };
            let replace = best.as_ref().is_none_or(|(old, id, _, _)| {
                value.total_cmp(old).is_gt()
                    || (value.total_cmp(old).is_eq() && candidate.fact.id < *id)
            });
            if replace {
                best = Some((value, candidate.fact.id.clone(), trial, proposal));
            }
        }
        let Some((_, _, indices, proposal)) = best else {
            break;
        };
        selected = indices;
        response = proposal;
    }
    // A bounded, honest preview is preferable to silently returning nothing when
    // even the strongest complete source item cannot fit. Never fill spare space
    // with truncated lower-value candidates after complete items have been packed.
    if selected.is_empty() && detail == Detail::Detailed && !candidates.is_empty() {
        let top = candidates
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                a.score
                    .total_cmp(&b.score)
                    .then_with(|| b.fact.id.cmp(&a.fact.id))
            })
            .map(|(index, _)| index)
            .expect("nonempty candidates");
        response = build(&response, &[top], candidates, edges, detail);
        legacy::measure(&mut response, format)?;
        if candidates[top].fact.kind == "document_section" {
            crate::knowledge::fit_context_item(&mut response, &candidates[top], format)?;
        } else {
            legacy::fit_last_item(&mut response, budget, format)?;
        }
        if response.estimated_tokens > budget {
            response.items.clear();
        }
        response.truncated = true;
        response.omitted_items = candidates.len() - response.items.len();
    }
    legacy::measure(&mut response, format)?;
    if response.estimated_tokens > budget {
        return Err(crate::BudgetTooSmall {
            minimum: response.estimated_tokens,
        }
        .into());
    }
    Ok(response)
}

/// Seed the best evidence from each scope when both fit. Keep this preference
/// separate from confirmed relationships: it only balances response coverage.
fn seed_scopes(
    candidates: &mut [Candidate],
    base: &ContextResponse,
    edges: &[Edge],
    format: Format,
    detail: Detail,
) -> Result<Vec<usize>> {
    let best = |docs: bool| {
        candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| (c.fact.kind == "document_section") == docs)
            .max_by(|(_, a), (_, b)| {
                a.score
                    .total_cmp(&b.score)
                    .then_with(|| b.fact.id.cmp(&a.fact.id))
            })
            .map(|(i, _)| i)
    };
    let (Some(code), Some(doc)) = (best(false), best(true)) else {
        return Ok(Vec::new());
    };
    let original = candidates[doc].clone();
    for cap in [original.fact.snippet.len(), 256, 128, 64] {
        candidates[doc] = crate::knowledge::preview_candidate(&original, cap);
        let mut proposal = build(base, &[code, doc], candidates, edges, detail);
        legacy::measure(&mut proposal, format)?;
        if proposal.estimated_tokens <= base.budget.max_tokens {
            return Ok(vec![code, doc]);
        }
    }
    candidates[doc] = original;
    Ok(Vec::new())
}

fn covered(index: usize, selected: &[usize], candidates: &[Candidate], detail: Detail) -> bool {
    detail == Detail::Detailed
        && selected
            .iter()
            .any(|old| contains(&candidates[*old].fact, &candidates[index].fact))
}

fn marginal_value(
    index: usize,
    selected: &[usize],
    candidates: &[Candidate],
    detail: Detail,
) -> f64 {
    let candidate = &candidates[index];
    if detail != Detail::Detailed {
        return candidate.score;
    }
    let total = candidate
        .fact
        .end_byte
        .saturating_sub(candidate.fact.start_byte)
        .max(1);
    let shared: usize = selected
        .iter()
        .map(|old| {
            let fact = &candidates[*old].fact;
            if fact.path != candidate.fact.path {
                return 0;
            }
            fact.end_byte
                .min(candidate.fact.end_byte)
                .saturating_sub(fact.start_byte.max(candidate.fact.start_byte))
        })
        .sum();
    candidate.score * f64::from(u32::try_from(total.saturating_sub(shared)).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total).unwrap_or(u32::MAX))
}

fn build(
    base: &ContextResponse,
    selected: &[usize],
    candidates: &[Candidate],
    edges: &[Edge],
    detail: Detail,
) -> ContextResponse {
    let mut response = base.clone();
    let mut ranked = selected.to_vec();
    ranked.sort_by(|a, b| {
        candidates[*b]
            .score
            .total_cmp(&candidates[*a].score)
            .then_with(|| candidates[*a].fact.id.cmp(&candidates[*b].fact.id))
    });
    response.items = ranked
        .iter()
        .map(|index| item(&candidates[*index], detail))
        .collect();
    let represented = (0..candidates.len())
        .filter(|index| selected.contains(index) || covered(*index, selected, candidates, detail))
        .count();
    response.omitted_items = candidates.len() - represented;
    response.truncated = response.omitted_items > 0 || response.candidate_limit_reached;
    response.truncated |= selected.iter().any(|index| candidates[*index].truncated);
    response.truncated |= response
        .items
        .iter()
        .any(|item| item.snippet_truncated == Some(true));
    response.relationships = if detail == Detail::Detailed {
        Vec::new()
    } else {
        edges
            .iter()
            .filter(|edge| {
                response.items.iter().any(|item| item.id == edge.source_id)
                    && response
                        .items
                        .iter()
                        .any(|item| Some(&item.id) == edge.target_id.as_ref())
            })
            .cloned()
            .collect()
    };
    response
}

fn item(candidate: &Candidate, detail: Detail) -> ContextItem {
    let fact = &candidate.fact;
    let mut document_start_byte = fact.start_byte;
    let (snippet, start_line, end_line, level) = match detail {
        Detail::Overview => (String::new(), fact.start_line, fact.end_line, "overview"),
        Detail::Focused if fact.kind == "document_section" => {
            let range = crate::excerpt::Windows::new(&fact.snippet, &candidate.matches).range(128);
            document_start_byte += range.start;
            let (start, end) = crate::excerpt::positions(&fact.snippet, &range, fact.start_line);
            (fact.snippet[range].to_owned(), start, end, "source")
        }
        Detail::Focused => fact.signature.as_ref().map_or_else(
            || {
                let preview = fact.snippet.lines().next().unwrap_or_default().to_owned();
                (
                    preview,
                    fact.start_line,
                    fact.start_line,
                    "signature_preview",
                )
            },
            |signature| {
                (
                    signature.snippet.clone(),
                    fact.start_line,
                    signature.end_line,
                    "signature",
                )
            },
        ),
        Detail::Detailed => (
            fact.snippet.clone(),
            fact.start_line,
            fact.end_line,
            "source",
        ),
    };
    ContextItem {
        start_byte: (fact.kind == "document_section" && detail != Detail::Overview)
            .then_some(document_start_byte),
        end_byte: (fact.kind == "document_section" && detail != Detail::Overview)
            .then_some(document_start_byte + snippet.len()),
        snippet_truncated: (fact.kind == "document_section").then_some(
            candidate.truncated
                || start_line > fact.provenance.start_line
                || end_line < fact.provenance.end_line,
        ),
        id: fact.id.clone(),
        kind: fact.kind.clone(),
        path: fact.path.clone(),
        start_line,
        end_line,
        snippet,
        score: (candidate.score * 1000.0).round() / 1000.0,
        why: vec![candidate.why.clone()],
        confidence: fact.provenance.confidence.clone(),
        provenance: fact.provenance.clone(),
        name: Some(fact.name.clone()),
        detail: Some(level.into()),
    }
}
