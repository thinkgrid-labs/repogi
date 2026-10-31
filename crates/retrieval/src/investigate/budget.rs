//! Work limits and output fitting. Work is counted in indexed rows read;
//! byte fitting drops whole steps and reports what it dropped.
use std::collections::BTreeSet;

use anyhow::Result;

use super::{Category, MAX_WORK, Response, Step, evidence::Ledger, render};
use crate::Format;

/// Per-request work counters and the limits they hit.
#[derive(Default)]
pub(super) struct Work {
    pub used: usize,
    /// Graph nodes visited and static links accepted, across every walk.
    pub nodes: usize,
    pub edges: usize,
    pub limited: bool,
    pub candidates_limited: bool,
    /// An unvisited incident edge was observed past `depth`, or frontier checks were capped.
    pub frontier: bool,
}

impl Work {
    /// Exhausting the work limit stops further discovery.
    pub fn spend(&mut self, rows: usize) -> bool {
        self.used += rows;
        if self.used > MAX_WORK {
            self.limited = true;
        }
        !self.limited
    }
}

/// The steps that survived step selection, before byte fitting.
pub(super) struct Fitting<'a> {
    pub steps: Vec<Step>,
    /// Draft index of each step, in step order.
    pub chosen: Vec<usize>,
    /// Draft indices in the order selection picked them.
    pub selection: &'a [usize],
    pub ledger: &'a Ledger,
    /// Categories that had at least one draft.
    pub present: &'a BTreeSet<Category>,
    pub total: usize,
}

/// Drop the latest-selected step until the rendered response fits, so the
/// category coverage that selection established is kept as long as possible.
pub(super) fn fit(
    mut response: Response,
    fitting: Fitting<'_>,
    format: Format,
) -> Result<Response> {
    let Fitting {
        mut steps,
        mut chosen,
        selection,
        ledger,
        present,
        total,
    } = fitting;
    loop {
        project(&mut response, &steps, ledger, present, total);
        measure(&mut response, format)?;
        if response.budget.used_bytes <= response.budget.max_bytes {
            break;
        }
        let Some(drop) =
            (0..steps.len()).max_by_key(|&k| selection.iter().position(|i| *i == chosen[k]))
        else {
            return Err(crate::BudgetTooSmall {
                minimum: response.budget.used_bytes,
            }
            .into());
        };
        steps.remove(drop);
        chosen.remove(drop);
        for (order, step) in steps.iter_mut().enumerate() {
            step.order = order + 1;
        }
        response.completeness.byte_truncated = true;
    }
    Ok(response)
}

/// Project the shared ledger onto the kept steps, in step order.
fn project(
    response: &mut Response,
    steps: &[Step],
    ledger: &Ledger,
    present: &BTreeSet<Category>,
    total: usize,
) {
    let mut target_ids = Vec::new();
    let mut evidence_ids = Vec::new();
    for step in steps {
        for t in &step.targets {
            if !target_ids.contains(t) {
                target_ids.push(t.clone());
            }
        }
        let mut ids: Vec<&String> = step.evidence.iter().collect();
        ids.extend(step.uncertainty.iter().flat_map(|u| u.evidence.iter()));
        for e in ids {
            if ledger.evidence.contains_key(e) && !evidence_ids.contains(e) {
                evidence_ids.push(e.clone());
            }
        }
    }
    for e in response.uncertainty.iter().flat_map(|u| u.evidence.iter()) {
        if ledger.evidence.contains_key(e) && !evidence_ids.contains(e) {
            evidence_ids.push(e.clone());
        }
    }
    response.steps = steps.to_vec();
    response.targets = target_ids
        .iter()
        .map(|t| ledger.targets[t].clone())
        .collect();
    response.evidence = evidence_ids
        .iter()
        .map(|e| ledger.evidence[e].clone())
        .collect();
    let kept: BTreeSet<Category> = steps.iter().map(|s| s.category).collect();
    let completeness = &mut response.completeness;
    completeness.omitted_steps = total - steps.len();
    completeness.omitted_categories = present.difference(&kept).copied().collect();
    completeness.complete_within_limits = completeness.omitted_steps == 0
        && !completeness.byte_truncated
        && !completeness.work_limit_reached
        && !completeness.candidate_limit_reached
        && !completeness.depth_frontier
        && completeness.unsupported.is_empty();
}

/// `used_bytes` is part of the output, so iterate until it is self-consistent.
fn measure(response: &mut Response, format: Format) -> Result<()> {
    for _ in 0..8 {
        let bytes = render(response, format)?.len();
        if bytes == response.budget.used_bytes {
            break;
        }
        response.budget.used_bytes = bytes;
    }
    Ok(())
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn work_limit_is_sticky_once_exceeded() {
        let mut work = Work::default();
        assert!(work.spend(MAX_WORK));
        assert!(!work.limited);
        assert!(!work.spend(1));
        assert!(work.limited && !work.spend(0));
    }
}
