//! Native implementation of the runtime failure command.
#![forbid(unsafe_code)]

use oms_json::{PyString, Value};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const USAGE: &str = "usage: oms runtime failure [-h] [--repo REPO] [--pretty]\n                           {classify,catalog} ...\n";
const CLASSIFY_USAGE: &str = "usage: oms runtime failure classify [-h] [--repo REPO] [--pretty]\n                                    [--exit EXIT] [--code CODE]\n                                    text\n";
const CATALOG_USAGE: &str = "usage: oms runtime failure catalog [-h] [--repo REPO] [--pretty]\n";
const ROOT_USAGE: &str = "usage: oms runtime [-h] [--repo REPO] [--pretty] [--version]\n                   {envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,resources,doctor}\n                   ...\n";
const HELP_COMMON: &str = "options:\n  -h, --help   show this help message and exit\n  --repo REPO  project repository or directory\n  --pretty     pretty-print JSON\n";

#[cfg(unix)]
fn points(value: &OsStr) -> Vec<u32> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = value.as_bytes();
    let mut result = Vec::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                result.extend(s.chars().map(u32::from));
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                result.extend(
                    std::str::from_utf8(&bytes[..good])
                        .unwrap()
                        .chars()
                        .map(u32::from),
                );
                let n = e.error_len().unwrap_or(bytes.len() - good);
                result.extend(bytes[good..good + n].iter().map(|b| 0xdc00 + u32::from(*b)));
                bytes = &bytes[good + n..];
            }
        }
    }
    result
}
#[cfg(not(unix))]
fn points(value: &OsStr) -> Vec<u32> {
    value.to_string_lossy().chars().map(u32::from).collect()
}

#[cfg(unix)]
fn attached_value(raw: &OsStr) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let bytes = raw.as_bytes();
    let index = bytes.iter().position(|b| *b == b'=').unwrap();
    OsString::from_vec(bytes[index + 1..].to_vec())
}
#[cfg(not(unix))]
fn attached_value(raw: &OsStr) -> OsString {
    raw.to_string_lossy().split_once('=').unwrap().1.into()
}

#[cfg(unix)]
fn expand_repo(raw: &OsStr) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    use users::os::unix::UserExt;
    let bytes = raw.as_bytes();
    if !bytes.starts_with(b"~") {
        return Some(PathBuf::from(raw));
    }
    let split = bytes.iter().position(|b| *b == b'/').unwrap_or(bytes.len());
    let name = &bytes[1..split];
    let home = if name.is_empty() {
        std::env::var_os("HOME").map(PathBuf::from).or_else(|| {
            users::get_user_by_uid(users::get_current_uid()).map(|u| u.home_dir().to_path_buf())
        })
    } else {
        users::get_user_by_name(OsStr::from_bytes(name)).map(|u| u.home_dir().to_path_buf())
    }?;
    let suffix = bytes[split..]
        .iter()
        .position(|b| *b != b'/')
        .map_or(&[][..], |i| &bytes[split + i..]);
    Some(home.join(OsStr::from_bytes(suffix)))
}
#[cfg(not(unix))]
fn expand_repo(raw: &OsStr) -> Option<PathBuf> {
    Some(PathBuf::from(raw))
}

fn json_string(value: &PyString) -> Vec<u8> {
    let mut out = vec![b'"'];
    for &p in value.codepoints() {
        match p {
            34 => out.extend_from_slice(b"\\\""),
            92 => out.extend_from_slice(b"\\\\"),
            8 => out.extend_from_slice(b"\\b"),
            9 => out.extend_from_slice(b"\\t"),
            10 => out.extend_from_slice(b"\\n"),
            12 => out.extend_from_slice(b"\\f"),
            13 => out.extend_from_slice(b"\\r"),
            0..=31 => out.extend_from_slice(format!("\\u{p:04x}").as_bytes()),
            0xdc80..=0xdcff => out.push((p - 0xdc00) as u8),
            _ => {
                let mut buf = [0; 4];
                out.extend_from_slice(
                    char::from_u32(p)
                        .unwrap_or('\u{fffd}')
                        .encode_utf8(&mut buf)
                        .as_bytes(),
                );
            }
        }
    }
    out.push(b'"');
    out
}

