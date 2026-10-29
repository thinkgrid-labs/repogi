use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use code_intel_core::ContextResponse;
use serde_json::Value;
use tempfile::TempDir;

fn run(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root)
        .args(arguments)
        .output()
        .unwrap()
}

fn success(root: &Path, arguments: &[&str]) -> String {
    let output = run(root, arguments);
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn progressive_views_and_selection_are_truthful_bounded_and_deterministic() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("sample.ts"),"export interface User { name: string; }\nexport function welcome(\n  user: { name: string },\n  suffix: string\n): string {\n  return user.name + suffix;\n}\n").unwrap();
    success(root.path(), &["index", "."]);
    for detail in ["overview", "focused", "detailed"] {
        for budget in [1000, 2000, 4000, 8000] {
            for format in ["json", "text"] {
                let arguments = [
                    "context",
                    "welcome User",
                    "--detail",
                    detail,
                    "--max-tokens",
                    &budget.to_string(),
                    "--format",
                    format,
                ];
                let output = success(root.path(), &arguments);
                assert!(output.len() <= budget);
                assert_eq!(output, success(root.path(), &arguments));
            }
        }
    }
    let overview: ContextResponse = serde_json::from_str(&success(
        root.path(),
        &[
            "context",
            "welcome User",
            "--detail",
            "overview",
            "--max-tokens",
            "8000",
        ],
    ))
    .unwrap();
    assert!(
        overview
            .items
            .iter()
            .all(|item| item.snippet.is_empty() && item.name.is_some())
    );
    assert!(
        overview
            .relationships
            .iter()
            .all(|edge| edge.provenance.confidence == "textual_heuristic")
    );
    let id = &overview
        .items
        .iter()
        .find(|item| item.name.as_deref() == Some("welcome"))
        .unwrap()
        .id;
    let focused: ContextResponse = serde_json::from_str(&success(
        root.path(),
        &[
            "context",
            "welcome",
            "--detail",
            "focused",
            "--select",
            id,
            "--max-tokens",
            "8000",
        ],
    ))
    .unwrap();
    let function = focused.items.iter().find(|item| item.id == *id).unwrap();
    assert_eq!(
        function.snippet,
        "function welcome(\n  user: { name: string },\n  suffix: string\n): string"
    );
    assert_eq!((function.start_line, function.end_line), (2, 5));
    assert_eq!(function.detail.as_deref(), Some("signature"));
    let detailed: ContextResponse = serde_json::from_str(&success(
        root.path(),
        &["context", "welcome", "--select", id, "--max-tokens", "8000"],
    ))
    .unwrap();
    assert_eq!(detailed.items.len(), 1);
    assert!(detailed.items[0].snippet.contains("return user.name"));
    let db = rusqlite::Connection::open(root.path().join(".code-intel/index.db")).unwrap();
    db.execute("UPDATE facts SET data=json_remove(data,'$.signature')", [])
        .unwrap();
    let old: ContextResponse = serde_json::from_str(&success(
        root.path(),
        &[
            "context",
            "welcome",
            "--detail",
            "focused",
            "--select",
            id,
            "--max-tokens",
            "8000",
        ],
    ))
    .unwrap();
    assert_eq!(old.items[0].detail.as_deref(), Some("signature_preview"));
}

#[test]
fn selection_errors_and_same_line_symbols_do_not_silently_lose_evidence() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("same.ts"),
        "export function first() { return 1; } export function second() { return 2; }\n",
    )
    .unwrap();
    success(root.path(), &["index", "."]);
    let response: ContextResponse = serde_json::from_str(&success(
        root.path(),
        &["context", "first second", "--max-tokens", "8000"],
    ))
    .unwrap();
    assert!(
        response
            .items
            .iter()
            .any(|item| item.name.as_deref() == Some("first"))
    );
    assert!(
        response
            .items
            .iter()
            .any(|item| item.name.as_deref() == Some("second"))
    );
    for arguments in [
        vec!["context", "first", "--select", "unknown-id"],
        vec![
            "context",
            "first",
            "--retrieval",
            "graph",
            "--detail",
            "focused",
        ],
    ] {
        let output = run(root.path(), &arguments);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["message"]
                .is_string()
        );
    }
}
