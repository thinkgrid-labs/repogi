use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use code_intel_retrieval::Format;
use code_intel_store::{Store, validate_state_path};

#[derive(Parser)]
#[command(
    name = "code-intel",
    version,
    about = "Local TypeScript/TSX evidence index and bounded task context"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index a repository offline. Outputs a JSON run report.
    Index { path: PathBuf },
    /// Inspect the nearest ancestor .code-intel index. Outputs JSON.
    Status,
    /// Retrieve lexical and nearby graph evidence from the persisted index.
    Context {
        task: String,
        /// Conservative estimate for the full rendered response, not a provider count.
        #[arg(long, default_value_t = 2000)]
        max_tokens: usize,
        #[arg(long,value_enum,default_value_t=OutputFormat::Json)]
        format: OutputFormat,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Text,
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

fn run(cli: Cli) -> Result<String> {
    match cli.command {
        Command::Index { path } => Ok(format!(
            "{}\n",
            serde_json::to_string(&code_intel_index::index(&path)?)?
        )),
        Command::Status => {
            let store = open_nearest()?;
            Ok(format!("{}\n", serde_json::to_string(&store.status()?)?))
        }
        Command::Context {
            task,
            max_tokens,
            format,
        } => {
            let store = open_nearest()?;
            let format = match format {
                OutputFormat::Json => Format::Json,
                OutputFormat::Text => Format::Text,
            };
            let response = code_intel_retrieval::context(&store, &task, max_tokens, format)?;
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
        "no .code-intel/index.db found in current directory or ancestors; run `code-intel index <path>`, then run status/context inside that repository"
    )
}
