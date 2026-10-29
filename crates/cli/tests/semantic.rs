use serde_json::Value;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn output(root: &Path, args: &[&str]) -> String {
    let result = Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}
fn fixture() -> TempDir {
    let temp = TempDir::new().unwrap();
    for entry in
        fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/semantics"))
            .unwrap()
    {
        let path = entry.unwrap().path();
        fs::copy(&path, temp.path().join(path.file_name().unwrap())).unwrap();
    }
    output(temp.path(), &["index", "."]);
    temp
}
fn graph(root: &Path) -> Vec<Value> {
    serde_json::from_str::<Value>(&output(root, &["graph", "--limit", "10000"])).unwrap()["edges"]
        .as_array()
        .unwrap()
        .clone()
}
fn call<'a>(edges: &'a [Value], name: &str) -> Vec<&'a Value> {
    edges
        .iter()
        .filter(|edge| edge["kind"] == "CALLS" && edge["target_text"] == name)
        .collect()
}

#[test]
fn aliases_defaults_namespace_barrels_cycles_and_static_methods_resolve() {
    let root = fixture();
    let edges = graph(root.path());
    for name in [
        "local",
        "chosen",
        "library.original",
        "Base.create",
        "arrow",
        "anchored",
    ] {
        let matches = call(&edges, name);
        assert!(
            matches.iter().any(|edge| edge["status"] == "resolved"),
            "{name}: {matches:?}"
        );
        for edge in matches.iter().filter(|edge| edge["status"] == "resolved") {
            assert!(edge["target_id"].as_str().unwrap().contains("leaf.ts"));
            assert!(
                !edge["evidence"]["binding_chain"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert!(edge["location"]["start_line"].as_u64().unwrap() > 0);
        }
    }
    assert!(edges.iter().any(|edge| edge["kind"] == "EXTENDS"
        && edge["status"] == "resolved"
        && edge["target_text"] == "Base"));
    for kind in ["IMPORTS", "EXPORTS", "RE_EXPORTS", "REFERENCES"] {
        assert!(
            edges
                .iter()
                .any(|edge| edge["kind"] == kind && edge["status"] == "resolved")
        );
    }
    assert_eq!(
        output(root.path(), &["graph"]),
        output(root.path(), &["graph"])
    );
    let before = graph(root.path());
    let report: Value = serde_json::from_str(&output(root.path(), &["index", "."])).unwrap();
    assert_eq!(report["files_changed"], 0);
    assert_eq!(before, graph(root.path()));
}

#[test]
fn ambiguity_shadowing_type_only_and_text_do_not_invent_calls() {
    let root = fixture();
    let edges = graph(root.path());
    assert!(
        call(&edges, "uncertain")
            .iter()
            .all(|edge| edge["status"] == "ambiguous" && edge["target_id"].is_null())
    );
    assert_eq!(call(&edges, "uncertain").len(), 1);
    assert!(
        call(&edges, "local")
            .iter()
            .any(|edge| edge["status"] == "unsupported" && edge["target_id"].is_null())
    );
    assert!(
        call(&edges, "onlyType")
            .iter()
            .all(|edge| edge["status"] == "unsupported")
    );
    assert!(
        edges
            .iter()
            .all(|edge| edge["target_text"] != "phantom" && edge["target_text"] != "imaginary")
    );
    assert!(
        edges
            .iter()
            .all(|edge| edge["status"] == "resolved" || edge["target_id"].is_null())
    );
    let text = "export function run() {}\nexport function f() { run = () => {}; return run(); }\n";
    fs::write(root.path().join("mutable.ts"), text).unwrap();
    output(root.path(), &["index", "."]);
    assert!(
        call(&graph(root.path()), "run")
            .iter()
            .all(|edge| edge["status"] == "unsupported")
    );
}

#[test]
fn export_rename_delete_and_barrel_edits_invalidate_unchanged_consumers_atomically() {
    let root = fixture();
    let path = root.path().join("leaf.ts");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(&path, source.replace("original", "replacement")).unwrap();
    let report: Value = serde_json::from_str(&output(root.path(), &["index", "."])).unwrap();
    assert_eq!(report["files_changed"], 1);
    assert!(
        call(&graph(root.path()), "local")
            .iter()
            .all(|edge| edge["target_id"].is_null())
    );
    fs::write(&path, &source).unwrap();
    output(root.path(), &["index", "."]);
    assert!(
        call(&graph(root.path()), "local")
            .iter()
            .any(|edge| edge["status"] == "resolved")
    );
    fs::write(
        root.path().join("barrel.ts"),
        "export { original as renamed } from './other';\n",
    )
    .unwrap();
    output(root.path(), &["index", "."]);
    assert!(
        call(&graph(root.path()), "local")
            .iter()
            .filter(|edge| edge["status"] == "resolved")
            .all(|edge| edge["target_id"].as_str().unwrap().contains("other.ts"))
    );
    fs::remove_file(root.path().join("other.ts")).unwrap();
    output(root.path(), &["index", "."]);
    assert!(
        call(&graph(root.path()), "local")
            .iter()
            .all(|edge| edge["target_id"].is_null())
    );
    let db = rusqlite::Connection::open(root.path().join(".code-intel/index.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, u32>(0)
        })
        .unwrap(),
        0
    );
    let before = graph(root.path());
    db.execute_batch("CREATE TRIGGER fail_semantics BEFORE INSERT ON semantic_edges BEGIN SELECT RAISE(ABORT,'test graph failure'); END;").unwrap();
    fs::write(&path, format!("{source}\n// changed\n")).unwrap();
    let failed = Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root.path())
        .args(["index", "."])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert_eq!(before, graph(root.path()));
}

