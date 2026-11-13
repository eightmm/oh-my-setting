//! Bounded native parser for the owned leaves of the canonical argparse tree.
const FILE_USAGE: &str = "usage: oms input attach file [-h] [--repo REPO] [--json]\n                             [--start-line START_LINE] [--end-line END_LINE]\n                             path\n";
const FILE_HELP: &str = "usage: oms input attach file [-h] [--repo REPO] [--json]\n                             [--start-line START_LINE] [--end-line END_LINE]\n                             path\n\npositional arguments:\n  path\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n  --start-line START_LINE\n  --end-line END_LINE\n";
const SELECTION_USAGE: &str = "usage: oms input attach selection [-h] [--repo REPO] [--json] --source\n                                  {browser,chat,text} --origin ORIGIN\n                                  (--text TEXT | --text-file TEXT_FILE)\n";
const SELECTION_HELP: &str = "usage: oms input attach selection [-h] [--repo REPO] [--json] --source\n                                  {browser,chat,text} --origin ORIGIN\n                                  (--text TEXT | --text-file TEXT_FILE)\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n  --source {browser,chat,text}\n  --origin ORIGIN\n  --text TEXT\n  --text-file TEXT_FILE\n";
const ONE_USAGE: &str = "usage: oms input attachment [-h] [--repo REPO] [--json] [--full] id\n";
const ONE_HELP: &str = "usage: oms input attachment [-h] [--repo REPO] [--json] [--full] id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n  --full       return the full bounded snapshot instead of its first 8 KiB\n";
const MANY_USAGE: &str = "usage: oms input attachments [-h] [--repo REPO] [--json]\n";
const MANY_HELP: &str = "usage: oms input attachments [-h] [--repo REPO] [--json]\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n";
const ROOT_USAGE: &str = "usage: oms input [-h] [--repo REPO] [--json]\n                 {attach,attachment,attachments,add,edit,move,list,show,preview,cancel,deliver,reconcile,feedback}\n                 ...\n";

const ATTACH_USAGE: &str = "usage: oms input attach [-h] [--repo REPO] [--json]\n                        {file,selection,terminal} ...\n";

use crate::json;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

pub type Exit = (i32, Vec<u8>, Vec<u8>);
#[derive(Default)]
pub struct Options {
    pub positional: Vec<OsString>,
    pub values: BTreeMap<String, OsString>,
    pub full: bool,
}
#[derive(Clone, Copy, PartialEq)]
enum Level {
    Root,
    Attach,
    File,
    Selection,
    One,
    Many,
}
impl Level {
    fn name(self) -> &'static str {
        match self {
            Self::Root => "oms input",
            Self::Attach => "oms input attach",
            Self::File => "oms input attach file",
            Self::Selection => "oms input attach selection",
            Self::One => "oms input attachment",
            Self::Many => "oms input attachments",
        }
    }
    fn usage(self) -> &'static str {
        match self {
            Self::Root => ROOT_USAGE,
            Self::Attach => ATTACH_USAGE,
            Self::File => FILE_USAGE,
            Self::Selection => SELECTION_USAGE,
            Self::One => ONE_USAGE,
            Self::Many => MANY_USAGE,
        }
    }
    fn help(self) -> &'static str {
        match self {
            Self::File => FILE_HELP,
            Self::Selection => SELECTION_HELP,
            Self::One => ONE_HELP,
            Self::Many => MANY_HELP,
            _ => "",
        }
    }
    fn flags(self) -> &'static [&'static str] {
        match self {
            Self::File => &["--help", "--repo", "--json", "--start-line", "--end-line"],
            Self::Selection => &[
                "--help",
                "--repo",
                "--json",
                "--source",
                "--origin",
                "--text",
                "--text-file",
            ],
            Self::One => &["--help", "--repo", "--json", "--full"],
            _ => &["--help", "--repo", "--json"],
        }
    }
    fn error(self, message: impl AsRef<str>) -> Exit {
        (
            2,
            vec![],
            format!(
                "{}{}: error: {}\n",
                self.usage(),
                self.name(),
                message.as_ref()
            )
            .into_bytes(),
        )
    }
}
fn display(raw: &OsStr) -> String {
    let json::J::String(bytes) = &json::os_string(raw) else {
        unreachable!()
    };
    let mut result = String::new();
    for p in json::points(bytes).unwrap_or_default() {
        if let Some(c) = char::from_u32(p) {
            result.push(c);
        } else {
            result.push_str(&format!("\\u{p:04x}"));
        }
    }
    result
}
fn repr(raw: &OsStr) -> String {
    let json::J::String(bytes) = &json::os_string(raw) else {
        unreachable!()
    };
    let points = json::points(bytes).unwrap_or_default();
    let quote = if points.contains(&39) && !points.contains(&34) {
        '"'
    } else {
        '\''
    };
    let mut out = quote.to_string();
    for p in points {
        match p {
            9 => out.push_str("\\t"),
            10 => out.push_str("\\n"),
            13 => out.push_str("\\r"),
            92 => out.push_str("\\\\"),
            _ if p == quote as u32 => {
                out.push('\\');
                out.push(quote);
            }
            0..=31 | 127..=159 => out.push_str(&format!("\\x{p:02x}")),
            0xd800..=0xdfff => out.push_str(&format!("\\u{p:04x}")),
            _ => out.push(char::from_u32(p).unwrap()),
        }
    }
    out.push(quote);
    out
}
fn decimal(c: char) -> Option<u8> {
    let point = c as u32;
    DECIMAL_STARTS
        .iter()
        .copied()
        .find(|start| (*start..*start + 10).contains(&point))
        .map(|start| (point - start) as u8)
}
fn negative_number(raw: &[u8]) -> bool {
    let Some(rest) = raw
        .strip_prefix(b"-")
        .and_then(|s| std::str::from_utf8(s).ok())
    else {
        return false;
    };
    if !rest.is_empty() && rest.chars().all(|c| decimal(c).is_some()) {
        return true;
    }
    if let Some((before, after)) = rest.split_once('.') {
        return before.chars().all(|c| decimal(c).is_some())
            && !after.is_empty()
            && after.chars().all(|c| decimal(c).is_some());
    }
    false
}

