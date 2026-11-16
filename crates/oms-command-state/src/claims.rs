//! Claims and figures compare recorded provenance; they never execute user code.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, capsule, epoch, error, helpers, json, json_output,
    require, storage, success, timestamp,
};
use std::ffi::OsString;
use std::path::Path;

pub(crate) const HELP: &str = "Usage: oms run claims add --metric KEY --value N --run ID [--tolerance N] [--where TEXT] [--id ID]\n       oms run claims list\n       oms run claims check [ID] [--json] [--strict] [--no-verify]\n       oms run figure record --output FILE --script FILE [--run ID]... [--command TEXT] [--id ID]\n       oms run figure verify [ID] [--json]\nAppend-only local records; numeric matches are not reproduction evidence.\n";

fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Number(v) => v.parse::<f64>().ok().filter(|v| v.is_finite()),
        _ => None,
    }
}

fn number_arg(raw: &str, field: &str) -> Result<f64> {
    raw.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .ok_or_else(|| error(2, format!("--{field} must be a finite number")))
}

fn metric(capsule: &Value, name: &str) -> Option<f64> {
    numeric(capsule.get("result").get("metrics").get(name))
}

fn latest(rows: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for row in rows {
        if row.text("id").is_empty() {
            continue;
        }
        if let Some(i) = out.iter().position(|r| r.text("id") == row.text("id")) {
            out[i] = row;
        } else {
            out.push(row);
        }
    }
    out
}

fn relative(ctx: &Context<'_>, raw: &str) -> Result<std::path::PathBuf> {
    let path = helpers::absolute(&ctx.cwd, raw);
    require(
        path.starts_with(&ctx.repo),
        "file must be inside the repository",
    )?;
    Ok(path)
}
fn digest(ctx: &Context<'_>, raw: &str) -> Result<String> {
    storage::hash_input(&relative(ctx, raw)?, 512 * 1024 * 1024)
}

