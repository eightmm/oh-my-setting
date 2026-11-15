#![forbid(unsafe_code)]
mod registry;
#[allow(dead_code)]
mod services_collab;
#[allow(dead_code)]
mod services_control;
#[allow(dead_code)]
mod services_runtime;

use oms_context::{Json, compare_manifests, inspect_manifest, parse_json};
use oms_json::{PyString, Value as Encoded};
use oms_safe_fs::{Error as FsError, ManifestError, Policy, Root};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const ROOT_HELP: &str = r#"usage: oms runtime [-h] [--repo REPO] [--pretty] [--version]
                   {envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,doctor}
                   ...

Typed semantic core over the existing hardened OMS execution plane.

positional arguments:
  {envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,doctor}
    envelope            project the effective TaskEnvelope
    evidence            criterion-level evidence coverage
    next                return the smallest currently valid internal actions
    brief               read a return brief, optionally comparing a saved
                        envelope
    failure             classify failures and inspect recovery metadata
    context             compile a bounded context manifest and bundle
    context-inspect     inspect saved context metadata and optionally verify
                        source files
    profile             optional capability contracts
    release             stable/edge update channel handling
    capsule             sanitized cross-machine continuity state
    backend             trusted-local, isolated, or remote execution
    experiment          ExperimentContract v2 and invariant packs
    benchmark           harness effectiveness telemetry
    doctor              validate the runtime projection without replacing OMS
                        doctor

options:
  -h, --help            show this help message and exit
  --repo REPO           project repository or directory
  --pretty              pretty-print JSON
  --version             show program's version number and exit"#;
const ROOT_USAGE: &str = r#"usage: oms runtime [-h] [--repo REPO] [--pretty] [--version]
                   {envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,doctor}
                   ..."#;
