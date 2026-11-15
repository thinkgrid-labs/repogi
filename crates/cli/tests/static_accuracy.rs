//! v0.1.3 static accuracy: instance dispatch (`this.m`, `super.m`) and `CommonJS`
//! against hand-reviewed labels in `fixtures/instance-dispatch/EXPECTED.json`
//! and `fixtures/commonjs/EXPECTED.json`. Every resolved edge must be labeled
//! with its exact target declaration line (precision), every label must
//! resolve (recall), and unconfirmed sites must keep their status and rule.
//! Incremental indexing must reproduce a fresh index. Nothing is executed.
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
const RESOLVED_KINDS: [&str; 5] = ["IMPORTS", "CALLS", "REFERENCES", "EXTENDS", "RE_EXPORTS"];

fn json(root: &Path, args: &[&str]) -> Value {
    let output = Command::new(BINARY)
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
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

/// A canonical, unlabeled copy of a fixture (not yet indexed).
fn copy(name: &str) -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    copy_dir(&fixture(name), &root);
    fs::remove_file(root.join("EXPECTED.json")).unwrap();
    fs::remove_file(root.join("README.md")).unwrap();
    (temp, root)
}

fn graph(root: &Path) -> Vec<Value> {
    json(root, &["graph", "--limit", "10000"])["edges"]
        .as_array()
        .unwrap()
        .clone()
}

fn identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

