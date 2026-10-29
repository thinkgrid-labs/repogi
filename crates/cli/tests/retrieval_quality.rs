//! Public retrieval contract regressions using generated TypeScript source.
use code_intel_core::ContextResponse;
use std::{collections::BTreeSet, fs, path::Path, process::Command};
use tempfile::TempDir;

fn run(root: &Path, args: &[&str]) -> String {
    let result = Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn generated_source_retrieval_is_bounded_deterministic_and_has_no_duplicate_lines() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("leaf.ts"),
        "export function greet(name: string) { return name; }\n",
    )
    .unwrap();
    fs::write(
        root.path().join("user.ts"),
        "import {greet} from './leaf';\nexport function user() { return greet('world'); }\n",
    )
    .unwrap();
    run(root.path(), &["index", "."]);
    for budget in [1000, 2000, 4000, 8000] {
        for format in ["json", "text"] {
            let size = budget.to_string();
            let args = [
                "context",
                "greet user",
                "--max-tokens",
                &size,
                "--format",
                format,
            ];
            let actual = run(root.path(), &args);
            assert!(actual.len() <= budget);
            assert_eq!(actual, run(root.path(), &args));
            if format == "json" {
                let response: ContextResponse = serde_json::from_str(&actual).unwrap();
                assert!(!response.items.is_empty());
                let mut lines = BTreeSet::new();
                for item in response.items {
                    for line in item.start_line..=item.end_line {
                        assert!(lines.insert((item.path.clone(), line)));
                    }
                }
            }
        }
    }
}
