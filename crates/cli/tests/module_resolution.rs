//! Project-aware module resolution: tsconfig/jsconfig `paths`, `baseUrl` and
//! local `extends`, through the typed semantic graph. Every case is offline.
use serde_json::Value;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn output(root: &Path, args: &[&str]) -> String {
    let result = Command::new(env!("CARGO_BIN_EXE_repogi"))
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

fn repo(files: &[(&str, &str)]) -> TempDir {
    let temp = TempDir::new().unwrap();
    write(temp.path(), files);
    temp
}

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn index(root: &Path) -> Value {
    serde_json::from_str(&output(root, &["index", "."])).unwrap()
}

fn graph(root: &Path) -> Vec<Value> {
    serde_json::from_str::<Value>(&output(root, &["graph", "--limit", "10000"])).unwrap()["edges"]
        .as_array()
        .unwrap()
        .clone()
}

/// The IMPORTS edge for local `name` in `path`.
fn import<'a>(edges: &'a [Value], path: &str, name: &str) -> &'a Value {
    edges
        .iter()
        .find(|edge| {
            edge["kind"] == "IMPORTS"
                && edge["location"]["path"] == path
                && edge["target_text"] == name
        })
        .unwrap_or_else(|| panic!("no import {name} in {path}"))
}

fn call<'a>(edges: &'a [Value], path: &str, text: &str) -> &'a Value {
    edges
        .iter()
        .find(|edge| {
            edge["kind"] == "CALLS"
                && edge["location"]["path"] == path
                && edge["target_text"] == text
        })
        .unwrap_or_else(|| panic!("no call {text} in {path}"))
}

fn target_in(edge: &Value, file: &str) -> bool {
    edge["status"] == "resolved"
        && edge["target_id"]
            .as_str()
            .is_some_and(|id| id.split(':').nth(1) == Some(file))
}

fn rule(edge: &Value) -> &str {
    edge["evidence"]["rule"].as_str().unwrap()
}

fn chain(edge: &Value) -> String {
    edge["evidence"]["binding_chain"].to_string()
}

const UTIL: &str = "export function helper(): number { return 1; }\n";

