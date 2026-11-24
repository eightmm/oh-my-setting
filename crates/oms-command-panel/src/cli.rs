use crate::{Error, Result, require};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

const FLAGS: &[&str] = &[
    "help",
    "reopen",
    "providers",
    "health",
    "desktop-usage",
    "desktop-chat",
    "desktop-work-items",
    "sessions",
    "next-attention",
    "switch",
    "usage",
    "metrics",
    "json",
    "watch",
    "log",
    "follow",
    "no-animation",
    "attention-only",
    "routes",
    "teams",
    "council",
    "until-agreement",
    "results",
    "chats",
    "allow-verifier-change",
    "finalize",
    "settle-review",
    "apply",
    "legacy",
    "standalone",
    "notify",
    "retry-delivery",
    "allow-stale-base",
    "no-web",
    "dry-run",
    "spread-accounts",
    "archived",
    "unbound",
];
const VALUES: &[&str] = &[
    "desktop-view",
    "repo",
    "layout",
    "host",
    "room",
    "launch",
    "spawn-main",
    "orchestrator",
    "focus-orchestrator",
    "lead",
    "swap-main",
    "resume",
    "model",
    "to",
    "account",
    "reasoning-effort",
    "release-main",
    "state",
    "provider",
    "role",
    "min-age",
    "compare",
    "since",
    "min-samples",
    "owner",
    "view",
    "recommend",
    "risk",
    "size",
    "diff",
    "failed",
    "task",
    "dispatch",
    "main",
    "scope",
    "rounds",
    "lenses",
    "max-rounds",
    "thread",
    "export",
    "page",
    "salvage",
    "mail",
    "participant",
    "open-chat",
    "chat-surface",
    "conversation-digest",
    "evidence-section",
    "evidence-attempt",
    "before",
    "accept",
    "allow-floor-change",
    "message",
    "expected-patch-sha256",
    "landed-commit",
    "owner-attempt",
    "legacy-older-than",
    "older-than",
    "aged-days",
    "attempt",
    "reason",
    "summary-file",
    "outcome",
    "evidence",
    "verify-timeout",
    "workload",
    "access",
    "purpose",
    "prompt",
    "brief-file",
    "continue",
    "verify",
    "task-id",
    "native-result",
    "native-reconcile",
    "native-adopt",
    "assignment-sha256",
    "result-sha256",
    "intent-sha256",
    "decision",
    "expected-previous-result-sha256",
    "after-native-result",
    "from-task",
    "label",
    "alias",
    "session",
    "cwd",
    "maximum",
    "generation",
    "id",
];
const REPEATED: &[&str] = &["scope", "attempt", "evidence", "from-task"];

#[derive(Clone, Debug, Default)]
pub struct Options {
    flags: BTreeSet<String>,
    values: BTreeMap<String, Vec<String>>,
    pub command: Option<String>,
    pub action: Option<String>,
    pub ident: Option<String>,
    pub argv: Vec<OsString>,
    pub count_rows: bool,
}
impl Options {
    pub(crate) fn selects_main(&self) -> bool {
        self.command.is_none()
            && !self.count_rows
            && self
                .flags
                .iter()
                .all(|key| matches!(key.as_str(), "attention-only" | "no-animation"))
            && self
                .values
                .keys()
                .all(|key| matches!(key.as_str(), "repo" | "room" | "host" | "layout" | "view"))
    }

