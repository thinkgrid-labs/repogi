//! Git snapshot correctness for `repogi impact` (Phase 4.1.1): commits,
//! staged/unstaged/untracked/ignored changes, renames and moves, branches,
//! detached HEAD, linked worktrees, configuration and ignore-rule changes,
//! symlinks, submodules, filters and line endings, fan-out and depth limits.
//! Every test also checks that refs, the Git index, sources and any user index
//! are byte-identical after the analysis.
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
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
            "protocol.file.allow=always",
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

const LIB: &str = "export function helper(n: number): number {\n  return n + 1;\n}\n";

/// A small project with a relative and an alias consumer of `helper`.
fn repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(
        root,
        "tsconfig.json",
        "{ \"compilerOptions\": { \"paths\": { \"@/*\": [\"./src/*\"] } } }\n",
    );
    write(root, ".gitignore", "ignored/\n");
    write(root, "src/lib.ts", LIB);
    write(
        root,
        "src/use.ts",
        "import { helper } from './lib';\n\nexport function useIt(): number {\n  return helper(1);\n}\n",
    );
    write(
        root,
        "src/alias.ts",
        "import { helper } from '@/lib';\n\nexport const viaAlias = (): number => helper(2);\n",
    );
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    temp
}

fn modify_helper_body(root: &Path) {
    write(root, "src/lib.ts", &LIB.replace("n + 1", "n + 2"));
}

fn run(root: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args(["--format", "json", "--max-bytes", "8000000"])
        .args(args)
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn impact(root: &Path, args: &[&str]) -> Value {
    let output = run(root, args, &[]);
    assert!(
        output.status.success(),
        "impact {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn files(report: &Value) -> BTreeSet<String> {
    report["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let letter = match f["status"].as_str().unwrap() {
                "added" => "A",
                "removed" => "D",
                "renamed" => "R",
                _ => "M",
            };
            let mut line = format!("{letter} {}", f["path"].as_str().unwrap());
            if let Some(previous) = f["previous_path"].as_str() {
                let _ = write!(line, "<-{previous}");
            }
            if f["category"] != "source" {
                let _ = write!(line, " [{}]", f["category"].as_str().unwrap());
            }
            if let Some(state) = f["git_state"].as_str() {
                let _ = write!(line, " {state}");
            }
            line
        })
        .collect()
}

fn declarations(report: &Value) -> BTreeSet<String> {
    report["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            format!(
                "{} {} {}",
                d["change"].as_str().unwrap(),
                d["name"].as_str().unwrap(),
                d["path"].as_str().unwrap()
            )
        })
        .collect()
}

fn direct(report: &Value, name: &str) -> BTreeSet<String> {
    report["impacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["name"] == name)
        .flat_map(|i| i["direct"].as_array().unwrap().iter())
        .map(|s| {
            format!(
                "{}:{} {}",
                s["path"].as_str().unwrap(),
                s["line"],
                s["snapshot"].as_str().unwrap()
            )
        })
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// Everything `impact` must leave untouched.
#[derive(Debug, PartialEq, Eq)]
struct State {
    refs: String,
    head: String,
    index: Vec<u8>,
    sources: Vec<(PathBuf, Vec<u8>)>,
    user_index: Option<Vec<u8>>,
}

fn sources(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == ".git" || name == ".code-intel" {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            sources(root, &path, out);
        } else if meta.is_file() {
            out.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                fs::read(&path).unwrap(),
            ));
        }
    }
}

impl State {
    fn capture(root: &Path) -> Self {
        let index = PathBuf::from(
            git(
                root,
                &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            )
            .trim(),
        );
        let mut files = Vec::new();
        sources(root, root, &mut files);
        files.sort();
        Self {
            refs: git(root, &["for-each-ref"]),
            head: fs::read_to_string(
                PathBuf::from(
                    git(root, &["rev-parse", "--path-format=absolute", "--git-dir"]).trim(),
                )
                .join("HEAD"),
            )
            .unwrap(),
            index: fs::read(index).unwrap_or_default(),
            sources: files,
            user_index: fs::read(root.join(".code-intel/index.db")).ok(),
        }
    }
}

