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
        (
            "antigravity",
            std::env::var_os("OMS_ANTIGRAVITY_SETTINGS")
                .map(PathBuf::from)
                .unwrap_or(paths.home.join(".gemini/antigravity-cli/settings.json")),
        ),
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
                        .any(|x| matches!(x.as_str(), Some("mcp(*)" | "unsandboxed(*)" | "*")))
                }),
                "blanket MCP or unsandboxed allow rule",
            ));
            checks.push((
                value["enableTerminalSandbox"] == false,
                "terminal sandbox explicitly disabled",
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
        match crate::config::read_toml(&config) {
            Ok(value) => {
                for (wide, description) in [
                    (value["approval_policy"] == "never", "approval policy never"),
                    (
                        value["sandbox_mode"] == "danger-full-access",
                        "full host access",
                    ),
                    (
                        value["sandbox_mode"] == "external-sandbox",
                        "external sandbox responsibility",
                    ),
                    (
                        value["sandbox_workspace_write"]["network_access"] == true,
                        "workspace network access",
                    ),
                    (
                        value["shell_environment_policy"]["inherit"] == "all",
                        "complete shell environment inheritance",
                    ),
                    (
                        value["shell_environment_policy"]["ignore_default_excludes"] == true,
                        "default environment exclusions disabled",
                    ),
                ] {
                    rows.push(finding(
                        "codex",
                        if wide { "warn" } else { "info" },
                        format!(
                            "{description}: {}",
                            if wide { "configured" } else { "not observed" }
                        ),
                    ));
                }
                if let Some(profiles) = value["profiles"].as_object() {
                    for (name, profile) in profiles {
                        if profile["approval_policy"] == "never"
                            || profile["sandbox_mode"] == "danger-full-access"
                        {
                            rows.push(finding(
                                "codex",
                                "warn",
                                format!("profile {} widens execution authority", clean(name)),
                            ));
                        }
                    }
                }
            }
            Err(_) => rows.push(finding(
                "codex",
                "unknown",
                "settings could not be parsed; nothing executed",
            )),
        }
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
    rows.extend(policy_baselines(paths)?);
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
        // An explicit OMS_BWRAP_BIN is what workers use; never report a PATH bwrap in its place.
        let available = oms_supervisor::sandbox::find_bwrap()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("OMS_BWRAP_BIN")
                    .is_none()
                    .then(|| r::find("bwrap", &paths.cwd))
                    .flatten()
            });
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
                &binary,
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
            rows.extend(sandbox_ssh_probe(&binary, paths, flag)?);
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
fn sandbox_ssh_probe(binary: &Path, paths: &Paths, flag: &AtomicBool) -> Result<Vec<Value>> {
    use std::ffi::{OsStr, OsString};
    let spec = oms_supervisor::sandbox::SandboxSpec {
        worktree: paths.cwd.clone(),
        extra_rw_dirs: Vec::new(),
        allow_network: true,
    };
    let openssh_dir = paths.home.join(".ssh");
    let runtime_ctl = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join("oms-ssh"))
        .unwrap_or_default();
    let fallback_ctl = PathBuf::from(format!(
        "/tmp/oms-ssh-{}",
        rustix::process::geteuid().as_raw()
    ));
    let auth_sock = std::env::var_os("SSH_AUTH_SOCK")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let check_sock_path = auth_sock.as_deref().is_some_and(Path::is_absolute);
    let script = concat!(
        "empty_or_absent() {\n",
        "  [ ! -e \"$1\" ] && [ ! -L \"$1\" ] && return 0\n",
        "  [ -d \"$1\" ] || return 1\n",
        "  for x in \"$1\"/* \"$1\"/.[!.]* \"$1\"/..?*; do\n",
        "    if [ -e \"$x\" ] || [ -L \"$x\" ]; then return 1; fi\n",
        "  done\n",
        "  return 0\n",
        "}\n",
        "if empty_or_absent \"$1\"; then printf 'openssh=ok\\n'; else printf 'openssh=exposed\\n'; fi\n",
        "if ( [ -z \"$2\" ] || empty_or_absent \"$2\" ) && empty_or_absent \"$3\"; then printf 'control=ok\\n'; else printf 'control=exposed\\n'; fi\n",
        "if [ -z \"${SSH_AUTH_SOCK+x}\" ] && ( [ \"$5\" != \"1\" ] || ( [ ! -e \"$4\" ] && [ ! -L \"$4\" ] ) || [ -c \"$4\" ] ); then printf 'agent=ok\\n'; else printf 'agent=exposed\\n'; fi\n",
    );
    let sh_args: Vec<OsString> = vec![
        "-c".into(),
        script.into(),
        "oms-doctor-ssh-probe".into(),
        openssh_dir.into_os_string(),
        runtime_ctl.into_os_string(),
        fallback_ctl.into_os_string(),
        // Probe the resolved socket: a hidden symlink can still point at a reachable target.
        auth_sock
            .as_deref()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
            .unwrap_or_default()
            .into_os_string(),
        if check_sock_path { "1" } else { "0" }.into(),
    ];
    let wrapped = oms_supervisor::sandbox::bwrap_args(OsStr::new("/bin/sh"), &sh_args, &spec);
    let injected_env: &[(&str, &OsStr)] = if auth_sock.is_none() {
        &[("SSH_AUTH_SOCK", OsStr::new("/tmp/oms-doctor-probe.sock"))]
    } else {
        &[]
    };
    let probe = r::command_env(
        binary,
        &wrapped,
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(5),
        injected_env,
    )?;
    let stdout = String::from_utf8_lossy(&probe.1);
    let checks = [
        ("openssh", "OpenSSH directory (.ssh)", "hidden"),
        ("control", "OMS SSH control directory (oms-ssh)", "hidden"),
        ("agent", "ssh-agent (SSH_AUTH_SOCK)", "isolated"),
    ];
    Ok(checks
        .into_iter()
        .map(|(tag, label, ok_word)| {
            if probe.0 != 0 {
                finding(
                    "sandbox",
                    "warn",
                    format!("{label}: unverified (bwrap probe exit {})", probe.0),
                )
            } else if stdout.lines().any(|line| line == format!("{tag}=ok")) {
                finding("sandbox", "ok", format!("{label}: {ok_word}"))
            } else {
                finding("sandbox", "warn", format!("{label}: exposed"))
            }
        })
        .collect())
}
fn extension_configs(paths: &Paths) -> Vec<(&'static str, &'static str, PathBuf)> {
    vec![
        ("claude", "user", paths.home.join(".claude.json")),
        ("claude", "project", paths.cwd.join(".mcp.json")),
        (
            "antigravity",
            "legacy",
            paths.home.join(".gemini/config/mcp_config.json"),
        ),
        (
            "antigravity",
            "user",
            paths.home.join(".gemini/settings.json"),
        ),
        (
            "antigravity",
            "project",
            paths.cwd.join(".gemini/mcp_config.json"),
        ),
    ]
}
struct DoctorSpec {
    key: String,
    provider: String,
    scope: String,
    name: String,
    spec: Value,
}
#[derive(Default)]
struct DoctorSpecs {
    servers: Vec<DoctorSpec>,
    unknown: Vec<String>,
    layers: Vec<Value>,
}
impl DoctorSpecs {
    fn layer(&mut self, provider: &str, scope: &str, path: &Path, toml: bool) {
        let state = match fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing",
            Err(_) => "unknown",
            Ok(_) => {
                let value = if toml {
                    crate::config::read_toml(path)
                } else {
                    r::value(path)
                };
                match value {
                    Ok(value) if value.is_object() => {
                        match value.get(if toml { "mcp_servers" } else { "mcpServers" }) {
                            None => "parsed",
                            Some(Value::Object(servers)) => {
                                let mut state = "parsed";
                                for (name, spec) in servers {
                                    if spec.is_object() {
                                        // Preserve the existing pin key spelling for plugins;
                                        // use original names separately for collision identity.
                                        let key_name = if scope.starts_with("plugin-") {
                                            clean(name)
                                        } else {
                                            name.clone()
                                        };
                                        self.servers.push(DoctorSpec {
                                            key: format!("{provider}:{scope}:{key_name}"),
                                            provider: provider.into(),
                                            scope: scope.into(),
                                            name: name.clone(),
                                            spec: spec.clone(),
                                        });
                                    } else {
                                        state = "unknown";
                                    }
                                }
                                state
                            }
                            Some(_) => "unknown",
                        }
                    }
                    _ => "unknown",
                }
            }
        };
        self.layer_state(provider, scope, state);
    }
    fn layer_state(&mut self, provider: &str, scope: &str, state: &str) {
        if state == "unknown" {
            self.unknown.push(provider.into());
        }
        self.layers.push(json!({"provider":provider,"scope":clean(scope),"status":state,"reason":match state {
            "parsed" => "configuration parsed; runtime selection not checked",
            "missing" => "file not found; runtime selection unknown",
            _ => "configuration unsafe, unreadable, or malformed; declarations may be incomplete",
        }}));
    }
    fn collisions(&self) -> Vec<Value> {
        let mut groups = BTreeMap::<(&str, &str), Vec<&str>>::new();
        for server in &self.servers {
            groups
                .entry((&server.provider, &server.name))
                .or_default()
                .push(&server.scope);
        }
        groups
            .into_iter()
            .filter_map(|((provider, name), mut scopes)| {
                scopes.sort_unstable();
                scopes.dedup();
                (scopes.len() > 1).then(|| json!({
                    "provider":provider,"name":clean(name),"scopes":scopes,
                    "effective":"unknown","reason":"scope precedence and runtime selection not established"
                }))
            })
            .collect()
    }
}
fn specs(paths: &Paths) -> Result<DoctorSpecs> {
    let mut inventory = DoctorSpecs::default();
    for (provider, scope, path) in extension_configs(paths) {
        inventory.layer(provider, scope, &path, false);
    }
    let plugins = paths.home.join(".gemini/config/plugins");
    match fs::read_dir(&plugins) {
        Ok(entries) => {
            for entry in entries {
                let Ok(entry) = entry else {
                    inventory.layer_state("antigravity", "plugins", "unknown");
                    continue;
                };
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => inventory.layer(
                        "antigravity",
                        &format!("plugin-{}", clean(&entry.file_name().to_string_lossy())),
                        &entry.path().join("mcp_config.json"),
                        false,
                    ),
                    Ok(_) => (),
                    Err(_) => inventory.layer_state("antigravity", "plugins", "unknown"),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            inventory.layer_state("antigravity", "plugins", "missing");
        }
        Err(_) => inventory.layer_state("antigravity", "plugins", "unknown"),
    }
    inventory.layer("codex", "user", &paths.codex_config()?, true);
    inventory.unknown.sort();
    inventory.unknown.dedup();
    inventory.layers.sort_by_key(|row| {
        (
            row["provider"].as_str().unwrap().to_owned(),
            row["scope"].as_str().unwrap().to_owned(),
        )
    });
    Ok(inventory)
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
fn risks(spec: &Value) -> Vec<&'static str> {
    let mut rows = Vec::new();
    let command = spec["command"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    let name = Path::new(command)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let args = spec["args"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let package = args.iter().find(|a| !a.starts_with('-')).copied();
    if matches!(name, "npx" | "bunx" | "pnpx")
        && package
            .is_some_and(|s| !s.trim_start_matches('@').contains('@') || s.ends_with("@latest"))
    {
        rows.push("package launcher uses an unpinned version");
    }
    if name == "uvx"
        && package.is_some_and(|s| !s.contains("=="))
        && !args.iter().any(|s| s.contains("=="))
    {
        rows.push("uvx uses an unpinned version");
    }
    if let Some(url) = spec["url"]
        .as_str()
        .or_else(|| spec["httpUrl"].as_str())
        .or_else(|| spec["serverUrl"].as_str())
        && let Some(rest) = url.strip_prefix("http://")
    {
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("");
        let local = host == "[::1]"
            || host.starts_with("[::1]:")
            || matches!(host.split(':').next(), Some("localhost" | "127.0.0.1"));
        if !local {
            rows.push("plain HTTP to a non-loopback host");
        }
    }
    rows
}
fn plugin_inventory(paths: &Paths) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for (scope, file) in [
        ("user", paths.claude_settings()?),
        ("project", paths.cwd.join(".claude/settings.json")),
    ] {
        if !r::exists(&file) {
            continue;
        }
        match r::value(&file) {
            Ok(v) => {
                if let Some(plugins) = v["enabledPlugins"].as_object() {
                    for (name, enabled) in plugins {
                        rows.push(json!({"provider":"claude","name":clean(name),"scope":scope,"enabled":enabled!=false,"owned":name.split('@').next()==Some("oh-my-setting")}));
                    }
                }
            }
            Err(_) => rows.push(json!({"provider":"claude","scope":scope,"status":"unread"})),
        }
    }
    for (scope, base) in [
        ("plugin", paths.home.join(".gemini/config/plugins")),
        ("extension", paths.home.join(".gemini/extensions")),
    ] {
        if !base.is_dir() {
            continue;
        }
        for entry in fs::read_dir(base)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') {
                    rows.push(json!({"provider":"antigravity","name":clean(&name),"scope":scope,"enabled":true,"owned":name=="oh-my-setting"}));
                }
            }
        }
    }
    Ok(rows)
}
fn extensions(
    paths: &Paths,
    pins: bool,
    options: &Options,
    flag: &AtomicBool,
) -> Result<crate::Output> {
    if options.has("--pin") {
        r::child_guard()?;
    }
    let inventory = specs(paths)?;
    let specs = &inventory.servers;
    let unknown = &inventory.unknown;
    let collisions = inventory.collisions();
    let mut rows = Vec::new();
    let mut current = serde_json::Map::new();
    for server in specs {
        let (key, spec) = (&server.key, &server.spec);
        let mut env_names = spec["env"]
            .as_object()
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        env_names.sort();
        let material = json!({"type":spec.get("type").or_else(||spec.get("transport")),"command":spec.get("command"),"args":spec.get("args"),"url":spec.get("url").or_else(||spec.get("httpUrl")).or_else(||spec.get("serverUrl")),"env_names":env_names});
        let compact = r::canonical_ascii(&material)?;
        current.insert(key.clone(), json!(r::hash(&compact)));
        rows.push(json!({"server":clean(key),"transport":if spec["command"].is_string(){"stdio"}else{"http"},"target":safe_target(spec),"env_names":env_names.len(),"disabled":spec["disabled"]==true || spec["enabled"]==false,"configuration":if spec["disabled"]==true || spec["enabled"]==false{"disabled"}else{"configured"},"effective":"unknown","effective_reason":if unknown.iter().any(|provider| key.starts_with(&format!("{provider}:"))){"provider configuration incomplete"}else{"scope precedence and runtime selection not established"},"runtime":"unknown","risks":risks(spec)}));
    }
    let plugins = plugin_inventory(paths)?;
    if !pins {
        return r::json(
            &json!({"schema":1,"kind":"oms-doctor-extensions","servers":rows,"plugins":plugins,"unknown":unknown,"config_layers":inventory.layers,"collisions":collisions}),
        );
    }
    let probed = if let Some(name) = options.get("--probe-tools") {
        let selected = specs
            .iter()
            .filter(|server| server.key == name || server.name == name)
            .collect::<Vec<_>>();
        if selected.len() != 1 {
            return r::fail("--probe-tools must name exactly one configured server");
        }
        let server = selected[0];
        Some((
            server.key.clone(),
            crate::mcp_probe::cached(&server.spec, &paths.cwd, flag)?,
        ))
    } else {
        None
    };
    let path = std::env::var_os("OMS_MCP_PIN_FILE")
        .map(PathBuf::from)
        .unwrap_or(paths.config.join("mcp-pins.json"));
    let _guard = if options.has("--pin") {
        r::child_guard()?;
        Some(r::lock(&path, &paths.cwd)?)
    } else {
        None
    };
    let previous = if r::exists(&path) {
        let row = r::value(&path)?;
        if row["schema"] != 1
            || !row["pins"].is_object()
            || row.get("tools").is_some_and(|v| !v.is_object())
        {
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
        // Unknown providers keep their existing baseline, never a false removal.
        for (k, v) in old {
            if unknown.iter().any(|p| k.starts_with(&format!("{p}:"))) {
                current.insert(k.clone(), v.clone());
            }
        }
        let mut tools = previous
            .get("tools")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(json!({}));
        tools
            .as_object_mut()
            .unwrap()
            .retain(|k, _| current.contains_key(k));
        if let Some((key, hashes)) = &probed {
            tools[key] = hashes.clone();
        }
        let row = json!({"schema":1,"pins":current,"tools":tools});
        r::atomic(&path, &r::bytes(&row)?)?;
    }
    let probe = probed.map(|(key, hashes)| {
        let now = hashes.as_object().unwrap();
        let old = previous["tools"][&key].as_object();
        let names = |kind: &str| now.iter().filter(|(k,v)| match kind {"new" => old.is_some_and(|o| !o.contains_key(*k)), _ => old.and_then(|o| o.get(*k)).is_some_and(|p| p != *v)}).map(|(k,_)| clean(k)).collect::<Vec<_>>();
        json!({"server":key,"tools":now.len(),"baseline":if old.is_some(){"recorded"}else{"none"},"added":names("new"),"changed":names("changed"),"removed":old.map(|o| o.keys().filter(|k| !now.contains_key(*k)).map(|k| clean(k)).collect::<Vec<_>>()).unwrap_or_default()})
    });
    r::json(
        &json!({"schema":1,"kind":"oms-mcp-pins","servers":rows,"new":added,"changed":changed,"removed":removed,"unknown":unknown,"config_layers":inventory.layers,"collisions":collisions,"pinned":options.has("--pin"),"probe":probe}),
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
    if options.has("--check") {
        let mut rooted = paths.clone();
        if let Some(root) = options.path("--root", &paths.cwd)? {
            rooted.source = r::physical(&root)?;
        }
        let result = crate::provider_check::check(&table, &rooted)?;
        let mut out = r::json(&result)?;
        out.0 = i32::from(result["ok"] != true);
        return Ok(out);
    }
    r::json(&table)
}
fn tool_lock(paths: &Paths) -> Result<Vec<Value>> {
    let explicit = std::env::var_os("OH_MY_SETTING_TOOL_LOCK").map(PathBuf::from);
    let path = explicit
        .as_ref()
        .map(|path| paths.cwd.join(path))
        .unwrap_or(paths.data.join(crate::supply::RELEASE_RECEIPT));
    if explicit.is_none() && !r::exists(&path) {
        return Ok(vec![finding(
            "tool releases",
            "info",
            "no release observations recorded; managed tools resolve current upstream releases when installed or updated",
        )]);
    }
    let table = r::value(&path)?;
    if table["schema"] != 2 {
        return r::fail("unsupported tool release record schema");
    }
    fn validate(v: &Value) -> Result<()> {
        match v {
            Value::Object(o) => {
                for (k, v) in o {
                    if matches!(k.as_str(), "sha256" | "sha512")
                        && v.as_str().is_none_or(|s| {
                            let length = if k == "sha256" { 64 } else { 128 };
                            s.len() != length || !s.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                    {
                        return r::fail("invalid recorded payload digest");
                    }
                    if k == "integrity"
                        && v.as_str().is_none_or(|s| {
                            s.strip_prefix("sha512-").is_none_or(|digest| {
                                digest.len() != 88
                                    || !digest.ends_with("==")
                                    || !digest.as_bytes()[..86].iter().all(|b| {
                                        b.is_ascii_alphanumeric() || matches!(*b, b'+' | b'/')
                                    })
                            })
                        })
                    {
                        return r::fail("invalid recorded npm integrity");
                    }
                    if k == "url"
                        && v.as_str()
                            .is_none_or(|s| !s.starts_with("https://") || s.contains(['\r', '\n']))
                    {
                        return r::fail("recorded download URL must use HTTPS");
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
        "tool releases",
        "ok",
        if explicit.is_some() {
            "explicit tool pin schema, HTTPS URLs and digest fields validated; installed bytes and upstream freshness were not checked"
        } else {
            "release observation schema, HTTPS URLs and digest fields validated; observations do not pin future updates or prove installed versions"
        },
    )])
}
/// Read-only report of registered provider accounts: home safety and
/// ownership-receipt health. Never mutates and never contacts a provider.
fn accounts(paths: &Paths) -> Vec<Value> {
    use std::os::unix::fs::MetadataExt;
    const SECTION: &str = "provider accounts";
    const LIMIT: usize = 64;
    let load = |name: &str| -> Option<std::result::Result<Value, ()>> {
        let path = paths.config.join(name);
        r::exists(&path).then(|| r::value(&path).map_err(|_| ()))
    };
    let mut rows = Vec::new();
    let reg_opt = load("provider-accounts.json");
    let claude_registry = load("provider-accounts.claude.json");
    let receipt_meta = [
        ("codex", "codex-account-links.json", "link receipt"),
        ("pi", "pi-account-links.json", "link receipt"),
        ("claude", "provider-account-effects.json", "effects receipt"),
    ];
    let raw_receipts = receipt_meta.map(|(_, name, _)| load(name));
    if reg_opt.is_none() && claude_registry.is_none() && raw_receipts.iter().all(Option::is_none) {
        return rows;
    }
    let registry = match reg_opt {
        None => None,
        Some(Ok(v)) if v["schema"] == 1 && v["providers"].is_object() => Some(v),
        Some(_) => {
            return vec![finding(
                SECTION,
                "warn",
                "provider-accounts.json is unreadable",
            )];
        }
    };
    if let Some(result) = &claude_registry
        && !matches!(result, Ok(doc) if doc["schema"] == 1 && doc["provider"] == "claude" && doc["accounts"].is_array())
    {
        rows.push(finding(
            SECTION,
            "warn",
            "Claude account registry unreadable or invalid",
        ));
    }
    for (index, receipt) in raw_receipts.iter().enumerate() {
        let (_, file_name, _) = receipt_meta[index];
        match receipt {
            None => (),
            Some(Err(())) => {
                rows.push(finding(
                    SECTION,
                    "warn",
                    format!("{file_name} receipt unreadable"),
                ));
            }
            Some(Ok(doc)) => {
                let valid = if index == 2 {
                    doc["schema"] == 1 && doc["accounts"].is_array()
                } else {
                    doc.is_array()
                };
                if !valid {
                    rows.push(finding(SECTION, "warn", "account receipt schema invalid"));
                }
            }
        }
    }
    let receipts = raw_receipts
        .iter()
        .map(|entry| entry.as_ref().map(|res| res.as_ref().ok().cloned()))
        .collect::<Vec<_>>();
    let claude_fallback = match &claude_registry {
        Some(Ok(doc)) if doc["schema"] == 1 && doc["provider"] == "claude" => {
            doc["accounts"].as_array()
        }
        _ => None,
    };
    let mut visited: Vec<(&'static str, Value, Value)> = Vec::new();
    let mut seen = 0;
    for provider in ["codex", "claude", "pi"] {
        let Some(list) = registry
            .as_ref()
            .and_then(|reg| reg["providers"][provider]["accounts"].as_array())
            .or_else(|| (provider == "claude").then_some(claude_fallback).flatten())
        else {
            continue;
        };
        let (index, label) = match provider {
            "codex" => (0, "link receipt"),
            "pi" => (1, "link receipt"),
            _ => (2, "effects receipt"),
        };
        for account in list.iter().filter(|a| a["id"] != "default") {
            seen += 1;
            if seen > LIMIT {
                rows.push(finding(SECTION, "note", "account report truncated"));
                return rows;
            }
            visited.push((provider, account["id"].clone(), account["home"].clone()));
            let id = clean(account["id"].as_str().unwrap_or("?"));
            let home = account["home"].as_str().map(PathBuf::from);
            let mut problems = Vec::new();
            match home.as_ref().map(fs::symlink_metadata) {
                Some(Ok(m)) if m.is_dir() => {
                    if m.uid() != rustix::process::geteuid().as_raw() {
                        problems.push("home is not user-owned".to_owned());
                    }
                    if m.mode() & 0o022 != 0 {
                        problems.push(format!(
                            "home mode {:o} is group/other-writable",
                            m.mode() & 0o777
                        ));
                    }
                }
                Some(Ok(_)) => problems.push("home is not a directory".into()),
                _ => problems.push("home is missing".into()),
            }
            let row = match &receipts[index] {
                None => None,
                Some(None) => {
                    problems.push(format!("{label} unreadable"));
                    None
                }
                Some(Some(doc)) => doc
                    .as_array()
                    .or_else(|| doc["accounts"].as_array())
                    .and_then(|rows| {
                        rows.iter()
                            .find(|r| r["id"] == account["id"] && r["home"] == account["home"])
                    }),
            };
            match row {
                None if !problems.iter().any(|p| p.ends_with("unreadable")) => {
                    problems.push(format!("no {label}"));
                }
                None => (),
                Some(row) => {
                    let expected = if provider == "claude" {
                        &row["identity"]
                    } else {
                        row
                    };
                    if let Some(Ok(meta)) = home.as_ref().map(fs::symlink_metadata)
                        && (expected["st_dev"].as_u64() != Some(meta.dev())
                            || expected["st_ino"].as_u64() != Some(meta.ino()))
                    {
                        problems.push("home identity changed".into());
                    }
                    if provider == "claude"
                        && !matches!(&claude_registry, Some(Ok(doc)) if doc["schema"] == 1 && doc["provider"] == "claude" && doc["accounts"].as_array().is_some_and(|entries| entries.iter().any(|entry| entry["id"] == account["id"] && entry["home"] == account["home"])))
                    {
                        problems.push("Claude registry entry missing or invalid".into());
                    }
                    let allowed_links: &[&str] = match provider {
                        "claude" => &["CLAUDE.md", "skills", "output-styles/oms-korean.md"],
                        "pi" => &["AGENTS.md", "skills"],
                        _ => &[
                            "AGENTS.md",
                            "config.toml",
                            "hooks.json",
                            ".hook-trust",
                            "plugins",
                            "rules",
                            "skills",
                        ],
                    };
                    match row["links"].as_object() {
                        None => problems.push("receipt links missing or invalid".into()),
                        Some(links) => {
                            let broken = links
                                .iter()
                                .filter(|(name, rec)| {
                                    let target = if provider == "claude" {
                                        rec["target"].as_str()
                                    } else {
                                        rec.as_str()
                                    };
                                    let path = allowed_links
                                        .contains(&name.as_str())
                                        .then(|| home.as_ref().map(|h| h.join(name)))
                                        .flatten();
                                    !path.zip(target).is_some_and(|(p, t)| {
                                        fs::read_link(&p).is_ok_and(|actual| actual == Path::new(t))
                                            && (provider != "claude"
                                                || fs::symlink_metadata(&p).is_ok_and(|m| {
                                                    rec["identity"]["st_uid"].as_u64()
                                                        == Some(m.uid() as u64)
                                                        && rec["identity"]["st_dev"].as_u64()
                                                            == Some(m.dev())
                                                        && rec["identity"]["st_ino"].as_u64()
                                                            == Some(m.ino())
                                                }))
                                    })
                                })
                                .count();
                            if broken > 0 {
                                problems.push(format!("{broken} owned link(s) missing or changed"));
                            }
                        }
                    }
                }
            }
            rows.push(if problems.is_empty() {
                finding(
                    SECTION,
                    "ok",
                    format!("{provider}:{id} home private; {label} ok"),
                )
            } else {
                finding(
                    SECTION,
                    "warn",
                    format!("{provider}:{id} {}", clean(&problems.join("; "))),
                )
            });
        }
    }
    for (index, receipt) in receipts.iter().enumerate() {
        let (provider, _, label) = receipt_meta[index];
        let Some(Some(doc)) = receipt else {
            continue;
        };
        let Some(list) = (if index == 2 {
            doc["accounts"].as_array()
        } else {
            doc.as_array()
        }) else {
            continue;
        };
        for row in list.iter().filter(|r| r["id"] != "default") {
            let already_reported = visited
                .iter()
                .any(|(p, id, home)| *p == provider && *id == row["id"] && *home == row["home"]);
            if already_reported {
                continue;
            }
            seen += 1;
            if seen > LIMIT {
                rows.push(finding(SECTION, "note", "account report truncated"));
                return rows;
            }
            let id = clean(row["id"].as_str().unwrap_or("?"));
            rows.push(finding(
                SECTION,
                "warn",
                format!("{provider}:{id} orphaned {label} without registry entry"),
            ));
        }
    }
    rows
}
fn source_matches(source: &Path, target: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return r::fail("managed source is not a regular file or directory");
    }
    let source_digest = crate::transaction::identity(source)?
        .ok_or_else(|| r::Error(2, "managed source disappeared".into()))?;
    if fs::symlink_metadata(target)?.file_type().is_symlink() {
        if !crate::install::owned(source, target)? {
            return r::fail("managed link changed during diagnosis");
        }
        return Ok(true);
    }
    let target_digest = crate::transaction::identity(target)?
        .ok_or_else(|| r::Error(2, "managed target disappeared".into()))?;
    Ok(source_digest == target_digest)
}

fn managed_inventory(paths: &Paths, source: &Path, target: &Path) -> Result<Value> {
    let owned = crate::install::owned(source, target)?;
    let (freshness, level, message) = if !owned {
        ("not checked", "error", "missing, modified or foreign")
    } else {
        match source_matches(source, target) {
            Ok(true) => ("current", "ok", "owned; current source matches"),
            Ok(false) => (
                "stale",
                "error",
                "owned copy is stale; run oms doctor --repair",
            ),
            Err(_) => (
                "unknown",
                "error",
                "owned; source freshness could not be verified (unsafe, missing or unreadable content)",
            ),
        }
    };
    let target = target.strip_prefix(&paths.home).unwrap_or(target).display();
    let mut row = finding("managed inventory", level, format!("{target}: {message}"));
    row["target"] = json!(target.to_string());
    row["owned"] = json!(owned);
    row["source_freshness"] = json!(freshness);
    Ok(row)
}

struct RunningMcpServer {
    pid: u32,
    home: PathBuf,
    revision: String,
    age: Option<String>,
    profile: Option<String>,
    panel_version: Option<String>,
}

fn proc_file_bytes(
    dir: rustix::fd::BorrowedFd<'_>,
    name: &str,
    uid: Option<u32>,
    limit: usize,
) -> Option<Vec<u8>> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let fd = rustix::fs::openat(
        dir,
        name,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let file = fs::File::from(fd);
    let meta = file.metadata().ok()?;
    if !meta.is_file() || uid.is_some_and(|expected| meta.uid() != expected) {
        return None;
    }
    let mut raw = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut raw).ok()?;
    (raw.len() <= limit).then_some(raw)
}

fn parse_uptime_centis(proc_fd: rustix::fd::BorrowedFd<'_>) -> Option<u64> {
    let raw = proc_file_bytes(proc_fd, "uptime", None, 256)?;
    let first = std::str::from_utf8(&raw).ok()?.split_whitespace().next()?;
    let (secs, frac) = first.split_once('.').unwrap_or((first, ""));
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let secs = secs.parse::<u64>().ok()?;
    let mut digits = frac.bytes().take(2);
    let d1 = digits.next().unwrap_or(b'0');
    let d2 = digits.next().unwrap_or(b'0');
    let centis = u64::from(d1 - b'0') * 10 + u64::from(d2 - b'0');
    secs.checked_mul(100)?.checked_add(centis)
}

fn parse_process_age(
    pid_fd: rustix::fd::BorrowedFd<'_>,
    uid: u32,
    uptime_centis: Option<u64>,
) -> Option<String> {
    let uptime = uptime_centis?;
    let raw = proc_file_bytes(pid_fd, "stat", Some(uid), 4096)?;
    let text = std::str::from_utf8(&raw).ok()?;
    let start_ticks = text
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)?
        .parse::<u64>()
        .ok()?;
    if start_ticks > uptime.saturating_add(100) {
        return None;
    }
    let secs = uptime.saturating_sub(start_ticks) / 100;
    Some(if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    })
}

fn match_mcp_server_cmdline(raw: &[u8]) -> Option<(PathBuf, String)> {
    use std::os::unix::ffi::OsStrExt;
    let body = raw.strip_suffix(b"\0").unwrap_or(raw);
    if body.is_empty() {
        return None;
    }
    let argv = body.split(|&b| b == 0).collect::<Vec<_>>();
    let [program, b"--oms-dispatch", home, rev, b"--", b"mcp-server"] = argv.as_slice() else {
        return None;
    };
    if program.is_empty() || home.is_empty() {
        return None;
    }
    let rev = std::str::from_utf8(rev).ok()?;
    crate::payload::valid_revision(rev).ok()?;
    let home = std::ffi::OsStr::from_bytes(home);
    Some((PathBuf::from(home), rev.to_owned()))
}

fn clean_env_scalar(raw: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(raw).ok()?;
    if text.is_empty() {
        return None;
    }
    Some(
        text.chars()
            .take(64)
            .map(|c| {
                if c.is_control() || c.is_whitespace() {
                    '?'
                } else {
                    c
                }
            })
            .collect(),
    )
}

fn parse_mcp_environ(raw: &[u8]) -> (Option<String>, Option<String>) {
    let mut profile = None;
    let mut panel_version = None;
    for entry in raw.split(|&b| b == 0) {
        if let Some(value) = entry.strip_prefix(b"OMS_MCP_TOOL_PROFILE=") {
            profile = clean_env_scalar(value);
        } else if let Some(value) = entry.strip_prefix(b"OMS_MCP_PANEL_RESOURCE_VERSION=") {
            panel_version = clean_env_scalar(value);
        }
    }
    (profile, panel_version)
}

fn scan_mcp_servers(proc_root: &Path) -> Option<Vec<RunningMcpServer>> {
    use std::os::unix::fs::MetadataExt;
    let proc_fd = rustix::fs::open(
        proc_root,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let entries = fs::read_dir(proc_root).ok()?;
    let uid = rustix::process::geteuid().as_raw();
    let uptime = parse_uptime_centis(rustix::fd::AsFd::as_fd(&proc_fd));
    let mut servers = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        if pid_str.is_empty() || !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        let Ok(pid_fd) = rustix::fs::openat(
            &proc_fd,
            pid_str,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) else {
            continue;
        };
        let pid_dir = fs::File::from(pid_fd);
        let Ok(meta) = pid_dir.metadata() else {
            continue;
        };
        if !meta.is_dir() || meta.uid() != uid {
            continue;
        }
        let borrowed = rustix::fd::AsFd::as_fd(&pid_dir);
        let Some(cmdline) = proc_file_bytes(borrowed, "cmdline", Some(uid), 64 * 1024) else {
            continue;
        };
        let Some((home, revision)) = match_mcp_server_cmdline(&cmdline) else {
            continue;
        };
        let (profile, panel_version) = proc_file_bytes(borrowed, "environ", Some(uid), 1024 * 1024)
            .map(|raw| parse_mcp_environ(&raw))
            .unwrap_or((None, None));
        let age = parse_process_age(borrowed, uid, uptime);
        servers.push(RunningMcpServer {
            pid,
            home,
            revision,
            age,
            profile,
            panel_version,
        });
    }
    servers.sort_by_key(|s| s.pid);
    Some(servers)
}

/// Read-only inspection of running OMS MCP servers for the current user.
/// Never signals, kills, or restarts any process.
fn mcp_servers_at(paths: &Paths, proc_root: Option<&Path>) -> Vec<Value> {
    const SECTION: &str = "mcp servers";
    const STALE_LIMIT: usize = 20;
    let Some(proc_root) = proc_root else {
        return vec![finding(
            SECTION,
            "info",
            "running OMS MCP servers were not inspected on this platform",
        )];
    };
    let Some(servers) = scan_mcp_servers(proc_root) else {
        return vec![finding(
            SECTION,
            "info",
            "running OMS MCP servers could not be inspected",
        )];
    };
    let row = paths.load().ok().flatten();
    let installed = row
        .as_ref()
        .and_then(|row| receipt::native_revision(row).map(str::to_owned));
    // Servers dispatched from another install root belong to that install, not this one;
    // without an owner and revision there is nothing host-independent to compare.
    let (Some(owner), Some(installed)) = (
        row.as_ref().and_then(|row| receipt::owner(row).ok()),
        installed,
    ) else {
        return vec![finding(
            SECTION,
            "info",
            "running OMS MCP servers not compared; install receipt unavailable",
        )];
    };
    let servers = servers
        .into_iter()
        .filter(|server| r::physical(&server.home).is_ok_and(|home| home == owner))
        .collect::<Vec<_>>();
    let stale = servers
        .iter()
        .filter(|server| server.revision != installed)
        .collect::<Vec<_>>();
    if stale.is_empty() {
        let count = servers.len();
        let message = if count == 0 {
            "0 running OMS MCP servers".to_owned()
        } else {
            format!(
                "{count} running OMS MCP server(s) at installed revision {}",
                &installed[..12]
            )
        };
        return vec![finding(SECTION, "ok", message)];
    }
    let installed_short = &installed[..12];
    let mut rows = Vec::new();
    for server in stale.iter().take(STALE_LIMIT) {
        let mut details = vec![format!("installed {installed_short}")];
        if let Some(age) = &server.age {
            details.push(format!("age {age}"));
        }
        if let Some(profile) = &server.profile {
            details.push(format!("OMS_MCP_TOOL_PROFILE={profile}"));
        }
        if let Some(panel) = &server.panel_version {
            details.push(format!("OMS_MCP_PANEL_RESOURCE_VERSION={panel}"));
        }
        rows.push(finding(
            SECTION,
            "warn",
            format!(
                "pid {} serves older build {} ({}); reconnect or restart the host's OMS MCP server for that conversation; ending only pid {} makes the host start a fresh server on its next call",
                server.pid,
                &server.revision[..12],
                details.join(", "),
                server.pid,
            ),
        ));
    }
    if stale.len() > STALE_LIMIT {
        let remaining = stale.len() - STALE_LIMIT;
        rows.push(finding(
            SECTION,
            "warn",
            format!(
                "{remaining} additional stale OMS MCP server(s) not listed; reconnect or restart those host OMS MCP servers"
            ),
        ));
    }
    rows
}

fn health(paths: &Paths, flag: &AtomicBool, services: &dyn crate::Services) -> Result<Vec<Value>> {
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
    let selection = crate::fingerprint::capability_provenance(paths);
    let (level, message) = match selection["status"].as_str() {
        Some("recorded") => (
            "info",
            "installed tool capability selection recorded; live provider selection unknown",
        ),
        Some("missing") => ("note", "no installed tool capability selection recorded"),
        _ => (
            "error",
            "capability receipt is invalid or unreadable; configured selection unknown",
        ),
    };
    let mut row = finding("capability selection", level, message);
    row["selection"] = selection;
    rows.push(row);
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
    rows.extend(mcp_servers_at(
        paths,
        cfg!(target_os = "linux").then(|| Path::new("/proc")),
    ));
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
    rows.extend(accounts(paths));
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
            .map_err(|e| r::Error(e.code, e.message))?
            .with_group_resolver(services.group_resolver());
        if let Some(raw) = store
            .optional(".oms/plan/tasks.json", r::LIMIT)
            .map_err(|e| r::Error(e.code, e.message))?
        {
            r::parse(&raw)?;
            rows.push(finding("plan", "ok", "plan state JSON readable"));
        }
    }
    if let Some(row) = paths.load()? {
        let mut owner_paths = paths.clone();
        owner_paths.source = receipt::owner(&row)?;
        let Some(frontdoor) = row["native"]["frontdoor"].as_str().map(PathBuf::from) else {
            rows.push(finding(
                "managed inventory",
                "error",
                "native frontdoor missing; run oms doctor --repair",
            ));
            return Ok(rows);
        };
        for (source, target) in crate::install::targets(&owner_paths, &frontdoor)? {
            rows.push(managed_inventory(paths, &source, &target)?);
        }
    }
    Ok(rows)
}
fn surfaces(paths: &Paths) -> Result<Vec<Value>> {
    let expected = crate::integrations::expected(paths)?;
    let value = paths.claude_settings().and_then(|path| r::value(&path));
    let configuration_read = value.is_ok();
    let value = value.unwrap_or(Value::Null);
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
                if !configuration_read {
                    "configuration unavailable"
                } else if matches {
                    "registered"
                } else {
                    "missing or drifted"
                }
            ),
        ));
    }
    Ok(rows)
}

