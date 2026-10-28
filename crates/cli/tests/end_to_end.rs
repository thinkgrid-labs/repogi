use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use code_intel_core::{ContextResponse, IndexReport, Status};
use rusqlite::Connection;
use tempfile::TempDir;

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".code-intel" {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

fn fixture() -> TempDir {
    let root = TempDir::new().unwrap();
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/typescript"),
        root.path(),
    );
    root
}

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
        "{:?}: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).unwrap()
}

fn index(root: &Path) -> IndexReport {
    serde_json::from_str(&success(root, &["index", "."])).unwrap()
}
fn status(root: &Path) -> Status {
    serde_json::from_str(&success(root, &["status"])).unwrap()
}
fn database(root: &Path) -> Connection {
    Connection::open(root.join(".code-intel/index.db")).unwrap()
}
fn scalar(root: &Path, sql: &str) -> usize {
    usize::try_from(
        database(root)
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn initial_index_persists_ast_graph_and_provenance_across_processes() {
    let root = fixture();
    let first = index(root.path());
    assert_eq!(first.files_changed, 5);
    assert_eq!(first.index_revision, 1);
    assert_eq!(first.syntax_error_files, 1);
    let state = status(&root.path().join("src"));
    assert_eq!(state.files, 5);
    assert!(state.edges > state.files);
    assert_eq!(state.unresolved_imports, 1); // external audit-package only
    let db = database(root.path());
    let facts: Vec<String> = db
        .prepare("SELECT data FROM facts ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let facts: Vec<code_intel_core::Fact> = facts
        .iter()
        .map(|text| serde_json::from_str(text).unwrap())
        .collect();
    for (kind, name) in [
        ("function", "authenticate"),
        ("class", "AuthService"),
        ("method", "AuthService.check"),
        ("interface", "User"),
        ("method", "User.describe"),
        ("type_alias", "UserId"),
        ("function", "UserBadge"),
        ("function", "entrypoint"),
        ("variable", "authVersion"),
    ] {
        assert!(
            facts
                .iter()
                .any(|fact| fact.kind == kind && fact.name == name),
            "missing {kind} {name}"
        );
    }
    assert!(
        !facts
            .iter()
            .any(|fact| fact.name == "CommentOnlyFake" || fact.name == "AuthService.computed")
    );
    let authenticate = facts
        .iter()
        .find(|fact| fact.name == "authenticate")
        .unwrap();
    assert_eq!((authenticate.start_line, authenticate.end_line), (5, 8));
    assert_eq!(
        authenticate.provenance.extraction_method,
        "tree_sitter_typescript"
    );
    assert_eq!(authenticate.provenance.confidence, "syntax");
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM edges WHERE kind='exports' AND target_id IS NOT NULL"
        ),
        8
    );
    assert!(
        scalar(
            root.path(),
            "SELECT count(*) FROM edges WHERE kind='module_candidate'"
        ) >= 4
    );
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM edges WHERE kind IN ('calls','references')"
        ),
        0
    );
}

#[test]
fn warm_index_skips_parsing_and_keeps_revision_and_context_stable() {
    let root = fixture();
    index(root.path());
    let before = success(
        root.path(),
        &["context", "authenticate", "--max-tokens", "8000"],
    );
    let second = index(root.path());
    assert_eq!(second.files_changed, 0);
    assert_eq!(second.files_unchanged, 5);
    assert_eq!(second.files_removed, 0);
    assert_eq!(second.index_revision, 1);
    assert_eq!(second.syntax_error_files, 1);
    assert_eq!(scalar(root.path(), "SELECT count(*) FROM index_runs"), 2);
    let after = success(
        root.path(),
        &["context", "authenticate", "--max-tokens", "8000"],
    );
    assert_eq!(before, after);
}

#[test]
fn modification_deletion_and_rename_purge_stale_facts_fts_and_edges() {
    let root = fixture();
    index(root.path());
    fs::write(
        root.path().join("src/auth.ts"),
        "export function replacementOnly() { return 2; }\n",
    )
    .unwrap();
    fs::remove_file(root.path().join("src/types.ts")).unwrap();
    fs::rename(
        root.path().join("src/ui.tsx"),
        root.path().join("src/card.tsx"),
    )
    .unwrap();
    let changed = index(root.path());
    assert_eq!(changed.files_changed, 2);
    assert_eq!(changed.files_removed, 2);
    assert_eq!(changed.index_revision, 2);
    assert_eq!(status(root.path()).files, 4);
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM facts WHERE path IN ('src/types.ts','src/ui.tsx')"
        ),
        0
    );
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM search WHERE search MATCH 'authVersion'"
        ),
        0
    );
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM facts WHERE json_extract(data,'$.name')='AuthService.check'"
        ),
        0
    );
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM edges e LEFT JOIN facts f ON f.id=e.target_id WHERE e.target_id IS NOT NULL AND f.id IS NULL"
        ),
        0
    );
    assert!(
        success(
            root.path(),
            &["context", "replacementOnly", "--max-tokens", "4000"]
        )
        .contains("replacementOnly")
    );
}

