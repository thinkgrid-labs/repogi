mod manifest_artifact;
mod mcp;

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use code_intel_retrieval::{Detail, Format, Options, Strategy};
use code_intel_store::{Store, validate_state_path};

#[derive(Parser)]
#[command(
    name = "repogi",
    version,
    about = "Repogi (Repo Graph Intelligence): local TypeScript/TSX evidence index and bounded task context"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a schema-1 canonical manifest and its identity; never runs code.
    ManifestVerify {
        /// Complete JSON artifact, or - for stdin.
        path: PathBuf,
        #[arg(long)]
        expected_manifest_id: Option<String>,
        #[arg(long)]
        expected_envelope_digest: Option<String>,
    },
    /// Index a repository offline. Outputs a JSON run report.
    Index { path: PathBuf },
    /// Inspect the nearest ancestor .code-intel index. Outputs JSON.
    Status,
    /// Inspect typed static edges and unresolved sites. Does not execute source.
    Graph {
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..=10000))]
        limit: u32,
    },
    /// Retrieve lexical and nearby graph evidence from the persisted index.
    Context {
        task: String,
        /// Conservative estimate for the full rendered response, not a provider count.
        #[arg(long, default_value_t = 2000)]
        max_tokens: usize,
        #[arg(long,value_enum,default_value_t=OutputFormat::Json)]
        format: OutputFormat,
        /// Retrieval ablations retain the Phase 1 lexical and graph controls.
        #[arg(long,value_enum,default_value_t=Retrieval::Optimized)]
        retrieval: Retrieval,
        /// Progressive view; source snippets remain the default.
        #[arg(long,value_enum,default_value_t=ContextDetail::Detailed)]
        detail: ContextDetail,
        /// Select an ID from an overview (repeatable; IDs belong to an index snapshot).
        #[arg(long)]
        select: Vec<String>,
    },
    /// Compare two Git states and report the declarations and files that may be
    /// affected, with static evidence. Read-only: writes only its snapshot cache.
    Impact {
        /// Repository directory (a Git work tree or a directory inside one).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Baseline commit, branch or tag.
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Changed commit; without it the working tree is compared.
        #[arg(long, conflicts_with = "worktree")]
        head: Option<String>,
        /// Compare the working tree, staged and unstaged (default without --head).
        #[arg(long)]
        worktree: bool,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        /// Upstream hops from each changed declaration; 1 = direct consumers only.
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=4))]
        depth: u8,
        /// Cap on every result list.
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u16).range(1..=1000))]
        max_results: u16,
        /// Changed declarations whose consumers are analysed (the rest are listed).
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=10000))]
        max_declarations: u16,
        /// Cap on the whole rendered output, in UTF-8 bytes (default 16000 for
        /// text, 64000 for JSON).
        #[arg(long, value_parser = clap::value_parser!(u32).range(2_000..=8_000_000))]
        max_bytes: Option<u32>,
        /// Evidence: rules, binding chains, transitive paths, import-only, possible and rebound sites.
        #[arg(long)]
        detail: bool,
        /// Group affected files with explicit completeness (JSON with --format json).
        #[arg(long, conflicts_with_all = ["detail", "evidence_file"])]
        summary: bool,
        /// Advisory affected-test discovery; never executes project code.
        #[arg(long, group = "test_discovery", conflicts_with = "summary")]
        tests: bool,
        /// Deterministic verification evidence artifact; JSON only, no execution.
        #[arg(long, group = "test_discovery", conflicts_with_all = ["tests", "summary", "detail", "evidence_file"])]
        manifest: bool,
        /// Additional literal test directory, relative to the repository (repeatable).
        #[arg(long = "test-dir", requires = "test_discovery")]
        test_dirs: Vec<String>,
        /// Detailed evidence for one repository-relative affected file.
        #[arg(long)]
        evidence_file: Option<String>,
        /// Snapshot cache directory (default: <repository>/.code-intel/impact).
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// Serve read-only MCP tools over stdio (JSON-RPC on stdout, logs on stderr).
    Mcp {
        /// Repository root clients may query (repeatable; default: current directory).
        #[arg(long = "root")]
        roots: Vec<PathBuf>,
        /// Tool set and schema verbosity; `full` keeps all five tools.
        #[arg(long, value_enum, default_value_t = mcp::Profile::Full)]
        profile: mcp::Profile,
        /// Comma-separated tools to list instead of the profile's set; the only way
        /// to enable `atlas_impact` (Git-aware change impact), which no profile lists.
        #[arg(long, value_delimiter = ',')]
        tools: Option<Vec<String>>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Text,
}

#[derive(Clone, Copy, ValueEnum)]
enum Retrieval {
    Lexical,
    Graph,
    Optimized,
    Semantic,
}

#[derive(Clone, Copy, ValueEnum)]
enum ContextDetail {
    Overview,
    Focused,
    Detailed,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(output) => match io::stdout().lock().write_all(output.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
            Err(error) => fail(&format!("write output: {error}")),
        },
        Err(error) => fail(&format!("{error:#}")),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!(
        "{}",
        serde_json::json!({"schema_version":1,"error":{"code":"command_failed","message":message}})
    );
    ExitCode::FAILURE
}

