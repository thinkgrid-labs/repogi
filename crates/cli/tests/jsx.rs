//! JSX component references (Phase 3.7) against the hand-written labels in
//! `fixtures/jsx/EXPECTED.json`. Targets are compared by declaration, never by
//! counts, so a wrong component target fails even when more edges resolve.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");

fn run(root: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let result = Command::new(BINARY)
        .current_dir(root)
        .args(args)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/jsx")
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

fn fixture() -> TempDir {
    let temp = TempDir::new().unwrap();
    copy_dir(&fixture_dir(), temp.path());
    temp
}

fn index(root: &Path, env: &[(&str, &str)]) -> Value {
    serde_json::from_str(&run(root, &["index", "."], env)).unwrap()
}

fn graph(root: &Path) -> Vec<Value> {
    serde_json::from_str::<Value>(&run(root, &["graph", "--limit", "10000"], &[])).unwrap()["edges"]
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

/// The declared name at a symbol id (`kind:path:start:end`), read from source
/// text rather than from Atlas, so labels name declarations independently.
fn declared(root: &Path, id: &str) -> (String, String) {
    let mut parts = id.rsplitn(3, ':');
    let end: usize = parts.next().unwrap().parse().unwrap();
    let start: usize = parts.next().unwrap().parse().unwrap();
    let path = parts.next().unwrap().split_once(':').unwrap().1.to_owned();
    let source = fs::read_to_string(root.join(&path)).unwrap();
    let text = source[start..end].trim_start();
    let text = ["function ", "class ", "const ", "let "]
        .iter()
        .find_map(|keyword| text.strip_prefix(keyword))
        .unwrap_or(text);
    let name = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .next()
        .unwrap()
        .to_owned();
    (path, name)
}

/// One observed JSX site: (owner, tag, line) -> (status, resolved target).
type Observed = BTreeMap<(String, String, u64), (String, Option<(String, String)>)>;

fn observe(root: &Path, edges: &[Value]) -> Observed {
    let mut observed = Observed::new();
    for edge in edges.iter().filter(|edge| is_jsx(edge)) {
        let owner = declared(root, edge["source_id"].as_str().unwrap()).1;
        let key = (
            owner,
            edge["target_text"].as_str().unwrap().to_owned(),
            edge["location"]["start_line"].as_u64().unwrap(),
        );
        let target = edge["target_id"].as_str().map(|id| declared(root, id));
        let previous = observed.insert(
            key.clone(),
            (edge["status"].as_str().unwrap().to_owned(), target),
        );
        assert!(previous.is_none(), "JSX site counted twice: {key:?}");
    }
    observed
}

#[derive(Debug, Default)]
struct Score {
    correct: usize,
    /// Resolved to a declaration other than the labelled one, or resolved
    /// where the label says no confirmed target exists.
    incorrect: Vec<String>,
    missed: Vec<String>,
    status_mismatch: Vec<String>,
    unlabelled: Vec<String>,
}

impl Score {
    fn passed(&self) -> bool {
        self.incorrect.is_empty()
            && self.missed.is_empty()
            && self.status_mismatch.is_empty()
            && self.unlabelled.is_empty()
    }
}

fn key(label: &Value) -> (String, String, u64) {
    (
        label["owner"].as_str().unwrap().to_owned(),
        label["tag"].as_str().unwrap().to_owned(),
        label["line"].as_u64().unwrap(),
    )
}

fn score(labels: &Value, observed: &Observed) -> Score {
    let mut score = Score::default();
    let mut remaining = observed.clone();
    for label in labels["confirmed"].as_array().unwrap() {
        let key = key(label);
        let want = (
            label["target"][0].as_str().unwrap().to_owned(),
            label["target"][1].as_str().unwrap().to_owned(),
        );
        match remaining.remove(&key) {
            Some((status, Some(target))) if status == "resolved" && target == want => {
                score.correct += 1;
            }
            Some((status, Some(target))) if status == "resolved" => {
                score
                    .incorrect
                    .push(format!("{key:?} -> {target:?}, expected {want:?}"));
            }
            _ => score.missed.push(format!("{key:?}")),
        }
    }
    for label in labels["not_confirmed"].as_array().unwrap() {
        let key = key(label);
        match remaining.remove(&key) {
            Some((status, Some(target))) if status == "resolved" => {
                score.incorrect.push(format!(
                    "{key:?} -> {target:?}, expected no confirmed target"
                ));
            }
            Some((status, _)) if status != label["status"] => {
                score.status_mismatch.push(format!("{key:?}: {status}"));
            }
            Some(_) => {}
            None => score.missed.push(format!("{key:?}")),
        }
    }
    score.unlabelled = remaining.keys().map(|key| format!("{key:?}")).collect();
    score
}

fn resolved(observed: &Observed) -> usize {
    observed
        .values()
        .filter(|(status, _)| status == "resolved")
        .count()
}

#[test]
fn jsx_references_match_independent_labels() {
    let root = fixture();
    index(root.path(), &[]);
    let edges = graph(root.path());
    let observed = observe(root.path(), &edges);
    let labels = expected();
    let score = score(&labels, &observed);
    assert!(score.passed(), "{score:#?}");
    assert_eq!(score.correct, labels["confirmed"].as_array().unwrap().len());
    let rules: BTreeMap<_, _> = edges
        .iter()
        .filter(|edge| is_jsx(edge))
        .map(|edge| {
            (
                edge["target_text"].as_str().unwrap(),
                edge["evidence"]["rule"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(rules["Panel"], "jsx_component_reference");
    assert_eq!(rules["Icons.Star"], "jsx_component_reference");
    assert_eq!(rules["Toaster"], "package_or_unmapped_specifier");
    assert_eq!(rules["Ambig"], "ambiguous_module_candidates");
    assert_eq!(rules["PanelProps"], "jsx_target_is_a_type");
}

#[test]
fn scoring_detects_a_wrong_target_even_when_more_edges_resolve() {
    let root = fixture();
    index(root.path(), &[]);
    let observed = observe(root.path(), &graph(root.path()));
    let labels = expected();
    let baseline = resolved(&observed);
    // A name-matching binder: retargets a renamed import and "resolves" the
    // ambiguous, shadowed and package sites to same-named declarations.
    let mut guessed = observed.clone();
    let retarget =
        |guessed: &mut Observed, owner: &str, tag: &str, line: u64, path: &str, name: &str| {
            guessed.insert(
                (owner.into(), tag.into(), line),
                ("resolved".into(), Some((path.into(), name.into()))),
            );
        };
    retarget(
        &mut guessed,
        "App",
        "PrimaryButton",
        17,
        "src/components/Card.tsx",
        "Card",
    );
    retarget(&mut guessed, "App", "Ambig", 24, "src/Ambig.tsx", "Ambig");
    retarget(
        &mut guessed,
        "Shadowing",
        "Shadowed",
        33,
        "src/components/Shadowed.tsx",
        "Shadowed",
    );
    assert!(resolved(&guessed) > baseline);
    let score = score(&labels, &guessed);
    assert!(!score.passed());
    assert_eq!(score.incorrect.len(), 3, "{score:#?}");
    assert_eq!(
        score.correct,
        labels["confirmed"].as_array().unwrap().len() - 1
    );
}

#[test]
fn intrinsics_closing_tags_and_attributes_add_no_jsx_sites() {
    let root = fixture();
    index(root.path(), &[]);
    let edges = graph(root.path());
    let jsx: Vec<&Value> = edges.iter().filter(|edge| is_jsx(edge)).collect();
    let labels = expected();
    let labelled = labels["confirmed"].as_array().unwrap().len()
        + labels["not_confirmed"].as_array().unwrap().len();
    assert_eq!(jsx.len(), labelled);
    for edge in &jsx {
        assert_eq!(edge["kind"], "REFERENCES", "{edge}");
    }
    for intrinsic in labels["no_jsx_reference"].as_array().unwrap() {
        assert!(
            edges.iter().all(|edge| edge["target_text"] != *intrinsic),
            "intrinsic {intrinsic} produced an edge"
        );
    }
    // `<Panel>…</Panel>` and similar elements count once each.
    for tag in ["Panel", "PrimaryButton", "ThemeContext.Provider"] {
        assert_eq!(
            jsx.iter().filter(|edge| edge["target_text"] == tag).count(),
            1
        );
    }
    // `data-x={Button}` is an ordinary value reference, not a JSX site.
    let attribute = edges
        .iter()
        .find(|edge| edge["location"]["start_line"] == 26 && edge["target_text"] == "Button")
        .unwrap();
    assert!(!is_jsx(attribute));
    assert_eq!(attribute["evidence"]["rule"], "unique_static_binding");
    // `<ThemeContext.Provider>…</ThemeContext.Provider>` still references its
    // receiver `ThemeContext`, once (opening tag only), as a resolved plain edge.
    let receivers: Vec<&Value> = edges
        .iter()
        .filter(|edge| edge["target_text"] == "ThemeContext" && edge["kind"] == "REFERENCES")
        .collect();
    assert_eq!(receivers.len(), 1);
    assert_eq!(receivers[0]["location"]["start_line"], 53);
    assert_eq!(receivers[0]["status"], "resolved");
    assert!(!is_jsx(receivers[0]));
    assert!(
        edges
            .iter()
            .filter(|edge| edge["kind"] == "CALLS")
            .all(|edge| !is_jsx(edge))
    );
}

#[test]
fn cached_ir_from_before_jsx_is_reparsed_once() {
    let root = fixture();
    let first = index(root.path(), &[("CODE_INTEL_DISABLE_JSX", "1")]);
    assert!(graph(root.path()).iter().all(|edge| !is_jsx(edge)));
    // Simulate an index written by a pre-3.7 binary.
    let db = rusqlite::Connection::open(root.path().join(".code-intel/index.db")).unwrap();
    db.execute("UPDATE metadata SET value='1' WHERE key='semantic_ir'", [])
        .unwrap();
    drop(db);
    let status = Mcp::start(root.path()).json("atlas_status", &json!({}));
    assert_eq!(status["fresh"], false, "{status}");
    assert_eq!(status["freshness"]["semantic_ir_outdated"], true);
    let upgraded = index(root.path(), &[]);
    assert_eq!(upgraded["files_changed"], first["files_scanned"]);
    let edges = graph(root.path());
    let score = score(&expected(), &observe(root.path(), &edges));
    assert!(score.passed(), "{score:#?}");
    let warm = index(root.path(), &[]);
    assert_eq!(warm["files_changed"], 0);
}

/// Each call runs in a fresh `repogi mcp` process: initialize, one tool call, EOF.
struct Mcp {
    root: std::path::PathBuf,
}

impl Mcp {
    fn start(root: &Path) -> Self {
        Self {
            root: root.canonicalize().unwrap(),
        }
    }

    fn json(&self, name: &str, arguments: &Value) -> Value {
        serde_json::from_str(&self.text(name, arguments)).unwrap()
    }

    fn text(&self, name: &str, arguments: &Value) -> String {
        let mut child = Command::new(BINARY)
            .arg("mcp")
            .arg("--root")
            .arg(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        for line in [
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
        ] {
            writeln!(stdin, "{line}").unwrap();
        }
        drop(stdin);
        let output = child.wait_with_output().unwrap();
        let response = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|message| message["id"] == 1)
            .unwrap();
        assert_eq!(response["result"]["isError"], false, "{response}");
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[test]
fn references_and_dependencies_expose_jsx_sites() {
    let root = fixture();
    index(root.path(), &[]);
    let mcp = Mcp::start(root.path());
    let references = mcp.json(
        "atlas_references",
        &json!({"name":"Button","path":"src/components/Button.tsx","format":"json","max_bytes":32000}),
    );
    assert_eq!(references["truncated"], false);
    let sites: Vec<(u64, &str, bool)> = references["resolved"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|site| site["location"]["path"] == "src/App.tsx")
        .map(|site| {
            (
                site["location"]["start_line"].as_u64().unwrap(),
                site["kind"].as_str().unwrap(),
                site["jsx"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    for jsx_line in [16, 17, 37] {
        assert!(sites.contains(&(jsx_line, "REFERENCES", true)), "{sites:?}");
    }
    assert!(sites.contains(&(26, "REFERENCES", false)), "{sites:?}");
    assert!(
        sites
            .iter()
            .any(|(_, kind, jsx)| *kind == "IMPORTS" && !jsx)
    );
    let agent = mcp.text(
        "atlas_references",
        &json!({"name":"Button","path":"src/components/Button.tsx"}),
    );
    assert!(agent.contains("REFERENCES(JSX)"), "{agent}");
    let upstream = mcp.json(
        "atlas_dependencies",
        &json!({"name":"Card","path":"src/components/Card.tsx","direction":"upstream","format":"json"}),
    );
    let names: Vec<&str> = upstream["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"App"), "{names:?}");
    assert!(
        upstream["links"]
            .as_array()
            .unwrap()
            .iter()
            .all(|link| link["status"] == "resolved")
    );
}

#[test]
fn disabling_jsx_changes_only_jsx_sites() {
    let root = fixture();
    index(root.path(), &[("CODE_INTEL_DISABLE_JSX", "1")]);
    let without = graph(root.path());
    let control = fixture();
    index(control.path(), &[]);
    let with = graph(control.path());
    let plain: Vec<&Value> = with.iter().filter(|edge| !is_jsx(edge)).collect();
    // Every non-JSX edge is unchanged.
    for edge in &plain {
        assert!(without.contains(edge), "new non-JSX edge {edge}");
    }
    // Before 3.7 member-expression tags were plain member REFERENCES, emitted for
    // the opening and again for the closing tag (with its receiver). The opening
    // one is now the JSX site at the same location, target and binding chain.
    let dropped: Vec<(&str, u64)> = without
        .iter()
        .filter(|edge| !plain.contains(edge))
        .map(|edge| {
            (
                edge["target_text"].as_str().unwrap(),
                edge["location"]["start_line"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        dropped,
        [
            ("Icons.Star", 22),
            ("ThemeContext.Provider", 53),
            ("ThemeContext", 55),
            ("ThemeContext.Provider", 55),
        ]
    );
    for edge in without
        .iter()
        .filter(|edge| !plain.contains(edge) && edge["location"]["start_line"] != 55)
    {
        let jsx = with
            .iter()
            .find(|jsx| is_jsx(jsx) && jsx["location"] == edge["location"])
            .unwrap();
        assert_eq!(jsx["target_text"], edge["target_text"]);
        assert_eq!(jsx["status"], edge["status"]);
        assert_eq!(jsx["target_id"], edge["target_id"]);
        assert_eq!(
            jsx["evidence"]["binding_chain"],
            edge["evidence"]["binding_chain"]
        );
    }
}