    pub fn has(&self, key: &str) -> bool {
        self.flags.contains(key) || self.values.contains_key(key)
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key)?.last().map(String::as_str)
    }
    pub fn text(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or(default).to_owned()
    }
    pub fn all(&self, key: &str) -> Vec<String> {
        self.values.get(key).cloned().unwrap_or_default()
    }
    pub fn required(&self, key: &str) -> Result<String> {
        self.get(key)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Error::new(format!("--{key} is required")))
    }
    pub fn integer(&self, key: &str, default: usize, low: usize, high: usize) -> Result<usize> {
        let value = match self.get(key) {
            Some(v) => v
                .parse::<usize>()
                .map_err(|_| Error::new(format!("invalid --{key}")))?,
            None => default,
        };
        require(
            (low..=high).contains(&value),
            &format!("--{key} must be {low} to {high}"),
        )?;
        Ok(value)
    }
    fn choice(&self, key: &str, choices: &[&str]) -> Result<()> {
        require(
            self.get(key).is_none_or(|v| choices.contains(&v)),
            &format!("invalid --{key}: choose {}", choices.join(", ")),
        )
    }
    fn needs(&self, keys: &[&str], parents: &[&str]) -> Result<()> {
        if keys.iter().any(|key| self.has(key)) {
            require(
                parents.iter().any(|key| self.has(key)),
                &format!(
                    "--{} requires --{}",
                    keys.join(", --"),
                    parents.join(" or --")
                ),
            )?;
        }
        Ok(())
    }
    pub fn parse(args: &[OsString]) -> Result<Self> {
        let mut out = Self::default();
        let mut i = 0;
        if let Some(v) = args.first().and_then(|v| v.to_str())
            && matches!(v, "terminal" | "job")
        {
            out.command = Some(v.into());
            i = 1;
        }
        while i < args.len() {
            let word = args[i]
                .to_str()
                .ok_or_else(|| Error::new("panel options must be UTF-8"))?;
            i += 1;
            if word == "--" {
                require(
                    out.command.as_deref() == Some("job"),
                    "explicit argv is only accepted by job run",
                )?;
                out.argv.extend_from_slice(&args[i..]);
                break;
            }
            if word == "-h" {
                out.flags.insert("help".into());
                continue;
            }
            if let Some(option) = word.strip_prefix("--") {
                let (key, inline) = option
                    .split_once('=')
                    .map_or((option, None), |(k, v)| (k, Some(v)));
                if FLAGS.contains(&key) {
                    require(inline.is_none(), &format!("--{key} takes no argument"))?;
                    out.flags.insert(key.into());
                } else if key == "count"
                    && inline.is_none()
                    && args
                        .get(i)
                        .is_none_or(|v| v.to_str().is_none_or(|v| v.starts_with("--")))
                {
                    out.count_rows = true;
                } else {
                    require(
                        VALUES.contains(&key) || key == "count",
                        &format!("unrecognized argument: --{key}"),
                    )?;
                    let value = match inline {
                        Some(v) => v,
                        None => {
                            let v = args.get(i).and_then(|v| v.to_str()).ok_or_else(|| {
                                Error::new(format!("--{key} requires an argument"))
                            })?;
                            require(
                                !v.starts_with("--"),
                                &format!("--{key} requires an argument"),
                            )?;
                            i += 1;
                            v
                        }
                    };
                    let values = out.values.entry(key.into()).or_default();
                    if matches!(key, "launch" | "spawn-main") {
                        require(values.is_empty(), "choose one Main provider")?;
                    }
                    if key == "lead" {
                        require(values.is_empty(), "choose one provider lead")?;
                    }
                    if !REPEATED.contains(&key) {
                        values.clear();
                    }
                    values.push(value.into());
                }
            } else if out.command.is_some() && out.action.is_none() {
                out.action = Some(word.into());
            } else if out.command.is_some() && out.ident.is_none() {
                out.ident = Some(word.into());
            } else {
                return Err(Error::new(format!("unrecognized argument: {word}")));
            }
        }
        if !out.has("help") {
            out.validate()?;
        }
        Ok(out)
    }
    fn validate(&self) -> Result<()> {
        if let Some(command) = self.command.as_deref() {
            let allowed = [
                "repo",
                "session",
                "cwd",
                "label",
                "maximum",
                "json",
                "archived",
                "generation",
                "id",
            ];
            require(
                self.flags
                    .iter()
                    .chain(self.values.keys())
                    .all(|key| allowed.contains(&key.as_str())),
                "unsupported managed command option",
            )?;
            require(
                !self.has("id")
                    || (command == "job" && self.action.as_deref() == Some("_supervise")),
                "--id is internal to job _supervise",
            )?;
            let actions: &[&str] = if command == "job" {
                &[
                    "run",
                    "list",
                    "logs",
                    "interrupt",
                    "stop",
                    "restart",
                    "forget",
                    "archive",
                    "restore",
                    "_supervise",
                ]
            } else {
                &[
                    "open",
                    "list",
                    "focus",
                    "stop",
                    "read",
                    "controls",
                    "forget",
                    "archive",
                    "restore",
                    "workspaces",
                    "workspace-focus",
                    "workspace-forget",
                    "workspace-archive",
                    "workspace-restore",
                ]
            };
            require(
                self.action.as_deref().is_some_and(|a| actions.contains(&a)),
                "invalid managed action",
            )?;
            self.required("repo")?;
            require(
                !self.has("archived")
                    || matches!(self.action.as_deref(), Some("list" | "workspaces")),
                "--archived requires list or workspaces",
            )?;
            require(
                self.argv.is_empty() || self.action.as_deref() == Some("run"),
                "argv requires job run",
            )?;
            self.integer("maximum", 32768, 1, 32768)?;
            if self.has("generation") {
                self.integer("generation", 1, 1, usize::MAX)?;
            }
            return Ok(());
        }
        require(
            !["archived", "session", "cwd", "maximum", "generation", "id"]
                .iter()
                .any(|key| self.has(key)),
            "managed command options require terminal or job",
        )?;
        for (key, choices) in [
            ("layout", &["auto", "inline", "split"][..]),
            ("host", &["auto", "inline", "tmux", "herdr", "desktop"][..]),
            (
                "view",
                &["auto", "summary", "compact", "detail", "graph", "tree"][..],
            ),
            ("access", &["read", "write"][..]),
            ("purpose", &["explain", "investigate", "implement"][..]),
            (
                "outcome",
                &["completed", "accepted", "failed", "rejected"][..],
            ),
            ("chat-surface", &["auto", "terminal", "app"][..]),
            ("risk", &["low", "medium", "high"][..]),
            ("size", &["small", "medium", "large"][..]),
        ] {
            self.choice(key, choices)?;
        }
        self.choice("desktop-view", crate::desktop_board::VIEWS)?;
        if self.has("desktop-view") {
            require(
                self.has("json")
                    && self.flags.iter().all(|s| s == "json")
                    && self.values.keys().all(|s| {
                        matches!(
                            s.as_str(),
                            "desktop-view" | "page" | "repo" | "conversation-digest"
                        )
                    }),
                "--desktop-view accepts only --json, --page, --repo and a host conversation binding",
            )?;
            self.integer("page", 1, 1, crate::desktop_board::MAX_PAGE)?;
        }
        if self.has("desktop-work-items") {
            require(
                self.has("json")
                    && self
                        .flags
                        .iter()
                        .all(|s| matches!(s.as_str(), "json" | "desktop-work-items"))
                    && self.values.keys().all(|s| s == "repo"),
                "--desktop-work-items accepts only --json and --repo",
            )?;
        }
        self.choice("dispatch", oms_panel_core::routing::ROLES)?;
        self.choice("workload", oms_panel_core::routing::TIERS)?;
        for key in ["orchestrator", "focus-orchestrator"] {
            self.choice(key, &crate::orchestrator::PROVIDERS)?;
        }
        if self.has("orchestrator") {
            require(
                !self.has("task") || !(self.has("model") || self.has("reasoning-effort")),
                "orchestrator model and effort apply only when opening a new session",
            )?;
            if let Some(effort) = self.get("reasoning-effort") {
                require(
                    !effort.is_empty()
                        && effort.len() <= 32
                        && effort.as_bytes()[0].is_ascii_lowercase()
                        && effort.bytes().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c)
                        }),
                    "invalid native reasoning effort token",
                )?;
            }
        } else {
            self.choice(
                "reasoning-effort",
                &["auto", "low", "medium", "high", "xhigh", "max", "ultra"],
            )?;
        }
        require(
            self.get("host") != Some("desktop")
                || (self.has("launch")
                    && self.has("task")
                    && self.has("json")
                    && self
                        .get("room")
                        .is_some_and(|id| id.starts_with("oms-app-"))
                    && !["account", "resume"].iter().any(|key| self.has(key))),
            "--host desktop requires --launch, --task and --json for a fresh managed Main",
        )?;
        require(
            (!self.has("before") || self.has("desktop-chat"))
                && (!self.has("conversation-digest")
                    || self.has("desktop-chat")
                    || self.has("desktop-view")),
            "desktop reader selectors require a compatible desktop view",
        )?;
        if self.has("desktop-chat") || self.has("desktop-usage") {
            require(self.has("json"), "desktop reads require --json")?;
        }
        let modes = [
            "watch",
            "launch",
            "spawn-main",
            "orchestrator",
            "native-reconcile",
            "native-adopt",
            "focus-orchestrator",
            "swap-main",
            "reopen",
            "dispatch",
            "routes",
            "teams",
            "council",
            "results",
            "accept",
            "finalize",
            "retry-delivery",
            "providers",
            "chats",
            "open-chat",
            "health",
            "mail",
            "metrics",
            "desktop-usage",
            "desktop-chat",
            "desktop-work-items",
            "sessions",
            "usage",
            "compare",
            "next-attention",
            "log",
            "recommend",
            "salvage",
            "release-main",
            "settle-review",
        ];
        require(
            modes.iter().filter(|v| self.has(v)).count() <= 1,
            "choose one panel operation",
        )?;
        require(
            !self.has("continue") || modes.iter().all(|key| *key == "dispatch" || !self.has(key)),
            "--continue cannot combine with another panel operation",
        )?;
        let active_modes: Vec<&str> = modes.iter().copied().filter(|k| self.has(k)).collect();
        require(
            !self.has("dry-run")
                || active_modes.iter().all(|key| {
                    matches!(
                        *key,
                        "launch" | "dispatch" | "continue" | "council" | "open-chat"
                    )
                }),
            "--dry-run only plans a launch, role dispatch, council or chat navigation",
        )?;
        require(
            !self.has("swap-main")
                || (self.get("layout") != Some("inline")
                    && self
                        .get("host")
                        .is_none_or(|v| ["auto", "tmux"].contains(&v))),
            "--swap-main requires an existing tmux panel",
        )?;
        require(
            !self.has("reopen")
                || (!self.has("json")
                    && self
                        .get("host")
                        .is_none_or(|v| ["auto", "tmux"].contains(&v))),
            "--reopen restores a tmux board; omit JSON and non-tmux hosts",
        )?;
        require(
            !(self.has("view") || self.has("attention-only"))
                || modes.iter().all(|key| *key == "watch" || !self.has(key)),
            "--view and --attention-only apply to the interactive menu or --watch",
        )?;
        require(
            !self.has("owner")
                || [
                    "watch",
                    "log",
                    "dispatch",
                    "continue",
                    "council",
                    "accept",
                    "finalize",
                    "results",
                    "settle-review",
                    "orchestrator",
                    "native-reconcile",
                    "native-adopt",
                    "focus-orchestrator",
                ]
                .iter()
                .any(|key| self.has(key)),
            "--owner requires an owner task operation or watch",
        )?;
        self.choice(
            "state",
            &["working", "waiting", "blocked", "done", "failed", "unknown"],
        )?;
        self.needs(&["state", "provider", "role", "min-age"], &["sessions"])?;
        self.needs(&["switch"], &["next-attention"])?;
        self.needs(&["since", "min-samples"], &["metrics"])?;
        self.needs(&["follow"], &["log"])?;
        self.needs(
            &["reasoning-effort"],
            &["dispatch", "continue", "recommend", "orchestrator"],
        )?;
        self.needs(
            &[
                "alias",
                "main",
                "to",
                "brief-file",
                "from-task",
                "purpose",
                "access",
                "no-web",
            ],
            &["dispatch", "continue"],
        )?;
        if self.has("lead") {
            self.choice("lead", &["alternate"])?;
            require(
                (self.get("orchestrator") == Some("codex") && self.has("account"))
                    || self.get("focus-orchestrator") == Some("codex"),
                "--lead alternate requires --orchestrator codex --account ID or --focus-orchestrator codex",
            )?;
        }
        self.needs(
            &["account"],
            &["dispatch", "continue", "orchestrator", "launch"],
        )?;
        require(
            !self.has("spread-accounts")
                || (self.get("orchestrator") == Some("claude") && !self.has("account")),
            "--spread-accounts requires --orchestrator claude without --account",
        )?;
        require(
            !(self.get("orchestrator") == Some("antigravity") && self.has("account")),
            "provider account isolation is unproved for antigravity",
        )?;
        if let Some(v) = self.get("account") {
            require(valid_account_id(v), "invalid account id")?;
            if let Some(harness) = self.get("launch") {
                require(
                    matches!(harness, "codex" | "claude"),
                    "--launch --account requires codex or claude",
                )?;
                require(
                    !self.has("resume"),
                    "--launch --account cannot resume without a verified native session account binding",
                )?;
            }
        }
        self.needs(&["prompt", "label"], &["dispatch", "continue", "council"])?;
        self.needs(
            &["workload"],
            &["dispatch", "continue", "council", "recommend"],
        )?;
        self.needs(&["verify"], &["dispatch", "continue", "accept", "finalize"])?;
        self.needs(
            &[
                "rounds",
                "lenses",
                "thread",
                "until-agreement",
                "max-rounds",
            ],
            &["council"],
        )?;
        self.needs(&["max-rounds"], &["until-agreement"])?;
        self.needs(
            &[
                "summary-file",
                "outcome",
                "evidence",
                "notify",
                "verify-timeout",
                "landed-commit",
            ],
            &["finalize"],
        )?;
        self.needs(
            &[
                "message",
                "expected-patch-sha256",
                "allow-verifier-change",
                "allow-floor-change",
            ],
            &["accept"],
        )?;
        self.needs(&["export"], &["results"])?;
        self.needs(&["page"], &["results", "desktop-view", "desktop-chat"])?;
        self.needs(&["evidence-section", "evidence-attempt"], &["desktop-chat"])?;
        if self.has("desktop-chat") {
            require(
                self.has("evidence-section") == self.has("evidence-attempt")
                    && (!self.has("page") || self.has("evidence-section"))
                    && !(self.has("before") && self.has("evidence-section")),
                "select an exact evidence attempt and section or a Room message page",
            )?;
            self.choice(
                "evidence-section",
                &["info", "events", "files", "diff", "transcript"],
            )?;
        }
        self.needs(&["participant"], &["results", "desktop-chat"])?;
        self.needs(&["resume"], &["launch"])?;
        self.needs(&["task"], &["launch", "spawn-main", "orchestrator"])?;
        self.needs(
            &["model"],
            &[
                "launch",
                "spawn-main",
                "swap-main",
                "dispatch",
                "continue",
                "recommend",
                "orchestrator",
            ],
        )?;
        self.needs(&["compare"], &["compare"])?;
        if let Some(v) = self.get("compare") {
            crate::identifier(v)?;
        }
        self.needs(&["allow-stale-base"], &["continue"])?;
        self.needs(
            &[
                "apply",
                "owner-attempt",
                "legacy",
                "legacy-older-than",
                "standalone",
                "older-than",
                "aged-days",
                "reason",
            ],
            &["settle-review"],
        )?;
        self.needs(&["attempt"], &["settle-review", "salvage"])?;
        self.needs(&["no-animation"], &["watch"])?;
        self.needs(
            &["task-id"],
            &[
                "dispatch",
                "council",
                "results",
                "finalize",
                "retry-delivery",
                "orchestrator",
            ],
        )?;
        if self.has("native-result") {
            require(
                self.has("orchestrator") && self.has("task-id"),
                "--native-result requires --orchestrator codex and --task-id",
            )?;
        }
        if self.has("task-id") && self.has("orchestrator") {
            require(
                self.get("orchestrator") == Some("codex")
                    && (self.has("task") != self.has("native-result"))
                    && !self.has("lead")
                    && !self.has("model")
                    && !self.has("reasoning-effort")
                    && !self.has("spread-accounts"),
                "bound --task-id operations require an existing ordinary Codex lead and either --task or --native-result",
            )?;
        }
        self.needs(&["result-sha256"], &["native-reconcile", "native-adopt"])?;
        self.needs(&["intent-sha256", "decision"], &["native-reconcile"])?;
        self.needs(&["assignment-sha256"], &["native-adopt"])?;
        if self.has("native-adopt") {
            self.required("assignment-sha256")?;
            crate::identifier(&self.required("native-adopt")?)?;
        }
        self.needs(&["expected-previous-result-sha256"], &["native-result"])?;
        self.needs(&["after-native-result", "unbound"], &["orchestrator"])?;
        if self.has("native-reconcile") {
            let decision = self.required("decision")?;
            self.choice(
                "decision",
                &["request-revision", "blocked", "reviewed", "retire-unsent"],
            )?;
            let (required, excluded) = if decision == "retire-unsent" {
                ("intent-sha256", "result-sha256")
            } else {
                ("result-sha256", "intent-sha256")
            };
            self.required(required)?;
            require(
                !self.has(excluded),
                "use --intent-sha256 for retire-unsent and --result-sha256 for result decisions",
            )?;
            crate::identifier(&self.required("native-reconcile")?)?;
        }
        if self.has("after-native-result") {
            require(
                self.has("task-id")
                    && self.has("task")
                    && self.get("orchestrator") == Some("codex"),
                "--after-native-result requires a bound ordinary Codex assignment",
            )?;
        }
        if self.has("unbound") {
            require(
                self.has("task") && !self.has("task-id"),
                "--unbound requires a native conversation without --task-id",
            )?;
        }
        for key in [
            "assignment-sha256",
            "result-sha256",
            "intent-sha256",
            "expected-previous-result-sha256",
            "after-native-result",
        ] {
            if let Some(sha) = self.get(key) {
                require(
                    sha.len() == 64
                        && sha
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                    "native digests must be lowercase SHA-256",
                )?;
            }
        }
        if self.has("count") {
            require(
                self.has("watch") || (self.has("log") && self.has("follow")),
                "--count requires --watch or --log --follow",
            )?;
            self.integer("count", 1, 1, 1_000_000)?;
        }
        require(
            !self.count_rows || self.has("sessions"),
            "--count without a number requires --sessions",
        )?;
        if self.has("page") {
            self.integer(
                "page",
                1,
                1,
                if self.has("desktop-chat") {
                    10000
                } else {
                    1000
                },
            )?;
            require(!self.has("task-id"), "--page takes no --task-id")?;
        }
        if self.has("verify-timeout") {
            self.integer("verify-timeout", 1800, 1, 86400)?;
        }
        if self.has("min-samples") {
            self.integer("min-samples", 3, 1, usize::MAX)?;
        }
        if self.has("rounds") {
            self.integer("rounds", 1, 0, 2)?;
        }
        if self.has("max-rounds") {
            self.integer("max-rounds", 4, 1, 6)?;
        }
        require(
            !self.has("until-agreement") || !(self.has("rounds") || self.has("lenses")),
            "--until-agreement replaces --rounds and excludes --lenses",
        )?;
        require(
            !self.has("watch") || !(self.has("json") || self.has("dry-run")),
            "--watch cannot combine with JSON or dry-run",
        )?;
        require(
            !self.has("json")
                || self.get("host") == Some("desktop")
                || !["launch", "orchestrator", "focus-orchestrator"]
                    .iter()
                    .any(|key| self.has(key)),
            "--json is read-only; use --dry-run for a launch plan",
        )?;
        require(
            !(self.has("json") && self.has("dry-run")) || self.has("open-chat"),
            "choose --json or --dry-run",
        )?;
        require(
            !self.has("export") || !self.has("json"),
            "--export takes no --json",
        )?;
        require(
            !self.has("mail") || (self.has("room") && !self.has("json") && !self.has("dry-run")),
            "--mail requires --room and plain read-only output",
        )?;
        require(
            !self.has("chat-surface") || self.has("open-chat"),
            "--chat-surface requires --open-chat",
        )?;
        require(
            !self.has("open-chat") || !self.has("json") || self.has("dry-run"),
            "chat navigation JSON requires --dry-run",
        )?;
        require(
            !self.has("no-web") || self.get("dispatch") == Some("researcher"),
            "--no-web requires researcher dispatch",
        )?;
        require(
            !self.has("from-task") || self.has("brief-file"),
            "--from-task requires --brief-file",
        )?;
        self.needs(&["risk", "size", "diff", "failed"], &["recommend"])?;
        require(
            !self.has("next-attention")
                || !(self.has("dry-run")
                    || self.has("view")
                    || self.has("attention-only")
                    || self.has("room")
                    || self.has("json")),
            "--next-attention takes no --dry-run, --view, --attention-only, --room or --json",
        )?;
        let write_dispatch = self
            .get("dispatch")
            .is_some_and(|r| oms_panel_core::routing::WRITERS.contains(&r))
            && self.text("access", "read") == "write";
        let write_continue = self.has("continue")
            && self
                .get("dispatch")
                .is_none_or(|r| oms_panel_core::routing::WRITERS.contains(&r))
            && self.get("access").is_none_or(|a| a == "write");
        require(
            !self.has("scope") || write_dispatch || write_continue,
            "--scope requires a write-capable dispatch with --access write (or --continue)",
        )?;
        if self.has("continue") {
            require(
                self.has("brief-file") && !self.has("prompt") && !self.has("task-id"),
                "--continue requires --brief-file and takes no --prompt or --task-id",
            )?;
        }
        if self.has("accept") {
            for key in ["repo", "message", "verify"] {
                self.required(key)?;
            }
            require(!self.has("task-id"), "--accept takes no --task-id")?;
        }
        if self.has("finalize") {
            for key in ["owner", "task-id", "summary-file", "outcome"] {
                self.required(key)?;
            }
            require(
                !self.has("landed-commit")
                    || matches!(self.get("outcome"), Some("completed" | "accepted")),
                "--landed-commit requires accepted or completed outcome",
            )?;
        }
        if self.has("retry-delivery") {
            self.required("task-id")?;
        }
        if self.has("council") {
            self.required("prompt")?;
        }
        for key in ["task", "prompt"] {
            if let Some(v) = self.get(key) {
                require(
                    v.len() <= 65536 && !v.contains('\0'),
                    "task exceeds panel input contract",
                )?;
            }
        }
        if let Some(v) = self.get("model") {
            require(
                valid_model_id(v)
                    || (v.len() <= 160
                        && self.get("orchestrator") == Some("antigravity")
                        && crate::orchestrator::agy_fixed_effort_model(v).is_some()),
                "invalid model ID",
            )?;
        }
        for key in ["task-id", "room", "main", "participant", "resume"] {
            if let Some(v) = self.get(key) {
                crate::identifier(v)?;
            }
        }
        if let Some(v) = self.get("allow-floor-change") {
            require(
                !v.trim().is_empty() && !v.chars().any(char::is_control),
                "--allow-floor-change requires a nonblank single-line reason",
            )?;
        }
        Ok(())
    }
}

