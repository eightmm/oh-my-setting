use crate::{ACTIONS, Options, arg_text};
use std::ffi::OsString;

pub(crate) fn parse(args: &[OsString]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut iter = args.iter();
    let mut action = false;
    let mut limit = None;
    let mut top = None;
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
                    "--repo" => o.repo = Some(value.into()),
                    "--file" => o.index = Some(value.into()),
                    "--event-id" => o.events.push(arg_text(value)),
                    "--reason" => o.reason = arg_text(value),
                    "--by" => {
                        let text = value
                            .to_str()
                            .filter(|s| {
                                ["day", "model", "provider", "task", "role", "effort"].contains(s)
                            })
                            .ok_or("--by must be day, model, provider, task, role or effort")?;
                        o.by = Some(text.into());
                    }
                    "--top" => top = Some(arg_text(value)),
                    "--since" => o.since = (!value.is_empty()).then(|| arg_text(value)),
                    _ => o.until = (!value.is_empty()).then(|| arg_text(value)),
                }
            }
            Some("--json") => o.json = true,
            Some("--waste") => o.waste = true,
            Some("--files") => o.files = true,
            Some("--stale") => o.stale = true,
            Some("--dry-run") => o.dry = true,
            Some("--apply") => o.apply = true,
            Some("-h" | "--help") => {
                o.help = true;
                return Ok(o);
            }
            Some(name) if ACTIONS.contains(&name) && name != "import" => {
                if action || limit.is_some() {
                    return Err(format!("unknown argument: {name}"));
                }
                o.action = name.into();
                action = true;
            }
            _ => {
                if limit.is_some() {
                    return Err(format!("unknown argument: {}", arg_text(arg)));
                }
                limit = Some(arg_text(arg));
            }
        }
    }
    if [
        "latest",
        "latest-run",
        "resolve",
        "validate",
        "migrate",
        "salvage",
        "resolve-superseded",
        "resolve-recovered",
    ]
    .contains(&o.action.as_str())
        && let Some(raw) = &limit
    {
        return Err(format!("unknown argument: {raw}"));
    }
    if o.action == "resolve" {
        if o.events.is_empty() {
            return Err("resolve requires --event-id".into());
        }
    } else {
        if !o.events.is_empty() {
            return Err("--event-id is only valid with resolve".into());
        }
        if !o.reason.is_empty() {
            return Err("--reason is only valid with resolve".into());
        }
    }
    if o.json
        && ![
            "list",
            "latest",
            "latest-run",
            "failures",
            "unresolved",
            "telemetry",
        ]
        .contains(&o.action.as_str())
    {
        return Err(format!("--json is not supported by {}", o.action));
    }
    for (active, allowed, message) in [
        (
            o.files,
            o.action == "prune",
            "--files is only valid with prune",
        ),
        (
            o.stale,
            o.action == "prune",
            "--stale is only valid with prune",
        ),
        (
            o.dry,
            ["prune", "resolve-superseded", "resolve-recovered"].contains(&o.action.as_str()),
            "--dry-run is only valid with prune, resolve-superseded, or resolve-recovered",
        ),
        (
            o.apply,
            o.action == "salvage",
            "--apply is only valid with salvage",
        ),
        (
            o.by.is_some(),
            o.action == "telemetry",
            "--by is only valid with telemetry",
        ),
        (
            o.since.is_some() || o.until.is_some(),
            o.action == "telemetry",
            "--since and --until are only valid with telemetry",
        ),
        (
            o.waste,
            o.action == "telemetry",
            "--waste is only valid with telemetry",
        ),
        (
            top.as_ref().is_some_and(|s| !s.is_empty()),
            o.waste,
            "--top is only valid with --waste",
        ),
        (
            o.waste,
            o.by.is_none(),
            "--waste cannot be combined with --by",
        ),
    ] {
        if active && !allowed {
            return Err(message.into());
        }
    }
    if let Some(raw) = top.filter(|s| !s.is_empty()) {
        o.top = positive(&raw)
            .filter(|v| *v <= 100)
            .ok_or("--top must be a whole number from 1 to 100")?;
    }
    o.limit = match limit {
        Some(raw) => positive(&raw).ok_or("N must be a positive integer")?,
        None if ["telemetry", "prune"].contains(&o.action.as_str()) => 1000,
        _ => 20,
    };
    Ok(o)
}
fn positive(raw: &str) -> Option<usize> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = raw.trim_start_matches('0').parse::<u64>().ok()?;
    if value == 0 || value > i64::MAX as u64 {
        return None;
    }
    usize::try_from(value).ok()
}
