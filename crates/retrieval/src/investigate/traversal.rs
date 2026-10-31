//! Bounded static traversal: upstream consumers of each primary code target,
//! test-convention consumers as advisory test candidates, same-name unresolved
//! sites as unconfirmed, and direct dependencies of the first target.
use std::collections::BTreeSet;

use anyhow::Result;
use code_intel_core::{relationships::SourceRange, semantic::Relationship};

use super::{
    EvidenceClass, FRONTIER_CHECKS, Investigation, MAX_DEPENDENCIES, MAX_GRAPH_EDGES,
    MAX_GRAPH_NODES, MAX_UNCONFIRMED, Relation, Role, STATIC_KINDS,
    evidence::{clean, edge_range, label},
};
use crate::navigate::{self, DependencyOptions, Direction, Link, Walk};

impl Investigation<'_> {
    /// One bounded upstream walk per primary target, sharing the request's
    /// node, link and work limits; then the first target's direct dependencies.
    pub(super) fn walk(&mut self, depth: usize) -> Result<()> {
        let kinds = navigate::kind_names(&STATIC_KINDS)?;
        let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
        for id in self.found.primary.clone() {
            if self.work.limited
                || self.work.nodes >= MAX_GRAPH_NODES
                || self.work.edges >= MAX_GRAPH_EDGES
            {
                self.work.limited = true;
                break;
            }
            let options = DependencyOptions {
                direction: Direction::Upstream,
                depth,
                kinds: STATIC_KINDS.to_vec(),
                max_nodes: (MAX_GRAPH_NODES - self.work.nodes).max(2),
                include_unresolved: false,
            };
            let walk = navigate::walk(self.store, &id, &options, &kinds, &mut self.facts)?;
            self.work.limited |= walk.limit_reached;
            self.work.nodes += walk.order.len().saturating_sub(1);
            self.spend(walk.order.len() + walk.links.len());
            self.record_links(&id, &walk)?;
            if !walk.limit_reached {
                self.check_frontier(&walk, depth, &kinds)?;
            }
            self.unconfirmed_sites(&id, &kinds)?;
        }
        if let Some(id) = self.found.primary.first().cloned()
            && !self.work.limited
        {
            self.direct_dependencies(&id)?;
        }
        Ok(())
    }

    /// Consumers in test-convention files become advisory test candidates;
    /// the rest are verified static consumers of `target`.
    fn record_links(&mut self, target: &str, walk: &Walk) -> Result<()> {
        // An identifier REFERENCES edge beside its CALLS edge is one site, not two.
        let calls: BTreeSet<(&str, &str, usize)> = walk
            .links
            .iter()
            .filter(|l| l.kind == Relationship::Calls)
            .map(|l| {
                (
                    l.source_id.as_str(),
                    l.target_id.as_str(),
                    l.location.start_byte,
                )
            })
            .collect();
        for link in &walk.links {
            if link.kind == Relationship::References
                && calls.contains(&(
                    link.source_id.as_str(),
                    link.target_id.as_str(),
                    link.location.start_byte,
                ))
            {
                continue;
            }
            if self.work.edges >= MAX_GRAPH_EDGES {
                self.work.limited = true;
                break;
            }
            self.work.edges += 1;
            let level = walk.depths.get(&link.source_id).copied().unwrap_or(1);
            self.record_link(target, link, level)?;
        }
        Ok(())
    }

    fn record_link(&mut self, target: &str, link: &Link, level: usize) -> Result<()> {
        let Some(source) = self.fact(&link.source_id)? else {
            return Ok(());
        };
        let relation = Relation {
            id: None,
            kind: label(&link.kind),
            source_id: link.source_id.clone(),
            target_id: Some(link.target_id.clone()),
            target_text: String::new(),
            status: label(&link.status),
        };
        let citation = SourceRange {
            path: link.location.path.clone(),
            start_byte: link.location.start_byte,
            end_byte: link.location.end_byte,
            start_line: link.location.start_line,
            end_line: link.location.end_line,
        };
        if let Some(reason) = crate::tests::convention(&source.path) {
            let evidence = self.ledger.add(
                EvidenceClass::AdvisoryTestCandidate,
                &source.id,
                format!("{}; {reason}; not executed", link.rule),
                citation,
                Some(relation),
                Some(level),
            );
            self.ledger.fact_target(&source, Role::Test);
            if let Some(evidence) = evidence {
                self.found.tests.push((source.id.clone(), evidence));
            }
        } else if let Some(evidence) = self.ledger.add(
            EvidenceClass::VerifiedStaticRelationship,
            &source.id,
            link.rule.clone(),
            citation,
            Some(relation),
            Some(level),
        ) {
            self.ledger.fact_target(&source, Role::Consumer);
            self.found
                .consumers
                .entry(target.to_owned())
                .or_default()
                .push((source.id.clone(), evidence));
        }
        Ok(())
    }

    /// A frontier exists only if an edge past `depth` reaches an unvisited node.
    fn check_frontier(&mut self, walk: &Walk, depth: usize, kinds: &[&str]) -> Result<()> {
        let edge: Vec<&String> = walk
            .order
            .iter()
            .filter(|node| walk.depths.get(*node) == Some(&depth))
            .collect();
        if edge.len() > FRONTIER_CHECKS {
            self.work.frontier = true;
        }
        for node in edge.into_iter().take(FRONTIER_CHECKS) {
            let incoming = self.store.semantic_incoming(node, kinds, 9)?;
            self.spend(incoming.len());
            if incoming
                .iter()
                .any(|e| !walk.depths.contains_key(&e.source_id))
            {
                self.work.frontier = true;
                break;
            }
        }
        Ok(())
    }

    /// Unresolved sites whose expression ends in the target's simple name: shown
    /// as unconfirmed, never counted as consumers.
    fn unconfirmed_sites(&mut self, id: &str, kinds: &[&str]) -> Result<()> {
        let Some(fact) = self.fact(id)? else {
            return Ok(());
        };
        let base = fact
            .name
            .rsplit('.')
            .next()
            .unwrap_or(&fact.name)
            .to_owned();
        let sites = self.store.semantic_unresolved_named(
            &base,
            kinds,
            u32::try_from(MAX_UNCONFIRMED + 1)?,
        )?;
        self.spend(sites.len());
        self.work.candidates_limited |= sites.len() > MAX_UNCONFIRMED;
        let mut seen_sites = BTreeSet::new();
        for site in sites.into_iter().take(MAX_UNCONFIRMED) {
            if !seen_sites.insert((site.source_id.clone(), site.location.start_byte)) {
                continue;
            }
            let relation = Relation {
                id: None,
                kind: label(&site.kind),
                source_id: site.source_id.clone(),
                target_id: None,
                target_text: clean(&site.target_text),
                status: label(&site.status),
            };
            if let Some(evidence) = self.ledger.add(
                EvidenceClass::UnresolvedReference,
                id,
                format!(
                    "{}: same-name site, no resolved target; {}",
                    label(&site.status),
                    site.evidence.rule
                ),
                edge_range(&site),
                Some(relation),
                None,
            ) {
                self.found
                    .unconfirmed
                    .entry(id.to_owned())
                    .or_default()
                    .push(evidence.clone());
                self.found.unresolved.push((id.to_owned(), evidence));
            }
        }
        Ok(())
    }

    fn direct_dependencies(&mut self, id: &str) -> Result<()> {
        let edges = self
            .store
            .semantic_dependencies(id, u32::try_from(MAX_DEPENDENCIES + 1)?)?;
        self.spend(edges.len());
        self.work.candidates_limited |= edges.len() > MAX_DEPENDENCIES;
        for edge in edges.into_iter().take(MAX_DEPENDENCIES) {
            let Some(target_id) = edge.target_id.clone() else {
                continue;
            };
            if target_id == id {
                continue;
            }
            let Some(target) = self.fact(&target_id)? else {
                continue;
            };
            let relation = Relation {
                id: None,
                kind: label(&edge.kind),
                source_id: edge.source_id.clone(),
                target_id: Some(target_id.clone()),
                target_text: clean(&edge.target_text),
                status: label(&edge.status),
            };
            if let Some(evidence) = self.ledger.add(
                EvidenceClass::VerifiedStaticRelationship,
                id,
                edge.evidence.rule.clone(),
                edge_range(&edge),
                Some(relation),
                Some(1),
            ) {
                self.ledger.fact_target(&target, Role::Dependency);
                let list = self.found.dependencies.entry(id.to_owned()).or_default();
                if !list.iter().any(|(t, _)| *t == target_id) {
                    list.push((target_id, evidence));
                }
            }
        }
        Ok(())
    }
}