pub(crate) fn valid_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte))
}

pub(crate) fn valid_account_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && (value.as_bytes()[0].is_ascii_lowercase() || value.as_bytes()[0].is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}

pub fn usage() -> String {
    let mut text = String::from(
        "usage: oms panel [terminal ACTION | job ACTION] [OPTIONS]\n\nOMS terminal control panel with registered CLI peers.\n\nInteractive startup reuses a Main or offers installed harnesses on tmux. Set OMS_PANEL_MAIN to select a fresh Main, or use --watch for the board alone.\n\n",
    );
    text.push_str("Provider sessions: --orchestrator codex|claude|antigravity [--account ID] [--model MODEL] [--reasoning-effort EFFORT]\nModel and effort apply to new sessions only. The native CLI validates availability.\nUnbound conversation: --orchestrator PROVIDER [--account ID] --unbound --task TEXT\nFocus a provider session: --focus-orchestrator PROVIDER\n\n");
    text.push_str("Codex alternate: --orchestrator codex --lead alternate --account ID [--task TEXT] | --focus-orchestrator codex --lead alternate\n\n");
    text.push_str("Bind a canonical task: --orchestrator codex --task-id ID --task TEXT\nReuses the exact claim and durable intent; an unknown send is never resent automatically.\n\n");
    text.push_str("Retain a native report: --orchestrator codex --task-id ID --native-result FILE\nImports digest-bound findings or candidate evidence; does not confirm consumption, completion or acceptance.\n\n");
    text.push_str("Typed return paths/templates and next actions: --results [--task-id ID] --json\nA changed typed report requires --expected-previous-result-sha256 SHA; first capture and identical replay do not.\nReconcile an exact retained result: --native-reconcile ATTEMPT --result-sha256 SHA --decision request-revision|blocked|reviewed\nRetire a verified-unsent first intent: --native-reconcile ATTEMPT --intent-sha256 SHA --decision retire-unsent\nNative next round: --orchestrator codex --task-id ID --task TEXT --after-native-result SHA\nReviewed candidates use existing Plan review, patch-admit and patch-land; reports do not accept or finish tasks.\nClaude/AGY canonical work uses managed --dispatch ROLE --task-id ID until bound native adapters exist.\n\n");
    text.push_str("Fresh Claude account spread: --orchestrator claude --spread-accounts\n\n");
    text.push_str("After a canonical Main swap: --native-adopt ATTEMPT --assignment-sha256 SHA [--result-sha256 SHA]\nResults supplies the exact adoption command. Omit result SHA only when no current result exists.\nAdoption preserves the frozen claim, attempt, account and round; it never resends input or grants acceptance.\nSettled tasks permit retirement-only adoption even after the checkout advances.\n\n");
    text.push_str("Managed native Main: --launch codex|claude [--account ID] [--task TEXT]\n\n");
    text.push_str("Read: --teams [--json] | --results [--task-id ID] [--page N] [--json] [--export FILE]\nWatch: --watch [--view auto|summary|compact|detail|graph|tree] [--count N]\nWork: --dispatch ROLE --prompt TEXT | --brief-file FILE [--dry-run]\nContinue: --continue TASK --brief-file FILE [--dispatch ROLE]\nCouncil: --council --prompt TEXT [--lenses NAMES | --rounds N] [--dry-run]\nAccept: --accept TASK --repo PATH --message TEXT --verify COMMAND\nFinalize: --finalize --owner OWNER --task-id ID --summary-file FILE --outcome OUTCOME\nRecovery: --salvage TASK [--attempt ID]; --retry-delivery --task-id ID\n\nOptions:\n");
    for key in FLAGS {
        text.push_str(&format!("  --{key}\n"));
    }
    for key in VALUES {
        text.push_str(&format!("  --{key} VALUE\n"));
    }
    text.push_str("  --count [N]\n  -h, --help\n\nTerminal actions: open list focus read controls forget archive restore workspaces workspace-forget workspace-archive workspace-restore\nJob actions: run list logs interrupt stop restart forget archive restore\nJob recipes follow -- as explicit argv. Publication remains an explicit owner action.\n");
    text
}

