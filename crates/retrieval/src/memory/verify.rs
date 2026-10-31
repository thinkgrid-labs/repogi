//! Evidence verification: re-resolve each snapshot's identity in the current
//! index, then compare the cited bytes in the working tree with the recorded
//! hash. A match proves only that those bytes are unchanged; it never proves
//! the claim. Read-only: nothing here writes, migrates or re-indexes.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use code_intel_core::{
    memory::{EvidenceRole, EvidenceSnapshot, Revision},
    relationships::{KnowledgeRelationship, ResolutionStatus, SourceRange},
};
use code_intel_store::Store;
use serde::Serialize;

use super::{MAX_VERIFY_FILES, Note, clean, indexed_hash, label};

/// Severity order: the worst backing state is the memory's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Current,
    Unverified,
    Stale,
    Unresolved,
    Missing,
}

impl State {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Unverified => "unverified",
            Self::Stale => "stale",
            Self::Unresolved => "unresolved",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceCheck {
    pub ordinal: usize,
    pub state: State,
    /// What changed since capture; empty when the evidence is current.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Note>,
    /// Where the identity is now, when it moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<SourceRange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryCheck {
    /// Worst state over backing evidence; `unverified` without backing.
    pub state: State,
    pub index_revision: u64,
    pub evidence: Vec<EvidenceCheck>,
}

enum File {
    Read(Vec<u8>),
    Missing,
    Unreadable(String),
    NotChecked,
}

/// One request's verifier; cached reads keep repeated citations consistent.
pub(crate) struct Checker<'a> {
    store: &'a Store,
    root: Option<PathBuf>,
    revision: u64,
    files: BTreeMap<String, File>,
    pub limit_reached: bool,
}

impl<'a> Checker<'a> {
    pub fn new(store: &'a Store) -> Result<Self> {
        Ok(Self {
            store,
            root: store.root()?.map(PathBuf::from),
            revision: store.revision()?,
            files: BTreeMap::new(),
            limit_reached: false,
        })
    }

    pub fn revision(&mut self, revision: &Revision) -> Result<MemoryCheck> {
        let mut evidence = Vec::new();
        let mut state: Option<State> = None;
        for (ordinal, snapshot) in revision.evidence.iter().enumerate() {
            self.store.check_cancelled()?;
            let check = self.evidence(ordinal, snapshot)?;
            if snapshot.role == EvidenceRole::Backing {
                state = Some(state.map_or(check.state, |s| s.max(check.state)));
            }
            evidence.push(check);
        }
        Ok(MemoryCheck {
            state: state.unwrap_or(State::Unverified),
            index_revision: self.revision,
            evidence,
        })
    }

    pub fn evidence(
        &mut self,
        ordinal: usize,
        snapshot: &EvidenceSnapshot,
    ) -> Result<EvidenceCheck> {
        let mut check = EvidenceCheck {
            ordinal,
            state: State::Current,
            changes: Vec::new(),
            current: None,
        };
        let Some(citation) = self.identity(snapshot, &mut check)? else {
            return Ok(check);
        };
        if citation != snapshot.citation {
            check.current = Some(citation.clone());
        }
        self.bytes(snapshot, &citation, &mut check)?;
        if self.revision != snapshot.index_revision {
            check.changes.push(Note {
                code: "index_revision_advanced",
                detail: format!(
                    "captured at index revision {}; the index is now at {}",
                    snapshot.index_revision, self.revision
                ),
            });
        }
        Ok(check)
    }

    /// Re-resolve the recorded identity. `None` ends the check (missing or
    /// unresolved identity); otherwise the current citation is returned.
    fn identity(
        &self,
        snapshot: &EvidenceSnapshot,
        check: &mut EvidenceCheck,
    ) -> Result<Option<SourceRange>> {
        let knowledge =
            self.store.knowledge_available()? && self.store.relationships_available()?;
        match snapshot.kind.as_str() {
            "knowledge_relationship"
            | "requirement"
            | "architecture_decision"
            | "document"
            | "document_section"
                if !knowledge =>
            {
                downgrade(
                    check,
                    State::Unverified,
                    "knowledge_unavailable",
                    "knowledge relationships are not indexed; run `repogi index <path>`".into(),
                );
                Ok(None)
            }
            "knowledge_relationship" => self.relationship(snapshot, check),
            "requirement" | "architecture_decision" | "document" | "document_section" => {
                self.entity(snapshot, check)
            }
            "static_relationship" => self.static_edge(snapshot, check),
            _ => self.code(snapshot, check),
        }
    }

