//! First-run setup and diagnostics: `init`, `doctor` and `agent add`.
//!
//! `doctor` never writes. `init` writes only untracked local state
//! (`.code-intel/.gitignore`) unless asked to index. `agent add` prints its
//! proposal and writes a project `.mcp.json` only with `--write`, and only when
//! no such file exists. Nothing here uses the network or runs project code.
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use code_intel_core::language::{Language, SOURCE_EXTENSIONS_TEXT};
use code_intel_store::{DATABASE_VERSION, MemoryStore, Store, validate_state_path};
use serde::Serialize;
use serde_json::{Value, json};

/// Navigation tools: the setup the agent guides recommend for ordinary work.
pub const NAVIGATION_TOOLS: &str = "atlas_context,atlas_references,atlas_dependencies";
const CODE_INTEL_IGNORE: &str =
    "# Created by `repogi init`: the index is rebuildable local state.\n*\n";
const SUPPORTED_PLATFORMS: [(&str, &str); 4] = [
    ("linux", "x86_64"),
    ("linux", "aarch64"),
    ("macos", "x86_64"),
    ("macos", "aarch64"),
];

/// How the repository root was chosen.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RootSource {
    /// The nearest ancestor holding `.code-intel/index.db` (what queries use).
    Index,
    /// The enclosing Git work tree.
    Git,
    /// The given directory; no index or Git work tree above it.
    Directory,
}

/// The repository a setup command acts on, from `start` (a path inside it).
pub fn locate(start: &Path, prefer_index: bool) -> Result<(PathBuf, RootSource)> {
    let start = start
        .canonicalize()
        .with_context(|| format!("open {}; provide an existing directory", start.display()))?;
    ensure!(start.is_dir(), "{} is not a directory", start.display());
    if prefer_index
        && let Some(root) = start
            .ancestors()
            .find(|dir| dir.join(".code-intel/index.db").exists())
    {
        return Ok((root.to_path_buf(), RootSource::Index));
    }
    if let Some(root) = start.ancestors().find(|dir| dir.join(".git").exists()) {
        return Ok((root.to_path_buf(), RootSource::Git));
    }
    Ok((start, RootSource::Directory))
}

/// Indexable sources per language and Markdown documents, from the indexer's
/// own discovery rules. Walks paths only; never reads file contents.
fn inventory(root: &Path) -> Result<(BTreeMap<&'static str, usize>, usize)> {
    let mut languages = BTreeMap::new();
    let mut documents = 0;
    for path in code_intel_index::inventory_paths(root)? {
        if let Some(language) = Language::of(&path) {
            *languages.entry(language.name()).or_insert(0) += 1;
        } else if code_intel_index::supported_document(Path::new(&path)) {
            documents += 1;
        }
    }
    Ok((languages, documents))
}