#[test]
fn discovery_honors_gitignore_without_git_and_excludes_binary_large_generated_files() {
    let root = fixture();
    for directory in [
        "node_modules",
        "dist",
        "generated",
        "private",
        ".code-intel",
    ] {
        fs::create_dir_all(root.path().join(directory)).unwrap();
    }
    for path in [
        "ignored.ts",
        "private/secret.ts",
        "node_modules/dependency.ts",
        "dist/bundle.ts",
        "generated/output.ts",
        ".code-intel/noise.ts",
    ] {
        fs::write(root.path().join(path), "export const excludedSecret = 1;").unwrap();
    }
    fs::write(root.path().join("src/invalid.ts"), [0xff, 0xfe]).unwrap();
    fs::write(
        root.path().join("src/binary.ts"),
        b"export\0const nope = 1;",
    )
    .unwrap();
    fs::write(root.path().join("src/huge.ts"), vec![b'x'; 1_048_577]).unwrap();
    fs::write(
        root.path().join("src/tool.generated.ts"),
        "export const nope = 1;",
    )
    .unwrap();
    fs::write(
        root.path().join("src/header.ts"),
        "// @generated\nexport const nope = 1;",
    )
    .unwrap();
    let report = index(root.path());
    assert_eq!(status(root.path()).files, 5);
    assert_eq!(report.files_skipped, 5);
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM search WHERE search MATCH 'excludedSecret'"
        ),
        0
    );
    // Newly ignored and newly invalid files must purge previous records too.
    fs::write(root.path().join(".gitignore"), "src/auth.ts\n").unwrap();
    fs::write(root.path().join("src/types.ts"), [0]).unwrap();
    assert_eq!(index(root.path()).files_removed, 2);
}

#[test]
fn retrieval_is_deterministic_evidenced_and_bounded_in_both_formats() {
    let root = fixture();
    index(root.path());
    for budget in [700, 1200, 2000, 8000] {
        let text = success(
            root.path(),
            &[
                "context",
                "authenticate",
                "--max-tokens",
                &budget.to_string(),
                "--format",
                "json",
            ],
        );
        let response: ContextResponse = serde_json::from_str(&text).unwrap();
        assert_eq!(response.estimated_tokens, text.len());
        assert!(response.estimated_tokens <= budget);
        assert_eq!(response.budget.method, "estimated_utf8_bytes");
        for item in &response.items {
            assert!(!Path::new(&item.path).is_absolute());
            assert!(item.start_line <= item.end_line);
            assert_eq!(item.provenance.source_path, item.path);
            let source = fs::read_to_string(root.path().join(&item.path)).unwrap();
            let lines: Vec<_> = source.lines().collect();
            assert!(item.end_line <= lines.len());
            let evidence = lines[item.start_line - 1..item.end_line].join("\n");
            assert!(evidence.contains(item.snippet.trim_end_matches('\n')));
        }
        let human = success(
            root.path(),
            &[
                "context",
                "authenticate",
                "--max-tokens",
                &budget.to_string(),
                "--format",
                "text",
            ],
        );
        assert!(human.len() <= budget);
        assert!(human.contains("estimated_utf8_bytes"));
    }
    let result: ContextResponse = serde_json::from_str(&success(
        root.path(),
        &["context", "authenticate", "--max-tokens", "12000"],
    ))
    .unwrap();
    assert!(
        result
            .items
            .iter()
            .any(|item| item.kind == "function" && item.path == "src/auth.ts")
    );
    assert!(
        result
            .items
            .iter()
            .any(|item| item.why.iter().any(|reason| reason.contains("graph")))
    );
    let no_match: ContextResponse =
        serde_json::from_str(&success(root.path(), &["context", "zzzzUnknownIdentifier"])).unwrap();
    assert!(no_match.items.is_empty());
    assert!(!no_match.truncated);
}

#[test]
fn syntax_errors_are_reported_and_valid_declarations_survive() {
    let root = fixture();
    let report = index(root.path());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|item| item.path == "src/broken.ts" && item.message.contains("partial"))
    );
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM facts WHERE json_extract(data,'$.name')='intactBeforeError'"
        ),
        1
    );
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM facts WHERE json_extract(data,'$.name')='broken'"
        ),
        0
    );
    fs::write(
        root.path().join("src/broken.ts"),
        "export function repaired() {}\n",
    )
    .unwrap();
    assert_eq!(index(root.path()).syntax_error_files, 0);
}

#[test]
fn invalid_queries_and_missing_index_have_actionable_machine_readable_errors() {
    let root = fixture();
    let missing = run(root.path(), &["status"]);
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("repogi index")
    );
    index(root.path());
    for args in [
        vec!["context", ""],
        vec!["context", "\" OR * ()", "--max-tokens", "50"],
        vec!["context", "authenticate", "--max-tokens", "50"],
        vec!["context", "authenticate", "--max-tokens", "0"],
    ] {
        let output = run(root.path(), &args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "command_failed");
    }
    // Quoted FTS-like text is treated as lexical words, never executable FTS syntax.
    success(
        root.path(),
        &["context", "\" OR * ()", "--max-tokens", "4000"],
    );
}

