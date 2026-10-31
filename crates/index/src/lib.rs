//! Offline discovery, content-hash incremental indexing, and Tree-sitter extraction.
pub mod git;
mod markdown;
mod relationships;
mod semantic;
pub mod snapshot;
mod source;
mod syntax;
mod tsconfig;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use code_intel_core::{
    Diagnostic, Freshness, IndexReport, MAX_FILE_BYTES, SCHEMA_VERSION,
    semantic::SEMANTIC_IR_VERSION,
};
use code_intel_store::{Store, validate_state_path};
use ignore::WalkBuilder;

pub use syntax::{module_residue, variable_header};

pub fn index(root: &Path) -> Result<IndexReport> {
    let root = root.canonicalize().with_context(|| {
        format!(
            "open repository {}; provide an existing directory",
            root.display()
        )
    })?;
    ensure!(root.is_dir(), "index path must be a directory");
    validate_state_path(&root)?;
    fs::create_dir_all(root.join(".code-intel"))
        .context("create .code-intel; check repository permissions")?;
    index_into(&root, &root.join(".code-intel/index.db"), &root)
}

/// Index the sources under `root` into `database`. `identity` is the repository
/// root recorded in (and checked against) the database; snapshots of one
/// repository share it, so any of them can seed another incrementally.
pub fn index_into(root: &Path, database: &Path, identity: &Path) -> Result<IndexReport> {
    index_with(root, database, identity, false)
}

/// Like [`index_into`], for impact snapshots: cached per-file IR is reused only
/// when this same build wrote it, so a database seeded from (or left by) another
/// build with the same IR version is fully reparsed rather than trusted.
pub fn index_snapshot(root: &Path, database: &Path, identity: &Path) -> Result<IndexReport> {
    index_with(root, database, identity, true)
}

/// Identity of this build: the IR version and a BLAKE3 prefix of the running
/// executable. Recorded in every database it writes (`indexer_build`).
pub fn build_key() -> Result<String> {
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(key) = KEY.get() {
        return Ok(key.clone());
    }
    let executable = std::env::current_exe().context("locate repogi binary")?;
    let bytes = fs::read(&executable).context("read repogi binary")?;
    let hash = blake3::hash(&bytes).to_hex();
    Ok(KEY
        .get_or_init(|| format!("ir{SEMANTIC_IR_VERSION}-{}", &hash[..16]))
        .clone())
}