/// Whether Git ignores the index directory: `Some(true|false)`, or `None`
/// when Git is unavailable or this is not a work tree.
fn index_ignored(root: &Path) -> Option<bool> {
    if fs::read_to_string(root.join(".code-intel/.gitignore"))
        .is_ok_and(|text| text.lines().any(|line| line.trim() == "*"))
    {
        return Some(true);
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--no-index", ".code-intel/index.db"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn summary(languages: &BTreeMap<&str, usize>) -> String {
    if languages.is_empty() {
        return "none".into();
    }
    languages
        .iter()
        .map(|(language, count)| format!("{language} {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------- init

#[derive(Serialize)]
pub struct InitReport {
    schema_version: u32,
    root: String,
    root_source: RootSource,
    sources: BTreeMap<&'static str, usize>,
    documents: usize,
    index: Value,
    memory_store: bool,
    actions: Vec<String>,
    next_steps: Vec<String>,
}

/// Prepare a repository. Idempotent; never touches tracked files or memory.
pub fn init(
    start: &Path,
    dry_run: bool,
    run_index: bool,
    index: &dyn Fn(&Path) -> Result<code_intel_core::IndexReport>,
) -> Result<InitReport> {
    let (root, root_source) = locate(start, false)?;
    validate_state_path(&root)?;
    let (sources, documents) = inventory(&root)?;
    let mut actions = Vec::new();
    if root_source == RootSource::Git && index_ignored(&root) != Some(true) {
        if dry_run {
            actions
                .push("would write .code-intel/.gitignore (`*`) so Git ignores the index".into());
        } else {
            write_index_ignore(&root)?;
            actions.push("wrote .code-intel/.gitignore (`*`) so Git ignores the index; no tracked file changed".into());
        }
    }
    let database = root.join(".code-intel/index.db");
    if run_index && !dry_run {
        let report = index(&root)?;
        actions.push(format!(
            "indexed {} source files ({} changed) and {} documents",
            report.files_scanned, report.files_changed, report.documents.scanned
        ));
    }
    let index = if database.exists() {
        match Store::open_read(&database).and_then(|store| {
            Ok(json!({
                "present": true,
                "owned": store.root()?.as_deref() == root.to_str(),
                "revision": store.revision()?,
                "languages": store.languages()?,
            }))
        }) {
            Ok(value) => value,
            Err(error) => json!({"present": true, "error": format!("{error:#}")}),
        }
    } else {
        json!({"present": false})
    };
    let shown = root.display();
    let mut next_steps = Vec::new();
    if !database.exists() {
        next_steps.push(format!("repogi index {shown}"));
    }
    next_steps.push(format!("repogi doctor {shown}"));
    next_steps.push(format!("repogi agent add claude --root {shown}"));
    Ok(InitReport {
        schema_version: 1,
        root: root.to_string_lossy().into(),
        root_source,
        sources,
        documents,
        index,
        memory_store: root.join(code_intel_store::MEMORY_FILE).exists(),
        actions,
        next_steps,
    })
}

/// Create `.code-intel/` and its self-ignoring `.gitignore`; an existing
/// file (or symlink) is left exactly as it is.
fn write_index_ignore(root: &Path) -> Result<()> {
    let dir = root.join(".code-intel");
    match fs::create_dir(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(anyhow::Error::from(error)
                .context("create .code-intel; check repository permissions"));
        }
    }
    validate_state_path(root)?;
    ensure!(
        fs::symlink_metadata(&dir)?.is_dir(),
        "{} is not a directory; move it aside",
        dir.display()
    );
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(".gitignore"))
    {
        Ok(mut file) => file.write_all(CODE_INTEL_IGNORE.as_bytes())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("write .code-intel/.gitignore"),
    }
    Ok(())
}

pub fn render_init(report: &InitReport) -> String {
    let mut out = format!("repogi init: {}\n", report.root);
    let source = match report.root_source {
        RootSource::Git => "Git work tree",
        RootSource::Index => "existing index",
        RootSource::Directory => "directory (no Git work tree found)",
    };
    let _ = writeln!(out, "  repository: {source}");
    let _ = writeln!(
        out,
        "  sources:    {} ({SOURCE_EXTENSIONS_TEXT}); {} Markdown documents",
        summary(&report.sources),
        report.documents
    );
    let index = &report.index;
    let _ = writeln!(
        out,
        "  index:      {}",
        if index["present"] == true {
            if let Some(error) = index["error"].as_str() {
                format!("present but unreadable: {error}")
            } else if index["owned"] == false {
                "present but recorded for a different root (copied?)".into()
            } else {
                format!("revision {}", index["revision"])
            }
        } else {
            "not built yet".into()
        }
    );
    let _ = writeln!(
        out,
        "  memory:     {}",
        if report.memory_store {
            ".repogi/memory.db present (never changed by init)"
        } else {
            "no .repogi/memory.db yet (created by the first memory write)"
        }
    );
    for action in &report.actions {
        let _ = writeln!(out, "  done:       {action}");
    }
    if report.actions.is_empty() {
        let _ = writeln!(out, "  done:       nothing to change");
    }
    let _ = writeln!(out, "next:");
    for step in &report.next_steps {
        let _ = writeln!(out, "  {step}");
    }
    out
}

// -------------------------------------------------------------- doctor

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    Skipped,
    Unsupported,
    Warning,
    Error,
}

#[derive(Serialize)]
pub struct Check {
    id: &'static str,
    status: Health,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
    #[serde(skip_serializing_if = "Value::is_null")]
    details: Value,
}

impl Check {
    fn new(id: &'static str, status: Health, summary: impl Into<String>) -> Self {
        Self {
            id,
            status,
            summary: summary.into(),
            fix: None,
            details: Value::Null,
        }
    }
    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
    fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

#[derive(Serialize)]
pub struct DoctorReport {
    schema_version: u32,
    repogi_version: &'static str,
    root: Option<String>,
    status: Health,
    checks: Vec<Check>,
}

impl DoctorReport {
    #[must_use]
    pub fn failed(&self) -> bool {
        self.status == Health::Error
    }
}

/// Read-only diagnostics. Never indexes, migrates or edits configuration.
pub fn doctor(start: &Path, check_freshness: bool) -> DoctorReport {
    let mut checks = vec![binary_check()];
    let root = match locate(start, true) {
        Ok((root, source)) => {
            checks.push(
                Check::new(
                    "repository",
                    Health::Ok,
                    format!("{} ({source:?})", root.display())
                        .replace("(Index)", "(nearest index)"),
                )
                .details(json!({"root": root, "source": source})),
            );
            Some(root)
        }
        Err(error) => {
            checks.push(
                Check::new("repository", Health::Error, format!("{error:#}"))
                    .fix("run inside the repository, or pass its path: repogi doctor <path>"),
            );
            None
        }
    };
    if let Some(root) = &root {
        repository_checks(root, check_freshness, &mut checks);
    }
    let status = checks
        .iter()
        .map(|check| check.status)
        .filter(|status| matches!(status, Health::Warning | Health::Error))
        .max()
        .unwrap_or(Health::Ok);
    DoctorReport {
        schema_version: 1,
        repogi_version: env!("CARGO_PKG_VERSION"),
        root: root.map(|root| root.to_string_lossy().into()),
        status,
        checks,
    }
}

fn binary_check() -> Check {
    let platform = (std::env::consts::OS, std::env::consts::ARCH);
    let executable = std::env::current_exe().ok();
    let details = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": platform.0,
        "arch": platform.1,
        "executable": executable,
        "build": code_intel_index::build_key().ok(),
    });
    let summary = format!(
        "repogi {} on {}-{}",
        env!("CARGO_PKG_VERSION"),
        platform.0,
        platform.1
    );
    if SUPPORTED_PLATFORMS.contains(&platform) {
        Check::new("binary", Health::Ok, summary).details(details)
    } else {
        Check::new(
            "binary",
            Health::Unsupported,
            format!("{summary}: no release binaries or CI for this platform"),
        )
        .fix("use Linux or macOS (x86_64 or arm64); see docs/RELEASES.md")
        .details(details)
    }
}

