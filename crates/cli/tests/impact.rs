//! Git-history fixtures for `repogi impact` (Phase 4.1). Each case in
//! `fixtures/impact/cases` applies an overlay to `fixtures/impact/repo` as a
//! commit, a staged change or an unstaged working-tree change; `case.json`
//! holds hand-written expectations (independent of Atlas output).
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/impact")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            // Fresh mtimes: macOS `fs::copy` keeps the source times, which can hide
            // same-size overlay edits from Git's stat check.
            fs::write(&target, fs::read(&path).unwrap()).unwrap();
        }
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Atlas Test",
            "-c",
            "user.email=atlas@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A repository holding the base commit.
fn base_repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    copy_dir(&fixtures().join("repo"), temp.path());
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["add", "-A"]);
    git(temp.path(), &["commit", "-q", "-m", "base"]);
    temp
}

/// Apply a case; returns the impact arguments selecting base and head.
fn apply(root: &Path, case: &Path, spec: &Value) -> Vec<String> {
    for pair in spec["rename"].as_array().unwrap() {
        let (from, to) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
        fs::create_dir_all(root.join(to).parent().unwrap()).unwrap();
        fs::rename(root.join(from), root.join(to)).unwrap();
    }
    for path in spec["delete"].as_array().unwrap() {
        fs::remove_file(root.join(path.as_str().unwrap())).unwrap();
    }
    if case.join("overlay").is_dir() {
        copy_dir(&case.join("overlay"), root);
    }
    let mut args: Vec<String> = match spec["mode"].as_str().unwrap() {
        "commit" => {
            git(root, &["add", "-A"]);
            git(root, &["commit", "-q", "-m", "head"]);
            vec![
                "--base".into(),
                "HEAD~1".into(),
                "--head".into(),
                "HEAD".into(),
            ]
        }
        "staged" => {
            git(root, &["add", "-A"]);
            vec!["--base".into(), "HEAD".into()]
        }
        "worktree" => vec!["--base".into(), "HEAD".into(), "--worktree".into()],
        mode => panic!("unknown mode {mode}"),
    };
    if case.join("unstaged").is_dir() {
        copy_dir(&case.join("unstaged"), root);
    }
    args.extend(
        spec["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_owned()),
    );
    args
}

fn impact(root: &Path, args: &[String]) -> Value {
    let output = Command::new(BINARY)
        .arg("impact")
        .arg(root)
        .args(["--format", "json", "--max-bytes", "8000000"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "impact {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn files(report: &Value) -> BTreeSet<String> {
    report["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            let letter = match text(&file["status"]) {
                "added" => "A",
                "removed" => "D",
                "renamed" => "R",
                _ => "M",
            };
            let mut line = format!("{letter} {}", text(&file["path"]));
            if let Some(previous) = file["previous_path"].as_str() {
                let _ = write!(line, "<-{previous}");
            }
            if file["category"] != "source" {
                let _ = write!(line, " {}", text(&file["category"]));
            }
            if let Some(state) = file["git_state"].as_str() {
                let _ = write!(line, " {state}");
            }
            line
        })
        .collect()
}

fn declarations(report: &Value) -> Vec<String> {
    let mut lines: Vec<String> = report["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| {
            let mut flags = Vec::new();
            if change["signature_changed"] == true {
                flags.push("signature");
            }
            if change["body_changed"] == true {
                flags.push("body");
            }
            if change["export_changed"] == true {
                flags.push("exports");
            }
            if change["whitespace_only"] == true {
                flags.push("whitespace");
            }
            if change["uncertain"] == true {
                flags.push("uncertain");
            }
            let mut line = format!(
                "{} {} {} {}",
                text(&change["change"]),
                text(&change["kind"]),
                text(&change["name"]),
                text(&change["path"])
            );
            if !flags.is_empty() {
                let _ = write!(line, " {}", flags.join(","));
            }
            line
        })
        .collect::<Vec<_>>();
    lines.sort();
    lines
}

fn modules(report: &Value) -> BTreeSet<String> {
    report["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| {
            let mut parts = Vec::new();
            for (field, name) in [
                ("imports_changed", "imports"),
                ("exports_changed", "exports"),
                ("statements_changed", "statements"),
            ] {
                if module[field] == true {
                    parts.push(name);
                }
            }
            format!("{} {}", text(&module["path"]), parts.join(","))
        })
        .collect()
}

