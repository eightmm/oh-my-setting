//! Bounded native parser for the canonical argparse tree of `oms input`.
use crate::help::TEXTS;
use crate::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

pub type Exit = (i32, Vec<u8>, Vec<u8>);
#[derive(Default)]
pub struct Options {
    pub level: Option<Level>,
    pub positional: Vec<OsString>,
    pub values: BTreeMap<String, OsString>,
    pub lists: BTreeMap<String, Vec<OsString>>,
    pub flags: BTreeSet<&'static str>,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Root,
    Attach,
    File,
    Selection,
    Terminal,
    One,
    Many,
    Add,
    Edit,
    Move,
    List,
    Show,
    Preview,
    Cancel,
    Deliver,
    Reconcile,
    Feedback,
}
const ROOT_CHOICES: &[(&str, Level)] = &[
    ("attach", Level::Attach),
    ("attachment", Level::One),
    ("attachments", Level::Many),
    ("add", Level::Add),
    ("edit", Level::Edit),
    ("move", Level::Move),
    ("list", Level::List),
    ("show", Level::Show),
    ("preview", Level::Preview),
    ("cancel", Level::Cancel),
    ("deliver", Level::Deliver),
    ("reconcile", Level::Reconcile),
    ("feedback", Level::Feedback),
];
const ATTACH_CHOICES: &[(&str, Level)] = &[
    ("file", Level::File),
    ("selection", Level::Selection),
    ("terminal", Level::Terminal),
];
impl Level {
    fn name(self) -> &'static str {
        match self {
            Self::Root => "oms input",
            Self::Attach => "oms input attach",
            Self::File => "oms input attach file",
            Self::Selection => "oms input attach selection",
            Self::Terminal => "oms input attach terminal",
            Self::One => "oms input attachment",
            Self::Many => "oms input attachments",
            Self::Add => "oms input add",
            Self::Edit => "oms input edit",
            Self::Move => "oms input move",
            Self::List => "oms input list",
            Self::Show => "oms input show",
            Self::Preview => "oms input preview",
            Self::Cancel => "oms input cancel",
            Self::Deliver => "oms input deliver",
            Self::Reconcile => "oms input reconcile",
            Self::Feedback => "oms input feedback",
        }
    }
    fn texts(self) -> (&'static str, &'static str) {
        TEXTS
            .iter()
            .find(|(name, _, _)| *name == self.name())
            .map(|(_, usage, help)| (*usage, *help))
            .unwrap_or(("", ""))
    }
    fn usage(self) -> &'static str {
        self.texts().0
    }
    fn help(self) -> &'static str {
        self.texts().1
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
            Self::Terminal => &["--help", "--repo", "--json", "--pane", "--lines"],
            Self::One => &["--help", "--repo", "--json", "--full"],
            Self::Add => &[
                "--help",
                "--repo",
                "--json",
                "--room",
                "--from",
                "--to",
                "--mode",
                "--text",
                "--text-file",
                "--attachment",
            ],
            Self::Edit => &[
                "--help",
                "--repo",
                "--json",
                "--text",
                "--text-file",
                "--mode",
                "--attachment",
                "--clear-attachments",
            ],
            Self::Move => &["--help", "--repo", "--json", "--before"],
            Self::Deliver => &[
                "--help",
                "--repo",
                "--json",
                "--apply",
                "--at-boundary",
                "--expected-sha256",
            ],
            Self::Feedback => &[
                "--help", "--repo", "--json", "--batch", "--room", "--from", "--to", "--mode",
            ],
            _ => &["--help", "--repo", "--json"],
        }
    }
    fn positional(self) -> Option<&'static str> {
        match self {
            Self::File => Some("path"),
            Self::One
            | Self::Edit
            | Self::Move
            | Self::Show
            | Self::Preview
            | Self::Cancel
            | Self::Deliver
            | Self::Reconcile => Some("id"),
            _ => None,
        }
    }
    fn required(self) -> &'static [&'static str] {
        match self {
            Self::Selection => &["--source", "--origin"],
            Self::Terminal => &["--pane"],
            Self::Add => &["--room", "--from", "--to"],
            Self::Feedback => &["--batch", "--room", "--from", "--to"],
            _ => &[],
        }
    }
    fn text_group_required(self) -> bool {
        matches!(self, Self::Selection | Self::Add)
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
fn boolean(flag: &str) -> bool {
    matches!(
        flag,
        "--help" | "--json" | "--full" | "--clear-attachments" | "--apply" | "--at-boundary"
    )
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
            if boolean(flag) {
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
                options.flags.insert(flag);
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
                let value = if matches!(flag, "--start-line" | "--end-line" | "--lines") {
                    integer(&value).ok_or_else(|| {
                        level.error(format!(
                            "argument {label}: invalid int value: {}",
                            repr(&value)
                        ))
                    })?
                } else {
                    value
                };
                let choices: &[&str] = match flag {
                    "--source" => &["browser", "chat", "text"],
                    "--mode" => &["steer", "next"],
                    _ => &[],
                };
                if !choices.is_empty() && !value.to_str().is_some_and(|v| choices.contains(&v)) {
                    return Err(level.error(format!(
                        "argument {flag}: invalid choice: {} (choose from {})",
                        repr(&value),
                        choices.join(", ")
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
                if flag == "--attachment" {
                    options
                        .lists
                        .entry(flag.to_owned())
                        .or_default()
                        .push(value);
                } else {
                    options.values.insert(flag.to_owned(), value);
                }
            }
        } else {
            let choices = match level {
                Level::Root => Some((ROOT_CHOICES, "action")),
                Level::Attach => Some((ATTACH_CHOICES, "attachment_kind")),
                _ => None,
            };
            if let Some((choices, dest)) = choices {
                let Some((_, next)) = choices.iter().find(|(name, _)| raw.to_str() == Some(*name))
                else {
                    let names: Vec<_> = choices.iter().map(|(n, _)| *n).collect();
                    return Err(level.error(format!(
                        "argument {dest}: invalid choice: {} (choose from {})",
                        repr(raw),
                        names.join(", ")
                    )));
                };
                options.positional.push(raw.clone());
                if *next != Level::Attach {
                    options.level = Some(*next);
                }
                return parse_level(*next, &args[i + 1..], options, unknown);
            }
            if level.positional().is_some() && !positional {
                options.positional.push(raw.clone());
                positional = true;
            } else {
                unknown.push(display(raw));
            }
        }
        i += 1;
    }
    match level {
        Level::Root => return Err(level.error("the following arguments are required: action")),
        Level::Attach => {
            return Err(level.error("the following arguments are required: attachment_kind"));
        }
        _ => {}
    }
    if let Some(name) = level.positional().filter(|_| !positional) {
        return Err(level.error(format!("the following arguments are required: {name}")));
    }
    let required: Vec<_> = level
        .required()
        .iter()
        .filter(|key| !options.values.contains_key(**key))
        .copied()
        .collect();
    if !required.is_empty() {
        return Err(level.error(format!(
            "the following arguments are required: {}",
            required.join(", ")
        )));
    }
    if level.text_group_required() && text_flag.is_none() {
        return Err(level.error("one of the arguments --text --text-file is required"));
    }
    Ok(())
}
