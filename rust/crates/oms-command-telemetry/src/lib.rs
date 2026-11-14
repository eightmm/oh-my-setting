#![forbid(unsafe_code)]

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
    limit: usize,
    by: Option<String>,
    since: Option<String>,
    until: Option<String>,
    json: bool,
    help: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            repo: None,
            index: None,
            limit: 1000,
            by: None,
            since: None,
            until: None,
            json: false,
            help: false,
        }
    }
}
fn fail(message: &str) -> (i32, Vec<u8>, Vec<u8>) {
    (2, Vec::new(), format!("error: {message}\n").into_bytes())
}
fn arg_text(arg: &std::ffi::OsStr) -> String {
    arg.to_string_lossy().into_owned()
}
fn parse(args: &[OsString]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut iter = args.iter();
    let mut action = false;
    let mut limit = None;
    let mut event = false;
    let mut reason = false;
    let mut files = false;
    let mut stale = false;
    let mut dry = false;
    let mut apply = false;
    let mut top = false;
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some(
                "--repo" | "--file" | "--event-id" | "--reason" | "--by" | "--top" | "--since"
                | "--until",
            ) => {
                let name = arg.to_str().unwrap();
                let suffix = match name {
                    "--repo" | "--file" => "path",
                    "--event-id" => "an id",
                    "--reason" => "text",
                    "--by" => "day, model, provider, task, role or effort",
                    "--top" => "a number",
                    _ => "a date or time",
                };
                let value = iter
                    .next()
                    .ok_or_else(|| format!("{name} requires {suffix}"))?;
                match name {
                    "--repo" => options.repo = Some(value.into()),
                    "--file" => options.index = Some(value.into()),
                    "--event-id" => event = true,
                    "--reason" => reason = !value.is_empty(),
                    "--by" => {
                        let text = value
                            .to_str()
                            .filter(|s| {
                                ["day", "model", "provider", "task", "role", "effort"].contains(s)
                            })
                            .ok_or("--by must be day, model, provider, task, role or effort")?;
                        options.by = Some(text.into());
                    }
                    "--top" => top = !value.is_empty(),
                    "--since" => {
                        options.since = if value.is_empty() {
                            None
                        } else {
                            Some(arg_text(value))
                        }
                    }
                    _ => {
                        options.until = if value.is_empty() {
                            None
                        } else {
                            Some(arg_text(value))
                        }
                    }
                }
            }
            Some("--json") => options.json = true,
            Some("--files") => files = true,
            Some("--stale") => stale = true,
            Some("--dry-run") => dry = true,
            Some("--apply") => apply = true,
            Some("-h" | "--help") => {
                options.help = true;
                return Ok(options);
            }
            Some(name) if ACTIONS.contains(&name) => {
                if action || limit.is_some() {
                    return Err(format!("unknown argument: {name}"));
                }
                action = true;
            }
            _ => {
                if limit.is_some() {
                    return Err(format!("unknown argument: {}", arg_text(arg)));
                }
                limit = Some(arg);
            }
        }
    }
    if event {
        return Err("--event-id is only valid with resolve".into());
    }
    if reason {
        return Err("--reason is only valid with resolve".into());
    }
    if files {
        return Err("--files is only valid with prune".into());
    }
    if stale {
        return Err("--stale is only valid with prune".into());
    }
    if dry {
        return Err(
            "--dry-run is only valid with prune, resolve-superseded, or resolve-recovered".into(),
        );
    }
    if apply {
        return Err("--apply is only valid with salvage".into());
    }
    if top {
        return Err("--top is only valid with --waste".into());
    }
    if let Some(raw) = limit {
        let value = raw
            .to_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or("N must be a positive integer")?;
        let digits = value.trim_start_matches('0');
        if digits.is_empty()
            || digits.len() > 19
            || digits
                .parse::<u64>()
                .ok()
                .is_none_or(|v| v > i64::MAX as u64)
        {
            return Err("N must be a positive integer".into());
        }
        options.limit = digits.parse().map_err(|_| "N must be a positive integer")?;
    }
    Ok(options)
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
fn git_file(path: &Path) -> Option<Vec<u8>> {
    use rustix::fs::{Mode, OFlags, open};
    use std::io::Read;
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let file = std::fs::File::from(fd);
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= 1024 * 1024).then_some(bytes)
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
    // Match git's worktree-root anchoring without invoking a child process.
    let mut root = physical.clone();
    for ancestor in physical.ancestors() {
        let marker = ancestor.join(".git");
        let gitfile = marker.is_file();
        let metadata = if gitfile {
            let Some(bytes) = git_file(&marker) else {
                break;
            };
            let Some(suffix) = bytes.strip_prefix(b"gitdir: ") else {
                break;
            };
            let path = std::ffi::OsStr::from_bytes(suffix.trim_ascii_end());
            ancestor.join(path)
        } else {
            marker
        };
        let common = git_file(&metadata.join("commondir"))
            .map(|bytes| metadata.join(std::ffi::OsStr::from_bytes(bytes.trim_ascii_end())))
            .unwrap_or_else(|| metadata.clone());
        if git_file(&metadata.join("HEAD")).is_some_and(|head| {
            let head = head.trim_ascii_end();
            head.strip_prefix(b"ref: ").is_some_and(|reference| {
                reference.starts_with(b"refs/") && !reference.iter().any(u8::is_ascii_whitespace)
            }) || [40, 64].contains(&head.len()) && head.iter().all(u8::is_ascii_hexdigit)
        }) && common.join("objects").is_dir()
            && common.join("refs").is_dir()
        {
            root = ancestor.to_path_buf();
            break;
        }
        if gitfile {
            break;
        }
    }
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
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err("artifact index: has a partial final JSONL row".into());
    }
    let mut rows = Vec::new();
    for (pos, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if interrupted.load(Ordering::Relaxed) {
            return Err("interrupted".into());
        }
        let n = pos + 1;
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
        rows.push(row);
    }
    Ok(rows)
}

/// Identifies the telemetry action without reading the repository or running a handler.
/// Waste mode belongs to the legacy implementation until its native slice is admitted.
pub fn owns(args: &[OsString]) -> bool {
    let mut iter = args.iter();
    let mut telemetry = false;
    let mut waste = false;
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some(
                "--repo" | "--file" | "--event-id" | "--reason" | "--by" | "--top" | "--since"
                | "--until",
            ) => {
                if iter.next().is_none() {
                    break;
                }
            }
            Some("--waste") => waste = true,
            Some("telemetry") => telemetry = true,
            Some(name) if ACTIONS.contains(&name) && !telemetry => {
                return false;
            }
            _ => {}
        }
    }
    telemetry && !waste
}

/// Executes the owned read-only report against the explicit caller directory.
/// All output is returned; the handler never changes process state or launches children.
pub fn run(
    args: &[OsString],
    caller_cwd: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    if !owns(args) {
        return (75, Vec::new(), Vec::new());
    }
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return fail(&message),
    };
    if options.help {
        return (0, include_bytes!("usage.txt").to_vec(), Vec::new());
    }
    if interrupted.load(Ordering::Relaxed) {
        return fail("interrupted");
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
