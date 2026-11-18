//! Native rendering of the existing argparse declaration metadata.
use crate::parser::{Mode, Request};
use crate::value::*;
fn path(r: &Request) -> Vec<&str> {
    let name = match r.mode {
        Mode::Envelope => "envelope",
        Mode::Evidence => "evidence",
        Mode::Next => "next",
        Mode::Brief => "brief",
        Mode::Failure => "failure",
        Mode::Context => "context",
        Mode::ContextInspect => "context-inspect",
        Mode::Profile => "profile",
        Mode::Release => "release",
        Mode::Capsule => "capsule",
        Mode::Backend => "backend",
        Mode::Experiment => "experiment",
        Mode::Benchmark => "benchmark",
        Mode::Resources => "resources",
        Mode::Doctor => "doctor",
        _ => return vec![],
    };
    let mut out = vec![name];
    if !r.action.is_empty() && r.mode != Mode::Envelope {
        out.push(&r.action)
    }
    if !r.subaction.is_empty() {
        out.push(&r.subaction)
    }
    out
}
fn metadata(r: &Request) -> Value {
    static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let all = SCHEMA
        .get_or_init(|| parse(crate::cli_schema::SCOPES.as_bytes()).expect("native CLI metadata"));
    let wanted = path(r);
    rows(all)
        .iter()
        .find(|s| s.get("path").strings().ok().as_deref() == Some(wanted.as_slice()))
        .cloned()
        .unwrap_or_else(|| rows(all)[0].clone())
}
pub(crate) fn program(r: &Request) -> String {
    let path = path(r);
    if path.is_empty() {
        "oms runtime".into()
    } else {
        format!("oms runtime {}", path.join(" "))
    }
}
fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(80)
        .saturating_sub(2)
        .max(20)
}
fn names(v: &Value) -> Vec<&str> {
    v.get("names").strings().expect("argument names")
}
fn positional(v: &Value) -> bool {
    !names(v)[0].starts_with('-')
}
fn choice(v: &Value) -> Option<String> {
    let values = rows(v.get("choices"));
    (!values.is_empty()).then(|| {
        format!(
            "{{{}}}",
            values.iter().map(text).collect::<Vec<_>>().join(",")
        )
    })
}
fn metavar(v: &Value) -> String {
    if let Some(value) = v.opt("metavar") {
        return text(value).into();
    }
    if let Some(c) = choice(v) {
        return c;
    }
    if positional(v) {
        names(v)[0].into()
    } else {
        names(v)
            .last()
            .unwrap()
            .trim_start_matches('-')
            .replace('-', "_")
            .to_uppercase()
    }
}
fn arity(v: &Value) -> String {
    if matches!(v.text("action"), "help" | "version" | "store_true") {
        return String::new();
    }
    let m = metavar(v);
    match v.text("nargs") {
        "?" => format!("[{m}]"),
        "+" => format!("{m} [{m} ...]"),
        "*" => format!("[{m} ...]"),
        "REMAINDER" => "...".into(),
        _ => m,
    }
}
fn invocation(v: &Value) -> String {
    let names = names(v);
    if positional(v) {
        return metavar(v);
    }
    let arity = arity(v);
    names
        .into_iter()
        .map(|n| {
            if arity.is_empty() {
                n.into()
            } else {
                format!("{n} {arity}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
fn usage_part(v: &Value) -> String {
    if positional(v) {
        return arity(v);
    }
    let n = names(v)[0].to_string();
    let arg = arity(v);
    let p = if arg.is_empty() {
        n
    } else {
        format!("{n} {arg}")
    };
    if v.get("required").truth() {
        p
    } else {
        format!("[{p}]")
    }
}
fn wrapped(
    words: impl IntoIterator<Item = String>,
    first: &str,
    indent: &str,
    width: usize,
) -> String {
    let mut out = String::from(first);
    let mut length = first.chars().count();
    let mut has = false;
    for word in words {
        let n = word.chars().count();
        if has && length + 1 + n > width {
            out.push('\n');
            out.push_str(indent);
            length = indent.chars().count();
            has = false;
        }
        if has {
            out.push(' ');
            length += 1;
        }
        out.push_str(&word);
        length += n;
        has = true;
    }
    out.push('\n');
    out
}
pub(crate) fn usage(r: &Request) -> String {
    let m = metadata(r);
    let prog = program(r);
    let mut parts = vec![];
    let mut groups = std::collections::BTreeSet::new();
    for arg in rows(m.get("arguments")).iter().filter(|a| !positional(a)) {
        if let Some(group) = arg.opt("group") {
            let group = group.int().expect("native group id");
            if !groups.insert(group) {
                continue;
            }
            let members = rows(m.get("arguments"))
                .iter()
                .filter(|a| a.get("group").is_int(group))
                .map(|a| {
                    usage_part(a)
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .to_owned()
                })
                .collect::<Vec<_>>();
            parts.push(format!("[{}]", members.join(" | ")));
        } else {
            parts.push(usage_part(arg));
        }
    }
    for arg in rows(m.get("arguments")).iter().filter(|a| positional(a)) {
        parts.push(usage_part(arg));
    }
    if !rows(m.get("children")).is_empty() {
        parts.push(format!(
            "{{{}}}",
            rows(m.get("children"))
                .iter()
                .map(|c| c.text("name"))
                .collect::<Vec<_>>()
                .join(",")
        ));
        parts.push("...".into());
    }
    let first = format!("usage: {prog} ");
    let indent = " ".repeat(first.len());
    wrapped(parts, &first, &indent, width())
}
fn paragraph(text: &str, indent: usize, width: usize) -> String {
    let prefix = " ".repeat(indent);
    text_lines(text, width.saturating_sub(indent))
        .into_iter()
        .map(|line| format!("{prefix}{line}\n"))
        .collect()
}
fn action(
    out: &mut String,
    label: &str,
    description: &str,
    indent: usize,
    column: usize,
    width: usize,
) {
    out.push_str(&" ".repeat(indent));
    out.push_str(label);
    if description.is_empty() {
        out.push('\n');
        return;
    }
    let used = indent + label.chars().count();
    if used + 2 > column {
        out.push('\n');
        out.push_str(&" ".repeat(column));
    } else {
        out.push_str(&" ".repeat(column - used));
    }
    let lines = text_lines(description, width.saturating_sub(column));
    for (index, line) in lines.into_iter().enumerate() {
        if index > 0 {
            out.push_str(&" ".repeat(column));
        }
        out.push_str(&line);
        out.push('\n');
    }
}
fn description(arg: &Value) -> String {
    let mut help = if arg.text("action") == "version" && arg.opt("help").is_none() {
        "show program's version number and exit".into()
    } else {
        arg.text("help").to_owned()
    };
    if help.contains("%(default)s") {
        let v = arg.get("default");
        let default = match v {
            Value::Null => "None".into(),
            Value::Bool(v) => if *v { "True" } else { "False" }.into(),
            Value::String(_) => text(v).into(),
            Value::Number(v) => v.clone(),
            _ => String::new(),
        };
        help = help.replace("%(default)s", &default);
    }
    help
}
pub(crate) fn render(r: &Request) -> String {
    let m = metadata(r);
    let args = rows(m.get("arguments"));
    let children = rows(m.get("children"));
    let child_label = format!(
        "{{{}}}",
        children
            .iter()
            .map(|v| v.text("name"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut max = args
        .iter()
        .map(|v| invocation(v).chars().count() + 2)
        .max()
        .unwrap_or(0);
    if !children.is_empty() {
        max = max.max(child_label.len() + 2)
    }
    let column = (max + 2).min(24).min(width().saturating_sub(20).max(4));
    let mut out = usage(r);
    out.push('\n');
    if !m.text("description").is_empty() {
        out.push_str(&paragraph(m.text("description"), 0, width()));
        out.push('\n');
    }
    let positionals = args.iter().filter(|v| positional(v)).collect::<Vec<_>>();
    if !positionals.is_empty() || !children.is_empty() {
        out.push_str("positional arguments:\n");
        for arg in positionals {
            action(
                &mut out,
                &invocation(arg),
                &description(arg),
                2,
                column,
                width(),
            )
        }
        if !children.is_empty() {
            action(&mut out, &child_label, "", 2, column, width());
            for child in children.iter().filter(|c| !c.text("help").is_empty()) {
                action(
                    &mut out,
                    child.text("name"),
                    child.text("help"),
                    4,
                    column,
                    width(),
                );
            }
        }
        out.push('\n');
    }
    out.push_str("options:\n");
    for arg in args.iter().filter(|v| !positional(v)) {
        action(
            &mut out,
            &invocation(arg),
            &description(arg),
            2,
            column,
            width(),
        )
    }
    out
}
pub(crate) fn parser_error(r: &Request, message: &str) -> crate::Error {
    let mut scope = r.clone();
    if message.starts_with("unrecognized arguments:") {
        scope.mode = Mode::Help;
        scope.action.clear();
        scope.subaction.clear();
    }
    error_value(format!(
        "{}{}: error: {message}\n",
        usage(&scope),
        program(&scope)
    ))
}
fn error_value(message: String) -> crate::Error {
    crate::error(message)
}
pub(crate) fn validate_option(r: &Request, name: &str) -> crate::Result<()> {
    let meta = metadata(r);
    let flag = format!("--{name}");
    if let Some(arg) = rows(meta.get("arguments"))
        .iter()
        .find(|v| names(v).contains(&flag.as_str()))
    {
        let raw = if name == "repo" {
            Some(r.repo.as_os_str())
        } else {
            r.os(name)
        };
        if let Some(raw) = raw {
            match arg.text("type") {
                "int" => {
                    r.integer_value(name, 0)
                        .map_err(|e| crate::error(format!("argument --{name}: {}", e.message)))?;
                }
                "float" => {
                    r.float(name, 0.0)
                        .map_err(|e| crate::error(format!("argument --{name}: {}", e.message)))?;
                }
                _ => {}
            }
            if !rows(arg.get("choices")).is_empty() {
                let choices = arg.get("choices").strings()?;
                crate::require(
                    raw.to_str().is_some_and(|s| choices.contains(&s)),
                    format!(
                        "argument --{name}: invalid choice: {} (choose from {})",
                        quote(raw),
                        choices
                            .iter()
                            .map(|v| quote(std::ffi::OsStr::new(v)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )?;
            }
        }
    }
    Ok(())
}

pub(crate) fn missing(r: &Request) -> Vec<String> {
    let meta = metadata(r);
    let mut missing = vec![];
    let mut position = 0;
    for argument in rows(meta.get("arguments")) {
        if positional(argument) {
            let name = names(argument)[0];
            let present = if name == "remainder" {
                !r.tail.is_empty()
            } else {
                let present = position < r.positionals.len();
                position += 1;
                present
            };
            if !present && !matches!(argument.text("nargs"), "?" | "*" | "REMAINDER") {
                missing.push(name.into())
            }
        } else if argument.get("required").truth() {
            let name = names(argument)[0];
            if !r.has(name.trim_start_matches('-')) {
                missing.push(name.into())
            }
        }
    }
    missing
}
pub(crate) fn quote(raw: &std::ffi::OsStr) -> String {
    let value = os(raw);
    let Value::String(bytes) = &value else {
        unreachable!()
    };
    let points = points(bytes).expect("OS string codepoints");
    let delimiter = if points.contains(&39) && !points.contains(&34) {
        34
    } else {
        39
    };
    let mut out = String::new();
    out.push(char::from_u32(delimiter).unwrap());
    static CATEGORY: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let category =
        CATEGORY.get_or_init(|| regex::Regex::new(r"[\p{C}\p{Z}]").expect("repr categories"));
    for p in points {
        match p {
            p if p == delimiter || p == 92 => {
                out.push('\\');
                out.push(char::from_u32(p).unwrap());
            }
            9 => out.push_str("\\t"),
            10 => out.push_str("\\n"),
            13 => out.push_str("\\r"),
            p if (0xd800..=0xdfff).contains(&p) => out.push_str(&format!("\\u{p:04x}")),
            p => {
                let c = char::from_u32(p).unwrap();
                let unprintable = c != ' ' && category.is_match(c.encode_utf8(&mut [0; 4]));
                if unprintable {
                    out.push_str(&if p <= 255 {
                        format!("\\x{p:02x}")
                    } else if p <= 65535 {
                        format!("\\u{p:04x}")
                    } else {
                        format!("\\U{p:08x}")
                    });
                } else {
                    out.push(c)
                }
            }
        }
    }
    out.push(char::from_u32(delimiter).unwrap());
    out
}

/// Python stderr encodes undecodable argv codepoints with backslash replacement.
pub(crate) fn display(raw: &std::ffi::OsStr) -> String {
    let Value::String(bytes) = os(raw) else {
        unreachable!()
    };
    points(&bytes)
        .expect("OS string codepoints")
        .into_iter()
        .map(|p| {
            char::from_u32(p)
                .map(|c| c.to_string())
                .unwrap_or_else(|| format!("\\u{p:04x}"))
        })
        .collect()
}

pub(crate) fn validate_positionals(r: &Request) -> crate::Result<()> {
    let meta = metadata(r);
    let positional_args = rows(meta.get("arguments"))
        .iter()
        .filter(|a| positional(a))
        .collect::<Vec<_>>();
    for (index, raw) in r.positionals.iter().enumerate() {
        let argument = positional_args.get(index).copied().or_else(|| {
            positional_args
                .last()
                .copied()
                .filter(|a| matches!(a.text("nargs"), "+" | "*"))
        });
        if let Some(argument) = argument
            && !rows(argument.get("choices")).is_empty()
        {
            let choices = argument.get("choices").strings()?;
            crate::require(
                raw.to_str().is_some_and(|s| choices.contains(&s)),
                format!(
                    "argument {}: invalid choice: {} (choose from {})",
                    names(argument)[0],
                    quote(raw),
                    choices
                        .iter()
                        .map(|v| quote(std::ffi::OsStr::new(v)))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )?;
        }
    }
    Ok(())
}

// HelpFormatter uses textwrap's whitespace, hyphen and long-word rules after
// collapsing whitespace. CLI declaration descriptions are ASCII; preserve other
// codepoints as complete characters when adapters supply additional text.
fn text_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut chunks = std::collections::VecDeque::new();
    let letter = |c: char| c.is_alphabetic() || c == '_';
    let word = |c: char| c.is_alphanumeric() || c == '_';
    for (index, part) in text.split_whitespace().enumerate() {
        if index > 0 {
            chunks.push_back(" ".to_owned());
        }
        let chars = part.chars().collect::<Vec<_>>();
        let mut begin = 0;
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '-' {
                let mut end = i;
                while end < chars.len() && chars[end] == '-' {
                    end += 1;
                }
                let punctuation =
                    i > 0 && (word(chars[i - 1]) || "!\"'&.,?".contains(chars[i - 1]));
                if end - i >= 2 && punctuation && end < chars.len() && word(chars[end]) {
                    if begin < i {
                        chunks.push_back(chars[begin..i].iter().collect());
                    }
                    chunks.push_back(chars[i..end].iter().collect());
                    begin = end;
                    i = end;
                    continue;
                }
                let before = i >= 2 && letter(chars[i - 1]) && letter(chars[i - 2])
                    || i >= 3
                        && letter(chars[i - 1])
                        && chars[i - 2] == '-'
                        && letter(chars[i - 3]);
                let after = i + 2 < chars.len()
                    && letter(chars[i + 1])
                    && (letter(chars[i + 2])
                        || chars[i + 2] == '-' && i + 3 < chars.len() && letter(chars[i + 3]));
                if before && after {
                    chunks.push_back(chars[begin..=i].iter().collect());
                    begin = i + 1;
                }
            }
            i += 1;
        }
        if begin < chars.len() {
            chunks.push_back(chars[begin..].iter().collect());
        }
    }
    let mut lines = vec![];
    while !chunks.is_empty() {
        if !lines.is_empty() && chunks.front().is_some_and(|s| s.trim().is_empty()) {
            chunks.pop_front();
        }
        let mut line = String::new();
        let mut size = 0;
        while chunks
            .front()
            .is_some_and(|s| size + s.chars().count() <= width)
        {
            let part = chunks.pop_front().unwrap();
            size += part.chars().count();
            line.push_str(&part);
        }
        if let Some(part) = chunks.pop_front_if(|s| s.chars().count() > width) {
            let keep = width - size;
            line.extend(part.chars().take(keep));
            let rest = part.chars().skip(keep).collect::<String>();
            if !rest.is_empty() {
                chunks.push_front(rest);
            }
        }
        let line = line.trim_end().to_owned();
        if !line.is_empty() {
            lines.push(line)
        }
    }
    lines
}
