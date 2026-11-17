//! Shared saved-ledger validation for soft usage accounting. Canonical bytes
//! and an active reservation identify a policy; they confer no owner authority.
use super::{StoreError, budget};
use oms_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_MANIFEST: usize = 1024 * 1024;
pub const MAX_EVENTS: usize = 16 * MAX_MANIFEST;
pub const KINDS: &[&str] = &["seat", "consolidator", "verifier", "repair", "final"];
type Result<T> = std::result::Result<T, StoreError>;
static NULL: Value = Value::Null;
fn invalid(message: &'static str) -> StoreError {
    StoreError::Invalid(message)
}
pub fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&NULL)
}
fn equal(left: &Value, right: &Value) -> Result<bool> {
    let encode = |value| {
        oms_json::canonical_json(value).map_err(|_| invalid("cannot encode campaign evidence"))
    };
    Ok(encode(left)? == encode(right)?)
}
pub fn text(value: &Value) -> String {
    value
        .as_string()
        .and_then(|s| s.to_str_checked())
        .unwrap_or_default()
}
fn keys(value: &Value, required: &[&str], optional: &[&str]) -> Result<()> {
    let map = value
        .as_object()
        .ok_or(invalid("campaign expects an object"))?;
    if required.iter().any(|key| value.get(key).is_none())
        || map.keys().any(|key| {
            key.to_str_checked().is_none_or(|key| {
                !required.contains(&key.as_str()) && !optional.contains(&key.as_str())
            })
        })
    {
        return Err(invalid("malformed or unsupported campaign fields"));
    }
    Ok(())
}
fn integer(value: &Value, low: u64, high: u64) -> Result<u64> {
    value
        .as_integer()
        .and_then(|n| n.as_u64())
        .filter(|n| (low..=high).contains(n))
        .ok_or(invalid("campaign integer exceeds its contract bounds"))
}
fn bounded(value: &Value, cap: usize) -> Result<String> {
    let text = text(value);
    if text.trim().is_empty() || text.len() > cap || text.contains('\0') {
        return Err(invalid(
            "campaign text is empty or exceeds its contract bounds",
        ));
    }
    Ok(text)
}
pub fn digest(value: &str) -> bool {
    value.len() == 64 && hex(value)
}
fn hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn directory(manifest_sha256: &str) -> Result<String> {
    if !digest(manifest_sha256) {
        return Err(invalid("invalid campaign manifest digest"));
    }
    Ok(format!(".oms/plan/campaigns/{}", &manifest_sha256[..16]))
}

