//! Independent envelope validation, shared by the CLI consumer and transport.
use anyhow::{Context, Result, ensure};
use code_intel_retrieval::manifest::{MAX_BYTES, SCHEMA_VERSION};
use serde_json::{Value, json};

pub struct Verified<'a> {
    pub bytes: &'a [u8],
    pub value: Value,
    pub manifest_id: String,
    pub envelope_digest: String,
}

pub fn digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Validate the consumer-visible bytes, without calling the manifest generator.
pub fn verify(bytes: &[u8]) -> Result<Verified<'_>> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_BYTES,
        "invalid artifact byte length"
    );
    let mut value: Value = serde_json::from_slice(bytes).context("invalid manifest JSON")?;
    ensure!(
        value["schema_version"].as_u64() == Some(u64::from(SCHEMA_VERSION)),
        "unsupported manifest schema_version"
    );
    let object = value
        .as_object_mut()
        .context("manifest must be an object")?;
    ensure!(object.len() == 9, "unexpected schema-1 envelope fields");
    for key in [
        "analysis",
        "snapshots",
        "changes",
        "affected_code",
        "verification",
        "coverage",
        "completeness",
    ] {
        ensure!(
            object.get(key).is_some_and(Value::is_object),
            "missing manifest section {key}"
        );
    }
    let id = object
        .get("manifest_id")
        .and_then(Value::as_str)
        .context("missing manifest_id")?
        .to_owned();
    ensure!(valid_id(&id), "invalid manifest_id");
    let mut canonical = serde_json::to_vec(&value)?;
    canonical.push(b'\n');
    ensure!(canonical == bytes, "noncanonical manifest envelope");
    value.as_object_mut().unwrap().remove("manifest_id");
    check_sets(&value, false)?;
    ensure!(
        digest(&serde_json::to_vec(&value)?) == id,
        "manifest_id mismatch"
    );
    value
        .as_object_mut()
        .unwrap()
        .insert("manifest_id".into(), json!(id));
    ensure!(
        value["snapshots"]["base"].is_object() && value["snapshots"]["head"].is_object(),
        "missing snapshot identity"
    );
    ensure!(
        value["completeness"]["output_truncated"] == false
            && value["completeness"]["output_omissions"] == json!({}),
        "artifact contains output omissions"
    );
    Ok(Verified {
        bytes,
        value,
        manifest_id: id,
        envelope_digest: digest(bytes),
    })
}

fn check_sets(value: &Value, ordered: bool) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                check_sets(child, matches!(key.as_str(), "via" | "binding_chain"))?;
            }
        }
        Value::Array(rows) => {
            for child in rows {
                check_sets(child, false)?;
            }
            if !ordered {
                let encoded = rows
                    .iter()
                    .map(serde_json::to_vec)
                    .collect::<Result<Vec<_>, _>>()?;
                ensure!(
                    encoded.windows(2).all(|w| w[0] < w[1]),
                    "noncanonical manifest set array"
                );
            }
        }
        Value::Number(n) => ensure!(n.is_i64() || n.is_u64(), "noninteger manifest number"),
        _ => {}
    }
    Ok(())
}

impl Verified<'_> {
    pub fn report(&self) -> Value {
        json!({"kind":"verification_manifest_validation","valid":true,
            "schema_version":SCHEMA_VERSION,"manifest_id":self.manifest_id,
            "envelope_digest":self.envelope_digest,"total_bytes":self.bytes.len()})
    }
}
