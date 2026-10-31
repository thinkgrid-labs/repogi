//! Reviewed project memory persistence (schema 5). Every write is one
//! `IMMEDIATE` transaction that re-checks the state it depends on, so a
//! concurrent writer either waits or fails without partial effects. Indexing
//! never touches these tables; reads never migrate.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail, ensure};
use code_intel_core::memory::{
    Assertion, EventRecord, EvidenceRole, EvidenceSnapshot, Memory, MemoryEvent, MemoryStatus,
    MemoryType, Revision, RevisionRecord, RevisionState, claim_key,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};

use crate::{DATABASE_VERSION, Store, configure};

/// A typed failure the CLI and MCP adapters report with a stable code.
#[derive(Debug)]
pub struct MemoryError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MemoryError {}

fn fail<T>(code: &'static str, message: impl Into<String>) -> Result<T> {
    Err(MemoryError {
        code,
        message: message.into(),
    }
    .into())
}

/// A new claim revision, with evidence already captured by value.
pub struct NewRevision<'a> {
    pub memory_type: MemoryType,
    pub title: &'a str,
    pub claim: &'a str,
    pub assertion: Option<&'a Assertion>,
    pub evidence: &'a [EvidenceSnapshot],
    pub digest: &'a str,
    pub index_revision: u64,
    pub reason: Option<&'a str>,
}

/// Who did what, when; recorded on every event. Actor names are self-reported.
pub struct Act<'a> {
    pub actor: &'a str,
    pub at: &'a str,
    pub note: Option<&'a str>,
    pub detail: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

/// Filters for bounded, ordered listings. `after` is an exclusive ID cursor.
#[derive(Debug, Default, Clone)]
pub struct MemoryFilter {
    pub statuses: Vec<MemoryStatus>,
    pub memory_type: Option<MemoryType>,
    pub after: Option<String>,
}

/// One FTS hit: which revision matched and how strongly (lower is better).
#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub memory_id: String,
    pub revision: u32,
    pub rank: f64,
}

/// Two approved memories asserting different statuses for one subject.
#[derive(Debug, Clone)]
pub struct AssertionClash {
    pub subject: String,
    /// (memory ID, approved revision, asserted status), ordered by memory ID.
    pub claims: Vec<(String, u32, String)>,
}

