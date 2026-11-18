//! Soft campaign admission, using lifetime physical-call receipts.
use crate::{Error, Result, require, value};
use oms_command_telemetry::store::usage::{self, Cost, Phase};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct Policy {
    max_tokens: Option<u64>,
    max_cost_microusd: Option<u64>,
    tokens: Option<u64>,
    cost_microusd: Option<u64>,
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
    Ok(Some(Policy {
        max_tokens,
        max_cost_microusd,
        tokens,
        cost_microusd,
    }))
}
impl Policy {
    pub fn reservation(&self) -> Value {
        json!({"tokens":self.tokens,"cost_microusd":self.cost_microusd})
    }
    pub fn admit(&self, totals: &Value) -> Result<()> {
        if totals["conflicts"].as_u64().unwrap_or(0) != 0 {
            return Err(Error::new(
                3,
                "campaign has conflicting provider usage receipts",
            ));
        }
        for (limit, reserve, key) in [
            (self.max_tokens, self.tokens, "tokens"),
            (self.max_cost_microusd, self.cost_microusd, "cost_microusd"),
        ] {
            if let (Some(max), Some(each)) = (limit, reserve) {
                let used = totals[key]["committed"]
                    .as_u64()
                    .ok_or_else(|| Error::new(2, "invalid usage commitment"))?;
                if used.checked_add(each).is_none_or(|n| n > max) {
                    return Err(Error::new(
                        3,
                        format!("campaign {key} admission budget exhausted"),
                    ));
                }
            }
        }
        Ok(())
    }
}
#[derive(Default)]
struct Dimension {
    measured: u64,
    reports: u64,
    settled: u64,
    active_reserved: u64,
    unresolved_reserved: u64,
    committed: u64,
    unknown_operations: u64,
}
fn add(to: &mut u64, n: u64) -> Result<()> {
    *to = to
        .checked_add(n)
        .ok_or_else(|| Error::new(2, "campaign usage arithmetic overflow"))?;
    Ok(())
}
impl Dimension {
    fn observe(
        &mut self,
        active: bool,
        known: bool,
        measured: u64,
        reserved: u64,
        reports: usize,
    ) -> Result<()> {
        add(&mut self.measured, measured)?;
        add(&mut self.reports, reports as u64)?;
        let commitment = if !active && known {
            measured
        } else {
            measured.max(reserved)
        };
        add(&mut self.committed, commitment)?;
        if active {
            add(&mut self.active_reserved, commitment)?;
        } else if known {
            add(&mut self.settled, measured)?;
        } else {
            add(&mut self.unresolved_reserved, commitment)?;
        }
        if !known {
            add(&mut self.unknown_operations, 1)?;
        }
        Ok(())
    }
    fn json(&self) -> Value {
        json!({"measured":(self.reports > 0).then_some(self.measured),"reports":self.reports,"settled":self.settled,"active_reserved":self.active_reserved,
            "unresolved_reserved":self.unresolved_reserved,"committed":self.committed,"unknown_operations":self.unknown_operations})
    }
}
pub(crate) fn summary(
    repo: &Path,
    policy: &Policy,
    rows: &[Value],
    reservations: &BTreeMap<String, Value>,
    settled: &BTreeSet<String>,
) -> Result<Value> {
    let projection = usage::load(repo).map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let mut tokens = Dimension::default();
    let mut cost = Dimension::default();
    let mut physical_calls = 0u64;
    let mut conflicts = 0u64;
    let mut provider_estimate_calls = 0u64;
    for id in reservations.keys() {
        let calls = projection
            .calls
            .iter()
            .filter(|c| c.receipt.identity.operation_id == *id)
            .collect::<Vec<_>>();
        add(&mut physical_calls, calls.len() as u64)?;
        // Only the producer's pre-invocation outbound refusal proves no call.
        // Failures, abandonment and timeouts are never evidence of zero spend.
        let refused = calls.is_empty()
            && rows
                .iter()
                .any(|r| r["type"] == "settled" && r["id"] == *id && r["outcome"] == "refused");
        let complete = refused
            || (!calls.is_empty()
                && calls
                    .iter()
                    .all(|c| c.receipt.phase == Phase::Final && !c.conflict));
        let mut used_tokens = 0;
        let mut used_cost = 0;
        let mut token_liability = 0;
        let mut cost_liability = 0;
        for call in &calls {
            if matches!(call.receipt.cost, Cost::ProviderEstimate(_)) {
                add(&mut provider_estimate_calls, 1)?;
            }
            if call.conflict {
                add(&mut conflicts, 1)?;
            }
            add(&mut used_tokens, call.tokens.unwrap_or(0))?;
            add(&mut used_cost, call.cost_microusd.unwrap_or(0))?;
            let final_ = call.receipt.phase == Phase::Final && !call.conflict;
            add(
                &mut token_liability,
                match (final_, call.tokens) {
                    (true, Some(tokens)) => tokens,
                    (_, tokens) => tokens.unwrap_or(0).max(policy.tokens.unwrap_or(0)),
                },
            )?;
            add(
                &mut cost_liability,
                match (final_, call.cost_microusd) {
                    (true, Some(cost)) => cost,
                    (_, cost) => cost.unwrap_or(0).max(policy.cost_microusd.unwrap_or(0)),
                },
            )?;
        }
        let active = !settled.contains(id);
        if calls.is_empty() || active {
            token_liability = token_liability.max(policy.tokens.unwrap_or(0));
            cost_liability = cost_liability.max(policy.cost_microusd.unwrap_or(0));
        }
        tokens.observe(
            active,
            complete && calls.iter().all(|c| c.tokens.is_some()),
            used_tokens,
            token_liability,
            calls.iter().filter(|c| c.tokens.is_some()).count(),
        )?;
        cost.observe(
            active,
            complete && calls.iter().all(|c| c.cost_microusd.is_some()),
            used_cost,
            cost_liability,
            calls.iter().filter(|c| c.cost_microusd.is_some()).count(),
        )?;
    }
    Ok(
        json!({"basis":"reported usage and explicit reservations; soft admission, not a provider spending cap",
        "physical_calls":physical_calls,"conflicts":conflicts,"provider_estimate_calls":provider_estimate_calls,"tokens":tokens.json(),"cost_microusd":cost.json()}),
    )
}
