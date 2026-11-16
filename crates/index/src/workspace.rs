//! Local workspace packages declared at the repository root.
//!
//! Read-only and offline: the root `package.json` `workspaces` field (npm and
//! Yarn: an array, or `{ "packages": [...] }`) and `pnpm-workspace.yaml`
//! (`packages:` list) are read as data. Member directories come from the
//! files the indexer already discovered, so nothing outside the root, inside
//! `node_modules` or behind a symlink is read. No package manager runs and
//! nothing is installed. A declaration Repogi cannot evaluate exactly makes
//! the whole workspace unsupported instead of guessing members.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use code_intel_core::semantic::{Workspace, WorkspacePackage};
use serde_json::Value;

use crate::tsconfig::read;

/// Bounds on what one repository may declare.
const MAX_PATTERNS: usize = 256;
const MAX_MEMBERS: usize = 10_000;
/// Dependency fields, in the order a duplicate declaration is taken from.
const DEPENDENCY_FIELDS: [&str; 4] = [
    "dependencies",
    "optionalDependencies",
    "devDependencies",
    "peerDependencies",
];

/// The root's workspace declaration and its members. `manifests` are the
/// directories holding a discovered `package.json`; `symlinks` are discovered
/// symlink entries (never followed).
#[allow(clippy::too_many_lines)] // Declarations, members, identities, dependencies: in order.
pub fn discover(
    root: &Path,
    manifests: &BTreeSet<String>,
    symlinks: &BTreeSet<String>,
) -> Option<Workspace> {
    let npm = npm_patterns(root);
    let pnpm = pnpm_patterns(root);
    if npm.is_none() && pnpm.is_none() {
        return None;
    }
    let mut workspace = Workspace::default();
    let mut declared: Vec<(String, Patterns)> = Vec::new();
    if let Some(patterns) = npm {
        workspace.declared_in.push("package.json".into());
        declared.push(("package.json".into(), patterns));
    }
    if let Some(patterns) = pnpm {
        workspace.declared_in.push("pnpm-workspace.yaml".into());
        declared.push(("pnpm-workspace.yaml".into(), patterns));
    }
    // Every member that can be evaluated keeps its identity even when the
    // declaration as a whole is unsupported: imports of those names then
    // report the workspace problem instead of looking like registry packages.
    let mut errors = Vec::new();
    let mut member_sets = Vec::new();
    for (file, patterns) in declared {
        let (patterns, error) = match patterns {
            Ok(patterns) => (patterns, None),
            Err((patterns, error)) => (patterns, Some(error)),
        };
        errors.extend(error.map(|error| format!("{file}: {error}")));
        let (members, error) = members(&patterns, manifests, symlinks);
        errors.extend(error.map(|error| format!("{file}: {error}")));
        member_sets.push(members);
    }
    if member_sets.windows(2).any(|pair| pair[0] != pair[1]) {
        errors.push(
            "package.json workspaces and pnpm-workspace.yaml declare different packages".into(),
        );
    }
    let mut members: BTreeSet<String> = member_sets.into_iter().flatten().collect();
    // pnpm always includes the root package as a workspace project; npm and
    // Yarn classic do not link it by name.
    if workspace
        .declared_in
        .iter()
        .any(|file| file == "pnpm-workspace.yaml")
        && manifests.contains("")
    {
        members.insert(String::new());
    }
    for dir in &members {
        let manifest = if dir.is_empty() {
            "package.json".to_owned()
        } else {
            format!("{dir}/package.json")
        };
        let Some(object) = manifest_object(root, &manifest) else {
            errors.push(format!("workspace member `{manifest}` cannot be read"));
            continue;
        };
        let Some(name) = object.get("name") else {
            continue; // A member without a name cannot be imported by name.
        };
        let Some(name) = name.as_str().filter(|name| valid_name(name)) else {
            errors.push(format!("workspace member `{manifest}` has an invalid name"));
            continue;
        };
        let types = ["types", "typings"]
            .iter()
            .find_map(|field| object.get(*field))
            .map(|value| value.as_str().map(str::to_owned).ok_or(()));
        let Ok(types) = types.transpose() else {
            errors.push(format!(
                "workspace member `{manifest}` has a non-string types"
            ));
            continue;
        };
        workspace.packages.push(WorkspacePackage {
            name: name.to_owned(),
            dir: dir.clone(),
            version: object
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned),
            types,
            types_versions: object.contains_key("typesVersions"),
        });
    }
    if !errors.is_empty() {
        workspace.error = Some(errors.join("; "));
    }
    let names: BTreeSet<&str> = workspace
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    for dir in manifests {
        let manifest = if dir.is_empty() {
            "package.json".to_owned()
        } else {
            format!("{dir}/package.json")
        };
        let Some(object) = manifest_object(root, &manifest) else {
            continue;
        };
        let mut declared = BTreeMap::new();
        for field in DEPENDENCY_FIELDS {
            for (name, spec) in object
                .get(field)
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                if names.contains(name.as_str())
                    && let Some(spec) = spec.as_str()
                {
                    declared
                        .entry(name.clone())
                        .or_insert_with(|| spec.to_owned());
                }
            }
        }
        if !declared.is_empty() {
            workspace.dependencies.insert(dir.clone(), declared);
        }
    }
    Some(workspace)
}

