//! Golden relevance labels are maintained independently of Tree-sitter extraction.
use std::{collections::BTreeSet, fs, path::Path, process::Command};

use code_intel_core::ContextResponse;
use serde_json::Value;
use tempfile::TempDir;

fn suite() -> Value {
    serde_json::from_str(include_str!("../../../benchmarks/suite.json")).unwrap()
}

fn workspace() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap()
}

fn number(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}

fn snapshot(corpus: &Value) -> TempDir {
    let root = TempDir::new().unwrap();
    for file in corpus["files"].as_array().unwrap() {
        let relative = text(file);
        let target = root.path().join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(
            workspace().join(text(&corpus["path"])).join(relative),
            target,
        )
        .unwrap();
    }
    root
}

fn run(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).unwrap()
}

fn query(root: &Path, task: &str, budget: usize, format: &str) -> String {
    run(
        root,
        &[
            "context",
            task,
            "--max-tokens",
            &budget.to_string(),
            "--format",
            format,
        ],
    )
}

fn fully_covered(root: &Path, response: &ContextResponse, symbol: &Value) -> bool {
    let path = text(&symbol["path"]);
    let source = fs::read_to_string(root.join(path)).unwrap();
    let source: Vec<&str> = source.lines().collect();
    let mut covered = BTreeSet::new();
    for item in response.items.iter().filter(|item| item.path == path) {
        let lines: Vec<&str> = item.snippet.lines().collect();
        assert_eq!(lines.len(), item.end_line - item.start_line + 1);
        for (offset, line) in lines.iter().enumerate() {
            let number = item.start_line + offset;
            let original = source[number - 1].trim();
            let expected = original
                .strip_prefix("export default ")
                .or_else(|| original.strip_prefix("export "))
                .unwrap_or(original);
            assert!(source[number - 1].contains(line.trim()));
            if line.trim().contains(expected) {
                covered.insert(number);
            }
        }
    }
    (number(&symbol["start_line"])..=number(&symbol["end_line"]))
        .all(|line| covered.contains(&line))
}

#[test]
fn benchmark_labels_have_valid_source_boundaries_and_targets() {
    let suite = suite();
    assert_eq!(
        suite["budgets"],
        serde_json::json!([1000, 2000, 4000, 8000])
    );
    for corpus in suite["corpora"].as_array().unwrap() {
        let root = workspace().join(text(&corpus["path"]));
        let mut names = BTreeSet::new();
        for symbol in corpus["symbols"].as_array().unwrap() {
            assert!(names.insert(text(&symbol["name"])));
            let source = fs::read_to_string(root.join(text(&symbol["path"]))).unwrap();
            let lines: Vec<&str> = source.lines().collect();
            assert_eq!(
                lines[number(&symbol["start_line"]) - 1],
                text(&symbol["first_line"])
            );
            assert_eq!(
                lines[number(&symbol["end_line"]) - 1],
                text(&symbol["last_line"])
            );
        }
        for task in corpus["tasks"].as_array().unwrap() {
            for symbol in task["relevant_symbols"].as_array().unwrap() {
                assert!(names.contains(text(symbol)));
            }
        }
    }
}

#[test]
fn all_benchmark_tasks_are_deterministic_and_bounded_at_every_budget_and_format() {
    let suite = suite();
    for corpus in suite["corpora"].as_array().unwrap() {
        let root = snapshot(corpus);
        run(root.path(), &["index", "."]);
        for task in corpus["tasks"].as_array().unwrap() {
            for budget in [1000, 2000, 4000, 8000] {
                for format in ["json", "text"] {
                    let output = query(root.path(), text(&task["query"]), budget, format);
                    assert!(
                        output.len() <= budget,
                        "{} {} {budget} {format}",
                        corpus["id"],
                        task["id"]
                    );
                    assert_eq!(
                        output,
                        query(root.path(), text(&task["query"]), budget, format)
                    );
                    if format == "json" {
                        let response: ContextResponse = serde_json::from_str(&output).unwrap();
                        assert_eq!(response.estimated_tokens, output.len());
                        assert_eq!(response.budget.method, "estimated_utf8_bytes");
                        for item in &response.items {
                            assert_eq!(
                                item.snippet.lines().count(),
                                item.end_line - item.start_line + 1,
                                "{} {} {budget} {}: {}",
                                corpus["id"],
                                task["id"],
                                item.path,
                                item.snippet
                            );
                        }
                        if task["relevant_files"].as_array().unwrap().is_empty() {
                            assert!(
                                response.items.is_empty(),
                                "negative-control evidence leaked"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn named_tasks_preserve_complete_primary_symbols_at_8000() {
    let suite = suite();
    for corpus in suite["corpora"].as_array().unwrap() {
        let root = snapshot(corpus);
        run(root.path(), &["index", "."]);
        for task in corpus["tasks"].as_array().unwrap() {
            let output = query(root.path(), text(&task["query"]), 8000, "json");
            let response: ContextResponse = serde_json::from_str(&output).unwrap();
            for target in task["must_complete_at_8000"].as_array().unwrap() {
                let symbol = corpus["symbols"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|symbol| symbol["name"] == *target)
                    .unwrap();
                assert!(
                    fully_covered(root.path(), &response, symbol),
                    "{} {} missing complete {target}",
                    corpus["id"],
                    task["id"]
                );
            }
        }
    }
}

#[test]
fn equivalent_snapshots_keep_ranking_after_warm_runs_and_edit_restore() {
    let suite = suite();
    for corpus in suite["corpora"].as_array().unwrap() {
        let root = snapshot(corpus);
        let independent = snapshot(corpus);
        run(root.path(), &["index", "."]);
        run(independent.path(), &["index", "."]);
        let task = text(&corpus["tasks"][0]["query"]);
        let original = query(root.path(), task, 4000, "json");
        assert_eq!(original, query(independent.path(), task, 4000, "json"));
        run(root.path(), &["index", "."]);
        assert_eq!(original, query(root.path(), task, 4000, "json"));
        let path = root.path().join(text(&corpus["incremental_file"]));
        let source = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{source}\n// temporary edit\n")).unwrap();
        run(root.path(), &["index", "."]);
        fs::write(&path, source).unwrap();
        run(root.path(), &["index", "."]);
        let mut restored: Value =
            serde_json::from_str(&query(root.path(), task, 4000, "json")).unwrap();
        let mut original: Value = serde_json::from_str(&original).unwrap();
        restored["index_revision"] = Value::Null;
        original["index_revision"] = Value::Null;
        assert_eq!(original, restored);
    }
}
