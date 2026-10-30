use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use code_intel_core::{Budget, ContextResponse, Edge, Fact, SCHEMA_VERSION};
use code_intel_store::Store;

use crate::{Detail, Format, Options, Scope, Strategy, packing, semantic};

const LIMIT: usize = 128;
const SEEDS: usize = 16;
const MODULES: u32 = 24;

pub(super) struct Candidate {
    pub fact: Fact,
    pub score: f64,
    pub why: String,
    pub forced: bool,
}

pub(super) fn context(
    store: &Store,
    query: &str,
    budget: usize,
    format: Format,
    options: &Options,
) -> Result<ContextResponse> {
    ensure!(
        !query.trim().is_empty(),
        "task must not be empty; provide symbol names or task keywords"
    );
    ensure!(
        query.len() <= 8192,
        "task exceeds 8192 UTF-8 bytes; provide a shorter query"
    );
    let terms: Vec<String> = words(query)
        .into_iter()
        .take(32)
        .map(str::to_lowercase)
        .collect();
    ensure!(
        !terms.is_empty(),
        "task must contain at least one word or identifier"
    );
    let (mut candidates, mut limited) = if options.scope == Scope::Docs {
        (BTreeMap::new(), false)
    } else {
        lexical(store, &terms)?
    };
    limited |= options.selection_truncated;
    if options.strategy == Strategy::Semantic && options.scope != Scope::Docs {
        limited |= semantic::discover(store, &terms, &mut candidates)?;
    }
    if !options.select.is_empty() {
        candidates.clear();
        for id in &options.select {
            let fact = store.fact(id)?;
            ensure!(
                fact.is_some(),
                "selected ID {id} is absent from this index; request a new overview after re-indexing"
            );
            candidates.insert(
                id.clone(),
                Candidate {
                    fact: fact.expect("checked above"),
                    score: 200.0,
                    why: "selected ID".into(),
                    forced: true,
                },
            );
        }
    }
    let mut relationships = Vec::new();
    if options.scope != Scope::Docs
        && !options.skip_dependencies
        && (options.select.is_empty() || options.detail != Detail::Detailed)
    {
        limited |= if options.strategy == Strategy::Semantic {
            semantic::dependencies(store, &mut candidates, &mut relationships)?
        } else {
            dependencies(store, &mut candidates, &mut relationships)?
        };
    }
    if options.scope != Scope::Code {
        let (documents, cap) = crate::knowledge::candidates(store, query)?;
        limited |= cap;
        candidates.extend(
            documents
                .into_iter()
                .map(|candidate| (candidate.fact.id.clone(), candidate)),
        );
    }
    let total = candidates.len();
    let response = ContextResponse {
        schema_version: SCHEMA_VERSION,
        query: query.into(),
        index_revision: store.revision()?,
        budget: Budget {
            max_tokens: budget,
            method: "estimated_utf8_bytes".into(),
        },
        estimated_tokens: 0,
        items: Vec::new(),
        truncated: total > 0 || limited,
        omitted_items: total,
        candidate_limit_reached: limited,
        syntax_error_files: store.status()?.syntax_error_files,
        detail: Some(options.detail.label().into()),
        relationships: Vec::new(),
        navigation: None,
        knowledge_scope: (options.scope != Scope::Code).then(|| options.scope.label().into()),
        knowledge_skipped_files: if options.scope == Scope::Code {
            None
        } else {
            Some(store.document_skipped()?)
        },
    };
    packing::pack(
        response,
        &candidates.into_values().collect::<Vec<_>>(),
        &relationships,
        format,
        options.detail,
    )
}

