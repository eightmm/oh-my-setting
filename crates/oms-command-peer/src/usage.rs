//! Provider envelopes, before answer decoding can discard accounting metadata.
use crate::{Error, Result, V, text};
use oms_command_telemetry::store::physical::{self, Registration};
use oms_command_telemetry::store::usage::{
    self, Cost, Cumulative, Identity, Phase, Receipt, Usage,
};
use oms_command_telemetry::store::{StoreError, budget, campaign, resources};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub(crate) struct Pending {
    pub receipt: Receipt,
    deadline: Instant,
    campaign_deadline: Option<i64>,
    resource: Option<(std::path::PathBuf, resources::Binding)>,
    expires_at: Option<i64>,
    campaign_binding: Option<Vec<(OsString, OsString)>>,
}
pub struct InteractiveRequest<'a> {
    pub state_root: &'a Path,
    pub provider: &'a str,
    pub selected_model: Option<&'a str>,
    pub account: Option<&'a str>,
    pub attempt: Option<&'a str>,
    pub resume_session: Option<&'a str>,
}
pub struct InteractiveContext<'a> {
    pub contract_repo: &'a Path,
    pub execution_repo: &'a Path,
    pub root: Option<crate::process::RootLocator<'a>>,
    pub team: Option<&'a crate::process::TeamLocator>,
    pub task_id: Option<&'a str>,
}
/// Dropping a handle retains its durable pending liability. It never refunds or
/// resends an invocation, and registration alone supplies no owner authority.
#[must_use]
pub struct InteractiveCall {
    state: std::path::PathBuf,
    pending: Pending,
    physical: crate::process::OwnedPhysicalAttempt,
}
/// The caller's selected shared state remains authoritative over an ambient
/// locator. Physical aliases are accepted; a different repository is refused.
pub fn interactive_state(selected: &Path) -> Result<std::path::PathBuf> {
    let selected = std::fs::canonicalize(selected)
        .map_err(|_| Error::new(2, "selected interactive state is unavailable"))?;
    if let Some(ambient) = std::env::var_os("OMS_STATE_REPO").filter(|value| !value.is_empty())
        && std::fs::canonicalize(ambient).ok().as_ref() != Some(&selected)
    {
        return Err(Error::new(
            2,
            "native accounting state differs from selected shared state",
        ));
    }
    Ok(selected)
}
pub fn begin_interactive(
    request: InteractiveRequest<'_>,
    interrupted: &AtomicBool,
) -> Result<InteractiveCall> {
    let selected = request.state_root;
    begin_interactive_with_executor(
        request,
        InteractiveContext {
            contract_repo: selected,
            execution_repo: selected,
            root: None,
            team: None,
            task_id: None,
        },
        &crate::process::Native,
        interrupted,
    )
}
pub fn begin_interactive_with_executor(
    request: InteractiveRequest<'_>,
    context: InteractiveContext<'_>,
    executor: &dyn crate::process::Executor,
    interrupted: &AtomicBool,
) -> Result<InteractiveCall> {
    let state = interactive_state(request.state_root)?;
    let mut extra = vec![(
        "OMS_RESUME_SESSION".into(),
        request.resume_session.unwrap_or("").into(),
    )];
    if let Some(attempt) = request.attempt {
        extra.push(("OMS_ATTEMPT_ID".into(), attempt.into()));
    }
    if let Some(task) = context.task_id {
        extra.push(("OMS_TASK_ID".into(), task.into()));
    }
    let extra = operation_env(&extra)?;
    let route = crate::providers::Route {
        provider: crate::providers::normalize_at(request.provider, context.contract_repo)?,
        model: request.selected_model.unwrap_or("provider-default").into(),
        effort: String::new(),
        explicit: request.selected_model.is_some(),
        account: request.account.unwrap_or("").into(),
        account_pinned: false,
        public_research: false,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let physical = crate::process::PhysicalAttempt {
        state_root: &state,
        contract_repo: context.contract_repo,
        execution_repo: context.execution_repo,
        root: context.root,
        team: context.team,
        provider: &route.provider,
        account: request.account.filter(|account| *account != "default"),
        selected_model: request.selected_model,
        selected_effort: None,
        task_id: context.task_id,
        write: true,
        persistent_session: false,
    };
    let mut registered = None;
    executor.with_physical_contract(
        &physical,
        deadline,
        interrupted,
        Box::new(|contract| {
            let pending = begin_before(
                &state,
                &route,
                request.account,
                true,
                &extra,
                interrupted,
                deadline,
                contract,
            )?;
            let call = pending.physical_call()?;
            registered = Some(pending);
            Ok(call)
        }),
    )?;
    let pending = registered.ok_or_else(|| {
        Error::new(
            75,
            "native admission returned without an interactive acknowledgement",
        )
    })?;
    let physical = physical.owned();
    Ok(InteractiveCall {
        state,
        pending,
        physical,
    })
}
impl InteractiveCall {
    pub fn call_id(&self) -> &str {
        &self.pending.receipt.identity.call_id
    }
    pub fn operation_id(&self) -> &str {
        &self.pending.receipt.identity.operation_id
    }
    pub fn fence(&self, interrupted: &AtomicBool) -> Result<()> {
        self.pending.fence(interrupted)
    }
    pub fn launch_fence(&self) -> crate::process::LaunchFence {
        self.pending.launch_fence()
    }
    /// Requalifies the original acknowledgement after the resource writer wait.
    /// The callback observes that call and cannot register or resend another one.
    pub fn fence_with_executor(
        &self,
        executor: &dyn crate::process::Executor,
        stop: &AtomicBool,
    ) -> Result<()> {
        self.pending.fence(stop)?;
        self.pending
            .qualify(&self.physical.borrow(), executor, stop)
    }
    /// Terminal output is never interpreted as accounting. Missing capture and
    /// missing native cleanup evidence remain absent independently of exit code.
    pub fn finish(self, output: Option<&[u8]>, cleanup_complete: Option<bool>) -> Result<()> {
        finish_unobserved(&self.state, self.pending.receipt, output, cleanup_complete)
    }
    pub fn finish_with_cleanup(
        self,
        output: Option<&[u8]>,
        cleanup_complete: Option<bool>,
        proof: Option<&oms_supervisor::process::OwnedCleanup>,
    ) -> Result<()> {
        let accounting = finish_unobserved(
            &self.state,
            self.pending.receipt.clone(),
            output,
            cleanup_complete,
        );
        let settlement = self.pending.settle_execution(proof);
        accounting?;
        settlement
    }
}
impl Pending {
    pub fn launch_fence(&self) -> crate::process::LaunchFence {
        crate::process::LaunchFence::new(self.deadline, self.expires_at)
            .with_campaign_deadline(self.campaign_deadline)
    }
    pub fn qualify(
        &self,
        attempt: &crate::process::PhysicalAttempt<'_>,
        executor: &dyn crate::process::Executor,
        stop: &AtomicBool,
    ) -> Result<()> {
        self.launch_fence().check(stop)?;
        let mut acknowledged = false;
        executor.with_physical_contract(
            attempt,
            self.deadline,
            stop,
            Box::new(|contract| {
                if contract.resource != self.resource.as_ref().map(|(_, binding)| binding)
                    || contract.expires_at != self.expires_at
                {
                    return Err(Error::new(
                        75,
                        "physical contract changed after registration; no resend occurred",
                    ));
                }
                if let Some(extra) = &self.campaign_binding {
                    let campaign = campaign_binding(
                        attempt.state_root,
                        extra,
                        &self.receipt.identity,
                        stop,
                        self.deadline,
                    )?
                    .ok_or_else(|| Error::new(75, "campaign binding disappeared before launch"))?;
                    campaign
                        .view
                        .active(
                            &campaign.manifest,
                            &self.receipt.identity.operation_id,
                            &self.receipt.identity.provider,
                            unix_now(),
                        )
                        .map_err(accounting)?;
                    campaign
                        .guard
                        .check()
                        .map_err(|error| Error::new(error.exit_code(), error.to_string()))?;
                }
                let call = self.physical_call()?;
                acknowledged = true;
                Ok(call)
            }),
        )?;
        if !acknowledged {
            return Err(Error::new(
                75,
                "native requalification returned without an acknowledgement; pending intent retained",
            ));
        }
        self.launch_fence().check(stop)
    }
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
        if self
            .campaign_deadline
            .is_some_and(|deadline| unix_now() >= deadline)
        {
            return Err(Error::new(
                3,
                "campaign deadline expired; pending intent retained",
            ));
        }
        if let Some((repo, binding)) = &self.resource {
            resources::fence(
                repo,
                binding,
                &self.physical_call()?,
                interrupted,
                self.deadline,
            )
            .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
        }
        self.launch_fence().check(interrupted)
    }
    pub fn physical_call(&self) -> Result<oms_supervisor::process::PhysicalCall> {
        oms_supervisor::process::PhysicalCall::new(&self.receipt.identity.call_id)
            .map_err(|e| Error::new(2, e))
    }
    pub fn settle_execution(
        &self,
        proof: Option<&oms_supervisor::process::OwnedCleanup>,
    ) -> Result<()> {
        if let (Some((repo, binding)), Some(proof)) = (&self.resource, proof) {
            // Persist owned cleanup after cancellation, just like terminal usage.
            // This independent deadline bounds its receipt writer wait.
            let settlement = AtomicBool::new(false);
            resources::settle_execution(
                repo,
                binding,
                &self.physical_call()?,
                proof,
                1,
                &settlement,
                Instant::now() + Duration::from_secs(10),
            )
            .map_err(|e| {
                Error::new(
                    e.exit_code(),
                    format!("physical cleanup settlement failed: {e}"),
                )
            })?;
        }
        Ok(())
    }
}
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(i64::MAX)
}
fn accounting(error: StoreError) -> Error {
    Error::new(error.exit_code(), error.to_string())
}
struct Campaign {
    manifest: campaign::Manifest,
    view: campaign::View,
    guard: oms_lock::Guard,
    deadline: i64,
}
fn campaign_binding(
    repo: &Path,
    extra: &[(OsString, OsString)],
    identity: &Identity,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Campaign>> {
    let binding = extra
        .iter()
        .rev()
        .find(|(key, _)| key == "OMS_CAMPAIGN_MANIFEST_SHA256")
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os("OMS_CAMPAIGN_MANIFEST_SHA256"));
    let Some(binding) = binding else {
        return Ok(None);
    };
    let digest = binding
        .to_str()
        .ok_or_else(|| Error::new(2, "campaign manifest binding is not UTF-8"))?;
    let directory = campaign::directory(digest).map_err(accounting)?;
    if identity.goal_id.as_deref() != Some(&digest[..16]) {
        return Err(Error::new(
            2,
            "campaign goal differs from the bound manifest",
        ));
    }
    let plan_error = |error: oms_command_plan::Error| Error::new(error.code, error.message);
    let store = oms_command_plan::storage::Store::open(repo).map_err(plan_error)?;
    let read = |path: &str, limit| store.read(path, limit).map_err(plan_error);
    let manifest_path = format!("{directory}/manifest.json");
    let events_path = format!("{directory}/events.jsonl");
    // The owning store checks the target before deriving its existing ledger
    // lock; producer bytes are read again after that lock is acquired.
    read(&manifest_path, campaign::MAX_MANIFEST)?;
    read(&events_path, campaign::MAX_EVENTS)?;
    let guard = oms_command_telemetry::store::lock_file_before(
        &store.repo().join(&events_path),
        interrupted,
        deadline,
    )
    .map_err(accounting)?;
    guard
        .check()
        .map_err(|error| accounting(StoreError::Lock(error)))?;
    let manifest = campaign::saved_manifest(
        &read(&manifest_path, campaign::MAX_MANIFEST)?,
        &digest[..16],
    )
    .map_err(accounting)?;
    if manifest.sha256 != digest {
        return Err(Error::new(2, "campaign manifest binding changed"));
    }
    let view = campaign::saved_events(&manifest, &read(&events_path, campaign::MAX_EVENTS)?)
        .map_err(accounting)?;
    let campaign_deadline = view
        .active(
            &manifest,
            &identity.operation_id,
            &identity.provider,
            unix_now(),
        )
        .map_err(accounting)?;
    guard
        .check()
        .map_err(|error| accounting(StoreError::Lock(error)))?;
    Ok(Some(Campaign {
        manifest,
        view,
        guard,
        deadline: campaign_deadline,
    }))
}
impl Campaign {
    fn admit(
        &self,
        identity: &Identity,
        intents: &[physical::Intent],
        projection: &usage::Projection,
    ) -> std::result::Result<(), StoreError> {
        self.guard.check().map_err(StoreError::Lock)?;
        self.view.active(
            &self.manifest,
            &identity.operation_id,
            &identity.provider,
            unix_now(),
        )?;
        if let Some(policy) = &self.manifest.policy {
            let operations = self.view.operations();
            let mut claims = Vec::new();
            for intent in intents.iter().filter(|intent| {
                self.view
                    .reservations
                    .contains_key(&intent.identity.operation_id)
            }) {
                if intent
                    .campaign_manifest_sha256
                    .as_ref()
                    .is_some_and(|digest| *digest != self.manifest.sha256)
                {
                    return Err(StoreError::Invalid(
                        "physical call has a conflicting campaign binding",
                    ));
                }
                claims.push(budget::PhysicalClaim {
                    call_id: &intent.identity.call_id,
                    operation_id: &intent.identity.operation_id,
                });
            }
            budget::admit_candidate(
                policy,
                projection,
                &operations,
                &claims,
                budget::PhysicalClaim {
                    call_id: &identity.call_id,
                    operation_id: &identity.operation_id,
                },
            )
            .map_err(StoreError::Budget)?;
        }
        self.guard.check().map_err(StoreError::Lock)
    }
}

