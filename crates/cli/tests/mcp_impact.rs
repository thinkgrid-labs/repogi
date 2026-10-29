//! `atlas_impact` over MCP (Phase 4.2): opt-in tool selection with unchanged
//! profiles, agreement with the CLI on the same analysis, bounded and
//! deterministic output, argument validation, configured-root confinement and
//! read-only behaviour. No dependency analysis is MCP-specific: the tool calls
//! `impact::compare` and the CLI's renderers.
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");
const TIMEOUT: Duration = Duration::from_secs(60);

struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next: i64,
    instructions: String,
}

impl Server {
    fn start(root: &Path, options: &[&str]) -> Self {
        let mut child = Command::new(BINARY)
            .arg("mcp")
            .args(options)
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take().unwrap();
        let mut server = Self {
            child,
            stdin,
            lines,
            next: 1,
            instructions: String::new(),
        };
        let init = server.request("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}));
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .clone_into(&mut server.instructions);
        server.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        server
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let mut message = serde_json::Map::new();
        message.insert("jsonrpc".into(), json!("2.0"));
        message.insert("id".into(), json!(id));
        message.insert("method".into(), json!(method));
        message.insert("params".into(), params);
        self.send(&Value::Object(message));
        let line = self.lines.recv_timeout(TIMEOUT).expect("response");
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        response
    }

    fn tools(&mut self) -> Vec<String> {
        self.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// (text, isError)
    fn call(&mut self, arguments: Value) -> (String, bool) {
        let mut params = serde_json::Map::new();
        params.insert("name".into(), json!("atlas_impact"));
        params.insert("arguments".into(), arguments);
        let response = self.request("tools/call", Value::Object(params));
        let result = &response["result"];
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap(),
        )
    }

    fn ok(&mut self, arguments: Value) -> String {
        let (text, error) = self.call(arguments);
        assert!(!error, "{text}");
        text
    }

    fn json(&mut self, mut arguments: Value) -> Value {
        arguments["format"] = json!("json");
        serde_json::from_str(&self.ok(arguments)).unwrap()
    }

    fn error_code(&mut self, arguments: Value) -> String {
        let (text, error) = self.call(arguments);
        assert!(error, "expected an error: {text}");
        text.strip_prefix("error[")
            .and_then(|rest| rest.split(']').next())
            .unwrap()
            .to_owned()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
            "-c",
            "core.hooksPath=/dev/null",
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

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            // Fresh mtimes: macOS `fs::copy` keeps the source times, which can hide
            // same-size overlay edits from Git's stat check.
            std::fs::write(&target, std::fs::read(&path).unwrap()).unwrap();
        }
    }
}

/// The Phase 4.1 fixture repository at one base commit.
fn repository() -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/impact/repo"),
        temp.path(),
    );
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["add", "-A"]);
    git(temp.path(), &["commit", "-q", "-m", "base"]);
    let root = temp.path().canonicalize().unwrap();
    (temp, root)
}

fn edit(root: &Path, path: &str, from: &str, to: &str) {
    let path = root.join(path);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "{from} not in {}", path.display());
    std::fs::write(path, text.replacen(from, to, 1)).unwrap();
}