fn usage(action: &str) -> &'static str {
    match action {
        "classify" => CLASSIFY_USAGE,
        "catalog" => CATALOG_USAGE,
        _ => USAGE,
    }
}
fn error(action: &str, message: &str) -> (i32, Vec<u8>, Vec<u8>) {
    let name = if action.is_empty() {
        "oms runtime failure".to_owned()
    } else {
        format!("oms runtime failure {action}")
    };
    (
        2,
        vec![],
        format!("{}{name}: error: {message}\n", usage(action)).into_bytes(),
    )
}
fn os_bytes(value: &OsStr) -> Vec<u8> {
    let mut out = Vec::new();
    for p in points(value) {
        if (0xdc80..=0xdcff).contains(&p) {
            out.push((p - 0xdc00) as u8);
        } else if let Some(c) = char::from_u32(p) {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}
fn py_repr(value: &OsStr) -> String {
    let data = points(value);
    let quote = if data.contains(&39) && !data.contains(&34) {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for p in data {
        if p == quote as u32 || p == 92 {
            out.push('\\');
            out.push(char::from_u32(p).unwrap());
        } else if p == 9 {
            out.push_str("\\t");
        } else if p == 10 {
            out.push_str("\\n");
        } else if p == 13 {
            out.push_str("\\r");
        } else if p < 32 || (0x7f..=0x9f).contains(&p) {
            out.push_str(&format!("\\x{p:02x}"));
        } else if (0xd800..=0xdfff).contains(&p) {
            out.push_str(&format!("\\u{p:04x}"));
        } else if let Some(c) = char::from_u32(p) {
            out.push(c);
        }
    }
    out.push(quote);
    out
}
fn unknown(args: &[OsString]) -> (i32, Vec<u8>, Vec<u8>) {
    let mut err = format!("{ROOT_USAGE}oms runtime: error: unrecognized arguments: ").into_bytes();
    for (i, arg) in args.iter().enumerate() {
        if i != 0 {
            err.push(b' ');
        }
        err.extend_from_slice(&os_bytes(arg));
    }
    err.push(b'\n');
    (2, vec![], err)
}
fn help(action: &str) -> (i32, Vec<u8>, Vec<u8>) {
    let text = match action {
        "classify" => format!(
            "{CLASSIFY_USAGE}\npositional arguments:\n  text\n\n{HELP_COMMON}  --exit EXIT\n  --code CODE\n"
        ),
        "catalog" => format!("{CATALOG_USAGE}\n{HELP_COMMON}"),
        _ => format!(
            "{USAGE}\npositional arguments:\n  {{classify,catalog}}\n\noptions:\n  -h, --help          show this help message and exit\n  --repo REPO         project repository or directory\n  --pretty            pretty-print JSON\n"
        ),
    };
    (0, text.into_bytes(), vec![])
}

// Python int() accepts decimal digits from each Unicode decimal block.
fn digit(c: char) -> Option<char> {
    const ZEROS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
        0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60,
        0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0,
        0x1e950, 0x1fbf0,
    ];
    let n = u32::from(c);
    ZEROS
        .iter()
        .find(|&&z| n >= z && n < z + 10)
        .map(|z| char::from(b'0' + (n - z) as u8))
}
fn negative_number(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('-') else {
        return false;
    };
    let (whole, fraction) = rest
        .split_once('.')
        .map_or((rest, None), |(a, b)| (a, Some(b)));
    let numeric = |v: &str| v.chars().all(|c| digit(c).is_some());
    match fraction {
        None => !whole.is_empty() && numeric(whole),
        Some(f) => !f.is_empty() && numeric(whole) && numeric(f),
    }
}
fn integer(raw: &str) -> Option<String> {
    let s = raw.trim_matches(char::is_whitespace);
    let (negative, s) = if let Some(x) = s.strip_prefix('-') {
        (true, x)
    } else if let Some(x) = s.strip_prefix('+') {
        (false, x)
    } else {
        (false, s)
    };
    let mut digits = String::new();
    let mut prev = false;
    for c in s.chars() {
        if c == '_' {
            if !prev {
                return None;
            }
            prev = false;
        } else {
            digits.push(digit(c)?);
            prev = true;
        }
    }
    if !prev || digits.len() > 4300 {
        return None;
    }
    let s = digits.trim_start_matches('0');
    if s.is_empty() {
        Some("0".into())
    } else if negative {
        Some(format!("-{s}"))
    } else {
        Some(s.into())
    }
}

/// The entire failure namespace, including invalid invocations, is handled here.
pub fn owns(_args: &[OsString]) -> bool {
    true
}

/// Execute the failure namespace without changing process state.
pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let mut action = "";
    let mut pretty = false;
    let mut repo: Option<OsString> = None;
    let mut text: Option<OsString> = None;
    let mut explicit = String::new();
    let mut exit: Option<String> = None;
    let mut unknowns = Vec::new();
    let mut i = 0;
    let mut positional = false;
    while i < args.len() {
        let raw = &args[i];
        let s = raw.to_string_lossy();
        if !positional && s == "--" {
            if action == "catalog" {
                unknowns.push(OsString::from("--"));
            }
            positional = true;
            i += 1;
            continue;
        }
        let (name, attached) = s
            .split_once('=')
            .map_or((s.as_ref(), None), |(a, b)| (a, Some(b)));
        let options: &[&str] = if action == "classify" {
            &["--help", "--repo", "--pretty", "--exit", "--code"]
        } else {
            &["--help", "--repo", "--pretty"]
        };
        let matches: Vec<_> = options
            .iter()
            .filter(|opt| opt.starts_with(name) && name.starts_with("--"))
            .copied()
            .collect();
        let option = if s.starts_with("-h") && !s.starts_with("-h=") {
            Some("--help")
        } else if matches.len() == 1 {
            Some(matches[0])
        } else {
            None
        };
        if let Some(opt) = option.filter(|_| !positional) {
            if opt == "--help" && attached.is_none() {
                return help(action);
            }
            if opt == "--pretty" && attached.is_none() {
                pretty = true;
                i += 1;
                continue;
            }
            if opt == "--help" || opt == "--pretty" {
                return error(
                    action,
                    &format!(
                        "argument {opt}: ignored explicit argument {}",
                        py_repr(&attached_value(raw))
                    ),
                );
            }
            let value = if attached.is_some() {
                attached_value(raw)
            } else {
                i += 1;
                if i >= args.len()
                    || (args[i].to_string_lossy().starts_with('-')
                        && args[i] != "-"
                        && !args[i].to_string_lossy().contains(' ')
                        && !negative_number(&args[i].to_string_lossy()))
                {
                    return error(action, &format!("argument {opt}: expected one argument"));
                }
                args[i].clone()
            };
            match opt {
                "--repo" => repo = Some(value),
                "--code" => explicit = value.to_string_lossy().into_owned(),
                "--exit" => {
                    let v = value.to_string_lossy();
                    match integer(&v) {
                        Some(n) => exit = Some(n),
                        None => {
                            return error(
                                action,
                                &format!("argument --exit: invalid int value: {}", py_repr(&value)),
                            );
                        }
                    }
                }
                _ => unreachable!(),
            }
        } else if !positional
            && s.starts_with('-')
            && s != "-"
            && !s.contains(' ')
            && !negative_number(&s)
        {
            unknowns.push(raw.clone());
        } else if action.is_empty() {
            if s == "classify" {
                action = "classify";
                positional = false
            } else if s == "catalog" {
                action = "catalog";
                positional = false
            } else {
                return error(
                    "",
                    &format!(
                        "argument failure_action: invalid choice: {} (choose from classify, catalog)",
                        py_repr(raw)
                    ),
                );
            }
        } else if action == "classify" && text.is_none() {
            text = Some(raw.clone())
        } else {
            unknowns.push(raw.clone())
        }
        i += 1;
    }
    if action.is_empty() {
        return error("", "the following arguments are required: failure_action");
    }
    if action == "classify" && text.is_none() {
        return error(action, "the following arguments are required: text");
    }
    if !unknowns.is_empty() {
        return unknown(&unknowns);
    }
    if interrupted.load(Ordering::Relaxed) {
        return (130, vec![], b"runtime-core: interrupted\n".to_vec());
    }
    let raw_repo = repo.unwrap_or_else(|| OsString::from("."));
    let mut path = match expand_repo(&raw_repo) {
        Some(path) => path,
        None => {
            return (
                1,
                vec![],
                b"RuntimeError: Could not determine home directory.\n".to_vec(),
            );
        }
    };
    if path.is_relative() {
        path = default_repo.join(path)
    }
    if !path.is_dir() {
        let mut err = b"runtime-core: repository path is not a directory: ".to_vec();
        err.extend_from_slice(&os_bytes(&raw_repo));
        err.push(b'\n');
        return (2, vec![], err);
    }
    let mut matched = None;
    let value = if action == "catalog" {
        Value::Object(
            oms_failure::catalog()
                .into_iter()
                .map(|(k, v)| (PyString::from(k), v.to_json_value()))
                .collect::<BTreeMap<_, _>>(),
        )
    } else {
        let n = exit.as_deref().and_then(|v| v.parse::<i64>().ok());
        let mut result =
            oms_failure::classify_codepoints(&points(text.as_ref().unwrap()), n, &explicit)
                .expect("valid codepoints");
        matched = Some(json_string(&result.matched));
        result.matched = "".into();
        let mut obj = result.to_json_value();
        if let Value::Object(ref mut fields) = obj {
            fields.insert(
                "exit".into(),
                exit.as_deref()
                    .map_or(Value::Null, |v| Value::integer(v).unwrap()),
            );
        }
        obj
    };
    let mut out = if pretty {
        oms_json::pretty_json(&value).unwrap()
    } else {
        let mut v = oms_json::canonical_json(&value).unwrap();
        v.push(b'\n');
        v
    };
    if let Some(value) = matched {
        let needle = if pretty {
            b"\"matched\": \"\"".as_slice()
        } else {
            b"\"matched\":\"\"".as_slice()
        };
        let index = out.windows(needle.len()).position(|w| w == needle).unwrap() + needle.len() - 2;
        out.splice(index..index + 2, value);
    }
    (0, out, vec![])
}