/// npm package-name characters only: no path traversal through a name.
fn valid_name(name: &str) -> bool {
    let (scope, rest) = match name.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, rest)) => (Some(scope), rest),
            None => return false,
        },
        None => (None, name),
    };
    let part = |text: &str| {
        !text.is_empty()
            && !text.starts_with('.')
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
    };
    name.len() <= 214 && scope.is_none_or(part) && part(rest)
}

fn manifest_object(root: &Path, manifest: &str) -> Option<serde_json::Map<String, Value>> {
    let text = read(root, manifest).ok()?;
    match serde_json::from_str::<Value>(text.strip_prefix('\u{feff}').unwrap_or(&text)) {
        Ok(Value::Object(object)) => Some(object),
        _ => None,
    }
}

/// Declared patterns, or the patterns that could be read plus why the
/// declaration as a whole is unsupported.
type Patterns = Result<Vec<String>, (Vec<String>, String)>;

/// The root `package.json` `workspaces` patterns, if it declares any.
fn npm_patterns(root: &Path) -> Option<Patterns> {
    let object = manifest_object(root, "package.json")?;
    let field = object.get("workspaces")?;
    let list = match field {
        Value::Array(list) => list,
        Value::Object(object) => match object.get("packages") {
            Some(Value::Array(list)) => list,
            _ => {
                return Some(Err((
                    Vec::new(),
                    "`workspaces` object has no `packages` array".into(),
                )));
            }
        },
        _ => {
            return Some(Err((
                Vec::new(),
                "`workspaces` is not an array or object".into(),
            )));
        }
    };
    let patterns: Vec<String> = list
        .iter()
        .filter_map(|pattern| pattern.as_str().map(str::to_owned))
        .collect();
    Some(if patterns.len() == list.len() {
        Ok(patterns)
    } else {
        Err((patterns, "a workspace pattern is not a string".into()))
    })
}

/// The `packages:` list of the root `pnpm-workspace.yaml`. Only a block list
/// of plain or quoted scalars is read; any other YAML in that key is refused.
fn pnpm_patterns(root: &Path) -> Option<Patterns> {
    let relative = "pnpm-workspace.yaml";
    root.join(relative).symlink_metadata().ok()?;
    Some(
        read(root, relative)
            .and_then(|text| parse_pnpm(&text))
            .map_err(|error| (Vec::new(), error)),
    )
}

fn parse_pnpm(text: &str) -> Result<Vec<String>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut patterns = Vec::new();
    let mut in_packages = false;
    let mut seen = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "---" || trimmed == "..." || trimmed.starts_with('%') {
            return Err("YAML documents or directives are not supported".into());
        }
        if !line.starts_with([' ', '\t']) {
            in_packages = false;
            let Some((key, rest)) = line.split_once(':') else {
                return Err(format!("unsupported line `{trimmed}`"));
            };
            if key.trim().trim_matches(['"', '\'']) == "packages" {
                if seen {
                    return Err("`packages` is declared twice".into());
                }
                seen = true;
                let rest = strip_comment(rest).trim();
                if !rest.is_empty() {
                    return Err("only a block list of `packages` is supported".into());
                }
                in_packages = true;
            }
            continue;
        }
        if !in_packages {
            continue;
        }
        let Some(item) = trimmed
            .strip_prefix("- ")
            .or((trimmed == "-").then_some(""))
        else {
            return Err(format!("unsupported `packages` entry `{trimmed}`"));
        };
        patterns.push(scalar(item)?);
    }
    Ok(patterns)
}

