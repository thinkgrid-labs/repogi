//! MCP stdio server: JSON-RPC framing, lifecycle, tools, confinement and budgets.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");
const TIMEOUT: Duration = Duration::from_secs(20);

#[path = "mcp/signal_regressions.rs"]
mod signal_regressions;

#[path = "mcp/efficiency_regressions.rs"]
mod efficiency_regressions;

#[path = "mcp/routing_regressions.rs"]
mod routing_regressions;

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl Server {
    fn start(roots: &[&Path]) -> Self {
        Self::start_with(roots, &[])
    }

    fn start_with(roots: &[&Path], options: &[&str]) -> Self {
        let mut command = Command::new(BINARY);
        command.arg("mcp").args(options);
        for root in roots {
            command.arg("--root").arg(root);
        }
        let mut child = command
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
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn ready(roots: &[&Path]) -> Self {
        Self::ready_with(roots, &[])
    }

    fn ready_with(roots: &[&Path], options: &[&str]) -> Self {
        let mut server = Self::start_with(roots, options);
        let result = server.request(
            0,
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}),
        );
        assert_eq!(result["result"]["protocolVersion"], "2025-06-18");
        server.raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        server
    }

    fn raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn next(&self) -> Value {
        let line = self.lines.recv_timeout(TIMEOUT).expect("server response");
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("non-JSON stdout {line:?}: {error}"))
    }

    fn quiet(&self, wait: Duration) -> bool {
        self.lines.recv_timeout(wait).is_err()
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        let mut message = serde_json::Map::new();
        message.insert("jsonrpc".into(), json!("2.0"));
        message.insert("id".into(), json!(id));
        message.insert("method".into(), json!(method));
        message.insert("params".into(), params);
        self.raw(&Value::Object(message).to_string());
        let response = self.next();
        assert_eq!(response["id"], id, "{response}");
        assert_eq!(response["jsonrpc"], "2.0");
        response
    }

    /// Tool text and isError for a tools/call.
    fn call(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let mut params = serde_json::Map::new();
        params.insert("name".into(), json!(name));
        params.insert("arguments".into(), arguments);
        let response = self.request(7, "tools/call", Value::Object(params));
        let result = &response["result"];
        let content = result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        (
            content[0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap(),
        )
    }

    fn ok(&mut self, name: &str, arguments: Value) -> String {
        let (text, error) = self.call(name, arguments);
        assert!(!error, "{name}: {text}");
        text
    }

    fn json(&mut self, name: &str, mut arguments: Value) -> Value {
        if name != "atlas_status" {
            arguments["format"] = json!("json");
        }
        serde_json::from_str(&self.ok(name, arguments)).unwrap()
    }

    fn error_code(&mut self, name: &str, arguments: Value) -> String {
        let (text, error) = self.call(name, arguments);
        assert!(error, "{name} should fail: {text}");
        text.strip_prefix("error[")
            .and_then(|rest| rest.split(']').next())
            .unwrap_or_else(|| panic!("unlabelled error {text}"))
            .to_owned()
    }

    fn close(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(start.elapsed() < TIMEOUT, "server did not exit after EOF");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

fn cli(root: &Path, args: &[&str]) -> String {
    let result = Command::new(BINARY)
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

fn fixture(name: &str) -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name),
        temp.path(),
    );
    cli(temp.path(), &["index", "."]);
    let root = temp.path().canonicalize().unwrap();
    (temp, root)
}

#[test]
fn initialize_negotiates_versions_and_lists_read_only_tools() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::start(&[&root]);
    assert!(server.request(1, "ping", json!({}))["result"].is_object());
    let early = server.request(2, "tools/list", json!({}));
    assert_eq!(early["error"]["code"], -32002);
    let init = server.request(
        3,
        "initialize",
        json!({"protocolVersion":"1999-01-01","capabilities":{},"clientInfo":{"name":"test","version":"0"}}),
    );
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert_eq!(result["serverInfo"]["name"], "repogi");
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["instructions"].as_str().unwrap().len() < 600);
    let again = server.request(4, "initialize", json!({"protocolVersion":"2025-06-18"}));
    assert_eq!(again["error"]["code"], -32600);
    let tools = server.request(5, "tools/list", json!({}))["result"]["tools"].clone();
    let names: Vec<_> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "atlas_context",
            "atlas_symbol",
            "atlas_references",
            "atlas_dependencies",
            "atlas_status"
        ]
    );
    for tool in tools.as_array().unwrap() {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["annotations"]["destructiveHint"], false);
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
    // Tool definitions are a fixed per-session cost; keep them compact.
    assert!(
        tools.to_string().len() < 8000,
        "{}",
        tools.to_string().len()
    );
    for version in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
        let mut other = Server::start(&[&root]);
        let response = other.request(1, "initialize", json!({"protocolVersion": version,"capabilities":{},"clientInfo":{"name":"test","version":"0"}}));
        assert_eq!(response["result"]["protocolVersion"], version);
    }
    assert!(server.close().success());
}

