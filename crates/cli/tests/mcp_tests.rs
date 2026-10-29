//! `atlas_impact` with `tests: true` (Phase 4.6): MCP returns the analysis and
//! renderer of `repogi impact --tests`. Expected files, targets and depths
//! are the independent Phase 4.5 labels in `fixtures/affected-tests/cases.json`,
//! never Atlas output. Nothing in the fixture repository is executed: its
//! `package.json` test script and `vitest.config.ts` are traps.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");
const TIMEOUT: Duration = Duration::from_secs(60);

struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next: i64,
}

impl Server {
    fn start(root: &Path, options: &[&str]) -> Self {
        let mut child = Command::new(BINARY)
            .arg("mcp")
            .args(options)
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take().unwrap();
        let mut server = Self {
            child,
            stdin,
            lines,
            next: 1,
        };
        server.request("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}));
        server.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        server
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let mut message = serde_json::Map::new();
        message.insert("jsonrpc".into(), json!("2.0"));
        message.insert("id".into(), json!(id));
        message.insert("method".into(), json!(method));
        message.insert("params".into(), params);
        self.send(&Value::Object(message));
        let line = self.lines.recv_timeout(TIMEOUT).expect("response");
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        response
    }

    /// (text, isError)
    fn call(&mut self, arguments: Value) -> (String, bool) {
        let mut params = serde_json::Map::new();
        params.insert("name".into(), json!("atlas_impact"));
        params.insert("arguments".into(), arguments);
        let response = self.request("tools/call", Value::Object(params));
        let result = &response["result"];
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap(),
        )
    }

    fn ok(&mut self, arguments: Value) -> String {
        let (text, error) = self.call(arguments);
        assert!(!error, "{text}");
        text
    }

    fn json(&mut self, mut arguments: Value) -> Value {
        arguments["format"] = json!("json");
        serde_json::from_str(&self.ok(arguments)).unwrap()
    }

    fn error_code(&mut self, arguments: &Value) -> String {
        let (text, error) = self.call(arguments.clone());
        assert!(error, "expected an error for {arguments}: {text}");
        text.strip_prefix("error[")
            .and_then(|rest| rest.split(']').next())
            .unwrap()
            .to_owned()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/affected-tests")
}

fn cases() -> Vec<Value> {
    let spec: Value =
        serde_json::from_str(&fs::read_to_string(fixtures().join("cases.json")).unwrap()).unwrap();
    spec["cases"].as_array().unwrap().clone()
}

fn case(id: &str) -> Value {
    cases().into_iter().find(|c| c["id"] == id).unwrap()
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

fn git(root: &Path, args: &[&str]) -> String {
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
    String::from_utf8(out.stdout).unwrap()
}

/// The fixture at its base commit; the root is canonical, as MCP requires.
fn repository() -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    copy(&fixtures().join("repo"), temp.path());
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["add", "-A", "--force"]);
    git(temp.path(), &["commit", "-q", "-m", "base"]);
    let root = temp.path().canonicalize().unwrap();
    (temp, root)
}

fn write(root: &Path, path: &str, content: &str) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, content).unwrap();
}

/// Tags `review-base`, then commits the case's literal edits (as in Phase 4.5).
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

fn cli(root: &Path, args: &[&str]) -> String {
    let output = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// The report without per-run provenance (timings, cache use).
fn stable(mut report: Value) -> Value {
    for side in ["base", "head"] {
        for key in ["seconds", "index", "files_reparsed", "seeded"] {
            report[side].as_object_mut().unwrap().remove(key);
        }
    }
    report
}

fn paths(report: &Value, category: &str) -> BTreeSet<String> {
    report[category]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap().into())
        .collect()
}

