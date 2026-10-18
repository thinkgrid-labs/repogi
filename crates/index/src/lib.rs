//! Offline discovery, content-hash incremental indexing, and Tree-sitter extraction.
mod syntax;

use std::{fs, io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use code_intel_core::{Diagnostic, IndexReport, MAX_FILE_BYTES, SCHEMA_VERSION};
use code_intel_store::{Store, validate_state_path};
use ignore::WalkBuilder;

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
    let mut store = Store::open_index(&root.join(".code-intel/index.db"), &root)?;
    let revision = store.revision()?;
    let hashes = store.hashes()?;
    let mut report = IndexReport {
        schema_version: SCHEMA_VERSION,
        ..IndexReport::default()
    };
    let mut retained = Vec::new();
    let mut changed = Vec::new();
    for entry in discover(&root)? {
        let path = entry.path();
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ts" | "tsx")
        ) {
            continue;
        }
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        let relative = path
            .strip_prefix(&root)
            .context("discovered path escaped repository root")?;
        let Some(relative) = relative.to_str() else {
            skip(
                &mut report,
                &relative.to_string_lossy(),
                "non-UTF8 path is unsupported",
            );
            continue;
        };
        report.files_scanned += 1;
        let Some(source) = read_source(&root, path, relative, &mut report)? else {
            continue;
        };
        retained.push(relative.to_owned());
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        if hashes.get(relative) == Some(&hash) {
            report.files_unchanged += 1;
            continue;
        }
        let file = syntax::extract(relative, &source, hash)?;
        if file.syntax_error {
            report.diagnostics.push(Diagnostic { path:relative.to_owned(),message:"Tree-sitter syntax errors: only intact declarations retained; extraction is partial".into() });
        }
        changed.push(file);
        report.files_changed += 1;
    }
    store
        .apply(&changed, &retained, report, revision)
        .context("update .code-intel/index.db; the previous index is preserved on failure")
}

fn skip(report: &mut IndexReport, path: &str, message: &str) {
    report.files_skipped += 1;
    report.diagnostics.push(Diagnostic {
        path: path.into(),
        message: message.into(),
    });
}

fn discover(root: &Path) -> Result<Vec<ignore::DirEntry>> {
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
                    )
                )
        });
    let mut entries = Vec::new();
    for entry in walker.build() {
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

fn read_source(
    root: &Path,
    path: &Path,
    relative: &str,
    report: &mut IndexReport,
) -> Result<Option<String>> {
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
    let file = fs::File::open(&canonical)
        .with_context(|| format!("read {relative}; fix file permissions and retry"))?;
    if file.metadata()?.len() > MAX_FILE_BYTES {
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
