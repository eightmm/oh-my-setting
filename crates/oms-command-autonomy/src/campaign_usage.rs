//! Soft campaign admission, using lifetime physical-call receipts.
use crate::{Error, Result, value};
use oms_command_telemetry::store::{budget, campaign, physical, usage};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug)]
pub(crate) struct Policy {
    pub(crate) inner: budget::Policy,
}
pub(crate) fn policy(budget: &Value) -> Result<Option<Policy>> {
    let raw = value::canonical(budget)?;
    let shared =
        oms_json::parse_strict(&raw).map_err(|_| Error::new(2, "invalid campaign budget JSON"))?;
    campaign::usage_policy(&shared)
        .map(|policy| policy.map(|inner| Policy { inner }))
        .map_err(|error| Error::new(error.exit_code(), error.to_string()))
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
    view: &campaign::View,
    interrupted: &AtomicBool,
) -> Result<Value> {
    let intents = physical::load(repo, interrupted)
        .map_err(|error| Error::new(error.exit_code(), error.to_string()))?;
    let projection = usage::load(repo).map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let operations = view.operations();
    let claims = intents
        .iter()
        .filter(|intent| {
            view.reservations
                .contains_key(&intent.identity.operation_id)
        })
        .map(|intent| budget::PhysicalClaim {
            call_id: &intent.identity.call_id,
            operation_id: &intent.identity.operation_id,
        })
        .collect::<Vec<_>>();
    let spent = budget::project(&policy.inner, &projection, &operations, &claims).map_err(error)?;
    Ok(
        json!({"basis":"reported usage and explicit reservations; soft admission, not a provider spending cap",
        "physical_calls":spent.physical_calls,"intent_only_calls":spent.intent_only_calls,"conflicts":spent.conflicts,
        "provider_estimate_calls":spent.provider_estimate_calls,
        "tokens":dimension(&spent.tokens),"cost_microusd":dimension(&spent.cost_microusd)}),
    )
}
