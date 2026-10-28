//! Count reuse, binding-backed aliases and selected-source scope boundaries.
use super::{Server, cli};
use serde_json::json;
use std::{fmt::Write, fs};
use tempfile::TempDir;

#[test]
fn counts_remain_exact_on_both_sides_of_the_unconfirmed_cap() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("leaf.ts"),
        "export function calc(){return 1;}",
    )
    .unwrap();
    fs::write(
        temp.path().join("user.ts"),
        "import {calc} from './leaf'; export function user(){return calc();}",
    )
    .unwrap();
    let mut server = Server::ready(&[temp.path()]);
    for count in [0, 1, 64, 65] {
        let mut text = String::new();
        for n in 0..count {
            writeln!(
                text,
                "export function uncertain{n}(object:any){{return object.calc();}}"
            )
            .unwrap();
        }
        fs::write(temp.path().join("unknown.ts"), text).unwrap();
        cli(temp.path(), &["index", "."]);
        let before = fs::read(temp.path().join(".code-intel/index.db")).unwrap();
        let args = json!({"name":"calc","path":"leaf.ts","kinds":["REFERENCES"],"limit":500,"max_bytes":32000,"format":"json"});
        let result = server.json("atlas_references", args.clone());
        assert_eq!(result["available_resolved"], 1);
        assert_eq!(result["available_unresolved"], count);
        assert_eq!(result["counts_exact"], true);
        assert_eq!(result["limit_reached"], count > 64);
        let returned = result["resolved"].as_array().unwrap().len()
            + result["unresolved"].as_array().unwrap().len();
        assert_eq!(
            result["omitted"].as_u64().unwrap() + u64::try_from(returned).unwrap(),
            u64::try_from(count + 1).unwrap()
        );
        assert_eq!(result, server.json("atlas_references", args));
        assert_eq!(
            fs::read(temp.path().join(".code-intel/index.db")).unwrap(),
            before
        );
    }
}

#[test]
fn alias_candidates_include_late_module_sites_and_keep_multiple_targets_explicit() {
    let temp = TempDir::new().unwrap();
    for (path, name) in [("one.ts", "SvgOne"), ("two.ts", "SvgTwo")] {
        fs::write(
            temp.path().join(path),
            format!("export default function {name}(){{return 1;}}"),
        )
        .unwrap();
    }
    // The source-ID file preview contains only the first 12 lines. Prefiltering
    // resolved edges by that preview's FTS terms would lose these real bindings.
    for (path, module) in [("a.ts", "one"), ("b.ts", "two")] {
        fs::write(
            temp.path().join(path),
            format!(
                "{}import Logo from './{module}';\n",
                "// padding\n".repeat(40)
            ),
        )
        .unwrap();
    }
    cli(temp.path(), &["index", "."]);
    let database = rusqlite::Connection::open_with_flags(
        temp.path().join(".code-intel/index.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let preview_hits: i64 = database.query_row(
        "SELECT count(*) FROM search s JOIN semantic_edges e ON e.source_id=s.id WHERE search MATCH '\"Logo\"' AND json_extract(e.data,'$.status')='resolved' AND json_extract(e.data,'$.target_text')='Logo'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        preview_hits, 0,
        "preview FTS must not gate real alias bindings"
    );
    drop(database);
    let before = fs::read(temp.path().join(".code-intel/index.db")).unwrap();
    let mut server = Server::ready(&[temp.path()]);
    let (text, error) = server.call("atlas_references", json!({"name":"Logo"}));
    assert!(error && text.starts_with("error[symbol_not_found]"));
    assert!(text.contains("SvgOne") && text.contains("SvgTwo"));
    let (filtered, error) = server.call("atlas_references", json!({"name":"Logo","path":"one.ts"}));
    assert!(error && filtered.contains("SvgOne") && !filtered.contains("SvgTwo"));
    assert_eq!(
        fs::read(temp.path().join(".code-intel/index.db")).unwrap(),
        before
    );
}

#[test]
fn moved_repository_requires_explicit_index_repair_and_never_claims_current_source() {
    let original = TempDir::new().unwrap();
    let root = original.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("leaf.ts"), "export function calc(){return 1;}").unwrap();
    cli(&root, &["index", "."]);
    let bytes = fs::read(root.join(".code-intel/index.db")).unwrap();
    let mut server = Server::ready(&[&root]);
    let destination = TempDir::new().unwrap();
    let moved = destination.path().join("moved");
    fs::rename(&root, &moved).unwrap();
    assert_eq!(
        server.error_code("atlas_symbol", json!({"name":"calc"})),
        "root_not_allowed"
    );
    let mut restarted = Server::ready(&[&moved]);
    assert_eq!(
        restarted.error_code("atlas_symbol", json!({"name":"calc"})),
        "index_root_mismatch"
    );
    assert_eq!(fs::read(moved.join(".code-intel/index.db")).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn ignore_changes_and_parent_symlinks_do_not_imply_graph_freshness() {
    let temp = TempDir::new().unwrap();
    fs::create_dir(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("src/leaf.ts"),
        "export function calc(){return 'indexed';}",
    )
    .unwrap();
    cli(temp.path(), &["index", "."]);
    let before = fs::read(temp.path().join(".code-intel/index.db")).unwrap();
    let mut server = Server::ready(&[temp.path()]);
    fs::write(temp.path().join(".gitignore"), "src/leaf.ts\n").unwrap();
    let value = server.json("atlas_symbol", json!({"name":"calc"}));
    assert_eq!(value["navigation"]["current_source"], "matched_at_read");
    assert_eq!(
        value["navigation"]["graph_freshness"],
        "unverified_other_files_and_resolution"
    );
    assert_eq!(server.json("atlas_status", json!({}))["fresh"], false);
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("leaf.ts"), "OUTSIDE_MUST_NOT_BE_READ").unwrap();
    fs::rename(temp.path().join("src"), temp.path().join("old-src")).unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join("src")).unwrap();
    let value = server.json("atlas_symbol", json!({"name":"calc"}));
    assert_eq!(value["navigation"]["current_source"], "unverified");
    assert!(!value.to_string().contains("OUTSIDE_MUST_NOT_BE_READ"));
    assert_eq!(
        fs::read(temp.path().join(".code-intel/index.db")).unwrap(),
        before
    );
}
