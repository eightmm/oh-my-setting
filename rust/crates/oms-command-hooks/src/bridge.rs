use crate::{Output, common::*};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const DEFAULT_ID: &str = "oh-my-setting@oh-my-setting-local";
const SURFACES: &[(&str, &str, &str, &[u64])] = &[
    ("UserPromptSubmit", "", "skill-router", &[5]),
    ("Stop", "", "turn-guard", &[12]),
    ("PreCompact", "", "precompact-handoff codex", &[30]),
    ("SessionStart", "", "resume-hook", &[10]),
    ("SessionStart", "", "telemetry-hook", &[5]),
    ("PostToolUse", "apply_patch", "syntax-guard-hook", &[5]),
    ("PostToolUse", "", "telemetry-hook", &[5]),
    ("SubagentStop", "", "telemetry-hook", &[5]),
    ("SessionEnd", "", "telemetry-hook", &[3, 5]),
];
fn read_json(path: &Path) -> Result<Value, String> {
    strict_document(&read(path, INPUT_MAX)?)
}
fn expected(native: &Path, id: &str) -> Result<BTreeMap<String, String>, String> {
    let hooks = read_json(&native.join("hooks.json"))?;
    let hooks = hooks["hooks"]
        .as_object()
        .ok_or("native hooks object missing")?;
    let mut result = BTreeMap::new();
    let boundary = regex::Regex::new(r"([a-z0-9])([A-Z])").unwrap();
    for (event, groups) in hooks {
        let snake = boundary.replace_all(event, "${1}_${2}").to_lowercase();
        for (gi, group) in groups
            .as_array()
            .ok_or("native hook groups invalid")?
            .iter()
            .enumerate()
        {
            for (hi, hook) in group["hooks"]
                .as_array()
                .ok_or("native hook rows invalid")?
                .iter()
                .enumerate()
            {
                if text(hook, "type") == "command" {
                    let command = text(hook, "command");
                    let label = command
                        .split_once("harness-hook.sh\" ")
                        .map_or(event.as_str(), |(_, s)| s);
                    result.insert(format!("{id}:hooks.json:{snake}:{gi}:{hi}"), label.into());
                }
            }
        }
    }
    Ok(result)
}
fn trust_response(
    response: &Value,
    expected: &BTreeMap<String, String>,
    id: &str,
    cwd: &Path,
) -> (String, Vec<String>) {
    let unknown = || ("unknown".into(), vec![]);
    if response.get("error").is_some() {
        return unknown();
    }
    let Some(entries) = response["result"]["data"].as_array() else {
        return unknown();
    };
    let mut found = BTreeMap::<String, String>::new();
    for entry in entries {
        if !entry.is_object()
            || entry["errors"].as_array().is_none()
            || entry["warnings"].as_array().is_none()
            || entry["hooks"].as_array().is_none()
            || entry["cwd"]
                .as_str()
                .and_then(|s| Path::new(s).canonicalize().ok())
                .as_deref()
                != Some(cwd)
        {
            return unknown();
        }
        for hook in entry["hooks"].as_array().unwrap() {
            if !hook.is_object() {
                return unknown();
            }
            if text(hook, "pluginId") != id {
                continue;
            }
            let key = text(hook, "key");
            let status = text(hook, "trustStatus");
            if !expected.contains_key(key)
                || !["trusted", "untrusted", "modified", "managed"].contains(&status)
                || found.contains_key(key)
            {
                return unknown();
            }
            found.insert(
                key.into(),
                if hook["enabled"] == true {
                    status
                } else {
                    "disabled"
                }
                .to_string(),
            );
        }
    }
    if found.len() != expected.len() || expected.keys().any(|k| !found.contains_key(k)) {
        return unknown();
    }
    let untrusted = expected
        .iter()
        .filter(|(k, _)| found.get(*k).is_none_or(|s| s != "trusted"))
        .map(|(_, v)| v.clone())
        .collect::<Vec<_>>();
    (
        if untrusted.is_empty() {
            "trusted"
        } else {
            "untrusted"
        }
        .into(),
        untrusted,
    )
}
fn trust(
    native: &Path,
    id: &str,
    binary: Option<&Path>,
    ctx: &Context<'_>,
) -> (String, Vec<String>) {
    let Ok(expected) = expected(native, id) else {
        return ("unknown".into(), vec![]);
    };
    let Some(binary) = binary
        .map(Path::to_path_buf)
        .or_else(|| executable(std::ffi::OsStr::new("codex")))
    else {
        return ("unknown".into(), vec![]);
    };
    let cwd = ctx.cwd.canonicalize().unwrap_or_else(|_| ctx.cwd.into());
    let mut cmd = Command::new(binary);
    cmd.args(["app-server", "--listen", "stdio://"])
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    for key in [
        concat!("OPENAI_API_", "KEY"),
        concat!("CODEX_API_", "KEY"),
        concat!("CODEX_ACCESS_", "TOKEN"),
    ] {
        cmd.env_remove(key);
    }
    let Ok(mut child) = cmd.spawn() else {
        return ("unknown".into(), vec![]);
    };
    let response = (|| -> Option<Value> {
        let mut input = child.stdin.take()?;
        let mut output = child.stdout.take()?;
        rustix::fs::fcntl_setfl(&output, rustix::fs::OFlags::NONBLOCK).ok()?;
        for row in [
            json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"oms_hook_trust","version":"1"}}}),
            json!({"method":"initialized","params":{}}),
            json!({"id":2,"method":"hooks/list","params":{"cwds":[cwd]}}),
        ] {
            input.write_all(&json_line(&row)).ok()?;
        }
        input.flush().ok()?;
        let until = Instant::now() + Duration::from_secs(8);
        let mut bytes = Vec::new();
        let mut start = 0;
        while Instant::now() < until && !interrupted(ctx) {
            let mut buffer = [0; 8192];
            match output.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    bytes.extend_from_slice(&buffer[..n]);
                    if bytes.len() > STATE_MAX {
                        return None;
                    }
                    while let Some(offset) = bytes[start..].iter().position(|b| *b == b'\n') {
                        let end = start + offset;
                        let row = document(&bytes[start..end]);
                        start = end + 1;
                        if let Some(row) = row
                            && row["id"] == 2
                        {
                            return Some(row);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break,
            }
        }
        None
    })();
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    response.map_or_else(
        || ("unknown".into(), vec![]),
        |r| trust_response(&r, &expected, id, &cwd),
    )
}
fn verified(raw: &[u8], native: &Path, source: &Path, id: &str) -> bool {
    let Ok(catalog) = strict_document(raw) else {
        return false;
    };
    let matches: Vec<_> = catalog["installed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| text(r, "pluginId") == id)
        .collect();
    if matches.len() != 1 {
        return false;
    }
    let row = matches[0];
    if row["installed"] != true
        || row["enabled"] != true
        || row["source"]["path"].as_str()
            != Some(
                source
                    .join("plugins/oh-my-setting")
                    .to_string_lossy()
                    .as_ref(),
            )
    {
        return false;
    }
    let Ok(manifest) = read_json(&native.join(".codex-plugin/plugin.json")) else {
        return false;
    };
    if text(&manifest, "name") != "oh-my-setting"
        || text(&manifest, "hooks") != "./hooks.json"
        || manifest["version"] != row["version"]
    {
        return false;
    }
    let Ok(hooks) = read_json(&native.join("hooks.json")) else {
        return false;
    };
    for (event, matcher, action, _) in SURFACES {
        if ["PostToolUse", "SubagentStop"].contains(event)
            && matcher.is_empty()
            && *action == "telemetry-hook"
        {
            continue;
        }
        let command =
            format!("bash \"${{CLAUDE_PLUGIN_ROOT:-.}}/scripts/harness-hook.sh\" {action}");
        if !hooks["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .any(|g| {
                text(g, "matcher") == *matcher
                    && g["hooks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|h| text(h, "type") == "command" && text(h, "command") == command)
            })
        {
            return false;
        }
    }
    true
}
fn known(event: &str, matcher: &str, hook: &Value, source: &Path) -> bool {
    let Some(fields) = hook.as_object() else {
        return false;
    };
    if fields
        .keys()
        .any(|k| !["type", "command", "timeout"].contains(&k.as_str()))
        || text(hook, "type") != "command"
    {
        return false;
    }
    let plugin = source.join("plugins/oh-my-setting");
    let p = plugin.to_string_lossy();
    let prefix = format!("CLAUDE_PLUGIN_ROOT=\"{p}\" bash \"{p}/scripts/harness-hook.sh\" ");
    SURFACES.iter().any(|(e, m, action, timeouts)| {
        *e == event
            && *m == matcher
            && hook["timeout"]
                .as_u64()
                .is_some_and(|n| timeouts.contains(&n))
            && (text(hook, "command") == format!("env {prefix}{action}")
                || *action == "turn-guard"
                    && text(hook, "command")
                        == format!("env OMS_TURN_GUARD_MAX_BLOCKS_PER_TURN=0 {prefix}{action}"))
    })
}
pub fn run(args: &[OsString], raw: &[u8], ctx: &Context<'_>) -> Output {
    let mut values = BTreeMap::<String, PathBuf>::new();
    let mut flags = BTreeSet::new();
    let mut id = DEFAULT_ID.to_owned();
    let mut i = 0;
    while i < args.len() {
        let Some(s) = args[i].to_str() else {
            return fail("invalid bridge argument");
        };
        match s {
            "--hooks" | "--source-root" | "--native-root" | "--codex" | "--plugin-id" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return fail(format!("{s} requires a value"));
                };
                if s == "--plugin-id" {
                    id = v.to_string_lossy().into_owned();
                } else {
                    values.insert(s.into(), absolute(ctx.cwd, Path::new(v)));
                }
            }
            "--trust-report" | "--dry-run" | "--probe" => {
                flags.insert(s.to_owned());
            }
            "--help" | "-h" => {
                return out(
                    "Usage: codex-hook-bridge --hooks PATH --source-root PATH [--native-root PATH] [--plugin-id ID] [--codex PATH] [--trust-report] [--dry-run] [--probe]\n",
                );
            }
            _ => return fail("unknown bridge argument"),
        }
        i += 1;
    }
    let (Some(path), Some(source)) = (values.get("--hooks"), values.get("--source-root")) else {
        return fail("bridge requires --hooks and --source-root");
    };
    let result = (|| -> Result<Output, String> {
        if flags.contains("--trust-report") {
            let native = values
                .get("--native-root")
                .ok_or("--native-root is required with --trust-report")?;
            let (state, untrusted) = trust(
                native,
                &id,
                values.get("--codex").map(PathBuf::as_path),
                ctx,
            );
            return Ok(out(format!(
                "{state}{}\n",
                if untrusted.is_empty() {
                    String::new()
                } else {
                    format!(": {}", untrusted.join(", "))
                }
            )));
        }
        if !path.exists() && !path.is_symlink() {
            return Ok((
                if flags.contains("--probe") { 1 } else { 0 },
                vec![],
                vec![],
            ));
        }
        let parent = path.parent().ok_or("hook parent missing")?;
        if parent
            .canonicalize()
            .map_err(|_| "hook parent unavailable")?
            != parent
        {
            return Err("hook parent must be a canonical non-link directory".into());
        }
        let original = read(path, INPUT_MAX)?;
        let mut data = strict_document(&original)?;
        let hooks = data["hooks"]
            .as_object_mut()
            .ok_or("user hooks must contain a hooks object")?;
        let mut count = 0;
        for (event, groups) in hooks.iter_mut() {
            let groups = groups
                .as_array_mut()
                .ok_or("hook event must contain a list")?;
            let mut kept = Vec::new();
            for group in groups.iter() {
                let Some(fields) = group.as_object().filter(|g| {
                    g.keys().all(|k| ["hooks", "matcher"].contains(&k.as_str()))
                        && g.get("hooks").is_some_and(Value::is_array)
                }) else {
                    kept.push(group.clone());
                    continue;
                };
                let rows = fields["hooks"].as_array().unwrap();
                let rows_after: Vec<_> = rows
                    .iter()
                    .filter(|h| !known(event, text(group, "matcher"), h, source))
                    .cloned()
                    .collect();
                let removed = rows.len() - rows_after.len();
                count += removed;
                if !rows_after.is_empty() || removed == 0 {
                    let mut g = group.clone();
                    g["hooks"] = json!(rows_after);
                    kept.push(g);
                }
            }
            *groups = kept;
        }
        if count == 0 {
            return Ok((
                if flags.contains("--probe") { 1 } else { 0 },
                vec![],
                vec![],
            ));
        }
        if flags.contains("--probe") {
            return Ok(empty());
        }
        let Some(native) = values
            .get("--native-root")
            .filter(|n| verified(raw, n, source, &id))
        else {
            return Ok(out(
                "codex-hook-bridge: preserved; native plugin hooks are not verified\n",
            ));
        };
        let (state, untrusted) = trust(
            native,
            &id,
            values.get("--codex").map(PathBuf::as_path),
            ctx,
        );
        if state != "trusted" {
            let detail = if state == "untrusted" {
                format!("untrusted: {}", untrusted.join(", "))
            } else {
                "unknown".into()
            };
            return Ok(out(format!(
                "codex-hook-bridge: preserved; native hook trust {detail} (open /hooks in Codex and trust the OMS hooks)\n"
            )));
        }
        if flags.contains("--dry-run") {
            return Ok(out(format!(
                "codex-hook-bridge: would retire {count} legacy commands with an exact-byte backup\n"
            )));
        }
        let _guard = lock(&path.with_extension("oms-bridge-transaction"), ctx.cwd)?;
        let backup = path.with_file_name(format!(
            "{}.oms-bridge-{}.bak",
            path.file_name().unwrap().to_string_lossy(),
            hash(&original)
        ));
        crate::common::backup(&backup, &original, INPUT_MAX, true)?;
        let mut encoded = serde_json::to_vec_pretty(&data).map_err(|_| "invalid hook JSON")?;
        encoded.push(b'\n');
        mutate(path, INPUT_MAX, false, |old| {
            if old != original {
                return Err("user hooks changed during migration".into());
            }
            Ok((encoded, ()))
        })?;
        Ok(out(format!(
            "codex-hook-bridge: retired {count} legacy commands; backup: {}\n",
            backup.display()
        )))
    })();
    result.unwrap_or_else(|e| {
        (
            2,
            vec![],
            format!("codex-hook-bridge: preserved; {e}\n").into_bytes(),
        )
    })
}
