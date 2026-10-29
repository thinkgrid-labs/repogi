//! Manifest contract tests; fixture sources and runner traps are data only.
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::Duration,
};
use tempfile::TempDir;
const BINARY: &str = env!("CARGO_BIN_EXE_repogi");

/// Waits for the impact run's test pause, so a mid-analysis edit lands after
/// its snapshots on runners of any speed.
fn wait_for_pause(child: &mut std::process::Child, marker: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while !marker.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "impact exited before pausing"
        );
        assert!(std::time::Instant::now() < deadline, "impact never paused");
        thread::sleep(Duration::from_millis(20));
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Atlas",
            "-c",
            "user.email=atlas@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn fixture() -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(root, ".gitignore", ".code-intel/\n");
    write(
        root,
        "src/math.ts",
        "export function f() { return 1; }\nexport function g() { return 2; }\n",
    );
    write(
        root,
        "src/math.test.ts",
        "import { f, g } from './math';\ntest('both', () => { f(); g(); });\n",
    );
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    temp
}
fn output(root: &Path, extra: &[&str]) -> Output {
    Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args(["--manifest", "--format", "json"])
        .args(extra)
        .output()
        .unwrap()
}
fn artifact(root: &Path, extra: &[&str]) -> (String, Value) {
    let out = output(root, extra);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    let mut payload = value.clone();
    let id = payload
        .as_object_mut()
        .unwrap()
        .remove("manifest_id")
        .unwrap();
    assert_eq!(
        id,
        format!(
            "blake3:{}",
            blake3::hash(&serde_json::to_vec(&payload).unwrap()).to_hex()
        )
    );
    assert_eq!(
        text,
        format!("{}\n", serde_json::to_string(&value).unwrap())
    );
    assert!(!text.contains(root.to_str().unwrap()));
    assert_eq!(value["completeness"]["output_omissions"], json!({}));
    assert_eq!(value["completeness"]["output_truncated"], false);
    assert_eq!(value["verification"]["behavioral_coverage"], "unknown");
    assert_eq!(value["verification"]["execution"], "not_performed");
    for key in ["confirmed", "possible", "unknown", "configurations", "gaps"] {
        assert_eq!(
            value["verification"]["totals"][key].as_u64().unwrap(),
            value["verification"][key].as_array().unwrap().len() as u64
        );
    }
    (text, value)
}
fn change(root: &Path) {
    write(
        root,
        "src/math.ts",
        "export function f() { return 3; }\nexport function g() { return 4; }\n",
    );
}

