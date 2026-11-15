//! Native provider hook adapters. Arguments exclude the command name; source
//! is the trusted installation root, never a path recovered from hook input.
#![forbid(unsafe_code)]

// Each literal call site owns one compiled expression for the process lifetime.
macro_rules! cached_regex {
    ($pattern:expr $(,)?) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| regex::Regex::new($pattern).expect("static regex"))
    }};
}

pub mod assignment;
mod bridge;
mod common;
mod config;
mod guards;
mod handoff;
mod lifecycle;
pub mod notify;
mod push;
mod routing;
mod shell;
mod statusline;
pub mod surfaces;
mod syntax;

use std::{ffi::OsString, io::Read, path::Path, sync::atomic::AtomicBool};
pub type Output = (i32, Vec<u8>, Vec<u8>);
pub use oms_command_telemetry::run as telemetry_report;
pub const HOOKS: &[&str] = &[
    "command-guard-hook",
    "syntax-guard-hook",
    "git-move-guard-hook",
    "fail-ledger-hook",
    "resume-hook",
    "telemetry-hook",
    "precompact-handoff",
    "turn-guard",
    "skill-router",
    "pre-push-check",
    "harness-hook",
    "claude-statusline",
    "claude-subagent-statusline",
    "codex-hook-bridge",
    "codex-hud-config",
    "codex-usage-config",
    "claude-app-notify",
    "codex-app-notify",
    "codex-turn-notify",
];

/// Host adapters for detached background commands and hook failure recording.
pub trait HostServices: Send + Sync {
    fn supports_detach(&self) -> bool {
        false
    }

    fn detach_command(&self, tool: &str, _args: &[OsString], _cwd: &Path) -> Result<(), String> {
        Err(format!("detached launch for '{tool}' is unavailable"))
    }

    fn record_hook_failure(
        &self,
        _repo: &Path,
        _cmd: &str,
        _exit: i32,
        _summary: &str,
        _next: &str,
    ) {
    }
}

/// Fail-closed default host adapter when no packaged host supplies `HostServices`.
pub struct NoHostServices;

impl HostServices for NoHostServices {}

/// Pure classification, including invalid arguments to a recognized command.
pub fn owns_hook(name: &str, _: &[OsString]) -> bool {
    HOOKS.contains(&name)
}

/// Read bounded provider input once. No process-global cwd, environment or
/// signal state is changed. Hook failures follow the original event contract;
/// maintenance/configuration commands retain their explicit error exits.
pub fn run_with_source(
    name: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
) -> Output {
    run_with_source_and_services(name, args, cwd, source, interrupted, &NoHostServices)
}

/// Read bounded provider input once using explicit host adapters.
pub fn run_with_source_and_services(
    name: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
    host: &dyn HostServices,
) -> Output {
    if !owns_hook(name, args) {
        return (75, vec![], vec![]);
    }
    if [
        "codex-hud-config",
        "codex-usage-config",
        "codex-app-notify",
        "claude-app-notify",
        "codex-turn-notify",
    ]
    .contains(&name)
        || name == "command-guard-hook" && args.first().is_some_and(|a| a == "--audit")
    {
        return run_with_services(name, args, &[], cwd, source, interrupted, host);
    }
    let mut input = Vec::new();
    if std::io::stdin()
        .lock()
        .take((common::INPUT_MAX + 1) as u64)
        .read_to_end(&mut input)
        .is_err()
    {
        return common::input_error(name);
    }
    run_with_services(name, args, &input, cwd, source, interrupted, host)
}

/// Execute from supplied bytes, with no implicit stdin read. Unknown names
/// return 75 so an embedding dispatcher can select another command family.
pub fn run_hook(
    name: &str,
    args: &[OsString],
    stdin: &[u8],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
) -> Output {
    run_with_services(name, args, stdin, cwd, source, interrupted, &NoHostServices)
}

