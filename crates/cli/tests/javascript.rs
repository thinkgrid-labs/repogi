//! JavaScript and JSX against the hand-reviewed labels in
//! `fixtures/javascript/EXPECTED.json`. Every resolved edge must be labeled
//! (precision), every label must resolve (recall), and unconfirmed sites must
//! keep their status. Nothing in the fixture is executed.
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");
const RESOLVED_KINDS: [&str; 5] = ["IMPORTS", "CALLS", "REFERENCES", "EXTENDS", "RE_EXPORTS"];

fn repogi(root: &Path, args: &[&str]) -> Output {
    Command::new(BINARY)
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn run(root: &Path, args: &[&str]) -> String {
    let output = repogi(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn json(root: &Path, args: &[&str]) -> Value {
    serde_json::from_str(&run(root, args)).unwrap()
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Repogi Test",
            "-c",
            "user.email=repogi@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/javascript")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

/// A canonical copy of the fixture without its labels, indexed.
fn indexed_fixture() -> (TempDir, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    copy_dir(&fixture_dir(), &root);
    fs::remove_file(root.join("EXPECTED.json")).unwrap();
    fs::remove_file(root.join("README.md")).unwrap();
    json(&root, &["index", "."]);
    (temp, root)
}

fn graph(root: &Path) -> Vec<Value> {
    json(root, &["graph", "--limit", "10000"])["edges"]
        .as_array()
        .unwrap()
        .clone()
}

fn expected() -> Value {
    serde_json::from_str(&fs::read_to_string(fixture_dir().join("EXPECTED.json")).unwrap()).unwrap()
}

fn is_jsx(edge: &Value) -> bool {
    edge["evidence"]["syntax"]
        .as_str()
        .is_some_and(|syntax| syntax.ends_with("> JSX element"))
}

fn identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `(path, declared name)` of a target ID, read from the fixture source text
/// rather than from Repogi. A declared name must appear as a whole word on the
/// declaration's first line; `*` marks a file target.
fn declared(root: &Path, id: &str, name: &str) -> Option<String> {
    if let Some(path) = id.strip_prefix("file:") {
        return (name == "*").then(|| path.to_owned());
    }
    let mut parts = id.rsplitn(3, ':');
    let end: usize = parts.next()?.parse().ok()?;
    let start: usize = parts.next()?.parse().ok()?;
    let path = parts.next()?.split_once(':')?.1;
    let bytes = fs::read(root.join(path)).ok()?;
    let text = std::str::from_utf8(bytes.get(start..end)?).ok()?;
    let line = text.lines().next()?;
    let found = line.match_indices(name).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(identifier)
            && !line[at + name.len()..]
                .chars()
                .next()
                .is_some_and(identifier)
    });
    found.then(|| path.to_owned())
}

fn site_kind(edge: &Value) -> String {
    if is_jsx(edge) {
        "JSX".into()
    } else {
        edge["kind"].as_str().unwrap().into()
    }
}

#[test]
fn labelled_graph_has_no_false_confirmed_edges_and_full_recall() {
    let (_temp, root) = indexed_fixture();
    let edges = graph(&root);
    let labels = expected();
    let mut labelled = BTreeSet::new();
    for label in labels["resolved"].as_array().unwrap() {
        let key = (
            label[0].as_str().unwrap().to_owned(),
            label[1].as_u64().unwrap(),
            label[2].as_str().unwrap().to_owned(),
            label[3].as_str().unwrap().to_owned(),
        );
        assert!(labelled.insert(key), "duplicate label {label}");
    }
    let mut seen = BTreeSet::new();
    let mut false_confirmed = Vec::new();
    for edge in edges.iter().filter(|edge| {
        edge["status"] == "resolved" && RESOLVED_KINDS.contains(&edge["kind"].as_str().unwrap())
    }) {
        let key = (
            edge["location"]["path"].as_str().unwrap().to_owned(),
            edge["location"]["start_line"].as_u64().unwrap(),
            site_kind(edge),
            edge["target_text"].as_str().unwrap().to_owned(),
        );
        let label = labels["resolved"].as_array().unwrap().iter().find(|label| {
            (
                label[0].as_str().unwrap(),
                label[1].as_u64().unwrap(),
                label[2].as_str().unwrap(),
                label[3].as_str().unwrap(),
            ) == (key.0.as_str(), key.1, key.2.as_str(), key.3.as_str())
        });
        let correct = label.is_some_and(|label| {
            declared(
                &root,
                edge["target_id"].as_str().unwrap(),
                label[5].as_str().unwrap(),
            )
            .as_deref()
                == label[4].as_str()
        });
        if !correct {
            false_confirmed.push(edge.clone());
        }
        seen.insert(key);
    }
    assert!(
        false_confirmed.is_empty(),
        "false confirmed edges: {false_confirmed:#?}"
    );
    let missed: Vec<_> = labelled.difference(&seen).collect();
    assert!(missed.is_empty(), "labelled edges not resolved: {missed:?}");
    for label in labels["unconfirmed"].as_array().unwrap() {
        let site: Vec<&Value> = edges
            .iter()
            .filter(|edge| {
                edge["location"]["path"] == label[0]
                    && edge["location"]["start_line"] == label[1]
                    && site_kind(edge) == label[2].as_str().unwrap()
                    && edge["target_text"] == label[3]
            })
            .collect();
        assert!(!site.is_empty(), "no edge at {label}");
        for edge in site {
            assert_ne!(edge["status"], "resolved", "{label}: {edge}");
            assert!(edge["target_id"].is_null(), "{label}: {edge}");
            if !label[4].is_null() {
                assert_eq!(edge["status"], label[4], "{label}: {edge}");
            }
        }
    }
    for path in labels["no_semantic_edges"].as_array().unwrap() {
        assert!(
            !edges
                .iter()
                .any(|edge| edge["location"]["path"] == *path && edge["kind"] != "EXPORTS"),
            "{path} is partial and must not produce use edges"
        );
    }
    for path in labels["no_incoming"].as_array().unwrap() {
        let prefix = format!(":{}:", path.as_str().unwrap());
        assert!(
            !edges.iter().any(|edge| edge["location"]["path"] != *path
                && edge["target_id"]
                    .as_str()
                    .is_some_and(|id| id.contains(&prefix))),
            "same-name declarations in {path} must stay unused"
        );
    }
    // Intrinsic elements are never component references.
    assert!(!edges.iter().any(|edge| is_jsx(edge)
        && matches!(
            edge["target_text"].as_str(),
            Some("div" | "main" | "button" | "h1")
        )));
}

#[test]
fn languages_provenance_and_partial_files_are_recorded() {
    let (_temp, root) = indexed_fixture();
    let report = json(&root, &["index", "."]);
    assert_eq!(
        report["files_changed"], 0,
        "unchanged files are not reparsed"
    );
    let status = json(&root, &["status"]);
    // 15 JavaScript-family files, 6 TypeScript-family, 1 .tsx, 2 .jsx.
    assert_eq!(status["files"], 24, "{status}");
    let edges = json(&root, &["graph", "--path", "src/app/intl.js"])["edges"].clone();
    let call = edges
        .as_array()
        .unwrap()
        .iter()
        .find(|edge| edge["kind"] == "CALLS")
        .unwrap();
    // Byte offsets stay exact around multi-byte identifiers.
    assert_eq!(call["target_id"], "function:src/lib/unicode.js:75:128");
    let source = fs::read(root.join("src/lib/unicode.js")).unwrap();
    assert!(String::from_utf8_lossy(&source[75..128]).starts_with("function größe(maß)"));
    let context = json(&root, &["context", "intact broken", "--max-tokens", "4000"]);
    let text = context.to_string();
    assert!(text.contains("tree_sitter_javascript"), "{text}");
    assert!(
        text.contains("src/broken.js"),
        "intact declarations stay searchable"
    );
    let diagnostics = json(&root, &["index", "."]);
    assert_eq!(diagnostics["files_changed"], 0);
}

#[test]
fn incremental_edits_and_deletions_rebind_javascript_consumers() {
    let (_temp, root) = indexed_fixture();
    let math = root.join("src/lib/math.js");
    let text = fs::read_to_string(&math).unwrap();
    fs::write(&math, text.replace("function add(", "function sum(")).unwrap();
    fs::remove_file(root.join("src/app/helpers.js")).unwrap();
    let report = json(&root, &["index", "."]);
    assert_eq!(report["files_changed"], 1, "{report}");
    let edges = graph(&root);
    let at = |path: &str, line: u64, kind: &str, text: &str| -> Vec<Value> {
        edges
            .iter()
            .filter(|edge| {
                edge["location"]["path"] == path
                    && edge["location"]["start_line"] == line
                    && edge["kind"] == kind
                    && edge["target_text"] == text
            })
            .cloned()
            .collect()
    };
    for (path, line, kind, name) in [
        ("tests/math.test.js", 1, "IMPORTS", "add"),
        ("src/ts/consumer.ts", 5, "CALLS", "add"),
        ("src/app/main.js", 1, "IMPORTS", "add"),
        ("src/app/main.js", 6, "IMPORTS", "formatName"),
    ] {
        let site = at(path, line, kind, name);
        assert!(!site.is_empty(), "{path}:{line}");
        assert!(
            site.iter().all(|edge| edge["status"] != "resolved"),
            "{site:?}"
        );
    }
    assert!(!edges.iter().any(|edge| {
        edge["location"]["path"] == "src/app/helpers.js"
            || edge["target_id"]
                .as_str()
                .is_some_and(|id| id.contains(":src/app/helpers.js:"))
    }));
    let unchanged = at("src/app/main.js", 13, "CALLS", "multiply");
    assert_eq!(unchanged[0]["status"], "resolved");
    let fresh = json(&root, &["status"]);
    assert_eq!(fresh["files"], 23);
}

#[test]
fn git_impact_and_test_discovery_follow_javascript_consumers() {
    let (_temp, root) = indexed_fixture();
    git(&root, &["init", "-q"]);
    fs::write(root.join(".gitignore"), ".code-intel/\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let math = root.join("src/lib/math.js");
    let text = fs::read_to_string(&math).unwrap();
    fs::write(&math, text.replace("return a + b;", "return b + a;")).unwrap();
    let impact = json(&root, &["impact", "--format", "json", "--summary"]);
    let rendered = impact.to_string();
    for consumer in [
        "src/app/main.js",
        "src/ts/consumer.ts",
        "tests/math.test.js",
    ] {
        assert!(rendered.contains(consumer), "{consumer}: {rendered}");
    }
    assert!(!rendered.contains("src/app/other.js"), "{rendered}");
    let tests = json(&root, &["impact", "--tests", "--format", "json"]);
    let confirmed: Vec<&str> = tests["confirmed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|test| test["path"].as_str().unwrap())
        .collect();
    assert_eq!(confirmed, ["tests/math.test.js"], "{tests}");
}

#[test]
fn deep_nesting_is_bounded_and_reported() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let depth = 300;
    let source = format!(
        "export const deep = {}1{};\n",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    fs::write(root.join("deep.js"), source).unwrap();
    fs::write(root.join("ok.mjs"), "export function fine() {}\n").unwrap();
    let report = json(&root, &["index", "."]);
    assert_eq!(report["files_scanned"], 2);
    assert!(
        report["diagnostics"]
            .to_string()
            .contains("semantic analysis skipped"),
        "{report}"
    );
}

#[test]
fn minified_and_generated_javascript_is_skipped_but_typescript_rules_are_unchanged() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for name in [
        "app.min.js",
        "schema.generated.mjs",
        "bundle.min.cjs",
        "keep.js",
        "keep.min.tsx",
    ] {
        fs::write(root.join(name), "export const x = 1;\n").unwrap();
    }
    let report = json(&root, &["index", "."]);
    assert_eq!(report["files_scanned"], 5);
    assert_eq!(report["files_skipped"], 3, "{report}");
    // `.min.tsx` was never a skipped name; that is unchanged.
    assert_eq!(json(&root, &["status"])["files"], 2);
}
