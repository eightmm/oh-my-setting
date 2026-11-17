#![forbid(unsafe_code)]
mod registry;
mod services_collab;
mod services_control;
mod services_landing;
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
                b"ssh-adapter",
                b"exec-snapshot",
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
fn wants_usage_hint(tool: &str) -> bool {
    registry::is_public(OsStr::new(tool)) && !matches!(tool, "panel" | "harness")
}

fn command_exit(tool: &str, (code, output, mut error): (i32, Vec<u8>, Vec<u8>)) -> ! {
    use std::io::Write;
    if code == 2 && wants_usage_hint(tool) {
        let hint = format!("hint: run `oms {tool} --help` for usage\n");
        if !error.ends_with(hint.as_bytes()) {
            error.extend_from_slice(hint.as_bytes());
        }
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
    if code == 2
        && wants_usage_hint(tool)
        && writeln!(stderr, "hint: run `oms {tool} --help` for usage").is_err()
    {
        std::process::exit(1);
    }
    if stdout.flush().is_err() || stderr.flush().is_err() {
        std::process::exit(1);
    }
    std::process::exit(code);
}
pub(crate) fn validate_staged_payload(
    root: &Path,
    commit: &OsStr,
    check_current_exe: bool,
) -> Result<(PathBuf, String), String> {
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
    let dir = root.join("target/oms-native").join(commit);
    let dir_meta =
        std::fs::symlink_metadata(&dir).map_err(|_| "native payload directory unavailable")?;
    if !dir_meta.file_type().is_dir() || dir_meta.permissions().mode() & 0o002 != 0 {
        return Err("invalid native payload directory".into());
    }
    let canonical_dir =
        std::fs::canonicalize(&dir).map_err(|_| "native payload directory unavailable")?;
    let executable = dir.join("oms");
    if check_current_exe
        && std::fs::canonicalize(
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
    if !manifest_meta.file_type().is_file()
        || manifest_meta.permissions().mode() & 0o002 != 0
        || manifest_meta.len() > 4 * 1024 * 1024
    {
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
    const ARTIFACTS: &[&str] = &[
        "oms",
        "oms-supervisor",
        "oms-state-queue-worker",
        "oms-mcp-peer-owner",
        "oms-run-bounded-helper",
        "oms-graph-python-helper",
    ];
    if entries
        .iter()
        .any(|(name, _)| !ARTIFACTS.iter().any(|known| name == known.as_bytes()))
    {
        return Err("invalid native artifact name".into());
    }
    let mut oms_digest = None;
    for &name in ARTIFACTS {
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
        if !meta.file_type().is_file()
            || meta.permissions().mode() & 0o111 == 0
            || meta.permissions().mode() & 0o002 != 0
        {
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
        let actual = format!("{:x}", hash.finalize());
        if actual.as_bytes() != expected {
            return Err("native artifact digest mismatch".into());
        }
        if name == "oms" {
            oms_digest = Some(actual);
        }
    }
    let dir_entries =
        std::fs::read_dir(&dir).map_err(|_| "cannot list native payload directory")?;
    for entry in dir_entries {
        let entry = entry.map_err(|_| "cannot list native payload directory")?;
        let name = entry.file_name();
        let allowed = name == OsStr::new("manifest.json")
            || name == OsStr::new("oms.sha256")
            || entries
                .iter()
                .any(|(key, _)| name.as_bytes() == key.as_slice());
        if !allowed {
            return Err("unexpected file in native payload directory".into());
        }
        if name == OsStr::new("oms.sha256") {
            let meta = std::fs::symlink_metadata(entry.path())
                .map_err(|_| "invalid native checksum file")?;
            if !meta.file_type().is_file() || meta.permissions().mode() & 0o002 != 0 {
                return Err("invalid native checksum file".into());
            }
        }
    }
    Ok((
        canonical_dir,
        oms_digest.ok_or("missing native executable digest")?,
    ))
}

fn validate_payload(root: &Path, commit: &OsStr) -> Result<(), String> {
    validate_staged_payload(root, commit, true).map(|_| ())
}

pub(crate) fn sibling_companion(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()?;
    let path = exe.parent()?.join(name);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    (meta.file_type().is_file()
        && meta.permissions().mode() & 0o111 != 0
        && meta.permissions().mode() & 0o002 == 0)
        .then_some(path)
}

pub(crate) fn check_python_syntax_with_companion(path: &Path, source: &[u8]) -> Option<bool> {
    let helper = sibling_companion("oms-graph-python-helper")?;
    oms_command_graph::check_python_syntax(
        &helper,
        path,
        source,
        std::time::Duration::from_secs(10),
    )
    .ok()
}

struct McpExecutor {
    interrupted: Arc<AtomicBool>,
    source: PathBuf,
}

impl oms_mcp_server::CommandExecutor for McpExecutor {
    fn execute(
        &self,
        request: &oms_mcp_server::CommandRequest<'_>,
    ) -> Result<oms_mcp_server::Captured, String> {
        let services = services_collab::CollabServices::new(&self.interrupted, &self.source)
            .map_err(|e| format!("error: {e}"))?;
        <services_collab::CollabServices<'_> as oms_mcp_server::CommandExecutor>::execute(
            &services, request,
        )
    }
}

struct NativeAdapter;

impl oms_bootstrap::CommandAdapter for NativeAdapter {
    fn is_public(&self, name: &OsStr) -> bool {
        registry::is_public(name)
    }

    fn owns_native(&self, args: &[OsString]) -> bool {
        owns_invocation(args)
    }

    fn navigation(&self, args: &[OsString], source: &Path) -> oms_bootstrap::CommandOutput {
        registry::run(args, source)
    }
}

fn normalize_tool(arg: &OsStr) -> Option<&str> {
    let s = arg.to_str()?;
    if s == "slurm-lint.sh" {
        Some("slurm-lint")
    } else if s == "oms-mcp-server" {
        Some("mcp-server")
    } else if let Some(stem) = s.strip_suffix(".sh")
        && oms_command_hooks::HOOKS.contains(&stem)
    {
        Some(stem)
    } else {
        Some(s)
    }
}

fn owns_invocation(argv: &[OsString]) -> bool {
    if registry::owns(argv) {
        return true;
    }
    let Some(tool) = argv.first().and_then(|v| normalize_tool(v)) else {
        return false;
    };
    let local = &argv[1..];
    match tool {
        "runtime" | "mcp-server" | "agent-supervisor" | "hud" => true,
        "artifact-index" => oms_command_telemetry::owns(local),
        "agent-role" => oms_command_agent_role::owns(local),
        "input" => oms_command_input::owns(local),
        "agent-events" => {
            oms_command_operations::owns_events(local) || oms_command_attempt_bundle::owns(local)
        }
        "approval-inbox" => oms_command_operations::owns_approvals(local),
        "agent-memory" => oms_command_memory::owns(local),
        "job-digest" => oms_command_job_digest::owns(local),
        "slurm-lint" => oms_command_slurm_lint::owns(local),
        "slurm-submit" => oms_command_slurm_submit::owns(local),
        "agent-task" => oms_command_task::owns(local),
        "room" => oms_command_room::owns(local),
        "journal" => oms_command_journal::owns(local),
        "graph" => oms_command_graph::owns(local),
        _ => {
            oms_command_lifecycle::owns(tool, local)
                || oms_command_hooks::owns_hook(tool, local)
                || oms_command_autonomy::owns(tool, local)
                || oms_command_plan::owns(tool, local)
                || oms_command_landing::owns(tool, local)
                || oms_command_state::owns(tool, local)
                || oms_command_runs::owns(tool)
                || oms_command_peer::owns(tool, local)
                || oms_command_providers_skills::owns(tool, local)
                || oms_command_panel::owns(tool, local)
        }
    }
}

fn frontdoor_error(err: oms_bootstrap::Error) -> ! {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(err.diagnostic());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
    std::process::exit(err.status);
}

fn prepare_frontdoor(argv: &[OsString]) -> (Vec<OsString>, PathBuf) {
    use std::io::Write;
    let parsed = match oms_bootstrap::PrivateInvocation::parse(argv) {
        Ok(Some(oms_bootstrap::PrivateInvocation::Frontdoor { entry, args })) => (entry, args),
        Ok(_) => {
            eprintln!("runtime-core: invalid private frontdoor invocation");
            std::process::exit(2);
        }
        Err(err) => frontdoor_error(err),
    };
    let prepared = oms_bootstrap::Frontdoor::prepare(
        &parsed.0,
        &parsed.1,
        oms_bootstrap::Terminal::current(),
        &NativeAdapter,
        &oms_bootstrap::SourceInventory::embedded(),
    )
    .unwrap_or_else(|err| frontdoor_error(err));
    match prepared.route() {
        oms_bootstrap::Route::Navigation { .. } => {
            let (code, out, err) = prepared
                .navigation(&NativeAdapter)
                .unwrap_or_else(|e| frontdoor_error(e));
            if std::io::stdout().write_all(&out).is_err()
                || std::io::stderr().write_all(&err).is_err()
            {
                std::process::exit(1);
            }
            std::process::exit(code);
        }
        oms_bootstrap::Route::Native { .. } => {
            let source = prepared.source().source_root().to_path_buf();
            let native_args = prepared
                .native_args()
                .unwrap_or_else(|err| frontdoor_error(err))
                .to_vec();
            (native_args, source)
        }
        oms_bootstrap::Route::Legacy { .. } => {
            let _ = prepared
                .into_legacy()
                .unwrap_or_else(|err| frontdoor_error(err));
            std::process::exit(75);
        }
    }
}

fn require_source_or_exit<'a>(tool: &str, source_root: Option<&'a Path>) -> &'a Path {
    source_root.unwrap_or_else(|| {
        command_exit(
            tool,
            (
                2,
                vec![],
                format!("error: {tool} requires validated native source metadata\n").into_bytes(),
            ),
        )
    })
}

fn maybe_notify_panel_wakeup(tool: &str, local: &[OsString], cwd: &Path) {
    if matches!(
        tool,
        "hud"
            | "oms-hud"
            | "agy-statusline"
            | "claude-statusline"
            | "claude-subagent-statusline"
            | "status"
            | "list"
            | "help"
            | "panel"
    ) {
        return;
    }
    let mut notified_repo = false;
    let mut i = 0;
    while i < local.len() {
        let arg = &local[i];
        if arg == "--repo" {
            if let Some(val) = local.get(i + 1).filter(|v| !v.is_empty()) {
                let target = Path::new(val);
                let resolved = if target.is_absolute() {
                    target.to_path_buf()
                } else {
                    cwd.join(target)
                };
                oms_command_panel::notify_wakeup(&resolved);
                notified_repo = true;
            }
            break;
        }
        if let Some(s) = arg.to_str()
            && let Some(val) = s.strip_prefix("--repo=")
            && !val.is_empty()
        {
            let target = Path::new(val);
            let resolved = if target.is_absolute() {
                target.to_path_buf()
            } else {
                cwd.join(target)
            };
            oms_command_panel::notify_wakeup(&resolved);
            notified_repo = true;
            break;
        }
        i += 1;
    }
    if !notified_repo {
        oms_command_panel::notify_wakeup(cwd);
    }
}

fn dispatch_exit(tool: &str, local: &[OsString], cwd: &Path, output: (i32, Vec<u8>, Vec<u8>)) -> ! {
    maybe_notify_panel_wakeup(tool, local, cwd);
    command_exit(tool, output);
}

fn dispatch_streaming_exit(
    tool: &str,
    local: &[OsString],
    cwd: &Path,
    code: i32,
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> ! {
    maybe_notify_panel_wakeup(tool, local, cwd);
    streaming_command_exit(tool, code, stdout, stderr);
}

fn dispatch_owned(
    tool: &str,
    local: &[OsString],
    source_root: Option<&Path>,
    interrupted: &Arc<AtomicBool>,
) -> ! {
    let cwd = caller_cwd(tool);
    if tool == "agent-events" {
        if oms_command_attempt_bundle::owns(local) {
            dispatch_exit(
                tool,
                local,
                &cwd,
                oms_command_attempt_bundle::run(local, &cwd, interrupted),
            );
        }
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        let code = oms_command_operations::run_streaming(
            oms_command_operations::App::Events,
            local,
            &cwd,
            interrupted,
            &mut stdout,
            &mut stderr,
        );
        dispatch_streaming_exit(tool, local, &cwd, code, &mut stdout, &mut stderr);
    }
    if tool == "approval-inbox" {
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        let code = oms_command_operations::run_streaming(
            oms_command_operations::App::Approvals,
            local,
            &cwd,
            interrupted,
            &mut stdout,
            &mut stderr,
        );
        dispatch_streaming_exit(tool, local, &cwd, code, &mut stdout, &mut stderr);
    }
    if tool == "agent-supervisor" {
        let runner = sibling_companion("oms-supervisor").unwrap_or_default();
        let mut host = oms_supervisor::CommandHost::new(vec![runner.into_os_string()]);
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        let code = oms_supervisor::run_streaming(
            local,
            &cwd,
            interrupted,
            &mut host,
            &mut stdout,
            &mut stderr,
        );
        while host.requires_foreground_cleanup() {
            let _ = host.drive();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        dispatch_streaming_exit(tool, local, &cwd, code, &mut stdout, &mut stderr);
    }
    if tool == "artifact-index" {
        dispatch_exit(
            tool,
            local,
            &cwd,
            oms_command_telemetry::run(local, &cwd, interrupted),
        );
    }
    if tool == "job-digest" {
        dispatch_exit(
            tool,
            local,
            &cwd,
            oms_command_job_digest::run(local, &cwd, interrupted),
        );
    }
    if tool == "room" {
        dispatch_exit(
            tool,
            local,
            &cwd,
            oms_command_room::run(local, &cwd, interrupted),
        );
    }
    if tool == "input" {
        let result = match source_root {
            Some(source) => match services_runtime::RuntimeServices::new(interrupted, source) {
                Ok(runtime_svc) => {
                    oms_command_input::run_with_services(local, &cwd, interrupted, &runtime_svc)
                }
                Err(e) => (2, vec![], format!("error: {e}\n").into_bytes()),
            },
            None => oms_command_input::run(local, &cwd, interrupted),
        };
        dispatch_exit(tool, local, &cwd, result);
    }
    let source = require_source_or_exit(tool, source_root);
    let err_output = |e: String| (2, Vec::new(), format!("error: {e}\n").into_bytes());
    if tool == "mcp-server" {
        let Some(peer_owner) = sibling_companion("oms-mcp-peer-owner") else {
            command_exit(
                tool,
                (
                    2,
                    vec![],
                    b"error: native companion oms-mcp-peer-owner unavailable\n".to_vec(),
                ),
            );
        };
        let mut config = match oms_mcp_server::Config::from_env(peer_owner) {
            Ok(cfg) => cfg,
            Err(msg) => command_exit(tool, (2, vec![], format!("{msg}\n").into_bytes())),
        };
        config.oms_program = std::env::current_exe().and_then(std::fs::canonicalize).ok();
        let executor: Arc<dyn oms_mcp_server::CommandExecutor> = Arc::new(McpExecutor {
            interrupted: Arc::clone(interrupted),
            source: source.to_path_buf(),
        });
        let mut reader = std::io::BufReader::new(std::io::stdin().lock());
        let mut writer = std::io::stdout().lock();
        match oms_mcp_server::serve_with_services(
            source,
            &cwd,
            interrupted,
            &mut reader,
            &mut writer,
            config,
            executor,
        ) {
            Ok(()) => std::process::exit(0),
            Err(err) => {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
    }
    if tool == "slurm-submit" {
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        let code = oms_command_slurm_submit::run_with_source_streaming(
            local,
            &cwd,
            source,
            interrupted,
            &mut stdout,
            &mut stderr,
        );
        dispatch_streaming_exit(tool, local, &cwd, code, &mut stdout, &mut stderr);
    }
    if tool == "agent-task" {
        let control = match services_control::ControlServices::new(interrupted, source) {
            Ok(svc) => svc,
            Err(e) => dispatch_exit(tool, local, &cwd, err_output(e)),
        };
        let mut task_svc = control.task();
        let mut stdin = std::io::stdin().lock();
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        let code = oms_command_task::run_with_io(
            local,
            &cwd,
            interrupted,
            None,
            &mut task_svc,
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        dispatch_streaming_exit(tool, local, &cwd, code, &mut stdout, &mut stderr);
    }
    if oms_command_runs::owns(tool) {
        let mut runtime_svc = match services_runtime::RuntimeServices::new(interrupted, source) {
            Ok(svc) => svc,
            Err(e) => dispatch_exit(tool, local, &cwd, err_output(e)),
        };
        let ctx = match oms_command_runs::Context::new(&cwd, interrupted) {
            Ok(c) => c,
            Err(e) => dispatch_exit(
                tool,
                local,
                &cwd,
                (
                    e.code,
                    vec![],
                    format!("error: {}\n", e.message).into_bytes(),
                ),
            ),
        };
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        let code = oms_command_runs::run(
            tool,
            local,
            &ctx,
            &mut runtime_svc,
            &mut stdout,
            &mut stderr,
        );
        dispatch_streaming_exit(tool, local, &cwd, code, &mut stdout, &mut stderr);
    }
    let result = match tool {
        "hud" => match services_runtime::RuntimeServices::new(interrupted, source) {
            Ok(runtime_svc) => oms_command_hooks::run_with_source_and_services(
                "oms-hud",
                local,
                &cwd,
                source,
                interrupted,
                &runtime_svc,
            ),
            Err(e) => err_output(e),
        },
        "agent-role" => oms_command_agent_role::run_with_source(local, &cwd, source, interrupted),
        "agent-memory" => oms_command_memory::run_with_source(local, &cwd, source, interrupted),
        "slurm-lint" => oms_command_slurm_lint::run_with_source(local, &cwd, source, interrupted),
        "journal" => {
            let append_cb = |repo: &Path, src: &str, lesson: &str| {
                let (code, _, stderr) = oms_command_memory::append_project_note(
                    repo,
                    src,
                    "journal",
                    lesson,
                    interrupted,
                );
                if code == 0 {
                    Ok(oms_command_journal::MemoryAppendOutcome::Appended)
                } else {
                    Ok(oms_command_journal::MemoryAppendOutcome::Refused(
                        String::from_utf8_lossy(&stderr).trim().to_owned(),
                    ))
                }
            };
            oms_command_journal::run_with_services(
                local,
                &cwd,
                source,
                Some(&append_cb),
                interrupted,
            )
        }
        "graph" => match (
            services_control::ControlServices::new(interrupted, source),
            services_runtime::RuntimeServices::new(interrupted, source),
        ) {
            (Ok(control), Ok(runtime_svc)) => {
                let helper = sibling_companion("oms-graph-python-helper");
                oms_command_graph::run_with_host(
                    local,
                    &cwd,
                    interrupted,
                    helper.as_deref(),
                    Some(&runtime_svc),
                    &control,
                    &control,
                )
            }
            (Err(e), _) | (_, Err(e)) => err_output(e),
        },
        _ if oms_command_lifecycle::owns(tool, local) => {
            match services_collab::CollabServices::new(interrupted, source) {
                Ok(collab) => oms_command_lifecycle::run_with_services(
                    tool,
                    local,
                    &cwd,
                    source,
                    interrupted,
                    &collab,
                ),
                Err(e) => err_output(e),
            }
        }
        _ if oms_command_hooks::owns_hook(tool, local) => {
            match services_runtime::RuntimeServices::new(interrupted, source) {
                Ok(runtime_svc) => oms_command_hooks::run_with_source_and_services(
                    tool,
                    local,
                    &cwd,
                    source,
                    interrupted,
                    &runtime_svc,
                ),
                Err(e) => err_output(e),
            }
        }
        _ if oms_command_autonomy::owns(tool, local) => {
            match services_control::ControlServices::new(interrupted, source) {
                Ok(control) => oms_command_autonomy::run_with_services(
                    tool,
                    local,
                    &cwd,
                    interrupted,
                    &control,
                ),
                Err(e) => err_output(e),
            }
        }
        _ if oms_command_plan::owns(tool, local) => {
            match services_control::ControlServices::new(interrupted, source) {
                Ok(control) => {
                    oms_command_plan::run_with_services(local, &cwd, interrupted, &control)
                }
                Err(e) => err_output(e),
            }
        }
        _ if oms_command_landing::owns(tool, local) => {
            match services_control::ControlServices::new(interrupted, source) {
                Ok(control) => oms_command_landing::run_with_services(
                    tool,
                    local,
                    &cwd,
                    source,
                    interrupted,
                    &control,
                ),
                Err(e) => err_output(e),
            }
        }
        _ if oms_command_state::owns(tool, local) => match (
            services_control::ControlServices::new(interrupted, source),
            services_runtime::RuntimeServices::new(interrupted, source),
        ) {
            (Ok(control), Ok(mut runtime_svc)) => oms_command_state::run_with_host(
                tool,
                local,
                &cwd,
                interrupted,
                &mut runtime_svc,
                &control,
            ),
            (Err(e), _) | (_, Err(e)) => err_output(e),
        },
        _ if oms_command_peer::owns(tool, local) => {
            match services_collab::CollabServices::new(interrupted, source) {
                Ok(collab) => oms_command_peer::run_with_executor(
                    tool,
                    local,
                    &cwd,
                    source,
                    interrupted,
                    &collab,
                ),
                Err(e) => err_output(e),
            }
        }
        _ if oms_command_providers_skills::owns(tool, local) => {
            oms_command_providers_skills::run_with_source(tool, local, &cwd, source, interrupted)
        }
        _ if oms_command_panel::owns(tool, local) => {
            match services_collab::CollabServices::new(interrupted, source) {
                Ok(collab) => oms_command_panel::run_with_services(
                    tool,
                    local,
                    &cwd,
                    source,
                    interrupted,
                    &collab,
                ),
                Err(e) => err_output(e),
            }
        }
        _ => unreachable!(),
    };
    dispatch_exit(tool, local, &cwd, result);
}
fn main() {
    let interrupted = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        if let Err(e) = signal_hook::flag::register(sig, Arc::clone(&interrupted)) {
            eprintln!("runtime-core: cannot install interrupt handler: {e}\n{HINT}");
            std::process::exit(2);
        }
    }
    let mut argv: Vec<_> = std::env::args_os().skip(1).collect();
    if argv
        .first()
        .is_some_and(|v| v == services_control::SCAN_WORKER_FLAG)
    {
        std::process::exit(services_control::scan_worker_main(&argv[1..], &interrupted));
    }
    if argv
        .first()
        .is_some_and(|v| v == services_collab::DETACH_WORKER_FLAG)
    {
        std::process::exit(services_collab::detach_worker_main());
    }
    if argv
        .first()
        .is_some_and(|v| v == services_control::SUPERVISE_WORKER_FLAG)
    {
        std::process::exit(services_control::supervise_worker_main(
            &argv[1..],
            &interrupted,
        ));
    }
    let mut select = false;
    let mut dispatch = false;
    let mut source_root = None;
    if argv.first().is_some_and(|v| v == "--oms-frontdoor") {
        let (frontdoor_args, frontdoor_source) = prepare_frontdoor(&argv);
        argv = frontdoor_args;
        source_root = Some(frontdoor_source);
    } else {
        select = argv.first().is_some_and(|v| v == "--oms-select");
        dispatch = select || argv.first().is_some_and(|v| v == "--oms-dispatch");
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
    }
    if registry::owns(&argv) {
        if select {
            return;
        }
        let source = source_root.as_deref().unwrap_or(Path::new(""));
        let (code, out, err) = registry::run(&argv, source);
        use std::io::Write;
        if std::io::stdout().write_all(&out).is_err() || std::io::stderr().write_all(&err).is_err()
        {
            std::process::exit(1);
        }
        std::process::exit(code);
    }
    if let Some(tool) = argv
        .first()
        .and_then(|v| normalize_tool(v))
        .filter(|t| *t != "runtime")
        .map(str::to_owned)
    {
        if !owns_invocation(&argv) {
            if dispatch {
                std::process::exit(75);
            }
        } else {
            if select {
                return;
            }
            dispatch_owned(&tool, &argv[1..], source_root.as_deref(), &interrupted);
        }
    }
    if dispatch && argv.first().is_none_or(|v| v != "runtime") {
        std::process::exit(75);
    }
    let mut subcommand = false;
    let mut legacy = false;
    let parsed = parse(argv.clone(), &mut subcommand, &mut legacy);
    if select {
        return;
    }
    if legacy {
        let Some(source) = source_root.as_deref() else {
            eprintln!("error: runtime requires validated native source metadata\n{HINT}");
            std::process::exit(2);
        };
        let runtime_svc = match services_runtime::RuntimeServices::new(&interrupted, source) {
            Ok(svc) => svc,
            Err(e) => command_exit(
                "runtime",
                (2, vec![], format!("runtime-core: {e}\n").into_bytes()),
            ),
        };
        let resp = oms_runtime::run(
            &argv[1..],
            &caller_cwd("runtime"),
            source,
            &runtime_svc,
            &interrupted,
        );
        command_exit("runtime", (resp.code, resp.stdout, resp.stderr));
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
