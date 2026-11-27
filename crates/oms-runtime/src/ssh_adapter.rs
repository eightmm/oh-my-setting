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
use std::time::{Duration, Instant, SystemTime};

mod resumable;
mod stream;

const USAGE: &str = "usage: oms runtime ssh-adapter <probe|sync-bundle|exec|reconcile|cancel|collect> [options] [-- command...]\nprobe, exec, reconcile, cancel: [--timeout-seconds N] bounds the complete local SSH observer, including connected waits; a timeout preserves remote outcome as unknown. Defaults: exec 300s, others 30s.\ncollect: --host HOST --repo REPO --remote-dir DIR --remote-path PATH --dest PATH [--expected-sha256 HEX] [--max-bytes N] [--timeout-seconds N] [--resume --expected-size N [--chunk-bytes N]]";
const DEFAULT_COLLECT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COLLECT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LEDGER_OUTPUT: usize = 512 * 1024;
const MAX_STDERR_BYTES: u64 = 64 * 1024;
const MAX_PRIVATE_LOG_BYTES: usize = 64 * 1024;

pub const INTERACTIVE_AUTH_UNSUPPORTED: &str =
    "interactive authentication required; this target is not supported";
pub const RECONNECT_STATE_FILE: &str = "ssh-connections.json";
pub const PRIVATE_CONTROL_LOG_FILE: &str = "ssh.log";
pub const MAX_NEW_CONNECTIONS_PER_HOUR: usize = 12;
pub const RECONNECT_WINDOW_MS: u64 = 3_600_000;
pub const INITIAL_BACKOFF_MS: u64 = 5_000;
pub const MAX_BACKOFF_MS: u64 = 600_000;

pub trait SshClock: Send + Sync {
    fn now_ms(&self) -> u64;
    /// Jitter ratio in `0..=1000`.
    fn jitter_permille(&self, consecutive_failures: u32) -> u64;
}

pub struct SystemSshClock;

impl SshClock for SystemSshClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    fn jitter_permille(&self, consecutive_failures: u32) -> u64 {
        let mut buf = [0u8; 8];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom")
            && f.read_exact(&mut buf).is_ok()
        {
            return u64::from_le_bytes(buf) % 1001;
        }
        let mut seed = Vec::with_capacity(24);
        seed.extend_from_slice(&self.now_ms().to_le_bytes());
        seed.extend_from_slice(&std::process::id().to_le_bytes());
        seed.extend_from_slice(&consecutive_failures.to_le_bytes());
        seed.extend_from_slice(
            &FRAME_NONCE_COUNTER
                .fetch_add(1, Ordering::Relaxed)
                .to_le_bytes(),
        );
        let hex = sha256(&seed);
        u64::from_str_radix(&hex[..8], 16).unwrap_or(500) % 1001
    }
}

pub fn ssh_connection_key(host: &str, port: Option<u16>) -> String {
    format!("{host}:{}", port.unwrap_or(22))
}

fn provision_private_user_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let uid = rustix::process::geteuid().as_raw();
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(error(e)),
    }
    let meta = std::fs::symlink_metadata(dir).map_err(error)?;
    require(
        meta.is_dir() && !meta.file_type().is_symlink() && meta.uid() == uid,
        format!("unsafe non-private SSH directory: {}", dir.display()),
    )?;
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(error)?;
        let refreshed = std::fs::symlink_metadata(dir).map_err(error)?;
        require(
            refreshed.is_dir()
                && !refreshed.file_type().is_symlink()
                && refreshed.uid() == uid
                && refreshed.permissions().mode() & 0o077 == 0,
            format!("cannot enforce 0700 on SSH directory: {}", dir.display()),
        )?;
    }
    Ok(())
}

pub fn ssh_control_path(host: &str, port: Option<u16>) -> Result<String> {
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
        if provision_private_user_dir(&dir).is_err() {
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

/// Private 0700 host-local state directory next to the control socket directory.
pub fn ssh_state_dir(control_path: &str) -> Result<PathBuf> {
    let uid = rustix::process::geteuid().as_raw();
    let control_dir = Path::new(control_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| error("invalid SSH control path"))?;
    let parent = control_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| error("invalid SSH control directory"))?;
    let name = control_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("oms-ssh");
    let state_name = if name == "oms-ssh" {
        "oms-ssh-state".to_owned()
    } else if let Some(rest) = name.strip_prefix("oms-ssh-") {
        format!("oms-ssh-state-{rest}")
    } else {
        format!("{name}-state-{uid}")
    };
    let dir = parent.join(state_name);
    provision_private_user_dir(&dir)?;
    Ok(dir)
}

pub fn run(args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> Response {
    run_with_clock(args, cwd, interrupted, &SystemSshClock)
}
pub fn run_with_clock(
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    clock: &dyn SshClock,
) -> Response {
    execute(args, cwd, interrupted, clock).unwrap_or_else(Response::failure)
}
fn execute(
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    clock: &dyn SshClock,
) -> Result<Response> {
    let started = Instant::now();
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
    let mut timeout_seconds = None;
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
            "--repo" => &mut repo,
            "--remote-dir" if action != "probe" => &mut remote,
            "--base" if action == "sync-bundle" => &mut base,
            "--target" => &mut target,
            "--snapshot" if action == "exec" => &mut snapshot,
            "--op-id" if ["exec", "reconcile", "cancel"].contains(&action) => &mut op_id,
            "--grace-seconds" if action == "cancel" => &mut grace_seconds,
            "--remote-path" if action == "collect" => &mut remote_path_arg,
            "--dest" if action == "collect" => &mut dest,
            "--expected-sha256" if action == "collect" => &mut expected_sha256,
            "--expected-size" if action == "collect" => &mut expected_size,
            "--chunk-bytes" if action == "collect" => &mut chunk_bytes,
            "--max-bytes" if action == "collect" => &mut max_bytes,
            "--timeout-seconds"
                if matches!(
                    action,
                    "collect" | "exec" | "reconcile" | "cancel" | "probe"
                ) =>
            {
                &mut timeout_seconds
            }
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
    let timeout_seconds = timeout_seconds
        .map(|raw| {
            raw.parse::<u64>()
                .ok()
                .filter(|n| (1..=86400).contains(n))
                .ok_or_else(|| error("--timeout-seconds must be in 1..=86400"))
        })
        .transpose()?
        .unwrap_or(if matches!(action, "reconcile" | "cancel" | "probe") {
            30
        } else {
            300
        });
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
    if action == "collect" {
        resumable::check_cancelled(interrupted)?;
    } else {
        require(!interrupted.load(Ordering::Relaxed), "interrupted")?;
    }
    let deadline = started + Duration::from_secs(timeout_seconds);
    let mut value = serde_json::json!({"schema":1,"action":action,"host":host});
    let code;
    if action == "probe" {
        let probe_root = repo
            .as_deref()
            .map_or_else(|| cwd.to_path_buf(), |r| cwd.join(r));
        let control_persist = resolve_probe_target(&probe_root, target.as_deref(), &host)?;
        let permit = admit_ssh_connection(
            cwd,
            target.as_deref(),
            &host,
            port,
            "probe",
            &control,
            deadline,
            clock,
        )?;
        let mut ssh = build_ssh(&host, port, &control, control_persist)?;
        let out = stream::output(ssh.arg("printf ok"), deadline, interrupted)?;
        code = exit_code(out.status);
        record_ssh_outcome(
            &permit,
            is_connect_failure(code, &out.stdout, &out.stderr),
            deadline,
            clock,
        );
        if !permit.reused_control && is_interactive_auth_failure(code, &out.stdout, &out.stderr) {
            return Err(error(INTERACTIVE_AUTH_UNSUPPORTED));
        }
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
                pretty,
                deadline,
                interrupted,
                clock,
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
                pretty,
                deadline,
                interrupted,
                clock,
            })
        };
    }
    let remote = remote.expect("non-reconcile/cancel requires --remote-dir");
    let control_persist = enforce_target_policy(&repo, target.as_deref(), &host, &remote)?;
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
            target: target.as_deref(),
            control_persist,
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
            deadline,
            clock,
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
        let permit = admit_ssh_connection(
            cwd,
            target.as_deref(),
            &host,
            port,
            "sync-bundle",
            &control,
            deadline,
            clock,
        )?;
        let mut ssh = build_ssh(&host, port, &control, control_persist)?;
        let out = ssh
            .arg(format!("sh -lc {}", quote(&script)))
            .stdin(Stdio::from(file))
            .output()
            .map_err(error)?;
        code = exit_code(out.status);
        record_ssh_outcome(
            &permit,
            is_connect_failure(code, &out.stdout, &out.stderr),
            deadline,
            clock,
        );
        if !permit.reused_control && is_interactive_auth_failure(code, &out.stdout, &out.stderr) {
            return Err(error(INTERACTIVE_AUTH_UNSUPPORTED));
        }
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
            control_persist,
            snapshot: snapshot.as_deref(),
            snapshot_pins,
            head_sha: &head,
            op_id,
            tail: &tail,
            control: &control,
            pretty,
            deadline,
            interrupted,
            clock,
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
        let permit = admit_ssh_connection(
            cwd,
            target.as_deref(),
            &host,
            port,
            "exec",
            &control,
            deadline,
            clock,
        )?;
        let mut ssh = build_ssh(&host, port, &control, control_persist)?;
        value["started_at"] = now()?.into();
        let out = stream::output(ssh.arg(&remote_command), deadline, interrupted)?;
        value["completed_at"] = now()?.into();
        code = exit_code(out.status);
        record_ssh_outcome(
            &permit,
            is_connect_failure(code, &out.stdout, &out.stderr),
            deadline,
            clock,
        );
        if !permit.reused_control && is_interactive_auth_failure(code, &out.stdout, &out.stderr) {
            return Err(error(INTERACTIVE_AUTH_UNSUPPORTED));
        }
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

fn ssh_host_matches(registered: &str, supplied: &str) -> bool {
    if registered.is_empty() || supplied.is_empty() {
        return false;
    }
    let r_host = registered.rsplit_once('@').map_or(registered, |(_, h)| h);
    let s_host = supplied.rsplit_once('@').map_or(supplied, |(_, h)| h);
    if r_host.is_empty() || s_host.is_empty() {
        return false;
    }
    registered.eq_ignore_ascii_case(supplied) || r_host.eq_ignore_ascii_case(s_host)
}

pub fn resolve_registered_target_persist(state_root: &Path, target_name: &str) -> Result<u64> {
    if !state_root.exists() {
        return Ok(crate::targets::DEFAULT_CONTROL_PERSIST);
    }
    let raw = read_runtime_file_opt(state_root, Path::new(crate::targets::FILE))?;
    if raw.is_empty() {
        return Ok(crate::targets::DEFAULT_CONTROL_PERSIST);
    }
    let registry = crate::value::parse(&raw)?;
    crate::value::keys(&registry, &["schema", "targets"], &[])?;
    require(
        registry.get("schema").is_int(1),
        "invalid remote target registry",
    )?;
    let Some(row) = registry.get("targets").opt(target_name) else {
        return Ok(crate::targets::DEFAULT_CONTROL_PERSIST);
    };
    crate::targets::record(row)?;
    crate::targets::control_persist(row)
}

fn resolve_probe_target(state_root: &Path, target: Option<&str>, host: &str) -> Result<u64> {
    if let Some(name) = target {
        let registry = crate::value::parse(&read_runtime_file(
            state_root,
            Path::new(crate::targets::FILE),
        )?)?;
        crate::value::keys(&registry, &["schema", "targets"], &[])?;
        require(
            registry.get("schema").is_int(1),
            "invalid remote target registry",
        )?;
        let row = registry
            .get("targets")
            .opt(name)
            .ok_or_else(|| error("unknown remote target"))?;
        crate::targets::record(row)?;
        require(
            row.text("ssh_alias") == host,
            "target does not match SSH host",
        )?;
        return crate::targets::control_persist(row);
    }
    if !state_root.exists() {
        return Ok(crate::targets::DEFAULT_CONTROL_PERSIST);
    }
    let raw = read_runtime_file_opt(state_root, Path::new(crate::targets::FILE))?;
    if raw.is_empty() {
        return Ok(crate::targets::DEFAULT_CONTROL_PERSIST);
    }
    let registry = crate::value::parse(&raw)?;
    crate::value::keys(&registry, &["schema", "targets"], &[])?;
    require(
        registry.get("schema").is_int(1),
        "invalid remote target registry",
    )?;
    let Ok(entries) = registry.get("targets").object() else {
        return Ok(crate::targets::DEFAULT_CONTROL_PERSIST);
    };
    let mut exact = None;
    let mut alias_match = None;
    for (_, row) in entries {
        let alias = row.text("ssh_alias");
        if alias == host {
            exact = Some(row);
            break;
        } else if alias_match.is_none() && ssh_host_matches(alias, host) {
            alias_match = Some(row);
        }
    }
    if let Some(row) = exact.or(alias_match) {
        crate::targets::record(row)?;
        return crate::targets::control_persist(row);
    }
    Ok(crate::targets::DEFAULT_CONTROL_PERSIST)
}

