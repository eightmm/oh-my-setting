//! OpenSSH transport for repository bundles and remote commands.
use crate::{Result, error, require, router::Response, status};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

mod resumable;

const USAGE: &str = "usage: oms runtime ssh-adapter <probe|sync-bundle|exec|reconcile|cancel|collect> [options] [-- command...]\ncollect: --host HOST --repo REPO --remote-dir DIR --remote-path PATH --dest PATH [--expected-sha256 HEX] [--max-bytes N] [--resume --expected-size N [--chunk-bytes N]]";
const DEFAULT_COLLECT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COLLECT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LEDGER_OUTPUT: usize = 512 * 1024;
const MAX_STDERR_BYTES: u64 = 64 * 1024;

fn ssh_control_path(host: &str, port: Option<u16>) -> Result<String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let uid = rustix::process::geteuid().as_raw();
    let suffix = format!(
        "s-{}.sock",
        &sha256(format!("{host}\0{}", port.unwrap_or(22)).as_bytes())[..16]
    );
    let mut candidates = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        let dir = PathBuf::from(runtime).join("oms-ssh");
        if dir.join(&suffix).to_string_lossy().len() <= 80 {
            candidates.push(dir);
        }
    }
    candidates.push(PathBuf::from(format!("/tmp/oms-ssh-{uid}")));
    for dir in candidates {
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => continue,
        }
        let Ok(meta) = std::fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid {
            continue;
        }
        if meta.permissions().mode() & 0o077 != 0
            && std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_err()
        {
            continue;
        }
        let control = dir.join(&suffix).to_string_lossy().into_owned();
        if control.len() <= 80 {
            return Ok(control);
        }
    }
    Err(error(
        "cannot provision user-private 0700 SSH control directory within socket limit",
    ))
}

pub fn run(args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> Response {
    execute(args, cwd, interrupted).unwrap_or_else(Response::failure)
}
fn execute(args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> Result<Response> {
    if args.is_empty()
        || args.iter().any(|a| a == "--help" || a == "-h") && !args.iter().any(|a| a == "--")
    {
        return Ok(Response::text(USAGE));
    }
    let action = args[0].to_str().ok_or_else(|| error("invalid action"))?;
    require(
        [
            "probe",
            "sync-bundle",
            "exec",
            "reconcile",
            "cancel",
            "collect",
        ]
        .contains(&action),
        "unknown ssh-adapter action",
    )?;
    require(
        action == "probe"
            || action == "reconcile"
            || std::env::var_os("OMS_HARNESS_CHILD").as_deref() != Some(OsStr::new("1")),
        "a harness child may only use read-only runtime actions",
    )?;
    let mut host = None;
    let mut port = None;
    let mut repo = None;
    let mut remote = None;
    let mut base = None;
    let mut target = None;
    let mut snapshot = None;
    let mut op_id = None;
    let mut grace_seconds = None;
    let mut remote_path_arg = None;
    let mut dest = None;
    let mut expected_sha256 = None;
    let mut expected_size = None;
    let mut chunk_bytes = None;
    let mut resume = false;
    let mut max_bytes = None;
    let mut pretty = false;
    let mut tail = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].to_str().ok_or_else(|| error("invalid option"))?;
        if flag == "--" {
            tail = args[i + 1..].to_vec();
            break;
        }
        if flag == "--pretty" {
            pretty = true;
            i += 1;
            continue;
        }
        if flag == "--resume" && action == "collect" {
            require(!resume, "duplicate option: --resume")?;
            resume = true;
            i += 1;
            continue;
        }
        if flag == "--json" && action != "exec" {
            i += 1;
            continue;
        }
        let slot = match flag {
            "--host" => &mut host,
            "--port" => &mut port,
            "--repo" if action != "probe" => &mut repo,
            "--remote-dir" if action != "probe" => &mut remote,
            "--base" if action == "sync-bundle" => &mut base,
            "--target" if action != "probe" => &mut target,
            "--snapshot" if action == "exec" => &mut snapshot,
            "--op-id" if ["exec", "reconcile", "cancel"].contains(&action) => &mut op_id,
            "--grace-seconds" if action == "cancel" => &mut grace_seconds,
            "--remote-path" if action == "collect" => &mut remote_path_arg,
            "--dest" if action == "collect" => &mut dest,
            "--expected-sha256" if action == "collect" => &mut expected_sha256,
            "--expected-size" if action == "collect" => &mut expected_size,
            "--chunk-bytes" if action == "collect" => &mut chunk_bytes,
            "--max-bytes" if action == "collect" => &mut max_bytes,
            _ => return Err(error(format!("unknown option: {flag}"))),
        };
        i += 1;
        let value = args
            .get(i)
            .ok_or_else(|| error(format!("{flag} requires a value")))?;
        require(slot.is_none(), format!("duplicate option: {flag}"))?;
        *slot = Some(
            value
                .to_str()
                .ok_or_else(|| error("option requires Unicode text"))?
                .to_owned(),
        );
        i += 1;
    }
    require(
        action == "exec" || tail.is_empty(),
        "unexpected command tail",
    )?;
    let host = host.ok_or_else(|| error("--host required"))?;
    require(
        !host.is_empty()
            && !host.starts_with('-')
            && host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-:@[]".contains(&c)),
        "invalid SSH host",
    )?;
    let port = port
        .map(|p| {
            p.parse::<u16>()
                .ok()
                .filter(|n| *n != 0)
                .ok_or_else(|| error("invalid SSH port"))
        })
        .transpose()?;
    if let Some(id) = op_id.as_deref() {
        validate_op_id(id)?;
    }
    let grace_seconds = grace_seconds
        .map(|raw| {
            raw.parse::<u64>()
                .ok()
                .filter(|n| (1..=300).contains(n))
                .ok_or_else(|| error("--grace-seconds must be an integer in 1..=300"))
        })
        .transpose()?
        .unwrap_or(5);
    let max_bytes = max_bytes
        .map(|raw| {
            raw.parse::<u64>()
                .ok()
                .filter(|n| (1..=MAX_COLLECT_BYTES).contains(n))
                .ok_or_else(|| {
                    error(format!(
                        "--max-bytes must be an integer in 1..={MAX_COLLECT_BYTES}"
                    ))
                })
        })
        .transpose()?
        .unwrap_or(DEFAULT_COLLECT_MAX_BYTES);
    if let Some(digest) = expected_sha256.as_deref() {
        validate_sha256_hex(digest, "--expected-sha256")?;
    }
    let resume = if resume {
        require(
            expected_sha256.is_some(),
            "--resume requires --expected-sha256",
        )?;
        let size = expected_size
            .as_deref()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|n| *n <= max_bytes)
            .ok_or_else(|| error("--resume requires --expected-size in 0..=--max-bytes"))?;
        let chunk_bytes = chunk_bytes
            .map(|raw| raw.parse::<u64>().map_err(error))
            .transpose()?
            .unwrap_or(resumable::DEFAULT_CHUNK_BYTES);
        require(
            (resumable::MIN_CHUNK_BYTES..=resumable::MAX_CHUNK_BYTES).contains(&chunk_bytes),
            "--chunk-bytes must be in 65536..=8388608",
        )?;
        Some(resumable::Options { size, chunk_bytes })
    } else {
        require(
            expected_size.is_none() && chunk_bytes.is_none(),
            "--expected-size and --chunk-bytes require --resume",
        )?;
        None
    };
    if let Some(rp) = remote_path_arg.as_deref() {
        validate_remote_rel_path(rp)?;
    }
    let control = ssh_control_path(&host, port)?;
    assert!(control.len() <= 80);
    if resume.is_some() {
        resumable::check_cancelled(interrupted)?;
    } else {
        require(!interrupted.load(Ordering::Relaxed), "interrupted")?;
    }
    let mut ssh = build_ssh(&host, port, &control);
    let mut value = serde_json::json!({"schema":1,"action":action,"host":host});
    let code;
    if action == "probe" {
        let out = ssh.arg("printf ok").output().map_err(error)?;
        code = exit_code(out.status);
        value["control_path"] = control.into();
        value["exit_code"] = code.into();
        value["reachable"] = (code == 0).into();
        value["stdout"] = String::from_utf8_lossy(&out.stdout).into_owned().into();
        value["stderr"] = String::from_utf8_lossy(&out.stderr).into_owned().into();
        return json_response(code, &value, pretty);
    }
    let repo = cwd.join(repo.ok_or_else(|| error("--repo required"))?);
    let remote = match remote {
        Some(r) => {
            require(!r.is_empty(), "--remote-dir required")?;
            Some(r)
        }
        None if matches!(action, "reconcile" | "cancel") => None,
        None => return Err(error("--remote-dir required")),
    };
    if action == "reconcile" || action == "cancel" {
        let op_id = op_id.as_deref().ok_or_else(|| error("--op-id required"))?;
        return if action == "reconcile" {
            run_reconcile(ReconcileRequest {
                cwd,
                repo: &repo,
                host: &host,
                port,
                remote: remote.as_deref(),
                target: target.as_deref(),
                op_id,
                control: &control,
                pretty,
            })
        } else {
            run_cancel(CancelRequest {
                cwd,
                repo: &repo,
                host: &host,
                port,
                remote: remote.as_deref(),
                target: target.as_deref(),
                op_id,
                grace_seconds,
                control: &control,
                pretty,
            })
        };
    }
    let remote = remote.expect("non-reconcile/cancel requires --remote-dir");
    enforce_target_policy(&repo, target.as_deref(), &host, &remote)?;
    let snapshot_pins = if let Some(digest) = snapshot.as_deref() {
        let snap = validate_snapshot(&repo, digest)?;
        Some(verify_snapshot_checkouts(&repo, &snap)?)
    } else {
        None
    };
    if action == "collect" {
        let remote_path_rel = remote_path_arg
            .as_deref()
            .ok_or_else(|| error("--remote-path required"))?;
        let dest_raw = dest.as_deref().ok_or_else(|| error("--dest required"))?;
        return run_collect(CollectRequest {
            cwd,
            host: &host,
            port,
            remote: &remote,
            remote_path_rel,
            dest_raw,
            expected_sha256: expected_sha256.as_deref(),
            resume,
            max_bytes,
            control: &control,
            pretty,
            interrupted,
        });
    }
    value["remote_dir"] = remote.clone().into();
    let head = git(&repo, &["rev-parse", "--verify", "HEAD"])?;
    let head = if head.status.success() {
        String::from_utf8_lossy(&head.stdout).trim().to_owned()
    } else {
        String::new()
    };
    value["head_sha"] = head.clone().into();
    if action == "sync-bundle" {
        require(!head.is_empty(), "repository has no HEAD")?;
        let range;
        let reference = match base.as_deref() {
            None => "HEAD",
            Some(base) => {
                require(!base.starts_with('-'), "invalid bundle ref")?;
                let resolved = git(
                    &repo,
                    &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
                )?;
                require(resolved.status.success(), "cannot resolve --base commit")?;
                let base_sha = String::from_utf8_lossy(&resolved.stdout).trim().to_owned();
                value["base_sha"] = base_sha.clone().into();
                if base_sha == head {
                    "HEAD"
                } else {
                    range = format!("{base_sha}..HEAD");
                    &range
                }
            }
        };
        let temp = TempDir::new()?;
        let bundle = temp.0.join("repo.bundle");
        let result = Command::new("git")
            .current_dir(&repo)
            .arg("bundle")
            .arg("create")
            .arg(&bundle)
            .arg(reference)
            .output()
            .map_err(error)?;
        require(
            result.status.success(),
            format!(
                "git bundle failed: {}",
                String::from_utf8_lossy(&result.stderr)
            ),
        )?;
        let file = std::fs::File::open(&bundle).map_err(error)?;
        value["bundle_bytes"] = file.metadata().map_err(error)?.len().into();
        let script = format!(
            "set -eu; mkdir -p -- {d}; cd -- {d}; t=$(mktemp .oms-bundle.XXXXXX); trap 'rm -f -- \"$t\"' EXIT HUP INT TERM; cat > \"$t\"; if [ ! -d .git ]; then git init; fi; test -z \"$(git status --porcelain)\"; git fetch \"$t\" HEAD; git checkout --no-overwrite-ignore --detach FETCH_HEAD; test \"$(git rev-parse HEAD)\" = {h}",
            d = remote_path(&remote),
            h = quote(&head)
        );
        // The bundle is outside the checkout so its presence cannot make it dirty.
        let script = script.replace(
            "mktemp .oms-bundle.XXXXXX",
            "mktemp \"${TMPDIR:-/tmp}/oms-bundle.XXXXXX\"",
        );
        let out = ssh
            .arg(format!("sh -lc {}", quote(&script)))
            .stdin(Stdio::from(file))
            .output()
            .map_err(error)?;
        code = exit_code(out.status);
        value["exit_code"] = code.into();
    } else if let Some(op_id) = op_id.as_deref() {
        require(!tail.is_empty(), "command required after --")?;
        return run_durable_exec(DurableExecRequest {
            cwd,
            repo: &repo,
            host: &host,
            port,
            remote: &remote,
            target: target.as_deref(),
            snapshot: snapshot.as_deref(),
            snapshot_pins,
            head_sha: &head,
            op_id,
            tail: &tail,
            control: &control,
            pretty,
        });
    } else {
        require(!tail.is_empty(), "command required after --")?;
        let command = tail
            .iter()
            .map(|v| {
                v.to_str()
                    .map(quote)
                    .ok_or_else(|| error("command requires Unicode text"))
            })
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        let script = if let Some(pins) = &snapshot_pins {
            format!(
                "cd -- {} && ( {} || {{ printf 'remote checkout does not match snapshot\\n' >&2; exit 2; }} ) && {}",
                remote_path(&remote),
                remote_snapshot_check_sh(pins),
                command
            )
        } else {
            format!("cd -- {} && {}", remote_path(&remote), command)
        };
        let remote_command = format!("sh -lc {}", quote(&script));
        value["command_sha256"] = sha256(remote_command.as_bytes()).into();
        value["control_path"] = control.into();
        value["started_at"] = now()?.into();
        let out = ssh.arg(&remote_command).output().map_err(error)?;
        value["completed_at"] = now()?.into();
        code = exit_code(out.status);
        value["exit_code"] = code.into();
        value["stdout"] = String::from_utf8_lossy(&out.stdout).into_owned().into();
        value["stderr"] = String::from_utf8_lossy(&out.stderr).into_owned().into();
    }
    json_response(code, &value, pretty)
}

fn read_runtime_file_opt(repo: &Path, path: &Path) -> Result<Vec<u8>> {
    let root = oms_safe_fs::Root::open(repo).map_err(|e| error(format!("repository: {e:?}")))?;
    root.read_no_follow(
        path,
        oms_safe_fs::ByteReadOptions {
            missing_ok: true,
            max_bytes: crate::storage::MAX_STATE,
        },
    )
    .map_err(|e| error(format!("runtime input: {e:?}")))
}

fn read_runtime_file(repo: &Path, path: &Path) -> Result<Vec<u8>> {
    let root = oms_safe_fs::Root::open(repo).map_err(|e| error(format!("repository: {e:?}")))?;
    root.read_no_follow(
        path,
        oms_safe_fs::ByteReadOptions {
            missing_ok: false,
            max_bytes: crate::storage::MAX_STATE,
        },
    )
    .map_err(|e| error(format!("runtime input: {e:?}")))
}

fn validate_target(repo: &Path, name: &str, host: &str, remote: &str) -> Result<()> {
    let registry = crate::value::parse(&read_runtime_file(repo, Path::new(crate::targets::FILE))?)?;
    crate::value::keys(&registry, &["schema", "targets"], &[])?;
    require(
        registry.get("schema").is_int(1),
        "invalid remote target registry",
    )?;
    let row = registry
        .get("targets")
        .opt(name)
        .ok_or_else(|| error("unknown remote target"))?;
    require(
        crate::targets::approved(row)?,
        "target execution is not approved",
    )?;
    require(
        row.text("ssh_alias") == host && row.text("workdir") == remote,
        "target does not match SSH host and remote directory",
    )
}

fn enforce_target_policy(
    repo: &Path,
    target: Option<&str>,
    host: &str,
    remote: &str,
) -> Result<()> {
    if let Some(name) = target {
        return validate_target(repo, name, host, remote);
    }
    require(
        std::env::var_os("OMS_SSH_REQUIRE_TARGET").as_deref() != Some(OsStr::new("1")),
        "--target is required by OMS_SSH_REQUIRE_TARGET",
    )?;
    let raw = read_runtime_file_opt(repo, Path::new(crate::targets::FILE))?;
    if raw.is_empty() {
        return Ok(());
    }
    let registry = crate::value::parse(&raw)?;
    crate::value::keys(&registry, &["schema", "targets"], &[])?;
    require(
        registry.get("schema").is_int(1),
        "invalid remote target registry",
    )?;
    let Ok(entries) = registry.get("targets").object() else {
        return Ok(());
    };
    let mut matched_exact = false;
    let mut exact_approved = false;
    let mut matched_host = false;
    let mut host_approved = false;
    fn ssh_host_matches(registered: &str, supplied: &str) -> bool {
        let r_host = registered.rsplit_once('@').map_or(registered, |(_, h)| h);
        let s_host = supplied.rsplit_once('@').map_or(supplied, |(_, h)| h);
        registered.eq_ignore_ascii_case(supplied) || r_host.eq_ignore_ascii_case(s_host)
    }

    for (_, row) in entries {
        if ssh_host_matches(row.text("ssh_alias"), host) {
            matched_host = true;
            let is_ok = crate::targets::approved(row)?;
            if is_ok {
                host_approved = true;
            }
            if row.text("workdir") == remote {
                matched_exact = true;
                if is_ok {
                    exact_approved = true;
                }
            }
        }
    }
    if matched_exact {
        require(
            exact_approved,
            "registered remote target is not approved for execution",
        )?;
    } else if matched_host {
        if !host_approved {
            return Err(error("registered SSH host is not approved for execution"));
        }
        return Err(error(
            "remote directory is not registered for this SSH host",
        ));
    }
    Ok(())
}

fn validate_sha256_hex(digest: &str, label: &str) -> Result<()> {
    require(
        digest.len() == 64
            && digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        format!("invalid {label}"),
    )
}

fn validate_snapshot(repo: &Path, digest: &str) -> Result<crate::value::Value> {
    validate_sha256_hex(digest, "snapshot SHA256")?;
    let path = PathBuf::from(format!(".oms/runtime/snapshots/{digest}.json"));
    let value = crate::value::parse(&read_runtime_file(repo, &path)?)?;
    crate::value::keys(
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
            && crate::value::sha(crate::value::canonical(&crate::value::without(
                &value,
                &["snapshot_sha256"],
            ))?) == digest,
        "snapshot integrity mismatch",
    )?;
    Ok(value)
}