pub fn usage_policy(budget_value: &Value) -> Result<Option<budget::Policy>> {
    let Some(raw) = budget_value.get("usage") else {
        return Ok(None);
    };
    keys(raw, &["per_call"], &["max_tokens", "max_cost_microusd"])?;
    keys(field(raw, "per_call"), &[], &["tokens", "cost_microusd"])?;
    let pair = |total: &str, reserve: &str| -> Result<Option<budget::Limit>> {
        match (raw.get(total), field(raw, "per_call").get(reserve)) {
            (None, None) => Ok(None),
            (Some(max), Some(each)) => {
                let max = integer(max, 1, i64::MAX as u64)?;
                Ok(Some(budget::Limit {
                    max,
                    per_call: integer(each, 1, max)?,
                }))
            }
            _ => Err(invalid(
                "usage cap requires its explicit per-call reservation",
            )),
        }
    };
    let policy = budget::Policy {
        tokens: pair("max_tokens", "tokens")?,
        cost_microusd: pair("max_cost_microusd", "cost_microusd")?,
    };
    policy.validate().map_err(StoreError::Budget)?;
    Ok(Some(policy))
}
pub fn reservation(policy: &budget::Policy) -> Value {
    Value::Object(
        [
            (
                "tokens".into(),
                policy.tokens.map_or(Value::Null, |limit| {
                    Value::integer(&limit.per_call.to_string()).unwrap()
                }),
            ),
            (
                "cost_microusd".into(),
                policy.cost_microusd.map_or(Value::Null, |limit| {
                    Value::integer(&limit.per_call.to_string()).unwrap()
                }),
            ),
        ]
        .into_iter()
        .collect(),
    )
}
pub struct Manifest {
    pub value: Value,
    pub sha256: String,
    pub policy: Option<budget::Policy>,
    pub providers: BTreeSet<String>,
}
pub fn saved_manifest(raw: &[u8], campaign_id: &str) -> Result<Manifest> {
    if raw.len() > MAX_MANIFEST || campaign_id.len() != 16 || !hex(campaign_id) {
        return Err(invalid("invalid campaign manifest size or directory"));
    }
    let value =
        oms_json::parse_strict(raw).map_err(|_| invalid("invalid campaign manifest JSON"))?;
    let sha256 = format!("{:x}", Sha256::digest(raw));
    if !sha256.starts_with(campaign_id)
        || oms_json::canonical_json(&value)
            .map_err(|_| invalid("cannot encode campaign manifest"))?
            != raw
    {
        return Err(invalid("campaign manifest digest mismatch"));
    }
    keys(
        &value,
        &[
            "problem",
            "base",
            "verify",
            "candidate_formats",
            "groups",
            "budget",
        ],
        &["public_data", "consolidator"],
    )?;
    let budget = field(&value, "budget");
    keys(
        budget,
        &[
            "max_concurrent",
            "total_calls",
            "deadline_s",
            "max_waves",
            "stall_waves",
            "verifier",
        ],
        &["per_provider", "usage"],
    )?;
    for (key, high) in [
        ("total_calls", 200),
        ("max_concurrent", 16),
        ("max_waves", 20),
        ("deadline_s", 86400),
        ("stall_waves", 20),
    ] {
        integer(field(budget, key), 1, high)?;
    }
    keys(
        field(budget, "verifier"),
        &["concurrency", "timeout_s", "max_artifact_bytes"],
        &[],
    )?;
    for (key, high) in [
        ("concurrency", 4),
        ("timeout_s", 3600),
        ("max_artifact_bytes", 1073741824),
    ] {
        integer(field(field(budget, "verifier"), key), 1, high)?;
    }
    let policy = usage_policy(budget)?;
    let groups = field(&value, "groups")
        .as_array()
        .ok_or(invalid("saved campaign groups are malformed"))?;
    if groups.is_empty() || groups.len() > 8 {
        return Err(invalid("saved campaign groups are malformed"));
    }
    let mut providers = BTreeSet::new();
    let mut route = |route: &Value| -> Result<()> {
        keys(route, &["provider", "model"], &["effort"])?;
        providers.insert(bounded(field(route, "provider"), 128)?);
        bounded(field(route, "model"), 160)?;
        Ok(())
    };
    for group in groups {
        keys(group, &["id", "formulation", "k", "routes"], &[])?;
        let routes = field(group, "routes")
            .as_array()
            .ok_or(invalid("saved campaign routes are malformed"))?;
        if routes.is_empty() || routes.len() > 64 {
            return Err(invalid("saved campaign routes are malformed"));
        }
        for r in routes {
            route(r)?;
        }
    }
    if let Some(consolidator) = value.get("consolidator") {
        route(consolidator)?;
    }
    if let Some(caps) = budget.get("per_provider") {
        for (provider, cap) in caps
            .as_object()
            .ok_or(invalid("campaign provider caps must be an object"))?
        {
            if !provider.to_str_checked().is_some_and(|provider| {
                !provider.is_empty() && provider.len() <= 128 && !provider.contains('\0')
            }) {
                return Err(invalid("invalid provider in campaign cap"));
            }
            keys(cap, &["calls", "concurrency"], &[])?;
            integer(field(cap, "calls"), 1, 200)?;
            integer(field(cap, "concurrency"), 1, 16)?;
        }
    }
    Ok(Manifest {
        value,
        sha256,
        policy,
        providers,
    })
}
pub struct View {
    pub rows: Vec<Value>,
    pub reservations: BTreeMap<String, Value>,
    pub settled: BTreeSet<String>,
}
pub fn saved_events(manifest: &Manifest, raw: &[u8]) -> Result<View> {
    if raw.len() > MAX_EVENTS || !raw.ends_with(b"\n") {
        return Err(invalid(
            "campaign events exceed bounds or contain an incomplete row",
        ));
    }
    let rows = raw
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            if line.len() > MAX_MANIFEST {
                return Err(invalid("campaign event exceeds row bound"));
            }
            oms_json::parse_strict(line).map_err(|_| invalid("invalid campaign event JSON"))
        })
        .collect::<Result<Vec<_>>>()?;
    let first = rows.first().ok_or(invalid("campaign start unavailable"))?;
    if text(field(first, "type")) != "campaign_started"
        || !equal(field(first, "budget"), field(&manifest.value, "budget"))?
    {
        return Err(invalid("campaign start budget differs from manifest"));
    }
    let mut reservations = BTreeMap::new();
    let mut settled = BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        if row.as_object().is_none() {
            return Err(invalid("invalid campaign event"));
        }
        let kind = bounded(field(row, "type"), 128)?;
        let id = text(field(row, "id"));
        if kind == "campaign_started" && index != 0 {
            return Err(invalid("duplicate campaign start event"));
        }
        if kind == "reservation" {
            keys(
                row,
                &["type", "id", "provider", "kind"],
                &["reserved_usage", "ts"],
            )?;
            if !id
                .strip_prefix("res_")
                .is_some_and(|id| id.len() == 32 && hex(id))
                || !manifest.providers.contains(&text(field(row, "provider")))
                || !KINDS.contains(&text(field(row, "kind")).as_str())
                || reservations.contains_key(&id)
            {
                return Err(invalid("invalid reservation event"));
            }
            if let Some(policy) = &manifest.policy
                && !equal(field(row, "reserved_usage"), &reservation(policy))?
            {
                return Err(invalid("campaign usage reservation differs from manifest"));
            }
            reservations.insert(
                id,
                row.clone_bounded()
                    .map_err(|_| invalid("campaign event exceeds nesting bound"))?,
            );
        } else if kind == "settled" {
            keys(row, &["type", "id", "outcome"], &["ts"])?;
            bounded(field(row, "outcome"), 256)?;
            if !reservations.contains_key(&id) || !settled.insert(id) {
                return Err(invalid("invalid settlement event"));
            }
        }
    }
    Ok(View {
        rows,
        reservations,
        settled,
    })
}
impl View {
    pub fn operations(&self) -> Vec<budget::Operation<'_>> {
        self.reservations
            .keys()
            .map(|id| budget::Operation {
                id,
                state: if !self.settled.contains(id) {
                    budget::OperationState::Active
                } else if self.rows.iter().any(|row| {
                    text(field(row, "type")) == "settled"
                        && text(field(row, "id")) == *id
                        && text(field(row, "outcome")) == "refused"
                }) {
                    budget::OperationState::RefusedBeforeInvocation
                } else {
                    budget::OperationState::Settled
                },
            })
            .collect()
    }
    /// This validates a soft budget locator, not a controller/root generation.
    pub fn active(
        &self,
        manifest: &Manifest,
        operation: &str,
        provider: &str,
        now: i64,
    ) -> Result<i64> {
        if !self
            .reservations
            .get(operation)
            .is_some_and(|row| text(field(row, "provider")) == provider)
            || self.settled.contains(operation)
            || self.rows.iter().any(|row| {
                ["campaign_stopped", "campaign_solved"].contains(&text(field(row, "type")).as_str())
            })
        {
            return Err(invalid(
                "campaign physical call has no active matching reservation",
            ));
        }
        let start = self
            .rows
            .first()
            .ok_or(invalid("campaign start unavailable"))?;
        let start = chrono::DateTime::parse_from_rfc3339(&text(field(start, "started_at")))
            .map_err(|_| invalid("invalid campaign start timestamp"))?
            .timestamp();
        let seconds = integer(
            field(field(&manifest.value, "budget"), "deadline_s"),
            1,
            86400,
        )?;
        let deadline = start
            .checked_add(seconds as i64)
            .ok_or(invalid("campaign deadline overflow"))?;
        if now >= deadline {
            return Err(StoreError::Budget(budget::Error::Exhausted("deadline")));
        }
        Ok(deadline)
    }
}