fn index_with(root: &Path, database: &Path, identity: &Path, strict: bool) -> Result<IndexReport> {
    let build = build_key()?;
    let root = root
        .canonicalize()
        .with_context(|| format!("open source directory {}", root.display()))?;
    ensure!(root.is_dir(), "index path must be a directory");
    let mut store = Store::open_index(database, identity)?;
    let revision = store.revision()?;
    let hashes = store.hashes()?;
    let semantic_missing = store.semantic_missing()?;
    // Cached IR from an older extractor lacks facts the binder now relies on
    // (JSX references since IR 2): reparse every file once rather than reuse it.
    let ir_current = store.semantic_ir_version()? == Some(SEMANTIC_IR_VERSION)
        && (!strict || store.indexer_build()?.as_deref() == Some(build.as_str()));
    let mut report = IndexReport {
        schema_version: SCHEMA_VERSION,
        ..IndexReport::default()
    };
    let started = std::time::Instant::now();
    let (resolution, config_diagnostics) = tsconfig::context(&root, &discover(&root, &|| Ok(()))?);
    let configured = started.elapsed();
    let fingerprint = resolution_digest(&resolution);
    report.diagnostics.extend(config_diagnostics);
    let mut retained = Vec::new();
    let mut changed = Vec::new();
    eligible_sources(
        &root,
        &mut report,
        &|| Ok(()),
        |report, relative, source| {
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            if ir_current
                && hashes.get(&relative) == Some(&hash)
                && !semantic_missing.contains(&relative)
            {
                retained.push(relative);
                report.files_unchanged += 1;
                return Ok(());
            }
            let file = syntax::extract(&relative, &source, hash)?;
            if file.syntax_error {
                report.diagnostics.push(Diagnostic { path:relative.clone(),message:"Tree-sitter syntax errors: only intact declarations retained; extraction is partial".into() });
            } else if !file.semantic.complete {
                report.diagnostics.push(Diagnostic { path: relative.clone(), message: "semantic analysis skipped: AST nesting exceeds 256; lexical declarations retained".into() });
            }
            retained.push(relative);
            changed.push(file);
            report.files_changed += 1;
            Ok(())
        },
    )?;
    let mut documents = Vec::new();
    let mut retained_documents = Vec::new();
    // Impact snapshot caches remain code-only. The explicit project index owns
    // the knowledge inventory and commits it with the code inventory below.
    if !strict {
        let hashes = store.document_hashes()?;
        let documents_current = store.knowledge_available()? && store.relationships_available()?;
        eligible_documents(&root, &mut report, |report, relative, source| {
            let hash = content_hash(&source);
            if documents_current && hashes.get(&relative) == Some(&hash) {
                report.documents.unchanged += 1;
                retained_documents.push(relative);
            } else {
                match markdown::extract(&relative, source, hash) {
                    Ok(document) => {
                        retained_documents.push(relative);
                        documents.push(document);
                        report.documents.changed += 1;
                    }
                    Err(error) => {
                        report.documents.skipped += 1;
                        report.diagnostics.push(Diagnostic {
                            path: relative,
                            message: error.to_string(),
                        });
                    }
                }
            }
            Ok(())
        })?;
    }
    let parsed = started.elapsed();
    code_intel_store::profile(&[
        ("discover_and_configs", configured),
        ("read_hash_parse", parsed.saturating_sub(configured)),
    ]);
    let applied = store
        .apply_index(
            (&changed, &retained),
            (!strict).then_some(code_intel_store::DocumentUpdate {
                changed: &documents,
                retained: &retained_documents,
            }),
            report,
            revision,
            &resolution,
            (&fingerprint, Some(&build)),
        )
        .context("update .code-intel/index.db; the previous index is preserved on failure");
    code_intel_store::profile(&[("apply_total", started.elapsed().saturating_sub(parsed))]);
    applied
}

/// Compare the files `index` would accept now against indexed content hashes.
/// Reads and hashes eligible sources only; never parses, writes or executes them.
pub fn freshness(
    root: &Path,
    indexed: &BTreeMap<String, String>,
    sample_limit: usize,
) -> Result<Freshness> {
    freshness_checked(root, indexed, sample_limit, &|| Ok(()))
}

/// Cooperative cancellation for long filesystem scans, shared with MCP status.
pub fn freshness_checked(
    root: &Path,
    indexed: &BTreeMap<String, String>,
    sample_limit: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<Freshness> {
    check()?;
    let root = root
        .canonicalize()
        .with_context(|| format!("open repository {}", root.display()))?;
    let mut report = IndexReport::default();
    let mut result = Freshness::default();
    let mut differing = BTreeSet::new();
    let mut seen = BTreeSet::new();
    eligible_sources(&root, &mut report, check, |_, relative, source| {
        result.files_checked += 1;
        match indexed.get(&relative) {
            None => result.added += 1,
            Some(hash) if *hash != blake3::hash(source.as_bytes()).to_hex().as_str() => {
                result.changed += 1;
            }
            Some(_) => {
                seen.insert(relative);
                return Ok(());
            }
        }
        seen.insert(relative.clone());
        differing.insert(relative);
        Ok(())
    })?;
    result.skipped = report.files_skipped;
    for path in indexed.keys().filter(|path| !seen.contains(*path)) {
        result.removed += 1;
        differing.insert(path.clone());
    }
    result.fresh = differing.is_empty();
    result.sample = differing.into_iter().take(sample_limit).collect();
    Ok(result)
}

/// Identity of the module-resolution settings `index` would use now (configs,
/// their effective `baseUrl`/`paths`, and module files a resolver could pick).
/// Reads configuration files only; never parses or executes sources.
pub fn resolution_fingerprint(root: &Path) -> Result<String> {
    let root = root
        .canonicalize()
        .with_context(|| format!("open repository {}", root.display()))?;
    let (resolution, _) = tsconfig::context(&root, &discover(&root, &|| Ok(()))?);
    Ok(resolution_digest(&resolution))
}

fn resolution_digest(resolution: &code_intel_core::semantic::ResolutionContext) -> String {
    let encoded = serde_json::to_vec(resolution).expect("resolution context serializes");
    blake3::hash(&encoded).to_hex().to_string()
}

/// The content hash `index` records for a source file's text.
#[must_use]
pub fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// The exact extension rule shared by indexing and navigation coverage.
#[must_use]
pub fn supported_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("ts" | "tsx")
    )
}

