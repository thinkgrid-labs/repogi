//! Project module-resolution settings from `tsconfig.json` / `jsconfig.json`.
//!
//! Read-only and offline: configuration files are parsed as JSONC (comments and
//! trailing commas), local relative `extends` chains are followed inside the
//! repository root, and only `baseUrl` and `paths` are extracted. Nothing is
//! executed and no package is looked up. Anything that would make the effective
//! settings unknowable marks the project as unsupported instead of guessing.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use code_intel_core::{
    Diagnostic, MAX_FILE_BYTES,
    semantic::{PathMapping, Project, ResolutionContext},
};
use serde_json::{Map, Value};

const MAX_EXTENDS_DEPTH: usize = 16;
/// Files a TypeScript resolver may select for a module specifier.
const MODULE_EXTENSIONS: [&str; 8] = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];

/// Build the resolution context from discovered repository entries.
pub fn context(root: &Path, entries: &[ignore::DirEntry]) -> (ResolutionContext, Vec<Diagnostic>) {
    let mut configs: BTreeMap<String, String> = BTreeMap::new();
    let mut module_files = BTreeSet::new();
    for entry in entries {
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        let Some(relative) = entry
            .path()
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .map(|path| path.replace('\\', "/"))
        else {
            continue;
        };
        let name = relative.rsplit('/').next().unwrap_or_default();
        let directory = relative
            .rsplit_once('/')
            .map_or("", |(dir, _)| dir)
            .to_owned();
        if name == "tsconfig.json" || (name == "jsconfig.json" && !configs.contains_key(&directory))
        {
            configs.insert(directory, relative.clone());
        }
        let extension = Path::new(name).extension().and_then(|value| value.to_str());
        if name == "package.json" || extension.is_some_and(|ext| MODULE_EXTENSIONS.contains(&ext)) {
            module_files.insert(relative);
        }
    }
    let mut diagnostics = Vec::new();
    let projects = configs
        .into_iter()
        .map(|(dir, config)| {
            let project = project(root, &config, &dir);
            if let Some(error) = &project.error {
                diagnostics.push(Diagnostic {
                    path: config.clone(),
                    message: format!(
                        "module resolution settings unsupported: {error}; non-relative imports under `{}` stay unsupported",
                        if dir.is_empty() { "." } else { &dir }
                    ),
                });
            }
            project
        })
        .collect();
    (
        ResolutionContext {
            projects,
            other_files: module_files,
        },
        diagnostics,
    )
}

/// A setting may be absent, known (with the directory/config that declared it)
/// or unknowable because it could come from an unread package configuration.
#[derive(Clone, Default)]
enum Setting<T> {
    #[default]
    Unset,
    Known(T),
    Unknown,
}

#[derive(Clone, Default)]
struct Options {
    /// (declaring directory, raw value)
    base_url: Setting<(String, String)>,
    /// (declaring config, declaring directory, raw mapping)
    paths: Setting<(String, String, Map<String, Value>)>,
}

fn project(root: &Path, config: &str, dir: &str) -> Project {
    let mut project = Project {
        config: config.to_owned(),
        dir: dir.to_owned(),
        base_url: None,
        base_url_outside_root: false,
        paths: Vec::new(),
        error: None,
    };
    match load(root, config, &mut Vec::new()).and_then(|options| effective(&options, dir)) {
        Ok((base_url, outside, paths)) => {
            project.base_url = base_url;
            project.base_url_outside_root = outside;
            project.paths = paths;
        }
        Err(error) => project.error = Some(error),
    }
    project
}

type Effective = (Option<String>, bool, Vec<PathMapping>);

