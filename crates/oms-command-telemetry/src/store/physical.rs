//! Physical call intent and usage registration share the existing dispatch
//! writer. Observed legacy bindings grant neither team authority nor a hard
//! bound on native descendants. Budget/owner policy must be checked by admission.
use super::{StoreError, before, campaign, dispatch, usage};
use crate::value::{V, encode, obj, parse};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Reserved,
    Ready,
}
#[derive(Clone, Debug)]
pub struct Intent {
    pub identity: usage::Identity,
    pub cumulative: Option<usage::Cumulative>,
    pub write: bool,
    pub phase: Phase,
    pub usage_ordinal: Option<u64>,
    pub campaign_manifest_sha256: Option<String>,
}
pub struct Request {
    pub identity: usage::Identity,
    pub cumulative: Option<usage::Cumulative>,
    pub write: bool,
    pub campaign_manifest_sha256: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Registration {
    New(usage::Receipt),
    /// A replay observes an existing obligation. It never authorizes resend,
    /// including a reserved intent whose pending receipt was not acknowledged.
    Existing {
        phase: Phase,
        receipt: Option<usage::Receipt>,
    },
}
type Result<T> = std::result::Result<T, dispatch::Error>;
fn invalid(message: &'static str) -> StoreError {
    StoreError::Invalid(message)
}
fn initial(error: StoreError) -> dispatch::Error {
    dispatch::Error {
        source: error,
        publication_uncertain: false,
    }
}
fn encoded(value: &V) -> std::result::Result<Vec<u8>, StoreError> {
    encode(value).map_err(|_| invalid("cannot encode physical invocation intent"))
}
fn cumulative(value: &V) -> std::result::Result<Option<usage::Cumulative>, StoreError> {
    if *value == V::Null {
        return Ok(None);
    }
    usage::keys(value, &["session_id", "baseline_unknown"])?;
    let (Some(session_id), V::Bool(baseline_unknown)) =
        (value["session_id"].text(), &value["baseline_unknown"])
    else {
        return Err(invalid("invalid physical session binding"));
    };
    Ok(Some(usage::Cumulative {
        session_id: session_id.into(),
        baseline_unknown: *baseline_unknown,
    }))
}
fn intents(value: &V) -> std::result::Result<Vec<Intent>, StoreError> {
    usage::keys(value, &["schema", "calls"])?;
    let V::Object(calls) = &value["calls"] else {
        return Err(invalid("invalid physical invocation registry"));
    };
    if value["schema"] != V::from(1) {
        return Err(invalid("unsupported physical invocation schema"));
    }
    let mut result = Vec::new();
    for (id, record) in calls {
        usage::keys(record, &["schema", "request", "state", "usage_ordinal"])?;
        let request = &record["request"];
        let campaign_manifest_sha256 = match record["schema"].int() {
            Some("1") => {
                usage::keys(
                    request,
                    &["identity", "cumulative", "access", "scope", "binding"],
                )?;
                if request["binding"].text() != Some("legacy-observed") {
                    return Err(invalid("invalid legacy physical binding"));
                }
                None
            }
            Some("2") => {
                usage::keys(
                    request,
                    &[
                        "identity",
                        "cumulative",
                        "access",
                        "scope",
                        "binding",
                        "campaign_manifest_sha256",
                    ],
                )?;
                let digest = request["campaign_manifest_sha256"]
                    .text()
                    .filter(|value| campaign::digest(value))
                    .ok_or(invalid("invalid campaign physical binding"))?;
                if request["binding"].text() != Some("campaign-observed") {
                    return Err(invalid("invalid campaign physical binding"));
                }
                Some(digest.to_owned())
            }
            _ => return Err(invalid("unsupported physical call schema")),
        };
        usage::keys(
            &request["identity"],
            &[
                "call_id",
                "operation_id",
                "provider",
                "selected_model",
                "account",
                "goal_id",
                "task_id",
                "attempt_id",
                "run_id",
            ],
        )?;
        let identity = usage::identity_from_value(&request["identity"])?;
        let cumulative = cumulative(&request["cumulative"])?;
        usage::validate_pending(&identity, &cumulative)?;
        if identity.call_id.as_bytes() != id
            || !matches!(request["access"].text(), Some("read" | "write"))
            || request["scope"].text() != Some("repository-local-observed")
            || campaign_manifest_sha256
                .as_ref()
                .is_some_and(|digest| identity.goal_id.as_deref() != Some(&digest[..16]))
        {
            return Err(invalid("invalid physical invocation binding"));
        }
        let (phase, usage_ordinal) = match record["state"].text() {
            Some("reserved") if record["usage_ordinal"] == V::Null => (Phase::Reserved, None),
            Some("ready") if usage::uint(&record["usage_ordinal"])? > 0 => {
                (Phase::Ready, Some(usage::uint(&record["usage_ordinal"])?))
            }
            _ => return Err(invalid("invalid physical invocation phase")),
        };
        result.push(Intent {
            identity,
            cumulative,
            write: request["access"].text() == Some("write"),
            phase,
            usage_ordinal,
            campaign_manifest_sha256,
        });
    }
    Ok(result)
}

/// An atomic-file observation only; its identities are legacy observations,
/// not owner-generation proof or a resource permit. Missing state stays absent.
pub fn load(repo: &Path, interrupted: &AtomicBool) -> std::result::Result<Vec<Intent>, StoreError> {
    let Some(raw) = dispatch::read_snapshot(repo, interrupted)? else {
        return Ok(vec![]);
    };
    let registry = parse(&raw, true).map_err(|_| invalid("invalid dispatch snapshot"))?;
    match registry.get("physical") {
        Some(value) => intents(value),
        None => Ok(vec![]),
    }
}

/// Registers one fresh legacy physical invocation before external execution.
/// All locks are released on return. The caller must treat Existing as read-only
/// reconciliation and recheck its launch fence after a New acknowledgement.
pub fn begin(
    repo: &Path,
    identity: usage::Identity,
    cumulative: Option<usage::Cumulative>,
    write: bool,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<Registration> {
    begin_checked(
        repo,
        Request {
            identity,
            cumulative,
            write,
            campaign_manifest_sha256: None,
        },
        interrupted,
        deadline,
        |_, _| Ok(()),
    )
}

/// The caller holds its canonical policy/operation lock. Admission runs under
/// the shared dispatch writer before any new intent; it grants no resend.
pub fn begin_checked(
    repo: &Path,
    request: Request,
    interrupted: &AtomicBool,
    deadline: Instant,
    admit: impl FnOnce(&[Intent], &usage::Projection) -> std::result::Result<(), StoreError>,
) -> Result<Registration> {
    let Request {
        identity,
        cumulative,
        write,
        campaign_manifest_sha256,
    } = request;
    before(interrupted, Some(deadline)).map_err(initial)?;
    usage::validate_pending(&identity, &cumulative).map_err(initial)?;
    if campaign_manifest_sha256.as_ref().is_some_and(|digest| {
        !campaign::digest(digest) || identity.goal_id.as_deref() != Some(&digest[..16])
    }) {
        return Err(initial(invalid("invalid campaign physical binding")));
    }
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(10));
    let tx = dispatch::Transaction::acquire(repo, timeout, interrupted)?;
    let snapshot = tx.snapshot()?;
    // Transaction validated strict JSON before this private value conversion.
    let mut registry = match snapshot.as_deref() {
        Some(raw) => {
            parse(raw, true).map_err(|_| tx.error(invalid("invalid dispatch snapshot")))?
        }
        None => obj!("schema":1,"calls":obj!()),
    };
    if registry.get("physical").is_none() {
        registry["physical"] = obj!("schema":1,"calls":obj!());
    }
    let mut request = obj!("identity":usage::identity_value(&identity),
        "cumulative":cumulative.as_ref().map(|value| obj!("session_id":value.session_id.clone(),"baseline_unknown":value.baseline_unknown)),
        "access":if write {"write"} else {"read"},"scope":"repository-local-observed","binding":if campaign_manifest_sha256.is_some() {"campaign-observed"} else {"legacy-observed"});
    if let Some(digest) = &campaign_manifest_sha256 {
        request["campaign_manifest_sha256"] = digest.clone().into();
    }
    let intents = intents(&registry["physical"]).map_err(|source| tx.error(source))?;
    let projection = usage::load(tx.repo()).map_err(|source| tx.error(source))?;
    let receipt = projection
        .calls
        .iter()
        .find(|call| call.receipt.identity.call_id == identity.call_id)
        .map(|call| call.receipt.clone());
    let existing = registry["physical"]["calls"].get(&identity.call_id);
    if let Some(record) = existing {
        if record["request"] != request {
            return Err(tx.error(invalid(
                "physical request identity conflicts with its existing intent",
            )));
        }
        if receipt
            .as_ref()
            .is_some_and(|receipt| receipt.identity != identity)
            || record["state"].text() == Some("ready")
                && receipt.as_ref().is_none_or(|receipt| {
                    record["usage_ordinal"] != V::Int(receipt.ordinal.to_string())
                })
        {
            return Err(tx.error(invalid(
                "physical intent and usage acknowledgement disagree",
            )));
        }
        return Ok(Registration::Existing {
            phase: if record["state"].text() == Some("ready") {
                Phase::Ready
            } else {
                Phase::Reserved
            },
            receipt,
        });
    }
    if receipt.is_some() {
        return Err(tx.error(invalid(
            "physical call ID already has a usage receipt; reconciliation required",
        )));
    }
    admit(&intents, &projection).map_err(|source| tx.error(source))?;
    before(interrupted, Some(deadline)).map_err(|source| tx.error(source))?;
    let V::Object(records) = &mut registry["physical"]["calls"] else {
        return Err(tx.error(invalid("invalid physical invocation registry")));
    };
    records.insert(
        identity.call_id.as_bytes().to_vec(),
        obj!("schema":if campaign_manifest_sha256.is_some() {2} else {1},"request":request,"state":"reserved","usage_ordinal":V::Null),
    );
    tx.replace(
        snapshot.as_deref(),
        &encoded(&registry).map_err(|source| tx.error(source))?,
    )?;
    let receipt = usage::begin_before(
        tx.repo(),
        identity.clone(),
        cumulative,
        interrupted,
        deadline,
    )
    .map_err(|source| tx.error(source))?;
    before(interrupted, Some(deadline)).map_err(|source| tx.error(source))?;
    let reserved = encoded(&registry).map_err(|source| tx.error(source))?;
    let record = &mut registry["physical"]["calls"][identity.call_id.as_str()];
    record["state"] = "ready".into();
    record["usage_ordinal"] = V::Int(receipt.ordinal.to_string());
    tx.replace(
        Some(&reserved),
        &encoded(&registry).map_err(|source| tx.error(source))?,
    )?;
    Ok(Registration::New(receipt))
}
