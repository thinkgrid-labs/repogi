//! Bounded, selected-file evidence checks. No scan, implicit re-index or graph
//! freshness claim: unchanged snippets do not verify other files or tsconfig.
use crate::{Format, Options, legacy, navigate::NavigationError};
use anyhow::Result;
use code_intel_core::{ContextResponse, NavigationEvidence};
use code_intel_store::Store;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    path::PathBuf,
};

pub const MAX_CHECK_FILES: usize = 32;

#[derive(Clone, Copy)]
enum State {
    Matched,
    Changed,
    Unverified,
}

/// Inspection is cached only within this request. Repeated calls read again.
pub struct Inspection {
    states: BTreeMap<String, State>,
}
impl Inspection {
    pub fn new(store: &Store, paths: impl IntoIterator<Item = String>) -> Result<Self> {
        let root = store.root()?.map(PathBuf::from);
        let mut states = BTreeMap::new();
        for path in paths {
            if states.contains_key(&path) {
                continue;
            }
            store.check_cancelled()?;
            let state = if states.len() < MAX_CHECK_FILES {
                match (root.as_deref(), store.file_hash(&path)?) {
                    (Some(root), Some(indexed)) => {
                        match code_intel_index::selected_source_hash(root, &path) {
                            Ok(Some(current)) if current == indexed => State::Matched,
                            Ok(_) => State::Changed,
                            Err(_) => State::Unverified,
                        }
                    }
                    _ => State::Unverified,
                }
            } else {
                State::Unverified
            };
            states.insert(path, state);
        }
        Ok(Self { states })
    }

    pub fn project(&self, paths: impl IntoIterator<Item = String>) -> NavigationEvidence {
        let paths: BTreeSet<_> = paths.into_iter().collect();
        let mut changed = Vec::new();
        let mut checked = 0;
        let mut unchecked = 0;
        for path in paths {
            match self.states.get(&path) {
                Some(State::Matched) => checked += 1,
                Some(State::Changed) => {
                    checked += 1;
                    changed.push(path);
                }
                _ => unchecked += 1,
            }
        }
        NavigationEvidence {
            source: "indexed_snapshot".into(),
            current_source: if !changed.is_empty() {
                "stale"
            } else if checked > 0 && unchecked == 0 {
                "matched_at_read"
            } else {
                "unverified"
            }
            .into(),
            checked_files: checked,
            unchecked_files: unchecked,
            changed_files: changed,
            graph_freshness: "unverified_other_files_and_resolution".into(),
            supported_extensions: vec![".ts".into(), ".tsx".into()],
        }
    }
}

#[must_use]
pub fn text(evidence: &NavigationEvidence) -> String {
    let mut text = format!(
        "evidence: indexed snapshot; source={} ({} checked, {} unchecked); graph freshness unverified; scope .ts/.tsx only\n",
        evidence.current_source, evidence.checked_files, evidence.unchecked_files
    );
    if !evidence.changed_files.is_empty() {
        writeln!(
            text,
            "stale indexed evidence: {:?}; Read current files or explicitly re-index",
            evidence.changed_files
        )
        .expect("write to string");
    }
    text
}

/// Attach evidence before final byte accounting. Re-pack with reserved metadata
/// space rather than stripping snippets or changing omission counts by hand.
pub fn context(
    store: &Store,
    query: &str,
    budget: usize,
    format: Format,
    options: &Options,
) -> Result<ContextResponse> {
    let mut available = budget;
    let mut inspections = BTreeMap::new();
    for _ in 0..8 {
        let mut response = crate::context_with_options(store, query, available, format, options)
            .map_err(|error| budget_error(error, budget))?;
        let paths: Vec<_> = response
            .items
            .iter()
            .map(|item| item.path.clone())
            .collect();
        for path in &paths {
            if !inspections.contains_key(path) {
                let state = if inspections.len() < MAX_CHECK_FILES {
                    Inspection::new(store, [path.clone()])?
                        .states
                        .remove(path)
                        .unwrap_or(State::Unverified)
                } else {
                    State::Unverified
                };
                inspections.insert(path.clone(), state);
            }
        }
        response.budget.max_tokens = budget;
        legacy::measure(&mut response, format)?;
        let base_bytes = response.estimated_tokens;
        response.navigation = Some(
            Inspection {
                states: inspections.clone(),
            }
            .project(paths),
        );
        legacy::measure(&mut response, format)?;
        if response.estimated_tokens <= budget {
            return Ok(response);
        }
        let overflow = response.estimated_tokens - budget;
        if available <= overflow + 8 {
            break;
        }
        let overhead = response.estimated_tokens.saturating_sub(base_bytes);
        available = (available - overflow - 8).min(budget.saturating_sub(overhead + 8));
    }
    Err(NavigationError{code:"budget_too_small",message:format!("output budget {budget} cannot hold navigation evidence; increase max_bytes (CLI: --max-tokens)")}.into())
}

fn budget_error(error: anyhow::Error, budget: usize) -> anyhow::Error {
    if let Some(small) = error.downcast_ref::<crate::BudgetTooSmall>() {
        NavigationError{code:"budget_too_small",message:format!("output budget {budget} is too small for metadata (minimum {} before evidence checks); increase max_bytes (CLI: --max-tokens)",small.minimum)}.into()
    } else {
        error
    }
}