impl Store {
    /// Open an existing schema-5 index for an explicit memory write. Never
    /// migrates: an older index must be upgraded with `repogi index` first.
    pub fn open_memory_writer(database: &Path, root: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| format!("open index {}", database.display()))?;
        configure(&connection)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < DATABASE_VERSION {
            return fail(
                "memory_unavailable",
                format!(
                    "index schema {version} has no project memory; run `repogi index <path>` to upgrade it to schema {DATABASE_VERSION} (memory commands never migrate)"
                ),
            );
        }
        ensure!(
            version == DATABASE_VERSION,
            "index schema {version} is newer than supported schema {DATABASE_VERSION}; use a compatible repogi binary"
        );
        let stored: Option<String> = connection
            .query_row("SELECT value FROM metadata WHERE key='root'", [], |row| {
                row.get(0)
            })
            .optional()?;
        ensure!(
            stored.as_deref() == root.to_str(),
            "index belongs to a different repository root; memory is scoped to the repository that owns the index"
        );
        Ok(Self {
            connection,
            cancelled: std::sync::Arc::default(),
        })
    }

    /// True when this snapshot has the schema-5 memory tables.
    pub fn memory_available(&self) -> Result<bool> {
        self.check_cancelled()?;
        let version: u32 = self
            .connection
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 5 {
            return Ok(false);
        }
        Ok(self.connection.query_row(
            "SELECT count(*)=5 FROM sqlite_master WHERE name IN ('memories','memory_revisions','memory_evidence','memory_events','memory_search')",
            [],
            |r| r.get(0),
        )?)
    }

    fn require_memory(&self) -> Result<()> {
        if !self.memory_available()? {
            return fail(
                "memory_unavailable",
                "this index has no project memory (schema < 5); run `repogi index <path>` with this binary (reads never migrate)",
            );
        }
        Ok(())
    }

    pub fn memory(&self, id: &str) -> Result<Option<Memory>> {
        self.require_memory()?;
        self.connection
            .query_row(
                "SELECT id,type,status,latest_revision,approved_revision,pending_revision,superseded_by,created_at,updated_at,version FROM memories WHERE id=?1",
                [id],
                memory_row,
            )
            .optional()?
            .transpose()
    }

    /// Bounded listing in ID order (IDs are allocated in creation order).
    pub fn memories(&self, filter: &MemoryFilter, limit: u32) -> Result<Vec<Memory>> {
        self.require_memory()?;
        ensure!(limit <= 1001, "memory listing limit exceeds 1001");
        let statuses: Vec<&str> = filter.statuses.iter().map(|s| s.label()).collect();
        let mut query = self.connection.prepare(
            "SELECT id,type,status,latest_revision,approved_revision,pending_revision,superseded_by,created_at,updated_at,version FROM memories
             WHERE (?1='[]' OR status IN (SELECT value FROM json_each(?1)))
               AND (?2 IS NULL OR type=?2)
               AND (?3 IS NULL OR id>?3)
             ORDER BY id LIMIT ?4",
        )?;
        let rows = query.query_map(
            params![
                serde_json::to_string(&statuses)?,
                filter.memory_type.map(MemoryType::label),
                filter.after,
                limit
            ],
            memory_row,
        )?;
        rows.map(|row| row?).collect()
    }

    pub fn memory_revision(&self, id: &str, revision: u32) -> Result<Option<Revision>> {
        self.require_memory()?;
        let row: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT state,digest,data FROM memory_revisions WHERE memory_id=?1 AND revision=?2",
                params![id, revision],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((state, digest, data)) = row else {
            return Ok(None);
        };
        let mut stored: RevisionRecord = serde_json::from_str(&data)?;
        let mut query = self.connection.prepare(
            "SELECT data FROM memory_evidence WHERE memory_id=?1 AND revision=?2 ORDER BY ordinal",
        )?;
        let evidence = query
            .query_map(params![id, revision], |r| r.get::<_, String>(0))?
            .map(|data| Ok(serde_json::from_str(&data?)?))
            .collect::<Result<Vec<EvidenceSnapshot>>>()?;
        Ok(Some(Revision {
            revision,
            memory_type: stored.memory_type,
            title: std::mem::take(&mut stored.title),
            claim: std::mem::take(&mut stored.claim),
            assertion: stored.assertion.take(),
            state: RevisionState::parse(&state).context("invalid memory revision state")?,
            digest,
            index_revision: stored.index_revision,
            created_at: std::mem::take(&mut stored.created_at),
            proposed_by: std::mem::take(&mut stored.proposed_by),
            reason: stored.reason.take(),
            evidence,
        }))
    }

    /// Newest first, bounded.
    pub fn memory_revisions(&self, id: &str, limit: u32) -> Result<Vec<Revision>> {
        self.require_memory()?;
        let revisions: Vec<u32> = {
            let mut query = self.connection.prepare(
                "SELECT revision FROM memory_revisions WHERE memory_id=?1 ORDER BY revision DESC LIMIT ?2",
            )?;
            query
                .query_map(params![id, limit], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        revisions
            .into_iter()
            .map(|revision| {
                self.memory_revision(id, revision)?
                    .context("memory revision vanished inside a read snapshot")
            })
            .collect()
    }

    /// Oldest first, bounded; `after` is an exclusive sequence cursor.
    pub fn memory_events(&self, id: &str, after: u64, limit: u32) -> Result<Vec<MemoryEvent>> {
        self.require_memory()?;
        let mut query = self.connection.prepare(
            "SELECT sequence,revision,action,data FROM memory_events WHERE memory_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        let rows = query.query_map(params![id, i64::try_from(after)?, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<u32>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (sequence, revision, action, data) = row?;
            let stored: EventRecord = serde_json::from_str(&data)?;
            Ok(MemoryEvent {
                sequence: u64::try_from(sequence)?,
                memory_id: id.to_owned(),
                revision,
                action,
                actor: stored.actor,
                at: stored.at,
                digest: stored.digest,
                note: stored.note,
                detail: stored.detail,
            })
        })
        .collect()
    }

    pub fn memory_event_count(&self, id: &str) -> Result<u64> {
        self.require_memory()?;
        let count: i64 = self.connection.query_row(
            "SELECT count(*) FROM memory_events WHERE memory_id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(count)?)
    }

    /// FTS over each memory's authoritative revision (its approved revision,
    /// else its latest: a rejected proposal) and, when asked, pending revisions. Bounded and
    /// ordered by rank then ID.
    pub fn memory_search(
        &self,
        expression: &str,
        statuses: &[MemoryStatus],
        include_pending: bool,
        limit: u32,
    ) -> Result<Vec<MemoryHit>> {
        self.require_memory()?;
        self.check_cancelled()?;
        let statuses: Vec<&str> = statuses.iter().map(|s| s.label()).collect();
        let mut query = self.connection.prepare(
            "SELECT s.memory_id, s.revision, bm25(memory_search, 0, 0, 4, 2, 3) AS rank
             FROM memory_search s JOIN memories m ON m.id = s.memory_id
             WHERE memory_search MATCH ?1
               AND ((m.status IN (SELECT value FROM json_each(?2)) AND s.revision = coalesce(m.approved_revision, m.latest_revision))
                 OR (?3 AND s.revision = m.pending_revision))
             ORDER BY rank, s.memory_id, s.revision LIMIT ?4",
        )?;
        let rows = query.query_map(
            params![
                expression,
                serde_json::to_string(&statuses)?,
                include_pending,
                limit
            ],
            |r| {
                Ok(MemoryHit {
                    memory_id: r.get(0)?,
                    revision: r.get(1)?,
                    rank: r.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Approved memories whose authoritative evidence cites one of `subjects`
    /// (exact entity/fact/relationship IDs, explicit REQ-/ADR- IDs, or the
    /// target of a cited relationship or static edge).
    pub fn memories_citing(&self, subjects: &[String], limit: u32) -> Result<Vec<String>> {
        self.require_memory()?;
        self.check_cancelled()?;
        let mut query = self.connection.prepare(
            "SELECT DISTINCT m.id FROM memory_evidence e JOIN memories m
               ON m.id = e.memory_id AND e.revision = m.approved_revision
             WHERE m.status = 'approved'
               AND (e.subject_id IN (SELECT value FROM json_each(?1)) OR e.explicit_id IN (SELECT value FROM json_each(?1)) OR e.target_id IN (SELECT value FROM json_each(?1)))
             ORDER BY m.id LIMIT ?2",
        )?;
        let rows = query.query_map(params![serde_json::to_string(subjects)?, limit], |r| {
            r.get(0)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Subjects with more than one asserted status among approved memories.
    pub fn memory_assertion_clashes(&self, limit: u32) -> Result<Vec<AssertionClash>> {
        self.require_memory()?;
        self.check_cancelled()?;
        let mut query = self.connection.prepare(
            "SELECT r.assertion_subject, m.id, r.revision, r.assertion_status
             FROM memory_revisions r JOIN memories m ON m.id = r.memory_id AND r.revision = m.approved_revision
             WHERE m.status = 'approved' AND r.assertion_subject IN (
                 SELECT r2.assertion_subject FROM memory_revisions r2
                 JOIN memories m2 ON m2.id = r2.memory_id AND r2.revision = m2.approved_revision
                 WHERE m2.status = 'approved' AND r2.assertion_subject IS NOT NULL
                 GROUP BY r2.assertion_subject HAVING count(DISTINCT r2.assertion_status) > 1
                 ORDER BY r2.assertion_subject LIMIT ?1)
             ORDER BY r.assertion_subject, m.id",
        )?;
        let rows = query.query_map([limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut clashes: Vec<AssertionClash> = Vec::new();
        for row in rows {
            let (subject, id, revision, status) = row?;
            match clashes.last_mut() {
                Some(last) if last.subject == subject => last.claims.push((id, revision, status)),
                _ => clashes.push(AssertionClash {
                    subject,
                    claims: vec![(id, revision, status)],
                }),
            }
        }
        Ok(clashes)
    }

    /// Existing pending or approved memories with the same normalized claim.
    pub fn memory_duplicates(
        &self,
        memory_type: MemoryType,
        claim: &str,
    ) -> Result<Vec<(String, MemoryStatus, bool)>> {
        self.require_memory()?;
        duplicates(&self.connection, &claim_key(memory_type, claim))
    }

    /// Create a memory with its first revision pending review.
    pub fn memory_propose(&mut self, new: &NewRevision<'_>, act: &Act<'_>) -> Result<String> {
        self.write(|tx| {
            let existing = duplicates(tx, &claim_key(new.memory_type, new.claim))?;
            if let Some((id, status, _)) = existing
                .iter()
                .find(|(_, s, live)| *live && matches!(s, MemoryStatus::PendingReview | MemoryStatus::Approved))
            {
                return fail(
                    "duplicate_memory",
                    format!(
                        "{id} already records this claim ({}); correct or review it instead of proposing a duplicate",
                        status.label()
                    ),
                );
            }
            let next: i64 = tx.query_row(
                "SELECT coalesce(max(CAST(substr(id,5) AS INTEGER)),0)+1 FROM memories",
                [],
                |r| r.get(0),
            )?;
            let id = format!("mem-{next:04}");
            tx.execute(
                "INSERT INTO memories(id,type,status,latest_revision,approved_revision,pending_revision,superseded_by,created_at,updated_at,version) VALUES(?1,?2,'pending_review',1,NULL,1,NULL,?3,?3,1)",
                params![id, new.memory_type.label(), act.at],
            )?;
            insert_revision(tx, &id, 1, new, act)?;
            event(tx, &id, Some(1), "proposed", Some(new.digest), act)?;
            Ok(id)
        })
    }

    /// Propose a corrected claim as a new pending revision of an approved
    /// memory. The approved revision stays authoritative until review.
    pub fn memory_correct(
        &mut self,
        id: &str,
        expected_version: Option<u64>,
        new: &NewRevision<'_>,
        act: &Act<'_>,
    ) -> Result<u32> {
        self.write(|tx| {
            let memory = locked(tx, id, expected_version)?;
            if memory.status != MemoryStatus::Approved {
                return fail(
                    "invalid_transition",
                    format!(
                        "{id} is {}; only approved memories take corrections",
                        memory.status.label()
                    ),
                );
            }
            if let Some(pending) = memory.pending_revision {
                return fail(
                    "review_pending",
                    format!("{id} revision {pending} is awaiting review; approve or reject it first"),
                );
            }
            let revision = memory.latest_revision + 1;
            insert_revision(tx, id, revision, new, act)?;
            tx.execute(
                "UPDATE memories SET latest_revision=?2, pending_revision=?2, updated_at=?3, version=version+1 WHERE id=?1",
                params![id, revision, act.at],
            )?;
            event(tx, id, Some(revision), "correction_proposed", Some(new.digest), act)?;
            Ok(revision)
        })
    }

    /// Approve or reject the pending revision whose digest starts with
    /// `digest`. The digest binds the decision to the exact claim and evidence.
    pub fn memory_review(
        &mut self,
        id: &str,
        digest: &str,
        decision: Decision,
        act: &Act<'_>,
    ) -> Result<u32> {
        self.write(|tx| {
            let memory = locked(tx, id, None)?;
            let Some(revision) = memory.pending_revision else {
                return fail(
                    "nothing_to_review",
                    format!("{id} has no revision awaiting review (status {})", memory.status.label()),
                );
            };
            let (state, full): (String, String) = tx.query_row(
                "SELECT state,digest FROM memory_revisions WHERE memory_id=?1 AND revision=?2",
                params![id, revision],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if state != "proposed" {
                return fail("nothing_to_review", format!("{id} revision {revision} was already reviewed"));
            }
            if digest.len() < 12 || !full.starts_with(digest) {
                return fail(
                    "digest_mismatch",
                    format!(
                        "the digest does not match {id} revision {revision}; run `repogi memory show {id}` and pass at least 12 characters of the pending digest"
                    ),
                );
            }
            match decision {
                Decision::Approve => {
                    tx.execute(
                        "UPDATE memory_revisions SET state='approved' WHERE memory_id=?1 AND revision=?2",
                        params![id, revision],
                    )?;
                    tx.execute(
                        "UPDATE memories SET status='approved', approved_revision=?2, pending_revision=NULL, updated_at=?3, version=version+1 WHERE id=?1",
                        params![id, revision, act.at],
                    )?;
                    event(tx, id, Some(revision), "approved", Some(&full), act)?;
                }
                Decision::Reject => {
                    tx.execute(
                        "UPDATE memory_revisions SET state='rejected' WHERE memory_id=?1 AND revision=?2",
                        params![id, revision],
                    )?;
                    // A rejected correction leaves the approved claim in force.
                    let status = if memory.approved_revision.is_some() {
                        memory.status.label()
                    } else {
                        MemoryStatus::Rejected.label()
                    };
                    tx.execute(
                        "UPDATE memories SET status=?2, pending_revision=NULL, updated_at=?3, version=version+1 WHERE id=?1",
                        params![id, status, act.at],
                    )?;
                    event(tx, id, Some(revision), "rejected", Some(&full), act)?;
                }
            }
            Ok(revision)
        })
    }

    /// Mark `old` superseded by `new`. Both must be approved; history is kept.
    pub fn memory_supersede(&mut self, old: &str, new: &str, act: &Act<'_>) -> Result<()> {
        self.write(|tx| {
            if old == new {
                return fail("invalid_transition", "a memory cannot supersede itself");
            }
            let replaced = locked(tx, old, None)?;
            let successor = locked(tx, new, None)?;
            for memory in [&replaced, &successor] {
                if memory.status != MemoryStatus::Approved {
                    return fail(
                        "invalid_transition",
                        format!("{} is {}; supersession links two approved memories", memory.id, memory.status.label()),
                    );
                }
            }
            if replaced.pending_revision.is_some() {
                return fail("review_pending", format!("{old} has a revision awaiting review; review it first"));
            }
            if successor.superseded_by.is_some() {
                return fail("invalid_transition", format!("{new} is itself superseded"));
            }
            tx.execute(
                "UPDATE memories SET status='superseded', superseded_by=?2, updated_at=?3, version=version+1 WHERE id=?1",
                params![old, new, act.at],
            )?;
            tx.execute(
                "UPDATE memories SET updated_at=?2, version=version+1 WHERE id=?1",
                params![new, act.at],
            )?;
            event(tx, old, replaced.approved_revision, "superseded", None, &Act { actor: act.actor, at: act.at, note: act.note, detail: Some(BTreeMap::from([("superseded_by".to_owned(), new.to_owned())])) })?;
            event(tx, new, successor.approved_revision, "supersedes", None, &Act { actor: act.actor, at: act.at, note: act.note, detail: Some(BTreeMap::from([("supersedes".to_owned(), old.to_owned())])) })?;
            Ok(())
        })
    }

    /// Retire an approved memory: kept with its history, no longer authoritative.
    pub fn memory_retire(&mut self, id: &str, act: &Act<'_>) -> Result<()> {
        self.write(|tx| {
            let memory = locked(tx, id, None)?;
            if memory.status != MemoryStatus::Approved {
                return fail(
                    "invalid_transition",
                    format!("{id} is {}; only approved memories can be retired", memory.status.label()),
                );
            }
            if memory.pending_revision.is_some() {
                return fail("review_pending", format!("{id} has a revision awaiting review; review it first"));
            }
            tx.execute(
                "UPDATE memories SET status='retired', updated_at=?2, version=version+1 WHERE id=?1",
                params![id, act.at],
            )?;
            event(tx, id, memory.approved_revision, "retired", None, act)
        })
    }

    /// Append a verification result to the history. Changes no claim or status.
    pub fn memory_record_verification(&mut self, id: &str, act: &Act<'_>) -> Result<()> {
        self.write(|tx| {
            let memory = locked(tx, id, None)?;
            event(
                tx,
                id,
                memory.approved_revision.or(memory.pending_revision),
                "verified",
                None,
                act,
            )
        })
    }

    fn write<T>(&mut self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.check_cancelled()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = body(&tx)?;
        tx.commit()
            .context("commit memory update; nothing was changed on failure")?;
        Ok(value)
    }
}

fn memory_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Memory>> {
    let memory_type: String = r.get(1)?;
    let status: String = r.get(2)?;
    let version: i64 = r.get(9)?;
    let memory = (|| {
        Ok(Memory {
            id: r.get(0)?,
            memory_type: MemoryType::parse(&memory_type).context("invalid stored memory type")?,
            status: MemoryStatus::parse(&status).context("invalid stored memory status")?,
            latest_revision: r.get(3)?,
            approved_revision: r.get(4)?,
            pending_revision: r.get(5)?,
            superseded_by: r.get(6)?,
            created_at: r.get(7)?,
            updated_at: r.get(8)?,
            version: u64::try_from(version)?,
        })
    })();
    Ok(memory)
}

fn locked(tx: &Transaction<'_>, id: &str, expected_version: Option<u64>) -> Result<Memory> {
    let Some(memory) = tx
        .query_row(
            "SELECT id,type,status,latest_revision,approved_revision,pending_revision,superseded_by,created_at,updated_at,version FROM memories WHERE id=?1",
            [id],
            memory_row,
        )
        .optional()?
        .transpose()?
    else {
        return fail("memory_not_found", format!("no memory {id:?} in this repository"));
    };
    if let Some(expected) = expected_version
        && expected != memory.version
    {
        return fail(
            "concurrent_update",
            format!(
                "{id} changed (version {} is now {}); re-read it and retry",
                expected, memory.version
            ),
        );
    }
    Ok(memory)
}

/// Memories with this normalized claim: (ID, status, whether it is the
/// memory's live claim, i.e. its approved or pending revision).
fn duplicates(connection: &Connection, key: &str) -> Result<Vec<(String, MemoryStatus, bool)>> {
    let mut query = connection.prepare(
        "SELECT m.id, m.status, max(r.revision IS m.approved_revision OR r.revision IS m.pending_revision)
         FROM memory_revisions r JOIN memories m ON m.id = r.memory_id
         WHERE r.claim_key = ?1 GROUP BY m.id ORDER BY m.id LIMIT 16",
    )?;
    let rows = query.query_map([key], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, bool>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (id, status, live) = row?;
        Ok((
            id,
            MemoryStatus::parse(&status).context("invalid stored memory status")?,
            live,
        ))
    })
    .collect()
}

fn insert_revision(
    tx: &Transaction<'_>,
    id: &str,
    revision: u32,
    new: &NewRevision<'_>,
    act: &Act<'_>,
) -> Result<()> {
    let stored = RevisionRecord {
        memory_type: new.memory_type,
        title: new.title.to_owned(),
        claim: new.claim.to_owned(),
        assertion: new.assertion.cloned(),
        index_revision: new.index_revision,
        created_at: act.at.to_owned(),
        proposed_by: act.actor.to_owned(),
        reason: new.reason.map(str::to_owned),
    };
    tx.execute(
        "INSERT INTO memory_revisions(memory_id,revision,state,digest,claim_key,assertion_subject,assertion_status,data) VALUES(?1,?2,'proposed',?3,?4,?5,?6,?7)",
        params![
            id,
            revision,
            new.digest,
            claim_key(new.memory_type, new.claim),
            new.assertion.map(|a| a.subject.as_str()),
            new.assertion.map(|a| a.status.as_str()),
            serde_json::to_string(&stored)?
        ],
    )?;
    let mut subjects: Vec<&str> = Vec::new();
    for (ordinal, evidence) in new.evidence.iter().enumerate() {
        tx.execute(
            "INSERT INTO memory_evidence(memory_id,revision,ordinal,role,class,subject_id,explicit_id,target_id,path,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                id,
                revision,
                i64::try_from(ordinal)?,
                match evidence.role {
                    EvidenceRole::Backing => "backing",
                    EvidenceRole::Context => "context",
                },
                serde_json::to_value(evidence.class)?.as_str().unwrap_or_default(),
                evidence.subject_id,
                evidence.explicit_id,
                evidence
                    .relationship
                    .as_ref()
                    .and_then(|r| r.target_id.as_deref()),
                evidence.citation.path,
                serde_json::to_string(evidence)?
            ],
        )?;
        subjects.extend(evidence.explicit_id.as_deref());
        subjects.push(&evidence.citation.path);
    }
    if let Some(assertion) = new.assertion {
        subjects.push(&assertion.subject);
    }
    tx.execute(
        "INSERT INTO memory_search(memory_id,revision,title,claim,subjects) VALUES(?1,?2,?3,?4,?5)",
        params![id, revision, new.title, new.claim, subjects.join(" ")],
    )?;
    Ok(())
}

fn event(
    tx: &Transaction<'_>,
    id: &str,
    revision: Option<u32>,
    action: &str,
    digest: Option<&str>,
    act: &Act<'_>,
) -> Result<()> {
    let stored = EventRecord {
        actor: act.actor.to_owned(),
        at: act.at.to_owned(),
        digest: digest.map(str::to_owned),
        note: act.note.map(str::to_owned),
        detail: act.detail.clone(),
    };
    tx.execute(
        "INSERT INTO memory_events(memory_id,revision,action,data) VALUES(?1,?2,?3,?4)",
        params![id, revision, action, serde_json::to_string(&stored)?],
    )?;
    if tx.changes() != 1 {
        bail!("memory history insert failed");
    }
    Ok(())
}
