//! Explicit custom-adapter probes, separate from capability-table evidence.
use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
};
use oms_supervisor::{
    capture::LimitAction,
    process::{Progress, Reactor, Request, Scope, StopReason, Streams},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 32 * 1024;
const CLEANUP_WAIT: Duration = Duration::from_secs(5);

pub(crate) fn probe_bound(options: &Options) -> Result<Duration> {
    let raw = options.get("--probe-bound").unwrap_or("10");
    let seconds = raw
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| raw.parse::<u64>().ok())
        .flatten()
        .filter(|n| (1..=60).contains(n))
        .ok_or_else(|| r::Error(2, "--probe-bound requires decimal seconds in 1..60".into()))?;
    Ok(Duration::from_secs(seconds))
}

fn row(name: &str, status: &str, reason: &str) -> Value {
    json!({"name":name,"status":status,"reason":reason})
}

fn failed_probe(name: &str, reason: &str) -> Value {
    json!({"name":name,"status":"fail","reason":reason,
        "cleanup_complete":null,"cleanup_scope":"process_group"})
}

fn adapter_id(path: &Path) -> Option<&str> {
    let id = path
        .file_name()?
        .to_str()?
        .strip_prefix("oms-agent-adapter-")?;
    (!id.is_empty()
        && id.len() <= 64
        && !id.contains("..")
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)))
    .then_some(id)
}

fn probe(
    adapter: &Path,
    cwd: &Path,
    argument: &str,
    name: &str,
    timeout: Duration,
    interrupted: &AtomicBool,
) -> Value {
    let deadline = Instant::now() + timeout;
    let mut request = Request::new(vec![adapter.as_os_str().into(), argument.into()]);
    request.cwd = Some(cwd.to_owned());
    request.streams = Streams::Separate;
    request.scope = Scope::ProcessGroup;
    request.deadline = Some(deadline);
    request.capture.max_bytes = OUTPUT_LIMIT;
    request.capture.on_limit = LimitAction::Terminate;
    // No stdin payload or run verb: these are probe invocations, not inference.
    let mut reactor = Reactor::new();
    let job = match reactor.start(request) {
        Ok(job) => job,
        Err(_) => return failed_probe(name, "launch_failed"),
    };
    let mut cancelled = false;
    loop {
        if interrupted.load(Ordering::Acquire) && !cancelled {
            if reactor.stop(job, StopReason::Cancelled).is_err() {
                return failed_probe(name, "supervisor_error");
            }
            cancelled = true;
        }
        match reactor.poll(job, Duration::from_millis(50)) {
            Ok(Progress::Complete(result)) => {
                let limited =
                    result.stdout.limited || result.stderr.as_ref().is_some_and(|s| s.limited);
                let exit = result.exit.map(|exit| exit.shell_code());
                let reason = match result.stop {
                    Some(StopReason::Cancelled) => "cancelled",
                    Some(StopReason::Deadline) => "deadline",
                    Some(StopReason::OutputLimit) => "output_limit",
                    Some(StopReason::SupervisorError) => "supervisor_error",
                    Some(StopReason::Signal(_)) => "signal",
                    Some(StopReason::ConsumerCutoff) => "output_cutoff",
                    None if limited => "output_limit",
                    None if !result.cleanup_complete() => "cleanup_incomplete",
                    None if !result.output_complete() => "output_incomplete",
                    None if exit != Some(0) => "nonzero_exit",
                    None => "completed",
                };
                return json!({
                    "name":name,"status":if reason=="completed" {"ok"} else {"fail"},
                    "reason":reason,"exit":exit,
                    "cleanup_complete":result.cleanup_complete(),
                    "cleanup_scope":"process_group",
                    "output_complete":result.output_complete(),
                    "stdout_bytes":result.stdout.observed_bytes,
                    "stderr_bytes":result.stderr.as_ref().map(|s|s.observed_bytes),
                });
            }
            Ok(Progress::Pending(_)) if Instant::now() < deadline + CLEANUP_WAIT => {}
            Ok(Progress::Pending(_)) => {
                // Drop attempts bounded owned cleanup. It cannot certify a
                // blocked kernel wait or a descendant that escaped this group.
                return failed_probe(name, "cleanup_incomplete");
            }
            Err(_) => return failed_probe(name, "supervisor_error"),
        }
    }
}

pub(crate) fn run(
    paths: &Paths,
    options: &Options,
    interrupted: &AtomicBool,
) -> Result<crate::Output> {
    let timeout = probe_bound(options)?;
    let adapter = options
        .path("--conformance", &paths.cwd)?
        .ok_or_else(|| r::Error(2, "--conformance requires an adapter path".into()))?;
    let id = adapter_id(&adapter);
    let executable =
        fs::metadata(&adapter).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    let mut checks = vec![
        row(
            "name",
            if id.is_some() { "ok" } else { "fail" },
            if id.is_some() {
                "marker_and_id"
            } else {
                "invalid_marker_or_id"
            },
        ),
        row(
            "executable",
            if executable { "ok" } else { "fail" },
            if executable {
                "executable_file"
            } else {
                "not_executable_file"
            },
        ),
    ];
    let mut allowed = id.is_some() && executable;
    let scratch = allowed.then(r::Scratch::new).transpose()?;
    for (argument, name) in [("--version", "version-probe"), ("--help", "help-probe")] {
        if interrupted.load(Ordering::Acquire) {
            allowed = false;
        }
        let result = if allowed {
            probe(
                &adapter,
                &scratch.as_ref().unwrap().0,
                argument,
                name,
                timeout,
                interrupted,
            )
        } else {
            row(name, "unknown", "not_started")
        };
        if result.get("cleanup_complete").is_some_and(|v| v != true) {
            allowed = false;
        }
        checks.push(result);
    }
    let ok = checks.iter().all(|row| row["status"] == "ok");
    checks.extend([
        row("read-call", "unknown", "run_conformance_not_implemented"),
        row("write-call", "unknown", "run_conformance_not_implemented"),
        row("prompt", "unknown", "file_and_dash_prompt_not_exercised"),
        row(
            "isolation",
            "unknown",
            "network_credentials_and_external_writes_not_observed",
        ),
        row(
            "admission",
            "unknown",
            "not_an_admission_or_live_capability_receipt",
        ),
        row(
            "whole-tree-cleanup",
            "unknown",
            "process_groups_do_not_prove_tree_containment",
        ),
    ]);
    let mut out = r::json(&json!({
        "schema":1,"kind":"oms-adapter-conformance","mode":"probe","adapter":id,
        "ok":ok,"probe_bound_seconds":timeout.as_secs(),
        "output_limit_bytes_per_stream":OUTPUT_LIMIT,"checks":checks,
    }))?;
    out.0 = if interrupted.load(Ordering::Acquire) {
        130
    } else {
        i32::from(!ok)
    };
    Ok(out)
}
