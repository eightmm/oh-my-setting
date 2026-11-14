use crate::broker::{Broker, SubscriptionBackend, subscription_env};
use crate::common::*;
use crate::gateway::Gateway;
use crate::https::CurlTransport;
use serde_json::json;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::Duration;

const EXTENSIONS: [(&str, &str); 2] = [
    (
        "pi-codex-extension.mjs",
        include_str!("../assets/pi-codex-extension.mjs"),
    ),
    (
        "pi-oms-extension.mjs",
        include_str!("../assets/pi-oms-extension.mjs"),
    ),
];

/// Digest-addressed private cache directory for the embedded Pi extensions.
fn extension_dir() -> Result<PathBuf> {
    extension_dir_in(std::env::var_os("XDG_CACHE_HOME"), home)
}

/// An unset, empty or relative cache base falls back to `$HOME/.cache` (XDG Base Directory);
/// `HOME` is only required in that case.
fn extension_dir_in(
    xdg: Option<OsString>,
    home: impl FnOnce() -> Result<PathBuf>,
) -> Result<PathBuf> {
    let mut bytes = Vec::new();
    for (name, body) in EXTENSIONS {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(body.as_bytes());
        bytes.push(0);
    }
    let base = match xdg.map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(base) => base,
        None => home()?.join(".cache"),
    };
    Ok(base.join(CACHE_SUBDIR).join(digest(&bytes)))
}

const CACHE_SUBDIR: &str = "oh-my-setting/harness";

/// Write the embedded extensions when absent or different; identical files are untouched.
/// `dir` is `<cache base>/oh-my-setting/harness/<digest>`: only the cache base (HOME or
/// `XDG_CACHE_HOME` and above) may be a symlink, as with macOS temp dirs; every component below
/// it is refused when it is a symlink.
fn materialize(dir: &Path) -> Result<()> {
    let below = Path::new(CACHE_SUBDIR).components().count() + 1;
    let base = dir
        .ancestors()
        .nth(below)
        .ok_or_else(|| error("unexpected cache directory layout"))?;
    let mut anchor = base;
    while !anchor.is_dir() {
        anchor = anchor
            .parent()
            .ok_or_else(|| error("cache directory has no existing ancestor"))?;
    }
    let relative = dir
        .strip_prefix(anchor)
        .map_err(|_| error("unexpected cache directory layout"))?
        .to_owned();
    let anchor = anchor.canonicalize()?;
    no_links(&anchor, &relative)?;
    let target = anchor.join(&relative);
    // Warm cache (vetted symlink-free above): nothing to write, so no lock.
    if EXTENSIONS
        .iter()
        .all(|(name, body)| optional(&target.join(name)).is_ok_and(|b| b == body.as_bytes()))
    {
        return Ok(());
    }
    let _guard = oms_lock::Lock::for_file(
        &target.join(EXTENSIONS[0].0),
        lock_settings(&anchor, &target),
    )
    .map_err(|_| error("cannot select cache lock"))?
    .acquire(None)
    .map_err(|e| Error {
        code: e.exit_code(),
        message: "cannot acquire cache lock".into(),
    })?;
    let store = Store::open(&anchor)?;
    for (name, body) in EXTENSIONS {
        let path = target.join(name);
        // Re-check under the lock: a concurrent launcher may have written it meanwhile.
        if optional(&path).is_ok_and(|b| b == body.as_bytes()) {
            continue;
        }
        store.write_locked(&path, body.as_bytes(), false, None)?;
    }
    Ok(())
}

