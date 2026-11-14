use crate::{App, CommandResult, Result};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

pub(crate) struct Args {
    pub repo: PathBuf,
    pub command: String,
    values: BTreeMap<String, Vec<OsString>>,
}
impl Args {
    pub fn flag(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }
    pub fn string(&self, key: &str) -> String {
        self.values
            .get(key)
            .and_then(|v| v.last())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| default(&self.command, key).into())
    }
    pub fn all(&self, key: &str) -> Vec<String> {
        self.values
            .get(key)
            .map(|v| v.iter().map(|s| s.to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    }
    pub fn number(&self, key: &str) -> Result<i64> {
        self.integer(key)?.py().parse().map_err(|_| {
            format!(
                "argument --{key}: invalid int value: {:?}",
                self.string(key)
            )
        })
    }
    pub fn integer(&self, key: &str) -> Result<crate::value::V> {
        let raw = self.string(key);
        crate::value::parse_integer(&raw).map_err(|_| {
            format!(
                "argument --{key}: invalid int value: {}",
                crate::value::V::from(raw.as_ref()).repr()
            )
        })
    }

    pub fn float(&self, key: &str) -> Result<f64> {
        self.string(key).trim().parse().map_err(|_| {
            format!(
                "argument --{key}: invalid float value: {:?}",
                self.string(key)
            )
        })
    }
}
fn default(command: &str, key: &str) -> &'static str {
    match key {
        "provider" => "local",
        "then-actor" => "agent-events",
        "actor-kind" | "then-actor-kind" => "owner",
        "actor" if command == "heartbeat" => "attempt-runner",
        "actor" if command == "usage" => "provider-telemetry",
        "actor" => "agent-events",
        "limit" => "0",
        "timeout" => "-1",
        "interval" => "0.5",
        "days" => "14",
        "stale-seconds" | "older-than-seconds" => "300",
        "expires-in" => "3600",
        "profile" => "trusted-local",
        _ => "",
    }
}
fn options(app: App, command: &str) -> Option<(&'static str, &'static str, &'static str)> {
    // valued, flags, required; repeatability is limited to ref and then.
    Some(match (app, command) {
        (App::Events, "start") => (
            "provider tool task-id run-id parent-attempt-id ref max-wall-seconds max-tokens max-cost-microusd idempotency-key then then-actor then-actor-kind",
            "",
            "tool",
        ),
        (App::Events, "transition") => (
            "attempt state expected-state expected-seq ref reason-code actor-kind actor idempotency-key then",
            "",
            "attempt state",
        ),
        (App::Events, "recover-verify-heartbeat") => {
            ("attempt expected-sequence", "", "attempt expected-sequence")
        }
        (App::Events, "heartbeat") => ("attempt actor idempotency-key", "", "attempt"),
        (App::Events, "usage") => (
            "attempt tokens duration-ms cost-microusd ref actor idempotency-key",
            "",
            "attempt",
        ),
        (App::Events, "show") => ("attempt transcript", "json bundle", "attempt"),
        (App::Events, "list") => ("state limit", "active json", ""),
        (App::Events, "resume") => ("attempt", "", "attempt"),
        (App::Events, "follow") => ("after-event limit timeout interval", "", ""),
        (App::Events, "reconcile") => ("stale-seconds", "apply json", ""),
        (App::Events, "compact") => ("days", "apply", ""),
        (_, "validate") => ("", "", ""),
        (App::Approvals, "request") => (
            "attempt action object-id summary expires-in base-sha patch-sha task-id lease-id profile parameters-json",
            "",
            "action object-id summary",
        ),
        (App::Approvals, "decide") => (
            "approval decision expected-version actor changes-json",
            "",
            "approval decision expected-version actor",
        ),
        (App::Approvals, "begin-consume") => (
            "approval token expected-version consumer",
            "",
            "approval token expected-version consumer",
        ),
        (App::Approvals, "finish-consume") => (
            "approval expected-version result consumer",
            "",
            "approval expected-version result consumer",
        ),
        (App::Approvals, "expire") => ("", "apply json", ""),
        (App::Approvals, "reconcile") => ("older-than-seconds", "apply json", ""),
        (App::Approvals, "show") => ("approval", "json", "approval"),
        (App::Approvals, "list") => ("state effective-state", "pending json", ""),
        (App::Approvals, "path") => ("", "", ""),
        _ => return None,
    })
}
fn resolve<'a>(raw: &str, candidates: &'a [&'a str]) -> Result<&'a str> {
    if let Some(exact) = candidates.iter().find(|s| **s == raw) {
        return Ok(exact);
    }
    let matches: Vec<_> = candidates.iter().filter(|s| s.starts_with(raw)).collect();
    match matches.as_slice() {
        [name] => Ok(name),
        [] => Err(format!("unrecognized arguments: --{raw}")),
        _ => Err(format!(
            "ambiguous option: --{raw} could match {}",
            matches
                .iter()
                .map(|s| format!("--{s}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}
pub fn parse(app: App, raw: &[OsString]) -> std::result::Result<Args, CommandResult> {
    let program = if app == App::Events {
        "agent-events.sh"
    } else {
        "approval-inbox.sh"
    };
    let mut command = String::new();
    let mut values: BTreeMap<String, Vec<OsString>> = BTreeMap::new();
    let mut repo = PathBuf::from(".");
    let mut i = 0;
    let result = (|| -> Result<bool> {
        while i < raw.len() {
            let arg = raw[i].to_string_lossy();
            let inline_os = raw[i]
                .as_bytes()
                .iter()
                .position(|b| *b == b'=')
                .map(|at| OsString::from_vec(raw[i].as_bytes()[at + 1..].to_vec()));
            if arg == "-h" || arg == "--help" {
                return Ok(true);
            }
            if command.is_empty() {
                if arg.starts_with("--") {
                    let (name, inline) = arg[2..]
                        .split_once('=')
                        .map_or((&arg[2..], None), |(a, b)| (a, Some(b)));
                    let option = resolve(name, &["repo", "help"])?;
                    if option == "help" {
                        if inline.is_some() {
                            return Err("argument -h/--help: ignored explicit argument".into());
                        }
                        return Ok(true);
                    }
                    let value = if let Some(value) = inline_os.clone() {
                        value
                    } else {
                        i += 1;
                        let value = raw
                            .get(i)
                            .cloned()
                            .ok_or("argument --repo: expected one argument")?;
                        if looks_optional(&value) {
                            return Err("argument --repo: expected one argument".into());
                        }
                        value
                    };
                    repo = value.into();
                } else {
                    command = arg.into_owned();
                    options(app, &command)
                        .ok_or_else(|| invalid_choice("command", &command, commands(app)))?;
                }
            } else {
                let (valued, flags, _) = options(app, &command).ok_or("invalid command")?;
                let candidates: Vec<_> = valued
                    .split_whitespace()
                    .chain(flags.split_whitespace())
                    .chain(["help"])
                    .collect();
                if !arg.starts_with("--") {
                    return Err(format!("unrecognized arguments: {arg}"));
                }
                let (name, inline) = arg[2..]
                    .split_once('=')
                    .map_or((&arg[2..], None), |(a, b)| (a, Some(b)));
                let option = resolve(name, &candidates)?;
                if option == "help" {
                    if inline.is_some() {
                        return Err("argument -h/--help: ignored explicit argument".into());
                    }
                    return Ok(true);
                }
                if flags.split_whitespace().any(|s| s == option) {
                    if inline.is_some() {
                        return Err(format!("argument --{option}: ignored explicit argument"));
                    }
                    values.insert(option.into(), Vec::new());
                } else {
                    let value = if let Some(v) = inline_os.clone() {
                        v
                    } else {
                        i += 1;
                        let v = raw
                            .get(i)
                            .cloned()
                            .ok_or_else(|| format!("argument --{option}: expected one argument"))?;
                        if looks_optional(&v) {
                            return Err(format!("argument --{option}: expected one argument"));
                        }
                        v
                    };
                    validate_option(app, &command, option, &value)?;
                    if matches!(option, "ref" | "then") {
                        values.entry(option.into()).or_default().push(value);
                    } else {
                        values.insert(option.into(), vec![value]);
                    }
                }
            }
            i += 1;
        }
        if command.is_empty() {
            return Err("the following arguments are required: command".into());
        }
        let (_, _, required) = options(app, &command).ok_or("invalid command")?;
        let missing: Vec<_> = required
            .split_whitespace()
            .filter(|key| !values.contains_key(*key))
            .map(|s| format!("--{s}"))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "the following arguments are required: {}",
                missing.join(", ")
            ));
        }
        Ok(false)
    })();
    match result {
        Ok(true) => Err((0, help(app, &command).into_bytes(), Vec::new())),
        Ok(false) => {
            let args = Args {
                repo,
                command,
                values,
            };
            Ok(args)
        }
        Err(error) => Err((
            2,
            Vec::new(),
            format!(
                "{}{program}{}: error: {error}\n",
                crate::help::text(app, &command).1,
                if command.is_empty() || options(app, &command).is_none() {
                    String::new()
                } else {
                    format!(" {command}")
                }
            )
            .into_bytes(),
        )),
    }
}
fn help(app: App, command: &str) -> String {
    crate::help::text(app, command).0.to_owned()
}

fn looks_optional(raw: &std::ffi::OsStr) -> bool {
    let bytes = raw.as_bytes();
    if !bytes.starts_with(b"-") || bytes.len() == 1 || bytes.contains(&b' ') {
        return false;
    }
    static NEGATIVE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let expression = NEGATIVE
        .get_or_init(|| regex::Regex::new(r"^-\d+$|^-\d*\.\d+$").expect("constant number pattern"));
    !raw.to_str()
        .is_some_and(|s| expression.is_match(s.strip_suffix('\n').unwrap_or(s)))
}
fn validate_option(app: App, command: &str, key: &str, value: &std::ffi::OsStr) -> Result<()> {
    let raw = value
        .to_str()
        .ok_or_else(|| format!("argument --{key}: value is not valid Unicode"))?;
    if matches!(
        key,
        "max-wall-seconds"
            | "max-tokens"
            | "max-cost-microusd"
            | "expected-seq"
            | "expected-sequence"
            | "tokens"
            | "duration-ms"
            | "cost-microusd"
            | "limit"
            | "stale-seconds"
            | "days"
            | "expires-in"
            | "expected-version"
            | "older-than-seconds"
    ) {
        crate::value::parse_integer(&raw).map_err(|_| {
            format!(
                "argument --{key}: invalid int value: {}",
                crate::value::V::from(raw.as_ref()).repr()
            )
        })?;
    }
    if matches!(key, "timeout" | "interval") {
        raw.trim().parse::<f64>().map_err(|_| {
            format!(
                "argument --{key}: invalid float value: {}",
                crate::value::V::from(raw.as_ref()).repr()
            )
        })?;
    }
    let choices: Option<&[&str]> = match key {
        "decision" => Some(&["approve", "deny", "reject"]),
        "result" => Some(&["consumed", "failed"]),
        "state" if command == "transition" => None,
        "expected-state" => Some(EVENT_STATES),
        "state" | "effective-state" if app == App::Events => Some(EVENT_STATES),
        "state" | "effective-state" => Some(APPROVAL_STATES),
        _ => None,
    };
    if let Some(choices) = choices
        && !choices.contains(&raw)
    {
        return Err(invalid_choice(&format!("--{key}"), raw, choices));
    }
    Ok(())
}

const EVENT_STATES: &[&str] = &[
    "abandoned",
    "blocked",
    "cancelled",
    "done",
    "failed",
    "queued",
    "review",
    "starting",
    "timed_out",
    "verifying",
    "waiting_approval",
    "waiting_input",
    "working",
];
const APPROVAL_STATES: &[&str] = &[
    "approved",
    "consumed",
    "consuming",
    "denied",
    "expired",
    "failed",
    "interrupted",
    "requested",
];
fn commands(app: App) -> &'static [&'static str] {
    match app {
        App::Events => &[
            "start",
            "transition",
            "recover-verify-heartbeat",
            "heartbeat",
            "usage",
            "show",
            "list",
            "resume",
            "follow",
            "reconcile",
            "validate",
            "compact",
        ],
        App::Approvals => &[
            "request",
            "decide",
            "begin-consume",
            "finish-consume",
            "expire",
            "reconcile",
            "show",
            "list",
            "validate",
            "path",
        ],
    }
}
fn invalid_choice(key: &str, value: &str, choices: &[&str]) -> String {
    let value = crate::value::V::from(value).repr();
    let choices = choices
        .iter()
        .map(|v| crate::value::V::from(*v).repr())
        .collect::<Vec<_>>()
        .join(", ");
    format!("argument {key}: invalid choice: {value} (choose from {choices})")
}
