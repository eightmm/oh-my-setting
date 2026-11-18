//! Lifetime provider accounting. Artifact retention is deliberately independent.
//! Receipts are absolute observations, never additive deltas. A later revision
//! replaces an earlier observation; cumulative session snapshots are differenced
//! only when their identity, ordering and measured baseline are known.
use super::*;
use crate::value::{encode, obj};
use std::collections::BTreeMap;

pub const RECEIPTS: &str = ".oms/usage/receipts.jsonl";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub call_id: String,
    pub operation_id: String,
    pub provider: String,
    /// Selected route, not an assertion about the model ultimately served.
    pub selected_model: Option<String>,
    pub account: Option<String>,
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
}

#[derive(Clone, Debug, Default)]
pub struct Projection {
    /// One current revision per physical call, ordered by invocation admission.
    pub calls: Vec<AccountedCall>,
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
        || receipt
            .cumulative
            .as_ref()
            .is_some_and(|s| !identifier(&s.session_id))
        || receipt.source_sha256.as_deref().is_some_and(|s| {
            s.len() != 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
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
    obj!("call_id":i.call_id.clone(),"operation_id":i.operation_id.clone(),"provider":i.provider.clone(),
        "selected_model":i.selected_model.clone(),"account":i.account.clone(),"goal_id":i.goal_id.clone(),
        "task_id":i.task_id.clone(),"attempt_id":i.attempt_id.clone(),"run_id":i.run_id.clone())
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
            "schema":2, "kind":"provider-usage",
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
    project(&receipts(&read_index(
        repo,
        Some(Path::new(RECEIPTS)),
        true,
    )?)?)
}
type SessionIdentity = (String, Option<String>, String);
type MeasuredTotals = (Option<u64>, Option<u64>);
fn project(rows: &[Receipt]) -> Result<Projection, StoreError> {
    let rows = current(rows)?;
    let mut sessions: BTreeMap<SessionIdentity, MeasuredTotals> = BTreeMap::new();
    let mut calls = Vec::new();
    for receipt in rows {
        let mut tokens = receipt.usage.tokens();
        let mut cost = receipt.cost.microusd();
        let mut conflict = false;
        if let Some(session) = &receipt.cumulative {
            let key = (
                receipt.identity.provider.clone(),
                receipt.identity.account.clone(),
                session.session_id.clone(),
            );
            let prior = sessions
                .get(&key)
                .copied()
                .unwrap_or(if session.baseline_unknown {
                    (None, None)
                } else {
                    (Some(0), Some(0))
                });
            conflict = matches!((tokens,prior.0),(Some(n),Some(p)) if n < p)
                || matches!((cost,prior.1),(Some(n),Some(p)) if n < p);
            sessions.insert(key, (tokens.or(prior.0), cost.or(prior.1)));
            tokens = tokens.zip(prior.0).and_then(|(n, p)| n.checked_sub(p));
            cost = cost.zip(prior.1).and_then(|(n, p)| n.checked_sub(p));
        }
        calls.push(AccountedCall {
            receipt,
            tokens,
            cost_microusd: cost,
            conflict,
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
                    && r.identity.account == identity.account
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
pub fn record(
    repo: &Path,
    receipt: &Receipt,
    interrupted: &AtomicBool,
) -> Result<bool, StoreError> {
    validate(receipt)?;
    let root = Root::open(repo)?;
    let target = canonical_index(repo, Some(Path::new(RECEIPTS)), false, false)?;
    let _guard = acquire(&target, interrupted)?;
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
