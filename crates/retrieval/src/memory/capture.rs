//! Evidence capture: resolve an explicit selector to one exact indexed
//! identity and snapshot it by value. Nothing is resolved by name or text
//! similarity; several matches are refused as ambiguous.
use std::path::{Component, Path};

use anyhow::Result;
use code_intel_core::{
    Fact,
    memory::{EvidenceRole, EvidenceSnapshot, MemoryEvidenceClass, RelationshipSnapshot},
    relationships::{EntityKind, KnowledgeEntity, ResolutionStatus, SourceRange},
};
use code_intel_store::Store;

use super::{MAX_SELECTOR_BYTES, clean, fail, indexed_hash, label};

/// What a proposer can cite. Paths are repository-relative and must be indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// An entity ID, `REQ-`/`ADR-` author ID, `knowledge-edge:` ID or fact ID.
    Id(String),
    /// An indexed Markdown document or source file.
    Source(String),
    /// A resolved static edge between two exact fact IDs.
    Edge { source: String, target: String },
}

impl Selector {
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Id(id) => id.clone(),
            Self::Source(path) => format!("source:{path}"),
            Self::Edge { source, target } => format!("edge:{source} -> {target}"),
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        let parts: Vec<&str> = match self {
            Self::Id(id) => vec![id],
            Self::Source(path) => vec![path],
            Self::Edge { source, target } => vec![source, target],
        };
        for part in parts {
            if part.is_empty()
                || part.len() > MAX_SELECTOR_BYTES
                || part.chars().any(char::is_control)
            {
                return fail(
                    "invalid_arguments",
                    format!(
                        "evidence selectors must be 1..={MAX_SELECTOR_BYTES} bytes without control characters"
                    ),
                );
            }
        }
        if let Self::Source(path) = self {
            let normal = !path.contains('\\')
                && Path::new(path)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)));
            if !normal {
                return fail(
                    "invalid_path",
                    format!(
                        "{:?} must be a repository-relative path without `..`, `.`, a root or backslashes",
                        clean(path)
                    ),
                );
            }
        }
        Ok(())
    }
}

/// Identity and provenance before the source bytes are read.
struct Found {
    class: MemoryEvidenceClass,
    subject_id: String,
    explicit_id: Option<String>,
    kind: String,
    title: String,
    citation: SourceRange,
    relationship: Option<RelationshipSnapshot>,
    decision_status: Option<String>,
    validation: &'static str,
}

/// Snapshot one selector. The cited file must currently match the index, so
/// the recorded bytes are both what was indexed and what was on disk.
pub(super) fn capture(
    store: &Store,
    root: &Path,
    selector: &Selector,
    role: EvidenceRole,
) -> Result<EvidenceSnapshot> {
    selector.validate()?;
    let found = resolve(store, selector)?;
    if role == EvidenceRole::Backing && !found.class.can_back() {
        return fail(
            "evidence_cannot_back",
            format!(
                "{} is {}; only recorded intent, validated knowledge references and verified static relationships can back a claim (pass it as context instead)",
                clean(&selector.text()),
                label(&found.class)
            ),
        );
    }
    let path = &found.citation.path;
    let Some(indexed) = indexed_hash(store, path)? else {
        return fail(
            "evidence_not_found",
            format!("{:?} is not indexed", clean(path)),
        );
    };
    let bytes = match code_intel_index::selected_source_bytes(root, path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return fail(
                "evidence_not_current",
                format!(
                    "{:?} was deleted since indexing; run `repogi index <path>` and cite current evidence",
                    clean(path)
                ),
            );
        }
        Err(error) => {
            return fail(
                "evidence_unreadable",
                format!("{:?}: {error:#}", clean(path)),
            );
        }
    };
    if blake3::hash(&bytes).to_hex().as_str() != indexed {
        return fail(
            "evidence_not_current",
            format!(
                "{:?} changed since indexing; run `repogi index <path>` before citing it",
                clean(path)
            ),
        );
    }
    let Some(cited) = bytes.get(found.citation.start_byte..found.citation.end_byte) else {
        return fail(
            "evidence_not_current",
            format!("the indexed range of {:?} is outside the file", clean(path)),
        );
    };
    Ok(EvidenceSnapshot {
        class: found.class,
        role,
        selector: clean(&selector.text()),
        subject_id: found.subject_id,
        explicit_id: found.explicit_id,
        kind: found.kind,
        title: found.title,
        range_hash: blake3::hash(cited).to_hex().to_string(),
        citation: found.citation,
        source_hash: indexed,
        index_revision: store.revision()?,
        relationship: found.relationship,
        decision_status: found.decision_status,
        validation: found.validation.into(),
    })
}

