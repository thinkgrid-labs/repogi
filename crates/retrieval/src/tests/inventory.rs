use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path},
};

use anyhow::{Result, ensure};
use code_intel_index::{git::Git, inventory_paths, snapshot::relevant};
use serde::Serialize;

use crate::impact::Side;

#[derive(Clone, Debug, Default, Serialize)]
pub struct TestFile {
    pub path: String,
    /// Per-snapshot classification; absence means not observed, never "deleted".
    pub snapshots: BTreeMap<String, String>,
    pub reasons: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Configuration {
    pub path: String,
    pub snapshot: String,
    pub hints: Vec<String>,
    pub interpreted: bool,
    pub digest: String,
}

pub struct Catalog {
    pub files: BTreeMap<String, TestFile>,
    pub configurations: Vec<Configuration>,
    pub limitations: BTreeSet<String>,
    pub digest: String,
    pub limited: bool,
}

/// Deliberately no glob/config evaluation. These are literal relative directories.
pub fn validate_directories(directories: &[String]) -> Result<()> {
    for directory in directories {
        ensure!(
            safe_relative(directory) && !directory.contains(['*', '?', '{', '}']),
            "test-dir must be a literal repository-relative directory"
        );
    }
    Ok(())
}

pub(super) fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn named_test(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    stem.rsplit_once('.')
        .is_some_and(|(_, suffix)| matches!(suffix, "test" | "spec"))
}

fn in_directory(path: &str, directory: &str) -> bool {
    path.strip_prefix(directory.trim_end_matches('/'))
        .is_some_and(|tail| tail.starts_with('/'))
}

fn configuration(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name == "package.json"
        || ((name.starts_with("vitest")
            || name.starts_with("jest")
            || name.starts_with("playwright"))
            && name.contains(".config."))
}

/// Reads data only. Rejects symlinked parents, traversal and files over 1 MiB.
fn data(root: &Path, path: &str) -> Option<Vec<u8>> {
    if !safe_relative(path) {
        return None;
    }
    let mut location = root.to_path_buf();
    for part in Path::new(path).components() {
        location.push(part);
        if fs::symlink_metadata(&location)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
    }
    if !location
        .canonicalize()
        .ok()?
        .starts_with(root.canonicalize().ok()?)
    {
        return None;
    }
    let file = fs::File::open(location).ok()?;
    if !file.metadata().ok()?.is_file() || file.metadata().ok()?.len() > 1_048_576 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= 1_048_576).then_some(bytes)
}

fn configs(
    source: &Path,
    paths: &BTreeSet<String>,
    visible: &BTreeSet<String>,
    committed: Option<&BTreeMap<String, Vec<u8>>>,
    label: &str,
    directories: &mut Vec<String>,
) -> Vec<Configuration> {
    paths.iter().filter(|path| configuration(path)).map(|path| {
        let bytes = if visible.contains(path) && relevant(path) {
            committed.map_or_else(|| data(source, path), |blobs| blobs.get(path).cloned())
        } else { None };
        let mut info = Configuration { path: path.clone(), snapshot: label.into(), hints: Vec::new(),
            interpreted: false, digest: code_intel_index::content_hash(&String::from_utf8_lossy(bytes.as_deref().unwrap_or_default())) };
        let Some(bytes) = bytes else { info.hints.push("unreadable, excluded or unsafe configuration".into()); return info; };
        if !path.ends_with("package.json") {
            info.hints.push("runner configuration recorded as data; runner include/exclude and executable settings are not evaluated".into());
            return info;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            info.hints.push("invalid manifest JSON".into()); return info;
        };
        info.interpreted = true;
        let parent = Path::new(path).parent().unwrap_or(Path::new(""));
        if let Some(dir) = value.pointer("/directories/test").and_then(serde_json::Value::as_str) {
            let dir = dir.strip_prefix("./").unwrap_or(dir);
            let relative = parent.join(dir).to_string_lossy().into_owned();
            if safe_relative(&relative) && !relative.contains(['*', '?', '{', '}']) {
                directories.push(relative.clone());
                info.hints.push(format!("manifest directories.test={relative}"));
            } else { info.interpreted = false; info.hints.push("unsupported directories.test value".into()); }
        }
        for runner in ["jest", "vitest", "@playwright/test"] {
            if ["dependencies", "devDependencies"].iter().any(|key| value.get(key).and_then(|d| d.get(runner)).is_some()) {
                info.hints.push(format!("declares {runner}; runner membership unverified"));
            }
        }
        if value.get("jest").is_some() {
            info.interpreted = false;
            info.hints.push("embedded Jest configuration not evaluated".into());
        }
        // Script bodies may execute anything. Only record their presence.
        if value.get("scripts").and_then(|s| s.get("test")).is_some() {
            info.hints.push("test script present; not executed or interpreted".into());
        }
        info
    }).collect()
}

