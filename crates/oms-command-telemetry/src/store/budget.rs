//! Shared spend projection. Callers supply canonical operation/intent snapshots
//! under their admission lock; this pure calculation grants no launch authority.
use super::usage::{AccountedCall, Cost, Phase, Projection};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
pub struct Limit {
    pub max: u64,
    pub per_call: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub tokens: Option<Limit>,
    pub cost_microusd: Option<Limit>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Commitment {
    pub tokens: u64,
    pub cost_microusd: u64,
    pub conflicts: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Conflict,
    Overflow,
    Exhausted(&'static str),
}
impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Conflict | Self::Exhausted(_) => 3,
            _ => 2,
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid usage budget: {message}"),
            Self::Conflict => write!(f, "conflicting provider usage receipts"),
            Self::Overflow => write!(f, "usage arithmetic overflow"),
            Self::Exhausted(key) => write!(f, "{key} admission budget exhausted"),
        }
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;

impl Policy {
    pub fn validate(&self) -> Result<()> {
        if self.tokens.is_none() && self.cost_microusd.is_none() {
            return Err(Error::Invalid("at least one total cap is required"));
        }
        for limit in [self.tokens, self.cost_microusd].into_iter().flatten() {
            if limit.per_call == 0 || limit.per_call > limit.max {
                return Err(Error::Invalid(
                    "cap requires a positive per-call reservation within the cap",
                ));
            }
        }
        Ok(())
    }
    pub fn check_after(&self, after: Commitment) -> Result<()> {
        self.validate()?;
        if after.conflicts != 0 {
            return Err(Error::Conflict);
        }
        for (limit, used, key) in [
            (self.tokens, after.tokens, "tokens"),
            (self.cost_microusd, after.cost_microusd, "cost_microusd"),
        ] {
            if limit.is_some_and(|limit| used > limit.max) {
                return Err(Error::Exhausted(key));
            }
        }
        Ok(())
    }
    /// A fresh logical operation has no physical identity yet. Its escrow is
    /// later replaced by the first physical claim, rather than added twice.
    pub fn admit_next_logical(&self, current: Commitment) -> Result<()> {
        self.check_after(current)?;
        let reserve = |used: u64, limit: Option<Limit>, key| -> Result<u64> {
            match limit {
                Some(limit) => used
                    .checked_add(limit.per_call)
                    .ok_or(Error::Exhausted(key)),
                None => Ok(used),
            }
        };
        self.check_after(Commitment {
            tokens: reserve(current.tokens, self.tokens, "tokens")?,
            cost_microusd: reserve(current.cost_microusd, self.cost_microusd, "cost_microusd")?,
            ..current
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
    Active,
    Settled,
    RefusedBeforeInvocation,
}
#[derive(Clone, Copy, Debug)]
pub struct Operation<'a> {
    pub id: &'a str,
    pub state: OperationState,
}
/// An outstanding physical intent, including one published before its receipt.
/// Equal call/operation identities join once; a changed operation is a conflict.
#[derive(Clone, Copy, Debug)]
pub struct PhysicalClaim<'a> {
    pub call_id: &'a str,
    pub operation_id: &'a str,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dimension {
    pub measured: u64,
    pub reports: u64,
    pub settled: u64,
    pub active_reserved: u64,
    pub unresolved_reserved: u64,
    pub committed: u64,
    pub unknown_operations: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub tokens: Dimension,
    pub cost_microusd: Dimension,
    pub physical_calls: u64,
    pub intent_only_calls: u64,
    pub conflicts: u64,
    pub provider_estimate_calls: u64,
}
impl Summary {
    pub fn commitment(&self) -> Commitment {
        Commitment {
            tokens: self.tokens.committed,
            cost_microusd: self.cost_microusd.committed,
            conflicts: self.conflicts,
        }
    }
}
fn add(to: &mut u64, n: u64) -> Result<()> {
    *to = to.checked_add(n).ok_or(Error::Overflow)?;
    Ok(())
}
impl Dimension {
    fn observe(
        &mut self,
        active: bool,
        known: bool,
        measured: u64,
        reserved: u64,
        reports: u64,
    ) -> Result<()> {
        add(&mut self.measured, measured)?;
        add(&mut self.reports, reports)?;
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
}
fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}

pub fn project(
    policy: &Policy,
    usage: &Projection,
    operations: &[Operation<'_>],
    claims: &[PhysicalClaim<'_>],
) -> Result<Summary> {
    calculate(policy, usage, operations, claims.iter().copied())
}
/// Tests the complete after-projection, including a fresh retry's own claim.
/// It never reserves resources, writes an intent or invokes an executor.
pub fn admit_candidate(
    policy: &Policy,
    usage: &Projection,
    operations: &[Operation<'_>],
    claims: &[PhysicalClaim<'_>],
    candidate: PhysicalClaim<'_>,
) -> Result<Summary> {
    if !operations
        .iter()
        .any(|op| op.id == candidate.operation_id && op.state == OperationState::Active)
    {
        return Err(Error::Invalid("candidate operation is not active"));
    }
    let after = calculate(
        policy,
        usage,
        operations,
        claims.iter().copied().chain([candidate]),
    )?;
    policy.check_after(after.commitment())?;
    Ok(after)
}
fn calculate<'a>(
    policy: &Policy,
    usage: &'a Projection,
    operations: &[Operation<'a>],
    claims: impl Iterator<Item = PhysicalClaim<'a>>,
) -> Result<Summary> {
    policy.validate()?;
    let mut by_operation = BTreeMap::new();
    for operation in operations {
        if !identifier(operation.id)
            || by_operation
                .insert(
                    operation.id,
                    (
                        operation.state,
                        BTreeMap::<&str, Option<&AccountedCall>>::new(),
                    ),
                )
                .is_some()
        {
            return Err(Error::Invalid("invalid or duplicate operation identity"));
        }
    }
    let mut identities = BTreeMap::new();
    for call in &usage.calls {
        let identity = &call.receipt.identity;
        if !identifier(&identity.call_id)
            || !identifier(&identity.operation_id)
            || identities
                .insert(identity.call_id.as_str(), identity.operation_id.as_str())
                .is_some()
        {
            return Err(Error::Invalid(
                "invalid or duplicate physical call identity",
            ));
        }
        if let Some((_, calls)) = by_operation.get_mut(identity.operation_id.as_str()) {
            calls.insert(identity.call_id.as_str(), Some(call));
        }
    }
    for claim in claims {
        if !identifier(claim.call_id) || !identifier(claim.operation_id) {
            return Err(Error::Invalid("invalid physical claim identity"));
        }
        if identities
            .get(claim.call_id)
            .is_some_and(|operation| *operation != claim.operation_id)
        {
            return Err(Error::Conflict);
        }
        identities.insert(claim.call_id, claim.operation_id);
        let (_, calls) = by_operation
            .get_mut(claim.operation_id)
            .ok_or(Error::Invalid("claim operation is missing"))?;
        calls.entry(claim.call_id).or_insert(None);
    }
    let mut summary = Summary::default();
    for (_, (state, calls)) in by_operation {
        add(&mut summary.physical_calls, calls.len() as u64)?;
        let refused = calls.is_empty() && state == OperationState::RefusedBeforeInvocation;
        let active = state == OperationState::Active;
        let complete = refused
            || !calls.is_empty()
                && calls.values().all(|call| {
                    call.is_some_and(|call| call.receipt.phase == Phase::Final && !call.conflict)
                });
        for call in calls.values().flatten() {
            if call.conflict {
                add(&mut summary.conflicts, 1)?;
            }
            if matches!(call.receipt.cost, Cost::ProviderEstimate(_)) {
                add(&mut summary.provider_estimate_calls, 1)?;
            }
        }
        add(
            &mut summary.intent_only_calls,
            calls.values().filter(|call| call.is_none()).count() as u64,
        )?;
        for (limit, dimension, tokens) in [
            (policy.tokens, &mut summary.tokens, true),
            (policy.cost_microusd, &mut summary.cost_microusd, false),
        ] {
            let reserve = limit.map_or(0, |limit| limit.per_call);
            let mut measured = 0;
            let mut liability = 0;
            let mut reports = 0;
            for call in calls.values() {
                let amount = call.and_then(|call| {
                    if tokens {
                        call.tokens
                    } else {
                        call.cost_microusd
                    }
                });
                if let Some(amount) = amount {
                    add(&mut measured, amount)?;
                    add(&mut reports, 1)?;
                }
                let final_ =
                    call.is_some_and(|call| call.receipt.phase == Phase::Final && !call.conflict);
                add(
                    &mut liability,
                    match (final_, amount) {
                        (true, Some(amount)) => amount,
                        (_, amount) => amount.unwrap_or(0).max(reserve),
                    },
                )?;
            }
            if calls.is_empty() || active {
                liability = liability.max(reserve);
            }
            dimension.observe(
                active,
                complete && reports == calls.len() as u64,
                measured,
                liability,
                reports,
            )?;
        }
    }
    Ok(summary)
}
