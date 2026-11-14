#![forbid(unsafe_code)]
//! Repository-local submission duplicate detection; not exactly-once across
//! machines or clones. See README.md for streaming and internal helper adapters.
mod json;
pub mod ledger;
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
fn parse(args: &[OsString]) -> Result<Option<Options>, Vec<u8>> {
    let mut options = Options {
        custom: OsString::new(),
        reason: OsString::new(),
        dry: false,
        force: false,
        command: Vec::new(),
    };
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_bytes();
        match arg {
            b"-h" | b"--help" => return Ok(None),
            b"--" => {
                options.command = args[index + 1..].to_vec();
                break;
            }
            b"--dry-run" => options.dry = true,
            b"--force-resubmit" => options.force = true,
            b"--key" | b"--reason" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| option_error(b"", OsStr::from_bytes(arg), b" needs a value"))?;
                if arg == b"--key" {
                    options.custom = value.clone();
                } else {
                    options.reason = value.clone();
                }
            }
            _ if arg.starts_with(b"--key=") => {
                options.custom = OsString::from_vec(arg[6..].to_vec())
            }
            _ if arg.starts_with(b"--reason=") => {
                options.reason = OsString::from_vec(arg[9..].to_vec())
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
    Ok(Some(options))
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
    let options = match parse(args) {
        Ok(Some(options)) => options,
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
    match execute(options, cwd, source_root, supplied, interrupted, out, err) {
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
    let ledger = std::env::var_os("OMS_SLURM_SUBMITS_FILE")
        .filter(|v| !v.is_empty())
        .map(|v| supplied_path(&logical, &v))
        .unwrap_or_else(|| supplied_path(&repository.root, OsStr::new(".oms/slurm-submits.jsonl")));
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
    match scheduler::run(
        &options.command[0],
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