/// Execute from supplied bytes using explicit host adapters.
pub fn run_with_services(
    name: &str,
    args: &[OsString],
    stdin: &[u8],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
    host: &dyn HostServices,
) -> Output {
    if !owns_hook(name, args) {
        return (75, vec![], vec![]);
    }
    if stdin.len() > common::INPUT_MAX {
        return common::input_error(name);
    }
    let snapshot = common::Snapshot::default();
    let ctx = common::Context {
        snapshot: &snapshot,
        cwd,
        source,
        interrupted,
        provider: None,
        host,
    };
    dispatch(name, args, stdin, &ctx)
}
/// Pi extension safe points. Pi enrolls as Codex; tool events only refresh
/// presence so mail output is never discarded before the agent sees it.
fn pi_hook(tool: &str, stdin: &[u8], ctx: &common::Context<'_>) -> Output {
    let Some(v) = common::document(stdin) else {
        return common::empty();
    };
    if tool == "panel-hook" && !common::panel() {
        return common::empty();
    }
    let Some(repo) = common::repo(&v, ctx) else {
        return common::empty();
    };
    let ctx = &common::Context {
        cwd: ctx.cwd,
        source: ctx.source,
        interrupted: ctx.interrupted,
        provider: Some("codex"),
        snapshot: ctx.snapshot,
        host: ctx.host,
    };
    let mut text = String::new();
    match tool {
        "panel-hook" => {
            lifecycle::activity(&v, &repo, ctx);
            if common::event(&v) == "UserPromptSubmit" {
                for hint in [
                    routing::panel_hint(&v, &repo, ctx),
                    routing::collaboration(&v, &repo, ctx),
                ] {
                    if !hint.is_empty() {
                        text.push_str(&hint);
                        text.push('\n');
                    }
                }
            }
        }
        "relay" => {
            lifecycle::activity(&v, &repo, ctx);
            lifecycle::relay(&v, &repo, ctx);
        }
        _ if common::adopted(&repo) => {
            let hint = routing::relay_hint(&v, &repo, ctx);
            if !hint.is_empty() {
                text = format!("{hint}\n");
            }
            let source = common::text(&v, "source");
            if common::panel()
                && common::event(&v) == "SessionStart"
                && ["startup", "compact", "clear", "resume"].contains(&source)
            {
                // Context loss re-arms the control-tower hint for the next prompt.
                let _ = common::write(
                    &common::state_path(&repo, &v, ".resume"),
                    &serde_json::json!({"schema":1,"updated_at":ctx.snapshot.now(),"source":source}),
                );
                let _ = std::fs::remove_file(common::state_path(&repo, &v, ".hint"));
                if ["compact", "clear"].contains(&source) {
                    let line = routing::panel_resume_line(&v, &repo, ctx, source);
                    if !line.is_empty() {
                        text.push_str(&line);
                        text.push('\n');
                    }
                }
            }
        }
        _ => {}
    }
    if text.is_empty() {
        common::empty()
    } else {
        if common::adopted(&repo) && tool != "relay" {
            let kind = if tool == "panel-hook" {
                "panel"
            } else {
                "session-start"
            };
            common::injection(ctx, &repo, &v, kind, text.as_bytes());
        }
        common::out(text)
    }
}
fn dispatch(name: &str, args: &[OsString], stdin: &[u8], ctx: &common::Context<'_>) -> Output {
    match name {
        "command-guard-hook" => guards::command(args, stdin, ctx),
        "git-move-guard-hook" => guards::git_move(stdin, ctx),
        "syntax-guard-hook" => syntax::run(stdin, ctx),
        "codex-hud-config" | "codex-usage-config" => config::run(name, args, ctx),
        "codex-hook-bridge" => bridge::run(args, stdin, ctx),
        "claude-statusline" | "claude-subagent-statusline" => statusline::run(name, stdin, ctx),
        "codex-app-notify" => notify::run(args, ctx),
        "claude-app-notify" => notify::claude(args, ctx),
        "codex-turn-notify" => notify::turn(args, ctx),
        "pre-push-check" => push::run(args, stdin, ctx),
        "harness-hook" => {
            let Some(tool) = args.first().and_then(|v| v.to_str()) else {
                return common::empty();
            };
            if ["panel-hook", "relay", "relay-hint"].contains(&tool) {
                return pi_hook(tool, stdin, ctx);
            }
            if ![
                "skill-router",
                "turn-guard",
                "precompact-handoff",
                "resume-hook",
                "syntax-guard-hook",
                "telemetry-hook",
            ]
            .contains(&tool)
            {
                return common::empty();
            }
            let selected = common::Context {
                cwd: ctx.cwd,
                source: ctx.source,
                interrupted: ctx.interrupted,
                provider: Some("codex"),
                snapshot: ctx.snapshot,
                host: ctx.host,
            };
            let mut out = dispatch(tool, &args[1..], stdin, &selected);
            if tool == "resume-hook" && !out.1.is_empty() {
                out.1 = common::json_line(&serde_json::json!({"hookSpecificOutput": {
                    "hookEventName": "SessionStart", "additionalContext": String::from_utf8_lossy(&out.1)
                }}));
            }
            out.0 = 0;
            out
        }
        "skill-router" => routing::run(stdin, ctx),
        _ => lifecycle::run(name, args, stdin, ctx),
    }
}