#[test]
fn framing_errors_are_reported_and_the_server_survives() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    let cases = [
        ("not json", -32700, Value::Null),
        ("[1,2]", -32600, Value::Null),
        ("42", -32600, Value::Null),
        (r#"{"jsonrpc":"2.0","id":9}"#, -32600, json!(9)),
        (
            r#"{"jsonrpc":"1.0","id":10,"method":"ping"}"#,
            -32600,
            json!(10),
        ),
        (
            r#"{"jsonrpc":"2.0","id":{"x":1},"method":"ping"}"#,
            -32600,
            Value::Null,
        ),
        (
            r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
            -32600,
            Value::Null,
        ),
        (
            r#"{"jsonrpc":"2.0","id":11,"method":"resources/list"}"#,
            -32601,
            json!(11),
        ),
        (
            r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"atlas_missing"}}"#,
            -32602,
            json!(12),
        ),
        (
            r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"atlas_status","arguments":[1]}}"#,
            -32602,
            json!(13),
        ),
        (
            r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{}}"#,
            -32602,
            json!(14),
        ),
    ];
    for (line, code, id) in cases {
        server.raw(line);
        let response = server.next();
        assert_eq!(response["error"]["code"], code, "{line}: {response}");
        assert_eq!(response["id"], id, "{line}");
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );
    }
    // Notifications, responses to nothing and blank lines produce no output.
    server.raw(r#"{"jsonrpc":"2.0","method":"notifications/unknown"}"#);
    server.raw(r#"{"jsonrpc":"2.0","id":99,"result":{}}"#);
    server.raw("");
    server.raw(
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":12345}}"#,
    );
    assert!(server.quiet(Duration::from_millis(300)));
    // Oversized messages are rejected and the rest of the stream is still read.
    let huge = format!(
        r#"{{"jsonrpc":"2.0","id":15,"method":"ping","params":{{"pad":"{}"}}}}"#,
        "x".repeat(4 * 1024 * 1024 + 10)
    );
    server.raw(&huge);
    let response = server.next();
    assert_eq!(response["error"]["code"], -32700);
    assert!(response["id"].is_null());
    server.raw("{\"jsonrpc\":\"2.0\",\"id\":16,\"method\":\"ping\"}\r");
    assert_eq!(server.next()["id"], 16);
    let (text, error) = server.call("atlas_status", json!({"unexpected": true}));
    assert!(
        error && text.starts_with("error[invalid_arguments]"),
        "{text}"
    );
    assert!(server.close().success());
}

#[test]
fn queued_requests_are_answered_before_exit_and_stdout_is_protocol_only() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    for id in 20..30 {
        server.raw(
            &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"atlas_context","arguments":{"task":"entry original"}}})
                .to_string(),
        );
    }
    let mut ids = Vec::new();
    drop(server.stdin.take());
    for _ in 20..30 {
        let response = server.next();
        assert!(response["result"]["content"].is_array(), "{response}");
        ids.push(response["id"].as_i64().unwrap());
    }
    assert_eq!(ids, (20..30).collect::<Vec<_>>());
    assert!(server.close().success());
}