fn validate_target(repo: &Path, name: &str, host: &str, remote: &str) -> Result<u64> {
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
    )?;
    crate::targets::control_persist(row)
}

fn enforce_target_policy(
    repo: &Path,
    target: Option<&str>,
    host: &str,
    remote: &str,
) -> Result<u64> {
    if let Some(name) = target {
        return validate_target(repo, name, host, remote);
    }
    require(
        std::env::var_os("OMS_SSH_REQUIRE_TARGET").as_deref() != Some(OsStr::new("1")),
        "--target is required by OMS_SSH_REQUIRE_TARGET",
    )?;
    let raw = read_runtime_file_opt(repo, Path::new(crate::targets::FILE))?;
    if raw.is_empty() {
        return Ok(crate::targets::DEFAULT_CONTROL_PERSIST);
    }
    let registry = crate::value::parse(&raw)?;
    crate::value::keys(&registry, &["schema", "targets"], &[])?;
    require(
        registry.get("schema").is_int(1),
        "invalid remote target registry",
    )?;
    let Ok(entries) = registry.get("targets").object() else {
        return Ok(crate::targets::DEFAULT_CONTROL_PERSIST);
    };
    let mut matched_exact = None;
    let mut exact_approved = false;
    let mut matched_host = false;
    let mut host_approved = false;

    for (_, row) in entries {
        if ssh_host_matches(row.text("ssh_alias"), host) {
            matched_host = true;
            let is_ok = crate::targets::approved(row)?;
            if is_ok {
                host_approved = true;
            }
            if row.text("workdir") == remote {
                matched_exact = Some(crate::targets::control_persist(row)?);
                if is_ok {
                    exact_approved = true;
                }
            }
        }
    }
    if let Some(persist) = matched_exact {
        require(
            exact_approved,
            "registered remote target is not approved for execution",
        )?;
        return Ok(persist);
    } else if matched_host {
        if !host_approved {
            return Err(error("registered SSH host is not approved for execution"));
        }
        return Err(error(
            "remote directory is not registered for this SSH host",
        ));
    }
    Ok(crate::targets::DEFAULT_CONTROL_PERSIST)
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

/// Resolve the retained target and snapshot identity for execution or replay.
pub(crate) fn experiment_snapshot(
    repo: &Path,
    target: &str,
    host: &str,
    remote: &str,
    snapshot: &str,
) -> Result<crate::value::Value> {
    validate_target(repo, target, host, remote)?;
    validate_snapshot(repo, snapshot)
}

/// Only a new experiment needs a matching live checkout.
pub(crate) fn verify_experiment_checkout(
    repo: &Path,
    snapshot: &crate::value::Value,
) -> Result<()> {
    verify_snapshot_checkouts(repo, snapshot).map(|_| ())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshControlMode {
    Auto,
    MasterStart,
    MultiplexOnly,
}

pub fn ssh_argv(
    program: &OsStr,
    host: &str,
    port: Option<u16>,
    control: &str,
    control_persist_secs: u64,
) -> Result<Vec<OsString>> {
    ssh_argv_with_mode(
        program,
        host,
        port,
        control,
        control_persist_secs,
        SshControlMode::Auto,
    )
}

pub fn ssh_argv_with_mode(
    program: &OsStr,
    host: &str,
    port: Option<u16>,
    control: &str,
    control_persist_secs: u64,
    mode: SshControlMode,
) -> Result<Vec<OsString>> {
    crate::targets::validate_control_persist(control_persist_secs)?;
    let control_master = match mode {
        SshControlMode::Auto => "ControlMaster=auto",
        SshControlMode::MasterStart => "ControlMaster=yes",
        SshControlMode::MultiplexOnly => "ControlMaster=no",
    };
    let mut argv = Vec::with_capacity(26);
    argv.push(program.to_os_string());
    argv.extend(
        [
            "-a",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            control_master,
            "-o",
            &format!("ControlPath={control}"),
            "-o",
            &format!("ControlPersist={control_persist_secs}"),
        ]
        .into_iter()
        .map(OsString::from),
    );
    if let Some(port) = port {
        argv.push(OsString::from("-p"));
        argv.push(OsString::from(port.to_string()));
    }
    if mode == SshControlMode::MasterStart {
        argv.push(OsString::from("-fN"));
    }
    argv.push(OsString::from("--"));
    argv.push(OsString::from(host));
    Ok(argv)
}

fn build_ssh(
    host: &str,
    port: Option<u16>,
    control: &str,
    control_persist_secs: u64,
) -> Result<Command> {
    let program = std::env::var_os("OMS_SSH_BIN")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "ssh".into());
    let argv = ssh_argv(&program, host, port, control, control_persist_secs)?;
    let mut ssh = Command::new(&argv[0]);
    ssh.args(&argv[1..]);
    Ok(ssh)
}

pub fn control_connection_open(control_path: &str) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let path = Path::new(control_path);
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_socket() || meta.uid() != rustix::process::geteuid().as_raw() {
        return false;
    }
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

pub fn is_interactive_auth_failure(exit_code: i32, stdout: &[u8], stderr: &[u8]) -> bool {
    if exit_code != 255 || !stdout.is_empty() || stderr.trim_ascii().is_empty() {
        return false;
    }
    let pw = concat!("pass", "word");
    let pp = concat!("pass", "phrase");
    let pc = concat!("pass", "code");
    let text = String::from_utf8_lossy(stderr);
    for raw_line in text.lines() {
        let line = raw_line.trim_ascii().to_ascii_lowercase();
        if line.is_empty() {
            continue;
        }
        if line == "authenticated with partial success."
            || line.starts_with("authenticated with partial success")
            || line.starts_with(&format!("read_{pp}:"))
            || line.contains(&format!(": read_{pp}:"))
        {
            return true;
        }
        if let Some((_, rest)) = line.split_once("permission denied (")
            && let Some((methods, _)) = rest.split_once(')')
            && methods.split(',').any(|m| {
                let m = m.trim_ascii();
                m == pw
                    || m == "keyboard-interactive"
                    || m.starts_with("keyboard-interactive/")
                    || m == "challenge-response"
                    || m.starts_with("challenge-response/")
            })
        {
            return true;
        }
        if let Some(stem) = line.strip_suffix(':') {
            let stem = stem.trim_ascii();
            if stem == pw
                || stem == pp
                || stem == pc
                || stem == format!("enter {pw}")
                || stem == format!("enter {pp}")
                || stem.starts_with(&format!("enter {pp} for "))
                || stem == "verification code"
                || stem == "enter verification code"
                || stem == format!("one-time {pw}")
                || stem == format!("enter one-time {pw}")
                || stem == "otp"
                || stem == "totp"
                || stem == "2fa"
                || stem == "2fa code"
                || stem == "two-factor code"
                || stem == "mfa"
                || stem == "mfa code"
                || (stem.ends_with(&format!("'s {pw}"))
                    && !stem[..stem.len() - (pw.len() + 3)]
                        .bytes()
                        .any(|b| b.is_ascii_whitespace()))
            {
                return true;
            }
        }
    }
    false
}

pub fn is_connect_failure(exit_code: i32, stdout: &[u8], stderr: &[u8]) -> bool {
    if exit_code != 255 || !stdout.is_empty() || stderr.trim_ascii().is_empty() {
        return false;
    }
    if is_interactive_auth_failure(exit_code, stdout, stderr) {
        return true;
    }
    let text = String::from_utf8_lossy(stderr);
    text.lines().any(|raw_line| {
        let line = raw_line.trim_ascii().to_ascii_lowercase();
        line.starts_with("ssh: connect to host ")
            || line.starts_with("ssh: could not resolve hostname ")
            || line.contains("could not resolve hostname ")
            || line.contains("connection refused")
            || line.contains("connection timed out")
            || line.contains("operation timed out")
            || line.contains("network is unreachable")
            || line.contains("no route to host")
            || line.contains("permission denied (")
            || line.contains("host key verification failed")
            || line.contains("remote host identification has changed")
            || line.starts_with("connection closed by ")
            || line.contains(": connection closed by ")
            || line.starts_with("connection reset by ")
            || line.contains(": connection reset by ")
            || line.starts_with("kex_exchange_identification:")
            || line.contains("kex_exchange_identification")
            || line.starts_with("ssh_exchange_identification:")
            || line.starts_with("banner exchange:")
            || line.starts_with("unable to negotiate with ")
            || line.starts_with("client_loop:")
            || line.contains("too many authentication failures")
            || line.starts_with("received disconnect from ")
            || line.starts_with("disconnected from ")
    })
}

pub fn compute_backoff_ms(consecutive_failures: u32, jitter_permille: u64) -> u64 {
    let shift = consecutive_failures.saturating_sub(1).min(20);
    let raw_base = INITIAL_BACKOFF_MS.saturating_mul(1u64 << shift);
    let jp = jitter_permille.min(1000);
    if raw_base < MAX_BACKOFF_MS {
        let jitter = (raw_base / 5) * jp / 1000;
        (raw_base + jitter).min(MAX_BACKOFF_MS)
    } else {
        let jitter = (MAX_BACKOFF_MS / 10) * jp / 1000;
        MAX_BACKOFF_MS - jitter
    }
}

fn format_rfc3339_ms(epoch_ms: u64) -> String {
    let secs = i64::try_from(epoch_ms / 1000).unwrap_or(i64::MAX);
    let nanos = ((epoch_ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, nanos)
        .map_or_else(|| format!("{epoch_ms}ms"), |dt| dt.to_rfc3339())
}

fn repair_unwritable_leaf(dir: &Path, file_name: &str) {
    let target = dir.join(file_name);
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.is_dir() && !meta.file_type().is_symlink() {
            let _ = std::fs::remove_dir_all(&target);
        } else {
            let _ = std::fs::remove_file(&target);
        }
    }
}

fn append_private_log_entry(state_dir: &Path, entry: &serde_json::Value) {
    if provision_private_user_dir(state_dir).is_err() {
        return;
    }
    let Ok(mut row) = serde_json::to_vec(entry) else {
        return;
    };
    row.push(b'\n');
    let abs = state_dir.join(PRIVATE_CONTROL_LOG_FILE);
    if let Ok(meta) = std::fs::symlink_metadata(&abs)
        && (!meta.is_file()
            || meta.file_type().is_symlink()
            || meta.permissions().mode() & 0o600 != 0o600
            || meta.len() > (MAX_PRIVATE_LOG_BYTES * 2) as u64)
    {
        repair_unwritable_leaf(state_dir, PRIVATE_CONTROL_LOG_FILE);
    }
    let Ok(root) = oms_safe_fs::Root::open(state_dir) else {
        return;
    };
    let rel = Path::new(PRIVATE_CONTROL_LOG_FILE);
    let _ = root.atomic_mutate(
        rel,
        oms_safe_fs::MutationOptions {
            read: oms_safe_fs::ByteReadOptions {
                missing_ok: true,
                max_bytes: MAX_PRIVATE_LOG_BYTES * 2,
            },
            create_parent: false,
            max_output: MAX_PRIVATE_LOG_BYTES * 2,
            preserve_existing: false,
            expected_exists: None,
        },
        move |prev| {
            let mut next = prev.to_vec();
            next.extend_from_slice(&row);
            if next.len() > MAX_PRIVATE_LOG_BYTES {
                let drop_from = next.len() - MAX_PRIVATE_LOG_BYTES;
                let cut = next[drop_from..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(drop_from, |p| drop_from + p + 1);
                next.drain(..cut);
            }
            Ok::<_, String>(next)
        },
    );
    if let Ok(meta) = std::fs::symlink_metadata(&abs)
        && meta.is_file()
        && !meta.file_type().is_symlink()
        && meta.permissions().mode() & 0o077 != 0
    {
        let _ = std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o600));
    }
}

#[allow(clippy::too_many_arguments)]
fn record_private_control_log(
    state_dir: &Path,
    action: &str,
    target_label: &str,
    connection_key: &str,
    host: &str,
    control_path: &str,
    reused: bool,
    now_ms: u64,
) {
    append_private_log_entry(
        state_dir,
        &serde_json::json!({
            "ts": format_rfc3339_ms(now_ms),
            "action": action,
            "target": target_label,
            "connection_key": connection_key,
            "host": host,
            "control_path": control_path,
            "reused": reused,
        }),
    );
}

fn record_private_diagnostic(state_dir: &Path, event: &str, detail: &str, now_ms: u64) {
    append_private_log_entry(
        state_dir,
        &serde_json::json!({
            "ts": format_rfc3339_ms(now_ms),
            "event": event,
            "detail": detail,
        }),
    );
}

pub fn ssh_connections_lock_path(state_dir: &Path, cwd: &Path) -> Result<PathBuf> {
    let resolved = resolve_existing(&state_dir.join(RECONNECT_STATE_FILE))?;
    let abs_cwd = resolve_existing(cwd)?;
    let settings = oms_lock::Settings::from_env()
        .map_err(|e| status(e.exit_code(), format!("ssh reconnect lock: {e}")))?;
    let root = resolve_lock_dir_root(&settings, &abs_cwd);
    let stamp = sha256(resolved.as_os_str().as_bytes());
    Ok(root.join(format!("ssh-connections-{}.lock", &stamp[..32])))
}

