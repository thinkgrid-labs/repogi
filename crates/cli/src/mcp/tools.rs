//! Read-only MCP tools. Argument validation, root confinement and error codes
//! live here; retrieval, traversal and freshness logic stay in their crates.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use code_intel_core::semantic::Relationship;
use code_intel_retrieval::{
    Detail, Format, Options, Strategy,
    navigate::{self, Direction, NavigationError, Selector},
};
use code_intel_store::{Interrupt, Store, validate_state_path};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

const DEFAULT_BYTES: usize = 4000;
const MIN_BYTES: usize = 256;
const MAX_BYTES: usize = 32_000;
const MAX_SELECT: usize = 32;
const MAX_TEXT: usize = 1024;
const FRESHNESS_SAMPLE: usize = 20;
const DIAGNOSTIC_SAMPLE: usize = 10;

/// The `full` profile's tools, in listing order (unchanged since Phase 3.0).
pub const ALL_TOOLS: [&str; 5] = [
    "atlas_context",
    "atlas_symbol",
    "atlas_references",
    "atlas_dependencies",
    "atlas_status",
];

/// Every tool `--tools` can select, in listing order. `atlas_impact` is opt-in:
/// no profile lists it, so default clients see no new schema.
pub const SELECTABLE_TOOLS: [&str; 6] = [
    "atlas_context",
    "atlas_symbol",
    "atlas_references",
    "atlas_dependencies",
    "atlas_impact",
    "atlas_status",
];

const IMPACT_DEFAULT_BYTES: usize = 8000;
const IMPACT_MIN_BYTES: usize = 2000;
const MAX_REVISION: usize = 256;
const MAX_TEST_DIRS: usize = 16;

/// Which tools are listed and how verbose their schemas are. Definitions are
/// sent to the model every session, so `compact` trades the graph and status
/// tools and optional schema detail for a smaller fixed prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile {
    /// All five tools with full schemas.
    Full,
    /// `atlas_context` and `atlas_symbol` with minimal schemas.
    Compact,
}

impl Profile {
    fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Full => &ALL_TOOLS,
            Self::Compact => &ALL_TOOLS[..2],
        }
    }

    pub fn instructions(self) -> &'static str {
        match self {
            Self::Full => {
                "Atlas: indexed TypeScript/TSX evidence, not live edits. atlas_context discovers declarations; atlas_symbol inspects a returned ID; atlas_references lists direct users; atlas_dependencies traverses upstream consumers or downstream dependencies by depth. Selected-file byte checks do not verify graph freshness; atlas_status checks the full index. Use native Read/Grep for literals, known files and other languages; inspect source before editing."
            }
            Self::Compact => {
                "Atlas: indexed TypeScript/TSX declarations, not live edits. Discover identifiers with atlas_context; inspect a returned ID with atlas_symbol. Selected-file checks do not verify graph freshness. Use native Read/Grep for known files, literals and other languages."
            }
        }
    }
}

pub struct Server {
    roots: Vec<PathBuf>,
    profile: Profile,
    tools: Vec<&'static str>,
}

pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
}

struct ToolError {
    code: &'static str,
    message: String,
}

impl ToolError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Typed navigation errors keep their codes; other failures are reported
    /// with their actionable message under the caller's code.
    fn from_anyhow(code: &'static str, error: &anyhow::Error) -> Self {
        if let Some(small) = error.downcast_ref::<code_intel_retrieval::BudgetTooSmall>() {
            return Self::new(
                "budget_too_small",
                format!(
                    "max_bytes is too small for metadata; increase max_bytes to at least {}",
                    small.minimum
                ),
            );
        }
        error.downcast_ref::<NavigationError>().map_or_else(
            || Self::new(code, format!("{error:#}")),
            |navigation| Self::new(navigation.code, navigation.message.clone()),
        )
    }
}

type ToolResult = std::result::Result<String, ToolError>;