#[cfg(test)]
mod tests {
    use super::{Options, usage};

    #[test]
    fn desktop_detail_uses_the_production_option_grammar() {
        let args = |s: &[&str]| s.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(
            Options::parse(&args(&[
                "--repo",
                ".",
                "--desktop-chat",
                "--json",
                "--room",
                "room-a",
                "--participant",
                "main",
                "--conversation-digest",
                "00000000000000000000000000000000",
                "--before",
                "42"
            ]))
            .is_ok()
        );
        assert!(Options::parse(&args(&["--repo", ".", "--desktop-usage", "--json"])).is_ok());
        assert!(
            Options::parse(&args(&[
                "--repo",
                ".",
                "--usage",
                "--conversation-digest",
                "wrong",
                "--json"
            ]))
            .is_err()
        );
    }

    #[test]
    fn only_startup_options_enter_the_overall_main_picker() {
        let parse = |args: &[&str]| {
            Options::parse(&crate::commands::strings(args))
                .unwrap_or_else(|error| panic!("{args:?}: {error}"))
        };
        for args in [
            &[][..],
            &["--repo", "."],
            &["--room", "room-one"],
            &["--repo", ".", "--room", "room-one", "--layout", "inline"],
        ] {
            assert!(parse(args).selects_main(), "{args:?}");
        }
        for args in [
            &["--watch", "--owner", "codex"][..],
            &["--watch", "--count", "1"],
            &["--launch", "claude", "--room", "room-one"],
            &["--spawn-main", "codex"],
            &["--json"],
            &["--dry-run"],
            &["terminal", "list", "--repo", "."],
        ] {
            assert!(!parse(args).selects_main(), "{args:?}");
        }
    }