fn strip_comment(text: &str) -> &str {
    text.find(" #").map_or(text, |at| &text[..at])
}

/// A plain, single-quoted or escape-free double-quoted YAML scalar.
fn scalar(item: &str) -> Result<String, String> {
    let item = item.trim();
    for quote in ['\'', '"'] {
        if let Some(inner) = item.strip_prefix(quote) {
            let Some(end) = inner.find(quote) else {
                return Err(format!("unterminated scalar `{item}`"));
            };
            let (value, rest) = inner.split_at(end);
            let rest = strip_comment(&rest[1..]).trim();
            if !rest.is_empty() || value.contains('\\') || (quote == '\'' && value.contains("''")) {
                return Err(format!("unsupported scalar `{item}`"));
            }
            return Ok(value.to_owned());
        }
    }
    let value = strip_comment(item).trim();
    if value.is_empty()
        || value.starts_with(['*', '&', '!', '{', '[', '|', '>', '@', '`', '%', '?', ':'])
        || value.contains(": ")
    {
        return Err(format!("unsupported scalar `{item}`"));
    }
    Ok(value.to_owned())
}

/// One pattern segment: a literal name, `*` (one directory) or `**` (any
/// number of directories). Character classes, braces, `?` and partial `*`
/// are not evaluated.
enum Segment {
    Literal(String),
    One,
    Any,
}

/// Bounds that keep `**` matching polynomial: at most this many segments and
/// `**` segments per pattern (consecutive `**` collapse into one).
const MAX_SEGMENTS: usize = 32;
const MAX_RECURSIVE: usize = 2;

fn pattern(text: &str) -> Result<Vec<Segment>, String> {
    let text = text
        .strip_prefix("./")
        .unwrap_or(text)
        .trim_end_matches('/');
    if text.is_empty() || text.starts_with('/') {
        return Err(format!(
            "workspace pattern `{text}` is not a relative directory"
        ));
    }
    let mut parts: Vec<&str> = text.split('/').collect();
    parts.dedup_by(|next, previous| *next == "**" && *previous == "**");
    if parts.len() > MAX_SEGMENTS
        || parts.iter().filter(|part| **part == "**").count() > MAX_RECURSIVE
    {
        return Err(format!("workspace pattern `{text}` is too complex"));
    }
    parts
        .into_iter()
        .map(|segment| match segment {
            "*" => Ok(Segment::One),
            "**" => Ok(Segment::Any),
            "." | ".." | "" => Err(format!(
                "workspace pattern `{text}` leaves its directory or is empty"
            )),
            literal
                if literal
                    .contains(['*', '?', '[', ']', '{', '}', '!', '\\', '(', ')', '+', '@']) =>
            {
                Err(format!(
                    "workspace pattern `{text}` uses unsupported glob syntax"
                ))
            }
            literal => Ok(Segment::Literal(literal.to_owned())),
        })
        .collect()
}

fn matches(segments: &[Segment], parts: &[&str]) -> bool {
    match (segments.first(), parts.first()) {
        (None, None) => true,
        (Some(Segment::Any), _) => {
            matches(&segments[1..], parts)
                || (!parts.is_empty()
                    && !parts[0].starts_with('.')
                    && matches(segments, &parts[1..]))
        }
        (Some(Segment::One), Some(part)) => {
            !part.starts_with('.') && matches(&segments[1..], &parts[1..])
        }
        (Some(Segment::Literal(literal)), Some(part)) => {
            literal == part && matches(&segments[1..], &parts[1..])
        }
        _ => false,
    }
}

