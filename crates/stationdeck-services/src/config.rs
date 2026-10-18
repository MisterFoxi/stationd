//! Versioned, transport-free configuration authoring and plugin schemas.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub const MAX_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub key: String,
    pub kind: String,
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    pub plugin: String,
    pub version: u32,
    pub fields: Vec<Field>,
    pub requires: Vec<String>,
    pub apply_mode: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub plugin: String,
    pub instance: String,
    pub version: u32,
    pub values: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub authority: String,
    pub target: String,
    pub revision: u64,
    pub documents: Vec<Document>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentReport {
    pub revision: u64,
    pub applied: bool,
    pub code: String,
    pub observed_at: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetView {
    pub id: String,
    pub draft_version: u64,
    pub draft: Vec<Document>,
    pub published: Option<Bundle>,
    #[serde(default)]
    pub history: Vec<Bundle>,
    pub report: Option<AgentReport>,
    #[serde(default)]
    pub applied_revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftRequest {
    pub base_draft: u64,
    pub documents: Vec<Document>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub draft_version: u64,
    pub base_revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub authority: String,
    pub schemas: Vec<Schema>,
    pub targets: Vec<TargetView>,
    pub permissions: Vec<String>,
}
pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_.".contains(&c))
        && value.as_bytes()[0].is_ascii_lowercase()
}
pub fn validate(
    schemas: &[Schema],
    allowed: &[String],
    docs: &[Document],
) -> Result<(), &'static str> {
    if docs.is_empty()
        || docs.len() > 64
        || serde_json::to_vec(docs)
            .map_err(|_| "Invalid document.")?
            .len()
            > MAX_BYTES
    {
        return Err("Configuration must contain 1 to 64 documents within 128 KiB.");
    }
    let mut instances = BTreeSet::new();
    for doc in docs {
        if !valid_id(&doc.instance)
            || !instances.insert((&doc.plugin, &doc.instance))
            || !allowed.contains(&doc.plugin)
        {
            return Err(
                "Plugin instance is invalid, duplicated, or not authorized for this target.",
            );
        }
        let schema = schemas
            .iter()
            .find(|s| s.plugin == doc.plugin && s.version == doc.version)
            .ok_or("Plugin schema version is not supported.")?;
        if schema.fields.len() != doc.values.len() {
            return Err("Configuration fields do not match the schema.");
        }
        for field in &schema.fields {
            let value = doc
                .values
                .get(&field.key)
                .ok_or("A required configuration field is missing.")?;
            let valid = match field.kind.as_str() {
                "integer" => value.as_i64().is_some_and(|n| {
                    field.minimum.is_none_or(|v| n >= v) && field.maximum.is_none_or(|v| n <= v)
                }),
                "boolean" => value.is_boolean(),
                "string" => value
                    .as_str()
                    .is_some_and(|s| s.len() <= 4096 && !s.contains('\0')),
                _ => false,
            };
            if !valid {
                return Err("Configuration value does not satisfy its plugin schema.");
            }
        }
        if schema
            .requires
            .iter()
            .any(|id| !docs.iter().any(|d| &d.plugin == id))
        {
            return Err("A required plugin configuration is missing.");
        }
    }
    Ok(())
}