pub(crate) fn run(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    if args.iter().any(|s| s == "--help" || s == "-h") {
        return Ok(success(HELP));
    }
    require(args.len() >= 2, HELP)?;
    let group = args[0].to_str().ok_or_else(|| error(2, HELP))?;
    let action = args[1].to_str().ok_or_else(|| error(2, HELP))?;
    require(["claims", "figure"].contains(&group), HELP)?;
    let opts = Options::parse(
        &args[2..],
        &[
            "metric",
            "value",
            "run",
            "tolerance",
            "where",
            "id",
            "output",
            "script",
            "command",
        ],
        &["json", "strict", "no-verify"],
    )?;
    let file = if group == "claims" {
        ".oms/claims.jsonl"
    } else {
        ".oms/figures.jsonl"
    };
    let path = Path::new(file);
    let write = matches!((group, action), ("claims", "add") | ("figure", "record"));
    let _guard = if write {
        Some(storage::lock(&ctx.state_repo.join(path))?)
    } else {
        None
    };
    let (rows, bad) = storage::rows(&ctx.state_repo, path)?;
    let mut rows = latest(rows);
    if write {
        require(opts.pos.is_empty(), "record takes options only")?;
        let id = if opts.get("id").is_empty() {
            crate::random_id(if group == "claims" { "claim-" } else { "fig-" })?
        } else {
            opts.get("id").into()
        };
        helpers::identifier(&id)?;
        require(id.len() <= 64, "id exceeds 64 characters")?;
        let mut row = object([
            ("schema", number(1)),
            ("id", string(&id)),
            ("ts", string(timestamp(epoch())?)),
            ("agent", string(helpers::agent())),
        ]);
        if group == "claims" {
            let value = number_arg(opts.get("value"), "value")?;
            let tolerance = if opts.get("tolerance").is_empty() {
                0.0
            } else {
                number_arg(opts.get("tolerance"), "tolerance")?
            };
            require(tolerance >= 0.0, "tolerance must not be negative")?;
            require(
                !opts.get("metric").is_empty() && !opts.get("run").is_empty(),
                "add requires --metric, --value and --run",
            )?;
            let (id, capsule, digest) = capsule::resolve(ctx, opts.get("run"))?;
            require(
                metric(&capsule, opts.get("metric")).is_some(),
                "capsule recorded no numeric metric",
            )?;
            for (key, value) in [
                ("where", string(opts.get("where"))),
                ("metric", string(opts.get("metric"))),
                ("value", number(value)),
                ("tolerance", number(tolerance)),
                ("run", string(opts.get("run"))),
                ("capsule", string(id)),
                ("capsule_sha256", string(digest)),
            ] {
                row.set(key, value)?;
            }
        } else {
            require(
                !opts.get("output").is_empty() && !opts.get("script").is_empty(),
                "record requires --output and --script",
            )?;
            for field in ["output", "script"] {
                row.set(
                    field,
                    string(
                        relative(ctx, opts.get(field))?
                            .strip_prefix(&ctx.repo)
                            .map_err(|_| error(2, "file escaped repository"))?
                            .to_string_lossy(),
                    ),
                )?;
                row.set(
                    &format!("{field}_sha256"),
                    string(digest(ctx, opts.get(field))?),
                )?;
            }
            require(opts.all("run").len() <= 32, "at most 32 input runs")?;
            let mut capsules = Vec::new();
            let mut digests = json::empty();
            for run in opts.all("run") {
                let (id, _, digest) = capsule::resolve(ctx, run)?;
                if let Some(previous) = digests.opt(&id) {
                    require(previous.str()? == digest, "capsule changed during capture")?;
                } else {
                    capsules.push(string(&id));
                    digests.set(&id, string(digest))?;
                }
            }
            row.set("capsules", Value::Array(capsules))?;
            row.set("capsule_sha256", digests)?;
            row.set("command", string(opts.get("command")))?;
        }
        helpers::scrub(&row)?;
        storage::append_locked(&ctx.state_repo, path, &row)?;
        let message = if group == "claims" {
            let cap = capsule::load(ctx, row.text("capsule"))?.0;
            let recorded = metric(&cap, row.text("metric")).unwrap_or(0.0);
            let value = numeric(row.get("value")).unwrap_or(0.0);
            let tolerance = numeric(row.get("tolerance")).unwrap_or(0.0);
            format!(
                "claim {id}: {}={} {} the recorded {recorded} (tolerance {tolerance}) in capsule {}\n",
                row.text("metric"),
                value,
                if (value - recorded).abs() <= tolerance {
                    "matches"
                } else {
                    "DOES NOT MATCH"
                },
                row.text("capsule")
            )
        } else {
            format!(
                "figure {id}: recorded {} from {} ({} input run(s)); nothing was run\n",
                row.text("output"),
                row.text("script"),
                row.get("capsules").array()?.len()
            )
        };
        return Ok((0, vec![], message.into_bytes()));
    }
    require(opts.pos.len() <= 1, "check or verify takes at most one id")?;
    if let Some(id) = opts.pos.first() {
        rows.retain(|r| r.text("id") == id);
        require(!rows.is_empty(), "no such record")?;
    }
    if group == "claims" && action == "list" {
        require(opts.pos.is_empty(), "list takes no id")?;
        if opts.flag("json") {
            return json_output(i32::from(bad > 0), &Value::Array(rows));
        }
        let mut out = String::new();
        for row in rows {
            out.push_str(&format!(
                "{} {}={} capsule={} {}\n",
                row.text("id"),
                row.text("metric"),
                helpers::display(row.get("value")),
                row.text("capsule"),
                row.text("where")
            ));
        }
        return Ok((i32::from(bad > 0), out.into_bytes(), vec![]));
    }
    require(
        matches!((group, action), ("claims", "check") | ("figure", "verify")),
        HELP,
    )?;
    let mut results = Vec::new();
    let mut counts = json::empty();
    let mut stale_count = 0;
    let mut unverified = 0;
    let mut broken = bad > 0;
    for row in rows {
        if group == "figure" {
            let mut reasons = Vec::new();
            for field in ["output", "script"] {
                if digest(ctx, row.text(field)).ok().as_deref()
                    != Some(row.text(&format!("{field}_sha256")))
                {
                    reasons.push(string(format!("{field} changed or missing")));
                }
            }
            if row.get("capsules").array().is_err()
                || row.get("capsules").array().is_ok_and(|v| {
                    v.len() > 32
                        || v.iter().any(|v| {
                            v.str().is_err() || helpers::identifier(v.str().unwrap_or("")).is_err()
                        })
                })
            {
                reasons.push(string("input capsule list is invalid"));
            }
            for id in row.get("capsules").array().unwrap_or(&[]) {
                let id = id.str().unwrap_or("");
                let expected = row.get("capsule_sha256").text(id);
                if !json::hex(expected, 64)
                    || capsule::load(ctx, id).ok().map(|(_, d)| d).as_deref() != Some(expected)
                {
                    reasons.push(string(format!("capsule {id} changed, missing or unpinned")));
                }
            }
            let stale = !reasons.is_empty();
            broken |= stale;
            stale_count += usize::from(stale);
            results.push(object([
                ("id", row.get("id").clone()),
                ("output", row.get("output").clone()),
                ("script", row.get("script").clone()),
                ("status", string(if stale { "stale" } else { "current" })),
                ("command", row.get("command").clone()),
                ("reasons", Value::Array(reasons)),
                ("capsules", row.get("capsules").clone()),
            ]));
        } else {
            let cap = capsule::load(ctx, row.text("capsule")).ok();
            let recorded = cap
                .as_ref()
                .and_then(|(c, _)| metric(c, row.text("metric")));
            let claimed = numeric(row.get("value"));
            let tolerance = numeric(row.get("tolerance"))
                .filter(|v| *v >= 0.0)
                .unwrap_or(0.0);
            let content = cap.as_ref().and_then(|(_, d)| {
                row.opt("capsule_sha256")
                    .map(|v| json::hex(v.str().unwrap_or(""), 64) && v.str().unwrap_or("") == d)
            });
            let tree = if opts.flag("no-verify") {
                None
            } else {
                cap.as_ref()
                    .and_then(|(c, _)| capsule::verifies(ctx, c).ok())
            };
            let status = match (recorded, claimed) {
                (Some(a), Some(b)) if (a - b).abs() <= tolerance => "match",
                (Some(_), Some(_)) => "mismatch",
                _ => "missing",
            };
            let stale = content == Some(false) || tree == Some(false);
            stale_count += usize::from(stale);
            unverified += usize::from(content.is_none());
            broken |= status != "match" || opts.flag("strict") && (stale || content.is_none());
            counts.set(status, number(counts.get(status).int().unwrap_or(0) + 1))?;
            let mut notes = Vec::new();
            if content.is_none() {
                notes.push(string("unverified: capsule bytes were not pinned"));
            }
            if stale {
                notes.push(string("stale: capsule or tree changed"));
            }
            if let Some((c, _)) = &cap {
                if c.get("seeds").array().unwrap_or(&[]).len() <= 1 {
                    notes.push(string("single seed"));
                }
                if c.get("git").get("dirty").truth() {
                    notes.push(string("run had uncommitted changes"));
                }
                if c.get("result").get("exit").int().is_ok_and(|n| n != 0) {
                    notes.push(string("run did not exit successfully"));
                }
            }
            let mut result = object([
                ("id", row.get("id").clone()),
                ("where", row.get("where").clone()),
                ("metric", row.get("metric").clone()),
                ("claimed", row.get("value").clone()),
                ("tolerance", number(tolerance)),
                ("capsule", row.get("capsule").clone()),
                ("recorded", recorded.map(number).unwrap_or(Value::Null)),
                ("status", string(status)),
                ("notes", Value::Array(notes)),
                (
                    "capsule_content_verifies",
                    content.map(Value::Bool).unwrap_or(Value::Null),
                ),
            ]);
            if !opts.flag("no-verify") {
                result.set(
                    "capsule_verifies",
                    tree.map(Value::Bool).unwrap_or(Value::Null),
                )?;
            }
            results.push(result);
        }
    }
    let report = if group == "claims" {
        object([
            ("schema", number(1)),
            ("kind", string("oms-run-claims-check")),
            ("claims", Value::Array(results.clone())),
            ("counts", counts),
            ("stale", number(stale_count)),
            ("unverified", number(unverified)),
            ("unreadable_rows", number(bad)),
            ("verified_capsules", Value::Bool(!opts.flag("no-verify"))),
        ])
    } else {
        object([
            ("schema", number(1)),
            ("kind", string("oms-run-figure-verify")),
            ("figures", Value::Array(results.clone())),
            ("stale", number(stale_count)),
            ("unreadable_rows", number(bad)),
        ])
    };
    if opts.flag("json") {
        return json_output(i32::from(broken), &report);
    }
    let mut out = String::new();
    for row in results {
        if group == "claims" {
            out.push_str(&format!(
                "{:<8} {}  {}: claimed {}, recorded {} (tolerance {})  capsule {}{}\n",
                row.text("status"),
                row.text("id"),
                row.text("metric"),
                helpers::display(row.get("claimed")),
                helpers::display(row.get("recorded")),
                helpers::display(row.get("tolerance")),
                row.text("capsule"),
                if row.text("where").is_empty() {
                    String::new()
                } else {
                    format!("  @ {}", row.text("where"))
                }
            ));
            for note in row.get("notes").strings()? {
                out.push_str(&format!("         note: {note}\n"));
            }
        } else {
            out.push_str(&format!(
                "{:<8} {}  {}{}\n",
                row.text("status"),
                row.text("id"),
                row.text("output"),
                if row.text("status") == "stale" {
                    format!("  {}", row.text("command"))
                } else {
                    String::new()
                }
            ));
        }
    }
    out.push_str("Recorded values only; nothing was rerun and a match is not a reproduction.\n");
    Ok((i32::from(broken), out.into_bytes(), vec![]))
}
