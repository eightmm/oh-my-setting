//! Read-only provider account contracts shared by connect, peer and panel.
//! Filesystem reads, physical-path resolution, implicit Claude defaults and
//! account selection remain with the caller. A decoded registry is not a pin.

use serde::{Deserialize, Serialize};
use serde_json::Map;
pub use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Providers with a proven per-account home boundary, in legacy peer order.
#[derive(Clone, Copy, Debug)]
pub struct HomeDescriptor {
    pub name: &'static str,
    pub env: &'static str,
    pub home_sub: &'static str,
}
pub const HOME_DESCRIPTORS: [HomeDescriptor; 3] = [
    HomeDescriptor {
        name: "codex",
        env: "CODEX_HOME",
        home_sub: ".codex",
    },
    HomeDescriptor {
        name: "claude",
        env: "CLAUDE_CONFIG_DIR",
        home_sub: ".claude",
    },
    HomeDescriptor {
        name: "pi",
        env: "PI_CODING_AGENT_DIR",
        home_sub: ".pi/agent",
    },
];
pub const ACCOUNT_PROVIDERS: &[&str] = &[
    HOME_DESCRIPTORS[0].name,
    HOME_DESCRIPTORS[1].name,
    HOME_DESCRIPTORS[2].name,
];
pub fn home_descriptor(provider: &str) -> Option<&'static HomeDescriptor> {
    HOME_DESCRIPTORS.iter().find(|spec| spec.name == provider)
}

pub fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && (id.as_bytes()[0].is_ascii_lowercase() || id.as_bytes()[0].is_ascii_digit())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}

/// Preserve peer's historical Python-number schema comparison and null policy.
/// New writers and panel previews retain the strict JSON schema representation.
#[derive(Clone, Copy)]
pub enum Dialect {
    Strict,
    PeerLegacy,
}

/// The caller supplies its existing byte bound and maps errors to its own UI.
/// Safe file ownership, symlink and unavailable handling are deliberately outside.
pub fn decode(bytes: &[u8], maximum: usize) -> Result<Value, &'static str> {
    crate::parse_json_bounded(bytes, maximum)?;
    serde_json::from_slice(bytes).map_err(|_| "invalid provider accounts JSON")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    Fields,
    Schema,
}
fn fields(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|obj| obj.keys().all(|key| allowed.contains(&key.as_str())))
}
fn schema_one(value: &Value, dialect: Dialect) -> bool {
    match dialect {
        Dialect::Strict => value.as_u64() == Some(1),
        Dialect::PeerLegacy => value == &Value::Bool(true) || value.as_f64() == Some(1.0),
    }
}
pub fn shared_providers(
    value: &Value,
    dialect: Dialect,
) -> Result<&Map<String, Value>, HeaderError> {
    if !fields(value, &["schema", "providers"]) || !value["providers"].is_object() {
        return Err(HeaderError::Fields);
    }
    if !schema_one(&value["schema"], dialect) {
        return Err(HeaderError::Schema);
    }
    Ok(value["providers"].as_object().unwrap())
}
pub fn claude_accounts(value: &Value, dialect: Dialect) -> Result<&[Value], HeaderError> {
    if !fields(value, &["schema", "provider", "policy", "accounts"])
        || value["provider"] != "claude"
        || !value["accounts"].is_array()
    {
        return Err(HeaderError::Fields);
    }
    if !schema_one(&value["schema"], dialect) {
        return Err(HeaderError::Schema);
    }
    Ok(value["accounts"].as_array().unwrap())
}
pub fn settings_fields(value: &Value) -> bool {
    fields(value, &["policy", "accounts"])
}
pub fn policy(value: &Value, dialect: Dialect) -> Option<&str> {
    match value.get("policy") {
        None => Some("ordered"),
        Some(Value::Null) if matches!(dialect, Dialect::PeerLegacy) => Some("ordered"),
        Some(Value::String(policy)) if matches!(policy.as_str(), "ordered" | "spread") => {
            Some(policy)
        }
        _ => None,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub id: String,
    pub home: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAccounts {
    #[serde(default = "ordered")]
    pub policy: String,
    pub accounts: Vec<Account>,
}
fn ordered() -> String {
    "ordered".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowError {
    Fields,
    IdOrHome,
}
pub fn account_row(value: &Value) -> Result<(&str, &Path), RowError> {
    if !fields(value, &["id", "home"]) || value.get("id").is_none() || value.get("home").is_none() {
        return Err(RowError::Fields);
    }
    let id = value["id"]
        .as_str()
        .filter(|id| valid_account_id(id))
        .ok_or(RowError::IdOrHome)?;
    let home = value["home"]
        .as_str()
        .map(Path::new)
        .filter(|home| home.is_absolute())
        .ok_or(RowError::IdOrHome)?;
    Ok((id, home))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplicate {
    Id,
    Home,
}
/// Check provider-local IDs and cross-provider physical homes. The caller must
/// resolve the home under its existing filesystem policy before reserving it.
pub fn reserve_identity(
    ids: &mut BTreeSet<String>,
    homes: &mut BTreeSet<PathBuf>,
    id: &str,
    home: &Path,
) -> Result<(), Duplicate> {
    if !ids.insert(id.to_owned()) {
        return Err(Duplicate::Id);
    }
    if !homes.insert(home.to_owned()) {
        return Err(Duplicate::Home);
    }
    Ok(())
}
