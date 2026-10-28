//! Source-derived navigation regressions, including requests observed in 5.2.
use super::{Server, cli};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
use tempfile::TempDir;

fn sources(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    for (path, text) in files {
        let file = temp.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    cli(temp.path(), &["index", "."]);
    let root = temp.path().canonicalize().unwrap();
    (temp, root)
}

#[test]
fn source_checks_are_scoped_current_and_never_reindex() {
    let (_temp, root) = sources(&[
        ("calc.ts", "export function calc(n:number){ return n+1; }"),
        (
            "user.ts",
            "import {calc} from './calc'; export function user(){ return calc(1); }",
        ),
    ]);
    let mut server = Server::ready(&[&root]);
    let before = fs::read(root.join(".code-intel/index.db")).unwrap();
    let selected = json!({"name":"calc","path":"calc.ts","format":"json","max_bytes":8000});
    let fresh = server.json("atlas_symbol", selected.clone());
    assert_eq!(fresh["navigation"]["current_source"], "matched_at_read");
    assert_eq!(
        fresh["navigation"]["graph_freshness"],
        "unverified_other_files_and_resolution"
    );
    fs::write(
        root.join("calc.ts"),
        "export function calc(n:number){ return n+7; }",
    )
    .unwrap();
    let stale = server.json("atlas_symbol", selected.clone());
    assert_eq!(stale["navigation"]["current_source"], "stale");
    assert_eq!(stale["navigation"]["changed_files"], json!(["calc.ts"]));
    assert!(
        stale["items"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("n+1")
    );
    for tool in ["atlas_references", "atlas_dependencies"] {
        let output = server.json(tool, selected.clone());
        assert_eq!(output["navigation"]["current_source"], "stale");
    }
    let text = server.ok("atlas_context", json!({"task":"calc","max_bytes":8000}));
    assert!(text.contains("source=stale") && text.contains("calc.ts"));
    fs::remove_file(root.join("calc.ts")).unwrap();
    assert_eq!(
        server.json("atlas_symbol", selected)["navigation"]["current_source"],
        "stale"
    );
    assert_eq!(fs::read(root.join(".code-intel/index.db")).unwrap(), before);
}

#[test]
fn language_and_exclusion_scope_are_explicit() {
    let files = [
        (".gitignore", "excluded.ts\n"),
        ("ok.ts", "export function okay(){}"),
        ("view.tsx", "export function View(){ return <div/>; }"),
        ("excluded.ts", "export function hidden(){}"),
        ("legacy.js", "throw new Error('must never run');"),
        ("legacy.jsx", "export const View=()=> <div/>;"),
        ("legacy.mts", "export const legacy=1;"),
        ("legacy.cts", "export const legacy=1;"),
    ];
    let (_temp, root) = sources(&files);
    let mut server = Server::ready(&[&root]);
    for path in ["legacy.js", "legacy.jsx", "legacy.mts", "legacy.cts"] {
        for tool in ["atlas_symbol", "atlas_references", "atlas_dependencies"] {
            let (text, error) = server.call(tool, json!({"name":"legacy","path":path}));
            assert!(
                error
                    && text.starts_with("error[unsupported_language]")
                    && text.contains(path)
                    && text.contains("Read/Grep"),
                "{text}"
            );
        }
    }
    assert_eq!(
        server.error_code(
            "atlas_symbol",
            json!({"name":"hidden","path":"excluded.ts"})
        ),
        "path_not_indexed"
    );
    assert_eq!(
        server.json("atlas_symbol", json!({"name":"okay","path":"ok.ts"}))["items"][0]["path"],
        "ok.ts"
    );
    assert_eq!(
        server.json("atlas_symbol", json!({"name":"View","path":"view.tsx"}))["items"][0]["path"],
        "view.tsx"
    );
    let empty = server.ok("atlas_context", json!({"task":"missingLegacyIdentifier"}));
    assert!(empty.contains("no indexed matches") && empty.contains(".ts/.tsx only"));
    assert_eq!(
        server.error_code("atlas_symbol", json!({"name":"x","path":"../legacy.js"})),
        "invalid_path"
    );
}

#[test]
fn observed_selector_errors_offer_evidence_backed_recovery_without_retargeting() {
    let (_temp, root) = sources(&[
        (
            "Logo.tsx",
            "export default function SvgLogo(){ return <svg/>; }",
        ),
        ("index.ts", "export {default as Logo} from './Logo';"),
        (
            "consumer.tsx",
            "import {Logo} from './index'; export function App(){ return <Logo/>; }",
        ),
        ("a.ts", "export function same(){}"),
        ("b.ts", "export function same(){}"),
    ]);
    let mut server = Server::ready_with(
        &[&root],
        &[
            "--tools",
            "atlas_context,atlas_symbol,atlas_references,atlas_dependencies,atlas_impact,atlas_status",
        ],
    );
    let (missing, error) = server.call(
        "atlas_references",
        json!({"path":"Logo.tsx","limit":100,"max_bytes":8000}),
    );
    assert!(
        error
            && missing.contains("provide `id` or `name`")
            && missing.contains("atlas_context")
            && missing.contains("Logo.tsx")
    );
    let (alias, error) = server.call("atlas_references", json!({"name":"Logo"}));
    assert!(
        error
            && alias.starts_with("error[symbol_not_found]")
            && alias.contains("SvgLogo")
            && alias.contains("Resolved use-site candidate"),
        "{alias}"
    );
    let resolved = server.json(
        "atlas_references",
        json!({"name":"SvgLogo","path":"Logo.tsx","max_bytes":8000}),
    );
    assert!(
        resolved["resolved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["jsx"] == true && s["kind"] == "REFERENCES")
    );
    assert_eq!(
        server.error_code("atlas_references", json!({"name":"same"})),
        "ambiguous_symbol"
    );
    // An exact declaration name wins over a use-site alias suggestion.
    assert_eq!(
        server.json("atlas_references", json!({"name":"same","path":"a.ts"}))["symbol"]["path"],
        "a.ts"
    );
    let (conflict, error) = server.call("atlas_impact", json!({"summary":true,"tests":true}));
    assert!(error && conflict.contains("omit summary") && conflict.contains("omit tests"));
    let (unknown, error) = server.call("atlas_references", json!({"name":"unprovenAlias"}));
    assert!(error && !unknown.contains("Resolved use-site candidate"));
    fs::write(
        root.join("actual.tsx"),
        "export function Logo(){ return <span/>; }",
    )
    .unwrap();
    fs::write(root.join("external.tsx"),"import {Icon as UnsupportedLogo} from 'external-package'; export function External(){return <UnsupportedLogo/>;}").unwrap();
    cli(&root, &["index", "."]);
    // A same-name declaration must never be replaced by an alias suggestion.
    assert_eq!(
        server.json("atlas_references", json!({"name":"Logo"}))["symbol"]["path"],
        "actual.tsx"
    );
    let (unbound, error) = server.call("atlas_references", json!({"name":"UnsupportedLogo"}));
    assert!(error && !unbound.contains("Resolved use-site candidate"));
}

#[test]
fn budget_matrix_has_exact_reference_omissions_and_honest_traversal_bounds() {
    let (_temp, root) = sources(&[
        ("leaf.ts", "export function calc(n:number){return n;}"),
        (
            "unknown.ts",
            "declare const object:any; export function maybe(){return object.calc(1);}",
        ),
        ("import.ts", "import {calc} from './leaf';"),
    ]);
    for n in 0..180 {
        fs::write(
            root.join(format!("user{n:03}.ts")),
            format!(
                "import {{calc}} from './leaf'; export function user{n}(){{return calc({n});}}"
            ),
        )
        .unwrap();
    }
    cli(&root, &["index", "."]);
    let mut server = Server::ready(&[&root]);
    for budget in [4000, 8000, 32000] {
        let args = json!({"name":"calc","path":"leaf.ts","kinds":["CALLS","REFERENCES"],"include_unresolved":true,"limit":50,"max_bytes":budget,"format":"json"});
        let text = server.ok("atlas_references", args.clone());
        assert!(text.len() <= budget);
        assert_eq!(text, server.ok("atlas_references", args));
        let result: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(result["available_resolved"], 360);
        assert_eq!(result["counts_exact"], true);
        let returned = result["resolved"].as_array().unwrap().len()
            + result["unresolved"].as_array().unwrap().len();
        assert_eq!(
            usize::try_from(result["omitted"].as_u64().unwrap()).unwrap() + returned,
            usize::try_from(result["available_resolved"].as_u64().unwrap()).unwrap()
                + usize::try_from(result["available_unresolved"].as_u64().unwrap()).unwrap()
        );
        assert_eq!(result["truncated"], true);
        assert_eq!(result["limit_reached"], true);
        let args = json!({"name":"calc","path":"leaf.ts","direction":"upstream","depth":2,"max_nodes":10,"include_unresolved":true,"max_bytes":budget,"format":"json"});
        let dep = server.json("atlas_dependencies", args);
        assert_eq!(dep["counts_exact"], false);
        assert_eq!(dep["limit_reached"], true);
        assert_eq!(dep["truncated"], true);
    }
}

#[test]
fn symlinked_selected_sources_are_not_read_or_claimed_current() {
    #[cfg(unix)]
    {
        let (_temp, root) = sources(&[("safe.ts", "export function safe(){ return 'indexed'; }")]);
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret.ts"), "OUTSIDE_MUST_NOT_BE_READ").unwrap();
        fs::remove_file(root.join("safe.ts")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.ts"), root.join("safe.ts")).unwrap();
        let mut server = Server::ready(&[&root]);
        assert_eq!(
            server.error_code("atlas_symbol", json!({"name":"safe","path":"safe.ts"})),
            "invalid_path"
        );
        let result = server.json("atlas_symbol", json!({"name":"safe","max_bytes":8000}));
        assert_eq!(result["navigation"]["current_source"], "unverified");
        assert!(!result.to_string().contains("OUTSIDE_MUST_NOT_BE_READ"));
    }
}

#[test]
fn undersized_context_and_symbol_envelopes_have_one_typed_error() {
    let (_temp, root) = sources(&[("leaf.ts", "export function calc(){return 1;}")]);
    let mut server = Server::ready(&[&root]);
    for (tool, args) in [
        (
            "atlas_context",
            json!({"task":"calc","max_bytes":256,"format":"json"}),
        ),
        (
            "atlas_symbol",
            json!({"name":"calc","max_bytes":256,"format":"json"}),
        ),
    ] {
        assert_eq!(server.error_code(tool, args), "budget_too_small");
    }
}
