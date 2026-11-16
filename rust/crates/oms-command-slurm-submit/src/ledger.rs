//! Native equivalents of slurm_submit.py's four internal operations.
use crate::{json as j, storage};
use oms_context::Json;
use oms_json::Value;
use oms_lock::{Cancellation, Guard, Lock, Settings};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

pub type Result<T> = std::result::Result<T, String>;

/// Inputs to the legacy `key`, `status`, and `begin` operations. Relative
/// script paths are interpreted against `cwd`, never the process-global cwd.
pub struct KeyOptions<'a> {
    pub root: &'a Path,
    pub cwd: &'a Path,
    pub commit: &'a OsStr,
    pub script: Option<&'a Path>,
    pub args: &'a [OsString],
    pub custom_key: &'a OsStr,
}

pub struct Key {
    pub value: OsString,
    pub short: String,
    custom: bool,
    document: Value,
    cwd: PathBuf,
}
impl Key {
    /// Sorted, ASCII, default-spaced Python JSON, with one trailing newline.
    pub fn json(&self) -> Vec<u8> {
        line(&self.document)
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn path_at(cwd: &Path, path: &Path) -> PathBuf {
    cwd.join(path)
}

// os.path.realpath(strict=False): resolve existing symlinks, retaining a
// missing tail. Script reads still fail normally when the script is missing.
fn realpath(path: &Path, links: &mut usize) -> Result<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::RootDir => out = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => {
                out.push(n);
                if let Ok(target) = std::fs::read_link(&out) {
                    *links += 1;
                    if *links > 40 {
                        return Err("too many symbolic links".into());
                    }
                    let parent = out.parent().unwrap_or(Path::new("/"));
                    out = realpath(&parent.join(target), links)?;
                }
            }
            _ => return Err("invalid path".into()),
        }
    }
    Ok(out)
}
fn relative(path: &Path, root: &Path) -> Result<OsString> {
    let real = realpath(path, &mut 0)?;
    let base = realpath(root, &mut 0)?;
    if real == base {
        return Ok(".".into());
    }
    // Legacy tests `base + os.sep`, so a root of / uses the // prefix.
    if base != Path::new("/")
        && let Ok(tail) = real.strip_prefix(&base)
    {
        return Ok(tail.as_os_str().to_owned());
    }
    let mut result = OsString::from("<outside-repo>/");
    result.push(real.file_name().unwrap_or_default());
    Ok(result)
}

/// Decode a legacy NUL-separated args-file, including surrogateescaped bytes;
/// empty elements are dropped, exactly as `read_args` did.
pub fn read_args(path: &Path) -> std::io::Result<Vec<OsString>> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::fs::read(path)?
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| OsString::from_vec(p.to_vec()))
        .collect())
}

/// Derive all digests without creating state or running a process.
pub fn key(options: &KeyOptions<'_>) -> Result<Key> {
    let cwd = std::fs::canonicalize(options.cwd).map_err(|_| "cannot resolve working directory")?;
    let args: Vec<_> = options.args.iter().filter(|a| !a.is_empty()).collect();
    let args_json = || Value::Array(args.iter().map(|a| j::os(a)).collect());
    let (script, script_digest) =
        if let Some(path) = options.script.filter(|p| !p.as_os_str().is_empty()) {
            let path = path_at(&cwd, path);
            let mut file = std::fs::File::open(&path).map_err(|_| "cannot read script")?;
            if !file.metadata().map_err(|_| "cannot read script")?.is_file() {
                return Err("cannot read script".into());
            }
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let n = file.read(&mut buffer).map_err(|_| "cannot read script")?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            (
                relative(&path, options.root)?,
                format!("{:x}", hash.finalize()),
            )
        } else {
            (OsString::new(), String::new())
        };
    let cwd_rel = relative(&cwd, options.root)?;
    let document = j::object([
        ("v", j::integer(1)),
        ("script_sha256", j::text(&script_digest)),
        ("args", args_json()),
        ("cwd", j::os(&cwd_rel)),
        ("commit", j::os(options.commit)),
    ]);
    let custom = !options.custom_key.is_empty();
    let value = if custom {
        options.custom_key.to_owned()
    } else {
        digest(&j::encode(&document, true)).into()
    };
    let short = if custom {
        digest(value.as_bytes())[..12].to_owned()
    } else {
        value.to_string_lossy()[..12].to_owned()
    };
    let document = j::object([
        ("key", j::os(&value)),
        ("short", j::text(&short)),
        (
            "key_source",
            j::text(if custom { "custom" } else { "default" }),
        ),
        ("script", j::os(&script)),
        ("script_sha256", j::text(&script_digest)),
        (
            "args_sha256",
            j::text(&digest(&j::encode(&args_json(), false))),
        ),
        ("cwd", j::os(&cwd_rel)),
        ("commit", j::os(options.commit)),
    ]);
    Ok(Key {
        value,
        short,
        custom,
        document,
        cwd: crate::repository::caller_spelling(options.cwd)?,
    })
}