fn repository_checks(root: &Path, check_freshness: bool, checks: &mut Vec<Check>) {
    let shown = root.display();
    if let Err(error) = validate_state_path(root) {
        checks.push(
            Check::new("state_paths", Health::Error, format!("{error:#}"))
                .fix("replace the symlink with a real local directory; Repogi never follows links for its state"),
        );
        return;
    }
    let discovered = match inventory(root) {
        Ok(found) => Some(found),
        Err(error) => {
            checks.push(
                Check::new("discovery", Health::Error, format!("{error:#}"))
                    .fix("fix file permissions or ignore rules, then retry"),
            );
            None
        }
    };
    let database = root.join(".code-intel/index.db");
    let store = if database.exists() {
        match Store::open_read(&database) {
            Ok(store) => Some(store),
            Err(error) => {
                checks.push(
                    Check::new("index", Health::Error, format!("{error:#}"))
                        .fix(format!("re-index with this binary: repogi index {shown} (memories in .repogi are not touched)")),
                );
                None
            }
        }
    } else {
        checks.push(
            Check::new("index", Health::Warning, "index is missing")
                .fix(format!("repogi index {shown}")),
        );
        None
    };
    if let Some(store) = &store {
        checks.push(index_check(store, root));
        if let Some((sources, _)) = &discovered {
            checks.push(coverage_check(store, sources, root));
        }
        checks.push(if check_freshness {
            freshness_check(store, root)
        } else {
            Check::new(
                "freshness",
                Health::Skipped,
                "source hashes not compared (reads every source file)",
            )
            .fix("repogi doctor --freshness")
        });
    }
    if let Some((sources, documents)) = discovered {
        checks.push(
            Check::new(
                "sources",
                if sources.is_empty() && documents == 0 {
                    Health::Warning
                } else {
                    Health::Ok
                },
                format!(
                    "{}; {documents} Markdown documents",
                    summary(&sources)
                ),
            )
            .details(json!({"languages": sources, "documents": documents, "extensions": SOURCE_EXTENSIONS_TEXT})),
        );
    }
    checks.push(memory_check(root));
    checks.push(ignore_check(root));
    checks.push(claude_check(root));
    checks.push(codex_check(root));
}

