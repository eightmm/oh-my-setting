use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

fn state_path(paths: &Paths) -> Result<PathBuf> {
    r::leaf(
        &std::env::var_os("OH_MY_SETTING_AUTO_UPDATE_STATE")
            .map(PathBuf::from)
            .unwrap_or(paths.source.join("local/auto-update.status")),
    )
}
fn read_state(paths: &Paths) -> Result<Option<Value>> {
    let file = state_path(paths)?;
    if !r::exists(&file) {
        return Ok(None);
    }
    let raw = r::read(&file, r::LIMIT)?;
    if raw.first() == Some(&b'{') {
        return Ok(Some(r::parse(&raw)?));
    }
    let text = std::str::from_utf8(&raw)
        .map_err(|_| r::Error(2, "auto-update state is not UTF-8".into()))?;
    let mut row = serde_json::Map::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=')
            && matches!(
                key,
                "last_run"
                    | "mode"
                    | "status"
                    | "message"
                    | "source_root"
                    | "local"
                    | "remote"
                    | "upstream"
            )
        {
            row.entry(key).or_insert(json!(value));
        }
    }
    Ok(Some(Value::Object(row)))
}
fn publish(
    paths: &Paths,
    status: &str,
    current: &str,
    target: &str,
    reference: &str,
) -> Result<()> {
    publish_mode(paths, status, current, target, reference, "check")
}
fn publish_mode(
    paths: &Paths,
    status: &str,
    current: &str,
    target: &str,
    reference: &str,
    mode: &str,
) -> Result<()> {
    let path = state_path(paths)?;
    let _guard = r::lock(&path, &paths.cwd)?;
    let source = paths
        .source
        .to_str()
        .filter(|s| !s.contains(['\r', '\n']))
        .ok_or_else(|| {
            r::Error(
                2,
                "auto-update state requires a UTF-8 source path without newlines".into(),
            )
        })?;
    let message = match status {
        "up_to_date" => "already up to date",
        "failed" => "update failed",
        "updated" => "update applied",
        "skipped" => "update deferred by an admission fence",
        _ => "update available",
    };
    let body = format!(
        "last_run={}\nmode={mode}\nstatus={status}\nmessage={message}\nsource_root={source}\nlocal={current}\nremote={target}\nupstream={reference}\n",
        r::now()
    );
    r::atomic(&path, body.as_bytes())
}
fn wired(paths: &Paths) -> Result<(bool, bool)> {
    let units = paths.config.parent().unwrap().join("systemd/user");
    let enabled = units.join("timers.target.wants/oh-my-setting-autoupdate.timer");
    if enabled.is_symlink()
        && r::physical(&enabled)? == r::physical(&units.join("oh-my-setting-autoupdate.timer"))?
    {
        return Ok((true, true));
    }
    let raw = if let Some(file) = std::env::var_os("OH_MY_SETTING_AUTO_UPDATE_CRON_FILE") {
        let file = PathBuf::from(file);
        if r::exists(&file) {
            Some(r::read(&file, r::LIMIT)?)
        } else {
            None
        }
    } else {
        crate::effects::cron(&paths.cwd).ok().flatten()
    };
    let cron = raw.is_some_and(|b| {
        String::from_utf8_lossy(&b).lines().any(|l| {
            !l.trim_start().starts_with('#')
                && (l.contains("auto-update.sh") || l.contains(" auto-update "))
        })
    });
    Ok((cron, false))
}
fn attention(paths: &Paths, flag: &AtomicBool) -> Result<&'static str> {
    let Some(owner) = paths.load()? else {
        return Ok("disabled");
    };
    if crate::receipt::owner(&owner)? != paths.source || owner["components"]["auto_update"] == false
    {
        return Ok("disabled");
    }
    let (wired, systemd) = wired(paths)?;
    if !wired {
        return Ok("unwired");
    }
    let Some(state) = read_state(paths)? else {
        return Ok("no-run");
    };
    let status = state["status"].as_str().unwrap_or("");
    if status.is_empty() {
        return Ok("unknown");
    }
    let last = state["last_run"]
        .as_str()
        .or_else(|| state["checked_at"].as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let installed = owner["installed_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let superseded = installed.zip(last).is_some_and(|(a, b)| a > b);
    if !superseded && status == "failed" {
        return Ok("failed");
    }
    if !superseded && status == "skipped" {
        return Ok("blocked");
    }
    if systemd && let Some(program) = r::find("loginctl", &paths.cwd) {
        let output = r::command(
            program,
            &[
                "show-user".into(),
                rustix::process::geteuid().as_raw().to_string().into(),
                "--property=Linger".into(),
                "--value".into(),
            ],
            &paths.cwd,
            None,
            flag,
            Duration::from_secs(5),
        )?;
        if output.0 == 0 && output.1.strip_suffix(b"\n").unwrap_or(&output.1) == b"no" {
            return Ok("session-only");
        }
    }
    let now = chrono::DateTime::parse_from_rfc3339(&r::now())
        .map_err(|_| r::Error(2, "clock unavailable".into()))?;
    if last.is_some_and(|t| now.signed_duration_since(t).num_seconds() >= 172800) {
        return Ok("overdue");
    }
    if last.is_none() {
        return Ok("unknown");
    }
    Ok("ok")
}
fn fetch(paths: &Paths, reference: &str, fallback: bool, flag: &AtomicBool) -> Result<String> {
    r::git(
        &paths.source,
        &["fetch", "--quiet", "--prune", "--tags", "--", "origin"],
        flag,
    )?;
    if reference != "edge" {
        for candidate in [
            format!("refs/tags/{reference}^{{commit}}"),
            format!("refs/remotes/origin/{reference}^{{commit}}"),
            format!("{reference}^{{commit}}"),
        ] {
            if let Ok(commit) = r::git(
                &paths.source,
                &["rev-parse", "--verify", "--quiet", &candidate],
                flag,
            ) {
                return Ok(commit);
            }
            r::check(flag)?;
        }
        if !fallback {
            return r::fail(
                "pinned update ref did not resolve; use --fallback-to-edge to follow the default branch",
            );
        }
    }
    r::git(&paths.source, &["remote", "set-head", "origin", "-a"], flag)?;
    r::git(
        &paths.source,
        &["rev-parse", "--verify", "refs/remotes/origin/HEAD^{commit}"],
        flag,
    )
}
pub(crate) fn run(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    if options.tool == "auto-update" {
        match options.action.as_str() {
            "install" | "remove" => {
                let mut o = options.clone();
                o.tool = if options.action == "install" {
                    "install-autoupdate"
                } else {
                    "uninstall-autoupdate"
                }
                .into();
                return crate::scheduler::run(paths, &o, flag);
            }
            "status" => {
                let Some(state) = read_state(paths)? else {
                    return Ok(r::text("auto-update: not checked"));
                };
                if options.has("--json") {
                    return r::json(&state);
                }
                let mut out = format!(
                    "auto-update: {}\nlast run: {}",
                    state["status"].as_str().unwrap_or("unknown"),
                    state["last_run"]
                        .as_str()
                        .or_else(|| state["checked_at"].as_str())
                        .unwrap_or("unknown")
                );
                for key in ["upstream", "local", "remote"] {
                    if let Some(value) = state[key].as_str() {
                        out.push_str(&format!(
                            "\n{key}: {}",
                            value
                                .chars()
                                .filter(|c| !c.is_control())
                                .take(160)
                                .collect::<String>()
                        ));
                    }
                }
                return Ok(r::text(out));
            }
            "attention" => {
                let verdict = attention(paths, flag).unwrap_or("unknown");
                return if options.has("--json") {
                    r::json(&json!({"attention":verdict}))
                } else {
                    Ok(r::text(format!("attention: {verdict}")))
                };
            }
            "" | "check" | "apply" => (),
            _ => return r::fail("unknown auto-update mode"),
        }
    }
    if options.preview() && options.has("--probe-agy-surfaces") {
        return Ok(r::text(
            "would certify Antigravity surfaces in an isolated home",
        ));
    }
    if options.has("--probe-agy-surfaces") {
        if options.has("--check") || options.has("--rollback") {
            return r::fail("--probe-agy-surfaces cannot be combined with --check or --rollback");
        }
        return crate::agy_surface::probe(paths, flag);
    }
    let old = paths.require_owner()?;
    let reference = options
        .get("--ref")
        .map(str::to_owned)
        .or_else(|| {
            old.as_ref()
                .and_then(|r| r["ref"].as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| "edge".into());
    if reference.is_empty()
        || reference.starts_with(['-', '/', '.'])
        || reference.ends_with(['/', '.'])
        || reference.contains("..")
        || reference.contains("//")
        || reference.contains("/.")
        || reference.split('/').any(|s| s.ends_with(".lock"))
        || !reference
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
    {
        return r::fail("invalid update ref");
    }
    let current = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    if options.preview() {
        return r::json(
            &json!({"schema":1,"current":current,"ref":reference,"actions":["fetch target","verify clean checkout and worker/call fences","verify native payload","reconcile recorded components","doctor gate","publish receipt"],"not_previewed":["fetch","provider refresh"]}),
        );
    }
    r::child_guard()?;
    let check = options.has("--check")
        || options.tool == "auto-update" && options.action != "apply" && !options.has("--apply");
    if !check {
        let result = apply_update(paths, options, flag, services, &reference);
        if options.tool == "auto-update"
            && let Err(error) = &result
        {
            publish_mode(
                paths,
                if matches!(error.0, 75 | 76) {
                    "skipped"
                } else {
                    "failed"
                },
                &current,
                "",
                &reference,
                "apply",
            )?;
        }
        return result;
    }
    let raw = r::git(&paths.source, &["rev-parse", "--git-common-dir"], flag)?;
    let raw = PathBuf::from(raw);
    let file = if raw.is_absolute() {
        raw
    } else {
        paths.source.join(raw)
    };
    let file = file.join("FETCH_HEAD");
    let _fetch_guard = r::lock(&file, &paths.cwd)?;
    let current = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    let target = match fetch(paths, &reference, options.has("--fallback-to-edge"), flag) {
        Ok(target) => target,
        Err(error) => {
            if options.tool == "auto-update" {
                publish(paths, "failed", &current, "", &reference)?;
            }
            return Err(error);
        }
    };
    if target.len() != 40 || !target.bytes().all(|b| b.is_ascii_hexdigit()) {
        return r::fail("invalid fetched update commit");
    }
    let result = json!({"schema":1,"checked_at":r::now(),"current_commit":current,"target_commit":target,"update_available":current!=target,"ref":reference});
    if options.tool == "auto-update" {
        publish(
            paths,
            if current == target {
                "up_to_date"
            } else {
                "update_available"
            },
            &current,
            &target,
            &reference,
        )?;
    }
    if options.has("--json") {
        r::json(&result)
    } else {
        Ok(r::text(format!(
            "update-check: {} {current}{}",
            if current == target {
                "up_to_date"
            } else {
                "available"
            },
            if current == target {
                String::new()
            } else {
                format!(" -> {target}")
            }
        )))
    }
}

pub(crate) fn process(pid: u32, flag: &AtomicBool, cwd: &std::path::Path) -> Option<(String, u32)> {
    if let Ok(raw) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let fields = raw
            .rsplit_once(") ")?
            .1
            .split_whitespace()
            .collect::<Vec<_>>();
        return Some((
            format!("proc:{}", fields.get(19)?),
            fields.get(1)?.parse().ok()?,
        ));
    }
    let ps = r::find("ps", cwd).or_else(|| {
        [
            std::path::Path::new("/bin/ps"),
            std::path::Path::new("/usr/bin/ps"),
        ]
        .into_iter()
        .find(|p| p.is_file())
        .map(std::path::Path::to_path_buf)
    })?;
    let out = r::command_env(
        ps,
        &[
            "-p".into(),
            pid.to_string().into(),
            "-o".into(),
            "ppid=".into(),
            "-o".into(),
            "lstart=".into(),
        ],
        cwd,
        None,
        flag,
        Duration::from_secs(3),
        &[("LC_ALL", std::ffi::OsStr::new("C"))],
    )
    .ok()?;
    if out.0 != 0 {
        return None;
    }
    let text = String::from_utf8(out.1).ok()?;
    let mut words = text.split_whitespace();
    let parent = words.next()?.parse().ok()?;
    Some((
        format!("ps:{}", words.collect::<Vec<_>>().join(" ")),
        parent,
    ))
}
pub(crate) fn ancestor(pid: u32, flag: &AtomicBool, paths: &Paths) -> bool {
    let mut current = std::process::id();
    for _ in 0..64 {
        if current == pid {
            return true;
        }
        if current <= 1 {
            return false;
        }
        let Some((_, parent)) = process(current, flag, &paths.cwd) else {
            return false;
        };
        if parent == current {
            return false;
        }
        current = parent;
    }
    false
}
fn text_field(path: &std::path::Path) -> Option<String> {
    String::from_utf8(r::read(path, 65536).ok()?)
        .ok()
        .map(|s| s.trim().to_owned())
}
pub(crate) fn fences(paths: &Paths, flag: &AtomicBool) -> Result<()> {
    let workers = std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or(paths.home.join(".cache"))
                .join("oh-my-setting/worktrees")
        });
    if workers.is_dir() {
        for row in std::fs::read_dir(workers)? {
            let row = row?;
            if !row
                .file_name()
                .to_string_lossy()
                .starts_with("oh-my-setting-delegate.")
            {
                continue;
            }
            let marker = row.path().join(".oh-my-setting-tmp");
            let Some(raw) = text_field(&marker) else {
                continue;
            };
            let field = |name| {
                raw.lines().find_map(|l| {
                    l.split_once('=')
                        .filter(|(k, _)| *k == name)
                        .map(|(_, v)| v)
                })
            };
            if field("kind") != Some("oh-my-setting-temp") {
                continue;
            }
            let Some(pid) = field("pid")
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|n| *n > 0)
            else {
                return Err(r::Error(
                    76,
                    "installation deferred: unproven worker identity".into(),
                ));
            };
            if let Some((start, _)) = process(pid, flag, &paths.cwd)
                && field("process-start").is_none_or(|v| v == start)
            {
                return Err(r::Error(76, "installation deferred: live worker".into()));
            }
        }
    }
    let mut roots = std::collections::BTreeSet::from([paths.source.clone(), paths.cwd.clone()]);
    if let Some(more) = std::env::var_os("OMS_UPDATE_CALL_REPOS") {
        for path in more.to_string_lossy().lines().filter(|s| !s.is_empty()) {
            roots.insert(r::physical(&PathBuf::from(path))?);
        }
    }
    for root in roots {
        for sub in [".oms", ".oms/locks", ".oms/locks/canonical-sync.d"] {
            let path = root.join(sub);
            if r::exists(&path) && (path.is_symlink() || !path.is_dir()) {
                return Err(r::Error(
                    76,
                    "installation deferred: unsafe panel registry".into(),
                ));
            }
        }
        let registry = root.join(".oms/locks/canonical-sync.d");
        if !registry.exists() {
            continue;
        }
        for row in std::fs::read_dir(registry)? {
            let entry = row?.path();
            let pid = text_field(&entry.join("pid"))
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|n| *n > 0);
            let saved = text_field(&entry.join("process-start"));
            if !entry.is_symlink()
                && entry.is_dir()
                && let (Some(pid), Some(saved)) = (pid, saved.filter(|s| !s.is_empty()))
            {
                if let Some((actual, _)) = process(pid, flag, &paths.cwd) {
                    if actual != saved || ancestor(pid, flag, paths) {
                        continue;
                    }
                } else if !crate::lifecycle_lock::alive(pid) {
                    continue;
                }
            }
            return Err(r::Error(
                76,
                "installation deferred: live or unproven panel call".into(),
            ));
        }
    }
    Ok(())
}
fn gate(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
    phase: crate::IncomingPhase,
    revision: &str,
    old: &Value,
) -> Result<crate::Output> {
    let out = services.incoming_lifecycle(
        phase,
        &paths.source,
        revision,
        &r::bytes(old)?,
        &options.raw,
        flag,
    );
    if out.0 != 0 {
        return Err(r::Error(
            out.0,
            format!("incoming native lifecycle {phase:?} failed; provider output withheld"),
        ));
    }
    r::check(flag)?;
    Ok(out)
}
fn managed_paths(paths: &Paths, old: &Value) -> Result<Vec<PathBuf>> {
    let mut targets = std::collections::BTreeSet::new();
    for name in [
        ".codex/AGENTS.md",
        ".claude/CLAUDE.md",
        ".gemini/AGENTS.md",
        ".oh-my-setting-prompts",
        ".oh-my-setting-workflows",
        ".local/bin/oms",
        ".claude/output-styles/oms-korean.md",
        ".claude.json",
        ".gemini/config/plugins/oh-my-setting",
    ] {
        targets.insert(paths.home.join(name));
    }
    for name in [
        ".codex/skills",
        ".claude/skills",
        ".gemini/config/skills",
        ".gemini/antigravity/skills",
    ] {
        let root = paths.home.join(name);
        if root.is_dir() {
            for entry in std::fs::read_dir(&root)? {
                let path = entry?.path();
                if path
                    .file_name()
                    .is_some_and(|s| s.to_string_lossy().starts_with("oms-"))
                {
                    targets.insert(path);
                }
            }
        }
        for source in crate::install::skill_sources(paths)? {
            if let Some(name) = source.file_name() {
                targets.insert(root.join(name));
            }
        }
    }
    targets.extend([
        paths.claude_settings()?,
        paths.codex_config()?,
        paths.receipt.clone(),
        crate::snapshot::output_path(paths)?,
        paths.config.join("capabilities.json"),
    ]);
    if let Some(file) = old["native"]["frontdoor"].as_str() {
        targets.insert(PathBuf::from(file));
    }
    targets.insert(paths.codex_home()?.join("config.toml"));
    for rel in [
        "plugins/cache/oh-my-setting-local/oh-my-setting",
        "plugins/marketplaces/oh-my-setting-local",
    ] {
        targets.insert(paths.codex_home()?.join(rel));
    }
    let original = targets.iter().cloned().collect::<Vec<_>>();
    for target in original {
        targets.insert(crate::install::marker(&target));
        if let Some(name) = target.file_name() {
            for suffix in [".oms-bak", ".oh-my-setting-backup"] {
                targets
                    .insert(target.with_file_name(format!("{}{suffix}", name.to_string_lossy())));
            }
        }
        if let Some(parent) = target.parent().filter(|p| p.is_dir()) {
            for entry in std::fs::read_dir(parent)? {
                let path = entry?.path();
                if path
                    .file_name()
                    .zip(target.file_name())
                    .is_some_and(|(n, t)| {
                        n.to_string_lossy()
                            .starts_with(&format!("{}.backup.", t.to_string_lossy()))
                    })
                {
                    targets.insert(path);
                }
            }
        }
    }
    Ok(targets.into_iter().collect())
}
fn apply_update(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
    reference: &str,
) -> Result<crate::Output> {
    use crate::{IncomingPhase as Phase, transaction::Transaction};
    r::child_guard()?;
    if borrowed(paths).unwrap_or(false) {
        return r::fail("an incoming gate cannot recursively update the installation");
    }

    fences(paths, flag)?;
    let _operation = crate::lifecycle_lock::acquire(paths, flag)?;
    let _guard = r::lock(&paths.receipt, &paths.cwd)?;
    let mut tx = Transaction::begin_update(paths)?;
    let old = paths
        .require_owner()?
        .ok_or_else(|| r::Error(2, "update requires an install receipt".into()))?;
    let lease_path = paths.config.join("native-update/lease.json");
    if r::exists(&lease_path) {
        std::fs::remove_file(&lease_path)?;
        r::sync_dir(lease_path.parent().unwrap())?;
    }
    let current = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    if !r::git(
        &paths.source,
        &["status", "--porcelain", "--untracked-files=normal"],
        flag,
    )?
    .is_empty()
    {
        return r::fail("refusing update or rollback from a dirty install checkout");
    }
    let common = PathBuf::from(r::git(
        &paths.source,
        &["rev-parse", "--git-common-dir"],
        flag,
    )?);
    let common = if common.is_absolute() {
        common
    } else {
        paths.source.join(common)
    };
    let _fetch = r::lock(&common.join("FETCH_HEAD"), &paths.cwd)?;
    let target = if options.has("--rollback") {
        old["previous_commit"]
            .as_str()
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| r::Error(2, "install receipt has no rollback commit".into()))?
            .to_owned()
    } else {
        fetch(paths, reference, options.has("--fallback-to-edge"), flag)?
    };
    let resolved = r::git(
        &paths.source,
        &["rev-parse", "--verify", &format!("{target}^{{commit}}")],
        flag,
    )?;
    if resolved != target || target.len() != 40 {
        return r::fail("invalid update commit");
    }
    if std::env::var("OH_MY_SETTING_UPDATE_EXPECTED_TARGET").is_ok_and(|v| v != target) {
        return Err(r::Error(75, "update target changed after preflight".into()));
    }
    let mut target = target;
    if let Ok(selected) = std::env::var("OH_MY_SETTING_UPDATE_TARGET_COMMIT") {
        if !std::env::var("OH_MY_SETTING_UPDATE_EXPECTED_TARGET").is_ok_and(|s| !s.is_empty())
            || selected.len() != 40
            || !selected.bytes().all(|b| b.is_ascii_hexdigit())
            || r::git(
                &paths.source,
                &["merge-base", "--is-ancestor", &current, &selected],
                flag,
            )
            .is_err()
            || r::git(
                &paths.source,
                &["merge-base", "--is-ancestor", &selected, &target],
                flag,
            )
            .is_err()
        {
            return Err(r::Error(
                75,
                "selected update target is not a checked fast-forward commit".into(),
            ));
        }
        target = selected;
    }
    if options.tool == "auto-update" && !options.has("--rollback") {
        r::git(
            &paths.source,
            &["merge-base", "--is-ancestor", &current, &target],
            flag,
        )?;
    }
    gate(
        paths,
        options,
        flag,
        services,
        Phase::Preflight,
        &target,
        &old,
    )?;
    let branch = r::git(&paths.source, &["symbolic-ref", "--quiet", "HEAD"], flag).ok();
    let edge_branch = if reference == "edge" && !options.has("--rollback") {
        let remote = r::git(
            &paths.source,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            flag,
        )?;
        let suffix = remote
            .strip_prefix("refs/remotes/origin/")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| r::Error(2, "invalid origin default branch".into()))?;
        let name = format!("refs/heads/{suffix}");
        let before = r::git(&paths.source, &["rev-parse", "--verify", &name], flag).ok();
        if let Some(tip) = &before {
            r::git(
                &paths.source,
                &["merge-base", "--is-ancestor", tip, &target],
                flag,
            )?;
        }
        tx.effect(crate::effects::Effect::GitRef {
            source: paths.source.clone(),
            name: name.clone(),
            before: before.clone(),
            after: target.clone(),
        })?;
        Some((name, before))
    } else {
        None
    };
    let original_targets = managed_paths(paths, &old)?;
    let start = tx.prepare_external(&original_targets)?;
    tx.effect(crate::effects::Effect::GitCheckout {
        source: paths.source.clone(),
        before: current.clone(),
        after: target.clone(),
        branch: branch.clone(),
    })?;
    let _lease = Lease::new(paths, flag)?;
    let result = (|| -> Result<Value> {
        r::git(&paths.source, &["checkout", "--detach", &target], flag)?;
        let mut known = managed_paths(paths, &old)?;
        if old["native"]["revision"] != target {
            known.push(paths.data.join("libexec").join(&target));
            known.push(paths.source.join("target/oms-native").join(&target));
        }
        let fresh = known
            .into_iter()
            .filter(|p| !original_targets.contains(p))
            .collect::<Vec<_>>();
        tx.prepare_external(&fresh)?;
        let raw = gate(
            paths,
            options,
            flag,
            services,
            Phase::Reconcile,
            &target,
            &old,
        )?;
        let mut row = r::parse(&raw.1)?;
        crate::receipt::validate(&row)?;
        if crate::receipt::owner(&row)? != paths.source
            || row["commit"] != target
            || row["native"]["revision"] != target
        {
            return r::fail("incoming gate returned a mismatched install receipt");
        }
        if !options.has("--no-doctor") {
            gate(paths, options, flag, services, Phase::Verify, &target, &row)?;
        }
        if !r::git(
            &paths.source,
            &["status", "--porcelain", "--untracked-files=normal"],
            flag,
        )?
        .is_empty()
        {
            return r::fail(
                "incoming gate changed the tracked checkout; rollback recovery retained",
            );
        }
        if r::git(&paths.source, &["rev-parse", "HEAD"], flag)? != target {
            return Err(r::Error(75, "checkout changed during incoming gate".into()));
        }
        row["ref"] = json!(reference);
        row["previous_commit"] = if current == target && !options.has("--rollback") {
            old["previous_commit"].clone()
        } else {
            json!(current)
        };
        Ok(row)
    })();
    tx.finish_external(start)?;
    let row = match result {
        Ok(row) => row,
        Err(mut error) => {
            drop(tx);
            if paths.config.join("native-update/transaction.json").exists() {
                error
                    .1
                    .push_str("; rollback incomplete, recovery journal and prior payload retained");
            }
            if options.tool == "auto-update" {
                publish_mode(paths, "failed", &current, &target, reference, "apply")?;
            }
            return Err(error);
        }
    };
    if let Some((name, before)) = edge_branch {
        let previous = before.unwrap_or_else(|| "0000000000000000000000000000000000000000".into());
        r::git(
            &paths.source,
            &["update-ref", &name, &target, &previous],
            flag,
        )?;
        r::git(
            &paths.source,
            &["checkout", name.trim_start_matches("refs/heads/")],
            flag,
        )?;
    }
    let mut row = row;
    row["channel"] = json!(
        r::git(
            &paths.source,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            flag
        )
        .unwrap_or_else(|_| "detached".into())
    );
    tx.write(&paths.receipt, &r::bytes(&row)?, 0o600)?;
    tx.commit()?;
    if !options.has("--no-tools")
        && !options.has("--rollback")
        && let Err(error) = gate(
            paths,
            options,
            flag,
            services,
            Phase::RefreshTools,
            &target,
            &row,
        )
    {
        return Err(r::Error(
            error.0,
            "core update succeeded; provider/tool refresh failed and was not rolled back".into(),
        ));
    }
    if options.tool == "auto-update" {
        publish_mode(
            paths,
            if current == target {
                "up_to_date"
            } else {
                "updated"
            },
            &current,
            &target,
            reference,
            "apply",
        )?;
    }
    Ok(r::text(format!(
        "updated: {current} -> {target}\nupdate: ok"
    )))
}