pub(crate) fn line(value: &Value) -> Vec<u8> {
    let mut bytes = j::encode(value, false);
    bytes.push(b'\n');
    bytes
}
pub(crate) fn f<'a>(row: &'a Json, name: &str) -> Option<&'a Json> {
    j::field(row, name)
}
pub(crate) fn kind(row: &Json, name: &str) -> bool {
    j::is_text(f(row, "type"), name.as_bytes())
}

// Python equality for JSON values, notably True == 1 for old/foreign attempt
// identifiers. Unhashable identifiers fail closed rather than freeing a key.
pub(crate) fn identity(value: Option<&Json>) -> Result<Vec<u8>> {
    match value {
        None | Some(Json::Null) => Ok(b"n".to_vec()),
        Some(Json::Bool(b)) => Ok(if *b { b"d1".to_vec() } else { b"d0".to_vec() }),
        Some(Json::Number(n)) => {
            let mut out = b"d".to_vec();
            let normalized = if !n.contains(['.', 'e', 'E'])
                && n.bytes().all(|b| b.is_ascii_digit() || b == b'-')
            {
                if n == "-0" { "0".into() } else { n.clone() }
            } else {
                let value = n.parse::<f64>().map_err(|_| "invalid attempt identifier")?;
                if value == 0.0 {
                    "0".into()
                } else if value.is_finite() && value.fract() == 0.0 {
                    format!("{value:.0}")
                } else {
                    format!("f{:x}", value.to_bits())
                }
            };
            out.extend_from_slice(normalized.as_bytes());
            Ok(out)
        }
        Some(Json::String(s)) => {
            let mut out = b"s".to_vec();
            out.extend_from_slice(s);
            Ok(out)
        }
        _ => Err("unhashable attempt identifier".into()),
    }
}
pub(crate) struct State<'a> {
    pub name: &'static str,
    pub row: Option<&'a Json>,
}
pub(crate) fn rows_for<'a>(rows: &'a [Json], key: &OsStr) -> Vec<&'a Json> {
    let matching = match j::os(key) {
        Value::String(ref s) => j::encode(&Value::String(s.clone()), true),
        _ => unreachable!(),
    };
    rows.iter()
        .filter(|r| {
            matches!(f(r, "key"), Some(Json::String(_)))
                && j::encode(&j::copy(f(r, "key")), true) == matching
        })
        .collect()
}
pub(crate) fn state<'a>(rows: &'a [Json], key: &OsStr) -> Result<State<'a>> {
    use std::collections::BTreeMap;
    let mine = rows_for(rows, key);
    let mut results = BTreeMap::new();
    for row in &mine {
        if kind(row, "submitted") || kind(row, "failed") {
            results.insert(identity(f(row, "attempt"))?, *row);
        }
    }
    let mut start = 0;
    for (index, row) in mine.iter().enumerate() {
        if kind(row, "intent")
            && j::truth(f(row, "forced"))
            && results
                .get(&identity(f(row, "attempt"))?)
                .is_none_or(|r| !kind(r, "failed"))
        {
            start = index;
        }
    }
    let window = &mine[start..];
    let window_attempts: std::collections::BTreeSet<Vec<u8>> = if start > 0 {
        window
            .iter()
            .filter(|r| kind(r, "intent"))
            .filter_map(|r| identity(f(r, "attempt")).ok())
            .collect()
    } else {
        std::collections::BTreeSet::new()
    };
    let in_window = |row: &Json| {
        start == 0
            || identity(f(row, "attempt"))
                .ok()
                .is_some_and(|att| window_attempts.contains(&att))
    };
    if let Some(row) = window
        .iter()
        .rev()
        .find(|r| kind(r, "submitted") && in_window(r))
    {
        return Ok(State {
            name: "submitted",
            row: Some(row),
        });
    }
    for row in window.iter().rev() {
        if kind(row, "intent") && !results.contains_key(&identity(f(row, "attempt"))?) {
            return Ok(State {
                name: "unknown",
                row: Some(row),
            });
        }
    }
    Ok(State {
        name: if window.iter().any(|r| kind(r, "failed") && in_window(r)) {
            "failed"
        } else {
            "none"
        },
        row: None,
    })
}

