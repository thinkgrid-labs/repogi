//! A bounded native static binder over persisted AST facts. No source is executed.
use code_intel_core::{
    file_id,
    language::Language,
    semantic::{
        Binding, ClassInfo, CommonJsExport, Evidence, Location, Module, PathMapping, Project,
        Relationship, Resolution, ResolutionContext, SemanticEdge, Symbol, Usage,
    },
};
use std::{
    cell::{OnceCell, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

/// Bound on `extends` chains and subclass sets walked for one member lookup.
const MAX_HIERARCHY: usize = 32;
const MAX_SUBCLASSES: usize = 512;

/// What a class's `extends` clause statically names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Super {
    /// An indexed class (fact ID).
    Known(String),
    /// A package or global class: Repogi cannot see its members.
    External,
    /// A dynamic expression (`extends mixin(Base)`) or a binding that does not
    /// resolve to a class: it may be any class, including an indexed one.
    Unknown,
}

/// Whole-graph facts that decide whether an instance member lookup is safe.
#[derive(Default)]
struct Hierarchy {
    supers: HashMap<String, Super>,
    subclasses: HashMap<String, Vec<String>>,
    /// Classes whose prototype may be modified (`C.prototype` used, prototype
    /// manipulation in their module).
    tainted: HashSet<String>,
    /// Member names written on receivers that may be instances or constructors.
    assigned: HashSet<String>,
    /// `(class, member)` written through a binding of the class (`C.m = …`);
    /// member `*` for computed writes.
    static_writes: HashSet<(String, String)>,
    /// Member names declared by classes whose superclass is `Unknown`: such a
    /// class may override a method of any class.
    unknown_overrides: HashSet<String>,
}

/// Directories the indexer never reads. A mapping into one cannot be checked,
/// so it is reported as unsupported rather than treated as absent.
const EXCLUDED_DIRECTORIES: [&str; 8] = [
    "node_modules",
    ".git",
    ".code-intel",
    "dist",
    "build",
    "coverage",
    ".next",
    "generated",
];

pub type Modules = BTreeMap<String, Module>;

#[derive(Default, Clone)]
struct Targets {
    ids: BTreeSet<String>,
    chain: Vec<String>,
    uncertain: bool,
    unsupported: bool,
    type_only: bool,
    /// Specific rule for a module-resolution outcome, used when the edge ends
    /// with the same status.
    reason: Option<(Resolution, &'static str)>,
}
impl Targets {
    fn target(id: String) -> Self {
        Self {
            chain: vec![id.clone()],
            ids: BTreeSet::from([id]),
            ..Self::default()
        }
    }
    fn failure(status: Resolution, reason: &'static str, chain: Vec<String>) -> Self {
        Self {
            chain,
            uncertain: status == Resolution::Unresolved,
            unsupported: status == Resolution::Unsupported,
            reason: Some((status, reason)),
            ..Self::default()
        }
    }
    fn merge(&mut self, other: Self) {
        self.ids.extend(other.ids);
        self.chain.extend(other.chain);
        self.uncertain |= other.uncertain;
        self.unsupported |= other.unsupported;
        self.type_only |= other.type_only;
        if self.reason.is_none() {
            self.reason = other.reason;
        }
    }
    fn status(&self) -> Resolution {
        if self.ids.len() > 1 {
            Resolution::Ambiguous
        } else if self.unsupported {
            Resolution::Unsupported
        } else if self.ids.len() == 1 && !self.uncertain {
            Resolution::Resolved
        } else {
            Resolution::Unresolved
        }
    }
}

/// A module specifier lookup: the indexed target file plus evidence steps, or
/// the non-resolved outcome with its reason.
type Located = Result<(String, Vec<String>), Targets>;

/// What exists for one candidate base path, in TypeScript's lookup order.
enum Found {
    File(String),
    Missing,
    Conflict(Vec<String>),
    Unindexed(String),
    Excluded,
}

struct Resolver<'a> {
    modules: &'a Modules,
    resolution: &'a ResolutionContext,
    projects: BTreeMap<&'a str, &'a Project>,
    /// First symbol per ID in path order (what a linear scan would return).
    symbols: HashMap<&'a str, &'a Symbol>,
    /// Symbols by parent ID, for static-member lookup.
    children: HashMap<&'a str, Vec<&'a Symbol>>,
    /// Top-level export lookups, memoized for one rebind.
    exports: RefCell<HashMap<(String, String), Targets>>,
    classes: HashMap<&'a str, &'a ClassInfo>,
    hierarchy: OnceCell<Hierarchy>,
}
impl Resolver<'_> {
    fn module(&self, path: &str, name: &str) -> Targets {
        match self.locate(path, name) {
            Ok((target, steps)) => {
                let mut result = Targets::target(file_id(&target));
                result.chain.extend(steps);
                result
            }
            Err(result) => result,
        }
    }
    fn module_path(&self, path: &str, name: &str) -> Located {
        self.locate(path, name)
    }
    /// Relative specifiers resolve against the importing file; everything else
    /// goes through the governing project's `paths`, then `baseUrl`. Package
    /// lookup (`node_modules`, `exports` conditions) is outside the profile.
    fn locate(&self, path: &str, name: &str) -> Located {
        if name.contains('\\') {
            return Err(Targets::failure(
                Resolution::Unsupported,
                "unsupported_module_specifier",
                Vec::new(),
            ));
        }
        let relative = name.starts_with("./") || name.starts_with("../");
        let (target, steps) = if relative {
            let directory = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            let Some(base) = join(directory, name) else {
                return Err(Targets::failure(
                    Resolution::Unsupported,
                    "module_outside_repository",
                    vec![format!("{path}: module {name} leaves the repository root")],
                ));
            };
            match self.find(path, &base) {
                Found::File(target) => (target, Vec::new()),
                other => return Err(Self::failed(path, name, other, Vec::new(), false)),
            }
        } else {
            self.mapped(path, name)?
        };
        if !self.modules[&target].complete {
            return Err(Targets {
                uncertain: true,
                ..Targets::default()
            });
        }
        let mut steps = steps;
        // Relative imports keep their pre-3.5 evidence: the importing binding and
        // the target's export steps already identify the module.
        if !relative {
            steps.push(format!("{path}: module {name} => {target}"));
        }
        Ok((target, steps))
    }
    fn mapped(&self, path: &str, name: &str) -> Located {
        let Some(project) = self.project(path) else {
            return Err(Targets::failure(
                Resolution::Unsupported,
                "package_or_unmapped_specifier",
                Vec::new(),
            ));
        };
        if let Some(error) = &project.error {
            return Err(Targets::failure(
                Resolution::Unsupported,
                "invalid_project_configuration",
                vec![format!("{}: {error}", project.config)],
            ));
        }
        let mut steps = Vec::new();
        let matched = best_match(&project.paths, name);
        if let Some((mapping, captured)) = matched {
            for substitution in &mapping.substitutions {
                let Some(template) = substitution else {
                    steps.push(format!(
                        "{}: paths {} -> outside the repository",
                        mapping.declared_in, mapping.pattern
                    ));
                    return Err(Targets::failure(
                        Resolution::Unsupported,
                        "path_mapping_outside_repository",
                        steps,
                    ));
                };
                let substituted = match captured {
                    Some(text) if !text.is_empty() => template.replacen('*', text, 1),
                    _ => template.clone(),
                };
                let Some(base) = join("", &substituted) else {
                    steps.push(format!(
                        "{}: paths {} -> ./{template} leaves the repository",
                        mapping.declared_in, mapping.pattern
                    ));
                    return Err(Targets::failure(
                        Resolution::Unsupported,
                        "path_mapping_outside_repository",
                        steps,
                    ));
                };
                match self.find(path, &base) {
                    Found::File(target) => {
                        steps.push(format!(
                            "{}: paths {} -> ./{template} => {target}",
                            mapping.declared_in, mapping.pattern
                        ));
                        return Ok((target, steps));
                    }
                    Found::Missing => steps.push(format!(
                        "{}: paths {} -> ./{template} (no module)",
                        mapping.declared_in, mapping.pattern
                    )),
                    other => return Err(Self::failed(path, name, other, steps, true)),
                }
            }
        }
        if project.base_url_outside_root {
            steps.push(format!(
                "{}: baseUrl outside the repository",
                project.config
            ));
            return Err(Targets::failure(
                Resolution::Unsupported,
                "base_url_outside_repository",
                steps,
            ));
        }
        if let Some(base_url) = &project.base_url
            && let Some(located) = self.via_base_url(project, base_url, path, name, &mut steps)
        {
            return located;
        }
        Err(if matched.is_some() {
            Targets::failure(Resolution::Unresolved, "module_not_found", steps)
        } else {
            Targets::failure(
                Resolution::Unsupported,
                "package_or_unmapped_specifier",
                steps,
            )
        })
    }
    /// `baseUrl` lookup after `paths`: `None` when nothing exists there.
    fn via_base_url(
        &self,
        project: &Project,
        base_url: &str,
        path: &str,
        name: &str,
        steps: &mut Vec<String>,
    ) -> Option<Located> {
        let Some(base) = join(base_url, name) else {
            return Some(Err(Targets::failure(
                Resolution::Unsupported,
                "module_outside_repository",
                std::mem::take(steps),
            )));
        };
        match self.find(path, &base) {
            Found::File(target) => {
                steps.push(format!(
                    "{}: baseUrl {} => {target}",
                    project.config,
                    if base_url.is_empty() { "." } else { base_url }
                ));
                Some(Ok((target, std::mem::take(steps))))
            }
            Found::Missing => None,
            other => Some(Err(Self::failed(
                path,
                name,
                other,
                std::mem::take(steps),
                true,
            ))),
        }
    }
    fn failed(
        path: &str,
        name: &str,
        found: Found,
        mut steps: Vec<String>,
        mapped: bool,
    ) -> Targets {
        match found {
            Found::File(_) => unreachable!("a found file is not a failure"),
            Found::Missing => Targets::failure(Resolution::Unresolved, "module_not_found", steps),
            Found::Unindexed(file) => {
                steps.push(format!("{path}: module {name} => {file} (not indexed)"));
                Targets::failure(
                    Resolution::Unsupported,
                    "module_resolves_to_unindexed_file",
                    steps,
                )
            }
            Found::Excluded => Targets::failure(
                Resolution::Unsupported,
                if mapped {
                    "path_mapping_into_excluded_directory"
                } else {
                    "module_in_excluded_directory"
                },
                steps,
            ),
            Found::Conflict(files) => {
                steps.push(format!(
                    "{path}: module {name} => {} (several indexed candidates)",
                    files.join(", ")
                ));
                Targets {
                    ids: files.iter().map(|file| file_id(file)).collect(),
                    chain: steps,
                    reason: Some((Resolution::Ambiguous, "ambiguous_module_candidates")),
                    ..Targets::default()
                }
            }
        }
    }
    /// The nearest config directory at or above the importing file.
    fn project(&self, path: &str) -> Option<&Project> {
        let mut directory = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        loop {
            if let Some(project) = self.projects.get(directory) {
                return Some(project);
            }
            if directory.is_empty() {
                return None;
            }
            directory = directory.rsplit_once('/').map_or("", |(dir, _)| dir);
        }
    }
    /// The module a specifier's base path names, by the importer's language.
    fn find(&self, importer: &str, base: &str) -> Found {
        if Language::of(importer).is_some_and(Language::is_javascript) {
            self.find_javascript(base)
        } else {
            self.find_typescript(base)
        }
    }
    /// TypeScript's candidate order for a base path. The first existing file
    /// wins; a first hit that Repogi does not index (a declaration-only
    /// `.d.mts`, a package.json redirect) is unsupported rather than skipped.
    /// Several indexed `.ts`/`.tsx`/`index` forms stay unchosen, as in the
    /// relative profile.
    fn find_typescript(&self, base: &str) -> Found {
        let extension = Path::new(base).extension().and_then(|value| value.to_str());
        let (ordered, forms): (Vec<String>, Vec<String>) =
            if matches!(extension, Some("ts" | "tsx")) {
                (vec![base.to_owned()], vec![base.to_owned()])
            } else if let Some(stem) = base
                .strip_suffix(".js")
                .or_else(|| base.strip_suffix(".jsx"))
            {
                (
                    vec![
                        format!("{stem}.ts"),
                        format!("{stem}.tsx"),
                        format!("{stem}.d.ts"),
                        base.to_owned(),
                    ],
                    vec![format!("{stem}.ts"), format!("{stem}.tsx")],
                )
            } else if let Some(stem) = base
                .strip_suffix(".mjs")
                .or_else(|| base.strip_suffix(".cjs"))
            {
                let kind = if extension == Some("mjs") {
                    "mts"
                } else {
                    "cts"
                };
                (
                    vec![
                        format!("{stem}.{kind}"),
                        format!("{stem}.d.{kind}"),
                        base.to_owned(),
                    ],
                    Vec::new(),
                )
            } else {
                let files = ["ts", "tsx", "d.ts", "js", "jsx"];
                let mut ordered: Vec<String> =
                    files.iter().map(|ext| format!("{base}.{ext}")).collect();
                ordered.push(format!("{base}/package.json"));
                ordered.extend(files.iter().map(|ext| format!("{base}/index.{ext}")));
                let forms = ["ts", "tsx"]
                    .iter()
                    .flat_map(|ext| [format!("{base}.{ext}"), format!("{base}/index.{ext}")])
                    .collect();
                (ordered, forms)
            };
        let exists = |candidate: &String| {
            self.modules.contains_key(candidate) || self.resolution.other_files.contains(candidate)
        };
        // Candidates are tried in order. One inside a directory the indexer
        // never reads cannot be checked, so the lookup stops there; only the
        // directories of a candidate count (`coverage.ts` is not `coverage/`).
        let mut first = None;
        for candidate in &ordered {
            let directories = candidate.rsplit_once('/').map_or("", |(dir, _)| dir);
            if directories
                .split('/')
                .any(|part| EXCLUDED_DIRECTORIES.contains(&part))
            {
                return Found::Excluded;
            }
            if exists(candidate) {
                first = Some(candidate);
                break;
            }
        }
        let Some(first) = first else {
            return Found::Missing;
        };
        if !self.modules.contains_key(first) {
            return Found::Unindexed(first.clone());
        }
        let indexed: Vec<String> = forms
            .into_iter()
            .filter(|candidate| self.modules.contains_key(candidate))
            .collect();
        if indexed.len() > 1 {
            Found::Conflict(indexed)
        } else {
            Found::File(first.clone())
        }
    }
    /// JavaScript lookup. Runtimes and bundlers agree on little beyond this:
    /// a specifier with an extension names exactly that file (or, when it is
    /// absent, the TypeScript source of that output name), and an
    /// extensionless one tries files before directories. Where tools differ
    /// (`a.js` beside `a.ts` or `a.mjs`, several `index` files) the result is
    /// ambiguous instead of one tool's choice. `package.json` (`main`,
    /// `exports`) and unindexed files are unsupported; JSON modules are not
    /// tracked, so they stay unresolved.
    fn find_javascript(&self, base: &str) -> Found {
        const FILES: [&str; 8] = ["js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"];
        const INDEXES: [&str; 6] = ["js", "jsx", "mjs", "cjs", "ts", "tsx"];
        let name = base.rsplit('/').next().unwrap_or(base);
        let explicit = name
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && (FILES.contains(&ext) || ext == "json"));
        let groups: Vec<Vec<String>> = if explicit {
            // The named file wins. Without it, the module can only be the
            // TypeScript source compiled to that name (`./a.js` from `a.ts`),
            // which is how TypeScript-built projects import from JavaScript.
            let sources: &[&str] = match name.rsplit_once('.').map(|(_, ext)| ext) {
                Some("js" | "jsx") => &["ts", "tsx"],
                Some("mjs") => &["mts"],
                Some("cjs") => &["cts"],
                _ => &[],
            };
            let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
            vec![
                vec![base.to_owned()],
                sources.iter().map(|ext| format!("{stem}.{ext}")).collect(),
            ]
        } else {
            vec![
                FILES.iter().map(|ext| format!("{base}.{ext}")).collect(),
                std::iter::once(format!("{base}/package.json"))
                    .chain(INDEXES.iter().map(|ext| format!("{base}/index.{ext}")))
                    .collect(),
            ]
        };
        for group in groups {
            if group.iter().any(|candidate| {
                candidate
                    .rsplit_once('/')
                    .map_or("", |(dir, _)| dir)
                    .split('/')
                    .any(|part| EXCLUDED_DIRECTORIES.contains(&part))
            }) {
                return Found::Excluded;
            }
            let present: Vec<&String> = group
                .iter()
                .filter(|candidate| {
                    self.modules.contains_key(*candidate)
                        || self.resolution.other_files.contains(*candidate)
                })
                .collect();
            match present.as_slice() {
                [] => {}
                [one] if self.modules.contains_key(*one) => return Found::File((*one).clone()),
                [one] => return Found::Unindexed((*one).clone()),
                several => {
                    // A package.json or an unindexed sibling may redirect or win.
                    return match several
                        .iter()
                        .find(|candidate| !self.modules.contains_key(**candidate))
                    {
                        Some(unindexed) => Found::Unindexed((*unindexed).clone()),
                        None => Found::Conflict(several.iter().map(|c| (*c).clone()).collect()),
                    };
                }
            }
        }
        Found::Missing
    }
    /// Export lookup. A lookup that starts outside any cycle (empty `seen`)
    /// depends only on the modules, so it is computed once per rebind; lookups
    /// inside a re-export chain stay uncached because their cycle cut-offs
    /// depend on the path taken.
    fn export(&self, path: &str, name: &str, seen: &mut BTreeSet<(String, String)>) -> Targets {
        if !seen.is_empty() {
            return self.export_uncached(path, name, seen);
        }
        let key = (path.to_owned(), name.to_owned());
        if let Some(hit) = self.exports.borrow().get(&key) {
            return hit.clone();
        }
        let result = self.export_uncached(path, name, seen);
        self.exports.borrow_mut().insert(key, result.clone());
        result
    }
    fn export_uncached(
        &self,
        path: &str,
        name: &str,
        seen: &mut BTreeSet<(String, String)>,
    ) -> Targets {
        let key = (path.into(), name.into());
        if seen.contains(&key) {
            return Targets::default();
        }
        if seen.len() >= 64 {
            return Targets {
                unsupported: true,
                ..Targets::default()
            };
        }
        seen.insert(key.clone());
        let module = &self.modules[path];
        let explicit: Vec<_> = module
            .exports
            .iter()
            .filter(|export| export.name == name && export.name != "*")
            .collect();
        let mut result = Targets::default();
        if !module.complete {
            result.uncertain = true;
        }
        for export in &explicit {
            result.type_only |= export.type_only;
            if let Some(from) = &export.module {
                match self.module_path(path, from) {
                    Ok((target, steps)) => {
                        result.merge(self.export(&target, &export.local, seen));
                        result.chain.extend(steps);
                        result.chain.push(format!(
                            "{path}: export {name} from {from}:{}",
                            export.local
                        ));
                    }
                    Err(failure) => result.merge(failure),
                }
            } else {
                result.merge(self.local(path, &export.local, None, 0, true, seen));
            }
        }
        if explicit.is_empty() && name != "default" {
            for export in module.exports.iter().filter(|export| export.name == "*") {
                result.type_only |= export.type_only;
                if export.local != "*" {
                    result.unsupported = true;
                    continue;
                }
                if let Some(from) = export.module.as_deref() {
                    match self.module_path(path, from) {
                        Ok((target, steps)) => {
                            result.merge(self.export(&target, name, seen));
                            result.chain.extend(steps);
                        }
                        Err(failure) => result.merge(failure),
                    }
                }
            }
        }
        result.chain.push(format!("{path}: export {name}"));
        seen.remove(&key);
        result
    }
    fn local(
        &self,
        path: &str,
        name: &str,
        member: Option<&str>,
        offset: usize,
        top: bool,
        seen: &mut BTreeSet<(String, String)>,
    ) -> Targets {
        let bindings: Vec<_> = self.modules[path]
            .bindings
            .iter()
            .filter(|binding| {
                binding.name == name
                    && if top {
                        binding.top_level
                    } else {
                        binding.scope_start <= offset && offset < binding.scope_end
                    }
            })
            .collect();
        let smallest = bindings
            .iter()
            .map(|binding| binding.scope_end - binding.scope_start)
            .min();
        let bindings: Vec<_> = bindings
            .into_iter()
            .filter(|binding| Some(binding.scope_end - binding.scope_start) == smallest)
            .collect();
        let mut result = Targets::default();
        for binding in &bindings {
            result.merge(self.binding(path, binding, member, seen));
        }
        if bindings.len() > 1 {
            result.uncertain = true;
        }
        result
    }
    fn binding(
        &self,
        path: &str,
        binding: &Binding,
        member: Option<&str>,
        seen: &mut BTreeSet<(String, String)>,
    ) -> Targets {
        if binding.mutated {
            return Targets {
                unsupported: true,
                ..Targets::default()
            };
        }
        if let Some(member) = member
            && binding
                .mutated_members
                .iter()
                .any(|written| written == member || written == "*")
        {
            return Targets::failure(
                Resolution::Unsupported,
                "member_reassigned",
                vec![format!("{path}: {}.{member} is written", binding.name)],
            );
        }
        if binding.commonjs {
            return self.require_binding(path, binding, member, seen, true);
        }
        let mut result = if let Some(from) = binding.module.as_deref() {
            match self.module_path(path, from) {
                Ok((target, steps))
                    if Language::of(path).is_some_and(Language::is_javascript)
                        && self.commonjs_module(&target) =>
                {
                    // Node.js and bundlers expose `module.exports` as the default
                    // export and its statically detectable properties as names.
                    let mut result = match binding.imported.as_str() {
                        "" => Targets::target(file_id(&target)),
                        "*" => member.map_or_else(
                            || Targets::target(file_id(&target)),
                            |member| self.commonjs_export(&target, member, seen),
                        ),
                        "default" => match member {
                            None => self.commonjs_value(&target, seen),
                            Some(member) => self.commonjs_export(&target, member, seen),
                        },
                        name => self.member(self.commonjs_export(&target, name, seen), member),
                    };
                    result.chain.extend(steps);
                    result.chain.push(format!(
                        "{path}: import {} from CommonJS module {target}",
                        binding.imported
                    ));
                    if result.reason.is_none() {
                        result.reason = Some((Resolution::Resolved, "esm_import_of_commonjs"));
                    }
                    result
                }
                Ok((target, steps)) => {
                    let mut result = if binding.imported == "*" {
                        if let Some(member) = member {
                            self.export(&target, member, seen)
                        } else {
                            Targets::target(file_id(&target))
                        }
                    } else if binding.imported.is_empty() {
                        Targets::target(file_id(&target))
                    } else {
                        let result = self.export(&target, &binding.imported, seen);
                        self.member(result, member)
                    };
                    result.chain.extend(steps);
                    result
                }
                Err(failure) => failure,
            }
        } else if let Some(target) = &binding.target {
            self.member(Targets::target(target.clone()), member)
        } else {
            Targets {
                unsupported: true,
                ..Targets::default()
            }
        };
        result.type_only |= binding.type_only;
        result.chain.push(format!(
            "{path}: {} => {} {}",
            binding.name,
            binding.module.as_deref().unwrap_or("local"),
            binding.imported
        ));
        result
    }
    fn symbol(&self, id: &str) -> Option<&Symbol> {
        self.symbols.get(id).copied()
    }
    fn member(&self, mut result: Targets, member: Option<&str>) -> Targets {
        let Some(member) = member else {
            return result;
        };
        if result.status() != Resolution::Resolved {
            return result;
        }
        let id = result.ids.first().expect("resolved target");
        if self.symbol(id).is_none_or(|symbol| symbol.kind != "class") {
            return Targets {
                unsupported: true,
                ..Targets::default()
            };
        }
        let members: BTreeSet<_> = self
            .children
            .get(id.as_str())
            .into_iter()
            .flatten()
            .filter(|symbol| symbol.static_member && symbol.name.rsplit('.').next() == Some(member))
            .map(|symbol| symbol.id.clone())
            .collect();
        if let Some(hierarchy) = self.hierarchy.get()
            && (hierarchy
                .static_writes
                .contains(&(id.clone(), member.to_owned()))
                || hierarchy.static_writes.contains(&(id.clone(), "*".into())))
        {
            return Targets::failure(
                Resolution::Unsupported,
                "member_reassigned",
                vec![format!("{id}: static member {member} is written elsewhere")],
            );
        }
        result.ids = members;
        result.chain.push(format!("static member {member}"));
        result
    }
    /// Explicit export names of `path` and of every module reachable from it
    /// through `export * from` chains: a superset of the names an export lookup
    /// on `path` can resolve, used to skip lookups that cannot succeed.
    fn star_reachable_names(&self, path: &str) -> std::collections::HashSet<&str> {
        let mut names = std::collections::HashSet::new();
        let mut visited = BTreeSet::new();
        let mut stack = vec![path.to_owned()];
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            let Some((key, module)) = self.modules.get_key_value(&current) else {
                continue;
            };
            for export in &module.exports {
                if export.name != "*" {
                    names.insert(export.name.as_str());
                } else if export.local == "*"
                    && let Some(from) = export.module.as_deref()
                    && let Ok((target, _)) = self.module_path(key, from)
                {
                    stack.push(target);
                }
            }
        }
        names
    }
    fn export_edges(
        &self,
        path: &str,
        module: &Module,
        names: &BTreeSet<String>,
        edges: &mut Vec<SemanticEdge>,
    ) {
        for export in &module.exports {
            let kind = if export.module.is_some() {
                Relationship::ReExports
            } else {
                Relationship::Exports
            };
            let expanded: Vec<_> = if export.name == "*" {
                // The star's source module does not depend on the name.
                let source = if export.local == "*" {
                    export
                        .module
                        .as_deref()
                        .and_then(|from| self.module_path(path, from).ok())
                        .map(|(target, _)| target)
                } else {
                    None
                };
                // Only names some star-reachable module declares can resolve.
                let reachable = source
                    .as_deref()
                    .map(|target| self.star_reachable_names(target))
                    .unwrap_or_default();
                names
                    .iter()
                    .filter(|name| reachable.contains(name.as_str()))
                    .filter(|name| name.as_str() != "default")
                    .filter(|name| {
                        !module
                            .exports
                            .iter()
                            .any(|explicit| explicit.name == **name)
                    })
                    .filter(|name| {
                        source.as_deref().is_some_and(|target| {
                            !self
                                .export(target, name, &mut BTreeSet::new())
                                .ids
                                .is_empty()
                        })
                    })
                    .cloned()
                    .collect()
            } else {
                vec![export.name.clone()]
            };
            let mut emitted = false;
            for name in expanded {
                let result = self.export(path, &name, &mut BTreeSet::new());
                // Empty star branches do not claim nonexistent exports.
                if export.name == "*"
                    && result.ids.is_empty()
                    && !result.uncertain
                    && !result.unsupported
                {
                    continue;
                }
                edges.push(self.edge(
                    file_id(path),
                    name.clone(),
                    kind,
                    export.location.clone(),
                    result,
                    format!(
                        "export {name} via {}",
                        export.module.as_deref().unwrap_or("local")
                    ),
                ));
                emitted = true;
            }
            if !emitted {
                edges.push(self.edge(
                    file_id(path),
                    export.name.clone(),
                    kind,
                    export.location.clone(),
                    if export.local == "*" {
                        export
                            .module
                            .as_deref()
                            .map_or_else(Targets::default, |from| self.module(path, from))
                    } else {
                        Targets {
                            unsupported: true,
                            ..Targets::default()
                        }
                    },
                    "export * module (no statically known names at this site)".into(),
                ));
            }
        }
    }

    /// EXPORTS edges for a `CommonJS` module's static export table.
    fn commonjs_edges(&self, path: &str, module: &Module, edges: &mut Vec<SemanticEdge>) {
        let Some(table) = module
            .commonjs
            .as_ref()
            .filter(|_| self.commonjs_capable(path))
        else {
            return;
        };
        for export in &table.exports {
            let name = if export.name.is_empty() {
                "module.exports".to_owned()
            } else {
                export.name.clone()
            };
            let mut result = if table.dynamic.is_some() {
                Targets::failure(
                    Resolution::Unresolved,
                    "dynamic_commonjs_exports",
                    Vec::new(),
                )
            } else {
                self.commonjs_entry(path, export, &mut BTreeSet::new())
            };
            if result.reason.is_none() {
                result.reason = Some((Resolution::Resolved, "commonjs_export"));
            }
            edges.push(self.edge(
                file_id(path),
                name.clone(),
                Relationship::Exports,
                export.location.clone(),
                result,
                format!("CommonJS export {name}"),
            ));
        }
    }

    /// A resolved JSX name must denote a value; a type declaration cannot be a
    /// rendered component. Resolved JSX sites carry their own rule.
    fn jsx_rule(&self, edge: &mut SemanticEdge) {
        if edge.status != Resolution::Resolved {
            return;
        }
        let is_type = edge
            .target_id
            .as_deref()
            .and_then(|id| self.symbol(id))
            .is_some_and(|symbol| matches!(symbol.kind.as_str(), "interface" | "type_alias"));
        if is_type {
            edge.status = Resolution::Unsupported;
            edge.target_id = None;
            "jsx_target_is_a_type".clone_into(&mut edge.evidence.rule);
        } else {
            "jsx_component_reference".clone_into(&mut edge.evidence.rule);
        }
    }
    fn edge(
        &self,
        source: String,
        name: String,
        kind: Relationship,
        location: Location,
        mut result: Targets,
        syntax: String,
    ) -> SemanticEdge {
        let mut status = result.status();
        if status == Resolution::Resolved
            && matches!(kind, Relationship::Calls | Relationship::Extends)
        {
            let valid = result
                .ids
                .first()
                .and_then(|id| self.symbol(id))
                .is_some_and(|symbol| {
                    if kind == Relationship::Calls {
                        symbol.callable
                    } else {
                        symbol.kind == "class"
                    }
                });
            if !valid || result.type_only {
                status = Resolution::Unsupported;
            }
        }
        result.chain.sort();
        result.chain.dedup();
        let rule = match (status, result.reason) {
            (status, Some((reason_status, reason))) if status == reason_status => reason,
            (Resolution::Resolved, _) => "unique_static_binding",
            (Resolution::Ambiguous, _) => "multiple_static_targets",
            (Resolution::Unresolved, _) => "missing_module_export_or_binding_or_cycle",
            (Resolution::Unsupported, _) => "unsupported_or_mutable_binding_or_receiver",
        };
        SemanticEdge {
            source_id: source,
            target_id: if status == Resolution::Resolved {
                result.ids.pop_first()
            } else {
                None
            },
            target_text: name,
            kind,
            status,
            location,
            evidence: Evidence {
                syntax,
                rule: rule.into(),
                binding_chain: result.chain,
                dispatch: None,
            },
        }
    }
}

