//! Symbol lookup and bounded typed-graph navigation for agent adapters.
//!
//! Only persisted Phase 2 semantic edges are traversed. Resolved edges are the
//! only confirmed bindings; unresolved/ambiguous/unsupported sites are reported
//! separately with null targets. The frozen heuristic graph is not consulted.
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    time::Instant,
};

use anyhow::{Result, ensure};
use code_intel_core::{
    ContextResponse, Fact, NavigationEvidence,
    semantic::{Location, Relationship, Resolution, SemanticEdge},
};
use code_intel_store::Store;
use serde::{Deserialize, Serialize};

use crate::{Detail, Format, Options, Strategy, agent};

/// Maximum declarations a name lookup returns.
pub const MAX_MATCHES: usize = 16;
/// Typed edges read per traversed node.
pub const PER_NODE: usize = 128;
/// Upper bound on distinct traversal links collected before budgeting.
pub const MAX_LINKS: usize = 512;
/// Upper bound on unconfirmed sites collected before budgeting.
pub const MAX_UNRESOLVED: usize = 64;
pub const MAX_DEPTH: usize = 3;
pub const MAX_NODES: usize = 200;
/// `atlas_dependencies` default `max_nodes`; the next-hop request relies on it.
pub const DEFAULT_NODES: usize = 50;
pub const MAX_REFERENCES: usize = 500;
const NEXT_HOP_DEPTH: usize = 2;
const NEXT_HOP_KINDS: [Relationship; 3] = [
    Relationship::Calls,
    Relationship::References,
    Relationship::Extends,
];
const LINE_BYTES: usize = 160;

/// A typed, user-facing failure that adapters can map to a stable code.
#[derive(Debug)]
pub struct NavigationError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for NavigationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NavigationError {}

fn fail(code: &'static str, message: String) -> anyhow::Error {
    NavigationError { code, message }.into()
}

#[derive(Debug, Default, Clone)]
pub struct Selector {
    pub id: Option<String>,
    pub name: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SymbolSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}