/// Read-only `status` document. Malformed and non-object lines are ignored;
/// unknown fields remain compatible and never become a second state store.
pub fn status(path: &Path, key: &Key) -> Result<Vec<u8>> {
    let rows = storage::load(path, &key.cwd).map_err(str::to_owned)?;
    let state = state(&rows, &key.value)?;
    let mut result = j::object([("state", j::text(state.name))]);
    if let Some(row) = state.row {
        for name in ["job_id", "ts", "attempt"] {
            j::put(
                &mut result,
                name,
                f(row, name).map_or_else(|| j::text(""), |v| j::copy(Some(v))),
            );
        }
    }
    Ok(line(&result))
}

fn describe(state: &State<'_>, rows: &[Json], short: &str) -> Vec<u8> {
    let row = state.row.unwrap();
    let mut out = Vec::new();
    let mut add = |bytes: &[u8]| out.extend_from_slice(bytes);
    if state.name == "submitted" {
        let job = f(row, "job_id");
        let id = if j::truth(job) {
            j::display(job)
        } else {
            b"(id not parsed from sbatch output)".to_vec()
        };
        add(b"duplicate: this key was already submitted as job ");
        add(&id);
        add(b" at ");
        add(&j::display(f(row, "ts")));
        add(b" (attempt ");
        add(&j::display(f(row, "attempt")));
        add(b")\ncheck it: oms run-reconcile list ; squeue -j ");
        let id = if j::truth(job) {
            j::display(job)
        } else {
            b"ID".to_vec()
        };
        add(&id);
        add(b" ; sacct -j ");
        add(&id);
        add(b"\n");
    } else {
        let intent = rows.iter().find(|r| {
            kind(r, "intent") && identity(f(r, "attempt")).ok() == identity(f(row, "attempt")).ok()
        });
        add(
            b"unknown: an earlier submission with this key may have gone through (intent recorded ",
        );
        add(&j::display(f(row, "ts")));
        add(b", no result row; OMS may have died before sbatch answered)\nlook for it ");
        if j::truth(intent.and_then(|r| f(r, "tag"))) {
            add(format!("by comment: squeue -u \"$USER\" -o '%i %j %k' | grep 'oms-key={short}' ; sacct -u \"$USER\" -o JobID,JobName,Comment | grep 'oms-key={short}'\n").as_bytes());
        } else {
            add(b"the comment could not be tagged for that attempt; match by job name or script with squeue -u \"$USER\" ; sacct -u \"$USER\" -o JobID,JobName,Comment\n");
        }
    }
    add(b"not submitting. This is duplicate detection inside this repository's .oms, not exactly-once across machines or clones.\nto submit anyway: --force-resubmit --reason TEXT\n");
    out
}

pub struct BeginOptions<'a> {
    pub force: bool,
    pub reason: &'a OsStr,
    /// Only the exact legacy value `yes` stores true.
    pub tag: &'a str,
    pub store_args: bool,
    pub args: &'a [OsString],
    pub cluster_name: Option<&'a str>,
    pub provenance: &'a Provenance,
}
/// Optional links from an intent to the orchestrator task that requested it.
#[derive(Default)]
pub struct Provenance {
    pub task_id: Option<String>,
    pub attempt_id: Option<String>,
    pub exec_snapshot_sha256: Option<String>,
}
pub(crate) const PROVENANCE_FIELDS: [&str; 3] = ["task_id", "attempt_id", "exec_snapshot_sha256"];
impl Provenance {
    fn fields(&self) -> [(&'static str, Option<&String>); 3] {
        [
            ("task_id", self.task_id.as_ref()),
            ("attempt_id", self.attempt_id.as_ref()),
            ("exec_snapshot_sha256", self.exec_snapshot_sha256.as_ref()),
        ]
    }
}
pub enum Begin {
    Started { attempt: String, json: Vec<u8> },
    Refused { diagnostic: Vec<u8> },
}
pub(crate) fn now() -> Result<String> {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "clock unavailable")?
        .as_secs();
    chrono::DateTime::from_timestamp(i64::try_from(seconds).map_err(|_| "clock out of range")?, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .ok_or_else(|| "clock out of range".into())
}
pub(crate) fn acquire(path: &Path, cwd: &Path, interrupted: &AtomicBool) -> Result<Guard> {
    storage::validate_path(path).map_err(str::to_owned)?;
    let mut settings = Settings::from_env().map_err(|e| e.to_string())?;
    settings.cwd = cwd.to_path_buf();
    let lock = Lock::for_file(path, settings).map_err(|e| e.to_string())?;
    let done = AtomicBool::new(false);
    let cancellation = Cancellation::default();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if interrupted.load(Ordering::Relaxed) {
                    cancellation.cancel();
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        let result = lock.acquire(Some(&cancellation));
        done.store(true, Ordering::Release);
        result.map_err(|e| e.to_string())
    })
}