impl Server {
    /// `tools` overrides the profile's tool set (schemas keep the profile's style).
    pub fn new(roots: Vec<PathBuf>, profile: Profile, tools: Option<Vec<String>>) -> Result<Self> {
        let tools = match tools {
            None => profile.tools().to_vec(),
            Some(requested) => {
                if requested.is_empty() {
                    bail!("--tools must name at least one tool");
                }
                for name in &requested {
                    if !SELECTABLE_TOOLS.contains(&name.as_str()) {
                        bail!(
                            "unknown tool {name}; available: {}",
                            SELECTABLE_TOOLS.join(", ")
                        );
                    }
                }
                SELECTABLE_TOOLS
                    .into_iter()
                    .filter(|tool| requested.iter().any(|name| name == tool))
                    .collect()
            }
        };
        let roots = if roots.is_empty() {
            vec![std::env::current_dir().context("read current directory")?]
        } else {
            roots
        };
        let mut canonical = Vec::new();
        for root in roots {
            let path = root
                .canonicalize()
                .with_context(|| format!("open repository root {}", root.display()))?;
            if !path.is_dir() {
                bail!("repository root {} is not a directory", path.display());
            }
            if path.to_str().is_none() {
                bail!("repository root {} must be valid UTF-8", path.display());
            }
            canonical.push(path);
        }
        canonical.sort();
        canonical.dedup();
        Ok(Self {
            roots: canonical,
            profile,
            tools,
        })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn tools(&self) -> &[&'static str] {
        &self.tools
    }

    /// Profile text for a profile's own tool set; otherwise guidance built
    /// from the listed tools, so a custom set never mentions an absent tool.
    pub fn instructions(&self) -> String {
        if self.tools == self.profile.tools() {
            return self.profile.instructions().to_owned();
        }
        let navigation = self.tools.iter().any(|tool| {
            matches!(
                *tool,
                "atlas_context" | "atlas_symbol" | "atlas_references" | "atlas_dependencies"
            )
        });
        let mut text = String::from(if navigation || !self.tools.contains(&"atlas_impact") {
            "Atlas: read-only TypeScript/TSX index from the last `repogi index` run."
        } else {
            "Atlas: read-only static analysis of TypeScript/TSX changes."
        });
        if self.tools.contains(&"atlas_impact") {
            text.push_str(" atlas_impact reviews Git changes: choose summary for files, tests for verification planning, or focused evidence. Possible/lost bindings need inspection; complete never means safe.");
        }
        if self.tools.contains(&"atlas_references") {
            text.push_str(" atlas_references finds direct users of one declaration.");
        }
        if self.tools.contains(&"atlas_context") {
            text.push_str(" atlas_context discovers declarations by identifiers.");
        }
        if self.tools.contains(&"atlas_symbol") {
            text.push_str(" atlas_symbol inspects a returned declaration ID.");
        }
        if self.tools.contains(&"atlas_dependencies") {
            text.push_str(" atlas_dependencies: upstream consumers, downstream dependencies; depth gives bounded hops.");
        }
        if navigation {
            text.push_str(" Selected-file byte checks do not verify graph freshness.");
        }
        if self.tools.contains(&"atlas_status") {
            text.push_str(" atlas_status checks full-index freshness.");
        }
        text.push_str(" Use native Read/Grep for known files, literals and other languages; read files directly before editing.");
        text
    }

    /// Listed definitions: enabled tools only. Compact single-root schemas use
    /// the implicit root; full schemas retain the complete argument interface.
    pub fn definitions(&self) -> Value {
        definitions(self.profile, &self.tools, self.roots.len() > 1)
    }

    /// `None` for an unknown tool (a protocol error); otherwise a tool result,
    /// with execution and argument failures reported as `isError` content.
    pub fn call(
        &self,
        name: &str,
        arguments: Value,
        register: &dyn Fn(Interrupt),
    ) -> Option<ToolOutput> {
        let response_budget = if name == "atlas_status" {
            MAX_BYTES
        } else if name == "atlas_impact" {
            arguments
                .get("max_bytes")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| (IMPACT_MIN_BYTES..=MAX_BYTES).contains(value))
                .unwrap_or(IMPACT_DEFAULT_BYTES)
        } else {
            arguments
                .get("max_bytes")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| (MIN_BYTES..=MAX_BYTES).contains(value))
                .unwrap_or(DEFAULT_BYTES)
        };
        if !self.tools.contains(&name) {
            return None;
        }
        let result = match name {
            "atlas_context" => self.context(arguments, register),
            "atlas_symbol" => self.symbol(arguments, register),
            "atlas_references" => self.references(arguments, register),
            "atlas_dependencies" => self.dependencies(arguments, register),
            "atlas_impact" => self.impact(arguments, register),
            "atlas_status" => self.status(arguments, register),
            _ => return None,
        };
        let result = result.and_then(|text| {
            if text.len() <= response_budget {
                Ok(text)
            } else {
                Err(ToolError::new(
                    "response_too_large",
                    "response exceeds the byte cap; narrow the request or reduce configured roots",
                ))
            }
        });
        Some(match result {
            Ok(text) => ToolOutput {
                text,
                is_error: false,
            },
            Err(error) => {
                let mut text = format!("error[{}]: {}\n", error.code, error.message);
                if text.len() > response_budget {
                    let suffix = "\n[error message truncated]\n";
                    let mut end = response_budget - suffix.len();
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    text.push_str(suffix);
                }
                ToolOutput {
                    text,
                    is_error: true,
                }
            }
        })
    }

    fn root(&self, requested: Option<&str>) -> std::result::Result<&Path, ToolError> {
        let listed = || {
            self.roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let Some(requested) = requested else {
            return match self.roots.as_slice() {
                [root] => Ok(root),
                _ => Err(ToolError::new(
                    "root_required",
                    format!(
                        "several roots are configured; pass `root` as one of: {}",
                        listed()
                    ),
                )),
            };
        };
        let path = Path::new(requested);
        if !path.is_absolute()
            || path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            return Err(ToolError::new(
                "root_not_allowed",
                format!("`root` must be an absolute configured root: {}", listed()),
            ));
        }
        let canonical = path.canonicalize().ok();
        self.roots
            .iter()
            .find(|root| Some(root.as_path()) == canonical.as_deref())
            .map(PathBuf::as_path)
            .ok_or_else(|| {
                ToolError::new(
                    "root_not_allowed",
                    format!(
                        "{requested} is not a configured root; allowed: {}",
                        listed()
                    ),
                )
            })
    }

    fn open(
        &self,
        requested: Option<&str>,
        register: &dyn Fn(Interrupt),
    ) -> std::result::Result<(Store, &Path), ToolError> {
        let root = self.root(requested)?;
        if root.canonicalize().ok().as_deref() != Some(root) {
            return Err(ToolError::new(
                "root_not_allowed",
                "configured root was moved or redirected; restart with its current canonical path",
            ));
        }
        validate_state_path(root)
            .map_err(|error| ToolError::from_anyhow("state_rejected", &error))?;
        let database = root.join(".code-intel/index.db");
        if !database.is_file() {
            return Err(ToolError::new(
                "index_missing",
                format!(
                    "no Atlas index for {0}; run `repogi index {0}` (offline; never executes repository code)",
                    root.display()
                ),
            ));
        }
        let store = Store::open_read(&database)
            .map_err(|error| ToolError::from_anyhow("index_unreadable", &error))?;
        register(store.interrupt_handle());
        let recorded = store
            .root()
            .map_err(|error| ToolError::from_anyhow("index_unreadable", &error))?;
        if recorded.as_deref() != root.to_str() {
            return Err(ToolError::new(
                "index_root_mismatch",
                format!(
                    "the index at {} was built for {}; move .code-intel aside and re-index",
                    database.display(),
                    recorded.as_deref().unwrap_or("an unknown root")
                ),
            ));
        }
        Ok((store, root))
    }

    fn open_semantic(
        &self,
        requested: Option<&str>,
        register: &dyn Fn(Interrupt),
    ) -> std::result::Result<(Store, &Path), ToolError> {
        let (store, root) = self.open(requested, register)?;
        if !store
            .has_semantic()
            .map_err(|error| ToolError::from_anyhow("index_unreadable", &error))?
        {
            return Err(outdated(root));
        }
        Ok((store, root))
    }

    fn navigation_path(
        &self,
        path: Option<&str>,
        root: Option<&str>,
    ) -> std::result::Result<(), ToolError> {
        if let Some(path) = path {
            relative_path(path)?;
            unlinked(self.root(root)?, path)?;
            if !code_intel_index::supported_source(Path::new(path)) {
                return Err(ToolError::new(
                    "unsupported_language",
                    format!(
                        "{path:?} is outside indexed language coverage (.ts/.tsx only); use native Read/Grep for this file; no claim is made about its declarations"
                    ),
                ));
            }
        }
        Ok(())
    }

    fn context(&self, arguments: Value, register: &dyn Fn(Interrupt)) -> ToolResult {
        let arguments: ContextArgs = parse(arguments)?;
        let max_bytes = budget(arguments.max_bytes)?;
        let select = arguments.select.unwrap_or_default();
        if select.len() > MAX_SELECT || select.iter().any(|id| id.len() > MAX_TEXT) {
            return Err(ToolError::new(
                "invalid_arguments",
                format!("select accepts at most {MAX_SELECT} IDs of at most {MAX_TEXT} bytes"),
            ));
        }
        let strategy = arguments.retrieval.unwrap_or(RetrievalArg::Optimized);
        let (store, _) = if strategy == RetrievalArg::Semantic {
            self.open_semantic(arguments.root.as_deref(), register)?
        } else {
            self.open(arguments.root.as_deref(), register)?
        };
        let format = arguments.format.unwrap_or_default().into();
        let options = Options {
            strategy: strategy.into(),
            detail: arguments.detail.unwrap_or(DetailArg::Detailed).into(),
            select,
            skip_dependencies: false,
            selection_truncated: false,
        };
        let response = code_intel_retrieval::signal::context(
            &store,
            &arguments.task,
            max_bytes,
            format,
            &options,
        )
        .map_err(|error| ToolError::from_anyhow("retrieval_failed", &error))?;
        code_intel_retrieval::render(&response, format)
            .map_err(|error| ToolError::from_anyhow("retrieval_failed", &error))
    }

    fn symbol(&self, arguments: Value, register: &dyn Fn(Interrupt)) -> ToolResult {
        let arguments: SymbolArgs = parse(arguments)?;
        let max_bytes = budget(arguments.max_bytes)?;
        self.navigation_path(arguments.path.as_deref(), arguments.root.as_deref())?;
        let selector = selector(arguments.id, arguments.name, arguments.path)?;
        let (store, _) = self.open(arguments.root.as_deref(), register)?;
        indexed_path(&store, selector.path.as_deref())?;
        let format = arguments.format.unwrap_or_default().into();
        let response = navigate::symbol(
            &store,
            &selector,
            arguments.detail.unwrap_or(DetailArg::Detailed).into(),
            max_bytes,
            format,
        )
        .map_err(|error| ToolError::from_anyhow("retrieval_failed", &error))?;
        code_intel_retrieval::render(&response, format)
            .map_err(|error| ToolError::from_anyhow("retrieval_failed", &error))
    }

    fn references(&self, arguments: Value, register: &dyn Fn(Interrupt)) -> ToolResult {
        let arguments: ReferencesArgs = parse(arguments)?;
        let max_bytes = budget(arguments.max_bytes)?;
        self.navigation_path(arguments.path.as_deref(), arguments.root.as_deref())?;
        let selector = selector(arguments.id, arguments.name, arguments.path)?;
        let limit = arguments.limit.unwrap_or(50);
        if !(1..=navigate::MAX_REFERENCES).contains(&limit) {
            return Err(ToolError::new(
                "invalid_arguments",
                format!("limit must be between 1 and {}", navigate::MAX_REFERENCES),
            ));
        }
        let (store, _) = self.open_semantic(arguments.root.as_deref(), register)?;
        indexed_path(&store, selector.path.as_deref())?;
        let options = navigate::ReferenceOptions {
            kinds: kinds(arguments.kinds, &ALL_KINDS)?,
            limit,
            include_unresolved: arguments.include_unresolved.unwrap_or(true),
            next_hop: self.tools.contains(&"atlas_dependencies"),
        };
        navigate::references(
            &store,
            &selector,
            &options,
            max_bytes,
            arguments.format.unwrap_or_default().into(),
        )
        .map(|(_, text)| text)
        .map_err(|error| ToolError::from_anyhow("retrieval_failed", &error))
    }

    fn dependencies(&self, arguments: Value, register: &dyn Fn(Interrupt)) -> ToolResult {
        let arguments: DependenciesArgs = parse(arguments)?;
        let max_bytes = budget(arguments.max_bytes)?;
        self.navigation_path(arguments.path.as_deref(), arguments.root.as_deref())?;
        let selector = selector(arguments.id, arguments.name, arguments.path)?;
        let depth = arguments.depth.unwrap_or(1);
        let max_nodes = arguments.max_nodes.unwrap_or(navigate::DEFAULT_NODES);
        if !(1..=navigate::MAX_DEPTH).contains(&depth)
            || !(1..=navigate::MAX_NODES).contains(&max_nodes)
        {
            return Err(ToolError::new(
                "invalid_arguments",
                format!(
                    "depth must be 1..={} and max_nodes 1..={}",
                    navigate::MAX_DEPTH,
                    navigate::MAX_NODES
                ),
            ));
        }
        let (store, _) = self.open_semantic(arguments.root.as_deref(), register)?;
        indexed_path(&store, selector.path.as_deref())?;
        let options = navigate::DependencyOptions {
            direction: arguments.direction.unwrap_or(Direction::Downstream),
            depth,
            kinds: kinds(arguments.kinds, &DEPENDENCY_KINDS)?,
            max_nodes,
            include_unresolved: arguments.include_unresolved.unwrap_or(false),
        };
        navigate::dependencies(
            &store,
            &selector,
            &options,
            max_bytes,
            arguments.format.unwrap_or_default().into(),
        )
        .map(|(_, text)| text)
        .map_err(|error| ToolError::from_anyhow("retrieval_failed", &error))
    }

    /// Git-aware change impact on the configured root. Argument validation and
    /// error codes only: snapshots, analysis and rendering are the CLI's
    /// (`impact::summary::respond`, or `tests::respond` with `tests: true`).
    #[allow(clippy::too_many_lines)]
    fn impact(&self, arguments: Value, _register: &dyn Fn(Interrupt)) -> ToolResult {
        use code_intel_retrieval::impact;
        if arguments.get("artifact").is_some() {
            return self.artifact_range(arguments);
        }
        let manifest = manifest_mode(&arguments)?;
        let arguments: ImpactArgs = parse(arguments)?;
        if manifest && matches!(arguments.format, Some(FormatArg::Agent)) {
            return Err(ToolError::new(
                "invalid_arguments",
                "manifest transport returns JSON; omit format or select json",
            ));
        }
        let summary = arguments.summary.unwrap_or(false);
        if summary && (arguments.detail.unwrap_or(false) || arguments.evidence_file.is_some()) {
            return Err(ToolError::new(
                "invalid_arguments",
                "summary cannot be combined with detail or evidence_file",
            ));
        }
        let tests = arguments.tests.unwrap_or(false);
        if tests && summary {
            return Err(ToolError::new(
                "invalid_arguments",
                "invalid summary:true with tests:true; omit summary for test recommendations (tests:true), or omit tests for a file summary (summary:true)",
            ));
        }
        if arguments.test_dirs.is_some() && !tests && !manifest {
            return Err(ToolError::new(
                "invalid_arguments",
                "test_dirs requires tests: true or manifest: true",
            ));
        }
        let directories = test_directories(arguments.test_dirs.unwrap_or_default())?;
        if tests && let Some(path) = &arguments.evidence_file {
            relative_path(path)?;
        }
        let max_bytes = impact_budget(arguments.max_bytes)?;
        let depth = arguments.depth.unwrap_or(2);
        if !(1..=impact::MAX_DEPTH).contains(&depth) {
            return Err(ToolError::new(
                "invalid_arguments",
                format!("depth must be between 1 and {}", impact::MAX_DEPTH),
            ));
        }
        let max_results = arguments.max_results.unwrap_or(200);
        if !(1..=impact::MAX_RESULTS).contains(&max_results) {
            return Err(ToolError::new(
                "invalid_arguments",
                format!("max_results must be between 1 and {}", impact::MAX_RESULTS),
            ));
        }
        let base = revision(arguments.base.as_deref().unwrap_or("HEAD"), "base")?;
        let head = arguments
            .head
            .as_deref()
            .map(|head| revision(head, "head"))
            .transpose()?;
        let root = self.impact_root(arguments.root.as_deref()).map_err(|e| {
            if manifest {
                private_manifest_error(&e)
            } else {
                e
            }
        })?;
        if tests || manifest {
            for path in directories.iter().chain(&arguments.evidence_file) {
                unlinked(root, path)?;
            }
        }
        let detail = arguments.detail.unwrap_or(false) || arguments.evidence_file.is_some();
        let request = impact::Request {
            root: root.to_path_buf(),
            base,
            head,
            // The cache location is fixed: `<root>/.code-intel/impact`.
            cache_dir: None,
            confined: true,
            options: impact::Options {
                depth,
                max_results,
                max_declarations: 100,
                detail,
            },
        };
        if manifest {
            return prepare_manifest(&request, directories, max_bytes);
        }
        let json = matches!(arguments.format.unwrap_or_default(), FormatArg::Json);
        if tests {
            return code_intel_retrieval::tests::respond(
                &request,
                &code_intel_retrieval::tests::Options { directories },
                arguments.evidence_file.as_deref(),
                json,
                max_bytes,
            )
            .map_err(|error| tests_error(&error));
        }
        impact::summary::respond(
            &request,
            summary,
            arguments.evidence_file.as_deref(),
            json,
            max_bytes,
        )
        .map_err(|error| impact_error(&error))
    }

    /// Retrieval has no analysis defaults, revision resolution or regeneration.
    fn artifact_range(&self, arguments: Value) -> ToolResult {
        if arguments.as_object().is_none_or(|o| {
            o.keys()
                .any(|k| !matches!(k.as_str(), "artifact" | "root" | "max_bytes"))
        }) {
            return Err(ToolError::new(
                "invalid_arguments",
                "artifact retrieval accepts only artifact, root and max_bytes",
            ));
        }
        let args: ArtifactRequest = parse(arguments)?;
        if !crate::manifest_artifact::valid_id(&args.artifact.id)
            || !(1..=super::artifacts::MAX_CHUNK).contains(&args.artifact.length)
        {
            return Err(ToolError::new(
                "invalid_arguments",
                "invalid artifact id or length (1..16000)",
            ));
        }
        let max_bytes = impact_budget(args.max_bytes)?;
        let root = self
            .impact_root(args.root.as_deref())
            .map_err(|e| private_manifest_error(&e))?;
        let cache = super::artifacts::Cache::open(root, false).map_err(artifact_error)?;
        let bytes = cache.read(&args.artifact.id).map_err(artifact_error)?;
        super::artifacts::chunk(
            &bytes,
            &args.artifact.id,
            args.artifact.offset,
            args.artifact.length,
            max_bytes,
        )
        .map_err(artifact_error)
    }

    fn impact_root(&self, requested: Option<&str>) -> std::result::Result<&Path, ToolError> {
        let root = self.root(requested)?;
        if root.canonicalize().ok().as_deref() != Some(root) {
            return Err(ToolError::new(
                "root_not_allowed",
                "configured root was moved or redirected; restart with its current canonical path",
            ));
        }
        validate_state_path(root)
            .map_err(|error| ToolError::from_anyhow("state_rejected", &error))?;
        Ok(root)
    }

    fn status(&self, arguments: Value, register: &dyn Fn(Interrupt)) -> ToolResult {
        let arguments: StatusArgs = parse(arguments)?;
        let root = self.root(arguments.root.as_deref())?;
        let server = json!({
            "name": "atlas",
            "version": env!("CARGO_PKG_VERSION"),
            "default_retrieval": "optimized",
            "budget_method": "estimated_utf8_bytes",
            "configured_roots": self.roots.iter().map(|root| root.display().to_string()).collect::<Vec<_>>(),
        });
        let (store, root) = match self.open(Some(root.to_str().unwrap_or_default()), register) {
            Ok(opened) => opened,
            Err(error) if error.code == "index_missing" => {
                return Ok(format!(
                    "{}\n",
                    json!({
                        "schema_version": code_intel_core::SCHEMA_VERSION,
                        "server": server,
                        "root": root.display().to_string(),
                        "index": {"present": false},
                        "fresh": false,
                        "action": error.message,
                    })
                ));
            }
            Err(error) => return Err(error),
        };
        let failed = |error: anyhow::Error| ToolError::from_anyhow("index_unreadable", &error);
        let status = store.status().map_err(failed)?;
        let semantic = store.has_semantic().map_err(failed)?;
        let hashes = store.hashes().map_err(failed)?;
        let (statuses, semantic_missing) = if semantic {
            (
                json!(store.semantic_status_counts().map_err(failed)?),
                store.semantic_missing().map_err(failed)?.len(),
            )
        } else {
            (Value::Null, hashes.len())
        };
        let freshness = if arguments.check_freshness.unwrap_or(true) {
            Some(freshness(root, &store, &hashes)?)
        } else {
            None
        };
        let fresh = freshness
            .as_ref()
            .map(|result| result.fresh && semantic && semantic_missing == 0);
        let last_run = status.last_run.as_ref().map(|run| {
            json!({
                "index_revision": run.index_revision,
                "files_scanned": run.files_scanned,
                "files_changed": run.files_changed,
                "files_unchanged": run.files_unchanged,
                "files_removed": run.files_removed,
                "files_skipped": run.files_skipped,
                "syntax_error_files": run.syntax_error_files,
                "diagnostics": run.diagnostics.len(),
                "diagnostic_sample": run.diagnostics.iter().take(DIAGNOSTIC_SAMPLE).collect::<Vec<_>>(),
            })
        });
        let action = match fresh {
            Some(true) => Value::Null,
            Some(false) => json!(format!(
                "index is stale or incomplete; run `repogi index {}`",
                root.display()
            )),
            None => json!("freshness not checked"),
        };
        Ok(format!(
            "{}\n",
            json!({
                "schema_version": code_intel_core::SCHEMA_VERSION,
                "server": server,
                "root": root.display().to_string(),
                "index": {
                    "present": true,
                    "path": ".code-intel/index.db",
                    "revision": status.index_revision,
                    "snapshot_digest": code_intel_index::snapshot_digest(&hashes),
                    "files": status.files,
                    "facts": status.facts,
                    "heuristic_edges": status.edges,
                    "unresolved_imports": status.unresolved_imports,
                    "syntax_error_files": status.syntax_error_files,
                    "semantic_graph": semantic,
                    "semantic_edges_by_status": statuses,
                    "semantic_missing_files": semantic_missing,
                    "last_run": last_run,
                },
                "freshness": freshness,
                "fresh": fresh,
                "action": action,
            })
        ))
    }
}