    fn entity(
        &self,
        snapshot: &EvidenceSnapshot,
        check: &mut EvidenceCheck,
    ) -> Result<Option<SourceRange>> {
        let exact = self
            .store
            .knowledge_entities(&snapshot.subject_id, 2)?
            .into_iter()
            .find(|e| e.id == snapshot.subject_id);
        let entity = if let Some(entity) = exact {
            entity
        } else {
            let candidates = match &snapshot.explicit_id {
                Some(explicit) => self.store.knowledge_entities(explicit, 3)?,
                None => vec![],
            };
            match candidates.as_slice() {
                [] => {
                    downgrade(
                        check,
                        State::Missing,
                        "identity_removed",
                        format!("{} is no longer indexed", clean(&snapshot.subject_id)),
                    );
                    return Ok(None);
                }
                [one] => {
                    downgrade(
                        check,
                        State::Current,
                        "identity_relocated",
                        format!(
                            "{} now resolves to {}",
                            snapshot.explicit_id.as_deref().unwrap_or_default(),
                            clean(&one.id)
                        ),
                    );
                    one.clone()
                }
                several => {
                    downgrade(
                        check,
                        State::Unresolved,
                        "identity_ambiguous",
                        format!(
                            "{} now matches {} entities",
                            snapshot.explicit_id.as_deref().unwrap_or_default(),
                            several.len()
                        ),
                    );
                    return Ok(None);
                }
            }
        };
        let now = entity.decision_status.as_deref().map(clean);
        if now != snapshot.decision_status {
            let historical = matches!(
                now.as_deref(),
                Some("superseded" | "deprecated" | "rejected")
            );
            downgrade(
                check,
                if historical {
                    State::Stale
                } else {
                    State::Current
                },
                "decision_status_changed",
                format!(
                    "recorded status {} is now {}",
                    snapshot.decision_status.as_deref().unwrap_or("undeclared"),
                    now.as_deref().unwrap_or("undeclared")
                ),
            );
        }
        Ok(Some(entity.source))
    }

    fn relationship(
        &self,
        snapshot: &EvidenceSnapshot,
        check: &mut EvidenceCheck,
    ) -> Result<Option<SourceRange>> {
        let Some(recorded) = &snapshot.relationship else {
            downgrade(
                check,
                State::Unverified,
                "snapshot_incomplete",
                "relationship snapshot has no relationship".into(),
            );
            return Ok(None);
        };
        let edge = if let Some(edge) = self.store.knowledge_relationship(&snapshot.subject_id)? {
            edge
        } else {
            // IDs include the claim's byte offset: look for the same claim
            // (source, kind, target text) elsewhere in the same source.
            let same: Vec<KnowledgeRelationship> = self
                .store
                .knowledge_neighbors(&recorded.source_id, 65)?
                .into_iter()
                .filter(|e| {
                    e.source_id == recorded.source_id
                        && label(&e.kind) == recorded.kind
                        && clean(&e.target) == recorded.target_text
                })
                .collect();
            match same.as_slice() {
                [] => {
                    downgrade(
                        check,
                        State::Missing,
                        "relationship_removed",
                        format!(
                            "the explicit {} claim to {:?} is no longer indexed",
                            recorded.kind, recorded.target_text
                        ),
                    );
                    return Ok(None);
                }
                [one] => {
                    downgrade(
                        check,
                        State::Current,
                        "identity_relocated",
                        format!("the same claim is now {}", clean(&one.id)),
                    );
                    one.clone()
                }
                several => {
                    downgrade(
                        check,
                        State::Unresolved,
                        "identity_ambiguous",
                        format!("{} identical claims now exist", several.len()),
                    );
                    return Ok(None);
                }
            }
        };
        let status = label(&edge.status);
        if status != recorded.status {
            let state = match edge.status {
                ResolutionStatus::Validated | ResolutionStatus::Stale => State::Stale,
                _ => State::Unresolved,
            };
            downgrade(
                check,
                state,
                "relationship_status_changed",
                format!("recorded {} is now {status}", recorded.status),
            );
        }
        if edge.target_id != recorded.target_id {
            downgrade(
                check,
                State::Stale,
                "relationship_target_changed",
                format!(
                    "target was {} and is now {}",
                    recorded
                        .target_id
                        .as_deref()
                        .map_or_else(|| "unresolved".into(), clean),
                    edge.target_id
                        .as_deref()
                        .map_or_else(|| "unresolved".into(), clean)
                ),
            );
        }
        Ok(Some(edge.evidence))
    }

