//! Snapshot-cache audit for `repogi impact` (Phase 4.1.1): reuse equals a
//! fresh build, tampered/corrupt/foreign entries are rebuilt rather than
//! trusted, interrupted builds are cleaned, concurrent runs agree, eviction is
//! bounded and touches only its own entries, materialisation keeps module
//! resolution inputs, and no repository code (hooks, filters, fsmonitor,
//! package scripts) runs.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Atlas Test",
            "-c",
            "user.email=atlas@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(
        root,
        "tsconfig.json",
        "{ \"extends\": \"./config/base.json\" }\n",
    );
    write(
        root,
        "config/base.json",
        "{ \"compilerOptions\": { \"paths\": { \"@/*\": [\"./src/*\"] } } }\n",
    );
    write(
        root,
        "package.json",
        "{ \"name\": \"fixture\", \"scripts\": { \"postinstall\": \"touch SCRIPT_RAN\" } }\n",
    );
    write(root, "src/legacy.js", "module.exports = 1;\n");
    write(
        root,
        "src/lib.ts",
        "export function helper(n: number): number {\n  return n + 1;\n}\n",
    );
    write(
        root,
        "src/use.ts",
        "import { helper } from '@/lib';\n\nexport const used = helper(1);\n",
    );
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    temp
}

fn edit(root: &Path) {
    write(
        root,
        "src/lib.ts",
        "export function helper(n: number): number {\n  return n + 2;\n}\n",
    );
}

