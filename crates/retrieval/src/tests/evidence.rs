use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use code_intel_core::semantic::{Relationship, Resolution, SemanticEdge};
use code_intel_store::Declaration;

use super::{Candidate, Evidence, TestFile};
use crate::impact::{DeclKey, DeclarationChange, ImpactReport, Side};

#[derive(Clone)]
struct Seed {
    changed: DeclKey,
    target: DeclKey,
    depth: usize,
    via: Vec<String>,
}

fn key(path: &str, kind: &str, name: &str) -> DeclKey {
    DeclKey {
        path: path.into(),
        kind: kind.into(),
        name: name.into(),
        ordinal: 0,
    }
}

const COLOCATED: &str = "colocated with an affected module; proximity does not establish usage";
const BASENAME: &str =
    "source/test basename match; collisions are possible and no target is inferred";
const IMPORT_ONLY: &str = "import-only link to an impact-reached declaration";
const IMPORT_RELATIONSHIP: &str =
    "import/re-export relationship alone does not establish a test use";

/// Heuristic or import-only hints. A confirmed row keeps them in JSON and
/// detailed text, but its confirmation never derives from them.
pub(super) fn hint(reason: &str) -> bool {
    [COLOCATED, BASENAME, IMPORT_ONLY, IMPORT_RELATIONSHIP].contains(&reason)
}

/// Possible rows, strongest signal first, so whole-row output fitting drops
/// proximity-only rows before evidence-backed ones. Never changes a category.
pub(super) fn strength(candidate: &Candidate) -> u8 {
    if candidate.evidence.is_some() {
        0
    } else if candidate
        .reasons
        .iter()
        .any(|r| r != COLOCATED && r != BASENAME)
    {
        1
    } else if candidate.reasons.contains(BASENAME) {
        2
    } else {
        3
    }
}

fn supported(file: &TestFile, snapshot: &str) -> bool {
    file.snapshots
        .get(snapshot)
        .is_some_and(|class| class == "supported_indexed" || class == "possible_convention")
}

fn uses(kind: &str) -> bool {
    matches!(kind, "CALLS" | "REFERENCES" | "EXTENDS")
}

fn relationship(kind: Relationship) -> String {
    match kind {
        Relationship::Calls => "CALLS",
        Relationship::References => "REFERENCES",
        Relationship::Extends => "EXTENDS",
        Relationship::Imports => "IMPORTS",
        Relationship::ReExports => "RE_EXPORTS",
        Relationship::Exports => "EXPORTS",
    }
    .into()
}

fn insert(
    candidates: &mut BTreeMap<String, Candidate>,
    file: &TestFile,
    reason: &str,
    proof: Option<Evidence>,
    confirmed: bool,
) {
    let candidate = candidates
        .entry(file.path.clone())
        .or_insert_with(|| Candidate {
            path: file.path.clone(),
            snapshots: file.snapshots.clone(),
            priority: "possible".into(),
            reasons: BTreeSet::new(),
            linked_changes: BTreeSet::new(),
            evidence: None,
        });
    candidate.reasons.insert(reason.into());
    if let Some(proof) = proof {
        if confirmed {
            candidate.linked_changes.insert(proof.changed.clone());
        }
        let rank = |e: &Evidence| {
            (
                e.depth,
                e.line,
                e.changed.clone(),
                e.snapshot.clone(),
                e.relationship.clone(),
            )
        };
        if candidate
            .evidence
            .as_ref()
            .is_none_or(|e| rank(&proof) < rank(e))
        {
            candidate.priority = if confirmed {
                if proof.depth == 1 {
                    "direct"
                } else {
                    "transitive"
                }
            } else {
                "possible"
            }
            .into();
            candidate.evidence = Some(proof);
        }
    }
}

type Seeds = BTreeMap<String, BTreeMap<DeclKey, Seed>>;

fn seed(seeds: &mut Seeds, id: &str, next: Seed) {
    let entries = seeds.entry(id.into()).or_default();
    if entries.get(&next.changed).is_none_or(|old| {
        (next.depth, &next.changed, &next.via) < (old.depth, &old.changed, &old.via)
    }) {
        entries.insert(next.changed.clone(), next);
    }
}

