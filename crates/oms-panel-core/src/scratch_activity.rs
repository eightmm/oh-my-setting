//! Read-only lifecycle records from individually proven managed worktrees.
use oms_reference::repository::{
    AttemptHead, DiscoveryIssue, LifecycleScope, Resolver, SourceKind, SourceStatus,
};
use serde_json::{Value, json};
use std::{path::Path, sync::atomic::AtomicBool};

fn issue_code(issue: DiscoveryIssue) -> &'static str {
    match issue {
        DiscoveryIssue::Unavailable => "unavailable",
        DiscoveryIssue::OutsideScope => "outside_scope",
        DiscoveryIssue::AmbiguousRegistry => "ambiguous_registry",
        DiscoveryIssue::RegistryLimit => "registry_limit",
        DiscoveryIssue::InspectionLimit => "inspection_limit",
        DiscoveryIssue::UnverifiedRegistration => "unverified_registration",
        DiscoveryIssue::Deadline => "deadline",
        DiscoveryIssue::Interrupted => "interrupted",
        DiscoveryIssue::Changed => "changed",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScratchPeerRun {
    pub id: String,
    pub worktree: String,
    pub provider: Option<&'static str>,
    pub tool: &'static str,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

/// The caller owns canonical attempts and all action targets. These IDs are
/// opaque observation keys, never arguments for native attempt/Room commands.
pub fn observe(repo: &Path, stop: &AtomicBool) -> Value {
    observe_with_peer_runs(repo, stop).0
}

pub(crate) fn observe_with_peer_runs(
    repo: &Path,
    stop: &AtomicBool,
) -> (Value, Vec<ScratchPeerRun>) {
    let result = Resolver::from_env()
        .map_err(|_| DiscoveryIssue::Unavailable)
        .and_then(|r| r.observe_managed_lifecycle(repo, LifecycleScope::ScratchOnly, stop));
    let report = match result {
        Ok(r) => r,
        Err(issue) => {
            return (
                json!({"status":"unavailable","reason":issue_code(issue)}),
                Vec::new(),
            );
        }
    };
    let mut partial = report.discovery_issue.is_some()
        || report.unverified_checkouts != 0
        || report.unexamined_checkouts != 0;
    let mut sources = Vec::with_capacity(report.sources.len());
    for source in report
        .sources
        .iter()
        .filter(|s| s.kind == SourceKind::Scratch)
    {
        let status = match source.status {
            SourceStatus::Ready => "ready",
            SourceStatus::Missing => "missing",
            SourceStatus::Changed => "changed",
            SourceStatus::Unavailable => "unavailable",
            SourceStatus::Invalid => "invalid",
            SourceStatus::SourceLimit => "source_limit",
            SourceStatus::BudgetLimit => "budget_limit",
            SourceStatus::Deadline => "deadline",
        };
        partial |= !matches!(source.status, SourceStatus::Ready | SourceStatus::Missing);
        let head = match source.head {
            AttemptHead::Branch => "branch",
            AttemptHead::Detached => "detached",
            AttemptHead::Unknown => "unknown",
        };
        let label = format!(
            "wt-{} ({})",
            &source.id[..12],
            source.branch_label.unwrap_or(head)
        );
        sources.push(json!({"id":source.id,"label":label,"head":head,"branch":source.branch_label,"status":status}));
    }
    let peer_runs: Vec<ScratchPeerRun> = report
        .attempts
        .iter()
        .filter_map(|a| {
            let tool = a.tool.filter(|t| {
                matches!(
                    *t,
                    "peer-delegate" | "peer-review" | "peer-ask" | "consult" | "advise"
                )
            })?;
            let prefix = &a.source_id[..12.min(a.source_id.len())];
            Some(ScratchPeerRun {
                id: a.id.clone(),
                worktree: format!("wt-{prefix}"),
                provider: a.provider,
                tool,
                state: a.state.clone(),
                created_at: a.created_at.clone(),
                updated_at: a.updated_at.clone(),
            })
        })
        .take(crate::board::MAX_ACTIVITY_ROWS)
        .collect();
    let attempts: Vec<_> = report
        .attempts
        .iter()
        .take(16)
        .map(|a| {
            json!({
                "id":a.id,"sourceId":a.source_id,"provider":a.provider,"tool":a.tool,
                "state":a.state,"updatedAt":a.updated_at
            })
        })
        .collect();
    let omitted = report.omitted_attempts + report.attempts.len() - attempts.len();
    (
        json!({"status":if partial || omitted > 0 {"partial"} else {"ready"},
            "reason":report.discovery_issue.map(issue_code),"sources":sources,"attempts":attempts,
            "unverifiedCheckouts":report.unverified_checkouts,"unexaminedCheckouts":report.unexamined_checkouts,"omittedAttempts":omitted}),
        peer_runs,
    )
}
