#![forbid(unsafe_code)]
//! Repository-local submission duplicate detection; not exactly-once across
//! machines or clones. See README.md for streaming and internal helper adapters.
mod json;
pub mod ledger;
mod reconcile;
pub mod repository;
mod scheduler;
mod storage;

use ledger::{Begin, BeginOptions, KeyOptions, Outcome};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const USAGE: &str = include_str!("usage.txt");

/// Every leaf argument belongs to this verb after the parent router matches it.
pub fn owns(_args: &[OsString]) -> bool {
    true
}

/// Buffered compatibility adapter for existing oms-command-* routers. The
/// public router should choose `run_with_source_streaming` to retain live output.
pub fn run_with_source(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with_source_streaming(args, cwd, source_root, interrupted, &mut out, &mut err);
    (code, out, err)
}

/// Stream both scheduler pipes to the caller without creating capture files.
/// The parent alone validates `source_root` and installs cancellation handling.
pub fn run_with_source_streaming(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    adapter(args, cwd, source_root, None, interrupted, out, err)
}

/// Equivalent streaming adapter reusing fresh parent-validated Git metadata
/// without repeating this command's root/HEAD observations or changing keys.
pub fn run_with_repository(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    repository: &repository::Repository,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    adapter(
        args,
        cwd,
        source_root,
        Some(repository),
        interrupted,
        out,
        err,
    )
}

struct Options {
    custom: OsString,
    reason: OsString,
    dry: bool,
    force: bool,
    command: Vec<OsString>,
    provenance: ledger::Provenance,
}
enum Parsed {
    Submit(Options),
    Reconcile(reconcile::Options),
}
fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
fn sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
// An explicit flag must be valid; an ambient environment value that is not is
// ignored rather than failing an unrelated submission.
fn provenance_value(
    flag: Option<OsString>,
    variable: &str,
    valid: fn(&str) -> bool,
) -> Option<String> {
    flag.and_then(|v| v.into_string().ok()).or_else(|| {
        std::env::var(variable)
            .ok()
            .filter(|v| !v.is_empty() && valid(v))
    })
}
fn fail(message: impl AsRef<[u8]>) -> Vec<u8> {
    let mut bytes = b"error: ".to_vec();
    bytes.extend_from_slice(message.as_ref());
    bytes.push(b'\n');
    bytes
}
fn option_error(prefix: &[u8], arg: &OsStr, suffix: &[u8]) -> Vec<u8> {
    let mut bytes = prefix.to_vec();
    bytes.extend_from_slice(arg.as_bytes());
    bytes.extend_from_slice(suffix);
    fail(bytes)
}
fn parse(args: &[OsString]) -> Result<Option<Parsed>, Vec<u8>> {
    if args.first().is_some_and(|a| a == "reconcile") {
        return Ok(reconcile::parse(&args[1..])?.map(Parsed::Reconcile));
    }
    let mut options = Options {
        custom: OsString::new(),
        reason: OsString::new(),
        dry: false,
        force: false,
        command: Vec::new(),
        provenance: ledger::Provenance::default(),
    };
    let (mut key, mut idempotency, mut task, mut attempt, mut snapshot) =
        (None, None, None, None, None);
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_bytes();
        let (name, inline) = match arg.iter().position(|&b| b == b'=') {
            Some(at) if arg.starts_with(b"--") => (&arg[..at], Some(&arg[at + 1..])),
            _ => (arg, None),
        };
        match name {
            b"-h" | b"--help" => return Ok(None),
            b"--" => {
                options.command = args[index + 1..].to_vec();
                break;
            }
            b"--dry-run" if inline.is_none() => options.dry = true,
            b"--force-resubmit" if inline.is_none() => options.force = true,
            b"--key" | b"--reason" | b"--idempotency-key" | b"--task-id" | b"--attempt-id"
            | b"--exec-snapshot" => {
                let value = match inline {
                    Some(value) => OsString::from_vec(value.to_vec()),
                    None => {
                        index += 1;
                        args.get(index)
                            .ok_or_else(|| {
                                option_error(b"", OsStr::from_bytes(name), b" needs a value")
                            })?
                            .clone()
                    }
                };
                let (slot, valid): (&mut Option<OsString>, fn(&str) -> bool) = match name {
                    b"--key" => (&mut key, |_| true),
                    b"--reason" => {
                        options.reason = value;
                        index += 1;
                        continue;
                    }
                    b"--idempotency-key" => (&mut idempotency, |_| true),
                    b"--task-id" => (&mut task, safe_id),
                    b"--attempt-id" => (&mut attempt, safe_id),
                    _ => (&mut snapshot, sha256_hex),
                };
                if !valid(&value.to_string_lossy()) {
                    return Err(option_error(
                        b"invalid value for ",
                        OsStr::from_bytes(name),
                        b" (safe ASCII identifier, or 64 lowercase hex for --exec-snapshot)",
                    ));
                }
                *slot = Some(value);
            }
            _ => {
                return Err(option_error(
                    b"unknown option: ",
                    &args[index],
                    b" (the command goes after --)",
                ));
            }
        }
        index += 1;
    }
    if let (Some(a), Some(b)) = (&key, &idempotency)
        && a != b
    {
        return Err(fail(b"--key and --idempotency-key disagree; pass one"));
    }
    options.custom = key.or(idempotency).unwrap_or_default();
    options.provenance = ledger::Provenance {
        task_id: provenance_value(task, "OMS_TASK_ID", safe_id),
        attempt_id: provenance_value(attempt, "OMS_ATTEMPT_ID", safe_id),
        exec_snapshot_sha256: provenance_value(snapshot, "OMS_EXEC_SNAPSHOT_SHA256", sha256_hex),
    };
    let Some(executable) = options.command.first() else {
        return Err(fail(
            b"usage: slurm-submit.sh [options] -- sbatch ARGS... SCRIPT",
        ));
    };
    if executable.as_bytes().rsplit(|&b| b == b'/').next() != Some(b"sbatch".as_slice()) {
        return Err(option_error(
            b"the command must be sbatch (got ",
            executable,
            b")",
        ));
    }
    if options.command.len() == 1 {
        return Err(fail(b"sbatch needs arguments and a script"));
    }
    if options.force && options.reason.is_empty() {
        return Err(fail(b"--force-resubmit requires --reason TEXT"));
    }
    Ok(Some(Parsed::Submit(options)))
}

