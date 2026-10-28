//! Next-hop line: exact identity, availability and leftover budget (Phase 5.6), and
//! per-consumer references when the printed walk would stop at a limit (Phase 5.9).
use super::{Server, cli};
use serde_json::{Value, json};
use std::{fmt::Write, fs};
use tempfile::TempDir;

const HINT: &str = "next hop: atlas_dependencies ";
const REFERENCES_HINT: &str = "next hop: atlas_references ";
const REFERENCES_ONLY: [&str; 2] = ["--tools", "atlas_context,atlas_references"];

fn chain(callers: usize) -> TempDir {
    let temp = TempDir::new().unwrap();
    let files = [
        (
            "leaf.ts",
            "export function calc(){return 1;}\nexport function quiet(){return 2;}\n".to_owned(),
        ),
        (
            "user.ts",
            "import {calc} from './leaf';\nexport function user(){return calc();}\n".to_owned(),
        ),
        (
            "top.ts",
            "import {user} from './user';\nexport function top(){return user();}\n".to_owned(),
        ),
        (
            "leaf.test.ts",
            "import {calc, quiet} from './leaf';\ncalc();\nquiet();\n".to_owned(),
        ),
        (
            "many.ts",
            (0..callers).fold(
                "import {calc} from './leaf';\n".to_owned(),
                |mut text, n| {
                    writeln!(text, "export function caller{n}(){{return calc();}}").unwrap();
                    text
                },
            ),
        ),
    ];
    for (path, text) in files {
        fs::write(temp.path().join(path), text).unwrap();
    }
    cli(temp.path(), &["index", "."]);
    temp
}

fn hint(text: &str) -> Option<Value> {
    hint_of(text, HINT)
}

fn hint_of(text: &str, prefix: &str) -> Option<Value> {
    let line = text.lines().find(|line| line.starts_with(prefix))?;
    let mut stream = serde_json::Deserializer::from_str(&line[prefix.len()..]).into_iter::<Value>();
    Some(stream.next().unwrap().unwrap())
}

/// `calc` has one consumer, `hub`, which has `callers` consumers of its own.
fn hub(callers: usize) -> TempDir {
    let temp = TempDir::new().unwrap();
    let many = (0..callers).fold("import {hub} from './hub';\n".to_owned(), |mut text, n| {
        writeln!(text, "export function caller{n}(){{return hub();}}").unwrap();
        text
    });
    for (path, text) in [
        ("leaf.ts", "export function calc(){return 1;}\n".to_owned()),
        (
            "hub.ts",
            "import {calc} from './leaf';\nexport function hub(){return calc();}\n".to_owned(),
        ),
        ("many.ts", many),
    ] {
        fs::write(temp.path().join(path), text).unwrap();
    }
    cli(temp.path(), &["index", "."]);
    temp
}

/// The response without the next-hop line or the byte count it changes.
fn without_hint(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|line| !line.starts_with("next hop: "))
        .map(str::to_owned)
        .collect();
    if let Some(first) = lines.first_mut() {
        if let Some(position) = first.rfind(" · ") {
            first.truncate(position);
        }
    }
    lines
}

fn references(server: &mut Server, name: &str, budget: usize) -> String {
    server.ok(
        "atlas_references",
        json!({"name":name,"path":"leaf.ts","max_bytes":budget}),
    )
}

#[test]
fn next_hop_names_the_exact_declaration_and_its_request_traverses_it() {
    let temp = chain(2);
    let mut server = Server::ready(&[temp.path()]);
    let text = references(&mut server, "calc", 4000);
    let request = hint(&text).expect("a consumer declaration is listed");
    let view = server.json("atlas_references", json!({"name":"calc","path":"leaf.ts"}));
    assert_eq!(request["id"], view["symbol"]["id"]);
    assert_eq!(request["direction"], "upstream");
    assert_eq!(request["depth"], 2);
    assert_eq!(request["max_bytes"], 8000);
    assert!(view.get("next_hop").is_none(), "JSON views carry no hint");
    let graph = server.json("atlas_dependencies", request);
    let nodes: Vec<(String, u64)> = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| {
            (
                node["name"].as_str().unwrap().to_owned(),
                node["depth"].as_u64().unwrap(),
            )
        })
        .collect();
    for expected in [("user", 1), ("caller0", 1), ("caller1", 1), ("top", 2)] {
        assert!(
            nodes.contains(&(expected.0.to_owned(), expected.1)),
            "{expected:?} in {nodes:?}"
        );
    }
    // JSON is byte-identical whether or not the dependencies tool is offered.
    let mut plain = Server::ready_with(&[temp.path()], &REFERENCES_ONLY);
    for name in ["calc", "quiet"] {
        let args = json!({"name":name,"path":"leaf.ts","max_bytes":4000});
        assert_eq!(
            server.json("atlas_references", args.clone()),
            plain.json("atlas_references", args)
        );
    }
}

#[test]
fn next_hop_needs_the_dependencies_tool_and_a_confirmed_consumer_declaration() {
    let temp = chain(2);
    let mut plain = Server::ready_with(&[temp.path()], &REFERENCES_ONLY);
    assert!(hint(&references(&mut plain, "calc", 4000)).is_none());
    let mut server = Server::ready(&[temp.path()]);
    // Only module-scope (test callback) uses: no declaration to walk from.
    assert!(hint(&references(&mut server, "quiet", 4000)).is_none());
    // Import/export rows alone are not consumers.
    let imports = server.ok(
        "atlas_references",
        json!({"name":"calc","path":"leaf.ts","kinds":["IMPORTS","EXPORTS"]}),
    );
    assert!(hint(&imports).is_none(), "{imports}");
    let calls_only = server.ok(
        "atlas_references",
        json!({"name":"calc","path":"leaf.ts","kinds":["CALLS"],"include_unresolved":false}),
    );
    assert!(hint(&calls_only).is_some());
}