/// Instance dispatch and `CommonJS`.
impl Resolver<'_> {
    fn hierarchy(&self) -> &Hierarchy {
        self.hierarchy.get_or_init(|| self.build_hierarchy())
    }
    fn is_class(&self, result: &Targets) -> Option<String> {
        (result.status() == Resolution::Resolved)
            .then(|| result.ids.first().cloned())
            .flatten()
            .filter(|id| self.symbol(id).is_some_and(|symbol| symbol.kind == "class"))
    }
    fn bound(module: &Module, name: &str, offset: usize) -> bool {
        module.bindings.iter().any(|binding| {
            binding.name == name && binding.scope_start <= offset && offset < binding.scope_end
        })
    }
    /// Lookups here never ask for a member, so they cannot re-enter
    /// [`Self::hierarchy`] through static-member checks.
    #[allow(clippy::too_many_lines)] // One pass per fact family, kept together for review.
    fn build_hierarchy(&self) -> Hierarchy {
        let mut hierarchy = Hierarchy::default();
        for (path, module) in self.modules {
            let declared: Vec<&str> = module
                .classes
                .iter()
                .map(|class| class.id.as_str())
                .collect();
            for usage in module.uses.iter().filter(|usage| {
                usage.kind == Relationship::Extends
                    && self.classes.contains_key(usage.source_id.as_str())
            }) {
                let result = self.local(
                    path,
                    &usage.name,
                    usage.member.as_deref(),
                    usage.location.start_byte,
                    false,
                    &mut BTreeSet::new(),
                );
                let parent = if let Some(id) = self.is_class(&result) {
                    Super::Known(id)
                } else if (usage.member.is_none()
                    && usage
                        .name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                    && !Self::bound(module, &usage.name, usage.location.start_byte))
                    || matches!(result.reason, Some((_, "package_or_unmapped_specifier")))
                {
                    // A global (`Error`, `HTMLElement`) or a package import.
                    Super::External
                } else {
                    Super::Unknown
                };
                hierarchy.supers.insert(usage.source_id.clone(), parent);
            }
            if module.prototype_opaque {
                hierarchy
                    .tainted
                    .extend(declared.iter().map(|id| (*id).to_owned()));
            }
            for name in &module.prototype_uses {
                if !Self::bound(module, &name.name, name.offset) {
                    continue; // A global such as `Object.prototype`.
                }
                let result = self.local(
                    path,
                    &name.name,
                    None,
                    name.offset,
                    false,
                    &mut BTreeSet::new(),
                );
                if let Some(id) = self.is_class(&result) {
                    hierarchy.tainted.insert(id);
                } else if !(result.status() == Resolution::Resolved
                    && result
                        .ids
                        .first()
                        .and_then(|id| self.symbol(id))
                        .is_some_and(|symbol| symbol.kind == "function"))
                {
                    // Possibly an alias of a class declared here.
                    hierarchy
                        .tainted
                        .extend(declared.iter().map(|id| (*id).to_owned()));
                }
            }
            for write in &module.member_writes {
                let Some(property) = &write.property else {
                    // A computed write on an arbitrary object is not tracked
                    // (documented open-world limit), except through a class binding.
                    if let Some(receiver) = &write.receiver
                        && receiver != "this"
                    {
                        let result = self.local(
                            path,
                            receiver,
                            None,
                            write.offset,
                            false,
                            &mut BTreeSet::new(),
                        );
                        if let Some(id) = self.is_class(&result) {
                            hierarchy.static_writes.insert((id, "*".into()));
                        }
                    }
                    continue;
                };
                match write.receiver.as_deref() {
                    Some("module" | "exports") => {}
                    // `this.m = …` inside a class is an own field of that class.
                    Some("this")
                        if module
                            .classes
                            .iter()
                            .filter_map(|class| self.symbol(&class.id).map(|_| class))
                            .any(|class| Self::contains(&class.id, write.offset)) => {}
                    Some(receiver) if receiver != "this" => {
                        let result = self.local(
                            path,
                            receiver,
                            None,
                            write.offset,
                            false,
                            &mut BTreeSet::new(),
                        );
                        let target = result
                            .ids
                            .first()
                            .filter(|_| result.status() == Resolution::Resolved);
                        let kind = target
                            .and_then(|id| self.symbol(id))
                            .map(|symbol| symbol.kind.as_str());
                        match (target, kind) {
                            (Some(id), Some("class")) => {
                                hierarchy
                                    .static_writes
                                    .insert((id.clone(), property.clone()));
                            }
                            // Properties of a function object or a module namespace.
                            (Some(_), Some("function")) => {}
                            (Some(id), None) if id.starts_with("file:") => {}
                            _ => {
                                hierarchy.assigned.insert(property.clone());
                            }
                        }
                    }
                    _ => {
                        hierarchy.assigned.insert(property.clone());
                    }
                }
            }
        }
        for (class, parent) in &hierarchy.supers {
            match parent {
                Super::Known(parent) => hierarchy
                    .subclasses
                    .entry(parent.clone())
                    .or_default()
                    .push(class.clone()),
                Super::Unknown => {
                    for symbol in self.children.get(class.as_str()).into_iter().flatten() {
                        if let Some(name) = symbol.name.rsplit('.').next() {
                            hierarchy.unknown_overrides.insert(name.to_owned());
                        }
                    }
                    if let Some(info) = self.classes.get(class.as_str()) {
                        hierarchy.unknown_overrides.extend(
                            info.instance_fields
                                .iter()
                                .chain(&info.static_fields)
                                .cloned(),
                        );
                    }
                }
                Super::External => {}
            }
        }
        for children in hierarchy.subclasses.values_mut() {
            children.sort();
        }
        hierarchy
    }
    /// Does the class fact `id` span byte `offset` in its file?
    fn contains(id: &str, offset: usize) -> bool {
        // Fact IDs are `kind:path:start:end`.
        let mut parts = id.rsplitn(3, ':');
        let end = parts.next().and_then(|value| value.parse::<usize>().ok());
        let start = parts.next().and_then(|value| value.parse::<usize>().ok());
        matches!((start, end), (Some(start), Some(end)) if start <= offset && offset < end)
    }
    /// The methods `class` itself declares under `member`.
    fn declared(&self, class: &str, member: &str, static_member: bool) -> Vec<&Symbol> {
        self.children
            .get(class)
            .into_iter()
            .flatten()
            .filter(|symbol| {
                symbol.kind == "method"
                    && symbol.static_member == static_member
                    && symbol.name.rsplit('.').next() == Some(member)
            })
            .copied()
            .collect()
    }
    /// `this.m` / `super.m` inside a class member. The result is the declaration
    /// the class hierarchy statically selects; it is withheld whenever an
    /// indexed subclass overrides it, an own property may shadow it, the
    /// prototype chain may be modified, or the hierarchy is not fully known.
    #[allow(clippy::too_many_lines)] // Each early return is one documented safety rule.
    fn instance_member(&self, path: &str, usage: &Usage) -> Targets {
        let (Some(context), Some(member)) = (usage.context.as_ref(), usage.member.as_deref())
        else {
            return Targets::failure(
                Resolution::Unresolved,
                "receiver_not_bound_to_indexed_class",
                Vec::new(),
            );
        };
        let hierarchy = self.hierarchy();
        let is_super = usage.name == "super";
        let is_static = context.static_context;
        let private = member.starts_with('#');
        let mut chain = vec![format!(
            "{path}: {} in {} member of {}",
            usage.name,
            if is_static { "static" } else { "instance" },
            context.class
        )];
        let fail = |status, reason: &'static str, chain: Vec<String>| {
            Targets::failure(status, reason, chain)
        };
        // Every class above the context class, bounded; a cycle (code that
        // throws at class definition) never resolves.
        let mut ancestors: Vec<String> = Vec::new();
        let mut cursor = context.class.clone();
        while let Some(Super::Known(parent)) = hierarchy.supers.get(&cursor) {
            if ancestors.len() >= MAX_HIERARCHY
                || ancestors.contains(parent)
                || *parent == context.class
            {
                return fail(
                    Resolution::Unsupported,
                    "class_hierarchy_cyclic_or_too_deep",
                    chain,
                );
            }
            ancestors.push(parent.clone());
            cursor = parent.clone();
        }
        let top = hierarchy.supers.get(&cursor).cloned();
        // An own property named like the member (a field, `this.m = …`) on the
        // context class or above shadows or replaces any prototype method.
        if !is_super && !private {
            for class in std::iter::once(&context.class).chain(&ancestors) {
                let fields = self.classes.get(class.as_str()).map(|info| {
                    if is_static {
                        &info.static_fields
                    } else {
                        &info.instance_fields
                    }
                });
                if fields.is_some_and(|fields| fields.iter().any(|field| field == member)) {
                    chain.push(format!("{class}: own property {member}"));
                    return fail(
                        Resolution::Unsupported,
                        "own_property_may_shadow_method",
                        chain,
                    );
                }
            }
        }
        let start = if is_super {
            if hierarchy.tainted.contains(&context.class) {
                return fail(Resolution::Unsupported, "prototype_may_be_modified", chain);
            }
            match hierarchy.supers.get(&context.class) {
                Some(Super::Known(parent)) => {
                    chain.push(format!("{}: extends {parent}", context.class));
                    parent.clone()
                }
                Some(Super::External) => {
                    return fail(
                        Resolution::Unresolved,
                        "inherited_from_unindexed_class",
                        chain,
                    );
                }
                Some(Super::Unknown) => {
                    return fail(Resolution::Unsupported, "dynamic_superclass", chain);
                }
                None => return fail(Resolution::Unresolved, "no_superclass", chain),
            }
        } else {
            context.class.clone()
        };
        // Walk up from `start` to the first class declaring the member.
        let mut current = start;
        let mut visited = HashSet::new();
        let declared = loop {
            if !visited.insert(current.clone()) || visited.len() > MAX_HIERARCHY {
                return fail(
                    Resolution::Unsupported,
                    "class_hierarchy_cyclic_or_too_deep",
                    chain,
                );
            }
            if !private && hierarchy.tainted.contains(&current) {
                return fail(Resolution::Unsupported, "prototype_may_be_modified", chain);
            }
            let info = self.classes.get(current.as_str());
            if info.is_some_and(|info| info.dynamic_members) && (is_static || !is_super) {
                return fail(Resolution::Unsupported, "dynamic_own_members", chain);
            }
            // Static fields are own properties of each constructor on the way.
            if is_static && info.is_some_and(|info| info.static_fields.iter().any(|f| f == member))
            {
                return fail(
                    Resolution::Unsupported,
                    "own_property_may_shadow_method",
                    chain,
                );
            }
            let found = self.declared(&current, member, is_static);
            if !found.is_empty() {
                chain.push(format!("{current}: declares {member}"));
                break found;
            }
            if private {
                return fail(Resolution::Unresolved, "private_member_not_declared", chain);
            }
            match hierarchy.supers.get(&current) {
                Some(Super::Known(parent)) => {
                    chain.push(format!("{current}: no own {member}; extends {parent}"));
                    current = parent.clone();
                }
                Some(Super::External) => {
                    return fail(
                        Resolution::Unresolved,
                        "inherited_from_unindexed_class",
                        chain,
                    );
                }
                Some(Super::Unknown) => {
                    return fail(Resolution::Unsupported, "dynamic_superclass", chain);
                }
                None => {
                    return fail(
                        Resolution::Unresolved,
                        "member_not_declared_in_class_hierarchy",
                        chain,
                    );
                }
            }
        };
        let concrete: BTreeSet<String> = declared
            .iter()
            .filter(|symbol| !symbol.signature_only)
            .map(|symbol| symbol.id.clone())
            .collect();
        let mut overrides = BTreeSet::new();
        if !is_super && !private {
            // `this` may be the context class or any subclass instance: every
            // class above it and below it contributes own properties.
            if top == Some(Super::Unknown) {
                return fail(Resolution::Unsupported, "dynamic_superclass", chain);
            }
            let mut descendants = Vec::new();
            let mut pending = vec![context.class.clone()];
            while let Some(class) = pending.pop() {
                for child in hierarchy.subclasses.get(&class).into_iter().flatten() {
                    if descendants.contains(child) || *child == context.class {
                        continue;
                    }
                    if descendants.len() >= MAX_SUBCLASSES {
                        return fail(
                            Resolution::Unsupported,
                            "class_hierarchy_cyclic_or_too_deep",
                            chain,
                        );
                    }
                    descendants.push(child.clone());
                    pending.push(child.clone());
                }
            }
            for class in std::iter::once(&context.class)
                .chain(&ancestors)
                .chain(&descendants)
            {
                let Some(info) = self.classes.get(class.as_str()) else {
                    continue;
                };
                let fields = if is_static {
                    &info.static_fields
                } else {
                    &info.instance_fields
                };
                if info.dynamic_members {
                    return fail(Resolution::Unsupported, "dynamic_own_members", chain);
                }
                if fields.iter().any(|field| field == member) {
                    chain.push(format!("{class}: own property {member}"));
                    return fail(
                        Resolution::Unsupported,
                        "own_property_may_shadow_method",
                        chain,
                    );
                }
            }
            if descendants
                .iter()
                .any(|class| hierarchy.tainted.contains(class))
            {
                return fail(Resolution::Unsupported, "prototype_may_be_modified", chain);
            }
            if hierarchy.assigned.contains(member) {
                chain.push(format!("{member} is assigned on an untracked receiver"));
                return fail(Resolution::Unsupported, "member_assigned_elsewhere", chain);
            }
            if hierarchy.unknown_overrides.contains(member) {
                return fail(
                    Resolution::Unsupported,
                    "possible_override_in_dynamic_subclass",
                    chain,
                );
            }
            for class in &descendants {
                for symbol in self.declared(class, member, is_static) {
                    if !symbol.signature_only {
                        overrides.insert(symbol.id.clone());
                    }
                }
            }
        }
        if !overrides.is_empty() {
            chain.push(format!(
                "{member} is overridden in {} subclass declaration(s)",
                overrides.len()
            ));
            return Targets {
                ids: concrete.into_iter().chain(overrides).collect(),
                chain,
                reason: Some((Resolution::Ambiguous, "overridden_in_subclass")),
                ..Targets::default()
            };
        }
        if concrete.is_empty() {
            return fail(
                Resolution::Unresolved,
                "abstract_or_signature_only_member",
                chain,
            );
        }
        let ambiguous = concrete.len() > 1;
        Targets {
            ids: concrete,
            chain,
            reason: Some(if ambiguous {
                (Resolution::Ambiguous, "multiple_static_targets")
            } else if is_super {
                (Resolution::Resolved, "super_member_static_declaration")
            } else {
                (Resolution::Resolved, "this_member_static_declaration")
            }),
            ..Targets::default()
        }
    }

    /// The nearest `package.json` `type` at or above `path`'s directory.
    fn package_type(&self, path: &str) -> Option<&str> {
        let mut directory = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        loop {
            if let Some(kind) = self.resolution.package_types.get(directory) {
                return Some(kind);
            }
            if directory.is_empty() {
                return None;
            }
            directory = directory.rsplit_once('/').map_or("", |(dir, _)| dir);
        }
    }
    /// Node.js would load `path` as `CommonJS`: `.cjs`, or `.js`/`.jsx` without
    /// ES module syntax and not under `"type": "module"`. Never TypeScript or `.mjs`.
    // Extensions match exactly and case-sensitively, as everywhere in indexing.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    fn commonjs_capable(&self, path: &str) -> bool {
        let Some(module) = self.modules.get(path) else {
            return false;
        };
        if module.esm_syntax || !Language::of(path).is_some_and(Language::is_javascript) {
            return false;
        }
        if path.ends_with(".cjs") {
            return true;
        }
        !path.ends_with(".mjs") && matches!(self.package_type(path), None | Some("commonjs"))
    }
    fn commonjs_module(&self, path: &str) -> bool {
        self.commonjs_capable(path) && self.modules[path].commonjs.is_some()
    }
    /// A `require` binding. With `lexical`, a plain use whose `require` cannot be
    /// followed (package, unindexed, ES module target, not a `CommonJS` file)
    /// still names its local declaration, as before `require` was modeled;
    /// the IMPORTS edge (`lexical = false`) reports why the module was not followed.
    fn require_binding(
        &self,
        path: &str,
        binding: &Binding,
        member: Option<&str>,
        seen: &mut BTreeSet<(String, String)>,
        lexical: bool,
    ) -> Targets {
        let specifier = binding.module.as_deref().unwrap_or_default();
        let mut result = if self.commonjs_capable(path) {
            match self.module_path(path, specifier) {
                Err(failure) => failure,
                Ok((target, steps)) if !self.commonjs_capable(&target) => Targets::failure(
                    Resolution::Unsupported,
                    "require_of_es_module_or_typescript",
                    steps
                        .into_iter()
                        .chain([format!(
                            "{path}: require({specifier}) => {target} is not CommonJS"
                        )])
                        .collect(),
                ),
                Ok((target, steps)) => {
                    let mut result = if binding.imported == "*" {
                        match member {
                            None => self.commonjs_value(&target, seen),
                            Some(member) => self.commonjs_export(&target, member, seen),
                        }
                    } else {
                        self.member(
                            self.commonjs_export(&target, &binding.imported, seen),
                            member,
                        )
                    };
                    result.chain.extend(steps);
                    result
                        .chain
                        .push(format!("{path}: require({specifier}) => {target}"));
                    result
                }
            }
        } else {
            Targets::failure(
                Resolution::Unsupported,
                "require_outside_commonjs_module",
                vec![format!(
                    "{path}: not loaded as CommonJS (ES module syntax or package type)"
                )],
            )
        };
        if lexical
            && member.is_none()
            && result.status() != Resolution::Resolved
            && let Some(target) = &binding.target
        {
            let mut local = Targets::target(target.clone());
            local.chain.push(format!(
                "{path}: {} = require({specifier}) not followed; local declaration",
                binding.name
            ));
            return local;
        }
        if result.reason.is_none() {
            result.reason = Some((Resolution::Resolved, "commonjs_require_binding"));
        }
        result.chain.push(format!(
            "{path}: {} = require({specifier}){}",
            binding.name,
            if binding.imported == "*" {
                String::new()
            } else {
                format!(".{}", binding.imported)
            }
        ));
        result
    }
    /// The `module.exports` value of a `CommonJS` module: its single assigned
    /// value when that is known, else the module's exports object itself.
    fn commonjs_value(&self, path: &str, seen: &mut BTreeSet<(String, String)>) -> Targets {
        let table = self.modules[path].commonjs.as_ref();
        match table.and_then(|table| table.exports.iter().find(|export| export.name.is_empty())) {
            Some(_) if table.is_some_and(|table| table.dynamic.is_some()) => Targets::failure(
                Resolution::Unresolved,
                "dynamic_commonjs_exports",
                Vec::new(),
            ),
            Some(export) => self.commonjs_entry(path, export, seen),
            None => Targets::target(file_id(path)),
        }
    }
    /// A named `CommonJS` export: an explicit `exports.name`, else a static member
    /// of an assigned class value.
    fn commonjs_export(
        &self,
        path: &str,
        name: &str,
        seen: &mut BTreeSet<(String, String)>,
    ) -> Targets {
        let module = &self.modules[path];
        if !module.complete {
            return Targets {
                uncertain: true,
                ..Targets::default()
            };
        }
        let Some(table) = module.commonjs.as_ref() else {
            return Targets::failure(
                Resolution::Unresolved,
                "missing_commonjs_export",
                Vec::new(),
            );
        };
        if let Some(reason) = &table.dynamic {
            return Targets::failure(
                Resolution::Unresolved,
                "dynamic_commonjs_exports",
                vec![format!("{path}: exports are {reason}")],
            );
        }
        if let Some(export) = table.exports.iter().find(|export| export.name == name) {
            return self.commonjs_entry(path, export, seen);
        }
        if let Some(whole) = table.exports.iter().find(|export| export.name.is_empty()) {
            if whole.module.is_some() && whole.member.is_none() {
                // `module.exports = require("./other")` re-exports that module.
                return self.commonjs_entry_member(path, whole, name, seen);
            }
            return self.member(self.commonjs_entry(path, whole, seen), Some(name));
        }
        Targets::failure(
            Resolution::Unresolved,
            "missing_commonjs_export",
            vec![format!("{path}: no CommonJS export {name}")],
        )
    }
    fn commonjs_entry_member(
        &self,
        path: &str,
        export: &CommonJsExport,
        name: &str,
        seen: &mut BTreeSet<(String, String)>,
    ) -> Targets {
        let key = (path.to_owned(), format!("commonjs:{name}"));
        if seen.contains(&key) || seen.len() >= 64 {
            return Targets::failure(
                Resolution::Unsupported,
                "commonjs_cycle_or_depth",
                Vec::new(),
            );
        }
        let specifier = export.module.as_deref().unwrap_or_default();
        match self.module_path(path, specifier) {
            Ok((target, steps)) if self.commonjs_capable(&target) => {
                seen.insert(key.clone());
                let mut result = self.commonjs_export(&target, name, seen);
                seen.remove(&key);
                result.chain.extend(steps);
                result
                    .chain
                    .push(format!("{path}: module.exports = require({specifier})"));
                result
            }
            Ok(_) => Targets::failure(
                Resolution::Unsupported,
                "require_of_es_module_or_typescript",
                Vec::new(),
            ),
            Err(failure) => failure,
        }
    }
    fn commonjs_entry(
        &self,
        path: &str,
        export: &CommonJsExport,
        seen: &mut BTreeSet<(String, String)>,
    ) -> Targets {
        let label = if export.name.is_empty() {
            "module.exports"
        } else {
            export.name.as_str()
        };
        let mut result = if let Some(local) = &export.local {
            self.local(path, local, None, 0, true, seen)
        } else if let Some(specifier) = &export.module {
            let key = (path.to_owned(), format!("commonjs:{label}"));
            if seen.contains(&key) || seen.len() >= 64 {
                return Targets::failure(
                    Resolution::Unsupported,
                    "commonjs_cycle_or_depth",
                    Vec::new(),
                );
            }
            match self.module_path(path, specifier) {
                Ok((target, steps)) if self.commonjs_capable(&target) => {
                    seen.insert(key.clone());
                    let mut result = match &export.member {
                        Some(member) => self.commonjs_export(&target, member, seen),
                        None => self.commonjs_value(&target, seen),
                    };
                    seen.remove(&key);
                    result.chain.extend(steps);
                    result
                }
                Ok(_) => Targets::failure(
                    Resolution::Unsupported,
                    "require_of_es_module_or_typescript",
                    Vec::new(),
                ),
                Err(failure) => failure,
            }
        } else {
            Targets::failure(
                Resolution::Unresolved,
                "commonjs_export_value_not_a_declaration",
                Vec::new(),
            )
        };
        result
            .chain
            .push(format!("{path}: CommonJS export {label}"));
        result
    }
}