pub(crate) fn joined(args: &[OsString]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            bytes.push(b' ');
        }
        bytes.extend_from_slice(arg.as_bytes());
    }
    bytes
}
pub(crate) fn clean(bytes: &[u8]) -> bool {
    let environment: Vec<_> = std::env::vars_os().collect();
    let environment: Vec<_> = environment
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect();
    let mut bytes = bytes.to_vec();
    bytes.push(b'\n');
    oms_scrub::scan(&bytes, &environment).is_ok_and(|hits| hits.is_empty())
}

fn adapter(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    supplied: Option<&repository::Repository>,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let parsed = match parse(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            return if out.write_all(USAGE.as_bytes()).is_ok() {
                0
            } else {
                1
            };
        }
        Err(diagnostic) => {
            let _ = err.write_all(&diagnostic);
            return 2;
        }
    };
    if interrupted.load(Ordering::Relaxed) {
        let _ = err.write_all(b"error: interrupted\n");
        return 130;
    }
    let result = match parsed {
        Parsed::Submit(options) => {
            execute(options, cwd, source_root, supplied, interrupted, out, err)
        }
        Parsed::Reconcile(options) => reconcile::run(options, cwd, supplied, interrupted, out),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            if interrupted.load(Ordering::Relaxed) {
                let _ = err.write_all(b"error: interrupted\n");
                130
            } else {
                let _ = err.write_all(&fail(message.as_bytes()));
                2
            }
        }
    }
}