fn declaration<'a>(
    declarations: &'a [Declaration],
    path: &str,
    kind: &str,
    name: &str,
    id: Option<&str>,
    line: Option<usize>,
) -> Option<&'a Declaration> {
    let matches: Vec<_> = declarations
        .iter()
        .filter(|d| d.path == path && d.kind == kind && d.name == name)
        .collect();
    if let Some(found) = matches
        .iter()
        .find(|d| Some(d.id.as_str()) == id || Some(d.start_line) == line)
    {
        return Some(found);
    }
    (matches.len() == 1).then(|| matches[0])
}

fn seeds_for(report: &ImpactReport, side: &Side<'_>, label: &str) -> Result<Seeds> {
    let declarations = side.store.declarations()?;
    let mut seeds = BTreeMap::new();
    // Both lists share the engine's declaration order; do not rematch overloads
    // or transfer symbol IDs from one snapshot to the other.
    for (change, impact) in report.declarations.iter().zip(&report.impacts) {
        let root = key(&change.path, &change.kind, &change.name);
        let reference = if label == "base" {
            &change.base
        } else {
            &change.head
        };
        if let Some(reference) = reference {
            seed(
                &mut seeds,
                &reference.id,
                Seed {
                    changed: root.clone(),
                    target: key(&reference.path, &change.kind, &change.name),
                    depth: 0,
                    via: vec![change.name.clone()],
                },
            );
        }
        for site in &impact.direct {
            if site.snapshot != "both" && site.snapshot != label {
                continue;
            }
            let Some(consumer) = &site.consumer else {
                continue;
            };
            if let Some(d) = declaration(
                &declarations,
                &site.path,
                &consumer.kind,
                &consumer.name,
                Some(&consumer.id),
                None,
            ) {
                seed(
                    &mut seeds,
                    &d.id,
                    Seed {
                        changed: root.clone(),
                        target: key(&d.path, &d.kind, &d.name),
                        depth: 1,
                        via: vec![change.name.clone(), consumer.name.clone()],
                    },
                );
            }
        }
        for dependent in impact.transitive.iter().filter(|d| d.snapshot == label) {
            if let Some(d) = declaration(
                &declarations,
                &dependent.path,
                &dependent.kind,
                &dependent.name,
                None,
                Some(dependent.line),
            ) {
                seed(
                    &mut seeds,
                    &d.id,
                    Seed {
                        changed: root.clone(),
                        target: key(&d.path, &d.kind, &d.name),
                        depth: dependent.depth,
                        via: dependent.via.clone(),
                    },
                );
            }
        }
    }
    Ok(seeds)
}

fn changed_target(change: &DeclarationChange, snapshot: &str) -> DeclKey {
    let reference = if snapshot == "base" {
        change.base.as_ref()
    } else {
        change.head.as_ref().or(change.base.as_ref())
    };
    key(
        reference.map_or(change.path.as_str(), |r| r.path.as_str()),
        &change.kind,
        &change.name,
    )
}

