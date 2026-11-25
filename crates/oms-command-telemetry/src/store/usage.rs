//! Lifetime provider accounting. Artifact retention is deliberately independent.
//! Receipts are absolute observations, never additive deltas. A later revision
//! replaces an earlier observation; cumulative session snapshots are differenced
//! only when their identity, ordering and measured baseline are known.
use super::*;
use crate::value::{encode, obj};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

pub const RECEIPTS: &str = ".oms/usage/receipts.jsonl";
/// Optional supplemental counters keyed by exact receipt identity. The strict
/// receipt schema and bytes never depend on this file.
pub const REASONING: &str = ".oms/usage/reasoning.jsonl";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub call_id: String,
    pub operation_id: String,
    pub provider: String,
    /// Selected route, not an assertion about the model ultimately served.
    pub selected_model: Option<String>,
    pub account: Option<String>,
    /// Producer-observed private account-home generation, never credentials,
    /// a billing identity, or evidence of effective provider permissions.
    /// Absent preserves the legacy receipt/physical-intent encoding.
    pub account_scope_sha256: Option<String>,
    pub goal_id: Option<String>,
    pub task_id: Option<String>,
    pub attempt_id: Option<String>,
    pub run_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Pending,
    Partial,
    Final,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cache_in_input: Option<bool>,
    pub total_tokens: Option<u64>,
}
impl Usage {
    pub fn tokens(&self) -> Option<u64> {
        if self.total_tokens.is_some() {
            return self.total_tokens;
        }
        let total = self.input_tokens?.checked_add(self.output_tokens?)?;
        match self.cache_in_input? {
            true => Some(total),
            false => total
                .checked_add(self.cache_read_tokens?)?
                .checked_add(self.cache_write_tokens?),
        }
    }
}