fn index_check(store: &Store, root: &Path) -> Check {
    let shown = root.display();
    let read = || -> Result<Check> {
        let schema = store.schema_version()?;
        let owner = store.root()?;
        if owner.as_deref() != root.to_str() {
            return Ok(Check::new(
                "index",
                Health::Error,
                format!(
                    "index records root {} (copied or moved from elsewhere)",
                    owner.as_deref().unwrap_or("<none>")
                ),
            )
            .fix(format!(
                "move .code-intel aside (it is rebuildable), then repogi index {shown}; keep .repogi"
            )));
        }
        let ir = store.semantic_ir_version()?;
        let details = json!({
            "schema": schema,
            "supported_schema": DATABASE_VERSION,
            "revision": store.revision()?,
            "semantic_ir": ir,
            "indexer_build": store.indexer_build()?,
            "current_build": code_intel_index::build_key().ok(),
            "knowledge": store.knowledge_available()?,
        });
        if schema < DATABASE_VERSION {
            return Ok(Check::new(
                "index",
                Health::Warning,
                format!("index schema {schema} is older than {DATABASE_VERSION}"),
            )
            .fix(format!(
                "repogi index {shown} (upgrades in place; memories are moved to .repogi first)"
            ))
            .details(details));
        }
        if ir != Some(code_intel_core::semantic::SEMANTIC_IR_VERSION) {
            return Ok(Check::new(
                "index",
                Health::Warning,
                "index was built by an older extractor",
            )
            .fix(format!("repogi index {shown}"))
            .details(details));
        }
        Ok(Check::new(
            "index",
            Health::Ok,
            format!("schema {schema}, revision {}", details["revision"]),
        )
        .details(details))
    };
    read().unwrap_or_else(|error| {
        Check::new("index", Health::Error, format!("{error:#}"))
            .fix(format!("repogi index {shown}"))
    })
}

/// Discovered sources per language against indexed files per language. Catches
/// an index built before a language was supported, without reading contents.
fn coverage_check(store: &Store, sources: &BTreeMap<&'static str, usize>, root: &Path) -> Check {
    let indexed = match store.languages() {
        Ok(indexed) => indexed,
        Err(error) => return Check::new("coverage", Health::Error, format!("{error:#}")),
    };
    let missing: Vec<String> = sources
        .iter()
        .filter(|(language, count)| **count > 0 && !indexed.contains_key(**language))
        .map(|(language, count)| format!("{language} ({count} files)"))
        .collect();
    let details = json!({"discovered": sources, "indexed": indexed});
    if missing.is_empty() {
        Check::new(
            "coverage",
            Health::Ok,
            format!(
                "indexed {}",
                summary(&indexed.iter().map(|(k, v)| (k.as_str(), *v)).collect())
            ),
        )
        .details(details)
    } else {
        Check::new(
            "coverage",
            Health::Warning,
            format!(
                "not in the index: {}; it predates support for them or is stale",
                missing.join(", ")
            ),
        )
        .fix(format!("repogi index {}", root.display()))
        .details(details)
    }
}