#[test]
#[allow(clippy::items_after_statements)]
fn cold_warm_evicted_and_relocated_states_have_identical_bytes() {
    let root = fixture();
    change(root.path());
    let cold = artifact(root.path(), &[]).0;
    assert_eq!(cold, artifact(root.path(), &[]).0);
    fs::remove_dir_all(root.path().join(".code-intel")).unwrap();
    assert_eq!(cold, artifact(root.path(), &[]).0);
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".code-intel" {
                continue;
            }
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                // Fresh mtimes: macOS `fs::copy` keeps the source times, which can hide
                // same-size overlay edits from Git's stat check.
                fs::write(target, fs::read(entry.path()).unwrap()).unwrap();
            }
        }
    }
    let other = TempDir::new().unwrap();
    copy(root.path(), other.path());
    assert_eq!(cold, artifact(other.path(), &[]).0);
}
#[test]
fn several_changed_declarations_keep_one_test_and_separate_origins() {
    let root = fixture();
    change(root.path());
    let (_, value) = artifact(root.path(), &[]);
    assert_eq!(
        value["changes"]["declarations"].as_array().unwrap().len(),
        2
    );
    let confirmed = value["verification"]["confirmed"].as_array().unwrap();
    assert_eq!(confirmed.len(), 1);
    assert_eq!(confirmed[0]["path"], "src/math.test.ts");
    assert_eq!(confirmed[0]["evidence"]["depth"], 1);
    assert_eq!(confirmed[0]["linked_changes"].as_array().unwrap().len(), 2);
    assert_eq!(value["snapshots"]["head"]["kind"], "worktree");
    assert!(value["snapshots"]["head"]["commit"].is_null());
    assert!(value["snapshots"]["head"]["tree"].is_null());
}
#[test]
fn options_staging_and_nonindexed_contents_participate_in_identity() {
    let root = fixture();
    change(root.path());
    let first = artifact(root.path(), &[]).0;
    for args in [
        vec!["--depth", "1"],
        vec!["--max-results", "10"],
        vec!["--max-declarations", "1"],
        vec!["--test-dir", "integration"],
    ] {
        assert_ne!(first, artifact(root.path(), &args).0);
    }
    git(root.path(), &["add", "src/math.ts"]);
    assert_ne!(first, artifact(root.path(), &[]).0);
    for (path, one, two) in [
        ("data.json", "{\"v\":1}", "{\"v\":2}"),
        ("source.js", "const a=1;", "const a=2;"),
    ] {
        write(root.path(), path, one);
        let before = artifact(root.path(), &[]).0;
        write(root.path(), path, two);
        assert_ne!(before, artifact(root.path(), &[]).0);
    }
}
#[test]
fn stopped_analysis_is_distinct_from_artifact_truncation() {
    let root = fixture();
    change(root.path());
    let (_, value) = artifact(root.path(), &["--max-declarations", "1"]);
    assert_eq!(value["completeness"]["analysis_limited"], true);
    assert_eq!(
        value["completeness"]["undiscovered_due_to_work_limits"],
        true
    );
    assert_eq!(
        value["completeness"]["analysis_omissions"]["declarations_not_analysed"],
        1
    );
    assert_eq!(value["completeness"]["complete"], false);
    assert!(
        value["verification"]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["kind"] == "analysis_work_limit")
    );
}
#[test]
fn fanout_is_complete_or_fails_without_partial_stdout() {
    let root = fixture();
    for i in 0..260 {
        write(
            root.path(),
            &format!("tests/{i}.test.ts"),
            "import { f } from '../src/math';\nf();\n",
        );
    }
    git(root.path(), &["add", "-A"]);
    git(root.path(), &["commit", "-q", "-m", "tests"]);
    change(root.path());
    let (text, value) = artifact(root.path(), &[]);
    assert_eq!(
        value["verification"]["confirmed"].as_array().unwrap().len(),
        261
    );
    assert!(text.len() > 32_000);
    for budget in [2000, 32_000] {
        let out = output(root.path(), &["--max-bytes", &budget.to_string()]);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("complete manifest exceeds max-bytes")
        );
    }
}
#[test]
fn unsupported_syntax_dynamic_imports_and_collisions_remain_unverified() {
    let root = fixture();
    #[cfg(unix)]
    let outside = TempDir::new().unwrap();
    #[cfg(unix)]
    {
        write(
            outside.path(),
            "secret.ts",
            "export const SECRET_FROM_OUTSIDE = 1;\n",
        );
        std::os::unix::fs::symlink(
            outside.path().join("secret.ts"),
            root.path().join("src/external.test.ts"),
        )
        .unwrap();
    }
    for (path, text) in [
        ("tests/math.test.ts", "test('collision', () => 0);\n"),
        (
            "src/dynamic.test.ts",
            "const m = await import('./math');\nm.f();\n",
        ),
        (
            "src/unresolved.test.ts",
            "import { f } from './missing';\nf();\n",
        ),
        ("src/alternate.ts", "export function f() { return 0; }\n"),
        (
            "src/ambiguous-barrel.ts",
            "export * from './math';\nexport * from './alternate';\n",
        ),
        (
            "src/ambiguous.test.ts",
            "import { f } from './ambiguous-barrel';\nf();\n",
        ),
        (
            "src/javascript.test.js",
            "throw new Error('DO_NOT_EXECUTE');\n",
        ),
        (
            "vitest.config.ts",
            "throw new Error('DO_NOT_EXECUTE_CONFIG');\n",
        ),
        (
            "src/grammar.test.ts",
            "import { f } from './math';\nconst actual = original<typeof import('./math')>();\nf();\n",
        ),
    ] {
        write(root.path(), path, text);
    }
    git(root.path(), &["add", "-A"]);
    git(root.path(), &["commit", "-q", "-m", "uncertain tests"]);
    change(root.path());
    let (text, value) = artifact(root.path(), &[]);
    let confirmed = value["verification"]["confirmed"].as_array().unwrap();
    assert_eq!(confirmed.len(), 1);
    assert_eq!(confirmed[0]["path"], "src/math.test.ts");
    for path in ["src/javascript.test.js", "src/grammar.test.ts"] {
        assert!(
            value["verification"]["unknown"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == path)
        );
    }
    for path in [
        "tests/math.test.ts",
        "src/unresolved.test.ts",
        "src/ambiguous.test.ts",
    ] {
        assert!(
            value["verification"]["possible"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == path)
        );
    }
    assert_eq!(value["completeness"]["source_coverage_unknown"], true);
    assert!(!text.contains("DO_NOT_EXECUTE"));
    assert!(!text.contains("SECRET_FROM_OUTSIDE"));
    assert!(
        value["affected_code"]["relationships"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| r["possible"].as_array().unwrap())
            .any(|p| p["path"] == "src/ambiguous.test.ts" && p["status"] == "ambiguous")
    );
    #[cfg(unix)]
    assert!(!text.contains(outside.path().to_str().unwrap()));
}
#[test]
fn clean_and_configuration_only_states_have_explicit_gaps() {
    let root = fixture();
    let (_, clean) = artifact(root.path(), &[]);
    assert!(clean["changes"]["files"].as_array().unwrap().is_empty());
    assert!(
        clean["verification"]["confirmed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(clean["completeness"]["complete"], true);
    write(
        root.path(),
        "tsconfig.json",
        "{\"compilerOptions\":{\"baseUrl\":\".\"}}\n",
    );
    let (_, config) = artifact(root.path(), &[]);
    assert_eq!(config["changes"]["resolution_changed"], true);
    assert!(
        config["verification"]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["path"] == "tsconfig.json")
    );
}
#[test]
fn concurrent_source_or_index_changes_prevent_freshness_claims() {
    for change_kind in ["source", "index", "json"] {
        let root = fixture();
        change(root.path());
        write(root.path(), "data.json", "{\"v\":1}\n");
        artifact(root.path(), &[]);
        let pause = TempDir::new().unwrap();
        let marker = pause.path().join("paused");
        let mut child = Command::new(BINARY)
            .arg("impact")
            .arg(root.path())
            .args(["--manifest", "--format", "json"])
            .env("CODE_INTEL_TEST_PAUSE_AFTER_SNAPSHOTS_MS", "1200")
            .env("CODE_INTEL_TEST_PAUSE_MARKER", &marker)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_for_pause(&mut child, &marker);
        if change_kind == "index" {
            git(root.path(), &["add", "src/math.ts"]);
        } else if change_kind == "json" {
            write(root.path(), "data.json", "{\"v\":2}\n");
        } else {
            write(
                root.path(),
                "src/math.ts",
                "export function f() { return 999; }\n",
            );
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["snapshots"]["freshness_verified"], false);
        assert_eq!(value["completeness"]["complete"], false);
        assert_eq!(value["completeness"]["direct_files_complete"], false);
        if change_kind == "index" {
            assert_eq!(value["snapshots"]["git_metadata_verified"], false);
        } else {
            assert_eq!(value["snapshots"]["file_inputs_verified"], false);
        }
    }
}
#[test]
fn repository_sources_index_refs_and_script_traps_are_untouched() {
    let root = fixture();
    write(
        root.path(),
        "package.json",
        "{\"scripts\":{\"test\":\"touch RAN_TEST\",\"postinstall\":\"touch RAN_INSTALL\"}}\n",
    );
    write(
        root.path(),
        "src/math.ts",
        "export function f() { return 'PRIVATE_LITERAL'; }\n",
    );
    let indexed = Command::new(BINARY)
        .arg("index")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(indexed.status.success());
    let user_index = fs::read(root.path().join(".code-intel/index.db")).unwrap();
    let index = fs::read(root.path().join(".git/index")).unwrap();
    let status = git(
        root.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let (text, _) = artifact(root.path(), &[]);
    assert!(!text.contains("PRIVATE_LITERAL"));
    assert!(!text.contains("touch RAN"));
    assert_eq!(
        user_index,
        fs::read(root.path().join(".code-intel/index.db")).unwrap()
    );
    assert_eq!(index, fs::read(root.path().join(".git/index")).unwrap());
    assert_eq!(
        status,
        git(
            root.path(),
            &["status", "--porcelain=v1", "--untracked-files=all"]
        )
    );
    assert_eq!(head, git(root.path(), &["rev-parse", "HEAD"]));
    assert!(!root.path().join("RAN_TEST").exists());
    assert!(!root.path().join("RAN_INSTALL").exists());
}
#[test]
fn incompatible_flags_fail_before_any_artifact_output() {
    let root = fixture();
    for flags in [
        vec!["--tests"],
        vec!["--summary"],
        vec!["--detail"],
        vec!["--evidence-file", "src/math.test.ts"],
    ] {
        let out = output(root.path(), &flags);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
    }
    let out = Command::new(BINARY)
        .arg("impact")
        .arg(root.path())
        .arg("--manifest")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires --format json"));
}
