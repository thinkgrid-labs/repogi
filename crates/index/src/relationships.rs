//! Narrow, bounded declarations, using `CommonMark` events rather than prose inference.
use anyhow::{Result, ensure};
use code_intel_core::{
    knowledge::{Document, line_starts},
    relationships::{
        EntityKind, KnowledgeEntity, KnowledgeExtraction, KnowledgeRelationship, RelationshipKind,
        ResolutionStatus, SourceRange,
    },
};
use pulldown_cmark::{Event, Options, Parser, Tag};
use std::collections::{BTreeMap, BTreeSet};

const MAX_LINKS: usize = 4096;

struct Source<'a> {
    document: &'a Document,
    starts: Vec<usize>,
}
impl std::ops::Deref for Source<'_> {
    type Target = Document;
    fn deref(&self) -> &Document {
        self.document
    }
}

fn range(document: &Source<'_>, start: usize, end: usize) -> SourceRange {
    let starts = &document.starts;
    SourceRange {
        path: document.path.clone(),
        start_byte: start,
        end_byte: end,
        start_line: starts.partition_point(|at| *at <= start),
        end_line: starts.partition_point(|at| *at < end).max(1),
    }
}

fn explicit_id(text: &str, prefix: &str) -> Option<String> {
    let id = text.split([':', ' ', '\t']).next()?;
    (id.starts_with(prefix)
        && id.len() > prefix.len()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-'))
    .then(|| id.to_owned())
}

fn entity(
    document: &Source<'_>,
    id: String,
    explicit: Option<String>,
    kind: EntityKind,
    title: String,
    start: usize,
    end: usize,
) -> KnowledgeEntity {
    KnowledgeEntity {
        id,
        explicit_id: explicit,
        kind,
        title,
        source: range(document, start, end),
        content_hash: document.hash.clone(),
        decision_status: None,
        acceptance_criteria: Vec::new(),
        diagnostics: Vec::new(),
    }
}

fn edge(
    document: &Source<'_>,
    source: &str,
    target: String,
    kind: RelationshipKind,
    evidence: SourceRange,
    direct: bool,
) -> Result<KnowledgeRelationship> {
    let identity = serde_json::to_vec(&(source, &target, kind, evidence.start_byte))?;
    Ok(KnowledgeRelationship {
        id: format!("knowledge-edge:{}", blake3::hash(&identity).to_hex()),
        source_id: source.into(),
        target_id: direct.then(|| target.clone()),
        target,
        kind,
        evidence,
        source_hash: document.hash.clone(),
        target_hash: None,
        target_path: None,
        status: if direct {
            ResolutionStatus::Validated
        } else {
            ResolutionStatus::Unresolved
        },
        rule: if direct {
            "source_containment"
        } else {
            "explicit_markdown_link"
        }
        .into(),
        reason: None,
        index_revision: 0,
        freshness: "indexed_snapshot".into(),
    })
}

/// A deliberately small front-matter subset: scalar ID/status and scalar or
/// inline-list relationship values. Reject unsupported YAML for recognized keys.
fn metadata(document: &Source<'_>) -> (BTreeMap<String, (String, SourceRange)>, Vec<String>) {
    let mut values = BTreeMap::new();
    let mut diagnostics = Vec::new();
    let starts = &document.starts;
    let lines: Vec<_> = starts
        .iter()
        .enumerate()
        .map(|(i, start)| {
            (
                *start,
                starts.get(i + 1).copied().unwrap_or(document.content.len()),
            )
        })
        .collect();
    if lines
        .first()
        .is_none_or(|(s, e)| document.content[*s..*e].trim() != "---")
    {
        return (values, diagnostics);
    }
    let mut closed = false;
    for (start, end) in lines.into_iter().skip(1).take(257) {
        let line = document.content[start..end].trim();
        if end > 16_384 {
            break;
        }
        if line == "---" {
            closed = true;
            break;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            diagnostics.push("malformed metadata line".into());
            continue;
        };
        if ![
            "id",
            "status",
            "supersedes",
            "depends_on",
            "relates_to",
            "implementation",
        ]
        .contains(&key)
        {
            continue;
        }
        let value = value.trim();
        let raw = &document.content[start..end];
        if raw.starts_with(char::is_whitespace)
            || (value.contains(['[', ']'])
                && (matches!(key, "id" | "status")
                    || !value.starts_with('[')
                    || !value.ends_with(']')
                    || value.bytes().filter(|b| *b == b'[' || *b == b']').count() != 2))
        {
            diagnostics.push(format!(
                "unsupported nested or malformed metadata for {key}"
            ));
            continue;
        }
        if value.is_empty()
            || value.len() > 2048
            || value.contains(['!', '&', '*', '{', '}', '|', '>'])
            || value.starts_with(['\'', '"'])
        {
            diagnostics.push(format!("unsupported metadata value for {key}"));
            continue;
        }
        if values
            .insert(key.into(), (value.into(), range(document, start, end)))
            .is_some()
        {
            diagnostics.push(format!("duplicate metadata key: {key}"));
        }
    }
    if !closed {
        diagnostics.push("unclosed or oversized front matter".into());
    }
    // Never resolve potentially contradictory or partially parsed metadata.
    if !diagnostics.is_empty() {
        values.clear();
    }
    (values, diagnostics)
}

pub fn extract(document: &Document) -> Result<KnowledgeExtraction> {
    let source = Source {
        document,
        starts: line_starts(&document.content),
    };
    let document = &source;
    let mut result = KnowledgeExtraction::default();
    let (metadata, diagnostics) = metadata(document);
    let mut doc = entity(
        document,
        document.id.clone(),
        None,
        EntityKind::Document,
        document.path.clone(),
        0,
        document.content.len(),
    );
    doc.diagnostics = diagnostics;
    result.entities.push(doc);
    let adr = decision(document, &metadata, &mut result)?;
    let owners = requirements(document, &mut result)?;
    metadata_links(document, &metadata, adr.as_deref(), &mut result)?;
    markdown_links(document, adr.as_deref(), &owners, &mut result)?;
    Ok(result)
}

type Metadata = BTreeMap<String, (String, SourceRange)>;
type Owners = Vec<(usize, usize, String)>;

fn decision(
    document: &Source<'_>,
    metadata: &Metadata,
    result: &mut KnowledgeExtraction,
) -> Result<Option<String>> {
    let mut adr = None;
    if let Some((id, _)) = metadata.get("id") {
        if explicit_id(id, "ADR-").as_deref() == Some(id) {
            let mut decision = entity(
                document,
                format!("decision:{id}:{}", document.path),
                Some(id.clone()),
                EntityKind::ArchitectureDecision,
                document
                    .sections
                    .iter()
                    .find(|s| !s.heading.is_empty())
                    .map_or_else(|| id.clone(), |s| s.heading.clone()),
                0,
                document.content.len(),
            );
            if let Some((status, _)) = metadata.get("status") {
                if [
                    "proposed",
                    "accepted",
                    "rejected",
                    "deprecated",
                    "superseded",
                ]
                .contains(&status.as_str())
                {
                    decision.decision_status = Some(status.clone());
                } else {
                    decision
                        .diagnostics
                        .push(format!("unsupported ADR status: {status}"));
                }
            }
            adr = Some(decision.id.clone());
            result.relationships.push(edge(
                document,
                &document.id,
                decision.id.clone(),
                RelationshipKind::Contains,
                decision.source.clone(),
                true,
            )?);
            result.entities.push(decision);
        } else {
            result.entities[0]
                .diagnostics
                .push("metadata id is not a supported ADR identifier".into());
        }
    }
    Ok(adr)
}

fn requirements(document: &Source<'_>, result: &mut KnowledgeExtraction) -> Result<Owners> {
    let mut owners = Vec::new();
    let mut occurrences = BTreeMap::<String, usize>::new();
    for (index, section) in document.sections.iter().enumerate() {
        let section_entity = entity(
            document,
            section.id.clone(),
            None,
            EntityKind::DocumentSection,
            section.heading.clone(),
            section.start_byte,
            section.end_byte,
        );
        result.relationships.push(edge(
            document,
            &document.id,
            section.id.clone(),
            RelationshipKind::Contains,
            section_entity.source.clone(),
            true,
        )?);
        result.entities.push(section_entity);
        if let Some(id) = explicit_id(&section.heading, "REQ-") {
            let occurrence = occurrences.entry(id.clone()).or_default();
            *occurrence += 1;
            let key = format!("requirement:{id}:{}:{occurrence}", document.path);
            let level = section.hierarchy.len();
            let end = document
                .sections
                .iter()
                .skip(index + 1)
                .find(|next| next.hierarchy.len() <= level)
                .map_or(document.content.len(), |s| s.start_byte);
            let mut requirement = entity(
                document,
                key.clone(),
                Some(id),
                EntityKind::Requirement,
                section.heading.clone(),
                section.start_byte,
                end,
            );
            for child in document
                .sections
                .iter()
                .skip(index + 1)
                .take_while(|s| s.start_byte < end)
            {
                if child.heading.eq_ignore_ascii_case("Acceptance criteria") {
                    requirement.acceptance_criteria.push(range(
                        document,
                        child.start_byte,
                        child.end_byte,
                    ));
                }
            }
            result.relationships.push(edge(
                document,
                &section.id,
                key.clone(),
                RelationshipKind::Contains,
                range(document, section.start_byte, section.end_byte),
                true,
            )?);
            owners.push((section.start_byte, end, key));
            result.entities.push(requirement);
        }
    }
    Ok(owners)
}

fn metadata_links(
    document: &Source<'_>,
    metadata: &Metadata,
    adr: Option<&str>,
    result: &mut KnowledgeExtraction,
) -> Result<()> {
    let metadata_owner = adr.unwrap_or(&document.id);
    for (key, kind) in [
        ("supersedes", RelationshipKind::Supersedes),
        ("depends_on", RelationshipKind::DependsOn),
        ("relates_to", RelationshipKind::RelatesTo),
        ("implementation", RelationshipKind::DeclaredImplementation),
    ] {
        if let Some((value, evidence)) = metadata.get(key) {
            if kind == RelationshipKind::Supersedes && adr.is_none() {
                result.entities[0]
                    .diagnostics
                    .push("supersedes requires an explicit ADR source identifier".into());
                continue;
            }
            let value = value
                .strip_prefix('[')
                .and_then(|v| v.strip_suffix(']'))
                .unwrap_or(value);
            let mut seen = BTreeSet::new();
            for target in value.split(',').map(str::trim) {
                if !target.is_empty() && seen.insert(target) {
                    let mut relation = edge(
                        document,
                        metadata_owner,
                        target.into(),
                        kind,
                        evidence.clone(),
                        false,
                    )?;
                    relation.rule = "explicit_metadata".into();
                    result.relationships.push(relation);
                }
            }
        }
    }
    Ok(())
}

fn markdown_links(
    document: &Source<'_>,
    adr: Option<&str>,
    owners: &Owners,
    result: &mut KnowledgeExtraction,
) -> Result<()> {
    let mut links = 0;
    for (event, offsets) in Parser::new_ext(
        &document.content,
        Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
    )
    .into_offset_iter()
    {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            links += 1;
            ensure!(
                links <= MAX_LINKS,
                "document exceeds {MAX_LINKS} explicit links"
            );
            ensure!(dest_url.len() <= 2048, "reference exceeds 2048 UTF-8 bytes");
            let section = document
                .sections
                .iter()
                .rev()
                .find(|s| s.start_byte <= offsets.start);
            let owner = owners
                .iter()
                .rev()
                .find(|(start, end, _)| *start <= offsets.start && offsets.start < *end)
                .map(|(_, _, id)| id.as_str())
                .or(adr)
                .or_else(|| section.map(|s| s.id.as_str()))
                .unwrap_or(&document.id);
            // Only the exact supported label declares implementation intent.
            let declared =
                section.is_some_and(|s| s.heading.eq_ignore_ascii_case("Related implementation"));
            result.relationships.push(edge(
                document,
                owner,
                dest_url.into_string(),
                if declared {
                    RelationshipKind::DeclaredImplementation
                } else {
                    RelationshipKind::References
                },
                range(document, offsets.start, offsets.end),
                false,
            )?);
        }
    }
    Ok(())
}