/// These are transport-reported amounts, not an OMS rate-card calculation.
/// ProviderEstimate explicitly distinguishes a client's price-table estimate
/// (for example Claude Code total_cost_usd) from a reported charge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Cost {
    Reported(u64),
    ProviderEstimate(u64),
    Unpriced,
    Subscription,
    #[default]
    Unknown,
}
impl Cost {
    pub fn microusd(&self) -> Option<u64> {
        match self {
            Self::Reported(n) | Self::ProviderEstimate(n) => Some(*n),
            _ => None,
        }
    }
    fn basis(&self) -> Option<&'static str> {
        match self {
            Self::Reported(_) => Some("provider-reported"),
            Self::ProviderEstimate(_) => Some("provider-estimate"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cumulative {
    pub session_id: String,
    /// A resumed session first seen by OMS has no measured invocation baseline.
    /// Its lifetime snapshot is retained, but no per-call delta is invented.
    pub baseline_unknown: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Receipt {
    pub identity: Identity,
    /// Assigned under the receipt lock before invocation, not by completion order.
    pub ordinal: u64,
    pub revision: u64,
    pub phase: Phase,
    pub usage: Usage,
    pub cost: Cost,
    pub cumulative: Option<Cumulative>,
    pub source_sha256: Option<String>,
    /// Native process-owner evidence, independent of usage completeness. Legacy
    /// receipts and missing executor acknowledgements retain unknown custody.
    pub cleanup_complete: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct AccountedCall {
    pub receipt: Receipt,
    pub tokens: Option<u64>,
    pub cost_microusd: Option<u64>,
    pub conflict: bool,
    // Derived from the retained revision order before projecting the latest rows.
    unscoped_home_overlap: bool,
    /// Provider-reported reasoning tokens proven for this exact final receipt.
    /// Absent means unknown, never zero.
    pub reasoning_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Projection {
    /// One current revision per physical call, ordered by invocation admission.
    pub calls: Vec<AccountedCall>,
}

pub(crate) struct StorageStats {
    pub retained_bytes: usize,
    pub receipt_rows: usize,
}

/// Provider-native token counts attributable to one physical call. Components
/// may overlap. The total uses a reported value when available and otherwise
/// requires enough compatible components to calculate it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttributedUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// Reported only for an exact standalone call; reasoning may overlap output.
    pub reasoning_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributedCall<'a> {
    pub call_id: &'a str,
    pub attempt_id: &'a str,
    pub provider: &'a str,
    pub usage: AttributedUsage,
}

#[derive(Default)]
struct AttributionSession<'a> {
    saw_unknown_account: bool,
    accounts: BTreeMap<&'a str, (AccountContinuity, Option<&'a Usage>)>,
    family: SessionFamily,
}

/// Home observations for one exact provider session, shared by both projections.
#[derive(Default)]
struct SessionFamily {
    aliases: BTreeMap<String, String>,
    last_unscoped: u64,
}

impl SessionFamily {
    fn observe(&mut self, identity: &Identity, ordinal: u64) -> bool {
        let Some((account, scope)) = identity
            .account
            .as_deref()
            .zip(identity.account_scope_sha256.as_deref())
        else {
            // An alias without a home proof could still refer to any proven home.
            self.last_unscoped = ordinal;
            return false;
        };
        if self
            .aliases
            .get(scope)
            .is_some_and(|previous| previous == account)
        {
            return false;
        }
        self.aliases
            .insert(scope.to_owned(), account.to_owned())
            .is_some()
    }
}

/// A renamed registration must not reuse the old home's cumulative baseline.
/// Once proof has been observed, missing proof cannot downgrade to alias-only
/// arithmetic, even if another legacy observation follows it.
#[derive(Default)]
struct AccountContinuity {
    scope: Option<String>,
    seen: bool,
    saw_proof: bool,
    last_unscoped: u64,
}
impl AccountContinuity {
    fn observe(&mut self, scope: Option<&str>, last_unscoped: u64) -> bool {
        let continuous = (!self.seen || self.scope.as_deref() == scope)
            && if scope.is_some() {
                self.last_unscoped == last_unscoped
            } else {
                !self.saw_proof
            };
        self.seen = true;
        self.saw_proof |= scope.is_some();
        self.scope = scope.map(str::to_owned);
        self.last_unscoped = last_unscoped;
        continuous
    }

    fn can_anchor(&self) -> bool {
        self.scope.is_some() || !self.saw_proof
    }
}

impl<'a> AttributionSession<'a> {
    fn observe(
        &mut self,
        receipt: &'a Receipt,
        observed: Option<&'a Usage>,
        baseline_unknown: bool,
    ) -> Option<AttributedUsage> {
        let identity = &receipt.identity;
        let alias_changed = self.family.observe(identity, receipt.ordinal);
        let Some(account) = identity.account.as_deref() else {
            let fresh = !baseline_unknown && !self.saw_unknown_account && self.accounts.is_empty();
            // Any named account in this family could own the observation.
            self.saw_unknown_account = true;
            for (_, snapshot) in self.accounts.values_mut() {
                *snapshot = None;
            }
            return observed.filter(|_| fresh).map(AttributedUsage::standalone);
        };
        let (continuity, snapshot) = self.accounts.entry(account).or_default();
        let first = !continuity.seen;
        let continuous = continuity.observe(
            identity.account_scope_sha256.as_deref(),
            self.family.last_unscoped,
        );
        let previous = snapshot.take();
        if !first && !baseline_unknown {
            // The same exact session cannot start fresh twice.
            return None;
        }
        *snapshot = observed.filter(|_| continuity.can_anchor());
        if !continuous || alias_changed {
            return None;
        }
        match (observed, previous) {
            (Some(usage), None) if first && !self.saw_unknown_account && !baseline_unknown => {
                Some(AttributedUsage::standalone(usage))
            }
            (Some(usage), Some(previous)) => {
                match AttributedUsage::between(usage, previous) {
                    Ok(delta) => Some(delta),
                    Err(()) => {
                        // A reset or contradictory delta cannot anchor the following call.
                        *snapshot = None;
                        None
                    }
                }
            }
            _ => None,
        }
    }
}

impl Projection {
    /// Display usage in physical admission order for calls bound to exact
    /// attempts. Unknown counts remain absent. A known fresh session can start
    /// at zero. Resumed attribution requires a named account and the immediately
    /// preceding final observation in that scope for each field.
    /// Comparing input snapshots also requires a shared, known cache basis.
    pub fn attributed_calls(&self) -> Vec<AttributedCall<'_>> {
        let mut sessions = BTreeMap::<_, AttributionSession<'_>>::new();
        let mut attributed = Vec::new();
        for call in &self.calls {
            let receipt = &call.receipt;
            let identity = &receipt.identity;
            let observed = (receipt.phase == Phase::Final && !call.unscoped_home_overlap)
                .then_some(&receipt.usage);
            let usage = match &receipt.cumulative {
                Some(cumulative) => sessions
                    .entry((identity.provider.as_str(), cumulative.session_id.as_str()))
                    .or_default()
                    .observe(receipt, observed, cumulative.baseline_unknown),
                None => observed.map(|observed| AttributedUsage {
                    reasoning_tokens: call.reasoning_tokens,
                    ..AttributedUsage::standalone(observed)
                }),
            };
            // An unbound call can still advance or break a known session's
            // baseline. Only the exact attempt receives the derived usage.
            if let Some(attempt_id) = identity.attempt_id.as_deref() {
                attributed.push(AttributedCall {
                    call_id: &identity.call_id,
                    attempt_id,
                    provider: &identity.provider,
                    usage: usage.unwrap_or_default(),
                });
            }
        }
        attributed
    }
}

impl AttributedUsage {
    fn standalone(usage: &Usage) -> Self {
        Self::with_total(usage, usage.tokens())
    }

    fn with_total(usage: &Usage, total_tokens: Option<u64>) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            total_tokens,
            reasoning_tokens: None,
        }
    }
    fn between(usage: &Usage, previous: &Usage) -> Result<Self, ()> {
        fn subtract(current: Option<u64>, baseline: Option<u64>) -> Result<Option<u64>, ()> {
            current
                .zip(baseline)
                .map(|(current, baseline)| current.checked_sub(baseline).ok_or(()))
                .transpose()
        }
        fn agrees_with_components(usage: &Usage) -> bool {
            Usage {
                total_tokens: None,
                ..usage.clone()
            }
            .tokens()
            .is_some_and(|computed| {
                usage
                    .total_tokens
                    .is_none_or(|reported| reported == computed)
            })
        }
        let cache_in_input = usage
            .cache_in_input
            .filter(|_| usage.cache_in_input == previous.cache_in_input);
        let delta = Usage {
            input_tokens: if cache_in_input.is_some() {
                subtract(usage.input_tokens, previous.input_tokens)?
            } else {
                None
            },
            output_tokens: subtract(usage.output_tokens, previous.output_tokens)?,
            cache_read_tokens: subtract(usage.cache_read_tokens, previous.cache_read_tokens)?,
            cache_write_tokens: subtract(usage.cache_write_tokens, previous.cache_write_tokens)?,
            cache_in_input,
            total_tokens: subtract(usage.total_tokens, previous.total_tokens)?,
        };
        // Even valid absolute observations can imply impossible per-call counts.
        let excessive_cache = usage.cache_in_input == Some(true)
            && delta
                .input_tokens
                .zip(delta.cache_read_tokens)
                .is_some_and(|(input, cache)| cache > input);
        let short_total = delta
            .input_tokens
            .zip(delta.output_tokens)
            .zip(delta.total_tokens)
            .is_some_and(|((input, output), total)| {
                input.checked_add(output).is_none_or(|sum| total < sum)
            });
        if excessive_cache || short_total {
            return Err(());
        }
        let total_tokens = delta.total_tokens.or_else(|| {
            // Both snapshots need a checked total on the same basis. A lone
            // reported total must agree with that basis before mixing sources.
            cache_in_input?;
            if agrees_with_components(usage) && agrees_with_components(previous) {
                delta.tokens()
            } else {
                None
            }
        });
        Ok(Self::with_total(&delta, total_tokens))
    }
}

