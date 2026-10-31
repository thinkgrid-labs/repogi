use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use code_intel_core::IndexReport;
use code_intel_store::Store;
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
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
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/knowledge"),
        root.path(),
    );
    root
}
fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}
fn success(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn index(root: &Path) -> IndexReport {
    serde_json::from_str(&success(root, &["index", "."])).unwrap()
}
fn db(root: &Path) -> Connection {
    Connection::open(root.join(".code-intel/index.db")).unwrap()
}
fn scalar(root: &Path, sql: &str) -> i64 {
    db(root).query_row(sql, [], |row| row.get(0)).unwrap()
}
fn read(root: &Path) -> Store {
    Store::open_read(&root.join(".code-intel/index.db")).unwrap()
}

#[test]
fn incremental_documents_do_not_rebind_code_and_deleted_search_records_are_purged() {
    let root = fixture();
    let initial = index(root.path());
    assert_eq!(initial.documents.changed, 3);
    assert_eq!(initial.files_changed, 1);
    assert_eq!(scalar(root.path(), "PRAGMA user_version"), 4);
    let graph = success(root.path(), &["graph"]);
    let cold_hits = read(root.path())
        .search_documents("\"expiration\"*", 10)
        .unwrap();
    assert_eq!(cold_hits[0].section.start_line, 12);
    let warm = index(root.path());
    assert_eq!(warm.index_revision, initial.index_revision);
    assert_eq!(warm.documents.unchanged, 3);
    assert_eq!(warm.documents.changed, 0);
    assert_eq!(graph, success(root.path(), &["graph"]));
    let path = root.path().join("docs/PRD.md");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("thirty", "sixty"),
    )
    .unwrap();
    fs::remove_file(root.path().join("docs/cache.markdown")).unwrap();
    let changed = index(root.path());
    assert_eq!(changed.index_revision, initial.index_revision + 1);
    assert_eq!(changed.documents.changed, 1);
    assert_eq!(changed.documents.removed, 1);
    assert_eq!(changed.files_changed, 0);
    let hits = read(root.path()).search_documents("\"sixty\"", 10).unwrap();
    assert_eq!(hits[0].section.id, cold_hits[0].section.id);
    assert_eq!(hits[0].index_revision, changed.index_revision);
    assert_eq!(
        scalar(
            root.path(),
            "SELECT count(*) FROM document_search WHERE path='docs/cache.markdown'"
        ),
        0
    );
    let before: Value = serde_json::from_str(&graph).unwrap();
    let after: Value = serde_json::from_str(&success(root.path(), &["graph"])).unwrap();
    assert_eq!(before["edges"], after["edges"]);
}