fn freshness_check(store: &Store, root: &Path) -> Check {
    let result = store
        .hashes()
        .and_then(|hashes| code_intel_index::freshness(root, &hashes, 10));
    match result {
        Ok(fresh) if fresh.fresh => Check::new(
            "freshness",
            Health::Ok,
            format!(
                "{} indexed files match the working tree",
                fresh.files_checked
            ),
        ),
        Ok(fresh) => Check::new(
            "freshness",
            Health::Warning,
            format!(
                "stale: {} added, {} changed, {} removed",
                fresh.added, fresh.changed, fresh.removed
            ),
        )
        .fix(format!("repogi index {}", root.display()))
        .details(json!({"sample": fresh.sample})),
        Err(error) => Check::new("freshness", Health::Error, format!("{error:#}")),
    }
}

fn memory_check(root: &Path) -> Check {
    let path = root.join(code_intel_store::MEMORY_FILE);
    let legacy_hint = "repogi memory migrate";
    if !path.exists() {
        // A schema-5 index may still hold memories that were never migrated.
        return match MemoryStore::open_read(root) {
            Ok(store) if !store.notes().is_empty() => {
                Check::new("memory", Health::Warning, store.notes()[0].1.clone()).fix(legacy_hint)
            }
            _ => Check::new(
                "memory",
                Health::Ok,
                "no .repogi/memory.db (created by the first memory write)",
            ),
        };
    }
    match MemoryStore::open_read(root)
        .and_then(|store| Ok((store.counts()?, store.notes().to_vec())))
    {
        Ok((counts, notes)) => {
            let check = Check::new(
                "memory",
                if notes.is_empty() { Health::Ok } else { Health::Warning },
                format!("{} memories in .repogi/memory.db", counts.memories),
            )
            .details(json!({"memories": counts.memories, "notes": notes.iter().map(|(code, text)| json!({"code": code, "message": text})).collect::<Vec<_>>()}));
            if notes.is_empty() {
                check
            } else {
                check.fix(legacy_hint)
            }
        }
        Err(error) => {
            let (code, message) = error
                .downcast_ref::<code_intel_store::MemoryError>()
                .map_or(("memory_unreadable", format!("{error:#}")), |memory| {
                    (memory.code, memory.message.clone())
                });
            Check::new("memory", Health::Error, message)
                .fix("inspect or move .repogi/memory.db aside with a compatible repogi binary; never delete it before exporting (repogi memory export)")
                .details(json!({"code": code}))
        }
    }
}

fn ignore_check(root: &Path) -> Check {
    if !root.join(".git").exists() {
        return Check::new("ignore", Health::Skipped, "not a Git work tree");
    }
    let mut problems = Vec::new();
    match index_ignored(root) {
        Some(true) => {}
        Some(false) => problems.push(".code-intel/ is not ignored by Git"),
        None => {
            return Check::new(
                "ignore",
                Health::Skipped,
                "git unavailable; ignore rules not checked",
            );
        }
    }
    if root.join(".repogi").is_dir() && !root.join(".repogi/.gitignore").exists() {
        problems.push(".repogi/.gitignore is missing, so memory.db could be committed");
    }
    if problems.is_empty() {
        Check::new("ignore", Health::Ok, "local state is ignored by Git")
    } else {
        Check::new("ignore", Health::Warning, problems.join("; ")).fix(
            "repogi init (writes .code-intel/.gitignore); for .repogi, restore a .gitignore containing `*`",
        )
    }
}

