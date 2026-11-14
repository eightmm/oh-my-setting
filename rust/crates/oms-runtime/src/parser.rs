//! One parser shared by all consumers; command tails retain raw operating-system arguments.
use crate::{Result, error, require};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Envelope,
    Evidence,
    Next,
    Brief,
    Failure,
    Context,
    ContextInspect,
    Profile,
    Release,
    Capsule,
    Backend,
    Experiment,
    Benchmark,
    Doctor,
    Help,
    Version,
}
#[derive(Debug, Clone)]
pub struct Request {
    pub mode: Mode,
    pub action: String,
    pub subaction: String,
    pub repo: OsString,
    pub pretty: bool,
    pub positionals: Vec<OsString>,
    pub tail: Vec<OsString>,
    pub options: BTreeMap<String, Vec<OsString>>,
}
impl Request {
    pub fn has(&self, key: &str) -> bool {
        self.options.contains_key(key)
    }
    pub fn os(&self, key: &str) -> Option<&OsStr> {
        self.options.get(key)?.last().map(OsString::as_os_str)
    }
    pub fn text<'a>(&'a self, key: &str, default: &'a str) -> Result<&'a str> {
        self.os(key)
            .map(|s| {
                s.to_str()
                    .ok_or_else(|| error(format!("--{key} requires Unicode text")))
            })
            .unwrap_or(Ok(default))
    }
    pub fn all(&self, key: &str) -> Vec<OsString> {
        self.options.get(key).cloned().unwrap_or_default()
    }
    pub fn integer(&self, key: &str, default: i128) -> Result<i128> {
        match self.os(key) {
            None => Ok(default),
            Some(v) => {
                let raw = v.to_str().ok_or_else(|| error("invalid integer value"))?;
                let s = raw.trim();
                require(
                    !s.is_empty() && !s.starts_with('_') && !s.ends_with('_') && !s.contains("__"),
                    format!("invalid int value: {raw}"),
                )?;
                require(
                    regex::Regex::new(r"^[+-]?[0-9]+(?:_[0-9]+)*$")
                        .map_err(error)?
                        .is_match(s),
                    format!("invalid int value: {raw}"),
                )?;
                s.replace('_', "")
                    .parse()
                    .map_err(|_| error(format!("invalid int value: {raw}")))
            }
        }
    }
    pub fn float(&self, key: &str, default: f64) -> Result<f64> {
        match self.os(key) {
            None => Ok(default),
            Some(v) => v
                .to_str()
                .and_then(|s| s.trim().replace('_', "").parse().ok())
                .ok_or_else(|| error("invalid float value")),
        }
    }
    pub fn positional(&self, index: usize) -> Result<&OsStr> {
        self.positionals
            .get(index)
            .map(OsString::as_os_str)
            .ok_or_else(|| error("missing positional argument"))
    }
    pub fn child_read_only(&self) -> bool {
        match self.mode {
            Mode::Envelope => self.action == "show",
            Mode::Evidence => matches!(self.action.as_str(), "show" | "unbound" | "matrix"),
            Mode::Next | Mode::Brief | Mode::Failure | Mode::Doctor => true,
            Mode::ContextInspect => !self.has("session"),
            Mode::Context => false,
            Mode::Profile => {
                matches!(
                    self.action.as_str(),
                    "list" | "current" | "show" | "check" | "install-plan"
                ) || self.action == "install" && self.has("dry-run")
            }
            Mode::Release => {
                self.action == "status" || self.action == "resolve" && !self.has("fetch")
            }
            Mode::Capsule => matches!(self.action.as_str(), "verify" | "diff"),
            Mode::Backend => {
                matches!(self.action.as_str(), "describe" | "check")
                    || self.action == "target" && self.subaction == "list"
            }
            Mode::Experiment => {
                self.action == "template" && self.os("output").is_none_or(OsStr::is_empty)
                    || matches!(
                        self.action.as_str(),
                        "validate" | "compare" | "show" | "summarize" | "evaluate" | "sweep"
                    )
            }
            Mode::Benchmark => matches!(self.action.as_str(), "show" | "compare"),
            Mode::Help | Mode::Version => true,
        }
    }
}
struct Spec {
    flags: &'static str,
    values: &'static str,
    repeat: &'static str,
    required: &'static str,
    min: usize,
    max: usize,
    tail: bool,
}
fn spec(r: &Request) -> Spec {
    let (flags, values, repeat, required, min, max, tail) = match (
        r.mode,
        r.action.as_str(),
        r.subaction.as_str(),
    ) {
        (Mode::Envelope, _, _) => ("", "output", "", "", 0, 0, false),
        (Mode::Evidence, "matrix", _) => ("", "task limit format", "", "", 0, 0, false),
        (Mode::Evidence, "bind", _) => (
            "",
            "criterion ref status type note depends",
            "depends",
            "criterion ref status",
            0,
            0,
            false,
        ),
        (Mode::Evidence, "revoke", _) => ("", "binding reason", "", "binding", 0, 0, false),
        (Mode::Next, _, _) => ("explain", "task limit format", "", "", 0, 0, false),
        (Mode::Brief, _, _) => ("", "before limit format", "", "", 0, 0, false),
        (Mode::Failure, "classify", _) => ("", "exit code", "", "", 1, 1, false),
        (Mode::Context, _, _) => (
            "allow-external",
            "target file require max-bytes bundle manifest phase",
            "target file require",
            "",
            0,
            0,
            false,
        ),
        (Mode::ContextInspect, _, _) => (
            "injections timeline verify-current",
            "session-hash session compare limit format",
            "",
            "",
            0,
            1,
            false,
        ),
        (Mode::Profile, "show", _) => ("", "", "", "", 1, 1, false),
        (Mode::Profile, "check", _) => ("", "", "", "", 1, usize::MAX, false),
        (Mode::Profile, "apply", _) => ("allow-missing", "", "", "", 1, usize::MAX, false),
        (Mode::Profile, "install-plan", _) => {
            ("", "primary-provider", "", "", 1, usize::MAX, false)
        }
        (Mode::Profile, "install", _) => (
            "allow-missing upgrade dry-run",
            "primary-provider receipt",
            "",
            "",
            1,
            usize::MAX,
            false,
        ),
        (Mode::Release, "resolve", _) => ("fetch", "", "", "", 1, 1, false),
        (Mode::Release, "apply", _) => ("no-fetch tools", "", "", "", 1, 1, false),
        (Mode::Release, "promote", _) => (
            "",
            "commit version expected-manifest-digest",
            "",
            "commit version expected-manifest-digest",
            0,
            0,
            false,
        ),
        (Mode::Capsule, "export", _) => ("", "output", "", "", 0, 0, false),
        (Mode::Capsule, "verify" | "import", _) => ("", "", "", "", 1, 1, false),
        (Mode::Capsule, "diff", _) => ("", "", "", "", 2, 2, false),
        (Mode::Backend, "target", "add") => ("", "ssh-alias workdir", "", "ssh-alias", 1, 1, false),
        (Mode::Backend, "target", "remove") => ("", "", "", "", 1, 1, false),
        (Mode::Backend, "target", "status") => ("", "timeout", "", "", 1, 1, false),
        (Mode::Backend, "target", "sync") => ("", "timeout sha", "", "", 1, 1, false),
        (Mode::Backend, "describe", _) => ("", "image adapter", "", "", 1, 1, false),
        (Mode::Backend, "check", _) => ("", "image adapter target", "", "", 1, 1, false),
        (Mode::Backend, "run", _) => (
            "allow-network",
            "worktree image adapter target inherit-env timeout-seconds log receipt log-limit memory-mb cpus pids-limit tmpfs-mb",
            "inherit-env",
            "",
            1,
            1,
            true,
        ),
        (Mode::Experiment, "template", _) => ("", "output", "", "", 0, 0, false),
        (Mode::Experiment, "validate", _) => ("", "", "", "", 1, 1, false),
        (Mode::Experiment, "register", _) => ("", "id", "", "", 1, 1, false),
        (Mode::Experiment, "compare" | "evaluate", _) => ("", "", "", "", 2, 2, false),
        (Mode::Experiment, "show", _) => ("", "", "", "", 1, 1, false),
        (Mode::Experiment, "run", _) => (
            "allow-existing-metrics",
            "id arm seed metrics profile image adapter worktree timeout-seconds reason",
            "",
            "id arm seed metrics",
            0,
            0,
            true,
        ),
        (Mode::Experiment, "summarize", _) => ("", "id", "", "id", 0, 0, false),
        (Mode::Experiment, "sweep", "plan") => ("", "emit", "", "", 1, 1, false),
        (Mode::Experiment, "invariants", _) => (
            "",
            "profile image adapter timeout-seconds",
            "",
            "",
            1,
            1,
            false,
        ),
        (Mode::Experiment, "launch", _) => (
            "no-gate dry-run",
            "question hypothesis prediction baseline metric success change contract metrics file reason",
            "",
            "",
            0,
            0,
            true,
        ),
        (Mode::Benchmark, "record", _) => (
            "",
            "task-id status human-corrections escaped-defects reverted-lines false-refusals duplicate-work note",
            "",
            "task-id status",
            0,
            0,
            false,
        ),
        (Mode::Benchmark, "compare", _) => ("", "", "", "", 2, 2, false),
        (Mode::Doctor, _, _) => ("strict", "", "", "", 0, 0, false),
        _ => ("", "", "", "", 0, 0, false),
    };
    Spec {
        flags,
        values,
        repeat,
        required,
        min,
        max,
        tail,
    }
}
fn includes(list: &str, key: &str) -> bool {
    list.split_whitespace().any(|s| s == key)
}
fn choose<'a>(value: &'a OsStr, choices: &[&str]) -> Result<&'a str> {
    let v = value.to_str().ok_or_else(|| error("invalid choice"))?;
    require(
        choices.contains(&v),
        format!("invalid choice: {v} (choose from {})", choices.join(", ")),
    )?;
    Ok(v)
}
pub const COMMANDS: &str = "envelope evidence next brief failure context context-inspect profile release capsule backend experiment benchmark doctor";
pub fn parse(argv: &[OsString]) -> Result<Request> {
    let mut r = Request {
        mode: Mode::Help,
        action: String::new(),
        subaction: String::new(),
        repo: ".".into(),
        pretty: false,
        positionals: vec![],
        tail: vec![],
        options: BTreeMap::new(),
    };
    let mut selected = false;
    let mut action_needed = false;
    let mut sub_needed = false;
    let mut ended = false;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        let raw = arg.as_bytes();
        if !ended && raw == b"--" {
            ended = true;
            i += 1;
            if selected && spec(&r).tail {
                r.tail.extend_from_slice(&argv[i..]);
                break;
            }
            continue;
        }
        if !ended && (raw == b"--help" || raw == b"-h") {
            r.options.insert("help".into(), vec![]);
            return Ok(r);
        }
        if !ended && raw == b"--version" && !selected {
            r.mode = Mode::Version;
            return Ok(r);
        }
        if !ended && raw.starts_with(b"--") {
            let split = raw.iter().position(|b| *b == b'=');
            let requested =
                std::str::from_utf8(&raw[2..split.unwrap_or(raw.len())]).map_err(error)?;
            let explicit = split.map(|j| OsString::from_vec(raw[j + 1..].to_vec()));
            let s = spec(&r);
            let mut available = vec!["help", "repo", "pretty"];
            if !selected {
                available.push("version");
            }
            if selected && !action_needed && !sub_needed {
                available.extend(s.flags.split_whitespace());
                available.extend(s.values.split_whitespace());
            }
            let name = if available.contains(&requested) {
                requested
            } else {
                let matches = available
                    .iter()
                    .filter(|v| v.starts_with(requested))
                    .copied()
                    .collect::<Vec<_>>();
                require(
                    matches.len() <= 1,
                    format!(
                        "ambiguous option: --{requested} could match {}",
                        matches
                            .iter()
                            .map(|s| format!("--{s}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )?;
                matches.first().copied().unwrap_or(requested)
            };
            if matches!(name, "help" | "version") {
                require(
                    explicit.is_none(),
                    format!("argument --{name}: ignored explicit argument"),
                )?;
                if name == "help" {
                    r.options.insert("help".into(), vec![]);
                } else {
                    r.mode = Mode::Version;
                }
                return Ok(r);
            }
            let shared = matches!(name, "repo" | "pretty");
            let flag = name == "pretty"
                || selected && !action_needed && !sub_needed && includes(s.flags, name);
            let takes = name == "repo"
                || selected && !action_needed && !sub_needed && includes(s.values, name);
            require(
                shared || flag || takes,
                format!("unrecognized arguments: {}", arg.to_string_lossy()),
            )?;
            if flag {
                require(
                    explicit.is_none(),
                    format!("argument --{name}: ignored explicit argument"),
                )?;
                if name == "pretty" {
                    r.pretty = true
                } else {
                    r.options.insert(name.into(), vec![]);
                }
            } else {
                let value = match explicit {
                    Some(v) => v,
                    None => {
                        i += 1;
                        let v = argv.get(i).ok_or_else(|| {
                            error(format!("argument --{name}: expected one argument"))
                        })?;
                        require(
                            !v.as_bytes().starts_with(b"--"),
                            format!("argument --{name}: expected one argument"),
                        )?;
                        v.clone()
                    }
                };
                if name == "repo" {
                    r.repo = value
                } else if includes(s.repeat, name) {
                    r.options.entry(name.into()).or_default().push(value)
                } else {
                    r.options.insert(name.into(), vec![value]);
                }
            }
            i += 1;
            continue;
        }
        if !selected {
            let name = choose(arg, &COMMANDS.split_whitespace().collect::<Vec<_>>())?;
            r.mode = match name {
                "envelope" => Mode::Envelope,
                "evidence" => Mode::Evidence,
                "next" => Mode::Next,
                "brief" => Mode::Brief,
                "failure" => Mode::Failure,
                "context" => Mode::Context,
                "context-inspect" => Mode::ContextInspect,
                "profile" => Mode::Profile,
                "release" => Mode::Release,
                "capsule" => Mode::Capsule,
                "backend" => Mode::Backend,
                "experiment" => Mode::Experiment,
                "benchmark" => Mode::Benchmark,
                _ => Mode::Doctor,
            };
            selected = true;
            action_needed = matches!(
                r.mode,
                Mode::Evidence
                    | Mode::Failure
                    | Mode::Profile
                    | Mode::Release
                    | Mode::Capsule
                    | Mode::Backend
                    | Mode::Experiment
                    | Mode::Benchmark
            );
            if r.mode == Mode::Envelope {
                r.action = "show".into()
            };
        } else if action_needed {
            let choices = match r.mode {
                Mode::Evidence => "show unbound matrix bind revoke",
                Mode::Failure => "classify catalog",
                Mode::Profile => "list current show check apply install-plan install",
                Mode::Release => "status resolve apply promote",
                Mode::Capsule => "export verify import diff",
                Mode::Backend => "target describe check run",
                Mode::Experiment => {
                    "template validate compare register show run launch sweep summarize evaluate invariants"
                }
                Mode::Benchmark => "show snapshot record compare",
                _ => "",
            };
            r.action = choose(arg, &choices.split_whitespace().collect::<Vec<_>>())?.into();
            action_needed = false;
            sub_needed = r.mode == Mode::Backend && r.action == "target"
                || r.mode == Mode::Experiment && r.action == "sweep";
        } else if sub_needed {
            let choices = if r.mode == Mode::Backend {
                vec!["add", "remove", "list", "status", "sync"]
            } else {
                vec!["plan"]
            };
            r.subaction = choose(arg, &choices)?.into();
            sub_needed = false;
        } else if r.mode == Mode::Envelope
            && r.positionals.is_empty()
            && matches!(arg.to_str(), Some("show" | "write"))
        {
            r.action = arg.to_str().unwrap().into();
            r.positionals.push(arg.clone());
        } else {
            let s = spec(&r);
            if s.tail && r.positionals.len() >= s.max {
                r.tail.extend_from_slice(&argv[i..]);
                break;
            }
            require(
                ended
                    || !raw.starts_with(b"-")
                    || raw == b"-"
                    || arg.to_str().is_some_and(|s| s.parse::<f64>().is_ok()),
                format!("unrecognized arguments: {}", arg.to_string_lossy()),
            )?;
            r.positionals.push(arg.clone());
        }
        i += 1;
    }
    require(selected, "the following arguments are required: command")?;
    require(
        !action_needed,
        "the following arguments are required: action",
    )?;
    require(!sub_needed, "the following arguments are required: action")?;
    if r.mode == Mode::Envelope {
        require(
            r.positionals.len() <= 1
                && r.positionals
                    .first()
                    .is_none_or(|v| matches!(v.to_str(), Some("show" | "write"))),
            "invalid envelope action",
        )?;
        r.positionals.clear();
    }
    let s = spec(&r);
    require(
        r.positionals.len() >= s.min,
        "the following arguments are required: positional arguments",
    )?;
    require(
        r.positionals.len() <= s.max,
        "unrecognized positional arguments",
    )?;
    for key in s.required.split_whitespace() {
        require(
            r.has(key),
            format!("the following arguments are required: --{key}"),
        )?;
    }
    if s.tail && !(r.mode == Mode::Experiment && r.action == "launch") {
        require(!r.tail.is_empty(), "command after -- is required")?;
    }
    require(
        !(r.has("adapter") && r.has("target")),
        "argument --target: not allowed with argument --adapter",
    )?;
    for (key, choices) in [
        ("format", vec!["json", "text"]),
        (
            "phase",
            vec![
                "orientation",
                "implementation",
                "verification",
                "review",
                "research",
            ],
        ),
        ("profile", vec!["trusted-local", "isolated", "remote"]),
        ("arm", vec!["baseline", "treatment"]),
        ("emit", vec!["", "tsv", "hydra-overrides", "sbatch-array"]),
    ] {
        if let Some(v) = r.os(key) {
            choose(v, &choices)?;
        }
    }
    for key in [
        "limit",
        "max-bytes",
        "timeout",
        "timeout-seconds",
        "log-limit",
        "memory-mb",
        "pids-limit",
        "tmpfs-mb",
        "seed",
        "exit",
        "human-corrections",
        "escaped-defects",
        "reverted-lines",
        "false-refusals",
        "duplicate-work",
    ] {
        if r.has(key) {
            r.integer(key, 0)?;
        }
    }
    if r.has("cpus") {
        r.float("cpus", 2.0)?;
    }
    if r.mode == Mode::Backend && r.action != "target" {
        choose(r.positional(0)?, &["trusted-local", "isolated", "remote"])?;
    }
    if r.mode == Mode::Release && matches!(r.action.as_str(), "resolve" | "apply") {
        choose(r.positional(0)?, &["stable", "edge"])?;
    }
    if r.mode == Mode::Evidence && r.action == "bind" {
        choose(
            r.os("status").unwrap(),
            &[
                "verified",
                "failed",
                "inconclusive",
                "skipped_with_reason",
                "stale",
            ],
        )?;
    }
    if r.mode == Mode::Benchmark && r.action == "record" {
        choose(
            r.os("status").unwrap(),
            &["verified", "failed", "partial", "blocked"],
        )?;
    }
    if r.mode == Mode::Profile {
        for name in &r.positionals {
            choose(
                name,
                &[
                    "core",
                    "council",
                    "github",
                    "notion",
                    "research",
                    "hpc",
                    "container",
                    "remote",
                    "full",
                ],
            )?;
        }
        if let Some(provider) = r.os("primary-provider") {
            if r.action == "install" {
                choose(provider, &["auto", "codex", "claude", "agy"])?;
            } else {
                choose(provider, &["codex", "claude", "agy"])?;
            }
        }
    }
    Ok(r)
}
pub fn help(request: &Request) -> String {
    format!(
        "usage: oms runtime [--repo REPO] [--pretty] [--version] COMMAND ...\n\nTyped semantic core over the existing hardened OMS execution plane.\n\ncommands: {COMMANDS}\n\nselected: {:?} {} {}\noptions: --help --repo REPO --pretty {} {}\n",
        request.mode,
        request.action,
        request.subaction,
        spec(request)
            .flags
            .split_whitespace()
            .map(|s| format!("--{s}"))
            .collect::<Vec<_>>()
            .join(" "),
        spec(request)
            .values
            .split_whitespace()
            .map(|s| format!("--{s} VALUE"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}