fn comment_in_script(path: &Path) -> bool {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(pattern) = regex::bytes::Regex::new(
        r"(?-u)^#SBATCH[\t\n\x0b\x0c\r ]+([^#]*[\t\n\x0b\x0c\r ])?--comment",
    ) else {
        return false;
    };
    std::io::BufReader::new(file)
        .split(b'\n')
        .filter_map(Result::ok)
        .any(|line| pattern.is_match(&line))
}
fn write(out: &mut dyn Write, bytes: &[u8]) -> Result<(), String> {
    out.write_all(bytes)
        .and_then(|()| out.flush())
        .map_err(|_| "cannot write command output".into())
}
fn supplied_path(cwd: &Path, value: &OsStr) -> PathBuf {
    if Path::new(value).is_absolute() {
        return PathBuf::from(value);
    }
    let mut raw = cwd.as_os_str().as_bytes().to_vec();
    raw.push(b'/');
    raw.extend_from_slice(value.as_bytes());
    PathBuf::from(OsString::from_vec(raw))
}
fn ledger_path(logical: &Path, repository: &repository::Repository) -> PathBuf {
    std::env::var_os("OMS_SLURM_SUBMITS_FILE")
        .filter(|v| !v.is_empty())
        .map(|v| supplied_path(logical, &v))
        .unwrap_or_else(|| supplied_path(&repository.root, OsStr::new(".oms/slurm-submits.jsonl")))
}
pub(crate) fn safe_cluster(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-,".contains(&b))
}

pub(crate) fn cluster_from_args(args: &[OsString]) -> Option<String> {
    let mut i = 0;
    let mut found = None;
    while i < args.len() {
        let s = args[i].to_str()?;
        let val = if s == "-M" || s == "--clusters" {
            i += 1;
            args.get(i).and_then(|v| v.to_str())
        } else if let Some(rest) = s.strip_prefix("--clusters=") {
            Some(rest)
        } else if s.len() > 2 && s.starts_with("-M") && !s.starts_with("-M-") {
            Some(&s[2..])
        } else {
            None
        };
        if let Some(c) = val
            && safe_cluster(c)
        {
            found = Some(c.to_owned());
        }
        i += 1;
    }
    found
}