fn invalid(message: &'static str) -> StoreError {
    StoreError::Invalid(message)
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
fn sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn validate(receipt: &Receipt) -> Result<(), StoreError> {
    let id = &receipt.identity;
    if receipt.ordinal == 0
        || [&id.call_id, &id.operation_id, &id.provider]
            .iter()
            .any(|s| !identifier(s))
        || [
            &id.account,
            &id.goal_id,
            &id.task_id,
            &id.attempt_id,
            &id.run_id,
        ]
        .iter()
        .any(|s| s.as_deref().is_some_and(|s| !identifier(s)))
        || id
            .selected_model
            .as_deref()
            .is_some_and(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        || id
            .account_scope_sha256
            .as_deref()
            .is_some_and(|s| id.account.is_none() || !sha256_hex(s))
        || receipt
            .cumulative
            .as_ref()
            .is_some_and(|s| !identifier(&s.session_id))
        || receipt
            .source_sha256
            .as_deref()
            .is_some_and(|s| !sha256_hex(s))
    {
        return Err(invalid("invalid usage receipt identity"));
    }
    if receipt.phase == Phase::Pending
        && (receipt.usage != Usage::default()
            || receipt.cost != Cost::Unknown
            || receipt.source_sha256.is_some()
            || receipt.cleanup_complete.is_some())
    {
        return Err(invalid("pending usage receipt contains measurements"));
    }
    let u = &receipt.usage;
    if u.cache_in_input == Some(true)
        && u.input_tokens
            .zip(u.cache_read_tokens)
            .is_some_and(|(input, cache)| cache > input)
    {
        return Err(invalid("cached tokens exceed inclusive input tokens"));
    }
    if let (Some(total), Some(input), Some(output)) =
        (u.total_tokens, u.input_tokens, u.output_tokens)
        && input
            .checked_add(output)
            .is_none_or(|minimum| total < minimum)
    {
        return Err(invalid(
            "total tokens are smaller than measured input and output",
        ));
    }
    Ok(())
}
fn number(n: u64) -> V {
    V::Int(n.to_string())
}
fn optional(n: Option<u64>) -> V {
    n.map(number).unwrap_or(V::Null)
}
pub(super) fn identity_value(i: &Identity) -> V {
    let mut value = obj!("call_id":i.call_id.clone(),"operation_id":i.operation_id.clone(),"provider":i.provider.clone(),
        "selected_model":i.selected_model.clone(),"account":i.account.clone(),"goal_id":i.goal_id.clone(),
        "task_id":i.task_id.clone(),"attempt_id":i.attempt_id.clone(),"run_id":i.run_id.clone());
    if let Some(scope) = &i.account_scope_sha256 {
        value["account_scope_sha256"] = scope.clone().into();
    }
    value
}
pub(super) fn validate_pending(
    identity: &Identity,
    cumulative: &Option<Cumulative>,
) -> Result<(), StoreError> {
    validate(&Receipt {
        identity: identity.clone(),
        cumulative: cumulative.clone(),
        ordinal: 1,
        ..Receipt::default()
    })
}
fn encode_receipt(r: &Receipt) -> Result<Vec<u8>, StoreError> {
    let i = &r.identity;
    let u = &r.usage;
    let (status, basis) = match r.cost {
        Cost::Reported(_) => ("reported", "provider-reported"),
        Cost::ProviderEstimate(_) => ("reported", "provider-estimate"),
        Cost::Unpriced => ("unpriced", "unknown"),
        Cost::Subscription => ("subscription", "unknown"),
        Cost::Unknown => ("unknown", "unknown"),
    };
    let (V::Object(mut fields), V::Object(payload)) = (
        identity_value(i),
        obj!(
            "schema":if i.account_scope_sha256.is_some() {3} else {2}, "kind":"provider-usage",
            "ordinal":number(r.ordinal), "revision":number(r.revision),
            "phase":match r.phase { Phase::Pending=>"pending", Phase::Partial=>"partial", Phase::Final=>"final", Phase::Unknown=>"unknown" },
            "provider_usage":obj!("input_tokens":optional(u.input_tokens),"output_tokens":optional(u.output_tokens),
                "cache_read_tokens":optional(u.cache_read_tokens),"cache_write_tokens":optional(u.cache_write_tokens),
                "cache_in_input":u.cache_in_input,"total_tokens":optional(u.total_tokens)),
            "cost":obj!("status":status,"basis":basis,"microusd":optional(r.cost.microusd())),
            "cumulative":r.cumulative.as_ref().map(|s| obj!("session_id":s.session_id.clone(),"baseline_unknown":s.baseline_unknown)),
            "source_sha256":r.source_sha256.clone(), "cleanup_complete":r.cleanup_complete
        ),
    ) else {
        return Err(invalid("cannot encode usage receipt"));
    };
    fields.extend(payload);
    encode(&V::Object(fields)).map_err(|_| invalid("cannot encode usage receipt"))
}
pub(super) fn uint(v: &V) -> Result<u64, StoreError> {
    v.int()
        .and_then(|s| s.parse().ok())
        .ok_or(invalid("usage count must be a nonnegative u64"))
}
fn opt_uint(v: &V) -> Result<Option<u64>, StoreError> {
    if *v == V::Null {
        Ok(None)
    } else {
        uint(v).map(Some)
    }
}
fn string(v: &V) -> Result<String, StoreError> {
    v.text()
        .map(str::to_owned)
        .ok_or(invalid("usage identity must be text"))
}
fn opt_string(v: &V) -> Result<Option<String>, StoreError> {
    if *v == V::Null {
        Ok(None)
    } else {
        string(v).map(Some)
    }
}
pub(super) fn keys(v: &V, expected: &[&str]) -> Result<(), StoreError> {
    if let V::Object(fields) = v
        && fields.len() == expected.len()
        && expected.iter().all(|key| v.get(key).is_some())
    {
        Ok(())
    } else {
        Err(invalid("unknown or missing usage receipt fields"))
    }
}
pub(super) fn identity_from_value(v: &V) -> Result<Identity, StoreError> {
    Ok(Identity {
        call_id: string(&v["call_id"])?,
        operation_id: string(&v["operation_id"])?,
        provider: string(&v["provider"])?,
        selected_model: opt_string(&v["selected_model"])?,
        account: opt_string(&v["account"])?,
        account_scope_sha256: opt_string(&v["account_scope_sha256"])?,
        goal_id: opt_string(&v["goal_id"])?,
        task_id: opt_string(&v["task_id"])?,
        attempt_id: opt_string(&v["attempt_id"])?,
        run_id: opt_string(&v["run_id"])?,
    })
}
fn decode(line: &[u8]) -> Result<Receipt, StoreError> {
    // Check framing and the row bound before the strict JSON parser.
    let v = row(line)?;
    // Accounting refuses duplicate keys instead of the legacy index's last-key policy.
    oms_context::parse_json(line)
        .map_err(|_| invalid("invalid or duplicate usage receipt JSON"))?;
    let mut fields = vec![
        "schema",
        "kind",
        "call_id",
        "operation_id",
        "provider",
        "selected_model",
        "account",
        "goal_id",
        "task_id",
        "attempt_id",
        "run_id",
        "ordinal",
        "revision",
        "phase",
        "provider_usage",
        "cost",
        "cumulative",
        "source_sha256",
    ];
    match v["schema"] {
        ref schema if *schema == V::from(1) => {}
        ref schema if *schema == V::from(2) => fields.push("cleanup_complete"),
        ref schema if *schema == V::from(3) => {
            fields.extend(["cleanup_complete", "account_scope_sha256"]);
            if !v["account_scope_sha256"].text().is_some_and(sha256_hex) {
                return Err(invalid("invalid account-home scope"));
            }
        }
        _ => return Err(invalid("unsupported usage receipt schema")),
    }
    keys(&v, &fields)?;
    if v["kind"].text() != Some("provider-usage") {
        return Err(invalid("unsupported usage receipt schema"));
    }
    let u = &v["provider_usage"];
    keys(
        u,
        &[
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "cache_in_input",
            "total_tokens",
        ],
    )?;
    let c = &v["cost"];
    keys(c, &["status", "basis", "microusd"])?;
    let amount = opt_uint(&c["microusd"])?;
    let cost = match (c["status"].text(), c["basis"].text(), amount) {
        (Some("reported"), Some("provider-reported"), Some(n)) => Cost::Reported(n),
        (Some("reported"), Some("provider-estimate"), Some(n)) => Cost::ProviderEstimate(n),
        (Some("unpriced"), Some("unknown"), None) => Cost::Unpriced,
        (Some("subscription"), Some("unknown"), None) => Cost::Subscription,
        (Some("unknown"), Some("unknown"), None) => Cost::Unknown,
        _ => return Err(invalid("invalid usage receipt cost")),
    };
    let cumulative = if v["cumulative"] == V::Null {
        None
    } else {
        let s = &v["cumulative"];
        keys(s, &["session_id", "baseline_unknown"])?;
        let V::Bool(baseline_unknown) = s["baseline_unknown"] else {
            return Err(invalid("invalid cumulative baseline"));
        };
        Some(Cumulative {
            session_id: string(&s["session_id"])?,
            baseline_unknown,
        })
    };
    let receipt = Receipt {
        identity: identity_from_value(&v)?,
        ordinal: uint(&v["ordinal"])?,
        revision: uint(&v["revision"])?,
        phase: match v["phase"].text() {
            Some("pending") => Phase::Pending,
            Some("partial") => Phase::Partial,
            Some("final") => Phase::Final,
            Some("unknown") => Phase::Unknown,
            _ => return Err(invalid("invalid usage phase")),
        },
        usage: Usage {
            input_tokens: opt_uint(&u["input_tokens"])?,
            output_tokens: opt_uint(&u["output_tokens"])?,
            cache_read_tokens: opt_uint(&u["cache_read_tokens"])?,
            cache_write_tokens: opt_uint(&u["cache_write_tokens"])?,
            total_tokens: opt_uint(&u["total_tokens"])?,
            cache_in_input: match u["cache_in_input"] {
                V::Null => None,
                V::Bool(b) => Some(b),
                _ => return Err(invalid("invalid cache token basis")),
            },
        },
        cost,
        cumulative,
        source_sha256: opt_string(&v["source_sha256"])?,
        cleanup_complete: match v["cleanup_complete"] {
            V::Null => None,
            V::Bool(value) => Some(value),
            _ => return Err(invalid("invalid native cleanup evidence")),
        },
    };
    validate(&receipt)?;
    Ok(receipt)
}
fn receipts(bytes: &[u8]) -> Result<Vec<Receipt>, StoreError> {
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(invalid("partial final JSONL row"));
    }
    // Each decoder validates its own JSONL row; the generic index pass would
    // parse every historical row again without adding a validation guarantee.
    bytes.split_inclusive(|b| *b == b'\n').map(decode).collect()
}
fn current(rows: &[Receipt]) -> Result<Vec<Receipt>, StoreError> {
    let mut revisions = BTreeMap::new();
    let mut latest: BTreeMap<&str, &Receipt> = BTreeMap::new();
    let mut ordinals = BTreeMap::new();
    let mut cumulative_cost_basis = BTreeMap::new();
    for row in rows {
        validate(row)?;
        let id = row.identity.call_id.as_str();
        if ordinals
            .insert(row.ordinal, id)
            .is_some_and(|old| old != id)
        {
            return Err(invalid("conflicting usage call ordinal"));
        }
        if revisions
            .insert((id, row.revision), row)
            .is_some_and(|old| old != row)
        {
            return Err(invalid("conflicting usage receipt revision"));
        }
        if let Some(old) = latest.get(id)
            && (old.identity != row.identity || old.ordinal != row.ordinal)
        {
            return Err(invalid("conflicting usage call identity"));
        }
        if let (Some(session), Some(basis)) = (&row.cumulative, row.cost.basis()) {
            let key = (
                &row.identity.provider,
                &row.identity.account,
                &row.identity.account_scope_sha256,
                &session.session_id,
            );
            if cumulative_cost_basis
                .insert(key, basis)
                .is_some_and(|old| old != basis)
            {
                return Err(invalid("conflicting cumulative cost basis"));
            }
        }
        if latest.get(id).is_none_or(|old| old.revision < row.revision) {
            latest.insert(id, row);
        }
    }
    // Validate in revision order, including stale observations appended later.
    // A provider may reveal its session after registration, but measured usage
    // must never switch between per-call and cumulative scope or session identity.
    let mut previous: Option<&Receipt> = None;
    for row in revisions.values() {
        if let Some(old) = previous.filter(|old| old.identity.call_id == row.identity.call_id)
            && old.cleanup_complete == Some(true)
            && row.cleanup_complete != Some(true)
        {
            return Err(invalid(
                "confirmed cleanup cannot be discarded by a usage revision",
            ));
        }
        if let Some(old) = previous.filter(|old| old.identity.call_id == row.identity.call_id)
            && old.cumulative != row.cumulative
            && !(old.phase == Phase::Pending && old.cumulative.is_none())
        {
            return Err(invalid("conflicting cumulative usage identity"));
        }
        previous = Some(row);
    }
    let mut rows = latest.values().map(|r| (*r).clone()).collect::<Vec<_>>();
    rows.sort_by_key(|r| r.ordinal);
    Ok(rows)
}

/// Read a strict, complete snapshot. Missing history means no tracked calls,
/// never an assertion that the provider has billed nothing outside OMS.
pub fn load(repo: &Path) -> Result<Projection, StoreError> {
    load_with_storage(repo).map(|(projection, _)| projection)
}

/// Storage counts and usage describe the same validated snapshot, including
/// superseded revisions. They do not require another read of the ledger.
pub(crate) fn load_with_storage(repo: &Path) -> Result<(Projection, StorageStats), StoreError> {
    let (mut projection, storage) = {
        let bytes = read_index(repo, Some(Path::new(RECEIPTS)), true)?;
        let rows = receipts(&bytes)?;
        let storage = StorageStats {
            retained_bytes: bytes.len(),
            receipt_rows: rows.len(),
        };
        (project(&rows)?, storage)
    };
    // Optional evidence: a missing, oversized, torn or unreadable supplement
    // leaves core usage intact and reasoning unknown.
    if let Ok(found) = read_reasoning(repo).and_then(|bytes| reasoning_index(&bytes)) {
        for call in &mut projection.calls {
            call.reasoning_tokens = proven_reasoning(&call.receipt, &found);
        }
    }
    Ok((projection, storage))
}

/// The supplement is small by contract; reads and rewrites enforce it before
/// any large allocation.
pub const MAX_REASONING_BYTES: usize = 1024 * 1024;
/// Optional evidence never waits longer than this for its lock.
const REASONING_LOCK_WAIT: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Reasoning {
    call_id: String,
    attempt_id: String,
    revision: u64,
    source_sha256: String,
    tokens: u64,
}
/// One entry per (call id, revision); `None` marks a conflicting duplicate.
type ReasoningIndex = BTreeMap<(String, u64), Option<Reasoning>>;
/// A final standalone receipt with an attempt and source digest is the only
/// identity a supplement may describe.
fn eligible(receipt: &Receipt) -> Option<(&str, &str)> {
    (receipt.phase == Phase::Final && receipt.cumulative.is_none())
        .then(|| {
            Some((
                receipt.identity.attempt_id.as_deref()?,
                receipt.source_sha256.as_deref()?,
            ))
        })
        .flatten()
}
fn proven_reasoning(receipt: &Receipt, found: &ReasoningIndex) -> Option<u64> {
    let (attempt, digest) = eligible(receipt)?;
    let r = found
        .get(&(receipt.identity.call_id.clone(), receipt.revision))?
        .as_ref()?;
    (r.attempt_id == attempt && r.source_sha256 == digest).then_some(r.tokens)
}
fn encode_reasoning(r: &Reasoning) -> Result<Vec<u8>, StoreError> {
    encode(
        &obj!("schema":1, "kind":"provider-reasoning", "call_id":r.call_id.clone(),
        "attempt_id":r.attempt_id.clone(), "revision":number(r.revision),
        "source_sha256":r.source_sha256.clone(), "reasoning_tokens":number(r.tokens)),
    )
    .map_err(|_| invalid("cannot encode reasoning evidence"))
}
fn decode_reasoning(line: &[u8]) -> Result<Reasoning, StoreError> {
    let v = row(line)?;
    oms_context::parse_json(line)
        .map_err(|_| invalid("invalid or duplicate reasoning evidence JSON"))?;
    keys(
        &v,
        &[
            "schema",
            "kind",
            "call_id",
            "attempt_id",
            "revision",
            "source_sha256",
            "reasoning_tokens",
        ],
    )?;
    if v["schema"] != V::from(1) || v["kind"].text() != Some("provider-reasoning") {
        return Err(invalid("unsupported reasoning evidence schema"));
    }
    let found = Reasoning {
        call_id: string(&v["call_id"])?,
        attempt_id: string(&v["attempt_id"])?,
        revision: uint(&v["revision"])?,
        source_sha256: string(&v["source_sha256"])?,
        tokens: uint(&v["reasoning_tokens"])?,
    };
    if !identifier(&found.call_id)
        || !identifier(&found.attempt_id)
        || found.revision == 0
        || !sha256_hex(&found.source_sha256)
    {
        return Err(invalid("invalid reasoning evidence identity"));
    }
    Ok(found)
}
fn reasoning_rows(bytes: &[u8]) -> Result<Vec<Reasoning>, StoreError> {
    if bytes.len() > MAX_REASONING_BYTES {
        return Err(invalid("reasoning evidence exceeds its size cap"));
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(invalid("partial final JSONL row"));
    }
    bytes
        .split_inclusive(|b| *b == b'\n')
        .map(decode_reasoning)
        .collect()
}
/// Index before any identity matching: identical repeats are idempotent, any
/// other row for the same call and revision proves nothing.
fn reasoning_index(bytes: &[u8]) -> Result<ReasoningIndex, StoreError> {
    let mut index = ReasoningIndex::new();
    for r in reasoning_rows(bytes)? {
        match index.entry((r.call_id.clone(), r.revision)) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(Some(r));
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if slot.get().as_ref() != Some(&r) {
                    *slot.get_mut() = None;
                }
            }
        }
    }
    Ok(index)
}
fn read_reasoning(repo: &Path) -> Result<Vec<u8>, StoreError> {
    let root = Root::open(repo)?;
    let target = canonical_index(repo, Some(Path::new(REASONING)), false, false)?;
    let body = root.read_no_follow(
        &target,
        ByteReadOptions {
            missing_ok: true,
            max_bytes: MAX_REASONING_BYTES,
        },
    )?;
    super::paths::validate(&root, &target, false)?;
    lines(&body)?;
    Ok(body)
}

