//! Native handler for the read-only experiment contract actions and template export.
#![forbid(unsafe_code)]
use oms_context::Json;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn error(s: impl std::fmt::Display) -> (i32, Vec<u8>, Vec<u8>) {
    (2, Vec::new(), format!("runtime-core: {s}\n").into_bytes())
}
fn usage(action: &str) -> String {
    match action {
        "template" => "usage: oms runtime experiment template [-h] [--repo REPO] [--pretty]\n                                       [--output OUTPUT]\n".into(),
        "validate" => "usage: oms runtime experiment validate [-h] [--repo REPO] [--pretty] contract\n".into(),
        "compare" => "usage: oms runtime experiment compare [-h] [--repo REPO] [--pretty] left right\n".into(),
        _ => format!("usage: oms runtime experiment {action}\n"),
    }
}
fn parse_error(action: &str, message: &str) -> (i32, Vec<u8>, Vec<u8>) {
    (
        2,
        Vec::new(),
        format!(
            "{}oms runtime experiment {action}: error: {message}\n",
            usage(action)
        )
        .into_bytes(),
    )
}
fn unrecognized(args: &[OsString]) -> (i32, Vec<u8>, Vec<u8>) {
    let joined = args
        .iter()
        .map(|x| x.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    (2, vec![], format!("usage: oms runtime [-h] [--repo REPO] [--pretty] [--version]\n                   {{envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,resources,doctor}}\n                   ...\noms runtime: error: unrecognized arguments: {joined}\n").into_bytes())
}
fn help(action: &str) -> Vec<u8> {
    let body = match action {
        "template" => {
            "\noptions:\n  -h, --help       show this help message and exit\n  --repo REPO      project repository or directory\n  --pretty         pretty-print JSON\n  --output OUTPUT\n"
        }
        "validate" => {
            "\npositional arguments:\n  contract\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO  project repository or directory\n  --pretty     pretty-print JSON\n"
        }
        _ => {
            "\npositional arguments:\n  left\n  right\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO  project repository or directory\n  --pretty     pretty-print JSON\n"
        }
    };
    format!("{}{body}", usage(action)).into_bytes()
}
fn absolute(base: &Path, path: &OsStr) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}
enum NativeError {
    Core(Vec<u8>),
    Runtime(&'static str),
}
impl From<String> for NativeError {
    fn from(message: String) -> Self {
        Self::Core(message.into_bytes())
    }
}
fn native_error(error: NativeError) -> (i32, Vec<u8>, Vec<u8>) {
    match error {
        NativeError::Core(message) => {
            let mut out = b"runtime-core: ".to_vec();
            out.extend(message);
            out.push(b'\n');
            (2, vec![], out)
        }
        NativeError::Runtime(message) => {
            (1, vec![], format!("RuntimeError: {message}\n").into_bytes())
        }
    }
}
fn diagnostic_os(value: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = value.as_bytes();
        let mut rendered = Vec::new();
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(_) => {
                    rendered.extend_from_slice(bytes);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    rendered.extend_from_slice(&bytes[..valid]);
                    let len = error.error_len().unwrap_or(bytes.len() - valid);
                    for byte in &bytes[valid..valid + len] {
                        rendered.extend_from_slice(format!("\\udc{byte:02x}").as_bytes());
                    }
                    bytes = &bytes[valid + len..];
                }
            }
        }
        rendered
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().as_bytes().to_vec()
    }
}
fn path_error(prefix: &str, path: &OsStr, suffix: &str) -> NativeError {
    let mut message = prefix.as_bytes().to_vec();
    message.extend_from_slice(&diagnostic_os(path));
    message.extend_from_slice(suffix.as_bytes());
    NativeError::Core(message)
}
#[cfg(unix)]
fn expand_repo_with(
    raw: &OsStr,
    home: Option<&OsStr>,
    lookup: impl FnOnce(&OsStr) -> Option<PathBuf>,
    current: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, NativeError> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = raw.as_bytes();
    if !bytes.starts_with(b"~") {
        return Ok(if bytes.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(raw)
        });
    }
    let split = bytes.iter().position(|b| *b == b'/').unwrap_or(bytes.len());
    let username = &bytes[1..split];
    let home = if username.is_empty() {
        home.map(PathBuf::from).or_else(current)
    } else {
        lookup(OsStr::from_bytes(username))
    }
    .ok_or(NativeError::Runtime("Could not determine home directory."))?;
    let home = if home.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        home
    };
    let suffix = &bytes[split..];
    let suffix = &suffix[suffix
        .iter()
        .position(|b| *b != b'/')
        .unwrap_or(suffix.len())..];
    Ok(if suffix.is_empty() {
        home
    } else {
        home.join(OsStr::from_bytes(suffix))
    })
}
fn resolve_repo(base: &Path, raw: &OsStr) -> Result<PathBuf, NativeError> {
    #[cfg(unix)]
    let expanded = {
        use users::os::unix::UserExt;
        expand_repo_with(
            raw,
            std::env::var_os("HOME").as_deref(),
            |name| users::get_user_by_name(name).map(|user| user.home_dir().to_path_buf()),
            || {
                users::get_user_by_uid(users::get_current_uid())
                    .map(|user| user.home_dir().to_path_buf())
            },
        )?
    };
    #[cfg(not(unix))]
    let expanded = PathBuf::from(raw);
    let path = absolute(base, expanded.as_os_str());
    if !path.is_dir() {
        return Err(path_error("repository path is not a directory: ", raw, ""));
    }
    if let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        && out.status.success()
    {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            return PathBuf::from(OsString::from_vec(
                out.stdout
                    .strip_suffix(b"\n")
                    .unwrap_or(&out.stdout)
                    .to_vec(),
            ))
            .canonicalize()
            .map_err(|e| e.to_string().into());
        }
        #[cfg(not(unix))]
        {
            return PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end())
                .canonicalize()
                .map_err(|e| e.to_string().into());
        }
    }
    path.canonicalize().map_err(|e| e.to_string().into())
}
fn os_json_bytes(raw: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut input = raw.as_bytes();
        let mut result = Vec::new();
        while !input.is_empty() {
            match std::str::from_utf8(input) {
                Ok(_) => {
                    result.extend_from_slice(input);
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    result.extend_from_slice(&input[..valid]);
                    let len = e.error_len().unwrap_or(input.len() - valid);
                    for &b in &input[valid..valid + len] {
                        let p = 0xdc00u32 + u32::from(b);
                        result.extend_from_slice(&[
                            0xe0 | (p >> 12) as u8,
                            0x80 | ((p >> 6) & 63) as u8,
                            0x80 | (p & 63) as u8,
                        ]);
                    }
                    input = &input[valid + len..];
                }
            }
        }
        result
    }
    #[cfg(not(unix))]
    {
        raw.to_string_lossy().as_bytes().to_vec()
    }
}
fn input(raw: &OsStr, cwd: &Path) -> Result<Json, NativeError> {
    let path = absolute(cwd, raw);
    let metadata = fs::symlink_metadata(&path).ok();
    let is_file = metadata
        .as_ref()
        .is_some_and(|m| m.is_file() && !m.file_type().is_symlink());
    let bytes = if is_file {
        let m = metadata.as_ref().unwrap();
        if m.len() > 8 * 1024 * 1024 {
            return Err(path_error(
                "refusing oversized state file ",
                raw,
                &format!(" ({} > 8388608 bytes)", m.len()),
            ));
        }
        let file = fs::File::open(&path)
            .map_err(|e| path_error("cannot read ", raw, &format!(": {e}")))?;
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| path_error("cannot read ", raw, &format!(": {e}")))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(path_error(
                "refusing oversized state file ",
                raw,
                &format!(" ({} > 8388608 bytes)", bytes.len()),
            ));
        }
        std::str::from_utf8(&bytes)
            .map_err(|e| path_error("state file is not UTF-8: ", raw, &format!(": {e}")))?;
        bytes
    } else {
        os_json_bytes(raw)
    };
    oms_context::parse_experiment_json(&bytes, usize::MAX).map_err(|e| {
        if is_file {
            path_error("invalid JSON in ", raw, &format!(": {e}"))
        } else {
            NativeError::from(format!("expected JSON or a JSON file: {e}"))
        }
    })
}
fn write_template(path: &Path, bytes: &[u8], interrupted: &AtomicBool) -> Result<(), String> {
    if interrupted.load(Ordering::Relaxed) {
        return Err("interrupted".into());
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let parts: Vec<_> = parent.components().collect();
    if let Some(start) = parts.iter().rposition(|part| part.as_os_str() == ".oms") {
        let mut current = PathBuf::new();
        for (index, part) in parts.iter().enumerate() {
            current.push(part.as_os_str());
            if index >= start {
                match fs::symlink_metadata(&current) {
                    Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                        return Err(format!(
                            "repository state directory must be a real directory: {}",
                            current.display()
                        ));
                    }
                    Ok(_) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        fs::create_dir(&current).map_err(|e| e.to_string())?
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
    } else {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if fs::symlink_metadata(parent).is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir()) {
        return Err(format!(
            "output directory must be a real directory: {}",
            parent.display()
        ));
    }
    let parent = fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .ok_or_else(|| "invalid output path".to_string())?;
    let target = parent.join(name);
    if fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink() || !m.is_file()) {
        return Err(format!(
            "output must be a regular non-symlink file: {}",
            target.display()
        ));
    }
    for _ in 0..100 {
        let id = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(
            ".{}.{}.{}",
            name.to_string_lossy(),
            std::process::id(),
            id
        ));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&temp) {
            Ok(mut f) => {
                let result = f
                    .write_all(bytes)
                    .and_then(|()| f.sync_all())
                    .and_then(|()| {
                        if interrupted.load(Ordering::Relaxed) {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::Interrupted,
                                "interrupted",
                            ))
                        } else {
                            fs::rename(&temp, &target)
                        }
                    });
                if result.is_err() {
                    let _ = fs::remove_file(&temp);
                }
                result.map_err(|e| e.to_string())?;
                if let Ok(dir) = fs::File::open(&parent) {
                    let _ = dir.sync_all();
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("cannot allocate temporary output".into())
}
fn option(arg: &OsStr) -> Option<(&'static str, Option<OsString>)> {
    #[cfg(unix)]
    let (name, val) = {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let bytes = arg.as_bytes();
        let pos = bytes.iter().position(|b| *b == b'=');
        (
            std::str::from_utf8(&bytes[..pos.unwrap_or(bytes.len())]).ok()?,
            pos.map(|p| OsString::from_vec(bytes[p + 1..].to_vec())),
        )
    };
    #[cfg(not(unix))]
    let (name, val) = {
        let text = arg.to_str()?;
        text.split_once('=')
            .map_or((text, None), |(a, b)| (a, Some(OsString::from(b))))
    };
    let matched = if name.starts_with("--") && name.len() > 2 {
        ["--repo", "--pretty", "--output", "--help"]
            .into_iter()
            .find(|item| item.starts_with(name))
    } else {
        None
    }?;
    Some((matched, val))
}
fn action_index(args: &[OsString]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        match option(&args[i]) {
            Some(("--repo", value)) => {
                if value.is_none() {
                    i += 1;
                    if i >= args.len()
                        || (args[i] != "-" && args[i].to_string_lossy().starts_with('-'))
                    {
                        return None;
                    }
                }
            }
            Some(("--pretty", None)) => (),
            Some(_) => return None,
            None => return Some(i),
        }
        i += 1;
    }
    None
}
/// True when the parsed action is owned by this handler.
pub fn owns(args: &[OsString]) -> bool {
    action_index(args).is_some_and(|i| {
        ["template", "validate", "compare"]
            .iter()
            .any(|a| args[i] == *a)
    })
}
fn exception(e: oms_experiment::Error) -> (i32, Vec<u8>, Vec<u8>) {
    let name = match &e {
        oms_experiment::Error::Type(_) => "TypeError",
        oms_experiment::Error::Value(_) => "ValueError",
        oms_experiment::Error::Overflow(_) => "OverflowError",
        _ => return error(e),
    };
    (1, vec![], format!("{name}: {e}\n").into_bytes())
}
/// Execute experiment arguments and return exit status, stdout and stderr.
pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    if !owns(args) {
        return (75, vec![], vec![]);
    }
    if interrupted.load(Ordering::Relaxed) {
        return (130, vec![], b"runtime-core: interrupted\n".to_vec());
    }
    let Some(action_pos) = action_index(args) else {
        return (75, vec![], vec![]);
    };
    let Some(action) = args[action_pos].to_str() else {
        return (75, vec![], vec![]);
    };
    let mut repo = default_repo.as_os_str().to_os_string();
    let mut pretty = false;
    let mut output: Option<OsString> = None;
    let expected = match action {
        "template" => 0,
        "validate" => 1,
        _ => 2,
    };
    let mut positional = Vec::new();
    let mut unknown = Vec::new();
    let mut i = 0;
    let mut end = false;
    while i < args.len() {
        if i == action_pos {
            i += 1;
            continue;
        }
        let arg = &args[i];
        if !end && arg == "--" {
            end = true;
            i += 1;
            continue;
        }
        if !end && let Some((name @ ("--help" | "--pretty"), Some(value))) = option(arg) {
            let label = if name == "--help" {
                "-h/--help"
            } else {
                "--pretty"
            };
            return parse_error(
                action,
                &format!(
                    "argument {label}: ignored explicit argument '{}'",
                    value.to_string_lossy()
                ),
            );
        }
        if !end && (arg == "-h" || matches!(option(arg), Some(("--help", None)))) {
            return (0, help(action), vec![]);
        }
        if !end && matches!(option(arg), Some(("--pretty", None))) {
            pretty = true;
            i += 1;
            continue;
        }
        if !end
            && let Some((name @ ("--repo" | "--output"), value)) = option(arg)
            && (name == "--repo" || (action == "template" && i > action_pos))
        {
            if let Some(value) = value {
                if name == "--repo" {
                    repo = value;
                } else {
                    output = Some(value);
                }
                i += 1;
                continue;
            }
            if i + 1 == args.len()
                || (args[i + 1] != "-" && args[i + 1].to_string_lossy().starts_with('-'))
            {
                return parse_error(
                    action,
                    &format!("argument {}: expected one argument", arg.to_string_lossy()),
                );
            }
            i += 1;
            if name == "--repo" {
                repo = args[i].clone();
            } else {
                output = Some(args[i].clone());
            }
            i += 1;
            continue;
        }
        if !end && arg != "-" && arg.to_string_lossy().starts_with('-') {
            unknown.push(arg.clone());
            i += 1;
            continue;
        }
        if positional.len() < expected {
            positional.push(arg.clone());
        } else {
            unknown.push(arg.clone());
        }
        i += 1;
    }
    if positional.len() < expected {
        return parse_error(
            action,
            if positional.len() < expected {
                if action == "validate" {
                    "the following arguments are required: contract"
                } else if positional.is_empty() {
                    "the following arguments are required: left, right"
                } else {
                    "the following arguments are required: right"
                }
            } else {
                unreachable!()
            },
        );
    }
    if !unknown.is_empty() {
        return unrecognized(&unknown);
    }
    if std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1")
        && output.as_ref().is_some_and(|v| !v.is_empty())
    {
        return error("a harness child may only use read-only runtime actions");
    }
    let repo = match resolve_repo(default_repo, &repo) {
        Ok(repo) => repo,
        Err(e) => return native_error(e),
    };
    let value = match action {
        "template" => oms_experiment::template(),
        "validate" => {
            let v = match input(&positional[0], default_repo) {
                Ok(v) => v,
                Err(e) => return native_error(e),
            };
            match oms_experiment::validate(&v) {
                Ok(v) => v.normalized,
                Err(e) => return exception(e),
            }
        }
        _ => {
            let a = match input(&positional[0], default_repo) {
                Ok(v) => v,
                Err(e) => return native_error(e),
            };
            let b = match input(&positional[1], default_repo) {
                Ok(v) => v,
                Err(e) => return native_error(e),
            };
            match oms_experiment::compare(&a, &b) {
                Ok(v) => v,
                Err(e) => return exception(e),
            }
        }
    };
    if interrupted.load(Ordering::Relaxed) {
        return (130, vec![], b"runtime-core: interrupted\n".to_vec());
    }
    if let Some(output) = output
        && !output.is_empty()
    {
        let bytes = match oms_experiment::encode_json(&value, true) {
            Ok(b) => b,
            Err(e) => return error(e),
        };
        if let Err(e) = write_template(&absolute(&repo, &output), &bytes, interrupted) {
            if interrupted.load(Ordering::Relaxed) {
                return (130, vec![], b"runtime-core: interrupted\n".to_vec());
            }
            return error(e);
        }
    }
    match oms_experiment::encode_json(&value, pretty) {
        Ok(mut bytes) => {
            if !pretty {
                bytes.push(b'\n');
            }
            (0, bytes, vec![])
        }
        Err(e) => error(e),
    }
}