fn resolve_lock_dir_root(settings: &oms_lock::Settings, cwd: &Path) -> PathBuf {
    if let Some(raw) = std::env::var_os("OMS_LOCK_DIR") {
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
    }
}

fn lock_ssh_connections(
    state_dir: &Path,
    cwd: &Path,
    deadline: Instant,
    best_effort: bool,
) -> Result<oms_lock::Guard> {
    let resolved = resolve_existing(&state_dir.join(RECONNECT_STATE_FILE))?;
    let abs_cwd = resolve_existing(cwd)?;
    let mut settings = oms_lock::Settings::from_env()
        .map_err(|e| status(e.exit_code(), format!("ssh reconnect lock: {e}")))?;
    settings.mode = oms_lock::Mode::Mkdir;
    settings.cwd = abs_cwd.clone();
    let max_wait = if best_effort {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(15)
    }
    .min(settings.timeout);
    let remaining = deadline.checked_duration_since(Instant::now());
    settings.timeout = match remaining.filter(|d| !d.is_zero()) {
        Some(rem) => rem.min(max_wait),
        None if best_effort => Duration::from_millis(200).min(settings.timeout),
        None => {
            return Err(status(
                124,
                "SSH operation deadline exceeded while acquiring reconnect state lock",
            ));
        }
    };
    let root = resolve_lock_dir_root(&settings, &abs_cwd);
    let stamp = sha256(resolved.as_os_str().as_bytes());
    let path = root.join(format!("ssh-connections-{}.lock", &stamp[..32]));
    oms_lock::Lock::at_path(path, settings)
        .map_err(|e| status(e.exit_code(), format!("ssh reconnect lock: {e}")))?
        .acquire(None)
        .map_err(|e| status(e.exit_code(), format!("ssh reconnect lock: {e}")))
}

fn empty_ssh_connections_state() -> serde_json::Value {
    serde_json::json!({"schema": 1, "targets": {}})
}

fn empty_ssh_target_entry() -> serde_json::Value {
    serde_json::json!({
        "connect_timestamps_ms": [],
        "consecutive_failures": 0,
        "backoff_until_ms": 0,
    })
}

fn load_ssh_connections_tolerant(state_dir: &Path, now_ms: u64) -> (serde_json::Value, bool) {
    use std::os::unix::fs::MetadataExt;
    if provision_private_user_dir(state_dir).is_err() {
        return (empty_ssh_connections_state(), false);
    }
    let abs = state_dir.join(RECONNECT_STATE_FILE);
    match std::fs::symlink_metadata(&abs) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (empty_ssh_connections_state(), false);
        }
        Err(e) => {
            record_private_diagnostic(
                state_dir,
                "ssh_state_reset",
                &format!("cannot stat state file: {e}"),
                now_ms,
            );
            return (empty_ssh_connections_state(), true);
        }
        Ok(meta) => {
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.uid() != rustix::process::geteuid().as_raw()
                || meta.permissions().mode() & 0o600 != 0o600
                || meta.len() > crate::storage::MAX_STATE as u64
            {
                record_private_diagnostic(
                    state_dir,
                    "ssh_state_reset",
                    "unreadable, non-writable, oversized or non-regular state file",
                    now_ms,
                );
                return (empty_ssh_connections_state(), true);
            }
        }
    }
    let Ok(root) = oms_safe_fs::Root::open(state_dir) else {
        record_private_diagnostic(
            state_dir,
            "ssh_state_reset",
            "cannot open state dir",
            now_ms,
        );
        return (empty_ssh_connections_state(), false);
    };
    let bytes = match root.read_no_follow(
        Path::new(RECONNECT_STATE_FILE),
        oms_safe_fs::ByteReadOptions {
            missing_ok: true,
            max_bytes: crate::storage::MAX_STATE,
        },
    ) {
        Ok(b) => b,
        Err(e) => {
            record_private_diagnostic(
                state_dir,
                "ssh_state_reset",
                &format!("cannot read state file: {e:?}"),
                now_ms,
            );
            return (empty_ssh_connections_state(), true);
        }
    };
    if bytes.is_empty() {
        record_private_diagnostic(state_dir, "ssh_state_reset", "empty state file", now_ms);
        return (empty_ssh_connections_state(), false);
    }
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(val)
            if val.get("schema").and_then(|v| v.as_i64()) == Some(1)
                && val.get("targets").is_some_and(serde_json::Value::is_object) =>
        {
            (val, false)
        }
        _ => {
            record_private_diagnostic(
                state_dir,
                "ssh_state_reset",
                "invalid JSON or schema in state file",
                now_ms,
            );
            (empty_ssh_connections_state(), false)
        }
    }
}

fn target_entry_valid(entry: &serde_json::Value) -> bool {
    let Some(map) = entry.as_object() else {
        return false;
    };
    let ts_ok = map.get("connect_timestamps_ms").is_none_or(|v| {
        v.as_array()
            .is_some_and(|arr| arr.iter().all(serde_json::Value::is_u64))
    });
    let failures_ok = map
        .get("consecutive_failures")
        .is_none_or(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()).is_some());
    let backoff_ok = map
        .get("backoff_until_ms")
        .is_none_or(serde_json::Value::is_u64);
    ts_ok && failures_ok && backoff_ok
}

fn normalize_target_entry<'a>(
    state: &'a mut serde_json::Value,
    state_dir: &Path,
    connection_key: &str,
    now_ms: u64,
) -> (
    &'a mut serde_json::Map<String, serde_json::Value>,
    Vec<u64>,
    u32,
    u64,
) {
    if !state
        .get("targets")
        .is_some_and(serde_json::Value::is_object)
    {
        *state = empty_ssh_connections_state();
    }
    let targets = state["targets"]
        .as_object_mut()
        .expect("targets object ensured");
    let entry = targets
        .entry(connection_key.to_owned())
        .or_insert_with(empty_ssh_target_entry);
    if !target_entry_valid(entry) {
        record_private_diagnostic(state_dir, "ssh_target_entry_reset", connection_key, now_ms);
        *entry = empty_ssh_target_entry();
    }
    let map = entry.as_object_mut().expect("target entry object ensured");
    let backoff_until_ms = map
        .get("backoff_until_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let consecutive_failures = map
        .get("consecutive_failures")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    let mut timestamps: Vec<u64> = map
        .get("connect_timestamps_ms")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_u64)
                .filter(|&ts| ts.saturating_add(RECONNECT_WINDOW_MS) > now_ms)
                .collect()
        })
        .unwrap_or_default();
    timestamps.sort_unstable();
    (map, timestamps, consecutive_failures, backoff_until_ms)
}

fn write_ssh_connections(
    state_dir: &Path,
    state: &serde_json::Value,
    repair_existing: bool,
) -> Result<()> {
    provision_private_user_dir(state_dir)?;
    if repair_existing {
        repair_unwritable_leaf(state_dir, RECONNECT_STATE_FILE);
    }
    let mut encoded = serde_json::to_vec(state).map_err(error)?;
    encoded.push(b'\n');
    let root =
        oms_safe_fs::Root::open(state_dir).map_err(|e| error(format!("ssh state dir: {e:?}")))?;
    let rel = Path::new(RECONNECT_STATE_FILE);
    if root
        .atomic_mutate(
            rel,
            oms_safe_fs::MutationOptions {
                read: oms_safe_fs::ByteReadOptions {
                    missing_ok: true,
                    max_bytes: crate::storage::MAX_STATE,
                },
                create_parent: false,
                max_output: crate::storage::MAX_STATE,
                preserve_existing: false,
                expected_exists: None,
            },
            {
                let encoded = encoded.clone();
                move |_| Ok::<_, String>(encoded)
            },
        )
        .is_err()
    {
        repair_unwritable_leaf(state_dir, RECONNECT_STATE_FILE);
        root.atomic_mutate(
            rel,
            oms_safe_fs::MutationOptions {
                read: oms_safe_fs::ByteReadOptions {
                    missing_ok: true,
                    max_bytes: crate::storage::MAX_STATE,
                },
                create_parent: false,
                max_output: crate::storage::MAX_STATE,
                preserve_existing: false,
                expected_exists: None,
            },
            move |_| Ok::<_, String>(encoded),
        )
        .map_err(|e| error(format!("persist SSH reconnect state: {e:?}")))?;
    }
    let abs = state_dir.join(rel);
    let meta = std::fs::symlink_metadata(&abs).map_err(error)?;
    require(
        meta.is_file() && !meta.file_type().is_symlink(),
        "unsafe SSH reconnect state file",
    )?;
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o600)).map_err(error)?;
    }
    Ok(())
}