/// Idempotently append provider-reported reasoning tokens for one exact, already
/// recorded final receipt. The receipt ledger is neither read under this lock
/// nor modified. The lock wait is short and cancellation-aware: contention,
/// an oversized or conflicting supplement leaves reasoning unknown, and a
/// repeat of the same settlement retries.
pub fn record_reasoning(
    repo: &Path,
    receipt: &Receipt,
    tokens: u64,
    interrupted: &AtomicBool,
) -> Result<bool, StoreError> {
    validate(receipt)?;
    let (attempt, digest) = eligible(receipt).ok_or(invalid(
        "reasoning evidence needs a final standalone receipt with attempt and source digest",
    ))?;
    let recorded = current(&receipts(&read_index(
        repo,
        Some(Path::new(RECEIPTS)),
        false,
    )?)?)?;
    if !recorded.iter().any(|r| r == receipt) {
        return Err(invalid("reasoning evidence has no matching usage receipt"));
    }
    let found = Reasoning {
        call_id: receipt.identity.call_id.clone(),
        attempt_id: attempt.to_owned(),
        revision: receipt.revision,
        source_sha256: digest.to_owned(),
        tokens,
    };
    let encoded = encode_reasoning(&found)?;
    let root = Root::open(repo)?;
    let target = canonical_index(repo, Some(Path::new(REASONING)), false, false)?;
    let _guard = lock_file_before(&target, interrupted, Instant::now() + REASONING_LOCK_WAIT)?;
    super::paths::validate(&root, &target, true)?;
    let mut added = false;
    root.atomic_mutate(
        &target,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: MAX_REASONING_BYTES,
            },
            create_parent: true,
            max_output: MAX_REASONING_BYTES,
            ..MutationOptions::default()
        },
        |old| {
            cancelled(interrupted)?;
            super::paths::validate(&root, &target, false)?;
            let rows = reasoning_rows(old)?;
            let mut same = rows
                .iter()
                .filter(|r| r.call_id == found.call_id && r.revision == found.revision)
                .peekable();
            if same.peek().is_some() {
                // Any differing row, even beside a matching one, is a conflict.
                return if same.all(|r| *r == found) {
                    Ok(old.to_vec())
                } else {
                    Err(invalid("conflicting reasoning evidence"))
                };
            }
            let mut new = old.to_vec();
            new.extend_from_slice(&encoded);
            lines(&new)?;
            cancelled(interrupted)?;
            added = true;
            Ok(new)
        },
    )?;
    Ok(added)
}
type SessionIdentity = (String, Option<String>, String);
type MeasuredTotals = (Option<u64>, Option<u64>);
struct LedgerSession {
    continuity: AccountContinuity,
    totals: MeasuredTotals,
    last_usage: Option<Usage>,
}
struct CallInterval<'a> {
    admitted_at: usize,
    observed_at: usize,
    latest: &'a Receipt,
    terminal_at: Option<usize>,
}

