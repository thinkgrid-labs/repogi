//! `repogi memory`: reviewed project memory for the repository that owns the
//! nearest index. Reads open the index read-only and never migrate; writes
//! open it read-write only for that command and never migrate either.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use code_intel_core::memory::{MemoryStatus, MemoryType};
use code_intel_retrieval::{
    Format,
    memory::{self, Draft, Selector},
};
use code_intel_store::{Decision, Store, validate_state_path};

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

/// The nearest ancestor that owns an index, with its database path.
fn nearest() -> Result<(PathBuf, PathBuf)> {
    let cwd = std::env::current_dir().context("read current directory")?;
    for root in Path::new(&cwd).ancestors() {
        validate_state_path(root)?;
        let database = root.join(".code-intel/index.db");
        if database.exists() {
            return Ok((root.to_path_buf(), database));
        }
    }
    bail!(
        "no .code-intel/index.db found in current directory or ancestors; run `repogi index <path>` first"
    )
}

fn writer() -> Result<Store> {
    let (root, database) = nearest()?;
    Store::open_memory_writer(&database, &root)
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
            let store = crate::open_nearest()?;
            let (format, max) = output.resolve();
            render(
                &mut memory::list(&store, statuses, memory_type, after, limit as usize)?,
                format,
                max,
            )
        }
        MemoryCommand::Show { id, output } => {
            let store = crate::open_nearest()?;
            let (format, max) = output.resolve();
            render(&mut memory::show(&store, &id)?, format, max)
        }
        MemoryCommand::History { id, limit, output } => {
            let store = crate::open_nearest()?;
            let (format, max) = output.resolve();
            render(
                &mut memory::history(&store, &id, limit as usize)?,
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
            let store = crate::open_nearest()?;
            let (format, max) = output.resolve();
            render(
                &mut memory::search(&store, &query, statuses, include_unreviewed, limit as usize)?,
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
                let mut store = writer()?;
                let mut verification = memory::verify(&store, &ids, all, limit as usize)?;
                memory::record_verification(&mut store, &mut verification, &actor)?;
                return render(&mut verification, format, max);
            }
            let store = crate::open_nearest()?;
            render(
                &mut memory::verify(&store, &ids, all, limit as usize)?,
                format,
                max,
            )
        }
        MemoryCommand::Conflicts {
            after,
            limit,
            output,
        } => {
            let store = crate::open_nearest()?;
            let (format, max) = output.resolve();
            render(
                &mut memory::conflicts(&store, after, limit as usize)?,
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
            let mut store = writer()?;
            let receipt = memory::propose(
                &mut store,
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
            let mut store = writer()?;
            let current = memory::show(&store, &id)?;
            let draft = evidence.draft(
                current.memory.memory_type,
                title.unwrap_or(current.memory.title),
                claim,
            );
            let receipt = memory::correct(
                &mut store,
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
            let mut store = writer()?;
            let receipt = memory::review(
                &mut store,
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
            let mut store = writer()?;
            let receipt = memory::review(
                &mut store,
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
            let mut store = writer()?;
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
            let mut store = writer()?;
            let receipt = memory::retire(&mut store, &id, &actor, &reason)?;
            memory::render_receipt(&receipt, format_of(format))
        }
    }
}