#[test]
fn document_failure_rolls_back_code_documents_and_revision_together() {
    let root = fixture();
    let initial = index(root.path());
    db(root.path()).execute_batch("CREATE TRIGGER reject_document BEFORE INSERT ON documents BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    fs::write(root.path().join("docs/PRD.md"), "# Changed document\n").unwrap();
    fs::write(
        root.path().join("src/session.ts"),
        "export const different = 1;\n",
    )
    .unwrap();
    fs::remove_file(root.path().join("README.md")).unwrap();
    assert!(!run(root.path(), &["index", "."]).status.success());
    assert_eq!(
        read(root.path()).revision().unwrap(),
        initial.index_revision
    );
    assert_eq!(read(root.path()).document_hashes().unwrap().len(), 3);
    assert!(
        !read(root.path())
            .search_documents("\"expiration\"", 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        success(
            root.path(),
            &["context", "expireSession", "--max-tokens", "8000"]
        )
        .contains("expireSession")
    );
}

#[test]
fn old_schemas_are_read_without_writes_then_migrate_without_graph_loss() {
    for version in [1, 2] {
        let root = fixture();
        index(root.path());
        // Build a real old-schema fixture from the shipped migration scripts.
        let old = root.path().join("old.db");
        let connection = Connection::open(&old).unwrap();
        connection
            .execute_batch(include_str!("../../store/src/migration_1.sql"))
            .unwrap();
        if version == 2 {
            connection
                .execute_batch(include_str!("../../store/src/migration_2.sql"))
                .unwrap();
        }
        connection
            .execute(
                "ATTACH ?1 AS current",
                [root.path().join(".code-intel/index.db").to_str().unwrap()],
            )
            .unwrap();
        for table in [
            "metadata",
            "files",
            "facts",
            "edges",
            "search",
            "index_runs",
        ] {
            connection
                .execute_batch(&format!(
                    "INSERT INTO main.{table} SELECT * FROM current.{table}"
                ))
                .unwrap();
        }
        connection
            .execute_batch("DELETE FROM metadata WHERE key='knowledge_indexed';")
            .unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(connection);
        fs::rename(&old, root.path().join(".code-intel/index.db")).unwrap();
        let bytes = fs::read(root.path().join(".code-intel/index.db")).unwrap();
        assert!(!read(root.path()).knowledge_available().unwrap());
        assert!(
            read(root.path())
                .search_documents("\"expiration\"", 10)
                .unwrap_err()
                .to_string()
                .contains("repogi index")
        );
        success(
            root.path(),
            &["context", "expireSession", "--max-tokens", "8000"],
        );
        assert_eq!(
            bytes,
            fs::read(root.path().join(".code-intel/index.db")).unwrap()
        );
        assert_eq!(scalar(root.path(), "PRAGMA user_version"), version);
        let facts = scalar(root.path(), "SELECT count(*) FROM facts");
        index(root.path());
        assert_eq!(scalar(root.path(), "PRAGMA user_version"), 4);
        assert_eq!(scalar(root.path(), "SELECT count(*) FROM facts"), facts);
        assert!(read(root.path()).knowledge_available().unwrap());
    }
}

#[test]
fn empty_and_code_only_repositories_initialize_empty_knowledge() {
    let root = TempDir::new().unwrap();
    for code in [false, true] {
        if code {
            fs::write(root.path().join("empty.ts"), "export const value = 1;").unwrap();
        }
        let report = index(root.path());
        assert_eq!(report.documents.scanned, 0);
        assert!(
            read(root.path())
                .search_documents("\"anything\"", 10)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn discovery_uses_shared_limits_ignores_and_generated_state_exclusions() {
    let root = fixture();
    fs::write(root.path().join(".gitignore"), "private/\nignored.md\n").unwrap();
    for directory in [
        "private",
        "node_modules",
        "generated",
        "dist",
        "target",
        ".code-intel",
    ] {
        fs::create_dir_all(root.path().join(directory)).unwrap();
        fs::write(
            root.path().join(directory).join("secret.md"),
            "# excludedSecret\n",
        )
        .unwrap();
    }
    fs::write(root.path().join("ignored.md"), "excludedSecret").unwrap();
    fs::write(root.path().join("bad.md"), [0xff]).unwrap();
    fs::write(root.path().join("binary.md"), [0]).unwrap();
    fs::write(root.path().join("huge.md"), vec![b'x'; 1_048_577]).unwrap();
    fs::write(root.path().join("empty.MD"), "").unwrap();
    let report = index(root.path());
    assert_eq!(report.documents.skipped, 3);
    assert_eq!(report.documents.changed, 4);
    assert!(
        read(root.path())
            .search_documents("\"excludedSecret\"", 10)
            .unwrap()
            .is_empty()
    );
    fs::write(root.path().join("docs/PRD.md"), [0]).unwrap();
    let report = index(root.path());
    assert_eq!(report.documents.removed, 1);
    assert!(
        read(root.path())
            .search_documents("\"expiration\"", 10)
            .unwrap()
            .is_empty()
    );
}

fn search(root: &Path, query: &str, extra: &[&str]) -> Value {
    let mut args = vec!["knowledge", "search", query, "--format", "json"];
    args.extend_from_slice(extra);
    serde_json::from_str(&success(root, &args)).unwrap()
}

#[test]
fn retrieval_evaluation_returns_rank_one_correctly_cited_sections() {
    let root = fixture();
    index(root.path());
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/knowledge-evaluation.json")).unwrap();
    for case in cases {
        let response = search(root.path(), case["query"].as_str().unwrap(), &[]);
        let top = &response["results"][0];
        assert_eq!(top["path"], case["path"]);
        for key in ["heading", "start_line", "end_line"] {
            assert_eq!(top["section"][key], case[key]);
        }
        assert_eq!(top["freshness"], "matched_at_read");
        assert_eq!(top["index_revision"], 1);
        assert!(top["relevance"].as_f64().unwrap() < 0.0);
        assert_eq!(response["counts_exact"], true);
        assert_eq!(response["truncated"], false);
        assert_eq!(
            response,
            search(root.path(), case["query"].as_str().unwrap(), &[])
        );
    }
}

#[test]
fn lexical_ties_limits_and_untrusted_fts_syntax_are_deterministic() {
    let root = fixture();
    for path in ["docs/a.md", "docs/b.md", "docs/c.md"] {
        fs::write(root.path().join(path), "# Zebra\nTieWord\n").unwrap();
    }
    index(root.path());
    let all = search(root.path(), "TieWord", &[]);
    assert_eq!(
        all["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["docs/a.md", "docs/b.md", "docs/c.md"]
    );
    let limited = search(root.path(), "TieWord", &["--limit", "1"]);
    assert_eq!(limited["results"].as_array().unwrap().len(), 1);
    assert_eq!(limited["counts_exact"], false);
    assert_eq!(limited["candidate_limit_reached"], true);
    assert_eq!(limited["matched_sections_at_least"], 2);
    search(root.path(), "TieWord\" OR * () NOT", &[]);
    for query in [
        String::new(),
        "!!!".into(),
        "x".repeat(8193),
        "term ".repeat(33),
    ] {
        let output = run(root.path(), &["knowledge", "search", &query]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let empty = search(root.path(), "noSuchRequirement", &[]);
    assert!(empty["results"].as_array().unwrap().is_empty());
    assert_eq!(empty["truncated"], false);
}

#[test]
fn search_flags_missing_stale_and_unreadable_sources_without_mutating_index() {
    let root = fixture();
    index(root.path());
    let bytes = fs::read(root.path().join(".code-intel/index.db")).unwrap();
    fs::write(root.path().join("docs/PRD.md"), "# Changed intent\n").unwrap();
    assert_eq!(
        search(root.path(), "inactivity", &[])["results"][0]["freshness"],
        "stale"
    );
    fs::remove_file(root.path().join("docs/PRD.md")).unwrap();
    assert_eq!(
        search(root.path(), "inactivity", &[])["results"][0]["freshness"],
        "missing"
    );
    fs::create_dir(root.path().join("docs/PRD.md")).unwrap();
    assert_eq!(
        search(root.path(), "inactivity", &[])["results"][0]["freshness"],
        "unverified"
    );
    assert_eq!(
        bytes,
        fs::read(root.path().join(".code-intel/index.db")).unwrap()
    );
}

#[test]
fn search_budgets_account_for_json_escaping_unicode_and_source_prefixes() {
    let root = fixture();
    fs::write(
        root.path().join("docs/large.md"),
        format!(
            "# BudgetEvidence\n{}",
            "café 雪 \\\" evidence\n".repeat(2000)
        ),
    )
    .unwrap();
    index(root.path());
    for format in ["json", "text"] {
        for budget in [700usize, 1200, 2000, 8000] {
            let text = success(
                root.path(),
                &[
                    "knowledge",
                    "search",
                    "BudgetEvidence",
                    "--format",
                    format,
                    "--max-bytes",
                    &budget.to_string(),
                ],
            );
            assert!(text.len() <= budget);
            if format == "json" {
                let response: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(response["used_bytes"], text.len());
                assert_eq!(response["truncated"], true);
                if let Some(hit) = response["results"].as_array().unwrap().first() {
                    assert_eq!(hit["truncated"], true);
                    let end =
                        usize::try_from(hit["section"]["end_line"].as_u64().unwrap()).unwrap();
                    let source = fs::read_to_string(root.path().join("docs/large.md")).unwrap();
                    assert_eq!(
                        hit["section"]["content"],
                        source.split_inclusive('\n').take(end).collect::<String>()
                    );
                    assert!(
                        end < usize::try_from(hit["source_end_line"].as_u64().unwrap()).unwrap()
                    );
                }
            }
        }
    }
    let error = run(
        root.path(),
        &["knowledge", "search", "BudgetEvidence", "--max-bytes", "50"],
    );
    assert!(!error.status.success());
    assert!(error.stdout.is_empty());
}

fn assert_source_items(root: &Path, items: &[Value], scope: &str) {
    for (position, item) in items.iter().enumerate() {
        if scope == "docs" {
            assert_eq!(item["kind"], "document_section");
        }
        if item["kind"] == "document_section" {
            assert_eq!(item["confidence"], "recorded_intent");
        }
        let source = fs::read_to_string(root.join(item["path"].as_str().unwrap())).unwrap();
        let start = usize::try_from(item["start_line"].as_u64().unwrap()).unwrap();
        let end = usize::try_from(item["end_line"].as_u64().unwrap()).unwrap();
        let lines: Vec<_> = source.lines().collect();
        assert!(
            lines[start - 1..end]
                .join("\n")
                .contains(item["snippet"].as_str().unwrap().trim_end_matches('\n'))
        );
        for other in &items[position + 1..] {
            if item["path"] == other["path"] {
                assert!(
                    end < usize::try_from(other["start_line"].as_u64().unwrap()).unwrap()
                        || start > usize::try_from(other["end_line"].as_u64().unwrap()).unwrap()
                );
            }
        }
    }
}

#[test]
fn context_scopes_reuse_existing_packing_and_distinguish_document_intent() {
    let root = fixture();
    index(root.path());
    let code = success(
        root.path(),
        &["context", "session", "--max-tokens", "12000"],
    );
    assert_eq!(
        code,
        success(
            root.path(),
            &[
                "context",
                "session",
                "--scope",
                "code",
                "--max-tokens",
                "12000"
            ]
        )
    );
    for scope in ["docs", "code,docs"] {
        for format in ["json", "text"] {
            for budget in [2000usize, 8000] {
                let text = success(
                    root.path(),
                    &[
                        "context",
                        "session",
                        "--scope",
                        scope,
                        "--format",
                        format,
                        "--max-tokens",
                        &budget.to_string(),
                    ],
                );
                assert!(text.len() <= budget);
                if format == "json" {
                    let response: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(response["estimated_tokens"], text.len());
                    assert_eq!(response["knowledge_scope"], scope);
                    let items = response["items"].as_array().unwrap();
                    if budget == 8000 {
                        assert!(items.iter().any(|item| item["kind"] == "document_section"));
                        assert_eq!(
                            items.iter().any(|item| item["path"] == "src/session.ts"),
                            scope == "code,docs"
                        );
                    }
                    assert_source_items(root.path(), items, scope);
                    assert!(
                        response["relationships"]
                            .as_array()
                            .is_none_or(Vec::is_empty)
                    );
                } else {
                    assert!(text.contains("record project intent"));
                }
            }
        }
    }
    assert!(
        !run(root.path(), &["context", "session", "--scope", "unknown"])
            .status
            .success()
    );
    assert!(
        !run(
            root.path(),
            &[
                "context",
                "session",
                "--scope",
                "docs",
                "--retrieval",
                "lexical"
            ]
        )
        .status
        .success()
    );
}

#[cfg(unix)]
#[test]
fn document_symlinks_foreign_indexes_and_paths_are_confined() {
    use std::os::unix::fs::symlink;
    let root = fixture();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret.md"), "# OutsideSecret\n").unwrap();
    symlink(
        outside.path().join("secret.md"),
        root.path().join("link.md"),
    )
    .unwrap();
    symlink(outside.path(), root.path().join("escape")).unwrap();
    let report = index(root.path());
    assert_eq!(report.documents.skipped, 1);
    assert!(
        search(root.path(), "OutsideSecret", &[])["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for path in [
        "../secret.md",
        "/secret.md",
        "escape/secret.md",
        "link.md",
        "docs/../../secret.md",
        "docs\\secret.md",
    ] {
        assert!(code_intel_index::selected_source_hash(root.path(), path).is_err());
    }
    fs::remove_file(root.path().join("docs/PRD.md")).unwrap();
    symlink(
        outside.path().join("secret.md"),
        root.path().join("docs/PRD.md"),
    )
    .unwrap();
    let hit = search(root.path(), "inactivity", &[]);
    assert_eq!(hit["results"][0]["freshness"], "unverified");
    assert!(!hit.to_string().contains("OutsideSecret"));
    fs::create_dir(outside.path().join(".code-intel")).unwrap();
    fs::copy(
        root.path().join(".code-intel/index.db"),
        outside.path().join(".code-intel/index.db"),
    )
    .unwrap();
    let bytes = fs::read(outside.path().join(".code-intel/index.db")).unwrap();
    assert!(
        !run(outside.path(), &["knowledge", "search", "inactivity"])
            .status
            .success()
    );
    assert!(!run(outside.path(), &["index", "."]).status.success());
    assert_eq!(
        bytes,
        fs::read(outside.path().join(".code-intel/index.db")).unwrap()
    );
}

#[test]
fn excessive_document_metadata_is_reported_as_incomplete_coverage() {
    let root = fixture();
    fs::write(root.path().join("docs/many.md"), "# Section\n".repeat(4097)).unwrap();
    fs::write(
        root.path().join("docs/long.md"),
        format!("# {}\n", "x".repeat(1025)),
    )
    .unwrap();
    fs::write(
        root.path().join("docs/fenced.md"),
        "# Malformed\n```md\n# remains fenced\n",
    )
    .unwrap();
    let report = index(root.path());
    assert_eq!(report.documents.skipped, 2);
    assert_eq!(report.documents.changed, 4);
    let result = search(root.path(), "inactivity", &[]);
    assert_eq!(result["index_skipped_files"], 2);
    let context: Value = serde_json::from_str(&success(
        root.path(),
        &[
            "context",
            "inactivity",
            "--scope",
            "docs",
            "--max-tokens",
            "8000",
        ],
    ))
    .unwrap();
    assert_eq!(context["knowledge_skipped_files"], 2);
    assert_eq!(
        search(root.path(), "Malformed", &[])["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn document_context_discloses_stale_and_missing_intent() {
    let root = fixture();
    index(root.path());
    for (missing, expected) in [(false, "stale"), (true, "missing")] {
        if missing {
            fs::remove_file(root.path().join("docs/PRD.md")).unwrap();
        } else {
            fs::write(root.path().join("docs/PRD.md"), "# Other intent\n").unwrap();
        }
        let response: Value = serde_json::from_str(&success(
            root.path(),
            &[
                "context",
                "inactivity",
                "--scope",
                "docs",
                "--max-tokens",
                "8000",
            ],
        ))
        .unwrap();
        assert_eq!(response["navigation"]["current_source"], "stale");
        assert!(
            response["items"][0]["why"]
                .as_array()
                .unwrap()
                .iter()
                .any(|why| why == &format!("selected source={expected}"))
        );
    }
}