fn unscoped_home_overlaps(rows: &[Receipt]) -> BTreeSet<u64> {
    let mut intervals = BTreeMap::new();
    // Admissions and revisions share the ledger's append lock. Ignore stale
    // and identical repeats, which add no new lifecycle or measurement evidence.
    for (at, row) in rows.iter().enumerate() {
        let interval = intervals
            .entry(row.identity.call_id.as_str())
            .or_insert(CallInterval {
                admitted_at: at,
                observed_at: at,
                latest: row,
                terminal_at: None,
            });
        if at != interval.admitted_at && row.revision <= interval.latest.revision {
            continue;
        }
        interval.latest = row;
        interval.observed_at = at;
        match row.phase {
            Phase::Pending | Phase::Partial => interval.terminal_at = None,
            Phase::Final | Phase::Unknown => {
                interval.terminal_at.get_or_insert(at);
            }
        }
    }

    let mut terminal_by_session = BTreeMap::new();
    let mut overlaps = BTreeSet::new();
    for (at, row) in rows.iter().enumerate() {
        let interval = &intervals[row.identity.call_id.as_str()];
        // A terminal revision can identify the session of an admission that
        // initially had no session evidence.
        let Some(session) = &interval.latest.cumulative else {
            continue;
        };
        let key = (row.identity.provider.as_str(), session.session_id.as_str());
        if row.identity.account_scope_sha256.is_none() {
            if at == interval.admitted_at {
                let terminal = terminal_by_session.entry(key).or_insert(0);
                *terminal = (*terminal).max(interval.terminal_at.unwrap_or(usize::MAX));
            }
        } else if at == interval.observed_at
            && row.phase == Phase::Final
            && terminal_by_session
                .get(&key)
                .is_some_and(|terminal| *terminal > interval.admitted_at)
        {
            overlaps.insert(row.ordinal);
        }
    }
    overlaps
}