fn site(site: &Value) -> String {
    let kind = if site["jsx"] == true {
        format!("{}(JSX)", text(&site["kind"]))
    } else {
        text(&site["kind"]).to_owned()
    };
    let consumer = site["consumer"]["name"].as_str().unwrap_or("module");
    format!(
        "{}:{} {kind} {consumer} {}",
        text(&site["path"]),
        site["line"],
        text(&site["snapshot"])
    )
}

fn possible(entry: &Value) -> String {
    if entry["reason"] == "unanalysed_file" {
        format!("unanalysed {}", text(&entry["path"]))
    } else {
        format!(
            "{}:{} {} {}",
            text(&entry["path"]),
            entry["line"],
            text(&entry["kind"]),
            text(&entry["target_text"])
        )
    }
}

fn binding(binding: &Value, long: bool) -> String {
    if binding.is_null() {
        return "absent".into();
    }
    let status = text(&binding["status"]);
    match (&binding["target"], long) {
        (target, true) if !target.is_null() => {
            format!(
                "{status} {}#{}",
                text(&target["path"]),
                text(&target["name"])
            )
        }
        _ => status.to_owned(),
    }
}

fn rebound(report: &Value, long: bool) -> BTreeSet<String> {
    report["rebound"]
        .as_array()
        .unwrap()
        .iter()
        .map(|site| {
            let kind = if site["jsx"] == true {
                format!("{}(JSX)", text(&site["kind"]))
            } else {
                text(&site["kind"]).to_owned()
            };
            format!(
                "{}:{} {kind} {} {}->{}",
                text(&site["path"]),
                site["line"],
                text(&site["target_text"]),
                binding(&site["before"], long),
                binding(&site["after"], long)
            )
        })
        .collect()
}

fn strings(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

/// The impact entry a case key names: `name`, or `name path change`.
fn find_impact<'a>(report: &'a Value, key: &str) -> &'a Value {
    let parts: Vec<&str> = key.split(' ').collect();
    let matches: Vec<&Value> = report["impacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|impact| {
            impact["name"] == parts[0]
                && (parts.len() < 3 || (impact["path"] == parts[1] && impact["change"] == parts[2]))
        })
        .collect();
    assert_eq!(matches.len(), 1, "impact {key}: {matches:#?}");
    matches[0]
}

fn check(name: &str, report: &Value, expected: &Value) {
    let context = |what: &str| {
        format!(
            "{name}: {what}\n{}",
            serde_json::to_string_pretty(report).unwrap()
        )
    };
    if let Some(want) = expected.get("files") {
        assert_eq!(files(report), strings(want), "{}", context("files"));
    }
    if let Some(want) = expected.get("declarations") {
        let mut want: Vec<String> = strings(want).into_iter().collect();
        if let Some(extra) = expected.get("declarations_repeated") {
            want.extend(
                extra
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned()),
            );
        }
        want.sort();
        assert_eq!(declarations(report), want, "{}", context("declarations"));
    }
    if let Some(want) = expected.get("modules") {
        assert_eq!(modules(report), strings(want), "{}", context("modules"));
    }
    if let Some(want) = expected.get("impacts") {
        check_impacts(report, want, &context);
    }
    if let Some(want) = expected.get("rebound") {
        let long = strings(want).iter().any(|entry| entry.contains('#'));
        assert_eq!(
            rebound(report, long),
            strings(want),
            "{}",
            context("rebound")
        );
    }
    if let Some(want) = expected.get("rebound_contains") {
        let got = rebound(report, false);
        for entry in strings(want) {
            assert!(
                got.contains(&entry),
                "{}",
                context(&format!("rebound lacks {entry}"))
            );
        }
    }
    if let Some(paths) = expected.get("absent_paths") {
        let rendered = serde_json::to_string(&report["impacts"]).unwrap()
            + &serde_json::to_string(&report["affected_files"]).unwrap();
        for path in strings(paths) {
            assert!(
                !rendered.contains(&format!("\"{path}\"")),
                "{}",
                context(&format!("{path} must not appear"))
            );
        }
    }
    for field in [
        "complete",
        "truncated",
        "resolution_changed",
        "unchanged_declarations",
    ] {
        if let Some(want) = expected.get(field) {
            assert_eq!(&report[field], want, "{}", context(field));
        }
    }
}