struct Lease {
    path: PathBuf,
    raw: Vec<u8>,
}
impl Lease {
    fn new(paths: &Paths, flag: &AtomicBool) -> Result<Self> {
        let path = paths.config.join("native-update/lease.json");
        let pid = std::process::id();
        let (generation, _) = process(pid, flag, &paths.cwd)
            .ok_or_else(|| r::Error(75, "cannot prove update process generation".into()))?;
        let raw = r::bytes(
            &json!({"schema":1,"pid":pid,"generation":generation,"receipt":paths.receipt}),
        )?;
        r::atomic(&path, &raw)?;
        Ok(Self { path, raw })
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self
            .path
            .parent()
            .unwrap()
            .join("transaction.json")
            .exists()
            && r::read(&self.path, 65536).is_ok_and(|v| v == self.raw)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
pub(crate) fn borrowed(paths: &Paths) -> Result<bool> {
    let file = paths.config.join("native-update/lease.json");
    if !r::exists(&file) {
        return Ok(false);
    }
    let row = r::value(&file)?;
    let pid = row["pid"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| r::Error(75, "invalid update lease".into()))?;
    let flag = AtomicBool::new(false);
    let expected = std::env::var("OMS_NATIVE_UPDATE_PARENT")
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    if row["schema"] == 1
        && row["receipt"] == json!(paths.receipt)
        && expected == Some(pid)
        && process(pid, &flag, &paths.cwd).is_some_and(|(g, _)| row["generation"] == g)
        && ancestor(pid, &flag, paths)
        && r::file_lock(&paths.receipt, &paths.cwd)?.probe() == oms_lock::Status::Held
    {
        return Ok(true);
    }
    Err(r::Error(75,"installation deferred: interrupted or concurrent update; retry the owning update for recovery".into()))
}