#[test]
fn next_hop_uses_leftover_budget_and_never_displaces_sites() {
    let temp = chain(60);
    let mut server = Server::ready(&[temp.path()]);
    let mut plain = Server::ready_with(&[temp.path()], &REFERENCES_ONLY);
    let (mut shown, mut withheld) = (0, 0);
    for budget in (600..=8000).step_by(200).chain([16000, 32000]) {
        let hinted = references(&mut server, "calc", budget);
        let bare = references(&mut plain, "calc", budget);
        assert!(hinted.len() <= budget, "{budget}: {} bytes", hinted.len());
        assert_eq!(without_hint(&hinted), without_hint(&bare), "{budget}");
        if hinted.contains("\nnext hop: ") {
            shown += 1;
        } else {
            assert_eq!(hinted, bare);
            withheld += 1;
        }
    }
    assert!(
        shown > 0 && withheld > 0,
        "{shown} shown, {withheld} withheld"
    );
}

#[test]
fn next_hop_suggests_per_consumer_references_when_the_walk_would_stop_at_a_limit() {
    let walk = |server: &mut Server, id: &Value| {
        server.json(
            "atlas_dependencies",
            json!({"id":id,"direction":"upstream","depth":2,"max_bytes":8000}),
        )
    };
    // Few direct consumers, one of them a hub: the buildPath -> useNavigation shape.
    for temp in [hub(80), chain(60)] {
        let mut server = Server::ready(&[temp.path()]);
        let text = references(&mut server, "calc", 32000);
        assert!(hint(&text).is_none(), "{text}");
        let request = hint_of(&text, REFERENCES_HINT).expect("per-consumer references are offered");
        let view = server.json("atlas_references", json!({"name":"calc","path":"leaf.ts"}));
        if view["truncated"] == true {
            assert!(text.contains("recover omitted callers here before claiming completeness"));
        }
        let consumers: Vec<&Value> = view["resolved"]
            .as_array()
            .unwrap()
            .iter()
            .map(|site| &site["source_id"])
            .collect();
        assert!(
            consumers.contains(&&request["id"]),
            "{request} in {consumers:?}"
        );
        assert_eq!(
            request.as_object().unwrap().len(),
            1,
            "only an id: {request}"
        );
        // The prediction is exact: the walk the old line printed reports lower bounds.
        let graph = walk(&mut server, &view["symbol"]["id"]);
        assert_eq!(graph["counts_exact"], false, "{graph}");
        assert_eq!(graph["limit_reached"], true);
        let next = server.ok("atlas_references", request);
        assert!(next.starts_with("atlas references "), "{next}");
    }
    // A walk that completes keeps the dependencies line, and it reports exact counts.
    let temp = chain(2);
    let mut server = Server::ready(&[temp.path()]);
    let text = references(&mut server, "calc", 4000);
    assert!(hint_of(&text, REFERENCES_HINT).is_none(), "{text}");
    let request = hint(&text).expect("the walk completes");
    assert_eq!(walk(&mut server, &request["id"])["counts_exact"], true);
}

#[test]
fn next_hop_checks_edge_limits_even_with_few_distinct_consumers() {
    // Repeated sites all come from one declaration. Node count alone cannot
    // predict completeness: PER_NODE applies to edges, including repeated sites.
    let temp = hub(0);
    let calls = "calc();".repeat(140);
    fs::write(
        temp.path().join("hub.ts"),
        format!("import {{calc}} from './leaf';\nexport function hub(){{{calls}}}\n"),
    )
    .unwrap();
    cli(temp.path(), &["index", "."]);
    let mut server = Server::ready(&[temp.path()]);
    let text = server.ok(
        "atlas_references",
        json!({"name":"calc","path":"leaf.ts","kinds":["CALLS"],"max_bytes":32000}),
    );
    assert!(hint(&text).is_none());
    assert!(hint_of(&text, REFERENCES_HINT).is_some(), "{text}");
    assert!(text.contains("recover omitted callers here before claiming completeness"));
    let graph = server.json(
        "atlas_dependencies",
        json!({"name":"calc","path":"leaf.ts","direction":"upstream","depth":2,"max_bytes":32000}),
    );
    assert_eq!(graph["counts_exact"], false);
    assert_eq!(graph["limit_reached"], true);
    assert!(graph["nodes"].as_array().unwrap().len() < 5);
}

#[test]
fn next_hop_handles_cycles_and_repeated_consumers_without_inventing_nodes() {
    let temp = hub(0);
    fs::write(
        temp.path().join("leaf.ts"),
        "import {hub} from './hub';\nexport function calc(){return hub();}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("hub.ts"),
        "import {calc} from './leaf';\nexport function hub(){calc(); return calc();}\n",
    )
    .unwrap();
    cli(temp.path(), &["index", "."]);
    let mut server = Server::ready(&[temp.path()]);
    let text = references(&mut server, "calc", 4000);
    let request = hint(&text).expect("a bounded two-node cycle completes");
    let graph = server.json("atlas_dependencies", request);
    assert_eq!(graph["counts_exact"], true);
    assert_eq!(graph["limit_reached"], false);
    let nodes = graph["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2, "{graph}");
    assert!(
        nodes
            .iter()
            .all(|n| n["name"] == "calc" || n["name"] == "hub")
    );
}