#[test]
fn wildcard_aliases_resolve_imports_and_calls_with_config_evidence() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./src/*"] } } }"#,
        ),
        ("src/lib/util.ts", UTIL),
        (
            "src/app/page.ts",
            "import { helper } from '@/lib/util';\nimport { helper as h } from '@/lib/util';\nexport function page() { return helper() + h(); }\n",
        ),
    ]);
    index(root.path());
    let edges = graph(root.path());
    let edge = import(&edges, "src/app/page.ts", "helper");
    assert!(target_in(edge, "src/lib/util.ts"), "{edge}");
    assert!(chain(edge).contains("tsconfig.json: paths @/* -> ./src/* => src/lib/util.ts"));
    assert!(chain(edge).contains("src/app/page.ts: module @/lib/util => src/lib/util.ts"));
    // Import aliases bind through the same module.
    assert!(target_in(
        import(&edges, "src/app/page.ts", "h"),
        "src/lib/util.ts"
    ));
    assert!(target_in(
        call(&edges, "src/app/page.ts", "helper"),
        "src/lib/util.ts"
    ));
    assert!(target_in(
        call(&edges, "src/app/page.ts", "h"),
        "src/lib/util.ts"
    ));
}

#[test]
fn exact_patterns_win_then_the_longest_wildcard_prefix() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": {
                "@/*": ["./src/*"],
                "@/lib/*": ["./src/lib2/*"],
                "@/lib/special": ["./src/other/special.ts"]
            } } }"#,
        ),
        ("src/lib/x.ts", UTIL),
        ("src/lib2/x.ts", UTIL),
        ("src/other/special.ts", UTIL),
        ("src/y.ts", UTIL),
        (
            "src/use.ts",
            "import { helper as a } from '@/lib/special';\nimport { helper as b } from '@/lib/x';\nimport { helper as c } from '@/y';\n",
        ),
    ]);
    index(root.path());
    let edges = graph(root.path());
    assert!(target_in(
        import(&edges, "src/use.ts", "a"),
        "src/other/special.ts"
    ));
    assert!(target_in(
        import(&edges, "src/use.ts", "b"),
        "src/lib2/x.ts"
    ));
    assert!(target_in(import(&edges, "src/use.ts", "c"), "src/y.ts"));
}

#[test]
fn substitutions_are_tried_in_order_and_the_first_existing_module_wins() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@shared/*": ["./missing/*", "./src/shared/*", "./src/fallback/*"] } } }"#,
        ),
        ("src/shared/a.ts", UTIL),
        ("src/fallback/a.ts", UTIL),
        ("src/fallback/b.ts", UTIL),
        (
            "src/use.ts",
            "import { helper as a } from '@shared/a';\nimport { helper as b } from '@shared/b';\n",
        ),
    ]);
    index(root.path());
    let edges = graph(root.path());
    let first = import(&edges, "src/use.ts", "a");
    assert!(target_in(first, "src/shared/a.ts"), "{first}");
    assert!(chain(first).contains("-> ./missing/* (no module)"));
    assert!(target_in(
        import(&edges, "src/use.ts", "b"),
        "src/fallback/b.ts"
    ));
}

#[test]
fn conflicting_candidates_are_explicitly_ambiguous_for_aliases_and_relative_imports() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./src/*"] } } }"#,
        ),
        ("src/dup.ts", UTIL),
        ("src/dup/index.ts", UTIL),
        (
            "src/use.ts",
            "import { helper as a } from '@/dup';\nimport { helper as b } from './dup';\nexport const value = a() + b();\n",
        ),
    ]);
    index(root.path());
    let edges = graph(root.path());
    for name in ["a", "b"] {
        let edge = import(&edges, "src/use.ts", name);
        assert_eq!(edge["status"], "ambiguous", "{edge}");
        assert!(edge["target_id"].is_null());
        assert_eq!(rule(edge), "ambiguous_module_candidates");
        assert_ne!(call(&edges, "src/use.ts", name)["status"], "resolved");
    }
}

#[test]
fn barrels_directory_indexes_and_alias_cycles_follow_explicit_exports() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"] } } }"#,
        ),
        ("src/components/Button.ts", "export function Button() {}\n"),
        ("src/components/Card.ts", "export function Card() {}\n"),
        (
            "src/components/index.ts",
            "export * from './Button';\nexport { Card as Panel } from '@/components/Card';\n",
        ),
        (
            "src/a.ts",
            "export { b } from '@/b';\nexport function a() {}\n",
        ),
        (
            "src/b.ts",
            "export { a } from '@/a';\nexport function b() {}\n",
        ),
        ("src/c.ts", "export { x } from '@/d';\n"),
        ("src/d.ts", "export { x } from '@/c';\n"),
        (
            "src/use.ts",
            "import { Button, Panel } from '@/components';\nimport { a, b } from '@/a';\nimport { x } from '@/c';\nButton(); Panel(); a(); b();\n",
        ),
    ]);
    index(root.path());
    let edges = graph(root.path());
    assert!(target_in(
        import(&edges, "src/use.ts", "Button"),
        "src/components/Button.ts"
    ));
    assert!(target_in(
        import(&edges, "src/use.ts", "Panel"),
        "src/components/Card.ts"
    ));
    assert!(target_in(
        call(&edges, "src/use.ts", "Panel"),
        "src/components/Card.ts"
    ));
    assert!(target_in(import(&edges, "src/use.ts", "a"), "src/a.ts"));
    assert!(target_in(import(&edges, "src/use.ts", "b"), "src/b.ts"));
    // A re-export cycle with no declaration never invents a target.
    let x = import(&edges, "src/use.ts", "x");
    assert_ne!(x["status"], "resolved", "{x}");
    assert!(x["target_id"].is_null());
}

#[test]
fn local_extends_chains_merge_jsonc_and_children_override_paths() {
    let root = repo(&[
        (
            "tsconfig.json",
            "{\n  // JSONC: comments and trailing commas\n  \"extends\": \"./config/base\",\n  \"compilerOptions\": { /* child */ },\n}\n",
        ),
        (
            "config/base.json",
            r#"{ "compilerOptions": { "baseUrl": "..", "paths": { "~/*": ["src/*"], }, }, }"#,
        ),
        ("src/lib/util.ts", UTIL),
        ("src/use.ts", "import { helper } from '~/lib/util';\n"),
    ]);
    index(root.path());
    let edges = graph(root.path());
    let edge = import(&edges, "src/use.ts", "helper");
    assert!(target_in(edge, "src/lib/util.ts"), "{edge}");
    assert!(chain(edge).contains("config/base.json: paths ~/*"));
    // A child `paths` replaces the inherited one; baseUrl is still inherited.
    write(
        root.path(),
        &[(
            "tsconfig.json",
            r##"{ "extends": "./config/base.json", "compilerOptions": { "paths": { "#/*": ["src/*"] } } }"##,
        )],
    );
    let report = index(root.path());
    assert_eq!(report["files_changed"], 0);
    assert_eq!(report["resolution_changed"], true);
    let edge = import(&graph(root.path()), "src/use.ts", "helper").clone();
    assert_eq!(edge["status"], "unsupported", "{edge}");
    assert_eq!(rule(&edge), "package_or_unmapped_specifier");
    // A bare specifier that exists under baseUrl resolves through it.
    write(
        root.path(),
        &[("src/use.ts", "import { helper } from 'src/lib/util';\n")],
    );
    index(root.path());
    let edge = import(&graph(root.path()), "src/use.ts", "helper").clone();
    assert!(target_in(&edge, "src/lib/util.ts"), "{edge}");
    assert!(chain(&edge).contains("tsconfig.json: baseUrl . => src/lib/util.ts"));
}