struct SnapshotPins {
    head_sha: String,
    tree_sha: String,
    lockfiles: Vec<(String, String)>,
}

fn lockfile_prefixes(rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for comp in rel.split('/') {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(comp);
        out.push(acc.clone());
    }
    out
}

fn remote_snapshot_check_sh(pins: &SnapshotPins) -> String {
    let mut checks = format!(
        "test \"$(git rev-parse --verify HEAD 2>/dev/null)\" = {} && test \"$(git rev-parse --verify 'HEAD^{{tree}}' 2>/dev/null)\" = {} && test -z \"$(git status --porcelain --untracked-files=no 2>/dev/null)\"",
        quote(&pins.head_sha),
        quote(&pins.tree_sha),
    );
    for (rel, expected_sha) in &pins.lockfiles {
        let prefixes = lockfile_prefixes(rel);
        for p in &prefixes[..prefixes.len().saturating_sub(1)] {
            checks.push_str(&format!(" && test ! -L {}", quote(p)));
        }
        let qp = quote(rel);
        let qs = quote(expected_sha);
        checks.push_str(&format!(
            " && test -f {qp} && test ! -L {qp} && test \"$(if command -v sha256sum >/dev/null 2>&1; then sha256sum -- {qp} | awk '{{print $1}}'; elif command -v shasum >/dev/null 2>&1; then shasum -a 256 -- {qp} | awk '{{print $1}}'; elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 -- {qp} | awk '{{print $NF}}'; else exit 1; fi)\" = {qs}"
        ));
    }
    checks
}

fn verify_snapshot_checkouts(repo: &Path, snap: &crate::value::Value) -> Result<SnapshotPins> {
    crate::exec_snapshot::clean(repo)?;
    let head = git(repo, &["rev-parse", "--verify", "HEAD"])?;
    require(
        head.status.success(),
        "cannot resolve local HEAD for snapshot",
    )?;
    let actual_head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    let expected_head = snap.text("head_sha").to_owned();
    require(
        actual_head == expected_head,
        "local HEAD does not match --snapshot",
    )?;
    let tree = git(repo, &["rev-parse", "--verify", "HEAD^{tree}"])?;
    require(
        tree.status.success(),
        "cannot resolve local tree for snapshot",
    )?;
    let actual_tree = String::from_utf8_lossy(&tree.stdout).trim().to_owned();
    let expected_tree = snap.text("tree_sha").to_owned();
    require(
        actual_tree == expected_tree,
        "local tree does not match --snapshot",
    )?;
    let mut lockfiles = Vec::new();
    for (rel_bytes, expected) in snap.get("lockfiles").object()? {
        let rel = std::str::from_utf8(rel_bytes).map_err(error)?;
        require(
            !rel.is_empty()
                && !rel.chars().any(char::is_control)
                && Path::new(rel)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "invalid lockfile path in snapshot",
        )?;
        let expected_sha = expected.str()?;
        validate_sha256_hex(expected_sha, "lockfile SHA256")?;
        let bytes = read_runtime_file(repo, Path::new(rel))?;
        require(
            crate::value::sha(&bytes) == expected_sha,
            format!("local lockfile digest mismatch for {rel}"),
        )?;
        lockfiles.push((rel.to_owned(), expected_sha.to_owned()));
    }
    Ok(SnapshotPins {
        head_sha: expected_head,
        tree_sha: expected_tree,
        lockfiles,
    })
}

fn validate_op_id(id: &str) -> Result<()> {
    require(
        !id.is_empty()
            && id.len() <= 64
            && !id.starts_with('-')
            && !id.starts_with('.')
            && !id.contains("..")
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')),
        "invalid --op-id",
    )
}

fn validate_remote_rel_path(path: &str) -> Result<()> {
    require(
        !path.is_empty()
            && !path.starts_with('/')
            && !path.starts_with('~')
            && !path.contains('\0')
            && !path.contains('\\')
            && path
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".."),
        "invalid --remote-path",
    )
}

fn build_ssh(host: &str, port: Option<u16>, control: &str) -> Command {
    let mut ssh = Command::new(std::env::var_os("OMS_SSH_BIN").unwrap_or_else(|| "ssh".into()));
    ssh.args([
        "-o",
        "ControlMaster=auto",
        "-o",
        "ControlPersist=120",
        "-o",
        &format!("ControlPath={control}"),
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
    ]);
    if let Some(port) = port {
        ssh.args(["-p", &port.to_string()]);
    }
    ssh.args(["--", host]);
    ssh
}

fn json_response(code: i32, value: &serde_json::Value, pretty: bool) -> Result<Response> {
    let mut stdout = if pretty {
        serde_json::to_vec_pretty(value)
    } else {
        serde_json::to_vec(value)
    }
    .map_err(error)?;
    stdout.push(b'\n');
    Ok(Response {
        code,
        stdout,
        stderr: vec![],
    })
}

fn bounded_output(bytes: &[u8]) -> String {
    let capped = &bytes[..bytes.len().min(MAX_LEDGER_OUTPUT)];
    String::from_utf8_lossy(capped).into_owned()
}

fn normalize_ws(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve_existing(path: &Path) -> Result<PathBuf> {
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(&cursor) {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    cursor
                        .file_name()
                        .ok_or_else(|| error("invalid lock target path"))?
                        .to_owned(),
                );
                require(cursor.pop(), "lock target has no existing ancestor")?;
            }
            Err(e) => return Err(error(e)),
        }
    }
}

fn lock_remote_op(repo: &Path, cwd: &Path, op_id: &str) -> Result<oms_lock::Guard> {
    let rel = PathBuf::from(format!(".oms/runtime/remote-ops/{op_id}.json"));
    let resolved = resolve_existing(&repo.join(&rel))?;
    let mut settings = oms_lock::Settings::from_env()
        .map_err(|e| status(e.exit_code(), format!("remote op lock: {e}")))?;
    settings.mode = oms_lock::Mode::Mkdir;
    settings.timeout = Duration::from_secs(15);
    settings.cwd = cwd.to_path_buf();
    let root = if let Some(raw) = std::env::var_os("OMS_LOCK_DIR") {
        let raw_bytes = raw.as_bytes();
        let expanded = if raw_bytes == b"~" || raw_bytes.starts_with(b"~/") {
            settings
                .home
                .join(OsStr::from_bytes(raw_bytes.get(2..).unwrap_or(&[])))
        } else {
            PathBuf::from(raw)
        };
        if expanded.is_absolute() {
            expanded
        } else {
            cwd.join(expanded)
        }
    } else {
        settings.home.join(".cache/oh-my-setting/locks")
    };
    let stamp = sha256(
        resolved
            .to_str()
            .ok_or_else(|| error("remote op target path must be UTF-8"))?
            .as_bytes(),
    );
    let path = root.join(format!("runtime-{}.lock", &stamp[..32]));
    oms_lock::Lock::at_path(path, settings)
        .map_err(|e| status(e.exit_code(), format!("remote op lock: {e}")))?
        .acquire(None)
        .map_err(|e| status(e.exit_code(), format!("remote op lock: {e}")))
}

fn ensure_remote_ops_dirs(repo: &Path) -> Result<()> {
    let mut dir = repo.to_path_buf();
    for segment in [".oms", "runtime", "remote-ops"] {
        dir.push(segment);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) => {
                require(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    format!("unsafe non-directory state path: {}", dir.display()),
                )?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(e)),
        }
    }
    Ok(())
}

fn op_ledger_rel(op_id: &str) -> PathBuf {
    PathBuf::from(format!(".oms/runtime/remote-ops/{op_id}.json"))
}

fn read_remote_op(repo: &Path, op_id: &str) -> Result<Option<serde_json::Value>> {
    ensure_remote_ops_dirs(repo)?;
    let root = oms_safe_fs::Root::open(repo).map_err(|e| error(format!("repository: {e:?}")))?;
    let bytes = root
        .read_no_follow(
            &op_ledger_rel(op_id),
            oms_safe_fs::ByteReadOptions {
                missing_ok: true,
                max_bytes: crate::storage::MAX_STATE,
            },
        )
        .map_err(|e| error(format!("remote op ledger: {e:?}")))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let val: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
    require(
        val.get("schema").and_then(|v| v.as_i64()) == Some(1)
            && val.get("op_id").and_then(|v| v.as_str()) == Some(op_id),
        "corrupt remote operation ledger record",
    )?;
    Ok(Some(val))
}

fn write_remote_op(repo: &Path, op_id: &str, record: &serde_json::Value) -> Result<()> {
    ensure_remote_ops_dirs(repo)?;
    let mut encoded = serde_json::to_vec(record).map_err(error)?;
    encoded.push(b'\n');
    let root = oms_safe_fs::Root::open(repo).map_err(|e| error(format!("repository: {e:?}")))?;
    let rel = op_ledger_rel(op_id);
    root.atomic_mutate(
        &rel,
        oms_safe_fs::MutationOptions {
            read: oms_safe_fs::ByteReadOptions {
                missing_ok: true,
                max_bytes: crate::storage::MAX_STATE,
            },
            create_parent: true,
            max_output: crate::storage::MAX_STATE,
            preserve_existing: false,
            expected_exists: None,
        },
        move |_| Ok::<_, String>(encoded),
    )
    .map_err(|e| error(format!("persist remote op ledger: {e:?}")))?;
    let abs = repo.join(&rel);
    let meta = std::fs::symlink_metadata(&abs).map_err(error)?;
    require(
        meta.is_file() && !meta.file_type().is_symlink(),
        "unsafe remote op ledger file",
    )?;
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o600)).map_err(error)?;
    }
    Ok(())
}

static FRAME_NONCE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn new_frame_nonce() -> String {
    let mut seed = Vec::with_capacity(48);
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let mut buf = [0u8; 16];
        if f.read_exact(&mut buf).is_ok() {
            seed.extend_from_slice(&buf);
        }
    }
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    seed.extend_from_slice(&nanos.to_le_bytes());
    seed.extend_from_slice(&std::process::id().to_le_bytes());
    seed.extend_from_slice(
        &FRAME_NONCE_COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .to_le_bytes(),
    );
    sha256(&seed)[..16].to_owned()
}

const REMOTE_SH_HELPERS: &str = r#"
norm() { printf '%s' "$1" | awk '{$1=$1;print}'; }
pid_live() {
  p="$1"
  [ -n "$p" ] && kill -0 "$p" 2>/dev/null || return 1
  st=$(LC_ALL=C ps -p "$p" -o stat= 2>/dev/null | tr -d ' \t\r\n' || true)
  case "$st" in
    Z*) return 1 ;;
    *) return 0 ;;
  esac
}
pg_live() {
  g="$1"
  [ -n "$g" ] && kill -0 -"$g" 2>/dev/null || return 1
  if command -v ps >/dev/null 2>&1; then
    alive_pg=$(LC_ALL=C ps -A -o pgid=,stat= 2>/dev/null | awk -v g="$g" '$1 == g && $2 !~ /^Z/ {print 1; exit}' || true)
    [ -n "$alive_pg" ] && return 0
    ps -A -o pgid= >/dev/null 2>&1 && return 1
  fi
  return 0
}
read_host_id() {
  cur_hn=$(uname -n 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || printf 'unknown')
  [ -n "$cur_hn" ] || cur_hn='unknown'
  if [ -r /proc/sys/kernel/random/boot_id ]; then
    cur_bid=$(tr -d ' \t\r\n' < /proc/sys/kernel/random/boot_id | tr -cd 'A-Za-z0-9._: +=,-')
  elif command -v sysctl >/dev/null 2>&1; then
    cur_bid=$(sysctl -n kern.boottime 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || printf 'unknown')
  else
    cur_bid='unknown'
  fi
  [ -n "$cur_bid" ] || cur_bid='unknown'
}
emit_part() {
  tok="$1"; name="$2"; file="$3"
  if [ -f "$file" ]; then
    tmp="$op_dir/.emit.$$.$tok.$name.tmp"
    rm -f -- "$tmp"
    dd if="$file" of="$tmp" bs=65536 count=8 2>/dev/null || cp -- "$file" "$tmp"
    sz=$(wc -c < "$tmp" | tr -d ' \t\r\n')
    printf '__OMS_PART_%s__ %s %s\n' "$tok" "$name" "$sz"
    cat -- "$tmp"
    rm -f -- "$tmp"
    printf '\n'
  else
    printf '__OMS_PART_%s__ %s -1\n' "$tok" "$name"
  fi
}
find_op_dir() {
  op_dir=""
  if [ -n "${OMS_REMOTE_STATE_DIR:-}" ] && [ -d "$OMS_REMOTE_STATE_DIR/$op_id_arg" ]; then
    op_dir="$OMS_REMOTE_STATE_DIR/$op_id_arg"
  elif [ -d "$rd/.git/oms-remote-ops/$op_id_arg" ]; then
    op_dir="$rd/.git/oms-remote-ops/$op_id_arg"
  elif [ -d "${TMPDIR:-/tmp}/oms-remote-ops-$(id -u)/$rd_key/$op_id_arg" ]; then
    op_dir="${TMPDIR:-/tmp}/oms-remote-ops-$(id -u)/$rd_key/$op_id_arg"
  fi
}
read_cpg() {
  cpgid=""
  cpg_ls=""
  if [ -f "$op_dir/child.pgid" ]; then
    cand_pg=$(tr -d ' \t\r\n' < "$op_dir/child.pgid" || true)
    case "$cand_pg" in
      ''|*[!0-9]*) ;;
      *)
        if [ "$cand_pg" -gt 1 ] 2>/dev/null && [ "$cand_pg" != "$$" ] && [ "$cand_pg" != "$cur_pg" ]; then
          cpgid="$cand_pg"
        fi
        ;;
    esac
  fi
  if [ -f "$op_dir/child.lstart" ]; then
    cpg_ls=$(cat -- "$op_dir/child.lstart" 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || true)
  fi
}
check_orphan_cpg() {
  cpg_verdict="none"
  [ ! -f "$op_dir/reaped.done" ] && [ -n "$cpgid" ] && pg_live "$cpgid" || return 0
  if [ -z "$m_bid" ] || [ -z "$cur_bid" ] || [ "$m_bid" = "unknown" ] || [ "$cur_bid" = "unknown" ]; then
    cpg_verdict="unverifiable"
    return 0
  fi
  [ "$m_bid" = "$cur_bid" ] || return 0
  if pid_live "$cpgid"; then
    cur_cpg_ls=$(LC_ALL=C ps -p "$cpgid" -o lstart= 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || true)
    if [ -z "$cpg_ls" ] || [ -z "$cur_cpg_ls" ]; then
      cpg_verdict="unverifiable"
    elif [ "$(norm "$cpg_ls")" = "$(norm "$cur_cpg_ls")" ]; then
      cpg_verdict="verified"
    fi
  elif [ -z "$m_tag" ] || [ ! -d /proc ] || [ ! -r "/proc/$$/environ" ]; then
    cpg_verdict="unverifiable"
  else
    pg_pids=$(LC_ALL=C ps -A -o pid=,pgid=,stat= 2>/dev/null | awk -v g="$cpgid" '$2 == g && $3 !~ /^Z/ {print $1}' || true)
    unreadable_env=0
    for p in $pg_pids; do
      if dd if="/proc/$p/environ" bs=1 count=1 >/dev/null 2>&1; then
        if tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | grep -Fxq -- "OMS_REMOTE_OP_TAG=$m_tag"; then
          cpg_verdict="verified"
          break
        fi
      elif [ -d "/proc/$p" ]; then
        unreadable_env=1
      fi
    done
    if [ "$cpg_verdict" != "verified" ] && [ "$unreadable_env" = 1 ]; then
      cpg_verdict="unverifiable"
    fi
  fi
}
"#;

struct ParsedStream {
    markers: Vec<(String, String)>,
    parts: BTreeMap<String, Vec<u8>>,
}

fn parse_framed_stream(data: &[u8], nonce: &str) -> Result<ParsedStream> {
    let part_prefix = format!("__OMS_PART_{nonce}__ ");
    let mark_prefix = format!("__OMS_MARK_{nonce}__ ");
    let mut pos = 0usize;
    let mut markers = Vec::new();
    let mut parts = BTreeMap::new();
    let mut seen_parts = BTreeSet::new();
    while pos < data.len() {
        let rest = &data[pos..];
        let line_end = rest
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| pos + i)
            .unwrap_or(data.len());
        let line = String::from_utf8_lossy(&data[pos..line_end]);
        pos = if line_end < data.len() {
            line_end + 1
        } else {
            data.len()
        };
        if let Some(after) = line.strip_prefix(&part_prefix) {
            let mut split = after.splitn(2, ' ');
            let name = split
                .next()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error("malformed remote part header"))?;
            require(
                seen_parts.insert(name.to_owned()),
                format!("duplicate remote part: {name}"),
            )?;
            let len_str = split
                .next()
                .ok_or_else(|| error("malformed remote part length"))?
                .trim();
            let len = len_str
                .parse::<i64>()
                .map_err(|_| error("invalid remote part length"))?;
            if len >= 0 {
                let len_usize = usize::try_from(len).map_err(error)?;
                let end = pos
                    .checked_add(len_usize)
                    .filter(|&e| e <= data.len())
                    .ok_or_else(|| error("truncated remote part payload"))?;
                parts.insert(name.to_owned(), data[pos..end].to_vec());
                pos = end;
                if pos < data.len() && data[pos] == b'\n' {
                    pos += 1;
                }
            }
        } else if let Some(after) = line.strip_prefix(&mark_prefix)
            && let Some((kind, payload)) = after.split_once(' ')
        {
            markers.push((kind.to_owned(), payload.to_owned()));
        }
    }
    Ok(ParsedStream { markers, parts })
}

struct DurableExecRequest<'a> {
    cwd: &'a Path,
    repo: &'a Path,
    host: &'a str,
    port: Option<u16>,
    remote: &'a str,
    target: Option<&'a str>,
    snapshot: Option<&'a str>,
    snapshot_pins: Option<SnapshotPins>,
    head_sha: &'a str,
    op_id: &'a str,
    tail: &'a [OsString],
    control: &'a str,
    pretty: bool,
}

fn request_matches(
    record: &serde_json::Value,
    host: &str,
    port: Option<u16>,
    remote: &str,
    target: Option<&str>,
    snapshot: Option<&str>,
    command_sha256: &str,
) -> bool {
    record.get("host").and_then(|v| v.as_str()) == Some(host)
        && record.get("port").and_then(|v| v.as_u64()) == port.map(u64::from)
        && record.get("remote_dir").and_then(|v| v.as_str()) == Some(remote)
        && record.get("target").and_then(|v| v.as_str()) == target
        && record.get("snapshot").and_then(|v| v.as_str()) == snapshot
        && record.get("command_sha256").and_then(|v| v.as_str()) == Some(command_sha256)
}