/// Run `impact` and assert the repository state is unchanged.
fn checked(root: &Path, args: &[&str]) -> Value {
    let before = State::capture(root);
    let report = impact(root, args);
    assert!(
        State::capture(root) == before,
        "impact modified repository state"
    );
    report
}

#[test]
fn commit_versus_commit() {
    let repo = repository();
    let root = repo.path();
    modify_helper_body(root);
    git(root, &["commit", "-q", "-am", "change"]);
    let report = checked(root, &["--base", "HEAD~1", "--head", "HEAD"]);
    assert_eq!(files(&report), set(&["M src/lib.ts"]));
    assert_eq!(declarations(&report), set(&["modified helper src/lib.ts"]));
    assert_eq!(
        direct(&report, "helper"),
        set(&["src/alias.ts:3 both", "src/use.ts:4 both"])
    );
    assert_eq!(report["head"]["kind"], "commit");
}

#[test]
fn unchanged_worktree_reports_nothing_and_keeps_the_user_index() {
    let repo = repository();
    let root = repo.path();
    let indexed = Command::new(BINARY)
        .current_dir(root)
        .args(["index", "."])
        .output()
        .unwrap();
    assert!(indexed.status.success());
    let report = checked(root, &["--base", "HEAD"]);
    assert!(report["files"].as_array().unwrap().is_empty());
    assert!(report["declarations"].as_array().unwrap().is_empty());
    assert_eq!(report["complete"], true);
    assert_eq!(report["head"]["index"], "user_index");
}

#[test]
fn staged_unstaged_and_both() {
    let repo = repository();
    let root = repo.path();
    modify_helper_body(root);
    git(root, &["add", "src/lib.ts"]);
    assert_eq!(
        files(&checked(root, &["--base", "HEAD"])),
        set(&["M src/lib.ts staged"])
    );
    // A further, different edit on disk: the working tree is what is analysed.
    write(
        root,
        "src/lib.ts",
        &LIB.replace("n + 1", "n + 2")
            .replace("(n: number)", "(n: number, m = 0)"),
    );
    let report = checked(root, &["--base", "HEAD"]);
    assert_eq!(files(&report), set(&["M src/lib.ts staged+unstaged"]));
    assert_eq!(report["declarations"][0]["signature_changed"], true);
    git(root, &["reset", "-q"]);
    assert_eq!(
        files(&checked(root, &["--base", "HEAD"])),
        set(&["M src/lib.ts unstaged"])
    );
}

#[test]
fn untracked_files_count_and_ignored_files_do_not() {
    let repo = repository();
    let root = repo.path();
    modify_helper_body(root);
    write(
        root,
        "src/new.ts",
        "import { helper } from './lib';\n\nexport const fresh = helper(3);\n",
    );
    write(
        root,
        "ignored/hidden.ts",
        "import { helper } from '../src/lib';\n\nexport const hidden = helper(4);\n",
    );
    let report = checked(root, &["--base", "HEAD"]);
    assert_eq!(
        files(&report),
        set(&["M src/lib.ts unstaged", "A src/new.ts untracked"])
    );
    assert_eq!(
        direct(&report, "helper"),
        set(&[
            "src/alias.ts:3 both",
            "src/new.ts:3 head",
            "src/use.ts:4 both"
        ])
    );
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("ignored/hidden.ts")
    );
}

#[test]
fn added_and_deleted_files_staged_and_unstaged() {
    let repo = repository();
    let root = repo.path();
    fs::remove_file(root.join("src/use.ts")).unwrap();
    write(root, "src/other.ts", "export const other = 1;\n");
    let report = checked(root, &["--base", "HEAD"]);
    assert_eq!(
        files(&report),
        set(&["D src/use.ts unstaged", "A src/other.ts untracked"])
    );
    assert_eq!(
        declarations(&report),
        set(&["removed useIt src/use.ts", "added other src/other.ts"])
    );
    git(root, &["add", "-A"]);
    assert_eq!(
        files(&checked(root, &["--base", "HEAD"])),
        set(&["D src/use.ts staged", "A src/other.ts staged"])
    );
}

