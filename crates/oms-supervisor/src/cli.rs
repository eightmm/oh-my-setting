use crate::Result;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) struct Args {
    pub repo: PathBuf,
    pub command: String,
    values: BTreeMap<String, Vec<OsString>>,
    pub argv: Vec<OsString>,
}
impl Args {
    pub fn flag(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }
    pub fn raw(&self, name: &str) -> Option<&OsString> {
        self.values.get(name).and_then(|a| a.last())
    }
    pub fn text(&self, name: &str) -> Result<String> {
        self.raw(name)
            .map(|s| {
                s.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("invalid text for --{name}"))
            })
            .unwrap_or(Ok(String::new()))
    }
    pub fn values(&self, name: &str) -> &[OsString] {
        self.values.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn int(&self, name: &str) -> Result<i64> {
        oms_command_operations::value::parse_integer(&self.text(name)?)?
            .py()
            .parse()
            .map_err(|_| format!("argument --{name}: integer exceeds native range"))
    }
    pub fn float(&self, name: &str) -> Result<f64> {
        self.text(name)?
            .trim()
            .parse()
            .map_err(|_| format!("argument --{name}: invalid float value"))
    }
}
const COMMANDS: &[&str] = &[
    "submit",
    "dispatch",
    "status",
    "wait",
    "attach",
    "cancel",
    "resume",
    "reconcile",
    "gc",
    "serve",
    "_run-one",
];
fn options(command: &str) -> Vec<(&'static str, Option<&'static str>)> {
    let mut values = match command {
        "submit" => vec![
            ("provider", Some("local")),
            ("profile", Some("trusted-local")),
            ("completion-state", Some("review")),
            ("cwd", Some("")),
            ("task-id", Some("")),
            ("run-id", Some("")),
            ("max-wall-seconds", Some("3600")),
            ("max-log-bytes", Some("8388608")),
            ("max-tokens", Some("")),
            ("max-cost-microusd", Some("")),
        ],
        "dispatch" | "serve" => vec![
            ("max-running", Some("1")),
            ("max-jobs", Some("1")),
            ("provider-limit", Some("")),
            ("max-total-tokens", Some("")),
            ("max-total-cost-microusd", Some("")),
        ],
        "status" | "cancel" => vec![("attempt", Some("")), ("json", None)],
        "wait" => vec![
            ("attempt", Some("")),
            ("timeout", Some("3600")),
            ("interval", Some("0.2")),
            ("json", None),
        ],
        "attach" => vec![("attempt", Some("")), ("tail", Some("80"))],
        "resume" | "_run-one" => vec![("attempt", Some(""))],
        "reconcile" => vec![("apply", None), ("json", None)],
        "gc" => vec![
            ("older-than-days", Some("30")),
            ("apply", None),
            ("json", None),
        ],
        _ => vec![],
    };
    if command == "serve" {
        values.extend([
            ("wall-seconds", Some("300")),
            ("until-idle", None),
            ("interval", Some("0.5")),
        ]);
    }
    values
}
pub(crate) fn help(command: Option<&str>) -> String {
    let mut out = String::from("usage: agent-supervisor.sh [--repo REPO] ");
    if let Some(command) = command {
        out.push_str(command);
        for (name, value) in options(command) {
            out.push_str(&format!(
                " [--{name}{}]",
                if value.is_some() { " VALUE" } else { "" }
            ));
        }
        if command == "submit" {
            out.push_str(" ...");
        }
        out.push('\n');
    } else {
        out.push_str("{submit,dispatch,status,wait,attach,cancel,resume,reconcile,gc,serve,_run-one} ...\n\nBounded durable process supervisor; never lands, commits, or pushes.\n");
    }
    out.push_str("\n-h, --help  show this help message and exit\n");
    out
}
pub(crate) enum Parsed {
    Help(String),
    Args(Args),
}
pub(crate) fn parse(raw: &[OsString]) -> Result<Parsed> {
    let mut i = 0;
    let mut repo = PathBuf::from(".");
    while let Some(value) = raw.get(i) {
        if value == "--help" || value == "-h" {
            return Ok(Parsed::Help(help(None)));
        }
        let text = value.to_str().unwrap_or("");
        let (key, inline) = text
            .split_once('=')
            .map_or((text, None), |(k, v)| (k, Some(v)));
        if key.starts_with("--") && "--repo".starts_with(key) && key.len() > 2 {
            i += 1;
            repo = if let Some(inline) = inline {
                PathBuf::from(inline)
            } else {
                let p = PathBuf::from(raw.get(i).ok_or("argument --repo: expected one argument")?);
                i += 1;
                p
            };
        } else {
            break;
        }
    }
    let command = raw
        .get(i)
        .and_then(|s| s.to_str())
        .ok_or("the following arguments are required: command")?
        .to_owned();
    if !COMMANDS.contains(&command.as_str()) {
        return Err(format!("argument command: invalid choice: '{command}'"));
    }
    i += 1;
    let specs = options(&command);
    let mut values = BTreeMap::new();
    for (key, default) in &specs {
        if let Some(default) = default
            && !default.is_empty()
        {
            values.insert((*key).to_owned(), vec![OsString::from(default)]);
        }
    }
    let mut argv = vec![];
    while i < raw.len() {
        let value = &raw[i];
        if command == "submit" && (value == "--" || !value.as_encoded_bytes().starts_with(b"-")) {
            argv.extend_from_slice(&raw[i..]);
            break;
        }
        if value == "--help" || value == "-h" {
            return Ok(Parsed::Help(help(Some(&command))));
        }
        let text = value.to_str().ok_or("unrecognized arguments")?;
        let (name, inline) = text
            .split_once('=')
            .map_or((text, None), |(k, v)| (k, Some(v)));
        let matches: Vec<_> = specs
            .iter()
            .filter(|(k, _)| name.starts_with("--") && name.len() > 2 && k.starts_with(&name[2..]))
            .collect();
        let exact = matches.iter().find(|(k, _)| *k == &name[2..]);
        let selected = if let Some(exact) = exact {
            **exact
        } else if matches.len() == 1 {
            *matches[0]
        } else if matches.is_empty() {
            return Err(format!("unrecognized arguments: {text}"));
        } else {
            return Err(format!("ambiguous option: {name}"));
        };
        let (key, takes) = selected;
        i += 1;
        if takes.is_none() {
            if inline.is_some() {
                return Err(format!("argument --{key}: ignored explicit argument"));
            }
            values.insert(key.into(), vec![]);
        } else {
            let value = if let Some(inline) = inline {
                OsString::from(inline)
            } else {
                let v = raw
                    .get(i)
                    .ok_or_else(|| format!("argument --{key}: expected one argument"))?
                    .clone();
                i += 1;
                v
            };
            if key == "provider-limit" {
                values.entry(key.into()).or_default().push(value);
            } else {
                values.insert(key.into(), vec![value]);
            }
        }
    }
    let args = Args {
        repo,
        command,
        values,
        argv,
    };
    if specs.iter().any(|(k, _)| *k == "attempt") && args.text("attempt")?.is_empty() {
        return Err("the following arguments are required: --attempt".into());
    }
    for (name, _) in &specs {
        if args.raw(name).is_some()
            && (name.starts_with("max-")
                || ["tail", "older-than-days", "wall-seconds"].contains(name))
        {
            args.int(name)?;
        }
    }
    for name in ["max-running", "max-jobs", "wall-seconds", "tail"] {
        if args.raw(name).is_some() && args.int(name)? < 1 {
            return Err(format!("{name} must be positive"));
        }
    }
    if args.raw("older-than-days").is_some() && !(0..=36500).contains(&args.int("older-than-days")?)
    {
        return Err("older-than-days must be between 0 and 36500".into());
    }
    for name in ["interval", "timeout"] {
        if args.raw(name).is_some() {
            let value = args.float(name)?;
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{name} must be a finite nonnegative number"));
            }
        }
    }
    if args.command == "submit" {
        if !["trusted-local", "isolated", "remote"].contains(&args.text("profile")?.as_str()) {
            return Err("argument --profile: invalid choice".into());
        }
        if !["review", "done"].contains(&args.text("completion-state")?.as_str()) {
            return Err("argument --completion-state: invalid choice".into());
        }
    }
    Ok(Parsed::Args(args))
}