impl<'a> Resolver<'a> {
    fn new(modules: &'a Modules, resolution: &'a ResolutionContext) -> Self {
        let mut symbols = HashMap::new();
        let mut children: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        for symbol in modules.values().flat_map(|module| &module.symbols) {
            symbols.entry(symbol.id.as_str()).or_insert(symbol);
            if let Some(parent) = &symbol.parent {
                children.entry(parent.as_str()).or_default().push(symbol);
            }
        }
        Self {
            modules,
            resolution,
            projects: resolution
                .projects
                .iter()
                .map(|project| (project.dir.as_str(), project))
                .collect(),
            symbols,
            children,
            exports: RefCell::new(HashMap::new()),
            classes: modules
                .values()
                .flat_map(|module| &module.classes)
                .map(|class| (class.id.as_str(), class))
                .collect(),
            hierarchy: OnceCell::new(),
        }
    }
}

#[allow(clippy::too_many_lines)] // The single ordered pass over every module.
pub fn resolve(modules: &Modules, resolution: &ResolutionContext) -> Vec<SemanticEdge> {
    let resolver = Resolver::new(modules, resolution);
    // Whole-graph write and inheritance facts first, so every member lookup
    // below sees the same invalidation state.
    resolver.hierarchy();
    let mut edges = Vec::new();
    // Enumerate all explicit export names to expand stars without inventing names.
    let names: BTreeSet<_> = modules
        .values()
        .flat_map(|module| &module.exports)
        .filter(|export| export.name != "*")
        .map(|export| export.name.clone())
        .collect();
    for (path, module) in modules {
        for binding in module
            .bindings
            .iter()
            .filter(|binding| binding.module.is_some())
        {
            let mut result = if binding.commonjs && !binding.mutated {
                resolver.require_binding(path, binding, None, &mut BTreeSet::new(), false)
            } else {
                resolver.binding(path, binding, None, &mut BTreeSet::new())
            };
            if binding.imported.is_empty() {
                result = resolver.module(path, binding.module.as_deref().unwrap_or_default());
            }
            edges.push(resolver.edge(
                file_id(path),
                binding.name.clone(),
                Relationship::Imports,
                binding.location.clone(),
                result,
                format!(
                    "import {} from {} ({})",
                    binding.name,
                    binding.module.as_deref().unwrap_or_default(),
                    binding.imported
                ),
            ));
        }
        resolver.export_edges(path, module, &names, &mut edges);
        resolver.commonjs_edges(path, module, &mut edges);
        for usage in &module.uses {
            let instance =
                matches!(usage.name.as_str(), "this" | "super") && usage.member.is_some();
            let mut result = if instance {
                resolver.instance_member(path, usage)
            } else {
                resolver.local(
                    path,
                    &usage.name,
                    usage.member.as_deref(),
                    usage.location.start_byte,
                    false,
                    &mut BTreeSet::new(),
                )
            };
            // Type-only imports cannot establish value calls, class inheritance or a
            // runtime JSX component.
            if (matches!(usage.kind, Relationship::Calls | Relationship::Extends) || usage.jsx)
                && module
                    .bindings
                    .iter()
                    .any(|binding| binding.name == usage.name && binding.type_only)
            {
                result.unsupported = true;
            }
            let expression = usage.syntax.clone();
            let syntax = if usage.jsx {
                format!("<{expression}> JSX element")
            } else {
                expression.clone()
            };
            let mut edge = resolver.edge(
                usage.source_id.clone(),
                expression,
                usage.kind,
                usage.location.clone(),
                result,
                syntax,
            );
            if usage.jsx {
                resolver.jsx_rule(&mut edge);
            }
            if instance && edge.status == Resolution::Resolved {
                edge.evidence.dispatch = Some("static_declaration".into());
            }
            edges.push(edge);
        }
    }
    edges.sort_by(|a, b| {
        (
            &a.location.path,
            a.location.start_byte,
            &a.source_id,
            format!("{:?}", a.kind),
            &a.target_text,
            &a.target_id,
        )
            .cmp(&(
                &b.location.path,
                b.location.start_byte,
                &b.source_id,
                format!("{:?}", b.kind),
                &b.target_text,
                &b.target_id,
            ))
    });
    edges.dedup();
    edges
}