#[test]
fn staged_and_committed_renames_versus_unstaged_moves() {
    let repo = repository();
    let root = repo.path();
    git(root, &["mv", "src/lib.ts", "src/helpers.ts"]);
    let report = checked(root, &["--base", "HEAD"]);
    assert_eq!(
        files(&report),
        set(&["R src/helpers.ts<-src/lib.ts staged"])
    );
    // The declaration moved unchanged; importers of the old path lost their binding.
    assert!(report["declarations"].as_array().unwrap().is_empty());
    let lost: BTreeSet<String> = report["rebound"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["after"]["status"] != "resolved")
        .map(|r| {
            format!(
                "{}:{} {}",
                r["path"].as_str().unwrap(),
                r["line"],
                r["kind"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        lost,
        set(&[
            "src/alias.ts:1 IMPORTS",
            "src/alias.ts:3 CALLS",
            "src/use.ts:1 IMPORTS",
            "src/use.ts:4 CALLS"
        ])
    );
    git(root, &["commit", "-q", "-m", "rename"]);
    assert_eq!(
        files(&checked(root, &["--base", "HEAD~1", "--head", "HEAD"])),
        set(&["R src/helpers.ts<-src/lib.ts"])
    );
    // A plain move with no Git evidence: removal plus addition, never a rename.
    fs::rename(root.join("src/helpers.ts"), root.join("src/moved.ts")).unwrap();
    let report = checked(root, &["--base", "HEAD"]);
    assert_eq!(
        files(&report),
        set(&["D src/helpers.ts unstaged", "A src/moved.ts untracked"])
    );
    assert_eq!(
        declarations(&report),
        set(&["removed helper src/helpers.ts", "added helper src/moved.ts"])
    );
}

#[test]
fn branches_and_detached_head() {
    let repo = repository();
    let root = repo.path();
    git(root, &["checkout", "-q", "-b", "feature"]);
    modify_helper_body(root);
    git(root, &["commit", "-q", "-am", "feature change"]);
    let report = checked(root, &["--base", "main", "--head", "feature"]);
    assert_eq!(declarations(&report), set(&["modified helper src/lib.ts"]));
    git(root, &["checkout", "-q", "--detach", "feature"]);
    let detached = checked(root, &["--base", "main"]);
    assert_eq!(
        declarations(&detached),
        set(&["modified helper src/lib.ts"])
    );
    assert_eq!(detached["head"]["kind"], "worktree");
    let same = checked(root, &["--base", "HEAD"]);
    assert!(same["files"].as_array().unwrap().is_empty());
}

#[test]
fn linked_worktree() {
    let repo = repository();
    let root = repo.path();
    let linked = TempDir::new().unwrap();
    let path = linked.path().join("wt");
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            path.to_str().unwrap(),
        ],
    );
    modify_helper_body(&path);
    let main_state = State::capture(root);
    let report = checked(&path, &["--base", "main"]);
    assert_eq!(files(&report), set(&["M src/lib.ts unstaged"]));
    assert!(
        State::capture(root) == main_state,
        "main work tree modified"
    );
}

#[test]
fn tsconfig_alias_change_rebinds_consumers() {
    let repo = repository();
    let root = repo.path();
    write(
        root,
        "tsconfig.json",
        "{ \"compilerOptions\": { \"paths\": { \"@/*\": [\"./lib/*\"] } } }\n",
    );
    let report = checked(root, &["--base", "HEAD"]);
    assert_eq!(files(&report), set(&["M tsconfig.json [config] unstaged"]));
    assert_eq!(report["resolution_changed"], true);
    let lost: BTreeSet<String> = report["rebound"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}:{} {}",
                r["path"].as_str().unwrap(),
                r["line"],
                r["after"]["status"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        lost,
        set(&["src/alias.ts:1 unresolved", "src/alias.ts:3 unresolved"])
    );
}

#[test]
fn gitignore_change_excludes_a_file_without_calling_it_deleted() {
    let repo = repository();
    let root = repo.path();
    write(root, ".gitignore", "ignored/\nsrc/use.ts\n");
    let report = checked(root, &["--base", "HEAD"]);
    assert_eq!(
        files(&report),
        set(&[
            "M .gitignore [ignore_rules] unstaged",
            "D src/use.ts excluded"
        ])
    );
    // Still on disk: its declarations are unknown, not removed.
    assert_eq!(declarations(&report), set(&["unknown useIt src/use.ts"]));
}

#[test]
fn symlinks_are_skipped_consistently_and_submodules_are_reported() {
    let repo = repository();
    let root = repo.path();
    std::os::unix::fs::symlink("lib.ts", root.join("src/link.ts")).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "symlink"]);
    let report = checked(root, &["--base", "HEAD"]);
    assert!(report["files"].as_array().unwrap().is_empty(), "{report}");
    assert!(
        report["coverage"]["base"]["skipped_files"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(
        report["coverage"]["head"]["skipped_files"]
            .as_u64()
            .unwrap()
            >= 1
    );
    let other = TempDir::new().unwrap();
    write(other.path(), "vendor.ts", "export const vendored = 1;\n");
    git(other.path(), &["init", "-q"]);
    git(other.path(), &["add", "-A"]);
    git(other.path(), &["commit", "-q", "-m", "vendor"]);
    git(
        root,
        &[
            "submodule",
            "add",
            "-q",
            other.path().to_str().unwrap(),
            "vendor/sub",
        ],
    );
    git(root, &["commit", "-q", "-m", "submodule"]);
    let report = checked(root, &["--base", "HEAD"]);
    assert_eq!(report["complete"], false);
    let limitations = serde_json::to_string(&report["limitations"]).unwrap();
    assert!(
        limitations.contains("submodule vendor/sub"),
        "{limitations}"
    );
}

#[test]
fn content_filters_never_run_and_are_reported() {
    let repo = repository();
    let root = repo.path();
    write(root, ".gitattributes", "*.ts filter=marker\n");
    git(root, &["add", ".gitattributes"]);
    git(root, &["commit", "-q", "-m", "attributes"]);
    // Configure the filter only now, so the test's own Git commands never ran it.
    let marker = root.join("filter-ran");
    let command = format!("sh -c 'touch {}; cat'", marker.display());
    git(root, &["config", "filter.marker.clean", &command]);
    git(root, &["config", "filter.marker.smudge", &command]);
    modify_helper_body(root);
    let report = impact(root, &["--base", "HEAD"]);
    assert!(!marker.exists(), "a configured filter ran");
    let limitations = serde_json::to_string(&report["limitations"]).unwrap();
    assert!(
        limitations.contains("content filter attribute on src/lib.ts"),
        "{limitations}"
    );
    assert_eq!(report["complete"], false);
}

#[test]
fn line_ending_conversion_is_reported() {
    let repo = repository();
    let root = repo.path();
    write(root, ".gitattributes", "*.ts text eol=crlf\n");
    git(root, &["add", ".gitattributes"]);
    git(root, &["commit", "-q", "-m", "attributes"]);
    write(
        root,
        "src/lib.ts",
        &LIB.replace("n + 1", "n + 2").replace('\n', "\r\n"),
    );
    let report = checked(root, &["--base", "HEAD"]);
    let limitations = serde_json::to_string(&report["limitations"]).unwrap();
    assert!(
        limitations.contains("line-ending conversion applies to src/lib.ts"),
        "{limitations}"
    );
    assert_eq!(report["complete"], false);
}

#[test]
fn source_change_during_analysis_is_detected() {
    let repo = repository();
    let root = repo.path();
    modify_helper_body(root);
    let child = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args(["--format", "json", "--base", "HEAD"])
        .env("CODE_INTEL_TEST_PAUSE_AFTER_SNAPSHOTS_MS", "1500")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    write(
        root,
        "src/use.ts",
        "export function useIt(): number {\n  return 0;\n}\n",
    );
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["complete"], false);
    let limitations = serde_json::to_string(&report["limitations"]).unwrap();
    assert!(
        limitations.contains("changed during analysis"),
        "{limitations}"
    );
}

