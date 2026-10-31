//! Regressions for matched excerpts, scope balance and document state boundaries.
use code_intel_store::Store;
use rusqlite::Connection;
use serde_json::Value;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}
fn fixture() -> TempDir {
    let root = TempDir::new().unwrap();
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/knowledge-hardening"),
        root.path(),
    );
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
    String::from_utf8(output.stdout).unwrap()
}
fn json(root: &Path, arguments: &[&str]) -> Value {
    serde_json::from_str(&run(root, arguments)).unwrap()
}
fn integer(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}
fn check_hit(root: &Path, hit: &Value) {
    let source = fs::read_to_string(root.join(hit["path"].as_str().unwrap())).unwrap();
    let section = &hit["section"];
    let start = integer(&section["start_byte"]);
    let end = integer(&section["end_byte"]);
    assert_eq!(&source[start..end], section["content"].as_str().unwrap());
    let starts = code_intel_core::knowledge::line_starts(&source);
    assert_eq!(
        integer(&section["start_line"]),
        starts.partition_point(|at| *at <= start)
    );
    assert_eq!(
        integer(&section["end_line"]),
        starts.partition_point(|at| *at < end)
    );
    assert!(start >= integer(&hit["source_start_byte"]));
    assert!(end <= integer(&hit["source_end_byte"]));
    assert_eq!(hit["freshness"], "matched_at_read");
}

#[test]
fn long_sections_return_matched_late_evidence_with_exact_original_offsets() {
    for newline in ["\n", "\r\n", "\r"] {
        let root = TempDir::new().unwrap();
        let source = format!(
            "# Policy{newline}{}{newline}Café authentication must revoke expired sessions.{newline}",
            format!("Unrelated operational notes.{newline}").repeat(200)
        );
        fs::write(root.path().join("policy.md"), &source).unwrap();
        run(root.path(), &["index", "."]);
        for budget in [1600usize, 2000, 4000] {
            let args = [
                "knowledge",
                "search",
                "cafe authentication",
                "--format",
                "json",
                "--max-bytes",
                &budget.to_string(),
            ];
            let text = run(root.path(), &args);
            let response: Value = serde_json::from_str(&text).unwrap();
            assert!(text.len() <= budget);
            assert_eq!(response["used_bytes"], text.len());
            let hit = &response["results"][0];
            assert!(
                hit["section"]["content"]
                    .as_str()
                    .unwrap()
                    .contains("Café authentication")
            );
            assert_eq!(hit["truncated"], true);
            assert!(integer(&hit["section"]["start_line"]) > 1);
            check_hit(root.path(), hit);
            assert_eq!(text, run(root.path(), &args));
        }
        let context = json(
            root.path(),
            &[
                "context",
                "cafe authentication",
                "--scope",
                "docs",
                "--max-tokens",
                "2000",
            ],
        );
        let item = &context["items"][0];
        assert!(
            item["snippet"]
                .as_str()
                .unwrap()
                .contains("Café authentication")
        );
        assert_eq!(item["snippet_truncated"], true);
        assert_eq!(context["truncated"], true);
        assert_eq!(item["provenance"]["start_line"], 1);
        assert!(integer(&item["start_line"]) > 1);
        let start = integer(&item["start_byte"]);
        let end = integer(&item["end_byte"]);
        assert_eq!(&source[start..end], item["snippet"].as_str().unwrap());
        let focused = json(
            root.path(),
            &[
                "context",
                "cafe authentication",
                "--scope",
                "docs",
                "--detail",
                "focused",
                "--max-tokens",
                "2000",
            ],
        );
        let focused = &focused["items"][0];
        assert!(
            focused["snippet"]
                .as_str()
                .unwrap()
                .contains("Café authentication")
        );
        assert_eq!(
            &source[integer(&focused["start_byte"])..integer(&focused["end_byte"])],
            focused["snippet"].as_str().unwrap()
        );
    }
}

#[test]
fn multibyte_single_line_excerpts_keep_matching_words_and_character_boundaries() {
    let root = TempDir::new().unwrap();
    let source = format!(
        "# Policy\n{} Café must expire credentials {}\n",
        "雪é".repeat(4000),
        "雪é".repeat(4000)
    );
    fs::write(root.path().join("policy.md"), source).unwrap();
    run(root.path(), &["index", "."]);
    let text = run(
        root.path(),
        &[
            "knowledge",
            "search",
            "cafe credentials",
            "--format",
            "json",
            "--max-bytes",
            "2000",
        ],
    );
    assert!(text.len() <= 2000);
    let result: Value = serde_json::from_str(&text).unwrap();
    let hit = &result["results"][0];
    assert!(
        hit["section"]["content"]
            .as_str()
            .unwrap()
            .contains("Café must expire credentials")
    );
    assert_eq!(hit["section"]["start_line"], 2);
    assert_eq!(hit["section"]["end_line"], 2);
    check_hit(root.path(), hit);
}