pub(crate) fn env(extra: &[(OsString, OsString)], key: &str) -> Option<String> {
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin_before(
    repo: &Path,
    route: &crate::providers::Route,
    account: Option<&str>,
    write: bool,
    extra: &[(OsString, OsString)],
    interrupted: &AtomicBool,
    deadline: Instant,
    contract: crate::process::PhysicalContract<'_>,
) -> Result<Pending> {
    let resource = contract.resource;
    crate::process::LaunchFence::new(deadline, contract.expires_at).check(interrupted)?;
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
    let campaign = campaign_binding(repo, extra, &identity, interrupted, deadline)?;
    let campaign_deadline = campaign.as_ref().map(|campaign| campaign.deadline);
    let pinned_campaign = campaign.as_ref().map(|campaign| {
        vec![
            (
                "OMS_CAMPAIGN_MANIFEST_SHA256".into(),
                campaign.manifest.sha256.clone().into(),
            ),
            (
                "OMS_OPERATION_ID".into(),
                identity.operation_id.clone().into(),
            ),
            (
                "OMS_GOAL_ID".into(),
                identity.goal_id.clone().unwrap_or_default().into(),
            ),
        ]
    });
    let request = physical::Request {
        identity: identity.clone(),
        cumulative,
        write,
        campaign_manifest_sha256: campaign
            .as_ref()
            .map(|campaign| campaign.manifest.sha256.clone()),
    };
    let admit = |intents: &[physical::Intent], projection: &usage::Projection| {
        campaign.as_ref().map_or(Ok(()), |campaign| {
            campaign.admit(&identity, intents, projection)
        })
    };
    let registered = match resource {
        Some(binding) => {
            physical::begin_bound_checked(repo, request, binding, interrupted, deadline, admit)
        }
        None => physical::begin_checked(repo, request, interrupted, deadline, admit),
    }
    .map_err(|e| {
        Error::new(
            e.exit_code(),
            format!("physical call registration failed; reconcile before retry: {e}"),
        )
    })?;
    if let Some(campaign) = &campaign {
        campaign.guard.check().map_err(|error| Error::new(error.exit_code(),format!("campaign guard changed after physical registration; reconcile before retry: {error}")))?;
        campaign
            .view
            .active(
                &campaign.manifest,
                &identity.operation_id,
                &identity.provider,
                unix_now(),
            )
            .map_err(accounting)?;
    }
    match registered {
        Registration::New(receipt) => Ok(Pending {
            receipt,
            deadline,
            campaign_deadline,
            resource: resource.map(|binding| (repo.into(), binding.clone())),
            expires_at: contract.expires_at,
            campaign_binding: pinned_campaign,
        }),
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
fn antigravity_usage(raw: &[u8]) -> Option<Usage> {
    // JSON print mode emits one terminal envelope. Usage in a continued
    // conversation can be cumulative.
    // https://antigravity.google/docs/cli/headless/#json
    let row = oms_json::parse_strict(raw).ok()?;
    let count = |row: &oms_json::Value, key: &str| row.get(key)?.as_integer()?.as_u64();
    let conversation_id = row.get("conversation_id")?.as_string()?.to_str_checked()?;
    row.get("response")?.as_string()?;
    if row.get("status")?.as_string()?.to_str_checked()? != "SUCCESS"
        || row
            .get("error")
            .is_some_and(|error| !matches!(error, oms_json::Value::Null))
        || count(&row, "num_turns")? != 1
        || conversation_id.is_empty()
        || conversation_id
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let usage = row.get("usage")?;
    let input = count(usage, "input_tokens")?;
    let output = count(usage, "output_tokens")?;
    let cache_read = count(usage, "cache_read_tokens")?;
    let total = count(usage, "total_tokens")?;
    count(usage, "thinking_tokens")?;
    // The receipt store requires at least input + output. The provider's
    // explicit total needs no adjustment for cache reads or thinking.
    if total < input.checked_add(output)? {
        return None;
    }
    Some(Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_tokens: Some(cache_read),
        total_tokens: Some(total),
        ..Usage::default()
    })
}
fn observation(provider: &str, raw: &[u8]) -> (Usage, Cost, Option<String>, bool) {
    if provider == "antigravity" {
        let usage = antigravity_usage(raw);
        let reported = usage.is_some();
        return (usage.unwrap_or_default(), Cost::Unknown, None, reported);
    }
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
    let (usage, cost, session, reported) = if receipt.identity.provider == "antigravity"
        && (!complete || receipt.cumulative.is_some())
    {
        (Usage::default(), Cost::Unknown, None, false)
    } else {
        observation(&receipt.identity.provider, raw)
    };
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
    persist(repo, &receipt)
}

/// Interactive terminal output is not the provider's accounting protocol.
pub(crate) fn finish_unobserved(
    repo: &Path,
    mut receipt: Receipt,
    raw: Option<&[u8]>,
    cleanup_complete: Option<bool>,
) -> Result<()> {
    receipt.revision = 1;
    receipt.phase = Phase::Unknown;
    receipt.usage = Usage::default();
    receipt.cost = Cost::Unknown;
    receipt.source_sha256 = raw.map(|raw| format!("{:x}", Sha256::digest(raw)));
    receipt.cleanup_complete = cleanup_complete;
    persist(repo, &receipt)
}
fn persist(repo: &Path, receipt: &Receipt) -> Result<()> {
    // Cancellation stops execution; it must not discard already observed spend.
    usage::record(repo, receipt, &AtomicBool::new(false))
        .map(|_| ())
        .map_err(|e| Error::new(e.exit_code(), e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGY_SUCCESS: &str = r#"{
        "conversation_id": "ae91bb20-7313-446e-a21d-eff7bbfe3461",
        "status": "SUCCESS",
        "response": "fixture answer",
        "duration_seconds": 7.16,
        "num_turns": 1,
        "usage": {
            "input_tokens": 10415,
            "output_tokens": 657,
            "thinking_tokens": 616,
            "cache_read_tokens": 8113,
            "total_tokens": 11072
        }
    }"#;

    const AGY_ZERO: &str = r#"{
        "conversation_id": "zero-fixture",
        "status": "SUCCESS",
        "response": "",
        "num_turns": 1,
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "thinking_tokens": 0,
            "cache_read_tokens": 0,
            "total_tokens": 0
        }
    }"#;

    #[test]
    fn antigravity_single_turn_reports_explicit_token_total() {
        let (usage, cost, _, reported) = observation("antigravity", AGY_SUCCESS.as_bytes());
        assert!(reported);
        assert_eq!(
            usage,
            Usage {
                input_tokens: Some(10415),
                output_tokens: Some(657),
                cache_read_tokens: Some(8113),
                total_tokens: Some(11072),
                ..Usage::default()
            }
        );
        assert_eq!(usage.tokens(), Some(11072));
        assert_eq!(cost, Cost::Unknown);
    }

    #[test]
    fn antigravity_accepts_zero_and_exact_large_integers() {
        let usage = antigravity_usage(AGY_ZERO.as_bytes()).unwrap();
        assert_eq!(
            usage,
            Usage {
                input_tokens: Some(0),
                output_tokens: Some(0),
                cache_read_tokens: Some(0),
                total_tokens: Some(0),
                ..Usage::default()
            }
        );

        let max = u64::MAX.to_string();
        let maximum = AGY_ZERO
            .replace(r#""input_tokens": 0"#, &format!(r#""input_tokens": {max}"#))
            .replace(r#""total_tokens": 0"#, &format!(r#""total_tokens": {max}"#));
        let usage = antigravity_usage(maximum.as_bytes()).unwrap();
        assert_eq!(usage.input_tokens, Some(u64::MAX));
        assert_eq!(usage.tokens(), Some(u64::MAX));
    }

    #[test]
    fn antigravity_rejects_ambiguous_or_invalid_envelopes() {
        for (field, replacement) in [
            (r#""status": "SUCCESS","#, r#""status": "ERROR","#),
            (r#""status": "SUCCESS","#, r#""status": "TIMEOUT","#),
            (
                r#""status": "SUCCESS","#,
                r#""status": "SUCCESS", "error": "failed","#,
            ),
            (
                r#""status": "SUCCESS","#,
                r#""status": "ERROR", "status": "SUCCESS","#,
            ),
            (r#""status": "SUCCESS","#, ""),
            (
                r#""conversation_id": "ae91bb20-7313-446e-a21d-eff7bbfe3461","#,
                "",
            ),
            (
                r#""conversation_id": "ae91bb20-7313-446e-a21d-eff7bbfe3461","#,
                r#""conversation_id": "","#,
            ),
            ("ae91bb20-7313-446e-a21d-eff7bbfe3461", " "),
            (
                r#""status":"#,
                r#""conversation_id": "other-id", "status":"#,
            ),
            (r#""response": "fixture answer","#, r#""response": null,"#),
            (r#""response": "fixture answer","#, ""),
            (r#""num_turns": 1,"#, r#""num_turns": 0,"#),
            (r#""num_turns": 1,"#, r#""num_turns": 2,"#),
            (r#""num_turns": 1,"#, r#""num_turns": 1.0,"#),
            (r#""num_turns": 1,"#, r#""num_turns": 2, "num_turns": 1,"#),
            (r#""num_turns": 1,"#, ""),
            (r#""usage": {"#, r#""usage": {}, "usage": {"#),
            (
                r#""input_tokens": 10415,"#,
                r#""input_tokens": 999, "input_tokens": 10415,"#,
            ),
            (
                r#""input_tokens": 10415,"#,
                r#""input_tokens": 18446744073709551616,"#,
            ),
            (r#""output_tokens": 657,"#, r#""output_tokens": -1,"#),
            (r#""output_tokens": 657,"#, r#""output_tokens": 6.57e2,"#),
            (r#""thinking_tokens": 616,"#, r#""thinking_tokens": "616","#),
            (r#""thinking_tokens": 616,"#, ""),
            (
                r#""cache_read_tokens": 8113,"#,
                r#""cache_read_tokens": false,"#,
            ),
            (r#""cache_read_tokens": 8113,"#, ""),
            (r#""total_tokens": 11072"#, r#""total_tokens": null"#),
            (",\n            \"total_tokens\": 11072", ""),
        ] {
            let raw = AGY_SUCCESS.replacen(field, replacement, 1);
            assert_ne!(raw, AGY_SUCCESS);
            assert_eq!(
                observation("antigravity", raw.as_bytes()),
                (Usage::default(), Cost::Unknown, None, false),
                "accepted altered field {field:?} as {replacement:?}"
            );
        }
        for raw in [
            AGY_SUCCESS[..AGY_SUCCESS.len() - 1].to_owned(),
            format!("{AGY_SUCCESS}\n{AGY_SUCCESS}"),
            format!("{AGY_SUCCESS}diagnostic"),
            format!("[{AGY_SUCCESS}]"),
        ] {
            assert_eq!(antigravity_usage(raw.as_bytes()), None);
        }
    }

    #[test]
    fn antigravity_settlement_keeps_unsupported_totals_unknown() {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let path = std::env::temp_dir().join(crate::id("oms-antigravity-usage-").unwrap());
        std::fs::create_dir(&path).unwrap();
        let repo = Fixture(path);
        let stop = AtomicBool::new(false);

        // The documented second-turn step reports these counts. A fresh result
        // with this counter relationship must also satisfy store validation.
        let high_cache = AGY_SUCCESS
            .replace("10415", "278")
            .replace("657", "4")
            .replace("616", "0")
            .replace("8113", "30214")
            .replace("11072", "282");
        let below_sum = high_cache.replace("282", "281");
        let overflow = high_cache.replace("278", &u64::MAX.to_string());
        let resumed = high_cache.replace(r#""num_turns": 1"#, r#""num_turns": 2"#);
        let cases = [
            (high_cache.as_str(), true, None, Some((282, 30214))),
            (AGY_ZERO, true, None, Some((0, 0))),
            (high_cache.as_str(), false, None, None),
            (below_sum.as_str(), true, None, None),
            (overflow.as_str(), true, None, None),
            (resumed.as_str(), true, None, None),
            (high_cache.as_str(), true, Some("fixture-resumed"), None),
        ];
        for (index, &(raw, complete, session_id, _)) in cases.iter().enumerate() {
            let pending = usage::begin(
                &repo.0,
                Identity {
                    call_id: format!("call_{index}"),
                    operation_id: format!("op_{index}"),
                    provider: "antigravity".into(),
                    ..Identity::default()
                },
                session_id.map(|id| Cumulative {
                    session_id: id.into(),
                    baseline_unknown: true,
                }),
                &stop,
            )
            .unwrap();
            finish(&repo.0, pending, raw.as_bytes(), complete, Some(true)).unwrap();
        }
        let calls = usage::load(&repo.0).unwrap().calls;
        assert_eq!(calls.len(), cases.len());
        for (call, (raw, _, session_id, expected)) in calls.iter().zip(cases) {
            assert_eq!(call.tokens, expected.map(|(tokens, _)| tokens));
            assert_eq!(
                call.receipt
                    .cumulative
                    .as_ref()
                    .map(|s| s.session_id.as_str()),
                session_id
            );
            assert_eq!(call.cost_microusd, None);
            assert_eq!(call.receipt.revision, 1);
            assert_eq!(call.receipt.cleanup_complete, Some(true));
            assert_eq!(
                call.receipt.source_sha256,
                Some(format!("{:x}", Sha256::digest(raw.as_bytes())))
            );
            if let Some((_, cache_read)) = expected {
                assert_eq!(call.receipt.phase, Phase::Final);
                assert_eq!(call.receipt.usage.cache_read_tokens, Some(cache_read));
                assert_eq!(call.receipt.usage.cache_in_input, None);
            } else {
                assert_eq!(call.receipt.phase, Phase::Unknown);
                assert_eq!(call.receipt.usage, Usage::default());
            }
        }
    }

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