#[test]
fn summary_cannot_claim_freshness_when_the_snapshot_changes_during_analysis() {
    let repo = repository();
    let root = repo.path();
    modify_helper_body(root);
    let child = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args(["--summary", "--format", "json", "--base", "HEAD"])
        .env("CODE_INTEL_TEST_PAUSE_AFTER_SNAPSHOTS_MS", "1500")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    write(
        root,
        "src/use.ts",
        "export function useIt(): number { return 0; }\n",
    );
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["snapshots_fresh"], false);
    assert_eq!(summary["direct_files_complete"], false);
    assert_eq!(summary["complete"], false);
}

#[test]
fn large_fan_out_depth_limits_and_bounded_json() {
    let repo = repository();
    let root = repo.path();
    for index in 0..240 {
        write(
            root,
            &format!("src/fan/c{index:03}.ts"),
            &format!(
                "import {{ helper }} from '../lib';\n\nexport const c{index} = helper({index});\n"
            ),
        );
    }
    // A chain helper ← one ← two ← three for depth limits.
    write(
        root,
        "src/one.ts",
        "import { helper } from './lib';\n\nexport function one(): number {\n  return helper(0);\n}\n",
    );
    write(
        root,
        "src/two.ts",
        "import { one } from './one';\n\nexport function two(): number {\n  return one();\n}\n",
    );
    write(
        root,
        "src/three.ts",
        "import { two } from './two';\n\nexport function three(): number {\n  return two();\n}\n",
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "fan-out"]);
    modify_helper_body(root);
    let report = checked(root, &["--base", "HEAD"]);
    let helper = &report["impacts"][0];
    assert_eq!(helper["direct"].as_array().unwrap().len(), 200);
    assert_eq!(helper["omitted"]["direct"], 43);
    assert_eq!(report["complete"], false);
    assert_eq!(report["truncated"], true);
    assert!(
        serde_json::to_string(&report["incomplete_reasons"])
            .unwrap()
            .contains("lists capped")
    );
    for (depth, expected) in [("1", 0), ("2", 1), ("3", 2)] {
        let chain = checked(
            root,
            &["--base", "HEAD", "--depth", depth, "--max-results", "1000"],
        );
        let transitive: Vec<&str> = chain["impacts"][0]["transitive"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(transitive.len(), expected, "depth {depth}: {transitive:?}");
    }
    for budget in ["2000", "5000", "20000"] {
        let output = Command::new(BINARY)
            .arg("impact")
            .arg(root)
            .args(["--format", "json", "--base", "HEAD", "--max-bytes", budget])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.len() <= budget.parse().unwrap());
        let bounded: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(bounded["complete"], false);
        assert_eq!(bounded["truncated"], true);
    }
}

#[test]
fn analysed_directory_below_the_repository_root() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(root, "packages/app/src/lib.ts", LIB);
    write(
        root,
        "packages/app/src/use.ts",
        "import { helper } from './lib';\n\nexport const used = helper(1);\n",
    );
    write(root, "packages/other/src/lib.ts", LIB);
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    modify_helper_body(&root.join("packages/app"));
    write(
        root,
        "packages/other/src/lib.ts",
        &LIB.replace("n + 1", "n + 9"),
    );
    let app = root.join("packages/app");
    let report = checked(&app, &["--base", "HEAD"]);
    // Paths are relative to the analysed directory; the sibling package is out of scope.
    assert_eq!(files(&report), set(&["M src/lib.ts unstaged"]));
    assert_eq!(direct(&report, "helper"), set(&["src/use.ts:3 both"]));
    git(
        root,
        &[
            "mv",
            "packages/app/src/lib.ts",
            "packages/app/src/helpers.ts",
        ],
    );
    let report = checked(&app, &["--base", "HEAD"]);
    assert_eq!(
        files(&report),
        set(&["R src/helpers.ts<-src/lib.ts staged+unstaged"])
    );
}
