//! Frozen Phase 1 ranking and packing control for reproducible ablations.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use code_intel_core::{Budget, ContextItem, ContextResponse, Fact, SCHEMA_VERSION};
use code_intel_store::Store;

const CANDIDATE_LIMIT: u32 = 128;
const NEIGHBOR_LIMIT: u32 = 24;

use crate::Format;

struct Candidate {
    fact: Fact,
    score: f64,
    why: Vec<String>,
}

pub fn context(
    store: &Store,
    query: &str,
    max_tokens: usize,
    format: Format,
    graph: bool,
) -> Result<ContextResponse> {
    let (mut candidates, lexical_limit) = lexical_candidates(store, query)?;
    let graph_limit = graph && expand_graph(store, &mut candidates)?;
    let mut ranked: Vec<Candidate> = candidates.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.fact.id.cmp(&b.fact.id))
    });
    let total = ranked.len();
    let mut response = ContextResponse {
        schema_version: SCHEMA_VERSION,
        query: query.into(),
        index_revision: store.revision()?,
        budget: Budget {
            max_tokens,
            method: "estimated_utf8_bytes".into(),
        },
        estimated_tokens: 0,
        items: Vec::new(),
        truncated: total > 0 || lexical_limit || graph_limit,
        omitted_items: total,
        candidate_limit_reached: lexical_limit || graph_limit,
        syntax_error_files: store.status()?.syntax_error_files,
        detail: None,
        relationships: Vec::new(),
        navigation: None,
        knowledge_scope: None,
        knowledge_skipped_files: None,
    };
    measure(&mut response, format)?;
    if response.estimated_tokens > max_tokens {
        return Err(crate::BudgetTooSmall {
            minimum: response.estimated_tokens,
        }
        .into());
    }
    for candidate in ranked {
        let fact = candidate.fact;
        let mut item = ContextItem {
            id: fact.id,
            kind: fact.kind,
            path: fact.path,
            start_line: fact.start_line,
            end_line: fact.end_line,
            snippet: fact.snippet,
            score: candidate.score,
            why: candidate.why,
            confidence: fact.provenance.confidence.clone(),
            provenance: fact.provenance,
            name: None,
            detail: None,
        };
        // Keep evidence concise even for high-degree nodes.
        item.why.truncate(3);
        response.items.push(item);
        response.omitted_items = total - response.items.len();
        response.truncated = response.omitted_items > 0 || response.candidate_limit_reached;
        measure(&mut response, format)?;
        if response.estimated_tokens > max_tokens {
            fit_last_item(&mut response, max_tokens, format)?;
        }
        if response.estimated_tokens > max_tokens {
            response.items.pop();
            response.omitted_items = total - response.items.len();
            response.truncated = true;
            measure(&mut response, format)?;
        }
    }
    // Truncated snippets are also reported even if every candidate was included.
    response.truncated |= response
        .items
        .iter()
        .any(|item| item.end_line < item.provenance.end_line);
    measure(&mut response, format)?;
    if response.estimated_tokens > max_tokens {
        return Err(crate::BudgetTooSmall {
            minimum: response.estimated_tokens,
        }
        .into());
    }
    Ok(response)
}

fn lexical_candidates(store: &Store, query: &str) -> Result<(BTreeMap<String, Candidate>, bool)> {
    ensure!(
        !query.trim().is_empty(),
        "task must not be empty; provide symbol names or task keywords"
    );
    ensure!(
        query.len() <= 8192,
        "task exceeds 8192 UTF-8 bytes; provide a shorter query"
    );
    let terms: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|term| !term.is_empty())
        .take(32)
        .map(|term| format!("\"{term}\"*"))
        .collect();
    ensure!(
        !terms.is_empty(),
        "task must contain at least one word or identifier"
    );
    // Quote terms ourselves so user input cannot inject FTS syntax/operators.
    let hits = store.search(&terms.join(" OR "), CANDIDATE_LIMIT + 1)?;
    let mut candidates = BTreeMap::new();
    let limit_reached = hits.len() > CANDIDATE_LIMIT as usize;
    for (position, fact) in hits.into_iter().take(CANDIDATE_LIMIT as usize).enumerate() {
        let lexical_rank = 10.0 / f64::from(u32::try_from(position + 1)?);
        candidates.insert(
            fact.id.clone(),
            Candidate {
                fact,
                score: lexical_rank,
                why: vec!["FTS5 lexical match (prefix terms)".into()],
            },
        );
    }
    Ok((candidates, limit_reached))
}

