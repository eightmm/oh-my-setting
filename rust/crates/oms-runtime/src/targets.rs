//! Local target metadata never grants remote command authority.
use crate::services::TargetRequest;
use crate::storage::Runtime;
use crate::value::*;
use crate::{Result, error, require};
use std::path::Path;
pub const FILE: &str = ".oms/runtime/targets.json";
fn name(s: &str, label: &str) -> Result<()> {
    require(
        !s.is_empty()
            && s.len() <= 64
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        format!(
            "{label} must be 1..64 letters, digits, dots, underscores or hyphens, starting with a letter or digit"
        ),
    )
}
fn record(v: &Value) -> Result<()> {
    keys(v, &["ssh_alias", "workdir"], &[])?;
    name(v.text("ssh_alias"), "SSH alias")?;
    let workdir = v.get("workdir").str()?;
    require(
        !workdir.chars().any(char::is_control),
        "remote workdir must be a path without control characters",
    )?;
    require(
        workdir.starts_with('/')
            || workdir.strip_prefix("~/").is_some_and(|s| {
                !s.is_empty() && s.split('/').all(|s| !matches!(s, "" | "." | ".."))
            }),
        "remote workdir must be ~/REL without dot components",
    )?;
    require(
        !crate::capsule::sensitive_value(workdir),
        "remote workdir contains sensitive-looking material",
    )
}
pub fn listing(rt: &Runtime<'_>) -> Result<Value> {
    let v = rt.json(FILE, object([("schema", number(1)), ("targets", empty())]))?;
    keys(&v, &["schema", "targets"], &[])?;
    require(v.get("schema").is_int(1), "invalid remote target registry")?;
    for (k, v) in v.get("targets").object()? {
        name(std::str::from_utf8(k).map_err(error)?, "target name")?;
        record(v)?;
    }
    Ok(v)
}
pub fn update(
    rt: &Runtime<'_>,
    target: &str,
    ssh: Option<&str>,
    workdir: Option<&str>,
) -> Result<Value> {
    name(target, "target name")?;
    let _guard = rt.lock(Path::new(FILE))?;
    rt.invalidate(Path::new(FILE));
    let mut value = listing(rt)?;
    let mut entries = value.get("targets").clone();
    if let Some(ssh) = ssh {
        name(ssh, "SSH alias")?;
        let home=std::env::var_os("HOME").ok_or_else(||error("repository is outside HOME; same-path remote targets require a repository under HOME"))?;
        let home = std::fs::canonicalize(home).map_err(error)?;
        let relative=rt.repo.strip_prefix(&home).map_err(|_|error("repository is outside HOME; same-path remote targets require a repository under HOME"))?;
        require(
            !relative.as_os_str().is_empty(),
            "repository cannot equal HOME",
        )?;
        let expected = format!(
            "~/{}",
            relative
                .to_str()
                .ok_or_else(|| error("remote workdir must be UTF-8"))?
        );
        require(
            workdir.is_none_or(|w| w == expected),
            format!(
                "remote workdir must equal the repository's resolved home-relative path: {expected}"
            ),
        )?;
        let row = object([("ssh_alias", string(ssh)), ("workdir", string(expected))]);
        record(&row)?;
        require(
            entries.opt(target).is_none(),
            format!("remote target already exists: {target}"),
        )?;
        entries.set(target, row)?;
    } else {
        require(
            entries.remove(target).is_some(),
            format!("unknown remote target: {target}"),
        )?;
    }
    value.set("targets", entries)?;
    let mut bytes = sorted_pretty(&value)?;
    bytes.push(b'\n');
    rt.write_locked(Path::new(FILE), &bytes, crate::storage::MAX_STATE)?;
    Ok(value)
}
pub fn check(rt: &Runtime<'_>, target: &str) -> Result<Value> {
    let valid = name(target, "target name").and_then(|_| {
        require(
            listing(rt)?.get("targets").opt(target).is_some(),
            format!("unknown remote target: {target}"),
        )
    });
    let reason = match &valid {
        Ok(_) => "SSH target execution is not yet supported".into(),
        Err(e) => format!("{e}; SSH target execution is not yet supported"),
    };
    Ok(object([
        ("profile", string("remote")),
        ("ready", b(false)),
        ("target_valid", b(valid.is_ok())),
        ("reason", string(reason)),
        ("missing", list(["ssh-execution-not-supported"])),
        ("details", object([("target", string(target))])),
        ("declared", object([("transport", string("ssh-alias"))])),
        (
            "enforced",
            object([("filesystem", b(false)), ("network", b(false))]),
        ),
        (
            "unknown",
            list([
                "SSH alias resolution",
                "remote filesystem",
                "remote network",
                "remote cleanup",
            ]),
        ),
        (
            "warning",
            string("Target metadata is local only; no SSH connection or isolation is established."),
        ),
    ]))
}
pub fn inspect(
    rt: &Runtime<'_>,
    target: &str,
    sync: bool,
    sha: Option<&str>,
    timeout: i128,
) -> Result<Value> {
    require(
        (10..=7200).contains(&timeout),
        "--timeout must be between 10 and 7200 seconds",
    )?;
    name(target, "target name")?;
    let mut result = object([
        ("schema", number(1)),
        ("target", string(target)),
        ("ok", b(false)),
        ("reason", string("missing")),
    ]);
    if sync {
        result.set("before_sha", Value::Null)?;
        result.set("after_sha", Value::Null)?;
    }
    let registry = listing(rt)?;
    let Some(row) = registry.get("targets").opt(target) else {
        result.set("reason", string("unknown-target"))?;
        return Ok(result);
    };
    if row.text("workdir").starts_with('/') {
        result.set("reason", string("legacy-absolute"))?;
        result.set("fix",string(format!("oms runtime backend target remove {target} && oms runtime backend target add {target} --ssh-alias {}",row.text("ssh_alias"))))?;
        return Ok(result);
    }
    if sync {
        require(
            sha.is_none_or(|s| {
                (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
            }),
            "--sha must be a full 40- or 64-character commit SHA",
        )?;
    }
    rt.services.target_transport(
        &rt.repo,
        &TargetRequest {
            name: target.into(),
            ssh_alias: row.text("ssh_alias").into(),
            workdir: row.text("workdir").into(),
            sync,
            sha: sha.map(str::to_owned),
            timeout: std::time::Duration::from_secs(timeout as u64),
        },
    )
}
