use crate::{Error, Result, require};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

const FLAGS: &[&str] = &[
    "help",
    "reopen",
    "providers",
    "health",
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
    "archived",
];
const VALUES: &[&str] = &[
    "repo",
    "layout",
    "host",
    "room",
    "launch",
    "spawn-main",
    "orchestrator",
    "focus-orchestrator",
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
            ("host", &["auto", "inline", "tmux", "herdr"][..]),
            (
                "view",
                &["auto", "summary", "compact", "detail", "graph", "tree"][..],
            ),
            ("reasoning-effort", &["low", "medium", "high"][..]),
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
        self.choice("dispatch", oms_panel_core::routing::ROLES)?;
        self.choice("workload", oms_panel_core::routing::TIERS)?;
        for key in ["launch", "spawn-main", "swap-main"] {
            if let Some(raw) = self.get(key) {
                crate::native_panel::normalize_harness(raw)?;
            }
        }
        for key in ["orchestrator", "focus-orchestrator"] {
            self.choice(key, &crate::orchestrator::PROVIDERS)?;
        }
        let modes = [
            "watch",
            "launch",
            "spawn-main",
            "orchestrator",
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
            &["dispatch", "continue", "recommend"],
        )?;
        self.needs(
            &[
                "alias",
                "main",
                "to",
                "account",
                "brief-file",
                "from-task",
                "purpose",
                "access",
                "no-web",
            ],
            &["dispatch", "continue"],
        )?;
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
        self.needs(&["export", "page", "participant"], &["results"])?;
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
            ],
        )?;
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
            self.integer("page", 1, 1, 1000)?;
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
                !v.is_empty()
                    && v.len() <= 160
                    && !v.starts_with('-')
                    && v.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_./:-".contains(&c)),
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

pub fn usage() -> String {
    let mut text = String::from(
        "usage: oms panel [terminal ACTION | job ACTION] [OPTIONS]\n\nOMS terminal control panel with registered CLI peers.\n\nInteractive startup reuses an existing Main or offers native Claude, Codex and Agy. Enter opens the board alone. Use --launch codex|claude|agy for an explicit Main, or --watch for the board.\n\n",
    );
    text.push_str("Provider sessions: --orchestrator codex|claude|antigravity [--task TEXT]\nFocus a provider session: --focus-orchestrator PROVIDER\n\n");
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
    use super::Options;

    #[test]
    fn orchestrator_actions_cannot_resume_unbound_vendor_sessions() {
        let parse = |args: &[&str]| Options::parse(&crate::commands::strings(args));
        let uuid = "00112233-4455-4677-8899-aabbccddeeff";
        for provider in crate::orchestrator::PROVIDERS {
            assert!(parse(&["--orchestrator", provider, "--resume", uuid]).is_err());
            assert!(parse(&["--focus-orchestrator", provider, "--resume", uuid]).is_err());
        }
        assert!(parse(&["--launch", "codex", "--resume", "legacy-session"]).is_ok());
        for args in [
            vec![],
            vec!["--repo", "."],
            vec!["--layout", "inline"],
            vec!["--host", "tmux", "--view", "graph"],
        ] {
            assert!(parse(&args).unwrap().selects_main(), "{args:?}");
        }
        for args in [
            vec!["--json"],
            vec!["--watch"],
            vec!["--dry-run"],
            vec!["--launch", "claude"],
            vec!["--spawn-main", "codex"],
            vec!["--orchestrator", "claude"],
            vec!["--providers"],
        ] {
            assert!(!parse(&args).unwrap().selects_main(), "{args:?}");
        }
    }
}