    fn static_edge(
        &self,
        snapshot: &EvidenceSnapshot,
        check: &mut EvidenceCheck,
    ) -> Result<Option<SourceRange>> {
        let Some(recorded) = &snapshot.relationship else {
            downgrade(
                check,
                State::Unverified,
                "snapshot_incomplete",
                "static edge snapshot has no relationship".into(),
            );
            return Ok(None);
        };
        let target = recorded.target_id.clone().unwrap_or_default();
        let mut endpoints = (recorded.source_id.clone(), target.clone());
        if self.store.fact(&endpoints.0)?.is_none() || self.store.fact(&endpoints.1)?.is_none() {
            let source = self.relocate(
                &recorded.source_id,
                recorded.source_name.as_deref(),
                Some(&snapshot.citation.path),
            )?;
            let target = self.relocate(
                &target,
                recorded.target_name.as_deref(),
                recorded.target_path.as_deref(),
            )?;
            match (source, target) {
                (Ok(source), Ok(target)) => {
                    downgrade(
                        check,
                        State::Current,
                        "identity_relocated",
                        "edge endpoints moved within their files".into(),
                    );
                    endpoints = (source, target);
                }
                (Err(state), _) | (_, Err(state)) => {
                    downgrade(
                        check,
                        state,
                        if state == State::Missing {
                            "identity_removed"
                        } else {
                            "identity_ambiguous"
                        },
                        "a static edge endpoint is no longer a unique indexed declaration".into(),
                    );
                    return Ok(None);
                }
            }
        }
        let edges = self
            .store
            .semantic_edges_between(&endpoints.0, &endpoints.1, 4)?;
        let Some(edge) = edges.iter().find(|e| label(&e.kind) == recorded.kind) else {
            downgrade(
                check,
                State::Missing,
                "relationship_removed",
                format!("no resolved {} edge links the endpoints now", recorded.kind),
            );
            return Ok(None);
        };
        Ok(Some(super::edge_range(edge)))
    }

    fn code(
        &self,
        snapshot: &EvidenceSnapshot,
        check: &mut EvidenceCheck,
    ) -> Result<Option<SourceRange>> {
        if let Some(fact) = self.store.fact(&snapshot.subject_id)? {
            return Ok(Some(super::fact_range(&fact)));
        }
        if snapshot.kind == "file" {
            downgrade(
                check,
                State::Missing,
                "identity_removed",
                format!("{} is no longer indexed", clean(&snapshot.citation.path)),
            );
            return Ok(None);
        }
        match self.relocate(
            &snapshot.subject_id,
            Some(&snapshot.title),
            Some(&snapshot.citation.path),
        )? {
            Ok(id) => {
                downgrade(
                    check,
                    State::Current,
                    "identity_relocated",
                    format!("the declaration is now {}", clean(&id)),
                );
                Ok(self.store.fact(&id)?.map(|f| super::fact_range(&f)))
            }
            Err(state) => {
                downgrade(
                    check,
                    state,
                    if state == State::Missing {
                        "identity_removed"
                    } else {
                        "identity_ambiguous"
                    },
                    format!(
                        "{} is not a unique indexed declaration in {}",
                        clean(&snapshot.title),
                        clean(&snapshot.citation.path)
                    ),
                );
                Ok(None)
            }
        }
    }

