//! Immutable execution identities bound to a clean checkout.
use crate::router::Response;
use crate::storage::{MAX_STATE, Runtime};
use crate::value::*;
use crate::{Result, Services, error, require};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "rust/Cargo.lock",
    "uv.lock",
    "pixi.lock",
    "conda-lock.yml",
    "poetry.lock",
    "package-lock.json",
];

pub fn run(
    args: &[OsString],
    cwd: &Path,
    install: &Path,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Response {
    execute(args, cwd, install, services, interrupted).unwrap_or_else(Response::failure)
}
fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(error)?;
    require(
        out.status.success(),
        format!("git failed: {}", String::from_utf8_lossy(&out.stderr)),
    )?;
    Ok(out.stdout)
}
pub(crate) fn clean(repo: &Path) -> Result<()> {
    let status = git(
        repo,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
    )?;
    let mut entries = status.split(|b| *b == 0);
    while let Some(entry) = entries.next() {
        if entry.is_empty() {
            continue;
        }
        require(entry.len() >= 4, "invalid git status")?;
        let path = &entry[3..];
        let allowed = path == b".oms" || path == b".oms/" || path.starts_with(b".oms/");
        let rename = entry[0] == b'R' || entry[0] == b'C' || entry[1] == b'R' || entry[1] == b'C';
        if rename {
            let other = entries
                .next()
                .ok_or_else(|| error("invalid git status rename"))?;
            require(
                allowed && (other == b".oms" || other.starts_with(b".oms/")),
                "dirty working tree",
            )?;
        } else {
            require(allowed, "dirty working tree")?;
        }
    }
    let subs = git(repo, &["submodule", "status", "--recursive"])?;
    require(
        subs.split(|b| *b == b'\n')
            .all(|line| line.is_empty() || line[0] == b' '),
        "dirty or unavailable submodule",
    )
}
fn valid_path(path: &str) -> Result<()> {
    require(
        !path.is_empty()
            && !path.chars().any(char::is_control)
            && Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "invalid lockfile path",
    )
}
fn lock_digest(rt: &Runtime<'_>, path: &str, required: bool) -> Result<Option<String>> {
    valid_path(path)?;
    let mut current = rt.repo.clone();
    for component in Path::new(path).components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(m) => require(
                !m.file_type().is_symlink(),
                "lockfile path contains a symlink",
            )?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
            Err(e) => return Err(error(e)),
        }
    }
    require(
        std::fs::symlink_metadata(&current)
            .map_err(error)?
            .is_file(),
        "lockfile must be regular",
    )?;
    let raw = rt
        .bytes(path, MAX_STATE)?
        .ok_or_else(|| error("lockfile missing"))?;
    Ok(Some(sha(raw.as_slice())))
}
fn execute(
    args: &[OsString],
    cwd: &Path,
    install: &Path,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Result<Response> {
    if args.is_empty() || args.iter().any(|v| v == "--help" || v == "-h") {
        return Ok(Response::text(
            "usage: oms runtime exec-snapshot <create|verify> [--repo PATH] [--lockfile REL] [--snapshot SHA256] [--json] [--pretty]",
        ));
    }
    let action = args[0].to_str().ok_or_else(|| error("invalid action"))?;
    require(
        matches!(action, "create" | "verify"),
        "invalid snapshot action",
    )?;
    if action == "create" {
        require(
            std::env::var_os("OMS_HARNESS_CHILD").as_deref() != Some(OsStr::new("1")),
            "a harness child may only use read-only runtime actions",
        )?;
    }
    let mut repo = OsString::from(".");
    let mut snapshot = None;
    let mut locks = Vec::new();
    let mut pretty = false;
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].to_str().ok_or_else(|| error("invalid option"))?;
        match flag {
            "--pretty" => pretty = true,
            "--json" => (),
            "--repo" | "--snapshot" | "--lockfile" => {
                i += 1;
                let arg = args
                    .get(i)
                    .ok_or_else(|| error(format!("{flag} requires a value")))?;
                match flag {
                    "--repo" => repo = arg.clone(),
                    "--snapshot" => {
                        require(snapshot.is_none(), "duplicate snapshot")?;
                        snapshot = Some(
                            arg.to_str()
                                .ok_or_else(|| error("invalid snapshot"))?
                                .to_owned(),
                        );
                    }
                    _ => locks.push(
                        arg.to_str()
                            .ok_or_else(|| error("invalid lockfile"))?
                            .to_owned(),
                    ),
                }
            }
            _ => return Err(error(format!("unknown option: {flag}"))),
        }
        i += 1;
    }
    require(
        action == "create" || locks.is_empty(),
        "--lockfile requires create",
    )?;
    let roots = services.resolve_roots(cwd, &repo)?;
    let rt = Runtime::open(
        &roots.execution_repo,
        &roots.state_repo,
        install,
        services,
        interrupted,
    )?;
    let result = if action == "create" {
        require(snapshot.is_none(), "--snapshot requires verify")?;
        clean(&rt.repo)?;
        let head = String::from_utf8(git(&rt.repo, &["rev-parse", "--verify", "HEAD"])?)
            .map_err(error)?
            .trim()
            .to_owned();
        let tree = String::from_utf8(git(&rt.repo, &["rev-parse", "--verify", "HEAD^{tree}"])?)
            .map_err(error)?
            .trim()
            .to_owned();
        let mut files = std::collections::BTreeMap::new();
        for path in LOCKFILES {
            if let Some(digest) = lock_digest(&rt, path, false)? {
                files.insert((*path).to_owned(), digest);
            }
        }
        for path in locks {
            files.insert(
                path.clone(),
                lock_digest(&rt, &path, true)?.ok_or_else(|| error("lockfile missing"))?,
            );
        }
        let mut entries = empty();
        for (path, digest) in files {
            entries.set(&path, string(digest))?;
        }
        let mut value = object([
            ("schema", number(1)),
            ("head_sha", string(head)),
            ("tree_sha", string(tree)),
            ("lockfiles", entries),
        ]);
        let digest = sha(canonical(&value)?);
        value.set("snapshot_sha256", string(&digest))?;
        let path = PathBuf::from(format!(".oms/runtime/snapshots/{digest}.json"));
        let guard = rt.lock(&path)?;
        let mut bytes = sorted_pretty(&value)?;
        bytes.push(b'\n');
        rt.write_locked(&path, &bytes, MAX_STATE, &guard)?;
        value
    } else {
        let digest = snapshot.ok_or_else(|| error("--snapshot required"))?;
        require(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "invalid snapshot SHA256",
        )?;
        let path = format!(".oms/runtime/snapshots/{digest}.json");
        let value = rt.json(path, Value::Null)?;
        keys(
            &value,
            &[
                "schema",
                "head_sha",
                "tree_sha",
                "lockfiles",
                "snapshot_sha256",
            ],
            &[],
        )?;
        require(
            value.get("schema").is_int(1)
                && value.text("snapshot_sha256") == digest
                && sha(canonical(&without(&value, &["snapshot_sha256"]))?) == digest,
            "snapshot integrity mismatch",
        )?;
        let verify = (|| -> Result<()> {
            clean(&rt.repo)?;
            for (field, spec) in [("head_sha", "HEAD"), ("tree_sha", "HEAD^{tree}")] {
                let actual = String::from_utf8(git(&rt.repo, &["rev-parse", "--verify", spec])?)
                    .map_err(error)?;
                require(
                    actual.trim() == value.text(field),
                    format!("{field} mismatch"),
                )?;
            }
            for (path, expected) in value.get("lockfiles").object()? {
                let path = std::str::from_utf8(path).map_err(error)?;
                require(
                    lock_digest(&rt, path, true)?.as_deref() == Some(expected.str()?),
                    format!("lockfile mismatch: {path}"),
                )?;
            }
            Ok(())
        })();
        match verify {
            Ok(()) => {
                return Response::json(
                    &object([("ok", b(true)), ("snapshot_sha256", string(digest))]),
                    pretty,
                    0,
                );
            }
            Err(e) => {
                return Response::json(
                    &object([("ok", b(false)), ("reason", string(e.to_string()))]),
                    pretty,
                    2,
                );
            }
        }
    };
    Response::json(&result, pretty, 0)
}