fn project(rows: &[Receipt]) -> Result<Projection, StoreError> {
    let latest = current(rows)?;
    let overlaps = unscoped_home_overlaps(rows);
    let mut sessions = BTreeMap::<SessionIdentity, LedgerSession>::new();
    let mut families = BTreeMap::<_, SessionFamily>::new();
    let mut calls = Vec::new();
    for receipt in latest {
        let mut tokens = receipt.usage.tokens();
        let mut cost = receipt.cost.microusd();
        let mut conflict = false;
        let unscoped_home_overlap = overlaps.contains(&receipt.ordinal);
        if let Some(session) = &receipt.cumulative {
            let family = families
                .entry((
                    receipt.identity.provider.clone(),
                    session.session_id.clone(),
                ))
                .or_default();
            let identity = &receipt.identity;
            let scope = identity.account_scope_sha256.as_deref();
            let alias_changed = family.observe(identity, receipt.ordinal);
            let key = (
                receipt.identity.provider.clone(),
                receipt.identity.account.clone(),
                session.session_id.clone(),
            );
            let LedgerSession {
                continuity,
                totals,
                last_usage,
            } = sessions.entry(key).or_insert_with(|| LedgerSession {
                continuity: AccountContinuity::default(),
                totals: if session.baseline_unknown {
                    (None, None)
                } else {
                    (Some(0), Some(0))
                },
                last_usage: None,
            });
            let first = !continuity.seen;
            let continuous = continuity.observe(scope, family.last_unscoped);
            let observed = receipt.phase == Phase::Final
                && !unscoped_home_overlap
                && continuity.can_anchor()
                && !(continuity.saw_proof && !first && !session.baseline_unknown);
            let comparable = continuous && observed && !alias_changed;
            let component_conflict = comparable
                && last_usage
                    .as_ref()
                    .is_some_and(|prior| AttributedUsage::between(&receipt.usage, prior).is_err());
            let prior = if comparable && !component_conflict {
                *totals
            } else {
                (None, None)
            };
            conflict = component_conflict
                || matches!((tokens,prior.0),(Some(n),Some(p)) if n < p)
                || matches!((cost,prior.1),(Some(n),Some(p)) if n < p);
            let can_anchor = observed && !(continuity.saw_proof && conflict);
            *last_usage = (can_anchor && continuity.saw_proof).then(|| receipt.usage.clone());
            *totals = if !can_anchor {
                (None, None)
            } else if continuity.saw_proof {
                (tokens, cost)
            } else {
                (tokens.or(prior.0), cost.or(prior.1))
            };
            tokens = tokens.zip(prior.0).and_then(|(n, p)| n.checked_sub(p));
            cost = cost.zip(prior.1).and_then(|(n, p)| n.checked_sub(p));
        }
        calls.push(AccountedCall {
            receipt,
            tokens,
            cost_microusd: cost,
            conflict,
            unscoped_home_overlap,
            reasoning_tokens: None,
        });
    }
    Ok(Projection { calls })
}