fn effective(options: &Options, leaf_dir: &str) -> Result<Effective, String> {
    let (base_url, outside) = match &options.base_url {
        Setting::Unset => (None, false),
        Setting::Unknown => {
            return Err(
                "`extends` names a package configuration whose baseUrl/paths are not read".into(),
            );
        }
        Setting::Known((dir, value)) => match resolve(dir, value, leaf_dir) {
            Some(path) => (Some(path), false),
            None => (None, true),
        },
    };
    let paths = match &options.paths {
        Setting::Unset => Vec::new(),
        Setting::Unknown => {
            return Err(
                "`extends` names a package configuration whose baseUrl/paths are not read".into(),
            );
        }
        Setting::Known((declared_in, dir, mapping)) => {
            // Relative substitutions are based on baseUrl when set, else on the
            // directory of the config that declared `paths`.
            let base = if outside {
                None
            } else {
                Some(base_url.clone().unwrap_or_else(|| dir.clone()))
            };
            let mut result = Vec::new();
            for (pattern, substitutions) in mapping {
                if pattern.matches('*').count() > 1 {
                    return Err(format!("paths pattern `{pattern}` has more than one `*`"));
                }
                let Some(list) = substitutions.as_array().filter(|list| !list.is_empty()) else {
                    return Err(format!("paths entry `{pattern}` must be a non-empty array"));
                };
                let mut resolved = Vec::new();
                for substitution in list {
                    let Some(text) = substitution.as_str() else {
                        return Err(format!("paths entry `{pattern}` must contain strings"));
                    };
                    if text.matches('*').count() > 1 {
                        return Err(format!("paths substitution `{text}` has more than one `*`"));
                    }
                    resolved.push(if text.starts_with("${configDir}") {
                        resolve(leaf_dir, text, leaf_dir)
                    } else {
                        base.as_deref()
                            .and_then(|base| resolve(base, text, leaf_dir))
                    });
                }
                result.push(PathMapping {
                    pattern: pattern.clone(),
                    substitutions: resolved,
                    declared_in: declared_in.clone(),
                });
            }
            result
        }
    };
    Ok((base_url, outside, paths))
}

/// Resolve `value` against repository-relative `base`. `None` when the result
/// leaves the repository root or is absolute.
fn resolve(base: &str, value: &str, leaf_dir: &str) -> Option<String> {
    let (base, value) = match value.strip_prefix("${configDir}") {
        Some(rest) => (leaf_dir, rest.trim_start_matches('/')),
        None => (base, value),
    };
    if value.starts_with('/') || value.contains('\\') || value.contains(':') {
        return None;
    }
    join(base, value)
}

