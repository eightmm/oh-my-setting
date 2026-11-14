//! Bounded per-file red checks are advisory and never admission authority.
use crate::{
    Result, Services, V, git::Git, obj, require, services::VerifyRequest, worktree::Sandbox,
};
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
pub fn split_patch(raw: &[u8]) -> Vec<(Option<String>, String, Vec<u8>)> {
    let mut chunks: Vec<Vec<u8>> = vec![];
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line.starts_with(b"diff --git ") {
            chunks.push(vec![]);
        }
        if let Some(chunk) = chunks.last_mut() {
            chunk.extend_from_slice(line);
        }
    }
    chunks
        .into_iter()
        .map(|chunk| {
            let mut path = None;
            let mut status = String::from("modified");
            for line in chunk.split(|b| *b == b'\n') {
                if let Some(rest) = line.strip_prefix(b"diff --git ") {
                    if !rest.contains(&b'"') {
                        if let Some(i) = rest.windows(3).position(|s| s == b" b/") {
                            let a = rest[..i].strip_prefix(b"a/");
                            let b = &rest[i + 3..];
                            if a == Some(b) {
                                path = String::from_utf8(b.to_vec()).ok();
                            }
                        }
                    }
                } else if let Some(name) = line.strip_prefix(b"rename to ") {
                    if !name.contains(&b'"') {
                        path = String::from_utf8(name.to_vec()).ok();
                    }
                } else if line.starts_with(b"new file mode") {
                    status = "added".into();
                } else if line.starts_with(b"deleted file mode") {
                    status = "deleted".into();
                }
            }
            (path, status, chunk)
        })
        .collect()
}
pub struct Request<'a> {
    pub repo: &'a Path,
    pub patch: &'a [u8],
    pub verify: &'a str,
    pub base: &'a str,
    pub timeout: Duration,
    pub max_files: usize,
}
pub fn check(
    request: &Request<'_>,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Result<V> {
    require(
        !request.timeout.is_zero() && request.max_files > 0,
        "red-check limits must be positive",
    )?;
    let base = Git::new(request.repo, interrupted).commit(request.base)?;
    let select = regex::Regex::new(
        r"(^|/)(tests?|spec)/|(^|/)test_[^/]+$|[^/]*(_test|_spec|\.test|\.spec)\.[^/.]+$",
    )
    .unwrap();
    let tests: Vec<_> = split_patch(request.patch)
        .into_iter()
        .filter(|(name, _, _)| name.as_ref().is_none_or(|p| select.is_match(p)))
        .collect();
    let mut result = obj([
        ("schema", 1usize.into()),
        ("advisory", true.into()),
        ("base", request.base.into()),
        ("files", V::Arr(vec![])),
        ("cleaned", true.into()),
    ]);
    if tests.is_empty() {
        result.set("note", "no test file in the patch");
        return Ok(result);
    }
    let started = Instant::now();
    let control = Sandbox::new(request.repo, &base, interrupted)?;
    let run = |repo: &Path, file: &str| -> Result<i32> {
        let remaining = request.timeout.saturating_sub(started.elapsed());
        require(!remaining.is_zero(), "red-check time budget spent")?;
        let command = request.verify.replace("{file}", file);
        let execution = services.supervise(
            &VerifyRequest {
                argv: vec!["bash".into(), "-c".into(), command.into()],
                cwd: repo.to_owned(),
                wall: remaining,
                environment: vec![
                    ("OMS_RED_CHECK".into(), "1".into()),
                    ("OMS_RED_CHECK_FILE".into(), file.into()),
                ],
            },
            interrupted,
        )?;
        Ok(execution.code)
    };
    let control_exit = run(&control.path, "");
    result.set(
        "control_exit",
        control_exit
            .as_ref()
            .map(|n| V::Num(n.to_string()))
            .unwrap_or(V::Null),
    );
    let control_ok = control_exit.is_ok_and(|n| n == 0);
    let mut rows = vec![];
    for (index, (path, status, chunk)) in tests.into_iter().enumerate() {
        let mut row = obj([
            (
                "path",
                path.clone()
                    .unwrap_or_else(|| "(unreadable path)".into())
                    .into(),
            ),
            ("status", status.clone().into()),
            ("result", "could not run".into()),
            ("detail", "".into()),
        ]);
        let reason = if !control_ok {
            Some("the command already fails or could not run on the bare base")
        } else if path.is_none() {
            Some("path cannot be read from the patch header")
        } else if status == "deleted" {
            Some("the patch deletes this test file")
        } else if index >= request.max_files {
            Some("over the per-file cap")
        } else if started.elapsed() >= request.timeout {
            Some("time budget spent")
        } else {
            None
        };
        if let Some(reason) = reason {
            row.set("detail", reason);
        } else {
            let scratch = Sandbox::new(request.repo, &base, interrupted)?;
            if Git::new(&scratch.path, interrupted)
                .apply(&chunk, false, false, false)
                .is_err()
            {
                row.set("detail", "the test change does not apply on the base");
            } else {
                match run(&scratch.path, path.as_deref().unwrap()) {
                    Ok(code) if (0..124).contains(&code) => {
                        row.set(
                            "result",
                            if code == 0 {
                                "passes on base (does not test the change)"
                            } else {
                                "fails on base (good)"
                            },
                        );
                        row.set("detail", format!("exit {code}"));
                    }
                    Ok(_) => row.set("detail", "command did not exit normally or timed out"),
                    Err(e) => row.set("detail", e.message),
                }
            }
        }
        rows.push(row);
    }
    result.set("files", V::Arr(rows));
    Ok(result)
}
pub fn render(result: &V) -> String {
    let mut out = format!(
        "red check (advisory, not admission evidence): do the patch's tests fail on base {}?\n",
        crate::string(result, "base")
    );
    if let Some(note) = result.get("note").text() {
        out += &format!("  {note}\n");
    }
    for row in result.get("files").array() {
        out += &format!(
            "  {}: {}{}\n",
            crate::string(row, "path"),
            crate::string(row, "result"),
            if row.get("result").text() == Some("could not run") {
                format!(" - {}", crate::string(row, "detail"))
            } else {
                String::new()
            }
        );
    }
    out
}