#[test]
fn rare_query_matches_win_over_repeated_generic_terms() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("policy.md"),
        format!(
            "# Policy\n{}\nSession expiration follows inactivity policy.\n",
            "Session notes without requirements.\n".repeat(200)
        ),
    )
    .unwrap();
    run(root.path(), &["index", "."]);
    let result = json(
        root.path(),
        &[
            "knowledge",
            "search",
            "session expiration inactivity",
            "--format",
            "json",
            "--max-bytes",
            "2000",
        ],
    );
    assert!(
        result["results"][0]["section"]["content"]
            .as_str()
            .unwrap()
            .contains("expiration follows inactivity")
    );
    check_hit(root.path(), &result["results"][0]);
}

#[test]
fn combined_scopes_include_labeled_code_and_requirements_at_two_thousand_bytes() {
    let root = fixture();
    run(root.path(), &["index", "."]);
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../fixtures/knowledge-hardening/cases.json"
    ))
    .unwrap();
    for case in cases
        .iter()
        .filter(|case| !case["code"].as_array().unwrap().is_empty())
    {
        for format in ["json", "text"] {
            let text = run(
                root.path(),
                &[
                    "context",
                    case["query"].as_str().unwrap(),
                    "--scope",
                    "code,docs",
                    "--format",
                    format,
                    "--max-tokens",
                    "2000",
                ],
            );
            assert!(text.len() <= 2000);
            if format == "json" {
                let response: Value = serde_json::from_str(&text).unwrap();
                let items = response["items"].as_array().unwrap();
                assert!(
                    items.iter().any(|item| item["kind"] == "document_section"
                        && case["documents"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|doc| item["path"] == doc[0])),
                    "{response}"
                );
                assert!(
                    items.iter().any(|item| item["kind"] != "document_section"
                        && case["code"].as_array().unwrap().contains(&item["path"])),
                    "{response}"
                );
                assert_eq!(response["estimated_tokens"], text.len());
            } else {
                assert!(text.contains("recorded_intent"));
            }
        }
    }
}

