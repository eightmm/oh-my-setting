use crate::{Output, common::*, shell};
use serde_json::json;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Component, Path},
    time::Duration,
};

pub fn run(raw: &[u8], ctx: &Context<'_>) -> Output {
    if off("OMS_SYNTAX_GUARD_HOOK") || interrupted(ctx) {
        return empty();
    }
    let Some(v) = document(raw) else {
        return empty();
    };
    let Some(repo) = repo(&v, ctx).filter(|r| adopted(r)) else {
        return empty();
    };
    let collaboration = crate::routing::collaboration(&v, &repo, ctx);
    let tool = text(&v, "tool_name");
    let cwd = absolute(ctx.cwd, payload_cwd(&v, ctx.cwd));
    let mut paths = Vec::new();
    if ["Edit", "Write", "MultiEdit"].contains(&tool) {
        let path = text(&v["tool_input"], "file_path");
        if !path.is_empty() {
            paths.push(absolute(&cwd, Path::new(path)));
        }
    } else if tool == "apply_patch" {
        let mut seen = BTreeSet::new();
        for name in shell::patch_paths(text(&v["tool_input"], "command"), false) {
            let p = Path::new(&name);
            if name.is_empty() || name.contains('\0') || p.is_absolute() {
                continue;
            }
            let mut depth = 0;
            let mut valid = true;
            for c in p.components() {
                match c {
                    Component::ParentDir => {
                        if depth == 0 {
                            valid = false;
                            break;
                        }
                        depth -= 1;
                    }
                    Component::Normal(_) => depth += 1,
                    _ => {}
                }
            }
            if !valid || depth == 0 {
                continue;
            }
            let p = absolute(&cwd, p);
            if seen.insert(p.clone()) {
                paths.push(p);
            }
            if paths.len() == 12 {
                break;
            }
        }
    }
    let mut findings = Vec::new();
    for path in paths {
        if interrupted(ctx) {
            break;
        }
        if let Some(problem) = problem(&path, &cwd, ctx) {
            let shown = path.strip_prefix(&cwd).unwrap_or(&path).display();
            findings.push(format!(
                "{shown} does not parse after this {tool} — {problem}"
            ));
        }
    }
    let mut parts = Vec::new();
    if !collaboration.is_empty() {
        parts.push(collaboration);
    }
    if !findings.is_empty() {
        parts.push(
            format!("syntax-guard: {}", findings.join(" | "))
                .chars()
                .take(599)
                .collect(),
        );
    }
    if parts.is_empty() {
        empty()
    } else {
        out(json_line(
            &json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":parts.join("\n")}}),
        ))
    }
}
fn problem(path: &Path, cwd: &Path, ctx: &Context<'_>) -> Option<String> {
    let raw = read(path, 2 * 1024 * 1024).ok()?;
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let ext = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let kind = match ext {
        "sh" | "bash" => "bash",
        "py" => "python",
        "json" => {
            if ["tsconfig", "jsconfig", "devcontainer"]
                .iter()
                .any(|p| name.starts_with(p))
            {
                return None;
            }
            "json"
        }
        "toml" => "toml",
        "" => {
            let head = &raw[..raw.len().min(256)];
            if !head.starts_with(b"#!") {
                return None;
            }
            let line = String::from_utf8_lossy(head.split(|b| *b == b'\n').next()?);
            if line.contains("bash") || line.trim_end().ends_with("sh") {
                "bash"
            } else if line.contains("python") {
                "python"
            } else {
                return None;
            }
        }
        _ => return None,
    };
    if kind == "toml" {
        return oms_toml::parse_bounded(&raw, 2 * 1024 * 1024)
            .err()
            .map(|e| format!("toml: {e}"));
    }
    if kind == "json" {
        std::str::from_utf8(&raw).ok()?;
        return serde_json::from_slice::<serde_json::Value>(&raw)
            .err()
            .map(|e| {
                format!(
                    "json: {} (line {} column {})",
                    e.to_string()
                        .split(" at line ")
                        .next()
                        .unwrap_or("invalid JSON"),
                    e.line(),
                    e.column()
                )
            });
    }
    let argv = if kind == "bash" {
        let shell = env_path("BASH")
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| "bash".into());
        vec![shell.into_os_string(), "-n".into(), path.as_os_str().into()]
    } else {
        // AST is the language oracle; Python runs only for a Python edit, in
        // isolated mode, with an explicit interpreter and no project imports.
        let interpreter =
            env_path("OMS_PYTHON").or_else(|| executable(std::ffi::OsStr::new("python3")))?;
        vec![
            interpreter.into_os_string(),
            "-I".into(),
            "-B".into(),
            "-c".into(),
            OsString::from(
                "import ast,sys\ntry:\n ast.parse(sys.stdin.buffer.read(), filename=sys.argv[1])\nexcept SyntaxError as e:\n print('python: %s (line %s)' % (e.msg,e.lineno)); sys.exit(1)\nexcept (ValueError,RecursionError):\n pass",
            ),
            path.as_os_str().into(),
        ]
    };
    let p = invocation(argv, Some(raw), cwd, Duration::from_secs(3), 64 * 1024, ctx).ok()?;
    if !p.complete || p.code == 0 {
        return None;
    }
    if kind == "python" {
        let s = String::from_utf8_lossy(&p.stdout).trim().to_owned();
        return (!s.is_empty()).then_some(s);
    }
    let shown = path.strip_prefix(cwd).unwrap_or(path).to_string_lossy();
    let detail =
        String::from_utf8_lossy(&p.stderr).replace(path.to_string_lossy().as_ref(), &shown);
    let detail = detail
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" | ");
    Some(format!(
        "bash -n: {}",
        if detail.is_empty() {
            "syntax error"
        } else {
            &detail
        }
    ))
}
