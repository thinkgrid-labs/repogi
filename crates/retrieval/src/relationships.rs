//! Bounded incident-edge traversal of source claims, separate from code dependencies.
use crate::{Format, signal::Inspection};
use anyhow::{Result, ensure};
use code_intel_core::relationships::{CLAIM_NOTICE, KnowledgeEntity, KnowledgeRelationship};
use code_intel_store::Store;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write,
};

const MAX_ENTITIES: usize = 128;
const MAX_EDGES: usize = 256;

#[derive(Debug, Serialize)]
pub struct TraceResponse {
    pub schema_version: u32,
    pub selector: String,
    pub selection: String,
    pub selected_ids: Vec<String>,
    pub index_revision: u64,
    pub notice: &'static str,
    pub depth: u32,
    pub entities: Vec<KnowledgeEntity>,
    pub relationships: Vec<KnowledgeRelationship>,
    pub source_freshness: BTreeMap<String, String>,
    pub visited_entities: usize,
    pub discovered_relationships: usize,
    pub work_limit_reached: bool,
    pub depth_limit_reached: bool,
    pub omitted_entities: usize,
    pub omitted_relationships: usize,
    pub truncated: bool,
    pub max_bytes: usize,
    pub used_bytes: usize,
}

pub fn render(response: &TraceResponse, format: Format) -> Result<String> {
    if format == Format::Json {
        return Ok(format!("{}\n", serde_json::to_string(response)?));
    }
    let mut text = format!(
        "Knowledge trace: {} [{}]; r{}; depth {}\n{}\n{} / {} bytes; truncated: {}; work limit: {}; depth frontier: {}\n",
        response.selector.escape_debug(),
        response.selection,
        response.index_revision,
        response.depth,
        response.notice,
        response.used_bytes,
        response.max_bytes,
        response.truncated,
        response.work_limit_reached,
        response.depth_limit_reached
    );
    for entity in &response.entities {
        writeln!(
            text,
            "\n{:?} {} | {}:{}-{} | status={} | {}",
            entity.kind,
            entity.explicit_id.as_deref().unwrap_or(&entity.id),
            entity.source.path.escape_debug(),
            entity.source.start_line,
            entity.source.end_line,
            entity.decision_status.as_deref().unwrap_or("undeclared"),
            entity.title
        )?;
        for diagnostic in &entity.diagnostics {
            writeln!(text, "diagnostic: {diagnostic}")?;
        }
    }
    for edge in &response.relationships {
        writeln!(
            text,
            "\n{} --{:?}--> {} [{:?}; {}; freshness={}]\nevidence: {}:{}-{} bytes {}..{}{}",
            edge.source_id.escape_debug(),
            edge.kind,
            edge.target.escape_debug(),
            edge.status,
            edge.rule,
            edge.freshness,
            edge.evidence.path.escape_debug(),
            edge.evidence.start_line,
            edge.evidence.end_line,
            edge.evidence.start_byte,
            edge.evidence.end_byte,
            edge.reason
                .as_ref()
                .map_or(String::new(), |r| format!("; {r}"))
        )?;
    }
    writeln!(
        text,
        "\nsource freshness: {:?}\nomitted: {} entities, {} relationships",
        response.source_freshness, response.omitted_entities, response.omitted_relationships
    )?;
    Ok(text)
}
fn measure(response: &mut TraceResponse, format: Format) -> Result<()> {
    for _ in 0..8 {
        let bytes = render(response, format)?.len();
        if bytes == response.used_bytes {
            break;
        }
        response.used_bytes = bytes;
    }
    Ok(())
}