/// Member directories (with a `package.json`) that an include pattern
/// matches and no `!` exclusion removes.
fn members(
    patterns: &[String],
    manifests: &BTreeSet<String>,
    symlinks: &BTreeSet<String>,
) -> (BTreeSet<String>, Option<String>) {
    let mut errors = Vec::new();
    if patterns.len() > MAX_PATTERNS {
        errors.push(format!("more than {MAX_PATTERNS} workspace patterns"));
    }
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for text in patterns.iter().take(MAX_PATTERNS) {
        let (list, text) = match text.strip_prefix('!') {
            Some(negated) => (&mut exclude, negated),
            None => (&mut include, text.as_str()),
        };
        match pattern(text) {
            Ok(segments) => list.push(segments),
            Err(error) => errors.push(error),
        }
    }
    let selected = |dir: &str| {
        let parts: Vec<&str> = dir.split('/').collect();
        include.iter().any(|segments| matches(segments, &parts))
            && !exclude.iter().any(|segments| matches(segments, &parts))
    };
    // A symlinked directory is never read; if a pattern selects it, the
    // member set (and so name uniqueness) is unknown.
    errors.extend(
        symlinks
            .iter()
            .filter(|link| selected(link))
            .map(|link| format!("workspace member `{link}` is a symlink")),
    );
    let mut members: BTreeSet<String> = manifests
        .iter()
        .filter(|dir| !dir.is_empty() && selected(dir))
        .cloned()
        .collect();
    if members.len() > MAX_MEMBERS {
        errors.push(format!("more than {MAX_MEMBERS} workspace members"));
        members = members.into_iter().take(MAX_MEMBERS).collect();
    }
    (members, (!errors.is_empty()).then(|| errors.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pnpm_block_lists_only() {
        assert_eq!(
            parse_pnpm("packages:\n  - 'packages/*'\n  - apps/web # app\n  - \"!**/test/**\"\ncatalog:\n  react: ^19\n")
                .unwrap(),
            vec!["packages/*", "apps/web", "!**/test/**"]
        );
        assert!(parse_pnpm("packages: [a, b]\n").is_err());
        assert!(parse_pnpm("packages:\n  - *anchor\n").is_err());
        assert!(parse_pnpm("packages:\n  - a\npackages:\n  - b\n").is_err());
        assert!(parse_pnpm("packages:\n  nested: true\n").is_err());
    }

    #[test]
    fn patterns_match_directories_exactly() {
        let manifests: BTreeSet<String> = [
            "",
            "packages/core",
            "packages/session",
            "packages/core/test/fixture",
            "apps/web",
            "tools/x",
            "packages/.hidden",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let (found, error) = members(
            &["packages/*".into(), "apps/*".into()],
            &manifests,
            &BTreeSet::new(),
        );
        assert!(error.is_none());
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            vec!["apps/web", "packages/core", "packages/session"]
        );
        let (deep, _) = members(
            &["packages/**".into(), "!**/test/**".into()],
            &manifests,
            &BTreeSet::new(),
        );
        assert!(deep.contains("packages/core") && !deep.contains("packages/core/test/fixture"));
        for bad in [
            "../outside",
            "/abs",
            "packages/{a,b}",
            "packages/pre-*",
            "a/../b",
        ] {
            let (found, error) = members(&[bad.into()], &manifests, &BTreeSet::new());
            assert!(found.is_empty() && error.is_some(), "{bad}");
        }
        // Collapsed and bounded `**`: deep paths cannot make matching explode.
        let deep_path: BTreeSet<String> = BTreeSet::from([vec!["d"; 200].join("/")]);
        let (found, error) = members(&["**/**/**/d".into()], &deep_path, &BTreeSet::new());
        assert!(error.is_none() && found.len() == 1);
        let (_, error) = members(&["**/a/**/b/**/c".into()], &deep_path, &BTreeSet::new());
        assert!(error.is_some());
        let links = BTreeSet::from(["packages/linked".to_owned()]);
        let (found, error) = members(&["packages/*".into()], &manifests, &links);
        assert!(found.contains("packages/core") && error.is_some());
    }

    #[test]
    fn names_cannot_traverse() {
        for good in ["@acme/session", "core", "a.b-c_d"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["../x", "@acme", "@acme/../x", ".hidden", "a/b", ""] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
