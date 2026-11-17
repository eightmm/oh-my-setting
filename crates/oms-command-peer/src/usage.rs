//! Provider envelopes, before answer decoding can discard accounting metadata.
use crate::{Error, Result, V, text};
use oms_command_telemetry::store::physical::{self, Registration};
use oms_command_telemetry::store::usage::{
    self, Cost, Cumulative, Identity, Phase, Receipt, Usage,
};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub(crate) struct Pending {
    pub receipt: Receipt,
    deadline: Instant,
}
impl Pending {
    pub fn fence(&self, interrupted: &AtomicBool) -> Result<()> {
        if interrupted.load(Ordering::Acquire) {
            return Err(Error::new(
                130,
                "provider launch cancelled; pending intent retained",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(Error::new(
                75,
                "provider admission deadline expired; pending intent retained",
            ));
        }
        Ok(())
    }
}

fn env(extra: &[(OsString, OsString)], key: &str) -> Option<String> {
    extra
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .or_else(|| std::env::var(key).ok())
        .filter(|s| !s.is_empty())
}
pub(crate) fn state_repository(
    repo: &Path,
    extra: &[(OsString, OsString)],
) -> Result<std::path::PathBuf> {
    let explicit = extra
        .iter()
        .rev()
        .find(|(key, _)| key == "OMS_STATE_REPO")
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os("OMS_STATE_REPO"))
        .filter(|s| !s.is_empty());
    let path = explicit
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.into());
    std::fs::canonicalize(path)
        .map_err(|_| Error::new(2, "cannot resolve provider usage state repository"))
}
pub(crate) fn operation_env(extra: &[(OsString, OsString)]) -> Result<Vec<(OsString, OsString)>> {
    let mut extra = extra.to_vec();
    let operation = env(&extra, "OMS_OPERATION_ID").unwrap_or(crate::id("op_")?);
    extra.push(("OMS_OPERATION_ID".into(), operation.into()));
    Ok(extra)
}
pub(crate) fn begin(
    repo: &Path,
    route: &crate::providers::Route,
    account: Option<&str>,
    write: bool,
    extra: &[(OsString, OsString)],
    interrupted: &AtomicBool,
) -> Result<Pending> {
    let identity = Identity {
        call_id: crate::id("call_")?,
        operation_id: env(extra, "OMS_OPERATION_ID")
            .ok_or_else(|| Error::new(2, "provider call has no operation identity"))?,
        provider: route.provider.clone(),
        selected_model: (route.model != "provider-default").then(|| route.model.clone()),
        account: account.filter(|s| *s != "default").map(str::to_owned),
        goal_id: env(extra, "OMS_GOAL_ID"),
        task_id: env(extra, "OMS_TASK_ID"),
        attempt_id: env(extra, "OMS_ATTEMPT_ID"),
        run_id: env(extra, "OMS_RUN_ID").or_else(|| env(extra, "OMS_GOAL_RUN_ID")),
    };
    let cumulative = if route.provider == "codex" {
        env(extra, "OMS_RESUME_SESSION").map(|session_id| Cumulative {
            session_id,
            baseline_unknown: true,
        })
    } else {
        None
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    match physical::begin(repo, identity, cumulative, write, interrupted, deadline).map_err(
        |e| {
            Error::new(
                e.exit_code(),
                format!("physical call registration failed; reconcile before retry: {e}"),
            )
        },
    )? {
        Registration::New(receipt) => Ok(Pending { receipt, deadline }),
        Registration::Existing { .. } => Err(Error::new(
            75,
            "physical call intent already exists; no provider resend occurred",
        )),
    }
}
fn count(row: &V, key: &str) -> Option<u64> {
    match row.get(key) {
        V::Num(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => n.parse().ok(),
        _ => None,
    }
}
fn observation(provider: &str, raw: &[u8]) -> (Usage, Cost, Option<String>, bool) {
    let mut usage = Usage::default();
    let mut cost = Cost::Unknown;
    let mut session = None;
    let mut reported = false;
    for line in raw.split(|b| *b == b'\n') {
        if oms_context::parse_json(line).is_err() {
            continue;
        }
        let Ok(row) = V::parse(line) else {
            continue;
        };
        match (provider, text(&row, "type")) {
            ("codex", "thread.started") => {
                session = row
                    .get("thread_id")
                    .text()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
            ("codex", "turn.completed") => {
                let u = row.get("usage");
                usage = Usage {
                    input_tokens: count(u, "input_tokens"),
                    output_tokens: count(u, "output_tokens"),
                    cache_read_tokens: count(u, "cached_input_tokens"),
                    cache_write_tokens: count(u, "cache_write_input_tokens"),
                    cache_in_input: Some(true),
                    total_tokens: count(u, "total_tokens"),
                };
                // Codex exec emits the latest thread total, not an additive event.
                // https://github.com/openai/codex/blob/main/codex-rs/exec/src/event_processor_with_jsonl_output.rs
                reported = true;
            }
            ("claude", "result") => {
                let u = row.get("usage");
                usage = Usage {
                    input_tokens: count(u, "input_tokens"),
                    output_tokens: count(u, "output_tokens"),
                    cache_read_tokens: count(u, "cache_read_input_tokens"),
                    cache_write_tokens: count(u, "cache_creation_input_tokens"),
                    cache_in_input: Some(false),
                    total_tokens: None,
                };
                cost = match row.get("total_cost_usd") {
                    V::Num(n) => usage::usd_to_microusd(n)
                        .map(Cost::ProviderEstimate)
                        .unwrap_or_default(),
                    _ => Cost::Unknown,
                };
                // The result covers this query, including its assistant/tool steps.
                // It is a client estimate, not authoritative provider billing.
                // https://code.claude.com/docs/en/agent-sdk/cost-tracking
                reported = true;
            }
            _ => {}
        }
    }
    (usage, cost, session, reported)
}
pub(crate) fn finish(
    repo: &Path,
    mut receipt: Receipt,
    raw: &[u8],
    complete: bool,
    cleanup_complete: Option<bool>,
) -> Result<()> {
    let (usage, cost, session, reported) = observation(&receipt.identity.provider, raw);
    receipt.revision = 1;
    receipt.phase = if complete && reported {
        Phase::Final
    } else {
        Phase::Unknown
    };
    receipt.usage = usage;
    receipt.cost = cost;
    receipt.source_sha256 = Some(format!("{:x}", Sha256::digest(raw)));
    receipt.cleanup_complete = cleanup_complete;
    if receipt.identity.provider == "codex" {
        if let Some(session_id) = session {
            if receipt
                .cumulative
                .as_ref()
                .is_some_and(|s| s.session_id != session_id)
            {
                return Err(Error::new(
                    2,
                    "provider reported a conflicting resumed session identity",
                ));
            }
            if receipt.cumulative.is_none() {
                receipt.cumulative = Some(Cumulative {
                    session_id,
                    baseline_unknown: false,
                });
            }
        } else if receipt.cumulative.is_none() && reported {
            // Without a thread identity a repeated cumulative snapshot cannot be
            // distinguished from fresh invocation usage. Keep its measurements
            // visible while retaining the unresolved liability.
            receipt.phase = Phase::Unknown;
        }
    }
    // Cancellation stops execution; it must not discard already observed spend.
    usage::record(repo, &receipt, &AtomicBool::new(false))
        .map(|_| ())
        .map_err(|e| Error::new(e.exit_code(), e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_envelopes_keep_cumulative_and_estimated_cost_semantics() {
        let (u,c,s,seen) = observation("codex",concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread_one\"}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":80,\"cached_input_tokens\":20,\"output_tokens\":3}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":30,\"output_tokens\":5}}\n"
        ).as_bytes());
        assert!(seen);
        assert_eq!(u.tokens(), Some(105));
        assert_eq!(c, Cost::Unknown);
        assert_eq!(s.as_deref(), Some("thread_one"));
        let (u,c,_,seen) = observation("claude",concat!(
            "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":100}}}\n",
            "{\"type\":\"result\",\"usage\":{\"input_tokens\":100,\"output_tokens\":5,\"cache_read_input_tokens\":20,\"cache_creation_input_tokens\":2},\"total_cost_usd\":0.0000011}\n"
        ).as_bytes());
        assert!(seen);
        assert_eq!(u.tokens(), Some(127));
        assert_eq!(c, Cost::ProviderEstimate(2));
        let (_, c, _, _) = observation("claude", b"{\"type\":\"result\",\"total_cost_usd\":0}\n");
        assert_eq!(c, Cost::ProviderEstimate(0));
        let (u, c, _, seen) = observation(
            "claude",
            b"usage detail: {\"input_tokens\":999}\ncost usd\n0\n",
        );
        assert!(!seen);
        assert_eq!(u.tokens(), None);
        assert_eq!(c, Cost::Unknown);
    }
}