fn parse_meta_part(parts: &BTreeMap<String, Vec<u8>>) -> Option<serde_json::Value> {
    parts
        .get("meta")
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .filter(|v| v.is_object())
}

fn apply_result_parts(
    record: &mut serde_json::Value,
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<Option<i32>> {
    if let Some(meta) = parse_meta_part(parts) {
        record["remote_meta"] = meta;
    }
    if let Some(out_bytes) = parts.get("stdout") {
        record["stdout"] = bounded_output(out_bytes).into();
    }
    if let Some(err_bytes) = parts.get("stderr") {
        record["stderr"] = bounded_output(err_bytes).into();
    }
    let Some(res_bytes) = parts.get("result") else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_slice(res_bytes).map_err(error)?;
    let rc = parsed
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| error("invalid remote result.json exit_code"))?;
    record["exit_code"] = rc.into();
    record["state"] = if rc == 0 { "succeeded" } else { "failed" }.into();
    let ts = now()?;
    record["updated_at"] = ts.clone().into();
    record["completed_at"] = ts.into();
    Ok(Some(rc))
}

fn run_durable_exec(req: DurableExecRequest<'_>) -> Result<Response> {
    let command = req
        .tail
        .iter()
        .map(|v| {
            v.to_str()
                .map(quote)
                .ok_or_else(|| error("command requires Unicode text"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let cmd_script = format!("exec {command}");
    let command_sha256 = sha256(cmd_script.as_bytes());
    let _guard = lock_remote_op(req.repo, req.cwd, req.op_id)?;

    if let Some(mut existing) = read_remote_op(req.repo, req.op_id)? {
        require(
            request_matches(
                &existing,
                req.host,
                req.port,
                req.remote,
                req.target,
                req.snapshot,
                &command_sha256,
            ),
            "conflicting reuse of --op-id",
        )?;
        let state = existing
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        existing["action"] = "exec".into();
        existing["deduplicated"] = true.into();
        return match state.as_str() {
            "succeeded" | "failed" => {
                let code = existing
                    .get("exit_code")
                    .and_then(|v| v.as_i64())
                    .and_then(|v| i32::try_from(v).ok())
                    .unwrap_or(1);
                json_response(code, &existing, req.pretty)
            }
            "cancelled" | "lost" => json_response(1, &existing, req.pretty),
            _ => {
                existing["reconcile_required"] = true.into();
                json_response(75, &existing, req.pretty)
            }
        };
    }

    let ts = now()?;
    let op_tag = new_frame_nonce();
    let mut record = serde_json::json!({
        "schema": 1,
        "action": "exec",
        "op_id": req.op_id,
        "op_tag": op_tag,
        "state": "intent",
        "host": req.host,
        "port": req.port,
        "remote_dir": req.remote,
        "head_sha": req.head_sha,
        "target": req.target,
        "snapshot": req.snapshot,
        "command_sha256": command_sha256,
        "control_path": req.control,
        "started_at": ts,
        "updated_at": ts,
        "completed_at": serde_json::Value::Null,
        "exit_code": serde_json::Value::Null,
        "stdout": serde_json::Value::Null,
        "stderr": serde_json::Value::Null,
        "remote_meta": serde_json::Value::Null,
    });
    write_remote_op(req.repo, req.op_id, &record)?;

    let rd_key = &sha256(req.remote.as_bytes())[..16];
    let tok = new_frame_nonce();
    let snap_check = if let Some(pins) = &req.snapshot_pins {
        format!(
            "if ! ( cd -- \"$rd\" && {} ); then printf '__OMS_MARK_%s__ SNAPSHOT_DRIFT 1\\n' {tok}; exit 0; fi",
            remote_snapshot_check_sh(pins),
            tok = quote(&tok),
        )
    } else {
        String::new()
    };
    let remote_script = format!(
        r#"set -eu
rd={rd}
rd_key={rd_key}
op_id_arg={op_id}
tok={tok}
{snap_check}
if [ -n "${{OMS_REMOTE_STATE_DIR:-}}" ]; then
  base="$OMS_REMOTE_STATE_DIR"
elif [ -d "$rd/.git" ]; then
  base="$rd/.git/oms-remote-ops"
else
  base="${{TMPDIR:-/tmp}}/oms-remote-ops-$(id -u)/$rd_key"
fi
mkdir -p -- "$base"
op_dir="$base/$op_id_arg"
if ! mkdir -- "$op_dir" 2>/dev/null; then
  printf '__OMS_MARK_%s__ EXISTS 1\n' "$tok"
  exit 75
fi
{helpers}
printf '%s\n' {cmd} > "$op_dir/cmd.sh"
cat <<'EOF' > "$op_dir/runner.sh"
set -eu
trap '' HUP
op_dir="$1"
rd="$2"
cmd_sha="$3"
snap_val="$4"
op_tag="${{5:-}}"
hn=$(uname -n 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || printf 'unknown')
[ -n "$hn" ] || hn='unknown'
if [ -r /proc/sys/kernel/random/boot_id ]; then
  bid=$(tr -d ' \t\r\n' < /proc/sys/kernel/random/boot_id | tr -cd 'A-Za-z0-9._: +=,-')
elif command -v sysctl >/dev/null 2>&1; then
  bid=$(sysctl -n kern.boottime 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || printf 'unknown')
else
  bid='unknown'
fi
[ -n "$bid" ] || bid='unknown'
pid=$$
ls_val=$(LC_ALL=C ps -p "$pid" -o lstart= 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || true)
printf '{{"hostname":"%s","boot_id":"%s","pid":%s,"lstart":"%s","command_sha256":"%s","snapshot":"%s","op_tag":"%s"}}\n' "$hn" "$bid" "$pid" "$ls_val" "$cmd_sha" "$snap_val" "$op_tag" >"$op_dir/meta.json.tmp"
mv -f -- "$op_dir/meta.json.tmp" "$op_dir/meta.json"
child=""
trap '[ -n "$child" ] && {{ kill -TERM -"$child" 2>/dev/null || kill -TERM "$child" 2>/dev/null || true; }}' TERM
export OMS_REMOTE_OP_TAG="$op_tag"
cpg_script='pg=$(LC_ALL=C ps -p $$ -o pgid= 2>/dev/null | tr -d " \t\r\n" || true); if [ "$pg" = "$$" ] && [ "$$" -gt 1 ] 2>/dev/null; then pg_ls=$(LC_ALL=C ps -p $$ -o lstart= 2>/dev/null | tr -cd "A-Za-z0-9._: +=,-" || true); printf "%s\n" "$pg_ls" >"$4.tmp" && mv -f -- "$4.tmp" "$4"; printf "%s\n" "$$" >"$3.tmp" && mv -f -- "$3.tmp" "$3"; fi; cd -- "$1" && exec sh "$2"'
if command -v setsid >/dev/null 2>&1; then
  setsid sh -c "$cpg_script" sh "$rd" "$op_dir/cmd.sh" "$op_dir/child.pgid" "$op_dir/child.lstart" >"$op_dir/stdout" 2>"$op_dir/stderr" </dev/null &
  child=$!
else
  trap '' TTIN TTOU TSTP 2>/dev/null || true
  set -m
  sh -c "trap - TTIN TTOU TSTP 2>/dev/null || true; $cpg_script" sh "$rd" "$op_dir/cmd.sh" "$op_dir/child.pgid" "$op_dir/child.lstart" >"$op_dir/stdout" 2>"$op_dir/stderr" </dev/null &
  child=$!
  set +m
  trap - TTIN TTOU TSTP 2>/dev/null || true
fi
printf '%s\n' "$child" >"$op_dir/child.pid.tmp"
mv -f -- "$op_dir/child.pid.tmp" "$op_dir/child.pid"
set +e
while :; do
  wait "$child"
  rc=$?
  if ! kill -0 "$child" 2>/dev/null; then
    break
  fi
done
set -e
if [ -f "$op_dir/cancel.req" ]; then
  exit 143
fi
printf '{{"exit_code":%d}}\n' "$rc" >"$op_dir/result.json.tmp"
mv -f -- "$op_dir/result.json.tmp" "$op_dir/result.json"
EOF
nohup sh "$op_dir/runner.sh" "$op_dir" "$rd" {cmd_sha} {snap_val} {op_tag} </dev/null >/dev/null 2>&1 &
rpid=$!
i=0
while [ ! -f "$op_dir/result.json" ]; do
  if [ -f "$op_dir/meta.json" ] && ! kill -0 "$rpid" 2>/dev/null; then
    sleep 0.05 2>/dev/null || sleep 1
    [ -f "$op_dir/result.json" ] && break
    printf '__OMS_MARK_%s__ RUNNER_DIED 1\n' "$tok"
    break
  fi
  i=$((i + 1))
  [ "$i" -ge 1200 ] && break
  sleep 0.05 2>/dev/null || sleep 1
done
if [ -f "$op_dir/result.json" ]; then
  emit_part "$tok" meta "$op_dir/meta.json"
  emit_part "$tok" result "$op_dir/result.json"
  emit_part "$tok" stdout "$op_dir/stdout"
  emit_part "$tok" stderr "$op_dir/stderr"
else
  printf '__OMS_MARK_%s__ STILL_RUNNING 1\n' "$tok"
  emit_part "$tok" meta "$op_dir/meta.json"
fi
"#,
        rd = remote_path(req.remote),
        rd_key = quote(rd_key),
        op_id = quote(req.op_id),
        tok = quote(&tok),
        snap_check = snap_check,
        helpers = REMOTE_SH_HELPERS,
        cmd = quote(&cmd_script),
        cmd_sha = quote(&command_sha256),
        snap_val = quote(req.snapshot.unwrap_or("")),
        op_tag = quote(&op_tag),
    );
    let out = build_ssh(req.host, req.port, req.control)
        .arg(format!("sh -lc {}", quote(&remote_script)))
        .output()
        .map_err(error)?;

    let ssh_uncertain = out.status.code().is_none() || out.status.code() == Some(255);
    if ssh_uncertain || !out.status.success() {
        if let Ok(parsed) = parse_framed_stream(&out.stdout, &tok)
            && let Some(meta) = parse_meta_part(&parsed.parts)
        {
            record["remote_meta"] = meta;
        }
        record["state"] = "unknown".into();
        record["updated_at"] = now()?.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return json_response(75, &record, req.pretty);
    }

    let parsed = match parse_framed_stream(&out.stdout, &tok) {
        Ok(parsed) => parsed,
        Err(_) => {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            return json_response(75, &record, req.pretty);
        }
    };
    if parsed
        .markers
        .iter()
        .any(|(kind, _)| kind == "SNAPSHOT_DRIFT")
    {
        record["state"] = "failed".into();
        record["exit_code"] = 2.into();
        let ts = now()?;
        record["updated_at"] = ts.clone().into();
        record["completed_at"] = ts.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return Err(error("remote checkout does not match --snapshot"));
    }
    if parsed
        .markers
        .iter()
        .any(|(kind, _)| kind == "STILL_RUNNING")
    {
        if let Some(meta) = parse_meta_part(&parsed.parts) {
            verify_remote_meta_binding(&record, &meta)?;
            record["remote_meta"] = meta;
        }
        record["state"] = "running".into();
        record["updated_at"] = now()?.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return json_response(75, &record, req.pretty);
    }
    if parsed
        .markers
        .iter()
        .any(|(kind, _)| kind == "EXISTS" || kind == "RUNNER_DIED")
    {
        if let Some(meta) = parse_meta_part(&parsed.parts) {
            record["remote_meta"] = meta;
        }
        record["state"] = "unknown".into();
        record["updated_at"] = now()?.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return json_response(75, &record, req.pretty);
    }

    if let Some(meta) = parse_meta_part(&parsed.parts) {
        verify_remote_meta_binding(&record, &meta)?;
    }
    match apply_result_parts(&mut record, &parsed.parts)? {
        Some(rc) => {
            write_remote_op(req.repo, req.op_id, &record)?;
            json_response(rc, &record, req.pretty)
        }
        None => {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            json_response(75, &record, req.pretty)
        }
    }
}

fn verify_remote_meta_binding(record: &serde_json::Value, meta: &serde_json::Value) -> Result<()> {
    let expected_cmd = record
        .get("command_sha256")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !expected_cmd.is_empty() {
        let meta_cmd = meta
            .get("command_sha256")
            .and_then(|v| v.as_str())
            .ok_or_else(|| error("remote operation metadata missing command_sha256"))?;
        require(
            meta_cmd == expected_cmd,
            "remote operation metadata command_sha256 mismatch",
        )?;
    }
    let expected_snap = record
        .get("snapshot")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let meta_snap = meta.get("snapshot").and_then(|v| v.as_str()).unwrap_or("");
    require(
        meta_snap == expected_snap,
        "remote operation metadata snapshot mismatch",
    )?;
    let expected_tag = record.get("op_tag").and_then(|v| v.as_str()).unwrap_or("");
    if !expected_tag.is_empty() {
        let meta_tag = meta
            .get("op_tag")
            .and_then(|v| v.as_str())
            .ok_or_else(|| error("remote operation metadata missing op_tag"))?;
        require(
            meta_tag == expected_tag,
            "remote operation metadata op_tag mismatch",
        )?;
    }
    Ok(())
}

struct ReconcileRequest<'a> {
    cwd: &'a Path,
    repo: &'a Path,
    host: &'a str,
    port: Option<u16>,
    remote: Option<&'a str>,
    target: Option<&'a str>,
    op_id: &'a str,
    control: &'a str,
    pretty: bool,
}

fn validate_existing_scope(
    record: &serde_json::Value,
    repo: &Path,
    host: &str,
    port: Option<u16>,
    remote: Option<&str>,
    target: Option<&str>,
) -> Result<String> {
    require(
        record.get("host").and_then(|v| v.as_str()) == Some(host),
        "operation belongs to a different SSH host",
    )?;
    if let Some(port) = port {
        require(
            record.get("port").and_then(|v| v.as_u64()) == Some(u64::from(port)),
            "operation belongs to a different SSH port",
        )?;
    }
    let stored_remote = record
        .get("remote_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| error("operation record missing remote_dir"))?
        .to_owned();
    if let Some(remote) = remote {
        require(
            remote == stored_remote,
            "operation belongs to a different --remote-dir",
        )?;
    }
    enforce_target_policy(
        repo,
        target.or_else(|| record.get("target").and_then(|v| v.as_str())),
        host,
        &stored_remote,
    )?;
    Ok(stored_remote)
}

fn run_reconcile(req: ReconcileRequest<'_>) -> Result<Response> {
    let _guard = lock_remote_op(req.repo, req.cwd, req.op_id)?;
    let mut record =
        read_remote_op(req.repo, req.op_id)?.ok_or_else(|| error("unknown --op-id"))?;
    let remote_dir = validate_existing_scope(
        &record, req.repo, req.host, req.port, req.remote, req.target,
    )?;
    record["action"] = "reconcile".into();
    let state = record
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    if matches!(state.as_str(), "succeeded" | "failed") {
        let code = record
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(1);
        return json_response(code, &record, req.pretty);
    }
    if matches!(state.as_str(), "cancelled" | "lost") {
        return json_response(1, &record, req.pretty);
    }

    let rd_key = &sha256(remote_dir.as_bytes())[..16];
    let tok = new_frame_nonce();
    let script = format!(
        r#"set -eu
rd={rd}
rd_key={rd_key}
op_id_arg={op_id}
tok={tok}
{helpers}
read_host_id
find_op_dir
if [ -z "$op_dir" ]; then
  printf '__OMS_MARK_%s__ PROBE missing|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
  exit 0
fi
if [ -f "$op_dir/result.json" ]; then
  printf '__OMS_MARK_%s__ PROBE result|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
  emit_part "$tok" meta "$op_dir/meta.json"
  emit_part "$tok" result "$op_dir/result.json"
  emit_part "$tok" stdout "$op_dir/stdout"
  emit_part "$tok" stderr "$op_dir/stderr"
  exit 0
fi
mpid=""
m_bid=""
m_tag=""
if [ -f "$op_dir/meta.json" ]; then
  mpid=$(sed -n 's/.*"pid":\([0-9][0-9]*\).*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
  m_bid=$(sed -n 's/.*"boot_id":"\([^"]*\)".*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
  m_tag=$(sed -n 's/.*"op_tag":"\([^"]*\)".*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
fi
alive=0
cur_ls=""
if pid_live "$mpid"; then
  alive=1
  cur_ls=$(LC_ALL=C ps -p "$mpid" -o lstart= 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || true)
fi
if [ "$alive" = 0 ] && [ -f "$op_dir/result.json" ]; then
  printf '__OMS_MARK_%s__ PROBE result|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
  emit_part "$tok" meta "$op_dir/meta.json"
  emit_part "$tok" result "$op_dir/result.json"
  emit_part "$tok" stdout "$op_dir/stdout"
  emit_part "$tok" stderr "$op_dir/stderr"
  exit 0
fi
cur_pg=$(LC_ALL=C ps -p $$ -o pgid= 2>/dev/null | tr -d ' \t\r\n' || true)
read_cpg
check_orphan_cpg
printf '__OMS_MARK_%s__ PROBE live|%s|%s|%s|%s|%s\n' "$tok" "$cur_hn" "$cur_bid" "$alive" "$cur_ls" "$cpg_verdict"
emit_part "$tok" meta "$op_dir/meta.json"
"#,
        rd = remote_path(&remote_dir),
        rd_key = quote(rd_key),
        op_id = quote(req.op_id),
        tok = quote(&tok),
        helpers = REMOTE_SH_HELPERS,
    );
    let effective_port = req.port.or_else(|| {
        record
            .get("port")
            .and_then(|v| v.as_u64())
            .and_then(|n| u16::try_from(n).ok())
    });
    let out = build_ssh(req.host, effective_port, req.control)
        .arg(format!("sh -lc {}", quote(&script)))
        .output()
        .map_err(error)?;
    if !out.status.success() {
        record["state"] = "unknown".into();
        record["updated_at"] = now()?.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return json_response(75, &record, req.pretty);
    }
    let parsed = match parse_framed_stream(&out.stdout, &tok) {
        Ok(v) => v,
        Err(_) => {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            return json_response(75, &record, req.pretty);
        }
    };
    let Some((_, payload)) = parsed.markers.iter().find(|(kind, _)| kind == "PROBE") else {
        record["state"] = "unknown".into();
        record["updated_at"] = now()?.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return json_response(75, &record, req.pretty);
    };
    let fields: Vec<&str> = payload.split('|').collect();
    let kind = fields.first().copied().unwrap_or("");
    let cur_hn = fields.get(1).copied().unwrap_or("unknown");
    let cur_bid = fields.get(2).copied().unwrap_or("unknown");
    if let Some(meta) = parse_meta_part(&parsed.parts) {
        if let Err(e) = verify_remote_meta_binding(&record, &meta) {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            return Err(e);
        }
        let meta_hn = meta.get("hostname").and_then(|v| v.as_str()).unwrap_or("");
        if !meta_hn.is_empty() && meta_hn != cur_hn {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            return json_response(75, &record, req.pretty);
        }
        record["remote_meta"] = meta;
    } else if kind != "missing" {
        record["state"] = "unknown".into();
        record["updated_at"] = now()?.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return json_response(75, &record, req.pretty);
    }
    match kind {
        "missing" => {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            json_response(75, &record, req.pretty)
        }
        "result" => match apply_result_parts(&mut record, &parsed.parts)? {
            Some(rc) => {
                write_remote_op(req.repo, req.op_id, &record)?;
                json_response(rc, &record, req.pretty)
            }
            None => {
                record["state"] = "unknown".into();
                record["updated_at"] = now()?.into();
                write_remote_op(req.repo, req.op_id, &record)?;
                json_response(75, &record, req.pretty)
            }
        },
        "live" => {
            let alive = fields.get(3).copied() == Some("1");
            let cur_ls = fields.get(4).copied().unwrap_or("");
            let cpg_verdict = fields.get(5).copied().unwrap_or("none");
            let Some(meta) = record.get("remote_meta").and_then(|v| v.as_object()) else {
                record["state"] = "unknown".into();
                record["updated_at"] = now()?.into();
                write_remote_op(req.repo, req.op_id, &record)?;
                return json_response(75, &record, req.pretty);
            };
            let meta_bid = meta
                .get("boot_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let meta_ls = meta.get("lstart").and_then(|v| v.as_str()).unwrap_or("");
            let rebooted = meta_bid != "unknown" && cur_bid != "unknown" && meta_bid != cur_bid;
            if alive && !rebooted && !meta_ls.is_empty() && cur_ls.is_empty() {
                record["state"] = "unknown".into();
                record["updated_at"] = now()?.into();
                write_remote_op(req.repo, req.op_id, &record)?;
                return json_response(75, &record, req.pretty);
            }
            let pid_reused = !meta_ls.is_empty()
                && !cur_ls.is_empty()
                && normalize_ws(meta_ls) != normalize_ws(cur_ls);
            if rebooted {
                record["state"] = "lost".into();
                let ts = now()?;
                record["updated_at"] = ts.clone().into();
                record["completed_at"] = ts.into();
                write_remote_op(req.repo, req.op_id, &record)?;
                return json_response(1, &record, req.pretty);
            }
            if !alive || pid_reused {
                return match cpg_verdict {
                    "verified" => {
                        record["state"] = "running".into();
                        record["updated_at"] = now()?.into();
                        write_remote_op(req.repo, req.op_id, &record)?;
                        json_response(75, &record, req.pretty)
                    }
                    "unverifiable" => {
                        record["state"] = "unknown".into();
                        record["updated_at"] = now()?.into();
                        write_remote_op(req.repo, req.op_id, &record)?;
                        json_response(75, &record, req.pretty)
                    }
                    _ => {
                        record["state"] = "lost".into();
                        let ts = now()?;
                        record["updated_at"] = ts.clone().into();
                        record["completed_at"] = ts.into();
                        write_remote_op(req.repo, req.op_id, &record)?;
                        json_response(1, &record, req.pretty)
                    }
                };
            }
            record["state"] = "running".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            json_response(75, &record, req.pretty)
        }
        _ => {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            json_response(75, &record, req.pretty)
        }
    }
}

struct CancelRequest<'a> {
    cwd: &'a Path,
    repo: &'a Path,
    host: &'a str,
    port: Option<u16>,
    remote: Option<&'a str>,
    target: Option<&'a str>,
    op_id: &'a str,
    grace_seconds: u64,
    control: &'a str,
    pretty: bool,
}

fn run_cancel(req: CancelRequest<'_>) -> Result<Response> {
    let _guard = lock_remote_op(req.repo, req.cwd, req.op_id)?;
    let mut record =
        read_remote_op(req.repo, req.op_id)?.ok_or_else(|| error("unknown --op-id"))?;
    let remote_dir = validate_existing_scope(
        &record, req.repo, req.host, req.port, req.remote, req.target,
    )?;
    record["action"] = "cancel".into();
    let state = record
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    if state == "cancelled" {
        return json_response(0, &record, req.pretty);
    }
    if matches!(state.as_str(), "succeeded" | "failed" | "lost") {
        record["signal_sent"] = false.into();
        return json_response(1, &record, req.pretty);
    }

    let rd_key = &sha256(remote_dir.as_bytes())[..16];
    let exp_cmd = record
        .get("command_sha256")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let exp_snap = record
        .get("snapshot")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let exp_tag = record
        .get("op_tag")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let tok = new_frame_nonce();
    let script = format!(
        r#"set -eu
rd={rd}
rd_key={rd_key}
op_id_arg={op_id}
exp_cmd={exp_cmd}
exp_snap={exp_snap}
exp_tag={exp_tag}
grace={grace}
tok={tok}
{helpers}
read_host_id
find_op_dir
if [ -z "$op_dir" ]; then
  printf '__OMS_MARK_%s__ CANCEL missing|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
  exit 0
fi
m_tag=""
if [ -f "$op_dir/meta.json" ]; then
  m_cmd=$(sed -n 's/.*"command_sha256":"\([^"]*\)".*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
  m_snap=$(sed -n 's/.*"snapshot":"\([^"]*\)".*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
  m_tag=$(sed -n 's/.*"op_tag":"\([^"]*\)".*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
  if {{ [ -n "$exp_cmd" ] && [ "$m_cmd" != "$exp_cmd" ]; }} || [ "$m_snap" != "$exp_snap" ] || {{ [ -n "$exp_tag" ] && [ "$m_tag" != "$exp_tag" ]; }}; then
    printf '__OMS_MARK_%s__ CANCEL meta_mismatch|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
    emit_part "$tok" meta "$op_dir/meta.json"
    exit 0
  fi
fi
if [ -f "$op_dir/result.json" ]; then
  printf '__OMS_MARK_%s__ CANCEL finished|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
  emit_part "$tok" meta "$op_dir/meta.json"
  emit_part "$tok" result "$op_dir/result.json"
  emit_part "$tok" stdout "$op_dir/stdout"
  emit_part "$tok" stderr "$op_dir/stderr"
  exit 0
fi
if [ ! -f "$op_dir/meta.json" ]; then
  printf '__OMS_MARK_%s__ CANCEL missing_meta|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
  exit 0
fi
m_hn=$(sed -n 's/.*"hostname":"\([^"]*\)".*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
m_bid=$(sed -n 's/.*"boot_id":"\([^"]*\)".*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
m_pid=$(sed -n 's/.*"pid":\([0-9][0-9]*\).*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
m_ls=$(sed -n 's/.*"lstart":"\([^"]*\)".*/\1/p' "$op_dir/meta.json" | head -n 1 || true)
if [ -z "$m_hn" ] || [ "$m_hn" != "$cur_hn" ]; then
  printf '__OMS_MARK_%s__ CANCEL host_mismatch|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
  emit_part "$tok" meta "$op_dir/meta.json"
  exit 0
fi
cur_pg=$(LC_ALL=C ps -p $$ -o pgid= 2>/dev/null | tr -d ' \t\r\n' || true)
reap_orphan_cpg() {{
  : > "$op_dir/cancel.req"
  kill -TERM -"$cpgid" 2>/dev/null || true
  ticks=$((grace * 10))
  i=0
  sig="SIGTERM"
  while [ "$i" -lt "$ticks" ]; do
    pg_live "$cpgid" || break
    i=$((i + 1))
    sleep 0.1 2>/dev/null || sleep 1
  done
  if pg_live "$cpgid"; then
    sig="SIGKILL"
    kill -KILL -"$cpgid" 2>/dev/null || true
    j=0
    while [ "$j" -lt 20 ]; do
      pg_live "$cpgid" || break
      j=$((j + 1))
      sleep 0.1 2>/dev/null || sleep 1
    done
  fi
  if pg_live "$cpgid"; then
    rm -f -- "$op_dir/cancel.req"
    printf '__OMS_MARK_%s__ CANCEL still_alive|%s\n' "$tok" "$sig"
  else
    : > "$op_dir/reaped.done"
    printf '__OMS_MARK_%s__ CANCEL cancelled|%s\n' "$tok" "$sig"
    emit_part "$tok" meta "$op_dir/meta.json"
    emit_part "$tok" stdout "$op_dir/stdout"
    emit_part "$tok" stderr "$op_dir/stderr"
  fi
  exit 0
}}
if [ -z "$m_pid" ] || ! pid_live "$m_pid"; then
  if [ -f "$op_dir/result.json" ]; then
    printf '__OMS_MARK_%s__ CANCEL finished|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
    emit_part "$tok" meta "$op_dir/meta.json"
    emit_part "$tok" result "$op_dir/result.json"
    emit_part "$tok" stdout "$op_dir/stdout"
    emit_part "$tok" stderr "$op_dir/stderr"
  else
    read_cpg
    check_orphan_cpg
    case "$cpg_verdict" in
      verified)
        reap_orphan_cpg
        ;;
      unverifiable)
        printf '__OMS_MARK_%s__ CANCEL unverifiable_lstart|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
        emit_part "$tok" meta "$op_dir/meta.json"
        exit 0
        ;;
    esac
    printf '__OMS_MARK_%s__ CANCEL lost|exited|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
    emit_part "$tok" meta "$op_dir/meta.json"
  fi
  exit 0
fi
cur_ls=$(LC_ALL=C ps -p "$m_pid" -o lstart= 2>/dev/null | tr -cd 'A-Za-z0-9._: +=,-' || true)
if [ -z "$m_ls" ] || [ -z "$cur_ls" ]; then
  printf '__OMS_MARK_%s__ CANCEL unverifiable_lstart|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
  emit_part "$tok" meta "$op_dir/meta.json"
  exit 0
fi
if [ -z "$m_bid" ] || [ -z "$cur_bid" ] || [ "$m_bid" != "$cur_bid" ] || [ "$(norm "$m_ls")" != "$(norm "$cur_ls")" ]; then
  if [ -n "$m_bid" ] && [ "$m_bid" = "$cur_bid" ]; then
    read_cpg
    check_orphan_cpg
    case "$cpg_verdict" in
      verified)
        reap_orphan_cpg
        ;;
      unverifiable)
        printf '__OMS_MARK_%s__ CANCEL unverifiable_lstart|%s|%s\n' "$tok" "$cur_hn" "$cur_bid"
        emit_part "$tok" meta "$op_dir/meta.json"
        exit 0
        ;;
    esac
  fi
  printf '__OMS_MARK_%s__ CANCEL mismatch|%s|%s|%s\n' "$tok" "$cur_hn" "$cur_bid" "$cur_ls"
  emit_part "$tok" meta "$op_dir/meta.json"
  exit 0
fi
w=0
while [ ! -f "$op_dir/child.pid" ] && [ ! -f "$op_dir/result.json" ] && pid_live "$m_pid" && [ "$w" -lt 20 ]; do
  w=$((w + 1))
  sleep 0.02 2>/dev/null || sleep 1
done
w=0
while [ ! -f "$op_dir/child.pgid" ] && [ ! -f "$op_dir/result.json" ] && pid_live "$m_pid" && [ "$w" -lt 5 ]; do
  w=$((w + 1))
  sleep 0.02 2>/dev/null || sleep 1
done
cpid=""
if [ -f "$op_dir/child.pid" ]; then
  cand_cpid=$(tr -d ' \t\r\n' < "$op_dir/child.pid" || true)
  if [ -n "$cand_cpid" ]; then
    cppid=$(LC_ALL=C ps -p "$cand_cpid" -o ppid= 2>/dev/null | tr -d ' \t\r\n' || true)
    if [ "$cppid" = "$m_pid" ]; then
      cpid="$cand_cpid"
    fi
  fi
fi
read_cpg
all_live() {{
  if pid_live "$m_pid"; then return 0; fi
  if [ -n "$cpid" ] && pid_live "$cpid"; then return 0; fi
  if [ -n "$cpgid" ] && pg_live "$cpgid"; then return 0; fi
  return 1
}}
: > "$op_dir/cancel.req"
[ -n "$cpgid" ] && kill -TERM -"$cpgid" 2>/dev/null || true
[ -n "$cpid" ] && kill -TERM "$cpid" 2>/dev/null || true
kill -TERM "$m_pid" 2>/dev/null || true
ticks=$((grace * 10))
i=0
sig="SIGTERM"
while [ "$i" -lt "$ticks" ]; do
  all_live || break
  i=$((i + 1))
  sleep 0.1 2>/dev/null || sleep 1
done
if all_live; then
  sig="SIGKILL"
  if [ -n "$cpgid" ]; then
    kill -KILL -"$cpgid" 2>/dev/null || true
  fi
  if [ -n "$cpid" ]; then
    if command -v pkill >/dev/null 2>&1; then
      pkill -KILL -P "$cpid" 2>/dev/null || true
    fi
    kill -KILL "$cpid" 2>/dev/null || true
  fi
  kill -KILL "$m_pid" 2>/dev/null || true
  j=0
  while [ "$j" -lt 20 ]; do
    all_live || break
    j=$((j + 1))
    sleep 0.1 2>/dev/null || sleep 1
  done
fi
if all_live; then
  rm -f -- "$op_dir/cancel.req"
  printf '__OMS_MARK_%s__ CANCEL still_alive|%s\n' "$tok" "$sig"
  exit 0
fi
: > "$op_dir/reaped.done"
printf '__OMS_MARK_%s__ CANCEL cancelled|%s\n' "$tok" "$sig"
emit_part "$tok" meta "$op_dir/meta.json"
emit_part "$tok" stdout "$op_dir/stdout"
emit_part "$tok" stderr "$op_dir/stderr"
"#,
        rd = remote_path(&remote_dir),
        rd_key = quote(rd_key),
        op_id = quote(req.op_id),
        exp_cmd = quote(&exp_cmd),
        exp_snap = quote(&exp_snap),
        exp_tag = quote(&exp_tag),
        grace = req.grace_seconds,
        tok = quote(&tok),
        helpers = REMOTE_SH_HELPERS,
    );
    let effective_port = req.port.or_else(|| {
        record
            .get("port")
            .and_then(|v| v.as_u64())
            .and_then(|n| u16::try_from(n).ok())
    });
    let out = build_ssh(req.host, effective_port, req.control)
        .arg(format!("sh -lc {}", quote(&script)))
        .output()
        .map_err(error)?;
    if !out.status.success() {
        record["state"] = "unknown".into();
        record["updated_at"] = now()?.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return json_response(75, &record, req.pretty);
    }
    let parsed = match parse_framed_stream(&out.stdout, &tok) {
        Ok(v) => v,
        Err(_) => {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            return json_response(75, &record, req.pretty);
        }
    };
    if let Some(meta) = parse_meta_part(&parsed.parts) {
        if let Err(e) = verify_remote_meta_binding(&record, &meta) {
            record["state"] = "unknown".into();
            record["signal_sent"] = false.into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            return Err(e);
        }
        record["remote_meta"] = meta;
    }
    if let Some(out_bytes) = parsed.parts.get("stdout") {
        record["stdout"] = bounded_output(out_bytes).into();
    }
    if let Some(err_bytes) = parsed.parts.get("stderr") {
        record["stderr"] = bounded_output(err_bytes).into();
    }
    let Some((_, payload)) = parsed.markers.iter().find(|(kind, _)| kind == "CANCEL") else {
        record["state"] = "unknown".into();
        record["updated_at"] = now()?.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return json_response(75, &record, req.pretty);
    };
    let fields: Vec<&str> = payload.split('|').collect();
    match fields.first().copied().unwrap_or("") {
        "meta_mismatch" => {
            record["state"] = "unknown".into();
            record["signal_sent"] = false.into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            Err(error("remote operation metadata mismatch"))
        }
        "cancelled" => {
            record["state"] = "cancelled".into();
            record["signal"] = fields.get(1).copied().unwrap_or("SIGTERM").into();
            record["signal_sent"] = true.into();
            let ts = now()?;
            record["updated_at"] = ts.clone().into();
            record["completed_at"] = ts.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            json_response(0, &record, req.pretty)
        }
        "finished" => match apply_result_parts(&mut record, &parsed.parts)? {
            Some(_) => {
                record["signal_sent"] = false.into();
                record["finished_before_cancel"] = true.into();
                write_remote_op(req.repo, req.op_id, &record)?;
                json_response(1, &record, req.pretty)
            }
            None => {
                record["state"] = "unknown".into();
                record["signal_sent"] = false.into();
                write_remote_op(req.repo, req.op_id, &record)?;
                json_response(75, &record, req.pretty)
            }
        },
        "mismatch" | "lost" => {
            record["state"] = "lost".into();
            record["signal_sent"] = false.into();
            let ts = now()?;
            record["updated_at"] = ts.clone().into();
            record["completed_at"] = ts.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            json_response(1, &record, req.pretty)
        }
        _ => {
            record["state"] = "unknown".into();
            record["signal_sent"] = false.into();
            record["updated_at"] = now()?.into();
            write_remote_op(req.repo, req.op_id, &record)?;
            json_response(75, &record, req.pretty)
        }
    }
}

struct CollectRequest<'a> {
    cwd: &'a Path,
    host: &'a str,
    port: Option<u16>,
    remote: &'a str,
    remote_path_rel: &'a str,
    dest_raw: &'a str,
    expected_sha256: Option<&'a str>,
    resume: Option<resumable::Options>,
    max_bytes: u64,
    control: &'a str,
    pretty: bool,
    interrupted: &'a AtomicBool,
}

struct TempFileGuard {
    path: PathBuf,
    keep: bool,
}
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn prepare_collect_dest(cwd: &Path, raw: &str) -> Result<(PathBuf, PathBuf)> {
    require(
        !raw.is_empty()
            && !raw.as_bytes().contains(&0)
            && !raw.ends_with('/')
            && !raw.ends_with("/.")
            && !raw.ends_with("/..")
            && raw != "."
            && raw != "..",
        "invalid --dest path",
    )?;
    let raw_path = Path::new(raw);
    let abs = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        require(
            raw_path
                .components()
                .all(|c| !matches!(c, Component::ParentDir)),
            "relative --dest cannot traverse parent directories",
        )?;
        let mut cursor = cwd.to_path_buf();
        let parts: Vec<_> = raw_path
            .components()
            .filter_map(|c| match c {
                Component::Normal(p) => Some(p),
                _ => None,
            })
            .collect();
        require(!parts.is_empty(), "invalid --dest path")?;
        for dir_part in &parts[..parts.len() - 1] {
            cursor.push(dir_part);
            match std::fs::symlink_metadata(&cursor) {
                Ok(meta) => require(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "destination parent contains a symlink or non-directory",
                )?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(error(e)),
            }
        }
        cwd.join(raw_path)
    };
    let parent = abs
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| error("destination requires a parent directory"))?
        .to_path_buf();
    let _file_name = abs
        .file_name()
        .ok_or_else(|| error("destination requires a file name"))?;
    match std::fs::symlink_metadata(&abs) {
        Ok(meta) => require(
            meta.is_file() && !meta.file_type().is_symlink(),
            "destination cannot be a symlink or directory",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(e)),
    }
    match std::fs::symlink_metadata(&parent) {
        Ok(meta) => require(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "destination parent must be a real non-symlink directory",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&parent)
                .map_err(error)?;
        }
        Err(e) => return Err(error(e)),
    }
    // Alternate spellings must select the same destination lock and cache.
    let parent = std::fs::canonicalize(&parent).map_err(error)?;
    let dest = parent.join(abs.file_name().expect("validated file name"));
    Ok((parent, dest))
}

fn create_private_temp(parent: &Path) -> Result<(std::fs::File, TempFileGuard)> {
    let nofollow = rustix::fs::OFlags::NOFOLLOW.bits() as i32;
    for n in 0..1000 {
        let candidate = parent.join(format!(".oms-collect-{}-{n}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nofollow)
            .open(&candidate)
        {
            Ok(file) => {
                return Ok((
                    file,
                    TempFileGuard {
                        path: candidate,
                        keep: false,
                    },
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(error(e)),
        }
    }
    Err(error("cannot create private collection temporary file"))
}

fn collect_target_script(remote: &str, remote_path_rel: &str) -> String {
    format!(
        r#"set -eu
rd={rd}
rp={rp}
if [ ! -d "$rd" ] || [ -L "$rd" ]; then
  printf '%s\n' 'remote_dir must be a real non-symlink directory' >&2
  exit 2
fi
cur="$rd"
old_ifs="$IFS"
IFS=/
set -f
set -- $rp
IFS="$old_ifs"
set +f
while [ "$#" -gt 1 ]; do
  cur="$cur/$1"
  if [ ! -d "$cur" ] || [ -L "$cur" ]; then
    printf '%s\n' 'remote path ancestor must be a real non-symlink directory' >&2
    exit 2
  fi
  shift
done
target="$cur/$1"
if [ -L "$target" ] || [ ! -f "$target" ]; then
  printf '%s\n' 'remote path must be a regular non-symlink file' >&2
  exit 2
fi
"#,
        rd = remote_path(remote),
        rp = quote(remote_path_rel),
    )
}

fn run_collect(req: CollectRequest<'_>) -> Result<Response> {
    let (parent, dest_abs) = prepare_collect_dest(req.cwd, req.dest_raw)?;
    let destination_lock = resumable::destination_lock(req.cwd, &dest_abs)?;
    if let Some(options) = req.resume {
        return resumable::run(req, options, &parent, &dest_abs, &destination_lock);
    }
    let script = format!(
        "{}exec cat -- \"$target\"\n",
        collect_target_script(req.remote, req.remote_path_rel)
    );
    let mut child = build_ssh(req.host, req.port, req.control)
        .arg(format!("sh -lc {}", quote(&script)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(error)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("missing ssh stdout pipe"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| error("missing ssh stderr pipe"))?;

    let (mut tmp_file, mut guard) = create_private_temp(&parent)?;
    let (stream_res, err_bytes) = std::thread::scope(|s| {
        let err_handle = s.spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut stderr).take(MAX_STDERR_BYTES).read_to_end(&mut buf);
            buf
        });
        let mut reader = stdout.take(req.max_bytes.saturating_add(1));
        let mut buf = [0u8; 65536];
        let mut total = 0u64;
        let mut hasher = Sha256::new();
        let copy_res: Result<()> = loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) => break Err(error(e)),
            };
            total = total.saturating_add(n as u64);
            if total > req.max_bytes {
                break Err(status(
                    2,
                    format!("collected stream exceeded --max-bytes ({})", req.max_bytes),
                ));
            }
            if let Err(e) = tmp_file.write_all(&buf[..n]) {
                break Err(error(e));
            }
            hasher.update(&buf[..n]);
        };
        drop(reader);
        if copy_res.is_err() {
            let _ = child.kill();
        }
        let err_buf = err_handle.join().unwrap_or_default();
        (copy_res.map(|()| (total, hasher.finish())), err_buf)
    });
    let status_out = child.wait().map_err(error)?;
    let (total_bytes, digest) = stream_res?;
    let exit = exit_code(status_out);
    if !status_out.success() {
        return Err(status(
            if exit == 0 { 1 } else { exit },
            format!(
                "remote collect failed (exit {exit}): {}",
                String::from_utf8_lossy(&err_bytes).trim()
            ),
        ));
    }
    if let Some(expected) = req.expected_sha256 {
        require(
            expected == digest,
            format!("collected file digest mismatch: expected {expected}, got {digest}"),
        )?;
    }
    tmp_file.sync_all().map_err(error)?;
    drop(tmp_file);
    destination_lock.check().map_err(error)?;
    match std::fs::symlink_metadata(&dest_abs) {
        Ok(meta) => require(
            meta.is_file() && !meta.file_type().is_symlink(),
            "destination became a symlink or directory",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(e)),
    }
    std::fs::rename(&guard.path, &dest_abs).map_err(error)?;
    guard.keep = true;

    let value = serde_json::json!({
        "schema": 1,
        "action": "collect",
        "host": req.host,
        "remote_dir": req.remote,
        "remote_path": req.remote_path_rel,
        "dest": dest_abs.to_string_lossy(),
        "bytes": total_bytes,
        "sha256": digest,
        "verified": req.expected_sha256.is_some(),
    });
    json_response(0, &value, req.pretty)
}
/// Shell word for a remote directory; a leading `~` expands on the remote host.
fn remote_path(s: &str) -> String {
    if s == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = s.strip_prefix("~/") {
        if rest.is_empty() {
            "\"$HOME\"".into()
        } else {
            format!("\"$HOME\"/{}", quote(rest))
        }
    } else {
        quote(s)
    }
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(128)
}
fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(error)
}
fn now() -> Result<String> {
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(error)?;
    Ok(
        chrono::DateTime::from_timestamp(t.as_secs() as i64, t.subsec_nanos())
            .ok_or_else(|| error("invalid time"))?
            .to_rfc3339(),
    )
}
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Result<Self> {
        for n in 0..1000 {
            let p = std::env::temp_dir().join(format!("oms-bundle-{}-{n}", std::process::id()));
            use std::os::unix::fs::DirBuilderExt;
            match std::fs::DirBuilder::new().mode(0o700).create(&p) {
                Ok(()) => return Ok(Self(p)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(error(e)),
            }
        }
        Err(error("cannot create temporary directory"))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_bytes: u64,
}
impl Sha256 {
    fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0u8; 64],
            buf_len: 0,
            total_bytes: 0,
        }
    }
    fn compress(h: &mut [u32; 8], chunk: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let a = w[i - 15];
            let b = w[i - 2];
            w[i] = w[i - 16]
                .wrapping_add(a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3))
                .wrapping_add(w[i - 7])
                .wrapping_add(b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10));
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut z] = *h;
        for i in 0..64 {
            let t1 = z
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            z = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, z]) {
            *x = x.wrapping_add(y);
        }
    }
    fn update(&mut self, mut data: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = data.len().min(64 - self.buf_len);
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                Self::compress(&mut self.h, &block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let block: &[u8; 64] = data[..64].try_into().expect("64-byte slice");
            Self::compress(&mut self.h, block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }
    fn finish(mut self) -> String {
        let bits = self.total_bytes.wrapping_mul(8);
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            self.buf[self.buf_len..].fill(0);
            let block = self.buf;
            Self::compress(&mut self.h, &block);
            self.buf_len = 0;
        }
        self.buf[self.buf_len..56].fill(0);
        self.buf[56..64].copy_from_slice(&bits.to_be_bytes());
        let block = self.buf;
        Self::compress(&mut self.h, &block);
        self.h.iter().map(|v| format!("{v:08x}")).collect()
    }
}