#[cfg(unix)]
#[test]
fn source_symlinks_are_skipped_and_state_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let root = fixture();
    let outside = TempDir::new().unwrap();
    fs::write(
        outside.path().join("secret.ts"),
        "export const outsideSecret = 1;",
    )
    .unwrap();
    symlink(
        outside.path().join("secret.ts"),
        root.path().join("src/link.ts"),
    )
    .unwrap();
    symlink(outside.path(), root.path().join("escape")).unwrap();
    let report = index(root.path());
    assert_eq!(report.files_skipped, 1);
    assert_eq!(status(root.path()).files, 5);
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM search WHERE search MATCH 'outsideSecret'"
        ),
        0
    );
    fs::remove_dir_all(root.path().join(".code-intel")).unwrap();
    symlink(outside.path(), root.path().join(".code-intel")).unwrap();
    let output = run(root.path(), &["index", "."]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("symlinked index state")
    );
    assert!(!outside.path().join("index.db").exists());
}

#[test]
fn failed_transaction_keeps_the_entire_previous_index() {
    let root = fixture();
    index(root.path());
    let before = success(
        root.path(),
        &["context", "authenticate", "--max-tokens", "8000"],
    );
    database(root.path()).execute_batch("CREATE TRIGGER reject_update BEFORE INSERT ON facts WHEN NEW.path='src/auth.ts' BEGIN SELECT RAISE(ABORT,'test injected write failure'); END;").unwrap();
    fs::write(
        root.path().join("src/auth.ts"),
        "export function changed() {}\n",
    )
    .unwrap();
    fs::remove_file(root.path().join("src/types.ts")).unwrap();
    let output = run(root.path(), &["index", "."]);
    assert!(!output.status.success());
    assert_eq!(status(root.path()).index_revision, 1);
    assert_eq!(status(root.path()).files, 5);
    let after = success(
        root.path(),
        &["context", "authenticate", "--max-tokens", "8000"],
    );
    let mut previous: serde_json::Value = serde_json::from_str(&before).unwrap();
    let mut current: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(previous["navigation"]["current_source"], "matched_at_read");
    assert_eq!(current["navigation"]["current_source"], "stale");
    assert_eq!(
        current["navigation"]["changed_files"],
        serde_json::json!(["src/auth.ts", "src/types.ts"])
    );
    // Indexed facts survive the failed transaction, while source checks disclose
    // the edited/deleted working-tree files.
    for response in [&mut previous, &mut current] {
        response.as_object_mut().unwrap().remove("navigation");
        response.as_object_mut().unwrap().remove("estimated_tokens");
    }
    assert_eq!(previous, current);
}

#[test]
fn ambiguous_and_escaped_imports_stay_unresolved_until_a_unique_file_exists() {
    let root = fixture();
    fs::write(
        root.path().join("src/ambiguous.ts"),
        "export const first = 1;\n",
    )
    .unwrap();
    fs::write(
        root.path().join("src/ambiguous.tsx"),
        "export const second = <div/>;\n",
    )
    .unwrap();
    fs::write(
        root.path().join("src/consumer.ts"),
        r#"import { first } from "./ambiguous";
import { first as other } from "./amb\u0069guous";
export const marker = first;
"#,
    )
    .unwrap();
    index(root.path());
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM edges WHERE path='src/consumer.ts' AND kind='module_candidate'"
        ),
        0
    );
    assert_eq!(status(root.path()).unresolved_imports, 3);
    fs::remove_file(root.path().join("src/ambiguous.tsx")).unwrap();
    index(root.path());
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM edges WHERE path='src/consumer.ts' AND kind='module_candidate' AND json_extract(data,'$.provenance.confidence')='path_heuristic'"
        ),
        1
    );
    assert_eq!(status(root.path()).unresolved_imports, 2);
}

#[test]
fn budget_accounts_for_unicode_escaping_and_large_multiline_snippet_truncation() {
    let root = fixture();
    let body = "  // café 雪 \\\" evidence\n".repeat(2000);
    fs::write(
        root.path().join("src/unicode.ts"),
        format!("export function unicodeEvidence() {{\n{body}}}\n\n"),
    )
    .unwrap();
    index(root.path());
    let query = "unicodeEvidence café 雪 \" \\";
    for format in ["json", "text"] {
        let output = success(
            root.path(),
            &["context", query, "--max-tokens", "2000", "--format", format],
        );
        assert!(output.len() <= 2000);
        if format == "json" {
            let response: ContextResponse = serde_json::from_str(&output).unwrap();
            assert_eq!(response.estimated_tokens, output.len());
            assert_eq!(response.query, query);
            assert!(response.truncated);
            assert!(
                response
                    .items
                    .iter()
                    .any(|item| item.path == "src/unicode.ts"
                        && item.end_line < item.provenance.end_line)
            );
        }
    }
}