/// Lock settings that agree whether or not `HOME` is set: the lock directory defaults to
/// `<harness cache>/.locks` (already vetted symlink-free), not to anything derived from `HOME`.
fn lock_settings(anchor: &Path, target: &Path) -> oms_lock::Settings {
    let get = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let seconds = |name: &str, default: u64| {
        std::time::Duration::from_secs(
            get(name)
                .and_then(|v| v.to_str().and_then(|s| s.parse().ok()))
                .unwrap_or(default),
        )
    };
    oms_lock::Settings {
        mode: oms_lock::Mode::LegacyAuto,
        timeout: seconds("OMS_LOCK_TIMEOUT", 300),
        poll_max: seconds("OMS_POLL_MAX_SECONDS", 10),
        lock_dir: Some(
            get("OMS_LOCK_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| target.parent().unwrap_or(anchor).join(".locks")),
        ),
        home: get("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| anchor.to_owned()),
        temp_dir: get("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp")),
        cwd: std::env::current_dir().unwrap_or_else(|_| anchor.to_owned()),
        search_path: get("PATH").unwrap_or_default(),
        force_mkdir: get("OMS_LOCK_FORCE_MKDIR").as_deref() == Some(OsStr::new("1")),
    }
}

/// A legacy source tree still ships `pi-oms-extension.mjs` together with the Python hook helper
/// it drives, and `oms` there has no `harness-hook` yet, so that pair is used as-is. Wave 3 drops
/// this guard once `oms harness-hook` is wired into `rust/crates/oms`; every other host runs the
/// embedded copy, which calls the native `oms`.
fn extension_path(file: &str, extensions: &Path, source: &Path) -> PathBuf {
    let legacy = source.join("scripts/lib");
    if file == "pi-oms-extension.mjs"
        && legacy.join(file).is_file()
        && legacy.join("hook_state.py").is_file()
    {
        return legacy.join(file);
    }
    extensions.join(file)
}

/// The native `oms` the embedded extension should call: this executable when it is `oms`,
/// else an `oms` file beside it.
fn oms_bin_for(exe: &Path) -> Option<PathBuf> {
    if exe.file_name().is_some_and(|n| n == "oms") {
        return Some(exe.to_owned());
    }
    let sibling = exe.parent()?.join("oms");
    sibling.is_file().then_some(sibling)
}

fn command(
    host: &str,
    model: &str,
    tail: &[OsString],
    effort: Option<&str>,
    extensions: &Path,
    source: &Path,
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![host.into()];
    if host == "codex" {
        if let Some(value) = effort {
            argv.extend([
                "-c".into(),
                format!("model_reasoning_effort=\"{value}\"").into(),
            ]);
        }
        argv.extend([
            "--model".into(),
            model.into(),
            "-c".into(),
            "model_provider=\"openai\"".into(),
        ]);
    } else if host == "claude" {
        argv.extend(["--model".into(), model.into()]);
    } else {
        for (file, _) in EXTENSIONS {
            argv.push("--extension".into());
            argv.push(extension_path(file, extensions, source).into_os_string());
        }
        argv.extend([
            "--provider".into(),
            "oms-codex".into(),
            "--model".into(),
            model.into(),
        ]);
        if let Some(value) = effort {
            argv.extend(["--thinking".into(), value.into()]);
        }
    }
    argv.extend_from_slice(tail);
    argv
}
pub(crate) fn run(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    if std::env::var("OMS_HARNESS_DELEGATE_DEPTH").is_ok_and(|s| s != "0") {
        return Err(error("A harness worker cannot launch another harness"));
    }
    host_mutation()?;
    let separator = raw.iter().position(|s| s == "--").unwrap_or(raw.len());
    let args = Args::parse(
        &raw[..separator],
        &["--model", "--effort", "--context-window"],
        &["--check", "--dry-run"],
    )?;
    args.count(1)?;
    let host = args
        .positional
        .first()
        .map(String::as_str)
        .filter(|s| ["codex", "pi", "claude"].contains(s))
        .ok_or_else(|| error("harness must be codex, pi or claude"))?;
    let model = args.required("--model")?;
    if model.len() > 160
        || !model
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || ["provider-default", "latest", "auto"].contains(&model)
    {
        return Err(error("An exact model ID is required"));
    }
    let requested = args.get("--effort");
    if requested.is_some_and(|s| !["low", "medium", "high", "xhigh", "max"].contains(&s)) {
        return Err(error("unsupported reasoning effort"));
    }
    let window = number(
        args.get("--context-window").unwrap_or("200000"),
        1024,
        2_000_000,
        "context-window",
    )?;
    let tail = raw.get(separator + 1..).unwrap_or(&[]);
    let mut denied = vec![
        "--model",
        "-m",
        "--provider",
        concat!("--api-", "key"),
        "--models",
    ];
    if host == "codex" {
        denied.extend([
            "--oss",
            "--local-provider",
            "--profile",
            "-p",
            "--config",
            "-c",
            "login",
            "logout",
        ]);
    }
    if host == "claude" {
        denied.push("--settings");
    }
    for arg in tail {
        if arg == "--" {
            break;
        }
        let s = arg
            .to_str()
            .ok_or_else(|| error("native arguments must be UTF-8"))?;
        let option = s.split('=').next().unwrap_or("");
        if denied.contains(&option)
            || (host == "codex"
                && ["-m", "-c", "-p"]
                    .iter()
                    .any(|p| s.starts_with(p) && s != *p))
        {
            return Err(error(
                "Use the OMS model selection; native routing overrides are unsupported",
            ));
        }
    }
    if args.has("--dry-run") {
        let extensions = if host == "pi" {
            extension_dir()?
        } else {
            PathBuf::new()
        };
        let argv = command(host, model, tail, requested, &extensions, ctx.source);
        return Ok(Output::json(
            json!({"harness":host,"model_provider":"codex","auth":"chatgpt-subscription","model":model,"argv":argv.iter().map(|s|s.to_string_lossy()).collect::<Vec<_>>()}),
        ));
    }
    if !installed("codex") || !installed(host) {
        return Err(error(
            "Both Codex CLI and the selected harness must already be installed",
        ));
    }
    let mut broker = Broker::start()?;
    broker.login(false)?;
    let details = broker.model(model)?;
    let supported: Vec<String> = details["supportedReasoningEfforts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["reasoningEffort"].as_str().map(str::to_owned))
        .collect();
    let effort = requested
        .or_else(|| details["defaultReasoningEffort"].as_str())
        .or_else(|| {
            [
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ]
            .into_iter()
            .find(|s| supported.iter().any(|v| v == s))
        })
        .map(str::to_owned);
    if effort
        .as_ref()
        .is_some_and(|s| !supported.is_empty() && !supported.contains(s))
    {
        return Err(error(
            "The selected model does not support this reasoning effort",
        ));
    }
    if args.has("--check") {
        return Ok(Output::json(
            json!({"harness":host,"model_provider":"codex","auth":"chatgpt-subscription","model":model,"catalog_verified":true,"inference_verified":false}),
        ));
    }
    let gateway = if host != "codex" {
        Some(Gateway::start(
            Arc::new(SubscriptionBackend {
                broker: Arc::new(Mutex::new(broker)),
                transport: Arc::new(CurlTransport),
            }),
            model.into(),
            effort.clone(),
            supported,
        )?)
    } else {
        None
    };
    let extensions = if host == "pi" {
        let dir = extension_dir()?;
        materialize(&dir)?;
        dir
    } else {
        PathBuf::new()
    };
    let argv = command(
        host,
        model,
        tail,
        effort.as_deref(),
        &extensions,
        ctx.source,
    );
    let mut child = Command::new(&argv[0]);
    child.args(&argv[1..]).current_dir(ctx.cwd);
    subscription_env(&mut child);
    for name in [
        concat!("ANTHROPIC_API_", "KEY"),
        concat!("ANTHROPIC_AUTH_", "TOKEN"),
        "ANTHROPIC_BASE_URL",
        concat!("CLAUDE_CODE_OAUTH_", "TOKEN"),
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    ] {
        child.env_remove(name);
    }
    child.env("OMS_AGENT", host);
    if host == "pi"
        && std::env::var_os("OMS_BIN").is_none_or(|v| v.is_empty())
        && let Some(bin) = std::env::current_exe().ok().and_then(|e| oms_bin_for(&e))
    {
        child.env("OMS_BIN", bin);
    }
    if let Some(gateway) = &gateway {
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            child.env_remove(name);
        }
        child
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env("OMS_HARNESS_BRIDGE", "1");
        if host == "claude" {
            child.env(concat!("ANTHROPIC_AUTH_", "TOKEN"), gateway.capability());
            child.env("ANTHROPIC_BASE_URL", gateway.url());
            for name in [
                "ANTHROPIC_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "CLAUDE_CODE_SUBAGENT_MODEL",
            ] {
                child.env(name, model);
            }
            for name in [
                "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            ] {
                child.env(name, "1");
            }
            child.env("ENABLE_TOOL_SEARCH", "false");
        } else {
            child
                .env("OMS_CODEX_HARNESS_URL", gateway.url())
                .env("OMS_CODEX_HARNESS_CAPABILITY", gateway.capability())
                .env(
                    "OMS_CODEX_HARNESS_MODEL",
                    json!({"id":model,"contextWindow":window}).to_string(),
                );
            if std::env::var_os("OMS_PANEL_MAIN_ATTEMPT").is_some()
                && std::env::var_os("OMS_PANEL_OWNER").as_deref() == Some(OsStr::new("codex"))
            {
                child.env("OMS_AGENT", "codex").env_remove("OMS_HOOK_AGENT");
            }
        }
    }
    let mut process = child.spawn()?;
    let code = loop {
        if ctx.interrupted.load(Ordering::Relaxed) {
            let _ = process.kill();
            let _ = process.wait();
            break 130;
        }
        if let Some(status) = process.try_wait()? {
            break status.code().unwrap_or(1);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(gateway);
    Ok(Output {
        code,
        out: vec![],
        err: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oms-harness-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn relative_or_empty_cache_base_falls_back_to_home() {
        let home = Path::new("/h");
        for xdg in [Some(".cache".into()), Some("".into()), None] {
            let dir = extension_dir_in(xdg, || Ok(home.to_path_buf())).unwrap();
            assert!(
                dir.starts_with("/h/.cache/oh-my-setting/harness"),
                "{dir:?}"
            );
        }
        let abs = extension_dir_in(Some("/x/cache".into()), || Ok(home.to_path_buf())).unwrap();
        assert!(abs.starts_with("/x/cache/oh-my-setting/harness"));
    }

    #[test]
    fn materialize_through_symlinked_base_is_private_and_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("link");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let dir = extension_dir_in(Some(root.join("link").into()), || Ok(root.clone())).unwrap();
        materialize(&dir).unwrap();
        materialize(&dir).unwrap();
        for (name, body) in EXTENSIONS {
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), body);
        }
        let mode = std::fs::metadata(dir.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "cache parent must be private: {mode:o}");
        // Symlinks below the cache base are refused, at every level.
        for level in [
            root.join("link/oh-my-setting"),
            root.join("link/oh-my-setting/harness"),
            dir.clone(),
        ] {
            let backup = root.join("moved");
            std::fs::rename(&level, &backup).unwrap();
            std::os::unix::fs::symlink(&backup, &level).unwrap();
            assert!(materialize(&dir).is_err(), "symlink at {level:?} accepted");
            std::fs::remove_file(&level).unwrap();
            std::fs::rename(&backup, &level).unwrap();
        }
        materialize(&dir).unwrap();
        let stale = dir.join(EXTENSIONS[0].0);
        std::fs::write(&stale, "tampered").unwrap();
        materialize(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(stale).unwrap(), EXTENSIONS[0].1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn embedded_pi_extension_calls_native_oms_without_python() {
        use std::os::unix::fs::PermissionsExt;
        let body = EXTENSIONS[1].1;
        assert!(
            !body.contains("python")
                && !body.contains("hook_state")
                && !body.contains("scripts/lib")
        );
        if Command::new("node").arg("--version").output().is_err() {
            eprintln!("node missing: skipping extension execution");
            return;
        }
        let root = scratch("node");
        let dir = extension_dir_in(Some(root.join("cache").into()), || Ok(root.clone())).unwrap();
        materialize(&dir).unwrap();
        let log = root.join("calls.log");
        let fake = root.join("oms");
        // Fails closed: wrong argv shape, agent, empty stdin or a missing subcommand exits 3.
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\n[ \"$#\" = 2 ] && [ \"$1\" = harness-hook ] || exit 3\ncase \"$2\" in panel-hook|relay|relay-hint) ;; *) exit 3 ;; esac\n[ \"$OMS_HOOK_AGENT\" = codex ] || exit 3\n[ -z \"$OMS_HOOK_PAYLOAD\" ] || exit 3\nbody=$(cat)\ncase \"$body\" in '{{\"hook_event_name\"'*) ;; *) exit 3 ;; esac\necho \"$1 $2 $body\" >> {log}\n[ \"$2\" = relay ] && exit 4\necho \"out-$2\"\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let driver = root.join("driver.mjs");
        std::fs::write(
            &driver,
            format!(
                "import ext from {url};\nconst h = {{}};\next({{ on: (n, f) => (h[n] = f) }});\nconst ctx = {{ cwd: '.' }};\nawait h.session_start({{ reason: 'new' }}, ctx);\nconst r = await h.before_agent_start({{ prompt: 'hi' }}, ctx);\nawait h.agent_settled({{}}, ctx);\nconsole.log(JSON.stringify(r));\n",
                url = serde_json::to_string(&format!("file://{}", dir.join(EXTENSIONS[1].0).display())).unwrap()
            ),
        )
        .unwrap();
        let out = Command::new("node")
            .arg(&driver)
            .env("OMS_BIN", &fake)
            .env("OMS_PANEL_MAIN_ATTEMPT", "t1")
            .env("OMS_HOOK_PAYLOAD", "inherited")
            .env_remove("OMS_HARNESS_CHILD")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(stdout.contains("out-panel-hook"), "{stdout}");
        let calls = std::fs::read_to_string(&log).unwrap();
        for sub in [
            "harness-hook relay-hint",
            "harness-hook panel-hook",
            "harness-hook relay",
        ] {
            assert!(calls.contains(sub), "{sub}: {calls}");
        }
        assert!(calls.contains("\"session_id\":\"oms-pi-t1\""), "{calls}");
        assert!(calls.contains("\"prompt\":\"hi\""), "{calls}");
        // relay exits non-zero in the stub: the extension degrades instead of failing Pi.
        // A missing or failing binary yields no injected context.
        for bin in [root.join("absent"), root.join("cache")] {
            let out = Command::new("node")
                .arg(&driver)
                .env("OMS_BIN", &bin)
                .env("OMS_PANEL_MAIN_ATTEMPT", "t1")
                .env_remove("OMS_HARNESS_CHILD")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "{stdout}");
            assert!(!stdout.contains("out-"), "{stdout}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn materialize_needs_no_home_and_extension_selection_is_guarded() {
        let root = scratch("cold");
        let dir = extension_dir_in(Some(root.join("cold").into()), || {
            Err(error("HOME must not be read"))
        })
        .unwrap();
        materialize(&dir).unwrap();
        assert!(dir.join(EXTENSIONS[1].0).is_file());

        let src = root.join("src");
        let ext = Path::new("/cache/x");
        let name = "pi-oms-extension.mjs";
        assert_eq!(extension_path(name, ext, &src), ext.join(name));
        std::fs::create_dir_all(src.join("scripts/lib")).unwrap();
        std::fs::write(src.join("scripts/lib").join(name), "x").unwrap();
        assert_eq!(
            extension_path(name, ext, &src),
            ext.join(name),
            "needs the helper too"
        );
        std::fs::write(src.join("scripts/lib/hook_state.py"), "x").unwrap();
        assert_eq!(
            extension_path(name, ext, &src),
            src.join("scripts/lib").join(name)
        );
        let other = "pi-codex-extension.mjs";
        assert_eq!(extension_path(other, ext, &src), ext.join(other));

        // OMS_BIN set to "" counts as unset.
        let unset = |v: Option<&str>| v.map(OsString::from).is_none_or(|v| v.is_empty());
        assert!(unset(None) && unset(Some("")) && !unset(Some("x")));
        assert_eq!(oms_bin_for(Path::new("/b/oms")), Some("/b/oms".into()));
        assert_eq!(oms_bin_for(Path::new("/nonexistent-dir/other")), None);
        std::fs::write(root.join("oms"), "").unwrap();
        assert_eq!(oms_bin_for(&root.join("other")), Some(root.join("oms")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Child of `concurrent_cold_cache_materialize_without_home`: HOME is unset by the parent.
    #[test]
    fn cold_cache_child() {
        let Some(base) = std::env::var_os("OMS_TEST_COLD_BASE") else {
            return;
        };
        assert!(std::env::var_os("HOME").is_none());
        let dir = extension_dir_in(Some(base), || Err(error("HOME must not be read"))).unwrap();
        materialize(&dir).unwrap();
        for (name, body) in EXTENSIONS {
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), body);
        }
    }

    #[test]
    fn concurrent_cold_cache_materialize_without_home() {
        let root = scratch("race");
        let base = root.join("cold");
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = (0..8)
            .map(|_| {
                Command::new(&exe)
                    .args([
                        "--exact",
                        "harness::tests::cold_cache_child",
                        "--test-threads=1",
                    ])
                    .env_remove("HOME")
                    .env_remove("OMS_LOCK_DIR")
                    .env("OMS_TEST_COLD_BASE", &base)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in children {
            let out = child.wait_with_output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && text.contains("1 passed"),
                "{text}{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
