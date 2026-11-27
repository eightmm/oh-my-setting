use crate::{Context, Result, V, delegate::Worktree, obj, process};
use std::{
    path::Path,
    time::{Duration, Instant},
};

struct FilePatch {
    path: Option<String>,
    status: &'static str,
    bytes: Vec<u8>,
}
fn split(raw: &[u8]) -> Vec<FilePatch> {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line.starts_with(b"diff --git ") {
            chunks.push(Vec::new());
        }
        if let Some(chunk) = chunks.last_mut() {
            chunk.extend_from_slice(line);
        }
    }
    chunks
        .into_iter()
        .map(|bytes| {
            let lines = bytes.split(|b| *b == b'\n').collect::<Vec<_>>();
            let header = String::from_utf8_lossy(lines[0]);
            let path = header.strip_prefix("diff --git a/").and_then(|rest| {
                rest.match_indices(" b/").find_map(|(at, _)| {
                    let (left, right) = rest.split_at(at);
                    (left == &right[3..] && !left.contains(['"', '\r', '\0']))
                        .then(|| left.to_owned())
                })
            });
            let status = if lines
                .iter()
                .any(|line| line.starts_with(b"deleted file mode"))
            {
                "deleted"
            } else if lines.iter().any(|line| line.starts_with(b"new file mode")) {
                "added"
            } else {
                "modified"
            };
            FilePatch {
                path,
                status,
                bytes,
            }
        })
        .collect()
}
fn command(
    command: &str,
    path: &str,
    tree: &Worktree,
    deadline: Instant,
    context: &Context<'_>,
) -> Result<process::Completed> {
    let mut environment = vec![
        ("OMS_RED_CHECK".into(), "1".into()),
        ("OMS_RED_CHECK_FILE".into(), path.into()),
    ];
    let cargo_guard = tree.cargo_env("", &mut environment);
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    let command = command.replace("{file}", &quoted);
    let result = context.executor.execute(
        process::Invocation {
            argv: vec![
                "bash".into(),
                "--noprofile".into(),
                "--norc".into(),
                "-c".into(),
                command.into(),
            ],
            cwd: tree.path.clone(),
            input: None,
            environment,
            remove_environment: std::env::vars_os()
                .filter_map(|(key, _)| {
                    key.to_str()
                        .is_some_and(|s| {
                            s.starts_with("GIT_")
                                || s.starts_with("OMS_APPROVAL_")
                                || s.starts_with("OMS_LANDING_")
                                || s.starts_with("OMS_LEASE_")
                        })
                        .then_some(key)
                })
                .collect(),
            timeout: deadline.saturating_duration_since(Instant::now()),
            max_bytes: 1024 * 1024,
            physical_call: None,
            launch_fence: None,
            heartbeat: None,
            merge: true,
        },
        context.interrupted,
    );
    drop(cargo_guard);
    result
}
fn remove(tree: Worktree, context: &Context<'_>, report: &mut V) {
    let leftover = tree.path.to_string_lossy().into_owned();
    if tree.remove(context).is_err() {
        report.set("cleaned", false);
        report.set("leftover", leftover);
    }
}
pub(crate) fn run(
    repo: &Path,
    patch: &[u8],
    verify: &str,
    base: &str,
    timeout: usize,
    context: &Context<'_>,
) -> Result<V> {
    let selector = regex::Regex::new(
        r"(^|/)(tests?|spec)/|(^|/)test_[^/]+$|[^/]*(_test|_spec|\.test|\.spec)\.[^/.]+$",
    )
    .unwrap();
    let tests = split(patch)
        .into_iter()
        .filter(|file| {
            file.path
                .as_ref()
                .is_none_or(|path| selector.is_match(path))
        })
        .collect::<Vec<_>>();
    let mut report = obj([
        ("schema", 1usize.into()),
        ("advisory", true.into()),
        ("base", base.into()),
        ("files", V::Arr(Vec::new())),
        ("cleaned", true.into()),
    ]);
    if tests.is_empty() {
        report.set("note", "no test file in the patch");
        return Ok(report);
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout as u64))
        .ok_or_else(|| crate::Error::new(2, "red-check deadline overflow"))?;
    let mut control_problem = None;
    match Worktree::create(repo, base, context) {
        Ok(tree) => {
            match command(verify, "", &tree, deadline, context) {
                Ok(result) if result.complete => {
                    report.set("control_exit", V::Num(result.code.to_string()));
                    if result.code != 0 {
                        control_problem = Some(format!(
                            "the command already fails (exit {}) on the bare base",
                            result.code
                        ));
                    }
                }
                _ => {
                    control_problem = Some("the command could not run on the bare base".into());
                    report.set("control_exit", V::Null);
                }
            }
            remove(tree, context, &mut report);
        }
        Err(_) => {
            control_problem = Some("scratch worktree unavailable".into());
        }
    }
    let mut files = Vec::new();
    for (index, file) in tests.into_iter().enumerate() {
        crate::check(context.interrupted)?;
        let path = file.path.as_deref().unwrap_or("(unreadable path)");
        let mut entry = obj([
            ("path", path.into()),
            ("status", file.status.into()),
            ("result", "could not run".into()),
            ("detail", "".into()),
        ]);
        let reason = if let Some(problem) = &control_problem {
            Some(problem.as_str())
        } else if file.path.is_none() {
            Some("path cannot be read from the patch header")
        } else if file.status == "deleted" {
            Some("the patch deletes this test file")
        } else if index >= 8 {
            Some("over the 8-file cap")
        } else if deadline.saturating_duration_since(Instant::now()) < Duration::from_secs(1) {
            Some("time budget spent")
        } else {
            None
        };
        if let Some(reason) = reason {
            entry.set("detail", reason);
        } else {
            match Worktree::create(repo, base, context) {
                Err(_) => entry.set("detail", "scratch worktree unavailable"),
                Ok(tree) => {
                    if process::git_input(
                        context,
                        &tree.path,
                        &["apply", "--whitespace=nowarn", "-"],
                        Some(file.bytes),
                    )
                    .is_err()
                    {
                        entry.set("detail", "the test change does not apply on the base");
                    } else {
                        match command(verify, path, &tree, deadline, context) {
                            Ok(result) if result.complete && ![126, 127].contains(&result.code) => {
                                entry.set(
                                    "result",
                                    if result.code == 0 {
                                        "passes on base (does not test the change)"
                                    } else {
                                        "fails on base (good)"
                                    },
                                );
                                entry.set("detail", format!("exit {}", result.code));
                            }
                            _ => entry
                                .set("detail", "command could not run or capture was incomplete"),
                        }
                    }
                    remove(tree, context, &mut report);
                }
            }
        }
        files.push(entry);
    }
    report.set("files", V::Arr(files));
    Ok(report)
}
pub(crate) fn render(report: &V) -> String {
    let mut output = format!(
        "red check (advisory, not admission evidence): do the patch's tests fail on base {}?\n",
        crate::text(report, "base")
    );
    if let Some(note) = report.get("note").text() {
        output.push_str(&format!("  {note}\n"));
    }
    for row in report.get("files").array() {
        output.push_str(&format!(
            "  {}: {} - {}\n",
            crate::text(row, "path"),
            crate::text(row, "result"),
            crate::text(row, "detail")
        ));
    }
    if !report.get("cleaned").truth() {
        output.push_str(&format!(
            "  scratch worktree left at {}\n",
            crate::text(report, "leftover")
        ));
    }
    output
}