fn claude_check(root: &Path) -> Check {
    let path = root.join(".mcp.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Check::new(
                "agent_claude",
                Health::Skipped,
                "no project .mcp.json (local and user Claude Code scopes are not inspected)",
            )
            .fix(format!("repogi agent add claude --root {}", root.display()));
        }
        Err(error) => {
            return Check::new(
                "agent_claude",
                Health::Error,
                format!("read .mcp.json: {error}"),
            );
        }
    };
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        return Check::new("agent_claude", Health::Error, ".mcp.json is not valid JSON")
            .fix("repair .mcp.json; Claude Code cannot load it either");
    };
    let entries: Vec<(&String, &Value)> = config["mcpServers"]
        .as_object()
        .map(|servers| {
            servers
                .iter()
                .filter(|(_, server)| {
                    server["args"].as_array().and_then(|a| a.first()) == Some(&json!("mcp"))
                        && server["command"]
                            .as_str()
                            .is_some_and(|command| command.contains("repogi"))
                })
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        return Check::new(
            "agent_claude",
            Health::Skipped,
            ".mcp.json has no Repogi server",
        )
        .fix(format!("repogi agent add claude --root {}", root.display()));
    }
    let mut problems = Vec::new();
    for (name, server) in &entries {
        let command = server["command"].as_str().unwrap_or_default();
        if !Path::new(command).is_absolute() {
            problems.push(format!(
                "{name}: command {command:?} is not an absolute path"
            ));
        } else if !Path::new(command).is_file() {
            problems.push(format!("{name}: command {command} does not exist"));
        }
        let args: Vec<&str> = server["args"]
            .as_array()
            .map(|args| args.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let roots: Vec<&str> = args
            .windows(2)
            .filter(|pair| pair[0] == "--root")
            .map(|pair| pair[1])
            .collect();
        if roots.is_empty() {
            problems.push(format!(
                "{name}: no --root (the server would use its working directory)"
            ));
        }
        for configured in roots {
            if Path::new(configured).canonicalize().ok().as_deref() != Some(root) {
                problems.push(format!(
                    "{name}: --root {configured} is not this repository"
                ));
            }
        }
    }
    let names: Vec<&String> = entries.iter().map(|(name, _)| *name).collect();
    if problems.is_empty() {
        Check::new(
            "agent_claude",
            Health::Ok,
            format!(".mcp.json server(s) {names:?} point at this repository"),
        )
    } else {
        Check::new("agent_claude", Health::Error, problems.join("; ")).fix(
            "edit .mcp.json: use the absolute repogi path (`command -v repogi`) and --root set to this repository",
        )
    }
}

/// A light reading of `.codex/config.toml` (no TOML parser): Repogi sections
/// and whether their arguments name this repository.
fn codex_check(root: &Path) -> Check {
    let path = root.join(".codex/config.toml");
    let Ok(text) = fs::read_to_string(&path) else {
        return Check::new(
            "agent_codex",
            Health::Skipped,
            "no project .codex/config.toml (user-wide ~/.codex is not inspected)",
        )
        .fix(format!("repogi agent add codex --root {}", root.display()));
    };
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line
            .strip_prefix("[mcp_servers.")
            .and_then(|rest| rest.strip_suffix(']'))
        {
            sections.push((name.into(), String::new()));
        } else if line.starts_with('[') {
            sections.push((String::new(), String::new()));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    let repogi: Vec<&(String, String)> = sections
        .iter()
        .filter(|(name, body)| {
            !name.is_empty() && body.contains("repogi") && body.contains("\"mcp\"")
        })
        .collect();
    if repogi.is_empty() {
        return Check::new(
            "agent_codex",
            Health::Skipped,
            ".codex/config.toml has no Repogi server",
        );
    }
    let root_arg = format!("\"--root\", {}", toml_string(&root.to_string_lossy()));
    let wrong: Vec<&str> = repogi
        .iter()
        .filter(|(_, body)| !body.contains(&root_arg))
        .map(|(name, _)| name.as_str())
        .collect();
    if wrong.is_empty() {
        Check::new(
            "agent_codex",
            Health::Ok,
            "project .codex/config.toml points at this repository (Codex loads it only for trusted projects)",
        )
    } else {
        Check::new(
            "agent_codex",
            Health::Warning,
            format!(
                "server(s) {wrong:?} in .codex/config.toml do not pass --root for this repository"
            ),
        )
        .fix("set args to [\"mcp\", \"--root\", \"<this repository>\", ...] with an absolute path")
    }
}

pub fn render_doctor(report: &DoctorReport) -> String {
    let mut out = format!(
        "repogi doctor: {}\n",
        report.root.as_deref().unwrap_or("<no repository>")
    );
    for check in &report.checks {
        let mark = match check.status {
            Health::Ok => "ok  ",
            Health::Skipped => "skip",
            Health::Unsupported => "unsp",
            Health::Warning => "warn",
            Health::Error => "FAIL",
        };
        let _ = writeln!(out, "  [{mark}] {:<13} {}", check.id, check.summary);
        if let Some(fix) = &check.fix
            && check.status != Health::Ok
        {
            let _ = writeln!(out, "         {:<13} fix: {fix}", "");
        }
    }
    let _ = writeln!(
        out,
        "status: {}",
        match report.status {
            Health::Error => "problems found",
            Health::Warning => "usable, with warnings",
            _ => "healthy",
        }
    );
    out
}

// ---------------------------------------------------------------- agent

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Client {
    Claude,
    Codex,
}

/// Print, or with `--write` create, a project MCP entry for exactly one
/// repository. Existing client files are never rewritten.
pub fn agent_add(
    client: Client,
    root: &Path,
    tools: &str,
    name: &str,
    write: bool,
) -> Result<String> {
    let root = root.canonicalize().with_context(|| {
        format!(
            "open {}; --root must be an existing directory",
            root.display()
        )
    })?;
    ensure!(root.is_dir(), "--root must be a directory");
    validate_state_path(&root)?;
    ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "--name may contain only letters, digits, '-' and '_'"
    );
    for tool in tools.split(',') {
        ensure!(
            crate::mcp::tools::SELECTABLE_TOOLS.contains(&tool),
            "unknown tool {tool:?}; choose from {}",
            crate::mcp::tools::SELECTABLE_TOOLS.join(", ")
        );
    }
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .context("locate the running repogi executable")?;
    let exe = executable.to_str().context("repogi path must be UTF-8")?;
    let root_text = root.to_str().context("repository path must be UTF-8")?;
    let args = [
        "mcp",
        "--root",
        root_text,
        "--profile",
        "compact",
        "--tools",
        tools,
    ];
    let mut out = match client {
        Client::Codex => codex_project(&root, name, exe, &args, write)?,
        Client::Claude => claude_project(&root, name, exe, &args, write)?,
    };
    if !root.join(".code-intel/index.db").exists() {
        let _ = write!(
            out,
            "\nnote: no index yet; run `repogi index {root_text}` before the agent connects\n"
        );
    }
    Ok(out)
}

/// A TOML basic string. Paths with control characters are refused earlier.
fn toml_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The Codex project entry in `.codex/config.toml`, which Codex loads only for
/// projects the user has marked trusted: printed, or created when no such file
/// exists. An existing file is never rewritten.
fn codex_project(root: &Path, name: &str, exe: &str, args: &[&str], write: bool) -> Result<String> {
    ensure!(
        !exe.chars()
            .chain(args.iter().flat_map(|arg| arg.chars()))
            .any(char::is_control),
        "paths with control characters cannot be written to TOML"
    );
    let quoted: Vec<String> = args.iter().map(|arg| toml_string(arg)).collect();
    let entry = format!(
        "[mcp_servers.{name}]\ncommand = {}\nargs = [{}]\n",
        toml_string(exe),
        quoted.join(", ")
    );
    let session = format!(
        "codex -c 'mcp_servers.{name}.command={}' -c 'mcp_servers.{name}.args=[{}]'",
        toml_string(exe),
        quoted.join(",")
    );
    let directory = root.join(".codex");
    let path = directory.join("config.toml");
    let trust = "Codex reads a project .codex/config.toml only after you trust the project.";
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            ensure!(
                !meta.file_type().is_symlink(),
                ".codex/config.toml is a symlink; refusing to read or write it"
            );
            let existing = fs::read_to_string(&path)?;
            let header = format!("[mcp_servers.{name}]");
            if existing.contains(&entry) {
                return Ok(format!(
                    ".codex/config.toml already has this `{name}` entry; nothing to do.\n{trust}\n"
                ));
            }
            ensure!(
                !existing.lines().any(|line| line.trim() == header),
                ".codex/config.toml already has a different `{name}` entry; it was not changed. Edit it yourself, or choose another --name"
            );
            ensure!(
                !write,
                ".codex/config.toml exists; repogi does not rewrite it. Add this entry by hand:\n\n{entry}"
            );
            Ok(format!(
                "Add this to {}:\n\n{entry}\n{trust}\nOr pass it for one session only:\n  {session}\n\nNothing was written.\n",
                path.display()
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !write {
                return Ok(format!(
                    "Proposed {} (project scope):\n\n{entry}\nWrite it with --write. {trust}\nOr pass it for one session only:\n  {session}\n\nNothing was written.\n",
                    path.display()
                ));
            }
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("create .codex"),
            }
            ensure!(
                fs::symlink_metadata(&directory)?.is_dir(),
                ".codex is not a directory; refusing to write"
            );
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .context("create .codex/config.toml")?
                .write_all(entry.as_bytes())?;
            Ok(format!(
                "Wrote {}. It holds absolute paths for this machine, so think twice before committing it.\n{trust}\n",
                path.display()
            ))
        }
        Err(error) => Err(error).context("inspect .codex/config.toml"),
    }
}