fn check_impacts(report: &Value, want: &Value, context: &dyn Fn(&str) -> String) {
    let want = want.as_object().unwrap();
    if want.is_empty() {
        assert!(
            report["impacts"].as_array().unwrap().is_empty(),
            "{}",
            context("impacts")
        );
    }
    for (key, want) in want {
        let impact = find_impact(report, key);
        for list in ["direct", "import_only"] {
            if let Some(want) = want.get(list) {
                let got: BTreeSet<String> =
                    impact[list].as_array().unwrap().iter().map(site).collect();
                assert_eq!(got, strings(want), "{}", context(&format!("{key} {list}")));
            }
        }
        if let Some(want) = want.get("transitive") {
            let got: BTreeSet<String> = impact["transitive"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| format!("{} {}", text(&d["name"]), d["depth"]))
                .collect();
            assert_eq!(
                got,
                strings(want),
                "{}",
                context(&format!("{key} transitive"))
            );
        }
        if let Some(want) = want.get("possible") {
            let got: BTreeSet<String> = impact["possible"]
                .as_array()
                .unwrap()
                .iter()
                .map(possible)
                .collect();
            assert_eq!(
                got,
                strings(want),
                "{}",
                context(&format!("{key} possible"))
            );
        }
        if let Some(want) = want.get("complete") {
            assert_eq!(
                &impact["complete"],
                want,
                "{}",
                context(&format!("{key} complete"))
            );
        }
        if let Some(want) = want.get("direct_count") {
            assert_eq!(
                impact["direct"].as_array().unwrap().len() as u64,
                want.as_u64().unwrap(),
                "{}",
                context(&format!("{key} direct_count"))
            );
        }
        if let Some(want) = want.get("omitted") {
            for (list, count) in want.as_object().unwrap() {
                assert_eq!(
                    &impact["omitted"][list],
                    count,
                    "{}",
                    context(&format!("{key} omitted {list}"))
                );
            }
        }
        // Never a confirmed site that is not resolved.
        for list in ["direct", "import_only", "re_exported_by"] {
            for entry in impact[list].as_array().unwrap() {
                assert_eq!(
                    entry["status"],
                    "resolved",
                    "{}",
                    context("unconfirmed site listed as confirmed")
                );
            }
        }
    }
}

/// Volatile provenance (timings, cache use) removed for determinism checks.
fn stable(mut report: Value) -> Value {
    for side in ["base", "head"] {
        for field in [
            "seconds",
            "index",
            "files_reparsed",
            "seeded",
            "diagnostics",
        ] {
            report[side].as_object_mut().unwrap().remove(field);
        }
    }
    report
}