fn cli(root: &Path, args: &[&str]) -> String {
    let output = Command::new(BINARY)
        .arg("impact")
        .arg(root)
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

/// The report without per-run provenance (timings, cache use).
fn stable(mut report: Value) -> Value {
    for side in ["base", "head"] {
        for field in ["seconds", "index", "files_reparsed", "seeded"] {
            report[side].as_object_mut().unwrap().remove(field);
        }
    }
    report
}

/// MCP and CLI agree on the same analysis: JSON (minus provenance) and the
/// agent text (same renderer and budget) are identical.
fn agrees(server: &mut Server, root: &Path, arguments: Value, cli_args: &[&str]) -> Value {
    let mut json_args = arguments.clone();
    json_args["max_bytes"] = json!(32000);
    let report = server.json(json_args);
    let mut args = vec!["--format", "json", "--max-bytes", "32000"];
    args.extend_from_slice(cli_args);
    let expected: Value = serde_json::from_str(&cli(root, &args)).unwrap();
    assert_eq!(
        stable(report.clone()),
        stable(expected),
        "MCP JSON differs from the CLI"
    );
    let mut text_args = arguments;
    text_args["max_bytes"] = json!(8000);
    let mut args = vec!["--max-bytes", "8000"];
    args.extend_from_slice(cli_args);
    assert_eq!(
        server.ok(text_args),
        cli(root, &args),
        "MCP agent text differs from the CLI text"
    );
    report
}

fn impact_only(root: &Path) -> Server {
    Server::start(root, &["--tools", "atlas_impact,atlas_status"])
}

#[test]
fn summaries_group_before_site_caps_and_evidence_matches_cli() {
    let (_temp, root) = repository();
    std::fs::remove_file(root.join("src/broken.test.tsx")).unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "analysable fixture"]);
    // More than the legacy per-list cap, all within the incoming work limit.
    let calls = (0..250).map(|_| "formatNumber(12);\n").collect::<String>();
    std::fs::write(
        root.join("src/a.ts"),
        format!(
            "import {{ formatNumber }} from './lib/format';\nexport function many() {{\n{calls}}}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("src/z.tsx"),
        "import { many } from './a';\nexport function later() { return many(); }\n",
    )
    .unwrap();
    for number in 0..30 {
        std::fs::write(
            root.join(format!("src/fan-{number:02}.ts")),
            "import { formatNumber } from './lib/format';\nformatNumber(12);\n",
        )
        .unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "many sites"]);
    edit(&root, "src/lib/format.ts", "toFixed(2)", "toFixed(3)");
    let mut server = impact_only(&root);
    let args = json!({"summary":true,"max_bytes":8000,"format":"json"});
    let summary: Value = serde_json::from_str(&server.ok(args.clone())).unwrap();
    let expected: Value = serde_json::from_str(&cli(
        &root,
        &["--summary", "--format", "json", "--max-bytes", "8000"],
    ))
    .unwrap();
    assert_eq!(summary, expected);
    assert_eq!(summary["direct_files_complete"], true);
    let rows = summary["files"].as_array().unwrap();
    assert_eq!(rows.iter().filter(|f| f["path"] == "src/a.ts").count(), 1);
    assert_eq!(
        rows.iter().find(|f| f["path"] == "src/a.ts").unwrap()["categories"]["direct_call"],
        250
    );
    assert!(rows.iter().any(
        |f| f["path"] == "src/z.tsx" && f["categories"]["transitive"].as_u64().unwrap_or(0) > 0
    ));
    assert_eq!(
        server.ok(json!({"summary":true})),
        cli(&root, &["--summary", "--max-bytes", "8000"])
    );
    let evidence =
        server.ok(json!({"evidence_file":"src/z.tsx","format":"json","max_bytes":32000}));
    let actual: Value = serde_json::from_str(&evidence).unwrap();
    let expected: Value = serde_json::from_str(&cli(
        &root,
        &[
            "--evidence-file",
            "src/z.tsx",
            "--format",
            "json",
            "--max-bytes",
            "32000",
        ],
    ))
    .unwrap();
    assert_eq!(stable(actual.clone()), stable(expected));
    assert_eq!(actual["complete"], false); // explicitly a selected subset
    for impact in actual["impacts"].as_array().unwrap() {
        for key in [
            "direct",
            "transitive",
            "possible",
            "import_only",
            "re_exported_by",
        ] {
            assert!(
                impact[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|site| site["path"] == "src/z.tsx")
            );
        }
    }
    let small = server.json(json!({"summary":true,"max_bytes":2000}));
    assert_eq!(small["truncated"], true);
    assert_eq!(small["complete"], false);
    assert_eq!(small["direct_files_complete"], false);
    assert_eq!(small["total_files"], summary["total_files"]);
    assert_eq!(
        small["omitted_files"].as_u64().unwrap() + small["files"].as_array().unwrap().len() as u64,
        summary["total_files"].as_u64().unwrap()
    );
    assert_eq!(
        server.error_code(json!({"summary":true,"detail":true})),
        "invalid_arguments"
    );
    assert!(server.call(json!({"evidence_file":"../outside"})).1);
}

