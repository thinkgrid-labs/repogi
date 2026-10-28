//! Offline diagnostic: independent SQL, rendering and selected-file checks.
//! Opens an existing index read-only. Never indexes or runs repository code.
use anyhow::{Result, ensure};
use code_intel_core::semantic::Relationship;
use code_intel_retrieval::{Format, navigate, signal};
use code_intel_store::Store;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, time::Instant};

fn measure<T>(
    stages: &mut Vec<Value>,
    name: &str,
    repeats: usize,
    mut operation: impl FnMut() -> Result<T>,
) -> Result<()> {
    let mut samples = Vec::new();
    for _ in 0..repeats {
        let started = Instant::now();
        std::hint::black_box(operation()?);
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let mut warm = samples[1..].to_vec();
    warm.sort_by(f64::total_cmp);
    stages.push(json!({"stage":name,"first_ms":samples[0],"warm_median_ms":warm[warm.len()/2],"samples_ms":samples}));
    Ok(())
}

fn sql_stages(store: &Store, repeats: usize, stages: &mut Vec<Value>) -> Result<()> {
    let selector = navigate::Selector {
        name: Some("checkRateLimit".into()),
        path: Some("src/lib/two-factor/rate-limit.ts".into()),
        ..Default::default()
    };
    let fact = navigate::unique(store, &selector)?;
    let kinds = [
        "CALLS",
        "REFERENCES",
        "EXTENDS",
        "IMPORTS",
        "EXPORTS",
        "RE_EXPORTS",
    ];
    measure(stages, "select_scoped_declaration", repeats, || {
        navigate::unique(store, &selector)
    })?;
    measure(stages, "enumerate_incoming_limit_501", repeats, || {
        store.semantic_incoming(&fact.id, &kinds, 501)
    })?;
    measure(stages, "full_incoming_count", repeats, || {
        store.semantic_incoming_count(&fact.id, &kinds)
    })?;
    measure(stages, "enumerate_unconfirmed_limit_65", repeats, || {
        store.semantic_unresolved_named("checkRateLimit", &kinds, 65)
    })?;
    measure(stages, "full_unconfirmed_count", repeats, || {
        store.semantic_unresolved_count("checkRateLimit", &kinds)
    })?;
    measure(stages, "binding_backed_alias_candidates", repeats, || {
        store.targets_named("Logo", None, 4)
    })?;
    let options = navigate::ReferenceOptions {
        kinds: vec![
            Relationship::Calls,
            Relationship::References,
            Relationship::Extends,
            Relationship::Imports,
            Relationship::Exports,
            Relationship::ReExports,
        ],
        limit: 500,
        include_unresolved: true,
        next_hop: false,
    };
    let (view, _) = navigate::references(store, &selector, &options, 32000, Format::Json)?;
    measure(stages, "serialize_reference_json_only", repeats, || {
        Ok(serde_json::to_string(&view)?)
    })?;
    measure(stages, "reference_query_total", repeats, || {
        navigate::references(store, &selector, &options, 32000, Format::Json)
    })?;
    Ok(())
}

fn source_stages(
    store: &Store,
    root: &std::path::Path,
    repeats: usize,
    stages: &mut Vec<Value>,
) -> Result<()> {
    let options = navigate::DependencyOptions {
        direction: navigate::Direction::Upstream,
        depth: 2,
        max_nodes: 100,
        kinds: vec![
            Relationship::Calls,
            Relationship::References,
            Relationship::Extends,
        ],
        include_unresolved: true,
    };
    let (view, _) = navigate::dependencies(
        store,
        &navigate::Selector {
            name: Some("getOffsetDateRange".into()),
            path: Some("src/lib/date.ts".into()),
            ..Default::default()
        },
        &options,
        8000,
        Format::Agent,
    )?;
    let paths: Vec<_> = view
        .nodes
        .iter()
        .map(|n| n.symbol.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ensure!(
        paths.len() == 32,
        "expected the independently labelled 32-file navigation snapshot"
    );
    measure(stages, "selected_file_digest_only_1", repeats, || {
        code_intel_index::selected_source_hash(root, "src/lib/two-factor/rate-limit.ts")
    })?;
    measure(stages, "selected_file_digests_only_32", repeats, || {
        paths
            .iter()
            .map(|path| code_intel_index::selected_source_hash(root, path))
            .collect::<Result<Vec<_>>>()
    })?;
    measure(stages, "inspection_with_store_hashes_32", repeats, || {
        signal::Inspection::new(store, paths.clone())
    })?;
    let inspection = signal::Inspection::new(store, paths.clone())?;
    measure(
        stages,
        "freshness_projection_and_text_only",
        repeats,
        || Ok(signal::text(&inspection.project(paths.clone()))),
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("usage: navigation_profile ROOT [REPEATS]"))?,
    )
    .canonicalize()?;
    let repeats = args.next().map_or(Ok(7), |n| n.parse::<usize>())?;
    ensure!((3..=100).contains(&repeats), "use 3..100 repeats");
    let store = Store::open_read(&root.join(".code-intel/index.db"))?;
    ensure!(
        store.root()?.as_deref() == root.to_str(),
        "index root mismatch"
    );
    let mut stages = Vec::new();
    sql_stages(&store, repeats, &mut stages)?;
    source_stages(&store, &root, repeats, &mut stages)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"phase":"5.4","repeats":repeats,"stages":stages,
        "scope":"isolated diagnostic components, reused read-only store; warm page caches; components are not additive",
        "model_calls":0,"project_scripts":0}))?
    );
    Ok(())
}