#[test]
fn cancelled_requests_receive_no_response() {
    let (_temp, root) = fixture("real-world/checkgate");
    let mut server = Server::ready(&[&root]);
    // The worker answers in order, so a cancellation for the last queued
    // request is applied long before the worker reaches it.
    let mut batch = String::new();
    for id in 100..130 {
        batch.push_str(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"atlas_context","arguments":{"task":"rule editor variant request flag drawer","retrieval":"semantic","max_bytes":32000}}}).to_string());
        batch.push('\n');
    }
    batch.push_str(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":129,"reason":"test"}}"#);
    batch.push('\n');
    batch.push_str(r#"{"jsonrpc":"2.0","id":130,"method":"ping"}"#);
    server.raw(batch.trim_end());
    let mut seen = Vec::new();
    for _ in 100..130 {
        seen.push(server.next()["id"].as_i64().unwrap());
    }
    assert!(!seen.contains(&129), "{seen:?}");
    assert!(seen.contains(&130));
    assert!(server.quiet(Duration::from_millis(300)));
    // A cancellation for a finished request is ignored.
    server
        .raw(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":100}}"#);
    assert!(server.request(131, "ping", json!({}))["result"].is_object());
}

#[test]
fn context_tool_reuses_cli_retrieval_byte_for_byte() {
    let (_temp, root) = fixture("benchmarks/service");
    let mut server = Server::ready(&[&root]);
    for (retrieval, detail) in [
        ("optimized", "detailed"),
        ("optimized", "overview"),
        ("optimized", "focused"),
        ("semantic", "detailed"),
        ("semantic", "overview"),
        ("lexical", "detailed"),
        ("graph", "detailed"),
    ] {
        let text = server.ok(
            "atlas_context",
            json!({"task":"authenticate user session","retrieval":retrieval,"detail":detail,"max_bytes":3000,"format":"json"}),
        );
        let expected = cli(
            &root,
            &[
                "context",
                "authenticate user session",
                "--retrieval",
                retrieval,
                "--detail",
                detail,
                "--max-tokens",
                "3000",
            ],
        );
        assert_eq!(text, expected, "{retrieval}/{detail}");
    }
    // The agent format is budgeted by the same packer and keeps follow-up IDs.
    for detail in ["overview", "focused", "detailed"] {
        for max_bytes in [400, 1200, 4000] {
            let text = server.ok(
                "atlas_context",
                json!({"task":"authenticate user session","detail":detail,"max_bytes":max_bytes}),
            );
            assert!(
                text.len() <= max_bytes,
                "{detail}/{max_bytes}: {}",
                text.len()
            );
            assert!(
                text.contains(&format!("{}/{max_bytes} bytes", text.len())),
                "{text}"
            );
            assert!(
                text.starts_with(&format!("atlas context r1 · {detail} · ")),
                "{text}"
            );
        }
    }
    let overview = server.ok(
        "atlas_context",
        json!({"task":"authenticate user session","detail":"overview"}),
    );
    let id = overview
        .lines()
        .find(|line| line.starts_with("## ["))
        .and_then(|line| {
            // The ID is the field just before the `L<lines>` field.
            let parts: Vec<&str> = line.split(" · ").collect();
            let position = parts.iter().position(|part| {
                part.strip_prefix('L')
                    .is_some_and(|rest| rest.split('-').all(|n| n.parse::<usize>().is_ok()))
            })?;
            parts[position - 1].rsplit("] ").next()
        })
        .unwrap()
        .to_owned();
    let selected = server.ok(
        "atlas_context",
        json!({"task":"authenticate","select":[id.clone()],"detail":"detailed"}),
    );
    assert!(selected.contains(&format!(" · {id} · ")), "{selected}");
    assert_eq!(
        server.error_code(
            "atlas_context",
            json!({"task":"x","select":["function:nope:1:2"]})
        ),
        "retrieval_failed"
    );
    assert_eq!(
        server.error_code("atlas_context", json!({"task":"x","max_bytes":100})),
        "invalid_arguments"
    );
    assert_eq!(
        server.error_code("atlas_context", json!({"task":"x","max_bytes":32001})),
        "invalid_arguments"
    );
    assert_eq!(
        server.error_code("atlas_context", json!({"task":"   "})),
        "retrieval_failed"
    );
    assert_eq!(
        server.error_code("atlas_context", json!({"task":"x","detail":"everything"})),
        "invalid_arguments"
    );
}

#[test]
fn symbol_lookup_returns_source_signatures_and_explicit_errors() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    let symbol = server.json("atlas_symbol", json!({"name":"create"}));
    let items = symbol["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "Base.create");
    assert!(
        items[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("static create()")
    );
    assert_eq!(items[0]["provenance"]["source_path"], "leaf.ts");
    let id = items[0]["id"].as_str().unwrap().to_owned();
    let focused = server.ok("atlas_symbol", json!({"id": id, "detail":"focused"}));
    assert!(
        focused.contains("static create(): string\n") && !focused.contains("return original"),
        "{focused}"
    );
    assert_eq!(
        server.error_code("atlas_symbol", json!({"name":"doesNotExist"})),
        "symbol_not_found"
    );
    assert_eq!(
        server.error_code("atlas_symbol", json!({"id":"function:leaf.ts:0:1"})),
        "symbol_not_found"
    );
    assert_eq!(
        server.error_code("atlas_symbol", json!({})),
        "invalid_arguments"
    );
    // `original` is declared in leaf.ts and other.ts: references need one target.
    let ambiguous = server
        .call("atlas_references", json!({"name":"original"}))
        .0;
    assert!(
        ambiguous.starts_with("error[ambiguous_symbol]") && ambiguous.contains("other.ts"),
        "{ambiguous}"
    );
}

#[test]
fn references_match_the_persisted_graph_and_separate_unconfirmed_sites() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    let references = server.json(
        "atlas_references",
        json!({"name":"original","path":"leaf.ts","max_bytes":32000}),
    );
    assert_eq!(references["truncated"], false);
    let target = references["symbol"]["id"].as_str().unwrap();
    let graph: Value = serde_json::from_str(&cli(&root, &["graph", "--limit", "10000"])).unwrap();
    let expected: Vec<&Value> = graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|edge| edge["target_id"] == target)
        .collect();
    let resolved = references["resolved"].as_array().unwrap();
    assert_eq!(resolved.len(), expected.len());
    for (site, edge) in resolved.iter().zip(&expected) {
        assert_eq!(site["status"], "resolved");
        assert_eq!(site["kind"], edge["kind"]);
        assert_eq!(site["location"], edge["location"]);
        assert_eq!(site["binding_chain"], edge["evidence"]["binding_chain"]);
        assert_eq!(site["rule"], "unique_static_binding");
    }
    assert!(
        resolved
            .iter()
            .any(|site| site["kind"] == "CALLS" && site["source_name"] == "entry")
    );
    let unresolved = references["unresolved"].as_array().unwrap();
    assert!(!unresolved.is_empty());
    assert!(unresolved.iter().all(|site| site["status"] != "resolved"));
    let calls_only = server.json(
        "atlas_references",
        json!({"name":"original","path":"leaf.ts","kinds":["CALLS"],"include_unresolved":false}),
    );
    assert!(
        calls_only["resolved"]
            .as_array()
            .unwrap()
            .iter()
            .all(|site| site["kind"] == "CALLS")
    );
    assert!(calls_only["unresolved"].as_array().unwrap().is_empty());
    let agent = server.ok(
        "atlas_references",
        json!({"name":"original","path":"leaf.ts"}),
    );
    assert!(
        agent.contains("\nunconfirmed (same name, no resolved target):\n"),
        "{agent}"
    );
}

#[test]
fn references_advertisement_matches_alias_resolution() {
    // The description once said tsconfig paths were unresolved (stale since
    // module resolution landed); an alias import binds through tsconfig.json.
    let (_temp, root) = fixture("affected-tests/repo");
    let mut server = Server::ready(&[&root]);
    let tools = server.request(5, "tools/list", json!({}))["result"]["tools"].clone();
    let description = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "atlas_references")
        .unwrap()["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!description.contains("tsconfig"), "{description}");
    let references = server.json(
        "atlas_references",
        json!({"name":"calc","path":"src/math.ts","max_bytes":16000}),
    );
    let alias = references["resolved"]
        .as_array()
        .unwrap()
        .iter()
        .find(|site| site["location"]["path"] == "tests/alias.test.ts")
        .expect("alias import site resolved");
    assert!(
        alias["binding_chain"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link.as_str().unwrap().contains("tsconfig.json")),
        "{alias}"
    );
}

#[test]
fn dependency_traversal_is_bounded_and_confirmed_only() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    for direction in ["downstream", "upstream", "both"] {
        for depth in 1..=3 {
            let result = server.json(
                "atlas_dependencies",
                json!({"name":"entry","direction":direction,"depth":depth,"include_unresolved":true}),
            );
            let nodes = result["nodes"].as_array().unwrap();
            assert_eq!(nodes[0]["name"], "entry");
            assert_eq!(nodes[0]["depth"], 0);
            assert!(
                nodes
                    .iter()
                    .all(|node| node["depth"].as_u64().unwrap() <= depth)
            );
            for link in result["links"].as_array().unwrap() {
                assert_eq!(link["status"], "resolved");
                assert!(link["target_id"].is_string());
                assert!(!link["binding_chain"].as_array().unwrap().is_empty());
            }
            assert!(
                result["unresolved"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|site| site["status"] != "resolved")
            );
        }
    }
    let downstream = server.json("atlas_dependencies", json!({"name":"entry","depth":2}));
    let names: Vec<_> = downstream["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Base.create") && names.contains(&"original"),
        "{names:?}"
    );
    let upstream = server.json(
        "atlas_dependencies",
        json!({"name":"Base","direction":"upstream"}),
    );
    assert!(
        upstream["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["kind"] == "EXTENDS")
    );
    let bounded = server.json(
        "atlas_dependencies",
        json!({"name":"entry","depth":3,"max_nodes":1}),
    );
    assert_eq!(bounded["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(bounded["limit_reached"], true);
    assert_eq!(bounded["truncated"], true);
    assert_eq!(
        server.error_code("atlas_dependencies", json!({"name":"entry","depth":4})),
        "invalid_arguments"
    );
    assert_eq!(
        server.error_code("atlas_dependencies", json!({"name":"entry","max_nodes":0})),
        "invalid_arguments"
    );
    assert_eq!(
        server.error_code(
            "atlas_dependencies",
            json!({"name":"entry","kinds":["CALL"]})
        ),
        "invalid_arguments"
    );
    assert_eq!(
        server.error_code("atlas_dependencies", json!({"name":"entry","kinds":[]})),
        "invalid_arguments"
    );
}

#[test]
fn every_tool_bounds_its_rendered_output() {
    let (_temp, root) = fixture("real-world/checkgate");
    let mut server = Server::ready(&[&root]);
    let calls = [
        (
            "atlas_context",
            json!({"task":"rule editor variant request"}),
        ),
        ("atlas_symbol", json!({"name":"RuleEditor"})),
        ("atlas_references", json!({"name":"request"})),
        (
            "atlas_dependencies",
            json!({"name":"RuleEditor","direction":"both","depth":3,"include_unresolved":true}),
        ),
    ];
    for (name, arguments) in calls {
        let full = server.ok(name, arguments.clone());
        for format in ["agent", "json"] {
            for max_bytes in [600, 1500, 5000] {
                let mut bounded = arguments.clone();
                bounded["max_bytes"] = json!(max_bytes);
                bounded["format"] = json!(format);
                let (text, error) = server.call(name, bounded);
                if error {
                    assert!(
                        text.starts_with("error[budget_too_small]")
                            || text.starts_with("error[retrieval_failed]"),
                        "{name}: {text}"
                    );
                    continue;
                }
                assert!(
                    text.len() <= max_bytes,
                    "{name}/{format}/{max_bytes}: {}",
                    text.len()
                );
                if format == "agent" && text.len() < full.len() {
                    assert!(
                        text.lines().next().unwrap().contains(" · truncated"),
                        "{name}/{max_bytes}: {text}"
                    );
                }
            }
        }
    }
    let large = server.json(
        "atlas_dependencies",
        json!({"name":"RuleEditor","direction":"both","depth":3,"max_bytes":1600}),
    );
    assert_eq!(large["truncated"], true);
    assert!(large["omitted"].as_u64().unwrap() > 0);
    // Evidence/scope metadata is mandatory; an envelope that no longer fits
    // fails explicitly instead of suppressing its freshness contract.
    assert_eq!(
        server.error_code(
            "atlas_references",
            json!({"name":"request","max_bytes":256})
        ),
        "budget_too_small"
    );
    assert_eq!(
        server.error_code("atlas_references", json!({"name":"request","limit":501})),
        "invalid_arguments"
    );
}

#[test]
fn roots_and_paths_are_confined() {
    let (_temp, root) = fixture("semantics");
    let (_other_temp, other) = fixture("benchmarks/web");
    let outside = TempDir::new().unwrap();
    let mut server = Server::ready(&[&root]);
    for path in [
        "../leaf.ts",
        "/etc/passwd",
        "a/../leaf.ts",
        "./leaf.ts",
        "a\\b.ts",
        "C:/x.ts",
        "a//b.ts",
        "",
        "leaf.ts/",
    ] {
        assert_eq!(
            server.error_code("atlas_symbol", json!({"name":"original","path":path})),
            "invalid_path",
            "{path:?}"
        );
    }
    let other_text = other.to_str().unwrap();
    assert_eq!(
        server.error_code("atlas_status", json!({"root": other_text})),
        "root_not_allowed"
    );
    assert_eq!(
        server.error_code(
            "atlas_context",
            json!({"task":"x","root": outside.path().to_str().unwrap()})
        ),
        "root_not_allowed"
    );
    assert_eq!(
        server.error_code("atlas_status", json!({"root": "."})),
        "root_not_allowed"
    );
    assert_eq!(
        server.error_code(
            "atlas_status",
            json!({"root": format!("{}/../{}", root.display(), "x")})
        ),
        "root_not_allowed"
    );
    let ok = server.json("atlas_status", json!({"root": root.to_str().unwrap()}));
    assert_eq!(ok["root"], root.to_str().unwrap());

    let mut both = Server::ready(&[&root, &other]);
    assert_eq!(
        both.error_code("atlas_context", json!({"task":"filter"})),
        "root_required"
    );
    let web = both.ok(
        "atlas_context",
        json!({"task":"filter products","root": other_text}),
    );
    assert!(web.contains("filterProducts"), "{web}");
    let status = both.json("atlas_status", json!({"root": other_text}));
    assert_eq!(
        status["server"]["configured_roots"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let failed = Command::new(BINARY)
        .args(["mcp", "--root", "/definitely/missing/root"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let error: Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert_eq!(error["error"]["code"], "command_failed");
    assert!(failed.stdout.is_empty());
}

#[test]
fn missing_indexes_are_explicit() {
    let empty = TempDir::new().unwrap();
    fs::write(empty.path().join("a.ts"), "export const a = 1;\n").unwrap();
    let mut server = Server::ready(&[empty.path()]);
    let status = server.json("atlas_status", json!({}));
    assert_eq!(status["index"]["present"], false);
    assert_eq!(status["fresh"], false);
    assert!(status["action"].as_str().unwrap().contains("repogi index"));
    for (name, arguments) in [
        ("atlas_context", json!({"task":"a"})),
        ("atlas_symbol", json!({"name":"a"})),
        ("atlas_references", json!({"name":"a"})),
        ("atlas_dependencies", json!({"name":"a"})),
    ] {
        assert_eq!(server.error_code(name, arguments), "index_missing");
    }
}

#[test]
fn stale_indexes_report_freshness_and_keep_serving_their_snapshot() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    let status = server.json("atlas_status", json!({}));
    assert_eq!(status["fresh"], true);
    assert_eq!(status["index"]["revision"], 1);
    assert_eq!(status["index"]["semantic_graph"], true);
    assert!(
        status["index"]["semantic_edges_by_status"]["resolved"]
            .as_u64()
            .unwrap()
            > 0
    );
    let digest = status["index"]["snapshot_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(digest.len(), 64);
    fs::write(
        root.join("leaf.ts"),
        fs::read_to_string(root.join("leaf.ts")).unwrap() + "// edit\n",
    )
    .unwrap();
    fs::write(root.join("added.ts"), "export const added = 1;\n").unwrap();
    fs::remove_file(root.join("other.ts")).unwrap();
    let stale = server.json("atlas_status", json!({}));
    assert_eq!(stale["fresh"], false);
    assert_eq!(stale["freshness"]["changed"], 1);
    assert_eq!(stale["freshness"]["added"], 1);
    assert_eq!(stale["freshness"]["removed"], 1);
    assert_eq!(
        stale["freshness"]["sample"],
        json!(["added.ts", "leaf.ts", "other.ts"])
    );
    assert!(stale["action"].as_str().unwrap().contains("repogi index"));
    let unchecked = server.json("atlas_status", json!({"check_freshness": false}));
    assert!(unchecked["fresh"].is_null() && unchecked["freshness"].is_null());
    // Tools keep serving the indexed snapshot, labelled with its revision.
    assert!(
        server
            .ok("atlas_symbol", json!({"name":"original","path":"other.ts"}))
            .starts_with("atlas context r1 · ")
    );
    cli(&root, &["index", "."]);
    let rebuilt = server.json("atlas_status", json!({}));
    assert_eq!(rebuilt["fresh"], true);
    assert_eq!(rebuilt["index"]["revision"], 2);
    assert_ne!(rebuilt["index"]["snapshot_digest"], digest);
    assert_eq!(
        server.error_code("atlas_symbol", json!({"name":"original","path":"other.ts"})),
        "path_not_indexed"
    );
}

#[test]
fn foreign_and_schema_one_indexes_are_explicit() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    // A copied index belongs to another root.
    let copy = TempDir::new().unwrap();
    copy_dir(&root, copy.path());
    let mut foreign = Server::ready(&[copy.path()]);
    assert_eq!(
        foreign.error_code("atlas_status", json!({})),
        "index_root_mismatch"
    );
    assert_eq!(
        foreign.error_code("atlas_context", json!({"task":"entry"})),
        "index_root_mismatch"
    );

    // Schema-1 snapshots keep serving the frozen strategies but not the typed graph.
    let database = rusqlite::Connection::open(root.join(".code-intel/index.db")).unwrap();
    database.execute_batch("DROP TRIGGER semantic_search_delete; DROP TABLE semantic_search; DROP TABLE semantic_edges; DROP TABLE semantic_modules; PRAGMA user_version=1;").unwrap();
    drop(database);
    let old = server.json("atlas_status", json!({}));
    assert_eq!(old["index"]["semantic_graph"], false);
    assert_eq!(old["fresh"], false);
    assert!(
        server
            .ok("atlas_context", json!({"task":"entry"}))
            .contains("entry")
    );
    assert_eq!(
        server.error_code(
            "atlas_context",
            json!({"task":"entry","retrieval":"semantic"})
        ),
        "index_outdated"
    );
    assert_eq!(
        server.error_code("atlas_references", json!({"name":"entry"})),
        "index_outdated"
    );
    assert_eq!(
        server.error_code("atlas_dependencies", json!({"name":"entry"})),
        "index_outdated"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_index_state_is_refused() {
    let (_temp, root) = fixture("semantics");
    let elsewhere = TempDir::new().unwrap();
    fs::rename(root.join(".code-intel"), elsewhere.path().join("state")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("state"), root.join(".code-intel")).unwrap();
    let mut server = Server::ready(&[&root]);
    assert_eq!(
        server.error_code("atlas_status", json!({})),
        "state_rejected"
    );
    assert_eq!(
        server.error_code("atlas_context", json!({"task":"entry"})),
        "state_rejected"
    );
}

#[test]
fn responses_are_deterministic_across_processes() {
    let (_temp, root) = fixture("real-world/recached");
    let script = [
        (
            "atlas_context",
            json!({"task":"subscribe to a pubsub channel","detail":"overview"}),
        ),
        (
            "atlas_context",
            json!({"task":"subscribe to a pubsub channel","retrieval":"semantic"}),
        ),
        ("atlas_symbol", json!({"name":"RecachedProvider"})),
        ("atlas_references", json!({"name":"useRecached"})),
        (
            "atlas_dependencies",
            json!({"name":"RecachedProvider","direction":"both","depth":2,"include_unresolved":true}),
        ),
        ("atlas_status", json!({})),
    ];
    let run = || {
        let mut server = Server::ready(&[&root]);
        script
            .iter()
            .map(|(name, arguments)| server.call(name, arguments.clone()).0)
            .collect::<Vec<_>>()
    };
    let first = run();
    assert_eq!(first, run());
    assert!(
        first.iter().all(|text| !text.starts_with("error[")),
        "{first:?}"
    );
}

#[test]
fn protocol_validation_and_cancellation_do_not_corrupt_initialization() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::start(&[&root]);
    let bad = server.request(1, "initialize", json!({"protocolVersion":"2025-11-25"}));
    assert_eq!(bad["error"]["code"], -32602);
    server.raw(&json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}).to_string());
    server.raw(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#);
    assert_eq!(server.next()["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        server.request(3, "ping", json!([1]))["error"]["code"],
        -32602
    );
    for _ in 0..2000 {
        server.raw(r#"{"jsonrpc":"2.0","method":"notifications/irrelevant"}"#);
    }
    assert!(server.request(4, "ping", json!({}))["result"].is_object());
}

#[test]
fn errors_and_status_responses_have_hard_byte_caps() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    let (text, error) = server.call(
        "atlas_symbol",
        json!({"name":"missing".repeat(10000),"max_bytes":256}),
    );
    assert!(error && text.len() <= 256);
    let (text,error) = server.call("atlas_symbol",json!({"id":"function:leaf.ts:0:1","root":"/missing/".to_owned()+&"雪".repeat(20000),"max_bytes":256}));
    assert!(error && text.len() <= 256);
    let (text, error) = server.call(
        "atlas_status",
        json!({"root":"/missing/".to_owned()+&"x".repeat(40000)}),
    );
    assert!(error && text.len() <= 32000);
    let roots = root.join("..").join(root.file_name().unwrap());
    assert_eq!(
        server.error_code("atlas_status", json!({"root":roots})),
        "root_not_allowed"
    );
}

#[test]
fn node_limit_includes_the_starting_symbol() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    let one = server.json(
        "atlas_dependencies",
        json!({"name":"entry","max_nodes":1,"depth":3,"max_bytes":32000}),
    );
    assert_eq!(one["nodes"].as_array().unwrap().len(), 1);
    assert!(one["links"].as_array().unwrap().is_empty());
    assert_eq!(one["limit_reached"], true);
}

#[cfg(unix)]
#[test]
fn roots_redirected_after_startup_are_rejected() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    let elsewhere = TempDir::new().unwrap();
    let moved = elsewhere.path().join("moved");
    fs::rename(&root, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &root).unwrap();
    assert_eq!(
        server.error_code("atlas_context", json!({"task":"entry"})),
        "root_not_allowed"
    );
    assert_eq!(
        server.error_code("atlas_status", json!({})),
        "root_not_allowed"
    );
    fs::remove_file(&root).unwrap();
    fs::rename(moved, &root).unwrap();
}

#[test]
fn cancellation_is_persistent_during_freshness_and_between_sql_queries() {
    let (_temp, root) = fixture("semantics");
    let store = code_intel_store::Store::open_read(&root.join(".code-intel/index.db")).unwrap();
    let hashes = store.hashes().unwrap();
    let interrupt = store.interrupt_handle();
    let visits = std::cell::Cell::new(0);
    let result = code_intel_index::freshness_checked(&root, &hashes, 20, &|| {
        visits.set(visits.get() + 1);
        if visits.get() == 3 {
            interrupt.interrupt();
        }
        store.check_cancelled()
    });
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert_eq!(visits.get(), 3);
    assert!(
        store
            .revision()
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert!(
        code_intel_store::Store::open_read(&root.join(".code-intel/index.db"))
            .unwrap()
            .revision()
            .is_ok()
    );
}

#[test]
fn custom_tool_sets_get_instructions_for_the_listed_tools() {
    let (_temp, root) = fixture("semantics");
    let instructions = |tools: &str| {
        Server::start_with(&[&root], &["--profile", "compact", "--tools", tools])
            .request(
                1,
                "initialize",
                json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}),
            )["result"]["instructions"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let navigate = instructions("atlas_context,atlas_symbol,atlas_references");
    assert!(navigate.contains("atlas_references") && navigate.contains("atlas_context"));
    let lookup = instructions("atlas_symbol");
    assert!(!lookup.contains("atlas_references") && !lookup.contains("atlas_context"));
    assert!(navigate.len() < 600 && lookup.contains("read files directly before editing"));
}

#[test]
fn profiles_select_tools_and_shrink_schemas_without_changing_results() {
    let (_temp, root) = fixture("semantics");
    let (_other_temp, other) = fixture("benchmarks/web");
    let list =
        |server: &mut Server| server.request(5, "tools/list", json!({}))["result"]["tools"].clone();
    let names = |tools: &Value| {
        tools
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let mut full = Server::ready(&[&root]);
    let full_tools = list(&mut full);
    assert_eq!(names(&full_tools).len(), 5);
    // Full schemas retain the complete argument interface, even with one root.
    for tool in full_tools.as_array().unwrap() {
        assert!(
            tool["inputSchema"]["properties"]["root"].is_object(),
            "{tool}"
        );
        assert!(tool["title"].is_string());
    }
    let mut multi = Server::ready(&[&root, &other]);
    for tool in list(&mut multi).as_array().unwrap() {
        assert!(
            tool["inputSchema"]["properties"]["root"].is_object(),
            "{tool}"
        );
    }

    let mut compact = Server::start_with(&[&root], &["--profile", "compact"]);
    let init = compact.request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}),
    );
    let full_init = Server::start(&[&root]).request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}),
    );
    let short = init["result"]["instructions"].as_str().unwrap();
    assert!(short.len() < full_init["result"]["instructions"].as_str().unwrap().len());
    assert!(!short.contains("atlas_references"));
    let compact_tools = list(&mut compact);
    assert_eq!(names(&compact_tools), ["atlas_context", "atlas_symbol"]);
    for tool in compact_tools.as_array().unwrap() {
        let properties = &tool["inputSchema"]["properties"];
        assert!(properties["format"].is_object() && properties.get("root").is_none());
        assert!(tool.get("title").is_none());
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
    }
    assert!(
        compact_tools.to_string().len() * 100 <= full_tools.to_string().len() * 35,
        "compact {} vs full {}",
        compact_tools.to_string().len(),
        full_tools.to_string().len()
    );
    // Unlisted tools are unknown; listed tools return exactly what the full profile returns.
    compact.raw(&json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"atlas_references","arguments":{"name":"original"}}}).to_string());
    assert_eq!(compact.next()["error"]["code"], -32602);
    for (name, arguments) in [
        (
            "atlas_context",
            json!({"task":"entry original","detail":"overview"}),
        ),
        (
            "atlas_context",
            json!({"task":"entry original","format":"json"}),
        ),
        ("atlas_symbol", json!({"name":"create"})),
    ] {
        assert_eq!(
            compact.ok(name, arguments.clone()),
            full.ok(name, arguments)
        );
    }

    let mut chosen = Server::ready_with(&[&root], &["--tools", "atlas_references,atlas_context"]);
    assert_eq!(
        names(&list(&mut chosen)),
        ["atlas_context", "atlas_references"]
    );
    let failed = Command::new(BINARY)
        .args(["mcp", "--tools", "atlas_everything"])
        .arg("--root")
        .arg(&root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("unknown tool atlas_everything"));
}

#[test]
fn status_reports_module_resolution_changes_as_stale() {
    let (_temp, root) = fixture("semantics");
    let mut server = Server::ready(&[&root]);
    let status = server.json("atlas_status", json!({}));
    assert_eq!(status["fresh"], true);
    assert_eq!(status["freshness"]["resolution_changed"], false);
    // No source changes: only the project's path mapping appears.
    fs::write(
        root.join("tsconfig.json"),
        r#"{ "compilerOptions": { "paths": { "@/*": ["./*"] } } }"#,
    )
    .unwrap();
    let stale = server.json("atlas_status", json!({}));
    assert_eq!(stale["fresh"], false);
    assert_eq!(stale["freshness"]["resolution_changed"], true);
    assert_eq!(stale["freshness"]["changed"], 0);
    cli(&root, &["index", "."]);
    assert_eq!(server.json("atlas_status", json!({}))["fresh"], true);
}
