//! Lossless command requests for the parent CLI adapter and internal consumers.
use crate::{Result, error, require};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SnapshotDecision,
    Init,
    EnsureLineage,
    Retire,
    ApplyProposal,
    Add,
    Claim,
    Start,
    Touch,
    Review,
    Repair,
    Land,
    Finish,
    Amend,
    Cancel,
    RelocateDecision,
    AcceptResearch,
    ApproveMainResearch,
    AcceptMainResearch,
    Satisfy,
    LintVerify,
    Block,
    Release,
    RecoverLease,
    RecoverOwner,
    Reclaim,
    Reopen,
    Show,
    EvidenceSnapshot,
    List,
    Ready,
    Discovered,
    Retro,
    Status,
    Accept,
    Brief,
    Next,
}
impl Action {
    pub const ALL: &'static [(&'static str, Action)] = &[
        ("snapshot-decision", Self::SnapshotDecision),
        ("init", Self::Init),
        ("ensure-lineage", Self::EnsureLineage),
        ("retire", Self::Retire),
        ("apply-proposal", Self::ApplyProposal),
        ("add", Self::Add),
        ("claim", Self::Claim),
        ("start", Self::Start),
        ("touch", Self::Touch),
        ("review", Self::Review),
        ("repair", Self::Repair),
        ("land", Self::Land),
        ("finish", Self::Finish),
        ("amend", Self::Amend),
        ("cancel", Self::Cancel),
        ("relocate-decision", Self::RelocateDecision),
        ("accept-research", Self::AcceptResearch),
        ("approve-main-research", Self::ApproveMainResearch),
        ("accept-main-research", Self::AcceptMainResearch),
        ("satisfy", Self::Satisfy),
        ("lint-verify", Self::LintVerify),
        ("block", Self::Block),
        ("release", Self::Release),
        ("recover-lease", Self::RecoverLease),
        ("recover-owner", Self::RecoverOwner),
        ("reclaim", Self::Reclaim),
        ("reopen", Self::Reopen),
        ("show", Self::Show),
        ("evidence-snapshot", Self::EvidenceSnapshot),
        ("list", Self::List),
        ("ready", Self::Ready),
        ("discovered", Self::Discovered),
        ("retro", Self::Retro),
        ("status", Self::Status),
        ("accept", Self::Accept),
        ("brief", Self::Brief),
        ("next", Self::Next),
    ];
    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(_, a)| *a == self).unwrap().0
    }
    pub fn completion(self) -> bool {
        matches!(
            self,
            Self::AcceptResearch
                | Self::ApproveMainResearch
                | Self::AcceptMainResearch
                | Self::Satisfy
        )
    }
}
#[derive(Debug, Clone)]
pub struct Request {
    pub action: Action,
    pub repo: Option<PathBuf>,
    pub file: Option<PathBuf>,
    pub values: BTreeMap<String, OsString>,
    pub flags: BTreeSet<String>,
}
pub(crate) const BOOLEANS: &[&str] = &[
    "all-inactive",
    "check",
    "apply",
    "claim",
    "refreeze-acceptance",
    "include-running",
    "include-review",
    "approve-reviewed-bundle",
    "json",
];
pub(crate) const NAMES: &[&str] = &[
    "repo",
    "file",
    "id",
    "title",
    "goal",
    "provider",
    "ttl",
    "reason",
    "artifact",
    "patch",
    "landed-commit",
    "executor-id",
    "executor-soul-sha256",
    "expected-review-patch",
    "expected-review-patch-sha256",
    "expected-review-verify",
    "expected-review-executor-id",
    "expected-review-executor-soul-sha256",
    "expected-review-lease-id",
    "expected-landing-receipt-sha256",
    "assignment",
    "depends",
    "resources",
    "contracts",
    "priority",
    "max-writers",
    "max-review-backlog",
    "allowed",
    "role",
    "discovered-from",
    "limit",
    "forbidden",
    "verify",
    "reuse-pass",
    "accept",
    "accept-files",
    "owner-id",
    "markers-dir",
    "expected-state",
    "expected-lease-id",
    "disposition",
    "proposal",
    "expected-proposal-sha256",
    "expected-plan-sha256",
    "expected-task-sha256",
    "operator-consent",
    "decision-artifact",
    "expected-decision-artifact-sha256",
    "completion-bundle",
    "expected-completion-bundle-sha256",
    "allowed-envelope",
    "max-tasks",
    "state",
    "lease-id",
    "main-review-approval",
    "expected-main-review-approval-sha256",
];
impl Request {
    pub fn new(action: Action) -> Self {
        Self {
            action,
            repo: None,
            file: None,
            values: BTreeMap::new(),
            flags: BTreeSet::new(),
        }
    }
    pub fn with(mut self, name: &str, value: impl Into<OsString>) -> Self {
        self.values.insert(name.into(), value.into());
        self
    }
    pub fn flag(mut self, name: &str) -> Self {
        self.flags.insert(name.into());
        self
    }
    pub fn has(&self, name: &str) -> bool {
        self.flags.contains(name) || self.values.contains_key(name)
    }
    pub fn text(&self, name: &str) -> Result<&str> {
        self.values
            .get(name)
            .map(|s| {
                s.to_str()
                    .ok_or_else(|| error(format!("--{name} requires UTF-8 text")))
            })
            .transpose()
            .map(|s| s.unwrap_or(""))
    }
    pub fn value(&self, name: &str) -> Result<&str> {
        let s = self.text(name)?;
        require(
            !s.is_empty(),
            format!("--{name} is required for {}", self.action.name()),
        )?;
        Ok(s)
    }
    pub fn path(&self, name: &str) -> Option<PathBuf> {
        self.values.get(name).map(PathBuf::from)
    }
    pub fn read_only(&self) -> bool {
        matches!(
            self.action,
            Action::Show
                | Action::EvidenceSnapshot
                | Action::List
                | Action::Ready
                | Action::Discovered
                | Action::Retro
                | Action::Status
                | Action::Brief
                | Action::LintVerify
        ) || self.action == Action::Next && !self.has("claim")
    }
    /// Exact agent-plan shell option spellings. No prefix aliases or `--x=y`
    /// extensions; options may precede or follow the command.
    pub fn parse(args: &[OsString]) -> Result<Self> {
        let (booleans, names) = (BOOLEANS, NAMES);
        let mut action = None;
        let mut values = BTreeMap::new();
        let mut flags = BTreeSet::new();
        let mut at = 0;
        while at < args.len() {
            let word = args[at]
                .to_str()
                .ok_or_else(|| error("unknown non-text argument"))?;
            if let Some(option) = word.strip_prefix("--") {
                if booleans.contains(&option) {
                    flags.insert(option.to_owned());
                    at += 1;
                    continue;
                }
                require(names.contains(&option), format!("unknown argument: {word}"))?;
                if ["depends", "allowed", "forbidden"].contains(&option) {
                    require(
                        !values.contains_key(option),
                        format!("--{option} given twice; pass one comma-separated list \"p1,p2\""),
                    )?;
                }
                let value = args
                    .get(at + 1)
                    .ok_or_else(|| error(format!("{word} requires value")))?;
                values.insert(option.to_owned(), value.clone());
                at += 2;
            } else {
                let selected = Action::ALL
                    .iter()
                    .find(|(name, _)| *name == word)
                    .map(|(_, a)| *a)
                    .ok_or_else(|| error(format!("unknown argument: {word}")))?;
                require(action.is_none(), "multiple commands")?;
                action = Some(selected);
                at += 1;
            }
        }
        let repo = values.remove("repo").map(PathBuf::from);
        let file = values.remove("file").map(PathBuf::from);
        let req = Self {
            action: action.ok_or_else(|| error("agent-plan requires a command"))?,
            repo,
            file,
            values,
            flags,
        };
        req.validate()?;
        Ok(req)
    }
    pub fn validate(&self) -> Result<()> {
        use Action::*;
        for (flag, allowed) in [
            ("check", matches!(self.action, RecoverLease | Retire)),
            ("apply", self.action == Retire),
            ("all-inactive", self.action == Cancel),
            ("discovered-from", self.action == Add),
            ("limit", self.action == Ready),
            ("reuse-pass", self.action == Accept),
            ("landed-commit", self.action == Finish),
        ] {
            require(
                !self.has(flag) || allowed,
                format!("--{flag} is invalid for {}", self.action.name()),
            )?;
        }
        require(
            !(self.has("check") && self.has("apply")),
            "retire --apply and --check are mutually exclusive",
        )?;
        require(
            !(self.has("landed-commit")
                && (self.has("expected-landing-receipt-sha256")
                    || self.has("refreeze-acceptance"))),
            "--landed-commit cannot be combined with typed patch-land finish options",
        )?;
        for name in [
            "ttl",
            "limit",
            "max-tasks",
            "max-writers",
            "max-review-backlog",
        ] {
            if self.has(name) {
                let s = self.value(name)?;
                require(
                    s.bytes().all(|b| b.is_ascii_digit()),
                    format!("--{name} must be an integer"),
                )?;
                let n = s
                    .parse::<u64>()
                    .map_err(|_| error(format!("--{name} is too large")))?;
                require(
                    name != "limit" || n > 0,
                    "--limit must be a positive integer",
                )?;
                require(
                    name != "max-tasks" || (1..=12).contains(&n),
                    "--max-tasks must be 1..12",
                )?;
            }
        }
        if self.has("priority") {
            self.value("priority")?
                .parse::<i32>()
                .map_err(|_| error("--priority must be an i32"))?;
        }
        for name in ["resources", "contracts", "priority"] {
            require(
                !self.has(name) || self.action == Add,
                format!("--{name} is invalid for {}", self.action.name()),
            )?;
        }
        for name in ["max-writers", "max-review-backlog"] {
            require(
                !self.has(name) || matches!(self.action, Ready | Next),
                format!("--{name} is invalid for {}", self.action.name()),
            )?;
        }
        let completion_options = [
            "completion-bundle",
            "expected-completion-bundle-sha256",
            "approve-reviewed-bundle",
            "main-review-approval",
            "expected-main-review-approval-sha256",
        ];
        require(
            !self.has("expected-task-sha256")
                || self.action.completion()
                || matches!(self.action, Amend | Cancel | RelocateDecision),
            "completion options require their matching completion action",
        )?;
        require(
            self.action.completion() || !completion_options.iter().any(|n| self.has(n)),
            "completion options require their matching completion action",
        )?;
        require(
            !self.has("approve-reviewed-bundle") || self.action == ApproveMainResearch,
            "approval and acceptance are separate actions",
        )?;
        require(
            !self.has("main-review-approval") && !self.has("expected-main-review-approval-sha256")
                || self.action == AcceptMainResearch,
            "main approval flags require accept-main-research",
        )?;
        require(
            self.has("main-review-approval") == self.has("expected-main-review-approval-sha256"),
            "main review approval requires its exact digest",
        )?;
        let only = match self.action {
            SnapshotDecision => Some(vec![
                "decision-artifact",
                "expected-decision-artifact-sha256",
                "json",
            ]),
            Amend => Some(vec![
                "id",
                "expected-plan-sha256",
                "expected-task-sha256",
                "allowed",
                "verify",
                "reason",
            ]),
            Cancel | RelocateDecision => Some(vec![
                "id",
                "all-inactive",
                "expected-plan-sha256",
                "expected-task-sha256",
                "expected-state",
                "expected-lease-id",
                "operator-consent",
                "reason",
                "decision-artifact",
                "expected-decision-artifact-sha256",
            ]),
            a if a.completion() => Some(vec![
                "id",
                "lease-id",
                "expected-state",
                "expected-lease-id",
                "expected-plan-sha256",
                "expected-task-sha256",
                "completion-bundle",
                "expected-completion-bundle-sha256",
                "approve-reviewed-bundle",
                "main-review-approval",
                "expected-main-review-approval-sha256",
                "json",
                "operator-consent",
                "decision-artifact",
                "expected-decision-artifact-sha256",
            ]),
            _ => None,
        };
        if let Some(allowed) = only {
            require(
                self.values
                    .keys()
                    .chain(self.flags.iter())
                    .all(|n| allowed.contains(&n.as_str())),
                "options cannot be mixed with unrelated actions",
            )?;
        }
        if matches!(
            self.action,
            SnapshotDecision | Amend | Cancel | RelocateDecision
        ) {
            require(
                self.file.is_none(),
                "action requires canonical repo-local plan",
            )?;
        }
        if self.action == Cancel && self.has("all-inactive") {
            require(
                ![
                    "id",
                    "expected-task-sha256",
                    "expected-state",
                    "expected-lease-id",
                ]
                .iter()
                .any(|n| self.has(n)),
                "--all-inactive cannot be combined with task-specific options",
            )?;
        }
        if self.action == RelocateDecision {
            require(
                !["all-inactive", "expected-state", "expected-lease-id"]
                    .iter()
                    .any(|n| self.has(n)),
                "relocate-decision cannot be mixed with cancel state/lease",
            )?;
        }
        Ok(())
    }
}
