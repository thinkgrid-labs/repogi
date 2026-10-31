//! Agent investigation contracts: evidence-backed, bounded, deterministic and read-only.
use code_intel_retrieval::{
    Format,
    investigate::{self, EvidenceClass, Ranking, Request, Response},
};
use code_intel_store::Store;
use rusqlite::Connection;
use serde_json::Value;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn copy(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        // Fixture documentation and labels are not part of the indexed corpus.
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

fn fixture() -> TempDir {
    let temp = TempDir::new().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/agent-investigation"),
        temp.path(),
    );
    code_intel_index::index(temp.path()).unwrap();
    temp
}

fn store(root: &Path) -> Store {
    Store::open_read(&root.canonicalize().unwrap().join(".code-intel/index.db")).unwrap()
}

fn plan(root: &Path, request: &Request) -> Response {
    investigate::investigate(&store(root), request, Format::Json).unwrap()
}

fn request(task: &str) -> Request {
    let mut request = Request::new(task);
    request.max_bytes = 1_000_000;
    request
}

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_repogi"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn names(response: &Response, action: &str) -> Vec<String> {
    let mut names = Vec::new();
    for step in &response.steps {
        if serde_json::to_value(step.action).unwrap() == action {
            for id in &step.targets {
                let target = response.targets.iter().find(|t| &t.id == id).unwrap();
                names.push(target.explicit_id.clone().unwrap_or(target.name.clone()));
            }
        }
    }
    names
}

fn codes(response: &Response) -> Vec<&'static str> {
    response
        .steps
        .iter()
        .flat_map(|s| s.uncertainty.iter().map(|u| u.code))
        .chain(response.uncertainty.iter().map(|u| u.code))
        .collect()
}

#[test]
fn feature_task_orders_code_requirements_consumers_and_tests_from_evidence() {
    let root = fixture();
    let response = plan(root.path(), &request("Modify session expiration"));
    assert_eq!(response.task.classification, "supported");
    assert_eq!(response.task.search_terms, ["session", "expir"]);
    let implementation = names(&response, "inspect_implementation");
    assert_eq!(implementation[0], "isSessionExpired");
    assert!(names(&response, "review_requirement").contains(&"REQ-AUTH-001".into()));
    assert!(names(&response, "review_decision").contains(&"ADR-0003".into()));
    let consumers = names(&response, "inspect_consumers");
    assert!(
        consumers.contains(&"requireSession".into()),
        "{consumers:?}"
    );
    assert!(consumers.contains(&"refreshToken".into()));
    let tests = names(&response, "review_test_candidates");
    assert!(tests.contains(&"expiresAfterTimeout".into()), "{tests:?}");
    // The legacy member call shares the name but has no resolved target.
    assert!(!consumers.contains(&"purgeExpired".into()));
    assert!(codes(&response).contains(&"unconfirmed_sites"));
    assert!(codes(&response).contains(&"tests_not_executed"));
    assert_eq!(response.steps[0].order, 1);
    for step in &response.steps {
        assert!(!step.evidence.is_empty(), "{step:?}");
        assert!(!step.next.is_empty());
        for id in &step.evidence {
            assert!(response.evidence.iter().any(|e| &e.id == id));
        }
    }
}

#[test]
fn evidence_classes_stay_distinct_and_lexical_matches_are_never_relationships() {
    let root = fixture();
    for task in [
        "Modify session expiration",
        "Investigate token refresh failures",
        "Why does this project use SQLite?",
        "What depends on the authentication service?",
    ] {
        let response = plan(root.path(), &request(task));
        let mut ids = std::collections::BTreeSet::new();
        for evidence in &response.evidence {
            assert!(ids.insert(evidence.id.clone()), "duplicate evidence id");
            let status = evidence.relationship.as_ref().map(|r| r.status.as_str());
            match evidence.class {
                EvidenceClass::LexicalCandidate
                | EvidenceClass::RecordedIntent
                | EvidenceClass::ExplicitSelector => assert_eq!(status, None, "{evidence:?}"),
                EvidenceClass::ValidatedKnowledgeReference => {
                    assert_eq!(status, Some("validated"));
                    assert!(evidence.relationship.as_ref().unwrap().target_id.is_some());
                }
                EvidenceClass::VerifiedStaticRelationship
                | EvidenceClass::AdvisoryTestCandidate => assert_eq!(status, Some("resolved")),
                EvidenceClass::UnresolvedReference => {
                    assert!(evidence.relationship.as_ref().unwrap().target_id.is_none());
                }
                EvidenceClass::StaleReference => {}
                EvidenceClass::ApprovedProjectMemory => {
                    panic!("memory evidence in a plan that did not request it: {evidence:?}")
                }
            }
            let source = fs::read(root.path().join(&evidence.citation.path)).unwrap();
            assert!(evidence.citation.end_byte <= source.len());
            let line = source[..evidence.citation.start_byte]
                .split(|b| *b == b'\n')
                .count();
            assert_eq!(line, evidence.citation.start_line, "{evidence:?}");
        }
    }
}