/// The Claude Code project entry: printed, or created when no `.mcp.json`
/// exists. An existing file is never rewritten.
fn claude_project(
    root: &Path,
    name: &str,
    exe: &str,
    args: &[&str],
    write: bool,
) -> Result<String> {
    let entry = json!({"type": "stdio", "command": exe, "args": args});
    let path = root.join(".mcp.json");
    let shell_args = args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let local = format!(
        "claude mcp add --scope local {name} -- {} {shell_args}",
        shell_quote(exe)
    );
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            ensure!(
                !meta.file_type().is_symlink(),
                ".mcp.json is a symlink; refusing to read or write it"
            );
            let existing: Value = serde_json::from_str(&fs::read_to_string(&path)?)
                .context(".mcp.json is not valid JSON; nothing was written")?;
            let current = &existing["mcpServers"][name];
            if *current == entry {
                return Ok(format!(
                    ".mcp.json already has this `{name}` entry; nothing to do.\n"
                ));
            }
            ensure!(
                current.is_null(),
                ".mcp.json already has a different `{name}` entry; it was not changed. Edit it yourself, or choose another --name"
            );
            ensure!(
                !write,
                ".mcp.json exists; repogi does not rewrite it. Add the entry below by hand, or run:\n  {local}"
            );
            Ok(format!(
                "Add this server to \"mcpServers\" in {} (other entries stay as they are):\n\n\"{name}\": {}\n\nOr keep it out of the repository with Claude Code's local scope:\n  {local}\n\nNothing was written.\n",
                path.display(),
                serde_json::to_string_pretty(&entry)?
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let document = json!({"mcpServers": {name: entry}});
            let text = format!("{}\n", serde_json::to_string_pretty(&document)?);
            if !write {
                return Ok(format!(
                    "Proposed {} (project scope):\n\n{text}\nWrite it with --write, or keep it out of the repository with Claude Code's local scope:\n  {local}\n\nNothing was written.\n",
                    path.display()
                ));
            }
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .context("create .mcp.json")?
                .write_all(text.as_bytes())?;
            Ok(format!(
                "Wrote {}. It holds absolute paths for this machine, so think twice before committing it.\nClaude Code asks you to approve project servers the first time.\n",
                path.display()
            ))
        }
        Err(error) => Err(error).context("inspect .mcp.json"),
    }
}

fn shell_quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-,=:@".contains(c))
    {
        text.into()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}