/// Rebuild inside the same transaction as files, FTS and legacy edges.
pub fn persist(
    transaction: &rusqlite::Transaction<'_>,
    resolution: &ResolutionContext,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let modules: Modules = {
        let mut query =
            transaction.prepare("SELECT path,data FROM semantic_modules ORDER BY path")?;
        query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (path, data) = row?;
                Ok((path, serde_json::from_str(&data)?))
            })
            .collect::<anyhow::Result<_>>()?
    };
    let loaded = started.elapsed();
    let edges = resolve(&modules, resolution);
    let resolved = started.elapsed();
    transaction.execute("DELETE FROM semantic_edges", [])?;
    let mut insert = transaction.prepare(
        "INSERT INTO semantic_edges(ordinal,path,source_id,target_id,data) VALUES(?1,?2,?3,?4,?5)",
    )?;
    for (ordinal, edge) in edges.iter().enumerate() {
        insert.execute(rusqlite::params![
            i64::try_from(ordinal)?,
            edge.location.path,
            edge.source_id,
            edge.target_id,
            serde_json::to_string(edge)?
        ])?;
    }
    profile(&[
        ("rebind_load_modules", loaded),
        ("rebind_resolve", resolved.saturating_sub(loaded)),
        (
            "rebind_write_edges",
            started.elapsed().saturating_sub(resolved),
        ),
    ]);
    Ok(())
}

