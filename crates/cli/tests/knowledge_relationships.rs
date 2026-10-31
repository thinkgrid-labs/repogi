//! Fixture-backed contracts: explicit claims, atomic updates and read-only traces.
use code_intel_core::{
    IndexReport,
    relationships::{EntityKind, RelationshipKind, ResolutionStatus},
};
use code_intel_retrieval::{Format, relationships::trace};
use code_intel_store::Store;
use rusqlite::Connection;
use std::{fmt::Write, fs, path::Path, process::Command};
use tempfile::TempDir;

fn copy(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}
fn fixture() -> TempDir {
    let temp = TempDir::new().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/knowledge-relationships"),
        temp.path(),
    );
    temp
}
fn index(root: &Path) -> IndexReport {
    code_intel_index::index(root).unwrap()
}
fn store(root: &Path) -> Store {
    Store::open_read(&root.join(".code-intel/index.db")).unwrap()
}
fn db(root: &Path) -> Connection {
    Connection::open(root.join(".code-intel/index.db")).unwrap()
}
fn run(root: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn requirements_have_explicit_ids_subtree_ranges_acceptance_and_no_prose_inference() {
    let root = fixture();
    index(root.path());
    let read = store(root.path());
    let req = read
        .knowledge_entities("REQ-AUTH-001", 2)
        .unwrap()
        .remove(0);
    assert_eq!(req.kind, EntityKind::Requirement);
    assert_eq!(req.source.start_line, 3);
    assert_eq!(req.acceptance_criteria.len(), 1);
    let source = fs::read_to_string(root.path().join(&req.source.path)).unwrap();
    assert!(
        source[req.source.start_byte..req.source.end_byte]
            .contains("Expired sessions are rejected")
    );
    assert!(!source[req.source.start_byte..req.source.end_byte].contains("## REQ-AUTH-002"));
    assert!(
        read.knowledge_entities("REQ-FAKE-001", 2)
            .unwrap()
            .is_empty()
    );
    assert_eq!(read.knowledge_entities("REQ-AUTH-002", 2).unwrap().len(), 1);
    let duplicate = read.knowledge_entities("REQ-DUP-001", 3).unwrap();
    assert_eq!(duplicate.len(), 2);
    assert!(
        duplicate
            .iter()
            .all(|e| e.diagnostics.iter().any(|d| d.contains("duplicate")))
    );
    let trace = trace(&read, "REQ-DUP-001", 8, 32000, Format::Json).unwrap();
    assert_eq!(trace.selection, "ambiguous");
    assert!(trace.relationships.is_empty());
}

#[test]
fn explicit_paths_targets_and_uncertainties_are_never_behavioral_confirmation() {
    let root = fixture();
    index(root.path());
    let read = store(root.path());
    let req = read
        .knowledge_entities("REQ-AUTH-001", 1)
        .unwrap()
        .remove(0);
    let links = read.knowledge_neighbors(&req.id, 257).unwrap();
    let get = |target: &str| links.iter().find(|e| e.target == target).unwrap();
    for target in [
        "../src/session.ts",
        "../src/session%20policy.ts",
        "ADR-0004.md#ADR-0004",
    ] {
        assert_eq!(get(target).status, ResolutionStatus::Validated);
        assert!(get(target).target_id.is_some());
    }
    assert_eq!(
        get("../src/session.ts").kind,
        RelationshipKind::DeclaredImplementation
    );
    for target in ["../src/missing.ts", "../src/session.ts#expireSession"] {
        assert_eq!(get(target).status, ResolutionStatus::Unresolved);
    }
    assert_eq!(get("#REQ-DUP-001").status, ResolutionStatus::Ambiguous);
    for target in [
        "../../outside.ts",
        "%2e%2e/%2e%2e/outside.ts",
        "../src/%ZZ.ts",
    ] {
        assert_eq!(get(target).status, ResolutionStatus::Invalid);
    }
    for target in [
        "https://example.invalid/session.ts",
        "javascript:alert%281%29",
    ] {
        assert_eq!(get(target).status, ResolutionStatus::Unsupported);
    }
    assert!(!links.iter().any(|e| e.target.contains("fake.ts")));
    for link in links
        .iter()
        .filter(|e| e.kind != RelationshipKind::Contains)
    {
        let original = fs::read_to_string(root.path().join(&link.evidence.path)).unwrap();
        let cited = &original[link.evidence.start_byte..link.evidence.end_byte];
        assert!(cited.contains(&link.target), "{cited}");
        assert_eq!(
            link.evidence.start_line,
            code_intel_core::knowledge::line_starts(&original)
                .partition_point(|at| *at <= link.evidence.start_byte)
        );
    }
}

#[test]
fn adr_status_history_chains_cycles_and_conflicting_metadata_are_preserved() {
    let root = fixture();
    index(root.path());
    let read = store(root.path());
    for (id, status) in [
        ("ADR-0002", "superseded"),
        ("ADR-0004", "accepted"),
        ("ADR-0005", "rejected"),
        ("ADR-0006", "deprecated"),
        ("ADR-0010", "proposed"),
    ] {
        let entity = read.knowledge_entities(id, 1).unwrap().remove(0);
        assert_eq!(entity.decision_status.as_deref(), Some(status));
    }
    let response = trace(&read, "ADR-0002", 3, 100_000, Format::Json).unwrap();
    assert!(
        response
            .entities
            .iter()
            .any(|e| e.explicit_id.as_deref() == Some("ADR-0004"))
    );
    for id in ["ADR-0010", "ADR-0011"] {
        assert!(
            read.knowledge_entities(id, 1).unwrap()[0]
                .diagnostics
                .iter()
                .any(|d| d.contains("cycle"))
        );
    }
    let rejected = read.knowledge_entities("ADR-0005", 1).unwrap().remove(0);
    assert!(
        read.knowledge_neighbors(&rejected.id, 257)
            .unwrap()
            .iter()
            .any(|e| e.target == "ADR-404" && e.status == ResolutionStatus::Unresolved)
    );
    drop(read);
    fs::write(
        root.path().join("docs/malformed.md"),
        "---\nid: ADR-0020\nstatus: accepted\nstatus: rejected\n---\n# Bad metadata\n",
    )
    .unwrap();
    fs::write(
        root.path().join("docs/tagged.md"),
        "---\nid: !execute ADR-0021\nstatus: accepted\n---\n# No execution\n",
    )
    .unwrap();
    index(root.path());
    let read = store(root.path());
    assert!(read.knowledge_entities("ADR-0020", 1).unwrap().is_empty());
    assert!(read.knowledge_entities("ADR-0021", 1).unwrap().is_empty());
    assert!(
        read.knowledge_entities("document:docs/malformed.md", 1)
            .unwrap()[0]
            .diagnostics
            .iter()
            .any(|d| d.contains("duplicate"))
    );
}

#[test]
fn exact_symbol_identity_is_path_constrained_and_never_name_resolved() {
    let root = fixture();
    index(root.path());
    let fact = store(root.path())
        .file_facts("src/session.ts", 20)
        .unwrap()
        .into_iter()
        .find(|f| f.name == "expireSession")
        .unwrap();
    fs::write(root.path().join("docs/symbols.md"),format!("# REQ-SYM-001: Exact binding\n[valid](../src/session.ts#{})\n[wrong path](../src/session%20policy.ts#{})\n[invalid](../src/session.ts#expireSession)\n",fact.id,fact.id)).unwrap();
    index(root.path());
    let read = store(root.path());
    let req = read.knowledge_entities("REQ-SYM-001", 1).unwrap().remove(0);
    let edges = read.knowledge_neighbors(&req.id, 257).unwrap();
    assert_eq!(
        edges
            .iter()
            .find(|e| e.target.ends_with(&fact.id) && e.target.starts_with("../src/session.ts"))
            .unwrap()
            .target_id
            .as_deref(),
        Some(fact.id.as_str())
    );
    assert!(
        edges
            .iter()
            .filter(|e| e.target.contains("policy") || e.target.ends_with("#expireSession"))
            .all(|e| e.status == ResolutionStatus::Unresolved)
    );
}

#[test]
fn incremental_identity_target_changes_deletion_and_recovery_are_transactional() {
    let root = fixture();
    let cold = index(root.path());
    let req = store(root.path())
        .knowledge_entities("REQ-AUTH-001", 1)
        .unwrap()
        .remove(0);
    let warm = index(root.path());
    assert_eq!(cold.index_revision, warm.index_revision);
    assert_eq!(warm.documents.changed, 0);
    let path = root.path().join("docs/PRD.md");
    let content = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        content
            .replace("Session expiration", "Changed title")
            .replace("This is project intent.", "More unrelated body text."),
    )
    .unwrap();
    index(root.path());
    assert_eq!(
        store(root.path())
            .knowledge_entities("REQ-AUTH-001", 1)
            .unwrap()[0]
            .id,
        req.id
    );
    fs::write(
        root.path().join("src/session.ts"),
        "export function expireSession(): boolean { return false; }\n",
    )
    .unwrap();
    let changed = index(root.path());
    assert_eq!(changed.documents.changed, 0);
    let read = store(root.path());
    let edge = read
        .knowledge_neighbors(&req.id, 257)
        .unwrap()
        .into_iter()
        .find(|e| e.target == "../src/session.ts")
        .unwrap();
    assert_eq!(edge.status, ResolutionStatus::Validated);
    assert_eq!(edge.target_hash, read.file_hash("src/session.ts").unwrap());
    drop(read);
    fs::rename(
        root.path().join("src/session.ts"),
        root.path().join("src/renamed.ts"),
    )
    .unwrap();
    let before = trace(&store(root.path()), "REQ-AUTH-001", 1, 32000, Format::Json).unwrap();
    assert!(
        before
            .relationships
            .iter()
            .any(|e| e.target == "../src/session.ts" && e.freshness == "stale")
    );
    assert_eq!(index(root.path()).documents.changed, 0);
    assert!(
        store(root.path())
            .knowledge_neighbors(&req.id, 257)
            .unwrap()
            .iter()
            .any(|e| e.target == "../src/session.ts"
                && e.status == ResolutionStatus::Stale
                && e.target_id.is_none())
    );
    fs::write(
        root.path().join("src/unrelated.ts"),
        "export const other=1;",
    )
    .unwrap();
    index(root.path());
    assert!(
        store(root.path())
            .knowledge_neighbors(&req.id, 257)
            .unwrap()
            .iter()
            .any(|e| e.target == "../src/session.ts" && e.status == ResolutionStatus::Stale)
    );
    fs::write(
        &path,
        content.replace("../src/session.ts", "../src/renamed.ts"),
    )
    .unwrap();
    index(root.path());
    assert!(
        store(root.path())
            .knowledge_neighbors(&req.id, 257)
            .unwrap()
            .iter()
            .any(|e| e.target == "../src/renamed.ts" && e.status == ResolutionStatus::Validated)
    );
    fs::remove_file(path).unwrap();
    index(root.path());
    assert!(
        store(root.path())
            .knowledge_entities("REQ-AUTH-001", 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn injected_relationship_failure_rolls_back_both_inventories_and_revision() {
    let root = fixture();
    let report = index(root.path());
    let read = store(root.path());
    let hashes = read.hashes().unwrap();
    let docs = read.document_hashes().unwrap();
    drop(read);
    db(root.path()).execute_batch("CREATE TRIGGER fail_relationship BEFORE INSERT ON knowledge_relationships BEGIN SELECT RAISE(ABORT,'relationship failure'); END;").unwrap();
    fs::write(
        root.path().join("src/session.ts"),
        "export const altered=true;",
    )
    .unwrap();
    fs::write(
        root.path().join("docs/PRD.md"),
        "# REQ-ROLLBACK-001: Do not partially commit\n[target](../src/session.ts)\n",
    )
    .unwrap();
    assert!(code_intel_index::index(root.path()).is_err());
    let read = store(root.path());
    assert_eq!(read.revision().unwrap(), report.index_revision);
    assert_eq!(hashes, read.hashes().unwrap());
    assert_eq!(docs, read.document_hashes().unwrap());
    assert!(
        !read
            .knowledge_entities("REQ-AUTH-001", 1)
            .unwrap()
            .is_empty()
    );
    assert!(
        read.knowledge_entities("REQ-ROLLBACK-001", 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn traces_are_deterministic_bounded_cli_compatible_and_leave_database_bytes_unchanged() {
    let root = fixture();
    index(root.path());
    let before = fs::read(root.path().join(".code-intel/index.db")).unwrap();
    let args = [
        "knowledge",
        "trace",
        "REQ-AUTH-001",
        "--depth",
        "2",
        "--format",
        "json",
        "--max-bytes",
        "8000",
    ];
    let text = run(root.path(), &args);
    assert_eq!(text, run(root.path(), &args));
    let response: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(response["used_bytes"], text.len());
    assert!(text.len() <= 8000);
    assert_eq!(response["selection"], "unique");
    assert!(trace(&store(root.path()), "REQ-AUTH-001", 1, 20, Format::Json).is_err());
    for bytes in [1800, 4000, 8000, 32000] {
        let response = trace(&store(root.path()), "REQ-AUTH-001", 2, bytes, Format::Json).unwrap();
        assert!(response.used_bytes <= bytes);
        if bytes == 1800 {
            assert!(response.truncated);
            assert!(response.omitted_relationships > 0);
        }
    }
    let response = trace(&store(root.path()), "ADR-0010", 8, 32000, Format::Json).unwrap();
    assert!(!response.work_limit_reached);
    run(
        root.path(),
        &[
            "knowledge",
            "trace",
            "ADR-0004",
            "--depth",
            "1",
            "--format",
            "text",
        ],
    );
    assert_eq!(
        before,
        fs::read(root.path().join(".code-intel/index.db")).unwrap()
    );
}

#[test]
fn combined_context_uses_explicit_path_when_code_has_no_lexical_overlap() {
    let root = fixture();
    fs::write(root.path().join("docs/unique.md"),"# REQ-UNIQUE-001: Quasar handling\n### Related implementation\n[Code](../src/session.ts)\n").unwrap();
    index(root.path());
    let code: serde_json::Value = serde_json::from_str(&run(
        root.path(),
        &[
            "context",
            "quasar",
            "--scope",
            "code",
            "--max-tokens",
            "16000",
        ],
    ))
    .unwrap();
    assert!(code["items"].as_array().unwrap().is_empty());
    let combined: serde_json::Value = serde_json::from_str(&run(
        root.path(),
        &[
            "context",
            "quasar",
            "--scope",
            "code,docs",
            "--max-tokens",
            "16000",
        ],
    ))
    .unwrap();
    assert!(
        combined["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["path"] == "src/session.ts"
                && i["why"].to_string().contains("behavior unverified"))
    );
    assert!(
        combined["relationships"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
}

#[cfg(unix)]
#[test]
fn symlink_targets_and_copied_indexes_cannot_escape_repository_roots() {
    use std::os::unix::fs::symlink;
    let root = fixture();
    let outside = TempDir::new().unwrap();
    fs::write(
        outside.path().join("evil.ts"),
        "export const external='secret';",
    )
    .unwrap();
    symlink(
        outside.path().join("evil.ts"),
        root.path().join("src/escape.ts"),
    )
    .unwrap();
    symlink(outside.path(), root.path().join("linked")).unwrap();
    fs::write(root.path().join("docs/escape.md"),"# REQ-ESCAPE-001: Isolated\n[file](../src/escape.ts)\n[directory](../linked/evil.ts)\n[absolute](/etc/passwd)\n[encoded](%2fetc/passwd)\n").unwrap();
    index(root.path());
    let read = store(root.path());
    let req = read
        .knowledge_entities("REQ-ESCAPE-001", 1)
        .unwrap()
        .remove(0);
    assert!(
        read.knowledge_neighbors(&req.id, 257)
            .unwrap()
            .iter()
            .filter(|e| e.kind != RelationshipKind::Contains)
            .all(|e| e.target_id.is_none())
    );
    drop(read);
    fs::create_dir(outside.path().join(".code-intel")).unwrap();
    fs::copy(
        root.path().join(".code-intel/index.db"),
        outside.path().join(".code-intel/index.db"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(outside.path())
        .args(["knowledge", "trace", "REQ-AUTH-001"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn schema_three_reads_never_migrate_and_explicit_index_preserves_static_facts() {
    let root = fixture();
    index(root.path());
    let connection = db(root.path());
    connection.execute_batch("DROP TABLE knowledge_relationships; DROP TABLE knowledge_entities; DELETE FROM metadata WHERE key='relationships_indexed'; PRAGMA user_version=3;").unwrap();
    drop(connection);
    let before = fs::read(root.path().join(".code-intel/index.db")).unwrap();
    let read = store(root.path());
    let hashes = read.hashes().unwrap();
    assert!(!read.relationships_available().unwrap());
    assert!(trace(&read, "REQ-AUTH-001", 1, 8000, Format::Json).is_err());
    drop(read);
    assert_eq!(
        before,
        fs::read(root.path().join(".code-intel/index.db")).unwrap()
    );
    index(root.path());
    let read = store(root.path());
    assert_eq!(read.hashes().unwrap(), hashes);
    assert!(read.relationships_available().unwrap());
    assert_eq!(read.knowledge_entities("REQ-AUTH-001", 1).unwrap().len(), 1);
}

#[test]
fn fanout_depth_and_parser_resource_limits_are_explicit() {
    let root = TempDir::new().unwrap();
    let mut document = String::from("# REQ-FAN-001: Fanout\n");
    for i in 0..300 {
        writeln!(document, "[missing](missing-{i}.ts)").unwrap();
    }
    fs::write(root.path().join("fan.md"), document).unwrap();
    index(root.path());
    let response = trace(
        &store(root.path()),
        "REQ-FAN-001",
        1,
        1_000_000,
        Format::Json,
    )
    .unwrap();
    assert!(response.work_limit_reached);
    assert!(response.truncated);
    assert_eq!(response.relationships.len(), 256);
    let zero = trace(&store(root.path()), "REQ-FAN-001", 0, 8000, Format::Json).unwrap();
    assert!(zero.relationships.is_empty());
    assert!(zero.depth_limit_reached);
    let mut too_many = String::from("# REQ-TOOMANY-001: Limited\n");
    for _ in 0..4097 {
        too_many.push_str("[missing](missing.ts)\n");
    }
    fs::write(root.path().join("oversized.md"), too_many).unwrap();
    let report = index(root.path());
    assert_eq!(report.documents.skipped, 1);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("4096 explicit links"))
    );
    assert!(
        store(root.path())
            .knowledge_entities("REQ-TOOMANY-001", 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn crlf_and_cr_relationship_citations_preserve_original_bytes() {
    for ending in ["\r\n", "\r"] {
        let root = TempDir::new().unwrap();
        let source = [
            "# REQ-LINES-001: Unicode 雪",
            "## Acceptance criteria",
            "- Café remains exact.",
            "[missing](missing.ts)",
            "",
        ]
        .join(ending);
        fs::write(root.path().join("policy.md"), &source).unwrap();
        index(root.path());
        let read = store(root.path());
        let req = read
            .knowledge_entities("REQ-LINES-001", 1)
            .unwrap()
            .remove(0);
        let edge = read
            .knowledge_neighbors(&req.id, 257)
            .unwrap()
            .into_iter()
            .find(|e| e.target == "missing.ts")
            .unwrap();
        assert_eq!(edge.evidence.start_line, 4);
        assert_eq!(
            &source[edge.evidence.start_byte..edge.evidence.end_byte],
            "[missing](missing.ts)"
        );
        assert_eq!(req.acceptance_criteria[0].start_line, 2);
    }
}

#[test]
fn duplicate_decisions_and_cross_document_requirements_remain_ambiguous() {
    let root = fixture();
    fs::write(root.path().join("docs/duplicate.md"),"---\nid: ADR-0004\nstatus: accepted\n---\n# Another declaration\n## REQ-AUTH-001: Duplicate requirement\n").unwrap();
    fs::write(
        root.path().join("docs/latest.md"),
        "---\nid: ADR-0022\nstatus: accepted\nsupersedes: ADR-0004\n---\n# Latest policy\n",
    )
    .unwrap();
    index(root.path());
    let read = store(root.path());
    assert_eq!(read.knowledge_entities("ADR-0004", 3).unwrap().len(), 2);
    assert_eq!(read.knowledge_entities("REQ-AUTH-001", 3).unwrap().len(), 2);
    let latest = read.knowledge_entities("ADR-0022", 1).unwrap().remove(0);
    let edges = read.knowledge_neighbors(&latest.id, 257).unwrap();
    assert!(edges.iter().any(|e| e.target == "ADR-0004"
        && e.status == ResolutionStatus::Ambiguous
        && e.target_id.is_none()));
    drop(read);
    fs::remove_file(root.path().join("docs/duplicate.md")).unwrap();
    let old = root.path().join("docs/ADR-0002.md");
    fs::write(
        &old,
        fs::read_to_string(&old)
            .unwrap()
            .replace("status: superseded", "status: accepted"),
    )
    .unwrap();
    index(root.path());
    let read = store(root.path());
    let response = trace(&read, "ADR-0022", 2, 100_000, Format::Json).unwrap();
    assert!(
        response
            .entities
            .iter()
            .any(|e| e.explicit_id.as_deref() == Some("ADR-0002"))
    );
    assert!(
        read.knowledge_entities("ADR-0002", 1).unwrap()[0]
            .diagnostics
            .iter()
            .any(|d| d.contains("conflicts with explicit supersession"))
    );
}

#[test]
fn nested_and_malformed_metadata_never_create_authoritative_declarations() {
    let root = TempDir::new().unwrap();
    for (name, source) in [
        (
            "nested.md",
            "---\nwrapper:\n  id: ADR-0030\n  status: accepted\n---\n# Nested example\n",
        ),
        (
            "list.md",
            "---\nid: ADR-0031\nstatus: accepted\nsupersedes: [ADR-0001\n---\n# Broken list\n",
        ),
    ] {
        fs::write(root.path().join(name), source).unwrap();
    }
    index(root.path());
    let read = store(root.path());
    for id in ["ADR-0030", "ADR-0031"] {
        assert!(read.knowledge_entities(id, 1).unwrap().is_empty());
    }
    for path in ["nested.md", "list.md"] {
        assert!(
            !read
                .knowledge_entities(&format!("document:{path}"), 1)
                .unwrap()[0]
                .diagnostics
                .is_empty()
        );
    }
}