#[test]
#[allow(clippy::too_many_lines)]
fn git_history_fixtures_match_independent_labels() {
    let mut cases: Vec<PathBuf> = fs::read_dir(fixtures().join("cases"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    cases.sort();
    assert!(cases.len() >= 16);
    for case in cases {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let spec: Value =
            serde_json::from_str(&fs::read_to_string(case.join("case.json")).unwrap()).unwrap();
        let repository = base_repository();
        let args = apply(repository.path(), &case, &spec);
        let first = impact(repository.path(), &args);
        check(&name, &first, &spec["expected"]);
        let mut manifest_args = args.clone();
        manifest_args.push("--manifest".into());
        let manifest = impact(repository.path(), &manifest_args);
        let mut expected = spec["expected"].clone();
        // These two fields describe legacy fitting; the manifest preserves all
        // discovered evidence and exposes analysis limits separately.
        expected.as_object_mut().unwrap().remove("complete");
        expected.as_object_mut().unwrap().remove("truncated");
        let projection = serde_json::json!({
            "files":manifest["changes"]["files"],
            "declarations":manifest["changes"]["declarations"],
            "modules":manifest["changes"]["modules"],
            "impacts":manifest["affected_code"]["relationships"],
            "rebound":manifest["affected_code"]["binding_changes"],
            "affected_files":manifest["affected_code"]["files"],
            "resolution_changed":manifest["changes"]["resolution_changed"],
            "unchanged_declarations":manifest["changes"]["unchanged_declarations"]
        });
        if first["truncated"] == true {
            // Legacy per-list caps discard already-discovered direct sites.
            // Manifest mode retains them and marks the traversal limit instead.
            assert_eq!(manifest["completeness"]["analysis_limited"], true);
            assert_eq!(manifest["completeness"]["output_truncated"], false);
        } else {
            check(&name, &projection, &expected);
        }
        for relationship in manifest["affected_code"]["relationships"]
            .as_array()
            .unwrap()
        {
            assert!(
                manifest["changes"]["declarations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["evidence_key"] == relationship["changed_declaration"])
            );
        }
        let mut summary_args = args.clone();
        summary_args.push("--summary".into());
        // Summary does not accept --detail; the fixture options set only work
        // limits and revisions today.
        let summary = impact(repository.path(), &summary_args);
        let projected = |category: &str| -> BTreeSet<String> {
            summary["files"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| {
                    f["categories"].as_object().unwrap().keys().any(|k| {
                        if category == "direct" {
                            k.starts_with("direct_")
                        } else {
                            k == category
                        }
                    })
                })
                .map(|f| text(&f["path"]).into())
                .collect()
        };
        for (key, category) in [
            ("direct", "direct"),
            ("transitive", "transitive"),
            ("possible", "possible"),
            ("import_only", "import_only"),
            ("re_exported_by", "re_export"),
        ] {
            let original: BTreeSet<String> = first["impacts"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|i| i[key].as_array().unwrap())
                .map(|s| text(&s["path"]).into())
                .collect();
            if summary["truncated"].as_bool().unwrap() {
                assert_eq!(summary["complete"], false, "{name}: omitted rows");
            } else {
                assert!(
                    original.is_subset(&projected(category)),
                    "{name}: {category} lost files"
                );
            }
            if !first["truncated"].as_bool().unwrap() && !summary["truncated"].as_bool().unwrap() {
                assert_eq!(
                    original,
                    projected(category),
                    "{name}: {category} invented files"
                );
            }
        }
        if first["impacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["limit_reached"] == true)
        {
            assert_eq!(summary["traversal_complete"], false, "{name}");
        }
        // Deterministic, and the second run reuses the cached base snapshot.
        let second = impact(repository.path(), &args);
        assert_eq!(
            second["base"]["index"], "cache",
            "{name}: base snapshot reused"
        );
        assert_eq!(
            stable(first),
            stable(second),
            "{name}: deterministic output"
        );
    }
}

#[test]
fn analysis_is_read_only_for_the_repository_and_user_index() {
    let repository = base_repository();
    let root = repository.path();
    let index = Command::new(BINARY)
        .current_dir(root)
        .args(["index", "."])
        .output()
        .unwrap();
    assert!(index.status.success());
    fs::write(
        root.join("src/lib/format.ts"),
        fs::read_to_string(root.join("src/lib/format.ts"))
            .unwrap()
            .replace("toFixed(2)", "toFixed(3)"),
    )
    .unwrap();
    // `git status` may refresh .git/index itself: run it before taking the snapshot.
    let status = git(
        root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            "src",
            "tsconfig.json",
        ],
    );
    let database = fs::read(root.join(".code-intel/index.db")).unwrap();
    let git_index = fs::read(root.join(".git/index")).unwrap();
    let head = git(root, &["rev-parse", "HEAD"]);
    let report = impact(root, &["--base".into(), "HEAD".into()]);
    // The user's index is stale (format.ts changed), so it is not used for the working tree.
    assert_eq!(report["head"]["index"], "built");
    assert_eq!(
        fs::read(root.join(".code-intel/index.db")).unwrap(),
        database
    );
    assert!(
        fs::read(root.join(".git/index")).unwrap() == git_index,
        ".git/index was modified"
    );
    assert_eq!(git(root, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(
            root,
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                "src",
                "tsconfig.json"
            ]
        ),
        status
    );
    // A fresh user index is reused read-only for the working tree.
    let index = Command::new(BINARY)
        .current_dir(root)
        .args(["index", "."])
        .output()
        .unwrap();
    assert!(index.status.success());
    let database = fs::read(root.join(".code-intel/index.db")).unwrap();
    let report = impact(root, &["--base".into(), "HEAD".into()]);
    assert_eq!(report["head"]["index"], "user_index");
    assert_eq!(
        fs::read(root.join(".code-intel/index.db")).unwrap(),
        database
    );
}

#[test]
fn seeded_snapshot_graph_equals_a_cold_index() {
    let repository = base_repository();
    let root = repository.path();
    let base = git(root, &["rev-parse", "HEAD"]).trim().to_owned();
    fs::write(
        root.join("src/lib/math.ts"),
        fs::read_to_string(root.join("src/lib/math.ts"))
            .unwrap()
            .replace("a + b", "b + a"),
    )
    .unwrap();
    git(root, &["commit", "-q", "-am", "head"]);
    // Head is seeded from the base snapshot and indexed incrementally.
    let report = impact(
        root,
        &[
            "--base".into(),
            base.clone(),
            "--head".into(),
            "HEAD".into(),
        ],
    );
    assert_eq!(report["head"]["seeded"], true);
    let tree = report["head"]["tree"].as_str().unwrap();
    let trees = root.join(".code-intel/impact/trees");
    let snapshot = fs::read_dir(&trees)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(tree)
        })
        .unwrap();
    let cold = TempDir::new().unwrap();
    copy_dir(&snapshot.join("src"), cold.path());
    let indexed = Command::new(BINARY)
        .current_dir(cold.path())
        .args(["index", "."])
        .output()
        .unwrap();
    assert!(indexed.status.success());
    let edges = |database: &Path| -> Vec<String> {
        let connection = rusqlite::Connection::open(database).unwrap();
        let mut query = connection
            .prepare("SELECT data FROM semantic_edges ORDER BY ordinal")
            .unwrap();
        query
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let seeded = edges(&snapshot.join("index.db"));
    assert!(!seeded.is_empty());
    assert_eq!(seeded, edges(&cold.path().join(".code-intel/index.db")));
}

#[test]
fn invalid_requests_fail_clearly() {
    let plain = TempDir::new().unwrap();
    let output = Command::new(BINARY)
        .args(["impact"])
        .arg(plain.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a Git work tree"));
    let repository = base_repository();
    let output = Command::new(BINARY)
        .args(["impact"])
        .arg(repository.path())
        .args(["--base", "no-such-branch"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown revision"));
    let output = Command::new(BINARY)
        .args(["impact"])
        .arg(repository.path())
        .args(["--base", "--output=/tmp/x"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn text_output_is_concise_and_bounded() {
    let repository = base_repository();
    let root = repository.path();
    fs::write(
        root.join("src/components/MetricCard.tsx"),
        fs::read_to_string(root.join("src/components/MetricCard.tsx"))
            .unwrap()
            .replace("<div", "<span")
            .replace("</div>", "</span>"),
    )
    .unwrap();
    let run = |extra: &[&str]| -> String {
        let output = Command::new(BINARY)
            .arg("impact")
            .arg(root)
            .args(extra)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let text = run(&[]);
    assert!(text.starts_with("atlas impact · HEAD ("), "{text}");
    assert!(
        text.contains("src/components/Stats.tsx:7 REFERENCES(JSX) from Stats"),
        "{text}"
    );
    assert!(text.contains("INCOMPLETE"), "{text}");
    assert!(
        text.contains("src/broken.test.tsx could not be analysed"),
        "{text}"
    );
    let detailed = run(&["--detail"]);
    assert!(
        detailed.contains("transitive d2: function Dashboard"),
        "{detailed}"
    );
    let small = run(&["--detail", "--max-bytes", "2000"]);
    assert!(small.len() <= 2000, "{}", small.len());
    let json = run(&["--format", "json", "--max-bytes", "2500"]);
    assert!(json.len() <= 2500, "{}", json.len());
    let bounded: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(bounded["truncated"], true);
    assert_eq!(bounded["complete"], false);
    assert!(
        bounded["omitted"]
            .as_object()
            .is_some_and(|o| !o.is_empty())
            || bounded["affected_files_omitted"].as_u64() > Some(0)
    );
}