#[allow(clippy::too_many_lines)]
fn run(cli: Cli) -> Result<String> {
    match cli.command {
        Command::ManifestVerify {
            path,
            expected_manifest_id,
            expected_envelope_digest,
        } => {
            use std::io::Read;
            let input: Box<dyn Read> = if path == Path::new("-") {
                Box::new(io::stdin())
            } else {
                Box::new(std::fs::File::open(path).context("open manifest artifact")?)
            };
            let mut bytes = Vec::new();
            input
                .take(u64::try_from(
                    code_intel_retrieval::manifest::MAX_BYTES + 1,
                )?)
                .read_to_end(&mut bytes)?;
            let verified = manifest_artifact::verify(&bytes)?;
            anyhow::ensure!(
                expected_manifest_id
                    .as_ref()
                    .is_none_or(|id| id == &verified.manifest_id),
                "expected manifest_id mismatch"
            );
            anyhow::ensure!(
                expected_envelope_digest
                    .as_ref()
                    .is_none_or(|id| id == &verified.envelope_digest),
                "expected envelope digest mismatch"
            );
            Ok(format!("{}\n", verified.report()))
        }
        Command::Index { path } => Ok(format!(
            "{}\n",
            serde_json::to_string(&code_intel_index::index(&path)?)?
        )),
        Command::Mcp {
            roots,
            profile,
            tools,
        } => {
            mcp::serve(roots, profile, tools)?;
            Ok(String::new())
        }
        Command::Impact {
            path,
            base,
            head,
            worktree: _,
            format,
            depth,
            max_results,
            max_declarations,
            max_bytes,
            detail,
            summary,
            tests,
            manifest,
            test_dirs,
            evidence_file,
            cache_dir,
        } => {
            if manifest && !matches!(format, OutputFormat::Json) {
                bail!("--manifest requires --format json");
            }
            let request = code_intel_retrieval::impact::Request {
                root: path,
                base,
                head,
                cache_dir,
                confined: false,
                options: code_intel_retrieval::impact::Options {
                    depth: depth.into(),
                    max_results: max_results.into(),
                    max_declarations: max_declarations.into(),
                    detail: detail || evidence_file.is_some(),
                },
            };
            let default = match format {
                OutputFormat::Text => 16_000,
                OutputFormat::Json => 64_000,
            };
            if manifest {
                return code_intel_retrieval::manifest::respond(
                    &request,
                    &code_intel_retrieval::tests::Options {
                        directories: test_dirs,
                    },
                    max_bytes.map_or(code_intel_retrieval::manifest::MAX_BYTES, |n| n as usize),
                );
            }
            if tests {
                return code_intel_retrieval::tests::respond(
                    &request,
                    &code_intel_retrieval::tests::Options {
                        directories: test_dirs,
                    },
                    evidence_file.as_deref(),
                    matches!(format, OutputFormat::Json),
                    max_bytes.unwrap_or(default) as usize,
                );
            }
            code_intel_retrieval::impact::summary::respond(
                &request,
                summary,
                evidence_file.as_deref(),
                matches!(format, OutputFormat::Json),
                max_bytes.unwrap_or(default) as usize,
            )
        }
        Command::Status => {
            let store = open_nearest()?;
            Ok(format!("{}\n", serde_json::to_string(&store.status()?)?))
        }
        Command::Graph { path, limit } => {
            let store = open_nearest()?;
            let mut edges = store.semantic_edges(path.as_deref(), None, limit + 1)?;
            let truncated = edges.len() > limit as usize;
            edges.truncate(limit as usize);
            Ok(format!(
                "{}\n",
                serde_json::json!({"schema_version":1,"index_revision":store.revision()?,"edges":edges,"truncated":truncated})
            ))
        }
        Command::Context {
            task,
            max_tokens,
            format,
            retrieval,
            detail,
            select,
        } => {
            let store = open_nearest()?;
            let format = match format {
                OutputFormat::Json => Format::Json,
                OutputFormat::Text => Format::Text,
            };
            let options = Options {
                strategy: match retrieval {
                    Retrieval::Lexical => Strategy::Lexical,
                    Retrieval::Graph => Strategy::Graph,
                    Retrieval::Optimized => Strategy::Optimized,
                    Retrieval::Semantic => Strategy::Semantic,
                },
                detail: match detail {
                    ContextDetail::Overview => Detail::Overview,
                    ContextDetail::Focused => Detail::Focused,
                    ContextDetail::Detailed => Detail::Detailed,
                },
                select,
                skip_dependencies: false,
                selection_truncated: false,
            };
            let response =
                code_intel_retrieval::signal::context(&store, &task, max_tokens, format, &options)?;
            code_intel_retrieval::render(&response, format)
        }
    }
}

fn open_nearest() -> Result<Store> {
    let cwd = std::env::current_dir().context("read current directory")?;
    for root in Path::new(&cwd).ancestors() {
        validate_state_path(root)?;
        let database = root.join(".code-intel/index.db");
        if database.exists() {
            return Store::open_read(&database);
        }
    }
    bail!(
        "no .code-intel/index.db found in current directory or ancestors; run `repogi index <path>`, then run status/context inside that repository"
    )
}