fn expand_graph(store: &Store, candidates: &mut BTreeMap<String, Candidate>) -> Result<bool> {
    let seeds: Vec<(String, f64)> = candidates
        .values()
        .map(|candidate| (candidate.fact.id.clone(), candidate.score))
        .collect();
    let mut graph_limit = false;
    // At most 16 lexical seeds, 2 hops, 24 neighbors per hop, and 128 total candidates.
    let mut seeds = seeds;
    seeds.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (seed, seed_rank) in seeds.into_iter().take(16) {
        let mut frontier = vec![seed.clone()];
        for depth in 1..=2 {
            let mut next = Vec::new();
            for id in frontier {
                let edges = store.neighbors(&id, NEIGHBOR_LIMIT + 1)?;
                graph_limit |= edges.len() > NEIGHBOR_LIMIT as usize;
                for edge in edges.into_iter().take(NEIGHBOR_LIMIT as usize) {
                    let Some(target) = edge.target_id else {
                        continue;
                    };
                    let neighbor = if edge.source_id == id {
                        target
                    } else {
                        edge.source_id
                    };
                    if neighbor == seed {
                        continue;
                    }
                    let reason = format!(
                        "graph {} ({}) via {}:{}-{}",
                        edge.kind,
                        edge.provenance.confidence,
                        edge.provenance.source_path,
                        edge.provenance.start_line,
                        edge.provenance.end_line
                    );
                    let boost = seed_rank * if depth == 1 { 0.2 } else { 0.08 };
                    if let Some(candidate) = candidates.get_mut(&neighbor) {
                        if !candidate.why.contains(&reason) {
                            candidate.score += boost;
                            candidate.why.push(reason);
                        }
                    } else if candidates.len() < CANDIDATE_LIMIT as usize {
                        if let Some(fact) = store.fact(&neighbor)? {
                            candidates.insert(
                                neighbor.clone(),
                                Candidate {
                                    fact,
                                    score: boost,
                                    why: vec![reason],
                                },
                            );
                        }
                    } else {
                        graph_limit = true;
                    }
                    if depth == 1
                        && next.len() < NEIGHBOR_LIMIT as usize
                        && !next.contains(&neighbor)
                    {
                        next.push(neighbor);
                    }
                }
            }
            frontier = next;
        }
    }
    Ok(graph_limit)
}

pub(super) fn fit_last_item(
    response: &mut ContextResponse,
    max_tokens: usize,
    format: Format,
) -> Result<()> {
    let original = response
        .items
        .last()
        .expect("just pushed an item")
        .snippet
        .clone();
    let boundaries: Vec<usize> = original
        .trim_end_matches('\n')
        .match_indices('\n')
        .map(|(offset, _)| offset)
        .collect();
    // Binary search complete line prefixes rather than repeatedly dropping one line.
    let (mut low, mut high) = (0, boundaries.len());
    let mut best = None;
    while low < high {
        let middle = low + (high - low) / 2;
        let item = response.items.last_mut().expect("just pushed an item");
        item.snippet = original[..boundaries[middle]].into();
        // A prefix ending with a newline does not also contain the following
        // empty line. Derive the evidence range from the actual returned text.
        item.end_line = item.start_line + item.snippet.lines().count().saturating_sub(1);
        response.truncated = true;
        measure(response, format)?;
        if response.estimated_tokens <= max_tokens {
            best = Some(middle);
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let item = response.items.last_mut().expect("just pushed an item");
    if let Some(best) = best {
        item.snippet = original[..boundaries[best]].into();
        item.end_line = item.start_line + item.snippet.lines().count().saturating_sub(1);
    } else {
        item.snippet = original;
        item.end_line = item.provenance.end_line;
    }
    measure(response, format)
}

/// One estimated token per UTF-8 output byte (including JSON escaping, metadata,
/// query, and final newline). Intentionally conservative; not a provider tokenizer.
pub fn render(response: &ContextResponse, format: Format) -> Result<String> {
    let mut output = match format {
        Format::Json => serde_json::to_string(response)?,
        Format::Agent => crate::agent::context(response)?,
        Format::Text => {
            use std::fmt::Write;
            let mut output = format!(
                "Query: {}\nIndex revision: {}\nBudget: {} estimated tokens; used: {} (estimated_utf8_bytes)\nTruncated: {}; omitted candidates: {}; candidate limit reached: {}; syntax-error files: {}\n",
                response.query,
                response.index_revision,
                response.budget.max_tokens,
                response.estimated_tokens,
                response.truncated,
                response.omitted_items,
                response.candidate_limit_reached,
                response.syntax_error_files
            );
            if let Some(detail) = &response.detail {
                writeln!(output, "Detail: {detail}")?;
            }
            output.push_str(&crate::knowledge::context_notice(response));
            if let Some(evidence) = &response.navigation {
                output.push_str(&crate::signal::text(evidence));
            }
            for item in &response.items {
                write!(
                    output,
                    "\n{}:{}-{} [{}; {}; score {:.4}]\nWhy: {}\n{}\n",
                    item.path,
                    item.start_line,
                    item.end_line,
                    item.kind,
                    item.confidence,
                    item.score,
                    item.why.join("; "),
                    if item.detail.as_deref() == Some("overview") {
                        item.name.as_deref().unwrap_or(&item.id)
                    } else {
                        &item.snippet
                    }
                )?;
            }
            for edge in &response.relationships {
                writeln!(
                    output,
                    "{} -> {} [{}; {}; {}:{}-{}]",
                    edge.source_id,
                    edge.target_id.as_deref().unwrap_or("unresolved"),
                    edge.kind,
                    edge.provenance.confidence,
                    edge.provenance.source_path,
                    edge.provenance.start_line,
                    edge.provenance.end_line
                )?;
            }
            output
        }
    };
    output.push('\n');
    Ok(output)
}

pub(super) fn measure(response: &mut ContextResponse, format: Format) -> Result<()> {
    // Include the decimal representation of the estimate itself (fixed point).
    for _ in 0..8 {
        let estimate = render(response, format)?.len();
        if estimate == response.estimated_tokens {
            return Ok(());
        }
        response.estimated_tokens = estimate;
    }
    Ok(())
}