    /// A moved declaration keeps its kind, exact name and file. Anything else
    /// (renamed, moved file, several candidates) is not re-located.
    fn relocate(
        &self,
        id: &str,
        name: Option<&str>,
        path: Option<&str>,
    ) -> Result<std::result::Result<String, State>> {
        if self.store.fact(id)?.is_some() {
            return Ok(Ok(id.to_owned()));
        }
        let (Some(name), Some(path)) = (name, path) else {
            return Ok(Err(State::Missing));
        };
        let kind = id.split(':').next().unwrap_or_default();
        let found: Vec<String> = self
            .store
            .facts_named(name, Some(path), 4)?
            .into_iter()
            .filter(|f| f.kind == kind && f.name == name)
            .map(|f| f.id)
            .collect();
        Ok(match found.as_slice() {
            [] => Err(State::Missing),
            [one] => Ok(one.clone()),
            _ => Err(State::Unresolved),
        })
    }

    /// Compare the working-tree bytes at the current citation with the recorded hash.
    fn bytes(
        &mut self,
        snapshot: &EvidenceSnapshot,
        citation: &SourceRange,
        check: &mut EvidenceCheck,
    ) -> Result<()> {
        let path = citation.path.clone();
        let Some(indexed) = indexed_hash(self.store, &path)? else {
            downgrade(
                check,
                State::Missing,
                "source_not_indexed",
                format!("{} is no longer indexed", clean(&path)),
            );
            return Ok(());
        };
        if !self.files.contains_key(&path) {
            let file = if self.files.len() >= MAX_VERIFY_FILES {
                self.limit_reached = true;
                File::NotChecked
            } else {
                match self
                    .root
                    .as_deref()
                    .map(|root| code_intel_index::selected_source_bytes(root, &path))
                {
                    Some(Ok(Some(bytes))) => File::Read(bytes),
                    Some(Ok(None)) => File::Missing,
                    Some(Err(error)) => File::Unreadable(format!("{error:#}")),
                    None => File::Unreadable("the index records no repository root".into()),
                }
            };
            self.files.insert(path.clone(), file);
        }
        match &self.files[&path] {
            File::NotChecked => downgrade(
                check,
                State::Unverified,
                "check_limit_reached",
                format!("at most {MAX_VERIFY_FILES} files are read per request"),
            ),
            File::Missing => downgrade(
                check,
                State::Missing,
                "source_deleted",
                format!("{} was deleted from the working tree", clean(&path)),
            ),
            File::Unreadable(reason) => {
                downgrade(check, State::Unverified, "source_unreadable", clean(reason));
            }
            File::Read(bytes) => {
                if blake3::hash(bytes).to_hex().as_str() != indexed {
                    downgrade(
                        check,
                        State::Stale,
                        "source_changed_since_index",
                        format!(
                            "{} differs from the index; run `repogi index <path>`, then verify again",
                            clean(&path)
                        ),
                    );
                } else if bytes
                    .get(citation.start_byte..citation.end_byte)
                    .is_none_or(|cited| {
                        blake3::hash(cited).to_hex().as_str() != snapshot.range_hash
                    })
                {
                    downgrade(
                        check,
                        State::Stale,
                        "cited_bytes_changed",
                        format!("the cited text in {} changed since capture", clean(&path)),
                    );
                }
            }
        }
        Ok(())
    }
}

/// Record a change and keep the worst state seen.
fn downgrade(check: &mut EvidenceCheck, state: State, code: &'static str, detail: String) {
    check.state = check.state.max(state);
    check.changes.push(Note { code, detail });
}

/// A short path list for messages; never more than a few entries.
#[must_use]
pub(crate) fn changed_paths(check: &MemoryCheck, revision: &Revision) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for item in &check.evidence {
        if item.state != State::Current {
            let path = &revision.evidence[item.ordinal].citation.path;
            if !paths.contains(path) && paths.len() < 4 {
                paths.push(path.clone());
            }
        }
    }
    paths
}