fn impact(root: &Path, extra: &[&str]) -> Value {
    let output = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args(["--format", "json", "--max-bytes", "8000000"])
        .args(if extra.contains(&"--base") {
            &[][..]
        } else {
            &["--base", "HEAD"][..]
        })
        .args(extra)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The report without provenance that legitimately differs between runs.
fn stable(mut report: Value) -> Value {
    for side in ["base", "head"] {
        for field in ["seconds", "index", "files_reparsed", "seeded"] {
            report[side].as_object_mut().unwrap().remove(field);
        }
    }
    report
}

fn trees(cache: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(cache.join("trees"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            p.join("snapshot.json").is_file() && !name.starts_with('.') && name.contains("-ir")
        })
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn cached_snapshot_equals_a_fresh_build() {
    let repo = repository();
    let root = repo.path();
    edit(root);
    let first = impact(root, &[]);
    let cached = impact(root, &[]);
    assert_eq!(cached["base"]["index"], "cache");
    let other = TempDir::new().unwrap();
    let fresh = impact(root, &["--cache-dir", other.path().to_str().unwrap()]);
    assert_eq!(fresh["base"]["index"], "built");
    assert_eq!(stable(first.clone()), stable(cached));
    assert_eq!(stable(first), stable(fresh));
}

#[test]
fn tampered_corrupt_or_missing_entries_are_rebuilt() {
    let repo = repository();
    let root = repo.path();
    edit(root);
    let reference = stable(impact(root, &[]));
    let cache = root.join(".code-intel/impact");
    let entry = trees(&cache).pop().unwrap();
    let marker = entry.join("snapshot.json");
    let original = fs::read_to_string(&marker).unwrap();
    for tampered in [
        original.replace("\"digest\":\"", "\"digest\":\"0"),
        original.replace("\"edges\":", "\"edges\":1"),
        "not json".to_owned(),
    ] {
        fs::write(&marker, &tampered).unwrap();
        let report = impact(root, &[]);
        assert_eq!(
            report["base"]["index"], "built",
            "tampered marker trusted: {tampered}"
        );
        assert_eq!(stable(report), reference);
    }
    let entry = trees(&cache).pop().unwrap();
    fs::write(entry.join("index.db"), b"corrupt").unwrap();
    let report = impact(root, &[]);
    assert_eq!(report["base"]["index"], "built");
    assert_eq!(stable(report), reference);
    let entry = trees(&cache).pop().unwrap();
    fs::remove_file(entry.join("index.db")).unwrap();
    assert_eq!(impact(root, &[])["base"]["index"], "built");
}

#[test]
fn edited_snapshot_sources_are_reported_not_trusted() {
    let repo = repository();
    let root = repo.path();
    edit(root);
    impact(root, &[]);
    let entry = trees(&root.join(".code-intel/impact")).pop().unwrap();
    write(
        &entry.join("src"),
        "src/lib.ts",
        "export function helper() {}\n",
    );
    let report = impact(root, &[]);
    assert_eq!(report["complete"], false);
    let limitations = serde_json::to_string(&report["limitations"]).unwrap();
    assert!(
        limitations.contains("source text of src/lib.ts"),
        "{limitations}"
    );
}

#[test]
fn interrupted_builds_are_never_used_and_stale_ones_are_removed() {
    let repo = repository();
    let root = repo.path();
    edit(root);
    impact(root, &[]);
    let trees_dir = root.join(".code-intel/impact/trees");
    let tree = "a".repeat(40);
    let stale = trees_dir.join(format!(".tmp-{tree}-ir2-0000000000000000-1-1"));
    let recent = trees_dir.join(format!(".tmp-{tree}-ir2-0000000000000000-2-2"));
    for dir in [&stale, &recent] {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("index.db"), b"half written").unwrap();
    }
    let old = SystemTime::now() - Duration::from_secs(7200);
    fs::File::open(&stale).unwrap().set_modified(old).unwrap();
    let report = impact(root, &[]);
    assert_eq!(report["base"]["index"], "cache");
    assert!(!stale.exists(), "stale interrupted build kept");
    assert!(recent.exists(), "a possibly running build was removed");
}

#[test]
fn concurrent_runs_agree() {
    let repo = repository();
    let root = repo.path();
    edit(root);
    let spawn = || {
        Command::new(BINARY)
            .arg("impact")
            .arg(root)
            .args(["--format", "json", "--base", "HEAD"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let children: Vec<_> = (0..3).map(|_| spawn()).collect();
    let mut reports = Vec::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        // A run that loses the race for the shared working-tree cache may fail
        // with an explicit retry error; it must never return a wrong result.
        if output.status.success() {
            reports.push(stable(serde_json::from_slice(&output.stdout).unwrap()));
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("retry"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert!(!reports.is_empty());
    let reference = stable(impact(root, &[]));
    for report in reports {
        assert_eq!(report, reference);
    }
    assert_eq!(trees(&root.join(".code-intel/impact")).len(), 1);
}

#[test]
fn eviction_is_bounded_and_touches_only_its_own_entries() {
    let repo = repository();
    let root = repo.path();
    let cache = TempDir::new().unwrap();
    let cache_dir = cache.path().to_str().unwrap().to_owned();
    fs::create_dir_all(cache.path().join("trees/user-dir")).unwrap();
    fs::write(
        cache.path().join("trees/user-dir/snapshot.json"),
        b"{\"mine\": true}",
    )
    .unwrap();
    fs::write(cache.path().join("trees/notes.txt"), b"keep me").unwrap();
    for round in 0..6 {
        write(
            root,
            "src/lib.ts",
            &format!("export function helper(n: number): number {{\n  return n + {round};\n}}\n"),
        );
        git(root, &["commit", "-q", "-am", &format!("round {round}")]);
        let report = impact(
            root,
            &[
                "--cache-dir",
                &cache_dir,
                "--base",
                "HEAD~1",
                "--head",
                "HEAD",
            ],
        );
        assert_eq!(report["declarations"].as_array().unwrap().len(), 1);
    }
    assert!(trees(cache.path()).len() <= 4, "{:?}", trees(cache.path()));
    assert!(cache.path().join("trees/user-dir/snapshot.json").is_file());
    assert_eq!(
        fs::read(cache.path().join("trees/notes.txt")).unwrap(),
        b"keep me"
    );
}

#[test]
fn materialised_commits_keep_module_resolution_inputs() {
    let repo = repository();
    let root = repo.path();
    let report = impact(root, &[]);
    assert!(report["files"].as_array().unwrap().is_empty());
    assert_eq!(report["resolution_changed"], false);
    assert!(report["rebound"].as_array().unwrap().is_empty());
    let entry = trees(&root.join(".code-intel/impact")).pop().unwrap();
    for input in [
        "tsconfig.json",
        "config/base.json",
        "package.json",
        "src/legacy.js",
    ] {
        assert!(
            entry.join("src").join(input).is_file(),
            "{input} not materialised"
        );
    }
}

#[test]
fn no_repository_code_runs() {
    let repo = repository();
    let root = repo.path();
    let marker = |name: &str| root.join(format!("RAN-{name}"));
    let hooks = root.join(".git/hooks");
    for hook in [
        "post-checkout",
        "post-index-change",
        "pre-commit",
        "reference-transaction",
        "fsmonitor-watchman",
    ] {
        let path = hooks.join(hook);
        fs::write(
            &path,
            format!("#!/bin/sh\ntouch '{}'\n", marker(hook).display()),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    let fsmonitor = root.join("fsmonitor.sh");
    fs::write(
        &fsmonitor,
        format!("#!/bin/sh\ntouch '{}'\n", marker("fsmonitor").display()),
    )
    .unwrap();
    #[cfg(unix)]
    fs::set_permissions(
        &fsmonitor,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    write(root, ".gitattributes", "*.ts filter=evil diff=evil\n");
    git(root, &["add", ".gitattributes", "fsmonitor.sh"]);
    git(root, &["commit", "-q", "-m", "traps"]);
    git(root, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    git(
        root,
        &["config", "core.fsmonitor", fsmonitor.to_str().unwrap()],
    );
    let filter = format!("sh -c 'touch {}; cat'", marker("filter").display());
    git(root, &["config", "filter.evil.clean", &filter]);
    git(root, &["config", "filter.evil.smudge", &filter]);
    git(root, &["config", "diff.evil.textconv", &filter]);
    git(root, &["config", "diff.external", &filter]);
    edit(root);
    impact(root, &[]);
    write(
        root,
        "src/use.ts",
        "import { helper } from '@/lib';\n\nexport const used = helper(2);\n",
    );
    git(
        root,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "filter.evil.clean=cat",
            "-c",
            "filter.evil.smudge=cat",
            "commit",
            "-q",
            "-am",
            "head",
        ],
    );
    impact(root, &["--base", "HEAD~1", "--head", "HEAD"]);
    let ran: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("RAN-") || name == "SCRIPT_RAN")
        .collect();
    assert!(ran.is_empty(), "repository code ran: {ran:?}");
}

#[test]
fn another_builds_index_is_not_trusted_and_moved_indexes_do_not_break_seeding() {
    let repo = repository();
    let root = repo.path();
    let indexed = Command::new(BINARY)
        .current_dir(root)
        .args(["index", "."])
        .output()
        .unwrap();
    assert!(indexed.status.success());
    let reference = stable(impact(root, &[]));
    let db = rusqlite::Connection::open(root.join(".code-intel/index.db")).unwrap();
    db.execute(
        "UPDATE metadata SET value='ir2-someotherbuild' WHERE key='indexer_build'",
        [],
    )
    .unwrap();
    drop(db);
    let report = impact(root, &[]);
    assert_eq!(
        report["head"]["index"], "built",
        "an index from another build was used"
    );
    assert_eq!(stable(report), reference);
    // The same repository at another path: its index records the old root.
    let moved = TempDir::new().unwrap();
    let copy = Command::new("cp")
        .args([
            "-a",
            &format!("{}/.", root.display()),
            moved.path().to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(copy.success());
    fs::remove_dir_all(moved.path().join(".code-intel/impact")).unwrap();
    edit(moved.path());
    let report = impact(moved.path(), &[]);
    assert_eq!(report["declarations"].as_array().unwrap().len(), 1);
    // The old-root index is neither used nor copied; the base snapshot may be
    // seeded from the new working-tree cache.
    assert_eq!(report["head"]["index"], "built");
    assert_eq!(report["head"]["seeded"], false);
}

#[test]
fn repeated_queries_reuse_snapshots() {
    let repo = repository();
    let root = repo.path();
    edit(root);
    impact(root, &[]);
    let again = impact(root, &[]);
    assert_eq!(again["base"]["index"], "cache");
    assert_eq!(again["base"]["files_reparsed"], 0);
    assert_eq!(again["head"]["files_reparsed"], 0);
}