fn from_report(
    report: &ImpactReport,
    files: &BTreeMap<String, TestFile>,
    confirmed: &mut BTreeMap<String, Candidate>,
    possible: &mut BTreeMap<String, Candidate>,
) {
    for (change, impact) in report.declarations.iter().zip(&report.impacts) {
        let root = key(&impact.path, &impact.kind, &impact.name);
        for site in &impact.direct {
            let Some(file) = files.get(&site.path) else {
                continue;
            };
            let valid = ["base", "head"].iter().any(|label| {
                (site.snapshot == "both" || site.snapshot == *label) && supported(file, label)
            });
            if !valid {
                continue;
            }
            let actual_target = changed_target(change, &site.snapshot);
            insert(
                confirmed,
                file,
                "resolved use of a changed declaration",
                Some(Evidence {
                    snapshot: site.snapshot.clone(),
                    changed: root.clone(),
                    relationship: site.kind.clone(),
                    depth: 1,
                    line: site.line,
                    via: vec![
                        impact.name.clone(),
                        site.consumer
                            .as_ref()
                            .map_or("module scope", |c| c.name.as_str())
                            .into(),
                    ],
                    target: actual_target,
                    rule: site.rule.clone(),
                    binding_chain: site.binding_chain.clone(),
                }),
                true,
            );
        }
        for site in &impact.possible {
            if let Some(file) = files.get(&site.path) {
                insert(
                    possible,
                    file,
                    &format!("{}: static target unverified", site.reason),
                    None,
                    false,
                );
            }
        }
        for site in impact.import_only.iter().chain(&impact.re_exported_by) {
            if let Some(file) = files.get(&site.path) {
                insert(
                    possible,
                    file,
                    IMPORT_RELATIONSHIP,
                    Some(Evidence {
                        snapshot: site.snapshot.clone(),
                        changed: root.clone(),
                        relationship: site.kind.clone(),
                        depth: 1,
                        line: site.line,
                        via: vec![impact.name.clone()],
                        target: changed_target(change, &site.snapshot),
                        rule: site.rule.clone(),
                        binding_chain: site.binding_chain.clone(),
                    }),
                    false,
                );
            }
        }
    }
}

fn terminal_edges(
    side: &Side<'_>,
    label: &str,
    report: &ImpactReport,
    files: &BTreeMap<String, TestFile>,
    depth: usize,
    confirmed: &mut BTreeMap<String, Candidate>,
    possible: &mut BTreeMap<String, Candidate>,
) -> Result<()> {
    let seeds = seeds_for(report, side, label)?;
    side.store.scan_edges(&mut |path, raw| {
        let Some(file) = files.get(path).filter(|file| supported(file, label)) else { return Ok(()); };
        let edge: SemanticEdge = serde_json::from_str(raw)?;
        if edge.status != Resolution::Resolved || edge.target_id.as_ref() == Some(&edge.source_id) { return Ok(()); }
        let Some(prefixes) = edge.target_id.as_ref().and_then(|id| seeds.get(id)) else { return Ok(()); };
        let kind = relationship(edge.kind);
        let is_use = uses(&kind);
        if !is_use && !matches!(edge.kind, Relationship::Imports | Relationship::ReExports) { return Ok(()); }
        for prefix in prefixes.values().filter(|p| p.depth < depth) {
        let mut via = prefix.via.clone();
        via.push(format!("{path}:{}", edge.location.start_line));
        let proof = Some(Evidence { snapshot: label.into(), changed: prefix.changed.clone(), relationship: kind.clone(),
            depth: prefix.depth + 1, line: edge.location.start_line, via, target: prefix.target.clone(),
            rule: edge.evidence.rule.clone(), binding_chain: edge.evidence.binding_chain.clone() });
        if is_use { insert(confirmed, file, "resolved terminal test use of an impact-reached declaration (including module scope)", proof, true); }
        else { insert(possible, file, IMPORT_ONLY, proof, false); }
        }
        Ok(())
    })
}

fn rebound(
    report: &ImpactReport,
    files: &BTreeMap<String, TestFile>,
    confirmed: &mut BTreeMap<String, Candidate>,
    possible: &mut BTreeMap<String, Candidate>,
) {
    for binding in &report.rebound {
        let Some(file) = files.get(&binding.path) else {
            continue;
        };
        let (label, state) = if binding.effect == "lost" {
            ("base", &binding.before)
        } else {
            ("head", &binding.after)
        };
        let target = state.as_ref().and_then(|s| s.target.as_ref());
        let is_confirmed = binding.effect != "lost"
            && uses(&binding.kind)
            && supported(file, label)
            && target.is_some();
        let proof = target.map(|target| Evidence {
            snapshot: label.into(),
            changed: target.clone(),
            target: target.clone(),
            relationship: binding.kind.clone(),
            depth: 1,
            line: binding.line,
            via: vec![binding.target_text.clone()],
            rule: format!(
                "{} binding; static relationship change, not compiler evidence",
                binding.effect
            ),
            binding_chain: Vec::new(),
        });
        let reason = format!(
            "{} binding{}",
            binding.effect,
            if is_confirmed {
                " at a resolved test use"
            } else {
                "; relevance to current code needs verification"
            }
        );
        if is_confirmed {
            insert(confirmed, file, &reason, proof, true);
        } else {
            insert(possible, file, &reason, proof, false);
        }
    }
}