#[test]
fn concurrent_indexers_preserve_consistent_fts_and_inventory() {
    let root = fixture();
    run(root.path(), &["index", "."]);
    fs::write(
        root.path().join("docs/new.md"),
        "# NewPolicy\nUniqueNeedle\n",
    )
    .unwrap();
    let launch = || {
        Command::new(env!("CARGO_BIN_EXE_repogi"))
            .current_dir(root.path())
            .args(["index", "."])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut first = launch();
    let mut second = launch();
    let statuses = [first.wait().unwrap(), second.wait().unwrap()];
    assert!(statuses.iter().any(std::process::ExitStatus::success));
    run(root.path(), &["index", "."]);
    let db = Connection::open(root.path().join(".code-intel/index.db")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let counts: (i64,i64) = db.query_row("SELECT (SELECT count(*) FROM document_sections),(SELECT count(*) FROM document_search)", [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(counts.0, counts.1);
    let hit = json(
        root.path(),
        &["knowledge", "search", "UniqueNeedle", "--format", "json"],
    );
    assert_eq!(hit["results"].as_array().unwrap().len(), 1);
    db.execute(
        "INSERT INTO document_search(document_search) VALUES('integrity-check')",
        [],
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn database_leaf_symlinks_are_rejected_by_storage_itself() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let database = outside.path().join("outside.db");
    fs::write(&database, []).unwrap();
    let alias = root.path().join("alias.db");
    symlink(&database, &alias).unwrap();
    assert!(Store::open_index(&alias, root.path()).is_err());
    assert!(Store::open_read(&alias).is_err());
    assert!(fs::read(&database).unwrap().is_empty());
}

#[test]
fn extractor_upgrade_is_explicit_read_only_and_reparses_documents_once() {
    let root = fixture();
    let first = json(root.path(), &["index", "."]);
    let database = root.path().join(".code-intel/index.db");
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE metadata SET value='1' WHERE key='knowledge_indexed'",
            [],
        )
        .unwrap();
    let before = fs::read(&database).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root.path())
        .args(["knowledge", "search", "session"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("repogi index"));
    run(
        root.path(),
        &["context", "expireSession", "--max-tokens", "8000"],
    );
    assert_eq!(before, fs::read(&database).unwrap());
    let updated = json(root.path(), &["index", "."]);
    assert_eq!(updated["files_changed"], 0);
    assert_eq!(
        updated["documents"]["changed"],
        first["documents"]["changed"]
    );
    assert_eq!(
        integer(&updated["index_revision"]),
        integer(&first["index_revision"]) + 1
    );
    let warm = json(root.path(), &["index", "."]);
    assert_eq!(warm["documents"]["changed"], 0);
    assert_eq!(warm["index_revision"], updated["index_revision"]);
}

#[test]
fn dense_match_locations_are_bounded_without_losing_the_rare_requirement() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("dense.md"),
        format!(
            "# Policy\n{}UniqueNeedle requirement expires sessions.\n",
            "Session note.\n".repeat(20_000)
        ),
    )
    .unwrap();
    run(root.path(), &["index", "."]);
    let store = Store::open_read(&root.path().join(".code-intel/index.db")).unwrap();
    let hits = store
        .search_documents("\"session\"* OR \"UniqueNeedle\"*", 1)
        .unwrap();
    assert_eq!(hits[0].match_locations_limited, Some(true));
    assert!(hits[0].matches.len() <= 2177);
    let response = json(
        root.path(),
        &[
            "knowledge",
            "search",
            "session UniqueNeedle",
            "--format",
            "json",
            "--max-bytes",
            "2000",
        ],
    );
    assert!(
        response["results"][0]["section"]["content"]
            .as_str()
            .unwrap()
            .contains("UniqueNeedle requirement")
    );
    check_hit(root.path(), &response["results"][0]);
}

#[test]
fn build_metadata_failure_rolls_back_code_documents_and_revision() {
    let root = fixture();
    run(root.path(), &["index", "."]);
    let database = root.path().join(".code-intel/index.db");
    let before = {
        let store = Store::open_read(&database).unwrap();
        (
            store.revision().unwrap(),
            store.hashes().unwrap(),
            store.document_hashes().unwrap(),
        )
    };
    Connection::open(&database).unwrap().execute_batch("CREATE TRIGGER reject_build BEFORE UPDATE OF value ON metadata WHEN NEW.key='indexer_build' BEGIN SELECT RAISE(ABORT,'injected build metadata failure'); END;").unwrap();
    fs::write(root.path().join("docs/PRD.md"), "# Changed requirement\n").unwrap();
    fs::write(
        root.path().join("src/session.ts"),
        "export const changed = 1;\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root.path())
        .args(["index", "."])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let store = Store::open_read(&database).unwrap();
    assert_eq!(store.revision().unwrap(), before.0);
    assert_eq!(store.hashes().unwrap(), before.1);
    assert_eq!(store.document_hashes().unwrap(), before.2);
}

#[test]
fn all_term_search_matches_precede_short_partial_matches() {
    let root = fixture();
    run(root.path(), &["index", "."]);
    let response = json(
        root.path(),
        &[
            "knowledge",
            "search",
            "session expiration",
            "--format",
            "json",
            "--max-bytes",
            "4000",
        ],
    );
    assert_eq!(response["results"][0]["path"], "docs/PRD.md");
    assert_eq!(response["results"][0]["match_mode"], "all_terms");
    assert!(
        response["results"][0]["section"]["content"]
            .as_str()
            .unwrap()
            .contains("Session expiration MUST")
    );
    let fallback = json(
        root.path(),
        &[
            "knowledge",
            "search",
            "session nonexistent",
            "--format",
            "json",
        ],
    );
    assert!(!fallback["results"].as_array().unwrap().is_empty());
    assert!(
        fallback["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["match_mode"] == "partial_terms")
    );
}

#[cfg(unix)]
#[test]
fn control_filenames_are_skipped_and_legacy_text_citations_are_escaped() {
    let root = TempDir::new().unwrap();
    let hostile = "\u{1b}]52;c;b3duZWQ=\u{7}.md";
    fs::write(root.path().join(hostile), "# ClipboardPolicy\n").unwrap();
    fs::write(
        root.path().join("policy.md"),
        "# LegacyPolicy\nRecorded intent.\n",
    )
    .unwrap();
    let report = json(root.path(), &["index", "."]);
    assert_eq!(report["documents"]["skipped"], 1);
    assert_eq!(report["documents"]["changed"], 1);
    assert!(
        code_intel_index::selected_source_hash(&root.path().canonicalize().unwrap(), hostile)
            .is_err()
    );
    fs::remove_file(root.path().join(hostile)).unwrap();
    fs::rename(root.path().join("policy.md"), root.path().join(hostile)).unwrap();
    let db = Connection::open(root.path().join(".code-intel/index.db")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    for table in ["documents", "document_sections", "document_search"] {
        db.execute(&format!("UPDATE {table} SET path=?1"), [hostile])
            .unwrap();
    }
    let bytes = fs::read(root.path().join(".code-intel/index.db")).unwrap();
    for args in [
        vec!["knowledge", "search", "LegacyPolicy", "--format", "text"],
        vec![
            "context",
            "LegacyPolicy",
            "--scope",
            "docs",
            "--format",
            "text",
        ],
    ] {
        let text = run(root.path(), &args);
        assert!(!text.contains(['\u{1b}', '\u{7}']), "{text:?}");
        assert!(text.contains("\\u{1b}"), "{text}");
    }
    assert_eq!(
        bytes,
        fs::read(root.path().join(".code-intel/index.db")).unwrap()
    );
}
