//! Versioned, deterministic evidence composition. No execution or new resolution.
use std::{collections::BTreeMap, io::Write};

use anyhow::{Context, Result, ensure};
use code_intel_core::semantic::SEMANTIC_IR_VERSION;
use code_intel_index::{
    git::Git,
    snapshot::{FileChange, SnapshotInfo},
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    impact::{self, Request},
    tests,
};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_BYTES: usize = 8_000_000;

#[derive(Serialize)]
pub struct Analysis {
    pub atlas_version: &'static str,
    pub build: String,
    pub semantic_ir_version: u32,
    pub requested_base: String,
    pub requested_head: Option<String>,
    pub depth: usize,
    pub max_results: usize,
    pub max_declarations: usize,
    pub incoming_sites_per_node: usize,
    pub evidence_profile: &'static str,
    pub languages: Vec<&'static str>,
    pub inventory_conventions: Vec<&'static str>,
    pub test_directories: Vec<String>,
}

#[derive(Serialize)]
pub struct Snapshot {
    pub kind: String,
    pub requested: Option<String>,
    pub commit: Option<String>,
    pub tree: Option<String>,
    pub source_digest: String,
    pub file_manifest_digest: String,
    pub test_inventory_digest: String,
    pub resolution_digest: Option<String>,
}

#[derive(Serialize, PartialEq, Eq)]
pub struct GitState {
    pub head_commit: String,
    pub index_digest: String,
}

#[derive(Serialize)]
pub struct Snapshots {
    pub base: Snapshot,
    pub head: Snapshot,
    pub git_before: GitState,
    pub git_after: GitState,
    pub git_metadata_verified: bool,
    pub file_inputs_verified: bool,
    pub freshness_verified: bool,
}

#[derive(Serialize)]
pub struct Declaration {
    pub evidence_key: String,
    #[serde(flatten)]
    pub change: impact::DeclarationChange,
}

#[derive(Serialize)]
pub struct Changes {
    pub files: Vec<FileChange>,
    pub declarations: Vec<Declaration>,
    pub unchanged_declarations: usize,
    pub resolution_changed: bool,
    pub modules: Vec<impact::ModuleChange>,
}

#[derive(Serialize)]
pub struct AffectedCode {
    pub files: Vec<impact::summary::File>,
    pub relationships: Vec<Relationships>,
    pub binding_changes: Vec<impact::Rebound>,
}

#[derive(Serialize)]
pub struct Relationships {
    pub changed_declaration: String,
    #[serde(flatten)]
    pub impact: impact::SymbolImpact,
}

#[derive(Serialize, PartialEq, Eq)]
pub struct Gap {
    pub kind: &'static str,
    pub path: Option<String>,
    pub declaration: Option<String>,
    pub snapshot: Option<String>,
    pub reason: String,
}

#[derive(Serialize)]
pub struct Verification {
    pub confirmed: Vec<tests::Candidate>,
    pub possible: Vec<tests::Candidate>,
    pub unknown: Vec<tests::TestFile>,
    pub inventory_counts: BTreeMap<String, usize>,
    pub configurations: Vec<tests::Configuration>,
    pub gaps: Vec<Gap>,
    pub totals: BTreeMap<String, usize>,
    pub behavioral_coverage: &'static str,
    pub execution: &'static str,
}

#[derive(Serialize)]
pub struct Diagnostic {
    pub path: String,
    pub category: &'static str,
}

#[derive(Serialize)]
pub struct SideCoverage {
    pub indexed_files: usize,
    pub syntax_error_files: Vec<String>,
    pub skipped_files: usize,
    pub skipped_sample: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
    pub diagnostic_count: usize,
}