fn outdated(root: &Path) -> ToolError {
    ToolError::new(
        "index_outdated",
        format!(
            "this index predates the typed semantic graph; run `repogi index {}` to migrate it",
            root.display()
        ),
    )
}

/// A Git revision argument: a commit, branch, tag or revision expression.
/// Option-like, empty, oversized and control-character values are refused
/// before Git sees them (Git is also called with `--end-of-options`).
fn revision(text: &str, field: &str) -> std::result::Result<String, ToolError> {
    let invalid = text.is_empty()
        || text.len() > MAX_REVISION
        || text.starts_with('-')
        || text.chars().any(|c| c.is_control() || c.is_whitespace());
    if invalid {
        return Err(ToolError::new(
            "invalid_revision",
            format!(
                "`{field}` must be a Git revision such as HEAD, main, HEAD~1 or a commit id (1..={MAX_REVISION} bytes, no leading `-`, whitespace or control characters)"
            ),
        ));
    }
    Ok(text.to_owned())
}

/// Actionable codes for impact failures.
fn impact_error(error: &anyhow::Error) -> ToolError {
    let message = format!("{error:#}");
    let code = if message.contains("unknown revision") {
        "invalid_revision"
    } else if message.contains("not a Git work tree") {
        "not_a_git_repository"
    } else if message.contains("is not a Git work-tree top level")
        || message.contains("outside its Git work tree")
    {
        "unsupported_repository"
    } else if message.contains("refusing symlinked") {
        "state_rejected"
    } else {
        "impact_failed"
    };
    ToolError::new(code, message)
}