#[test]
fn semantic_context_is_deterministic_bounded_and_preserves_progressive_selection() {
    let root = fixture();
    for budget in [1000, 2000, 4000, 8000] {
        for format in ["json", "text"] {
            for detail in ["overview", "focused", "detailed"] {
                let budget = budget.to_string();
                let args = [
                    "context",
                    "entry",
                    "--retrieval",
                    "semantic",
                    "--max-tokens",
                    &budget,
                    "--format",
                    format,
                    "--detail",
                    detail,
                ];
                let text = output(root.path(), &args);
                assert!(text.len() <= budget.parse().unwrap());
                assert_eq!(text, output(root.path(), &args));
            }
        }
    }
    let value: Value = serde_json::from_str(&output(
        root.path(),
        &[
            "context",
            "entry",
            "--retrieval",
            "semantic",
            "--max-tokens",
            "16000",
        ],
    ))
    .unwrap();
    assert!(
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["path"] == "leaf.ts")
    );
    let db = rusqlite::Connection::open(root.path().join(".code-intel/index.db")).unwrap();
    let id: String = db
        .query_row(
            "SELECT id FROM facts WHERE json_extract(data,'$.name')='entry'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let value: Value = serde_json::from_str(&output(
        root.path(),
        &[
            "context",
            "entry",
            "--retrieval",
            "semantic",
            "--max-tokens",
            "16000",
            "--select",
            &id,
        ],
    ))
    .unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
}

#[test]
fn version_one_migrates_and_syntax_errors_never_supply_verified_exports() {
    let root = fixture();
    let db = rusqlite::Connection::open(root.path().join(".code-intel/index.db")).unwrap();
    db.execute_batch("DROP TRIGGER semantic_search_delete; DROP TABLE semantic_search; DROP TABLE semantic_edges; DROP TABLE semantic_modules; PRAGMA user_version=1;").unwrap();
    let old = output(
        root.path(),
        &[
            "context",
            "entry",
            "--retrieval",
            "optimized",
            "--max-tokens",
            "8000",
        ],
    );
    let report: Value = serde_json::from_str(&output(root.path(), &["index", "."])).unwrap();
    assert_eq!(report["files_changed"], 7);
    let mut before: Value = serde_json::from_str(&old).unwrap();
    let mut after: Value = serde_json::from_str(&output(
        root.path(),
        &[
            "context",
            "entry",
            "--retrieval",
            "optimized",
            "--max-tokens",
            "8000",
        ],
    ))
    .unwrap();
    before["index_revision"] = Value::Null;
    after["index_revision"] = Value::Null;
    assert_eq!(before, after);
    fs::write(root.path().join("leaf.ts"), "export function original( {\n").unwrap();
    output(root.path(), &["index", "."]);
    assert!(
        call(&graph(root.path()), "local")
            .iter()
            .all(|edge| edge["target_id"].is_null())
    );
}