#[must_use]
pub fn supported_document(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

fn eligible_documents(
    root: &Path,
    report: &mut IndexReport,
    mut visit: impl FnMut(&mut IndexReport, String, String) -> Result<()>,
) -> Result<()> {
    for entry in discover(root, &|| Ok(()))? {
        let path = entry.path();
        if !supported_document(path) || entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        report.documents.scanned += 1;
        let relative = path
            .strip_prefix(root)
            .context("document escaped repository root")?;
        let Some(relative) = relative.to_str() else {
            report.documents.skipped += 1;
            report.diagnostics.push(Diagnostic {
                path: relative.to_string_lossy().into(),
                message: "non-UTF8 document path is unsupported".into(),
            });
            continue;
        };
        // Reuse precisely the code reader's path, size, text and generated-file
        // checks, while keeping its inventory counters separate.
        let mut reading = IndexReport::default();
        let source = read_source(root, path, relative, &mut reading)?;
        report.documents.skipped += reading.files_skipped;
        report.diagnostics.extend(reading.diagnostics);
        if let Some(source) = source {
            visit(report, relative.into(), source)?;
        }
    }
    Ok(())
}

/// Read one selected indexed source without discovery, parsing or writes.
/// Refuse links in every component, nonregular/oversized sources and escapes.
/// `None` means the selected source is missing; other unreadable states fail.
pub fn selected_source_hash(root: &Path, relative: &str) -> Result<Option<String>> {
    use std::path::Component;
    let path = Path::new(relative);
    ensure!(
        !relative.is_empty()
            && !relative.contains('\\')
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "invalid selected source path"
    );
    ensure!(
        root.canonicalize()?.as_path() == root,
        "source root was redirected"
    );
    let mut selected = root.to_path_buf();
    for component in path.components() {
        selected.push(component);
        match fs::symlink_metadata(&selected) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "selected source passes through a symbolic link"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    let canonical = selected.canonicalize()?;
    ensure!(canonical.starts_with(root), "selected source escaped root");
    let selected_metadata = fs::symlink_metadata(&selected)?;
    ensure!(
        selected_metadata.is_file() && selected_metadata.len() <= MAX_FILE_BYTES,
        "selected source is nonregular or exceeds 1 MiB"
    );
    let file = source::open(root, relative)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_FILE_BYTES,
        "selected source is nonregular or exceeds 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= MAX_FILE_BYTES,
        "selected source exceeds 1 MiB"
    );
    ensure!(
        root.canonicalize()?.as_path() == root && selected.canonicalize()? == canonical,
        "selected source moved during read"
    );
    Ok(Some(blake3::hash(&bytes).to_hex().to_string()))
}

/// Order-independent identity of an indexed snapshot: BLAKE3 over sorted
/// `(path, content hash)` pairs. Equal digests mean equal indexed source bytes.
#[must_use]
pub fn snapshot_digest(hashes: &BTreeMap<String, String>) -> String {
    let mut digest = blake3::Hasher::new();
    for (path, hash) in hashes {
        digest.update(path.as_bytes());
        digest.update(b"\0");
        digest.update(hash.as_bytes());
        digest.update(b"\n");
    }
    digest.finalize().to_hex().to_string()
}

/// Discovery, exclusion and source eligibility shared by indexing and freshness checks.
fn eligible_sources(
    root: &Path,
    report: &mut IndexReport,
    check: &dyn Fn() -> Result<()>,
    mut visit: impl FnMut(&mut IndexReport, String, String) -> Result<()>,
) -> Result<()> {
    for entry in discover(root, check)? {
        check()?;
        let path = entry.path();
        if !supported_source(path) {
            continue;
        }
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .context("discovered path escaped repository root")?;
        let Some(relative) = relative.to_str() else {
            skip(
                report,
                &relative.to_string_lossy(),
                "non-UTF8 path is unsupported",
            );
            continue;
        };
        report.files_scanned += 1;
        if let Some(source) = read_source(root, path, relative, report)? {
            visit(report, relative.to_owned(), source)?;
        }
    }
    Ok(())
}

fn skip(report: &mut IndexReport, path: &str, message: &str) {
    report.files_skipped += 1;
    report.diagnostics.push(Diagnostic {
        path: path.into(),
        message: message.into(),
    });
}

fn discover(root: &Path, check: &dyn Fn() -> Result<()>) -> Result<Vec<ignore::DirEntry>> {
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .follow_links(false)
        .parents(false)
        .require_git(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_str(),
                    Some(
                        "node_modules"
                            | ".git"
                            | ".code-intel"
                            | "dist"
                            | "build"
                            | "coverage"
                            | ".next"
                            | "generated"
                            | "target"
                    )
                )
        });
    let mut entries = Vec::new();
    for entry in walker.build() {
        check()?;
        let entry = entry.context(
            "repository traversal failed; previous index was not updated; fix access and retry",
        )?;
        if let Some(error) = entry.error() {
            anyhow::bail!(
                "read repository ignore rules: {error}; fix ignore rules and retry; previous index was not updated"
            );
        }
        entries.push(entry);
    }
    entries.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(entries)
}