#[derive(Serialize)]
pub struct Coverage {
    pub base: SideCoverage,
    pub head: SideCoverage,
    pub unsupported: Vec<String>,
    pub limitations: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct Completeness {
    #[serde(flatten)]
    pub axes: tests::Completeness,
    pub analysis_limited: bool,
    pub undiscovered_due_to_work_limits: bool,
    pub analysis_omissions: BTreeMap<String, usize>,
    pub output_truncated: bool,
    pub output_omissions: BTreeMap<String, usize>,
    pub excluded_by_scope: BTreeMap<String, usize>,
}

#[derive(Serialize)]
pub struct Payload {
    pub schema_version: u32,
    pub analysis: Analysis,
    pub snapshots: Snapshots,
    pub changes: Changes,
    pub affected_code: AffectedCode,
    pub verification: Verification,
    pub coverage: Coverage,
    pub completeness: Completeness,
}

/// Arrays are sets except the two named, ordered static-proof sequences.
fn canonicalize(value: &mut Value, ordered: bool) {
    match value {
        Value::Array(values) => {
            for value in &mut *values {
                canonicalize(value, false);
            }
            if !ordered {
                values
                    .sort_by_cached_key(|v| serde_json::to_vec(v).expect("JSON value serializes"));
                values.dedup();
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                canonicalize(value, matches!(key.as_str(), "via" | "binding_chain"));
            }
        }
        _ => {}
    }
}

struct BoundedBytes {
    bytes: Vec<u8>,
    max: usize,
}
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other(
                "complete manifest exceeds max-bytes; increase the output budget",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bytes(value: &Value, max: usize) -> Result<Vec<u8>> {
    let mut writer = BoundedBytes {
        bytes: Vec::new(),
        max,
    };
    serde_json::to_writer(&mut writer, value)
        .context("serialize complete verification manifest")?;
    Ok(writer.bytes)
}

/// Canonical payload bytes exclude the identity field and stdout newline.
pub fn canonical_payload(payload: &Payload) -> Result<Vec<u8>> {
    let mut value = serde_json::to_value(payload)?;
    canonicalize(&mut value, false);
    bytes(&value, MAX_BYTES)
}

fn digest(value: &impl Serialize) -> Result<String> {
    let mut value = serde_json::to_value(value)?;
    canonicalize(&mut value, false);
    Ok(format!(
        "blake3:{}",
        blake3::hash(&bytes(&value, MAX_BYTES)?).to_hex()
    ))
}

fn git_state(git: &Git) -> Result<GitState> {
    let entries: BTreeMap<_, _> = git
        .index_entries()?
        .into_iter()
        .map(|(path, entry)| {
            (
                path,
                serde_json::json!({"mode": entry.mode, "oid": entry.oid}),
            )
        })
        .collect();
    Ok(GitState {
        head_commit: git.revision("HEAD")?.commit,
        index_digest: digest(&entries)?,
    })
}

fn snapshot(
    info: &SnapshotInfo,
    inventory: String,
    resolution: Option<String>,
    files: String,
) -> Snapshot {
    Snapshot {
        kind: info.kind.clone(),
        requested: info.spec.clone(),
        commit: if info.kind == "commit" {
            info.commit.clone()
        } else {
            None
        },
        tree: if info.kind == "commit" {
            info.tree.clone()
        } else {
            None
        },
        source_digest: info.digest.clone(),
        file_manifest_digest: files,
        test_inventory_digest: inventory,
        resolution_digest: resolution,
    }
}

fn coverage(side: impact::SideCoverage) -> SideCoverage {
    SideCoverage {
        indexed_files: side.indexed_files,
        syntax_error_files: side.syntax_error_files,
        skipped_files: side.skipped_files,
        skipped_sample: side.skipped_sample,
        diagnostic_count: side.diagnostics.len(),
        diagnostics: side
            .diagnostics
            .into_iter()
            .map(|d| Diagnostic {
                path: d.path,
                category: "indexing_or_materialization_diagnostic",
            })
            .collect(),
    }
}

fn gap(
    kind: &'static str,
    path: Option<String>,
    declaration: Option<String>,
    snapshot: Option<String>,
    reason: impl Into<String>,
) -> Gap {
    Gap {
        kind,
        path,
        declaration,
        snapshot,
        reason: reason.into(),
    }
}

fn gaps(tests: &tests::Report, limited: bool) -> Vec<Gap> {
    let mut gaps: Vec<_> = tests
        .gaps
        .iter()
        .map(|g| {
            gap(
                "no_confirmed_test_link",
                Some(g.path.clone()),
                Some(g.declaration.clone()),
                None,
                g.reason.clone(),
            )
        })
        .collect();
    for file in &tests.unknown {
        gaps.push(gap(
            "unknown_test_semantics",
            Some(file.path.clone()),
            None,
            None,
            file.reasons.iter().cloned().collect::<Vec<_>>().join("; "),
        ));
    }
    for config in tests.configurations.iter().filter(|c| !c.interpreted) {
        gaps.push(gap("uninterpreted_runner_configuration", Some(config.path.clone()), None,
            Some(config.snapshot.clone()), "runner membership and behavioral coverage cannot be established from configuration data alone"));
    }
    for candidate in &tests.possible {
        if candidate.evidence.is_some()
            || candidate
                .reasons
                .iter()
                .any(|r| r.contains("unverified") || r.contains("binding"))
        {
            gaps.push(gap("unverified_test_relationship", Some(candidate.path.clone()), None, None,
                "possible relevance has no confirmed test-use proof; see original candidate reasons"));
        }
    }
    if limited {
        gaps.push(gap("analysis_work_limit", None, None, None,
        "analysis stopped within configured work limits; the number of undiscovered relationships is unknown"));
    }
    if !tests.completeness.snapshots_fresh {
        gaps.push(gap(
            "unverified_freshness",
            None,
            None,
            None,
            "source/configuration or Git state freshness could not be verified",
        ));
    }
    gaps.push(gap("unknown_behavioral_coverage", None, None, None,
        "static linkage does not establish execution, mocked-path behavior or coverage; no tests ran"));
    gaps.sort_by_cached_key(|g| serde_json::to_string(g).expect("gap serializes"));
    gaps.dedup();
    gaps
}

/// Compose the unrendered analyses through one snapshot/comparison lifecycle.
#[allow(clippy::too_many_lines)]
pub fn compare(request: &Request, options: &tests::Options) -> Result<Payload> {
    let git = if request.confined {
        Git::open_confined(&request.root)?
    } else {
        Git::open(&request.root)?
    };
    let before = git_state(&git)?;
    let mut effective = request.clone();
    effective.options.detail = true;
    let tests::Comparison {
        impact,
        mut tests,
        base_resolution,
        head_resolution,
    } = tests::compare_with_impact(&effective, options)?;
    let (base_files, head_files) = impact
        .input_digests
        .clone()
        .context("shared comparison file identities unavailable")?;
    let file_inputs_verified = request.head.is_some()
        || code_intel_index::snapshot::working_files_digest(&git)? == head_files;
    let after = git_state(&git)?;
    let metadata_verified = before == after;
    let summary = impact::summary::project(&impact, request.options.depth);
    let limited = summary.analysis_limited;
    if !metadata_verified
        || !file_inputs_verified
        || base_resolution.is_none()
        || head_resolution.is_none()
    {
        tests.completeness.snapshots_fresh = false;
        tests.completeness.direct_files_complete = false;
        tests.completeness.static_links_complete = false;
        tests.completeness.inventory_complete = false;
        tests.completeness.source_coverage_unknown = true;
    }
    tests.completeness.complete &= summary.complete && tests.completeness.snapshots_fresh;
    let gaps = gaps(&tests, limited);
    let totals = BTreeMap::from([
        ("confirmed".into(), tests.confirmed.len()),
        ("possible".into(), tests.possible.len()),
        ("unknown".into(), tests.unknown.len()),
        ("configurations".into(), tests.configurations.len()),
        ("gaps".into(), gaps.len()),
        ("inventory".into(), tests.inventory.len()),
    ]);
    let declarations = impact
        .declarations
        .into_iter()
        .map(|change| {
            Ok(Declaration {
                evidence_key: digest(&change)?,
                change,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut omissions = impact.omitted.clone();
    omissions.insert(
        "declarations_not_analysed".into(),
        impact.declarations_not_analysed,
    );
    omissions.insert(
        "module_importers".into(),
        impact.modules.iter().map(|m| m.importers_omitted).sum(),
    );
    omissions.insert("rebound".into(), impact.rebound_omitted);
    omissions.insert("affected_files".into(), impact.affected_files_omitted);
    for (i, symbol) in impact.impacts.iter().enumerate() {
        for (name, count) in &symbol.omitted {
            let key = &declarations
                .get(i)
                .context("impact declaration/proof alignment")?
                .evidence_key;
            omissions.insert(format!("relationships.{key}.{name}"), *count);
        }
    }
    omissions.retain(|_, count| *count > 0);
    let relationships = impact
        .impacts
        .into_iter()
        .enumerate()
        .map(|(i, impact)| {
            Ok(Relationships {
                changed_declaration: declarations
                    .get(i)
                    .context("impact declaration/proof alignment")?
                    .evidence_key
                    .clone(),
                impact,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Payload {
        schema_version: SCHEMA_VERSION,
        analysis: Analysis {
            atlas_version: env!("CARGO_PKG_VERSION"),
            build: code_intel_index::build_key()?,
            semantic_ir_version: SEMANTIC_IR_VERSION,
            requested_base: request.base.clone(),
            requested_head: request.head.clone(),
            depth: request.options.depth,
            max_results: request.options.max_results,
            max_declarations: request.options.max_declarations,
            incoming_sites_per_node: 5_000,
            evidence_profile: "selected_static_proofs_v1",
            languages: vec!["typescript", "tsx"],
            inventory_conventions: vec![
                "test/spec filename conventions",
                "__tests__ directories",
                "literal configured test directories",
                ".gitignore and built-in exclusions; no runner membership proof",
            ],
            test_directories: options.directories.clone(),
        },
        snapshots: Snapshots {
            base: snapshot(
                &impact.base,
                tests.base_inventory_digest,
                base_resolution,
                base_files,
            ),
            head: snapshot(
                &impact.head,
                tests.head_inventory_digest,
                head_resolution,
                head_files,
            ),
            git_before: before,
            git_after: after,
            git_metadata_verified: metadata_verified,
            file_inputs_verified,
            freshness_verified: tests.completeness.snapshots_fresh,
        },
        changes: Changes {
            files: impact.files,
            declarations,
            unchanged_declarations: impact.unchanged_declarations,
            resolution_changed: impact.resolution_changed,
            modules: impact.modules,
        },
        affected_code: AffectedCode {
            files: summary.files,
            relationships,
            binding_changes: impact.rebound,
        },
        verification: Verification {
            confirmed: tests.confirmed,
            possible: tests.possible,
            unknown: tests.unknown,
            inventory_counts: tests.inventory_counts,
            configurations: tests.configurations,
            gaps,
            totals,
            behavioral_coverage: "unknown",
            execution: "not_performed",
        },
        coverage: Coverage {
            base: coverage(impact.coverage.base),
            head: coverage(impact.coverage.head),
            unsupported: impact.coverage.not_analysed,
            limitations: tests.limitations,
            warnings: impact.warnings,
        },
        completeness: Completeness {
            axes: tests.completeness,
            analysis_limited: limited,
            undiscovered_due_to_work_limits: limited,
            analysis_omissions: omissions,
            output_truncated: false,
            output_omissions: BTreeMap::new(),
            excluded_by_scope: BTreeMap::new(),
        },
    })
}

/// All discovered evidence or an error: no output fitting or repository writes.
pub fn respond(request: &Request, options: &tests::Options, max_bytes: usize) -> Result<String> {
    ensure!(
        (2_000..=MAX_BYTES).contains(&max_bytes),
        "manifest max-bytes must be between 2000 and {MAX_BYTES}"
    );
    let payload = compare(request, options)?;
    let canonical = canonical_payload(&payload)?;
    let id = format!("blake3:{}", blake3::hash(&canonical).to_hex());
    let mut value: Value = serde_json::from_slice(&canonical)?;
    value
        .as_object_mut()
        .context("manifest payload must be an object")?
        .insert("manifest_id".into(), Value::String(id));
    let mut output = bytes(&value, max_bytes - 1)?;
    output.push(b'\n');
    String::from_utf8(output).context("canonical JSON must be UTF-8")
}

#[cfg(test)]
mod unit {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_sets_deduplicate_but_proof_order_is_content() {
        let mut a = json!({"paths":["b","a","a"],"via":["root","leaf"],"binding_chain":["import","export"]});
        let mut b =
            json!({"binding_chain":["import","export"],"via":["root","leaf"],"paths":["a","b"]});
        canonicalize(&mut a, false);
        canonicalize(&mut b, false);
        assert_eq!(bytes(&a, 2000).unwrap(), bytes(&b, 2000).unwrap());
        b["via"] = json!(["leaf", "root"]);
        assert_ne!(digest(&a).unwrap(), digest(&b).unwrap());
    }

    #[test]
    fn too_small_serialization_never_succeeds_with_a_prefix() {
        assert!(bytes(&json!({"confirmed":["some/test/path"]}), 4).is_err());
    }
}