fn sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn remote_path_expands_leading_tilde() {
        assert_eq!(remote_path("~"), "\"$HOME\"");
        assert_eq!(remote_path("~/foo/bar"), "\"$HOME\"/'foo/bar'");
        assert_eq!(remote_path("/a b"), "'/a b'");
        assert_eq!(remote_path("a~/b"), "'a~/b'");
    }

    fn invoke(args: &[&str], cwd: &Path) -> Response {
        run(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            cwd,
            &AtomicBool::new(false),
        )
    }
    #[test]
    fn digest_and_validation() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        for help in ["-h", "--help"] {
            assert_eq!(invoke(&[help], Path::new(".")).code, 0);
        }
        assert_ne!(
            invoke(&["probe", "--host", "-oProxyCommand=bad"], Path::new(".")).code,
            0
        );
        assert_ne!(
            invoke(&["probe", "--host", "host", "--port", "0"], Path::new(".")).code,
            0
        );
    }
    #[test]
    fn execution_inputs_require_approval_and_integrity() {
        use crate::value::*;
        let temp = TempDir::new().unwrap();
        let runtime = temp.0.join(".oms/runtime");
        std::fs::create_dir_all(runtime.join("snapshots")).unwrap();
        let identity = object([
            ("ssh_alias", string("host")),
            ("workdir", string("~/repo")),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
        ]);
        let fingerprint = sha(canonical(&identity).unwrap());
        let mut row = identity;
        row.set("execution_approved", b(false)).unwrap();
        row.set("approved_fingerprint", string(&fingerprint))
            .unwrap();
        let write_registry = |row: Value| {
            let registry = object([("schema", number(1)), ("targets", object([("test", row)]))]);
            std::fs::write(runtime.join("targets.json"), canonical(&registry).unwrap()).unwrap();
        };
        write_registry(row.clone());
        assert!(validate_target(&temp.0, "test", "host", "~/repo").is_err());
        row.set("execution_approved", b(true)).unwrap();
        write_registry(row);
        assert!(validate_target(&temp.0, "test", "host", "~/repo").is_ok());
        assert!(validate_target(&temp.0, "test", "other", "~/repo").is_err());

        let mut snapshot = object([
            ("schema", number(1)),
            ("head_sha", string("head")),
            ("tree_sha", string("tree")),
            ("lockfiles", empty()),
        ]);
        let digest = sha(canonical(&snapshot).unwrap());
        snapshot.set("snapshot_sha256", string(&digest)).unwrap();
        let path = runtime.join("snapshots").join(format!("{digest}.json"));
        std::fs::write(&path, canonical(&snapshot).unwrap()).unwrap();
        assert!(validate_snapshot(&temp.0, &digest).is_ok());
        snapshot.set("head_sha", string("changed")).unwrap();
        std::fs::write(path, canonical(&snapshot).unwrap()).unwrap();
        assert!(validate_snapshot(&temp.0, &digest).is_err());
    }
    #[test]
    fn transport() {
        if std::env::var_os("OMS_TEST_SSH_CHILD").is_none() {
            let temp = TempDir::new().unwrap();
            let fake = temp.0.join("ssh");
            std::fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$OMS_TEST_ARGS\"\nfor last do :; done\ncase \"$last\" in *oms-bundle*) cat > \"$OMS_TEST_BUNDLE\"; exec sh -c \"$last\" < \"$OMS_TEST_BUNDLE\";; *) exec sh -c \"$last\";; esac\n").unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "ssh_adapter::tests::transport", "--nocapture"])
                .env("OMS_TEST_SSH_CHILD", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_ARGS", temp.0.join("args"))
                .env("OMS_TEST_BUNDLE", temp.0.join("bundle"))
                .env("OMS_TEST_DIR", &temp.0)
                .env_remove("OMS_HARNESS_CHILD")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote space");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir_all(repo.join(".oms/runtime")).unwrap();
        std::fs::write(
            repo.join(".oms/runtime/targets.json"),
            r#"{"schema":1,"targets":{"test":{"ssh_alias":"host","workdir":"/remote","execution_approved":false}}}"#,
        )
        .unwrap();
        let args_path = PathBuf::from(std::env::var_os("OMS_TEST_ARGS").unwrap());
        let denied = invoke(
            &[
                "exec",
                "--host",
                "host",
                "--repo",
                "repo",
                "--remote-dir",
                "/remote",
                "--target",
                "test",
                "--",
                "true",
            ],
            &root,
        );
        assert_eq!(denied.code, 2);
        assert!(!args_path.exists());
        for args in [
            &["init", "-q"][..],
            &["config", "user.name", "Test"][..],
            &["config", "user.email", "test@example.invalid"][..],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(repo.join("file"), "content").unwrap();
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "file"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-qm", "initial"])
                .status()
                .unwrap()
                .success()
        );
        let long_host = "a".repeat(300);
        let response = invoke(&["probe", "--host", &long_host, "--json"], &root);
        assert_eq!(response.code, 0);
        let v: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        assert_eq!(v["stdout"], "ok");
        let ctrl = v["control_path"].as_str().unwrap();
        assert!(ctrl.len() <= 80);
        let ctrl_dir = Path::new(ctrl).parent().unwrap();
        assert_eq!(
            std::fs::symlink_metadata(ctrl_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let args = std::fs::read_to_string(root.join("args")).unwrap();
        for option in [
            "ControlMaster=auto",
            "ControlPersist=120",
            "ControlPath=",
            "BatchMode=yes",
            "ConnectTimeout=10",
        ] {
            assert!(args.contains(option));
        }
        let repo = repo.to_str().unwrap();
        let remote_str = remote.to_str().unwrap();
        let response = invoke(
            &[
                "sync-bundle",
                "--repo",
                repo,
                "--host",
                "example",
                "--remote-dir",
                remote_str,
            ],
            &root,
        );
        assert_eq!(
            response.code,
            0,
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        assert_eq!(v["head_sha"].as_str().unwrap().len(), 40);
        assert!(v["bundle_bytes"].as_u64().unwrap() > 0);
        assert!(
            std::fs::read(root.join("bundle"))
                .unwrap()
                .starts_with(b"# v2 git bundle")
        );
        assert_eq!(
            std::fs::read_to_string(remote.join("file")).unwrap(),
            "content"
        );
        let first = String::from_utf8(git(Path::new(repo), &["rev-parse", "HEAD"]).unwrap().stdout)
            .unwrap();
        let first = first.trim();
        std::fs::write(Path::new(repo).join("file"), "second").unwrap();
        assert!(
            git(Path::new(repo), &["commit", "-qam", "second"])
                .unwrap()
                .status
                .success()
        );
        let response = invoke(
            &[
                "sync-bundle",
                "--repo",
                repo,
                "--host",
                "example",
                "--remote-dir",
                remote_str,
                "--base",
                "HEAD~1",
            ],
            &root,
        );
        assert_eq!(response.code, 0);
        let v: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        assert_eq!(v["base_sha"], first);
        assert_ne!(v["head_sha"], first);
        assert_eq!(
            std::fs::read_to_string(remote.join("file")).unwrap(),
            "second"
        );
        let response = invoke(
            &[
                "exec",
                "--repo",
                repo,
                "--host",
                "example",
                "--remote-dir",
                remote_str,
                "--",
                "sh",
                "-c",
                "printf hello; exit 7",
            ],
            &root,
        );
        assert_eq!(response.code, 7);
        let v: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["stdout"], "hello");
        assert_eq!(v["exit_code"], 7);
        let logged = std::fs::read_to_string(root.join("args")).unwrap();
        assert_eq!(
            v["command_sha256"],
            sha256(logged.lines().last().unwrap().as_bytes())
        );
        for key in ["started_at", "completed_at"] {
            assert!(chrono::DateTime::parse_from_rfc3339(v[key].as_str().unwrap()).is_ok());
        }
    }

    #[test]
    fn parse_framed_stream_handles_binary_and_marker_lines() {
        let nonce = "0123456789abcdef";
        let payload = b"__OMS_PART_0123456789abcdef__ spoofed 999\n__OMS_PART__ old 1\nline2\n";
        let stream = format!(
            "untrusted banner\n__OMS_MARK_{nonce}__ PROBE result|h1|b1\n__OMS_PART_{nonce}__ stdout {}\n{}__OMS_PART_{nonce}__ stderr 0\n\n",
            payload.len(),
            String::from_utf8_lossy(payload),
        );
        let parsed = parse_framed_stream(stream.as_bytes(), nonce).unwrap();
        assert_eq!(
            parsed.markers,
            vec![("PROBE".to_owned(), "result|h1|b1".to_owned())]
        );
        assert_eq!(parsed.parts.get("stdout").unwrap(), payload);
        assert_eq!(parsed.parts.get("stderr").unwrap(), b"");

        let dup = format!(
            "__OMS_PART_{nonce}__ result 15\n{{\"exit_code\":1}}\n__OMS_PART_{nonce}__ result 15\n{{\"exit_code\":0}}\n"
        );
        assert!(parse_framed_stream(dup.as_bytes(), nonce).is_err());
    }

    #[test]
    fn remote_op_id_validation_and_child_read_only_gate() {
        if std::env::var_os("OMS_TEST_SSH_CHILD_GATE").is_none() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ssh_adapter::tests::remote_op_id_validation_and_child_read_only_gate",
                    "--nocapture",
                ])
                .env("OMS_TEST_SSH_CHILD_GATE", "1")
                .env("OMS_HARNESS_CHILD", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        for bad in ["", "-op", ".op", "a/../b", "a/b", &"x".repeat(65)] {
            assert!(validate_op_id(bad).is_err(), "{bad}");
        }
        assert!(validate_op_id("op_1.a-b").is_ok());
        for bad_rp in [
            "",
            "/etc/passwd",
            "~/a",
            "../secret",
            "a/../b",
            "a/./b",
            "a/",
        ] {
            assert!(validate_remote_rel_path(bad_rp).is_err(), "{bad_rp}");
        }
        assert!(validate_remote_rel_path("outputs/artifact.bin").is_ok());

        for blocked in ["sync-bundle", "exec", "cancel", "collect"] {
            let resp = invoke(&[blocked, "--host", "example"], Path::new("."));
            assert_eq!(resp.code, 2, "{blocked}");
            assert!(
                String::from_utf8_lossy(&resp.stderr)
                    .contains("a harness child may only use read-only runtime actions")
            );
        }
        // reconcile passes the child gate and fails only on missing --repo / --op-id
        let rec = invoke(&["reconcile", "--host", "example"], Path::new("."));
        assert!(
            !String::from_utf8_lossy(&rec.stderr).contains("read-only runtime actions"),
            "{}",
            String::from_utf8_lossy(&rec.stderr)
        );
    }

    struct ChildKillGuard(Option<std::process::Child>);
    impl Drop for ChildKillGuard {
        fn drop(&mut self) {
            if let Some(mut c) = self.0.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    fn init_temp_git_repo(repo: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.name", "Test"][..],
            &["config", "user.email", "test@example.invalid"][..],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(repo.join("file"), "init\n").unwrap();
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(["add", "file"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(["commit", "-qm", "init"])
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn exec_op_id_disconnect_recovers_via_reconcile_without_duplicate() {
        if std::env::var_os("OMS_TEST_SCENARIO4").is_none() {
            let temp = TempDir::new().unwrap();
            let state_dir = temp.0.join("remote_state");
            let fake = temp.0.join("ssh");
            let script = r#"#!/bin/sh
printf '%s\n' "$@" >> "$OMS_TEST_ARGS"
for last do :; done
if [ -f "$OMS_TEST_DROP_FLAG" ]; then
  rm -f "$OMS_TEST_DROP_FLAG"
  sh -c "$last" >/dev/null 2>&1 &
  i=0
  while [ ! -f "$OMS_REMOTE_STATE_DIR/op-4/result.json" ] && [ "$i" -lt 250 ]; do
    i=$((i + 1))
    sleep 0.02 2>/dev/null || sleep 1
  done
  exit 255
fi
exec sh -c "$last"
"#;
            std::fs::write(&fake, script).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(temp.0.join("drop_once"), "1").unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ssh_adapter::tests::exec_op_id_disconnect_recovers_via_reconcile_without_duplicate",
                    "--nocapture",
                ])
                .env("OMS_TEST_SCENARIO4", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_DIR", &temp.0)
                .env("OMS_TEST_ARGS", temp.0.join("args"))
                .env("OMS_TEST_DROP_FLAG", temp.0.join("drop_once"))
                .env("OMS_REMOTE_STATE_DIR", &state_dir)
                .env("OMS_LOCK_DIR", temp.0.join("locks"))
                .env_remove("OMS_HARNESS_CHILD")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote");
        init_temp_git_repo(&repo);
        std::fs::create_dir_all(&remote).unwrap();
        let exec_log = root.join("exec_count.log");
        let cmd_str = format!(
            "printf '1\\n' >> '{}'; printf 'recovered-ok\\n'",
            exec_log.display()
        );
        let rs = repo.to_str().unwrap();
        let rd = remote.to_str().unwrap();
        let first = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-4",
                "--",
                "sh",
                "-c",
                &cmd_str,
            ],
            &root,
        );
        assert_eq!(first.code, 75, "{}", String::from_utf8_lossy(&first.stderr));
        let v1: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
        assert_eq!(v1["state"], "unknown");
        let ledger_path = repo.join(".oms/runtime/remote-ops/op-4.json");
        assert_eq!(
            std::fs::symlink_metadata(&ledger_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let ssh_calls_after_1 = std::fs::read_to_string(root.join("args"))
            .unwrap()
            .lines()
            .count();

        // Repeat exec before reconcile must fail closed without touching SSH.
        let repeat_before_reconcile = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-4",
                "--",
                "sh",
                "-c",
                &cmd_str,
            ],
            &root,
        );
        assert_eq!(repeat_before_reconcile.code, 75);
        assert_eq!(
            std::fs::read_to_string(root.join("args"))
                .unwrap()
                .lines()
                .count(),
            ssh_calls_after_1
        );

        let rec = invoke(
            &[
                "reconcile",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-4",
            ],
            &root,
        );
        assert_eq!(rec.code, 0, "{}", String::from_utf8_lossy(&rec.stderr));
        let vr: serde_json::Value = serde_json::from_slice(&rec.stdout).unwrap();
        assert_eq!(vr["state"], "succeeded");
        assert_eq!(vr["exit_code"], 0);
        assert_eq!(vr["stdout"], "recovered-ok\n");
        let ssh_calls_after_reconcile = std::fs::read_to_string(root.join("args"))
            .unwrap()
            .lines()
            .count();

        // Subsequent exec --op-id must deduplicate without invoking SSH or re-executing command.
        let dedup = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-4",
                "--",
                "sh",
                "-c",
                &cmd_str,
            ],
            &root,
        );
        assert_eq!(dedup.code, 0);
        let vd: serde_json::Value = serde_json::from_slice(&dedup.stdout).unwrap();
        assert_eq!(vd["deduplicated"], true);
        assert_eq!(vd["stdout"], "recovered-ok\n");
        assert_eq!(
            std::fs::read_to_string(root.join("args"))
                .unwrap()
                .lines()
                .count(),
            ssh_calls_after_reconcile
        );
        assert_eq!(
            std::fs::read_to_string(&exec_log).unwrap().lines().count(),
            1
        );
    }

    #[test]
    fn pid_reuse_boot_id_and_conflicting_op_id_are_fenced() {
        if std::env::var_os("OMS_TEST_SCENARIO7").is_none() {
            let temp = TempDir::new().unwrap();
            let state_dir = temp.0.join("remote_state");
            let fake = temp.0.join("ssh");
            let script = r#"#!/bin/sh
printf '%s\n' "$@" >> "$OMS_TEST_ARGS"
for last do :; done
if [ -f "$OMS_TEST_DROP_ACTIVE" ]; then
  rm -f "$OMS_TEST_DROP_ACTIVE"
  sh -c "$last" >/dev/null 2>&1 &
  i=0
  while [ ! -f "$OMS_REMOTE_STATE_DIR/op-live/child.pid" ] && [ "$i" -lt 250 ]; do
    i=$((i + 1))
    sleep 0.02 2>/dev/null || sleep 1
  done
  exit 255
fi
exec sh -c "$last"
"#;
            std::fs::write(&fake, script).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ssh_adapter::tests::pid_reuse_boot_id_and_conflicting_op_id_are_fenced",
                    "--nocapture",
                ])
                .env("OMS_TEST_SCENARIO7", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_DIR", &temp.0)
                .env("OMS_TEST_ARGS", temp.0.join("args"))
                .env("OMS_TEST_DROP_ACTIVE", temp.0.join("drop_active"))
                .env("OMS_REMOTE_STATE_DIR", &state_dir)
                .env("OMS_LOCK_DIR", temp.0.join("locks"))
                .env_remove("OMS_HARNESS_CHILD")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let state_dir = PathBuf::from(std::env::var_os("OMS_REMOTE_STATE_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote");
        init_temp_git_repo(&repo);
        std::fs::create_dir_all(&remote).unwrap();
        let rs = repo.to_str().unwrap();
        let rd = remote.to_str().unwrap();

        // 1. Conflicting --op-id reuse refuses locally with exit 2 and remotely with exit 75.
        let r0 = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-conflict",
                "--",
                "true",
            ],
            &root,
        );
        assert_eq!(r0.code, 0, "{}", String::from_utf8_lossy(&r0.stderr));
        let diff_cmd = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-conflict",
                "--",
                "false",
            ],
            &root,
        );
        assert_eq!(diff_cmd.code, 2);
        std::fs::remove_file(repo.join(".oms/runtime/remote-ops/op-conflict.json")).unwrap();
        let remote_dup = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-conflict",
                "--",
                "true",
            ],
            &root,
        );
        assert_eq!(remote_dup.code, 75);

        // 2. Live detached process -> reconcile reports running (75), cancel stops cleanly (0).
        std::fs::write(root.join("drop_active"), "1").unwrap();
        let live_exec = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-live",
                "--",
                "sleep",
                "30",
            ],
            &root,
        );
        assert_eq!(live_exec.code, 75);
        let live_rec = invoke(
            &[
                "reconcile",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-live",
            ],
            &root,
        );
        assert_eq!(live_rec.code, 75);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&live_rec.stdout).unwrap()["state"],
            "running"
        );
        let canceled = invoke(
            &[
                "cancel",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-live",
                "--grace-seconds",
                "1",
            ],
            &root,
        );
        assert_eq!(
            canceled.code,
            0,
            "{}",
            String::from_utf8_lossy(&canceled.stderr)
        );
        let vc: serde_json::Value = serde_json::from_slice(&canceled.stdout).unwrap();
        assert_eq!(vc["state"], "cancelled");
        assert_eq!(vc["signal_sent"], true);

        // 3. Simulated PID reuse (live unrelated sleep child with different lstart) blocks cancel.
        let mut sleeper = ChildKillGuard(Some(Command::new("sleep").arg("30").spawn().unwrap()));
        let sleeper_pid = sleeper.0.as_ref().unwrap().id();
        let host_probe = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-meta-src",
                "--",
                "true",
            ],
            &root,
        );
        assert_eq!(host_probe.code, 0);
        let meta_src: serde_json::Value =
            serde_json::from_slice::<serde_json::Value>(&host_probe.stdout).unwrap()["remote_meta"]
                .clone();
        let cur_hn = meta_src["hostname"].as_str().unwrap();
        let cur_bid = meta_src["boot_id"].as_str().unwrap();
        let cur_ls_out = Command::new("ps")
            .env("LC_ALL", "C")
            .args(["-p", &sleeper_pid.to_string(), "-o", "lstart="])
            .output()
            .unwrap();
        let cur_ls = String::from_utf8_lossy(&cur_ls_out.stdout)
            .trim()
            .to_owned();

        let reuse_cmd_sha = sha256(b"exec 'true'");
        let reuse_op_dir = state_dir.join("op-reuse");
        std::fs::create_dir_all(&reuse_op_dir).unwrap();
        std::fs::write(
            reuse_op_dir.join("meta.json"),
            serde_json::to_vec(&serde_json::json!({
                "hostname": cur_hn,
                "boot_id": cur_bid,
                "pid": sleeper_pid,
                "lstart": "Mon Jan  1 00:00:00 1990",
                "command_sha256": reuse_cmd_sha,
                "snapshot": ""
            }))
            .unwrap(),
        )
        .unwrap();
        write_remote_op(
            &repo,
            "op-reuse",
            &serde_json::json!({
                "schema": 1,
                "op_id": "op-reuse",
                "state": "unknown",
                "host": "example",
                "port": serde_json::Value::Null,
                "remote_dir": rd,
                "command_sha256": reuse_cmd_sha,
            }),
        )
        .unwrap();
        let fenced_cancel = invoke(
            &[
                "cancel", "--host", "example", "--repo", rs, "--op-id", "op-reuse",
            ],
            &root,
        );
        assert_eq!(fenced_cancel.code, 1);
        let vfc: serde_json::Value = serde_json::from_slice(&fenced_cancel.stdout).unwrap();
        assert_eq!(vfc["state"], "lost");
        assert_eq!(vfc["signal_sent"], false);
        assert!(sleeper.0.as_mut().unwrap().try_wait().unwrap().is_none());

        // 4. boot_id mismatch (reboot) marks lost on reconcile instead of running.
        if cur_bid != "unknown" {
            let reboot_dir = state_dir.join("op-reboot");
            std::fs::create_dir_all(&reboot_dir).unwrap();
            std::fs::write(
                reboot_dir.join("meta.json"),
                serde_json::to_vec(&serde_json::json!({
                    "hostname": cur_hn,
                    "boot_id": "00000000-0000-0000-0000-000000000000",
                    "pid": sleeper_pid,
                    "lstart": cur_ls,
                    "command_sha256": reuse_cmd_sha,
                    "snapshot": ""
                }))
                .unwrap(),
            )
            .unwrap();
            write_remote_op(
                &repo,
                "op-reboot",
                &serde_json::json!({
                    "schema": 1,
                    "op_id": "op-reboot",
                    "state": "unknown",
                    "host": "example",
                    "port": serde_json::Value::Null,
                    "remote_dir": rd,
                    "command_sha256": sha256(b"exec 'true'"),
                }),
            )
            .unwrap();
            let reboot_rec = invoke(
                &[
                    "reconcile",
                    "--host",
                    "example",
                    "--repo",
                    rs,
                    "--op-id",
                    "op-reboot",
                ],
                &root,
            );
            assert_eq!(reboot_rec.code, 1);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&reboot_rec.stdout).unwrap()["state"],
                "lost"
            );
        }
    }

    #[test]
    fn collect_enforces_paths_bounds_and_digest_atomically() {
        if std::env::var_os("OMS_TEST_SCENARIO8").is_none() {
            let temp = TempDir::new().unwrap();
            let fake = temp.0.join("ssh");
            let script = r#"#!/bin/sh
for last do :; done
if [ -f "$OMS_TEST_FAIL_STREAM" ]; then
  printf 'partial-chunk'
  exit 23
fi
case "$last" in
  *'exec dd '*)
    index=$(printf '%s\n' "$last" | sed -n 's/.* skip=\([0-9]*\) count=.*/\1/p')
    printf '%s\n' "$index" >> "$OMS_TEST_DIR/chunks.log"
    if [ -f "$OMS_TEST_DIR/resume_fail" ] && [ "$index" = 1 ]; then
      printf 'partial-chunk'
      exit 23
    fi
    if [ -f "$OMS_TEST_DIR/resume_hold" ] && [ "$index" = "$(cat "$OMS_TEST_DIR/resume_hold")" ]; then
      : > "$OMS_TEST_DIR/resume_ready"
      tries=0
      while [ -f "$OMS_TEST_DIR/resume_hold" ] && [ "$tries" -lt 100 ]; do
        sleep 0.05
        tries=$((tries + 1))
      done
    fi
    ;;
esac
exec sh -c "$last"
"#;
            std::fs::write(&fake, script).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ssh_adapter::tests::collect_enforces_paths_bounds_and_digest_atomically",
                    "--nocapture",
                ])
                .env("OMS_TEST_SCENARIO8", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_DIR", &temp.0)
                .env("OMS_LOCK_DIR", temp.0.join("locks"))
                .env("OMS_TEST_FAIL_STREAM", temp.0.join("fail_stream"))
                .env_remove("OMS_HARNESS_CHILD")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote");
        let dest_dir = root.join("dests");
        init_temp_git_repo(&repo);
        std::fs::create_dir_all(remote.join("outputs")).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();
        let payload = b"valid-payload-12345";
        let digest = sha256(payload);
        std::fs::write(remote.join("outputs/artifact.bin"), payload).unwrap();
        std::fs::write(remote.join("outputs/large.bin"), vec![b'x'; 4096]).unwrap();

        let existing = dest_dir.join("existing.bin");
        std::fs::write(&existing, b"untouched-original").unwrap();
        let link_dest = dest_dir.join("link.bin");
        std::os::unix::fs::symlink(&existing, &link_dest).unwrap();

        let rs = repo.to_str().unwrap();
        let rd = remote.to_str().unwrap();

        for bad_rp in ["../secret", "/etc/passwd", "~/secret"] {
            let r = invoke(
                &[
                    "collect",
                    "--host",
                    "example",
                    "--repo",
                    rs,
                    "--remote-dir",
                    rd,
                    "--remote-path",
                    bad_rp,
                    "--dest",
                    dest_dir.join("nope.bin").to_str().unwrap(),
                ],
                &root,
            );
            assert_eq!(r.code, 2, "{bad_rp}");
        }
        assert_eq!(
            invoke(
                &[
                    "collect",
                    "--host",
                    "example",
                    "--repo",
                    rs,
                    "--remote-dir",
                    rd,
                    "--remote-path",
                    "outputs/artifact.bin",
                    "--dest",
                    link_dest.to_str().unwrap(),
                ],
                &root,
            )
            .code,
            2
        );
        let overflow = dest_dir.join("overflow.bin");
        let over = invoke(
            &[
                "collect",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--remote-path",
                "outputs/large.bin",
                "--dest",
                overflow.to_str().unwrap(),
                "--max-bytes",
                "128",
            ],
            &root,
        );
        assert_eq!(over.code, 2);
        assert!(!overflow.exists());

        std::fs::write(root.join("fail_stream"), "1").unwrap();
        let partial = invoke(
            &[
                "collect",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--remote-path",
                "outputs/artifact.bin",
                "--dest",
                existing.to_str().unwrap(),
            ],
            &root,
        );
        assert_eq!(partial.code, 23);
        assert_eq!(std::fs::read(&existing).unwrap(), b"untouched-original");
        std::fs::remove_file(root.join("fail_stream")).unwrap();

        let wrong_hash = invoke(
            &[
                "collect",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--remote-path",
                "outputs/artifact.bin",
                "--dest",
                existing.to_str().unwrap(),
                "--expected-sha256",
                &"0".repeat(64),
            ],
            &root,
        );
        assert_eq!(wrong_hash.code, 2);
        assert_eq!(std::fs::read(&existing).unwrap(), b"untouched-original");

        let ok = invoke(
            &[
                "collect",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--remote-path",
                "outputs/artifact.bin",
                "--dest",
                existing.to_str().unwrap(),
                "--expected-sha256",
                &digest,
            ],
            &root,
        );
        assert_eq!(ok.code, 0, "{}", String::from_utf8_lossy(&ok.stderr));
        let v: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
        assert_eq!(v["verified"], true);
        assert_eq!(v["bytes"], payload.len() as u64);
        assert_eq!(v["sha256"], digest);
        assert_eq!(std::fs::read(&existing).unwrap(), payload);

        let leftovers: Vec<_> = std::fs::read_dir(&dest_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".oms-collect-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        collect_resumable_contract(&root, &repo, &remote, &dest_dir);
    }

    fn collect_resumable_contract(root: &Path, repo: &Path, remote: &Path, dest_dir: &Path) {
        let chunk = resumable::MIN_CHUNK_BYTES as usize;
        let payload: Vec<_> = (0..2 * chunk + 17).map(|n| (n % 251) as u8).collect();
        let digest = sha256(&payload);
        let source = remote.join("outputs/resume.bin");
        std::fs::write(&source, &payload).unwrap();
        let args = |dest: &Path, hash: &str, size: usize| -> Vec<OsString> {
            [
                "collect",
                "--host",
                "example",
                "--repo",
                repo.to_str().unwrap(),
                "--remote-dir",
                remote.to_str().unwrap(),
                "--remote-path",
                "outputs/resume.bin",
                "--dest",
                dest.to_str().unwrap(),
                "--resume",
                "--expected-sha256",
                hash,
                "--expected-size",
                &size.to_string(),
                "--chunk-bytes",
                &chunk.to_string(),
            ]
            .into_iter()
            .map(OsString::from)
            .collect()
        };
        let request = |argv: &[OsString]| run(argv, root, &AtomicBool::new(false));
        let log = || std::fs::read_to_string(root.join("chunks.log")).unwrap_or_default();
        let dest = dest_dir.join("resume.bin");
        std::fs::write(&dest, b"original").unwrap();
        let argv = args(&dest, &digest, payload.len());
        for name in ["--expected-size", "--expected-sha256", "--resume"] {
            let mut bad = argv.clone();
            let index = bad.iter().position(|a| a == name).unwrap();
            bad.remove(index);
            if name != "--resume" {
                bad.remove(index);
            }
            assert_eq!(request(&bad).code, 2, "{name}");
        }
        for (flag, value) in [("--chunk-bytes", "1"), ("--expected-size", "536870913")] {
            let mut bad = argv.clone();
            let index = bad.iter().position(|a| a == flag).unwrap();
            bad[index + 1] = value.into();
            assert_eq!(request(&bad).code, 2);
        }
        assert!(log().is_empty());
        assert_eq!(run(&argv, root, &AtomicBool::new(true)).code, 130);
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert!(log().is_empty());
        std::fs::write(root.join("resume_fail"), "1").unwrap();
        assert_eq!(request(&argv).code, 23);
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert_eq!(log(), "0\n1\n");
        let cache_parent = dest_dir.join(".oms-collect-cache");
        let cache = std::fs::read_dir(&cache_parent)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let manifest = cache.join("manifest.json");
        let saved = std::fs::read(&manifest).unwrap();
        let original: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(original["chunks"].as_object().unwrap().len(), 1);
        for field in ["schema", "transfer_id", "identity", "chunks", "extra"] {
            let mut corrupt = original.clone();
            match field {
                "schema" => corrupt[field] = 99.into(),
                "transfer_id" => corrupt[field] = "0".repeat(64).into(),
                "identity" => corrupt[field]["port"] = 23.into(),
                "chunks" => corrupt[field]["0"]["bytes"] = 1.into(),
                _ => corrupt[field] = true.into(),
            }
            std::fs::write(&manifest, serde_json::to_vec(&corrupt).unwrap()).unwrap();
            assert_eq!(request(&argv).code, 2, "{field}");
            assert_eq!(log(), "0\n1\n");
            assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        }
        std::fs::write(&manifest, b"{").unwrap();
        assert_eq!(request(&argv).code, 2);
        std::fs::write(&manifest, &saved).unwrap();
        let cached_chunk = cache.join("00000.chunk");
        let saved_chunk = std::fs::read(&cached_chunk).unwrap();
        let mut corrupt_chunk = saved_chunk.clone();
        corrupt_chunk[0] ^= 1;
        std::fs::write(&cached_chunk, &corrupt_chunk).unwrap();
        assert_eq!(request(&argv).code, 2);
        std::fs::write(&cached_chunk, b"short").unwrap();
        assert_eq!(request(&argv).code, 2);
        std::fs::remove_file(&cached_chunk).unwrap();
        std::os::unix::fs::symlink(&source, &cached_chunk).unwrap();
        assert_eq!(request(&argv).code, 2);
        std::fs::remove_file(&cached_chunk).unwrap();
        std::fs::write(&cached_chunk, &saved_chunk).unwrap();
        std::fs::set_permissions(&cached_chunk, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&cache_parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(request(&argv).code, 2);
        std::fs::set_permissions(&cache_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(log(), "0\n1\n");
        std::fs::remove_file(root.join("resume_fail")).unwrap();
        let complete = request(&argv);
        assert_eq!(
            complete.code,
            0,
            "{}",
            String::from_utf8_lossy(&complete.stderr)
        );
        let row: serde_json::Value = serde_json::from_slice(&complete.stdout).unwrap();
        assert_eq!(row["chunks_reused"], 1);
        assert_eq!(row["chunks_fetched"], 2);
        assert_eq!(row["bytes_reused"], chunk);
        assert_eq!(row["bytes_fetched"], chunk + 17);
        assert_eq!(row["verified"], true);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert_eq!(log(), "0\n1\n1\n2\n");

        // Effective port and physical destination spelling retain cache identity.
        let mut same = args(
            &dest_dir.join("../dests/resume.bin"),
            &digest,
            payload.len(),
        );
        same.extend([OsString::from("--port"), OsString::from("22")]);
        let cached = request(&same);
        assert_eq!(
            cached.code,
            0,
            "{}",
            String::from_utf8_lossy(&cached.stderr)
        );
        let cached: serde_json::Value = serde_json::from_slice(&cached.stdout).unwrap();
        assert_eq!(cached["transfer_id"], row["transfer_id"]);
        assert_eq!(cached["chunks_reused"], 3);
        assert_eq!(cached["chunks_fetched"], 0);
        assert_eq!(log(), "0\n1\n1\n2\n");

        let wrong = dest_dir.join("wrong-hash.bin");
        assert_eq!(
            request(&args(&wrong, &"0".repeat(64), payload.len())).code,
            2
        );
        assert!(!wrong.exists());
        for size in [payload.len() - 1, payload.len() + 1, 2 * chunk] {
            let wrong = dest_dir.join(format!("wrong-size-{size}.bin"));
            assert_eq!(request(&args(&wrong, &digest, size)).code, 2);
            assert!(!wrong.exists());
        }
        // Remote mutation between attempts is detected by the full caller hash.
        let changed = dest_dir.join("changed.bin");
        let changed_args = args(&changed, &digest, payload.len());
        std::fs::write(root.join("resume_fail"), "1").unwrap();
        assert_eq!(request(&changed_args).code, 23);
        std::fs::remove_file(root.join("resume_fail")).unwrap();
        let mut changed_payload = payload.clone();
        changed_payload[chunk] ^= 1;
        std::fs::write(&source, changed_payload).unwrap();
        assert_eq!(request(&changed_args).code, 2);
        assert!(!changed.exists());
        std::fs::write(&source, &payload).unwrap();

        let cancelled_dest = dest_dir.join("cancelled.bin");
        let cancelled_args = args(&cancelled_dest, &digest, payload.len());
        let interrupted = AtomicBool::new(false);
        let before_cancel = log();
        std::fs::write(root.join("resume_hold"), "1").unwrap();
        std::thread::scope(|scope| {
            let pending = scope.spawn(|| run(&cancelled_args, root, &interrupted));
            let started = std::time::Instant::now();
            while !root.join("resume_ready").exists() && started.elapsed() < Duration::from_secs(3)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(root.join("resume_ready").exists());
            interrupted.store(true, Ordering::Relaxed);
            assert_eq!(pending.join().unwrap().code, 130);
        });
        assert!(!cancelled_dest.exists());
        assert_eq!(log(), format!("{before_cancel}0\n1\n"));
        std::fs::remove_file(root.join("resume_hold")).unwrap();
        std::fs::remove_file(root.join("resume_ready")).unwrap();
        let resumed = request(&cancelled_args);
        assert_eq!(
            resumed.code,
            0,
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
        assert_eq!(resumed["chunks_reused"], 1);
        assert_eq!(log(), format!("{before_cancel}0\n1\n1\n2\n"));

        // A live resumable writer excludes both another transfer and legacy collect.
        let exclusive = dest_dir.join("exclusive.bin");
        let exclusive_args = args(&exclusive, &digest, payload.len());
        std::fs::write(root.join("resume_hold"), "0").unwrap();
        std::thread::scope(|scope| {
            let pending = scope.spawn(|| request(&exclusive_args));
            let started = std::time::Instant::now();
            while !root.join("resume_ready").exists() && started.elapsed() < Duration::from_secs(3)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(root.join("resume_ready").exists());
            let before = log();
            let other = args(
                &dest_dir.join("../dests/exclusive.bin"),
                &"0".repeat(64),
                payload.len(),
            );
            assert_eq!(request(&other).code, 75);
            let legacy = invoke(
                &[
                    "collect",
                    "--host",
                    "example",
                    "--repo",
                    repo.to_str().unwrap(),
                    "--remote-dir",
                    remote.to_str().unwrap(),
                    "--remote-path",
                    "outputs/resume.bin",
                    "--dest",
                    exclusive.to_str().unwrap(),
                ],
                root,
            );
            assert_eq!(legacy.code, 75);
            assert_eq!(log(), before);
            assert!(!exclusive.exists());
            std::fs::remove_file(root.join("resume_hold")).unwrap();
            assert_eq!(pending.join().unwrap().code, 0);
        });
        assert_eq!(std::fs::read(&exclusive).unwrap(), payload);
        std::fs::write(&source, []).unwrap();
        let empty = dest_dir.join("empty.bin");
        assert_eq!(request(&args(&empty, &sha256(b""), 0)).code, 0);
        assert_eq!(std::fs::read(&empty).unwrap(), b"");
        assert!(std::fs::read_dir(dest_dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn omitted_target_blocks_unapproved_host_and_snapshot_binds_remote_head() {
        if std::env::var_os("OMS_TEST_TARGET_SNAPSHOT").is_none() {
            let temp = TempDir::new().unwrap();
            let fake = temp.0.join("ssh");
            let args_log = temp.0.join("args");
            std::fs::write(
                &fake,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$OMS_TEST_ARGS\"\nfor last do :; done\nexec sh -c \"$last\"\n",
            )
            .unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ssh_adapter::tests::omitted_target_blocks_unapproved_host_and_snapshot_binds_remote_head",
                    "--nocapture",
                ])
                .env("OMS_TEST_TARGET_SNAPSHOT", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_DIR", &temp.0)
                .env("OMS_TEST_ARGS", &args_log)
                .env_remove("OMS_HARNESS_CHILD")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        use crate::value::*;
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote");
        init_temp_git_repo(&repo);
        init_temp_git_repo(&remote);
        std::fs::create_dir_all(repo.join(".oms/runtime/snapshots")).unwrap();
        let identity = object([
            ("ssh_alias", string("cluster-a")),
            ("workdir", string(remote.to_str().unwrap())),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
        ]);
        let fp = sha(canonical(&identity).unwrap());
        let mut row = identity;
        row.set("execution_approved", b(false)).unwrap();
        row.set("approved_fingerprint", string(&fp)).unwrap();
        let reg = object([
            ("schema", number(1)),
            ("targets", object([("gpu", row.clone())])),
        ]);
        std::fs::write(
            repo.join(".oms/runtime/targets.json"),
            canonical(&reg).unwrap(),
        )
        .unwrap();

        let rs = repo.to_str().unwrap();
        let rd = remote.to_str().unwrap();
        // Omitting --target still blocks unapproved registered host (including user@alias).
        for h in ["cluster-a", "user@cluster-a"] {
            let omitted = invoke(
                &[
                    "exec",
                    "--host",
                    h,
                    "--repo",
                    rs,
                    "--remote-dir",
                    rd,
                    "--",
                    "true",
                ],
                &root,
            );
            assert_eq!(omitted.code, 2, "host {h} was not blocked");
        }
        assert!(!root.join("args").exists());

        row.set("execution_approved", b(true)).unwrap();
        let reg = object([("schema", number(1)), ("targets", object([("gpu", row)]))]);
        std::fs::write(
            repo.join(".oms/runtime/targets.json"),
            canonical(&reg).unwrap(),
        )
        .unwrap();

        // Approved host still refuses unregistered --remote-dir when --target is omitted.
        let wrong_dir = invoke(
            &[
                "exec",
                "--host",
                "cluster-a",
                "--repo",
                rs,
                "--remote-dir",
                root.join("other-remote").to_str().unwrap(),
                "--",
                "true",
            ],
            &root,
        );
        assert_eq!(wrong_dir.code, 2);
        assert!(!root.join("args").exists());

        std::fs::write(repo.join(".gitignore"), "ignored.lock\n").unwrap();
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", ".gitignore"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-qm", "gitignore"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(repo.join("ignored.lock"), "v1\n").unwrap();
        let lf_sha = sha(b"v1\n");
        let head = String::from_utf8(git(&repo, &["rev-parse", "HEAD"]).unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        let tree = String::from_utf8(git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        let mut snap = object([
            ("schema", number(1)),
            ("head_sha", string(&head)),
            ("tree_sha", string(&tree)),
            ("lockfiles", object([("ignored.lock", string(&lf_sha))])),
        ]);
        let snap_digest = sha(canonical(&snap).unwrap());
        snap.set("snapshot_sha256", string(&snap_digest)).unwrap();
        std::fs::write(
            repo.join(format!(".oms/runtime/snapshots/{snap_digest}.json")),
            canonical(&snap).unwrap(),
        )
        .unwrap();

        // Sync repo commit to remote so local+remote HEAD match snapshot -> fails until ignored.lock matches.
        let synced = invoke(
            &[
                "sync-bundle",
                "--host",
                "cluster-a",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--target",
                "gpu",
            ],
            &root,
        );
        assert_eq!(synced.code, 0);
        std::fs::write(remote.join("ignored.lock"), "tampered\n").unwrap();
        let lf_drift = invoke(
            &[
                "exec",
                "--host",
                "cluster-a",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--target",
                "gpu",
                "--snapshot",
                &snap_digest,
                "--",
                "printf",
                "should-not-run",
            ],
            &root,
        );
        assert_eq!(lf_drift.code, 2);
        let lf_drift_op = invoke(
            &[
                "exec",
                "--host",
                "cluster-a",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--target",
                "gpu",
                "--snapshot",
                &snap_digest,
                "--op-id",
                "op-lf-drift",
                "--",
                "printf",
                "should-not-run",
            ],
            &root,
        );
        assert_eq!(lf_drift_op.code, 2);

        std::fs::write(remote.join("ignored.lock"), "v1\n").unwrap();
        let ok = invoke(
            &[
                "exec",
                "--host",
                "cluster-a",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--target",
                "gpu",
                "--snapshot",
                &snap_digest,
                "--",
                "printf",
                "ok",
            ],
            &root,
        );
        assert_eq!(ok.code, 0, "{}", String::from_utf8_lossy(&ok.stderr));

        // Remote checkout advances away from snapshot -> refused with exit 2 before running payload.
        std::fs::write(remote.join("file"), "drifted\n").unwrap();
        assert!(
            Command::new("git")
                .current_dir(&remote)
                .args(["commit", "-qam", "drift"])
                .status()
                .unwrap()
                .success()
        );
        let drifted = invoke(
            &[
                "exec",
                "--host",
                "cluster-a",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--target",
                "gpu",
                "--snapshot",
                &snap_digest,
                "--",
                "sh",
                "-c",
                "printf ran > should-not-exist",
            ],
            &root,
        );
        assert_eq!(drifted.code, 2);
        assert!(!remote.join("should-not-exist").exists());
    }

    #[test]
    fn cancel_escalates_sigkill_and_blocks_spoofed_frames_and_unverifiable_lstart() {
        fn pid_live(pid: impl ToString) -> bool {
            let pid = pid.to_string();
            if !Command::new("kill")
                .args(["-0", &pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
            {
                return false;
            }
            // Orphans can await a different subreaper. Match the remote
            // liveness contract: zombies have exited, regardless of kill -0.
            let process = Command::new("ps")
                .args(["-p", &pid, "-o", "stat="])
                .output()
                .unwrap();
            !process.status.success()
                || !String::from_utf8_lossy(&process.stdout)
                    .trim()
                    .starts_with('Z')
        }
        if std::env::var_os("OMS_TEST_CANCEL_HARDENING").is_none() {
            let temp = TempDir::new().unwrap();
            let state_dir = temp.0.join("remote_state");
            let fake = temp.0.join("ssh");
            let script = r#"#!/bin/sh
for last do :; done
if [ -f "$OMS_TEST_DROP_ACTIVE" ]; then
  target=$(cat "$OMS_TEST_DROP_ACTIVE" 2>/dev/null || true)
  rm -f "$OMS_TEST_DROP_ACTIVE"
  [ "$target" = "1" ] && target="$OMS_REMOTE_STATE_DIR/op-sigign/child.pid"
  sh -c "$last" >/dev/null 2>&1 &
  i=0
  while [ ! -f "$target" ] && [ "$i" -lt 250 ]; do
    i=$((i + 1))
    sleep 0.02 2>/dev/null || sleep 1
  done
  exit 255
fi
exec sh -c "$last"
"#;
            std::fs::write(&fake, script).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ssh_adapter::tests::cancel_escalates_sigkill_and_blocks_spoofed_frames_and_unverifiable_lstart",
                    "--nocapture",
                ])
                .env("OMS_TEST_CANCEL_HARDENING", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_DIR", &temp.0)
                .env("OMS_TEST_DROP_ACTIVE", temp.0.join("drop_active"))
                .env("OMS_REMOTE_STATE_DIR", &state_dir)
                .env("OMS_LOCK_DIR", temp.0.join("locks"))
                .env_remove("OMS_HARNESS_CHILD")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let state_dir = PathBuf::from(std::env::var_os("OMS_REMOTE_STATE_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote");
        init_temp_git_repo(&repo);
        std::fs::create_dir_all(&remote).unwrap();
        let rs = repo.to_str().unwrap();
        let rd = remote.to_str().unwrap();

        // 1. Workload ignoring SIGTERM is escalated to SIGKILL and actually terminated.
        std::fs::write(root.join("drop_active"), "1").unwrap();
        let launched = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-sigign",
                "--",
                "sh",
                "-c",
                "trap '' TERM; printf '__OMS_PART__ result 15\\n{\"exit_code\":0}\\n'; sleep 30",
            ],
            &root,
        );
        assert_eq!(launched.code, 75);
        // Un-nonced forged frame printed to stdout never causes reconcile to mark succeeded.
        let rec_running = invoke(
            &[
                "reconcile",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-sigign",
            ],
            &root,
        );
        assert_eq!(rec_running.code, 75);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&rec_running.stdout).unwrap()["state"],
            "running"
        );
        let child_pid: i32 = std::fs::read_to_string(state_dir.join("op-sigign/child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let canceled = invoke(
            &[
                "cancel",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-sigign",
                "--grace-seconds",
                "1",
            ],
            &root,
        );
        assert_eq!(
            canceled.code,
            0,
            "{}",
            String::from_utf8_lossy(&canceled.stderr)
        );
        let vc: serde_json::Value = serde_json::from_slice(&canceled.stdout).unwrap();
        assert_eq!(vc["state"], "cancelled");
        assert_eq!(vc["signal"], "SIGKILL");
        assert!(
            !pid_live(child_pid),
            "workload ignoring SIGTERM survived cancel"
        );

        // 2. Empty lstart on a live process causes cancel to hold unknown (75), not lost.
        let mut sleeper = ChildKillGuard(Some(Command::new("sleep").arg("20").spawn().unwrap()));
        let sleeper_pid = sleeper.0.as_ref().unwrap().id();
        assert!(pid_live(sleeper_pid));
        let meta_src: serde_json::Value = serde_json::from_slice::<serde_json::Value>(
            &rec_running.stdout,
        )
        .unwrap()["remote_meta"]
            .clone();
        let cur_hn = meta_src["hostname"].as_str().unwrap();
        let cur_bid = meta_src["boot_id"].as_str().unwrap();
        let empty_ls_sha = sha256(b"exec 'true'");
        std::fs::create_dir_all(state_dir.join("op-empty-ls")).unwrap();
        std::fs::write(
            state_dir.join("op-empty-ls/meta.json"),
            serde_json::to_vec(&serde_json::json!({
                "hostname": cur_hn,
                "boot_id": cur_bid,
                "pid": sleeper_pid,
                "lstart": "",
                "command_sha256": empty_ls_sha,
                "snapshot": ""
            }))
            .unwrap(),
        )
        .unwrap();
        write_remote_op(
            &repo,
            "op-empty-ls",
            &serde_json::json!({
                "schema": 1,
                "op_id": "op-empty-ls",
                "state": "unknown",
                "host": "example",
                "port": serde_json::Value::Null,
                "remote_dir": rd,
                "command_sha256": empty_ls_sha,
            }),
        )
        .unwrap();
        let unverifiable = invoke(
            &[
                "cancel",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-empty-ls",
            ],
            &root,
        );
        assert_eq!(unverifiable.code, 75);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&unverifiable.stdout).unwrap()["state"],
            "unknown"
        );
        assert!(sleeper.0.as_mut().unwrap().try_wait().unwrap().is_none());

        // 3. Reconcile rejects remote meta.json bound to a different command_sha256.
        std::fs::write(
            state_dir.join("op-empty-ls/meta.json"),
            serde_json::to_vec(&serde_json::json!({
                "hostname": cur_hn,
                "boot_id": cur_bid,
                "pid": sleeper_pid,
                "lstart": "",
                "command_sha256": sha256(b"exec 'false'"),
                "snapshot": ""
            }))
            .unwrap(),
        )
        .unwrap();
        let mismatch = invoke(
            &[
                "reconcile",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-empty-ls",
            ],
            &root,
        );
        assert_eq!(mismatch.code, 2);

        // 4. Cancel checks remote meta identity BEFORE creating cancel.req or signaling sleeper.
        let sleeper_ls = String::from_utf8(
            Command::new("ps")
                .args(["-p", &sleeper_pid.to_string(), "-o", "lstart="])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let sleeper_ls: String = sleeper_ls
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || "._: +=,-".contains(*c))
            .collect();
        std::fs::write(
            state_dir.join("op-empty-ls/meta.json"),
            serde_json::to_vec(&serde_json::json!({
                "hostname": cur_hn,
                "boot_id": cur_bid,
                "pid": sleeper_pid,
                "lstart": sleeper_ls,
                "command_sha256": sha256(b"exec 'false'"),
                "snapshot": ""
            }))
            .unwrap(),
        )
        .unwrap();
        let pre_cancel = invoke(
            &[
                "cancel",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-empty-ls",
            ],
            &root,
        );
        assert_eq!(pre_cancel.code, 2);
        assert!(sleeper.0.as_mut().unwrap().try_wait().unwrap().is_none());
        assert!(!state_dir.join("op-empty-ls/cancel.req").exists());
        let op_rec = read_remote_op(&repo, "op-empty-ls").unwrap().unwrap();
        assert_eq!(op_rec["state"], "unknown");
        assert_eq!(op_rec["signal_sent"], serde_json::json!(false));

        // 5. Orphaned grandchild in child.pgid surviving parent shell exit is killed on cancel.
        std::fs::write(
            root.join("drop_active"),
            remote.join("grand.ready").to_str().unwrap(),
        )
        .unwrap();
        let launch_grand = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-grand",
                "--",
                "sh",
                "-c",
                "trap 'exit 0' TERM; (trap '' TERM; exec sleep 30) </dev/null >/dev/null 2>&1 & printf '%s\\n' \"$!\" > grand.pid; : > grand.ready; wait",
            ],
            &root,
        );
        assert_eq!(launch_grand.code, 75);
        assert!(state_dir.join("op-grand/child.pgid").exists());
        let grand_pid: i32 = std::fs::read_to_string(remote.join("grand.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let canceled_grand = invoke(
            &[
                "cancel",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-grand",
                "--grace-seconds",
                "1",
            ],
            &root,
        );
        assert_eq!(
            canceled_grand.code,
            0,
            "{}",
            String::from_utf8_lossy(&canceled_grand.stderr)
        );
        let vg: serde_json::Value = serde_json::from_slice(&canceled_grand.stdout).unwrap();
        assert_eq!(vg["state"], "cancelled");
        assert_eq!(vg["signal"], "SIGKILL");
        let mut grand_alive = true;
        for _ in 0..50 {
            grand_alive = pid_live(grand_pid);
            if !grand_alive {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if grand_alive {
            let process = Command::new("ps")
                .args([
                    "-p",
                    &grand_pid.to_string(),
                    "-o",
                    "pid=,ppid=,pgid=,stat=,lstart=,comm=",
                ])
                .output()
                .unwrap();
            panic!(
                "orphaned grandchild {grand_pid} survived cancel: {}",
                String::from_utf8_lossy(&process.stdout)
            );
        }

        // 6a. When runner.sh exits early while child leader lives (no result.json),
        //     reconcile keeps state=running via child.lstart and cancel reaps the group.
        std::fs::write(
            root.join("drop_active"),
            remote.join("dead.ready").to_str().unwrap(),
        )
        .unwrap();
        let launch_dead = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-runner-dead",
                "--",
                "sh",
                "-c",
                "(trap '' TERM; exec sleep 30) </dev/null >/dev/null 2>&1 & printf '%s\\n' \"$!\" > dead.pid; : > dead.ready; exec sleep 30",
            ],
            &root,
        );
        assert_eq!(launch_dead.code, 75);
        assert!(state_dir.join("op-runner-dead/child.pgid").exists());
        assert!(state_dir.join("op-runner-dead/child.lstart").exists());
        let dead_meta: serde_json::Value = serde_json::from_slice(
            &std::fs::read(state_dir.join("op-runner-dead/meta.json")).unwrap(),
        )
        .unwrap();
        let dead_runner_pid = dead_meta["pid"].as_i64().unwrap();
        let dead_cpid: i32 = std::fs::read_to_string(state_dir.join("op-runner-dead/child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let dead_grand_pid: i32 = std::fs::read_to_string(remote.join("dead.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let _ = Command::new("kill")
            .args(["-KILL", &dead_runner_pid.to_string()])
            .status();
        for _ in 0..50 {
            let r_alive = pid_live(dead_runner_pid);
            if !r_alive {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!state_dir.join("op-runner-dead/result.json").exists());
        let rec_dead = invoke(
            &[
                "reconcile",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-runner-dead",
            ],
            &root,
        );
        assert_eq!(
            rec_dead.code,
            75,
            "{}",
            String::from_utf8_lossy(&rec_dead.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&rec_dead.stdout).unwrap()["state"],
            "running"
        );
        let canceled_dead = invoke(
            &[
                "cancel",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-runner-dead",
                "--grace-seconds",
                "1",
            ],
            &root,
        );
        assert_eq!(
            canceled_dead.code,
            0,
            "{}",
            String::from_utf8_lossy(&canceled_dead.stderr)
        );
        let vd: serde_json::Value = serde_json::from_slice(&canceled_dead.stdout).unwrap();
        assert_eq!(vd["state"], "cancelled");
        assert_eq!(vd["signal"], "SIGKILL");
        for pid in [dead_cpid, dead_grand_pid] {
            let mut alive = true;
            for _ in 0..50 {
                alive = pid_live(pid);
                if !alive {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(!alive, "pid {pid} survived dead-runner cancel");
        }

        // 6b-bis. When boot_id is "unknown" while orphan group lives, reconcile must return
        //         state=unknown (75) rather than prematurely finalizing state=lost (1).
        std::fs::write(
            root.join("drop_active"),
            remote.join("unkbid.ready").to_str().unwrap(),
        )
        .unwrap();
        let launch_unk = invoke(
            &[
                "exec",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--op-id",
                "op-unknown-boot",
                "--",
                "sh",
                "-c",
                ": > unkbid.ready; exec sleep 30",
            ],
            &root,
        );
        assert_eq!(launch_unk.code, 75);
        let mut unk_meta: serde_json::Value = serde_json::from_slice(
            &std::fs::read(state_dir.join("op-unknown-boot/meta.json")).unwrap(),
        )
        .unwrap();
        let unk_runner_pid = unk_meta["pid"].as_i64().unwrap();
        let unk_cpid: i32 = std::fs::read_to_string(state_dir.join("op-unknown-boot/child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        unk_meta["boot_id"] = serde_json::Value::String("unknown".into());
        std::fs::write(
            state_dir.join("op-unknown-boot/meta.json"),
            format!("{}\n", serde_json::to_string(&unk_meta).unwrap()),
        )
        .unwrap();
        let _ = Command::new("kill")
            .args(["-KILL", &unk_runner_pid.to_string()])
            .status();
        for _ in 0..50 {
            let r_alive = pid_live(unk_runner_pid);
            if !r_alive {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let rec_unk = invoke(
            &[
                "reconcile",
                "--host",
                "example",
                "--repo",
                rs,
                "--op-id",
                "op-unknown-boot",
            ],
            &root,
        );
        assert_eq!(rec_unk.code, 75);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&rec_unk.stdout).unwrap()["state"],
            "unknown"
        );
        let _ = Command::new("kill")
            .args(["-KILL", &unk_cpid.to_string()])
            .status();

        // 6b & 7. On Linux (/proc available):
        //   - When both runner.sh and child leader die, OMS_REMOTE_OP_TAG proves PGID ownership
        //     across reconcile (running) -> cancel (cancelled).
        //   - An unrelated PGID whose leader exited and lacks OMS_REMOTE_OP_TAG is NOT signaled.
        #[cfg(target_os = "linux")]
        {
            std::fs::write(
                root.join("drop_active"),
                remote.join("dead2.ready").to_str().unwrap(),
            )
            .unwrap();
            let launch_dead2 = invoke(
                &[
                    "exec",
                    "--host",
                    "example",
                    "--repo",
                    rs,
                    "--remote-dir",
                    rd,
                    "--op-id",
                    "op-runner-leader-dead",
                    "--",
                    "sh",
                    "-c",
                    "(trap '' TERM; exec sleep 30) </dev/null >/dev/null 2>&1 & printf '%s\\n' \"$!\" > dead2.pid; : > dead2.ready; exec sleep 30",
                ],
                &root,
            );
            assert_eq!(launch_dead2.code, 75);
            let dead2_meta: serde_json::Value = serde_json::from_slice(
                &std::fs::read(state_dir.join("op-runner-leader-dead/meta.json")).unwrap(),
            )
            .unwrap();
            let dead2_runner_pid = dead2_meta["pid"].as_i64().unwrap();
            let dead2_cpid: i32 =
                std::fs::read_to_string(state_dir.join("op-runner-leader-dead/child.pid"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
            let dead2_grand_pid: i32 = std::fs::read_to_string(remote.join("dead2.pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let _ = Command::new("kill")
                .args([
                    "-KILL",
                    &dead2_runner_pid.to_string(),
                    &dead2_cpid.to_string(),
                ])
                .status();
            for _ in 0..50 {
                let r_alive = pid_live(dead2_runner_pid);
                let c_alive = pid_live(dead2_cpid);
                if !r_alive && !c_alive {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let rec_dead2 = invoke(
                &[
                    "reconcile",
                    "--host",
                    "example",
                    "--repo",
                    rs,
                    "--op-id",
                    "op-runner-leader-dead",
                ],
                &root,
            );
            assert_eq!(rec_dead2.code, 75);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&rec_dead2.stdout).unwrap()["state"],
                "running"
            );
            let cancel_dead2 = invoke(
                &[
                    "cancel",
                    "--host",
                    "example",
                    "--repo",
                    rs,
                    "--op-id",
                    "op-runner-leader-dead",
                    "--grace-seconds",
                    "1",
                ],
                &root,
            );
            assert_eq!(cancel_dead2.code, 0);
            let vd2: serde_json::Value = serde_json::from_slice(&cancel_dead2.stdout).unwrap();
            assert_eq!(vd2["state"], "cancelled");
            assert_eq!(vd2["signal"], "SIGKILL");
            let mut dead2_grand_alive = true;
            for _ in 0..50 {
                dead2_grand_alive = pid_live(dead2_grand_pid);
                if !dead2_grand_alive {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(
                !dead2_grand_alive,
                "grandchild {dead2_grand_pid} survived dead-runner+leader cancel"
            );

            // Section 7: Reused PGID without OMS_REMOTE_OP_TAG must fail closed (lost, signal_sent=false).
            let unrel_pid_file = remote.join("unrel.pid");
            let unrel_pgid_file = remote.join("unrel.pgid");
            let unrel_ready_file = remote.join("unrel.ready");
            assert!(
                Command::new("setsid")
                    .args([
                        "sh",
                        "-c",
                        "(exec sleep 30) </dev/null >/dev/null 2>&1 & printf '%s\\n' \"$!\" > \"$1\"; printf '%s\\n' \"$$\" > \"$2\"; : > \"$3\"; exit 0",
                        "sh",
                        unrel_pid_file.to_str().unwrap(),
                        unrel_pgid_file.to_str().unwrap(),
                        unrel_ready_file.to_str().unwrap(),
                    ])
                    .env_remove("OMS_REMOTE_OP_TAG")
                    .status()
                    .unwrap()
                    .success()
            );
            for _ in 0..50 {
                if unrel_ready_file.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let unrel_pid: i32 = std::fs::read_to_string(&unrel_pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let unrel_pgid: i32 = std::fs::read_to_string(&unrel_pgid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            for _ in 0..50 {
                let leader_alive = Command::new("kill")
                    .args(["-0", &unrel_pgid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success();
                if !leader_alive {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            std::fs::write(
                root.join("drop_active"),
                remote.join("reused.ready").to_str().unwrap(),
            )
            .unwrap();
            let launch_reused = invoke(
                &[
                    "exec",
                    "--host",
                    "example",
                    "--repo",
                    rs,
                    "--remote-dir",
                    rd,
                    "--op-id",
                    "op-reused-pgid",
                    "--",
                    "sh",
                    "-c",
                    ": > reused.ready; exec sleep 30",
                ],
                &root,
            );
            assert_eq!(launch_reused.code, 75);
            let reused_meta: serde_json::Value = serde_json::from_slice(
                &std::fs::read(state_dir.join("op-reused-pgid/meta.json")).unwrap(),
            )
            .unwrap();
            let reused_runner = reused_meta["pid"].as_i64().unwrap();
            let reused_cpid: i32 =
                std::fs::read_to_string(state_dir.join("op-reused-pgid/child.pid"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
            let _ = Command::new("kill")
                .args([
                    "-KILL",
                    &reused_runner.to_string(),
                    &reused_cpid.to_string(),
                ])
                .status();
            for _ in 0..50 {
                let r_alive = Command::new("kill")
                    .args(["-0", &reused_runner.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success();
                let c_alive = Command::new("kill")
                    .args(["-0", &reused_cpid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success();
                if !r_alive && !c_alive {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            std::fs::write(
                state_dir.join("op-reused-pgid/child.pgid"),
                format!("{unrel_pgid}\n"),
            )
            .unwrap();
            let cancel_reused = invoke(
                &[
                    "cancel",
                    "--host",
                    "example",
                    "--repo",
                    rs,
                    "--op-id",
                    "op-reused-pgid",
                    "--grace-seconds",
                    "1",
                ],
                &root,
            );
            assert_eq!(cancel_reused.code, 1);
            let vr: serde_json::Value = serde_json::from_slice(&cancel_reused.stdout).unwrap();
            assert_eq!(vr["state"], "lost");
            assert_eq!(vr["signal_sent"], false);
            let unrel_still_alive = Command::new("kill")
                .args(["-0", &unrel_pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            let _ = Command::new("kill")
                .args(["-KILL", &unrel_pid.to_string()])
                .status();
            assert!(
                unrel_still_alive,
                "unrelated PGID child {unrel_pid} was improperly signaled on PGID reuse"
            );
        }
    }

    #[test]
    fn collect_rejects_symlink_ancestors_and_overflow_with_open_stderr() {
        if std::env::var_os("OMS_TEST_COLLECT_HARDENING").is_none() {
            let temp = TempDir::new().unwrap();
            let fake = temp.0.join("ssh");
            let script = r#"#!/bin/sh
for last do :; done
if [ -f "$OMS_TEST_OPEN_STDERR_FLOOD" ]; then
  i=0
  while [ "$i" -lt 8192 ]; do
    printf '0123456789abcdef0123456789abcdef'
    i=$((i + 1))
  done
  exit 0
fi
exec sh -c "$last"
"#;
            std::fs::write(&fake, script).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ssh_adapter::tests::collect_rejects_symlink_ancestors_and_overflow_with_open_stderr",
                    "--nocapture",
                ])
                .env("OMS_TEST_COLLECT_HARDENING", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_DIR", &temp.0)
                .env("OMS_LOCK_DIR", temp.0.join("locks"))
                .env("OMS_TEST_OPEN_STDERR_FLOOD", temp.0.join("flood"))
                .env_remove("OMS_HARNESS_CHILD")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote");
        let outside = root.join("outside");
        let dest_dir = root.join("dest");
        init_temp_git_repo(&repo);
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();
        std::fs::write(outside.join("secret.txt"), "top-secret\n").unwrap();
        std::os::unix::fs::symlink(&outside, remote.join("sym_sub")).unwrap();

        let rs = repo.to_str().unwrap();
        let rd = remote.to_str().unwrap();
        let symlink_ancestor = invoke(
            &[
                "collect",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--remote-path",
                "sym_sub/secret.txt",
                "--dest",
                dest_dir.join("out.txt").to_str().unwrap(),
            ],
            &root,
        );
        assert_eq!(symlink_ancestor.code, 2);
        assert!(!dest_dir.join("out.txt").exists());

        std::fs::write(root.join("flood"), "1").unwrap();
        let started = std::time::Instant::now();
        let flood_over = invoke(
            &[
                "collect",
                "--host",
                "example",
                "--repo",
                rs,
                "--remote-dir",
                rd,
                "--remote-path",
                "sym_sub/secret.txt",
                "--dest",
                dest_dir.join("flood.txt").to_str().unwrap(),
                "--max-bytes",
                "128",
            ],
            &root,
        );
        assert_eq!(flood_over.code, 2);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(!dest_dir.join("flood.txt").exists());
    }
}