/// Candidate paths per agent-text section, so a formatting change can never
/// move an uncertain candidate into the confirmed section unnoticed.
fn sections(text: &str) -> BTreeMap<&'static str, BTreeSet<String>> {
    let mut found: BTreeMap<_, BTreeSet<String>> = BTreeMap::new();
    let mut current = None;
    for line in text.lines() {
        if line.starts_with("confirmed linked tests") {
            current = Some("confirmed");
        } else if line.starts_with("possible candidates") {
            current = Some("possible");
        } else if !line.starts_with(' ') && !line.is_empty() {
            current = None;
        } else if let (Some(section), Some(row)) = (current, line.strip_prefix("  ")) {
            if !row.starts_with(' ') && !row.starts_with('…') && row.contains(" [") {
                let path = row.split(" [").next().unwrap();
                found.entry(section).or_default().insert(path.into());
            }
        }
    }
    found
}

/// The text never lists a path in a different category than the JSON plan.
fn consistent(text: &str, report: &Value) {
    let found = sections(text);
    for category in ["confirmed", "possible"] {
        let listed = found.get(category).cloned().unwrap_or_default();
        assert!(
            listed.is_subset(&paths(report, category)),
            "{category} text rows {listed:?} differ from JSON"
        );
    }
}

/// MCP agrees with `repogi impact --tests` on the same inputs: JSON minus
/// per-run provenance, and byte-identical default and detailed agent text.
fn agrees(server: &mut Server, root: &Path, arguments: &Value, cli_args: &[&str]) -> Value {
    let mut args = arguments.clone();
    args["tests"] = json!(true);
    args["max_bytes"] = json!(32000);
    let report = server.json(args.clone());
    let mut command = vec!["--tests", "--format", "json", "--max-bytes", "32000"];
    command.extend_from_slice(cli_args);
    let expected: Value = serde_json::from_str(&cli(root, &command)).unwrap();
    assert_eq!(
        stable(report.clone()),
        stable(expected),
        "MCP JSON differs from the CLI"
    );
    let mut text_args = arguments.clone();
    text_args["tests"] = json!(true);
    let text = server.ok(text_args);
    let mut command = vec!["--tests", "--max-bytes", "8000"];
    command.extend_from_slice(cli_args);
    assert_eq!(text, cli(root, &command), "MCP agent text differs");
    consistent(&text, &report);
    args.as_object_mut().unwrap().remove("format");
    args["detail"] = json!(true);
    let detail = server.ok(args);
    let mut command = vec!["--tests", "--detail", "--max-bytes", "32000"];
    command.extend_from_slice(cli_args);
    assert_eq!(detail, cli(root, &command), "MCP detailed text differs");
    consistent(&detail, &report);
    report
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

/// The Phase 4.5 label check, applied to the MCP response.
fn labelled(case: &Value, report: &Value) {
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
        "{id}: confirmed files"
    );
    for candidate in report["confirmed"].as_array().unwrap() {
        let path = candidate["path"].as_str().unwrap();
        if let Some(label) = case["confirmed"].get(path) {
            assert!(
                target_matches(candidate, label),
                "{id}: target/depth at {path}: {candidate}"
            );
        }
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
    for path in case["wrong_targets"].as_array().into_iter().flatten() {
        assert!(!paths(report, "confirmed").contains(path.as_str().unwrap()));
    }
    if case["baseline_only_evidence"] == true {
        assert!(
            report["confirmed"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["evidence"]["snapshot"] == "base"),
            "{id}: baseline evidence"
        );
    }
}

fn tests_only(root: &Path) -> Server {
    Server::start(root, &["--tools", "atlas_impact,atlas_status"])
}

fn committed() -> (Value, [&'static str; 4]) {
    (
        json!({"base":"review-base","head":"HEAD"}),
        ["--base", "review-base", "--head", "HEAD"],
    )
}

/// Scenarios 1–6, 11, 12 and the remaining Phase 4.5 history cases: every
/// independently labelled file, target and depth survives the MCP adapter.
#[test]
fn labelled_scenarios_match_the_cli_and_the_independent_labels() {
    let (arguments, cli_args) = committed();
    for case in cases() {
        if case["fanout"].is_u64() {
            continue; // Exceeds the 32 KB MCP cap: see the truncation test.
        }
        let (_temp, root) = repository();
        apply(&root, &case);
        let mut server = tests_only(&root);
        let report = agrees(&mut server, &root, &arguments, &cli_args);
        assert_eq!(report["truncated"], false, "{}", case["id"]);
        labelled(&case, &report);
        let id = case["id"].as_str().unwrap();
        let confirmed = paths(&report, "confirmed");
        match id {
            "function-and-unit" => {
                let unit = &report["confirmed"][0];
                assert_eq!(unit["priority"], "direct");
                assert_eq!(unit["evidence"]["depth"], 1);
            }
            "transitive-integration" => {
                let integration = report["confirmed"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["path"] == "tests/integration.spec.ts")
                    .unwrap();
                assert_eq!(integration["priority"], "transitive");
                // Direct before transitive.
                let depths: Vec<_> = report["confirmed"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c["evidence"]["depth"].as_u64().unwrap())
                    .collect();
                assert!(depths.is_sorted(), "{depths:?}");
                // Scenario 10: an import without a use stays possible.
                let unused = report["possible"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["path"] == "tests/unused.test.ts")
                    .unwrap();
                assert_eq!(unused["priority"], "possible");
                assert_eq!(unused["evidence"]["relationship"], "IMPORTS");
            }
            "react-tsx" => {
                let jsx = &report["confirmed"][0];
                assert_eq!(jsx["path"], "tests/component.test.tsx");
                assert_eq!(jsx["evidence"]["relationship"], "REFERENCES");
            }
            "deleted-export" => {
                assert!(
                    report["changed"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|c| c["change"] == "removed")
                );
            }
            "test-naming-collision" => {
                assert!(!confirmed.contains("tests/collision/math.test.ts"));
                assert!(paths(&report, "possible").contains("tests/collision/math.test.ts"));
            }
            "no-known-test-consumer" => {
                assert!(confirmed.is_empty());
                assert!(
                    report["gaps"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|g| g["declaration"] == "untested")
                );
                let text = server.ok(json!({"tests":true,"base":"review-base","head":"HEAD"}));
                assert!(text.contains("src/math.ts#untested: no confirmed test linkage"));
            }
            "test-added-and-removed" => {
                let text = server.ok(json!({"tests":true,"base":"review-base","head":"HEAD"}));
                assert!(text.contains(
                    "tests/math.test.ts [direct] (baseline only; not observed in the changed state)"
                ));
                assert!(text.contains("tests/new.test.ts [direct] (changed state only)"));
            }
            _ => {}
        }
        // A test with no semantically supported snapshot is never confirmed.
        for file in report["unknown"].as_array().unwrap() {
            let supported = file["snapshots"]
                .as_object()
                .unwrap()
                .values()
                .any(|class| class == "supported_indexed" || class == "possible_convention");
            let path = file["path"].as_str().unwrap();
            assert!(
                supported || !confirmed.contains(path),
                "{id}: {path} confirmed"
            );
        }
        assert!(!root.join("ATLAS_MUST_NOT_RUN").exists());
    }
}

/// Scenario 7: a changed test file is reviewed, not assumed to be covered.
#[test]
fn a_changed_test_file_is_listed_for_its_own_review() {
    let (_temp, root) = repository();
    git(&root, &["tag", "review-base"]);
    write(
        &root,
        "tests/math.test.ts",
        "import { compute } from '../src/barrel';\ntest('unit', () => compute(2));\n",
    );
    let mut server = tests_only(&root);
    let report = agrees(
        &mut server,
        &root,
        &json!({"base":"review-base"}),
        &["--base", "review-base"],
    );
    let changed = report["possible"]
        .as_array()
        .unwrap()
        .iter()
        .chain(report["confirmed"].as_array().unwrap())
        .find(|c| c["path"] == "tests/math.test.ts")
        .expect("changed test file listed");
    assert!(
        changed["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().starts_with("test file changed")),
        "{changed}"
    );
    // A test edit changes no production declaration: nothing is confirmed.
    assert!(paths(&report, "confirmed").is_empty());
}

/// Scenarios 8, 9 and 13: unindexed JavaScript, syntax errors and runner
/// configuration stay unknown or uninterpreted, never confirmed or complete.
#[test]
fn unknown_coverage_and_unsupported_configuration_stay_distinct() {
    let (_temp, root) = repository();
    git(&root, &["tag", "review-base"]);
    // Embedded Jest settings are executable configuration: never evaluated.
    write(
        &root,
        "package.json",
        "{\"directories\": {\"test\": \"integration\"}, \"jest\": {\"testMatch\": [\"**/*.check.ts\"]}, \"scripts\": {\"test\": \"touch ATLAS_MUST_NOT_RUN\"}}\n",
    );
    write(
        &root,
        "src/math.ts",
        "export function calc(n: number) { return n + 9; }\nexport function untested() { return 42; }\n",
    );
    let mut server = tests_only(&root);
    let report = agrees(
        &mut server,
        &root,
        &json!({"base":"review-base"}),
        &["--base", "review-base"],
    );
    let unknown: BTreeMap<_, _> = report["unknown"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap(), f))
        .collect();
    let js = unknown["tests/dynamic.test.js"];
    assert_eq!(js["snapshots"]["head"], "unsupported_or_excluded");
    assert!(
        js["reasons"]
            .to_string()
            .contains("unsupported test language")
    );
    let broken = unknown["tests/broken.test.ts"];
    assert_eq!(broken["snapshots"]["head"], "recognized_unindexed");
    assert!(broken["reasons"].to_string().contains("syntax errors"));
    // Possibly relevant by colocation, but never confirmed: no supported semantics.
    for path in ["tests/dynamic.test.js", "tests/broken.test.ts"] {
        assert!(!paths(&report, "confirmed").contains(path));
    }
    let configurations = report["configurations"].as_array().unwrap();
    let vitest = configurations
        .iter()
        .find(|c| c["path"] == "vitest.config.ts" && c["snapshot"] == "head")
        .unwrap();
    assert_eq!(vitest["interpreted"], false);
    let manifest = configurations
        .iter()
        .find(|c| c["path"] == "package.json" && c["snapshot"] == "head")
        .unwrap();
    assert_eq!(manifest["interpreted"], false);
    assert!(
        manifest["hints"]
            .to_string()
            .contains("embedded Jest configuration not evaluated")
    );
    let completeness = &report["completeness"];
    assert_eq!(completeness["inventory_complete"], false);
    assert_eq!(completeness["source_coverage_unknown"], true);
    assert_eq!(completeness["static_links_complete"], false);
    assert_eq!(completeness["complete"], false);
    let text = server.ok(json!({"tests":true,"base":"review-base"}));
    assert!(
        text.contains("\nunknown test coverage:\n  generated/excluded.test.ts"),
        "{text}"
    );
    assert!(text.contains("tests/broken.test.ts"));
    assert!(!root.join("ATLAS_MUST_NOT_RUN").exists());
}

/// Scenario 14 and argument validation: literal, confined directories only;
/// the same combinations are refused as in the CLI.
#[cfg(unix)]
#[test]
fn test_dirs_and_evidence_paths_are_literal_confined_and_validated() {
    let (_temp, root) = repository();
    git(&root, &["tag", "review-base"]);
    write(
        &root,
        "src/math.ts",
        "export function calc(n: number) { return n + 2; }\nexport function untested() { return 42; }\n",
    );
    write(
        &root,
        "custom/arbitrary.ts",
        "import { calc } from '../src/math';\ncalc(2);\n",
    );
    git(&root, &["add", "-A", "--force"]);
    git(&root, &["commit", "-q", "-m", "change"]);
    let (arguments, cli_args) = committed();
    let mut server = tests_only(&root);
    let plain = agrees(&mut server, &root, &arguments, &cli_args);
    assert!(!paths(&plain, "confirmed").contains("custom/arbitrary.ts"));
    let mut custom = arguments.clone();
    custom["test_dirs"] = json!(["custom"]);
    let mut custom_cli = cli_args.to_vec();
    custom_cli.extend(["--test-dir", "custom"]);
    let report = agrees(&mut server, &root, &custom, &custom_cli);
    assert!(paths(&report, "confirmed").contains("custom/arbitrary.ts"));
    custom["test_dirs"] = json!(["custom/"]);
    custom["tests"] = json!(true);
    custom["format"] = json!("json");
    custom["max_bytes"] = json!(32000);
    assert_eq!(
        stable(serde_json::from_str(&server.ok(custom)).unwrap()),
        stable(report)
    );
    std::os::unix::fs::symlink(root.join("custom"), root.join("linked")).unwrap();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", root.join("tests/escape.test.ts")).unwrap();
    let many: Vec<_> = (0..17).map(|i| format!("dir{i}")).collect();
    for (arguments, code) in [
        (
            json!({"tests":true,"test_dirs":["../outside"]}),
            "invalid_path",
        ),
        (json!({"tests":true,"test_dirs":["/etc"]}), "invalid_path"),
        (
            json!({"tests":true,"test_dirs":["tests/*"]}),
            "invalid_path",
        ),
        (json!({"tests":true,"test_dirs":["a\\b"]}), "invalid_path"),
        (
            json!({"tests":true,"test_dirs":["./tests"]}),
            "invalid_path",
        ),
        (json!({"tests":true,"test_dirs":[""]}), "invalid_path"),
        (json!({"tests":true,"test_dirs":["linked"]}), "invalid_path"),
        (
            json!({"tests":true,"test_dirs":["escape/tests"]}),
            "invalid_path",
        ),
        (json!({"tests":true,"test_dirs":many}), "invalid_arguments"),
        (
            json!({"tests":true,"test_dirs":"custom"}),
            "invalid_arguments",
        ),
        (json!({"test_dirs":["custom"]}), "invalid_arguments"),
        (json!({"tests":false,"test_dirs":[]}), "invalid_arguments"),
        (json!({"tests":true,"summary":true}), "invalid_arguments"),
        (
            json!({"tests":true,"summary":true,"detail":true}),
            "invalid_arguments",
        ),
        (json!({"tests":"yes"}), "invalid_arguments"),
        (
            json!({"tests":true,"test_dir":["custom"]}),
            "invalid_arguments",
        ),
        (
            json!({"tests":true,"cache_dir":"/tmp"}),
            "invalid_arguments",
        ),
        (json!({"tests":true,"max_bytes":1000}), "invalid_arguments"),
        (
            json!({"tests":true,"evidence_file":"../outside.test.ts"}),
            "invalid_path",
        ),
        (
            json!({"tests":true,"evidence_file":"/etc/hostname"}),
            "invalid_path",
        ),
        (
            json!({"tests":true,"evidence_file":"tests/escape.test.ts"}),
            "invalid_path",
        ),
        (
            json!({"tests":true,"evidence_file":"escape/x.test.ts"}),
            "invalid_path",
        ),
        (
            json!({"tests":true,"evidence_file":"src/math.ts"}),
            "test_not_found",
        ),
        (
            json!({"tests":true,"base":"no-such-branch"}),
            "invalid_revision",
        ),
        (json!({"tests":true,"root":"/"}), "root_not_allowed"),
    ] {
        assert_eq!(server.error_code(&arguments), code, "{arguments}");
    }
    // The CLI refuses the same combinations.
    for args in [
        &["--tests", "--summary"][..],
        &["--test-dir", "custom"],
        &["--tests", "--test-dir", "../outside"],
    ] {
        let status = Command::new(BINARY)
            .arg("impact")
            .arg(&root)
            .args(args)
            .output()
            .unwrap()
            .status;
        assert!(!status.success(), "{args:?}");
    }
    // `tests: false` is the existing impact tool, byte for byte.
    assert_eq!(server.ok(json!({"tests":false})), server.ok(json!({})));
}

/// Invariants of a budget-fitted plan relative to the complete one.
fn fitted(limited: &Value, full: &Value, budget: u64) {
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["completeness"]["complete"], false);
    assert_eq!(limited["completeness"]["static_links_complete"], false);
    assert_eq!(limited["totals"], full["totals"]);
    assert_eq!(limited["completeness"]["source_coverage_unknown"], true);
    for key in [
        "confirmed",
        "possible",
        "unknown",
        "gaps",
        "changed",
        "limitations",
    ] {
        assert_eq!(
            limited[key].as_array().unwrap().len() as u64
                + limited["omitted"][key].as_u64().unwrap_or(0),
            full["totals"][key].as_u64().unwrap(),
            "{budget}: {key}"
        );
    }
    assert!(paths(limited, "confirmed").is_subset(&paths(full, "confirmed")));
    assert!(paths(limited, "possible").is_subset(&paths(full, "possible")));
    // Global uncertainty outlasts per-file rows.
    if limited["omitted"].get("limitations").is_some() {
        for key in ["possible", "unknown", "gaps"] {
            assert!(
                limited[key].as_array().unwrap().is_empty(),
                "{budget}: {key}"
            );
        }
    }
}

/// Scenario 15: whole rows are dropped with counts,
/// confirmed rows last and global limitations after per-file rows; JSON stays
/// valid and nothing is reclassified at any budget.
#[test]
fn truncation_is_explicit_and_never_reclassifies() {
    let fanout = case("large-test-fanout");
    let (_temp, root) = repository();
    apply(&root, &fanout);
    let (arguments, cli_args) = committed();
    let full: Value = serde_json::from_str(&cli(
        &root,
        &[
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
        ],
    ))
    .unwrap();
    labelled(&fanout, &full);
    let mut server = tests_only(&root);
    for budget in [2000_u64, 4000, 8000, 32000] {
        let mut args = arguments.clone();
        args["tests"] = json!(true);
        args["max_bytes"] = json!(budget);
        args["max_results"] = json!(1000);
        let mut text_args = args.clone();
        args["format"] = json!("json");
        let text = server.ok(args);
        assert!(text.len() as u64 <= budget);
        let limited: Value = serde_json::from_str(&text).unwrap();
        let mut command = cli_args.to_vec();
        let budget_text = budget.to_string();
        command.extend([
            "--tests",
            "--format",
            "json",
            "--max-results",
            "1000",
            "--max-bytes",
            &budget_text,
        ]);
        assert_eq!(
            stable(limited.clone()),
            stable(serde_json::from_str(&cli(&root, &command)).unwrap())
        );
        fitted(&limited, &full, budget);
        let agent = server.ok(text_args.clone());
        assert!(agent.len() as u64 <= budget);
        assert!(
            agent.lines().next().unwrap().ends_with("· truncated"),
            "{agent}"
        );
        assert!(agent.contains(" omitted\n"));
        let found = sections(&agent);
        assert!(
            found
                .get("confirmed")
                .cloned()
                .unwrap_or_default()
                .is_subset(&paths(&full, "confirmed"))
        );
        assert!(
            found
                .get("possible")
                .cloned()
                .unwrap_or_default()
                .is_subset(&paths(&full, "possible"))
        );
        text_args["detail"] = json!(true);
        assert!(server.ok(text_args).len() as u64 <= budget);
    }
}

/// Evidence for one selected test: exact path, scoped, never globally complete.
#[test]
fn selected_test_evidence_is_exact_and_scoped() {
    let fanout = case("large-test-fanout");
    let (_temp, root) = repository();
    apply(&root, &fanout);
    let (arguments, cli_args) = committed();
    let mut server = tests_only(&root);
    let mut select = arguments.clone();
    select["tests"] = json!(true);
    select["evidence_file"] = json!("tests/fan/0007.test.ts");
    select["max_bytes"] = json!(8000);
    let mut command = cli_args.to_vec();
    command.extend([
        "--tests",
        "--evidence-file",
        "tests/fan/0007.test.ts",
        "--max-bytes",
        "8000",
    ]);
    let text = server.ok(select.clone());
    assert_eq!(text, cli(&root, &command));
    assert!(text.contains("selected test: tests/fan/0007.test.ts"));
    assert!(
        text.contains(" (unique_static_binding) ["),
        "detailed rule and chain: {text}"
    );
    select["format"] = json!("json");
    let selected: Value = serde_json::from_str(&server.ok(select)).unwrap();
    command.extend(["--format", "json"]);
    assert_eq!(
        stable(selected.clone()),
        stable(serde_json::from_str(&cli(&root, &command)).unwrap())
    );
    assert_eq!(
        paths(&selected, "confirmed"),
        BTreeSet::from(["tests/fan/0007.test.ts".into()])
    );
    assert_eq!(selected["scope"], "tests/fan/0007.test.ts");
    assert_eq!(selected["truncated"], false);
    assert_eq!(selected["completeness"]["complete"], false);
    assert_eq!(selected["completeness"]["static_links_complete"], false);
    // Global totals survive selection: 7 labelled files plus the fan-out.
    let labelled =
        fanout["confirmed"].as_object().unwrap().len() as u64 + fanout["fanout"].as_u64().unwrap();
    assert_eq!(selected["totals"]["confirmed"], labelled);
    assert_eq!(
        selected["excluded_by_selection"]["confirmed"]
            .as_u64()
            .unwrap()
            + 1,
        labelled
    );
    assert!(!selected["limitations"].as_array().unwrap().is_empty());
    // An unknown-coverage test can be selected; it is not promoted.
    let broken = server.json(json!({"tests":true,"base":"review-base","head":"HEAD","evidence_file":"tests/broken.test.ts"}));
    assert_eq!(
        paths(&broken, "unknown"),
        BTreeSet::from(["tests/broken.test.ts".into()])
    );
    assert!(paths(&broken, "confirmed").is_empty());
}

/// Scenario 16: the working tree changes between a plan and an evidence
/// request. Distinct snapshot identities expose it; nothing stale is reused.
#[test]
fn snapshot_change_between_evidence_requests_is_visible() {
    let (_temp, root) = repository();
    git(&root, &["tag", "review-base"]);
    write(
        &root,
        "src/math.ts",
        "export function calc(n: number) { return n + 2; }\nexport function untested() { return 42; }\n",
    );
    let mut server = tests_only(&root);
    let plan = server.json(json!({"tests":true,"base":"review-base","max_bytes":32000}));
    assert!(paths(&plan, "confirmed").contains("tests/math.test.ts"));
    write(
        &root,
        "tests/math.test.ts",
        "export const noLongerUsesCompute = true;\n",
    );
    let arguments = json!({"tests":true,"base":"review-base","evidence_file":"tests/math.test.ts","max_bytes":32000});
    let evidence = server.json(arguments.clone());
    assert_eq!(plan["base"]["digest"], evidence["base"]["digest"]);
    assert_ne!(plan["head"]["digest"], evidence["head"]["digest"]);
    assert_eq!(
        plan["base_inventory_digest"],
        evidence["base_inventory_digest"]
    );
    assert_eq!(evidence["completeness"]["snapshots_fresh"], true);
    let expected: Value = serde_json::from_str(&cli(
        &root,
        &[
            "--tests",
            "--base",
            "review-base",
            "--evidence-file",
            "tests/math.test.ts",
            "--format",
            "json",
            "--max-bytes",
            "32000",
        ],
    ))
    .unwrap();
    assert_eq!(stable(evidence.clone()), stable(expected));
    // The head no longer uses the change; any remaining link is baseline evidence.
    for candidate in evidence["confirmed"].as_array().unwrap() {
        assert_eq!(candidate["evidence"]["snapshot"], "base", "{candidate}");
    }
    let text =
        server.ok(json!({"tests":true,"base":"review-base","evidence_file":"tests/math.test.ts"}));
    assert!(text.contains(evidence["head"]["digest"].as_str().unwrap()));
}

/// No repository code, runner or hook runs; Git, sources and the user index
/// are unchanged; only the impact cache is written.
#[test]
fn discovery_over_mcp_is_read_only() {
    let (_temp, root) = repository();
    git(&root, &["tag", "review-base"]);
    let hooks = root.join(".git/hooks");
    for hook in [
        "post-checkout",
        "post-index-change",
        "reference-transaction",
    ] {
        let path = hooks.join(hook);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\ntouch '{}'\n",
                root.join(format!("RAN-{hook}")).display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    write(
        &root,
        "src/math.ts",
        "export function calc(n: number) { return n + 2; }\nexport function untested() { return 42; }\n",
    );
    let state = |root: &Path| {
        (
            git(root, &["rev-parse", "HEAD", "refs/tags/review-base"]),
            git(
                root,
                &[
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignored",
                ],
            )
            .lines()
            .filter(|line| !line.contains(".code-intel/"))
            .map(str::to_owned)
            .collect::<Vec<_>>(),
            fs::read(root.join(".git/index")).unwrap(),
            fs::read(root.join("src/math.ts")).unwrap(),
        )
    };
    let before = state(&root);
    let mut server = tests_only(&root);
    for arguments in [
        json!({"tests":true,"base":"review-base"}),
        json!({"tests":true,"base":"review-base","format":"json","detail":true,"max_bytes":32000}),
        json!({"tests":true,"base":"review-base","evidence_file":"tests/math.test.ts","test_dirs":["integration"]}),
    ] {
        server.ok(arguments);
    }
    assert_eq!(state(&root), before);
    assert!(!root.join("ATLAS_MUST_NOT_RUN").exists());
    assert!(!root.join(".code-intel/index.db").exists());
    assert!(root.join(".code-intel/impact").is_dir());
    for entry in fs::read_dir(&root).unwrap() {
        let name = entry.unwrap().file_name();
        assert!(!name.to_string_lossy().starts_with("RAN-"), "{name:?} ran");
    }
}

/// The opt-in tool advertises the new arguments; default profiles are untouched.
#[test]
fn schemas_advertise_tests_only_on_the_opt_in_tool() {
    let (_temp, root) = repository();
    let definitions = |options: &[&str]| -> Vec<Value> {
        let mut server = Server::start(&root, options);
        server.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .clone()
    };
    for profile in [&[][..], &["--profile", "compact"]] {
        let tools = definitions(profile);
        assert!(tools.iter().all(|t| t["name"] != "atlas_impact"));
        assert!(!serde_json::to_string(&tools).unwrap().contains("test_dirs"));
    }
    let full = definitions(&["--tools", "atlas_impact"]);
    let properties = &full[0]["inputSchema"]["properties"];
    assert_eq!(properties["tests"]["type"], "boolean");
    assert_eq!(properties["test_dirs"]["maxItems"], 16);
    assert!(
        full[0]["description"]
            .as_str()
            .unwrap()
            .contains("tests=true")
    );
    let compact = definitions(&["--profile", "compact", "--tools", "atlas_impact"]);
    let properties = &compact[0]["inputSchema"]["properties"];
    assert_eq!(properties["tests"]["type"], "boolean");
    assert!(properties.get("test_dirs").is_none());
}