fn codex_mcp_finding(paths: &Paths, codex_config: &Path) -> Value {
    if !r::exists(codex_config) {
        return finding(
            "codex_mcp",
            "error",
            "recorded integration is missing, changed, or unproven",
        );
    }
    let mut dry_run = Options::default();
    dry_run.tool = "install-mcp".into();
    match r::read(codex_config, r::LIMIT)
        .and_then(|raw| crate::integrations::converge_codex_mcp(paths, &dry_run, &raw))
    {
        Ok((Some(current), _)) if current["args"] == json!(["mcp-server"]) => {
            finding("codex_mcp", "ok", "recorded integration matches")
        }
        Ok(_) => finding(
            "codex_mcp",
            "error",
            "recorded integration is missing, changed, or unproven",
        ),
        Err(err) => finding(
            "codex_mcp",
            "error",
            format!(
                "{}; hint: move or delete the outside table by hand",
                clean(&err.1)
            ),
        ),
    }
}

fn integrations(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    integrations_at(paths, options, flag, &paths.codex_config()?)
}

pub(crate) fn integrations_at(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    codex_config: &Path,
) -> Result<crate::Output> {
    let receipt = paths
        .require_owner()?
        .ok_or_else(|| r::Error(2, "integration checks require an install receipt".into()))?;
    let components = &receipt["components"];
    let mut rows = Vec::new();
    let record = |component: &str, enabled: bool, healthy: bool| {
        let (level, message) = if !enabled {
            ("note", "not recorded; skipped")
        } else if healthy {
            ("ok", "recorded integration matches")
        } else {
            (
                "error",
                "recorded integration is missing, changed, or unproven",
            )
        };
        finding(component, level, message)
    };
    r::check(flag)?;
    let hooks = components["claude_hooks"] == true;
    rows.push(record(
        "claude_hooks",
        hooks,
        !hooks || surfaces(paths).is_ok_and(|rows| rows.iter().all(|row| row["level"] != "error")),
    ));
    let codex = components["codex_plugin"] == true;
    rows.push(record(
        "codex_plugin",
        codex,
        !codex || crate::plugins::codex_healthy(paths, flag).unwrap_or(false),
    ));
    r::check(flag)?;
    let agy = components["agy_plugin"] == true;
    let agy_owned = !agy
        || crate::install::owned(
            &paths.source.join("plugins/antigravity"),
            &paths.home.join(".gemini/config/plugins/oh-my-setting"),
        )
        .unwrap_or(false);
    rows.push(record("agy_plugin", agy, agy_owned));
    let mcp = components["mcp"] == true;
    if mcp || agy {
        // Reuse doctor inventory parsing for JSON MCP manifests; Codex runs
        // install's own TOML dry-run classifier on the file bytes so doctor
        // never reports ok where install, repair, or uninstall would refuse.
        let inventory = specs(paths)?;
        let json_mcp_healthy = |provider: &str, scope: &str| {
            inventory.servers.iter().any(|server| {
                server.provider == provider
                    && server.scope == scope
                    && server.name == "oh-my-setting"
                    && server.spec["args"] == json!(["mcp-server"])
                    && crate::integrations::owned_mcp_entry(
                        &server.spec,
                        paths,
                        &paths.home.join(".local/bin/oms"),
                    )
                    .unwrap_or(false)
            }) && inventory.layers.iter().any(|layer| {
                layer["provider"] == provider
                    && layer["scope"] == scope
                    && layer["status"] == "parsed"
            })
        };
        rows.push(record(
            "claude_mcp",
            mcp,
            !mcp || json_mcp_healthy("claude", "user"),
        ));
        if mcp {
            rows.push(codex_mcp_finding(paths, codex_config));
        } else {
            rows.push(record("codex_mcp", false, true));
        }
        rows.push(record(
            "agy_mcp",
            agy,
            !agy || json_mcp_healthy("antigravity", "plugin-oh-my-setting"),
        ));
    } else {
        rows.push(record("mcp", false, true));
    }
    r::check(flag)?;
    let mut result = report(paths, rows);
    result.as_object_mut().unwrap().remove("repo");
    result["kind"] = json!("oms-doctor-integrations");
    result["runtime"] = json!("not_probed");
    emit(result, options)
}
pub(crate) fn run(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    if options.has("--integrations") {
        return integrations(paths, options, flag);
    }
    if options.has("--contract") {
        return contract(paths, options, flag, services);
    }

    if options.has("--extensions") || options.has("--mcp") {
        return extensions(paths, options.has("--mcp"), options, flag);
    }
    if options.has("--providers") {
        if options.has("--conformance") {
            return crate::adapter_conformance::run(paths, options, flag);
        }
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
        if !options.preview() {
            let _guard = r::receipt_lock(&repair_paths, flag)?;
            crate::prepared::recover(&repair_paths)?;
            crate::payload::repair_cache(&repair_paths, flag)?;
        }
        let mut repair = options.clone();
        repair.tool = "install".into();
        repair.flags.remove("--repair");
        crate::install::run(&repair_paths, &repair, flag, services)?;
    }
    let mut rows = if options.has("--security") {
        if options.has("--residue") {
            residue(paths, flag)?
        } else {
            security(paths)?
        }
    } else if options.has("--sandbox") {
        sandbox(paths, options, flag)?
    } else if options.has("--surfaces") {
        let mut rows = surfaces(paths)?;
        crate::hook_diagnostics::append(paths, &mut rows, services)?;
        rows
    } else if options.has("--tool-lock") {
        tool_lock(paths)?
    } else {
        health(paths, flag, services)?
    };
    let model_mode = if options.has("--no-model-doctor") {
        "0".to_owned()
    } else if options.has("--live-models") || options.has("--strict-diversity") {
        "1".to_owned()
    } else {
        std::env::var("OH_MY_SETTING_MODEL_DOCTOR").unwrap_or_else(|_| "auto".into())
    };
    if !matches!(model_mode.as_str(), "auto" | "0" | "1") {
        return r::fail("OH_MY_SETTING_MODEL_DOCTOR must be auto, 0, or 1");
    }
    let model_applicable = model_mode == "1"
        || model_mode == "auto"
            && ["codex", "claude", "agy"]
                .iter()
                .any(|s| r::find(s, &paths.cwd).is_some());
    if model_applicable
        && !["--security", "--sandbox", "--surfaces", "--tool-lock"]
            .iter()
            .any(|s| options.has(s))
    {
        let models = crate::provider_check::models(
            paths,
            options.has("--live-models"),
            options.has("--strict-diversity"),
            flag,
        )?;
        for message in models["errors"].as_array().unwrap() {
            rows.push(finding(
                "model doctor",
                if model_mode == "1" { "error" } else { "warn" },
                message.as_str().unwrap_or("provider failure"),
            ));
        }
        for message in models["warnings"].as_array().unwrap() {
            rows.push(finding(
                "model doctor",
                "warn",
                message.as_str().unwrap_or("provider warning"),
            ));
        }
        if models["ok"] == true {
            rows.push(finding(
                "model doctor",
                "ok",
                "bounded provider invocation checks passed",
            ));
        }
    }
    let mut result = report(paths, rows);
    if options.has("--security") {
        result["kind"] = json!("oms-doctor-security");
    }
    if options.has("--sandbox") {
        result["kind"] = json!("oms-doctor-sandbox");
    }
    if options.has("--security") && !options.has("--residue") {
        let file = paths.state.join("doctor-security.json");
        let child = std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1");
        let _guard = if child {
            None
        } else {
            Some(r::lock(&file, &paths.cwd)?)
        };
        let previous = if r::exists(&file) {
            r::value(&file).ok()
        } else {
            None
        };
        let mut sections = serde_json::Map::new();
        let mut groups = BTreeMap::<String, Vec<&Value>>::new();
        for row in result["findings"].as_array().unwrap() {
            groups
                .entry(row["section"].as_str().unwrap_or("unknown").into())
                .or_default()
                .push(row);
        }
        for (section, rows) in groups {
            sections.insert(section, json!(r::hash(&r::canonical_ascii(&json!(rows))?)));
        }
        let old = previous.as_ref().and_then(|v| v["sections"].as_object());
        let mut changed = sections
            .iter()
            .filter(|(k, v)| old.is_some_and(|o| o.get(*k) != Some(*v)))
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>();
        if let Some(old) = old {
            changed.extend(old.keys().filter(|k| !sections.contains_key(*k)).cloned());
        }
        result["since"] = json!({"baseline":old.is_none(),"checked_at":previous.as_ref().and_then(|v| v.get("checked_at")),"changed":changed});
        if !child {
            r::atomic(
                &file,
                &r::bytes(&json!({"schema":1,"checked_at":r::now(),"sections":sections}))?,
            )?;
        }
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
            &json!({"schema":1,"install":receipt,"native":true,"source_root":paths.source,"state_root":paths.state,
                "install_profile_scope":"installation components",
                "capability_selection":crate::fingerprint::capability_provenance(paths)}),
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

fn contract(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    let scratch = r::Scratch::new()?;
    let fixture = scratch.0.join("fixture");
    fs::create_dir(&fixture)?;
    let mut rows = Vec::new();
    let blocks = |path: &Path| -> Result<String> {
        let raw = String::from_utf8(r::read(path, r::LIMIT)?)
            .map_err(|_| r::Error(2, "invalid loader fixture".into()))?;
        let mut inside = false;
        let mut result = String::new();
        for line in raw.lines() {
            if line.starts_with("<!-- oh-my-setting:") && line.ends_with(":begin -->") {
                inside = true;
            }
            if inside {
                result.push_str(line);
                result.push('\n');
            }
            if line.starts_with("<!-- oh-my-setting:") && line.ends_with(":end -->") {
                inside = false;
            }
        }
        if inside {
            return r::fail("unterminated loader block");
        }
        Ok(result)
    };
    for style in ["general", "ml"] {
        if style == "ml" {
            r::new_file(&fixture.join("GEMINI.md"),b"<!-- oh-my-setting:general:begin -->\nstale\n<!-- oh-my-setting:general:end -->\n",0o600)?;
        }
        let out = services.apply_project_template(style, &fixture, &paths.source, flag);
        if out.0 != 0 {
            return Err(r::Error(
                out.0,
                "native project-template contract producer unavailable or failed".into(),
            ));
        }
        let agents = blocks(&fixture.join("AGENTS.md"))?;
        let mut equal = !agents.is_empty() && agents == blocks(&fixture.join("CLAUDE.md"))?;
        if style == "general" {
            equal &= !fixture.join("GEMINI.md").exists();
        } else {
            equal &= agents == blocks(&fixture.join("GEMINI.md"))?
                && !agents.contains(":general:")
                && agents.contains(":ml:");
        }
        rows.push(finding(
            "provider contract",
            if equal { "ok" } else { "error" },
            format!("loader parity ({style})"),
        ));
    }
    let plain = scratch.0.join("unadopted");
    fs::create_dir(&plain)?;
    for name in [
        "skill-router",
        "turn-guard",
        "fail-ledger-hook",
        "precompact-handoff",
    ] {
        let out = services.provider_hook(name, b"", &plain, &paths.source, false, flag);
        rows.push(finding(
            "provider contract",
            if out.0 == 0 && !plain.join(".oms").exists() {
                "ok"
            } else {
                "error"
            },
            format!("unadopted hook no-op: {name}"),
        ));
    }
    let inventory = specs(paths)?;
    let servers = &inventory.servers;
    let unknown = &inventory.unknown;
    let mut registrations = std::collections::BTreeSet::new();
    for (provider, binary) in [
        ("claude", "claude"),
        ("codex", "codex"),
        ("antigravity", "agy"),
    ] {
        let matching = servers
            .iter()
            .filter(|server| server.provider == provider && server.name == "oh-my-setting")
            .collect::<Vec<_>>();
        if matching.is_empty() && r::find(binary, &paths.cwd).is_some() {
            rows.push(finding(
                "provider contract",
                "error",
                format!("{provider}: managed MCP registration missing"),
            ));
        }
        for server in matching {
            let spec = &server.spec;
            registrations.insert(r::canonical_ascii(
                &json!({"command":spec["command"],"args":spec["args"]}),
            )?);
        }
    }
    rows.push(finding(
        "provider contract",
        if registrations.len() > 1 || !unknown.is_empty() {
            "error"
        } else {
            "ok"
        },
        "MCP registrations resolve to one shared launch contract",
    ));
    emit(report(paths, rows), options)
}

fn policy_baselines(paths: &Paths) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut files = vec![
        ("project claude", paths.cwd.join(".claude/settings.json")),
        (
            "project claude local",
            paths.cwd.join(".claude/settings.local.json"),
        ),
        ("project MCP", paths.cwd.join(".mcp.json")),
        ("project codex", paths.cwd.join(".codex/config.toml")),
        (
            "antigravity cli",
            paths.home.join(".gemini/antigravity-cli/settings.json"),
        ),
        (
            "managed claude",
            paths.home.join(".claude/managed-settings.json"),
        ),
        (
            "system claude",
            PathBuf::from("/etc/claude-code/managed-settings.json"),
        ),
    ];
    if let Some(root) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        files.push((
            "claude config directory",
            PathBuf::from(root).join("settings.json"),
        ));
    }
    for (label, file) in files {
        if !r::exists(&file) {
            continue;
        }
        let raw = match r::read(&file, r::LIMIT) {
            Ok(raw) => raw,
            Err(_) => {
                rows.push(finding(
                    label,
                    "unknown",
                    "policy file is unsafe or unreadable; nothing executed",
                ));
                continue;
            }
        };
        let value = if file.extension().is_some_and(|s| s == "toml") {
            crate::config::toml(&raw)
        } else {
            r::parse(&raw)
        };
        let value = match value {
            Ok(value) if value.is_object() => value,
            _ => {
                rows.push(finding(
                    label,
                    "unknown",
                    "policy file is malformed; posture unproven",
                ));
                continue;
            }
        };
        let mut digest = finding(label, "info", "policy inventory recorded without values");
        digest["sha256"] = json!(r::hash(&raw));
        rows.push(digest);
        for (wide, description) in [
            (
                value["permissions"]["defaultMode"] == "bypassPermissions",
                "permission bypass",
            ),
            (value["approval_policy"] == "never", "approval policy never"),
            (
                value["sandbox_mode"] == "danger-full-access",
                "full host access",
            ),
            (
                value["sandbox"]["allowUnsandboxedCommands"] == true,
                "unsandboxed fallback",
            ),
            (
                value["enableAllProjectMcpServers"] == true,
                "all project MCP servers preapproved",
            ),
            (
                value.get("notify").is_some(),
                "configured notification command",
            ),
            (
                value["permissions"]["allow"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|v| matches!(v.as_str(), Some("*" | "Bash" | "Bash(*)" | "Bash(:*)")))
                }),
                "blanket shell allow",
            ),
        ] {
            if wide {
                rows.push(finding(label, "warn", description));
            }
        }
        for (group, kind) in [
            ("hooks", "hook declarations"),
            ("mcpServers", "MCP declarations"),
            ("mcp_servers", "MCP declarations"),
            ("env", "environment declarations"),
            ("model_providers", "model provider declarations"),
        ] {
            if let Some(table) = value[group].as_object()
                && !table.is_empty()
            {
                rows.push(finding(
                    label,
                    "info",
                    format!("{} {kind}; values withheld", table.len()),
                ));
            }
        }
        for name in [
            ["api", "KeyHelper"].concat(),
            ["aws", "AuthRefresh"].concat(),
            ["aws", "CredentialExport"].concat(),
            "gcpAuthRefresh".into(),
            "otelHeadersHelper".into(),
            "statusLine".into(),
            "fileSuggestion".into(),
        ] {
            if name == "statusLine"
                && value[&name]["command"].as_str().is_some_and(|c| {
                    ["claude-statusline", "agy-statusline"]
                        .iter()
                        .any(|verb| crate::integrations::owned_native(c, verb, paths))
                })
            {
                rows.push(finding(label, "ok", "statusLine is the OMS-owned HUD"));
            } else if value.get(&name).is_some() {
                rows.push(finding(
                    label,
                    "warn",
                    format!(
                        "{} names an executable helper; command withheld",
                        clean(&name)
                    ),
                ));
            }
        }
        if let Some(domains) = value["sandbox"]["network"]["allowedDomains"].as_array()
            && domains.iter().any(|v| {
                v.as_str().is_some_and(|s| {
                    s.contains('*')
                        && s.split('.')
                            .filter(|p| !p.is_empty() && !p.contains('*'))
                            .count()
                            < 2
                })
            })
        {
            rows.push(finding(
                label,
                "warn",
                "network policy has a catch-all domain",
            ));
        }
        if value["sandbox"]["excludedCommands"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            rows.push(finding(
                label,
                "info",
                "sandbox command exclusions configured; command values withheld",
            ));
        }
        if let Some(deny) = value["permissions"]["deny"].as_array() {
            rows.push(finding(
                label,
                "info",
                format!("{} explicit deny rules", deny.len()),
            ));
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrations_accept_owned_generated_agy_payload_and_detect_drift() {
        let scratch = r::Scratch::new().unwrap();
        let paths = Paths {
            home: scratch.0.join("home"),
            source: scratch.0.join("source"),
            receipt: scratch.0.join("config/install.json"),
            config: scratch.0.join("config"),
            state: scratch.0.join("state"),
            data: scratch.0.join("data"),
            cwd: scratch.0.join("source"),
        };
        let source = paths.source.join("plugins/antigravity");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            br#"{"name":"oh-my-setting","version":"template"}"#,
        )
        .unwrap();
        let target = paths.home.join(".gemini/config/plugins/oh-my-setting");
        let plugin = br#"{"name":"oh-my-setting","version":"1"}"#;
        let mcp = r::bytes(&json!({"mcpServers":{"oh-my-setting":{
            "command":paths.home.join(".local/bin/oms"),"args":["mcp-server"]
        }}}))
        .unwrap();
        // Use the same generated-directory writer and ownership identity as
        // plugin installation; source-template equality is not its contract.
        let mut tx = crate::transaction::Transaction::begin(&paths).unwrap();
        tx.files(
            &target,
            &[
                ("plugin.json", plugin, 0o600),
                ("mcp_config.json", &mcp, 0o600),
            ],
        )
        .unwrap();
        let marker = json!({"schema":1,"kind":"directory","source":source,
            "sha256":crate::transaction::identity(&target).unwrap()});
        tx.write(
            &crate::install::marker(&target),
            &r::bytes(&marker).unwrap(),
            0o600,
        )
        .unwrap();
        let receipt = r::bytes(
            &json!({"schema":1,"source_root":paths.source,"commit":"fixture",
            "version":"1","channel":"edge","installed_at":"fixture",
            "plugin":{"name":"oh-my-setting","version":"1","sha256":"fixture"},
            "components":{"agy_plugin":true}}),
        )
        .unwrap();
        tx.write(&paths.receipt, &receipt, 0o600).unwrap();
        tx.commit().unwrap();
        let options =
            crate::cli::parse("doctor", &["--integrations".into(), "--json".into()]).unwrap();
        let inspect = || {
            let out = integrations(&paths, &options, &AtomicBool::new(false)).unwrap();
            assert_eq!(fs::read(&paths.receipt).unwrap(), receipt);
            let row = r::parse(&out.1).unwrap();
            (out.0, row)
        };
        let (code, report) = inspect();
        assert_eq!(code, 0, "{report}");
        for section in ["agy_plugin", "agy_mcp"] {
            assert!(
                report["findings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["section"] == section && r["level"] == "ok")
            );
        }
        for (env, expected) in [
            (json!({"OMS_MCP_TOOL_PROFILE":"core"}), "ok"),
            (json!({"OMS_MCP_TOOL_PROFILE":"full"}), "ok"),
            (json!({"OMS_MCP_TOOL_PROFILE":"unknown"}), "error"),
            (
                json!({"OMS_MCP_TOOL_PROFILE":"full","USER_CUSTOM":"1"}),
                "error",
            ),
        ] {
            let mut selected = r::parse(&mcp).unwrap();
            selected["mcpServers"]["oh-my-setting"]["env"] = env;
            let selected = r::bytes(&selected).unwrap();
            let mut tx = crate::transaction::Transaction::begin(&paths).unwrap();
            // Publish the generated directory as installation does. Replacing
            // one child first would include its temporary undo file in the
            // directory identity recorded before transaction cleanup.
            tx.files(
                &target,
                &[
                    ("plugin.json", plugin, 0o600),
                    ("mcp_config.json", &selected, 0o600),
                ],
            )
            .unwrap();
            let marker = json!({"schema":1,"kind":"directory","source":source,
                "sha256":crate::transaction::identity(&target).unwrap()});
            tx.write(
                &crate::install::marker(&target),
                &r::bytes(&marker).unwrap(),
                0o600,
            )
            .unwrap();
            tx.commit().unwrap();
            assert!(crate::install::owned(&source, &target).unwrap());
            let (code, report) = inspect();
            assert_eq!(code, i32::from(expected == "error"), "{report}");
            assert_eq!(fs::read(target.join("mcp_config.json")).unwrap(), selected);
            for (section, level) in [("agy_plugin", "ok"), ("agy_mcp", expected)] {
                assert!(
                    report["findings"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|r| r["section"] == section && r["level"] == level)
                );
            }
        }
        fs::write(target.join("mcp_config.json"), b"{\"mcpServers\":{}}").unwrap();
        let (code, report) = inspect();
        assert_eq!(code, 1);
        for section in ["agy_plugin", "agy_mcp"] {
            assert!(
                report["findings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["section"] == section && r["level"] == "error")
            );
        }
    }

    #[test]
    fn owned_status_lines_are_ok_and_foreign_ones_stay_a_warning() {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("oms-doctor-hud-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let paths = Paths {
            home: root.join("home"),
            source: root.join("source"),
            receipt: root.join("receipt"),
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            cwd: root.clone(),
        };
        let level = |command: &str| {
            let body = json!({"statusLine":{"type":"command","command":command}});
            std::fs::write(root.join(".claude/settings.json"), body.to_string()).unwrap();
            policy_baselines(&paths)
                .unwrap()
                .iter()
                .find(|row| {
                    row["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("tatusLine"))
                })
                .map(|row| row["level"].as_str().unwrap().to_owned())
        };
        let oms = paths.home.join(".local/bin/oms");
        for verb in ["claude-statusline", "agy-statusline"] {
            assert_eq!(
                level(&format!("{} {verb}", oms.display())).as_deref(),
                Some("ok")
            );
        }
        assert_eq!(level("my-hud --claude-statusline").as_deref(), Some("warn"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mcp_servers_report_stale_builds_with_exact_argv_and_bounded_env() {
        use std::os::unix::fs::symlink;
        let scratch = r::Scratch::new().unwrap();
        let paths = Paths {
            home: scratch.0.join("home"),
            source: scratch.0.join("source"),
            receipt: scratch.0.join("config/install.json"),
            config: scratch.0.join("config"),
            state: scratch.0.join("state"),
            data: scratch.0.join("data"),
            cwd: scratch.0.join("source"),
        };
        for dir in [&paths.home, &paths.source, &paths.config, &paths.data] {
            fs::create_dir_all(dir).unwrap();
        }
        let installed_rev = r::hash(b"installed-native-build")[..40].to_owned();
        let stale_rev = r::hash(b"older-native-build")[..40].to_owned();
        let receipt = r::bytes(&json!({
            "schema": 1,
            "source_root": paths.source,
            "commit": installed_rev,
            "version": "1",
            "channel": "edge",
            "installed_at": "fixture",
            "plugin": {"name": "oh-my-setting", "version": "1", "sha256": "fixture"},
            "native": {
                "revision": installed_rev,
                "libexec": paths.data.join("libexec"),
                "frontdoor": paths.data.join("libexec").join(&installed_rev).join("oms-frontdoor")
            }
        }))
        .unwrap();
        fs::write(&paths.receipt, &receipt).unwrap();

        let proc_dir = scratch.0.join("proc");
        fs::create_dir_all(&proc_dir).unwrap();
        fs::write(proc_dir.join("uptime"), b"7200.00 0.00\n").unwrap();
        // Non-numeric and unreadable/symlinked entries must be ignored cleanly.
        symlink("201", proc_dir.join("self")).unwrap();
        fs::create_dir(proc_dir.join("bus")).unwrap();
        fs::create_dir(proc_dir.join("199")).unwrap();
        fs::create_dir(proc_dir.join("200")).unwrap();
        symlink(proc_dir.join("uptime"), proc_dir.join("200/cmdline")).unwrap();

        let write_proc =
            |pid: u32, argv: &[&str], env: &[(&str, &str)], start_ticks: Option<u64>| {
                let dir = proc_dir.join(pid.to_string());
                fs::create_dir_all(&dir).unwrap();
                let mut cmdline = Vec::new();
                for token in argv {
                    cmdline.extend_from_slice(token.as_bytes());
                    cmdline.push(0);
                }
                fs::write(dir.join("cmdline"), cmdline).unwrap();
                let mut environ = Vec::new();
                for (k, v) in env {
                    environ.extend_from_slice(k.as_bytes());
                    environ.push(b'=');
                    environ.extend_from_slice(v.as_bytes());
                    environ.push(0);
                }
                fs::write(dir.join("environ"), environ).unwrap();
                if let Some(ticks) = start_ticks {
                    let tail = format!("S 1 {pid} {pid} 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {ticks} 0");
                    fs::write(dir.join("stat"), format!("{pid} (oms) {tail}\n")).unwrap();
                }
            };

        let current_bin = paths.data.join("libexec").join(&installed_rev).join("oms");
        let stale_bin = paths.data.join("libexec").join(&stale_rev).join("oms");
        let source_str = paths.source.to_str().unwrap();

        // One current server.
        write_proc(
            201,
            &[
                current_bin.to_str().unwrap(),
                "--oms-dispatch",
                source_str,
                &installed_rev,
                "--",
                "mcp-server",
            ],
            &[
                ("OMS_MCP_TOOL_PROFILE", "core"),
                ("PRIVATE_VALUE", "current-private-material"),
            ],
            Some(700_000),
        );
        // One stale server with profile and panel version env plus unrelated env.
        write_proc(
            202,
            &[
                stale_bin.to_str().unwrap(),
                "--oms-dispatch",
                source_str,
                &stale_rev,
                "--",
                "mcp-server",
            ],
            &[
                ("OMS_MCP_TOOL_PROFILE", "full"),
                ("OMS_MCP_PANEL_RESOURCE_VERSION", "v34"),
                ("USER_CUSTOM", "stale-private-material"),
            ],
            Some(360_000),
        );
        // Unrelated processes whose argv merely contains `mcp-server` inside another token.
        let embedded = format!(
            "{} --oms-dispatch {source_str} {stale_rev} -- mcp-server",
            stale_bin.display()
        );
        write_proc(203, &["/bin/sh", "-c", &embedded], &[], Some(360_000));
        write_proc(
            204,
            &[
                stale_bin.to_str().unwrap(),
                "--oms-dispatch",
                source_str,
                &stale_rev,
                "--",
                "mcp-server-helper",
            ],
            &[],
            Some(360_000),
        );

        // A stale server dispatched from another install root is that install's concern.
        let other_root = scratch.0.join("other-install");
        fs::create_dir_all(&other_root).unwrap();
        write_proc(
            205,
            &[
                stale_bin.to_str().unwrap(),
                "--oms-dispatch",
                other_root.to_str().unwrap(),
                &stale_rev,
                "--",
                "mcp-server",
            ],
            &[],
            Some(360_000),
        );

        let doc = report(&paths, mcp_servers_at(&paths, Some(&proc_dir)));
        assert_eq!(doc["status"], "ok", "{doc}");
        assert_eq!(doc["exit"], 0, "{doc}");
        let findings = doc["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "{doc}");
        assert_eq!(findings[0]["section"], "mcp servers");
        assert_eq!(findings[0]["level"], "warn");
        let msg = findings[0]["message"].as_str().unwrap();
        assert!(msg.contains("pid 202"), "{msg}");
        assert!(msg.contains(&stale_rev[..12]), "{msg}");
        assert!(msg.contains(&installed_rev[..12]), "{msg}");
        assert!(msg.contains("age 1h"), "{msg}");
        assert!(msg.contains("OMS_MCP_TOOL_PROFILE=full"), "{msg}");
        assert!(msg.contains("OMS_MCP_PANEL_RESOURCE_VERSION=v34"), "{msg}");
        assert!(msg.contains("reconnect or restart"), "{msg}");
        assert!(msg.contains("ending only pid 202"), "{msg}");
        let rendered = doc.to_string();
        for forbidden in [
            "pid 201",
            "pid 203",
            "pid 204",
            "pid 205",
            "USER_CUSTOM",
            "PRIVATE_VALUE",
            "stale-private-material",
            "current-private-material",
        ] {
            assert!(!rendered.contains(forbidden), "{forbidden} in {rendered}");
        }

        // Without the stale server, only the exact-matching current server is counted.
        fs::remove_dir_all(proc_dir.join("202")).unwrap();
        let current_only = report(&paths, mcp_servers_at(&paths, Some(&proc_dir)));
        assert_eq!(current_only["status"], "ok");
        let rows = current_only["findings"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{current_only}");
        assert_eq!(rows[0]["section"], "mcp servers");
        assert_eq!(rows[0]["level"], "ok");
        let ok_msg = rows[0]["message"].as_str().unwrap();
        assert!(ok_msg.contains("1 running OMS MCP server(s)"), "{ok_msg}");
        assert!(ok_msg.contains(&installed_rev[..12]), "{ok_msg}");

        // With zero matching servers running, one ok row states 0.
        fs::remove_dir_all(proc_dir.join("201")).unwrap();
        let none_running = report(&paths, mcp_servers_at(&paths, Some(&proc_dir)));
        assert_eq!(none_running["status"], "ok");
        assert_eq!(none_running["findings"][0]["level"], "ok");
        assert_eq!(
            none_running["findings"][0]["message"],
            "0 running OMS MCP servers"
        );

        // Stale server warnings cap at 20 rows plus one summary row for the remainder.
        for pid in 300..322 {
            write_proc(
                pid,
                &[
                    stale_bin.to_str().unwrap(),
                    "--oms-dispatch",
                    source_str,
                    &stale_rev,
                    "--",
                    "mcp-server",
                ],
                &[],
                None,
            );
        }
        let capped = report(&paths, mcp_servers_at(&paths, Some(&proc_dir)));
        assert_eq!(capped["status"], "ok");
        let capped_rows = capped["findings"].as_array().unwrap();
        assert_eq!(capped_rows.len(), 21, "{capped}");
        assert!(capped_rows.iter().all(|r| r["level"] == "warn"));
        assert!(
            capped_rows[20]["message"]
                .as_str()
                .unwrap()
                .contains("2 additional stale OMS MCP server(s)"),
            "{capped}"
        );

        // Non-Linux callers pass None and emit a single info row.
        let uninspected = mcp_servers_at(&paths, None);
        assert_eq!(uninspected.len(), 1);
        assert_eq!(uninspected[0]["section"], "mcp servers");
        assert_eq!(uninspected[0]["level"], "info");
        assert!(
            uninspected[0]["message"]
                .as_str()
                .unwrap()
                .contains("not inspected")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_probe_reports_three_ssh_isolation_checks() {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = std::env::var_os("OMS_DOCTOR_SANDBOX_PROBE_TEST") {
            let root = PathBuf::from(std::env::var_os("OMS_DOCTOR_SANDBOX_PROBE_ROOT").unwrap());
            let cwd = root.join("wt");
            let paths = Paths::new(&cwd, &cwd).unwrap();
            let options =
                crate::cli::parse("doctor", &["--sandbox".into(), "--probe".into()]).unwrap();
            let rows = sandbox(&paths, &options, &AtomicBool::new(false)).unwrap();
            let expected_level = if mode == "ok" { "ok" } else { "warn" };
            let expected_suffix = if mode == "ok" {
                ["hidden", "hidden", "isolated"]
            } else {
                ["exposed", "exposed", "exposed"]
            };
            for (prefix, suffix) in [
                "OpenSSH directory (.ssh)",
                "OMS SSH control directory (oms-ssh)",
                "ssh-agent (SSH_AUTH_SOCK)",
            ]
            .into_iter()
            .zip(expected_suffix)
            {
                let expected_msg = format!("{prefix}: {suffix}");
                assert!(
                    rows.iter().any(|row| {
                        row["section"] == "sandbox"
                            && row["level"] == expected_level
                            && row["message"] == expected_msg
                    }),
                    "missing {expected_level} '{expected_msg}' in {rows:?}"
                );
            }
            return;
        }
        let scratch = r::Scratch::new().unwrap();
        let root = scratch.0.join("doctor-sbx");
        let home = root.join("home");
        let openssh = home.join(".ssh");
        let runtime = root.join("runtime");
        let control = runtime.join("oms-ssh");
        let agent = root.join("agent");
        let wt = root.join("wt");
        for dir in [&openssh, &control, &agent, &wt] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(openssh.join("id_probe"), b"probe\n").unwrap();
        fs::write(control.join("s.sock"), b"probe\n").unwrap();
        let sock = agent.join("agent.sock");
        fs::write(&sock, b"probe\n").unwrap();
        let ok_bwrap = root.join("bwrap-ok");
        fs::write(
            &ok_bwrap,
            concat!(
                "#!/bin/sh\n",
                "for last; do :; done\n",
                "if [ \"$last\" = \"true\" ]; then exit 0; fi\n",
                "tmpfs_list=''\n",
                "while [ \"$#\" -gt 0 ]; do\n",
                "  case \"$1\" in\n",
                "    --tmpfs) tmpfs_list=\"$tmpfs_list|$2|\"; shift 2 ;;\n",
                "    --unsetenv) unset \"$2\"; shift 2 ;;\n",
                "    --) shift; break ;;\n",
                "    *) shift ;;\n",
                "  esac\n",
                "done\n",
                "empty=\"$OMS_DOCTOR_SANDBOX_EMPTY\"\n",
                "sock=\"$8\"\n",
                "case \"$tmpfs_list\" in *\"|$5|\"*) s1=\"$empty\" ;; *) s1=\"$5\" ;; esac\n",
                "case \"$tmpfs_list\" in *\"|$6|\"*) s2=\"$empty\" ;; *) s2=\"$6\" ;; esac\n",
                "sock_dir=$(dirname \"$sock\")\n",
                "case \"$tmpfs_list\" in *\"|$sock_dir|\"*) s4=\"$empty/none\" ;; *) s4=\"$sock\" ;; esac\n",
                "exec \"$1\" \"$2\" \"$3\" \"$4\" \"$s1\" \"$s2\" \"$empty\" \"$s4\" \"$9\"\n",
            ),
        )
        .unwrap();
        fs::set_permissions(&ok_bwrap, fs::Permissions::from_mode(0o755)).unwrap();
        let leaky_bwrap = root.join("bwrap-leaky");
        fs::write(
            &leaky_bwrap,
            concat!(
                "#!/bin/sh\n",
                "for last; do :; done\n",
                "if [ \"$last\" = \"true\" ]; then exit 0; fi\n",
                "while [ \"$#\" -gt 0 ]; do\n",
                "  if [ \"$1\" = \"--\" ]; then shift; break; fi\n",
                "  shift\n",
                "done\n",
                "exec \"$@\"\n",
            ),
        )
        .unwrap();
        fs::set_permissions(&leaky_bwrap, fs::Permissions::from_mode(0o755)).unwrap();
        let empty_dir = root.join("empty");
        fs::create_dir_all(&empty_dir).unwrap();
        for (mode, bin) in [("ok", &ok_bwrap), ("warn", &leaky_bwrap)] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "doctor::tests::sandbox_probe_reports_three_ssh_isolation_checks",
                ])
                .env("OMS_DOCTOR_SANDBOX_PROBE_TEST", mode)
                .env("OMS_DOCTOR_SANDBOX_PROBE_ROOT", &root)
                .env("OMS_DOCTOR_SANDBOX_EMPTY", &empty_dir)
                .env("HOME", &home)
                .env("XDG_RUNTIME_DIR", &runtime)
                .env("SSH_AUTH_SOCK", &sock)
                .env("OMS_BWRAP_BIN", bin)
                .status()
                .unwrap();
            assert!(status.success(), "mode {mode} failed");
        }
    }
}