#[cfg(all(test, unix))]
mod home_tests {
    use super::*;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    fn expanded(raw: &OsStr, home: Option<&OsStr>) -> PathBuf {
        expand_repo_with(
            raw,
            home,
            |name| {
                assert_eq!(name.as_bytes(), b"account-\xff");
                Some(PathBuf::from(OsString::from_vec(
                    b"account-home-\xfe".to_vec(),
                )))
            },
            || Some(PathBuf::from("current-home")),
        )
        .ok()
        .unwrap()
    }

    #[test]
    fn home_lookup_policy_preserves_os_paths() {
        assert_eq!(
            expanded(OsStr::new("~"), None),
            PathBuf::from("current-home")
        );
        assert_eq!(
            expanded(OsStr::new("~/nested"), None),
            PathBuf::from("current-home/nested")
        );
        assert_eq!(
            expanded(OsStr::new("~"), Some(OsStr::new(""))),
            PathBuf::from("/")
        );
        assert_eq!(
            expanded(OsStr::new("~/nested"), Some(OsStr::new(""))),
            PathBuf::from("/nested")
        );
        assert_eq!(
            expanded(OsStr::new("~///nested"), Some(OsStr::new("relative/"))),
            PathBuf::from("relative/nested")
        );
        let raw_home = OsString::from_vec(b"home-\xff".to_vec());
        assert_eq!(
            expanded(OsStr::new("~/nested"), Some(&raw_home)),
            PathBuf::from(&raw_home).join("nested")
        );
        let named = OsString::from_vec(b"~account-\xff/nested-\xfd".to_vec());
        assert_eq!(
            expanded(&named, Some(OsStr::new("ignored")))
                .as_os_str()
                .as_bytes(),
            b"account-home-\xfe/nested-\xfd"
        );
        let raw = OsString::from_vec(b"plain-\xff".to_vec());
        assert_eq!(expanded(&raw, None), PathBuf::from(raw));
        assert_eq!(expanded(OsStr::new(""), None), PathBuf::from("."));
        assert!(matches!(
            expand_repo_with(OsStr::new("~missing"), None, |_| None, || None),
            Err(NativeError::Runtime("Could not determine home directory."))
        ));
        assert!(matches!(
            expand_repo_with(OsStr::new("~"), None, |_| None, || None),
            Err(NativeError::Runtime("Could not determine home directory."))
        ));
    }

    #[test]
    fn current_account_uses_system_lookup() {
        use users::os::unix::UserExt;
        let Some(user) = users::get_user_by_uid(users::get_current_uid()) else {
            return;
        };
        let mut named = b"~".to_vec();
        named.extend_from_slice(user.name().as_bytes());
        let selected = expand_repo_with(
            OsStr::from_bytes(&named),
            None,
            |name| users::get_user_by_name(name).map(|u| u.home_dir().to_path_buf()),
            || None,
        )
        .ok()
        .unwrap();
        let expected = expand_repo_with(
            OsStr::new("~"),
            None,
            |_| None,
            || Some(user.home_dir().to_path_buf()),
        )
        .ok()
        .unwrap();
        assert_eq!(selected, expected);
        let repo = resolve_repo(Path::new("."), OsStr::from_bytes(&named));
        assert!(
            repo.is_ok(),
            "account home is a directory on supported test hosts"
        );
    }
}
