//! Indexed snapshots of Git commits and of the working tree, for change impact.
//!
//! A commit snapshot materialises the commit's tree (raw blobs, only files the
//! indexer can read or that affect module resolution) and indexes it into its
//! own database under the impact cache. The working-tree snapshot is the user's
//! index when it is provably fresh (opened read-only by the caller), otherwise
//! a cache database indexed incrementally from the repository. New databases are
//! seeded with a consistent copy of an existing snapshot, so only changed files
//! are reparsed. The user's `.code-intel/index.db` is never written.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

use anyhow::{Context, Result, bail, ensure};
use code_intel_core::{Diagnostic, IndexReport, semantic::SEMANTIC_IR_VERSION};
use code_intel_store::{Store, copy_database};
use serde::Serialize;

use crate::git::{Entry, Git, Revision};

/// Directory names the indexer never descends into (see `discover`).
const EXCLUDED: [&str; 9] = [
    "node_modules",
    ".git",
    ".code-intel",
    "dist",
    "build",
    "coverage",
    ".next",
    "generated",
    "target",
];
const MODULE_EXTENSIONS: [&str; 9] = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "json"];
/// Commit snapshots kept in the cache (most recently used first).
const KEEP_TREES: usize = 4;

/// Identity and provenance of one analysed state.
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotInfo {
    /// `commit` or `worktree`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// BLAKE3 over the indexed (path, content hash) pairs.
    pub digest: String,
    /// `cache` (reused), `user_index` (fresh, read-only), `built` (indexed now).
    pub index: String,
    /// Files parsed while building; 0 when reused.
    pub files_reparsed: usize,
    /// Database seeded from another snapshot before incremental indexing.
    pub seeded: bool,
    pub seconds: f64,
    /// Indexing and materialisation diagnostics (reported by consumers in
    /// summarised form, e.g. impact coverage; not serialised here).
    #[serde(skip)]
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub info: SnapshotInfo,
    /// Directory holding this state's source text.
    pub source: PathBuf,
    pub database: PathBuf,
}

/// The impact cache: `<dir>/trees/<tree>-<key>/{src,index.db,snapshot.json}`
/// and `<dir>/worktree/index.db`.
pub struct Cache {
    dir: PathBuf,
    identity: PathBuf,
    key: String,
}

/// Written last, after the database is complete; a directory without a valid
/// marker is never used or evicted as a snapshot.
#[derive(Serialize, serde::Deserialize, PartialEq, Eq)]
struct Marker {
    tree: String,
    key: String,
    digest: String,
    files: usize,
    edges: usize,
}

/// Interrupted builds (`.tmp-*`) older than this are removed.
const STALE_TEMPORARY: std::time::Duration = std::time::Duration::from_secs(3600);

impl Cache {
    /// `identity` is the canonical repository root; `dir` defaults to
    /// `<identity>/.code-intel/impact`.
    pub fn new(identity: &Path, dir: Option<&Path>) -> Result<Self> {
        let dir = if let Some(dir) = dir {
            dir.to_path_buf()
        } else {
            code_intel_store::validate_state_path(identity)?;
            identity.join(".code-intel/impact")
        };
        for ancestor in [
            dir.as_path(),
            dir.join("trees").as_path(),
            dir.join("worktree").as_path(),
        ] {
            if fs::symlink_metadata(ancestor).is_ok_and(|meta| meta.file_type().is_symlink()) {
                bail!("refusing symlinked impact cache {}", ancestor.display());
            }
        }
        fs::create_dir_all(dir.join("trees")).context("create impact cache")?;
        fs::create_dir_all(dir.join("worktree")).context("create impact cache")?;
        let cache = Self {
            dir: dir.canonicalize()?,
            identity: identity.to_path_buf(),
            key: crate::build_key()?,
        };
        cache.evict()?;
        Ok(cache)
    }

