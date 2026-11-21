use crate::common;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Instant,
};

const FLAG: &str = "OMS_CODEX_HOOK_TRACE";

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
enum Event {
    UserPromptSubmit,
    Stop,
    PreCompact,
    SessionStart,
    PostToolUse,
    SessionEnd,
    #[serde(other)]
    Unknown,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::Stop => "Stop",
            Self::PreCompact => "PreCompact",
            Self::SessionStart => "SessionStart",
            Self::PostToolUse => "PostToolUse",
            Self::SessionEnd => "SessionEnd",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Deserialize)]
struct Header {
    #[serde(alias = "hookEventName")]
    hook_event_name: Option<Event>,
}

pub(crate) struct Callback {
    pub(crate) handler: &'static str,
    events: &'static [Event],
}

pub(crate) fn callback(tool: &str) -> Option<&'static Callback> {
    use Event::*;
    static CALLBACKS: &[Callback] = &[
        Callback {
            handler: "skill-router",
            events: &[UserPromptSubmit],
        },
        Callback {
            handler: "turn-guard",
            events: &[Stop],
        },
        Callback {
            handler: "precompact-handoff",
            events: &[PreCompact],
        },
        Callback {
            handler: "resume-hook",
            events: &[SessionStart],
        },
        Callback {
            handler: "syntax-guard-hook",
            events: &[PostToolUse],
        },
        Callback {
            handler: "telemetry-hook",
            events: &[SessionStart, SessionEnd],
        },
    ];
    CALLBACKS.iter().find(|callback| callback.handler == tool)
}

pub(crate) struct Trace {
    path: PathBuf,
    row: Value,
    started: Instant,
}

pub(crate) enum Rejection {
    Unreadable,
    Oversize,
    RuntimeUnavailable,
}

impl Trace {
    pub(crate) fn receive(name: &str, args: &[OsString], cwd: &Path) -> Option<Self> {
        if name != "harness-hook" || !std::env::var_os(FLAG).is_some_and(|value| value == "1") {
            return None;
        }
        let callback = callback(args.first()?.to_str()?)?;
        let repo = common::root(Path::new("."), cwd).filter(|repo| common::adopted(repo))?;
        let path = repo.join(".oms/hooks/codex-trace.jsonl");
        let row = json!({
            "schema": 1,
            "phase": "received",
            "handler": callback.handler,
            "event": Event::Unknown.name(),
            "nonce": &common::hash(common::random::<16>().ok()?)[..32],
            "received_at": common::now(),
        });
        common::append_private_diagnostic(&path, &row).ok()?;
        Some(Self {
            path,
            row,
            started: Instant::now(),
        })
    }

    pub(crate) fn enter(&mut self, callback: &Callback, args: &[OsString], input: &[u8]) {
        let configured_args = if callback.handler == "precompact-handoff" {
            args == ["codex"]
        } else {
            args.is_empty()
        };
        // Only inspect the typed event header; serde skips all other payload fields.
        let event = (configured_args
            && input.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{'))
        .then(|| serde_json::from_slice::<Header>(input).ok())
        .flatten()
        .and_then(|header| header.hook_event_name)
        .filter(|event| callback.events.contains(event))
        .unwrap_or(Event::Unknown);
        self.row["phase"] = json!("entry");
        self.row["event"] = json!(event.name());
        let _ = common::append_private_diagnostic(&self.path, &self.row);
        // Time the dispatch rather than the diagnostic append.
        self.started = Instant::now();
    }

    pub(crate) fn dispatched(self, exit: i32, internal_failure: bool) {
        let outcome = if exit != 0 {
            "nonzero"
        } else if internal_failure {
            "internal_failure"
        } else {
            "returned"
        };
        self.finish(outcome, Some(exit));
    }

    pub(crate) fn rejected(self, reason: Rejection) {
        self.finish(
            match reason {
                Rejection::Unreadable => "input_unreadable",
                Rejection::Oversize => "input_oversize",
                Rejection::RuntimeUnavailable => "runtime_unavailable",
            },
            None,
        );
    }

    fn finish(mut self, outcome: &str, exit: Option<i32>) {
        let elapsed = self.started.elapsed().as_micros();
        self.row["phase"] = json!("exit");
        self.row["elapsed_us"] = json!(u64::try_from(elapsed).unwrap_or(u64::MAX));
        self.row["outcome"] = json!(outcome);
        if let Some(exit) = exit {
            self.row["inner_exit"] = json!(exit);
        }
        let _ = common::append_private_diagnostic(&self.path, &self.row);
    }
}