#[test]
fn explicit_requirement_selector_comes_first_and_follows_only_validated_claims() {
    let root = fixture();
    let response = plan(root.path(), &request("What implements REQ-AUTH-001?"));
    assert_eq!(response.task.selectors, ["REQ-AUTH-001"]);
    assert_eq!(names(&response, "review_requirement")[0], "REQ-AUTH-001");
    assert_eq!(response.steps[0].priority.tier, 1);
    let implementation = names(&response, "inspect_implementation");
    assert!(implementation.contains(&"isSessionExpired".into()));
    let session = fs::read_to_string(root.path().join("src/auth/session.ts")).unwrap();
    for name in &implementation {
        assert!(
            session.contains(name.as_str()),
            "{name} is not in the claimed file"
        );
    }
    assert!(codes(&response).contains(&"behavior_unverified"));
}

#[test]
fn unresolved_and_ambiguous_references_are_reported_without_targets() {
    let root = fixture();
    let response = plan(
        root.path(),
        &request("Find the implementation of REQ-AUDIT-001"),
    );
    assert!(names(&response, "inspect_implementation").is_empty());
    let codes = codes(&response);
    assert!(codes.contains(&"unresolved_reference"), "{codes:?}");
    assert!(codes.contains(&"ambiguous_reference"));
    assert!(response.completeness.unresolved_findings >= 3);
    assert!(!response.evidence.iter().any(|e| {
        e.class == EvidenceClass::ValidatedKnowledgeReference
            && e.relationship
                .as_ref()
                .unwrap()
                .source_id
                .contains("REQ-AUDIT-001")
    }));
}

#[test]
fn architecture_questions_put_decisions_first_and_supersession_is_explicit() {
    let root = fixture();
    let response = plan(root.path(), &request("Why does this project use SQLite?"));
    assert_eq!(names(&response, "review_decision")[0], "ADR-0001");
    assert_eq!(
        serde_json::to_value(response.steps[0].category).unwrap(),
        "knowledge"
    );
    let response = plan(
        root.path(),
        &request("Identify relevant tests for session timeout"),
    );
    assert!(codes(&response).contains(&"superseded_decision"));
    assert!(names(&response, "review_decision").contains(&"ADR-0003".into()));
}

#[test]
fn deterministic_bounded_and_honest_under_tight_budgets() {
    let root = fixture();
    let read = store(root.path());
    let mut previous = None;
    for _ in 0..3 {
        let mut request = Request::new("Modify session expiration");
        request.max_bytes = 4000;
        let response = investigate::investigate(&read, &request, Format::Text).unwrap();
        let text = investigate::render(&response, Format::Text).unwrap();
        assert_eq!(response.budget.used_bytes, text.len());
        assert!(text.len() <= 4000);
        assert!(response.completeness.byte_truncated);
        assert!(!response.completeness.complete_within_limits);
        assert!(!response.completeness.omitted_categories.is_empty());
        if let Some(previous) = &previous {
            assert_eq!(previous, &text);
        }
        previous = Some(text);
    }
    for budget in [1500, 6000, 12_000, 32_000] {
        let mut request = Request::new("What depends on the authentication service?");
        request.max_bytes = budget;
        for format in [Format::Json, Format::Text] {
            if let Ok(response) = investigate::investigate(&read, &request, format) {
                let text = investigate::render(&response, format).unwrap();
                assert_eq!(response.budget.used_bytes, text.len());
                assert!(text.len() <= budget);
            }
        }
    }
    let mut request = Request::new("Modify session expiration");
    request.max_bytes = 200;
    let error = investigate::investigate(&read, &request, Format::Text).unwrap_err();
    assert!(
        error
            .downcast_ref::<code_intel_retrieval::BudgetTooSmall>()
            .is_some()
    );
}