/// Test-discovery failures that have a more specific code than `impact_error`.
fn tests_error(error: &anyhow::Error) -> ToolError {
    let message = format!("{error:#}");
    if message.contains("not observed in the configured inventory") {
        ToolError::new("test_not_found", message)
    } else if message.contains("exceeds max-bytes") {
        ToolError::new("response_too_large", message)
    } else {
        impact_error(error)
    }
}

fn manifest_mode(arguments: &Value) -> std::result::Result<bool, ToolError> {
    let manifest = arguments.get("manifest") == Some(&Value::Bool(true));
    if manifest
        && arguments.as_object().is_some_and(|o| {
            ["tests", "summary", "detail", "evidence_file"]
                .iter()
                .any(|k| o.contains_key(*k))
        })
    {
        return Err(ToolError::new(
            "invalid_arguments",
            "manifest cannot be combined with tests, summary, detail or evidence_file",
        ));
    }
    Ok(manifest)
}

fn prepare_manifest(
    request: &code_intel_retrieval::impact::Request,
    directories: Vec<String>,
    max_bytes: usize,
) -> ToolResult {
    // Serialize transport producers through analysis as well as installation.
    let cache = super::artifacts::Cache::open(&request.root, true).map_err(artifact_error)?;
    let text = code_intel_retrieval::manifest::respond(
        request,
        &code_intel_retrieval::tests::Options { directories },
        code_intel_retrieval::manifest::MAX_BYTES,
    )
    .map_err(|error| private_manifest_error(&tests_error(&error)))?;
    let verified = crate::manifest_artifact::verify(text.as_bytes()).map_err(|_| {
        ToolError::new(
            "artifact_corrupt",
            "manifest construction failed envelope validation",
        )
    })?;
    let descriptor = super::artifacts::descriptor(&verified, max_bytes).map_err(artifact_error)?;
    cache.retain(&verified).map_err(artifact_error)?;
    Ok(descriptor)
}

