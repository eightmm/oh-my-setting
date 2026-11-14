//! Backend semantics and resource validation before the native supervisor boundary.
use crate::parser::Request;
use crate::services::{BackendRequest, BackendResult};
use crate::storage::Runtime;
use crate::value::*;
use crate::{Result, error, require};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;
pub fn describe(profile: &str) -> Result<Value> {
    let row = match profile {
        "trusted-local" => object([
            ("profile", string(profile)),
            (
                "declared",
                object([
                    ("filesystem", string("host-user")),
                    ("network", string("host")),
                    (concat!("creden", "tials"), string("ambient-host-readable")),
                    ("wall_clock", b(true)),
                ]),
            ),
            (
                "enforced",
                object([
                    ("filesystem", b(false)),
                    ("network", b(false)),
                    (concat!("creden", "tials"), b(false)),
                    ("wall_clock", b(true)),
                    ("process_group", b(true)),
                ]),
            ),
            (
                "unknown",
                list([
                    "reads",
                    "writes outside repository",
                    "background process escape on unsupported hosts",
                ]),
            ),
            (
                "warning",
                string("Process supervision only; this is not a sandbox."),
            ),
        ]),
        "isolated" => object([
            ("profile", string(profile)),
            (
                "declared",
                object([
                    ("repo", string("read-only")),
                    ("worktree", string("explicit-read-write")),
                    ("network", string("off-by-default")),
                    (concat!("creden", "tials"), string("not-mounted")),
                    ("wall_clock", b(true)),
                ]),
            ),
            (
                "enforced",
                object([
                    ("filesystem", string("container-mount-policy")),
                    ("network", string("container-network-policy")),
                    (
                        concat!("creden", "tials"),
                        string("environment-and-mount-filter"),
                    ),
                    ("wall_clock", b(true)),
                    ("capabilities", string("drop-all")),
                    ("no_new_privileges", b(true)),
                ]),
            ),
            (
                "unknown",
                list([
                    "container image contents",
                    "container daemon trust",
                    "kernel isolation",
                ]),
            ),
            (
                "warning",
                string("Image contents and the container daemon remain operator-owned."),
            ),
        ]),
        "remote" => object([
            ("profile", string(profile)),
            (
                "declared",
                object([
                    ("transport", string("external-adapter")),
                    ("isolation", string("external-adapter")),
                    ("wall_clock", b(true)),
                ]),
            ),
            ("enforced", object([("wall_clock", b(true))])),
            (
                "unknown",
                list([
                    "remote authentication",
                    "remote filesystem",
                    "remote network",
                    "remote cleanup",
                ]),
            ),
            (
                "warning",
                string("Isolation claims are adapter attestations, not locally enforced facts."),
            ),
        ]),
        _ => return Err(error(format!("unsupported execution profile: {profile}"))),
    };
    Ok(row)
}
pub fn check(rt: &Runtime<'_>, profile: &str, image: &OsStr, adapter: &OsStr) -> Result<Value> {
    let mut row = describe(profile)?;
    let image = if image.is_empty() {
        std::env::var_os("OMS_ISOLATED_IMAGE").unwrap_or_default()
    } else {
        image.to_owned()
    };
    let adapter = if adapter.is_empty() {
        std::env::var_os("OMS_REMOTE_ADAPTER").unwrap_or_default()
    } else {
        adapter.to_owned()
    };
    let readiness = if profile == "trusted-local" {
        object([("ready", b(true)), ("missing", a([])), ("details", empty())])
    } else {
        rt.services.backend_check(profile, &image, &adapter)?
    };
    require(
        matches!(readiness.get("ready"), Value::Bool(_))
            && readiness.get("missing").strings().is_ok()
            && readiness.get("details").object().is_ok(),
        "native backend readiness projection is invalid",
    )?;
    for key in ["ready", "missing", "details"] {
        row.set(key, readiness.get(key).clone())?;
    }
    Ok(row)
}
pub fn request(
    rt: &Runtime<'_>,
    r: &Request,
    profile: &str,
    default_wall: i128,
) -> Result<BackendRequest> {
    describe(profile)?;
    let wall = r.integer("timeout-seconds", default_wall)?;
    let log = r.integer("log-limit", 4 * 1024 * 1024)?;
    let memory = r.integer("memory-mb", 4096)?;
    let cpus = r.float("cpus", 2.0)?;
    let pids = r.integer("pids-limit", 512)?;
    let tmpfs = r.integer("tmpfs-mb", 1024)?;
    require(
        (1..=86400).contains(&wall),
        "timeout must be between 1 and 86400 seconds",
    )?;
    require(
        (1024..=64 * 1024 * 1024).contains(&log),
        "log limit must be between 1024 and 67108864 bytes",
    )?;
    require(
        (64..=1048576).contains(&memory),
        "memory limit must be between 64 and 1048576 MiB",
    )?;
    require(
        cpus.is_finite() && (0.1..=1024.0).contains(&cpus),
        "CPU limit must be between 0.1 and 1024",
    )?;
    require(
        (16..=1000000).contains(&pids),
        "PID limit must be between 16 and 1000000",
    )?;
    require(
        (16..=1048576).contains(&tmpfs),
        "tmpfs limit must be between 16 and 1048576 MiB",
    )?;
    let argv = r.tail.clone();
    require(!argv.is_empty(), "execution requires a command after --")?;
    let command = argv
        .iter()
        .map(|s| s.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    require(
        !crate::capsule::sensitive_value(&command),
        "command contains sensitive-looking material",
    )?;
    let inherit = r.all("inherit-env");
    for key in &inherit {
        let name = key
            .to_str()
            .ok_or_else(|| error("invalid environment name"))?;
        let field = object([(name, number(0))]);
        require(
            crate::capsule::sanitize(&field, "").is_some_and(|v| v.opt(name).is_some()),
            "refusing to inherit sensitive environment variable",
        )?;
        if let Some(value) = std::env::var_os(key) {
            require(
                !crate::capsule::sensitive_value(&format!("{}={}", name, value.to_string_lossy())),
                "refusing sensitive-looking environment value",
            )?;
        }
    }
    let resolve = |key: &str| -> Result<Option<PathBuf>> {
        r.os(key)
            .filter(|s| !s.is_empty())
            .map(|s| rt.user_path(s))
            .transpose()
    };
    let worktree = resolve("worktree")?;
    let cwd = worktree
        .as_ref()
        .map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                rt.repo.join(p)
            }
        })
        .unwrap_or_else(|| rt.repo.clone());
    let info = std::fs::symlink_metadata(&cwd).map_err(error)?;
    require(
        info.is_dir() && !info.file_type().is_symlink(),
        "execution directory must be a real directory",
    )?;
    let cwd = std::fs::canonicalize(cwd).map_err(error)?;
    Ok(BackendRequest {
        profile: profile.into(),
        argv,
        cwd,
        worktree,
        image: r
            .os("image")
            .map(OsStr::to_os_string)
            .or_else(|| std::env::var_os("OMS_ISOLATED_IMAGE"))
            .unwrap_or_default(),
        adapter: r
            .os("adapter")
            .filter(|s| !s.is_empty())
            .map(OsStr::to_os_string)
            .or_else(|| std::env::var_os("OMS_REMOTE_ADAPTER"))
            .unwrap_or_default(),
        allow_network: r.has("allow-network"),
        inherit_env: inherit,
        wall: Duration::from_secs(wall as u64),
        log: resolve("log")?,
        receipt: resolve("receipt")?,
        log_limit: log as usize,
        memory_mb: memory as u64,
        cpus,
        pids: pids as u64,
        tmpfs_mb: tmpfs as u64,
    })
}
pub fn execute(rt: &Runtime<'_>, request: &BackendRequest) -> Result<BackendResult> {
    rt.parent_write()?;
    let readiness = check(rt, &request.profile, &request.image, &request.adapter)?;
    if !readiness.get("ready").truth() {
        return Err(crate::status(
            3,
            format!(
                "execution backend is not ready: {}",
                readiness.get("missing").strings()?.join(", ")
            ),
        ));
    }
    rt.services.supervise(&rt.repo, request)
}
