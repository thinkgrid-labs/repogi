//! Independent source-labelled Git-history tests. Never executes fixture scripts.
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/affected-tests")
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Atlas Test",
            "-c",
            "user.email=test@example.invalid",
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
}

fn repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    copy(&fixtures().join("repo"), temp.path());
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["add", "-A", "--force"]);
    git(temp.path(), &["commit", "-q", "-m", "base"]);
    temp
}

fn write(root: &Path, path: &str, content: &str) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, content).unwrap();
}

fn apply(root: &Path, case: &Value) {
    if let Some(n) = case["fanout"].as_u64() {
        for i in 0..n {
            write(
                root,
                &format!("tests/fan/{i:04}.test.ts"),
                "import { calc } from '../../src/math';\ntest('fan', () => calc(1));\n",
            );
        }
        git(root, &["add", "-A", "--force"]);
        git(root, &["commit", "-q", "-m", "fanout baseline"]);
    }
    git(root, &["tag", "review-base"]);
    if let Some(renames) = case["renames"].as_array() {
        for pair in renames {
            fs::rename(
                root.join(pair[0].as_str().unwrap()),
                root.join(pair[1].as_str().unwrap()),
            )
            .unwrap();
        }
    }
    if let Some(deletes) = case["deletes"].as_array() {
        for path in deletes {
            fs::remove_file(root.join(path.as_str().unwrap())).unwrap();
        }
    }
    for edit in case["edits"].as_array().unwrap() {
        let path = edit["path"].as_str().unwrap();
        let text = fs::read_to_string(root.join(path)).unwrap();
        let old = edit["old"].as_str().unwrap();
        assert!(text.contains(old), "missing independent patch context");
        write(
            root,
            path,
            &text.replacen(old, edit["new"].as_str().unwrap(), 1),
        );
    }
    if let Some(writes) = case["writes"].as_object() {
        for (path, content) in writes {
            write(root, path, content.as_str().unwrap());
        }
    }
    git(root, &["add", "-A", "--force"]);
    git(root, &["commit", "-q", "-m", "change"]);
}