    /// The indexed snapshot of a commit, from cache or built (seeded from `seed`).
    pub fn commit(&self, git: &Git, revision: &Revision, seed: Option<&Path>) -> Result<Snapshot> {
        let started = Instant::now();
        let name = format!("{}-{}", revision.tree, self.key);
        let final_dir = self.dir.join("trees").join(&name);
        if let Some(snapshot) = self.cached(&final_dir, revision)? {
            touch(&final_dir.join("snapshot.json"));
            return Ok(snapshot);
        }
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let temporary = self
            .dir
            .join("trees")
            .join(format!(".tmp-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(temporary.join("src"))?;
        let built = (|| -> Result<(IndexReport, Vec<Diagnostic>, bool)> {
            let diagnostics = materialize(git, &revision.tree, &temporary.join("src"))?;
            let database = temporary.join("index.db");
            let seeded = match seed {
                Some(seed) if self.seedable(seed) => copy_database(seed, &database).is_ok(),
                _ => false,
            };
            let report = crate::index_snapshot(&temporary.join("src"), &database, &self.identity)?;
            Ok((report, diagnostics, seeded))
        })();
        let (report, mut diagnostics, seeded) = match built {
            Ok(built) => built,
            Err(error) => {
                remove_own(&temporary);
                return Err(error);
            }
        };
        let (digest, files, edges) = shape(&temporary.join("index.db"))?;
        let marker = Marker {
            tree: revision.tree.clone(),
            key: self.key.clone(),
            digest: digest.clone(),
            files,
            edges,
        };
        fs::write(
            temporary.join("snapshot.json"),
            serde_json::to_vec(&marker)?,
        )?;
        if final_dir.exists() {
            if self.cached(&final_dir, revision)?.is_some() {
                // Another run installed a valid copy of this snapshot meanwhile.
                remove_own(&temporary);
            } else {
                // An invalid entry (tampered, corrupt, half removed): replace it.
                remove_own(&final_dir);
            }
        }
        if temporary.exists() && fs::rename(&temporary, &final_dir).is_err() {
            remove_own(&temporary);
            ensure!(
                self.cached(&final_dir, revision)?.is_some(),
                "could not install the snapshot of {}; retry",
                revision.spec
            );
        }
        self.evict()?;
        diagnostics.extend(report.diagnostics.iter().cloned());
        Ok(Snapshot {
            info: SnapshotInfo {
                kind: "commit".into(),
                spec: Some(revision.spec.clone()),
                commit: Some(revision.commit.clone()),
                tree: Some(revision.tree.clone()),
                digest,
                index: "built".into(),
                files_reparsed: report.files_changed,
                seeded,
                seconds: started.elapsed().as_secs_f64(),
                diagnostics,
            },
            source: final_dir.join("src"),
            database: final_dir.join("index.db"),
        })
    }

    fn cached(&self, dir: &Path, revision: &Revision) -> Result<Option<Snapshot>> {
        let Ok(bytes) = fs::read(dir.join("snapshot.json")) else {
            return Ok(None);
        };
        let Ok(marker) = serde_json::from_slice::<Marker>(&bytes) else {
            return Ok(None);
        };
        if marker.tree != revision.tree || marker.key != self.key || !dir.join("index.db").is_file()
        {
            return Ok(None);
        }
        // Validate identity: the database must still hold the recorded snapshot,
        // written by this build. Anything unreadable is rebuilt, never trusted.
        let database = dir.join("index.db");
        let Ok((digest, files, edges)) = shape(&database) else {
            return Ok(None);
        };
        let build = Store::open_read(&database).and_then(|store| store.indexer_build());
        if digest != marker.digest
            || files != marker.files
            || edges != marker.edges
            || build.ok().flatten().as_deref() != Some(self.key.as_str())
        {
            return Ok(None);
        }
        let diagnostics = last_diagnostics(&dir.join("index.db"))?;
        Ok(Some(Snapshot {
            info: SnapshotInfo {
                kind: "commit".into(),
                spec: Some(revision.spec.clone()),
                commit: Some(revision.commit.clone()),
                tree: Some(revision.tree.clone()),
                digest: marker.digest,
                index: "cache".into(),
                files_reparsed: 0,
                seeded: false,
                seconds: 0.0,
                diagnostics,
            },
            source: dir.join("src"),
            database: dir.join("index.db"),
        }))
    }

    /// The working tree: the user's index if provably fresh, else the cache
    /// database brought up to date incrementally (seeded from the user's index).
    pub fn worktree(&self, root: &Path) -> Result<Snapshot> {
        let started = Instant::now();
        let user = root.join(".code-intel/index.db");
        if user.is_file() && fresh(root, &user, &self.key)? {
            return Ok(Snapshot {
                info: SnapshotInfo {
                    kind: "worktree".into(),
                    spec: None,
                    commit: None,
                    tree: None,
                    digest: digest_of(&user)?,
                    index: "user_index".into(),
                    files_reparsed: 0,
                    seeded: false,
                    seconds: started.elapsed().as_secs_f64(),
                    diagnostics: last_diagnostics(&user)?,
                },
                source: root.to_path_buf(),
                database: user,
            });
        }
        let database = self.dir.join("worktree/index.db");
        let mut seeded = false;
        if !database.exists() && self.seedable(&user) {
            seeded = copy_database(&user, &database).is_ok();
        }
        // Concurrent runs share this database; a run that loses the revision
        // check (`retry`) re-indexes, which is then a no-op if nothing changed.
        let mut attempt = 0;
        let report = loop {
            match crate::index_snapshot(root, &database, &self.identity) {
                Err(error) if attempt < 3 && format!("{error:#}").contains("retry") => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50 * attempt));
                }
                result => break result?,
            }
        };
        Ok(Snapshot {
            info: SnapshotInfo {
                kind: "worktree".into(),
                spec: None,
                commit: None,
                tree: None,
                digest: digest_of(&database)?,
                index: "built".into(),
                files_reparsed: report.files_changed,
                seeded,
                seconds: started.elapsed().as_secs_f64(),
                diagnostics: report.diagnostics,
            },
            source: root.to_path_buf(),
            database,
        })
    }

    /// A database another snapshot may be seeded from: readable, current IR,
    /// recorded for this repository root. (A different build's database is
    /// still a valid seed: snapshot indexing reparses it in full.)
    fn seedable(&self, seed: &Path) -> bool {
        seed.is_file()
            && Store::open_read(seed).is_ok_and(|store| {
                store.semantic_ir_version().ok().flatten() == Some(SEMANTIC_IR_VERSION)
                    && store.root().ok().flatten().as_deref() == self.identity.to_str()
            })
    }

    /// Keep the most recently used commit snapshots and remove interrupted
    /// builds. Only directories this cache created are touched: snapshots carry
    /// a valid marker; temporaries are named `.tmp-<tree>-ir<IR>-…`.
    fn evict(&self) -> Result<()> {
        let mut entries: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in fs::read_dir(self.dir.join("trees"))? {
            let entry = entry?;
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(rest) = name.strip_prefix(".tmp-") {
                let ours = rest.split('-').next().is_some_and(is_hex_oid)
                    && rest.contains(&format!("-ir{SEMANTIC_IR_VERSION}-"));
                let old = entry
                    .metadata()?
                    .modified()?
                    .elapsed()
                    .is_ok_and(|age| age > STALE_TEMPORARY);
                if ours && old {
                    remove_own(&path);
                }
                continue;
            }
            let marked = fs::read(path.join("snapshot.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
                .is_some_and(|marker| name.starts_with(&marker.tree));
            if !marked {
                continue;
            }
            let used = fs::metadata(path.join("snapshot.json"))?.modified()?;
            entries.push((used, path));
        }
        entries.sort_by(|a, b| b.cmp(a));
        for (_, path) in entries.into_iter().skip(KEEP_TREES) {
            remove_own(&path);
        }
        Ok(())
    }
}

/// True when `database` provably describes the files `index` would read now:
/// same extractor IR, same resolution settings, same content hashes.
fn fresh(root: &Path, database: &Path, build: &str) -> Result<bool> {
    let Ok(store) = Store::open_read(database) else {
        return Ok(false);
    };
    if store.semantic_ir_version()? != Some(SEMANTIC_IR_VERSION)
        || !store.has_semantic()?
        || store.indexer_build()?.as_deref() != Some(build)
        || store.root()?.as_deref() != root.to_str()
    {
        return Ok(false);
    }
    if store.resolution_fingerprint()? != Some(crate::resolution_fingerprint(root)?) {
        return Ok(false);
    }
    let hashes = store.hashes()?;
    Ok(crate::freshness(root, &hashes, 0)?.fresh && store.semantic_missing()?.is_empty())
}

/// (digest, file count, edge count) of a snapshot database.
fn shape(database: &Path) -> Result<(String, usize, usize)> {
    let store = Store::open_read(database)?;
    let hashes = store.hashes()?;
    Ok((
        crate::snapshot_digest(&hashes),
        hashes.len(),
        store.semantic_edge_count()?,
    ))
}

fn is_hex_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest_of(database: &Path) -> Result<String> {
    Ok(crate::snapshot_digest(
        &Store::open_read(database)?.hashes()?,
    ))
}

fn last_diagnostics(database: &Path) -> Result<Vec<Diagnostic>> {
    let status = Store::open_read(database)?.status()?;
    Ok(status
        .last_run
        .map(|run| run.diagnostics)
        .unwrap_or_default())
}

fn touch(path: &Path) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// Remove a directory this cache created (temporary or evicted snapshot).
fn remove_own(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Files the indexer reads or that affect module resolution, outside excluded
/// directories: TS/TSX, JS-family modules, JSON (configs, `package.json`) and
/// `.gitignore` (it decides what the indexer sees).
#[must_use]
pub fn relevant(path: &str) -> bool {
    let mut parts = path.split('/');
    let name = parts.next_back().unwrap_or_default();
    if parts.any(|part| EXCLUDED.contains(&part)) || EXCLUDED.contains(&name) {
        return false;
    }
    name == ".gitignore"
        || name.rsplit_once('.').is_some_and(|(stem, extension)| {
            !stem.is_empty() && MODULE_EXTENSIONS.contains(&extension)
        })
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'))
}

/// Write the relevant blobs of `tree` under `dest`. Symlinks and submodules are
/// not materialised (the indexer skips symlinks; submodules are other repositories).
fn materialize(git: &Git, tree: &str, dest: &Path) -> Result<Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let mut wanted: Vec<(String, String)> = Vec::new();
    for (path, Entry { mode, oid }) in git.ls_tree(tree)? {
        if mode == "160000" {
            diagnostics.push(Diagnostic {
                path,
                message: "Git submodule not analysed".into(),
            });
            continue;
        }
        if !relevant(&path) {
            continue;
        }
        if !safe_relative(&path) {
            diagnostics.push(Diagnostic {
                path,
                message: "unsafe path in Git tree skipped".into(),
            });
            continue;
        }
        if mode == "120000" {
            diagnostics.push(Diagnostic {
                path,
                message: "symlink in Git tree not materialised".into(),
            });
            continue;
        }
        wanted.push((path, oid));
    }
    let oids: Vec<String> = wanted.iter().map(|(_, oid)| oid.clone()).collect();
    for ((path, _), content) in wanted.iter().zip(git.blobs(&oids)?) {
        let target = dest.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, content).with_context(|| format!("materialise {path}"))?;
    }
    Ok(diagnostics)
}