fn heuristics(
    report: &ImpactReport,
    files: &BTreeMap<String, TestFile>,
    possible: &mut BTreeMap<String, Candidate>,
) {
    // Module trivia alone does not justify test recommendations. Inventory and
    // unknown coverage are still reported for a valid no-impact comparison.
    if report.declarations.is_empty()
        && report.modules.is_empty()
        && report.rebound.is_empty()
        && !report.resolution_changed
        && !report.files.iter().any(|f| f.status == "renamed")
    {
        return;
    }
    let affected: BTreeSet<_> = report
        .affected_files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    let parents: BTreeSet<_> = affected
        .iter()
        .map(|path| path.rsplit_once('/').map_or("", |(parent, _)| parent))
        .collect();
    let stems: BTreeSet<_> = affected
        .iter()
        .filter_map(|path| {
            path.rsplit('/')
                .next()?
                .rsplit_once('.')
                .map(|(stem, _)| stem)
        })
        .collect();
    for file in files.values() {
        let parent = file.path.rsplit_once('/').map_or("", |(p, _)| p);
        let name = file.path.rsplit('/').next().unwrap_or_default();
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
        let stem = stem
            .strip_suffix(".test")
            .or_else(|| stem.strip_suffix(".spec"))
            .unwrap_or(stem);
        if parents.contains(parent)
            || (parent == "__tests__" && parents.contains(""))
            || parent
                .strip_suffix("/__tests__")
                .is_some_and(|p| parents.contains(p))
        {
            insert(possible, file, COLOCATED, None, false);
        }
        if stems.contains(stem) {
            insert(possible, file, BASENAME, None, false);
        }
        if report.files.iter().any(|f| f.path == file.path) {
            insert(
                possible,
                file,
                "test file changed or existed only in the baseline; review its own verification",
                None,
                false,
            );
        }
    }
    for module in &report.modules {
        for path in &module.importers {
            if let Some(file) = files.get(path) {
                insert(
                    possible,
                    file,
                    "imports a changed module without a confirmed declaration-use link",
                    None,
                    false,
                );
            }
        }
    }
}

pub fn discover(
    base: &Side<'_>,
    head: &Side<'_>,
    report: &ImpactReport,
    files: &BTreeMap<String, TestFile>,
    depth: usize,
) -> Result<(Vec<Candidate>, Vec<Candidate>)> {
    let (mut confirmed, mut possible) = (BTreeMap::new(), BTreeMap::new());
    from_report(report, files, &mut confirmed, &mut possible);
    terminal_edges(
        base,
        "base",
        report,
        files,
        depth,
        &mut confirmed,
        &mut possible,
    )?;
    terminal_edges(
        head,
        "head",
        report,
        files,
        depth,
        &mut confirmed,
        &mut possible,
    )?;
    rebound(report, files, &mut confirmed, &mut possible);
    heuristics(report, files, &mut possible);
    for (path, candidate) in &mut confirmed {
        if let Some(hints) = possible.remove(path) {
            candidate.reasons.extend(hints.reasons);
        }
    }
    let mut confirmed: Vec<_> = confirmed.into_values().collect();
    confirmed.sort_by(|a, b| {
        (a.evidence.as_ref().map(|e| e.depth), &a.path)
            .cmp(&(b.evidence.as_ref().map(|e| e.depth), &b.path))
    });
    let mut possible: Vec<_> = possible.into_values().collect();
    possible.sort_by(|a, b| (strength(a), &a.path).cmp(&(strength(b), &b.path)));
    Ok((confirmed, possible))
}