/// Register a physical invocation before spawning it. An interrupted or killed
/// producer therefore leaves a durable unknown liability, even without output.
pub fn begin(
    repo: &Path,
    identity: Identity,
    cumulative: Option<Cumulative>,
    interrupted: &AtomicBool,
) -> Result<Receipt, StoreError> {
    begin_inner(repo, identity, cumulative, interrupted, None, false)
}
/// Shared admission bounds nested receipt/ignore lock waits by one monotonic
/// deadline. Persistence may finish later; the caller must fence external launch.
pub fn begin_before(
    repo: &Path,
    identity: Identity,
    cumulative: Option<Cumulative>,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<Receipt, StoreError> {
    begin_inner(
        repo,
        identity,
        cumulative,
        interrupted,
        Some(deadline),
        false,
    )
}
pub(super) fn writer_before(
    repo: &Path,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<oms_lock::Guard, StoreError> {
    let target = canonical_index(repo, Some(Path::new(RECEIPTS)), false, false)?;
    acquire_before(&target, interrupted, Some(deadline))
}
pub(super) fn begin_registered_before(
    tx: &super::dispatch::Transaction<'_>,
    identity: Identity,
    cumulative: Option<Cumulative>,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<Receipt, StoreError> {
    begin_inner(
        tx.repo(),
        identity,
        cumulative,
        interrupted,
        Some(deadline),
        true,
    )
}
fn begin_inner(
    repo: &Path,
    identity: Identity,
    cumulative: Option<Cumulative>,
    interrupted: &AtomicBool,
    deadline: Option<Instant>,
    registered: bool,
) -> Result<Receipt, StoreError> {
    before(interrupted, deadline)?;
    let root = Root::open(repo)?;
    let target = canonical_index(repo, Some(Path::new(RECEIPTS)), false, false)?;
    let _guard = acquire_before(&target, interrupted, deadline)?;
    // Activation holds dispatch then this same receipt writer across its CAS.
    // A direct writer only observes dispatch, preserving the legacy lock contract.
    if !registered
        && let Some(raw) = super::dispatch::read_snapshot(root.physical_root(), interrupted)?
    {
        let registry =
            crate::value::parse(&raw, true).map_err(|_| invalid("invalid dispatch registry"))?;
        if registry["schema"] == V::from(2) {
            return Err(invalid(
                "resource domain requires physical intent admission",
            ));
        }
    }
    before(interrupted, deadline)?;
    ensure_ignore_before(root.physical_root(), interrupted, deadline)?;
    let mut result = None;
    mutate(&root, &target, true, None, interrupted, |old| {
        before(interrupted, deadline)?;
        let rows = current(&receipts(old)?)?;
        if rows.iter().any(|r| r.identity.call_id == identity.call_id) {
            return Err(invalid("usage call already exists"));
        }
        // Simultaneous writers into one resumed provider session have no reliable
        // snapshot order. Refuse a second start until its prior call is terminal.
        if let Some(session) = &cumulative
            && rows.iter().any(|r| {
                r.identity.provider == identity.provider
                    && (r.identity.account == identity.account
                        || identity.account_scope_sha256.is_some()
                            && r.identity.account_scope_sha256 == identity.account_scope_sha256)
                    && r.cumulative
                        .as_ref()
                        .is_some_and(|s| s.session_id == session.session_id)
                    && matches!(r.phase, Phase::Pending | Phase::Partial)
            })
        {
            return Err(invalid("resumed session has unresolved provider usage"));
        }
        let ordinal = rows
            .last()
            .map_or(Some(1), |r| r.ordinal.checked_add(1))
            .ok_or(invalid("usage ordinal overflow"))?;
        let receipt = Receipt {
            identity,
            ordinal,
            cumulative,
            ..Receipt::default()
        };
        validate(&receipt)?;
        let mut new = old.to_vec();
        new.extend(encode_receipt(&receipt)?);
        before(interrupted, deadline)?;
        result = Some(receipt);
        Ok(new)
    })?;
    result.ok_or(StoreError::Io)
}

/// Idempotent append of an absolute snapshot or an explicit revision correction.
/// Out-of-order revisions remain auditable but cannot replace newer evidence.
/// Physical admission reads this ledger while holding only dispatch, so every
/// revision takes dispatch then receipts, the registration order. The caller
/// must not already hold either lock. Both waits share the lock timeout.
pub fn record(
    repo: &Path,
    receipt: &Receipt,
    interrupted: &AtomicBool,
) -> Result<bool, super::dispatch::Error> {
    let timeout = oms_lock::Settings::from_env()
        .map_err(|error| super::dispatch::Error::new(StoreError::Lock(error), false))?
        .timeout;
    record_before(repo, receipt, interrupted, Instant::now() + timeout)
}
/// Like [`record`], with both lock waits bounded by one deadline.
pub fn record_before(
    repo: &Path,
    receipt: &Receipt,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<bool, super::dispatch::Error> {
    let initial = |error| super::dispatch::Error::new(error, false);
    validate(receipt).map_err(initial)?;
    before(interrupted, Some(deadline)).map_err(initial)?;
    let dispatch = super::dispatch::Transaction::acquire(
        repo,
        deadline.saturating_duration_since(Instant::now()),
        interrupted,
    )
    .map_err(|error| error.in_file(super::dispatch::LOCK))?;
    append_revision(&dispatch, receipt, interrupted, deadline)
        .map_err(|error| dispatch.error(error).in_file(RECEIPTS))
}
fn append_revision(
    dispatch: &super::dispatch::Transaction<'_>,
    receipt: &Receipt,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<bool, StoreError> {
    let target = canonical_index(dispatch.repo(), Some(Path::new(RECEIPTS)), false, false)?;
    let guard = acquire_before(&target, interrupted, Some(deadline))?;
    let held = || {
        dispatch.check()?;
        guard.check().map_err(StoreError::Lock)
    };
    let root = Root::open(dispatch.repo())?;
    held()?;
    let encoded = encode_receipt(receipt)?;
    let mut added = false;
    mutate(&root, &target, false, None, interrupted, |old| {
        let mut rows = receipts(old)?;
        if !rows
            .iter()
            .any(|r| r.identity.call_id == receipt.identity.call_id)
        {
            return Err(invalid("usage receipt has no registered call"));
        }
        if rows.iter().any(|r| r == receipt) {
            current(&rows)?;
            return Ok(old.to_vec());
        }
        rows.push(receipt.clone());
        current(&rows)?;
        let mut new = old.to_vec();
        new.extend_from_slice(&encoded);
        held()?;
        added = true;
        Ok(new)
    })?;
    Ok(added)
}

/// Decimal USD to integer microUSD, rounding a sub-microUSD remainder upward.
/// No floating-point conversion and no token-based price inference occurs.
pub fn usd_to_microusd(raw: &str) -> Option<u64> {
    let (mantissa, exponent) = raw
        .split_once(['e', 'E'])
        .map_or(Some((raw, 0i32)), |(a, b)| Some((a, b.parse().ok()?)))?;
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
        || raw.len() > 128
    {
        return None;
    }
    let digits = format!("{whole}{fraction}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    let scale = 6i64 + i64::from(exponent) - fraction.len() as i64;
    if scale >= 0 {
        if digits.len() as i64 + scale > 20 {
            return None;
        }
        digits
            .parse::<u64>()
            .ok()?
            .checked_mul(10u64.checked_pow(scale as u32)?)
    } else {
        let places = (-scale) as usize;
        if places >= digits.len() {
            return Some(1);
        }
        let (integer, remainder) = digits.split_at(digits.len() - places);
        integer
            .parse::<u64>()
            .ok()?
            .checked_add(u64::from(remainder.bytes().any(|b| b != b'0')))
    }
}