#[test]
fn lexical_scopes_destructuring_writes_and_type_reexports_are_conservative() {
    let root = fixture();
    fs::write(
        root.path().join("scopes.ts"),
        r"
import { original as invoke } from './leaf';
export function cases(values: Array<() => string>) {
  for (const invoke of values) { invoke(); }
  try {} catch (invoke) { invoke(); }
  { const { invoke } = { invoke: values[0] }; invoke(); }
  return invoke('known');
}
export class Accessor { static get value() { return () => 'x'; } }
Accessor.value();
export function mutate() { ({ invoke } = { invoke: values[0] }); invoke('unknown'); }
",
    )
    .unwrap();
    fs::write(
        root.path().join("type-barrel.ts"),
        "export type { original as typed } from './leaf';\n",
    )
    .unwrap();
    fs::write(
        root.path().join("type-user.ts"),
        "import { typed } from './type-barrel'; typed('no');\n",
    )
    .unwrap();
    output(root.path(), &["index", "."]);
    let edges = graph(root.path());
    assert!(
        call(&edges, "invoke")
            .iter()
            .all(|edge| edge["status"] != "resolved")
    );
    assert!(
        call(&edges, "typed")
            .iter()
            .all(|edge| edge["status"] == "unsupported")
    );
    assert!(
        call(&edges, "Accessor.value")
            .iter()
            .all(|edge| edge["status"] == "unsupported")
    );
}

#[test]
fn generic_awaited_calls_and_parentheses_keep_static_bindings() {
    let root = fixture();
    fs::write(
        root.path().join("generic.ts"),
        r"
import { original as fn } from './leaf';
async function run() { return await fn<string>('x') + (fn)('y'); }
export const table = [1];
export function length() { return table.length; }
",
    )
    .unwrap();
    output(root.path(), &["index", "."]);
    let edges = graph(root.path());
    assert_eq!(
        call(&edges, "fn")
            .iter()
            .filter(|edge| edge["status"] == "resolved")
            .count(),
        2
    );
    assert!(edges.iter().any(|edge| edge["kind"] == "REFERENCES"
        && edge["target_text"] == "table"
        && edge["status"] == "resolved"));
}

#[test]
fn star_reexports_keep_their_actual_source_site_and_explicit_exports_win() {
    let root = fixture();
    fs::write(
        root.path().join("star.ts"),
        "export * from './leaf';\nexport * from './other';\nexport { original } from './leaf';\n",
    )
    .unwrap();
    output(root.path(), &["index", "."]);
    let edges = graph(root.path());
    assert!(!edges.iter().any(|edge| edge["kind"] == "RE_EXPORTS"
        && edge["location"]["path"] == "star.ts"
        && edge["location"]["start_line"] == 2
        && edge["target_text"] == "Base"));
    let original: Vec<_> = edges
        .iter()
        .filter(|edge| {
            edge["kind"] == "RE_EXPORTS"
                && edge["location"]["path"] == "star.ts"
                && edge["target_text"] == "original"
        })
        .collect();
    assert_eq!(original.len(), 1);
    assert_eq!(original[0]["location"]["start_line"], 3);
    assert_eq!(original[0]["status"], "resolved");
}

#[test]
fn deeply_nested_ast_is_reported_without_claiming_partial_semantic_completeness() {
    let root = TempDir::new().unwrap();
    let source = format!(
        "export function nested() {{ return {}1{}; }}\n",
        "(".repeat(300),
        ")".repeat(300)
    );
    fs::write(root.path().join("nested.ts"), source).unwrap();
    let report: Value = serde_json::from_str(&output(root.path(), &["index", "."])).unwrap();
    assert_eq!(report["syntax_error_files"], 0);
    assert!(
        report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["message"]
                .as_str()
                .unwrap()
                .contains("nesting exceeds 256"))
    );
    assert!(graph(root.path()).is_empty());
}

#[test]
fn template_interpolations_track_bindings_while_literal_text_has_no_references() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("template.ts"),
        r"
export function target() { return 'known'; }
export function shadow(value: () => string) {
  return `${((target: () => string) => target())(value)}`;
}
export function known() { return `target() is text, ${target()} is code`; }
",
    )
    .unwrap();
    output(root.path(), &["index", "."]);
    let edges = graph(root.path());
    let targets = call(&edges, "target");
    assert_eq!(targets.len(), 2);
    assert_eq!(
        targets
            .iter()
            .filter(|edge| edge["status"] == "resolved")
            .count(),
        1
    );
    assert_eq!(
        targets
            .iter()
            .filter(|edge| edge["status"] == "unsupported" && edge["target_id"].is_null())
            .count(),
        1
    );
}
