//! Additive source knowledge persistence and indexed-only target validation.
use crate::Store;
use anyhow::{Result, ensure};
use code_intel_core::{
    knowledge::Document,
    relationships::{
        EntityKind, KnowledgeEntity, KnowledgeRelationship, RELATIONSHIP_IR_VERSION,
        RelationshipKind, ResolutionStatus, SourceRange,
    },
};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, BTreeSet};

impl Store {
    pub fn relationships_available(&self) -> Result<bool> {
        self.check_cancelled()?;
        let version: u32 = self
            .connection
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 4 {
            return Ok(false);
        }
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key='relationships_indexed'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .as_deref()
            == Some(&RELATIONSHIP_IR_VERSION.to_string()))
    }

    pub fn knowledge_entities(&self, selector: &str, limit: u32) -> Result<Vec<KnowledgeEntity>> {
        ensure!(
            self.relationships_available()?,
            "knowledge relationships unavailable; run `repogi index <path>` first"
        );
        ensure!(limit <= 257, "knowledge entity limit exceeds 257");
        self.check_cancelled()?;
        let mut query = self.connection.prepare("SELECT data FROM knowledge_entities WHERE id=?1 OR explicit_id=?1 ORDER BY id LIMIT ?2")?;
        let entities: Vec<KnowledgeEntity> = query
            .query_map(params![selector, limit], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        if !entities.is_empty() {
            return Ok(entities);
        }
        // Code entities preserve existing fact identities. No name-only lookup.
        if let Some(fact) = self.fact(selector)? {
            return Ok(vec![KnowledgeEntity {
                id: fact.id,
                explicit_id: None,
                kind: if fact.kind == "file" {
                    EntityKind::CodeFile
                } else {
                    EntityKind::CodeSymbol
                },
                title: fact.name,
                source: SourceRange {
                    path: fact.path.clone(),
                    start_byte: fact.start_byte,
                    end_byte: fact.end_byte,
                    start_line: fact.start_line,
                    end_line: fact.end_line,
                },
                content_hash: self.file_hash(&fact.path)?.unwrap_or_default(),
                decision_status: None,
                acceptance_criteria: vec![],
                diagnostics: vec![],
            }]);
        }
        Ok(vec![])
    }

    /// A bounded original-byte excerpt for source-backed context candidates.
    pub fn knowledge_text(&self, entity: &KnowledgeEntity, cap: u32) -> Result<String> {
        self.check_cancelled()?;
        ensure!(cap <= 8000, "knowledge excerpt exceeds 8000 bytes");
        let length = entity
            .source
            .end_byte
            .saturating_sub(entity.source.start_byte)
            .min(cap as usize);
        let bytes: Vec<u8> = self.connection.query_row(
            "SELECT substr(CAST(content AS BLOB),?2,?3) FROM documents WHERE path=?1",
            params![
                entity.source.path,
                i64::try_from(entity.source.start_byte + 1)?,
                i64::try_from(length)?
            ],
            |r| r.get(0),
        )?;
        let end = std::str::from_utf8(&bytes).map_or_else(|e| e.valid_up_to(), str::len);
        Ok(std::str::from_utf8(&bytes[..end])?.to_owned())
    }

    /// Indexed incident edges. LIMIT+1 lets callers report incomplete work.
    pub fn knowledge_neighbors(&self, id: &str, limit: u32) -> Result<Vec<KnowledgeRelationship>> {
        self.check_cancelled()?;
        ensure!(limit <= 257, "knowledge edge limit exceeds 257");
        let mut query = self.connection.prepare("SELECT data FROM knowledge_relationships WHERE source_id=?1 OR target_id=?1 ORDER BY id LIMIT ?2")?;
        query
            .query_map(params![id, limit], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }

    pub fn knowledge_section_owner(&self, path: &str, start: usize) -> Result<Option<String>> {
        self.check_cancelled()?;
        let mut q = self.connection.prepare("SELECT id FROM knowledge_entities WHERE path=?1 AND json_extract(data,'$.kind') IN ('requirement','architecture_decision') AND json_extract(data,'$.source.start_byte')<=?2 AND json_extract(data,'$.source.end_byte')>?2 ORDER BY json_extract(data,'$.source.start_byte') DESC,id LIMIT 1")?;
        Ok(
            q.query_row(params![path, i64::try_from(start)?], |r| r.get(0))
                .optional()?,
        )
    }
}

pub(super) fn insert(tx: &Transaction<'_>, document: &Document) -> Result<()> {
    for entity in &document.knowledge.entities {
        tx.execute(
            "INSERT INTO knowledge_entities(id,path,explicit_id,data) VALUES(?1,?2,?3,?4)",
            params![
                entity.id,
                document.path,
                entity.explicit_id,
                serde_json::to_string(entity)?
            ],
        )?;
    }
    for edge in &document.knowledge.relationships {
        save(tx, edge, true)?;
    }
    Ok(())
}
fn save(tx: &Transaction<'_>, edge: &KnowledgeRelationship, insert: bool) -> Result<()> {
    if insert {
        tx.execute("INSERT INTO knowledge_relationships(id,path,source_id,target_id,data) VALUES(?1,?2,?3,?4,?5)", params![edge.id,edge.evidence.path,edge.source_id,edge.target_id,serde_json::to_string(edge)?])?;
    } else {
        tx.execute(
            "UPDATE knowledge_relationships SET target_id=?2,data=?3 WHERE id=?1",
            params![edge.id, edge.target_id, serde_json::to_string(edge)?],
        )?;
    }
    Ok(())
}

fn decode(value: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut it = value.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let a = it.next().and_then(|v| char::from(v).to_digit(16));
            let b = it.next().and_then(|v| char::from(v).to_digit(16));
            ensure!(a.is_some() && b.is_some(), "malformed percent encoding");
            bytes.push(u8::try_from(a.unwrap() * 16 + b.unwrap())?);
        } else {
            bytes.push(b);
        }
    }
    Ok(String::from_utf8(bytes)?)
}