pub fn trace(
    store: &Store,
    selector: &str,
    depth: u32,
    max_bytes: usize,
    format: Format,
) -> Result<TraceResponse> {
    ensure!(
        !selector.is_empty() && selector.len() <= 1024 && !selector.chars().any(char::is_control),
        "knowledge selector must be 1..=1024 bytes without controls"
    );
    ensure!(depth <= 8, "knowledge depth must be 0..=8");
    ensure!(
        max_bytes <= 8_000_000,
        "knowledge budget exceeds 8000000 bytes"
    );
    let matches = store.knowledge_entities(selector, 17)?;
    let selection = match matches.len() {
        0 => "not_found",
        1 => "unique",
        _ => "ambiguous",
    };
    let mut queue = VecDeque::new();
    let mut entities = BTreeMap::new();
    let mut edges = BTreeMap::new();
    let mut selected_ids = Vec::new();
    let mut limited = matches.len() > 16;
    for entity in matches.into_iter().take(16) {
        if selection == "unique" {
            queue.push_back((entity.id.clone(), 0));
        }
        selected_ids.push(entity.id.clone());
        entities.insert(entity.id.clone(), entity);
    }
    let mut visited = BTreeSet::new();
    let mut frontier = false;
    while let Some((id, level)) = queue.pop_front() {
        store.check_cancelled()?;
        if !visited.insert(id.clone()) {
            continue;
        }
        if visited.len() > MAX_ENTITIES {
            limited = true;
            break;
        }
        let neighbors = store.knowledge_neighbors(&id, 257)?;
        if level == depth {
            // A frontier exists only when an incident edge was not already traversed.
            frontier |= neighbors.iter().any(|e| !edges.contains_key(&e.id));
            continue;
        }
        for edge in neighbors {
            if edges.contains_key(&edge.id) {
                continue;
            }
            if edges.len() == MAX_EDGES {
                limited = true;
                break;
            }
            for next in std::iter::once(&edge.source_id).chain(edge.target_id.iter()) {
                if !entities.contains_key(next) {
                    if entities.len() == MAX_ENTITIES {
                        limited = true;
                        continue;
                    }
                    if let Some(entity) = store.knowledge_entities(next, 1)?.into_iter().next() {
                        queue.push_back((next.clone(), level + 1));
                        entities.insert(next.clone(), entity);
                    }
                }
            }
            edges.insert(edge.id.clone(), edge);
        }
        if limited {
            break;
        }
    }
    let freshness = inspect(store, &entities, &mut edges)?;
    let mut response = TraceResponse {
        schema_version: 1,
        selector: selector.into(),
        selection: selection.into(),
        selected_ids,
        index_revision: store.revision()?,
        notice: CLAIM_NOTICE,
        depth,
        visited_entities: entities.len(),
        discovered_relationships: edges.len(),
        entities: entities.into_values().collect(),
        relationships: edges.into_values().collect(),
        source_freshness: freshness,
        work_limit_reached: limited,
        depth_limit_reached: frontier,
        omitted_entities: 0,
        omitted_relationships: 0,
        truncated: limited || frontier,
        max_bytes,
        used_bytes: 0,
    };
    prioritize(&mut response);
    fit(&mut response, format)?;
    Ok(response)
}

fn prioritize(response: &mut TraceResponse) {
    response.entities.sort_by(|a, b| {
        (!response.selected_ids.contains(&a.id), &a.id)
            .cmp(&(!response.selected_ids.contains(&b.id), &b.id))
    });
    response.relationships.sort_by(|a, b| {
        (
            !response.selected_ids.contains(&a.source_id),
            a.kind == code_intel_core::relationships::RelationshipKind::Contains,
            &a.id,
        )
            .cmp(&(
                !response.selected_ids.contains(&b.source_id),
                b.kind == code_intel_core::relationships::RelationshipKind::Contains,
                &b.id,
            ))
    });
}

fn inspect(
    store: &Store,
    entities: &BTreeMap<String, KnowledgeEntity>,
    edges: &mut BTreeMap<String, KnowledgeRelationship>,
) -> Result<BTreeMap<String, String>> {
    let paths = entities.values().map(|e| e.source.path.clone()).chain(
        edges
            .values()
            .flat_map(|e| std::iter::once(e.evidence.path.clone()).chain(e.target_path.clone())),
    );
    let inspection = Inspection::new(store, paths.clone())?;
    let freshness: BTreeMap<_, _> = paths
        .map(|p| {
            let s = inspection.source_state(&p).to_owned();
            (p, s)
        })
        .collect();
    for edge in edges.values_mut() {
        let source = inspection.source_state(&edge.evidence.path);
        let target = edge
            .target_path
            .as_deref()
            .map(|p| inspection.source_state(p));
        edge.freshness = if edge.status == code_intel_core::relationships::ResolutionStatus::Stale
            || source == "stale"
            || source == "missing"
            || target.is_some_and(|s| s == "stale" || s == "missing")
        {
            "stale"
        } else if source == "matched_at_read" && target.is_none_or(|s| s == "matched_at_read") {
            "matched_at_read"
        } else {
            "unverified"
        }
        .into();
    }
    Ok(freshness)
}

fn fit(response: &mut TraceResponse, format: Format) -> Result<()> {
    loop {
        let paths: BTreeSet<_> = response
            .entities
            .iter()
            .map(|e| e.source.path.clone())
            .chain(response.relationships.iter().flat_map(|e| {
                std::iter::once(e.evidence.path.clone()).chain(e.target_path.clone())
            }))
            .collect();
        response
            .source_freshness
            .retain(|path, _| paths.contains(path));
        measure(response, format)?;
        if response.used_bytes <= response.max_bytes {
            break;
        }
        response.truncated = true;
        if response.relationships.pop().is_some() {
            response.omitted_relationships += 1;
        } else if response.entities.pop().is_some() {
            response.omitted_entities += 1;
        } else {
            return Err(crate::BudgetTooSmall {
                minimum: response.used_bytes,
            }
            .into());
        }
    }
    Ok(())
}