fn run(root: &Path, extra: &[&str]) -> Value {
    let output = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args([
            "--tests",
            "--base",
            "review-base",
            "--head",
            "HEAD",
            "--format",
            "json",
            "--max-bytes",
            "8000000",
            "--max-results",
            "1000",
        ])
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

fn manifest(root: &Path) -> Value {
    let output = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args([
            "--manifest",
            "--format",
            "json",
            "--base",
            "review-base",
            "--head",
            "HEAD",
            "--max-results",
            "1000",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn paths(report: &Value, category: &str) -> BTreeSet<String> {
    report[category]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap().into())
        .collect()
}

fn target_matches(candidate: &Value, label: &Value) -> bool {
    let evidence = &candidate["evidence"];
    let target = if evidence["snapshot"] == "base" {
        label.get("base_target").unwrap_or(&label["target"])
    } else {
        &label["target"]
    };
    evidence["depth"] == label["depth"]
        && evidence["target"]["path"] == *target
        && evidence["target"]["name"] == label["name"]
}

fn labels(case: &Value, report: &Value) {
    let id = case["id"].as_str().unwrap();
    let mut expected: BTreeSet<String> = case["confirmed"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    if let Some(n) = case["fanout"].as_u64() {
        expected.extend((0..n).map(|i| format!("tests/fan/{i:04}.test.ts")));
    }
    assert_eq!(
        paths(report, "confirmed"),
        expected,
        "{id}: incorrect confirmed test file"
    );
    for candidate in report["confirmed"].as_array().unwrap() {
        let path = candidate["path"].as_str().unwrap();
        if let Some(label) = case["confirmed"].get(path) {
            assert!(
                target_matches(candidate, label),
                "{id}: incorrect target/depth at {path}: {candidate}"
            );
        } else {
            assert_eq!(candidate["evidence"]["target"]["path"], "src/math.ts");
            assert_eq!(candidate["evidence"]["depth"], 1);
        }
        assert!(!candidate["linked_changes"].as_array().unwrap().is_empty());
    }
    for category in ["possible", "unknown"] {
        let actual = paths(report, category);
        for path in case[category].as_array().unwrap() {
            assert!(
                actual.contains(path.as_str().unwrap()),
                "{id}: missing {category} {path}"
            );
        }
    }
    if let Some(wrong) = case["wrong_targets"].as_array() {
        for path in wrong {
            assert!(!paths(report, "confirmed").contains(path.as_str().unwrap()));
        }
    }
    if case["baseline_only_evidence"] == true {
        assert!(
            report["confirmed"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["evidence"]["snapshot"] == "base")
        );
    }
    if let Some(presence) = case["presence"].as_object() {
        for (path, expected) in presence {
            let actual = report["confirmed"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["path"] == path.as_str())
                .unwrap();
            let labels: Vec<_> = actual["snapshots"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(serde_json::to_value(labels).unwrap(), *expected);
        }
    }
}

fn stable(mut report: Value) -> Value {
    for side in ["base", "head"] {
        for key in ["seconds", "index", "files_reparsed", "seeded"] {
            report[side].as_object_mut().unwrap().remove(key);
        }
    }
    report
}

#[test]
fn git_history_discovery_matches_independent_files_targets_and_depths() {
    let spec: Value =
        serde_json::from_str(&fs::read_to_string(fixtures().join("cases.json")).unwrap()).unwrap();
    for case in spec["cases"].as_array().unwrap() {
        let repo = repository();
        apply(repo.path(), case);
        let first = run(repo.path(), &[]);
        labels(case, &first);
        let artifact = manifest(repo.path());
        labels(case, &artifact["verification"]);
        assert_eq!(artifact["completeness"]["output_truncated"], false);
        assert_eq!(artifact["verification"]["behavioral_coverage"], "unknown");
        assert_eq!(
            artifact,
            manifest(repo.path()),
            "manifest identity is not stable"
        );
        let again = run(repo.path(), &[]);
        assert_eq!(stable(first), stable(again));
        assert!(!repo.path().join("ATLAS_MUST_NOT_RUN").exists());
    }
}

#[test]
fn grammar_gap_diagnostics_do_not_become_confirmed_links_or_complete_coverage() {
    let repo = repository();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/typescript-grammar");
    let mut partial = Vec::new();
    for name in [
        "import-type.ts",
        "import-type.tsx",
        "import-member.ts",
        "import-member.tsx",
        "malformed.ts",
        "shadowed.ts",
        "reassigned.ts",
        "clean.ts",
        "clean.tsx",
    ] {
        let (stem, ext) = name.rsplit_once('.').unwrap();
        let path = format!("tests/grammar/{stem}.test.{ext}");
        let text = fs::read_to_string(source.join(name))
            .unwrap()
            .replace("'./module'", "'../../src/math'")
            .replace("target as", "calc as");
        write(repo.path(), &path, &text);
        if !name.starts_with("clean.") {
            partial.push(path);
        }
    }
    git(repo.path(), &["add", "-A", "--force"]);
    git(repo.path(), &["commit", "-q", "-m", "grammar fixtures"]);
    git(repo.path(), &["tag", "review-base"]);
    let math = fs::read_to_string(repo.path().join("src/math.ts")).unwrap();
    write(repo.path(), "src/math.ts", &math.replace("n + 1", "n + 2"));
    git(repo.path(), &["add", "-A", "--force"]);
    git(repo.path(), &["commit", "-q", "-m", "changed calc"]);
    let report = run(repo.path(), &[]);
    let confirmed = paths(&report, "confirmed");
    assert!(confirmed.contains("tests/grammar/clean.test.ts"));
    assert!(confirmed.contains("tests/grammar/clean.test.tsx"));
    for path in partial {
        assert!(!confirmed.contains(&path), "{path}");
        let unknown = report["unknown"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["path"] == path)
            .unwrap();
        assert!(
            unknown["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r.as_str().unwrap().contains("syntax errors"))
        );
    }
    assert_eq!(report["completeness"]["source_coverage_unknown"], true);
    assert_eq!(report["completeness"]["static_links_complete"], false);
    assert_eq!(report["completeness"]["complete"], false);
}

#[test]
fn weak_summaries_preserve_candidates_and_strong_rows_are_individual() {
    let repo = repository();
    for i in 0..40 {
        write(
            repo.path(),
            &format!("src/near-{i}.test.ts"),
            "test('unrelated', () => 0);\n",
        );
    }
    git(repo.path(), &["add", "-A", "--force"]);
    git(repo.path(), &["commit", "-q", "-m", "proximity fixtures"]);
    git(repo.path(), &["tag", "review-base"]);
    let math = fs::read_to_string(repo.path().join("src/math.ts")).unwrap();
    write(repo.path(), "src/math.ts", &math.replace("n + 1", "n + 2"));
    git(repo.path(), &["add", "-A", "--force"]);
    git(repo.path(), &["commit", "-q", "-m", "changed calc"]);
    let full = run(repo.path(), &[]);
    let render = |args: &[&str]| {
        let output = Command::new(BINARY)
            .arg("impact")
            .arg(repo.path())
            .args([
                "--tests",
                "--base",
                "review-base",
                "--head",
                "HEAD",
                "--max-bytes",
                "8000000",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let default = render(&[]);
    let detailed = render(&["--detail"]);
    assert!(default.contains("weak heuristic: colocation only: src/ (40 possible candidates"));
    assert!(default.contains("weak heuristic: basename only"));
    assert!(!default.contains("src/near-0.test.ts [possible]"));
    assert!(!default.lines().next().unwrap().contains("truncated"));
    for row in full["possible"].as_array().unwrap() {
        let path = row["path"].as_str().unwrap();
        assert!(detailed.contains(&format!("{path} [possible]")), "{path}");
        let strong = row.get("evidence").is_some()
            || row["reasons"].as_array().unwrap().iter().any(|r| {
                let r = r.as_str().unwrap();
                !r.starts_with("colocated with") && !r.starts_with("source/test basename match")
            });
        if strong {
            assert!(default.contains(&format!("{path} [possible]")), "{path}");
        }
    }
    let selected = render(&["--evidence-file", "src/near-0.test.ts"]);
    assert!(selected.contains("src/near-0.test.ts [possible]"));
    assert!(selected.contains("weak heuristic:"));
    assert!(!selected.contains("summarized possible candidates"));
    assert_eq!(stable(full), stable(run(repo.path(), &[])));
}

#[test]
fn wrong_target_is_rejected_even_with_the_same_test_and_resolved_relationship_count() {
    let candidate = serde_json::json!({"evidence":{"snapshot":"head","depth":1,"target":{"path":"src/other/math.ts","name":"calc"}}});
    let label = serde_json::json!({"depth":1,"target":"src/math.ts","name":"calc"});
    assert!(!target_matches(&candidate, &label));
}

#[test]
fn optional_directories_and_evidence_are_explicit_and_root_safe() {
    let repo = repository();
    let root = repo.path();
    git(root, &["tag", "review-base"]);
    write(
        root,
        "src/math.ts",
        "export function calc(n: number) { return n + 2; }\n",
    );
    write(
        root,
        "custom/arbitrary.ts",
        "import { calc } from '../src/math';\ncalc(2);\n",
    );
    git(root, &["add", "-A", "--force"]);
    git(root, &["commit", "-q", "-m", "change"]);
    assert!(!paths(&run(root, &[]), "confirmed").contains("custom/arbitrary.ts"));
    assert!(
        paths(&run(root, &["--test-dir", "custom"]), "confirmed").contains("custom/arbitrary.ts")
    );
    let selected = run(root, &["--evidence-file", "tests/math.test.ts", "--detail"]);
    assert_eq!(
        paths(&selected, "confirmed"),
        BTreeSet::from(["tests/math.test.ts".into()])
    );
    assert_eq!(selected["completeness"]["complete"], false);
    assert!(
        selected["excluded_by_selection"]["confirmed"]
            .as_u64()
            .unwrap()
            > 0
    );
    let invalid = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args(["--tests", "--test-dir", "../outside"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
}

#[test]
fn budgets_count_omissions_without_reclassifying_candidates() {
    let spec: Value =
        serde_json::from_str(&fs::read_to_string(fixtures().join("cases.json")).unwrap()).unwrap();
    let case = spec["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "large-test-fanout")
        .unwrap();
    let repo = repository();
    apply(repo.path(), case);
    let full = run(repo.path(), &[]);
    for budget in [2000, 3000, 8000] {
        let output = Command::new(BINARY)
            .arg("impact")
            .arg(repo.path())
            .args([
                "--tests",
                "--base",
                "review-base",
                "--head",
                "HEAD",
                "--format",
                "json",
                "--max-results",
                "1000",
                "--max-bytes",
                &budget.to_string(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "budget {budget}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.len() <= budget);
        let limited: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["completeness"]["complete"], false);
        assert_eq!(limited["totals"], full["totals"]);
        let count = limited["confirmed"].as_array().unwrap().len() as u64;
        assert_eq!(
            count + limited["omitted"]["confirmed"].as_u64().unwrap_or(0),
            full["totals"]["confirmed"].as_u64().unwrap()
        );
        assert!(paths(&limited, "confirmed").is_subset(&paths(&full, "confirmed")));
    }
}

#[test]
fn an_excluded_test_is_unindexed_in_head_and_never_called_deleted() {
    let repo = repository();
    let root = repo.path();
    git(root, &["tag", "review-base"]);
    write(root, ".gitignore", "ignored/\ntests/math.test.ts\n");
    write(
        root,
        "src/math.ts",
        "export function calc(n: number) { return n + 2; }\n",
    );
    let output = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args([
            "--tests",
            "--base",
            "review-base",
            "--format",
            "json",
            "--detail",
            "--max-bytes",
            "8000000",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let test = report["inventory"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "tests/math.test.ts")
        .unwrap();
    assert_eq!(test["snapshots"]["base"], "supported_indexed");
    assert_eq!(test["snapshots"]["head"], "recognized_unindexed");
    assert!(paths(&report, "unknown").contains("tests/math.test.ts"));
    assert!(paths(&report, "confirmed").contains("tests/math.test.ts"));
    assert_eq!(report["completeness"]["complete"], false);
}

#[test]
fn minimal_supported_profile_enumeration_can_be_complete_without_a_safety_claim() {
    let repo = TempDir::new().unwrap();
    let root = repo.path();
    write(root, "calc.ts", "export function calc() { return 1; }\n");
    write(
        root,
        "calc.test.ts",
        "import { calc } from './calc';\ncalc();\n",
    );
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["tag", "review-base"]);
    write(root, "calc.ts", "export function calc() { return 2; }\n");
    git(root, &["commit", "-q", "-am", "change"]);
    let report = run(root, &[]);
    assert_eq!(report["completeness"]["static_links_complete"], true);
    assert_eq!(report["completeness"]["complete"], true);
    assert_eq!(
        paths(&report, "confirmed"),
        BTreeSet::from(["calc.test.ts".into()])
    );
}

#[test]
fn committed_inventory_uses_git_manifest_data_after_cache_source_tampering() {
    let repo = repository();
    let root = repo.path();
    git(root, &["tag", "review-base"]);
    write(
        root,
        "src/math.ts",
        "export function calc(n: number) { return n + 2; }\n",
    );
    git(root, &["commit", "-q", "-am", "change"]);
    let first = run(root, &[]);
    for entry in fs::read_dir(root.join(".code-intel/impact/trees")).unwrap() {
        let manifest = entry.unwrap().path().join("src/package.json");
        if manifest.is_file() {
            fs::write(
                manifest,
                "{\"directories\":{\"test\":\"wrong-directory\"}}\n",
            )
            .unwrap();
        }
    }
    let again = run(root, &[]);
    assert_eq!(
        first["base_inventory_digest"],
        again["base_inventory_digest"]
    );
    assert_eq!(
        first["head_inventory_digest"],
        again["head_inventory_digest"]
    );
    assert_eq!(paths(&first, "confirmed"), paths(&again, "confirmed"));
    assert!(paths(&again, "confirmed").contains("integration/custom.ts"));
}

#[test]
fn source_changes_during_discovery_prevent_freshness_and_completeness_claims() {
    let repo = repository();
    let root = repo.path();
    git(root, &["tag", "review-base"]);
    write(
        root,
        "src/math.ts",
        "export function calc(n: number) { return n + 2; }\n",
    );
    let child = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args([
            "--tests",
            "--base",
            "review-base",
            "--format",
            "json",
            "--max-bytes",
            "8000000",
        ])
        .env("CODE_INTEL_TEST_PAUSE_AFTER_SNAPSHOTS_MS", "1500")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    write(
        root,
        "tests/alias.test.ts",
        "export const changedDuringAnalysis = true;\n",
    );
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["completeness"]["snapshots_fresh"], false);
    assert_eq!(report["completeness"]["static_links_complete"], false);
    assert_eq!(report["completeness"]["complete"], false);
}

#[test]
fn root_colocation_and_root_test_directory_never_imply_confirmed_usage() {
    let repo = TempDir::new().unwrap();
    let root = repo.path();
    write(root, "calc.ts", "export function calc() { return 1; }\n");
    write(root, "nearby.test.ts", "export const unrelated = 42;\n");
    write(
        root,
        "__tests__/nearby.ts",
        "export const unrelated = 42;\n",
    );
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["tag", "review-base"]);
    write(root, "calc.ts", "export function calc() { return 2; }\n");
    git(root, &["commit", "-q", "-am", "change"]);
    let report = run(root, &[]);
    assert!(paths(&report, "confirmed").is_empty());
    assert_eq!(
        paths(&report, "possible"),
        BTreeSet::from(["nearby.test.ts".into(), "__tests__/nearby.ts".into()])
    );
}