impl From<&Fact> for SymbolSummary {
    fn from(fact: &Fact) -> Self {
        Self {
            id: fact.id.clone(),
            name: fact.name.clone(),
            kind: fact.kind.clone(),
            path: fact.path.clone(),
            start_line: fact.start_line,
            end_line: fact.end_line,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ByteBudget {
    pub max_bytes: usize,
    pub method: &'static str,
}

impl ByteBudget {
    const fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            method: "estimated_utf8_bytes",
        }
    }
}

/// Matching declarations, exact names first. `truncated` means more matched.
pub fn find(store: &Store, selector: &Selector) -> Result<(Vec<Fact>, bool)> {
    if let Some(id) = &selector.id {
        let Some(fact) = store.fact(id)? else {
            return Err(fail(
                "symbol_not_found",
                format!(
                    "symbol ID {id} is absent from this index snapshot; IDs change when files are edited, so look it up again by name or with atlas_context"
                ),
            ));
        };
        if selector
            .path
            .as_ref()
            .is_some_and(|path| *path != fact.path)
        {
            return Err(fail(
                "symbol_not_found",
                format!("symbol ID {id} is not in path {}", fact.path),
            ));
        }
        return Ok((vec![fact], false));
    }
    let Some(name) = selector.name.as_deref().filter(|name| !name.is_empty()) else {
        return Err(fail(
            "invalid_arguments",
            "provide a symbol `id` or `name`".into(),
        ));
    };
    let mut facts = store.facts_named(
        name,
        selector.path.as_deref(),
        u32::try_from(MAX_MATCHES + 1)?,
    )?;
    let truncated = facts.len() > MAX_MATCHES;
    facts.truncate(MAX_MATCHES);
    if facts.is_empty() {
        let suggestions = if store.has_semantic()? {
            store.targets_named(name, selector.path.as_deref(), 4)?
        } else {
            Vec::new()
        };
        let mut recovery = String::new();
        for candidate in suggestions {
            write!(
                recovery,
                "\nResolved use-site candidate (not a declaration named {name}): {} in {}; retry with id={}",
                candidate.name, candidate.path, candidate.id
            )?;
        }
        return Err(fail(
            "symbol_not_found",
            format!(
                "no indexed declaration named `{name}`{}; export aliases may name a different declaration. Use atlas_context to discover a declaration ID, or native Read/Grep (.ts/.tsx indexed only).{recovery}",
                selector
                    .path
                    .as_ref()
                    .map_or_else(String::new, |path| format!(" in {path}"))
            ),
        ));
    }
    Ok((facts, truncated))
}

/// Exactly one declaration, or an `ambiguous_symbol` error listing candidates.
pub fn unique(store: &Store, selector: &Selector) -> Result<Fact> {
    let (mut facts, truncated) = find(store, selector)?;
    if facts.len() == 1 {
        return Ok(facts.remove(0));
    }
    let mut message = format!(
        "{}{} declarations match; pass `id` (or `path`) to choose one:",
        facts.len(),
        if truncated { "+" } else { "" }
    );
    for fact in facts.iter().take(10) {
        write!(
            message,
            "\n- {} · {} · {}:{} · id={}",
            fact.name,
            fact.kind,
            fact.path,
            agent::lines(fact.start_line, fact.end_line),
            fact.id
        )?;
    }
    Err(fail("ambiguous_symbol", message))
}

/// Source for matching declarations through the existing selection packer.
pub fn symbol(
    store: &Store,
    selector: &Selector,
    detail: Detail,
    max_bytes: usize,
    format: Format,
) -> Result<ContextResponse> {
    let (facts, truncated) = find(store, selector)?;
    let label = selector
        .id
        .as_deref()
        .or(selector.name.as_deref())
        .unwrap_or_default();
    let options = Options {
        strategy: Strategy::Optimized,
        detail,
        select: facts.into_iter().map(|fact| fact.id).collect(),
        skip_dependencies: true,
        selection_truncated: truncated,
    };
    crate::signal::context(
        store,
        &format!("symbol {label}"),
        max_bytes,
        format,
        &options,
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct Site {
    pub source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    pub kind: Relationship,
    pub status: Resolution,
    pub target_text: String,
    pub location: Location,
    pub rule: String,
    pub binding_chain: Vec<String>,
    /// The use-site line from persisted source, when the index holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// A JSX element's component name (a REFERENCES use, not a call).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub jsx: bool,
}

#[derive(Debug, Clone, Serialize)]
// Query scope, budget truncation and count certainty are independent axes.
#[allow(clippy::struct_excessive_bools)]
pub struct References {
    pub schema_version: u32,
    pub index_revision: u64,
    pub budget: ByteBudget,
    pub used_bytes: usize,
    pub symbol: SymbolSummary,
    pub resolved: Vec<Site>,
    pub unresolved: Vec<Site>,
    pub truncated: bool,
    pub omitted: usize,
    pub limit_reached: bool,
    pub available_resolved: usize,
    pub available_unresolved: usize,
    pub counts_exact: bool,
    pub include_unresolved: bool,
    pub navigation: NavigationEvidence,
    /// Agent text only: the follow-up request printed for this exact ID.
    #[serde(skip)]
    pub next_hop: Option<NextHop>,
}

/// Which follow-up a references response suggests (agent text only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextHop {
    /// The printed upstream `atlas_dependencies` walk finishes within its limits.
    Dependencies,
    /// That walk would stop at a work limit with lower-bound counts, so
    /// per-consumer `atlas_references` is suggested, starting from this consumer ID.
    References(String),
}

#[derive(Debug, Clone)]
pub struct ReferenceOptions {
    pub kinds: Vec<Relationship>,
    pub limit: usize,
    pub include_unresolved: bool,
    /// Offer the upstream traversal when confirmed consumer declarations are
    /// listed; adapters set it only when `atlas_dependencies` is available.
    pub next_hop: bool,
}

pub fn references(
    store: &Store,
    selector: &Selector,
    options: &ReferenceOptions,
    max_bytes: usize,
    format: Format,
) -> Result<(References, String)> {
    let mut profile = (std::env::var_os("ATLAS_PROFILE_NAVIGATION").as_deref()
        == Some(std::ffi::OsStr::new("1")))
    .then(Instant::now);
    ensure!(
        (1..=MAX_REFERENCES).contains(&options.limit),
        "limit must be between 1 and {MAX_REFERENCES}"
    );
    let fact = unique(store, selector)?;
    let kinds = kind_names(&options.kinds)?;
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    let mut facts = BTreeMap::new();
    let incoming = store.semantic_incoming(&fact.id, &kinds, u32::try_from(options.limit + 1)?)?;
    let mut limit_reached = incoming.len() > options.limit;
    // A non-exhausted LIMIT+1 query already establishes the exact scope count.
    // Only a capped enumeration needs another indexed-edge count query.
    let available_resolved = if limit_reached {
        store.semantic_incoming_count(&fact.id, &kinds)?
    } else {
        incoming.len()
    };
    let resolved = incoming
        .into_iter()
        .take(options.limit)
        .map(|edge| site(store, &mut facts, edge))
        .collect::<Result<Vec<_>>>()?;
    let mut unresolved = Vec::new();
    let mut available_unresolved = 0;
    if options.include_unresolved {
        let cap = options.limit.min(MAX_UNRESOLVED);
        let base = fact.name.rsplit('.').next().unwrap_or(&fact.name);
        let sites = store.semantic_unresolved_named(base, &kinds, u32::try_from(cap + 1)?)?;
        limit_reached |= sites.len() > cap;
        available_unresolved = if sites.len() > cap {
            store.semantic_unresolved_count(base, &kinds)?
        } else {
            sites.len()
        };
        for edge in sites.into_iter().take(cap) {
            unresolved.push(site(store, &mut facts, edge)?);
        }
    }
    profile_stage(&mut profile, "direct_reference_enumeration");
    let full = References {
        schema_version: code_intel_core::SCHEMA_VERSION,
        index_revision: store.revision()?,
        budget: ByteBudget::new(max_bytes),
        used_bytes: 0,
        symbol: SymbolSummary::from(&fact),
        resolved,
        unresolved,
        truncated: false,
        omitted: 0,
        limit_reached,
        available_resolved,
        available_unresolved,
        counts_exact: true,
        include_unresolved: options.include_unresolved,
        navigation: crate::signal::Inspection::new(store, [])?.project([]),
        next_hop: None,
    };
    let inspection = crate::signal::Inspection::new(
        store,
        std::iter::once(fact.path.clone()).chain(
            full.resolved
                .iter()
                .chain(&full.unresolved)
                .map(|site| site.location.path.clone()),
        ),
    )?;
    let total = full.resolved.len() + full.unresolved.len();
    let fitted = fit(
        total,
        max_bytes,
        |kept| {
            let mut view = full.clone();
            let resolved = kept.min(view.resolved.len());
            view.resolved.truncate(resolved);
            view.unresolved.truncate(kept - resolved);
            view.omitted = full.available_resolved + full.available_unresolved - kept;
            view.truncated = view.omitted > 0 || view.limit_reached;
            view.navigation = inspection.project(
                std::iter::once(fact.path.clone()).chain(
                    view.resolved
                        .iter()
                        .chain(&view.unresolved)
                        .map(|site| site.location.path.clone()),
                ),
            );
            view
        },
        |view, used| view.used_bytes = used,
        |view| render_references(view, format),
    )?;
    profile_stage(&mut profile, "reference_freshness_and_fit");
    if options.next_hop && format == Format::Agent {
        with_next_hop(store, fitted, max_bytes, format, profile)
    } else {
        Ok(fitted)
    }
}

/// Opt-in offline diagnostics on stderr; never changes the query or writes state.
fn profile_stage(started: &mut Option<Instant>, stage: &str) {
    if let Some(start) = started {
        eprintln!(
            "atlas_navigation_profile {{\"stage\":\"{stage}\",\"ms\":{}}}",
            start.elapsed().as_secs_f64() * 1000.0
        );
        *start = Instant::now();
    }
}

/// Append the next-hop line only into leftover budget, so it never displaces a
/// returned site, and only when a confirmed consumer declaration is listed.
/// The printed walk is run first: when it would stop at a work limit, the line
/// suggests per-consumer references instead of lower-bound traversal counts.
fn with_next_hop(
    store: &Store,
    fitted: (References, String),
    max_bytes: usize,
    format: Format,
    mut profile: Option<Instant>,
) -> Result<(References, String)> {
    let consumer = first_consumer(&fitted.0);
    profile_stage(&mut profile, "eligible_consumer");
    let Some(consumer) = consumer else {
        return Ok(fitted);
    };
    // Both possible lines use the very same renderer as the final response.
    // Adding a line cannot shrink the settled byte-count header. If even the
    // shorter line exceeds the remaining budget, neither choice can be shown.
    let dependencies = next_hop_line(&fitted.0, &NextHop::Dependencies)?;
    let references = NextHop::References(consumer);
    let per_consumer = next_hop_line(&fitted.0, &references)?;
    let cannot_fit = dependencies.len().min(per_consumer.len()) > max_bytes - fitted.1.len();
    profile_stage(&mut profile, "hint_budget_preflight");
    if cannot_fit {
        return Ok(fitted);
    }
    let completes = next_hop_walk_completes(store, &fitted.0.symbol.id)?;
    profile_stage(&mut profile, "bounded_hint_walk");
    let mut hinted = fitted.0.clone();
    hinted.next_hop = Some(if completes {
        NextHop::Dependencies
    } else {
        references
    });
    let hinted = settle(
        hinted,
        |view, used| view.used_bytes = used,
        |view| render_references(view, format),
    )?;
    profile_stage(&mut profile, "hint_format_and_append");
    Ok(if hinted.1.len() <= max_bytes {
        hinted
    } else {
        fitted
    })
}

/// The first returned, resolved call/reference/extends site from a declaration:
/// the same edges an upstream `atlas_dependencies` walk follows.
fn first_consumer(view: &References) -> Option<String> {
    if view.symbol.kind == "file" {
        return None;
    }
    view.resolved
        .iter()
        .find(|site| site.source_name.is_some() && NEXT_HOP_KINDS.contains(&site.kind))
        .map(|site| site.source_id.clone())
}

/// Whether the printed request (upstream, depth 2, default kinds and
/// `max_nodes`, resolved edges only) reports exact counts.
fn next_hop_walk_completes(store: &Store, id: &str) -> Result<bool> {
    let options = DependencyOptions {
        direction: Direction::Upstream,
        depth: NEXT_HOP_DEPTH,
        kinds: NEXT_HOP_KINDS.to_vec(),
        max_nodes: DEFAULT_NODES,
        include_unresolved: false,
    };
    let kinds = kind_names(&options.kinds)?;
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    Ok(!walk(store, id, &options, &kinds, &mut BTreeMap::new())?.limit_reached)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Downstream,
    Upstream,
    Both,
}

#[derive(Debug, Clone)]
pub struct DependencyOptions {
    pub direction: Direction,
    pub depth: usize,
    pub kinds: Vec<Relationship>,
    pub max_nodes: usize,
    pub include_unresolved: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    #[serde(flatten)]
    pub symbol: SymbolSummary,
    pub depth: usize,
}

/// Distinct (source, target, kind) binding; `sites` counts its call/use sites.
#[derive(Debug, Clone, Serialize)]
pub struct Link {
    pub source_id: String,
    pub target_id: String,
    pub kind: Relationship,
    pub status: Resolution,
    pub sites: usize,
    pub location: Location,
    pub rule: String,
    pub binding_chain: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Dependencies {
    pub schema_version: u32,
    pub index_revision: u64,
    pub budget: ByteBudget,
    pub used_bytes: usize,
    pub direction: Direction,
    pub max_depth: usize,
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub unresolved: Vec<Site>,
    pub truncated: bool,
    pub omitted: usize,
    pub limit_reached: bool,
    /// Counts cover the bounded traversal; false means an unknown remainder.
    pub counts_exact: bool,
    pub available_links: usize,
    pub available_unresolved: usize,
    pub available_nodes_by_depth: BTreeMap<usize, usize>,
    pub include_unresolved: bool,
    pub navigation: NavigationEvidence,
}

#[allow(clippy::too_many_lines)] // One bounded graph projection and its byte-fit closure.
pub fn dependencies(
    store: &Store,
    selector: &Selector,
    options: &DependencyOptions,
    max_bytes: usize,
    format: Format,
) -> Result<(Dependencies, String)> {
    ensure!(
        (1..=MAX_DEPTH).contains(&options.depth),
        "depth must be between 1 and {MAX_DEPTH}"
    );
    ensure!(
        (1..=MAX_NODES).contains(&options.max_nodes),
        "max_nodes must be between 1 and {MAX_NODES}"
    );
    let root = unique(store, selector)?;
    let kinds = kind_names(&options.kinds)?;
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    let mut facts = BTreeMap::new();
    let Walk {
        depths,
        order,
        links,
        mut unresolved,
        mut limit_reached,
    } = walk(store, &root.id, options, &kinds, &mut facts)?;
    if options.include_unresolved && options.direction != Direction::Downstream {
        let base = root.name.rsplit('.').next().unwrap_or(&root.name);
        let capacity = MAX_UNRESOLVED.saturating_sub(unresolved.len());
        let sites = store.semantic_unresolved_named(base, &kinds, u32::try_from(capacity + 1)?)?;
        limit_reached |= sites.len() > capacity;
        for edge in sites.into_iter().take(capacity) {
            unresolved.push(site(store, &mut facts, edge)?);
        }
    }
    let mut nodes = Vec::new();
    for id in &order {
        if let Some(fact) = cached(store, &mut facts, id)? {
            nodes.push(Node {
                symbol: SymbolSummary::from(&fact),
                depth: depths[id],
            });
        }
    }
    let mut by_depth = BTreeMap::new();
    for node in &nodes {
        *by_depth.entry(node.depth).or_default() += 1;
    }
    let inspection = crate::signal::Inspection::new(
        store,
        nodes
            .iter()
            .map(|node| node.symbol.path.clone())
            .chain(unresolved.iter().map(|site| site.location.path.clone())),
    )?;
    let full = Dependencies {
        schema_version: code_intel_core::SCHEMA_VERSION,
        index_revision: store.revision()?,
        budget: ByteBudget::new(max_bytes),
        used_bytes: 0,
        direction: options.direction,
        max_depth: options.depth,
        counts_exact: !limit_reached,
        available_links: links.len(),
        available_unresolved: unresolved.len(),
        available_nodes_by_depth: by_depth,
        include_unresolved: options.include_unresolved,
        navigation: inspection.project([]),
        nodes,
        links,
        unresolved,
        truncated: false,
        omitted: 0,
        limit_reached,
    };
    let total = full.links.len() + full.unresolved.len();
    fit(
        total,
        max_bytes,
        |kept| {
            let mut view = full.clone();
            let links = kept.min(view.links.len());
            view.links.truncate(links);
            view.unresolved.truncate(kept - links);
            // Keep the root and only nodes that a kept link or site mentions.
            let mentioned: BTreeSet<&str> = view
                .links
                .iter()
                .flat_map(|link| [link.source_id.as_str(), link.target_id.as_str()])
                .chain(view.unresolved.iter().map(|site| site.source_id.as_str()))
                .collect();
            let root = full.nodes.first().map(|node| node.symbol.id.clone());
            view.nodes.retain(|node| {
                Some(&node.symbol.id) == root.as_ref()
                    || mentioned.contains(node.symbol.id.as_str())
            });
            view.omitted = total - kept;
            view.truncated = view.omitted > 0 || view.limit_reached;
            view.navigation = inspection.project(
                view.nodes
                    .iter()
                    .map(|node| node.symbol.path.clone())
                    .chain(
                        view.unresolved
                            .iter()
                            .map(|site| site.location.path.clone()),
                    ),
            );
            view
        },
        |view, used| view.used_bytes = used,
        |view| render_dependencies(view, format),
    )
}

struct Walk {
    depths: BTreeMap<String, usize>,
    order: Vec<String>,
    links: Vec<Link>,
    unresolved: Vec<Site>,
    limit_reached: bool,
}

/// Breadth-first over resolved typed edges; discovery order is the ordinal order.
fn walk(
    store: &Store,
    root: &str,
    options: &DependencyOptions,
    kinds: &[&str],
    facts: &mut BTreeMap<String, Option<Fact>>,
) -> Result<Walk> {
    let mut depths = BTreeMap::from([(root.to_owned(), 0)]);
    let mut order = vec![root.to_owned()];
    let mut links: Vec<Link> = Vec::new();
    let mut index: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    let mut sites = BTreeSet::new();
    let mut unresolved = Vec::new();
    let mut limit_reached = false;
    let mut frontier = vec![root.to_owned()];
    let per_node = u32::try_from(PER_NODE + 1)?;
    for depth in 1..=options.depth {
        let mut next = Vec::new();
        for id in &frontier {
            store.check_cancelled()?;
            let mut edges = Vec::new();
            if options.direction != Direction::Upstream {
                let outgoing = store.semantic_outgoing(id, kinds, per_node)?;
                limit_reached |= outgoing.len() > PER_NODE;
                edges.extend(outgoing.into_iter().take(PER_NODE).map(|edge| (true, edge)));
            }
            if options.direction != Direction::Downstream {
                let incoming = store.semantic_incoming(id, kinds, per_node)?;
                limit_reached |= incoming.len() > PER_NODE;
                edges.extend(
                    incoming
                        .into_iter()
                        .take(PER_NODE)
                        .map(|edge| (false, edge)),
                );
            }
            for (outgoing, edge) in edges {
                let Some(target) = edge.target_id.clone() else {
                    if options.include_unresolved {
                        if unresolved.len() < MAX_UNRESOLVED {
                            unresolved.push(site(store, facts, edge)?);
                        } else {
                            limit_reached = true;
                        }
                    }
                    continue;
                };
                // With direction=both an edge can be reached from either endpoint.
                if edge.status != Resolution::Resolved
                    || !sites.insert((
                        edge.location.path.clone(),
                        edge.location.start_byte,
                        edge.location.end_byte,
                        edge.source_id.clone(),
                        format!("{:?}", edge.kind),
                    ))
                {
                    continue;
                }
                let other = if outgoing {
                    target.clone()
                } else {
                    edge.source_id.clone()
                };
                let key = (
                    edge.source_id.clone(),
                    target.clone(),
                    format!("{:?}", edge.kind),
                );
                if let Some(position) = index.get(&key) {
                    links[*position].sites += 1;
                    continue;
                }
                if links.len() >= MAX_LINKS {
                    limit_reached = true;
                    continue;
                }
                if !depths.contains_key(&other) {
                    if order.len() >= options.max_nodes {
                        limit_reached = true;
                        continue;
                    }
                    depths.insert(other.clone(), depth);
                    order.push(other.clone());
                    next.push(other);
                }
                index.insert(key, links.len());
                links.push(Link {
                    source_id: edge.source_id,
                    target_id: target,
                    kind: edge.kind,
                    status: edge.status,
                    sites: 1,
                    location: edge.location,
                    rule: edge.evidence.rule,
                    binding_chain: edge.evidence.binding_chain,
                });
            }
        }
        frontier = next;
    }
    Ok(Walk {
        depths,
        order,
        links,
        unresolved,
        limit_reached,
    })
}

fn kind_names(kinds: &[Relationship]) -> Result<Vec<String>> {
    ensure!(!kinds.is_empty(), "kinds must not be empty");
    kinds
        .iter()
        .map(|kind| {
            Ok(serde_json::to_value(kind)?
                .as_str()
                .unwrap_or_default()
                .to_owned())
        })
        .collect()
}

fn cached(
    store: &Store,
    facts: &mut BTreeMap<String, Option<Fact>>,
    id: &str,
) -> Result<Option<Fact>> {
    if !facts.contains_key(id) {
        facts.insert(id.to_owned(), store.fact(id)?);
    }
    Ok(facts[id].clone())
}

fn site(
    store: &Store,
    facts: &mut BTreeMap<String, Option<Fact>>,
    edge: SemanticEdge,
) -> Result<Site> {
    let source = cached(store, facts, &edge.source_id)?;
    let line = source.as_ref().and_then(|fact| {
        let offset = edge.location.start_line.checked_sub(fact.start_line)?;
        let text = fact.snippet.lines().nth(offset)?.trim();
        let mut end = text.len().min(LINE_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Some(if end < text.len() {
            format!("{}…", &text[..end])
        } else {
            text.to_owned()
        })
    });
    Ok(Site {
        jsx: edge.evidence.syntax.ends_with("> JSX element"),
        source_name: source
            .filter(|fact| fact.kind != "file")
            .map(|fact| fact.name),
        source_id: edge.source_id,
        kind: edge.kind,
        status: edge.status,
        target_text: edge.target_text,
        location: edge.location,
        rule: edge.evidence.rule,
        binding_chain: edge.evidence.binding_chain,
        line,
    })
}

/// Largest prefix of `total` entries whose rendering fits `max_bytes`, with the
/// rendered size written back into the view (fixed point, like context packing).
fn fit<T>(
    total: usize,
    max_bytes: usize,
    view: impl Fn(usize) -> T,
    set_used: impl Fn(&mut T, usize),
    render: impl Fn(&T) -> Result<String>,
) -> Result<(T, String)> {
    let measure = |kept: usize| settle(view(kept), &set_used, &render);
    let (mut low, mut high) = (0, total);
    let empty = measure(0)?;
    if empty.1.len() > max_bytes {
        return Err(fail(
            "budget_too_small",
            format!(
                "max_bytes {max_bytes} is too small for the response envelope (minimum {})",
                empty.1.len()
            ),
        ));
    }
    let mut best = empty;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let candidate = measure(middle)?;
        if candidate.1.len() <= max_bytes {
            low = middle;
            best = candidate;
        } else {
            high = middle - 1;
        }
    }
    Ok(best)
}

/// Render a view whose printed size includes its own byte count.
fn settle<T>(
    mut value: T,
    set_used: impl Fn(&mut T, usize),
    render: impl Fn(&T) -> Result<String>,
) -> Result<(T, String)> {
    // Views start with zero used bytes; iterate until the printed size is stable.
    let mut size = 0;
    for _ in 0..8 {
        let text = render(&value)?;
        if text.len() == size {
            return Ok((value, text));
        }
        size = text.len();
        set_used(&mut value, size);
    }
    let text = render(&value)?;
    Ok((value, text))
}

fn status(value: Resolution) -> &'static str {
    match value {
        Resolution::Resolved => "resolved",
        Resolution::Unresolved => "unresolved",
        Resolution::Ambiguous => "ambiguous",
        Resolution::Unsupported => "unsupported",
    }
}

fn relationship(value: Relationship) -> &'static str {
    match value {
        Relationship::Imports => "IMPORTS",
        Relationship::Exports => "EXPORTS",
        Relationship::ReExports => "RE_EXPORTS",
        Relationship::References => "REFERENCES",
        Relationship::Calls => "CALLS",
        Relationship::Extends => "EXTENDS",
    }
}

/// `name · id · Llines`; the ID already carries the kind and path.
fn summary(output: &mut String, symbol: &SymbolSummary) -> Result<()> {
    if symbol.kind != "file" {
        write!(output, "{} · ", symbol.name)?;
    }
    write!(
        output,
        "{} · L{}",
        symbol.id,
        agent::lines(symbol.start_line, symbol.end_line)
    )?;
    Ok(())
}

/// Sites sharing a source and location (an identifier REFERENCES edge beside
/// its CALLS edge, for example) render as one line with joined kinds.
fn merged(sites: &[Site]) -> Vec<(&Site, Vec<&'static str>)> {
    let mut groups: Vec<(&Site, Vec<&'static str>)> = Vec::new();
    for site in sites {
        let position = groups.iter().position(|(first, _)| {
            first.source_id == site.source_id
                && first.location == site.location
                && first.status == site.status
                && first.target_text == site.target_text
        });
        let kind = if site.jsx {
            "REFERENCES(JSX)"
        } else {
            relationship(site.kind)
        };
        match position {
            Some(position) if !groups[position].1.contains(&kind) => {
                groups[position].1.push(kind);
            }
            Some(_) => {}
            None => groups.push((site, vec![kind])),
        }
    }
    groups
}

/// One line per merged site; adjacent identical lines (distinct sites on one
/// source line) collapse into a count rather than repeating the source text.
fn site_lines(output: &mut String, sites: &[Site], confirmed: bool) -> Result<()> {
    let mut rendered: Vec<(String, usize)> = Vec::new();
    for (site, kinds) in merged(sites) {
        let mut line = format!(
            "{}:{} {} ",
            site.location.path,
            agent::lines(site.location.start_line, site.location.end_line),
            kinds.join("+")
        );
        if !confirmed {
            write!(line, "\"{}\" {} ", site.target_text, status(site.status))?;
        }
        // The caller's file is the site's path; `name` + `path` re-selects it
        // (JSON keeps the exact source ID).
        match &site.source_name {
            Some(name) => write!(line, "from {name}")?,
            None => line.push_str("at module scope"),
        }
        if let Some(text) = &site.line {
            write!(line, " | {text}")?;
        }
        match rendered.last_mut() {
            Some((last, count)) if *last == line => *count += 1,
            _ => rendered.push((line, 1)),
        }
    }
    for (line, count) in rendered {
        if count > 1 {
            writeln!(output, "({count} sites) {line}")?;
        } else {
            writeln!(output, "{line}")?;
        }
    }
    Ok(())
}

fn render_references(view: &References, format: Format) -> Result<String> {
    if format == Format::Json {
        return Ok(serde_json::to_string(view)? + "\n");
    }
    let mut output = String::from("atlas references r");
    write!(output, "{} · ", view.index_revision)?;
    summary(&mut output, &view.symbol)?;
    writeln!(
        output,
        " · {} resolved, {} unconfirmed edge(s) · {} · {}",
        view.resolved.len(),
        view.unresolved.len(),
        agent::completeness(view.truncated, view.omitted, view.limit_reached),
        agent::budget(view.used_bytes, view.budget.max_bytes)
    )?;
    output.push_str(&crate::signal::text(&view.navigation));
    writeln!(
        output,
        "available: {} resolved, {} unconfirmed; returned: {} + {}; omitted: {} (exact); unconfirmed included: {}",
        view.available_resolved,
        view.available_unresolved,
        view.resolved.len(),
        view.unresolved.len(),
        view.omitted,
        view.include_unresolved
    )?;
    if !view.resolved.is_empty() {
        output.push_str("\nresolved:\n");
        site_lines(&mut output, &view.resolved, true)?;
    }
    if !view.unresolved.is_empty() {
        output.push_str("\nunconfirmed (same name, no resolved target):\n");
        site_lines(&mut output, &view.unresolved, false)?;
    }
    if view.truncated {
        output.push_str("follow-up: increase max_bytes (up to 32000) and limit (up to 500), or narrow kinds; unconfirmed sites are not bindings\n");
    }
    if let Some(hop) = &view.next_hop {
        output.push_str(&next_hop_line(view, hop)?);
    }
    Ok(output)
}

fn next_hop_line(view: &References, hop: &NextHop) -> Result<String> {
    let mut output = String::new();
    match hop {
        NextHop::Dependencies => writeln!(
            output,
            "next hop: atlas_dependencies {{\"id\":{},\"direction\":\"upstream\",\"depth\":{NEXT_HOP_DEPTH},\"max_bytes\":8000}} follows confirmed consumers two levels up as compact declaration nodes; sites above stay the use-site evidence",
            serde_json::to_string(&view.symbol.id)?
        )?,
        NextHop::References(consumer) => writeln!(
            output,
            "next hop: atlas_references {{\"id\":{}}}, then each other consumer above by name and path; a two-level atlas_dependencies walk from here stops at its work limit, so its counts would be lower bounds{}",
            serde_json::to_string(consumer)?,
            if view.truncated {
                "; recover omitted callers here before claiming completeness"
            } else {
                ""
            }
        )?,
    }
    Ok(output)
}

#[allow(clippy::too_many_lines)] // Node identities and both confidence sections render together.
fn render_dependencies(view: &Dependencies, format: Format) -> Result<String> {
    if format == Format::Json {
        return Ok(serde_json::to_string(view)? + "\n");
    }
    let labels: BTreeMap<&str, usize> = view
        .nodes
        .iter()
        .enumerate()
        .map(|(position, node)| (node.symbol.id.as_str(), position))
        .collect();
    let label = |id: &str| {
        labels
            .get(id)
            .map_or_else(|| id.to_owned(), |position| format!("#{position}"))
    };
    let mut output = format!(
        "atlas dependencies r{} · {} · depth ≤{} · {} nodes, {} links, {} unconfirmed · {} · {}\n",
        view.index_revision,
        match view.direction {
            Direction::Downstream => "downstream",
            Direction::Upstream => "upstream",
            Direction::Both => "both",
        },
        view.max_depth,
        view.nodes.len(),
        view.links.len(),
        view.unresolved.len(),
        agent::completeness(view.truncated, view.omitted, view.limit_reached),
        agent::budget(view.used_bytes, view.budget.max_bytes)
    );
    output.push_str(&crate::signal::text(&view.navigation));
    writeln!(
        output,
        "available depths: {:?}; links {}, unconfirmed {} (counts {}); unconfirmed included: {}",
        view.available_nodes_by_depth,
        view.available_links,
        view.available_unresolved,
        if view.counts_exact {
            "exact"
        } else {
            "lower bounds; unknown remainder"
        },
        view.include_unresolved
    )?;
    output.push_str("\nnodes:\n");
    for (position, node) in view.nodes.iter().enumerate() {
        write!(output, "#{position} ")?;
        if node.depth > 0 {
            write!(output, "d{} ", node.depth)?;
        }
        summary(&mut output, &node.symbol)?;
        output.push('\n');
    }
    if !view.links.is_empty() {
        output.push_str("\nresolved (source -KINDS-> target @first site):\n");
        let mut groups: Vec<(&Link, Vec<String>)> = Vec::new();
        for link in &view.links {
            let kind = if link.sites > 1 {
                format!("{}×{}", relationship(link.kind), link.sites)
            } else {
                relationship(link.kind).to_owned()
            };
            match groups.iter().position(|(first, _)| {
                first.source_id == link.source_id && first.target_id == link.target_id
            }) {
                Some(position) => groups[position].1.push(kind),
                None => groups.push((link, vec![kind])),
            }
        }
        for (link, kinds) in groups {
            let same_path = view.nodes.iter().any(|node| {
                node.symbol.id == link.source_id && node.symbol.path == link.location.path
            });
            let site = if same_path {
                format!(
                    "L{}",
                    agent::lines(link.location.start_line, link.location.end_line)
                )
            } else {
                format!(
                    "{}:{}",
                    link.location.path,
                    agent::lines(link.location.start_line, link.location.end_line)
                )
            };
            writeln!(
                output,
                "{} -{}-> {} @{site} resolved",
                label(&link.source_id),
                kinds.join("+"),
                label(&link.target_id),
            )?;
        }
    }
    if view.truncated {
        output.push_str("follow-up: increase max_bytes (up to 32000); if work-limited, increase max_nodes or narrow depth/kinds and follow a selected node\n");
    }
    output.push_str("scope: indexed static links; HTTP/runtime consumers and unmocked test execution unknown; use native Read/Grep for mocks and possible consumers\n");
    if !view.unresolved.is_empty() {
        output.push_str("\nunconfirmed (no resolved target):\n");
        for site in &view.unresolved {
            write!(
                output,
                "{} -{}-> \"{}\" {} @{}:{}",
                label(&site.source_id),
                relationship(site.kind),
                site.target_text,
                status(site.status),
                site.location.path,
                agent::lines(site.location.start_line, site.location.end_line)
            )?;
            if let Some(line) = &site.line {
                write!(output, " | {line}")?;
            }
            output.push('\n');
        }
    }
    Ok(output)
}