/// What changed in one file between the two states.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    /// `added`, `removed`, `modified` or `renamed`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<u32>,
    /// `source`, `config`, `package`, `json`, `module` or `ignore_rules`.
    pub category: String,
    /// Working tree only: `staged`, `unstaged`, `staged+unstaged`, `untracked`,
    /// or `excluded` (removed from the analysed set but still on disk: newly
    /// ignored by `.gitignore`, or no longer a regular file).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_state: Option<String>,
}

/// The second state of a comparison.
pub enum Head<'a> {
    Commit(&'a Revision),
    Worktree,
}

/// Relevant regular files under `dir` as the indexer would discover them
/// (excluded directories and `.gitignore` honoured), relative paths.
fn listing(dir: &Path) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    for entry in crate::discover(dir, &|| Ok(()))? {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(relative) = entry
            .path()
            .strip_prefix(dir)
            .ok()
            .and_then(|path| path.to_str())
        else {
            continue;
        };
        if relevant(relative) {
            paths.insert(relative.to_owned());
        }
    }
    Ok(paths)
}

// Extensions are matched exactly, like the indexer's `ts`/`tsx` rule.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn category(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == ".gitignore" {
        "ignore_rules"
    } else if name.ends_with(".ts") || name.ends_with(".tsx") {
        "source"
    } else if (name.starts_with("tsconfig") || name.starts_with("jsconfig"))
        && name.ends_with(".json")
    {
        "config"
    } else if name == "package.json" {
        "package"
    } else if name.ends_with(".json") {
        "json"
    } else {
        "module"
    }
}