fn artifact_error(error: super::artifacts::Error) -> ToolError {
    ToolError::new(error.code, error.to_string())
}

/// New transport errors never export engine diagnostics containing host paths.
/// Existing interactive modes retain their established errors.
fn private_manifest_error(error: &ToolError) -> ToolError {
    let message = match error.code {
        "invalid_revision" => "base or head is unavailable; provide existing Git commit objects",
        "not_a_git_repository" => "configured root is not a Git work tree",
        "unsupported_repository" => "repository layout is unsupported for confined impact analysis",
        "state_rejected" => "Atlas state path was rejected; use local nonsymlinked state",
        "root_required" | "root_not_allowed" => "select an authorized configured repository root",
        "response_too_large" => "complete manifest exceeds the 8000000-byte artifact limit",
        _ => "manifest analysis failed; no verification artifact was returned",
    };
    ToolError::new(error.code, message)
}

fn impact_budget(requested: Option<usize>) -> std::result::Result<usize, ToolError> {
    let budget = requested.unwrap_or(IMPACT_DEFAULT_BYTES);
    if !(IMPACT_MIN_BYTES..=MAX_BYTES).contains(&budget) {
        return Err(ToolError::new(
            "invalid_arguments",
            format!("max_bytes must be between {IMPACT_MIN_BYTES} and {MAX_BYTES}"),
        ));
    }
    Ok(budget)
}

