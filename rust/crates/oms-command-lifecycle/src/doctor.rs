use crate::{
    cli::Options,
    receipt::{self, Paths},
    runtime::{self as r, Result},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn finding(section: &str, level: &str, message: impl Into<String>) -> Value {
    json!({"section":section,"level":level,"message":message.into()})
}
fn clean(s: &str) -> String {
    s.chars()
        .take(160)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}
fn report(paths: &Paths, rows: Vec<Value>) -> Value {
    let failed = rows.iter().any(|row| row["level"] == "error");
    let mut counts = BTreeMap::<String, u64>::new();
    for row in &rows {
        *counts
            .entry(row["level"].as_str().unwrap_or("unknown").into())
            .or_default() += 1;
    }
    let remediations = if failed {
        json!([{"command":"oms doctor --repair","authority":"local_mutation","source":"canonical install drift"}])
    } else {
        json!([])
    };
    json!({"schema":1,"repo":paths.cwd,"status":if failed{"failed"}else{"ok"},"exit":i32::from(failed),"counts":counts,"findings":rows,"remediations":remediations})
}
fn emit(value: Value, options: &Options) -> Result<crate::Output> {
    let mut out = if options.has("--json") {
        r::json(&value)?
    } else if options.has("--remediation-plan") {
        let rows = value["remediations"].as_array().unwrap();
        r::text(format!(
            "doctor remediation plan: {} item(s)\n{}",
            rows.len(),
            rows.iter()
                .map(|v| format!(
                    "[{}] {}",
                    v["authority"].as_str().unwrap_or("read"),
                    v["command"].as_str().unwrap_or("")
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    } else {
        r::text(
            value["findings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    format!(
                        "{}: {}",
                        v["level"].as_str().unwrap_or("unknown"),
                        v["message"].as_str().unwrap_or("")
                    )
                })
                .chain(std::iter::once(format!(
                    "doctor: {}",
                    value["status"].as_str().unwrap_or("unknown")
                )))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    out.0 = value["exit"].as_i64().unwrap_or(0) as i32;
    Ok(out)
}
fn security(paths: &Paths) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for (provider, path) in [
        ("claude", paths.claude_settings()?),
        ("agy", paths.home.join(".gemini/config/settings.json")),
    ] {
        if !r::exists(&path) {
            rows.push(finding(
                provider,
                "info",
                "settings absent; provider defaults apply",
            ));
            continue;
        }
        let value = match r::value(&path) {
            Ok(v) if v.is_object() => v,
            _ => {
                rows.push(finding(
                    provider,
                    "unknown",
                    "settings could not be parsed; nothing executed",
                ));
                continue;
            }
        };
        let mut checks = Vec::new();
        if provider == "claude" {
            checks.push((
                value["permissions"]["defaultMode"] == "bypassPermissions",
                "bypass permission mode",
            ));
            checks.push((
                value["skipDangerousModePermissionPrompt"] == true,
                "dangerous-mode confirmation skipped",
            ));
            checks.push((
                value["permissions"]["allow"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|x| matches!(x.as_str(), Some("Bash" | "Bash(*)" | "Bash(*:*)")))
                }),
                "blanket shell allow rule",
            ));
            checks.push((
                value["sandbox"]["enabled"] == false,
                "sandbox explicitly disabled",
            ));
        } else {
            checks.push((
                value["permissions"]["allow"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|x| matches!(x.as_str(), Some("mcp(*)" | "unsandboxed(*)")))
                }),
                "blanket MCP or unsandboxed allow rule",
            ));
        }
        for (wide, description) in checks {
            rows.push(finding(
                provider,
                if wide { "warn" } else { "info" },
                format!(
                    "{description}: {}",
                    if wide { "configured" } else { "not observed" }
                ),
            ));
        }
    }
    let config = paths.codex_config()?;
    if r::exists(&config) {
        rows.push(finding(
            "codex",
            "unknown",
            "Codex TOML requires a native TOML settings adapter; no executable was started",
        ));
    } else {
        rows.push(finding(
            "codex",
            "info",
            "settings absent; provider defaults apply",
        ));
    }
    for name in ["bwrap", "socat", "sandbox-exec", "srt", "docker", "podman"] {
        rows.push(finding(
            "sandbox tools",
            "info",
            format!(
                "{name}: {} (not executed)",
                if r::find(name, &paths.cwd).is_some() {
                    "on PATH"
                } else {
                    "absent"
                }
            ),
        ));
    }
    Ok(rows)
}
fn sandbox(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let actual = rustix::system::uname();
    rows.push(finding(
        "host",
        "info",
        format!(
            "host kernel: {}",
            clean(&actual.sysname().to_string_lossy())
        ),
    ));
    if cfg!(target_os = "linux") {
        for (name, path, zero_restricts) in [
            (
                "userns_clone",
                "/proc/sys/kernel/unprivileged_userns_clone",
                true,
            ),
            (
                "max_user_namespaces",
                "/proc/sys/user/max_user_namespaces",
                true,
            ),
            (
                "apparmor_restrict_userns",
                "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
                false,
            ),
        ] {
            let value = fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());
            let state = match value {
                Some(n) if (n == 0) == zero_restricts => "restricted",
                Some(_) => "available",
                None => "unknown",
            };
            rows.push(finding(
                "sandbox",
                if state == "restricted" {
                    "warn"
                } else {
                    "info"
                },
                format!("{name}: {state} ({path})"),
            ));
        }
        for (name, path) in [
            ("Landlock", "/sys/kernel/security/lsm"),
            ("seccomp", "/proc/self/status"),
        ] {
            let state = fs::read_to_string(path)
                .ok()
                .map(|s| {
                    if s.contains(if name == "Landlock" {
                        "landlock"
                    } else {
                        "Seccomp:"
                    }) {
                        "available"
                    } else {
                        "unknown"
                    }
                })
                .unwrap_or("unknown");
            rows.push(finding("sandbox", "info", format!("{name}: {state}")));
        }
        let available = r::find("bwrap", &paths.cwd);
        rows.push(finding(
            "sandbox",
            "info",
            format!(
                "bwrap: {}",
                if available.is_some() {
                    "available, not executed"
                } else {
                    "unavailable"
                }
            ),
        ));
        if options.has("--probe") {
            let binary =
                available.ok_or_else(|| r::Error(2, "bwrap is unavailable for --probe".into()))?;
            let output = r::command(
                binary,
                &[
                    "--ro-bind",
                    "/",
                    "/",
                    "--unshare-all",
                    "--die-with-parent",
                    "true",
                ]
                .map(Into::into),
                &paths.cwd,
                None,
                flag,
                Duration::from_secs(5),
            )?;
            rows.push(finding(
                "sandbox",
                if output.0 == 0 { "ok" } else { "warn" },
                format!("bwrap smoke probe exit {}", output.0),
            ));
        }
    } else if cfg!(target_os = "macos") {
        rows.push(finding(
            "sandbox",
            "info",
            format!(
                "sandbox-exec: {}",
                if r::find("sandbox-exec", &paths.cwd).is_some() {
                    "available, not executed"
                } else {
                    "unavailable"
                }
            ),
        ));
    }
    Ok(rows)
}
fn extension_configs(paths: &Paths) -> Vec<(&'static str, &'static str, PathBuf)> {
    vec![
        ("claude", "user", paths.home.join(".claude.json")),
        ("claude", "project", paths.cwd.join(".mcp.json")),
        (
            "agy",
            "user",
            paths.home.join(".gemini/config/mcp_config.json"),
        ),
        ("agy", "project", paths.cwd.join(".gemini/mcp_config.json")),
    ]
}
fn specs(paths: &Paths) -> Result<(Vec<(String, Value)>, Vec<String>)> {
    let mut specs = Vec::new();
    let mut unknown = Vec::new();
    for (provider, scope, path) in extension_configs(paths) {
        if !r::exists(&path) {
            continue;
        }
        match r::value(&path) {
            Ok(v) if v.is_object() => {
                if let Some(servers) = v["mcpServers"].as_object() {
                    for (name, spec) in servers {
                        specs.push((format!("{provider}:{scope}:{}", clean(name)), spec.clone()));
                    }
                } else if !v["mcpServers"].is_null() {
                    unknown.push(provider.into());
                }
            }
            _ => unknown.push(provider.into()),
        }
    }
    if r::exists(&paths.codex_config()?) {
        unknown.push("codex".into());
    }
    unknown.sort();
    unknown.dedup();
    Ok((specs, unknown))
}
fn safe_target(spec: &Value) -> String {
    if let Some(url) = spec["url"]
        .as_str()
        .or_else(|| spec["httpUrl"].as_str())
        .or_else(|| spec["serverUrl"].as_str())
    {
        let Some((scheme, rest)) = url.split_once("://") else {
            return "?".into();
        };
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("?")
            .rsplit('@')
            .next()
            .unwrap_or("?");
        return clean(&format!("{scheme}://{host}"));
    }
    clean(
        Path::new(spec["command"].as_str().unwrap_or("?"))
            .file_name()
            .and_then(|p| p.to_str())
            .unwrap_or("?"),
    )
}
fn extensions(paths: &Paths, pins: bool, options: &Options) -> Result<crate::Output> {
    if options.has("--probe-tools") {
        return r::fail(
            "native MCP tools/list protocol probe adapter is not yet available; no server was started",
        );
    }
    let (specs, unknown) = specs(paths)?;
    let mut rows = Vec::new();
    let mut current = serde_json::Map::new();
    for (key, spec) in &specs {
        let env_names = spec["env"]
            .as_object()
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let material = json!({"type":spec.get("type").or_else(||spec.get("transport")),"command":spec.get("command"),"args":spec.get("args"),"url":spec.get("url").or_else(||spec.get("httpUrl")).or_else(||spec.get("serverUrl")),"env_names":env_names});
        let compact = serde_json::to_vec(&material)
            .map_err(|_| r::Error(2, "MCP pin serialization failed".into()))?;
        current.insert(key.clone(), json!(r::hash(&compact)));
        rows.push(json!({"server":key,"transport":if spec["command"].is_string(){"stdio"}else{"http"},"target":safe_target(spec),"env_names":env_names.len(),"disabled":spec["disabled"]==true || spec["enabled"]==false}));
    }
    if !pins {
        return r::json(
            &json!({"schema":1,"kind":"oms-doctor-extensions","servers":rows,"unknown":unknown}),
        );
    }
    let path = std::env::var_os("OMS_MCP_PIN_FILE")
        .map(PathBuf::from)
        .unwrap_or(paths.config.join("mcp-pins.json"));
    let previous = if r::exists(&path) {
        let row = r::value(&path)?;
        if row["schema"] != 1 || !row["pins"].is_object() {
            return r::fail("invalid MCP pin store");
        }
        row
    } else {
        json!({"schema":1,"pins":{},"tools":{}})
    };
    let old = previous["pins"].as_object().unwrap();
    let added = current
        .keys()
        .filter(|k| !old.contains_key(*k))
        .cloned()
        .collect::<Vec<_>>();
    let changed = current
        .iter()
        .filter(|(k, v)| old.get(*k).is_some_and(|o| o != *v))
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>();
    let removed = old
        .keys()
        .filter(|k| {
            !current.contains_key(*k) && !unknown.iter().any(|p| k.starts_with(&format!("{p}:")))
        })
        .cloned()
        .collect::<Vec<_>>();
    if options.has("--pin") {
        r::child_guard()?;
        let _guard = r::lock(&path, &paths.cwd)?;
        // Unknown providers keep their existing baseline, never a false removal.
        for (k, v) in old {
            if unknown.iter().any(|p| k.starts_with(&format!("{p}:"))) {
                current.insert(k.clone(), v.clone());
            }
        }
        let row = json!({"schema":1,"pins":current,"tools":previous.get("tools").cloned().unwrap_or(json!({}))});
        r::atomic(&path, &r::bytes(&row)?)?;
    }
    r::json(
        &json!({"schema":1,"servers":rows,"new":added,"changed":changed,"removed":removed,"unknown":unknown,"pinned":options.has("--pin")}),
    )
}
fn residue(paths: &Paths, flag: &AtomicBool) -> Result<Vec<Value>> {
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut files = 0;
    fn walk(
        path: &Path,
        rows: &mut Vec<Value>,
        files: &mut usize,
        started: Instant,
        flag: &AtomicBool,
        depth: usize,
    ) -> Result<()> {
        r::check(flag)?;
        if depth > 64 || *files >= 1000 || started.elapsed() > Duration::from_secs(5) {
            return Ok(());
        }
        let Ok(m) = fs::symlink_metadata(path) else {
            return Ok(());
        };
        if m.file_type().is_symlink() {
            return Ok(());
        }
        if m.is_dir() {
            for e in fs::read_dir(path)? {
                walk(&e?.path(), rows, files, started, flag, depth + 1)?;
            }
        } else if m.is_file() && m.len() <= 1024 * 1024 {
            *files += 1;
            let raw = r::read(path, 1024 * 1024)?;
            let hits = oms_scrub::scan(&raw, &[])
                .map_err(|_| r::Error(2, "residue scanner failed".into()))?;
            for hit in hits {
                rows.push(finding(
                    "residue",
                    "warn",
                    format!("{} line {} class {}", path.display(), hit.line, hit.class),
                ));
            }
        }
        Ok(())
    }
    for path in [
        paths.home.join(".claude/projects"),
        paths.home.join(".claude/shell-snapshots"),
        paths.codex_home()?.join("sessions"),
        paths.codex_home()?.join("shell_snapshots"),
        paths.codex_home()?.join("history.jsonl"),
    ] {
        walk(&path, &mut rows, &mut files, started, flag, 0)?;
    }
    rows.push(finding("residue","info",format!("scanned {files} files; limits: 1000 files, 1 MiB/file, 64 levels, 5 seconds; values never printed")));
    Ok(rows)
}
fn providers(paths: &Paths, options: &Options) -> Result<crate::Output> {
    let path = options
        .path("--table", &paths.cwd)?
        .unwrap_or(paths.source.join("config/provider-capabilities.json"));
    let table = r::value(&path)?;
    if table["schema"] != 1 {
        return r::fail("unsupported provider capability table schema");
    }
    r::json(&table)
}
fn tool_lock(paths: &Paths) -> Result<Vec<Value>> {
    let path = std::env::var_os("OH_MY_SETTING_TOOL_LOCK")
        .map(PathBuf::from)
        .unwrap_or(paths.source.join("tools.lock.json"));
    let table = r::value(&path)?;
    if table["schema"] != 2 {
        return r::fail("unsupported tool lock schema");
    }
    fn validate(v: &Value) -> Result<()> {
        match v {
            Value::Object(o) => {
                for (k, v) in o {
                    if k == "sha256"
                        && v.as_str().is_none_or(|s| {
                            s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                    {
                        return r::fail("invalid locked payload digest");
                    }
                    if k == "url"
                        && v.as_str()
                            .is_none_or(|s| !s.starts_with("https://") || s.contains(['\r', '\n']))
                    {
                        return r::fail("locked download URL must use HTTPS");
                    }
                    validate(v)?;
                }
            }
            Value::Array(a) => {
                for v in a {
                    validate(v)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    validate(&table)?;
    Ok(vec![finding(
        "tool lock",
        "ok",
        "tool lock schema, HTTPS URLs and payload digests validated",
    )])
}
fn health(paths: &Paths, flag: &AtomicBool) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    match paths.load() {
        Ok(Some(row)) => {
            let owner = receipt::owner(&row)?;
            rows.push(finding(
                "install receipt",
                if owner == paths.source { "ok" } else { "warn" },
                if owner == paths.source {
                    "canonical owner matches this checkout"
                } else {
                    "this checkout differs from the canonical owner"
                },
            ));
            if !owner.join("rules/global-AGENTS.md").is_file() {
                rows.push(finding(
                    "install receipt",
                    "error",
                    "canonical install owner is unavailable",
                ));
            }
            for target in [".codex/AGENTS.md", ".claude/CLAUDE.md", ".gemini/AGENTS.md"] {
                let managed = crate::install::owned(
                    &owner.join("rules/global-AGENTS.md"),
                    &paths.home.join(target),
                )?;
                rows.push(finding(
                    "managed targets",
                    if managed { "ok" } else { "error" },
                    format!(
                        "{target}: {}",
                        if managed {
                            "owned"
                        } else {
                            "missing, modified or foreign"
                        }
                    ),
                ));
            }
        }
        Ok(None) => rows.push(finding(
            "install receipt",
            "error",
            "install receipt missing",
        )),
        Err(_) => rows.push(finding(
            "install receipt",
            "error",
            "install receipt invalid; automatic takeover refused",
        )),
    }
    match crate::payload::verify(paths, flag) {
        Ok(p) => rows.push(finding(
            "native payload",
            "ok",
            format!(
                "native payload verified at revision {}; graph helper {}",
                p.revision,
                if p.helper.is_some() {
                    "present"
                } else {
                    "absent"
                }
            ),
        )),
        Err(_) => rows.push(finding(
            "native payload",
            "error",
            "native payload missing, unsafe or invalid",
        )),
    }
    for name in ["git", "bash"] {
        rows.push(finding(
            "runtime",
            if r::find(name, &paths.cwd).is_some() {
                "ok"
            } else {
                "error"
            },
            format!(
                "{name}: {}",
                if r::find(name, &paths.cwd).is_some() {
                    "available"
                } else {
                    "missing"
                }
            ),
        ));
    }
    for name in ["codex", "claude", "agy"] {
        rows.push(finding(
            "provider inventory",
            if r::find(name, &paths.cwd).is_some() {
                "info"
            } else {
                "note"
            },
            format!(
                "{name}: {} (not executed)",
                if r::find(name, &paths.cwd).is_some() {
                    "selected on PATH"
                } else {
                    "not installed"
                }
            ),
        ));
    }
    match crate::install::skill_sources(paths) {
        Ok(s) => rows.push(finding(
            "skills",
            "ok",
            format!("{} enabled skill sources validated", s.len()),
        )),
        Err(_) => rows.push(finding(
            "skills",
            "error",
            "skill manifest or source invalid",
        )),
    }
    if paths.cwd.join(".oms").is_dir() {
        let args = [
            "--repo".into(),
            paths.cwd.as_os_str().to_owned(),
            "validate".into(),
        ];
        for (app, name) in [
            (oms_command_operations::App::Events, "events"),
            (oms_command_operations::App::Approvals, "approvals"),
        ] {
            let out = oms_command_operations::run(app, &args, &paths.cwd, flag);
            rows.push(finding(
                "operations",
                if out.0 == 0 { "ok" } else { "error" },
                format!("{name} validation exit {}", out.0),
            ));
        }
        if paths.cwd.join(".oms/memory").is_dir() {
            let out = oms_command_memory::run_with_source(
                &[
                    "--repo".into(),
                    paths.cwd.as_os_str().to_owned(),
                    "health".into(),
                    "--json".into(),
                ],
                &paths.cwd,
                &paths.source,
                flag,
            );
            rows.push(finding(
                "memory",
                if out.0 == 0 { "ok" } else { "error" },
                format!("native memory health exit {}", out.0),
            ));
        }
        let store = oms_command_plan::storage::Store::open(&paths.cwd)
            .map_err(|e| r::Error(e.code, e.message))?;
        if let Some(raw) = store
            .optional(".oms/plan/tasks.json", r::LIMIT)
            .map_err(|e| r::Error(e.code, e.message))?
        {
            r::parse(&raw)?;
            rows.push(finding("plan", "ok", "plan state JSON readable"));
        }
    }
    Ok(rows)
}
fn surfaces(paths: &Paths) -> Result<Vec<Value>> {
    let expected = crate::integrations::expected(paths)?;
    let value = r::value(&paths.claude_settings()?)?;
    let mut rows = Vec::new();
    for s in expected["surfaces"].as_array().unwrap() {
        let event = s["event"].as_str().unwrap();
        let matches = value["hooks"][event].as_array().is_some_and(|groups| {
            groups.iter().any(|g| {
                g.get("matcher").unwrap_or(&Value::Null) == &s["matcher"]
                    && g["hooks"].as_array().is_some_and(|a| {
                        a.iter().any(|h| {
                            h["command"] == s["command"]
                                && h.get("timeout").unwrap_or(&Value::Null) == &s["timeout"]
                        })
                    })
            })
        });
        rows.push(finding(
            "hook surfaces",
            if matches { "ok" } else { "error" },
            format!(
                "{event} {}: {}",
                s["script"].as_str().unwrap(),
                if matches {
                    "registered"
                } else {
                    "missing or drifted"
                }
            ),
        ));
    }
    Ok(rows)
}
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    for key in [
        "--contract",
        "--live-models",
        "--strict-diversity",
        "--root",
    ] {
        if options.has(key) {
            return r::fail(format!("native doctor {key} adapter is not yet available"));
        }
    }
    if options.has("--providers") && options.has("--check") {
        return r::fail(
            "native provider definition/test-reference check adapter is not yet available",
        );
    }

    if options.has("--extensions") || options.has("--mcp") {
        return extensions(paths, options.has("--mcp"), options);
    }
    if options.has("--providers") {
        return providers(paths, options);
    }
    if options.has("--repair") {
        r::child_guard()?;
        let row = paths.load()?;
        let mut repair_paths = paths.clone();
        if let Some(row) = row {
            repair_paths.source = receipt::owner(&row)?;
            if !repair_paths.source.join("rules/global-AGENTS.md").is_file() {
                return r::fail(
                    "canonical receipt owner unavailable; refusing automatic replacement",
                );
            }
        }
        let mut repair = options.clone();
        repair.tool = "link".into();
        repair.flags.remove("--repair");
        crate::install::link(&repair_paths, &repair, flag)?;
    }
    let rows = if options.has("--security") {
        if options.has("--residue") {
            residue(paths, flag)?
        } else {
            security(paths)?
        }
    } else if options.has("--sandbox") {
        sandbox(paths, options, flag)?
    } else if options.has("--surfaces") {
        surfaces(paths)?
    } else if options.has("--tool-lock") {
        tool_lock(paths)?
    } else {
        health(paths, flag)?
    };
    let mut result = report(paths, rows);
    if options.has("--security") {
        result["kind"] = json!("oms-doctor-security");
    }
    if options.has("--sandbox") {
        result["kind"] = json!("oms-doctor-sandbox");
    }
    emit(result, options)
}
pub(crate) fn status(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    if options.has("--fingerprint") {
        return r::json(&crate::fingerprint::build(paths)?);
    }
    if let Some(path) = options.path("--compare", &paths.cwd)? {
        return crate::fingerprint::compare(paths, &path, options.has("--json"));
    }
    r::check(flag)?;
    let receipt = paths.load()?;
    if options.has("--json") {
        return r::json(
            &json!({"schema":1,"install":receipt,"native":true,"source_root":paths.source,"state_root":paths.state}),
        );
    }
    Ok(r::text(format!(
        "oh-my-setting {}\ncanonical install: {}",
        receipt
            .as_ref()
            .and_then(|r| r["version"].as_str())
            .unwrap_or("unknown"),
        receipt
            .as_ref()
            .map(receipt::owner)
            .transpose()?
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not installed".into())
    )))
}