/// File-level differences between the base commit and the head state, from blob
/// IDs (no content filters). Renames come only from Git's similarity evidence.
pub fn file_changes(
    git: &Git,
    base: &Revision,
    base_dir: &Path,
    head: &Head<'_>,
    head_dir: &Path,
) -> Result<Vec<FileChange>> {
    file_comparison(git, base, base_dir, head, head_dir).map(|comparison| comparison.changes)
}

pub struct FileComparison {
    pub changes: Vec<FileChange>,
    pub base_files_digest: String,
    pub head_files_digest: String,
}

/// BLAKE3 of compact JSON for a sorted path-to-raw-Git-blob-OID map.
fn files_digest(files: &BTreeMap<String, String>) -> Result<String> {
    Ok(crate::content_hash(&serde_json::to_string(files)?))
}

/// Final manifest integrity check over the same relevant-file profile as diff.
pub fn working_files_digest(git: &Git) -> Result<String> {
    let paths: Vec<_> = listing(&git.root)?.into_iter().collect();
    let hashes = git.hash_files(&paths)?;
    files_digest(&paths.into_iter().zip(hashes).collect())
}

pub fn file_comparison(
    git: &Git,
    base: &Revision,
    base_dir: &Path,
    head: &Head<'_>,
    head_dir: &Path,
) -> Result<FileComparison> {
    let base_tree = git.ls_tree(&base.tree)?;
    let oids =
        |listing: BTreeSet<String>, tree: &BTreeMap<String, Entry>| -> BTreeMap<String, String> {
            listing
                .into_iter()
                .filter_map(|path| tree.get(&path).map(|entry| (path, entry.oid.clone())))
                .collect()
        };
    let before = oids(listing(base_dir)?, &base_tree);
    let (after, index, renames) = match head {
        Head::Commit(revision) => {
            let tree = git.ls_tree(&revision.tree)?;
            (
                oids(listing(head_dir)?, &tree),
                None,
                git.renames(base, Some(revision))?,
            )
        }
        Head::Worktree => {
            let paths: Vec<String> = listing(head_dir)?.into_iter().collect();
            let hashes = git.hash_files(&paths)?;
            (
                paths.into_iter().zip(hashes).collect(),
                Some(git.index_entries()?),
                git.renames(base, None)?,
            )
        }
    };
    let mut changes: BTreeMap<String, FileChange> = BTreeMap::new();
    for (path, oid) in &before {
        match after.get(path) {
            Some(other) if other == oid => {}
            Some(_) => {
                changes.insert(path.clone(), change(path, None, "modified"));
            }
            None => {
                changes.insert(path.clone(), change(path, None, "removed"));
            }
        }
    }
    for path in after.keys().filter(|path| !before.contains_key(*path)) {
        changes.insert(path.clone(), change(path, None, "added"));
    }
    for rename in renames {
        let removed = changes
            .get(&rename.from)
            .is_some_and(|c| c.status == "removed");
        let added = changes.get(&rename.to).is_some_and(|c| c.status == "added");
        if removed && added {
            changes.remove(&rename.from);
            let mut moved = change(&rename.to, Some(rename.from), "renamed");
            moved.similarity = Some(rename.similarity);
            changes.insert(rename.to.clone(), moved);
        }
    }
    if let Some(index) = index {
        for (path, entry) in &mut changes {
            let disk = after.get(path);
            let staged_path = entry.previous_path.as_deref().unwrap_or(path);
            let base_oid = before.get(staged_path);
            let index_oid = index.get(path.as_str()).map(|e| &e.oid);
            let untracked = index_oid.is_none() && disk.is_some() && entry.status != "removed";
            // Still on disk but not read: newly ignored, or no longer a regular file.
            let excluded = entry.status == "removed"
                && fs::symlink_metadata(head_dir.join(path.as_str())).is_ok();
            entry.git_state = Some(if excluded {
                "excluded".into()
            } else if untracked && base_oid.is_none() {
                "untracked".into()
            } else {
                let staged = if entry.status == "removed" {
                    index.get(path.as_str()).map(|e| &e.oid) != base_oid
                } else {
                    index_oid != base_oid || entry.previous_path.is_some()
                };
                let unstaged = disk != index_oid;
                match (staged, unstaged) {
                    (true, true) => "staged+unstaged",
                    (true, false) => "staged",
                    _ => "unstaged",
                }
                .into()
            });
        }
    }
    Ok(FileComparison {
        changes: changes.into_values().collect(),
        base_files_digest: files_digest(&before)?,
        head_files_digest: files_digest(&after)?,
    })
}

fn change(path: &str, previous: Option<String>, status: &str) -> FileChange {
    FileChange {
        path: path.to_owned(),
        previous_path: previous,
        status: status.to_owned(),
        similarity: None,
        category: category(path).to_owned(),
        git_state: None,
    }
}

/// Fail early with a clear message when the Atlas root is not inside the work tree.
pub fn ensure_inside(git: &Git) -> Result<()> {
    ensure!(
        git.root.starts_with(&git.toplevel),
        "repository root {} is outside its Git work tree",
        git.root.display()
    );
    Ok(())
}