/// Relevance-gated one-hop expansion, only for the explicit combined scope.
/// Evidence paths live in item explanations, never in the static code edge list.
pub(super) fn context_candidates(
    store: &Store,
    candidates: &mut BTreeMap<String, crate::optimized::Candidate>,
) -> Result<bool> {
    use code_intel_core::{
        file_id,
        relationships::{RelationshipKind, ResolutionStatus},
    };
    if !store.relationships_available()? {
        return Ok(false);
    }
    let mut seeds: Vec<_> = candidates.values().collect();
    seeds.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.fact.id.cmp(&b.fact.id)));
    let mut ids = BTreeSet::new();
    for candidate in seeds.into_iter().take(8) {
        if candidate.fact.kind == "document_section" {
            if let Some(owner) =
                store.knowledge_section_owner(&candidate.fact.path, candidate.fact.start_byte)?
            {
                ids.insert(owner);
            }
        } else {
            ids.insert(candidate.fact.id.clone());
            ids.insert(file_id(&candidate.fact.path));
        }
    }
    let mut added = BTreeMap::new();
    let mut limited = false;
    let mut work = 0;
    'seeds: for id in ids {
        let links = store.knowledge_neighbors(&id, 17)?;
        limited |= links.len() > 16;
        for edge in links.into_iter().take(16) {
            work += 1;
            if work > 32 {
                limited = true;
                break 'seeds;
            }
            if edge.kind == RelationshipKind::Contains || edge.status != ResolutionStatus::Validated
            {
                continue;
            }
            let next = if edge.source_id == id {
                edge.target_id.as_deref()
            } else {
                Some(edge.source_id.as_str())
            };
            let Some(next) = next else {
                continue;
            };
            let why = format!(
                "explicit {:?} claim: {} -> {}; evidence {}:{}-{}; {}; target validated in indexed snapshot, behavior unverified",
                edge.kind,
                edge.source_id,
                edge.target,
                edge.evidence.path,
                edge.evidence.start_line,
                edge.evidence.end_line,
                edge.rule
            );
            let Some(entity) = store.knowledge_entities(next, 1)?.into_iter().next() else {
                continue;
            };
            let (linked, cap) = entity_candidates(store, entity, &why)?;
            limited |= cap;
            for candidate in linked {
                added.entry(candidate.fact.id.clone()).or_insert(candidate);
            }
            if added.len() >= 16 {
                limited = true;
                break;
            }
        }
        if added.len() >= 16 {
            break;
        }
    }
    for (id, candidate) in added.into_iter().take(16) {
        candidates
            .entry(id)
            .and_modify(|old| {
                old.why = format!("{}; {}", old.why, candidate.why);
                old.score = old.score.max(candidate.score);
            })
            .or_insert(candidate);
    }
    Ok(limited)
}

fn entity_candidates(
    store: &Store,
    entity: KnowledgeEntity,
    why: &str,
) -> Result<(Vec<crate::optimized::Candidate>, bool)> {
    use crate::optimized::Candidate;
    use code_intel_core::{Fact, Provenance, relationships::EntityKind};
    let mut linked = Vec::new();
    let mut limited = false;
    if matches!(entity.kind, EntityKind::CodeFile | EntityKind::CodeSymbol) {
        let facts = if entity.kind == EntityKind::CodeFile {
            store.file_facts(&entity.source.path, 9)?
        } else {
            store.fact(&entity.id)?.into_iter().collect()
        };
        limited = facts.len() > 8;
        for fact in facts
            .into_iter()
            .filter(|f| !matches!(f.kind.as_str(), "file" | "import" | "export"))
            .take(8)
        {
            linked.push(Candidate {
                fact,
                score: 24.0,
                why: format!("{why}; symbol located in explicitly referenced file"),
                forced: false,
                matches: vec![],
                truncated: false,
            });
        }
    } else {
        let text = store.knowledge_text(&entity, 2000)?;
        let end_byte = entity.source.start_byte + text.len();
        let end_line = entity.source.start_line
            + code_intel_core::knowledge::line_starts(&text)
                .iter()
                .filter(|at| **at > 0 && **at < text.len())
                .count();
        let fact = Fact {
            id: entity.id.clone(),
            kind: "document_section".into(),
            name: entity.title,
            path: entity.source.path.clone(),
            start_byte: entity.source.start_byte,
            end_byte,
            start_line: entity.source.start_line,
            end_line,
            snippet: text,
            signature: None,
            provenance: Provenance {
                source_path: entity.source.path,
                start_line: entity.source.start_line,
                end_line: entity.source.end_line,
                extraction_method: "explicit_knowledge_relationship".into(),
                confidence: "recorded_intent".into(),
            },
        };
        linked.push(Candidate {
            fact,
            score: 24.0,
            why: why.to_owned(),
            forced: false,
            matches: vec![],
            truncated: end_byte < entity.source.end_byte,
        });
    }
    Ok((linked, limited))
}
