//! Soft campaign admission, using lifetime physical-call receipts.
use crate::{Error, Result, require, value};
use oms_command_telemetry::store::{budget, usage};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct Policy {
    inner: budget::Policy,
}
pub(crate) fn policy(budget: &Value) -> Result<Option<Policy>> {
    let Some(raw) = budget.get("usage") else {
        return Ok(None);
    };
    value::keys(raw, &["per_call"], &["max_tokens", "max_cost_microusd"])?;
    value::keys(&raw["per_call"], &[], &["tokens", "cost_microusd"])?;
    let pair = |total: &str, reserve: &str| -> Result<(Option<u64>, Option<u64>)> {
        match (raw.get(total), raw["per_call"].get(reserve)) {
            (None, None) => Ok((None, None)),
            (Some(max), Some(each)) => {
                let max = value::integer(max, 1, i64::MAX as u64, total)?;
                let each = value::integer(each, 1, max, reserve)?;
                Ok((Some(max), Some(each)))
            }
            _ => Err(Error::new(
                2,
                "usage cap requires its explicit per_call reservation",
            )),
        }
    };
    let (max_tokens, tokens) = pair("max_tokens", "tokens")?;
    let (max_cost_microusd, cost_microusd) = pair("max_cost_microusd", "cost_microusd")?;
    require(
        max_tokens.is_some() || max_cost_microusd.is_some(),
        "usage budget needs at least one total cap",
    )?;
    let limit = |max: Option<u64>, each: Option<u64>| {
        max.zip(each)
            .map(|(max, per_call)| budget::Limit { max, per_call })
    };
    Ok(Some(Policy {
        inner: budget::Policy {
            tokens: limit(max_tokens, tokens),
            cost_microusd: limit(max_cost_microusd, cost_microusd),
        },
    }))
}
fn error(error: budget::Error) -> Error {
    let message = match &error {
        budget::Error::Conflict => "campaign has conflicting provider usage receipts".into(),
        _ => format!("campaign {error}"),
    };
    Error::new(error.exit_code(), message)
}
impl Policy {
    pub fn reservation(&self) -> Value {
        json!({"tokens":self.inner.tokens.map(|limit| limit.per_call),
            "cost_microusd":self.inner.cost_microusd.map(|limit| limit.per_call)})
    }
    pub fn admit(&self, totals: &Value) -> Result<()> {
        let used = |key: &str, enabled: bool| -> Result<u64> {
            if !enabled {
                return Ok(0);
            }
            totals[key]["committed"]
                .as_u64()
                .ok_or_else(|| Error::new(2, "invalid usage commitment"))
        };
        self.inner
            .admit_next_logical(budget::Commitment {
                tokens: used("tokens", self.inner.tokens.is_some())?,
                cost_microusd: used("cost_microusd", self.inner.cost_microusd.is_some())?,
                conflicts: totals["conflicts"].as_u64().unwrap_or(0),
            })
            .map_err(error)
    }
}
fn dimension(value: &budget::Dimension) -> Value {
    json!({"measured":(value.reports > 0).then_some(value.measured),"reports":value.reports,
        "settled":value.settled,"active_reserved":value.active_reserved,
        "unresolved_reserved":value.unresolved_reserved,"committed":value.committed,
        "unknown_operations":value.unknown_operations})
}
pub(crate) fn summary(
    repo: &Path,
    policy: &Policy,
    rows: &[Value],
    reservations: &BTreeMap<String, Value>,
    settled: &BTreeSet<String>,
) -> Result<Value> {
    let projection = usage::load(repo).map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let operations = reservations
        .keys()
        .map(|id| budget::Operation {
            id,
            state: if !settled.contains(id) {
                budget::OperationState::Active
            } else if rows
                .iter()
                .any(|r| r["type"] == "settled" && r["id"] == *id && r["outcome"] == "refused")
            {
                budget::OperationState::RefusedBeforeInvocation
            } else {
                budget::OperationState::Settled
            },
        })
        .collect::<Vec<_>>();
    let spent = budget::project(&policy.inner, &projection, &operations, &[]).map_err(error)?;
    Ok(
        json!({"basis":"reported usage and explicit reservations; soft admission, not a provider spending cap",
        "physical_calls":spent.physical_calls,"conflicts":spent.conflicts,
        "provider_estimate_calls":spent.provider_estimate_calls,
        "tokens":dimension(&spent.tokens),"cost_microusd":dimension(&spent.cost_microusd)}),
    )
}