fn resolve(store: &Store, selector: &Selector) -> Result<Found> {
    let knowledge = store.knowledge_available()? && store.relationships_available()?;
    match selector {
        Selector::Id(id) if id.starts_with("knowledge-edge:") => {
            if !knowledge {
                return fail(
                    "knowledge_unavailable",
                    "knowledge relationships are not indexed; run `repogi index <path>`",
                );
            }
            relationship(store, id)
        }
        Selector::Id(id) => {
            let matches = if knowledge {
                store.knowledge_entities(id, 3)?
            } else {
                store
                    .fact(id)?
                    .map(|fact| code_entity(&fact))
                    .into_iter()
                    .collect()
            };
            match matches.as_slice() {
                [] => fail(
                    "evidence_not_found",
                    format!(
                        "{:?} matches no indexed entity, author ID, relationship or fact ID; names are never resolved by similarity",
                        clean(id)
                    ),
                ),
                [entity] => Ok(entity_evidence(
                    entity,
                    if entity.explicit_id.as_deref() == Some(id.as_str()) && entity.id != *id {
                        "explicit_author_id"
                    } else {
                        "exact_indexed_identity"
                    },
                )),
                several => fail(
                    "ambiguous_evidence",
                    format!(
                        "{:?} matches {} indexed entities ({}); cite one exact entity ID",
                        clean(id),
                        several.len(),
                        several
                            .iter()
                            .map(|e| clean(&e.id))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
            }
        }
        Selector::Source(path) => {
            if knowledge
                && code_intel_index::supported_document(Path::new(path))
                && let Some(entity) = store
                    .knowledge_entities(&format!("document:{path}"), 1)?
                    .into_iter()
                    .next()
            {
                return Ok(entity_evidence(&entity, "indexed_repository_path"));
            }
            match store.fact(&code_intel_core::file_id(path))? {
                Some(fact) => Ok(entity_evidence(
                    &code_entity(&fact),
                    "indexed_repository_path",
                )),
                None => fail(
                    "evidence_not_found",
                    format!(
                        "{:?} is not an indexed document or source file",
                        clean(path)
                    ),
                ),
            }
        }
        Selector::Edge { source, target } => static_edge(store, source, target),
    }
}

fn relationship(store: &Store, id: &str) -> Result<Found> {
    let Some(edge) = store.knowledge_relationship(id)? else {
        return fail(
            "evidence_not_found",
            format!("no knowledge relationship {:?}", clean(id)),
        );
    };
    let class = match edge.status {
        ResolutionStatus::Validated => MemoryEvidenceClass::ValidatedKnowledgeReference,
        ResolutionStatus::Stale => MemoryEvidenceClass::StaleReference,
        _ => MemoryEvidenceClass::UnresolvedReference,
    };
    let owner = store
        .knowledge_entities(&edge.source_id, 1)?
        .into_iter()
        .next();
    Ok(Found {
        class,
        subject_id: edge.id.clone(),
        explicit_id: owner.as_ref().and_then(|o| o.explicit_id.clone()),
        kind: "knowledge_relationship".into(),
        title: clean(&format!(
            "{} {} {}",
            owner
                .as_ref()
                .and_then(|o| o.explicit_id.as_deref())
                .unwrap_or(&edge.source_id),
            label(&edge.kind),
            edge.target
        )),
        citation: edge.evidence.clone(),
        relationship: Some(RelationshipSnapshot {
            id: Some(edge.id.clone()),
            kind: label(&edge.kind),
            source_id: edge.source_id.clone(),
            target_id: edge.target_id.clone(),
            target_text: clean(&edge.target),
            status: label(&edge.status),
            target_path: edge.target_path.clone(),
            source_name: None,
            target_name: None,
        }),
        decision_status: None,
        validation: if edge.status == ResolutionStatus::Validated {
            "validated_knowledge_relationship"
        } else {
            "recorded_relationship_status"
        },
    })
}

fn static_edge(store: &Store, source: &str, target: &str) -> Result<Found> {
    let (Some(from), Some(to)) = (store.fact(source)?, store.fact(target)?) else {
        return fail(
            "evidence_not_found",
            "both static edge endpoints must be exact indexed fact IDs",
        );
    };
    let edges = store.semantic_edges_between(source, target, 2)?;
    let Some(edge) = edges.first() else {
        return fail(
            "evidence_not_found",
            format!(
                "no resolved static edge from {:?} to {:?}; unresolved sites are never confirmed",
                clean(source),
                clean(target)
            ),
        );
    };
    let kind = label(&edge.kind);
    Ok(Found {
        class: MemoryEvidenceClass::VerifiedStaticRelationship,
        subject_id: format!("static-edge:{source} {kind} {target}"),
        explicit_id: None,
        kind: "static_relationship".into(),
        title: clean(&format!("{} {kind} {}", from.name, to.name)),
        citation: super::edge_range(edge),
        relationship: Some(RelationshipSnapshot {
            id: None,
            kind,
            source_id: source.into(),
            target_id: Some(target.into()),
            target_text: clean(&edge.target_text),
            status: label(&edge.status),
            target_path: Some(to.path.clone()),
            source_name: Some(clean(&from.name)),
            target_name: Some(clean(&to.name)),
        }),
        decision_status: None,
        validation: "resolved_static_edge",
    })
}

fn code_entity(fact: &Fact) -> KnowledgeEntity {
    KnowledgeEntity {
        id: fact.id.clone(),
        explicit_id: None,
        kind: if fact.kind == "file" {
            EntityKind::CodeFile
        } else {
            EntityKind::CodeSymbol
        },
        title: fact.name.clone(),
        source: super::fact_range(fact),
        content_hash: String::new(),
        decision_status: None,
        acceptance_criteria: vec![],
        diagnostics: vec![],
    }
}

fn entity_evidence(entity: &KnowledgeEntity, validation: &'static str) -> Found {
    let (class, kind) = match entity.kind {
        EntityKind::Requirement => (MemoryEvidenceClass::RecordedIntent, "requirement"),
        EntityKind::ArchitectureDecision => {
            (MemoryEvidenceClass::RecordedIntent, "architecture_decision")
        }
        EntityKind::Document => (MemoryEvidenceClass::RecordedIntent, "document"),
        EntityKind::DocumentSection => (MemoryEvidenceClass::RecordedIntent, "document_section"),
        EntityKind::CodeFile => (MemoryEvidenceClass::IndexedCodeLocation, "file"),
        EntityKind::CodeSymbol => (MemoryEvidenceClass::IndexedCodeLocation, "code_symbol"),
    };
    Found {
        class,
        subject_id: entity.id.clone(),
        explicit_id: entity.explicit_id.clone(),
        kind: kind.into(),
        title: clean(&entity.title),
        citation: entity.source.clone(),
        relationship: None,
        decision_status: entity.decision_status.as_deref().map(clean),
        validation,
    }
}
