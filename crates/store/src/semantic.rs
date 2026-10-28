//! A bounded native static binder over persisted AST facts. No source is executed.
use code_intel_core::{
    file_id,
    semantic::{
        Binding, Evidence, Location, Module, PathMapping, Project, Relationship, Resolution,
        ResolutionContext, SemanticEdge, Symbol,
    },
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
};

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
            match self.find(&base) {
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
                match self.find(&base) {
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
        if let Some(base_url) = &project.base_url {
            if let Some(located) = self.via_base_url(project, base_url, path, name, &mut steps) {
                return located;
            }
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
        match self.find(&base) {
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
    /// TypeScript's candidate order for a base path. The first existing file
    /// wins; a first hit that Atlas does not index (JavaScript, `.mts`, a
    /// package.json redirect) is unsupported rather than skipped. Several indexed
    /// `.ts`/`.tsx`/`index` forms stay unchosen, as in the relative profile.
    fn find(&self, base: &str) -> Found {
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
        let mut result = if let Some(from) = binding.module.as_deref() {
            match self.module_path(path, from) {
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
                } else if export.local == "*" {
                    if let Some(from) = export.module.as_deref() {
                        if let Ok((target, _)) = self.module_path(key, from) {
                            stack.push(target);
                        }
                    }
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
            },
        }
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
        }
    }
}

pub fn resolve(modules: &Modules, resolution: &ResolutionContext) -> Vec<SemanticEdge> {
    let resolver = Resolver::new(modules, resolution);
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
            let mut result = resolver.binding(path, binding, None, &mut BTreeSet::new());
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
        for usage in &module.uses {
            let mut result = resolver.local(
                path,
                &usage.name,
                usage.member.as_deref(),
                usage.location.start_byte,
                false,
                &mut BTreeSet::new(),
            );
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