fn execute(
    mut options: Options,
    cwd: &Path,
    source_root: &Path,
    supplied: Option<&repository::Repository>,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    let logical = repository::caller_spelling(cwd)?;
    let caller = std::fs::canonicalize(cwd).map_err(|_| "cannot resolve working directory")?;
    let args = &options.command[1..];
    let last = Path::new(args.last().unwrap());
    let script: Option<PathBuf> = caller.join(last).is_file().then(|| last.to_owned());
    if script.is_none() && options.custom.is_empty() {
        return Err("the last argument must be the job script (a readable file); use --key for a --wrap job".into());
    }
    if !options.custom.is_empty() && !clean(options.custom.as_bytes()) {
        return Err("--key looks sensitive; use an opaque id".into());
    }
    let store_args = clean(&joined(args));
    if !options.reason.is_empty() && !clean(options.reason.as_bytes()) {
        options.reason = "[withheld: looks sensitive]".into();
    }
    let discovered;
    let repository = match supplied {
        Some(repository) => repository,
        None => {
            discovered = repository::discover_with_interrupt(&logical, interrupted)?;
            &discovered
        }
    };
    let key = ledger::key(&KeyOptions {
        root: &repository.root,
        cwd: &logical,
        commit: &repository.commit,
        script: script.as_deref(),
        args,
        custom_key: &options.custom,
    })
    .map_err(|_| "could not derive the key")?;
    if let Some(script) = &script {
        let (_, advice, errors) = oms_command_slurm_lint::run_with_source(
            &[script.as_os_str().to_owned()],
            &caller,
            source_root,
            interrupted,
        );
        write(err, &advice)?;
        write(err, &errors)?;
    }
    let already_commented = args
        .iter()
        .any(|a| a == "--comment" || a.as_bytes().starts_with(b"--comment="))
        || script
            .as_ref()
            .is_some_and(|s| comment_in_script(&caller.join(s)));
    let tag = if already_commented { "set" } else { "yes" };
    let mut run_args = Vec::new();
    if !already_commented {
        run_args.push("--comment".into());
        run_args.push(format!("oms-key={}", key.short).into());
    }
    run_args.extend_from_slice(args);
    let ledger = ledger_path(&logical, repository);
    storage::validate_path(&ledger).map_err(str::to_owned)?;
    if options.dry {
        let status = ledger::status(&ledger, &key)?;
        let status = oms_context::parse_json(&status).map_err(str::to_owned)?;
        write(out, b"key: ")?;
        write(out, key.value.as_bytes())?;
        write(out, b"\nearlier record: ")?;
        write(out, &json::display(json::field(&status, "state")))?;
        write(out, b"\nwould run: ")?;
        write(out, options.command[0].as_bytes())?;
        write(out, b" ")?;
        write(out, &joined(&run_args))?;
        if already_commented {
            write(
                out,
                b"   (comment already set by the caller; the attempt is not tagged)",
            )?;
        }
        write(out, b"\ndry run: nothing was run or written\n")?;
        return Ok(0);
    }
    if interrupted.load(Ordering::Relaxed) {
        return Err("interrupted".into());
    }
    let cluster_name = cluster_from_args(args);
    storage::ensure_ignore(&ledger, &logical, interrupted);
    let attempt = match ledger::begin(
        &ledger,
        &logical,
        &key,
        &BeginOptions {
            force: options.force,
            reason: &options.reason,
            tag,
            store_args,
            args,
            cluster_name: cluster_name.as_deref(),
            provenance: &options.provenance,
        },
        interrupted,
    )
    .map_err(|error| {
        let code = if error.starts_with("file lock: Invalid") {
            2
        } else if error.starts_with("file lock:") {
            75
        } else {
            1
        };
        format!("could not record the submission intent (status {code})")
    })? {
        Begin::Refused { diagnostic } => {
            write(err, &diagnostic)?;
            return Ok(3);
        }
        Begin::Started { attempt, .. } => attempt,
    };
    // A bare `sbatch` may be redirected (tests, wrapper installs); an explicit
    // path was already validated by name and is used as given.
    let program = match std::env::var_os("OMS_SLURM_SBATCH_BIN") {
        Some(bin) if options.command[0] == "sbatch" && !bin.is_empty() => bin,
        _ => options.command[0].clone(),
    };
    match scheduler::run(
        &program,
        &run_args,
        &caller,
        scheduler::Input::Caller,
        interrupted,
        out,
        err,
    ) {
        scheduler::Execution::Interrupted => {
            write(err, b"error: interrupted; submission outcome is unknown\n")?;
            Ok(130)
        }
        scheduler::Execution::Unknown => {
            write(err, b"warning: scheduler output or process status was lost; the next attempt will report unknown\n")?;
            Ok(1)
        }
        scheduler::Execution::Answered {
            code,
            job_id,
            cluster_name,
            output_complete,
        } => {
            // Persist a known answer before further diagnostics, irrespective
            // of a late signal or failure of an output writer.
            let outcome = if code == 0 {
                Outcome::Submitted {
                    job_id: &job_id,
                    cluster_name: &cluster_name,
                }
            } else {
                Outcome::Failed(code)
            };
            let recorded = ledger::finish(
                &ledger,
                &logical,
                &key.value,
                OsStr::new(&attempt),
                outcome,
                interrupted,
            )
            .is_ok();
            let mut diagnostic = Vec::new();
            if !output_complete {
                diagnostic.extend_from_slice(b"warning: scheduler outcome is known but its output was not completely forwarded\n");
            }
            if !recorded {
                diagnostic.extend_from_slice(if code == 0 {
                    b"warning: the job was submitted but its row could not be written; the next attempt may report unknown\n"
                } else {
                    b"warning: sbatch failed and the failed row could not be written; the next attempt may report unknown\n"
                });
            }
            if code != 0 {
                diagnostic.extend_from_slice(if recorded { format!("failed: sbatch exited {code}; recorded, a retry with this key is allowed\n") }
                    else { format!("failed: sbatch exited {code}; ledger result was not confirmed durable; check the ledger before retrying\n") }
                    .as_bytes());
            } else if !job_id.is_empty() {
                diagnostic.extend_from_slice(if recorded {
                    b"recorded: job "
                } else {
                    b"submitted: job "
                });
                diagnostic.extend_from_slice(job_id.as_bytes());
                diagnostic.extend_from_slice(b" key ");
                diagnostic.extend_from_slice(key.value.as_bytes());
                diagnostic.extend_from_slice(b" (check with: oms run-reconcile list)\n");
            } else {
                diagnostic.extend_from_slice(if recorded {
                    b"recorded: submitted, but no job id was found in sbatch's output (key "
                } else {
                    b"submitted, but no job id was found in sbatch's output (key "
                });
                diagnostic.extend_from_slice(key.value.as_bytes());
                diagnostic.extend_from_slice(b")\n");
            }
            let _ = write(err, &diagnostic);
            Ok(if code == 3 { 1 } else { code })
        }
    }
}
