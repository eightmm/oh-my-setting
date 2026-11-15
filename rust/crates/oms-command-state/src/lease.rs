//! Advisory file intent. Leases never grant mutation or publication authority.
use crate::json::{list, number, object, string};
use crate::{
    Context, Output, Result, Value, epoch, error, json_output, parse_timestamp, random_id, require,
    storage, strings, success, timestamp,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

const STORE: &str = ".oms/leases.jsonl";
pub(crate) const HELP: &str = "Usage: oms lease add PATH_OR_GLOB... [--ttl 30m] [--reason TEXT] [--shared]\n       oms lease list [--json]\n       oms lease release ID | --mine\n       oms lease check PATH... | --staged\n\nAdvisory append-only intent in .oms/leases.jsonl; expiry is computed on read.\n--ttl is 1m to 8h (default 30m). Shared leases overlap each other.\nHolder: OMS_ATTEMPT_ID, OMS_ROOM_PARTICIPANT, or unknown-session.\ncheck exits 1 for another holder's exclusive lease, 2 on usage errors.\n";

pub fn parse_ttl(raw: &str) -> Result<i64> {
    let text = raw.trim();
    let (digits, factor) = match text.as_bytes().last() {
        Some(b's') => (&text[..text.len() - 1], 1),
        Some(b'm') => (&text[..text.len() - 1], 60),
        Some(b'h') => (&text[..text.len() - 1], 3600),
        _ => (text, 60),
    };
    require(
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        "--ttl must look like 90s, 30m or 2h",
    )?;
    let seconds = digits
        .parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .ok_or_else(|| error(2, "--ttl must be between 1m and 8h"))?;
    require(
        (60..=28800).contains(&seconds),
        "--ttl must be between 1m and 8h",
    )?;
    Ok(seconds)
}

fn holder() -> (String, &'static str) {
    for name in ["OMS_ATTEMPT_ID", "OMS_ROOM_PARTICIPANT"] {
        let value = std::env::var(name).unwrap_or_default();
        let value = value.trim();
        if !value.is_empty() && value.chars().count() <= 128 && !value.chars().any(char::is_control)
        {
            return (value.into(), name);
        }
    }
    ("unknown-session".into(), "")
}

fn resolved(path: &Path) -> Result<PathBuf> {
    if let Ok(value) = path.canonicalize() {
        return Ok(value);
    }
    let parent = path
        .parent()
        .ok_or_else(|| error(2, "cannot resolve path"))?;
    Ok(resolved(parent)?.join(path.file_name().ok_or_else(|| error(2, "invalid path"))?))
}

fn pattern(ctx: &Context<'_>, raw: &str) -> Result<String> {
    require(!raw.trim().is_empty(), "empty path")?;
    let raw = raw.replace('\\', "/");
    let parts: Vec<_> = raw.split('/').collect();
    let index = parts
        .iter()
        .position(|p| p.contains(['*', '?', '[']))
        .unwrap_or(parts.len());
    let tail = &parts[index..];
    require(
        !tail.iter().any(|p| matches!(*p, "." | "..")),
        "'.' and '..' are not allowed after a glob character",
    )?;
    let head = parts[..index].join("/");
    let head = if head.is_empty() {
        if raw.starts_with('/') { "/" } else { "." }
    } else {
        &head
    };
    let path = if Path::new(head).is_absolute() {
        PathBuf::from(head)
    } else {
        ctx.cwd.join(head)
    };
    let path = resolved(&path)?;
    let relative = path
        .strip_prefix(&ctx.repo)
        .map_err(|_| error(2, "path is outside the repository"))?;
    let mut result = relative.to_string_lossy().replace('\\', "/");
    if !tail.is_empty() {
        if !result.is_empty() {
            result.push('/');
        }
        result.push_str(&tail.join("/"));
    }
    if result.is_empty() {
        result.push('.');
    }
    require(
        !Path::new(&result)
            .components()
            .any(|p| matches!(p, Component::ParentDir | Component::RootDir)),
        "invalid repository pattern",
    )?;
    Ok(result)
}

pub fn overlap(a: &str, b: &str) -> bool {
    fn prefix(s: &str) -> &str {
        let end = s.find(['*', '?', '[']).unwrap_or(s.len());
        &s[..end]
    }
    if a == "." || b == "." {
        return true;
    }
    let (ap, bp) = (prefix(a), prefix(b));
    if a == ap && b == bp {
        a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
    } else {
        ap.starts_with(bp)
            || bp.starts_with(ap)
            || ap
                .trim_end_matches('/')
                .starts_with(&format!("{}/", bp.trim_end_matches('/')))
            || bp
                .trim_end_matches('/')
                .starts_with(&format!("{}/", ap.trim_end_matches('/')))
    }
}

pub fn project(rows: &[Value], now: i64) -> Result<Vec<Value>> {
    let mut found: Vec<Value> = Vec::new();
    let mut indices = BTreeMap::new();
    for row in rows {
        let id = row.get("id").str()?;
        require(!id.is_empty(), "leases.jsonl has an invalid id")?;
        match row.text("op") {
            "add" => {
                let expires = parse_timestamp(row.text("expires"))
                    .ok_or_else(|| error(2, "leases.jsonl has an unreadable add row"))?;
                row.get("paths").strings()?;
                row.get("holder").str()?;
                let mut item = row.clone();
                item.set("seconds_left", number((expires - now).max(0)))?;
                item.set(
                    "state",
                    string(if expires <= now { "expired" } else { "active" }),
                )?;
                if let Some(&i) = indices.get(id) {
                    found[i] = item;
                } else {
                    indices.insert(id.to_owned(), found.len());
                    found.push(item);
                }
            }
            "release" => {
                if let Some(&i) = indices.get(id) {
                    found[i].set("state", string("released"))?;
                    found[i].set("released_at", row.get("ts").clone())?;
                    found[i].set("seconds_left", number(0))?;
                }
            }
            _ => return Err(error(2, "leases.jsonl has an invalid operation")),
        }
    }
    Ok(found)
}

pub(crate) fn run(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = strings(args)?;
    if args
        .first()
        .is_some_and(|s| matches!(s.as_str(), "-h" | "--help" | "help"))
    {
        return Ok(success(HELP));
    }
    let action = args.first().ok_or_else(|| error(2, HELP))?.as_str();
    require(["add", "list", "check", "release"].contains(&action), HELP)?;
    let mut paths = Vec::new();
    let (mut ttl, mut reason) = (1800, String::new());
    let (mut shared, mut as_json, mut mine, mut staged) = (false, false, false, false);
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--ttl" | "--reason" if action == "add" => {
                let flag = &args[index];
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| error(2, format!("{flag} requires a value")))?;
                if flag == "--ttl" {
                    ttl = parse_ttl(value)?;
                } else {
                    reason = value.clone();
                }
            }
            "--shared" if action == "add" => shared = true,
            "--json" if action == "list" => as_json = true,
            "--mine" if action == "release" => mine = true,
            "--staged" if action == "check" => staged = true,
            flag if flag.starts_with('-') => {
                return Err(error(2, format!("unknown argument: {flag}")));
            }
            value => paths.push(value.to_owned()),
        }
        index += 1;
    }
    require(
        reason.chars().count() <= 200 && !reason.chars().any(char::is_control),
        "--reason must be printable text of at most 200 characters",
    )?;
    let _guard = if matches!(action, "add" | "release") {
        Some(storage::lock(&ctx.state_repo.join(STORE))?)
    } else {
        None
    };
    let (rows, bad) = storage::rows(&ctx.state_repo, Path::new(STORE))?;
    require(
        bad == 0,
        "leases.jsonl is not a lease ledger; fix or move the file aside",
    )?;
    let now = epoch();
    let found = project(&rows, now)?;
    let (me, source) = holder();
    if action == "list" {
        require(paths.is_empty(), "list takes no paths")?;
        if as_json {
            return json_output(0, &Value::Array(found));
        }
        let mut text = String::new();
        for item in found.iter().filter(|l| l.text("state") != "released") {
            text.push_str(&format!(
                "{} {:9} {:9} {} {} {}\n",
                item.text("id"),
                item.text("state"),
                if item.get("shared").truth() {
                    "shared"
                } else {
                    "exclusive"
                },
                item.get("seconds_left").int().unwrap_or(0),
                item.text("holder"),
                item.get("paths").strings()?.join(", ")
            ));
        }
        return Ok(success(text));
    }
    if action == "release" {
        require(
            (mine && paths.is_empty()) || (!mine && paths.len() == 1),
            "give one ID or --mine",
        )?;
        let targets: Vec<_> = found
            .iter()
            .filter(|l| {
                l.text("state") != "released"
                    && if mine {
                        l.text("holder") == me
                    } else {
                        l.text("id") == paths[0]
                    }
            })
            .collect();
        if !mine && targets.is_empty() {
            return Ok((
                1,
                Vec::new(),
                format!("no unreleased lease {}\n", paths[0]).into_bytes(),
            ));
        }
        if targets
            .iter()
            .any(|l| l.text("state") == "active" && l.text("holder") != me)
        {
            return Ok((
                1,
                Vec::new(),
                b"lease is held by another session\n".to_vec(),
            ));
        }
        let mut text = String::new();
        let mut batch = Vec::new();
        for item in targets {
            let row = object([
                ("schema", number(1)),
                ("op", string("release")),
                ("id", item.get("id").clone()),
                ("holder", string(&me)),
                ("ts", string(timestamp(now)?)),
            ]);
            batch.push(row);
            text.push_str(&format!("released {}\n", item.text("id")));
        }
        storage::append_many_locked(&ctx.state_repo, Path::new(STORE), &batch)?;
        if text.is_empty() {
            text = format!("no leases held by {me}\n");
        }
        return Ok(success(text));
    }
    if action == "check" {
        require(
            staged != !paths.is_empty(),
            "give PATH... or --staged, not both and not neither",
        )?;
        if staged {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&ctx.repo)
                .args(["diff", "--cached", "--name-only", "-z"])
                .output()
                .map_err(|e| error(2, e))?;
            require(output.status.success(), "git diff --cached failed")?;
            paths = output
                .stdout
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
        }
    } else {
        require(!paths.is_empty(), "add requires PATH_OR_GLOB...")?;
    }
    let patterns: BTreeSet<String> = paths
        .iter()
        .map(|p| {
            if staged {
                Ok(p.clone())
            } else {
                pattern(ctx, p)
            }
        })
        .collect::<Result<_>>()?;
    let mut clashes = Vec::new();
    for item in &found {
        if item.text("state") != "active" || item.text("holder") == me {
            continue;
        }
        if action == "add" && shared && item.get("shared").truth() {
            continue;
        }
        let others = item.get("paths").strings()?;
        for pattern in &patterns {
            if others.iter().any(|p| overlap(pattern, p)) {
                clashes.push((pattern, item));
            }
        }
    }
    if action == "check" {
        let held = clashes.iter().any(|(_, l)| !l.get("shared").truth());
        let mut text = String::new();
        for (path, item) in clashes {
            text.push_str(&format!(
                "{}: {path} is under {} by {}\n",
                if item.get("shared").truth() {
                    "shared"
                } else {
                    "held"
                },
                item.text("id"),
                item.text("holder")
            ));
        }
        if !held {
            text.push_str(&format!(
                "free: {} path(s) have no exclusive lease held by someone else\n",
                patterns.len()
            ));
        }
        return Ok((i32::from(held), text.into_bytes(), Vec::new()));
    }
    let id = format!("l-{}", &random_id("")?[..8]);
    let expires = timestamp(now + ttl)?;
    let row = object([
        ("schema", number(1)),
        ("op", string("add")),
        ("id", string(&id)),
        ("holder", string(&me)),
        ("holder_source", string(source)),
        ("paths", list(patterns.iter().map(String::as_str))),
        ("shared", Value::Bool(shared)),
        ("reason", string(reason)),
        ("ts", string(timestamp(now)?)),
        ("expires", string(&expires)),
    ]);
    storage::append_locked(&ctx.state_repo, Path::new(STORE), &row)?;
    let mut warnings = String::new();
    for (_, item) in clashes {
        warnings.push_str(&format!(
            "warning: overlaps {} by {}\n",
            item.text("id"),
            item.text("holder")
        ));
    }
    if me == "unknown-session" {
        warnings.push_str(
            "note: no OMS_ATTEMPT_ID or OMS_ROOM_PARTICIPANT; holder recorded as unknown-session\n",
        );
    }
    Ok((
        0,
        format!(
            "{id} {} until {expires} paths {}\n",
            if shared { "shared" } else { "exclusive" },
            patterns.into_iter().collect::<Vec<_>>().join(", ")
        )
        .into_bytes(),
        warnings.into_bytes(),
    ))
}