/// Atomically check and append the intent under the *existing* Bash-compatible
/// file lock. Do not hold a legacy lock around this function (non-reentrant).
pub fn begin(
    path: &Path,
    cwd: &Path,
    key: &Key,
    options: &BeginOptions<'_>,
    interrupted: &AtomicBool,
) -> Result<Begin> {
    if key.custom && !crate::clean(key.value.as_bytes()) {
        return Err("--key looks sensitive; use an opaque id".into());
    }
    let guard = acquire(path, cwd, interrupted)?;
    let rows = storage::load(path, cwd).map_err(str::to_owned)?;
    let state = state(&rows, &key.value)?;
    if matches!(state.name, "submitted" | "unknown") && !options.force {
        return Ok(Begin::Refused {
            diagnostic: describe(&state, &rows, &key.short),
        });
    }
    let mut random = [0u8; 6];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut random))
        .map_err(|_| "cannot generate attempt id")?;
    let attempt: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut row = j::object([
        ("schema", j::integer(1)),
        ("type", j::text("intent")),
        ("key", j::os(&key.value)),
        ("attempt", j::text(&attempt)),
        ("ts", j::text(&now()?)),
        ("tag", Value::Bool(options.tag == "yes")),
    ]);
    if let Value::Object(doc) = &key.document {
        for (name, value) in doc {
            let name = String::from_utf8(name.utf8_with_replacement()).unwrap();
            if name != "short" {
                j::put(&mut row, &name, j::copy_value(value));
            }
        }
    }
    for (name, value) in options.provenance.fields() {
        if let Some(value) = value {
            j::put(&mut row, name, j::text(value));
        }
    }
    if let Some(cluster) = options.cluster_name.filter(|c| !c.is_empty()) {
        j::put(&mut row, "cluster_name", j::text(cluster));
    }
    if options.store_args && crate::clean(&crate::joined(options.args)) {
        j::put(
            &mut row,
            "args",
            Value::Array(
                options
                    .args
                    .iter()
                    .filter(|a| !a.is_empty())
                    .map(|a| j::os(a))
                    .collect(),
            ),
        );
    }
    if options.force {
        j::put(&mut row, "forced", Value::Bool(true));
        j::put(
            &mut row,
            "reason",
            if crate::clean(options.reason.as_bytes()) {
                j::os(options.reason)
            } else {
                j::text("[withheld: looks sensitive]")
            },
        );
        j::put(&mut row, "overrides", j::text(state.name));
    }
    if interrupted.load(Ordering::Relaxed) {
        return Err("interrupted".into());
    }
    guard.check().map_err(|e| e.to_string())?;
    storage::append(path, cwd, &line(&row)).map_err(str::to_owned)?;
    let json = line(&j::object([
        ("attempt", j::text(&attempt)),
        ("short", j::text(&key.short)),
        ("key", j::os(&key.value)),
    ]));
    Ok(Begin::Started { attempt, json })
}

pub enum Outcome<'a> {
    Failed(i32),
    Submitted {
        job_id: &'a OsStr,
        cluster_name: &'a OsStr,
    },
}
/// Append a matching result using the same lock as `begin`. An absent result
/// deliberately leaves an unknown intent; no scheduler query or retry occurs.
/// An Outcome is already known, so cancellation cannot discard it. Acquisition
/// still uses the ambient bounded lock timeout and the existing shared protocol.
pub fn finish(
    path: &Path,
    cwd: &Path,
    key: &OsStr,
    attempt: &OsStr,
    outcome: Outcome<'_>,
    _interrupted: &AtomicBool,
) -> Result<()> {
    let persist = AtomicBool::new(false);
    let guard = acquire(path, cwd, &persist)?;
    let mut row = j::object([
        ("schema", j::integer(1)),
        ("key", j::os(key)),
        ("attempt", j::os(attempt)),
        ("ts", j::text(&now()?)),
    ]);
    match outcome {
        Outcome::Failed(code) => {
            j::put(&mut row, "type", j::text("failed"));
            j::put(&mut row, "exit", j::integer(i64::from(code)));
        }
        Outcome::Submitted {
            job_id,
            cluster_name,
        } => {
            j::put(&mut row, "type", j::text("submitted"));
            j::put(&mut row, "job_id", j::os(job_id));
            j::put(&mut row, "cluster_name", j::os(cluster_name));
        }
    }
    guard.check().map_err(|e| e.to_string())?;
    storage::append(path, cwd, &line(&row)).map_err(str::to_owned)
}