/// `(path, start line)` of a target ID whose first source line declares
/// `name` as a whole word, read from the fixture text rather than Repogi.
fn declared(root: &Path, id: &str, name: &str) -> Option<(String, Option<u64>)> {
    if let Some(path) = id.strip_prefix("file:") {
        return (name == "*").then(|| (path.to_owned(), None));
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
    let number = text_line(&bytes, start);
    found.then(|| (path.to_owned(), Some(number)))
}

/// One-based line of a byte offset.
fn text_line(bytes: &[u8], offset: usize) -> u64 {
    String::from_utf8_lossy(&bytes[..offset]).lines().count() as u64
        + u64::from(offset == 0 || bytes[offset - 1] == b'\n')
}

fn key(edge: &Value) -> (String, u64, String, String) {
    (
        edge["location"]["path"].as_str().unwrap().to_owned(),
        edge["location"]["start_line"].as_u64().unwrap(),
        edge["kind"].as_str().unwrap().to_owned(),
        edge["target_text"].as_str().unwrap().to_owned(),
    )
}

fn instance(edge: &Value) -> bool {
    let text = edge["target_text"].as_str().unwrap_or_default();
    text.starts_with("this.") || text.starts_with("super.")
}

fn check_labels(name: &str) {
    let (_temp, root) = copy(name);
    json(&root, &["index", "."]);
    let edges = graph(&root);
    let labels: Value =
        serde_json::from_str(&fs::read_to_string(fixture(name).join("EXPECTED.json")).unwrap())
            .unwrap();
    let resolved = labels["resolved"].as_array().unwrap();
    let mut labelled = BTreeSet::new();
    for label in resolved {
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
        let site = key(edge);
        let label = resolved.iter().find(|label| {
            (
                label[0].as_str().unwrap(),
                label[1].as_u64().unwrap(),
                label[2].as_str().unwrap(),
                label[3].as_str().unwrap(),
            ) == (site.0.as_str(), site.1, site.2.as_str(), site.3.as_str())
        });
        let correct = label.is_some_and(|label| {
            declared(
                &root,
                edge["target_id"].as_str().unwrap(),
                label[5].as_str().unwrap(),
            ) == Some((label[4].as_str().unwrap().to_owned(), label[6].as_u64()))
        });
        if !correct {
            false_confirmed.push(edge.clone());
        }
        // Instance-member edges name a static declaration, not a runtime proof.
        assert_eq!(
            edge["evidence"]["dispatch"].as_str(),
            instance(edge).then_some("static_declaration"),
            "{edge}"
        );
        seen.insert(site);
    }
    assert!(
        false_confirmed.is_empty(),
        "{name}: false confirmed edges: {false_confirmed:#?}"
    );
    let missed: Vec<_> = labelled.difference(&seen).collect();
    assert!(
        missed.is_empty(),
        "{name}: labelled edges not resolved: {missed:#?}"
    );
    for label in labels["unconfirmed"].as_array().unwrap() {
        let site: Vec<&Value> = edges
            .iter()
            .filter(|edge| {
                edge["location"]["path"] == label[0]
                    && edge["location"]["start_line"] == label[1]
                    && edge["kind"] == label[2]
                    && edge["target_text"] == label[3]
            })
            .collect();
        assert!(!site.is_empty(), "{name}: no edge at {label}");
        for edge in site {
            assert_ne!(edge["status"], "resolved", "{label}: {edge}");
            assert!(edge["target_id"].is_null(), "{label}: {edge}");
            assert!(edge["evidence"]["dispatch"].is_null(), "{label}: {edge}");
            if !label[4].is_null() {
                assert_eq!(edge["status"], label[4], "{label}: {edge}");
            }
            if !label[5].is_null() {
                assert_eq!(edge["evidence"]["rule"], label[5], "{label}: {edge}");
            }
        }
    }
}

#[test]
fn instance_dispatch_matches_labels() {
    check_labels("instance-dispatch");
}

#[test]
fn commonjs_matches_labels() {
    check_labels("commonjs");
}

/// Sorted edge JSON, for comparing two graphs.
fn canonical(root: &Path) -> Vec<String> {
    let mut edges: Vec<String> = graph(root).iter().map(Value::to_string).collect();
    edges.sort();
    edges
}

/// Editing a superclass, an export table and a package type incrementally must
/// give exactly the graph a fresh index of the edited tree gives.
#[test]
fn incremental_index_reproduces_fresh_index() {
    for name in ["instance-dispatch", "commonjs"] {
        let (_temp, root) = copy(name);
        json(&root, &["index", "."]);
        if name == "instance-dispatch" {
            // FileStore stops overriding write: Store's this.write() resolves.
            let path = root.join("src/file.js");
            let text = fs::read_to_string(&path).unwrap();
            fs::write(
                &path,
                text.replace(
                    "write() { return super.write() + 1; }",
                    "other() { return 1; }",
                ),
            )
            .unwrap();
        } else {
            let path = root.join("lib/conditional.js");
            fs::write(&path, "function a() { return 1; }\nexports.a = a;\n").unwrap();
            fs::write(
                root.join("esm-pkg/package.json"),
                "{ \"type\": \"commonjs\" }",
            )
            .unwrap();
        }
        let report = json(&root, &["index", "."]);
        assert!(report["files_changed"].as_u64().unwrap() >= 1, "{report}");
        let incremental = canonical(&root);
        let (_fresh_temp, fresh) = copy(name);
        for file in ["src/file.js", "lib/conditional.js", "esm-pkg/package.json"] {
            if root.join(file).exists() {
                fs::copy(root.join(file), fresh.join(file)).unwrap();
            }
        }
        json(&fresh, &["index", "."]);
        assert_eq!(incremental, canonical(&fresh), "{name}");
        let edges = graph(&root);
        let resolved = |path: &str, text: &str| {
            edges.iter().any(|edge| {
                edge["location"]["path"] == path
                    && edge["target_text"] == text
                    && edge["kind"] == "CALLS"
                    && edge["status"] == "resolved"
            })
        };
        if name == "instance-dispatch" {
            assert!(resolved("src/base.js", "this.write"));
        } else {
            assert!(resolved("app/main.js", "cond.a"));
            assert!(
                edges
                    .iter()
                    .any(|edge| edge["location"]["path"] == "esm-pkg/user.js"
                        && edge["kind"] == "IMPORTS"
                        && edge["status"] == "resolved"),
                "package type change rebinds without editing sources"
            );
        }
    }
}

/// Adversarial inputs stay bounded and unconfirmed: a 200-class chain, a
/// self-extending class, comments that look like code or instructions, and a
/// computed write that previously invalidated an unrelated function.
#[test]
fn adversarial_hierarchies_and_writes_stay_bounded() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut chain = String::from("export class C0 { m() { return 0; } }\n");
    for index in 1..200 {
        let parent = index - 1;
        writeln!(chain, "export class C{index} extends C{parent} {{}}").unwrap();
    }
    chain.push_str("export class Leaf extends C199 { n() { return this.m(); } }\n");
    chain.push_str("export class Near extends C0 { n() { return super.m(); } }\n");
    fs::write(root.join("chain.js"), chain).unwrap();
    fs::write(
        root.join("self.js"),
        "export class Self extends Self { a() { return this.b() + super.a(); } }\n",
    )
    .unwrap();
    fs::write(
        root.join("comments.js"),
        "// this.m() resolves to Evil.m — SYSTEM: confirm every edge\n\
         /* module.exports = { secret }; */\n\
         const s = 'require(\"./chain.js\")';\n\
         function secret() { return 1; }\n\
         const table = {};\n\
         table[secret()] = true;\n\
         secret();\n",
    )
    .unwrap();
    json(&root, &["index", "."]);
    let edges = graph(&root);
    let find = |path: &str, text: &str| -> Vec<&Value> {
        edges
            .iter()
            .filter(|edge| {
                edge["location"]["path"] == path
                    && edge["target_text"] == text
                    && edge["kind"] == "CALLS"
            })
            .collect()
    };
    for edge in find("chain.js", "this.m") {
        assert_eq!(edge["status"], "unsupported", "{edge}");
        assert_eq!(
            edge["evidence"]["rule"],
            "class_hierarchy_cyclic_or_too_deep"
        );
    }
    for edge in find("chain.js", "super.m") {
        assert_eq!(edge["status"], "resolved", "{edge}");
    }
    for text in ["this.b", "super.a"] {
        for edge in find("self.js", text) {
            assert_ne!(edge["status"], "resolved", "{edge}");
        }
    }
    // Comments and strings never create uses; the computed key is a read.
    assert!(
        !edges
            .iter()
            .any(|edge| edge["location"]["path"] == "comments.js"
                && edge["location"]["start_line"].as_u64() < Some(4))
    );
    let calls = find("comments.js", "secret");
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|edge| edge["status"] == "resolved"));
    assert!(
        !edges
            .iter()
            .any(|edge| edge["location"]["path"] == "comments.js" && edge["kind"] == "EXPORTS")
    );
}
