use crate::{
    Context, Error, Output, Result, V, cli::Args, fail, json, now, obj, out, safety, storage, text,
};
use std::{ffi::OsString, path::Path};

fn replay(rows: &[V]) -> Vec<V> {
    let mut current: Vec<V> = Vec::new();
    for event in rows {
        let ident = text(event, "id");
        if ident.is_empty() {
            continue;
        }
        let position = current
            .iter()
            .position(|r| text(r, "id") == ident)
            .unwrap_or_else(|| {
                current.push(obj([("id", ident.into())]));
                current.len() - 1
            });
        if let V::Obj(fields) = event {
            for (key, value) in fields {
                if !matches!(value, V::Null) && value.text() != Some("") {
                    if key == b"owner"
                        && current[position].has("owner")
                        && text(event, "status") != "claimed"
                    {
                        continue;
                    }
                    if let V::Obj(row) = &mut current[position] {
                        row.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }
    current
}
fn stale(row: &V) -> bool {
    let ttl = std::env::var("OMS_EXPERIMENT_CLAIM_TTL")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(86400);
    text(row, "status") == "claimed"
        && chrono::DateTime::parse_from_rfc3339(text(row, "ts"))
            .ok()
            .is_some_and(|stamp| crate::seconds() as i64 - stamp.timestamp() >= ttl)
}
pub(crate) fn run(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &[
            "id",
            "hypothesis",
            "baseline",
            "result",
            "next",
            "job",
            "reason",
            "verdict",
            "tags",
            "owner",
        ],
        &["force", "all", "stale", "json"],
    )?;
    if args.has("help") {
        return Ok(out(include_bytes!("help/experiment-board.txt").to_vec()));
    }
    args.exact_operands(1)?;
    if args.has("json") && args.action() != "list" {
        return fail("--json is only supported by list");
    }
    if args.has("verdict")
        && (args.action() != "finish"
            || !["supported", "not_supported", "inconclusive", "aborted"]
                .contains(&args.get("verdict")))
    {
        return fail(
            "--verdict belongs to finish and must be supported, not_supported, inconclusive or aborted",
        );
    }
    let mut tags: Vec<String> = Vec::new();
    if args.has("tags") {
        if !["claim", "finish"].contains(&args.action()) {
            return fail("--tags belongs to claim or finish");
        }
        for tag in args
            .get("tags")
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
        {
            if tag.len() > 40
                || !tag
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
            {
                return fail("invalid tag");
            }
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        if tags.is_empty() || tags.len() > 8 {
            return fail("--tags takes 1..8 labels");
        }
    }
    let repo = storage::repository(context, context.cwd)?;
    let path = Path::new(".oms/experiments.jsonl");
    let rows = storage::load(&repo, path)?;
    let current = replay(&rows);
    if args.action() == "list" {
        let mut selected = Vec::new();
        let mut output = String::new();
        for mut row in current {
            let old = stale(&row);
            let status = text(&row, "status").to_owned();
            if !args.has("all") && ["done", "aborted"].contains(&status.as_str())
                || args.has("stale") && !old
                || args.has("owner") && text(&row, "owner") != args.get("owner")
            {
                continue;
            }
            row.set("stale", old);
            output.push_str(&format!(
                "{:40} {:8} owner={}{}{}  {}\n",
                text(&row, "id"),
                status,
                text(&row, "owner"),
                if row.has("job") {
                    format!(" job={}", text(&row, "job"))
                } else {
                    String::new()
                },
                if old { " STALE" } else { "" },
                text(&row, "hypothesis")
                    .chars()
                    .take(60)
                    .collect::<String>()
            ));
            selected.push(row);
        }
        return Ok(out(if args.has("json") {
            json(&obj([
                ("schema", 1usize.into()),
                ("experiments", V::Arr(selected)),
            ]))?
        } else if output.is_empty() {
            b"no experiments\n".to_vec()
        } else {
            output.into_bytes()
        }));
    }
    let ident = if args.action() == "claim" && !args.has("id") {
        slug(args.required("hypothesis")?)
    } else {
        args.required("id")?.to_owned()
    };
    if !storage::identifier(&ident) {
        return fail("invalid experiment id");
    }
    if args.action() == "show" {
        let history: Vec<_> = rows
            .into_iter()
            .filter(|r| text(r, "id") == ident)
            .collect();
        if history.is_empty() {
            return fail("no such experiment");
        }
        return Ok(out(json(&V::Arr(history))?));
    }
    safety::parent()?;
    let owner = args.value("owner", "");
    let owner = if owner.is_empty() {
        std::env::var("OMS_AGENT").unwrap_or_else(|_| "agent".into())
    } else {
        owner.into()
    };
    let mut event = obj([
        ("schema", 1usize.into()),
        ("id", ident.clone().into()),
        ("ts", now().into()),
        ("owner", owner.clone().into()),
    ]);
    for key in [
        "hypothesis",
        "baseline",
        "result",
        "next",
        "job",
        "reason",
        "verdict",
    ] {
        if args.has(key) {
            event.set(key, args.get(key));
        }
    }
    if !tags.is_empty() {
        event.set(
            "tags",
            V::Arr(tags.iter().map(|s| s.as_str().into()).collect()),
        );
    }
    if args.action() == "claim" {
        args.required("hypothesis")?;
    }
    let mut notes = String::new();
    if args.action() == "claim" {
        for row in current.iter().filter(|r| {
            ["done", "aborted"].contains(&text(r, "status"))
                && (text(r, "id") == ident
                    || r.get("tags")
                        .array()
                        .iter()
                        .any(|v| tags.iter().any(|s| v.text() == Some(s))))
        }) {
            notes.push_str(&format!(
                "prior outcome: {} {} | {}\n",
                text(row, "id"),
                text(row, "verdict"),
                text(row, "result")
            ));
        }
    }
    let status = storage::log(&repo, path, context.interrupted, |old, _| {
        let current = replay(&storage::rows(old)?);
        let previous = current.iter().find(|r| text(r, "id") == ident);
        let status = match args.action() {
            "claim" => {
                if previous.is_some_and(|r| {
                    ["claimed", "running"].contains(&text(r, "status")) && !stale(r)
                }) && !args.has("force")
                {
                    return Err(Error::new(
                        4,
                        format!(
                            "experiment '{ident}' already claimed/running; use --force or another id"
                        ),
                    ));
                }
                "claimed"
            }
            "start" => "running",
            "finish" => "done",
            "abort" => "aborted",
            "touch" => {
                let row = previous.ok_or_else(|| Error::new(2, "no such experiment"))?;
                match text(row, "status") {
                    "claimed" => "claimed",
                    "running" => "running",
                    _ => return fail("touch requires a claimed/running experiment"),
                }
            }
            _ => return fail("unknown experiment-board subcommand"),
        };
        if args.action() != "claim" && previous.is_none() {
            return fail("no such experiment");
        }
        event.set("status", status);
        let mut bytes = event.json(false)?;
        bytes.push(b'\n');
        Ok((bytes, status.to_owned()))
    })?;
    let output = if args.action() == "claim" {
        format!("{ident}\n").into_bytes()
    } else {
        Vec::new()
    };
    notes.push_str(&format!("{status}: {ident} (owner {owner})\n"));
    Ok((0, output, notes.into_bytes()))
}
pub(crate) fn slug(raw: &str) -> String {
    let mut value = String::new();
    let mut dash = false;
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !value.is_empty() {
                value.push('-');
            }
            value.push(c);
            dash = false;
        } else {
            dash = true;
        }
        if value.len() >= 60 {
            break;
        }
    }
    value
}