/// Inventory paths using the indexer's ignore/exclusion and no-follow policy.
/// Does not read file contents or descend into excluded directories.
pub fn inventory_paths(root: &Path) -> Result<Vec<String>> {
    Ok(discover(root, &|| Ok(()))?
        .iter()
        .filter(|entry| !entry.file_type().is_some_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.path().strip_prefix(root).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect())
}

fn read_source(
    root: &Path,
    path: &Path,
    relative: &str,
    report: &mut IndexReport,
) -> Result<Option<String>> {
    if relative.chars().any(char::is_control) {
        skip(
            report,
            relative,
            "control characters in source paths are unsupported",
        );
        return Ok(None);
    }
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        skip(report, relative, "symlink or non-regular source skipped");
        return Ok(None);
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("inspect {relative}; previous index was not updated"))?;
    if !canonical.starts_with(root) || fs::symlink_metadata(path)?.file_type().is_symlink() {
        skip(
            report,
            relative,
            "symlink or path outside repository skipped",
        );
        return Ok(None);
    }
    let file = source::open(root, relative)
        .with_context(|| format!("read {relative}; fix file permissions and retry"))?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_FILE_BYTES {
        skip(report, relative, "source exceeds 1 MiB limit");
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {relative}; previous index was not updated"))?;
    if u64::try_from(bytes.len())? > MAX_FILE_BYTES {
        skip(report, relative, "source exceeds 1 MiB limit");
        return Ok(None);
    }
    let Ok(source) = String::from_utf8(bytes) else {
        skip(report, relative, "non-UTF8/binary source skipped");
        return Ok(None);
    };
    if source
        .as_bytes()
        .iter()
        .any(|byte| *byte == 0 || (*byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t')))
    {
        skip(report, relative, "binary source skipped");
        return Ok(None);
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if name.ends_with(".min.ts")
        || name.ends_with(".generated.ts")
        || name.ends_with(".generated.tsx")
        || source
            .lines()
            .take(5)
            .any(|line| line.trim_start().starts_with("// @generated"))
    {
        skip(report, relative, "generated source skipped");
        return Ok(None);
    }
    Ok(Some(source))
}