#[test]
fn config_changes_rebind_without_reparsing_and_warm_runs_stay_put() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./src/*"] } } }"#,
        ),
        ("src/util.ts", UTIL),
        ("alt/util.ts", UTIL),
        ("use.ts", "import { helper } from '@/util';\nhelper();\n"),
    ]);
    let first = index(root.path());
    assert_eq!(first["resolution_changed"], false);
    let revision = first["index_revision"].as_u64().unwrap();
    assert!(target_in(
        import(&graph(root.path()), "use.ts", "helper"),
        "src/util.ts"
    ));
    let warm = index(root.path());
    assert_eq!(warm["index_revision"].as_u64().unwrap(), revision);
    assert_eq!(warm["resolution_changed"], false);

    write(
        root.path(),
        &[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./alt/*"] } } }"#,
        )],
    );
    let changed = index(root.path());
    assert_eq!(changed["files_changed"], 0);
    assert_eq!(changed["resolution_changed"], true);
    assert_eq!(changed["index_revision"].as_u64().unwrap(), revision + 1);
    let edges = graph(root.path());
    assert!(target_in(import(&edges, "use.ts", "helper"), "alt/util.ts"));
    assert!(target_in(call(&edges, "use.ts", "helper"), "alt/util.ts"));

    fs::remove_file(root.path().join("tsconfig.json")).unwrap();
    assert_eq!(index(root.path())["resolution_changed"], true);
    let edge = import(&graph(root.path()), "use.ts", "helper").clone();
    assert_eq!(edge["status"], "unsupported");
    assert_eq!(rule(&edge), "package_or_unmapped_specifier");
}

#[test]
fn missing_targets_packages_and_paths_outside_the_root_stay_unconfirmed() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": {
                "@/*": ["./src/*"],
                "@ext/*": ["../outside/*"],
                "@gen/*": ["./generated/*"]
            } } }"#,
        ),
        ("src/util.ts", UTIL),
        (
            "src/use.ts",
            "import { a } from '@/nope';\nimport { b } from 'react';\nimport { c } from '@ext/x';\nimport { d } from '@/../../escape';\nimport { e } from '../../escape';\nimport { f } from '@gen/x';\n",
        ),
    ]);
    index(root.path());
    let edges = graph(root.path());
    let expected = [
        ("a", "unresolved", "module_not_found"),
        ("b", "unsupported", "package_or_unmapped_specifier"),
        ("c", "unsupported", "path_mapping_outside_repository"),
        ("d", "unsupported", "path_mapping_outside_repository"),
        ("e", "unsupported", "module_outside_repository"),
        ("f", "unsupported", "path_mapping_into_excluded_directory"),
    ];
    for (name, status, reason) in expected {
        let edge = import(&edges, "src/use.ts", name);
        assert_eq!(edge["status"], status, "{edge}");
        assert_eq!(rule(edge), reason, "{edge}");
        assert!(edge["target_id"].is_null());
    }
}

#[test]
fn unindexed_modules_that_typescript_would_pick_first_block_resolution() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "allowJs": true, "paths": { "@/*": ["./src/*"] } } }"#,
        ),
        ("src/thing.js", "export function helper() {}\n"),
        ("src/thing/index.ts", UTIL),
        ("src/use.ts", "import { helper } from '@/thing';\n"),
    ]);
    index(root.path());
    let edge = import(&graph(root.path()), "src/use.ts", "helper").clone();
    assert_eq!(edge["status"], "unsupported", "{edge}");
    assert_eq!(rule(&edge), "module_resolves_to_unindexed_file");
}

#[test]
fn the_nearest_config_governs_and_jsconfig_is_used_without_tsconfig() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./src/*"] } } }"#,
        ),
        ("src/util.ts", UTIL),
        ("root.ts", "import { helper } from '@/util';\n"),
        (
            "packages/app/tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./lib/*"] } } }"#,
        ),
        ("packages/app/lib/util.ts", UTIL),
        ("packages/app/use.ts", "import { helper } from '@/util';\n"),
        (
            "packages/js/jsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./code/*"] } } }"#,
        ),
        ("packages/js/code/util.ts", UTIL),
        ("packages/js/use.ts", "import { helper } from '@/util';\n"),
    ]);
    index(root.path());
    let edges = graph(root.path());
    assert!(target_in(
        import(&edges, "root.ts", "helper"),
        "src/util.ts"
    ));
    assert!(target_in(
        import(&edges, "packages/app/use.ts", "helper"),
        "packages/app/lib/util.ts"
    ));
    assert!(target_in(
        import(&edges, "packages/js/use.ts", "helper"),
        "packages/js/code/util.ts"
    ));
}

#[test]
fn invalid_cyclic_missing_and_package_configs_are_reported_not_guessed() {
    let cases: [(&[(&str, &str)], &str); 5] = [
        (
            &[(
                "tsconfig.json",
                r#"{ "compilerOptions": { "paths": { "@/*": ["./src/*"] } } /* open"#,
            )],
            "not valid JSONC",
        ),
        (
            &[
                ("tsconfig.json", r#"{ "extends": "./a.json" }"#),
                ("a.json", r#"{ "extends": "./tsconfig.json" }"#),
            ],
            "cyclic `extends`",
        ),
        (
            &[("tsconfig.json", r#"{ "extends": "./missing.json" }"#)],
            "does not exist",
        ),
        (
            &[(
                "tsconfig.json",
                r#"{ "extends": "@tsconfig/next/tsconfig.json" }"#,
            )],
            "package configuration",
        ),
        (
            &[(
                "tsconfig.json",
                r#"{ "compilerOptions": { "paths": { "@/*/*": ["./src/*"] } } }"#,
            )],
            "more than one `*`",
        ),
    ];
    for (files, message) in cases {
        let root = repo(files);
        write(
            root.path(),
            &[
                ("src/util.ts", UTIL),
                ("use.ts", "import { helper } from '@/util';\n"),
            ],
        );
        let report = index(root.path());
        let diagnostics = report["diagnostics"].to_string();
        assert!(diagnostics.contains(message), "{message}: {diagnostics}");
        assert!(
            diagnostics.contains("non-relative imports"),
            "{diagnostics}"
        );
        let edge = import(&graph(root.path()), "use.ts", "helper").clone();
        assert_eq!(edge["status"], "unsupported", "{edge}");
        assert_eq!(rule(&edge), "invalid_project_configuration");
    }
}

#[test]
fn relative_imports_keep_their_profile_alongside_project_settings() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./src/*"] } } }"#,
        ),
        ("src/util.ts", UTIL),
        ("src/dir/index.tsx", "export function view() {}\n"),
        (
            "src/use.ts",
            "import { helper } from './util';\nimport { helper as js } from './util.js';\nimport { view } from './dir';\nimport './styles.css';\n",
        ),
    ]);
    index(root.path());
    let edges = graph(root.path());
    assert!(target_in(
        import(&edges, "src/use.ts", "helper"),
        "src/util.ts"
    ));
    assert!(target_in(import(&edges, "src/use.ts", "js"), "src/util.ts"));
    assert!(target_in(
        import(&edges, "src/use.ts", "view"),
        "src/dir/index.tsx"
    ));
    let style = edges
        .iter()
        .find(|edge| {
            edge["kind"] == "IMPORTS"
                && edge["evidence"]["syntax"]
                    .as_str()
                    .unwrap()
                    .contains("styles.css")
        })
        .unwrap();
    assert_eq!(style["status"], "unresolved");
    assert_eq!(rule(style), "module_not_found");
}

#[test]
fn excluded_directory_names_only_block_directories_not_file_stems() {
    let root = repo(&[
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["./src/*"] } } }"#,
        ),
        ("src/openapi/coverage.ts", UTIL),
        ("src/build.ts", UTIL),
        ("src/generated/types.ts", UTIL),
        (
            "src/use.ts",
            "import { helper as a } from '@/openapi/coverage';\nimport { helper as b } from './build';\nimport { helper as c } from '@/generated/types';\n",
        ),
    ]);
    index(root.path());
    let edges = graph(root.path());
    assert!(target_in(
        import(&edges, "src/use.ts", "a"),
        "src/openapi/coverage.ts"
    ));
    assert!(target_in(import(&edges, "src/use.ts", "b"), "src/build.ts"));
    let excluded = import(&edges, "src/use.ts", "c");
    assert_eq!(excluded["status"], "unsupported", "{excluded}");
    assert_eq!(rule(excluded), "path_mapping_into_excluded_directory");
}