/// Opt-in phase timings (`CODE_INTEL_PROFILE=1`) on stderr; never in outputs.
pub fn profile(phases: &[(&str, std::time::Duration)]) {
    if std::env::var_os("CODE_INTEL_PROFILE").is_some_and(|value| value == "1") {
        for (name, elapsed) in phases {
            eprintln!("profile {name} {:.1}", elapsed.as_secs_f64() * 1000.0);
        }
    }
}

/// TypeScript's `paths` selection: an exact pattern wins; otherwise the
/// wildcard pattern with the longest prefix, the first declared on a tie.
/// Returns the mapping and the text matched by `*` (`None` for exact).
fn best_match<'a>(
    paths: &'a [PathMapping],
    name: &'a str,
) -> Option<(&'a PathMapping, Option<&'a str>)> {
    if let Some(exact) = paths
        .iter()
        .find(|mapping| !mapping.pattern.contains('*') && mapping.pattern == name)
    {
        return Some((exact, None));
    }
    let mut best: Option<(&PathMapping, &str, usize)> = None;
    for mapping in paths {
        let Some((prefix, suffix)) = mapping.pattern.split_once('*') else {
            continue;
        };
        if name.len() >= prefix.len() + suffix.len()
            && name.starts_with(prefix)
            && name.ends_with(suffix)
            && best.is_none_or(|(_, _, length)| prefix.len() > length)
        {
            best = Some((
                mapping,
                &name[prefix.len()..name.len() - suffix.len()],
                prefix.len(),
            ));
        }
    }
    best.map(|(mapping, captured, _)| (mapping, Some(captured)))
}

/// Join and normalize `/`-separated paths below the root; `None` on escape.
fn join(base: &str, relative: &str) -> Option<String> {
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