fn optional(level: Level, raw: &OsStr) -> bool {
    let b = raw.as_bytes();
    let name = &b[..b.iter().position(|c| *c == b'=').unwrap_or(b.len())];
    // argparse treats unmatched dash words containing a space as positional values.
    if b.contains(&b' ')
        && !level
            .flags()
            .iter()
            .any(|flag| flag.as_bytes().starts_with(name))
        && !(name.starts_with(b"-h") && !name.starts_with(b"--"))
    {
        return false;
    }
    b.len() > 1 && b[0] == b'-' && !negative_number(b)
}

const DECIMAL_STARTS: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650,
    0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60, 0x16ac0,
    0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950,
    0x1fbf0,
];
fn integer(raw: &OsStr) -> Option<OsString> {
    let text = raw.to_str()?.trim();
    let (sign, digits) = if let Some(d) = text.strip_prefix('-') {
        ("-", d)
    } else {
        ("", text.strip_prefix('+').unwrap_or(text))
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut plain = String::new();
    for c in digits.chars().filter(|c| *c != '_') {
        plain.push((b'0' + decimal(c)?) as char);
    }
    if plain.len() > 4300 {
        return None;
    }
    let plain = plain.trim_start_matches('0');
    Some(OsString::from(if plain.is_empty() {
        "0".to_owned()
    } else {
        format!("{sign}{plain}")
    }))
}

pub fn parse(args: &[OsString]) -> Result<Options, Exit> {
    let mut options = Options::default();
    let mut unknown = Vec::new();
    parse_level(Level::Root, args, &mut options, &mut unknown)?;
    if !unknown.is_empty() {
        return Err(Level::Root.error(format!("unrecognized arguments: {}", unknown.join(" "))));
    }
    Ok(options)
}
fn parse_level(
    level: Level,
    args: &[OsString],
    options: &mut Options,
    unknown: &mut Vec<String>,
) -> Result<(), Exit> {
    let mut i = 0;
    let mut literal = false;
    let mut positional = false;
    let mut text_flag: Option<&str> = None;
    while i < args.len() {
        let raw = &args[i];
        let bytes = raw.as_bytes();
        if !literal && bytes == b"--" {
            literal = true;
            i += 1;
            continue;
        }
        if !literal && optional(level, raw) {
            let at = bytes.iter().position(|b| *b == b'=');
            let name = &bytes[..at.unwrap_or(bytes.len())];
            let candidates: Vec<_> = level
                .flags()
                .iter()
                .copied()
                .filter(|f| f.as_bytes().starts_with(name))
                .collect();
            let flag = if name.starts_with(b"-h") && !name.starts_with(b"--") {
                Some("--help")
            } else if let Some(exact) = level.flags().iter().copied().find(|f| f.as_bytes() == name)
            {
                Some(exact)
            } else if name.starts_with(b"--") && candidates.len() == 1 {
                Some(candidates[0])
            } else if name.starts_with(b"--") && candidates.len() > 1 {
                return Err(level.error(format!(
                    "ambiguous option: {} could match {}",
                    display(raw),
                    candidates.join(", ")
                )));
            } else {
                None
            };
            let Some(flag) = flag else {
                unknown.push(display(raw));
                i += 1;
                continue;
            };
            let label = if flag == "--help" { "-h/--help" } else { flag };
            if matches!(flag, "--help" | "--json" | "--full") {
                if flag == "--help" && name.starts_with(b"-h") && at.is_none() {
                    return Err((0, level.help().as_bytes().to_vec(), vec![]));
                }
                if let Some(at) = at {
                    return Err(level.error(format!(
                        "argument {label}: ignored explicit argument {}",
                        repr(OsStr::from_bytes(&bytes[at + 1..]))
                    )));
                }
                if flag == "--help" {
                    return Err((0, level.help().as_bytes().to_vec(), vec![]));
                }
                if flag == "--full" {
                    options.full = true;
                }
            } else {
                let value = if let Some(at) = at {
                    OsStr::from_bytes(&bytes[at + 1..]).to_owned()
                } else {
                    i += 1;
                    let Some(next) = args
                        .get(i)
                        .filter(|v| !optional(level, v) && v.as_bytes() != b"--")
                    else {
                        return Err(level.error(format!("argument {label}: expected one argument")));
                    };
                    next.clone()
                };
                let value = if matches!(flag, "--start-line" | "--end-line") {
                    integer(&value).ok_or_else(|| {
                        level.error(format!(
                            "argument {label}: invalid int value: {}",
                            repr(&value)
                        ))
                    })?
                } else {
                    value
                };
                if flag == "--source"
                    && !matches!(value.to_str(), Some("browser" | "chat" | "text"))
                {
                    return Err(level.error(format!(
                        "argument --source: invalid choice: {} (choose from browser, chat, text)",
                        repr(&value)
                    )));
                }
                if matches!(flag, "--text" | "--text-file") {
                    if let Some(previous) = text_flag.filter(|previous| *previous != flag) {
                        return Err(level.error(format!(
                            "argument {flag}: not allowed with argument {previous}"
                        )));
                    }
                    text_flag = Some(flag);
                }
                options.values.insert(flag.to_owned(), value);
            }
        } else {
            match level {
                Level::Root | Level::Attach => {
                    let next = match (level, raw.to_str()) {
                        (Level::Root, Some("attach")) => Level::Attach,
                        (Level::Root, Some("attachment")) => Level::One,
                        (Level::Root, Some("attachments")) => Level::Many,
                        (Level::Attach, Some("file")) => Level::File,
                        (Level::Attach, Some("selection")) => Level::Selection,
                        _ => return Err(level.error("invalid input action")),
                    };
                    options.positional.push(raw.clone());
                    return parse_level(next, &args[i + 1..], options, unknown);
                }
                Level::File | Level::One if !positional => {
                    options.positional.push(raw.clone());
                    positional = true;
                }
                _ => unknown.push(display(raw)),
            }
        }
        i += 1;
    }
    match level {
        Level::File if !positional => {
            return Err(level.error("the following arguments are required: path"));
        }
        Level::One if !positional => {
            return Err(level.error("the following arguments are required: id"));
        }
        Level::Selection => {
            let required: Vec<_> = ["--source", "--origin"]
                .into_iter()
                .filter(|key| !options.values.contains_key(*key))
                .collect();
            if !required.is_empty() {
                return Err(level.error(format!(
                    "the following arguments are required: {}",
                    required.join(", ")
                )));
            }
            if text_flag.is_none() {
                return Err(level.error("one of the arguments --text --text-file is required"));
            }
        }
        _ => {}
    }
    Ok(())
}
