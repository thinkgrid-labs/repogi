//! Durable project memory (v0.5.1): the separate `.repogi/memory.db` store,
//! lossless migration from schema-5 indexes with crash recovery, index
//! deletion and rebuild, export/import, approval integrity under concurrency,
//! impact-cache isolation, and read-only guarantees.
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");
/// Schema-5 memory tables frozen by v0.5.1 (older binaries wrote them).
const FREEZE_TRIGGERS: [&str; 6] = [
    "memory_moved_memories_insert",
    "memory_moved_memories_update",
    "memory_moved_memory_revisions_insert",
    "memory_moved_memory_revisions_update",
    "memory_moved_memory_evidence_insert",
    "memory_moved_memory_events_insert",
];

fn copy(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if ["README.md", "cases.json"].contains(&entry.file_name().to_str().unwrap()) {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

fn fixture_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/project-memory")
}

fn fixture() -> TempDir {
    let temp = TempDir::new().unwrap();
    copy(&fixture_source(), temp.path());
    index(temp.path());
    temp
}

fn run_env(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(BINARY);
    command
        .current_dir(root)
        .args(args)
        .env_remove("REPOGI_MEMORY_REVIEWER")
        .env_remove("REPOGI_MEMORY_AUTHOR")
        .env_remove("REPOGI_TEST_FAULT");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn run(root: &Path, args: &[&str]) -> Output {
    run_env(root, args, &[])
}

fn index(root: &Path) {
    let output = run(root, &["index", "."]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn ok(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn json(root: &Path, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.extend(["--format", "json"]);
    serde_json::from_str(&ok(root, &args)).unwrap()
}

fn code(output: &Output) -> String {
    assert!(!output.status.success(), "expected a refusal");
    let error: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    error["error"]["code"].as_str().unwrap().to_owned()
}

fn refused(root: &Path, args: &[&str]) -> String {
    code(&run(root, args))
}

fn index_db(root: &Path) -> Connection {
    Connection::open(root.join(".code-intel/index.db")).unwrap()
}

fn mem(root: &Path) -> Connection {
    let path = root.join(".repogi/memory.db");
    assert!(path.is_file(), "no memory store at {}", path.display());
    Connection::open(path).unwrap()
}

fn scalar(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn propose(
    root: &Path,
    kind: &str,
    title: &str,
    claim: &str,
    evidence: &[&str],
) -> (String, String) {
    let mut args = vec![
        "memory", "propose", "--type", kind, "--title", title, "--claim", claim,
    ];
    args.extend(evidence);
    let receipt = json(root, &args);
    (
        receipt["id"].as_str().unwrap().to_owned(),
        receipt["digest"].as_str().unwrap().to_owned(),
    )
}

fn approve(root: &Path, id: &str, digest: &str) -> Value {
    json(
        root,
        &[
            "memory",
            "approve",
            id,
            "--digest",
            &digest[..16],
            "--reviewer",
            "alice",
        ],
    )
}

fn approved(root: &Path, kind: &str, title: &str, claim: &str, evidence: &[&str]) -> String {
    let (id, digest) = propose(root, kind, title, claim, evidence);
    approve(root, &id, &digest);
    id
}

/// A store with every lifecycle state: approved, pending, rejected, corrected,
/// superseded (with successor) and retired, plus a recorded verification.
#[allow(clippy::too_many_lines)] // One fixture with every lifecycle state.
fn lifecycle(root: &Path) -> Vec<String> {
    let decision = approved(
        root,
        "architecture_decision",
        "Sessions persist in SQLite",
        "Session records are stored in an embedded SQLite database file.",
        &["--evidence", "ADR-0001", "--source", "docs/ARCHITECTURE.md"],
    );
    let (pending, _) = propose(
        root,
        "requirement",
        "Login validates credentials",
        "Login validates credentials before a session exists.",
        &["--evidence", "REQ-AUTH-003"],
    );
    let (rejected, digest) = propose(
        root,
        "requirement",
        "Sessions never expire",
        "Sessions never expire.",
        &["--evidence", "REQ-AUTH-001"],
    );
    ok(
        root,
        &[
            "memory",
            "reject",
            &rejected,
            "--digest",
            &digest[..16],
            "--reviewer",
            "bob",
            "--reason",
            "contradicts REQ-AUTH-001",
        ],
    );
    let corrected = approved(
        root,
        "requirement",
        "Audit log",
        "Authentication events are written to the audit log.",
        &["--evidence", "REQ-AUDIT-001"],
    );
    let correction = json(
        root,
        &[
            "memory",
            "correct",
            &corrected,
            "--claim",
            "Every authentication event is written to the audit log.",
            "--reason",
            "scope is every event",
            "--evidence",
            "REQ-AUDIT-001",
        ],
    );
    approve(root, &corrected, correction["digest"].as_str().unwrap());
    let old = approved(
        root,
        "architecture_decision",
        "Absolute session timeout",
        "Sessions end after an absolute timeout.",
        &["--evidence", "ADR-0002"],
    );
    let new = approved(
        root,
        "architecture_decision",
        "Inactivity session timeout",
        "Sessions end after a period of inactivity.",
        &["--evidence", "ADR-0003"],
    );
    ok(
        root,
        &[
            "memory",
            "supersede",
            &old,
            "--by",
            &new,
            "--reason",
            "ADR-0003 replaces ADR-0002",
            "--reviewer",
            "carol",
        ],
    );
    let retired = approved(
        root,
        "project_convention",
        "Session secrets",
        "Session tokens are compared in constant time.",
        &["--evidence", "REQ-AUTH-002"],
    );
    ok(
        root,
        &[
            "memory",
            "retire",
            &retired,
            "--reason",
            "no longer tracked",
            "--reviewer",
            "carol",
        ],
    );
    ok(
        root,
        &[
            "memory",
            "verify",
            &decision,
            "--record",
            "--reviewer",
            "dave",
        ],
    );
    vec![decision, pending, rejected, corrected, old, new, retired]
}

/// Every memory row, as text, from one database (column order fixed).
fn rows(connection: &Connection) -> Vec<String> {
    let mut all = Vec::new();
    for sql in [
        "SELECT id,type,status,latest_revision,approved_revision,pending_revision,superseded_by,created_at,updated_at,version FROM memories ORDER BY id",
        "SELECT memory_id,revision,state,digest,claim_key,assertion_subject,assertion_status,data FROM memory_revisions ORDER BY memory_id,revision",
        "SELECT memory_id,revision,ordinal,role,class,subject_id,explicit_id,target_id,path,data FROM memory_evidence ORDER BY memory_id,revision,ordinal",
        "SELECT sequence,memory_id,revision,action,data FROM memory_events ORDER BY sequence",
        "SELECT memory_id,revision,title,claim,subjects FROM memory_search ORDER BY memory_id,revision",
    ] {
        let mut query = connection.prepare(sql).unwrap();
        let count = query.column_count();
        let mapped = query
            .query_map([], |r| {
                let mut row = Vec::new();
                for i in 0..count {
                    row.push(format!("{:?}", r.get_ref(i)?));
                }
                Ok(row.join("|"))
            })
            .unwrap();
        all.extend(mapped.map(Result::unwrap));
    }
    all
}

/// Move the durable store's rows into the index's schema-5 tables and delete
/// the store: exactly the state a v0.5 binary leaves behind.
fn to_legacy(root: &Path) -> Vec<String> {
    let durable = rows(&mem(root));
    let index = index_db(root);
    for trigger in FREEZE_TRIGGERS {
        index
            .execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))
            .unwrap();
    }
    index
        .execute(
            "ATTACH ?1 AS m",
            [root.join(".repogi/memory.db").to_str().unwrap()],
        )
        .unwrap();
    index
        .execute_batch(
            "INSERT INTO main.memories SELECT * FROM m.memories;
             INSERT INTO main.memory_revisions SELECT * FROM m.memory_revisions;
             INSERT INTO main.memory_evidence SELECT * FROM m.memory_evidence;
             INSERT INTO main.memory_events SELECT * FROM m.memory_events;
             INSERT INTO main.memory_search(memory_id,revision,title,claim,subjects) SELECT memory_id,revision,title,claim,subjects FROM m.memory_search;
             DETACH m;",
        )
        .unwrap();
    drop(index);
    fs::remove_dir_all(root.join(".repogi")).unwrap();
    assert_eq!(rows(&index_db(root)), durable);
    durable
}

fn file_bytes(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    for dir in [".code-intel", ".repogi"] {
        let Ok(entries) = fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_file() {
                files.push((path.display().to_string(), fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[test]
fn migration_from_schema_5_is_lossless_for_every_lifecycle_state() {
    let root = fixture();
    let ids = lifecycle(root.path());
    let before = to_legacy(root.path());
    // Reads before migration come from the index, read-only, and say so.
    let bytes = file_bytes(root.path());
    let listing = json(root.path(), &["memory", "list"]);
    assert_eq!(listing["memories"].as_array().unwrap().len(), ids.len());
    assert_eq!(listing["storage"][0]["code"], "memory_not_migrated");
    assert_eq!(bytes, file_bytes(root.path()), "reads never migrate");
    assert!(!root.path().join(".repogi").exists());

    let report = json(root.path(), &["memory", "migrate"]);
    assert_eq!(report["outcome"], "migrated");
    assert_eq!(report["migration"]["counts"]["memories"], 7);
    assert_eq!(report["migration"]["legacy_frozen"], true);
    // Row for row, column for column, including event sequences and FTS rows.
    assert_eq!(rows(&mem(root.path())), before);
    assert_eq!(
        rows(&index_db(root.path())),
        before,
        "the source rows are kept"
    );
    let status = json(root.path(), &["memory", "status", "--check"]);
    assert_eq!(status["location"], ".repogi/memory.db");
    assert_eq!(status["integrity"], json!([]));
    assert!(status["notes"].as_array().unwrap().is_empty());
    // Histories, supersession and corrections read back unchanged.
    let history = json(root.path(), &["memory", "history", &ids[3]]);
    assert_eq!(history["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(
        json(root.path(), &["memory", "show", &ids[4]])["memory"]["superseded_by"],
        ids[5].as_str()
    );
    assert!(
        json(root.path(), &["memory", "list"])
            .get("storage")
            .is_none()
    );
    // The frozen legacy tables refuse writes (older binaries write them).
    let error = index_db(root.path())
        .execute_batch("UPDATE memories SET status='retired' WHERE id='mem-0001'")
        .unwrap_err()
        .to_string();
    assert!(error.contains("moved to .repogi/memory.db"), "{error}");
}

#[test]
fn repeated_and_concurrent_migrations_are_idempotent() {
    let root = fixture();
    lifecycle(root.path());
    let before = to_legacy(root.path());
    let children: Vec<_> = (0..4)
        .map(|_| {
            Command::new(BINARY)
                .current_dir(root.path())
                .args(["memory", "migrate", "--format", "json"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut outcomes: Vec<String> = children
        .into_iter()
        .map(|child| {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            report["outcome"].as_str().unwrap().to_owned()
        })
        .collect();
    outcomes.sort();
    assert_eq!(
        outcomes.iter().filter(|o| *o == "migrated").count(),
        1,
        "{outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .all(|o| o == "migrated" || o == "already_migrated"),
        "{outcomes:?}"
    );
    assert_eq!(rows(&mem(root.path())), before);
    let events = scalar(&mem(root.path()), "SELECT count(*) FROM store_events");
    assert_eq!(
        json(root.path(), &["memory", "migrate"])["outcome"],
        "already_migrated"
    );
    assert_eq!(rows(&mem(root.path())), before);
    assert_eq!(
        scalar(&mem(root.path()), "SELECT count(*) FROM store_events"),
        events
    );
    // No temporary databases are left behind.
    let leftovers: Vec<_> = fs::read_dir(root.path().join(".repogi"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.contains("migrating"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn interrupted_migrations_leave_the_source_intact_and_can_be_retried() {
    let root = fixture();
    lifecycle(root.path());
    let before = to_legacy(root.path());
    let legacy_bytes = fs::read(root.path().join(".code-intel/index.db")).unwrap();
    // Failure (and a crash) before the destination commits, and after it
    // commits but before it is installed: no store appears, nothing changes.
    for fault in [
        "migrate_before_commit",
        "migrate_before_commit:abort",
        "migrate_before_install",
        "migrate_before_install:abort",
    ] {
        let output = run_env(
            root.path(),
            &["memory", "migrate"],
            &[("REPOGI_TEST_FAULT", fault)],
        );
        assert!(!output.status.success(), "{fault}");
        assert!(!root.path().join(".repogi/memory.db").exists(), "{fault}");
        assert_eq!(
            legacy_bytes,
            fs::read(root.path().join(".code-intel/index.db")).unwrap(),
            "{fault}: the source is untouched"
        );
        // Reads still serve the legacy rows.
        assert_eq!(
            json(root.path(), &["memory", "list"])["memories"]
                .as_array()
                .unwrap()
                .len(),
            7
        );
    }
    // Crash after install, before the source is frozen (completion is not
    // acknowledged): the store is complete; a retry acknowledges it.
    let output = run_env(
        root.path(),
        &["memory", "migrate"],
        &[("REPOGI_TEST_FAULT", "migrate_after_install:abort")],
    );
    assert!(!output.status.success());
    assert_eq!(rows(&mem(root.path())), before);
    let retry = json(root.path(), &["memory", "migrate"]);
    assert_eq!(retry["outcome"], "already_migrated");
    assert_eq!(retry["migration"]["legacy_frozen"], true);
    assert_eq!(rows(&mem(root.path())), before);
    assert_eq!(
        json(root.path(), &["memory", "status", "--check"])["integrity"],
        json!([])
    );
}

#[test]
fn migration_without_hard_links_builds_the_store_in_place() {
    let root = fixture();
    lifecycle(root.path());
    let before = to_legacy(root.path());
    let legacy_bytes = fs::read(root.path().join(".code-intel/index.db")).unwrap();
    // Failure and crash inside the in-place transaction: at most an empty
    // store file remains, the source is untouched, reads serve legacy rows.
    for fault in [
        "migrate_hard_link,migrate_in_place_before_commit",
        "migrate_hard_link,migrate_in_place_before_commit:abort",
    ] {
        let output = run_env(
            root.path(),
            &["memory", "migrate"],
            &[("REPOGI_TEST_FAULT", fault)],
        );
        assert!(!output.status.success(), "{fault}");
        assert_eq!(
            legacy_bytes,
            fs::read(root.path().join(".code-intel/index.db")).unwrap(),
            "{fault}: the source is untouched"
        );
        assert_eq!(
            fs::metadata(root.path().join(".repogi/memory.db"))
                .unwrap()
                .len(),
            0,
            "{fault}: the in-place store was created and rolled back"
        );
        let listing = json(root.path(), &["memory", "list"]);
        assert_eq!(listing["memories"].as_array().unwrap().len(), 7, "{fault}");
        assert_eq!(listing["storage"][0]["code"], "memory_not_migrated");
    }
    let output = run_env(
        root.path(),
        &["memory", "migrate", "--format", "json"],
        &[("REPOGI_TEST_FAULT", "migrate_hard_link")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"], "migrated");
    assert_eq!(report["migration"]["legacy_frozen"], true);
    assert!(
        report["migration"]["notes"][0]
            .as_str()
            .unwrap()
            .contains("built in place"),
        "{report}"
    );
    // Row for row, including event sequences, exactly as the linked install.
    assert_eq!(rows(&mem(root.path())), before);
    assert_eq!(
        json(root.path(), &["memory", "status", "--check"])["integrity"],
        json!([])
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| {
            fs::metadata(root.path().join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(".repogi"), 0o700);
        assert_eq!(mode(".repogi/memory.db"), 0o600);
    }
    let leftovers: Vec<_> = fs::read_dir(root.path().join(".repogi"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.contains("migrating") || n.ends_with("-journal"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let retry = run_env(
        root.path(),
        &["memory", "migrate", "--format", "json"],
        &[("REPOGI_TEST_FAULT", "migrate_hard_link")],
    );
    let retry: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retry["outcome"], "already_migrated");
    assert_eq!(rows(&mem(root.path())), before);
}

#[test]
fn exports_without_hard_links_still_never_overwrite() {
    let root = fixture();
    lifecycle(root.path());
    let out = TempDir::new().unwrap();
    let linked = out.path().join("linked.json");
    let unlinked = out.path().join("unlinked.json");
    ok(
        root.path(),
        &["memory", "export", "--output", linked.to_str().unwrap()],
    );
    let export = |path: &Path| {
        run_env(
            root.path(),
            &["memory", "export", "--output", path.to_str().unwrap()],
            &[("REPOGI_TEST_FAULT", "export_hard_link")],
        )
    };
    let output = export(&unlinked);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let read = |path: &Path| -> Value { serde_json::from_slice(&fs::read(path).unwrap()).unwrap() };
    assert_eq!(read(&unlinked)["memories"], read(&linked)["memories"]);
    assert_eq!(
        read(&unlinked)["integrity"]["digest"],
        read(&linked)["integrity"]["digest"]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&unlinked).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let names: Vec<_> = fs::read_dir(out.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "no temporary file is left: {names:?}");
    let report = json(
        root.path(),
        &["memory", "import", unlinked.to_str().unwrap(), "--dry-run"],
    );
    assert_eq!(report["outcomes"]["unchanged"], 7, "{report}");
    let kept = fs::read(&unlinked).unwrap();
    assert_eq!(code(&export(&unlinked)), "output_exists");
    assert_eq!(fs::read(&unlinked).unwrap(), kept);
}

#[test]
fn memory_writes_and_indexing_migrate_first_and_never_lose_legacy_rows() {
    let root = fixture();
    lifecycle(root.path());
    let before = to_legacy(root.path());
    // `repogi index` migrates before indexing, so deleting .code-intel later is safe.
    index(root.path());
    assert_eq!(rows(&mem(root.path())), before);
    fs::remove_dir_all(root.path().join(".code-intel")).unwrap();
    index(root.path());
    assert_eq!(rows(&mem(root.path())), before);
    // A legacy index restored later with extra rows merges by fast-forward.
    let other = fixture();
    lifecycle(other.path());
    to_legacy(other.path());
    fs::remove_dir_all(root.path().join(".repogi")).unwrap();
    fs::copy(
        other.path().join(".code-intel/index.db"),
        root.path().join(".code-intel/index.db"),
    )
    .unwrap();
    // (a copied index belongs to another root and is refused, not migrated)
    assert_eq!(
        refused(root.path(), &["memory", "migrate"]),
        "command_failed"
    );
}

#[test]
fn approved_memory_survives_index_deletion_and_rebuild() {
    let root = fixture();
    let ids = lifecycle(root.path());
    let before = rows(&mem(root.path()));
    let history = json(root.path(), &["memory", "history", &ids[3]]);
    fs::remove_dir_all(root.path().join(".code-intel")).unwrap();
    // Without an index, memories and history are readable; evidence is
    // explicitly unverified rather than current.
    let shown = json(root.path(), &["memory", "show", &ids[0]]);
    assert_eq!(shown["memory"]["status"], "approved");
    assert_eq!(shown["approved"]["verification"], "unverified");
    assert!(shown.get("index_revision").is_none());
    assert!(
        shown["storage"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["code"] == "index_unavailable")
    );
    assert!(
        shown["approved"]["evidence"][0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "index_unavailable")
    );
    let found = json(root.path(), &["memory", "search", "SQLite"]);
    assert_eq!(found["results"][0]["id"], ids[0].as_str());
    assert_eq!(found["results"][0]["verification"], "unverified");
    assert!(
        ok(root.path(), &["memory", "show", &ids[0]]).contains("no index"),
        "text output names the missing index"
    );
    // Writes that capture or verify evidence need the index; nothing changes.
    assert_eq!(
        refused(
            root.path(),
            &[
                "memory",
                "propose",
                "--type",
                "requirement",
                "--title",
                "t",
                "--claim",
                "x y z",
                "--evidence",
                "REQ-AUTH-001"
            ]
        ),
        "index_required"
    );
    assert_eq!(rows(&mem(root.path())), before);
    index(root.path());
    assert_eq!(
        rows(&mem(root.path())),
        before,
        "rebuilding never rewrites memories"
    );
    assert_eq!(
        json(root.path(), &["memory", "history", &ids[3]])["revisions"],
        history["revisions"]
    );
    assert_eq!(
        json(root.path(), &["memory", "history", &ids[3]])["events"],
        history["events"]
    );
    let verified = json(root.path(), &["memory", "verify", "--all"]);
    for result in verified["results"].as_array().unwrap() {
        assert_eq!(result["verification"], "current", "{result}");
    }
}

#[test]
fn evidence_is_re_resolved_after_a_rebuild_and_staleness_stays_accurate() {
    let root = fixture();
    // Advance the index revision before capture, so a rebuild goes backwards.
    let notes = root.path().join("docs/notes/session-notes.md");
    fs::write(
        &notes,
        format!("{}\nMore notes.\n", fs::read_to_string(&notes).unwrap()),
    )
    .unwrap();
    index(root.path());
    let id = approved(
        root.path(),
        "architecture_decision",
        "Sessions persist in SQLite",
        "Session records are stored in an embedded SQLite database file.",
        &["--evidence", "ADR-0001"],
    );
    let captured = json(root.path(), &["memory", "show", &id])["approved"]["index_revision"]
        .as_u64()
        .unwrap();
    assert!(captured > 1);
    fs::remove_dir_all(root.path().join(".code-intel")).unwrap();
    index(root.path());
    let check = json(root.path(), &["memory", "verify", &id]);
    let evidence = &check["results"][0]["evidence"][0];
    assert_eq!(
        evidence["state"], "current",
        "an unchanged source stays current"
    );
    assert!(
        evidence["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "index_rebuilt"),
        "{evidence}"
    );
    // A rebuild never establishes correctness: changed bytes are stale.
    let adr = root.path().join("docs/adr/ADR-0001-sqlite.md");
    fs::write(
        &adr,
        fs::read_to_string(&adr)
            .unwrap()
            .replace("SQLite", "Postgres"),
    )
    .unwrap();
    fs::remove_dir_all(root.path().join(".code-intel")).unwrap();
    index(root.path());
    let check = json(root.path(), &["memory", "verify", &id]);
    assert_ne!(check["results"][0]["verification"], "current");
    assert_eq!(
        json(root.path(), &["memory", "show", &id])["memory"]["status"],
        "approved"
    );
}

#[test]
fn approval_is_checked_inside_the_write_transaction() {
    let root = fixture();
    // No index: approval cannot verify, so it is refused and nothing changes.
    let (id, digest) = propose(
        root.path(),
        "requirement",
        "Login validates credentials",
        "Login validates credentials before a session exists.",
        &["--evidence", "REQ-AUTH-003"],
    );
    let index_dir = root.path().join(".code-intel");
    let aside = root.path().join("index-aside");
    fs::rename(&index_dir, &aside).unwrap();
    assert_eq!(
        refused(
            root.path(),
            &[
                "memory",
                "approve",
                &id,
                "--digest",
                &digest[..16],
                "--reviewer",
                "a"
            ]
        ),
        "index_required"
    );
    fs::rename(&aside, &index_dir).unwrap();
    // A damaged stored digest is detected under the lock.
    let store = mem(root.path());
    store
        .execute_batch("DROP TRIGGER memory_revisions_review_once;")
        .unwrap();
    let data: String = store
        .query_row(
            "SELECT data FROM memory_revisions WHERE memory_id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    store
        .execute(
            "UPDATE memory_revisions SET data=?2 WHERE memory_id=?1",
            rusqlite::params![id, data.replace("before a session", "after a session")],
        )
        .unwrap();
    assert_eq!(
        refused(
            root.path(),
            &[
                "memory",
                "approve",
                &id,
                "--digest",
                &digest[..16],
                "--reviewer",
                "a"
            ]
        ),
        "digest_invalid"
    );
    store
        .execute(
            "UPDATE memory_revisions SET data=?2 WHERE memory_id=?1",
            rusqlite::params![id, data],
        )
        .unwrap();
    drop(store);
    // A failure between the checks and the write rolls everything back.
    let output = run_env(
        root.path(),
        &[
            "memory",
            "approve",
            &id,
            "--digest",
            &digest[..16],
            "--reviewer",
            "a",
        ],
        &[("REPOGI_TEST_FAULT", "review_before_write")],
    );
    assert!(!output.status.success());
    let store = mem(root.path());
    assert_eq!(scalar(&store, "SELECT count(*) FROM memory_events"), 1);
    assert_eq!(
        scalar(
            &store,
            "SELECT count(*) FROM memories WHERE status='pending_review' AND version=1"
        ),
        1
    );
    drop(store);
    // After a rebuild the same evidence re-verifies and approval succeeds,
    // recording the verification in the same event.
    fs::remove_dir_all(&index_dir).unwrap();
    index(root.path());
    let receipt = approve(root.path(), &id, &digest);
    assert_eq!(receipt["verification"], "current");
    let history = json(root.path(), &["memory", "history", &id]);
    let approval = &history["events"][1];
    assert_eq!(approval["action"], "approved");
    assert_eq!(approval["detail"]["verification"], "current");
}

#[test]
#[allow(clippy::too_many_lines)] // One race per competing transition.
fn competing_review_transitions_commit_exactly_once() {
    let root = fixture();
    for contender in ["approve", "reject", "correct"] {
        let id = approved(
            root.path(),
            "requirement",
            &format!("Audit {contender}"),
            &format!("Authentication events reach the audit log ({contender})."),
            &["--evidence", "REQ-AUDIT-001"],
        );
        let correction = json(
            root.path(),
            &[
                "memory",
                "correct",
                &id,
                "--claim",
                &format!("Every authentication event reaches the audit log ({contender})."),
                "--reason",
                "scope",
                "--evidence",
                "REQ-AUDIT-001",
            ],
        );
        let digest = correction["digest"].as_str().unwrap()[..16].to_owned();
        let other: Vec<String> = match contender {
            "approve" => vec![
                "memory".into(),
                "approve".into(),
                id.clone(),
                "--digest".into(),
                digest.clone(),
                "--reviewer".into(),
                "b".into(),
            ],
            "reject" => vec![
                "memory".into(),
                "reject".into(),
                id.clone(),
                "--digest".into(),
                digest.clone(),
                "--reviewer".into(),
                "b".into(),
                "--reason".into(),
                "no".into(),
            ],
            _ => vec![
                "memory".into(),
                "correct".into(),
                id.clone(),
                "--claim".into(),
                "A different claim about audit logging.".into(),
                "--reason".into(),
                "race".into(),
                "--evidence".into(),
                "REQ-AUDIT-001".into(),
            ],
        };
        let first = vec![
            "memory".to_owned(),
            "approve".into(),
            id.clone(),
            "--digest".into(),
            digest.clone(),
            "--reviewer".into(),
            "a".into(),
        ];
        let spawn = |args: Vec<String>| {
            let root = root.path().to_path_buf();
            std::thread::spawn(move || {
                Command::new(BINARY)
                    .current_dir(root)
                    .args(&args)
                    .env_remove("REPOGI_TEST_FAULT")
                    .output()
                    .unwrap()
            })
        };
        let (a, b) = (spawn(first), spawn(other));
        let (a, b) = (a.join().unwrap(), b.join().unwrap());
        let reviews = scalar(
            &mem(root.path()),
            &format!(
                "SELECT count(*) FROM memory_events WHERE memory_id='{id}' AND revision=2 AND action IN ('approved','rejected')"
            ),
        );
        match contender {
            // Two reviews of one revision: exactly one commits.
            "approve" | "reject" => {
                assert_eq!(
                    u8::from(a.status.success()) + u8::from(b.status.success()),
                    1,
                    "{contender}"
                );
                assert_eq!(reviews, 1, "{contender}");
                let loser = if a.status.success() { &b } else { &a };
                assert!(
                    matches!(
                        code(loser).as_str(),
                        "nothing_to_review" | "digest_mismatch"
                    ),
                    "{contender}"
                );
            }
            // A correction while revision 2 is pending is refused
            // (review_pending); if the approval wins, the correction proceeds.
            _ => {
                assert!(a.status.success() || b.status.success());
                assert!(reviews <= 1);
                if !b.status.success() {
                    assert_eq!(code(&b), "review_pending");
                }
            }
        }
        // Every refusal changed nothing: versions and history are consistent.
        assert_eq!(
            json(root.path(), &["memory", "status", "--check"])["integrity"],
            json!([])
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One round trip, asserted step by step.
fn export_and_restore_round_trip_preserves_every_record() {
    let root = fixture();
    let ids = lifecycle(root.path());
    let backup = root.path().join("../backup.json");
    let backup = backup.as_path();
    let _ = fs::remove_file(backup);
    let bytes = file_bytes(root.path());
    let summary = json(
        root.path(),
        &["memory", "export", "--output", backup.to_str().unwrap()],
    );
    assert_eq!(bytes, file_bytes(root.path()), "export is read-only");
    assert_eq!(summary["counts"]["memories"], 7);
    let export: Value = serde_json::from_slice(&fs::read(backup).unwrap()).unwrap();
    assert_eq!(export["format"], "repogi-memory-export");
    assert_eq!(export["format_version"], 1);
    assert!(
        !fs::read_to_string(backup)
            .unwrap()
            .contains(root.path().to_str().unwrap()),
        "no absolute paths"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Restore into a fresh copy of the repository.
    let clone = TempDir::new().unwrap();
    copy(&fixture_source(), clone.path());
    index(clone.path());
    let report = json(
        clone.path(),
        &["memory", "import", backup.to_str().unwrap(), "--restore"],
    );
    assert_eq!(report["mode"], "restore");
    assert_eq!(report["outcomes"]["created"], 7);
    for line in report["memories"].as_array().unwrap() {
        assert!(line["verification"].is_string(), "{line}");
    }
    let original: Vec<String> = rows(&mem(root.path()))
        .into_iter()
        .filter(|r| !r.starts_with("Integer"))
        .collect();
    let restored: Vec<String> = rows(&mem(clone.path()))
        .into_iter()
        .filter(|r| !r.starts_with("Integer"))
        .collect();
    assert_eq!(
        original, restored,
        "every row except store-local event sequences"
    );
    for id in &ids {
        assert_eq!(
            json(root.path(), &["memory", "history", id])["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| (
                    e["action"].clone(),
                    e["actor"].clone(),
                    e["at"].clone(),
                    e["detail"].clone()
                ))
                .collect::<Vec<_>>(),
            json(clone.path(), &["memory", "history", id])["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| (
                    e["action"].clone(),
                    e["actor"].clone(),
                    e["at"].clone(),
                    e["detail"].clone()
                ))
                .collect::<Vec<_>>()
        );
    }
    // Re-export from the restored store: identical payload and digest.
    let again = clone.path().join("../again.json");
    let _ = fs::remove_file(&again);
    ok(
        clone.path(),
        &["memory", "export", "--output", again.to_str().unwrap()],
    );
    let second: Value = serde_json::from_slice(&fs::read(&again).unwrap()).unwrap();
    assert_eq!(second["memories"], export["memories"]);
    assert_eq!(second["integrity"]["digest"], export["integrity"]["digest"]);
    // Repeated import changes nothing.
    let store = rows(&mem(clone.path()));
    let repeat = json(
        clone.path(),
        &["memory", "import", backup.to_str().unwrap()],
    );
    assert_eq!(repeat["outcomes"]["unchanged"], 7);
    assert_eq!(rows(&mem(clone.path())), store);
    // Output safety: never clobbered or written through a symlink.
    assert_eq!(
        refused(
            root.path(),
            &["memory", "export", "--output", backup.to_str().unwrap()]
        ),
        "output_exists"
    );
    #[cfg(unix)]
    {
        let target = root.path().join("../target.txt");
        fs::write(&target, "keep").unwrap();
        let link = root.path().join("../link.json");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            refused(
                root.path(),
                &[
                    "memory",
                    "export",
                    "--output",
                    link.to_str().unwrap(),
                    "--force"
                ]
            ),
            "output_rejected"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
    }
    let _ = fs::remove_file(backup);
    let _ = fs::remove_file(&again);
}

#[test]
fn imports_fast_forward_refuse_divergence_and_never_overwrite() {
    let root = fixture();
    let id = approved(
        root.path(),
        "architecture_decision",
        "Sessions persist in SQLite",
        "Session records are stored in an embedded SQLite database file.",
        &["--evidence", "ADR-0001"],
    );
    let dir = TempDir::new().unwrap();
    let older = dir.path().join("older.json");
    ok(
        root.path(),
        &["memory", "export", "--output", older.to_str().unwrap()],
    );
    // The source advances: a correction is proposed and approved.
    let correction = json(
        root.path(),
        &[
            "memory",
            "correct",
            &id,
            "--claim",
            "Session records live in one embedded SQLite file.",
            "--reason",
            "precision",
            "--evidence",
            "ADR-0001",
        ],
    );
    approve(root.path(), &id, correction["digest"].as_str().unwrap());
    let newer = dir.path().join("newer.json");
    ok(
        root.path(),
        &["memory", "export", "--output", newer.to_str().unwrap()],
    );
    // A clone restored from the older backup fast-forwards to the newer one.
    let clone = TempDir::new().unwrap();
    copy(&fixture_source(), clone.path());
    index(clone.path());
    ok(
        clone.path(),
        &["memory", "import", older.to_str().unwrap(), "--restore"],
    );
    let report = json(clone.path(), &["memory", "import", newer.to_str().unwrap()]);
    assert_eq!(report["outcomes"]["fast_forwarded"], 1);
    assert_eq!(
        json(clone.path(), &["memory", "show", &id])["approved"]["revision"],
        2
    );
    // An older backup over a newer store changes nothing.
    let store = rows(&mem(clone.path()));
    let report = json(clone.path(), &["memory", "import", older.to_str().unwrap()]);
    assert_eq!(report["outcomes"]["store_newer"], 1);
    assert_eq!(rows(&mem(clone.path())), store);
    // Diverged histories are a conflict: nothing is written.
    let fork = TempDir::new().unwrap();
    copy(&fixture_source(), fork.path());
    index(fork.path());
    ok(
        fork.path(),
        &["memory", "import", older.to_str().unwrap(), "--restore"],
    );
    ok(
        fork.path(),
        &[
            "memory",
            "retire",
            &id,
            "--reason",
            "fork",
            "--reviewer",
            "x",
        ],
    );
    let bytes = file_bytes(fork.path());
    assert_eq!(
        refused(fork.path(), &["memory", "import", newer.to_str().unwrap()]),
        "import_conflict"
    );
    assert_eq!(bytes, file_bytes(fork.path()));
    // Dry runs report the same outcomes (and conflicts) and never write.
    assert_eq!(
        refused(
            fork.path(),
            &["memory", "import", newer.to_str().unwrap(), "--dry-run"]
        ),
        "import_conflict"
    );
    let report = json(
        fork.path(),
        &["memory", "import", older.to_str().unwrap(), "--dry-run"],
    );
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["outcomes"]["store_newer"], 1);
    assert_eq!(bytes, file_bytes(fork.path()));
}

#[test]
fn backups_from_another_lineage_need_local_review() {
    let source = fixture();
    let id = approved(
        source.path(),
        "architecture_decision",
        "Sessions persist in SQLite",
        "Session records are stored in an embedded SQLite database file.",
        &["--evidence", "ADR-0001"],
    );
    let (rejected, digest) = propose(
        source.path(),
        "requirement",
        "Never expire",
        "Sessions never expire.",
        &["--evidence", "REQ-AUTH-001"],
    );
    ok(
        source.path(),
        &[
            "memory",
            "reject",
            &rejected,
            "--digest",
            &digest[..16],
            "--reviewer",
            "b",
            "--reason",
            "no",
        ],
    );
    let dir = TempDir::new().unwrap();
    let backup = dir.path().join("backup.json");
    ok(
        source.path(),
        &["memory", "export", "--output", backup.to_str().unwrap()],
    );
    // A different repository that already has its own memory.
    let other = fixture();
    let own = approved(
        other.path(),
        "requirement",
        "Audit",
        "Authentication events are audited.",
        &["--evidence", "REQ-AUDIT-001"],
    );
    assert_eq!(
        refused(
            other.path(),
            &["memory", "import", backup.to_str().unwrap(), "--restore"]
        ),
        "repository_mismatch"
    );
    let report = json(
        other.path(),
        &["memory", "import", backup.to_str().unwrap()],
    );
    assert_eq!(report["mode"], "review");
    assert_eq!(report["outcomes"]["pending_local_review"], 1);
    assert_eq!(report["outcomes"]["archived_only"], 1);
    let local = report["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source_memory"] == id.as_str())
        .unwrap()["local_memory"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(local, own, "IDs never collide with local memories");
    let shown = json(other.path(), &["memory", "show", &local]);
    assert_eq!(shown["memory"]["status"], "pending_review");
    assert_eq!(shown["memory"]["authoritative"], false);
    // Not authoritative: default search never returns it.
    assert!(
        json(other.path(), &["memory", "search", "SQLite"])["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let history = json(other.path(), &["memory", "history", &local]);
    assert_eq!(history["imported_from"]["source_memory"], id.as_str());
    assert_eq!(history["imported_from"]["source_status"], "approved");
    assert!(
        history["imported_from"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "approved" && e["actor"] == "alice")
    );
    // Idempotent; then a local reviewer approves the same digest deliberately.
    assert_eq!(
        json(
            other.path(),
            &["memory", "import", backup.to_str().unwrap()]
        )["outcomes"]["already_imported"],
        2
    );
    let digest = shown["pending"]["digest"].as_str().unwrap();
    assert_eq!(approve(other.path(), &local, digest)["status"], "approved");
}

#[test]
#[allow(clippy::too_many_lines)] // One case table of hostile inputs.
fn malformed_tampered_or_oversized_imports_are_refused_without_writes() {
    let root = fixture();
    approved(
        root.path(),
        "architecture_decision",
        "Sessions persist in SQLite",
        "Session records are stored in an embedded SQLite database file.",
        &["--evidence", "ADR-0001"],
    );
    let dir = TempDir::new().unwrap();
    let backup = dir.path().join("backup.json");
    ok(
        root.path(),
        &["memory", "export", "--output", backup.to_str().unwrap()],
    );
    let good: Value = serde_json::from_slice(&fs::read(&backup).unwrap()).unwrap();
    let target = fixture();
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    };
    let mut edited = good.clone();
    edited["memories"][0]["revisions"][0]["claim"] = json!("Sessions are stored in Redis.");
    let tampered = write(
        "tampered.json",
        serde_json::to_string(&edited).unwrap().as_bytes(),
    );
    let mut unknown = good.clone();
    unknown["memories"][0]["run"] = json!("rm -rf /");
    let mut traversal = good.clone();
    traversal["memories"][0]["revisions"][0]["evidence"][0]["citation"]["path"] =
        json!("../../etc/passwd");
    let mut control = good.clone();
    control["memories"][0]["revisions"][0]["title"] = json!("a\u{1b}[2Jb");
    let mut version = good.clone();
    version["format_version"] = json!(99);
    let cases = [
        (tampered, "integrity_mismatch"),
        (
            write(
                "unknown.json",
                serde_json::to_string(&unknown).unwrap().as_bytes(),
            ),
            "import_invalid",
        ),
        (
            write(
                "traversal.json",
                serde_json::to_string(&traversal).unwrap().as_bytes(),
            ),
            "integrity_mismatch",
        ),
        (
            write(
                "control.json",
                serde_json::to_string(&control).unwrap().as_bytes(),
            ),
            "integrity_mismatch",
        ),
        (
            write(
                "version.json",
                serde_json::to_string(&version).unwrap().as_bytes(),
            ),
            "import_unsupported",
        ),
        (
            write("utf8.json", b"{\"format\":\"\xff\"}"),
            "import_invalid",
        ),
        (
            write("deep.json", "[".repeat(10_000).as_bytes()),
            "import_invalid",
        ),
        (write("empty.json", b""), "import_invalid"),
    ];
    for (path, expected) in cases {
        assert_eq!(
            refused(target.path(), &["memory", "import", path.to_str().unwrap()]),
            expected,
            "{}",
            path.display()
        );
        assert!(
            !target.path().join(".repogi").exists(),
            "{}",
            path.display()
        );
    }
    // Structural validation runs even when the digest is recomputed to match:
    // the claim digest, evidence paths and control characters are checked.
    for (mut value, expected) in [
        (edited, "digest_mismatch"),
        (traversal_fixed(&good), "import_invalid"),
        (control_fixed(&good), "import_invalid"),
    ] {
        reseal(&mut value);
        let path = write(
            "resealed.json",
            serde_json::to_string(&value).unwrap().as_bytes(),
        );
        assert_eq!(
            refused(target.path(), &["memory", "import", path.to_str().unwrap()]),
            expected
        );
        assert!(!target.path().join(".repogi").exists());
    }
    // Oversized inputs are refused before they are read.
    let huge = dir.path().join("huge.json");
    fs::File::create(&huge)
        .unwrap()
        .set_len(257 * 1024 * 1024)
        .unwrap();
    assert_eq!(
        refused(target.path(), &["memory", "import", huge.to_str().unwrap()]),
        "import_invalid"
    );
}

fn traversal_fixed(good: &Value) -> Value {
    let mut value = good.clone();
    value["memories"][0]["revisions"][0]["evidence"][0]["citation"]["path"] =
        json!("../../etc/passwd");
    value
}

fn control_fixed(good: &Value) -> Value {
    let mut value = good.clone();
    value["memories"][0]["revisions"][0]["title"] = json!("a\u{1b}[2Jb");
    value
}

/// Recompute the payload digest with the documented algorithm, so only the
/// structural checks can refuse the import.
fn reseal(value: &mut Value) {
    let export: code_intel_retrieval::memory::Export =
        serde_json::from_value(value.clone()).unwrap();
    value["integrity"]["digest"] = json!(code_intel_retrieval::memory::payload_digest(
        export.format_version,
        &export.source,
        &export.memories
    ));
}

#[test]
fn reads_are_read_only_and_memory_is_scoped_to_its_root() {
    let root = fixture();
    let ids = lifecycle(root.path());
    let bytes = file_bytes(root.path());
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("x.json");
    for args in [
        vec!["memory", "list"],
        vec!["memory", "show", &ids[0]],
        vec!["memory", "history", &ids[3]],
        vec!["memory", "search", "session"],
        vec!["memory", "verify", "--all"],
        vec!["memory", "conflicts"],
        vec!["memory", "status", "--check"],
        vec!["memory", "export", "--output", out.to_str().unwrap()],
        vec![
            "investigate",
            "Why does this project use SQLite?",
            "--include-memory",
        ],
    ] {
        ok(root.path(), &args);
    }
    let dry = json(
        root.path(),
        &["memory", "import", out.to_str().unwrap(), "--dry-run"],
    );
    assert_eq!(dry["outcomes"]["unchanged"], 7);
    assert_eq!(bytes, file_bytes(root.path()), "no read changed any byte");
    // A memory store copied into another repository is refused.
    let other = fixture();
    copy(&root.path().join(".repogi"), &other.path().join(".repogi"));
    assert_eq!(
        refused(other.path(), &["memory", "list"]),
        "memory_root_mismatch"
    );
    assert_eq!(
        refused(
            other.path(),
            &[
                "memory",
                "retire",
                &ids[0],
                "--reason",
                "x",
                "--reviewer",
                "a"
            ]
        ),
        "memory_root_mismatch"
    );
    // ...and cannot be rebound while the original still holds it.
    let recorded = root.path().to_str().unwrap();
    assert_eq!(
        refused(other.path(), &["memory", "rebind", "--from", recorded]),
        "memory_store_copied"
    );
    // A moved repository is rebound explicitly.
    let parent = TempDir::new().unwrap();
    let moved = parent.path().join("moved");
    fs::rename(root.path(), &moved).unwrap();
    assert_eq!(refused(&moved, &["memory", "list"]), "memory_root_mismatch");
    assert_eq!(
        refused(&moved, &["memory", "rebind", "--from", "/elsewhere"]),
        "memory_root_mismatch"
    );
    ok(&moved, &["memory", "rebind", "--from", recorded]);
    fs::remove_dir_all(moved.join(".code-intel")).unwrap();
    index(&moved);
    assert_eq!(
        json(&moved, &["memory", "list"])["memories"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    let status = json(&moved, &["memory", "status"]);
    assert!(
        status["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "rebound")
    );
    fs::rename(&moved, root.path()).unwrap();
}

#[test]
fn damaged_or_foreign_memory_stores_are_detected_not_overwritten() {
    let root = fixture();
    approved(
        root.path(),
        "architecture_decision",
        "Sessions persist in SQLite",
        "Session records are stored in an embedded SQLite database file.",
        &["--evidence", "ADR-0001"],
    );
    let path = root.path().join(".repogi/memory.db");
    let original = fs::read(&path).unwrap();
    // Garbage, a truncated file, and another SQLite database are all refused
    // by reads and writes, and never replaced.
    let foreign = TempDir::new().unwrap();
    let other = Connection::open(foreign.path().join("x.db")).unwrap();
    other
        .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);")
        .unwrap();
    drop(other);
    for (name, bytes) in [
        ("garbage", vec![0x42; 8192]),
        ("truncated", original[..original.len() / 3].to_vec()),
        ("foreign", fs::read(foreign.path().join("x.db")).unwrap()),
        (
            "index",
            fs::read(root.path().join(".code-intel/index.db")).unwrap(),
        ),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(
            !run(root.path(), &["memory", "list"]).status.success(),
            "{name}"
        );
        assert!(
            !run(
                root.path(),
                &[
                    "memory",
                    "propose",
                    "--type",
                    "requirement",
                    "--title",
                    "t",
                    "--claim",
                    "x y",
                    "--evidence",
                    "REQ-AUTH-001"
                ]
            )
            .status
            .success(),
            "{name}"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes, "{name}: never overwritten");
    }
    fs::write(&path, &original).unwrap();
    assert_eq!(
        json(root.path(), &["memory", "status", "--check"])["integrity"],
        json!([])
    );
    // Symlinked memory state is refused.
    #[cfg(unix)]
    {
        let elsewhere = TempDir::new().unwrap();
        fs::rename(root.path().join(".repogi"), elsewhere.path().join("store")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path().join("store"), root.path().join(".repogi"))
            .unwrap();
        let output = run(root.path(), &["memory", "list"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("symlinked memory state"));
    }
}

#[test]
fn impact_snapshots_never_contain_memory_records() {
    let root = fixture();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(root.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q"]);
    fs::write(root.path().join(".gitignore"), ".code-intel/\n.repogi/\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    let claim = "Session records are stored in an embedded SQLite database file zqxjmarker.";
    approved(
        root.path(),
        "architecture_decision",
        "Sessions persist in SQLite zqxjmarker",
        claim,
        &["--evidence", "ADR-0001"],
    );
    // Legacy rows in the index are the only memory an index copy could carry.
    to_legacy(root.path());
    assert!(contains(
        &fs::read(root.path().join(".code-intel/index.db")).unwrap(),
        "zqxjmarker"
    ));
    let session = root.path().join("src/auth/session.ts");
    if session.exists() {
        fs::write(
            &session,
            format!("{}\n// edit\n", fs::read_to_string(&session).unwrap()),
        )
        .unwrap();
    }
    for args in [
        vec!["impact", ".", "--base", "HEAD"],
        vec!["impact", ".", "--base", "HEAD", "--head", "HEAD"],
    ] {
        let output = run(root.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut checked = 0;
    let mut stack = vec![root.path().join(".code-intel/impact")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "src") {
                    stack.push(path);
                }
            } else {
                checked += 1;
                assert!(
                    !contains(&fs::read(&path).unwrap(), "zqxjmarker"),
                    "{} holds memory bytes",
                    path.display()
                );
            }
        }
    }
    assert!(checked > 0, "impact wrote snapshots");
}

#[test]
fn mcp_memory_stays_read_only_and_works_without_an_index() {
    let root = fixture();
    let id = approved(
        root.path(),
        "architecture_decision",
        "Sessions persist in SQLite",
        "Session records are stored in an embedded SQLite database file.",
        &["--evidence", "ADR-0001"],
    );
    fs::remove_dir_all(root.path().join(".code-intel")).unwrap();
    let bytes = file_bytes(root.path());
    let canonical = root.path().canonicalize().unwrap();
    let calls = [
        json!({"name":"atlas_memory","arguments":{"operation":"get","id":id,"format":"json"}}),
        json!({"name":"atlas_memory","arguments":{"operation":"search","query":"SQLite"}}),
        json!({"name":"atlas_memory","arguments":{"operation":"export"}}),
        json!({"name":"atlas_memory","arguments":{"operation":"import","path":"/tmp/x.json"}}),
        json!({"name":"atlas_memory","arguments":{"operation":"migrate"}}),
    ];
    let mut input = String::new();
    input.push_str(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}).to_string());
    input.push('\n');
    input.push_str(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string());
    input.push('\n');
    for (i, call) in calls.iter().enumerate() {
        input.push_str(
            &json!({"jsonrpc":"2.0","id":i + 1,"method":"tools/call","params":call}).to_string(),
        );
        input.push('\n');
    }
    let mut child = Command::new(BINARY)
        .args([
            "mcp",
            "--root",
            canonical.to_str().unwrap(),
            "--tools",
            "atlas_memory",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let mut responses = std::collections::BTreeMap::new();
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        if let Some(id) = value["id"].as_u64().filter(|id| *id > 0) {
            let result = &value["result"];
            responses.insert(
                id,
                (
                    result["content"][0]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    result["isError"].as_bool().unwrap_or(false),
                ),
            );
        }
    }
    let (text, error) = &responses[&1];
    assert!(!error, "{text}");
    let got: Value = serde_json::from_str(text).unwrap();
    assert_eq!(got["approved"]["verification"], "unverified");
    assert!(!responses[&2].1);
    for id in 3..=5 {
        assert!(
            responses[&id].1 && responses[&id].0.contains("invalid_arguments"),
            "{:?}",
            responses[&id]
        );
    }
    assert_eq!(bytes, file_bytes(root.path()), "MCP never writes");
}
