#![forbid(unsafe_code)]

use store::actions;
mod cli;
use store::waste;
mod diagnostic;
mod emit;
mod metrics;
mod report;
pub mod store;
mod time;
mod value;

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use value::V;

const INDEX_LIMIT: usize = 256 * 1024 * 1024;
const ROW_LIMIT: usize = 1024 * 1024;
const ACTIONS: [&str; 14] = [
    "list",
    "latest",
    "latest-run",
    "failures",
    "unresolved",
    "telemetry",
    "resolve",
    "resolve-superseded",
    "resolve-recovered",
    "validate",
    "migrate",
    "salvage",
    "prune",
    "import",
];

struct Options {
    repo: Option<PathBuf>,
    index: Option<PathBuf>,
    action: String,
    limit: usize,
    by: Option<String>,
    since: Option<String>,
    until: Option<String>,
    json: bool,
    help: bool,
    waste: bool,
    top: usize,
    events: Vec<String>,
    reason: String,
    files: bool,
    stale: bool,
    dry: bool,
    apply: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            repo: None,
            index: None,
            action: "list".into(),
            limit: 20,
            by: None,
            since: None,
            until: None,
            json: false,
            help: false,
            waste: false,
            top: 10,
            events: Vec::new(),
            reason: String::new(),
            files: false,
            stale: false,
            dry: false,
            apply: false,
        }
    }
}
fn fail(message: &str) -> (i32, Vec<u8>, Vec<u8>) {
    (2, Vec::new(), format!("error: {message}\n").into_bytes())
}
fn arg_text(arg: &std::ffi::OsStr) -> String {
    arg.to_string_lossy().into_owned()
}
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}
fn repository(options: &Options, caller: &Path) -> Result<(PathBuf, PathBuf), String> {
    let caller = if caller.is_absolute() {
        caller.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| "cannot resolve logical repo path")?
            .join(caller)
    };
    let supplied = options.repo.as_deref().unwrap_or(&caller);
    let logical = if supplied.is_absolute() {
        normalize(supplied)
    } else {
        normalize(&caller.join(supplied))
    };
    let physical =
        std::fs::canonicalize(&logical).map_err(|_| "cannot resolve logical repo path")?;
    if !physical.is_dir() {
        return Err("cannot resolve logical repo path".into());
    }
    let root = std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_PREFIX")
        .arg("-C")
        .arg(&physical)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|r| r.status.success())
        .and_then(|r| {
            let bytes = r.stdout.strip_suffix(b"\n").unwrap_or(&r.stdout);
            let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
            path.is_absolute().then_some(path)
        })
        .unwrap_or_else(|| physical.clone());
    let mut logical_root = logical;
    if let Ok(suffix) = physical.strip_prefix(&root) {
        for _ in suffix.components() {
            logical_root.pop();
        }
    }
    Ok((root, logical_root))
}
fn read_index(
    root: &oms_safe_fs::Root,
    logical_root: &Path,
    index: Option<&Path>,
    interrupted: &AtomicBool,
) -> Result<Vec<V>, String> {
    let supplied = index
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new(".oms/artifacts/index.jsonl"));
    let target = if supplied.is_absolute() {
        let normalized = normalize(supplied);
        if let Ok(relative) = normalized.strip_prefix(logical_root) {
            root.physical_root().join(relative)
        } else {
            normalized
        }
    } else {
        normalize(&root.physical_root().join(supplied))
    };
    let relative=target.strip_prefix(root.physical_root()).map_err(|_| "artifact index must stay inside the physical repository\nerror: artifact index path is unsafe")?;
    if relative.as_os_str().is_empty() {
        return Err("artifact index must stay inside the physical repository\nerror: artifact index path is unsafe".into());
    }
    let policy = oms_safe_fs::Policy {
        single_link: true,
        owner: None,
        max_components: None,
    };
    // Preserve the public canonicalization errors, then use descriptor-relative
    // validation for the actual read so these preliminary checks grant no trust.
    use std::os::unix::fs::MetadataExt;
    let mut current = root.physical_root().to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        current.push(part.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(info) if parts.peek().is_some() && !info.is_dir()=>return Err("artifact index parent components must be real directories\nerror: artifact index path is unsafe".into()),
            Ok(info) if parts.peek().is_none() && (!info.is_file() || info.nlink()!=1)=>return Err("artifact index must be a regular non-symlink, non-hard-linked file\nerror: artifact index path is unsafe".into()),
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>break,
            _=>{},
        }
    }
    let bytes = match root.read_path(relative, INDEX_LIMIT, policy) {
        Ok(bytes) => bytes,
        Err(oms_safe_fs::Error::Missing) => Vec::new(),
        Err(oms_safe_fs::Error::Oversize) => {
            return Err(format!(
                "artifact index exceeds the {INDEX_LIMIT}-byte recovery limit"
            ));
        }
        Err(_) => return Err("artifact index path is unsafe or unreadable".into()),
    };
    parse_snapshot(&bytes, interrupted)
}
fn parse_snapshot(bytes: &[u8], interrupted: &AtomicBool) -> Result<Vec<V>, String> {
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err("artifact index: has a partial final JSONL row".into());
    }
    bytes
        .split_inclusive(|b| *b == b'\n')
        .enumerate()
        .map(|(pos, line)| {
            if interrupted.load(Ordering::Relaxed) {
                return Err("interrupted".into());
            }
            parse_row(line, pos + 1)
        })
        .collect()
}
fn parse_row(line: &[u8], n: usize) -> Result<V, String> {
    if line.len() > ROW_LIMIT {
        return Err(format!(
            "artifact index: line {n} exceeds the {ROW_LIMIT}-byte row limit"
        ));
    }
    let payload = line.strip_suffix(b"\n").unwrap();
    let payload = payload.strip_suffix(b"\r").unwrap_or(payload);
    if payload.contains(&b'\r') {
        return Err(format!("artifact index: line {n} contains a bare CR"));
    }
    if payload.iter().all(|b| b" \t\n\r\x0b\x0c".contains(b)) {
        return Err(format!("artifact index: line {n} is blank"));
    }
    if payload.contains(&0) {
        return Err(format!("artifact index: line {n} contains a NUL byte"));
    }
    if let Err(error) = std::str::from_utf8(payload) {
        return Err(format!(
            "artifact index: invalid UTF-8 at line {n}: {}",
            diagnostic::utf8(error, payload)
        ));
    }
    if payload.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(format!(
            "artifact index: invalid JSON at line {n}: Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)"
        ));
    }
    let row = value::parse(payload, true).map_err(|_| {
        format!(
            "artifact index: invalid JSON at line {n}: {}",
            diagnostic::json(payload)
        )
    })?;
    if !matches!(row, V::Object(_)) {
        return Err(format!("artifact index: line {n} is not an object"));
    }
    Ok(row)
}

