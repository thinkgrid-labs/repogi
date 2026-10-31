//! `repogi memory`: reviewed project memory for the nearest repository that
//! has a code index (`.code-intel/index.db`) or a memory store
//! (`.repogi/memory.db`). Memories live in the durable memory store; the code
//! index is only read, to capture and verify evidence, and is optional for
//! reads. Reads never create, migrate or write anything. Writes open the
//! memory store for that command only (creating it, or migrating memories out
//! of a schema-5 index first) and never write the code index's own data.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use code_intel_core::memory::{MemoryStatus, MemoryType, utc_now};
use code_intel_retrieval::{
    Format,
    memory::{self, Draft, ImportMode, Selector, Stores},
};
use code_intel_store::{
    Decision, MEMORY_FILE, MemoryStore, MigrationReport, Store, validate_state_path,
};

use crate::OutputFormat;

/// Who reviews: `--reviewer`, else `REPOGI_MEMORY_REVIEWER`. Required for every
/// review write; recorded in the history. Self-reported, not authenticated.
const REVIEWER_ENV: &str = "REPOGI_MEMORY_REVIEWER";
const AUTHOR_ENV: &str = "REPOGI_MEMORY_AUTHOR";

#[derive(Subcommand)]
pub enum MemoryCommand {
    /// List memories in ID order (read-only; no verification).
    List {
        /// `pending_review`, `approved`, `rejected`, `superseded` or `retired` (repeatable; default all).
        #[arg(long = "status", value_parser = parse_status)]
        statuses: Vec<MemoryStatus>,
        #[arg(long = "type", value_parser = parse_type)]
        memory_type: Option<MemoryType>,
        /// Continue after this memory ID.
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[command(flatten)]
        output: Output,
    },
    /// Show a memory with its approved and pending revisions, verified live (read-only).
    Show {
        id: String,
        #[command(flatten)]
        output: Output,
    },
    /// Revisions and the append-only review log (read-only).
    History {
        id: String,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[command(flatten)]
        output: Output,
    },
    /// Search approved memories (read-only). Others only when asked, never authoritative.
    Search {
        query: String,
        /// Statuses to search (repeatable; default approved).
        #[arg(long = "status", value_parser = parse_status)]
        statuses: Vec<MemoryStatus>,
        /// Also search revisions awaiting review, labelled unreviewed.
        #[arg(long)]
        include_unreviewed: bool,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[command(flatten)]
        output: Output,
    },
    /// Check recorded evidence against the current index and working tree.
    /// Read-only unless --record; never edits, approves or supersedes.
    Verify {
        ids: Vec<String>,
        /// Verify approved memories (bounded by --limit).
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        /// Append a `verified` event to each memory's history (an explicit write).
        #[arg(long)]
        record: bool,
        #[arg(long)]
        reviewer: Option<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Structured conflicts among approved memories (read-only).
    Conflicts {
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[command(flatten)]
        output: Output,
    },
    /// Propose a claim with evidence. It stays pending until explicitly approved.
    Propose {
        #[arg(long = "type", value_parser = parse_type)]
        memory_type: MemoryType,
        #[arg(long)]
        title: String,
        #[arg(long)]
        claim: String,
        #[command(flatten)]
        evidence: EvidenceArgs,
        /// Proposer label for the history (default: `REPOGI_MEMORY_AUTHOR` or "unspecified").
        #[arg(long)]
        author: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Propose a corrected revision of an approved memory; the approved
    /// revision stays authoritative until the correction is reviewed.
    Correct {
        id: String,
        #[arg(long)]
        claim: String,
        /// Defaults to the approved title.
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        evidence: EvidenceArgs,
        /// Refuse if the memory changed since this version (from `show --format json`).
        #[arg(long)]
        expect_version: Option<u64>,
        #[arg(long)]
        author: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Approve the pending revision named by its digest (from `memory show`).
    Approve {
        id: String,
        #[command(flatten)]
        review: ReviewArgs,
        #[arg(long)]
        note: Option<String>,
    },
    /// Reject the pending revision named by its digest; history is kept.
    Reject {
        id: String,
        #[command(flatten)]
        review: ReviewArgs,
        #[arg(long)]
        reason: String,
    },
    /// Mark an approved memory superseded by another approved memory.
    Supersede {
        id: String,
        #[arg(long)]
        by: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        reviewer: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Retire an approved memory; it is kept with its history.
    Retire {
        id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        reviewer: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Where memories are stored, store identity and history (read-only).
    Status {
        /// Also run SQLite integrity and foreign-key checks.
        #[arg(long)]
        check: bool,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Copy memories out of a schema-5 `.code-intel/index.db` into
    /// `.repogi/memory.db` (idempotent; the index rows are kept and frozen).
    Migrate {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Write every memory, revision, evidence snapshot and review event to a
    /// portable JSON backup (read-only for the store). May contain sensitive
    /// project knowledge.
    Export {
        /// The file to create; refused if it exists unless --force.
        #[arg(long)]
        output: PathBuf,
        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
        /// Export format (only `json`).
        #[arg(long = "export-format", default_value = "json", value_parser = ["json"])]
        export_format: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Validate a backup and import it in one transaction. A backup of this
    /// store's lineage is restored; any other is imported for local review.
    Import {
        file: PathBuf,
        /// Assert the backup is this repository's own memory: keep its review
        /// states (only into an empty or same-lineage store).
        #[arg(long)]
        restore: bool,
        /// Validate and report without writing.
        #[arg(long)]
        dry_run: bool,
        /// Label for the import event (default: `REPOGI_MEMORY_AUTHOR` or "unspecified").
        #[arg(long)]
        actor: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Rebind a moved repository's memory store to its new location.
    Rebind {
        /// The root the store records (exactly as printed by the refusal).
        #[arg(long)]
        from: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

#[derive(Args)]
pub struct Output {
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
    /// Cap on the whole output in UTF-8 bytes (default 12000 text, 24000 JSON).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=8_000_000))]
    max_bytes: Option<u32>,
}

#[derive(Args)]
pub struct EvidenceArgs {
    /// Backing evidence: entity ID, REQ-/ADR- ID, knowledge-edge: ID or fact ID (repeatable).
    #[arg(long = "evidence")]
    evidence: Vec<String>,
    /// Backing evidence: an indexed Markdown document path (repeatable).
    #[arg(long = "source")]
    sources: Vec<String>,
    /// Backing evidence: a resolved static edge between two exact fact IDs.
    #[arg(long = "edge", num_args = 2, value_names = ["SOURCE_ID", "TARGET_ID"])]
    edges: Vec<String>,
    /// Context only (never backs the claim): any evidence ID (repeatable).
    #[arg(long = "context")]
    context: Vec<String>,
    /// Context only: an indexed document or source path (repeatable).
    #[arg(long = "context-source")]
    context_sources: Vec<String>,
    /// Structured assertion for conflict detection: SUBJECT=status.
    #[arg(long)]
    asserts: Option<String>,
}

impl EvidenceArgs {
    fn draft(self, memory_type: MemoryType, title: String, claim: String) -> Draft {
        let mut evidence: Vec<Selector> = self.evidence.into_iter().map(Selector::Id).collect();
        evidence.extend(self.sources.into_iter().map(Selector::Source));
        evidence.extend(self.edges.chunks(2).map(|pair| Selector::Edge {
            source: pair[0].clone(),
            target: pair[1].clone(),
        }));
        let mut context: Vec<Selector> = self.context.into_iter().map(Selector::Id).collect();
        context.extend(self.context_sources.into_iter().map(Selector::Source));
        Draft {
            memory_type,
            title,
            claim,
            assertion: self.asserts,
            evidence,
            context,
        }
    }
}

#[derive(Args)]
pub struct ReviewArgs {
    /// At least 12 hex characters of the pending revision digest.
    #[arg(long)]
    digest: String,
    /// Reviewer label (default: `REPOGI_MEMORY_REVIEWER`); required.
    #[arg(long)]
    reviewer: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

fn parse_status(value: &str) -> std::result::Result<MemoryStatus, String> {
    MemoryStatus::parse(value).ok_or_else(|| {
        "status must be pending_review, approved, rejected, superseded or retired".into()
    })
}

fn parse_type(value: &str) -> std::result::Result<MemoryType, String> {
    MemoryType::parse(value).ok_or_else(|| {
        "type must be architecture_decision, project_constraint, requirement, project_convention or implementation_note".into()
    })
}

fn format_of(format: OutputFormat) -> Format {
    match format {
        OutputFormat::Json => Format::Json,
        OutputFormat::Text => Format::Text,
    }
}

impl Output {
    fn resolve(&self) -> (Format, usize) {
        let format = format_of(self.format);
        let default = if format == Format::Json { 2 } else { 1 } * memory::DEFAULT_BYTES;
        (format, self.max_bytes.map_or(default, |b| b as usize))
    }
}

fn reviewer(flag: Option<String>) -> Result<String> {
    match flag.or_else(|| std::env::var(REVIEWER_ENV).ok()) {
        Some(name) if !name.trim().is_empty() => Ok(name),
        _ => Err(code_intel_store::MemoryError {
            code: "reviewer_required",
            message: format!(
                "review writes need an explicit reviewer: pass --reviewer <name> or set {REVIEWER_ENV}; the name is recorded in the memory history"
            ),
        }
        .into()),
    }
}

fn author(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var(AUTHOR_ENV).ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "unspecified".into())
}

/// The nearest ancestor with a code index or a memory store.
fn nearest() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("read current directory")?;
    for root in Path::new(&cwd).ancestors() {
        validate_state_path(root)?;
        if root.join(".code-intel/index.db").exists() || root.join(MEMORY_FILE).exists() {
            return Ok(root.to_path_buf());
        }
    }
    bail!(
        "no .code-intel/index.db or .repogi/memory.db found in current directory or ancestors; run `repogi index <path>` first"
    )
}

/// The code index of `root`, read-only, when it exists; refused when it was
/// built for another root (a copied index).
fn index_of(root: &Path) -> Result<Option<Store>> {
    let database = root.join(".code-intel/index.db");
    if !database.exists() {
        return Ok(None);
    }
    let store = Store::open_read(&database)?;
    anyhow::ensure!(
        store.root()?.as_deref() == root.to_str(),
        "index belongs to a different repository root; run `repogi index <path>` for this repository"
    );
    Ok(Some(store))
}

/// Read-only stores: nothing is created, migrated or written.
struct Reads {
    memory: MemoryStore,
    index: Option<Store>,
}

impl Reads {
    fn open() -> Result<Self> {
        let root = nearest()?;
        // Memory identity errors come first: they name the irreplaceable state.
        let memory = MemoryStore::open_read(&root)?;
        Ok(Self {
            memory,
            index: index_of(&root)?,
        })
    }

    fn stores(&self) -> Stores<'_> {
        Stores {
            memory: &self.memory,
            index: self.index.as_ref(),
        }
    }
}

/// The durable store for one explicit write (created or migrated first when
/// needed) and the index for evidence, when present.
fn writer() -> Result<(MemoryStore, Option<Store>)> {
    let root = nearest()?;
    let (memory, migration) = MemoryStore::open_write(&root, &utc_now())?;
    announce(migration.as_ref());
    Ok((memory, index_of(&root)?))
}

fn announce(migration: Option<&MigrationReport>) {
    if let Some(migration) = migration
        && migration.outcome != "already_migrated"
    {
        eprintln!(
            "repogi: {} {} memories from .code-intel/index.db into .repogi/memory.db (the index rows are kept, read-only)",
            migration.outcome, migration.counts.memories
        );
    }
}

fn required(index: Option<&Store>) -> Result<&Store> {
    index.ok_or_else(|| {
        code_intel_store::MemoryError {
            code: "index_required",
            message: "capturing evidence needs the code index; run `repogi index <path>` first"
                .into(),
        }
        .into()
    })
}

fn json_or<T: serde::Serialize>(
    value: &T,
    format: OutputFormat,
    text: impl FnOnce(&T) -> Result<String>,
) -> Result<String> {
    match format {
        OutputFormat::Json => Ok(format!("{}\n", serde_json::to_string(value)?)),
        OutputFormat::Text => text(value),
    }
}

#[allow(clippy::too_many_lines)] // One arm per subcommand, as in `main::run`.
pub fn run(command: MemoryCommand) -> Result<String> {
    use memory::render;
    match command {
        MemoryCommand::List {
            statuses,
            memory_type,
            after,
            limit,
            output,
        } => {
            let reads = Reads::open()?;
            let (format, max) = output.resolve();
            render(
                &mut memory::list(reads.stores(), statuses, memory_type, after, limit as usize)?,
                format,
                max,
            )
        }
        MemoryCommand::Show { id, output } => {
            let reads = Reads::open()?;
            let (format, max) = output.resolve();
            render(&mut memory::show(reads.stores(), &id)?, format, max)
        }
        MemoryCommand::History { id, limit, output } => {
            let reads = Reads::open()?;
            let (format, max) = output.resolve();
            render(
                &mut memory::history(reads.stores(), &id, limit as usize)?,
                format,
                max,
            )
        }
        MemoryCommand::Search {
            query,
            statuses,
            include_unreviewed,
            limit,
            output,
        } => {
            let reads = Reads::open()?;
            let (format, max) = output.resolve();
            render(
                &mut memory::search(
                    reads.stores(),
                    &query,
                    statuses,
                    include_unreviewed,
                    limit as usize,
                )?,
                format,
                max,
            )
        }
        MemoryCommand::Verify {
            ids,
            all,
            limit,
            record,
            reviewer: name,
            output,
        } => {
            if ids.is_empty() && !all {
                bail!("name memory IDs or pass --all");
            }
            for id in &ids {
                memory::validate_id(id)?;
            }
            let (format, max) = output.resolve();
            if record {
                let actor = reviewer(name)?;
                let (mut store, index) = writer()?;
                let stores = Stores {
                    memory: &store,
                    index: index.as_ref(),
                };
                let mut verification = memory::verify(stores, &ids, all, limit as usize)?;
                memory::record_verification(&mut store, &mut verification, &actor)?;
                return render(&mut verification, format, max);
            }
            let reads = Reads::open()?;
            render(
                &mut memory::verify(reads.stores(), &ids, all, limit as usize)?,
                format,
                max,
            )
        }
        MemoryCommand::Conflicts {
            after,
            limit,
            output,
        } => {
            let reads = Reads::open()?;
            let (format, max) = output.resolve();
            render(
                &mut memory::conflicts(reads.stores(), after, limit as usize)?,
                format,
                max,
            )
        }
        MemoryCommand::Propose {
            memory_type,
            title,
            claim,
            evidence,
            author: name,
            format,
        } => {
            let (mut store, index) = writer()?;
            let receipt = memory::propose(
                &mut store,
                required(index.as_ref())?,
                &evidence.draft(memory_type, title, claim),
                &author(name),
            )?;
            memory::render_receipt(&receipt, format_of(format))
        }
        MemoryCommand::Correct {
            id,
            claim,
            title,
            reason,
            evidence,
            expect_version,
            author: name,
            format,
        } => {
            memory::validate_id(&id)?;
            let (mut store, index) = writer()?;
            let index = required(index.as_ref())?;
            let current = memory::show(
                Stores {
                    memory: &store,
                    index: Some(index),
                },
                &id,
            )?;
            let draft = evidence.draft(
                current.memory.memory_type,
                title.unwrap_or(current.memory.title),
                claim,
            );
            let receipt = memory::correct(
                &mut store,
                index,
                &id,
                &draft,
                &reason,
                &author(name),
                expect_version,
            )?;
            memory::render_receipt(&receipt, format_of(format))
        }
        MemoryCommand::Approve { id, review, note } => {
            let actor = reviewer(review.reviewer)?;
            let (mut store, index) = writer()?;
            let receipt = memory::review(
                &mut store,
                index.as_ref(),
                &id,
                &review.digest,
                Decision::Approve,
                &actor,
                note.as_deref(),
            )?;
            memory::render_receipt(&receipt, format_of(review.format))
        }
        MemoryCommand::Reject { id, review, reason } => {
            let actor = reviewer(review.reviewer)?;
            let (mut store, index) = writer()?;
            let receipt = memory::review(
                &mut store,
                index.as_ref(),
                &id,
                &review.digest,
                Decision::Reject,
                &actor,
                Some(&reason),
            )?;
            memory::render_receipt(&receipt, format_of(review.format))
        }
        MemoryCommand::Supersede {
            id,
            by,
            reason,
            reviewer: name,
            format,
        } => {
            let actor = reviewer(name)?;
            let (mut store, _) = writer()?;
            let receipt = memory::supersede(&mut store, &id, &by, &actor, &reason)?;
            memory::render_receipt(&receipt, format_of(format))
        }
        MemoryCommand::Retire {
            id,
            reason,
            reviewer: name,
            format,
        } => {
            let actor = reviewer(name)?;
            let (mut store, _) = writer()?;
            let receipt = memory::retire(&mut store, &id, &actor, &reason)?;
            memory::render_receipt(&receipt, format_of(format))
        }
        MemoryCommand::Status { check, format } => {
            let report = memory::status(&nearest()?, check)?;
            json_or(&report, format, |r| {
                let mut text = String::new();
                writeln!(
                    text,
                    "Project memory store · {} · {}",
                    r.location,
                    r.repository_root.escape_debug()
                )?;
                writeln!(
                    text,
                    "source {} · schema {} · repository {} · {} memories, {} revisions, {} evidence, {} events{}",
                    r.store.source,
                    r.store
                        .schema_version
                        .map_or_else(|| "-".into(), |v| v.to_string()),
                    r.store.repository_id.as_deref().unwrap_or("-"),
                    r.store.counts.memories,
                    r.store.counts.revisions,
                    r.store.counts.evidence,
                    r.store.counts.events,
                    r.size_bytes
                        .map_or_else(String::new, |b| format!(" · {b} bytes")),
                )?;
                for event in &r.store.events {
                    writeln!(
                        text,
                        "store event {} {} on {}",
                        event.sequence, event.action, event.at
                    )?;
                }
                if let Some(problems) = &r.integrity {
                    if problems.is_empty() {
                        writeln!(text, "integrity: ok")?;
                    }
                    for problem in problems {
                        writeln!(text, "integrity: {}", problem.escape_debug())?;
                    }
                }
                for note in &r.notes {
                    writeln!(text, "! {}: {}", note.code, note.detail.escape_debug())?;
                }
                Ok(text)
            })
        }
        MemoryCommand::Migrate { format } => {
            let report = memory::migrate(&nearest()?)?;
            json_or(&report, format, |r| {
                let mut text = format!("memory migration: {}\n", r.outcome);
                if let Some(m) = &r.migration {
                    writeln!(
                        text,
                        "{} memories, {} revisions, {} evidence, {} events · source digest {} · {} written · legacy frozen: {}",
                        m.counts.memories,
                        m.counts.revisions,
                        m.counts.evidence,
                        m.counts.events,
                        m.source_digest,
                        m.written,
                        m.legacy_frozen
                    )?;
                    for note in &m.notes {
                        writeln!(text, "! {}", note.escape_debug())?;
                    }
                }
                Ok(text)
            })
        }
        MemoryCommand::Export {
            output,
            force,
            export_format: _,
            format,
        } => {
            let reads = Reads::open()?;
            let export = memory::export(reads.stores())?;
            let mut bytes = serde_json::to_vec_pretty(&export)?;
            bytes.push(b'\n');
            memory::write_file(&output, &bytes, force)?;
            let summary = serde_json::json!({
                "schema_version": 1,
                "kind": "memory_export",
                "output": output.display().to_string(),
                "bytes": bytes.len(),
                "counts": export.counts,
                "source": export.source,
                "integrity": export.integrity,
                "notice": memory::backup_notice(),
            });
            json_or(&summary, format, |_| {
                Ok(format!(
                    "exported {} memories ({} revisions, {} evidence, {} events) to {}\nintegrity blake3 {}\n! {}\n",
                    export.counts.memories,
                    export.counts.revisions,
                    export.counts.evidence,
                    export.counts.events,
                    output.display().to_string().escape_debug(),
                    export.integrity.digest,
                    memory::backup_notice()
                ))
            })
        }
        MemoryCommand::Import {
            file,
            restore,
            dry_run,
            actor,
            format,
        } => {
            let root = nearest()?;
            let export = memory::read_export(&file)?;
            let report = memory::import(
                &root,
                &export,
                if restore {
                    ImportMode::Restore
                } else {
                    ImportMode::Auto
                },
                &author(actor),
                dry_run,
            )?;
            json_or(&report, format, |r| {
                let mut text = format!(
                    "memory import ({}{}) · export {} · {}\n",
                    r.mode,
                    if r.dry_run {
                        ", dry run: nothing written"
                    } else {
                        ""
                    },
                    &r.export_digest[..16],
                    r.outcomes
                        .iter()
                        .map(|(k, v)| format!("{k} {v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                for line in r.memories.iter().take(200) {
                    writeln!(
                        text,
                        "{} -> {} · {}{}{}",
                        line.result.source_memory,
                        line.result.local_memory.as_deref().unwrap_or("-"),
                        line.result.outcome,
                        line.verification
                            .map_or_else(String::new, |s| format!(" · evidence {}", s.label())),
                        line.result
                            .detail
                            .as_deref()
                            .map_or_else(String::new, |d| format!(" · {}", d.escape_debug()))
                    )?;
                }
                if r.memories.len() > 200 {
                    writeln!(
                        text,
                        "… {} more (use --format json)",
                        r.memories.len() - 200
                    )?;
                }
                for note in &r.notes {
                    writeln!(text, "! {}: {}", note.code, note.detail.escape_debug())?;
                }
                Ok(text)
            })
        }
        MemoryCommand::Rebind { from, format } => {
            let root = nearest()?;
            let id = code_intel_store::rebind_memory(&root, &from, &utc_now())?;
            let report = serde_json::json!({
                "schema_version": 1,
                "kind": "memory_rebind",
                "repository_id": id,
                "from": from,
                "to": root.display().to_string(),
            });
            json_or(&report, format, |_| {
                Ok(format!(
                    "memory store {id} rebound from {} to {}\n",
                    from.escape_debug(),
                    root.display().to_string().escape_debug()
                ))
            })
        }
    }
}
