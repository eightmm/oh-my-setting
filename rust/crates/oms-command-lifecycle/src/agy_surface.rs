use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde_json::json;
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

pub(crate) fn cache(paths: &Paths) -> Result<PathBuf> {
    r::leaf(
        &std::env::var_os("OMS_CAPABILITY_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or(paths.home.join(".cache"))
                    .join("oh-my-setting/capabilities")
            })
            .join("agy-surfaces.env"),
    )
}
fn binary(paths: &Paths) -> Option<PathBuf> {
    let name = std::env::var_os("OMS_AGY_BIN")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "agy".into());
    resolve_binary(paths, name)
}
pub(crate) fn resolve_binary(paths: &Paths, name: OsString) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let absolute = |p: &Path| {
        if p.is_absolute() {
            p.to_owned()
        } else {
            paths.cwd.join(p)
        }
    };
    let executable =
        |p: &Path| fs::metadata(absolute(p)).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0);
    let mut spelling = if name.as_bytes().contains(&b'/') {
        let path = PathBuf::from(name);
        if !executable(&path) {
            return None;
        }
        path
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(&name))
            .find(|p| executable(p))?
    };
    for _ in 0..40 {
        let path = absolute(&spelling);
        if !path.is_symlink() {
            return executable(&spelling).then_some(spelling);
        }
        let parent = fs::canonicalize(path.parent()?).ok()?;
        let destination = fs::read_link(path).ok()?;
        spelling = if destination.is_absolute() {
            destination
        } else {
            parent.join(destination)
        };
    }
    None
}
pub(crate) fn identity(paths: &Paths, binary: Option<&Path>) -> Result<String> {
    let Some(binary) = binary else {
        return Ok("absent".into());
    };
    let physical = if binary.is_absolute() {
        binary.to_owned()
    } else {
        paths.cwd.join(binary)
    };
    let info = fs::metadata(physical)?;
    let display = binary
        .to_str()
        .filter(|s| !s.contains(['\n', '\r']))
        .ok_or_else(|| {
            r::Error(
                2,
                "surface cache requires a single-line UTF-8 binary path".into(),
            )
        })?;
    Ok(format!("{display}:{}.{}", info.len(), info.mtime()))
}
fn run(
    binary: &Path,
    args: &[OsString],
    cwd: &Path,
    home: &Path,
    seconds: u64,
    flag: &AtomicBool,
) -> Result<crate::Output> {
    let result = r::command_env(
        binary,
        args,
        cwd,
        None,
        flag,
        Duration::from_secs(seconds),
        &[
            ("HOME", home.as_os_str()),
            ("XDG_CONFIG_HOME", home.as_os_str()),
            ("XDG_DATA_HOME", home.as_os_str()),
            ("XDG_CACHE_HOME", home.as_os_str()),
        ],
    );
    r::check(flag)?;
    Ok(result.unwrap_or((126, Vec::new(), Vec::new())))
}
fn shape(path: &Path, stop: bool) -> bool {
    r::value(path).is_ok_and(|v| {
        v.is_object()
            && v["conversationId"].as_str().is_some_and(|s| !s.is_empty())
            && v["workspacePaths"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
            && (!stop || v.get("terminationReason").is_some())
    })
}

/// Certifies delivery, payload shape and timeout in a disposable provider home.
/// Only the bounded verdict is persisted; probe output and account data are not.
pub(crate) fn probe(paths: &Paths, flag: &AtomicBool) -> Result<crate::Output> {
    r::child_guard()?;
    let executable = binary(paths);
    let original = identity(paths, executable.as_deref())?;
    let invocation = executable.as_ref().map(|p| {
        if p.is_absolute() {
            p.clone()
        } else {
            paths.cwd.join(p)
        }
    });
    let scratch = r::Scratch::new()?;
    let home = scratch.0.join("home");
    let workspace = scratch.0.join("workspace");
    let fixture = scratch.0.join("fixture");
    let markers = scratch.0.join("markers");
    for dir in [&home, &workspace, &fixture, &markers] {
        fs::create_dir(dir)?;
    }
    r::new_file(
        &fixture.join("plugin.json"),
        &r::bytes(
            &json!({"name":"oms-surface-probe","version":"0.0.0","description":"oh-my-setting Antigravity surface certification probe"}),
        )?,
        0o600,
    )?;
    let hooks = json!({"oms-surface-probe":{
        "PreInvocation":[{"type":"command","command":"bash ./probe-hook.sh preinvocation","timeout":5}],
        "Stop":[{"type":"command","command":"bash ./probe-hook.sh stop","timeout":5},{"type":"command","command":"bash ./probe-slow.sh","timeout":1}]
    }});
    r::new_file(&fixture.join("hooks.json"), &r::bytes(&hooks)?, 0o600)?;
    let quoted = r::shell(markers.as_os_str())?;
    let handler = format!(
        "#!/bin/sh\nmarkers={quoted}\nevent=\"${{1:-unknown}}\"\ncase \"$event\" in preinvocation|stop) ;; *) exit 2 ;; esac\ncat > \"$markers/$event.json\"\n: > \"$markers/$event.fired\"\nif [ \"$event\" = preinvocation ]; then\n  printf '%s\\n' '{{\"injectSteps\":[{{\"ephemeralMessage\":\"oh-my-setting surface probe\"}}]}}'\nelse\n  printf '%s\\n' '{{}}'\nfi\n"
    );
    let slow =
        format!("#!/bin/sh\nsleep 3\n: > {quoted}/timeout-violated\nprintf '%s\\n' '{{}}'\n");
    r::new_file(&fixture.join("probe-hook.sh"), handler.as_bytes(), 0o700)?;
    r::new_file(&fixture.join("probe-slow.sh"), slow.as_bytes(), 0o700)?;
    let mut accepted = false;
    let mut pre = false;
    let mut stop = false;
    let mut payload = false;
    let mut bounded = false;
    let (mut verdict, mut reason) = ("unverified", "absent");
    let mut version = String::from("unknown");
    if let Some(executable) = invocation.as_deref() {
        let out = run(
            executable,
            &["--version".into()],
            &workspace,
            &home,
            10,
            flag,
        )?;
        r::check(flag)?;
        if out.0 == 0 {
            version = String::from_utf8_lossy(&out.1)
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
                .take(120)
                .collect();
        }
        let out = run(
            executable,
            &[
                "plugin".into(),
                "validate".into(),
                fixture.as_os_str().into(),
                "--json".into(),
            ],
            &workspace,
            &home,
            60,
            flag,
        )?;
        r::check(flag)?;
        let count = if out.0 == 0 {
            r::parse(&out.1)
                .ok()
                .and_then(|v| v["components"]["hooks"].as_u64())
        } else {
            None
        };
        reason = "validate";
        if let Some(count) = count {
            if count == 0 {
                verdict = "unsupported";
                reason = "schema";
            } else {
                accepted = true;
                reason = "install";
                let out = run(
                    executable,
                    &[
                        "plugin".into(),
                        "install".into(),
                        fixture.as_os_str().into(),
                    ],
                    &workspace,
                    &home,
                    60,
                    flag,
                )?;
                r::check(flag)?;
                if out.0 == 0 {
                    let out = run(
                        executable,
                        &[
                            "-p".into(),
                            "Reply with the single word: ok".into(),
                            "--output-format".into(),
                            "text".into(),
                        ],
                        &workspace,
                        &home,
                        120,
                        flag,
                    )?;
                    r::check(flag)?;
                    reason = "run";
                    if out.0 != 0
                        && String::from_utf8_lossy(&[out.1, out.2].concat())
                            .to_lowercase()
                            .contains("authentication")
                    {
                        reason = "auth";
                    }
                    if out.0 == 0 {
                        pre = markers.join("preinvocation.fired").is_file();
                        stop = markers.join("stop.fired").is_file();
                        payload = pre
                            && stop
                            && shape(&markers.join("preinvocation.json"), false)
                            && shape(&markers.join("stop.json"), true);
                        bounded = !markers.join("timeout-violated").exists();
                        verdict = "unsupported";
                        reason = if !pre {
                            "preinvocation"
                        } else if !stop {
                            "stop"
                        } else if !payload {
                            "payload"
                        } else if !bounded {
                            "handler_timeout"
                        } else {
                            verdict = "verified";
                            "ok"
                        };
                    }
                }
            }
        }
    }
    if identity(paths, executable.as_deref()).ok().as_ref() != Some(&original)
        || binary(paths) != executable
    {
        verdict = "unverified";
        reason = "binary_changed";
    }
    let yes = |v| if v { "yes" } else { "no" };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let body = format!(
        "binary_key={original}\nagy_version={version}\nverdict={verdict}\nreason={reason}\nschema_accepted={}\npreinvocation_delivered={}\nstop_delivered={}\npayload_shape={}\nhandler_timeout_enforced={}\nprobe_schema=2\nprobed_at={stamp}\n",
        yes(accepted),
        yes(pre),
        yes(stop),
        yes(payload),
        yes(bounded)
    );
    let file = cache(paths)?;
    let _guard = r::lock(&file, &paths.cwd)?;
    r::check(flag)?;
    r::atomic(&file, body.as_bytes())?;
    Ok(r::text(format!("agy-surfaces: {verdict} ({reason})")))
}

pub(crate) fn verified(paths: &Paths) -> Result<bool> {
    let file = cache(paths)?;
    if !r::exists(&file) {
        return Ok(false);
    }
    let raw = String::from_utf8(r::read(&file, 65536)?)
        .map_err(|_| r::Error(2, "invalid Antigravity surface cache".into()))?;
    let field = |name| {
        raw.lines().find_map(|l| {
            l.split_once('=')
                .filter(|(n, _)| *n == name)
                .map(|(_, v)| v)
        })
    };
    let executable = binary(paths);
    let current = identity(paths, executable.as_deref())?;
    Ok(executable.is_some()
        && field("binary_key") == Some(current.as_str())
        && field("probe_schema") == Some("2")
        && field("verdict") == Some("verified")
        && [
            "schema_accepted",
            "preinvocation_delivered",
            "stop_delivered",
            "payload_shape",
            "handler_timeout_enforced",
        ]
        .iter()
        .all(|n| field(n) == Some("yes")))
}