/// Decoding precedes component validation. Never inspect unindexed filesystem targets.
fn local_target(source: &str, target: &str) -> Result<(String, Option<String>)> {
    let (path, fragment) = target
        .split_once('#')
        .map_or((target, None), |(p, f)| (p, Some(f)));
    let path = decode(path)?;
    ensure!(
        !path.starts_with('/')
            && !path.contains(['\\', ':', '?', '#'])
            && !path.chars().any(char::is_control),
        "unsafe or unsupported local path"
    );
    let mut parts: Vec<_> = source.split('/').collect();
    parts.pop();
    if path.is_empty() {
        parts = source.split('/').collect();
    } else {
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    ensure!(parts.pop().is_some(), "path escapes repository");
                }
                part => parts.push(part),
            }
        }
    }
    ensure!(!parts.is_empty(), "reference is not a file");
    let fragment = fragment.map(decode).transpose()?;
    ensure!(
        fragment
            .as_ref()
            .is_none_or(|v| !v.chars().any(char::is_control)),
        "invalid fragment"
    );
    Ok((parts.join("/"), fragment))
}

fn lookup(tx: &Transaction<'_>, edge: &mut KnowledgeRelationship) -> Result<()> {
    let previous_target = edge.target_id.take();
    edge.reason = None;
    edge.target_hash = None;
    edge.target_path = None;
    if edge.kind == RelationshipKind::Contains {
        edge.target_id = Some(edge.target.clone());
        return Ok(());
    }
    let mut candidates: Vec<(String, String, String)> = Vec::new();
    if edge.kind == RelationshipKind::Supersedes
        && !(edge.target.starts_with("ADR-")
            && edge
                .target
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-'))
    {
        edge.status = ResolutionStatus::Invalid;
        edge.reason = Some("supersedes requires an explicit ADR identifier".into());
        return Ok(());
    }
    if (edge.target.starts_with("REQ-") || edge.target.starts_with("ADR-"))
        && edge
            .target
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    {
        let mut q = tx.prepare("SELECT id,path,json_extract(data,'$.content_hash') FROM knowledge_entities WHERE explicit_id=?1 ORDER BY id LIMIT 2")?;
        candidates = q
            .query_map([&edge.target], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        edge.rule = "explicit_repository_identifier".into();
    } else {
        if edge.target.contains("://")
            || ["mailto:", "data:", "javascript:"]
                .iter()
                .any(|prefix| edge.target.starts_with(prefix))
        {
            edge.status = ResolutionStatus::Unsupported;
            edge.reason = Some("remote links and URL schemes are never followed".into());
            return Ok(());
        }
        let Ok((path, fragment)) = local_target(&edge.evidence.path, &edge.target) else {
            edge.status = ResolutionStatus::Invalid;
            edge.reason = Some("invalid encoding, path, or repository boundary".into());
            return Ok(());
        };
        edge.target_path = Some(path.clone());
        let hash: Option<String> = tx.query_row("SELECT content_hash FROM files WHERE path=?1 UNION ALL SELECT content_hash FROM documents WHERE path=?1 LIMIT 1", [&path], |r| r.get(0)).optional()?;
        if let Some(hash) = hash {
            let mut q = tx.prepare(if fragment.is_some() {
                // Exact static identity constrained to an explicit indexed path.
                "SELECT id FROM facts WHERE path=?1 AND id=?2 ORDER BY id LIMIT 2"
            } else {
                "SELECT id FROM facts WHERE path=?1 AND json_extract(data,'$.kind')='file' AND ?2 IS NULL ORDER BY id LIMIT 2"
            })?;
            let ids: Vec<String> = q
                .query_map(params![path, fragment], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in ids {
                candidates.push((id, path.clone(), hash.clone()));
            }
            if candidates.is_empty() {
                let mut q = tx.prepare("SELECT id FROM knowledge_entities WHERE path=?1 AND ((?2 IS NULL AND json_extract(data,'$.kind')='document') OR explicit_id=?2 OR id=?2) ORDER BY id LIMIT 2")?;
                let ids: Vec<String> = q
                    .query_map(params![path, fragment], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for id in ids {
                    candidates.push((id, path.clone(), hash.clone()));
                }
            }
        }
        edge.rule = if fragment.is_some() {
            "exact_identity_in_explicit_path"
        } else {
            "exact_indexed_repository_path"
        }
        .into();
    }
    match candidates.len() {
        1 => {
            let (id, path, hash) = candidates.remove(0);
            edge.target_id = Some(id);
            edge.target_path = Some(path);
            edge.target_hash = Some(hash);
            edge.status = ResolutionStatus::Validated;
        }
        0 => {
            edge.status = if previous_target.is_some() || edge.status == ResolutionStatus::Stale {
                ResolutionStatus::Stale
            } else {
                ResolutionStatus::Unresolved
            };
            edge.reason = Some("target absent from indexed inventory or unsupported fragment; explicit re-index cannot prove implementation".into());
        }
        _ => {
            edge.status = ResolutionStatus::Ambiguous;
            edge.reason = Some("multiple explicit targets; no target selected".into());
        }
    }
    Ok(())
}

pub(super) fn resolve(tx: &Transaction<'_>, changed: bool, revision: u64) -> Result<bool> {
    let initialized: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM metadata WHERE key='relationships_indexed' AND value=?1)",
        [RELATIONSHIP_IR_VERSION.to_string()],
        |r| r.get(0),
    )?;
    if !changed && initialized {
        return Ok(false);
    }
    let mut entities: Vec<KnowledgeEntity> = {
        let mut q = tx.prepare("SELECT data FROM knowledge_entities ORDER BY id")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?
    };
    let mut counts = BTreeMap::<String, usize>::new();
    for entity in &entities {
        if let Some(id) = &entity.explicit_id {
            *counts.entry(id.clone()).or_default() += 1;
        }
    }
    let mut edges: Vec<KnowledgeRelationship> = {
        let mut q = tx.prepare("SELECT data FROM knowledge_relationships ORDER BY id")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?
    };
    for edge in &mut edges {
        lookup(tx, edge)?;
        edge.index_revision = revision;
        save(tx, edge, false)?;
    }
    let mut lineage = BTreeMap::<String, Vec<String>>::new();
    for edge in &edges {
        if edge.kind == RelationshipKind::Supersedes {
            if let Some(target) = &edge.target_id {
                lineage
                    .entry(edge.source_id.clone())
                    .or_default()
                    .push(target.clone());
            }
        }
    }
    let statuses: BTreeMap<_, _> = entities
        .iter()
        .map(|e| (e.id.clone(), e.decision_status.clone()))
        .collect();
    let accepted_superseded: BTreeSet<_> = edges
        .iter()
        .filter(|e| {
            e.kind == RelationshipKind::Supersedes
                && statuses.get(&e.source_id).and_then(|s| s.as_deref()) == Some("accepted")
        })
        .filter_map(|e| e.target_id.clone())
        .collect();
    diagnose(tx, &mut entities, &counts, &lineage, &accepted_superseded)?;
    tx.execute("INSERT INTO metadata(key,value) VALUES('relationships_indexed',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [RELATIONSHIP_IR_VERSION.to_string()])?;
    Ok(true)
}

fn diagnose(
    tx: &Transaction<'_>,
    entities: &mut [KnowledgeEntity],
    counts: &BTreeMap<String, usize>,
    lineage: &BTreeMap<String, Vec<String>>,
    accepted_superseded: &BTreeSet<String>,
) -> Result<()> {
    for entity in entities {
        let previous_diagnostics = entity.diagnostics.clone();
        entity
            .diagnostics
            .retain(|d| !d.starts_with("relationship: "));
        if entity.explicit_id.as_ref().is_some_and(|id| counts[id] > 1) {
            entity
                .diagnostics
                .push("relationship: duplicate explicit identifier".into());
        }
        if lineage.contains_key(&entity.id) && entity.decision_status.as_deref() == Some("rejected")
        {
            entity
                .diagnostics
                .push("relationship: rejected decision declares supersession".into());
        }
        if accepted_superseded.contains(&entity.id)
            && entity.decision_status.as_deref() == Some("accepted")
        {
            entity.diagnostics.push("relationship: accepted status conflicts with explicit supersession by an accepted decision".into());
        }
        let mut pending = lineage.get(&entity.id).cloned().unwrap_or_default();
        let mut visited = BTreeSet::new();
        while let Some(next) = pending.pop() {
            if next == entity.id {
                entity
                    .diagnostics
                    .push("relationship: supersession cycle".into());
                break;
            }
            if visited.len() >= 256 {
                entity
                    .diagnostics
                    .push("relationship: lineage diagnostic work limit reached".into());
                break;
            }
            if visited.insert(next.clone()) {
                pending.extend(lineage.get(&next).into_iter().flatten().cloned());
            }
        }
        if entity.diagnostics != previous_diagnostics {
            tx.execute(
                "UPDATE knowledge_entities SET data=?2 WHERE id=?1",
                params![entity.id, serde_json::to_string(entity)?],
            )?;
        }
    }
    Ok(())
}