    #[test]
    fn canonical_native_task_requires_the_supported_surface_and_body() {
        let parse = |args: &[&str]| Options::parse(&crate::commands::strings(args));
        let bound = [
            "--orchestrator",
            "codex",
            "--task-id",
            "task-one",
            "--task",
            "inspect source",
        ];
        assert!(parse(&bound).is_ok());
        assert!(parse(&[bound.as_slice(), &["--account", "work"]].concat()).is_ok());
        let returned = [
            "--orchestrator",
            "codex",
            "--task-id",
            "task-one",
            "--native-result",
            "return.json",
        ];
        assert!(parse(&returned).is_ok());
        assert!(parse(&[returned.as_slice(), &["--account", "work"]].concat()).is_ok());
        for args in [
            vec!["--native-result", "return.json"],
            vec![
                "--results",
                "--task-id",
                "task-one",
                "--native-result",
                "return.json",
            ],
            vec!["--orchestrator", "codex", "--native-result", "return.json"],
            vec![
                "--orchestrator",
                "claude",
                "--task-id",
                "task-one",
                "--native-result",
                "return.json",
            ],
            [returned.as_slice(), &["--task", "send again"]].concat(),
        ] {
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
        for args in [
            &["--orchestrator", "codex", "--task-id", "task-one"][..],
            &[
                "--orchestrator",
                "claude",
                "--task-id",
                "task-one",
                "--task",
                "inspect source",
            ],
            &[
                "--orchestrator",
                "antigravity",
                "--task-id",
                "task-one",
                "--task",
                "inspect source",
            ],
        ] {
            assert!(parse(args).is_err(), "accepted {args:?}");
        }
        for extra in [
            &["--lead", "alternate", "--account", "work"][..],
            &["--model", "vendor/model-x"],
            &["--reasoning-effort", "high"],
        ] {
            assert!(parse(&[bound.as_slice(), extra].concat()).is_err());
            assert!(parse(&[returned.as_slice(), extra].concat()).is_err());
        }
        assert!(usage().contains("--orchestrator codex --task-id ID --task TEXT"));
        assert!(usage().contains("--orchestrator codex --task-id ID --native-result FILE"));
        let sha = "a".repeat(64);
        let adopt = ["--native-adopt", "attempt-one", "--assignment-sha256", &sha];
        assert!(parse(&adopt).is_ok());
        assert!(parse(&[adopt.as_slice(), &["--result-sha256", &sha]].concat()).is_ok());
        for invalid in [
            &["--native-adopt", "attempt-one"][..],
            &[
                "--native-adopt",
                "attempt-one",
                "--assignment-sha256",
                "bad",
            ],
            &["--assignment-sha256", &sha],
        ] {
            assert!(parse(invalid).is_err());
        }
        assert!(parse(&[adopt.as_slice(), &["--decision", "reviewed"]].concat()).is_err());
        assert!(parse(&[adopt.as_slice(), &["--orchestrator", "codex"]].concat()).is_err());
        assert!(parse(&[bound.as_slice(), &["--after-native-result", &sha]].concat()).is_ok());
        assert!(
            parse(
                &[
                    returned.as_slice(),
                    &["--expected-previous-result-sha256", &sha]
                ]
                .concat()
            )
            .is_ok()
        );
        for decision in ["request-revision", "blocked", "reviewed"] {
            assert!(
                parse(&[
                    "--native-reconcile",
                    "attempt-one",
                    "--result-sha256",
                    &sha,
                    "--decision",
                    decision
                ])
                .is_ok()
            );
        }
        let retire = [
            "--native-reconcile",
            "attempt-one",
            "--intent-sha256",
            &sha,
            "--decision",
            "retire-unsent",
            "--room",
            "room-valid",
            "--owner",
            "codex",
        ];
        assert!(parse(&retire).is_ok());
        assert!(parse(&[retire.as_slice(), &["--result-sha256", &sha]].concat()).is_err());
        assert!(parse(&[retire.as_slice(), bound.as_slice()].concat()).is_err());
        for args in [
            &["--native-reconcile", "attempt-one"][..],
            &[
                "--native-reconcile",
                "attempt-one",
                "--intent-sha256",
                "bad",
                "--decision",
                "retire-unsent",
            ],
            &[
                "--native-reconcile",
                "attempt-one",
                "--result-sha256",
                &sha,
                "--decision",
                "retire-unsent",
            ],
            &[
                "--native-reconcile",
                "attempt-one",
                "--intent-sha256",
                &sha,
                "--decision",
                "reviewed",
            ],
            &[
                "--native-reconcile",
                "attempt-one",
                "--result-sha256",
                &sha,
                "--decision",
                "accepted",
            ],
            &[
                "--native-reconcile",
                "attempt-one",
                "--result-sha256",
                "bad",
                "--decision",
                "reviewed",
            ],
            &[
                "--orchestrator",
                "codex",
                "--task",
                "chat",
                "--after-native-result",
                &sha,
            ],
            &["--unbound"],
        ] {
            assert!(parse(args).is_err(), "accepted {args:?}");
        }
        assert!(parse(&["--orchestrator", "claude", "--unbound", "--task", "chat"]).is_ok());
        assert!(parse(&[bound.as_slice(), &["--unbound"]].concat()).is_err());
        assert!(parse(&[returned.as_slice(), &["--after-native-result", &sha]].concat()).is_err());
        for guide in [
            "--native-adopt",
            "--native-reconcile",
            "--expected-previous-result-sha256",
            "--after-native-result",
            "--unbound",
        ] {
            assert!(usage().contains(guide));
        }
    }

    #[test]
    fn alternate_lead_is_only_for_explicit_codex_launch_task_and_focus() {
        let parse = |args: &[&str]| Options::parse(&crate::commands::strings(args));
        let launch = [
            "--orchestrator",
            "codex",
            "--lead",
            "alternate",
            "--account",
            "work",
        ];
        let focus = ["--focus-orchestrator", "codex", "--lead", "alternate"];
        assert!(parse(&launch).is_ok());
        assert!(parse(&[launch.as_slice(), &["--task", "do work"]].concat()).is_ok());
        assert!(parse(&focus).is_ok());
        for args in [
            &["--lead", "alternate"][..],
            &["--orchestrator", "codex", "--lead", "alternate"],
            &[
                "--orchestrator",
                "claude",
                "--lead",
                "alternate",
                "--account",
                "work",
            ],
            &["--focus-orchestrator", "claude", "--lead", "alternate"],
            &["--focus-orchestrator", "codex", "--lead", "primary"],
            &[
                "--launch",
                "codex",
                "--lead",
                "alternate",
                "--account",
                "work",
            ],
            &["--dispatch", "builder", "--lead", "alternate"],
        ] {
            assert!(parse(args).is_err(), "accepted {args:?}");
        }
        for extra in [
            &["--spread-accounts"][..],
            &["--lead", "alternate"],
            &["--resume", "00112233-4455-4677-8899-aabbccddeeff"],
        ] {
            assert!(parse(&[launch.as_slice(), extra].concat()).is_err());
        }
        assert!(parse(&[focus.as_slice(), &["--account", "work"]].concat()).is_err());
        assert!(usage().contains("--focus-orchestrator codex --lead alternate"));
    }

    #[test]
    fn orchestrator_actions_cannot_resume_unbound_vendor_sessions() {
        let parse = |args: &[&str]| Options::parse(&crate::commands::strings(args));
        let uuid = "00112233-4455-4677-8899-aabbccddeeff";
        for provider in crate::orchestrator::PROVIDERS {
            assert!(parse(&["--orchestrator", provider, "--resume", uuid]).is_err());
            assert!(parse(&["--focus-orchestrator", provider, "--resume", uuid]).is_err());
        }
        assert!(
            parse(&[
                "--host",
                "desktop",
                "--launch",
                "codex",
                "--room",
                "oms-app-example",
                "--task=--report",
                "--json"
            ])
            .is_ok()
        );
        assert!(
            parse(&[
                "--host",
                "desktop",
                "--launch",
                "codex",
                "--task=goal",
                "--json"
            ])
            .is_err()
        );
        assert!(parse(&["--launch", "codex", "--resume", "legacy-session"]).is_ok());
    }

    #[test]
    fn explicit_main_account_is_only_for_fresh_codex_and_claude_launches() {
        let parse = |args: &[&str]| Options::parse(&crate::commands::strings(args));
        for provider in ["codex", "claude"] {
            assert!(parse(&["--launch", provider, "--account", "work"]).is_ok());
            assert!(parse(&["--launch", provider, "--account", "work", "--dry-run"]).is_ok());
            assert!(parse(&["--launch", provider, "--account", "Work"]).is_err());
            assert!(
                parse(&[
                    "--launch",
                    provider,
                    "--account",
                    "work",
                    "--resume",
                    "native-session"
                ])
                .is_err()
            );
            assert!(parse(&["--launch", provider, "--resume", "native-session"]).is_ok());
        }
        for provider in ["pi", "agy", "antigravity"] {
            assert!(parse(&["--launch", provider, "--account", "work"]).is_err());
            assert!(parse(&["--launch", provider]).is_ok());
        }
        assert!(parse(&["--spawn-main", "codex", "--account", "work"]).is_err());
        assert!(parse(&["--launch", "gemini", "--launch=codex"]).is_err());
        assert!(parse(&["--spawn-main=claude", "--spawn-main", "claude"]).is_err());
        assert!(usage().contains("--launch codex|claude [--account ID]"));
    }

    #[test]
    fn native_claude_spread_has_only_the_unpinned_orchestrator_mode() {
        let parse = |args: &[&str]| Options::parse(&crate::commands::strings(args));
        assert!(parse(&["--orchestrator", "claude", "--spread-accounts"]).is_ok());
        for args in [
            &["--spread-accounts"][..],
            &["--orchestrator", "codex", "--spread-accounts"],
            &["--orchestrator", "antigravity", "--spread-accounts"],
            &["--launch", "claude", "--spread-accounts"],
            &["--focus-orchestrator", "claude", "--spread-accounts"],
            &[
                "--orchestrator",
                "claude",
                "--account",
                "work",
                "--spread-accounts",
            ],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
        assert!(usage().contains("--orchestrator claude --spread-accounts"));
    }

    #[test]
    fn orchestrator_account_cli_validation_and_usage() {
        let parse = |args: &[&str]| Options::parse(&crate::commands::strings(args));
        for provider in ["codex", "claude"] {
            assert!(parse(&["--orchestrator", provider, "--account", "work"]).is_ok());
            assert!(parse(&["--orchestrator", provider, "--account", "0_acct-1"]).is_ok());
            assert!(
                parse(&[
                    "--orchestrator",
                    provider,
                    "--account",
                    "work",
                    "--task",
                    "check"
                ])
                .is_ok()
            );
            assert!(parse(&["--focus-orchestrator", provider, "--account", "work"]).is_err());
        }
        let agy_err = parse(&["--orchestrator", "antigravity", "--account", "work"]).unwrap_err();
        assert_eq!(
            agy_err.message,
            "provider account isolation is unproved for antigravity"
        );
        assert!(parse(&["--focus-orchestrator", "antigravity", "--account", "work"]).is_err());

        let too_long = "a".repeat(65);
        for bad in [
            "",
            "-work",
            "_work",
            "Work",
            "bad.id",
            "bad/id",
            too_long.as_str(),
        ] {
            assert!(
                parse(&["--orchestrator", "codex", "--account", bad]).is_err(),
                "{bad:?}"
            );
            assert!(
                parse(&[
                    "--dispatch",
                    "worker",
                    "--prompt",
                    "x",
                    "--to",
                    "codex",
                    "--account",
                    bad
                ])
                .is_err(),
                "{bad:?}"
            );
        }
        let help = usage();
        assert!(help.contains(
            "Provider sessions: --orchestrator codex|claude|antigravity [--account ID] [--model MODEL] [--reasoning-effort EFFORT]"
        ));
    }

    #[test]
    fn orchestrator_selection_is_start_only() {
        let parse = |args: &[&str]| Options::parse(&crate::commands::strings(args));
        for provider in crate::orchestrator::PROVIDERS {
            assert!(
                parse(&[
                    "--orchestrator",
                    provider,
                    "--model",
                    "fixture/model-v2",
                    "--reasoning-effort",
                    "xhigh",
                ])
                .is_ok()
            );
            for option in ["--model", "--reasoning-effort"] {
                assert!(parse(&["--focus-orchestrator", provider, option, "high"]).is_err());
                assert!(
                    parse(&[
                        "--orchestrator",
                        provider,
                        "--task",
                        "inspect",
                        option,
                        "high"
                    ])
                    .is_err()
                );
            }
            for effort in ["", "x high", "HIGH", "high\"", "high\\", "-high", "high\n"] {
                assert!(
                    parse(&[
                        "--orchestrator",
                        provider,
                        &format!("--reasoning-effort={effort}"),
                    ])
                    .is_err()
                );
            }
        }
        assert!(parse(&["--dispatch", "worker", "--reasoning-effort", "high"]).is_ok());
        for effort in ["auto", "xhigh", "max", "ultra"] {
            assert!(parse(&["--dispatch", "worker", "--reasoning-effort", effort]).is_ok());
        }
        assert!(parse(&["--dispatch", "worker", "--reasoning-effort", "unknown"]).is_err());
    }
}