/// Own every artifact-index invocation, including help and invalid arguments.
pub fn owns(_args: &[OsString]) -> bool {
    true
}

/// Execute artifact-index against the explicit caller directory.
/// Output is returned without changing the process working directory.
pub fn run(
    args: &[OsString],
    caller_cwd: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    if args.first().is_some_and(|v| v == "import") {
        return actions::import(&args[1..], caller_cwd, interrupted);
    }
    let options = match cli::parse(args) {
        Ok(options) => options,
        Err(message) => return fail(&message),
    };
    if options.help {
        return (0, include_bytes!("usage.txt").to_vec(), Vec::new());
    }
    if interrupted.load(Ordering::Relaxed) {
        return fail("interrupted");
    }
    if options.action != "telemetry" {
        return actions::run(&options, caller_cwd, interrupted);
    }
    let execute = || -> Result<Vec<u8>, String> {
        let (repo, logical) = repository(&options, caller_cwd)?;
        let root = oms_safe_fs::Root::open(&repo).map_err(|_| "cannot resolve repo path")?;
        let rows = read_index(&root, &logical, options.index.as_deref(), interrupted)?;
        let report = report::report(&root, &rows, &options, interrupted)?;
        if interrupted.load(Ordering::Relaxed) {
            return Err("interrupted".into());
        }
        if options.json {
            return value::encode(&report);
        }
        if options.waste {
            return Ok(waste::emit(&report["waste"]).into_bytes());
        }
        emit::validate(&report, options.by.is_some())?;
        Ok(if options.by.is_some() {
            emit::grouped(&report).into_bytes()
        } else {
            emit::human(&report).into_bytes()
        })
    };
    match execute() {
        Ok(stdout) => (0, stdout, Vec::new()),
        Err(message) => fail(&message),
    }
}