pub struct SshConnectPermit {
    pub reused_control: bool,
    state_dir: PathBuf,
    cwd: PathBuf,
    connection_key: String,
    had_prior_backoff: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn admit_ssh_connection(
    cwd: &Path,
    target_label: Option<&str>,
    host: &str,
    port: Option<u16>,
    action: &str,
    control_path: &str,
    deadline: Instant,
    clock: &dyn SshClock,
) -> Result<SshConnectPermit> {
    let state_dir = ssh_state_dir(control_path)?;
    let connection_key = ssh_connection_key(host, port);
    let label = match target_label.filter(|n| !n.is_empty() && *n != connection_key) {
        Some(name) => format!("{name} ({connection_key})"),
        None => connection_key.clone(),
    };
    let now_ms = clock.now_ms();
    let reused = control_connection_open(control_path);
    record_private_control_log(
        &state_dir,
        action,
        target_label.unwrap_or(&connection_key),
        &connection_key,
        host,
        control_path,
        reused,
        now_ms,
    );
    if reused {
        return Ok(SshConnectPermit {
            reused_control: true,
            state_dir,
            cwd: cwd.to_path_buf(),
            connection_key,
            had_prior_backoff: false,
        });
    }
    let _guard = lock_ssh_connections(&state_dir, cwd, deadline, false)?;
    let now_ms = clock.now_ms();
    let (mut state, repair_existing) = load_ssh_connections_tolerant(&state_dir, now_ms);
    let (entry, mut timestamps, consecutive_failures, backoff_until_ms) =
        normalize_target_entry(&mut state, &state_dir, &connection_key, now_ms);

    if backoff_until_ms > now_ms {
        let retry_at = format_rfc3339_ms(backoff_until_ms);
        let wait_secs = (backoff_until_ms - now_ms).div_ceil(1000);
        if repair_existing {
            let _ = write_ssh_connections(&state_dir, &state, true);
        }
        return Err(status(
            75,
            format!(
                "SSH reconnect backoff active for target {label}; retry at {retry_at} (in {wait_secs}s)"
            ),
        ));
    }
    if timestamps.len() >= MAX_NEW_CONNECTIONS_PER_HOUR {
        let retry_ms = timestamps[0].saturating_add(RECONNECT_WINDOW_MS);
        let retry_at = format_rfc3339_ms(retry_ms);
        let wait_secs = retry_ms.saturating_sub(now_ms).div_ceil(1000);
        if repair_existing {
            let _ = write_ssh_connections(&state_dir, &state, true);
        }
        return Err(status(
            75,
            format!(
                "SSH connection rate limit (12/hour) exceeded for target {label}; retry at {retry_at} (in {wait_secs}s)"
            ),
        ));
    }
    timestamps.push(now_ms);
    entry.insert(
        "connect_timestamps_ms".to_owned(),
        serde_json::json!(timestamps),
    );
    write_ssh_connections(&state_dir, &state, repair_existing)?;
    Ok(SshConnectPermit {
        reused_control: false,
        state_dir,
        cwd: cwd.to_path_buf(),
        connection_key,
        had_prior_backoff: consecutive_failures > 0 || backoff_until_ms > 0,
    })
}

pub fn record_ssh_outcome(
    permit: &SshConnectPermit,
    connect_failed: bool,
    deadline: Instant,
    clock: &dyn SshClock,
) {
    if permit.reused_control {
        return;
    }
    if !connect_failed && !permit.had_prior_backoff {
        return;
    }
    if let Err(e) = try_record_ssh_outcome(permit, connect_failed, deadline, clock) {
        record_private_diagnostic(
            &permit.state_dir,
            "record_ssh_outcome_failed",
            &e.message,
            clock.now_ms(),
        );
    }
}

fn try_record_ssh_outcome(
    permit: &SshConnectPermit,
    connect_failed: bool,
    deadline: Instant,
    clock: &dyn SshClock,
) -> Result<()> {
    let _guard = lock_ssh_connections(&permit.state_dir, &permit.cwd, deadline, true)?;
    let now_ms = clock.now_ms();
    let (mut state, repair_existing) = load_ssh_connections_tolerant(&permit.state_dir, now_ms);
    let (entry, _, prev_failures, _) = normalize_target_entry(
        &mut state,
        &permit.state_dir,
        &permit.connection_key,
        now_ms,
    );
    if connect_failed {
        let next_failures = prev_failures.saturating_add(1);
        let delay_ms = compute_backoff_ms(next_failures, clock.jitter_permille(next_failures));
        entry.insert("consecutive_failures".to_owned(), next_failures.into());
        entry.insert(
            "backoff_until_ms".to_owned(),
            now_ms.saturating_add(delay_ms).into(),
        );
    } else {
        entry.insert("consecutive_failures".to_owned(), 0.into());
        entry.insert("backoff_until_ms".to_owned(), 0.into());
    }
    write_ssh_connections(&permit.state_dir, &state, repair_existing)
}

fn json_response(code: i32, value: &serde_json::Value, pretty: bool) -> Result<Response> {
    let redacted;
    let out_val = if let Some(map) = value.as_object()
        && map.contains_key("control_path")
    {
        let mut clean = map.clone();
        clean.remove("control_path");
        redacted = serde_json::Value::Object(clean);
        &redacted
    } else {
        value
    };
    let mut stdout = if pretty {
        serde_json::to_vec_pretty(out_val)
    } else {
        serde_json::to_vec(out_val)
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

fn lock_remote_op(
    repo: &Path,
    cwd: &Path,
    op_id: &str,
    deadline: Instant,
) -> Result<oms_lock::Guard> {
    let rel = PathBuf::from(format!(".oms/runtime/remote-ops/{op_id}.json"));
    let resolved = resolve_existing(&repo.join(&rel))?;
    let mut settings = oms_lock::Settings::from_env()
        .map_err(|e| status(e.exit_code(), format!("remote op lock: {e}")))?;
    settings.mode = oms_lock::Mode::Mkdir;
    settings.timeout = deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| {
            status(
                124,
                "SSH operation deadline exceeded while acquiring custody",
            )
        })?
        .min(Duration::from_secs(15));
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
    control_persist: u64,
    snapshot: Option<&'a str>,
    snapshot_pins: Option<SnapshotPins>,
    head_sha: &'a str,
    op_id: &'a str,
    tail: &'a [OsString],
    control: &'a str,
    pretty: bool,
    deadline: Instant,
    interrupted: &'a AtomicBool,
    clock: &'a dyn SshClock,
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

/// Only an exact, locally retained SSH receipt can resolve experiment custody.
pub(crate) fn experiment_receipt(
    repo: &Path,
    run: &crate::value::Value,
) -> Result<Option<crate::value::Value>> {
    let op_id = run.text("run_id");
    validate_op_id(op_id)?;
    let Some(receipt) =
        read_remote_op(repo, op_id).map_err(|_| error("cannot verify remote operation receipt"))?
    else {
        return Ok(None);
    };
    require(
        receipt.get("port").is_some_and(serde_json::Value::is_null)
            && request_matches(
                &receipt,
                run.text("host"),
                None,
                run.text("remote_dir"),
                Some(run.text("target")),
                Some(run.text("snapshot")),
                run.text("command_sha256"),
            ),
        "remote operation receipt does not match the experiment identity",
    )?;
    crate::value::from_serde(&receipt).map(Some)
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

fn command_script(argv: &[OsString]) -> Result<String> {
    let command = argv
        .iter()
        .map(|v| {
            v.to_str()
                .map(quote)
                .ok_or_else(|| error("command requires Unicode text"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!("exec {command}"))
}

pub(crate) fn command_digest(argv: &[OsString]) -> Result<String> {
    Ok(sha256(command_script(argv)?.as_bytes()))
}

fn run_durable_exec(req: DurableExecRequest<'_>) -> Result<Response> {
    let cmd_script = command_script(req.tail)?;
    let command_sha256 = sha256(cmd_script.as_bytes());
    let _guard = lock_remote_op(req.repo, req.cwd, req.op_id, req.deadline)?;

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

    let permit = admit_ssh_connection(
        req.cwd,
        req.target,
        req.host,
        req.port,
        "exec",
        req.control,
        req.deadline,
        req.clock,
    )?;
    let mut ssh = build_ssh(req.host, req.port, req.control, req.control_persist)?;

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
export OMS_REMOTE_OP_DIR="$op_dir"
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
    let out = operation_output(
        ssh.arg(format!("sh -lc {}", quote(&remote_script))),
        req.repo,
        req.op_id,
        &mut record,
        req.deadline,
        req.interrupted,
    )?;
    let out_code = exit_code(out.status);
    record_ssh_outcome(
        &permit,
        is_connect_failure(out_code, &out.stdout, &out.stderr),
        req.deadline,
        req.clock,
    );
    if !permit.reused_control
        && out.stdout.is_empty()
        && is_interactive_auth_failure(out_code, &out.stdout, &out.stderr)
    {
        record["state"] = "failed".into();
        record["failure_kind"] = "interactive_auth_unsupported".into();
        record["exit_code"] = out_code.into();
        record["stderr"] = INTERACTIVE_AUTH_UNSUPPORTED.into();
        let ts = now()?;
        record["updated_at"] = ts.clone().into();
        record["completed_at"] = ts.into();
        write_remote_op(req.repo, req.op_id, &record)?;
        return Err(error(INTERACTIVE_AUTH_UNSUPPORTED));
    }

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
    if parsed.parts.is_empty()
        && matches!(parsed.markers.as_slice(), [(kind, payload)] if kind == "SNAPSHOT_DRIFT" && payload == "1")
    {
        record["state"] = "failed".into();
        record["failure_kind"] = "snapshot_drift".into();
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

fn operation_output(
    command: &Command,
    repo: &Path,
    op_id: &str,
    record: &mut serde_json::Value,
    deadline: Instant,
    interrupted: &AtomicBool,
) -> Result<std::process::Output> {
    match stream::output(command, deadline, interrupted) {
        Ok(output) => Ok(output),
        Err(e) => {
            record["state"] = "unknown".into();
            record["updated_at"] = now()?.into();
            record["observation_exit"] = e.code.into();
            record["reconcile_required"] = true.into();
            // In particular, a killed cancel observer is not a remote cancel
            // receipt. Reconciliation must prove the eventual remote result.
            if record["action"] == "cancel" {
                record["signal_sent"] = serde_json::Value::Null;
            }
            write_remote_op(repo, op_id, record)?;
            Err(e)
        }
    }
}

struct ReconcileRequest<'a> {
    cwd: &'a Path,
    repo: &'a Path,
    host: &'a str,
    port: Option<u16>,
    remote: Option<&'a str>,
    target: Option<&'a str>,
    op_id: &'a str,
    pretty: bool,
    deadline: Instant,
    interrupted: &'a AtomicBool,
    clock: &'a dyn SshClock,
}

fn validate_existing_scope(
    record: &serde_json::Value,
    repo: &Path,
    host: &str,
    port: Option<u16>,
    remote: Option<&str>,
    target: Option<&str>,
) -> Result<(String, Option<String>, u64, Option<u16>)> {
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
    let effective_port = port.or_else(|| {
        record
            .get("port")
            .and_then(|v| v.as_u64())
            .and_then(|n| u16::try_from(n).ok())
    });
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
    let effective_target = target
        .or_else(|| record.get("target").and_then(|v| v.as_str()))
        .map(str::to_owned);
    let control_persist =
        enforce_target_policy(repo, effective_target.as_deref(), host, &stored_remote)?;
    Ok((
        stored_remote,
        effective_target,
        control_persist,
        effective_port,
    ))
}

fn run_reconcile(req: ReconcileRequest<'_>) -> Result<Response> {
    let _guard = lock_remote_op(req.repo, req.cwd, req.op_id, req.deadline)?;
    let mut record =
        read_remote_op(req.repo, req.op_id)?.ok_or_else(|| error("unknown --op-id"))?;
    let (remote_dir, effective_target, control_persist, effective_port) = validate_existing_scope(
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
    let control = ssh_control_path(req.host, effective_port)?;
    let permit = admit_ssh_connection(
        req.cwd,
        effective_target.as_deref(),
        req.host,
        effective_port,
        "reconcile",
        &control,
        req.deadline,
        req.clock,
    )?;
    let mut ssh = build_ssh(req.host, effective_port, &control, control_persist)?;
    let out = operation_output(
        ssh.arg(format!("sh -lc {}", quote(&script))),
        req.repo,
        req.op_id,
        &mut record,
        req.deadline,
        req.interrupted,
    )?;
    let out_code = exit_code(out.status);
    record_ssh_outcome(
        &permit,
        is_connect_failure(out_code, &out.stdout, &out.stderr),
        req.deadline,
        req.clock,
    );
    if !permit.reused_control
        && out.stdout.is_empty()
        && is_interactive_auth_failure(out_code, &out.stdout, &out.stderr)
    {
        return Err(error(INTERACTIVE_AUTH_UNSUPPORTED));
    }
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
    pretty: bool,
    deadline: Instant,
    interrupted: &'a AtomicBool,
    clock: &'a dyn SshClock,
}

fn run_cancel(req: CancelRequest<'_>) -> Result<Response> {
    let _guard = lock_remote_op(req.repo, req.cwd, req.op_id, req.deadline)?;
    let mut record =
        read_remote_op(req.repo, req.op_id)?.ok_or_else(|| error("unknown --op-id"))?;
    let (remote_dir, effective_target, control_persist, effective_port) = validate_existing_scope(
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
    let control = ssh_control_path(req.host, effective_port)?;
    let permit = admit_ssh_connection(
        req.cwd,
        effective_target.as_deref(),
        req.host,
        effective_port,
        "cancel",
        &control,
        req.deadline,
        req.clock,
    )?;
    let mut ssh = build_ssh(req.host, effective_port, &control, control_persist)?;
    let out = operation_output(
        ssh.arg(format!("sh -lc {}", quote(&script))),
        req.repo,
        req.op_id,
        &mut record,
        req.deadline,
        req.interrupted,
    )?;
    let out_code = exit_code(out.status);
    record_ssh_outcome(
        &permit,
        is_connect_failure(out_code, &out.stdout, &out.stderr),
        req.deadline,
        req.clock,
    );
    if !permit.reused_control
        && out.stdout.is_empty()
        && is_interactive_auth_failure(out_code, &out.stdout, &out.stderr)
    {
        return Err(error(INTERACTIVE_AUTH_UNSUPPORTED));
    }
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

#[derive(Clone, Copy)]
struct CollectRequest<'a> {
    cwd: &'a Path,
    target: Option<&'a str>,
    control_persist: u64,
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
    deadline: Instant,
    clock: &'a dyn SshClock,
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
    let directory = resumable::open_directory(&parent)?;
    let (mut tmp_file, mut guard) = create_private_temp(&parent)?;
    let (total_bytes, digest) = stream::collect(&req, &script, &mut tmp_file)?;
    if let Some(expected) = req.expected_sha256 {
        require(
            expected == digest,
            format!("collected file digest mismatch: expected {expected}, got {digest}"),
        )?;
    }
    tmp_file.sync_all().map_err(error)?;
    destination_lock.check().map_err(error)?;
    resumable::check_request(&req)?;
    resumable::publish(&directory, &parent, &tmp_file, &mut guard, &dest_abs)?;

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
    use std::os::unix::process::CommandExt;

    fn run_fake_remote() -> bool {
        let Some(script) = std::env::var_os("OMS_TEST_REMOTE_SCRIPT") else {
            return false;
        };
        // A real remote host is outside the local SSH client's process group.
        // Keep the fake transport supervised while its remote shell survives
        // local transport cleanup, using the same boundary on Linux and macOS.
        let mut remote = Command::new("sh")
            .args([OsStr::new("-c"), script.as_os_str()])
            .env_remove("OMS_TEST_REMOTE_SCRIPT")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = rustix::process::Pid::from_raw(remote.id().try_into().unwrap()).unwrap();
        assert_eq!(rustix::process::getpgid(Some(pid)).unwrap(), pid);
        assert_ne!(rustix::process::getpgid(None).unwrap(), pid);
        let _ = remote.wait().unwrap();
        true
    }

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
            let rt_dir = short_runtime_dir("trp");
            let fake = temp.0.join("ssh");
            std::fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$OMS_TEST_ARGS\"\nfor last do :; done\ncase \"$last\" in *oms-bundle*) cat > \"$OMS_TEST_BUNDLE\"; exec sh -c \"$last\" < \"$OMS_TEST_BUNDLE\";; *) exec sh -c \"$last\";; esac\n").unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = isolated_child("ssh_adapter::tests::transport", &temp, &rt_dir)
                .env("OMS_TEST_SSH_CHILD", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_ARGS", temp.0.join("args"))
                .env("OMS_TEST_BUNDLE", temp.0.join("bundle"))
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
        assert!(
            v.get("control_path").is_none(),
            "probe JSON must not expose control_path: {v}"
        );
        assert!(
            !root.join(".oms").exists(),
            "probe must not create .oms inside cwd"
        );
        let expected_ctrl = ssh_control_path(&long_host, None).unwrap();
        let state_dir = ssh_state_dir(&expected_ctrl).unwrap();
        assert_eq!(
            std::fs::symlink_metadata(&state_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let private_log = state_dir.join(PRIVATE_CONTROL_LOG_FILE);
        assert_eq!(
            std::fs::symlink_metadata(&private_log)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let log_row: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&private_log)
                .unwrap()
                .lines()
                .last()
                .unwrap(),
        )
        .unwrap();
        let ctrl = log_row["control_path"].as_str().unwrap();
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
        let arg_lines: Vec<&str> = args.lines().collect();
        assert!(arg_lines.contains(&"-a"), "{args}");
        for option in [
            "ClearAllForwardings=yes",
            "ForwardAgent=no",
            "BatchMode=yes",
            "ConnectTimeout=10",
            "ServerAliveInterval=30",
            "ServerAliveCountMax=3",
            "ControlMaster=auto",
            "ControlPersist=1800",
            "ControlPath=",
        ] {
            assert!(args.contains(option), "missing {option} in {args}");
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
        assert!(v.get("control_path").is_none());
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
            let temp = TempDir::new().unwrap();
            let rt_dir = short_runtime_dir("gat");
            let status = isolated_child(
                "ssh_adapter::tests::remote_op_id_validation_and_child_read_only_gate",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_SSH_CHILD_GATE", &temp.0)
            .env("OMS_HARNESS_CHILD", "1")
            .status()
            .unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_SSH_CHILD_GATE").unwrap());
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
            let resp = invoke(&[blocked, "--host", "example"], &root);
            assert_eq!(resp.code, 2, "{blocked}");
            assert!(
                String::from_utf8_lossy(&resp.stderr)
                    .contains("a harness child may only use read-only runtime actions")
            );
        }
        // reconcile passes the child gate and fails only on missing --repo / --op-id
        let rec = invoke(&["reconcile", "--host", "example"], &root);
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
    fn connected_exec_reconcile_and_cancel_observers_share_a_wall_deadline() {
        if std::env::var_os("OMS_SSH_WALL_CHILD").is_none() {
            let temp = TempDir::new().unwrap();
            let rt_dir = short_runtime_dir("wal");
            let fake = temp.0.join("ssh");
            std::fs::write(
                &fake,
                "#!/bin/sh\nprintf 'connected\\n' >> \"$OMS_TEST_ARGS\"\nexec sleep 30\n",
            )
            .unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = isolated_child(
                "ssh_adapter::tests::connected_exec_reconcile_and_cancel_observers_share_a_wall_deadline",
                &temp,
                &rt_dir,
            )
            .env("OMS_SSH_WALL_CHILD", "1")
            .env("OMS_SSH_BIN", fake)
            .env("OMS_TEST_ARGS", temp.0.join("calls"))
            .status()
            .unwrap();
            assert!(result.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote");
        init_temp_git_repo(&repo);
        std::fs::create_dir(&remote).unwrap();
        let args = [
            "exec",
            "--host",
            "fixture",
            "--repo",
            repo.to_str().unwrap(),
            "--remote-dir",
            remote.to_str().unwrap(),
            "--op-id",
            "wall-op",
            "--timeout-seconds",
            "1",
            "--",
            "true",
        ];
        let before = Instant::now();
        let first = invoke(&args, &root);
        assert_eq!(
            first.code,
            124,
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        assert!(
            before.elapsed() < Duration::from_secs(5),
            "connected exec observer was unbounded"
        );
        let path = repo.join(".oms/runtime/remote-ops/wall-op.json");
        let read =
            || serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(read()["state"], "unknown");
        assert_eq!(read()["observation_exit"], 124);
        let calls = || {
            std::fs::read_to_string(root.join("calls"))
                .unwrap()
                .lines()
                .count()
        };
        assert_eq!(calls(), 1);
        assert_eq!(invoke(&args, &root).code, 75);
        assert_eq!(calls(), 1, "unknown start must never resend exec");
        for action in ["reconcile", "cancel"] {
            let start = Instant::now();
            let out = invoke(
                &[
                    action,
                    "--host",
                    "fixture",
                    "--repo",
                    repo.to_str().unwrap(),
                    "--op-id",
                    "wall-op",
                    "--timeout-seconds",
                    "1",
                ],
                &root,
            );
            assert_eq!(
                out.code,
                124,
                "{action}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "connected {action} observer was unbounded"
            );
            let row = read();
            assert_eq!(row["state"], "unknown");
            assert_eq!(row["reconcile_required"], true);
            if action == "cancel" {
                assert!(
                    row["signal_sent"].is_null(),
                    "local timeout is not remote cancellation proof"
                );
            }
        }
        assert_eq!(calls(), 3);
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
            let rt_dir = short_runtime_dir("sc4");
            let status = isolated_child(
                "ssh_adapter::tests::exec_op_id_disconnect_recovers_via_reconcile_without_duplicate",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_SCENARIO4", "1")
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_ARGS", temp.0.join("args"))
            .env("OMS_TEST_DROP_FLAG", temp.0.join("drop_once"))
            .env("OMS_REMOTE_STATE_DIR", &state_dir)
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
        if run_fake_remote() {
            return;
        }
        if std::env::var_os("OMS_TEST_SCENARIO7").is_none() {
            let temp = TempDir::new().unwrap();
            let state_dir = temp.0.join("remote_state");
            let fake = temp.0.join("ssh");
            let script = r#"#!/bin/sh
printf '%s\n' "$@" >> "$OMS_TEST_ARGS"
for last do :; done
if [ -f "$OMS_TEST_DROP_ACTIVE" ]; then
  rm -f "$OMS_TEST_DROP_ACTIVE"
  OMS_TEST_REMOTE_SCRIPT="$last" "$OMS_TEST_REMOTE_EXE" --exact "$OMS_TEST_REMOTE_CASE" --nocapture >/dev/null 2>&1 &
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
            let rt_dir = short_runtime_dir("sc7");
            let status = isolated_child(
                "ssh_adapter::tests::pid_reuse_boot_id_and_conflicting_op_id_are_fenced",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_SCENARIO7", "1")
            .env("OMS_TEST_REMOTE_EXE", std::env::current_exe().unwrap())
            .env(
                "OMS_TEST_REMOTE_CASE",
                "ssh_adapter::tests::pid_reuse_boot_id_and_conflicting_op_id_are_fenced",
            )
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_ARGS", temp.0.join("args"))
            .env("OMS_TEST_DROP_ACTIVE", temp.0.join("drop_active"))
            .env("OMS_REMOTE_STATE_DIR", &state_dir)
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
            let rt_dir = short_runtime_dir("sc8");
            let status = isolated_child(
                "ssh_adapter::tests::collect_enforces_paths_bounds_and_digest_atomically",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_SCENARIO8", "1")
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_FAIL_STREAM", temp.0.join("fail_stream"))
            .status()
            .unwrap();
            assert!(status.success());
            return;
        }
        let _mux = bind_control_mux("example", None);
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

        // The request deadline covers all chunks and their child cleanup; it
        // is shorter than the existing per-chunk ceiling in this fixture.
        let timed_dest = dest_dir.join("timed.bin");
        std::fs::write(&timed_dest, b"original").unwrap();
        let mut timed_args = args(&timed_dest, &digest, payload.len());
        timed_args.extend(["--timeout-seconds".into(), "1".into()]);
        std::fs::write(root.join("resume_hold"), "1").unwrap();
        let started = Instant::now();
        let timed = request(&timed_args);
        assert_eq!(
            timed.code,
            124,
            "{}",
            String::from_utf8_lossy(&timed.stderr)
        );
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(std::fs::read(&timed_dest).unwrap(), b"original");
        std::fs::remove_file(root.join("resume_hold")).unwrap();
        std::fs::remove_file(root.join("resume_ready")).unwrap();
        let resumed = request(&timed_args);
        assert_eq!(
            resumed.code,
            0,
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
        assert_eq!(resumed["chunks_reused"], 1);
        assert_eq!(std::fs::read(&timed_dest).unwrap(), payload);

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
        // Losing a lock during an external fetch must precede any checkpoint write.
        std::fs::remove_file(root.join("resume_ready")).unwrap();
        let lost = dest_dir.join("lost-lock.bin");
        std::fs::write(&lost, b"original").unwrap();
        let lost_args = args(&lost, &digest, payload.len());
        std::fs::write(root.join("resume_hold"), "2").unwrap();
        std::thread::scope(|scope| {
            let pending = scope.spawn(|| request(&lost_args));
            let started = std::time::Instant::now();
            while !root.join("resume_ready").exists() && started.elapsed() < Duration::from_secs(3)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(root.join("resume_ready").exists());
            let physical_dest = lost.canonicalize().unwrap();
            let cache = std::fs::read_dir(&cache_parent)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    std::fs::read(path.join("manifest.json"))
                        .ok()
                        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
                        .is_some_and(|row| {
                            row["identity"]["dest"] == physical_dest.to_str().unwrap()
                        })
                })
                .unwrap();
            let manifest = cache.join("manifest.json");
            let before = std::fs::read(&manifest).unwrap();
            std::fs::rename(cache.join("transfer.lock"), cache.join("detached.lock")).unwrap();
            std::fs::create_dir(cache.join("transfer.lock")).unwrap();
            std::fs::remove_file(root.join("resume_hold")).unwrap();
            assert_ne!(pending.join().unwrap().code, 0);
            assert_eq!(std::fs::read(&manifest).unwrap(), before);
            assert!(!cache.join("00002.chunk").exists());
            assert_eq!(std::fs::read(&lost).unwrap(), b"original");
        });
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
            let rt_dir = short_runtime_dir("snp");
            let status = isolated_child(
                "ssh_adapter::tests::omitted_target_blocks_unapproved_host_and_snapshot_binds_remote_head",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_TARGET_SNAPSHOT", "1")
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_ARGS", &args_log)
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
        if run_fake_remote() {
            return;
        }
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
  OMS_TEST_REMOTE_SCRIPT="$last" "$OMS_TEST_REMOTE_EXE" --exact "$OMS_TEST_REMOTE_CASE" --nocapture >/dev/null 2>&1 &
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
            let rt_dir = short_runtime_dir("can");
            let status = isolated_child(
                "ssh_adapter::tests::cancel_escalates_sigkill_and_blocks_spoofed_frames_and_unverifiable_lstart",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_CANCEL_HARDENING", "1")
            .env("OMS_TEST_REMOTE_EXE", std::env::current_exe().unwrap())
            .env(
                "OMS_TEST_REMOTE_CASE",
                "ssh_adapter::tests::cancel_escalates_sigkill_and_blocks_spoofed_frames_and_unverifiable_lstart",
            )
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_DROP_ACTIVE", temp.0.join("drop_active"))
            .env("OMS_REMOTE_STATE_DIR", &state_dir)
            .status()
            .unwrap();
            assert!(status.success());
            return;
        }
        let _mux = bind_control_mux("example", None);
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
if [ -f "$OMS_TEST_DIR/stall" ]; then
  printf '%s\n' "$$" > "$OMS_TEST_DIR/stall_pid"
  mode=$(cat "$OMS_TEST_DIR/stall")
  : > "$OMS_TEST_DIR/stall_ready"
  case "$mode" in
    stderr) while :; do printf 'diagnostic-only-output\n' >&2; done ;;
    closed) exec 1>&- ;;
    bytes) printf 'complete-bytes-without-eof' ;;
  esac
  exec sleep 20
fi
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
            let rt_dir = short_runtime_dir("col");
            let status = isolated_child(
                "ssh_adapter::tests::collect_rejects_symlink_ancestors_and_overflow_with_open_stderr",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_COLLECT_HARDENING", "1")
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_OPEN_STDERR_FLOOD", temp.0.join("flood"))
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
        std::fs::remove_file(root.join("flood")).unwrap();

        let dest = dest_dir.join("preserved.txt");
        std::fs::write(&dest, b"original").unwrap();
        let args: Vec<OsString> = [
            "collect",
            "--host",
            "example",
            "--repo",
            rs,
            "--remote-dir",
            rd,
            "--remote-path",
            "artifact.txt",
            "--dest",
            dest.to_str().unwrap(),
            "--timeout-seconds",
            "1",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert_eq!(run(&args, &root, &AtomicBool::new(true)).code, 130);
        assert!(!root.join("stall_pid").exists());
        let assert_reaped = || {
            let pid = std::fs::read_to_string(root.join("stall_pid")).unwrap();
            assert!(
                !Command::new("kill")
                    .args(["-0", pid.trim()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        // Closed stdout and an unlimited stderr writer exercise waits that a
        // blocking stdout read plus stderr join could never time out.
        for mode in ["quiet", "closed", "stderr"] {
            std::fs::write(root.join("stall"), mode).unwrap();
            let started = Instant::now();
            let result = run(&args, &root, &AtomicBool::new(false));
            assert_eq!(
                result.code,
                124,
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(started.elapsed() < Duration::from_secs(4));
            assert_eq!(std::fs::read(&dest).unwrap(), b"original");
            assert_reaped();
        }
        std::fs::write(root.join("stall"), "bytes").unwrap();
        std::fs::remove_file(root.join("stall_ready")).unwrap();
        let interrupted = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let started = Instant::now();
                while !root.join("stall_ready").exists() {
                    assert!(started.elapsed() < Duration::from_secs(3));
                    std::thread::sleep(Duration::from_millis(10));
                }
                interrupted.store(true, Ordering::Relaxed);
            });
            let started = Instant::now();
            let result = run(&args, &root, &interrupted);
            assert_eq!(
                result.code,
                130,
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(started.elapsed() < Duration::from_secs(4));
        });
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert_reaped();
        assert!(
            !std::fs::read_dir(&dest_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with(".oms-collect-"))
        );
    }

    struct ShortRuntimeDir(PathBuf);
    impl Drop for ShortRuntimeDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn short_runtime_dir(tag: &str) -> ShortRuntimeDir {
        let pid = std::process::id();
        for raw_base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
            let Ok(base) = std::fs::canonicalize(&raw_base) else {
                continue;
            };
            for n in 0..1000 {
                let p = base.join(format!("oms-rt-{tag}-{pid}-{n}"));
                if p.to_string_lossy().len() > 48 {
                    break;
                }
                match std::fs::DirBuilder::new().mode(0o700).create(&p) {
                    Ok(()) => return ShortRuntimeDir(p),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(_) => break,
                }
            }
        }
        panic!("cannot allocate short runtime dir");
    }

    fn isolated_child(exact: &str, temp: &TempDir, rt_dir: &ShortRuntimeDir) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", exact, "--nocapture"])
            .env("OMS_TEST_DIR", &temp.0)
            .env("OMS_LOCK_DIR", temp.0.join("locks"))
            .env("XDG_RUNTIME_DIR", &rt_dir.0)
            .env("TMPDIR", &temp.0)
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_HARNESS_DELEGATE_DEPTH");
        cmd
    }

    struct MuxSocketGuard {
        _listener: std::os::unix::net::UnixListener,
        path: PathBuf,
    }
    impl Drop for MuxSocketGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn bind_control_mux(host: &str, port: Option<u16>) -> MuxSocketGuard {
        let ctrl = PathBuf::from(ssh_control_path(host, port).unwrap());
        let _ = std::fs::remove_file(&ctrl);
        let listener = std::os::unix::net::UnixListener::bind(&ctrl).unwrap();
        MuxSocketGuard {
            _listener: listener,
            path: ctrl,
        }
    }

    struct TestClock {
        now_ms: AtomicU64,
        jitter_permille: AtomicU64,
    }
    impl TestClock {
        fn new(initial_ms: u64, jitter_permille: u64) -> Self {
            Self {
                now_ms: AtomicU64::new(initial_ms),
                jitter_permille: AtomicU64::new(jitter_permille),
            }
        }
        fn advance_ms(&self, delta_ms: u64) {
            self.now_ms.fetch_add(delta_ms, Ordering::SeqCst);
        }
        fn set_jitter_permille(&self, jp: u64) {
            self.jitter_permille.store(jp, Ordering::SeqCst);
        }
    }
    impl SshClock for TestClock {
        fn now_ms(&self) -> u64 {
            self.now_ms.load(Ordering::SeqCst)
        }
        fn jitter_permille(&self, _consecutive_failures: u32) -> u64 {
            self.jitter_permille.load(Ordering::SeqCst)
        }
    }

    fn invoke_clock(args: &[&str], cwd: &Path, clock: &dyn SshClock) -> Response {
        run_with_clock(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            cwd,
            &AtomicBool::new(false),
            clock,
        )
    }

    #[test]
    fn control_persist_backoff_hourly_cap_interactive_auth_and_legacy_approval() {
        if std::env::var_os("OMS_TEST_SSH_SPEC3").is_none() {
            let temp = TempDir::new().unwrap();
            let rt_dir = short_runtime_dir("sp3");
            let fake = temp.0.join("ssh");
            let script = concat!(
                "#!/bin/sh\n",
                "printf '%s\\n' \"$@\" > \"$OMS_TEST_ARGS\"\n",
                "printf '1\\n' >> \"$OMS_TEST_CALLS\"\n",
                "mode=$(cat \"$OMS_TEST_MODE\" 2>/dev/null || printf 'ok')\n",
                "case \"$mode\" in\n",
                "  refused)\n",
                "    printf 'ssh: connect to host cluster port 22: Connection refused\\n' >&2\n",
                "    exit 255\n",
                "    ;;\n",
                "  otp)\n",
                "    printf 'Authenticated with partial success.\\nVerification code: \\n' >&2\n",
                "    exit 255\n",
                "    ;;\n",
                "  kbdint)\n",
                "    printf 'Permission denied (publickey,keyboard-interactive).\\n' >&2\n",
                "    exit 255\n",
                "    ;;\n",
                "  pw)\n",
                "    printf 'Enter pass",
                "word:\\n' >&2\n",
                "    exit 255\n",
                "    ;;\n",
                "  pubkey)\n",
                "    printf 'Permission denied (publickey).\\n' >&2\n",
                "    exit 255\n",
                "    ;;\n",
                "esac\n",
                "for last do :; done\n",
                "exec sh -c \"$last\"\n",
            );
            std::fs::write(&fake, script).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = isolated_child(
                "ssh_adapter::tests::control_persist_backoff_hourly_cap_interactive_auth_and_legacy_approval",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_SSH_SPEC3", "1")
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_ARGS", temp.0.join("args"))
            .env("OMS_TEST_CALLS", temp.0.join("calls"))
            .env("OMS_TEST_MODE", temp.0.join("mode"))
            .status()
            .unwrap();
            assert!(status.success());
            return;
        }
        use crate::value::*;
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let args_file = PathBuf::from(std::env::var_os("OMS_TEST_ARGS").unwrap());
        let calls_file = PathBuf::from(std::env::var_os("OMS_TEST_CALLS").unwrap());
        let mode_file = PathBuf::from(std::env::var_os("OMS_TEST_MODE").unwrap());
        let call_count = || -> usize {
            std::fs::read_to_string(&calls_file)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        };

        let repo = root.join("repo");
        let remote = root.join("remote");
        init_temp_git_repo(&repo);
        init_temp_git_repo(&remote);
        std::fs::create_dir_all(repo.join(".oms/runtime")).unwrap();
        let rs = repo.to_str().unwrap();
        let rd = remote.to_str().unwrap();

        // 1. Existing target file without `control_persist` loads, keeps its exact approval
        //    fingerprint, and uses default ControlPersist=1800; adding `control_persist`
        //    preserves the identical approval fingerprint.
        let legacy_identity = object([
            ("ssh_alias", string("legacy-host")),
            ("workdir", string("~/project")),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
        ]);
        let expected_fp = sha(canonical(&legacy_identity).unwrap());
        let mut legacy_row = object([
            ("ssh_alias", string("legacy-host")),
            ("workdir", string(rd)),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
        ]);
        let rd_fp = sha(canonical(&legacy_row).unwrap());
        legacy_row.set("execution_approved", b(true)).unwrap();
        legacy_row
            .set("approved_fingerprint", string(&rd_fp))
            .unwrap();
        let mut home_row = legacy_identity;
        home_row.set("execution_approved", b(true)).unwrap();
        home_row
            .set("approved_fingerprint", string(&expected_fp))
            .unwrap();
        assert!(crate::targets::approved(&home_row).unwrap());
        assert_eq!(
            crate::targets::control_persist(&home_row).unwrap(),
            crate::targets::DEFAULT_CONTROL_PERSIST
        );
        home_row.set("control_persist", number(3600)).unwrap();
        assert!(
            crate::targets::approved(&home_row).unwrap(),
            "adding control_persist must not invalidate existing approval fingerprint"
        );
        assert_eq!(crate::targets::control_persist(&home_row).unwrap(), 3600);

        let write_targets = |row: Value| {
            let reg = object([("schema", number(1)), ("targets", object([("gpu", row)]))]);
            std::fs::write(
                repo.join(".oms/runtime/targets.json"),
                canonical(&reg).unwrap(),
            )
            .unwrap();
        };
        write_targets(legacy_row.clone());

        let clock = TestClock::new(1_700_000_000_000, 500);
        let ok_default = invoke_clock(
            &[
                "exec",
                "--repo",
                rs,
                "--host",
                "legacy-host",
                "--remote-dir",
                rd,
                "--target",
                "gpu",
                "--",
                "true",
            ],
            &root,
            &clock,
        );
        assert_eq!(
            ok_default.code,
            0,
            "{}",
            String::from_utf8_lossy(&ok_default.stderr)
        );
        let captured = std::fs::read_to_string(&args_file).unwrap();
        assert!(captured.contains("ControlPersist=1800"), "{captured}");

        // 2. Custom `control_persist` (60, 7200, 14400) is honored; invalid values are refused.
        for valid_secs in [60u64, 7200, 14400] {
            let mut row = legacy_row.clone();
            row.set("control_persist", number(valid_secs)).unwrap();
            write_targets(row);
            clock.advance_ms(60_000);
            let res = invoke_clock(
                &[
                    "exec",
                    "--repo",
                    rs,
                    "--host",
                    "legacy-host",
                    "--remote-dir",
                    rd,
                    "--target",
                    "gpu",
                    "--",
                    "true",
                ],
                &root,
                &clock,
            );
            assert_eq!(res.code, 0, "{}", String::from_utf8_lossy(&res.stderr));
            let captured = std::fs::read_to_string(&args_file).unwrap();
            assert!(
                captured.contains(&format!("ControlPersist={valid_secs}")),
                "{captured}"
            );
        }
        let calls_before_invalid = call_count();
        for bad_json in [
            Value::Number("0".into()),
            Value::Number("59".into()),
            Value::Number("14401".into()),
            Value::Number("-5".into()),
            Value::Number("120.5".into()),
            string("1800"),
            b(true),
            Value::Null,
        ] {
            let mut row = legacy_row.clone();
            row.set("control_persist", bad_json.clone()).unwrap();
            write_targets(row);
            let bad_exec = invoke_clock(
                &[
                    "exec",
                    "--repo",
                    rs,
                    "--host",
                    "legacy-host",
                    "--remote-dir",
                    rd,
                    "--target",
                    "gpu",
                    "--",
                    "true",
                ],
                &root,
                &clock,
            );
            assert_eq!(bad_exec.code, 2, "{bad_json:?}");
            let bad_probe = invoke_clock(
                &[
                    "probe",
                    "--repo",
                    rs,
                    "--host",
                    "legacy-host",
                    "--target",
                    "gpu",
                ],
                &root,
                &clock,
            );
            assert_eq!(bad_probe.code, 2, "{bad_json:?}");
        }
        assert_eq!(
            call_count(),
            calls_before_invalid,
            "invalid control_persist must refuse before spawning ssh"
        );

        // 3. Interactive-auth stub failures produce the exact unsupported message, while
        //    non-interactive publickey rejection stays an ordinary SSH exit without it.
        let mut auth_row = object([
            ("ssh_alias", string("otp-host")),
            ("workdir", string(rd)),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
        ]);
        let auth_fp = sha(canonical(&auth_row).unwrap());
        auth_row.set("execution_approved", b(true)).unwrap();
        auth_row
            .set("approved_fingerprint", string(&auth_fp))
            .unwrap();
        let reg = object([
            ("schema", number(1)),
            ("targets", object([("otp-box", auth_row)])),
        ]);
        std::fs::write(
            repo.join(".oms/runtime/targets.json"),
            canonical(&reg).unwrap(),
        )
        .unwrap();

        for mode in ["otp", "kbdint", "pw"] {
            std::fs::write(&mode_file, mode).unwrap();
            clock.advance_ms(RECONNECT_WINDOW_MS + 1_000);
            let probe_fail = invoke_clock(
                &[
                    "probe", "--repo", rs, "--host", "otp-host", "--target", "otp-box",
                ],
                &root,
                &clock,
            );
            assert_eq!(probe_fail.code, 2, "{mode}");
            assert!(
                String::from_utf8_lossy(&probe_fail.stderr).contains(INTERACTIVE_AUTH_UNSUPPORTED),
                "{mode}: {}",
                String::from_utf8_lossy(&probe_fail.stderr)
            );

            clock.advance_ms(RECONNECT_WINDOW_MS + 1_000);
            let exec_fail = invoke_clock(
                &[
                    "exec",
                    "--repo",
                    rs,
                    "--host",
                    "otp-host",
                    "--remote-dir",
                    rd,
                    "--target",
                    "otp-box",
                    "--op-id",
                    &format!("op-auth-{mode}"),
                    "--",
                    "true",
                ],
                &root,
                &clock,
            );
            assert_eq!(exec_fail.code, 2, "{mode}");
            assert!(
                String::from_utf8_lossy(&exec_fail.stderr).contains(INTERACTIVE_AUTH_UNSUPPORTED),
                "{mode}: {}",
                String::from_utf8_lossy(&exec_fail.stderr)
            );
        }
        std::fs::write(&mode_file, "pubkey").unwrap();
        clock.advance_ms(RECONNECT_WINDOW_MS + 1_000);
        let pubkey_probe = invoke_clock(
            &[
                "probe", "--repo", rs, "--host", "otp-host", "--target", "otp-box",
            ],
            &root,
            &clock,
        );
        assert_eq!(pubkey_probe.code, 255);
        assert!(
            !String::from_utf8_lossy(&pubkey_probe.stderr).contains(INTERACTIVE_AUTH_UNSUPPORTED)
        );

        // 4. Exponential backoff with jitter (start 5 s, cap 10 min) using injectable clock.
        assert_eq!(compute_backoff_ms(1, 0), 5_000);
        assert_eq!(compute_backoff_ms(1, 500), 5_500);
        assert_eq!(compute_backoff_ms(2, 500), 11_000);
        assert_eq!(compute_backoff_ms(3, 500), 22_000);
        assert_eq!(compute_backoff_ms(10, 0), 600_000);
        assert_eq!(compute_backoff_ms(10, 500), 570_000);
        assert!(compute_backoff_ms(20, 1000) <= 600_000);

        let mut backoff_row = object([
            ("ssh_alias", string("backoff-host")),
            ("workdir", string(rd)),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
        ]);
        let bf_fp = sha(canonical(&backoff_row).unwrap());
        backoff_row.set("execution_approved", b(true)).unwrap();
        backoff_row
            .set("approved_fingerprint", string(&bf_fp))
            .unwrap();
        let mut cap_row = object([
            ("ssh_alias", string("cap-host")),
            ("workdir", string(rd)),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
        ]);
        let cap_fp = sha(canonical(&cap_row).unwrap());
        cap_row.set("execution_approved", b(true)).unwrap();
        cap_row
            .set("approved_fingerprint", string(&cap_fp))
            .unwrap();
        let reg = object([
            ("schema", number(1)),
            (
                "targets",
                object([("backoff-target", backoff_row), ("cap-target", cap_row)]),
            ),
        ]);
        std::fs::write(
            repo.join(".oms/runtime/targets.json"),
            canonical(&reg).unwrap(),
        )
        .unwrap();

        std::fs::write(&mode_file, "refused").unwrap();
        clock.set_jitter_permille(500); // failure 1 => 5_500 ms, failure 2 => 11_000 ms
        let f1 = invoke_clock(
            &[
                "probe",
                "--repo",
                rs,
                "--host",
                "backoff-host",
                "--target",
                "backoff-target",
            ],
            &root,
            &clock,
        );
        assert_eq!(f1.code, 255);
        let after_f1_calls = call_count();

        clock.advance_ms(5_499);
        let blocked_1 = invoke_clock(
            &[
                "probe",
                "--repo",
                rs,
                "--host",
                "backoff-host",
                "--target",
                "backoff-target",
            ],
            &root,
            &clock,
        );
        assert_eq!(blocked_1.code, 75);
        let err_1 = String::from_utf8_lossy(&blocked_1.stderr);
        assert!(
            err_1.contains("backoff-target") && err_1.contains("retry at 20"),
            "{err_1}"
        );
        assert_eq!(
            call_count(),
            after_f1_calls,
            "backoff must block before ssh"
        );

        clock.advance_ms(1); // total +5_500 ms -> failure 2 allowed, sets 11_000 ms backoff
        let f2 = invoke_clock(
            &[
                "probe",
                "--repo",
                rs,
                "--host",
                "backoff-host",
                "--target",
                "backoff-target",
            ],
            &root,
            &clock,
        );
        assert_eq!(f2.code, 255);
        assert_eq!(call_count(), after_f1_calls + 1);

        clock.advance_ms(10_999);
        assert_eq!(
            invoke_clock(
                &[
                    "probe",
                    "--repo",
                    rs,
                    "--host",
                    "backoff-host",
                    "--target",
                    "backoff-target",
                ],
                &root,
                &clock,
            )
            .code,
            75
        );
        clock.advance_ms(1); // total +11_000 ms -> recovery succeeds and resets failure count
        std::fs::write(&mode_file, "ok").unwrap();
        let recovered = invoke_clock(
            &[
                "probe",
                "--repo",
                rs,
                "--host",
                "backoff-host",
                "--target",
                "backoff-target",
            ],
            &root,
            &clock,
        );
        assert_eq!(recovered.code, 0);

        // 5. Hourly cap (at most 12 new connections per target per hour); reusing an open
        //    control connection does not count.
        for i in 0..12 {
            clock.advance_ms(10_000);
            let res = invoke_clock(
                &[
                    "probe",
                    "--repo",
                    rs,
                    "--host",
                    "cap-host",
                    "--target",
                    "cap-target",
                ],
                &root,
                &clock,
            );
            assert_eq!(
                res.code,
                0,
                "connection {i}: {}",
                String::from_utf8_lossy(&res.stderr)
            );
        }
        let calls_at_cap = call_count();
        clock.advance_ms(10_000);
        let over_cap = invoke_clock(
            &[
                "probe",
                "--repo",
                rs,
                "--host",
                "cap-host",
                "--target",
                "cap-target",
            ],
            &root,
            &clock,
        );
        assert_eq!(over_cap.code, 75);
        let cap_msg = String::from_utf8_lossy(&over_cap.stderr);
        assert!(
            cap_msg.contains("cap-target") && cap_msg.contains("retry at 20"),
            "{cap_msg}"
        );
        assert_eq!(call_count(), calls_at_cap, "capped call must not spawn ssh");

        // Reusing an open control connection succeeds without counting against the cap.
        let mux = bind_control_mux("cap-host", None);
        for _ in 0..5 {
            let reused = invoke_clock(
                &[
                    "probe",
                    "--repo",
                    rs,
                    "--host",
                    "cap-host",
                    "--target",
                    "cap-target",
                ],
                &root,
                &clock,
            );
            assert_eq!(
                reused.code,
                0,
                "{}",
                String::from_utf8_lossy(&reused.stderr)
            );
        }
        assert_eq!(call_count(), calls_at_cap + 5);
        drop(mux);

        // Once the control socket closes within the same hour, a 13th new connection is still refused.
        assert_eq!(
            invoke_clock(
                &[
                    "probe",
                    "--repo",
                    rs,
                    "--host",
                    "cap-host",
                    "--target",
                    "cap-target",
                ],
                &root,
                &clock,
            )
            .code,
            75
        );

        // Advancing past 1 hour from the earliest connection frees slots again.
        clock.advance_ms(RECONNECT_WINDOW_MS);
        let after_window = invoke_clock(
            &[
                "probe",
                "--repo",
                rs,
                "--host",
                "cap-host",
                "--target",
                "cap-target",
            ],
            &root,
            &clock,
        );
        assert_eq!(
            after_window.code,
            0,
            "{}",
            String::from_utf8_lossy(&after_window.stderr)
        );
    }

    #[test]
    fn round2_connect_detection_host_state_locking_corruption_and_target_isolation() {
        let pw = concat!("pass", "word");
        let pp = concat!("pass", "phrase");

        // Unit checks for narrowed connect-failure and interactive-auth classifiers.
        for ssh_err in [
            "ssh: connect to host box port 22: Connection refused\n",
            "ssh: connect to host box port 22: Connection timed out\n",
            "ssh: Could not resolve hostname box: Name or service not known\n",
            "Permission denied (publickey).\n",
            "Host key verification failed.\n",
            "Connection closed by 10.0.0.1 port 22\n",
            "kex_exchange_identification: Connection closed by remote host\n",
        ] {
            assert!(
                is_connect_failure(255, b"", ssh_err.as_bytes()),
                "expected connect failure: {ssh_err}"
            );
            assert!(
                !is_interactive_auth_failure(255, b"", ssh_err.as_bytes()),
                "must not be interactive-auth: {ssh_err}"
            );
        }
        for interactive_err in [
            format!("Permission denied (publickey,{pw}).\n"),
            "Permission denied (publickey,keyboard-interactive).\n".to_owned(),
            "Authenticated with partial success.\nVerification code: \n".to_owned(),
            format!("alice@box's {pw}: "),
            format!("Enter {pp} for key '/tmp/k': "),
            format!("read_{pp}: can't open /dev/tty: No such device or address\n"),
        ] {
            assert!(
                is_interactive_auth_failure(255, b"", interactive_err.as_bytes()),
                "expected interactive-auth: {interactive_err}"
            );
            assert!(
                is_connect_failure(255, b"", interactive_err.as_bytes()),
                "interactive-auth is also a connect failure: {interactive_err}"
            );
            // Never match on non-255 exit codes.
            assert!(
                !is_interactive_auth_failure(1, b"", interactive_err.as_bytes()),
                "exit 1 must never be interactive-auth: {interactive_err}"
            );
            assert!(
                !is_connect_failure(1, b"", interactive_err.as_bytes()),
                "exit 1 must never be connect failure: {interactive_err}"
            );
        }
        for remote_err in [
            "remote worker exited 255\n".to_owned(),
            format!("error: missing database {pw} in environment\n"),
            "otp check failed in application\n".to_owned(),
            format!("{pw}: invalid format in config\n"),
        ] {
            assert!(
                !is_interactive_auth_failure(255, b"", remote_err.as_bytes()),
                "remote stderr on 255 must not be interactive-auth: {remote_err}"
            );
            assert!(
                !is_connect_failure(255, b"", remote_err.as_bytes()),
                "remote stderr on 255 must not be connect failure: {remote_err}"
            );
            assert!(
                !is_interactive_auth_failure(1, b"", remote_err.as_bytes()),
                "remote stderr on 1 must not be interactive-auth: {remote_err}"
            );
        }

        if std::env::var_os("OMS_TEST_SSH_ROUND2").is_none() {
            let temp = TempDir::new().unwrap();
            let rt_dir = short_runtime_dir("r2");
            let fake_home = temp.0.join("home");
            let fake_xdg_cache = temp.0.join("xdg-cache");
            std::fs::create_dir_all(&fake_home).unwrap();
            std::fs::create_dir_all(&fake_xdg_cache).unwrap();
            let fake = temp.0.join("ssh");
            let script = concat!(
                "#!/bin/sh\n",
                "mode=$(cat \"$OMS_TEST_MODE\" 2>/dev/null || printf 'run')\n",
                "if [ -n \"${OMS_HOLD_LOCK_DIR:-}\" ]; then mkdir -p \"$OMS_HOLD_LOCK_DIR\" 2>/dev/null || true; fi\n",
                "case \"$mode\" in\n",
                "  refused)\n",
                "    printf 'ssh: connect to host target port 22: Connection refused\\n' >&2\n",
                "    exit 255\n",
                "    ;;\n",
                "  remote255)\n",
                "    printf 'application fatal error on remote host\\n' >&2\n",
                "    exit 255\n",
                "    ;;\n",
                "esac\n",
                "for last do :; done\n",
                "exec sh -c \"$last\"\n",
            );
            std::fs::write(&fake, script).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = isolated_child(
                "ssh_adapter::tests::round2_connect_detection_host_state_locking_corruption_and_target_isolation",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_SSH_ROUND2", "1")
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_MODE", temp.0.join("mode"))
            .env("HOME", &fake_home)
            .env("XDG_CACHE_HOME", &fake_xdg_cache)
            .env("OMS_LOCK_TIMEOUT", "1")
            .env_remove("OMS_LOCK_DIR")
            .status()
            .unwrap();
            assert!(status.success());

            // Finding 4: invalid lock settings (relative HOME) must return an error (exit 2),
            // never falling back to /tmp.
            let bad_lock_env = isolated_child(
                "ssh_adapter::tests::round2_connect_detection_host_state_locking_corruption_and_target_isolation",
                &temp,
                &rt_dir,
            )
            .env("OMS_TEST_SSH_ROUND2", "bad_home")
            .env("OMS_SSH_BIN", &fake)
            .env("HOME", "relative-home")
            .env_remove("OMS_LOCK_DIR")
            .status()
            .unwrap();
            assert!(bad_lock_env.success());
            return;
        }

        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        if std::env::var_os("OMS_TEST_SSH_ROUND2").as_deref() == Some(OsStr::new("bad_home")) {
            let err_resp = invoke(&["probe", "--host", "bad-home-host"], &root);
            assert_eq!(
                err_resp.code,
                2,
                "invalid lock settings must fail with exit 2: {}",
                String::from_utf8_lossy(&err_resp.stderr)
            );
            return;
        }

        use crate::value::*;
        let mode_file = PathBuf::from(std::env::var_os("OMS_TEST_MODE").unwrap());
        let fake_home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let fake_xdg_cache = PathBuf::from(std::env::var_os("XDG_CACHE_HOME").unwrap());
        let clock = TestClock::new(1_800_000_000_000, 500);

        // Finding 3: probe creates nothing inside the repository; state (0600) and private log
        // (0600) live in the 0700 host directory next to the control socket directory; all repos
        // and target names sharing host:port share one budget.
        let clean_repo = root.join("clean-repo");
        init_temp_git_repo(&clean_repo);
        let probe_ok = invoke_clock(
            &[
                "probe",
                "--repo",
                clean_repo.to_str().unwrap(),
                "--host",
                "shared-host",
            ],
            &root,
            &clock,
        );
        assert_eq!(probe_ok.code, 0);
        assert!(
            !clean_repo.join(".oms").exists(),
            "probe must create nothing inside the repository"
        );
        let shared_ctrl = ssh_control_path("shared-host", None).unwrap();
        let state_dir = ssh_state_dir(&shared_ctrl).unwrap();
        assert_eq!(
            state_dir.parent(),
            Path::new(&shared_ctrl).parent().unwrap().parent()
        );
        assert_eq!(
            std::fs::symlink_metadata(&state_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let state_file = state_dir.join(RECONNECT_STATE_FILE);
        let log_file = state_dir.join(PRIVATE_CONTROL_LOG_FILE);
        for f in [&state_file, &log_file] {
            assert_eq!(
                std::fs::symlink_metadata(f).unwrap().permissions().mode() & 0o077,
                0
            );
        }

        // Finding 4: lock_remote_op and ssh_connections_lock_path use $HOME/.cache/oh-my-setting/locks
        // (ignoring XDG_CACHE_HOME and never falling back to /tmp, even for /tmp state dirs).
        let lock_path = ssh_connections_lock_path(&state_dir, &root).unwrap();
        assert!(
            lock_path.starts_with(fake_home.join(".cache/oh-my-setting/locks")),
            "unexpected lock path: {}",
            lock_path.display()
        );
        let tmp_state = short_runtime_dir("lck");
        let tmp_lock_path = ssh_connections_lock_path(&tmp_state.0, &root).unwrap();
        drop(tmp_state);
        assert!(
            tmp_lock_path.starts_with(fake_home.join(".cache/oh-my-setting/locks")),
            "lock for /tmp state must not fall back to /tmp: {}",
            tmp_lock_path.display()
        );

        let repo_a = root.join("repo-a");
        let repo_b = root.join("repo-b");
        let remote_dir = root.join("remote-shared");
        init_temp_git_repo(&repo_a);
        init_temp_git_repo(&repo_b);
        init_temp_git_repo(&remote_dir);
        let rd = remote_dir.to_str().unwrap();
        for (r, tname) in [(&repo_a, "t-alpha"), (&repo_b, "t-beta")] {
            std::fs::create_dir_all(r.join(".oms/runtime")).unwrap();
            let mut row = object([
                ("ssh_alias", string("shared-host")),
                ("workdir", string(rd)),
                ("scheduler", string("none")),
                ("scratch_root", string("")),
            ]);
            let fp = sha(canonical(&row).unwrap());
            row.set("execution_approved", b(true)).unwrap();
            row.set("approved_fingerprint", string(&fp)).unwrap();
            let reg = object([("schema", number(1)), ("targets", object([(tname, row)]))]);
            std::fs::write(
                r.join(".oms/runtime/targets.json"),
                canonical(&reg).unwrap(),
            )
            .unwrap();
        }
        // Durable op in repo_a exercises lock_remote_op; XDG_CACHE_HOME must remain untouched.
        let op_ok = invoke_clock(
            &[
                "exec",
                "--repo",
                repo_a.to_str().unwrap(),
                "--host",
                "shared-host",
                "--port",
                "22",
                "--remote-dir",
                rd,
                "--target",
                "t-alpha",
                "--op-id",
                "op-lock-check",
                "--",
                "true",
            ],
            &root,
            &clock,
        );
        assert_eq!(op_ok.code, 0, "{}", String::from_utf8_lossy(&op_ok.stderr));
        assert!(
            !fake_xdg_cache.join("oh-my-setting").exists(),
            "lock_remote_op must keep $HOME/.cache/oh-my-setting/locks from origin/main"
        );
        // 2 connections already used on shared-host:22 (probe on clean_repo + exec on repo_a).
        // Use 10 more from repo_b under target t-beta -> 13th call across repos/targets hits cap!
        for _ in 0..10 {
            clock.advance_ms(1_000);
            let r = invoke_clock(
                &[
                    "probe",
                    "--repo",
                    repo_b.to_str().unwrap(),
                    "--host",
                    "shared-host",
                    "--target",
                    "t-beta",
                ],
                &root,
                &clock,
            );
            assert_eq!(r.code, 0);
        }
        let shared_capped = invoke_clock(
            &[
                "probe",
                "--repo",
                repo_a.to_str().unwrap(),
                "--host",
                "shared-host",
                "--target",
                "t-alpha",
            ],
            &root,
            &clock,
        );
        assert_eq!(shared_capped.code, 75);
        let parsed_state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
        assert_eq!(
            parsed_state["targets"]["shared-host:22"]["connect_timestamps_ms"]
                .as_array()
                .unwrap()
                .len(),
            12
        );

        // Finding 2 (integration):
        // (a) Reused connection with a failing remote command (exit 255 + stderr) leaves state unchanged.
        let mux = bind_control_mux("reused-fail-host", None);
        let state_before_reused = std::fs::read(&state_file).unwrap();
        std::fs::write(&mode_file, "refused").unwrap();
        let reused_fail = invoke_clock(
            &[
                "exec",
                "--repo",
                repo_a.to_str().unwrap(),
                "--host",
                "reused-fail-host",
                "--remote-dir",
                rd,
                "--",
                "false",
            ],
            &root,
            &clock,
        );
        assert_eq!(reused_fail.code, 255);
        assert_eq!(
            std::fs::read(&state_file).unwrap(),
            state_before_reused,
            "reused connection with failing remote command must leave state unchanged"
        );
        drop(mux);

        // (b) Non-reused connection where remote command exits 255 with ordinary stderr does not backoff;
        //     remote command printing "password" on stderr with exit 1 is not interactive-auth.
        std::fs::write(&mode_file, "remote255").unwrap();
        let remote_255 = invoke_clock(
            &[
                "exec",
                "--repo",
                repo_a.to_str().unwrap(),
                "--host",
                "remote255-host",
                "--remote-dir",
                rd,
                "--",
                "false",
            ],
            &root,
            &clock,
        );
        assert_eq!(remote_255.code, 255);
        let after_r255: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
        assert_eq!(
            after_r255["targets"]["remote255-host:22"]["backoff_until_ms"],
            0
        );
        assert_eq!(
            after_r255["targets"]["remote255-host:22"]["consecutive_failures"],
            0
        );

        std::fs::write(&mode_file, "run").unwrap();
        let pw_cmd = format!("printf '%s\\n' '{pw}' >&2; exit 1");
        let pw_exit_1 = invoke_clock(
            &[
                "exec",
                "--repo",
                repo_a.to_str().unwrap(),
                "--host",
                "pw-exit1-host",
                "--remote-dir",
                rd,
                "--",
                "sh",
                "-c",
                &pw_cmd,
            ],
            &root,
            &clock,
        );
        assert_eq!(
            pw_exit_1.code,
            1,
            "remote stderr containing password with exit 1 must not trigger interactive-auth: {}",
            String::from_utf8_lossy(&pw_exit_1.stderr)
        );
        let pw_json: serde_json::Value = serde_json::from_slice(&pw_exit_1.stdout).unwrap();
        assert_eq!(pw_json["stderr"], format!("{pw}\n"));

        // Finding 5: corrupt state tolerance (non-object entry, wrong-type fields, invalid JSON,
        // unreadable file).
        std::fs::write(
            &state_file,
            r#"{"schema":1,"targets":{"corrupt-entry:22":"not-an-object"}}"#,
        )
        .unwrap();
        assert_eq!(
            invoke_clock(&["probe", "--host", "corrupt-entry"], &root, &clock).code,
            0
        );
        std::fs::write(
            &state_file,
            r#"{"schema":1,"targets":{"corrupt-fields:22":{"connect_timestamps_ms":"bad","consecutive_failures":"bad","backoff_until_ms":true}}}"#,
        )
        .unwrap();
        assert_eq!(
            invoke_clock(&["probe", "--host", "corrupt-fields"], &root, &clock).code,
            0
        );
        let repaired: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
        assert_eq!(
            repaired["targets"]["corrupt-fields:22"]["connect_timestamps_ms"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            repaired["targets"]["corrupt-fields:22"]["backoff_until_ms"],
            0
        );

        std::fs::write(&state_file, b"{{not-valid-json").unwrap();
        assert_eq!(
            invoke_clock(&["probe", "--host", "corrupt-json"], &root, &clock).code,
            0
        );
        if rustix::process::geteuid().as_raw() != 0 {
            std::fs::set_permissions(&state_file, std::fs::Permissions::from_mode(0o000)).unwrap();
            assert_eq!(
                invoke_clock(&["probe", "--host", "unreadable-state"], &root, &clock).code,
                0
            );
            assert_eq!(
                std::fs::symlink_metadata(&state_file)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let log_contents = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            log_contents.contains("ssh_state_reset")
                && log_contents.contains("ssh_target_entry_reset"),
            "{log_contents}"
        );

        // Finding 6: record_ssh_outcome after remote command is best effort (on lock timeout/error,
        // logs privately and returns the command's real result and output).
        std::fs::write(
            &state_file,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "targets": {
                    "best-effort-host:22": {
                        "connect_timestamps_ms": [],
                        "consecutive_failures": 1,
                        "backoff_until_ms": 1_000
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let lock_dir_to_hold = ssh_connections_lock_path(&state_dir, &root).unwrap();
        let best_effort_cmd = format!(
            "mkdir -p '{}' && printf 'real-command-output'",
            lock_dir_to_hold.display()
        );
        let best_effort = invoke_clock(
            &[
                "exec",
                "--repo",
                repo_a.to_str().unwrap(),
                "--host",
                "best-effort-host",
                "--remote-dir",
                rd,
                "--",
                "sh",
                "-c",
                &best_effort_cmd,
            ],
            &root,
            &clock,
        );
        let _ = std::fs::remove_dir_all(&lock_dir_to_hold);
        assert_eq!(
            best_effort.code,
            0,
            "best-effort outcome recording must preserve command result: {}",
            String::from_utf8_lossy(&best_effort.stderr)
        );
        let be_val: serde_json::Value = serde_json::from_slice(&best_effort.stdout).unwrap();
        assert_eq!(be_val["stdout"], "real-command-output");
        assert!(
            std::fs::read_to_string(&log_file)
                .unwrap()
                .contains("record_ssh_outcome_failed")
        );

        // Finding 7: only the matching target row is validated; one bad row does not block SSH
        // to other hosts.
        let mut good_row = object([
            ("ssh_alias", string("good-host")),
            ("workdir", string(rd)),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
            ("control_persist", number(900)),
        ]);
        let good_fp = sha(canonical(&object([
            ("ssh_alias", string("good-host")),
            ("workdir", string(rd)),
            ("scheduler", string("none")),
            ("scratch_root", string("")),
        ]))
        .unwrap());
        good_row.set("execution_approved", b(true)).unwrap();
        good_row
            .set("approved_fingerprint", string(&good_fp))
            .unwrap();
        let bad_row = object([
            ("ssh_alias", string("broken-host")),
            ("workdir", string("/bad/..")),
            ("control_persist", number(5)),
        ]);
        let mixed_reg = object([
            ("schema", number(1)),
            ("targets", object([("good", good_row), ("broken", bad_row)])),
        ]);
        std::fs::write(
            repo_a.join(".oms/runtime/targets.json"),
            canonical(&mixed_reg).unwrap(),
        )
        .unwrap();
        assert_eq!(
            invoke_clock(
                &[
                    "probe",
                    "--repo",
                    repo_a.to_str().unwrap(),
                    "--host",
                    "good-host",
                    "--target",
                    "good",
                ],
                &root,
                &clock,
            )
            .code,
            0
        );
        assert_eq!(
            invoke_clock(
                &[
                    "probe",
                    "--repo",
                    repo_a.to_str().unwrap(),
                    "--host",
                    "good-host",
                ],
                &root,
                &clock,
            )
            .code,
            0
        );
        assert_eq!(
            invoke_clock(
                &[
                    "exec",
                    "--repo",
                    repo_a.to_str().unwrap(),
                    "--host",
                    "good-host",
                    "--remote-dir",
                    rd,
                    "--target",
                    "good",
                    "--",
                    "true",
                ],
                &root,
                &clock,
            )
            .code,
            0
        );
        assert_eq!(
            invoke_clock(
                &[
                    "exec",
                    "--repo",
                    repo_a.to_str().unwrap(),
                    "--host",
                    "good-host",
                    "--remote-dir",
                    rd,
                    "--",
                    "true",
                ],
                &root,
                &clock,
            )
            .code,
            0
        );
        assert_eq!(
            invoke_clock(
                &[
                    "probe",
                    "--repo",
                    repo_a.to_str().unwrap(),
                    "--host",
                    "broken-host",
                    "--target",
                    "broken",
                ],
                &root,
                &clock,
            )
            .code,
            2
        );
    }
}