/// `test_dirs`: a bounded list of literal, normalized repository-relative
/// directories (a trailing `/` is accepted). Globs are refused, as in the CLI.
fn test_directories(directories: Vec<String>) -> std::result::Result<Vec<String>, ToolError> {
    if directories.len() > MAX_TEST_DIRS {
        return Err(ToolError::new(
            "invalid_arguments",
            format!("test_dirs accepts at most {MAX_TEST_DIRS} directories"),
        ));
    }
    directories
        .into_iter()
        .map(|directory| {
            let directory = directory
                .strip_suffix('/')
                .map_or(directory.clone(), str::to_owned);
            relative_path(&directory)?;
            if directory.contains(['*', '?', '{', '}']) {
                return Err(ToolError::new(
                    "invalid_path",
                    format!(
                        "test_dirs entry {directory:?} must be a literal directory, not a glob"
                    ),
                ));
            }
            Ok(directory)
        })
        .collect()
}

/// No existing component of a path argument may be a symbolic link. Discovery
/// never follows links (it matches Git and walk paths); refusing them makes
/// that explicit. A path absent from the working tree may exist in a commit.
fn unlinked(root: &Path, path: &str) -> std::result::Result<(), ToolError> {
    let mut location = root.to_path_buf();
    for part in path.split('/') {
        location.push(part);
        match std::fs::symlink_metadata(&location) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ToolError::new(
                    "invalid_path",
                    format!(
                        "path {path:?} passes through a symbolic link; links are never followed"
                    ),
                ));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

fn parse<T: DeserializeOwned>(arguments: Value) -> std::result::Result<T, ToolError> {
    serde_json::from_value(arguments)
        .map_err(|error| ToolError::new("invalid_arguments", error.to_string()))
}

fn budget(requested: Option<usize>) -> std::result::Result<usize, ToolError> {
    let bytes = requested.unwrap_or(DEFAULT_BYTES);
    if (MIN_BYTES..=MAX_BYTES).contains(&bytes) {
        Ok(bytes)
    } else {
        Err(ToolError::new(
            "invalid_arguments",
            format!("max_bytes must be between {MIN_BYTES} and {MAX_BYTES}"),
        ))
    }
}

fn indexed_path(store: &Store, path: Option<&str>) -> std::result::Result<(), ToolError> {
    if let Some(path) = path {
        if store
            .file_hash(path)
            .map_err(|error| ToolError::from_anyhow("index_unreadable", &error))?
            .is_none()
        {
            return Err(ToolError::new(
                "path_not_indexed",
                format!(
                    "{path:?} is not in this index (possibly excluded, missing or skipped); use native Read/Grep; this is not proof that the file contains no matches"
                ),
            ));
        }
    }
    Ok(())
}

fn selector(
    id: Option<String>,
    name: Option<String>,
    path: Option<String>,
) -> std::result::Result<Selector, ToolError> {
    if id.is_none() && name.is_none() {
        return Err(ToolError::new(
            "invalid_arguments",
            format!(
                "provide `id` or `name`; `path` only filters declarations. Discover an indexed selector with atlas_context {} then retry using a returned id",
                json!({"task":path.as_deref().unwrap_or("declaration identifiers"),"detail":"overview"})
            ),
        ));
    }
    if [&id, &name]
        .into_iter()
        .flatten()
        .any(|text| text.is_empty() || text.len() > MAX_TEXT)
    {
        return Err(ToolError::new(
            "invalid_arguments",
            format!("`id` and `name` must be 1..={MAX_TEXT} bytes"),
        ));
    }
    if let Some(path) = &path {
        relative_path(path)?;
    }
    Ok(Selector { id, name, path })
}

/// Repository-relative, normalized, forward-slash paths only. Paths filter the
/// persisted index; the server never opens them, but traversal is still refused.
fn relative_path(path: &str) -> std::result::Result<(), ToolError> {
    let invalid = path.is_empty()
        || path.len() > MAX_TEXT
        || path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
        || path.as_bytes().get(1) == Some(&b':');
    if invalid {
        return Err(ToolError::new(
            "invalid_path",
            format!(
                "path {path:?} must be a normalized repository-relative path such as src/app.ts; absolute paths, `..`, `.`, backslashes and drive prefixes are rejected"
            ),
        ));
    }
    Ok(())
}

const ALL_KINDS: [Relationship; 6] = [
    Relationship::Calls,
    Relationship::References,
    Relationship::Extends,
    Relationship::Imports,
    Relationship::Exports,
    Relationship::ReExports,
];
const DEPENDENCY_KINDS: [Relationship; 3] = [
    Relationship::Calls,
    Relationship::References,
    Relationship::Extends,
];

fn kinds(
    requested: Option<Vec<Relationship>>,
    default: &[Relationship],
) -> std::result::Result<Vec<Relationship>, ToolError> {
    let mut kinds = Vec::new();
    for kind in requested.unwrap_or_else(|| default.to_vec()) {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        return Err(ToolError::new(
            "invalid_arguments",
            "kinds must not be empty",
        ));
    }
    Ok(kinds)
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DetailArg {
    Overview,
    Focused,
    Detailed,
}

impl From<DetailArg> for Detail {
    fn from(value: DetailArg) -> Self {
        match value {
            DetailArg::Overview => Self::Overview,
            DetailArg::Focused => Self::Focused,
            DetailArg::Detailed => Self::Detailed,
        }
    }
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RetrievalArg {
    Optimized,
    Semantic,
    Lexical,
    Graph,
}

impl From<RetrievalArg> for Strategy {
    fn from(value: RetrievalArg) -> Self {
        match value {
            RetrievalArg::Optimized => Self::Optimized,
            RetrievalArg::Semantic => Self::Semantic,
            RetrievalArg::Lexical => Self::Lexical,
            RetrievalArg::Graph => Self::Graph,
        }
    }
}

#[derive(Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
enum FormatArg {
    #[default]
    Agent,
    Json,
}

impl From<FormatArg> for Format {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::Agent => Self::Agent,
            FormatArg::Json => Self::Json,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextArgs {
    task: String,
    detail: Option<DetailArg>,
    retrieval: Option<RetrievalArg>,
    max_bytes: Option<usize>,
    select: Option<Vec<String>>,
    format: Option<FormatArg>,
    root: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolArgs {
    id: Option<String>,
    name: Option<String>,
    path: Option<String>,
    detail: Option<DetailArg>,
    max_bytes: Option<usize>,
    format: Option<FormatArg>,
    root: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferencesArgs {
    id: Option<String>,
    name: Option<String>,
    path: Option<String>,
    kinds: Option<Vec<Relationship>>,
    limit: Option<usize>,
    include_unresolved: Option<bool>,
    max_bytes: Option<usize>,
    format: Option<FormatArg>,
    root: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DependenciesArgs {
    id: Option<String>,
    name: Option<String>,
    path: Option<String>,
    direction: Option<Direction>,
    depth: Option<usize>,
    kinds: Option<Vec<Relationship>>,
    max_nodes: Option<usize>,
    include_unresolved: Option<bool>,
    max_bytes: Option<usize>,
    format: Option<FormatArg>,
    root: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImpactArgs {
    // Presence validated before deserialization; false keeps ordinary mode.
    #[serde(rename = "manifest")]
    _manifest: Option<bool>,
    base: Option<String>,
    head: Option<String>,
    depth: Option<usize>,
    max_results: Option<usize>,
    detail: Option<bool>,
    summary: Option<bool>,
    tests: Option<bool>,
    test_dirs: Option<Vec<String>>,
    evidence_file: Option<String>,
    max_bytes: Option<usize>,
    format: Option<FormatArg>,
    root: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRequest {
    artifact: ArtifactRange,
    root: Option<String>,
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRange {
    id: String,
    offset: usize,
    length: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArgs {
    root: Option<String>,
    check_freshness: Option<bool>,
}

/// Tool definitions. Descriptions and schemas are injected into every agent
/// session (and usually every model request), so they are kept short;
/// Omitted optional properties are still
/// accepted by the tools; the schema only advertises what the profile needs.
pub fn definitions(profile: Profile, enabled: &[&str], multi_root: bool) -> Value {
    let kit = Kit {
        full: profile == Profile::Full,
        multi_root,
    };
    Value::Array(enabled.iter().map(|name| kit.tool(name)).collect())
}

/// Shared schema fragments for one profile.
struct Kit {
    full: bool,
    multi_root: bool,
}

impl Kit {
    fn tool(&self, name: &str) -> Value {
        let (title, description, properties, required): (&str, &str, Value, &[&str]) = match name {
            "atlas_context" => (
                "Atlas: task context",
                if self.full {
                    "Ranked TypeScript/TSX declarations from the local index, with IDs and locations. Matches identifiers and name fragments; prose can miss differently named code. For broad tasks use detail=overview, then `select` IDs or atlas_symbol instead of reading whole files."
                } else {
                    "Find TypeScript/TSX declarations by identifiers or name fragments: ranked items with IDs and source. For broad tasks use detail=overview, then pass IDs in `select`."
                },
                json!({
                    "task": {"type":"string","description":"Identifiers, name fragments or keywords."},
                    "detail": Self::detail(),
                    "retrieval": self.retrieval(),
                    "select": {"type":"array","items":{"type":"string"},"maxItems":MAX_SELECT,"description":"IDs from an earlier result."},
                }),
                &["task"],
            ),
            "atlas_symbol" => (
                "Atlas: symbol source",
                if self.full {
                    "Inspect indexed declarations by returned id or declared name (not a barrel alias): source, signature or location."
                } else {
                    "Get up to 16 declarations by `id` or `name`, with source."
                },
                Self::selector(json!({"detail": Self::detail()})),
                &[],
            ),
            "atlas_references" => (
                "Atlas: references and call sites",
                "Direct users of one indexed declaration, with enclosing names and use sites. JSX is REFERENCES, not CALLS. Same-name unconfirmed sites stay separate; runtime/instance/package resolution is limited. For multiple hops use atlas_dependencies upstream.",
                Self::selector(json!({
                    "kinds": Self::kinds(),
                    "limit": {"type":"integer","minimum":1,"maximum":navigate::MAX_REFERENCES,"default":50},
                    "include_unresolved": {"type":"boolean","default":true},
                })),
                &[],
            ),
            "atlas_dependencies" => (
                "Atlas: dependency graph",
                "Multiple static dependency hops: upstream finds consumers, downstream finds dependencies. Nodes show depth and exact IDs; agent format avoids a JSON graph dump. Unconfirmed sites are opt-in; runtime/HTTP and unmocked test coverage remain unknown.",
                Self::selector(json!({
                    "direction": {"type":"string","enum":["downstream","upstream","both"],"default":"downstream"},
                    "depth": {"type":"integer","minimum":1,"maximum":navigate::MAX_DEPTH,"default":1},
                    "max_nodes": {"type":"integer","minimum":1,"maximum":navigate::MAX_NODES,"default":navigate::DEFAULT_NODES},
                    "kinds": Self::kinds(),
                    "include_unresolved": {"type":"boolean","default":false},
                })),
                &[],
            ),
            "atlas_impact" => (
                "Atlas: change impact",
                self.impact_description(),
                self.impact_properties(),
                &[],
            ),
            _ => (
                "Atlas: index status",
                "Check full navigation-index freshness, including added/removed sources and resolution settings. Other navigation checks only selected file bytes. Never re-indexes; explicit index command is required after edits.",
                json!({"check_freshness": {"type":"boolean","default":true,"description":"Hash files to detect edits since indexing."}}),
                &[],
            ),
        };
        let mut properties = self.shared(properties, name != "atlas_status");
        if name == "atlas_impact" {
            properties["manifest"] = json!({"type":"boolean","description":"Prepare a complete verification artifact descriptor; conflicts with tests/summary/detail/evidence_file. JSON; retrieve bytes with artifact."});
            properties["artifact"] = json!({"type":"object","additionalProperties":false,
                "required":["id","offset","length"],"properties":{
                    "id":{"type":"string","pattern":"^blake3:[0-9a-f]{64}$"},
                    "offset":{"type":"integer","minimum":0},
                    "length":{"type":"integer","minimum":1,"maximum":super::artifacts::MAX_CHUNK}},
                "description":"Retrieve immutable base64 bytes; only root/max_bytes may accompany it. Advance by next_offset; verify the full envelope digest."});
            properties["max_bytes"]["minimum"] = json!(IMPACT_MIN_BYTES);
            properties["max_bytes"]["default"] = json!(IMPACT_DEFAULT_BYTES);
            properties["summary"] = json!({"type":"boolean","description":"Grouped files and completeness; use first. Omit for legacy evidence; format=json for structure."});
            properties["evidence_file"] = json!({"type":"string","description":"Detailed evidence for one repo-relative file; omit summary."});
            if self.full {
                properties["tests"] = json!({"type":"boolean","default":false,"description":"Existing tests linked to the change, for verification planning; omit summary."});
                properties["test_dirs"] = json!({"type":"array","items":{"type":"string"},"maxItems":MAX_TEST_DIRS,"description":"Extra literal repo-relative test directories; needs tests or manifest."});
            } else {
                properties["tests"] =
                    json!({"type":"boolean","description":"Linked tests to consider running."});
            }
        }
        let schema = Self::input_schema(name, properties, required);
        let mut definition = json!({
            "name": name,
            "description": description,
            "inputSchema": schema,
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        });
        if self.full {
            definition["title"] = json!(title);
        }
        definition
    }

    fn input_schema(name: &str, properties: Value, required: &[&str]) -> Value {
        let mut schema = json!({"type":"object","additionalProperties":false});
        schema["properties"] = properties;
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        if matches!(
            name,
            "atlas_symbol" | "atlas_references" | "atlas_dependencies"
        ) {
            schema["anyOf"] = json!([{"required":["id"]},{"required":["name"]}]);
        }
        schema
    }

    fn impact_description(&self) -> &'static str {
        if self.full {
            "Review Git TypeScript/TSX impact: choose summary=true for files, tests=true for tests, or evidence_file for details; do not combine summary with tests/detail/evidence_file. manifest/artifact provide durable verification handoff. No tests run; complete never means safe. First analysis builds snapshots; repeats reuse them."
        } else {
            "Static change impact: summary=true for files, tests=true for test recommendations, evidence_file for evidence. manifest=true prepares a complete verification artifact descriptor; artifact retrieves immutable bytes for handoff/CI. Nothing runs; complete never means safe."
        }
    }

    fn impact_properties(&self) -> Value {
        if self.full {
            json!({
                "base": {"type":"string","default":"HEAD","description":"Git revision to compare against."},
                "head": {"type":"string","description":"Git revision; omit for the working tree."},
                "depth": {"type":"integer","minimum":1,"maximum":4,"default":2,"description":"Upstream hops; 1 = direct consumers."},
                "max_results": {"type":"integer","minimum":1,"maximum":1000,"default":200},
                "detail": {"type":"boolean","default":false,"description":"Add rules, binding chains, transitive paths and possible sites."},
            })
        } else {
            json!({
                "base": {"type":"string","default":"HEAD"},
                "head": {"type":"string"},
                "detail": {"type":"boolean"},
            })
        }
    }

    fn shared(&self, mut properties: Value, sized: bool) -> Value {
        if sized {
            properties["max_bytes"] = json!({"type":"integer","minimum":MIN_BYTES,"maximum":MAX_BYTES,"default":DEFAULT_BYTES,"description":"Cap on the whole result in UTF-8 bytes."});
            if self.full {
                properties["format"] = json!({"type":"string","enum":["agent","json"],"default":"agent","description":"agent: compact text; json: CLI JSON."});
            } else {
                properties["format"] =
                    json!({"type":"string","enum":["agent","json"],"default":"agent"});
            }
        }
        if self.full || self.multi_root {
            properties["root"] = json!({"type":"string","description":"Absolute configured root."});
        }
        properties
    }

    fn selector(mut extra: Value) -> Value {
        extra["id"] = json!({"type":"string","description":"ID from an earlier result."});
        extra["name"] = json!({"type":"string","description":"Declared name: foo or Owner.foo; aliases may differ."});
        extra["path"] = json!({"type":"string","description":"Repository-relative file filter; still supply id or name."});
        extra
    }

    fn detail() -> Value {
        json!({"type":"string","enum":["overview","focused","detailed"],"default":"detailed","description":"overview: locations; focused: signatures; detailed: source."})
    }

    fn kinds() -> Value {
        json!({"type":"array","items":{"type":"string","enum":["CALLS","REFERENCES","EXTENDS","IMPORTS","EXPORTS","RE_EXPORTS"]}})
    }

    fn retrieval(&self) -> Value {
        if self.full {
            json!({"type":"string","enum":["optimized","semantic","lexical","graph"],"default":"optimized","description":"semantic adds verified static dependencies; lexical/graph are frozen controls."})
        } else {
            json!({"type":"string","enum":["optimized","semantic"],"default":"optimized"})
        }
    }
}

/// Source freshness plus module-resolution settings: a tsconfig/jsconfig edit
/// changes bindings without changing any source file.
fn freshness(
    root: &Path,
    store: &Store,
    hashes: &std::collections::BTreeMap<String, String>,
) -> std::result::Result<code_intel_core::Freshness, ToolError> {
    let stale = |error: anyhow::Error| ToolError::from_anyhow("freshness_failed", &error);
    let mut result = code_intel_index::freshness_checked(root, hashes, FRESHNESS_SAMPLE, &|| {
        store.check_cancelled()
    })
    .map_err(stale)?;
    let current = code_intel_index::resolution_fingerprint(root).map_err(stale)?;
    if store.resolution_fingerprint().map_err(stale)?.as_deref() != Some(current.as_str()) {
        result.resolution_changed = true;
        result.fresh = false;
    }
    if store.semantic_ir_version().map_err(stale)?
        != Some(code_intel_core::semantic::SEMANTIC_IR_VERSION)
    {
        result.semantic_ir_outdated = true;
        result.fresh = false;
    }
    Ok(result)
}