#[test]
fn summary_empty_and_snapshot_changes() {
    let (_temp, root) = repository();
    std::fs::remove_file(root.join("src/broken.test.tsx")).unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "analysable fixture"]);
    let mut server = impact_only(&root);
    let empty = server.json(json!({"summary":true,"max_bytes":2000}));
    assert_eq!(empty["complete"], true);
    assert_eq!(empty["total_files"], 0);
    edit(&root, "src/lib/format.ts", "toFixed(2)", "toFixed(3)");
    let changed = server.json(json!({"summary":true,"max_bytes":8000}));
    assert_ne!(empty["head_digest"], changed["head_digest"]);
    assert_eq!(empty["base_digest"], changed["base_digest"]);
    assert_eq!(changed["snapshots_fresh"], true);
}

#[test]
fn summary_repeated_jsx_and_hundreds_of_rebound_sites_stay_distinct() {
    let (_temp, root) = repository();
    let jsx = "<MetricCard value={1} />".repeat(250);
    std::fs::write(root.join("src/repeated.tsx"), format!(
        "import {{ MetricCard }} from '@/components/MetricCard';\nexport function Repeated() {{ return <>{jsx}</>; }}\n"
    )).unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "repeated JSX"]);
    edit(&root, "src/components/MetricCard.tsx", "<div", "<section");
    edit(
        &root,
        "src/components/MetricCard.tsx",
        "</div>",
        "</section>",
    );
    let mut server = impact_only(&root);
    let summary = server.json(json!({"summary":true,"max_bytes":8000}));
    let repeated = summary["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "src/repeated.tsx")
        .unwrap();
    assert_eq!(repeated["categories"]["direct_jsx"], 250);
    assert_eq!(summary["truncated"], false);
    // Restore the component, then invalidate its alias. Lost bindings stay
    // unverified even though they were resolved in the baseline.
    git(&root, &["checkout", "--", "src/components/MetricCard.tsx"]);
    edit(&root, "tsconfig.json", "./src/*", "./src/components/*");
    let rebound = server.json(json!({"summary":true,"max_bytes":8000}));
    let repeated = rebound["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "src/repeated.tsx")
        .unwrap();
    assert_eq!(repeated["categories"]["binding_lost"], 251);
    assert!(repeated["categories"].get("direct_jsx").is_none());
    assert_eq!(rebound["possible_unverified"], true);
    assert_eq!(rebound["complete"], false);
    assert_eq!(rebound["truncated"], false);
    let capped = server.json(json!({"summary":true,"max_results":1,"max_bytes":2000}));
    assert_eq!(capped["truncated"], true);
    assert_eq!(capped["possible_unverified"], true);
    assert_eq!(capped["complete"], false);
    assert_eq!(capped["total_files"], rebound["total_files"]);
    assert_eq!(
        capped["total_by_category"]["binding_lost"],
        rebound["total_by_category"]["binding_lost"]
    );
    // Unresolved export-star source: no fabricated direct consumer.
    git(&root, &["checkout", "--", "tsconfig.json"]);
    std::fs::write(
        root.join("src/lib/index.ts"),
        "export * from './missing';\n",
    )
    .unwrap();
    let barrel = server.json(json!({"summary":true,"max_bytes":8000}));
    assert_eq!(barrel["possible_unverified"], true);
    assert_eq!(barrel["complete"], false);
    assert!(
        barrel["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["categories"]["binding_lost"].as_u64().unwrap_or(0) > 0)
    );
}

#[test]
fn profiles_are_unchanged_and_impact_is_opt_in() {
    let (_temp, root) = repository();
    assert_eq!(
        Server::start(&root, &[]).tools(),
        [
            "atlas_context",
            "atlas_symbol",
            "atlas_references",
            "atlas_dependencies",
            "atlas_status"
        ]
    );
    assert_eq!(
        Server::start(&root, &["--profile", "compact"]).tools(),
        ["atlas_context", "atlas_symbol"]
    );
    let mut minimal = Server::start(&root, &["--profile", "compact", "--tools", "atlas_impact"]);
    assert_eq!(minimal.tools(), ["atlas_impact"]);
    let mut pair = impact_only(&root);
    assert_eq!(pair.tools(), ["atlas_impact", "atlas_status"]);
    for instructions in [&minimal.instructions, &pair.instructions] {
        assert!(
            instructions.contains("atlas_impact") && instructions.len() < 600,
            "{instructions}"
        );
        assert!(
            !instructions.contains("atlas_context") && !instructions.contains("atlas_references")
        );
        assert!(
            !instructions.contains("last `repogi index` run"),
            "{instructions}"
        );
    }
    let mut all = Server::start(
        &root,
        &["--tools", "atlas_context,atlas_impact,atlas_references"],
    );
    assert_eq!(
        all.tools(),
        ["atlas_context", "atlas_references", "atlas_impact"]
    );
}

#[test]
fn scenarios_match_the_cli_and_keep_categories_apart() {
    let (_temp, root) = repository();
    let mut server = impact_only(&root);
    // No change.
    let report = agrees(&mut server, &root, json!({}), &[]);
    assert!(report["files"].as_array().unwrap().is_empty());
    assert_eq!(report["complete"], true);
    // Modified function, worktree.
    edit(&root, "src/lib/format.ts", "toFixed(2)", "toFixed(3)");
    let report = agrees(&mut server, &root, json!({}), &[]);
    let direct = &report["impacts"][0]["direct"];
    assert_eq!(direct[0]["path"], "src/components/MetricCard.tsx");
    assert_eq!(direct[0]["kind"], "CALLS");
    // Deleted function, committed: baseline consumers survive.
    edit(
        &root,
        "src/lib/format.ts",
        "export function formatDate(value: Date): string {\n  return value.toISOString();\n}\n\n",
        "",
    );
    git(&root, &["commit", "-q", "-am", "delete formatDate"]);
    let report = agrees(
        &mut server,
        &root,
        json!({"base":"HEAD~1","head":"HEAD"}),
        &["--base", "HEAD~1", "--head", "HEAD"],
    );
    let deleted = report["impacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "formatDate")
        .unwrap();
    assert_eq!(deleted["change"], "removed");
    assert_eq!(deleted["direct"][0]["path"], "src/pages/Report.tsx");
    assert_eq!(deleted["direct"][0]["snapshot"], "base");
    // Possible impact stays possible: the member call on a parameter.
    assert!(
        deleted["possible"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["status"] != "resolved")
    );
    assert_eq!(deleted["complete"], false);
    // React component with a JSX consumer, plus a file with syntax errors.
    edit(
        &root,
        "src/components/MetricCard.tsx",
        "<div className",
        "<span className",
    );
    edit(&root, "src/components/MetricCard.tsx", "</div>", "</span>");
    let report = agrees(&mut server, &root, json!({}), &[]);
    let card = report["impacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "MetricCard")
        .unwrap();
    assert_eq!(card["direct"][0]["jsx"], true);
    assert!(
        card["possible"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["reason"] == "unanalysed_file")
    );
    let text = server.ok(json!({}));
    assert!(
        text.contains("REFERENCES(JSX) from Stats") && text.contains("INCOMPLETE"),
        "{text}"
    );
    git(&root, &["checkout", "-q", "--", "."]);
    // Barrel change: a retargeted binding in an unchanged consumer.
    edit(
        &root,
        "src/lib/index.ts",
        "from './format'",
        "from '../legacy/format'",
    );
    let report = agrees(&mut server, &root, json!({}), &[]);
    assert!(
        report["rebound"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["effect"] == "retargeted")
    );
    git(&root, &["checkout", "-q", "--", "."]);
    // Path-alias change: bindings lost, never presented as confirmed breaks.
    edit(&root, "tsconfig.json", "./src/*", "./src/components/*");
    let report = agrees(&mut server, &root, json!({}), &[]);
    assert_eq!(report["resolution_changed"], true);
    assert!(
        report["rebound"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["effect"] == "lost")
    );
    let text = server.ok(json!({"detail": true}));
    assert!(
        text.contains("lost = no longer confirmed, not necessarily broken"),
        "{text}"
    );
    git(&root, &["checkout", "-q", "--", "."]);
    // Committed file rename.
    git(&root, &["mv", "src/lib/math.ts", "src/lib/arith.ts"]);
    git(&root, &["commit", "-q", "-m", "rename"]);
    let report = agrees(
        &mut server,
        &root,
        json!({"base":"HEAD~1","head":"HEAD"}),
        &["--base", "HEAD~1", "--head", "HEAD"],
    );
    assert_eq!(report["files"][0]["status"], "renamed");
}

#[test]
fn bounded_deterministic_and_valid_at_small_budgets() {
    let (_temp, root) = repository();
    edit(&root, "src/lib/math.ts", "a + b", "b + a");
    let mut server = impact_only(&root);
    let first = server.ok(json!({"detail": true, "depth": 3}));
    assert_eq!(
        first,
        server.ok(json!({"detail": true, "depth": 3})),
        "non-deterministic text"
    );
    for budget in [2000, 2500, 4000] {
        let text =
            server.ok(json!({"format":"json","max_bytes":budget,"max_results":1,"detail":true}));
        assert!(text.len() <= budget);
        let report: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(report["complete"], false);
        assert_eq!(report["truncated"], true);
        let agent = server.ok(json!({"max_bytes":budget,"detail":true,"depth":3}));
        assert!(agent.len() <= budget);
    }
}

#[test]
fn invalid_arguments_and_revisions_are_refused() {
    let (_temp, root) = repository();
    let mut server = impact_only(&root);
    for (arguments, code) in [
        (json!({"base":"--output=/tmp/x"}), "invalid_revision"),
        (json!({"base":"no-such-branch"}), "invalid_revision"),
        (json!({"head":"HEAD HEAD"}), "invalid_revision"),
        (json!({"base":""}), "invalid_revision"),
        (json!({"base":"a".repeat(300)}), "invalid_revision"),
        (json!({"head":"HEAD:src/lib/format.ts"}), "invalid_revision"),
        (json!({"depth":0}), "invalid_arguments"),
        (json!({"depth":5}), "invalid_arguments"),
        (json!({"max_results":0}), "invalid_arguments"),
        (json!({"max_bytes":1000}), "invalid_arguments"),
        (json!({"max_bytes":40000}), "invalid_arguments"),
        (json!({"cache_dir":"/tmp"}), "invalid_arguments"),
        (json!({"path":"src"}), "invalid_arguments"),
        (json!({"format":"text"}), "invalid_arguments"),
        (json!({"root":"/"}), "root_not_allowed"),
        (json!({"root":"../.."}), "root_not_allowed"),
    ] {
        assert_eq!(server.error_code(arguments.clone()), code, "{arguments}");
    }
}

#[test]
fn configured_root_boundaries_and_read_only_behaviour() {
    // Not a Git repository.
    let plain = TempDir::new().unwrap();
    let plain_root = plain.path().canonicalize().unwrap();
    assert_eq!(
        impact_only(&plain_root).error_code(json!({})),
        "not_a_git_repository"
    );
    // A configured root inside a larger repository is not analysed as part of it.
    let (_temp, root) = repository();
    // Discovery never looks above the configured root, so the enclosing
    // repository is not found at all.
    let nested = root.join("src");
    let mut server = impact_only(&nested);
    let (text, error) = server.call(json!({}));
    assert!(
        error && text.starts_with("error[not_a_git_repository]"),
        "{text}"
    );
    assert!(text.contains("work-tree top level"), "{text}");
    // A directory below a repository that is not itself one: discovery stops at the root.
    let outer = TempDir::new().unwrap();
    git(outer.path(), &["init", "-q"]);
    let inner = outer.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    assert_eq!(
        impact_only(&inner.canonicalize().unwrap()).error_code(json!({})),
        "not_a_git_repository"
    );
    // Traps: none may run; repository state stays byte-identical.
    let marker = |name: &str| root.join(format!("RAN-{name}"));
    let hooks = root.join(".git/hooks");
    for hook in [
        "post-checkout",
        "post-index-change",
        "reference-transaction",
    ] {
        let path = hooks.join(hook);
        std::fs::write(
            &path,
            format!("#!/bin/sh\ntouch '{}'\n", marker(hook).display()),
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
    git(
        &root,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let filter = format!("sh -c 'touch {}; cat'", marker("filter").display());
    git(&root, &["config", "filter.evil.clean", &filter]);
    git(&root, &["config", "diff.external", &filter]);
    std::fs::write(root.join(".gitattributes"), "*.ts filter=evil\n").unwrap();
    edit(&root, "src/lib/format.ts", "toFixed(2)", "toFixed(3)");
    let index = std::fs::read(root.join(".git/index")).unwrap();
    let head = std::fs::read(root.join(".git/HEAD")).unwrap();
    let refs = git(&root, &["for-each-ref"]);
    let source = std::fs::read(root.join("src/lib/format.ts")).unwrap();
    let mut server = impact_only(&root);
    let report = server.json(json!({}));
    let again = server.json(json!({}));
    assert_eq!(
        again["base"]["index"], "cache",
        "warm request rebuilt the base snapshot"
    );
    assert!(
        serde_json::to_string(&report["limitations"])
            .unwrap()
            .contains("content filter attribute")
    );
    assert!(
        std::fs::read(root.join(".git/index")).unwrap() == index,
        ".git/index modified"
    );
    assert_eq!(std::fs::read(root.join(".git/HEAD")).unwrap(), head);
    assert_eq!(git(&root, &["for-each-ref"]), refs);
    assert_eq!(
        std::fs::read(root.join("src/lib/format.ts")).unwrap(),
        source
    );
    assert!(
        !root.join(".code-intel/index.db").exists(),
        "impact created a user index"
    );
    let ran: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("RAN-"))
        .collect();
    assert!(ran.is_empty(), "repository code ran: {ran:?}");
}

#[test]
fn warm_requests_reuse_snapshots_and_see_new_edits() {
    let (_temp, root) = repository();
    let mut server = impact_only(&root);
    edit(&root, "src/lib/format.ts", "toFixed(2)", "toFixed(3)");
    let cold = server.json(json!({}));
    assert_eq!(cold["base"]["index"], "built");
    let warm = server.json(json!({}));
    assert_eq!(warm["base"]["index"], "cache");
    assert_eq!(warm["head"]["files_reparsed"], 0);
    // A further working-tree edit is seen on the next request.
    edit(&root, "src/lib/math.ts", "a + b", "b + a");
    let changed = server.json(json!({}));
    assert_eq!(changed["declarations"].as_array().unwrap().len(), 2);
    assert_eq!(changed["head"]["files_reparsed"], 1);
    // Commit-to-commit selects the commits' snapshots.
    git(&root, &["commit", "-q", "-am", "edits"]);
    let committed = server.json(json!({"base":"HEAD~1","head":"HEAD"}));
    assert_eq!(committed["declarations"].as_array().unwrap().len(), 2);
    assert_eq!(committed["head"]["kind"], "commit");
    let clean = server.json(json!({}));
    assert!(clean["files"].as_array().unwrap().is_empty());
}