const HINT: &str = "hint: run `oms runtime --help` for usage";
const USAGE: &str = "usage: oms runtime context-inspect [-h] [--repo REPO] [--pretty]\n                                   [--injections]\n                                   [--session-hash SESSION_HASH]\n                                   [--session TRANSCRIPT] [--timeline]\n                                   [--compare COMPARE] [--verify-current]\n                                   [--limit LIMIT] [--format {json,text}]\n                                   [manifest]";
const HELP: &str = "positional arguments:\n  manifest              existing manifest inside the selected repository\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO           project repository or directory\n  --pretty              pretty-print JSON\n  --injections          instead of a manifest: bytes OMS hooks injected into\n                        sessions, from the hook event ledger\n  --session-hash SESSION_HASH\n                        with --injections: one recorded session hash\n  --session TRANSCRIPT  instead of a manifest: per-tool result bytes and\n                        repeated reads in one Claude Code JSONL transcript\n  --timeline            with --session: ordered tool calls (name, recorded\n                        duration, error flag, byte sizes, first error row); no\n                        arguments or outputs; up to 200 steps, --limit does\n                        not apply\n  --compare COMPARE     compare with another saved manifest; never run either\n                        pack\n  --verify-current      compare current source hashes (incompatible with\n                        --compare)\n  --limit LIMIT         rows per displayed list, 1..100 (default: 10)\n  --format {json,text}  output format (default: json); text is a read-only\n                        report";
#[derive(Default)]
struct Args {
    failure_argv: Option<Vec<OsString>>,
    experiment_argv: Option<Vec<OsString>>,
    repo: Option<OsString>,
    pretty: bool,
    manifest: Option<OsString>,
    compare: Option<OsString>,
    current: bool,
    injections: bool,
    session: Option<OsString>,
    session_hash: Option<OsString>,
    timeline: bool,
    limit: Option<OsString>,
    format: Option<OsString>,
}
fn decimal_digit(c: char) -> Option<u32> {
    use unicode_general_category::{GeneralCategory, get_general_category};
    if get_general_category(c) != GeneralCategory::DecimalNumber {
        return None;
    }
    let mut start = u32::from(c);
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if get_general_category(prev) != GeneralCategory::DecimalNumber {
            break;
        }
        start -= 1;
    }
    Some((u32::from(c) - start) % 10)
}
fn negative_number(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(rest) = text.strip_prefix('-') else {
        return false;
    };
    let mut pieces = rest.split('.');
    let first = pieces.next().unwrap_or_default();
    match (pieces.next(), pieces.next()) {
        (None, None) => !first.is_empty() && first.chars().all(|c| decimal_digit(c).is_some()),
        (Some(last), None) => {
            first.chars().all(|c| decimal_digit(c).is_some())
                && !last.is_empty()
                && last.chars().all(|c| decimal_digit(c).is_some())
        }
        _ => false,
    }
}
fn normalize_integer(text: &str) -> Option<String> {
    let text = text.trim();
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    if digits.chars().filter(|c| *c != '_').count() > 4300 {
        return None;
    }
    let mut result = String::new();
    if text.starts_with('-') {
        result.push('-');
    }
    for ch in digits.chars() {
        if ch != '_' {
            result.push(char::from_digit(decimal_digit(ch)?, 10)?);
        }
    }
    Some(result)
}
fn argument_repr(value: &str) -> String {
    use unicode_general_category::{GeneralCategory as G, get_general_category};
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for c in value.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if c != ' '
                && matches!(
                    get_general_category(c),
                    G::Control
                        | G::Format
                        | G::LineSeparator
                        | G::ParagraphSeparator
                        | G::PrivateUse
                        | G::SpaceSeparator
                        | G::Surrogate
                        | G::Unassigned
                ) =>
            {
                let n = u32::from(c);
                if n < 256 {
                    result.push_str(&format!("\\x{n:02x}"));
                } else if n < 65536 {
                    result.push_str(&format!("\\u{n:04x}"));
                } else {
                    result.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => result.push(c),
        }
    }
    result.push(quote);
    result
}
fn invalid_command(arg: &OsStr) -> String {
    format!(
        "argument command: invalid choice: '{}' (choose from envelope, evidence, next, brief, failure, context, context-inspect, profile, release, capsule, backend, experiment, benchmark, doctor)",
        arg.to_string_lossy()
    )
}
fn parse(args: Vec<OsString>, command_state: &mut bool, legacy: &mut bool) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.into_iter().peekable();
    if it.next().as_deref() != Some(OsStr::new("runtime")) {
        return Err("expected runtime namespace".into());
    }
    let mut command = false;
    let mut positional = false;
    let mut unknown = Vec::new();
    while let Some(arg) = it.next() {
        let bytes = arg.as_bytes();
        if !command && bytes == b"context-inspect" {
            command = true;
            *command_state = true;
            positional = false;
            continue;
        }
        if !positional && bytes == b"--" {
            positional = true;
            continue;
        }
        if !positional && bytes.starts_with(b"-h") {
            let suffix = &bytes[2..];
            if suffix.starts_with(b"=") || suffix.starts_with(b"-") {
                let explicit = suffix.strip_prefix(b"=").unwrap_or(suffix);
                return Err(format!(
                    "argument -h/--help: ignored explicit argument {}",
                    argument_repr(&String::from_utf8_lossy(explicit))
                ));
            }
            return Err(if command {
                "help requested"
            } else {
                "root help requested"
            }
            .into());
        }
        if !positional && bytes.starts_with(b"--") {
            let (name, inline) = match bytes.iter().position(|b| *b == b'=') {
                Some(i) => (
                    &bytes[..i],
                    Some(OsString::from_vec(bytes[i + 1..].to_vec())),
                ),
                None => (bytes, None),
            };
            // Python 3.12 reports the subcommand choices for ambiguous options;
            // Python 3.11 reports the root choices for some malformed arguments.
            let options: &[&[u8]] = if command {
                &[
                    b"--help",
                    b"--repo",
                    b"--pretty",
                    b"--injections",
                    b"--session-hash",
                    b"--session",
                    b"--timeline",
                    b"--compare",
                    b"--verify-current",
                    b"--limit",
                    b"--format",
                ]
            } else {
                &[b"--help", b"--repo", b"--pretty", b"--version"]
            };
            let matches: Vec<_> = if options.contains(&name) {
                vec![name]
            } else {
                options
                    .iter()
                    .filter(|o| o.starts_with(name))
                    .copied()
                    .collect()
            };
            if matches.is_empty() {
                if bytes.contains(&b' ') {
                    if !command {
                        return Err(invalid_command(&arg));
                    }
                    if a.manifest.is_none() {
                        a.manifest = Some(arg);
                        continue;
                    }
                }
                unknown.push(arg);
                continue;
            }
            if matches.len() > 1 {
                let choices = matches
                    .iter()
                    .map(|s| String::from_utf8_lossy(s))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "ambiguous option: {} could match {choices}",
                    arg.to_string_lossy()
                ));
            }
            let option = matches[0];
            if (option == b"--help" || option == b"--version")
                && let Some(value) = &inline
            {
                let label = if option == b"--help" {
                    "-h/--help"
                } else {
                    "--version"
                };
                let value = value.to_string_lossy();
                return Err(format!(
                    "argument {label}: ignored explicit argument {}",
                    argument_repr(&value)
                ));
            }
            if option == b"--version" {
                return Err("version requested".into());
            }
            if option == b"--help" {
                return Err(if command {
                    "help requested"
                } else {
                    "root help requested"
                }
                .into());
            }
            if [
                b"--pretty".as_slice(),
                b"--injections",
                b"--timeline",
                b"--verify-current",
            ]
            .contains(&option)
            {
                if let Some(value) = &inline {
                    return Err(format!(
                        "argument {}: ignored explicit argument {}",
                        String::from_utf8_lossy(option),
                        argument_repr(&value.to_string_lossy())
                    ));
                }
                match option {
                    b"--pretty" => a.pretty = true,
                    b"--injections" => a.injections = true,
                    b"--timeline" => a.timeline = true,
                    _ => a.current = true,
                };
                continue;
            }
            let value = inline
                .or_else(|| {
                    let next = it.peek()?;
                    let bytes = next.as_bytes();
                    if bytes.len() > 1
                        && bytes.starts_with(b"-")
                        && !bytes.contains(&b' ')
                        && !negative_number(bytes)
                    {
                        None
                    } else {
                        it.next()
                    }
                })
                .ok_or_else(|| {
                    format!(
                        "argument {}: expected one argument",
                        String::from_utf8_lossy(option)
                    )
                })?;
            let value = if option == b"--limit" {
                let text = value.to_string_lossy();
                OsString::from(normalize_integer(&text).ok_or_else(|| {
                    format!(
                        "argument --limit: invalid int value: {}",
                        argument_repr(&text)
                    )
                })?)
            } else {
                value
            };
            if option == b"--format" && value != "json" && value != "text" {
                return Err(format!(
                    "argument --format: invalid choice: '{}' (choose from json, text)",
                    value.to_string_lossy()
                ));
            }
            match option {
                b"--repo" => a.repo = Some(value),
                b"--session-hash" => a.session_hash = Some(value),
                b"--session" => a.session = Some(value),
                b"--compare" => a.compare = Some(value),
                b"--limit" => a.limit = Some(value),
                b"--format" => a.format = Some(value),
                _ => unreachable!(),
            }
        } else if !positional
            && bytes.len() > 1
            && bytes.starts_with(b"-")
            && !bytes.contains(&b' ')
            && !negative_number(bytes)
        {
            unknown.push(arg);
        } else if command && a.manifest.is_none() {
            a.manifest = Some(arg);
        } else if !command {
            if bytes == b"failure" || bytes == b"experiment" {
                let mut forwarded = Vec::new();
                if let Some(value) = a.repo.take() {
                    forwarded.extend([OsString::from("--repo"), value]);
                }
                if a.pretty {
                    forwarded.push(OsString::from("--pretty"));
                }
                if bytes == b"failure" {
                    forwarded.extend(unknown);
                    forwarded.extend(it);
                    a.failure_argv = Some(forwarded);
                    return Ok(a);
                }
                let mut tail: Vec<_> = it.collect();
                if oms_command_experiment::owns(&tail) {
                    // Keep root unknowns ahead of leaf unknowns, but after the
                    // action so the handler's pure ownership parser sees it.
                    let action_end = (1..=tail.len())
                        .find(|&end| oms_command_experiment::owns(&tail[..end]))
                        .expect("owned experiment action");
                    tail.splice(action_end..action_end, unknown);
                    forwarded.extend(tail);
                    a.experiment_argv = Some(forwarded);
                    return Ok(a);
                }
            }
            *legacy = [
                b"envelope".as_slice(),
                b"evidence",
                b"next",
                b"brief",
                b"failure",
                b"context",
                b"profile",
                b"release",
                b"capsule",
                b"backend",
                b"experiment",
                b"benchmark",
                b"doctor",
            ]
            .contains(&bytes);
            return Err(invalid_command(&arg));
        } else {
            unknown.push(arg);
        }
    }
    if !command {
        return Err("the following arguments are required: command".into());
    }
    if !unknown.is_empty() {
        return Err(format!(
            "\u{1}unrecognized arguments: {}",
            unknown
                .iter()
                .map(|v| v.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    Ok(a)
}
fn expand_repo(
    raw: &OsStr,
    home: Option<OsString>,
    lookup: impl FnOnce(&OsStr) -> Option<PathBuf>,
    current: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, String> {
    let bytes = raw.as_bytes();
    if !bytes.starts_with(b"~") {
        return Ok(if bytes.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(raw)
        });
    }
    let split = bytes.iter().position(|b| *b == b'/').unwrap_or(bytes.len());
    let user = &bytes[1..split];
    let home = if user.is_empty() {
        home.map(PathBuf::from).or_else(current)
    } else {
        lookup(OsStr::from_bytes(user))
    }
    .ok_or_else(|| {
        format!(
            "could not determine home directory for {}",
            raw.to_string_lossy()
        )
    })?;
    let home = if home.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        home
    };
    let suffix = bytes[split..]
        .iter()
        .position(|b| *b != b'/')
        .map_or(&[][..], |i| &bytes[split + i..]);
    Ok(if suffix.is_empty() {
        home
    } else {
        home.join(OsStr::from_bytes(suffix))
    })
}
fn expand_session(
    raw: &OsStr,
    home: Option<OsString>,
    lookup: impl FnOnce(&OsStr) -> Option<PathBuf>,
    current: impl FnOnce() -> Option<PathBuf>,
) -> PathBuf {
    let bytes = raw.as_bytes();
    if !bytes.starts_with(b"~") {
        return PathBuf::from(raw);
    }
    let split = bytes.iter().position(|b| *b == b'/').unwrap_or(bytes.len());
    let user = &bytes[1..split];
    let selected = if user.is_empty() {
        home.map(PathBuf::from).or_else(current)
    } else {
        lookup(OsStr::from_bytes(user))
    };
    let Some(home) = selected else {
        return PathBuf::from(raw);
    };
    let home = home.as_os_str().as_bytes();
    let end = home.iter().rposition(|b| *b != b'/').map_or(0, |i| i + 1);
    let mut expanded = home[..end].to_vec();
    expanded.extend_from_slice(&bytes[split..]);
    if expanded.is_empty() {
        expanded.push(b'/');
    }
    PathBuf::from(OsString::from_vec(expanded))
}
fn repo(raw: &OsStr, interrupted: &AtomicBool) -> Result<PathBuf, String> {
    use users::os::unix::UserExt;
    let path = expand_repo(
        raw,
        std::env::var_os("HOME"),
        |name| users::get_user_by_name(name).map(|u| u.home_dir().to_path_buf()),
        || users::get_user_by_uid(users::get_current_uid()).map(|u| u.home_dir().to_path_buf()),
    )?;
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "--show-toplevel"]);
    let selected = git_output(
        &mut command,
        interrupted,
        std::time::Duration::from_secs(20),
    )?
    .and_then(|bytes| normalize_git_output(&bytes))
    .map(PathBuf::from)
    .unwrap_or(path);
    if !selected.is_dir() {
        return Err(format!(
            "repository path is not a directory: {}",
            raw.to_string_lossy()
        ));
    }
    std::fs::canonicalize(selected)
        .map_err(|_| "invalid repository or context manifest path".into())
}
// Match subprocess text-mode universal newline translation and str.strip();
// non-UTF-8 output is kept as OS bytes and trimmed conservatively.
fn normalize_git_output(raw: &[u8]) -> Option<OsString> {
    let mut translated = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\r' {
            translated.push(b'\n');
            if raw.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
        } else {
            translated.push(raw[i]);
        }
        i += 1;
    }
    let bytes = if let Ok(text) = std::str::from_utf8(&translated) {
        text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
            .as_bytes()
    } else {
        translated.trim_ascii()
    };
    if bytes.is_empty() {
        None
    } else {
        Some(OsString::from_vec(bytes.to_vec()))
    }
}
fn git_output(
    command: &mut std::process::Command,
    interrupted: &AtomicBool,
    timeout: std::time::Duration,
) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let Ok(mut child) = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Ok(None);
    };
    let mut pipe = child.stdout.take().expect("piped stdout");
    let flags = rustix::fs::fcntl_getfl(&pipe);
    if flags
        .and_then(|f| rustix::fs::fcntl_setfl(&pipe, f | rustix::fs::OFlags::NONBLOCK))
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(None);
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    let mut ended = false;
    let mut status = None;
    loop {
        if interrupted.load(Ordering::SeqCst) || std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return if interrupted.load(Ordering::SeqCst) {
                Err("interrupted".into())
            } else {
                Ok(None)
            };
        }
        if !ended {
            match pipe.read(&mut buf) {
                Ok(0) => ended = true,
                Ok(n) => {
                    if bytes.len() + n > 1024 * 1024 {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(None);
                    }
                    bytes.extend_from_slice(&buf[..n]);
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(s) => status = s,
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
            }
        }
        if ended && status.is_some() {
            return Ok(if status.is_some_and(|s| s.success()) {
                Some(bytes)
            } else {
                None
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
fn read_manifest(root: &Root, p: &OsStr) -> Result<Vec<u8>, String> {
    root.read_manifest_detailed(Path::new(p), 4 * 1024 * 1024)
        .map_err(|e| {
            match e {
                ManifestError::External => {
                    "context manifest must be inside the selected repository"
                }
                ManifestError::Path => "invalid context manifest path",
                ManifestError::Policy | ManifestError::InitialOversize => {
                    "context manifest must be an owned, bounded regular file without links"
                }
                ManifestError::Overflow => "context manifest exceeds the read limit",
                ManifestError::Changed => "context manifest changed while being read",
                ManifestError::Open(_) => "cannot read a valid repo-local context manifest",
            }
            .into()
        })
}
fn get<'a>(v: &'a Json, key: &[u8]) -> Option<&'a Json> {
    if let Json::Object(fields) = v {
        fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    } else {
        None
    }
}
fn source_status(
    root: &Root,
    raw: &[u8],
    result: &mut Value,
    limit: usize,
    interrupted: &AtomicBool,
) -> Result<(), String> {
    let parsed = parse_json(raw).map_err(str::to_owned)?;
    let Some(Json::Array(rows)) = get(&parsed, b"selected") else {
        return Err("invalid context manifest: selected".into());
    };
    let mut sources = Vec::new();
    for row in rows {
        if let (Some(Json::String(path)), Some(Json::String(hash))) =
            (get(row, b"path"), get(row, b"sha256"))
        {
            sources.push((path, hash));
        } else {
            return Err("invalid context manifest: selected row".into());
        }
    }
    sources.sort_by(|a, b| a.0.cmp(b.0));
    let mut counts = [0usize; 4];
    let mut changed = Vec::new();
    let mut missing = Vec::new();
    for (path, hash) in sources {
        if interrupted.load(Ordering::SeqCst) {
            return Err("interrupted".into());
        }
        let status = match root.read_os(OsStr::from_bytes(path), 2 * 1024 * 1024, Policy::source())
        {
            Ok(data) => {
                let digest = format!("{:x}", Sha256::digest(data));
                if digest.as_bytes() == hash { 0 } else { 1 }
            }
            Err(FsError::Missing) => 2,
            Err(_) => 3,
        };
        counts[status] += 1;
        if status == 1 {
            changed.push(std::str::from_utf8(path).map_err(|_| "invalid source path")?);
        } else if status == 2 {
            missing.push(std::str::from_utf8(path).map_err(|_| "invalid source path")?);
        }
    }
    let bounded = |v: &Vec<&str>| json!({"total":v.len(),"items":&v[..v.len().min(limit)],"omitted":v.len().saturating_sub(limit)});
    result["sources_current"] = json!({"unchanged":counts[0],"changed":counts[1],"missing":counts[2],"unreadable":counts[3],"changed_paths":bounded(&changed),"missing_paths":bounded(&missing)});
    result["note"] = json!(
        "Not the provider's full context; the bundle was not read. Repeated source hashes do not prove duplicate selected bytes. No token or billing savings are inferred."
    );
    Ok(())
}
fn encode(v: &Value) -> Result<Encoded, String> {
    Ok(match v {
        Value::Null => Encoded::Null,
        Value::Bool(b) => Encoded::Bool(*b),
        Value::Number(n) if n.is_f64() => Encoded::Float(n.as_f64().ok_or("invalid number")?),
        Value::Number(n) => Encoded::integer(&n.to_string()).map_err(|e| e.to_string())?,
        Value::String(s) => Encoded::String(PyString::from(s.as_str())),
        Value::Array(a) => Encoded::Array(a.iter().map(encode).collect::<Result<_, _>>()?),
        Value::Object(o) => Encoded::Object(
            o.iter()
                .map(|(k, v)| Ok((PyString::from(k.as_str()), encode(v)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}
fn execute(a: Args, interrupted: &AtomicBool) -> Result<Vec<u8>, String> {
    if std::env::var_os("OMS_HARNESS_CHILD").as_deref() == Some(OsStr::new("1"))
        && a.session.is_some()
    {
        return Err("a harness child may only use read-only runtime actions".into());
    }
    let repo = repo(a.repo.as_deref().unwrap_or(OsStr::new(".")), interrupted)?;
    let limit = match a.limit {
        None => 10,
        Some(v) => v
            .to_str()
            .and_then(|s| s.trim().replace('_', "").parse::<i64>().ok())
            .unwrap_or(0),
    };
    let format = a.format.as_deref().unwrap_or(OsStr::new("json"));
    if format != "json" && format != "text" {
        return Err("invalid output format".into());
    }
    if format == "text" && a.pretty {
        return Err("--pretty applies only to JSON output".into());
    }
    if format == "text" && (a.injections || a.session.is_some()) {
        return Err("--format text renders a MANIFEST inspection or comparison".into());
    }
    let modes = usize::from(a.manifest.is_some())
        + usize::from(a.injections)
        + usize::from(a.session.is_some());
    if modes != 1 || (a.manifest.is_none() && (a.compare.is_some() || a.current)) {
        return Err("context-inspect takes one of: MANIFEST, --injections, --session TRANSCRIPT (--compare and --verify-current need MANIFEST)".into());
    }
    if a.session_hash.is_some() && !a.injections {
        return Err("--session-hash needs --injections".into());
    }
    if a.timeline && a.session.is_none() {
        return Err("--timeline needs --session TRANSCRIPT".into());
    }
    let limit = usize::try_from(limit).unwrap_or(0);
    if a.injections || a.session.is_some() {
        let cancel = || interrupted.load(Ordering::SeqCst);
        let value = if a.injections {
            let session = a
                .session_hash
                .as_deref()
                .map(|s| s.to_str().unwrap_or("\0"));
            oms_context_source::inspect_injections(&repo, session, limit, cancel)
        } else {
            use users::os::unix::UserExt;
            let raw = a.session.as_deref().unwrap();
            let path = expand_session(
                raw,
                std::env::var_os("HOME"),
                |name| users::get_user_by_name(name).map(|u| u.home_dir().to_path_buf()),
                || {
                    users::get_user_by_uid(users::get_current_uid())
                        .map(|u| u.home_dir().to_path_buf())
                },
            );
            if a.timeline {
                oms_context_source::inspect_timeline(&path, 200, cancel)
            } else {
                oms_context_source::inspect_session(&repo, &path, limit, cancel)
            }
        }
        .map_err(|e| {
            use oms_context_source::Error;
            let label = if a.injections {
                "hook event ledger"
            } else {
                "transcript"
            };
            match e {
                Error::Missing | Error::Unreadable => format!("{label} is missing or unreadable"),
                Error::Unsafe => format!("{label} is not a safe regular file"),
                Error::Oversize => format!(
                    "{label} is larger than {} bytes",
                    if a.injections {
                        67_108_864
                    } else {
                        268_435_456
                    }
                ),
                other => other.to_string(),
            }
        })?;
        let value = encode(&value)?;
        let mut bytes = if a.pretty {
            oms_json::pretty_json(&value)
        } else {
            oms_json::canonical_json(&value)
        }
        .map_err(|e| e.to_string())?;
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        return Ok(bytes);
    }
    let manifest = a.manifest.expect("mode was validated");
    if a.compare.is_some() && a.current {
        return Err("--verify-current cannot be combined with --compare".into());
    }
    let root = Root::open(repo).map_err(|_| "cannot read a valid repo-local context manifest")?;
    let left = read_manifest(&root, &manifest)?;
    let mut value = if let Some(right) = a.compare {
        compare_manifests(&left, &read_manifest(&root, &right)?, limit)?
    } else {
        inspect_manifest(&left, limit)?
    };
    if a.current {
        source_status(&root, &left, &mut value, limit, interrupted)?;
    }
    let mut bytes = if format == "text" {
        oms_context_report::render_report(&value)
            .map_err(|e| e.to_string())?
            .into_bytes()
    } else {
        let v = encode(&value)?;
        if a.pretty {
            oms_json::pretty_json(&v)
        } else {
            oms_json::canonical_json(&v)
        }
        .map_err(|e| e.to_string())?
    };
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    Ok(bytes)
}
fn write_stdout(bytes: &[u8]) {
    use std::io::Write;
    if std::io::stdout().write_all(bytes).is_err() {
        std::process::exit(1);
    }
}
fn command_exit(tool: &str, (code, output, mut error): (i32, Vec<u8>, Vec<u8>)) -> ! {
    use std::io::Write;
    if code == 2 {
        error.extend_from_slice(format!("hint: run `oms {tool} --help` for usage\n").as_bytes());
    }
    if std::io::stdout().write_all(&output).is_err() || std::io::stderr().write_all(&error).is_err()
    {
        std::process::exit(1);
    }
    // Ownership was decided before execution. In particular, an action's 75
    // is a final status, never permission to retry it through the legacy path.
    std::process::exit(code);
}
fn caller_cwd(tool: &str) -> PathBuf {
    std::env::current_dir().unwrap_or_else(|error| {
        command_exit(
            tool,
            (
                2,
                vec![],
                format!("runtime-core: cannot resolve current directory: {error}\n").into_bytes(),
            ),
        )
    })
}
fn streaming_command_exit(
    tool: &str,
    code: i32,
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> ! {
    if code == 2 && writeln!(stderr, "hint: run `oms {tool} --help` for usage").is_err() {
        std::process::exit(1);
    }
    if stdout.flush().is_err() || stderr.flush().is_err() {
        std::process::exit(1);
    }
    std::process::exit(code);
}
fn validate_payload(root: &Path, commit: &OsStr) -> Result<(), String> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let commit = commit.to_str().ok_or("invalid native source revision")?;
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("invalid native source revision".into());
    }
    let dir = root.join("rust/target/oms-native").join(commit);
    let executable = dir.join("oms");
    if std::fs::canonicalize(
        std::env::current_exe().map_err(|_| "cannot identify native executable")?,
    )
    .map_err(|_| "cannot identify native executable")?
        != std::fs::canonicalize(&executable).map_err(|_| "native executable unavailable")?
    {
        return Err("native executable does not belong to this checkout".into());
    }
    let manifest_path = dir.join("manifest.json");
    let manifest_meta =
        std::fs::symlink_metadata(&manifest_path).map_err(|_| "native manifest unavailable")?;
    if !manifest_meta.is_file() || manifest_meta.len() > 4 * 1024 * 1024 {
        return Err("invalid native manifest file".into());
    }
    let mut manifest = Vec::new();
    std::fs::File::open(&manifest_path)
        .map_err(|_| "cannot read native manifest")?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut manifest)
        .map_err(|_| "cannot read native manifest")?;
    if manifest.len() > 4 * 1024 * 1024 {
        return Err("invalid native manifest file".into());
    }
    let manifest = parse_json(&manifest).map_err(|_| "invalid native manifest")?;
    if get(&manifest, b"schema") != Some(&Json::Number("1".into()))
        || get(&manifest, b"source_commit") != Some(&Json::String(commit.as_bytes().to_vec()))
    {
        return Err("native manifest revision mismatch".into());
    }
    let artifacts = get(&manifest, b"artifacts").ok_or("missing native artifacts")?;
    let Json::Object(entries) = artifacts else {
        return Err("invalid native artifacts".into());
    };
    if entries
        .iter()
        .any(|(name, _)| name != b"oms" && name != b"oms-graph-python-helper")
    {
        return Err("invalid native artifact name".into());
    }
    for name in ["oms", "oms-graph-python-helper"] {
        let Some(value) = get(artifacts, name.as_bytes()) else {
            if name == "oms" {
                return Err("missing native executable digest".into());
            }
            continue;
        };
        let Json::String(expected) = value else {
            return Err("invalid native digest".into());
        };
        if expected.len() != 64
            || !expected
                .iter()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("invalid native digest".into());
        }
        let path = dir.join(name);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "native artifact unavailable")?;
        if !meta.file_type().is_file() || meta.permissions().mode() & 0o111 == 0 {
            return Err("invalid native artifact".into());
        }
        let mut file = std::fs::File::open(path).map_err(|_| "cannot read native artifact")?;
        let mut hash = Sha256::new();
        let mut buf = [0; 32768];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|_| "cannot read native artifact")?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        if format!("{:x}", hash.finalize()).as_bytes() != expected {
            return Err("native artifact digest mismatch".into());
        }
    }
    Ok(())
}
fn main() {
    let interrupted = Arc::new(AtomicBool::new(false));
    if let Err(e) =
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted))
    {
        eprintln!("runtime-core: cannot install interrupt handler: {e}\n{HINT}");
        std::process::exit(2);
    }
    let mut argv: Vec<_> = std::env::args_os().skip(1).collect();
    if argv
        .first()
        .is_some_and(|v| v == services_control::SCAN_WORKER_FLAG)
    {
        std::process::exit(services_control::scan_worker_main(&argv[1..], &interrupted));
    }
    let select = argv.first().is_some_and(|v| v == "--oms-select");
    let dispatch = select || argv.first().is_some_and(|v| v == "--oms-dispatch");
    let mut source_root = None;
    if dispatch {
        if argv.len() < 5 || argv[3] != "--" {
            eprintln!("runtime-core: invalid private dispatch invocation");
            std::process::exit(2);
        }
        if let Err(e) = validate_payload(Path::new(&argv[1]), &argv[2]) {
            eprintln!(
                "runtime-core: {e}\nhint: run `oms doctor --repair` to check the native cache"
            );
            std::process::exit(2);
        }
        source_root = Some(PathBuf::from(&argv[1]));
        argv.drain(..4);
    }
    if registry::owns(&argv) {
        if select {
            return;
        }
        let Some(source) = source_root.as_deref() else {
            eprintln!("runtime-core: root navigation requires validated native source metadata");
            std::process::exit(2);
        };
        let (code, out, err) = registry::run(&argv, source);
        use std::io::Write;
        if std::io::stdout().write_all(&out).is_err() || std::io::stderr().write_all(&err).is_err()
        {
            std::process::exit(1);
        }
        std::process::exit(code);
    }
    if let Some(
        tool @ ("artifact-index" | "agent-role" | "input" | "agent-events" | "agent-memory"
        | "approval-inbox" | "job-digest" | "slurm-lint"),
    ) = argv.first().and_then(|v| v.to_str()).map(|v| {
        if v == "slurm-lint.sh" {
            "slurm-lint"
        } else {
            v
        }
    }) {
        let local = &argv[1..];
        let owned = match tool {
            "artifact-index" => oms_command_telemetry::owns(local),
            "agent-role" => oms_command_agent_role::owns(local),
            "input" => {
                // Native input lacks sealed-room receipt verification. Keep
                // every child invocation on the canonical legacy guard path.
                let child = std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1")
                    || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH").is_some_and(|v| v != "0");
                !child && oms_command_input::owns(local)
            }
            "agent-events" => oms_command_operations::owns_events(local),
            "approval-inbox" => oms_command_operations::owns_approvals(local),
            "agent-memory" => oms_command_memory::owns(local),
            "job-digest" => oms_command_job_digest::owns(local),
            "slurm-lint" => oms_command_slurm_lint::owns(local),
            _ => unreachable!(),
        };
        if !owned {
            std::process::exit(75);
        }
        if select {
            return;
        }
        if matches!(tool, "agent-role" | "agent-memory" | "slurm-lint") && source_root.is_none() {
            command_exit(
                tool,
                (
                    2,
                    vec![],
                    format!("error: {tool} requires validated native source metadata\n")
                        .into_bytes(),
                ),
            );
        }
        let cwd = caller_cwd(tool);
        if let Some(app) = oms_command_operations::App::from_namespace(tool) {
            let mut stdout = std::io::stdout().lock();
            let mut stderr = std::io::stderr().lock();
            let code = oms_command_operations::run_streaming(
                app,
                local,
                &cwd,
                &interrupted,
                &mut stdout,
                &mut stderr,
            );
            streaming_command_exit(tool, code, &mut stdout, &mut stderr);
        }
        let result = match tool {
            "artifact-index" => oms_command_telemetry::run(local, &cwd, &interrupted),
            "agent-role" => oms_command_agent_role::run_with_source(
                local,
                &cwd,
                source_root.as_deref().expect("validated source metadata"),
                &interrupted,
            ),
            "input" => oms_command_input::run(local, &cwd, &interrupted),
            "agent-memory" => oms_command_memory::run_with_source(
                local,
                &cwd,
                source_root.as_deref().expect("validated source metadata"),
                &interrupted,
            ),
            "job-digest" => oms_command_job_digest::run(local, &cwd, &interrupted),
            "slurm-lint" => oms_command_slurm_lint::run_with_source(
                local,
                &cwd,
                source_root.as_deref().expect("validated source metadata"),
                &interrupted,
            ),
            _ => unreachable!(),
        };
        command_exit(tool, result);
    }
    if dispatch && argv.first().is_none_or(|v| v != "runtime") {
        std::process::exit(75);
    }
    let mut subcommand = false;
    let mut legacy = false;
    let parsed = parse(argv, &mut subcommand, &mut legacy);
    if dispatch
        && (legacy
            || matches!(&parsed, Err(e) if e == "root help requested" || e == "version requested"))
    {
        std::process::exit(75);
    }
    if select {
        return;
    }
    let args = match parsed {
        Ok(v) => v,
        Err(e) if e == "version requested" => {
            write_stdout(b"0.7.0\n");
            return;
        }
        Err(e) if e == "root help requested" => {
            write_stdout(format!("{ROOT_HELP}\n").as_bytes());
            return;
        }
        Err(e) if e == "help requested" => {
            write_stdout(format!("{USAGE}\n\n{HELP}\n").as_bytes());
            return;
        }
        Err(e) => {
            if let Some(e) = e.strip_prefix('\u{1}') {
                eprintln!("{ROOT_USAGE}\noms runtime: error: {e}\n{HINT}");
                std::process::exit(2);
            }
            if subcommand {
                eprintln!("{USAGE}\noms runtime context-inspect: error: {e}\n{HINT}");
            } else {
                eprintln!("{ROOT_USAGE}\noms runtime: error: {e}\n{HINT}");
            }
            std::process::exit(2)
        }
    };
    if let Some(argv) = args.failure_argv {
        let cwd = caller_cwd("runtime");
        command_exit(
            "runtime",
            oms_command_failure::run(&argv, &cwd, &interrupted),
        );
    }
    if let Some(argv) = args.experiment_argv {
        let cwd = caller_cwd("runtime");
        command_exit(
            "runtime",
            oms_command_experiment::run(&argv, &cwd, &interrupted),
        );
    }
    let result = execute(args, &interrupted);
    if interrupted.load(Ordering::SeqCst) {
        eprintln!("runtime-core: interrupted");
        std::process::exit(130);
    }
    match result {
        Ok(output) => {
            use std::io::Write;
            if std::io::stdout().write_all(&output).is_err() {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("runtime-core: {e}\n{HINT}");
            std::process::exit(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_double_dash_argument_with_space_is_root_positional() {
        let argv = ["runtime", "--foo bar", "failure", "catalog"]
            .into_iter()
            .map(OsString::from)
            .collect();
        let mut command = false;
        let mut legacy = false;
        let error = parse(argv, &mut command, &mut legacy)
            .err()
            .expect("invalid root command");
        assert!(error.starts_with("argument command: invalid choice: '--foo bar'"));
        assert!(!command && !legacy);
    }
    #[test]
    fn encodes_float_values_with_python_json_rules() {
        let value = serde_json::json!([0.5, -0.0, 1e-7]);
        assert_eq!(
            oms_json::canonical_json(&encode(&value).unwrap()).unwrap(),
            b"[0.5,-0.0,1e-07]"
        );
    }
    #[test]
    fn git_output_is_bounded_and_normalizes_text() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(normalize_git_output(b" \r\n").as_deref(), None);
        assert_eq!(
            normalize_git_output(b" /x\r\ny \r\n").unwrap().as_bytes(),
            b"/x\ny"
        );
        let mut command = std::process::Command::new("/usr/bin/yes");
        let outcome = git_output(
            &mut command,
            &AtomicBool::new(false),
            std::time::Duration::from_secs(20),
        )
        .unwrap();
        assert!(outcome.is_none());
        let fifo = std::env::temp_dir().join(format!("oms-git-timeout-{}", std::process::id()));
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let mut blocked = std::process::Command::new("/bin/cat");
        blocked.arg(&fifo);
        let outcome = git_output(
            &mut blocked,
            &AtomicBool::new(false),
            std::time::Duration::from_millis(20),
        );
        std::fs::remove_file(fifo).unwrap();
        assert!(outcome.unwrap().is_none());
    }
    #[test]
    fn repo_expansion_preserves_username_and_home_bytes() {
        let raw = OsStr::from_bytes(b"~name\xff//dir");
        let result = expand_repo(
            raw,
            None,
            |name| {
                assert_eq!(name.as_bytes(), b"name\xff");
                Some(PathBuf::from(OsStr::from_bytes(b"/tmp/home\xfe")))
            },
            || panic!("current user fallback"),
        );
        assert_eq!(result.unwrap().as_os_str().as_bytes(), b"/tmp/home\xfe/dir");
        assert_eq!(
            expand_repo(
                OsStr::new("~//tmp"),
                Some(OsString::new()),
                |_| None,
                || None
            )
            .unwrap(),
            PathBuf::from("/tmp")
        );
        assert!(expand_repo(OsStr::new("~missing"), None, |_| None, || None).is_err());
        assert_eq!(
            expand_repo(
                OsStr::new("~"),
                None,
                |_| None,
                || Some(PathBuf::from("fallback"))
            )
            .unwrap(),
            PathBuf::from("fallback")
        );
    }
    #[test]
    fn session_expansion_preserves_slashes_and_raw_users() {
        let raw = OsStr::from_bytes(b"~name\xff/");
        let value = expand_session(
            raw,
            None,
            |name| {
                assert_eq!(name.as_bytes(), b"name\xff");
                Some(PathBuf::from(OsStr::from_bytes(b"/tmp/h\xfe//")))
            },
            || None,
        );
        assert_eq!(value.as_os_str().as_bytes(), b"/tmp/h\xfe/");
        assert_eq!(
            expand_session(OsStr::new("~unknown/"), None, |_| None, || None),
            PathBuf::from("~unknown/")
        );
        assert_eq!(
            expand_session(OsStr::new("~"), Some(OsString::new()), |_| None, || None),
            PathBuf::from("/")
        );
        assert_eq!(
            expand_session(OsStr::new(""), None, |_| None, || None),
            PathBuf::new()
        );
    }
}
