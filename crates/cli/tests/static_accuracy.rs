//! Static accuracy: instance dispatch (`this.m`, `super.m`), `CommonJS`, and the
//! v0.1.4 `JavaScript` analysis (callable values, object and prototype tables,
//! alias origins, package entry points) against hand-reviewed labels in
//! `fixtures/{instance-dispatch,commonjs,javascript-intelligence}/EXPECTED.json`. Every resolved edge must be labeled
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

/// Edges that name a statically selected method rather than a binding:
/// `this.m`/`super.m`, and prototype methods of a `new F()` receiver.
fn instance(edge: &Value) -> bool {
    let text = edge["target_text"].as_str().unwrap_or_default();
    text.starts_with("this.")
        || text.starts_with("super.")
        || edge["evidence"]["rule"] == "prototype_method_static_declaration"
        || edge["evidence"]["rule"] == "constructed_instance_static_declaration"
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
fn javascript_intelligence_matches_labels() {
    check_labels("javascript-intelligence");
}

#[test]
fn commonjs_matches_labels() {
    check_labels("commonjs");
}

#[test]
fn receiver_intelligence_matches_labels() {
    check_labels("receiver-intelligence");
}

#[test]
fn workspace_packages_match_labels() {
    check_labels("workspace-packages");
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
    for name in [
        "instance-dispatch",
        "commonjs",
        "javascript-intelligence",
        "receiver-intelligence",
        "workspace-packages",
    ] {
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
        } else if name == "javascript-intelligence" {
            // A consumer write disappears, an `exports` subpath moves and an
            // object stops escaping: tables in other modules must follow.
            fs::write(
                root.join("lib/patch.js"),
                "const patched = require('./patched');\npatched.refresh();\n",
            )
            .unwrap();
            let package = fs::read_to_string(root.join("package.json")).unwrap();
            fs::write(
                root.join("package.json"),
                package.replace(
                    "\"./utils\": \"./lib/utils.js\"",
                    "\"./utils\": \"./lib/browser.js\"",
                ),
            )
            .unwrap();
            let objects = fs::read_to_string(root.join("lib/objects-dynamic.js")).unwrap();
            fs::write(
                root.join("lib/objects-dynamic.js"),
                objects.replace("register(a);", "register();"),
            )
            .unwrap();
        } else if name == "receiver-intelligence" {
            // A computed key becomes a well-known symbol and a receiver stops
            // escaping: instances in the same and other modules follow.
            let classes = fs::read_to_string(root.join("lib/classes.js")).unwrap();
            fs::write(
                root.join("lib/classes.js"),
                classes.replace("this[key] = 1;", "this[Symbol.iterator] = key;"),
            )
            .unwrap();
            let negative = fs::read_to_string(root.join("lib/negative.js")).unwrap();
            fs::write(
                root.join("lib/negative.js"),
                negative.replace("consume(e);\n", "consume();\n"),
            )
            .unwrap();
        } else if name == "workspace-packages" {
            // Only manifests change: a package is renamed, a duplicate is
            // excluded from the workspace, and a dependency range now selects
            // the local package. Importers rebind without being reparsed.
            fs::write(
                root.join("packages/core/package.json"),
                "{ \"name\": \"@acme/kernel\", \"version\": \"2.0.0\", \"main\": \"./lib/core.js\" }",
            )
            .unwrap();
            let manifest = fs::read_to_string(root.join("package.json")).unwrap();
            fs::write(
                root.join("package.json"),
                manifest.replace(
                    "[\"packages/*\", \"apps/*\"]",
                    "[\"packages/*\", \"!packages/dup-b\", \"apps/*\"]",
                ),
            )
            .unwrap();
            let web = fs::read_to_string(root.join("apps/web/package.json")).unwrap();
            fs::write(
                root.join("apps/web/package.json"),
                web.replace("\"^9.0.0\"", "\"workspace:*\""),
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
        if name == "workspace-packages" {
            assert_eq!(report["resolution_changed"], true, "{report}");
        } else {
            assert!(report["files_changed"].as_u64().unwrap() >= 1, "{report}");
        }
        let incremental = canonical(&root);
        let (_fresh_temp, fresh) = copy(name);
        for file in [
            "src/file.js",
            "lib/conditional.js",
            "esm-pkg/package.json",
            "lib/patch.js",
            "package.json",
            "lib/objects-dynamic.js",
            "lib/classes.js",
            "lib/negative.js",
            "packages/core/package.json",
            "apps/web/package.json",
        ] {
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
        } else if name == "javascript-intelligence" {
            assert!(resolved("app/patch-user.js", "patched.validate"));
            assert!(edges.iter().any(|edge| {
                edge["location"]["path"] == "app/main.js"
                    && edge["target_text"] == "utils.slug"
                    && edge["kind"] == "CALLS"
                    && edge["target_id"]
                        .as_str()
                        .is_some_and(|id| id.contains("lib/browser.js"))
            }));
        } else if name == "receiver-intelligence" {
            assert!(resolved("lib/classes.js", "r5.turn"));
            assert!(resolved("lib/negative.js", "e.close"));
        } else if name == "workspace-packages" {
            for text in ["dup", "old"] {
                assert!(edges.iter().any(|edge| {
                    edge["location"]["path"] == "apps/web/src/app.js"
                        && edge["kind"] == "IMPORTS"
                        && edge["target_text"] == text
                        && edge["status"] == "resolved"
                }));
            }
            let core = edges
                .iter()
                .find(|edge| {
                    edge["location"]["path"] == "apps/api/server.js"
                        && edge["kind"] == "IMPORTS"
                        && edge["target_text"] == "Core"
                })
                .unwrap();
            assert_eq!(
                core["evidence"]["rule"], "package_or_unmapped_specifier",
                "a renamed package is no longer the dependency's identity: {core}"
            );
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

/// v0.1.4 inputs that must stay bounded and never confirm a guessed target:
/// long alias chains, `main` redirect cycles, symlinked and malformed
/// `package.json` files, deeply nested `exports`, a huge object literal and
/// `__proto__` in a literal. Nothing is executed or fetched.
#[test]
#[allow(clippy::too_many_lines)] // One tree with every adversarial input, checked together.
fn javascript_adversarial_inputs_stay_bounded() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(root.join("package.json"), "{ \"name\": \"adv\" }").unwrap();
    // 400 chained aliases of one function, then a call through the last one.
    let mut chain = String::from("function f() { return 1; }\nconst a0 = f;\n");
    for i in 1..400 {
        writeln!(chain, "const a{i} = a{};", i - 1).unwrap();
    }
    chain.push_str("a399();\nmodule.exports = { a399 };\n");
    fs::write(root.join("chain.js"), chain).unwrap();
    // `main` redirect cycle between two local packages.
    for (dir, main) in [("p", "../q"), ("q", "../p")] {
        fs::create_dir_all(root.join(dir)).unwrap();
        fs::write(
            root.join(dir).join("package.json"),
            format!("{{ \"main\": \"{main}\" }}"),
        )
        .unwrap();
    }
    // A symlinked package.json pointing outside the root, and a malformed one.
    fs::create_dir_all(root.join("linked")).unwrap();
    fs::write(
        outside.path().join("package.json"),
        "{ \"main\": \"../../etc/passwd\" }",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("package.json"),
        root.join("linked/package.json"),
    )
    .unwrap();
    fs::write(
        root.join("linked/index.js"),
        "exports.x = function x() {};\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("broken")).unwrap();
    fs::write(root.join("broken/package.json"), "{ \"main\": 7, ").unwrap();
    fs::write(
        root.join("broken/index.js"),
        "exports.x = function x() {};\n",
    )
    .unwrap();
    // `exports` nested far beyond the bound, as a self-reference target.
    let mut nested = String::from("\"./lib.js\"");
    // 60 levels: within serde_json's recursion limit, beyond Repogi's bound.
    for _ in 0..60 {
        nested = format!("{{ \"default\": {nested} }}");
    }
    fs::create_dir_all(root.join("deep")).unwrap();
    fs::write(
        root.join("deep/package.json"),
        format!("{{ \"name\": \"deep\", \"exports\": {nested} }}"),
    )
    .unwrap();
    fs::write(root.join("deep/lib.js"), "exports.x = function x() {};\n").unwrap();
    fs::write(
        root.join("deep/use.js"),
        "const d = require('deep');\nd.x();\n",
    )
    .unwrap();
    // A huge literal and a `__proto__` key.
    let mut huge = String::from("const big = {\n");
    for i in 0..3000 {
        writeln!(huge, "  m{i}() {{ return {i}; }},").unwrap();
    }
    huge.push_str("};\nbig.m2999();\nconst bad = { __proto__: null, run() {} };\nbad.run();\n");
    fs::write(root.join("huge.js"), huge).unwrap();
    fs::write(
        root.join("use.js"),
        "const p = require('./p');\nconst l = require('./linked');\nconst b = require('./broken');\np.x(); l.x(); b.x();\n",
    )
    .unwrap();
    let started = std::time::Instant::now();
    json(&root, &["index", "."]);
    assert!(
        started.elapsed().as_secs() < 60,
        "indexing took {:?}",
        started.elapsed()
    );
    let edges = graph(&root);
    let edge = |path: &str, kind: &str, text: &str| {
        edges
            .iter()
            .find(|edge| {
                edge["location"]["path"] == path
                    && edge["kind"] == kind
                    && edge["target_text"] == text
            })
            .unwrap_or_else(|| panic!("no {kind} {text} in {path}"))
            .clone()
    };
    // The alias walk is bounded: the call does not reach `f` through 400 hops.
    let call = edge("chain.js", "CALLS", "a399");
    assert_ne!(call["status"], "resolved", "{call}");
    // A short chain still resolves.
    assert_eq!(edge("chain.js", "REFERENCES", "a0")["status"], "resolved");
    for (text, rule) in [
        ("p", "package_entry_tool_dependent"),
        ("l", "invalid_package_entry"),
        ("b", "invalid_package_entry"),
    ] {
        let import = edge("use.js", "IMPORTS", text);
        assert_eq!(import["status"], "unsupported", "{import}");
        assert_eq!(import["evidence"]["rule"], rule, "{import}");
    }
    let deep = edge("deep/use.js", "IMPORTS", "d");
    assert_eq!(
        deep["evidence"]["rule"], "package_exports_unsupported",
        "{deep}"
    );
    assert_eq!(edge("huge.js", "CALLS", "big.m2999")["status"], "resolved");
    let bad = edge("huge.js", "CALLS", "bad.run");
    assert_eq!(bad["evidence"]["rule"], "dynamic_object_members", "{bad}");
    // Nothing outside the root was read into the graph.
    assert!(
        !serde_json::to_string(&edges).unwrap().contains("passwd"),
        "an outside path leaked into evidence"
    );
}

/// Indexes a tree written by `build` and returns its graph edges.
fn indexed(build: impl FnOnce(&Path)) -> (TempDir, Vec<Value>) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    build(&root);
    json(&root, &["index", "."]);
    let edges = graph(&root);
    (temp, edges)
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn import_rule(edges: &[Value], path: &str, text: &str) -> (String, String) {
    let edge = edges
        .iter()
        .find(|edge| {
            edge["location"]["path"] == path
                && edge["kind"] == "IMPORTS"
                && edge["target_text"] == text
        })
        .unwrap_or_else(|| panic!("no import {text} in {path}"));
    (
        edge["status"].as_str().unwrap().to_owned(),
        edge["evidence"]["rule"].as_str().unwrap().to_owned(),
    )
}

/// A consumer of `@x/a` in a workspace whose root declaration is `workspaces`.
fn workspace_tree(root: &Path, workspaces: &str) {
    write(
        root,
        "package.json",
        &format!("{{ \"private\": true, \"workspaces\": {workspaces} }}"),
    );
    write(
        root,
        "packages/a/package.json",
        "{ \"name\": \"@x/a\", \"version\": \"1.0.0\" }",
    );
    write(
        root,
        "packages/a/index.js",
        "exports.a = function a() {};\n",
    );
    write(
        root,
        "apps/use/package.json",
        "{ \"dependencies\": { \"@x/a\": \"workspace:*\" } }",
    );
    write(
        root,
        "apps/use/main.js",
        "const a = require('@x/a');\na.a();\n",
    );
}

/// Workspace declarations Repogi cannot evaluate exactly never resolve a
/// package by name, and nothing outside the root is read: symlinked and
/// malformed members, unsupported glob syntax, out-of-root patterns, huge
/// pattern lists, unsupported YAML, traversal in names and targets, and a
/// re-export cycle between two packages.
#[test]
#[allow(clippy::too_many_lines)] // One tree per adversarial declaration.
fn workspace_adversarial_inputs_stay_bounded() {
    let (_ok, edges) = indexed(|root| workspace_tree(root, "[\"packages/*\"]"));
    assert_eq!(
        import_rule(&edges, "apps/use/main.js", "a"),
        ("resolved".into(), "commonjs_require_binding".into())
    );
    let outside = TempDir::new().unwrap();
    write(
        outside.path(),
        "package.json",
        "{ \"name\": \"@x/evil\", \"main\": \"../../etc/passwd\" }",
    );
    let unsupported = |build: &dyn Fn(&Path)| {
        let (_temp, edges) = indexed(|root| build(root));
        let (status, rule) = import_rule(&edges, "apps/use/main.js", "a");
        assert_eq!(status, "unsupported", "{rule}");
        assert!(
            !serde_json::to_string(&edges).unwrap().contains("passwd"),
            "an outside path leaked into evidence"
        );
        rule
    };
    #[cfg(unix)]
    assert_eq!(
        unsupported(&|root| {
            workspace_tree(root, "[\"packages/*\"]");
            std::os::unix::fs::symlink(outside.path(), root.join("packages/evil")).unwrap();
        }),
        "invalid_workspace_configuration"
    );
    assert_eq!(
        unsupported(&|root| {
            workspace_tree(root, "[\"packages/*\"]");
            write(root, "packages/b/package.json", "{ \"name\": ");
        }),
        "invalid_workspace_configuration"
    );
    for patterns in [
        "[\"packages/*\", \"packages/{a,b}\"]",
        "[\"packages/*\", \"../outside/*\"]",
        "[\"packages/*\", 7]",
    ] {
        assert_eq!(
            unsupported(&|root| workspace_tree(root, patterns)),
            "invalid_workspace_configuration",
            "{patterns}"
        );
    }
    // Nothing evaluable: the name is not known to be local, so it stays a package.
    assert_eq!(
        unsupported(&|root| workspace_tree(root, "{ \"nohoist\": [] }")),
        "package_or_unmapped_specifier"
    );
    let many: Vec<String> = (0..300).map(|i| format!("\"p{i}\"")).collect();
    assert_eq!(
        unsupported(&|root| {
            workspace_tree(root, &format!("[\"packages/*\", {}]", many.join(",")));
        }),
        "invalid_workspace_configuration"
    );
    assert_eq!(
        unsupported(&|root| {
            workspace_tree(root, "[\"packages/*\"]");
            write(root, "pnpm-workspace.yaml", "packages: &list\n  - *list\n");
        }),
        "invalid_workspace_configuration"
    );
    // pnpm and npm declarations that disagree are tool dependent.
    assert_eq!(
        unsupported(&|root| {
            workspace_tree(root, "[\"packages/*\"]");
            write(root, "pnpm-workspace.yaml", "packages:\n  - 'apps/*'\n");
        }),
        "invalid_workspace_configuration"
    );
    // A name that could traverse is not an identity.
    assert_eq!(
        unsupported(&|root| {
            workspace_tree(root, "[\"packages/*\"]");
            write(root, "packages/b/package.json", "{ \"name\": \"../../b\" }");
        }),
        "invalid_workspace_configuration"
    );
    // An `exports` target that leaves the package.
    let (_temp, edges) = indexed(|root| {
        workspace_tree(root, "[\"packages/*\"]");
        write(
            root,
            "packages/a/package.json",
            "{ \"name\": \"@x/a\", \"exports\": \"../../outside.js\" }",
        );
    });
    assert_eq!(
        import_rule(&edges, "apps/use/main.js", "a"),
        ("unsupported".into(), "invalid_package_target".into())
    );
    // pnpm always includes the root package; npm does not link it by name.
    for (declaration, expected) in [
        ("pnpm", ("resolved", "commonjs_require_binding")),
        ("npm", ("unsupported", "package_or_unmapped_specifier")),
    ] {
        let (_temp, edges) = indexed(|root| {
            if declaration == "pnpm" {
                write(
                    root,
                    "package.json",
                    "{ \"name\": \"lib\", \"exports\": { \".\": { \"types\": \"./dist/x.d.ts\", \"require\": \"./src/lib.js\" }, \"./built\": \"./dist/lib.js\" } }",
                );
                write(root, "pnpm-workspace.yaml", "packages:\n  - 'test/*'\n");
            } else {
                write(
                    root,
                    "package.json",
                    "{ \"name\": \"lib\", \"exports\": { \".\": { \"types\": \"./dist/x.d.ts\", \"require\": \"./src/lib.js\" }, \"./built\": \"./dist/lib.js\" }, \"workspaces\": [\"test/*\"] }",
                );
            }
            write(root, "src/lib.js", "exports.x = function x() {};\n");
            write(
                root,
                "test/use/package.json",
                "{ \"dependencies\": { \"lib\": \"workspace:*\" } }",
            );
            write(
                root,
                "test/use/main.js",
                "const lib = require('lib');\nconst built = require('lib/built');\nlib.x();\n",
            );
        });
        let (status, rule) = import_rule(&edges, "test/use/main.js", "lib");
        assert_eq!((status.as_str(), rule.as_str()), expected, "{declaration}");
        if declaration == "pnpm" {
            assert_eq!(
                import_rule(&edges, "test/use/main.js", "built"),
                ("unsupported".into(), "module_in_excluded_directory".into())
            );
        }
    }
    // A class extending one imported from a workspace package that is not
    // followed (here: an undeclared dependency on the root package, as in
    // Chart.js's docs) is an unindexed-package subclass, as before workspace
    // packages were modeled: it does not withhold unrelated `this.draw` sites.
    let (_temp, edges) = indexed(|root| {
        write(
            root,
            "package.json",
            "{ \"name\": \"lib\", \"type\": \"module\", \"exports\": \"./dist/lib.js\" }",
        );
        write(root, "pnpm-workspace.yaml", "packages:\n  - 'docs'\n");
        write(
            root,
            "src/chart.js",
            "export class Chart {\n  draw() { return 1; }\n  render() { return this.draw(); }\n}\n",
        );
        write(
            root,
            "docs/package.json",
            "{ \"name\": \"docs\", \"type\": \"module\" }",
        );
        write(
            root,
            "docs/custom.js",
            "import { Chart } from 'lib';\nexport class Custom extends Chart {\n  draw() { return 2; }\n}\n",
        );
    });
    assert_eq!(
        import_rule(&edges, "docs/custom.js", "Chart"),
        (
            "unsupported".into(),
            "workspace_dependency_not_declared".into()
        )
    );
    let draw = edges
        .iter()
        .find(|edge| edge["target_text"] == "this.draw" && edge["kind"] == "CALLS")
        .unwrap();
    assert_eq!(draw["status"], "resolved", "{draw}");
    // pnpm alone resolves; a re-export cycle across packages stays bounded.
    let started = std::time::Instant::now();
    let (_temp, edges) = indexed(|root| {
        write(root, "package.json", "{ \"private\": true }");
        write(root, "pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n");
        for (me, other) in [("a", "b"), ("b", "a")] {
            write(
                root,
                &format!("packages/{me}/package.json"),
                &format!(
                    "{{ \"name\": \"@x/{me}\", \"type\": \"module\", \"exports\": \"./index.js\", \"dependencies\": {{ \"@x/{other}\": \"workspace:*\" }} }}"
                ),
            );
            write(
                root,
                &format!("packages/{me}/index.js"),
                &format!("export * from '@x/{other}';\nexport function {me}() {{}}\n"),
            );
        }
        write(
            root,
            "packages/a/use.js",
            "import { b, missing } from '@x/b';\nb();\nmissing();\n",
        );
    });
    assert!(started.elapsed().as_secs() < 60);
    assert_eq!(import_rule(&edges, "packages/a/use.js", "b").0, "resolved");
    assert_ne!(
        import_rule(&edges, "packages/a/use.js", "missing").0,
        "resolved"
    );
}

/// 0.1.3: thousands of constructed receivers, aliases and escapes in one file
/// stay bounded, and every receiver that escapes through its alias stays unconfirmed.
#[test]
fn constructed_receivers_at_scale_stay_bounded() {
    let started = std::time::Instant::now();
    let (_temp, edges) = indexed(|root| {
        let mut receivers = String::from("class R { m() { return 1; } }\nfunction take() {}\n");
        for i in 0..4000 {
            writeln!(
                receivers,
                "const r{i} = new R();\nconst s{i} = r{i};\nif (r{i}) take(s{i});\nr{i}.m();"
            )
            .unwrap();
        }
        write(root, "receivers.js", &receivers);
    });
    assert!(
        started.elapsed().as_secs() < 60,
        "indexing took {:?}",
        started.elapsed()
    );
    let calls: Vec<&Value> = edges
        .iter()
        .filter(|edge| {
            edge["kind"] == "CALLS"
                && edge["target_text"]
                    .as_str()
                    .and_then(|text| text.rsplit_once('.'))
                    .is_some_and(|(_, member)| member == "m")
        })
        .collect();
    assert!(!calls.is_empty());
    for edge in calls {
        assert_eq!(edge["evidence"]["rule"], "receiver_escapes", "{edge}");
    }
}