/// Join and normalize `/`-separated paths below the root; `None` on escape.
pub(crate) fn join(base: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn load(root: &Path, config: &str, visiting: &mut Vec<String>) -> Result<Options, String> {
    if visiting.iter().any(|seen| seen == config) {
        return Err(format!("cyclic `extends` through `{config}`"));
    }
    if visiting.len() >= MAX_EXTENDS_DEPTH {
        return Err(format!("`extends` deeper than {MAX_EXTENDS_DEPTH}"));
    }
    visiting.push(config.to_owned());
    let text = read(root, config)?;
    let value =
        parse_jsonc(&text).map_err(|error| format!("`{config}` is not valid JSONC: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("`{config}` must contain a JSON object"))?;
    let dir = config
        .rsplit_once('/')
        .map_or("", |(dir, _)| dir)
        .to_owned();
    let mut options = Options::default();
    let parents = match object.get("extends") {
        None => Vec::new(),
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or("`extends` must contain strings")
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(format!("`extends` in `{config}` must be a string or array")),
    };
    // Later bases override earlier ones; the file's own options override all bases.
    for parent in parents {
        if parent.starts_with("./") || parent.starts_with("../") {
            let target = join(&dir, &parent)
                .ok_or_else(|| format!("`extends` `{parent}` leaves the repository root"))?;
            let is_json = Path::new(&target)
                .extension()
                .is_some_and(|ext| ext == "json");
            let target = if !is_json && !root.join(&target).is_file() {
                format!("{target}.json")
            } else {
                target
            };
            let inherited = load(root, &target, visiting)?;
            merge(&mut options, inherited);
        } else if parent.starts_with('/') || parent.contains(':') || parent.contains('\\') {
            return Err(format!(
                "`extends` `{parent}` is an absolute path outside the supported profile"
            ));
        } else {
            options.base_url = Setting::Unknown;
            options.paths = Setting::Unknown;
        }
    }
    if let Some(compiler) = object.get("compilerOptions") {
        let compiler = compiler
            .as_object()
            .ok_or_else(|| format!("`compilerOptions` in `{config}` must be an object"))?;
        if let Some(value) = compiler.get("baseUrl") {
            let value = value
                .as_str()
                .ok_or_else(|| format!("`baseUrl` in `{config}` must be a string"))?;
            options.base_url = Setting::Known((dir.clone(), value.to_owned()));
        }
        if let Some(value) = compiler.get("paths") {
            let mapping = value
                .as_object()
                .ok_or_else(|| format!("`paths` in `{config}` must be an object"))?;
            options.paths = Setting::Known((config.to_owned(), dir.clone(), mapping.clone()));
        }
        if compiler
            .get("moduleResolution")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("classic"))
        {
            return Err("`moduleResolution: classic` is outside the supported profile".into());
        }
        if compiler
            .get("moduleSuffixes")
            .is_some_and(|suffixes| suffixes != &Value::Array(vec![Value::String(String::new())]))
        {
            return Err("`moduleSuffixes` is outside the supported profile".into());
        }
    }
    visiting.pop();
    Ok(options)
}

fn merge(options: &mut Options, inherited: Options) {
    if !matches!(inherited.base_url, Setting::Unset) {
        options.base_url = inherited.base_url;
    }
    if !matches!(inherited.paths, Setting::Unset) {
        options.paths = inherited.paths;
    }
}

/// A regular, non-symlinked config file inside the root, within the size cap.
fn read(root: &Path, relative: &str) -> Result<String, String> {
    let path = root.join(relative);
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| format!("`{relative}` does not exist"))?;
    if !metadata.file_type().is_file() {
        return Err(format!("`{relative}` is a symlink or not a regular file"));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("`{relative}` exceeds {MAX_FILE_BYTES} bytes"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("inspect `{relative}`: {error}"))?;
    if !canonical.starts_with(root) {
        return Err(format!("`{relative}` resolves outside the repository"));
    }
    let bytes = crate::source::open(root, relative)
        .and_then(crate::source::read)
        .map_err(|error| format!("read `{relative}`: {error}"))?;
    String::from_utf8(bytes).map_err(|error| format!("read `{relative}`: {error}"))
}

/// JSONC as TypeScript accepts it for configs: `//` and `/* */` comments and
/// trailing commas before `}`/`]`. Everything else must be strict JSON.
pub(crate) fn parse_jsonc(text: &str) -> Result<Value, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            stripped.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    stripped.push(next);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                stripped.push(c);
            }
            ('/', Some('/')) => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        stripped.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = '\0';
                let mut closed = false;
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        closed = true;
                        break;
                    }
                    previous = next;
                }
                if !closed {
                    return Err("unterminated block comment".into());
                }
                stripped.push(' ');
            }
            _ => stripped.push(c),
        }
    }
    // Drop commas that are followed (after whitespace) by a closing bracket.
    let mut cleaned = String::with_capacity(stripped.len());
    let characters: Vec<char> = stripped.chars().collect();
    let mut in_string = false;
    let mut index = 0;
    while index < characters.len() {
        let c = characters[index];
        if in_string {
            cleaned.push(c);
            if c == '\\' && index + 1 < characters.len() {
                cleaned.push(characters[index + 1]);
                index += 2;
                continue;
            }
            in_string = c != '"';
        } else if c == '"' {
            in_string = true;
            cleaned.push(c);
        } else if c == ',' {
            let next = characters[index + 1..]
                .iter()
                .find(|next| !next.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                cleaned.push(c);
            }
        } else {
            cleaned.push(c);
        }
        index += 1;
    }
    serde_json::from_str(&cleaned).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_comments_trailing_commas_and_strings() {
        let value = parse_jsonc(
            "\u{feff}{\n // line\n \"a\": \"// not a comment /* */\", /* block */\n \"b\": [1, 2,],\n \"c\": {\"d\": \"x\\\"y,\",},\n}",
        )
        .unwrap();
        assert_eq!(value["a"], "// not a comment /* */");
        assert_eq!(value["b"], serde_json::json!([1, 2]));
        assert_eq!(value["c"]["d"], "x\"y,");
        assert!(parse_jsonc("{\"a\": 1 /* open").is_err());
        assert!(parse_jsonc("{\"a\": 1,, }").is_err());
        assert!(parse_jsonc("{a: 1}").is_err());
    }

    #[test]
    fn joins_stay_inside_the_root() {
        assert_eq!(join("src", "../lib/./x").as_deref(), Some("lib/x"));
        assert_eq!(join("", "./*").as_deref(), Some("*"));
        assert_eq!(join("src", "../../x"), None);
        assert_eq!(resolve("", "/abs/*", ""), None);
        assert_eq!(
            resolve("ignored", "${configDir}/src/*", "pkg").as_deref(),
            Some("pkg/src/*")
        );
    }
}