fn committed_configs(
    git: &Git,
    entries: &BTreeMap<String, code_intel_index::git::Entry>,
    visible: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let wanted: Vec<_> = entries
        .iter()
        .filter(|(path, entry)| {
            configuration(path)
                && visible.contains(*path)
                && relevant(path)
                && entry.mode.starts_with("100")
        })
        .collect();
    let oids: Vec<_> = wanted.iter().map(|(_, entry)| entry.oid.clone()).collect();
    Ok(wanted
        .into_iter()
        .zip(git.blobs(&oids)?)
        .filter_map(|((path, _), bytes)| (bytes.len() <= 1_048_576).then(|| (path.clone(), bytes)))
        .collect::<BTreeMap<_, _>>())
}

fn classify(
    path: &str,
    indexed: &BTreeMap<String, String>,
    errors: &BTreeSet<String>,
    missing: &BTreeSet<String>,
    directories: &[String],
    label: &str,
) -> Option<TestFile> {
    let named = named_test(path);
    let folder = path.split('/').any(|part| part == "__tests__")
        || directories.iter().any(|dir| in_directory(path, dir));
    if !named && !folder {
        return None;
    }
    let supported = matches!(
        Path::new(path).extension().and_then(|ext| ext.to_str()),
        Some("ts" | "tsx")
    );
    let class = if !relevant(path) || !supported {
        "unsupported_or_excluded"
    } else if !indexed.contains_key(path) || errors.contains(path) || missing.contains(path) {
        "recognized_unindexed"
    } else if named {
        "supported_indexed"
    } else {
        "possible_convention"
    };
    let mut reasons = BTreeSet::new();
    reasons.insert(
        if named {
            "test/spec filename convention"
        } else {
            "test-directory convention; runner membership unverified"
        }
        .into(),
    );
    if errors.contains(path) {
        reasons.insert("syntax errors; semantic coverage unknown".into());
    }
    if missing.contains(path) {
        reasons.insert("semantic module unavailable".into());
    }
    if !indexed.contains_key(path) {
        reasons.insert("not semantically indexed in this snapshot".into());
    }
    if !supported {
        reasons.insert("unsupported test language/extension".into());
    }
    Some(TestFile {
        path: path.into(),
        snapshots: BTreeMap::from([(label.into(), class.into())]),
        reasons,
    })
}

pub fn collect(git: &Git, side: &Side<'_>, label: &str, configured: &[String]) -> Result<Catalog> {
    let indexed = side.store.hashes()?;
    let errors: BTreeSet<_> = side.store.syntax_error_files()?.into_iter().collect();
    let missing = side.store.semantic_missing()?;
    let visible: BTreeSet<_> = inventory_paths(side.source)?.into_iter().collect();
    let entries = side
        .info
        .tree
        .as_ref()
        .map(|tree| git.ls_tree(tree))
        .transpose()?;
    let committed = entries
        .as_ref()
        .map(|entries| committed_configs(git, entries, &visible))
        .transpose()?;
    let mut paths: BTreeSet<String> = if let Some(entries) = entries {
        entries.into_keys().collect()
    } else {
        let mut paths = visible.clone();
        paths.extend(git.index_entries()?.into_keys().filter(|path| {
            safe_relative(path) && fs::symlink_metadata(side.source.join(path)).is_ok()
        }));
        paths
    };
    paths.extend(indexed.keys().cloned());
    let mut directories = configured.to_vec();
    let configurations = configs(
        side.source,
        &paths,
        &visible,
        committed.as_ref(),
        label,
        &mut directories,
    );
    let files: BTreeMap<_, _> = paths
        .iter()
        .filter(|path| safe_relative(path))
        .filter_map(|path| classify(path, &indexed, &errors, &missing, &directories, label))
        .map(|file| (file.path.clone(), file))
        .collect();
    let unsafe_paths = paths.iter().filter(|path| !safe_relative(path)).count();
    let mut limitations = BTreeSet::from([
        "inventory follows .gitignore and index exclusions; ignored untracked files and excluded directory contents are not exhaustively inventoried".into(),
        "filename/directory conventions identify probable tests, not runner membership or behavioral coverage".into(),
        "dynamic imports, runtime dispatch, generated code, unsupported languages and external packages have unknown verification coverage".into(),
    ]);
    if unsafe_paths > 0 {
        limitations.insert(format!(
            "{label}: {unsafe_paths} unsafe inventory paths not listed"
        ));
    }
    let digest = code_intel_index::content_hash(&serde_json::to_string(&(
        &files,
        &configurations,
        unsafe_paths,
    ))?);
    Ok(Catalog {
        files,
        configurations,
        limitations,
        digest,
        limited: unsafe_paths > 0,
    })
}

pub fn merge(mut base: Catalog, head: Catalog) -> Catalog {
    for (path, file) in head.files {
        let entry = base.files.entry(path).or_default();
        entry.path = file.path;
        entry.snapshots.extend(file.snapshots);
        entry.reasons.extend(file.reasons);
    }
    base.configurations.extend(head.configurations);
    base.limitations.extend(head.limitations);
    base.limited |= head.limited;
    base
}