fn lexical(store: &Store, terms: &[String]) -> Result<(BTreeMap<String, Candidate>, bool)> {
    let expression = terms
        .iter()
        .map(|term| format!("\"{term}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let hits = store.search(&expression, u32::try_from(LIMIT + 1)?)?;
    let limited = hits.len() > LIMIT;
    let best_name = hits
        .iter()
        .filter(|fact| declaration(fact))
        .map(|fact| name_score(&fact.name, terms))
        .fold(0.0, f64::max);
    let mut candidates = BTreeMap::new();
    for (position, fact) in hits.into_iter().take(LIMIT).enumerate() {
        let identifier = name_score(&fact.name, terms);
        if best_name > 0.0 && (!declaration(&fact) || identifier < best_name * 0.25) {
            continue;
        }
        if best_name == 0.0 && matches!(fact.kind.as_str(), "import" | "export") {
            continue;
        }
        let body: BTreeSet<String> = words(&fact.snippet)
            .into_iter()
            .map(str::to_lowercase)
            .collect();
        let matches = terms
            .iter()
            .filter(|term| body.iter().any(|word| word.starts_with(term.as_str())))
            .count();
        let relevance = identifier
            + 3.0 * f64::from(u32::try_from(matches)?)
            + 1.0 / f64::from(u32::try_from(position + 1)?);
        if relevance < 1.0 {
            continue;
        }
        let why = if identifier > 0.0 {
            "identifier match"
        } else {
            "lexical source match"
        };
        candidates.insert(
            fact.id.clone(),
            Candidate {
                fact,
                score: relevance,
                why: why.into(),
                forced: false,
            },
        );
    }
    // File previews add no new declaration evidence when a real declaration matched.
    let paths: BTreeSet<String> = candidates
        .values()
        .filter(|candidate| declaration(&candidate.fact))
        .map(|candidate| candidate.fact.path.clone())
        .collect();
    candidates.retain(|_, candidate| {
        candidate.fact.kind != "file" || !paths.contains(&candidate.fact.path)
    });
    Ok((candidates, limited))
}

fn name_score(name: &str, terms: &[String]) -> f64 {
    let base = name.rsplit('.').next().unwrap_or(name).to_lowercase();
    let components: Vec<String> = name
        .split(|character: char| !character.is_alphanumeric())
        .flat_map(|word| {
            let mut pieces = Vec::new();
            let mut start = 0;
            for (offset, character) in word.char_indices() {
                if offset > start && character.is_uppercase() {
                    pieces.push(word[start..offset].to_lowercase());
                    start = offset;
                }
            }
            pieces.push(word[start..].to_lowercase());
            pieces
        })
        .collect();
    terms
        .iter()
        .map(|term| {
            if base == *term {
                100.0
            } else if components.contains(term) {
                40.0
            } else if base.starts_with(term) {
                12.0
            } else {
                0.0
            }
        })
        .sum()
}

fn dependencies(
    store: &Store,
    candidates: &mut BTreeMap<String, Candidate>,
    relationships: &mut Vec<Edge>,
) -> Result<bool> {
    let mut ranked: Vec<_> = candidates
        .values()
        .map(|candidate| (candidate.fact.clone(), candidate.score))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
    let mut frontier: Vec<_> = ranked.into_iter().take(SEEDS).collect();
    let mut visited = BTreeSet::new();
    let mut cache = BTreeMap::new();
    let mut limited = false;
    for _ in 0..2 {
        let mut next = Vec::new();
        for (source, rank) in frontier {
            if !visited.insert(source.id.clone()) {
                continue;
            }
            let identifiers: BTreeSet<&str> = words(&source.snippet).into_iter().collect();
            let mut paths = vec![source.path.clone()];
            let modules = store.module_edges(&source.path, MODULES + 1)?;
            limited |= modules.len() > MODULES as usize;
            for edge in modules.into_iter().take(MODULES as usize) {
                if let Some(target) = edge
                    .target_id
                    .as_deref()
                    .and_then(|id| id.strip_prefix("file:"))
                {
                    paths.push(target.into());
                }
            }
            for path in paths {
                if !cache.contains_key(&path) {
                    let facts = store.file_facts(&path, u32::try_from(LIMIT + 1)?)?;
                    limited |= facts.len() > LIMIT;
                    cache.insert(path.clone(), facts);
                }
                let facts = &cache[&path];
                let mut names: BTreeMap<&str, Vec<&Fact>> = BTreeMap::new();
                for fact in facts.iter().take(LIMIT).filter(|fact| declaration(fact)) {
                    names
                        .entry(fact.name.rsplit('.').next().unwrap_or(&fact.name))
                        .or_default()
                        .push(fact);
                }
                for (name, targets) in names {
                    if targets.len() != 1 || !identifiers.contains(name) {
                        continue;
                    }
                    let target = targets[0];
                    if target.id == source.id || contains(&source, target) {
                        continue;
                    }
                    // Textual co-occurrence within the same file or an outgoing
                    // module candidate is evidence for a dependency *candidate*.
                    // No binding, call or reference resolution is asserted.
                    let mut provenance = source.provenance.clone();
                    provenance.confidence = "textual_heuristic".into();
                    provenance.extraction_method =
                        "identifier_in_source_and_local_or_module_candidate".into();
                    let edge = Edge {
                        resolution: None,
                        evidence: None,
                        location: None,
                        source_id: source.id.clone(),
                        target_id: Some(target.id.clone()),
                        target_text: Some(name.into()),
                        kind: "textual_dependency".into(),
                        provenance,
                    };
                    if relationships.len() < LIMIT
                        && !relationships.iter().any(|old| {
                            old.source_id == edge.source_id && old.target_id == edge.target_id
                        })
                    {
                        relationships.push(edge);
                    } else if relationships.len() >= LIMIT {
                        limited = true;
                    }
                    let boost = rank * 0.35;
                    if candidates.len() < LIMIT && !candidates.contains_key(&target.id) {
                        candidates.insert(
                            target.id.clone(),
                            Candidate {
                                fact: target.clone(),
                                score: boost,
                                why: "graph textual dependency (heuristic)".into(),
                                forced: false,
                            },
                        );
                        if next.len() < SEEDS {
                            next.push((target.clone(), boost));
                        }
                    } else if candidates.len() >= LIMIT {
                        limited = true;
                    }
                }
            }
        }
        frontier = next;
    }
    Ok(limited)
}

fn words(source: &str) -> Vec<&str> {
    source
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .collect()
}

fn declaration(fact: &Fact) -> bool {
    !matches!(fact.kind.as_str(), "file" | "import" | "export")
}

pub(super) fn contains(parent: &Fact, child: &Fact) -> bool {
    parent.path == child.path
        && parent.start_byte <= child.start_byte
        && parent.end_byte >= child.end_byte
}