#[test]
fn balanced_selection_keeps_categories_that_tiered_selection_starves() {
    let root = fixture();
    let mut request = request("Modify session expiration");
    request.max_steps = 3;
    let balanced = plan(root.path(), &request);
    request.ranking = Ranking::Tiered;
    let tiered = plan(root.path(), &request);
    let categories = |r: &Response| -> Vec<String> {
        r.steps
            .iter()
            .map(|s| {
                serde_json::to_value(s.category)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    };
    assert_eq!(
        categories(&balanced),
        ["implementation", "knowledge", "consumers"]
    );
    assert!(
        !categories(&tiered).contains(&"knowledge".to_owned()),
        "{:?}",
        categories(&tiered)
    );
    assert!(balanced.completeness.step_limit_reached);
}

#[test]
fn stale_sources_are_labelled_and_never_offered_as_current_ranges() {
    let root = fixture();
    let path = root.path().join("src/auth/session.ts");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("// moved\n// moved\n{source}")).unwrap();
    let response = plan(root.path(), &request("Modify session expiration"));
    assert!(
        response
            .freshness
            .stale_files
            .contains(&"src/auth/session.ts".into())
    );
    let step = response
        .steps
        .iter()
        .find(|s| {
            s.targets
                .iter()
                .any(|t| t.starts_with("function:src/auth/session.ts"))
        })
        .unwrap();
    assert!(step.uncertainty.iter().any(|u| u.code == "stale_source"));
    for next in &step.next {
        if next.tool == "read" && next.path.as_deref() == Some("src/auth/session.ts") {
            assert_eq!(next.start_line, None);
        }
    }
    let target = response
        .targets
        .iter()
        .find(|t| t.location.path == "src/auth/session.ts")
        .unwrap();
    assert_eq!(target.freshness, "stale");
}

#[test]
fn deleted_claim_targets_lose_their_identity_after_explicit_reindexing() {
    let root = fixture();
    fs::remove_file(root.path().join("src/auth/login.ts")).unwrap();
    code_intel_index::index(root.path()).unwrap();
    let mut request = request("Review requirement impact");
    request.selectors = vec!["REQ-AUTH-003".into()];
    let response = plan(root.path(), &request);
    assert!(names(&response, "inspect_implementation").is_empty());
    let codes = codes(&response);
    assert!(
        codes.contains(&"stale_reference") || codes.contains(&"unresolved_reference"),
        "{codes:?}"
    );
}

#[test]
fn repository_text_never_becomes_instructions_and_inputs_are_validated() {
    let root = fixture();
    for task in [
        "Modify session expiration",
        "session expiration notes instructions",
    ] {
        let response = plan(root.path(), &request(task));
        for format in [Format::Json, Format::Text] {
            let text = investigate::render(&response, format).unwrap();
            assert!(!text.contains("rm -rf"), "{text}");
            assert!(!text.contains(".mcp.json"));
            assert!(!text.contains("IMPORTANT SYSTEM"));
        }
        for step in &response.steps {
            for next in &step.next {
                assert!(
                    [
                        "read",
                        "atlas_references",
                        "atlas_dependencies",
                        "atlas_knowledge_trace"
                    ]
                    .contains(&next.tool)
                );
                if let Some(path) = &next.path {
                    assert!(!path.starts_with('/') && !path.contains(".."));
                }
            }
        }
    }
    let read = store(root.path());
    for bad in ["x".repeat(8193), "session\u{1b}[2J".into(), String::new()] {
        assert!(investigate::investigate(&read, &Request::new(bad), Format::Text).is_err());
    }
    let mut request = request("Modify session");
    request.depth = 4;
    assert!(investigate::investigate(&read, &request, Format::Text).is_err());
    let mut request = Request::new("session");
    request.selectors = vec!["../../../etc/passwd".into()];
    let response = investigate::investigate(&read, &request, Format::Json).unwrap();
    assert!(
        response
            .uncertainty
            .iter()
            .any(|u| u.code == "selector_not_found")
    );
    let response =
        investigate::investigate(&read, &Request::new("the of and"), Format::Json).unwrap();
    assert_eq!(response.task.classification, "unsupported");
    assert!(response.steps.is_empty());
}

#[test]
fn cli_is_read_only_never_indexes_and_reports_unavailable_knowledge() {
    let root = fixture();
    let database = root.path().join(".code-intel/index.db");
    let before = fs::read(&database).unwrap();
    let listing = |dir: &Path| {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let state_before = listing(&root.path().join(".code-intel"));
    let text = run(root.path(), &["investigate", "Modify session expiration"]);
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.starts_with("Investigation r1"));
    assert!(text.len() <= 12_000);
    let json = run(
        root.path(),
        &[
            "investigate",
            "x",
            "--selector",
            "REQ-AUTH-001",
            "--format",
            "json",
            "--max-steps",
            "3",
            "--depth",
            "1",
        ],
    );
    let value: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["kind"], "investigation");
    assert_eq!(value["budget"]["max_bytes"], 24_000);
    assert!(value["steps"].as_array().unwrap().len() <= 3);
    assert_eq!(before, fs::read(&database).unwrap());
    assert_eq!(state_before, listing(&root.path().join(".code-intel")));

    let empty = TempDir::new().unwrap();
    let missing = run(empty.path(), &["investigate", "session"]);
    assert!(!missing.status.success());
    assert!(!empty.path().join(".code-intel").exists());

    // An index whose knowledge layer is outdated still yields code evidence.
    Connection::open(&database)
        .unwrap()
        .execute("DELETE FROM metadata WHERE key='relationships_indexed'", [])
        .unwrap();
    let response = plan(root.path(), &request("Modify session expiration"));
    assert!(!response.completeness.unsupported.is_empty());
    assert!(names(&response, "review_requirement").is_empty());
    assert!(!names(&response, "inspect_implementation").is_empty());
}
