#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use crate::adapters::ObserveOverrides;
use crate::sanitize::{diagnostic_os_str, os_str_to_utf8};

#[derive(Clone, Debug)]
pub enum JournalCommand {
    Configure {
        data_source_id: Option<String>,
        discover: bool,
        exclude_repos: Vec<PathBuf>,
        include_repos: Vec<PathBuf>,
        lang: Option<String>,
        validate: bool,
    },
    Disconnect {
        managed: bool,
    },
    Identity {
        repo: Option<PathBuf>,
        json: bool,
        adopt_detected: bool,
    },
    Status {
        repo: Option<PathBuf>,
        json: bool,
    },
    Observe(ObserveOverridesWithRepo),
    Tick {
        repo: Option<PathBuf>,
        local_only: bool,
        digest: bool,
        autodistill: bool,
    },
    Materialize {
        repo: Option<PathBuf>,
    },
    Rebuild {
        repo: Option<PathBuf>,
    },
    Seal {
        repo: Option<PathBuf>,
        apply: bool,
    },
    Show {
        repo: Option<PathBuf>,
        mode: ShowMode,
        json: bool,
    },
    Resolve {
        repo: Option<PathBuf>,
        event_id: String,
        how: String,
    },
    Sync {
        repo: Option<PathBuf>,
        force: bool,
        today: bool,
        recent_days: Option<i64>,
        budget: f64,
        max_per_tick: Option<usize>,
    },
    Distill {
        repo: Option<PathBuf>,
        dry_run: bool,
    },
    Help,
}

#[derive(Clone, Debug)]
pub struct ObserveOverridesWithRepo {
    pub repo: Option<PathBuf>,
    pub inner: ObserveOverrides,
}

#[derive(Clone, Debug)]
pub enum ShowMode {
    Today,
    Week,
    Period(String),
    Blockers,
    Decisions {
        since: Option<String>,
    },
    Recent(usize),
    Grep {
        query: String,
        kinds: Vec<String>,
        limit: usize,
    },
}

fn strip_repo_eq_prefix(arg: &OsStr) -> Option<OsString> {
    #[cfg(unix)]
    {
        arg.as_bytes()
            .strip_prefix(b"--repo=")
            .map(|rest| OsStr::from_bytes(rest).to_os_string())
    }
    #[cfg(not(unix))]
    {
        arg.to_str()
            .and_then(|s| s.strip_prefix("--repo="))
            .map(OsString::from)
    }
}

/// Normalize leading `--repo PATH <cmd> ...` or `--repo=PATH <cmd> ...` into `<cmd> --repo PATH ...`,
/// matching `scripts/journal.sh:7-24`.
pub fn normalize_leading_repo(raw_args: &[OsString]) -> Result<Vec<OsString>, String> {
    if raw_args.len() >= 2 && raw_args[0] == OsStr::new("--repo") {
        let repo_val = raw_args[1].clone();
        if raw_args.len() >= 3 {
            let mut out = vec![raw_args[2].clone(), OsString::from("--repo"), repo_val];
            out.extend_from_slice(&raw_args[3..]);
            return Ok(out);
        }
        return Err("error: --repo requires a path and subcommand".to_string());
    }
    if let Some(first) = raw_args.first() {
        if let Some(repo_val) = strip_repo_eq_prefix(first) {
            if raw_args.len() >= 2 {
                let mut out = vec![raw_args[1].clone(), OsString::from("--repo"), repo_val];
                out.extend_from_slice(&raw_args[2..]);
                return Ok(out);
            }
            return Err("error: --repo= requires a subcommand".to_string());
        }
    }
    Ok(raw_args.to_vec())
}

pub fn help_text() -> &'static str {
    concat!(
        "usage: oms journal <subcommand> [options]\n\n",
        "subcommands:\n",
        "  configure    Configure optional Notion export, language (ko|en), or repo exclusions\n",
        "  disconnect   Remove persisted Notion configuration\n",
        "  identity     Show or adopt detected repository journal identity\n",
        "  status       Show journal storage, index, and sync status\n",
        "  observe      Record an event from a source file or CLI fields\n",
        "  tick         Capture HEAD commit, materialize dirty periods, emit prompt digest/distill\n",
        "  materialize  Materialize dirty daily and weekly markdown summaries\n",
        "  rebuild      Rebuild SQLite index and all markdown summaries from canonical events\n",
        "  seal         Partition closed-month events into cold storage (default dry-run, --apply to write)\n",
        "  show         Display summaries, blockers, decisions, recent events, or --grep search\n",
        "  resolve      Record a correction event superseding an active event\n",
        "  sync         Explicitly export closed/recent summaries to Notion\n",
        "  distill      Promote distilled decisions/blockers into project shared memory\n"
    )
}

pub fn parse_command(raw_args: &[OsString]) -> Result<JournalCommand, String> {
    if raw_args.is_empty() {
        return Err(help_text().to_string());
    }
    let normalized = normalize_leading_repo(raw_args)?;
    let Some((sub_os, rest)) = normalized.split_first() else {
        return Err(help_text().to_string());
    };
    let sub = sub_os.to_str().ok_or_else(|| {
        format!(
            "error: unknown journal subcommand '{}'\n{}",
            diagnostic_os_str(sub_os),
            help_text()
        )
    })?;
    if matches!(sub, "-h" | "--help" | "help") {
        return Ok(JournalCommand::Help);
    }

    match sub {
        "configure" => parse_configure(rest),
        "disconnect" => parse_disconnect(rest),
        "identity" => parse_identity(rest),
        "status" => parse_status(rest),
        "observe" => parse_observe(rest),
        "tick" => parse_tick(rest),
        "materialize" => {
            parse_repo_only(rest, "materialize").map(|repo| JournalCommand::Materialize { repo })
        }
        "rebuild" => parse_repo_only(rest, "rebuild").map(|repo| JournalCommand::Rebuild { repo }),
        "seal" => parse_seal(rest),
        "show" => parse_show(rest),
        "resolve" => parse_resolve(rest),
        "sync" => parse_sync(rest),
        "distill" => parse_distill(rest),
        other => Err(format!(
            "error: unknown journal subcommand '{other}'\n{}",
            help_text()
        )),
    }
}

fn next_value_os<'a>(args: &'a [OsString], i: &mut usize, flag: &str) -> Result<&'a OsStr, String> {
    if *i + 1 >= args.len() {
        return Err(format!("error: {flag} requires a value"));
    }
    *i += 1;
    Ok(args[*i].as_os_str())
}

fn split_flag<'a>(arg: &'a OsStr, cmd_name: &str) -> Result<(&'a str, Option<&'a OsStr>), String> {
    #[cfg(unix)]
    {
        let bytes = arg.as_bytes();
        if let Some(pos) = bytes.iter().position(|&b| b == b'=') {
            let flag = std::str::from_utf8(&bytes[..pos]).map_err(|_| {
                format!(
                    "error: unrecognized argument '{}' for {cmd_name}",
                    diagnostic_os_str(arg)
                )
            })?;
            let val = OsStr::from_bytes(&bytes[pos + 1..]);
            return Ok((flag, Some(val)));
        }
        let flag = std::str::from_utf8(bytes).map_err(|_| {
            format!(
                "error: unrecognized argument '{}' for {cmd_name}",
                diagnostic_os_str(arg)
            )
        })?;
        Ok((flag, None))
    }
    #[cfg(not(unix))]
    {
        let s = arg.to_str().ok_or_else(|| {
            format!(
                "error: unrecognized argument '{}' for {cmd_name}",
                diagnostic_os_str(arg)
            )
        })?;
        match s.split_once('=') {
            Some((k, v)) => Ok((k, Some(OsStr::new(v)))),
            None => Ok((s, None)),
        }
    }
}

fn take_val_os<'a>(
    args: &'a [OsString],
    i: &mut usize,
    flag: &str,
    inline: Option<&'a OsStr>,
) -> Result<PathBuf, String> {
    let raw = match inline {
        Some(v) => v,
        None => next_value_os(args, i, flag)?,
    };
    Ok(PathBuf::from(raw))
}

fn take_val_str<'a>(
    args: &'a [OsString],
    i: &mut usize,
    flag: &str,
    inline: Option<&'a OsStr>,
) -> Result<&'a str, String> {
    let raw = match inline {
        Some(v) => v,
        None => next_value_os(args, i, flag)?,
    };
    os_str_to_utf8(raw, flag)
}

fn arg_starts_with_dash(arg: &OsStr) -> bool {
    #[cfg(unix)]
    {
        arg.as_bytes().starts_with(b"-")
    }
    #[cfg(not(unix))]
    {
        arg.to_str().is_some_and(|s| s.starts_with('-'))
    }
}

fn parse_configure(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut data_source_id = None;
    let mut discover = false;
    let mut exclude_repos = Vec::new();
    let mut include_repos = Vec::new();
    let mut lang = None;
    let mut validate = true;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "configure")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--data-source-id" => {
                data_source_id = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--discover" => discover = true,
            "--exclude-repo" => exclude_repos.push(take_val_os(args, &mut i, flag, inline)?),
            "--include-repo" => include_repos.push(take_val_os(args, &mut i, flag, inline)?),
            "--lang" => {
                let v = take_val_str(args, &mut i, flag, inline)?;
                if v != "ko" && v != "en" {
                    return Err(format!("error: --lang must be 'ko' or 'en', got '{v}'"));
                }
                lang = Some(v.to_string());
            }
            "--no-validate" => validate = false,
            other => {
                return Err(format!(
                    "error: unrecognized argument '{other}' for configure"
                ));
            }
        }
        i += 1;
    }
    Ok(JournalCommand::Configure {
        data_source_id,
        discover,
        exclude_repos,
        include_repos,
        lang,
        validate,
    })
}

fn parse_disconnect(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut managed = false;
    for arg in args {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(JournalCommand::Help),
            Some("--managed") => managed = true,
            Some(other) => {
                return Err(format!(
                    "error: unrecognized argument '{other}' for disconnect"
                ));
            }
            None => {
                return Err(format!(
                    "error: unrecognized argument '{}' for disconnect",
                    diagnostic_os_str(arg)
                ));
            }
        }
    }
    Ok(JournalCommand::Disconnect { managed })
}

fn parse_identity(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut json = false;
    let mut adopt_detected = false;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "identity")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--json" => json = true,
            "--adopt-detected" => adopt_detected = true,
            other => {
                return Err(format!(
                    "error: unrecognized argument '{other}' for identity"
                ));
            }
        }
        i += 1;
    }
    Ok(JournalCommand::Identity {
        repo,
        json,
        adopt_detected,
    })
}

fn parse_status(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut json = false;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "status")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--json" => json = true,
            other => return Err(format!("error: unrecognized argument '{other}' for status")),
        }
        i += 1;
    }
    Ok(JournalCommand::Status { repo, json })
}

fn parse_repo_only(args: &[OsString], cmd_name: &str) -> Result<Option<PathBuf>, String> {
    let mut repo = None;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], cmd_name)?;
        match flag {
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            other => {
                return Err(format!(
                    "error: unrecognized argument '{other}' for {cmd_name}"
                ));
            }
        }
        i += 1;
    }
    Ok(repo)
}

fn parse_observe(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut inner = ObserveOverrides::default();
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "observe")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--source-type" => {
                inner.source_type = take_val_str(args, &mut i, flag, inline)?.to_string()
            }
            "--source-file" => inner.source_file = Some(take_val_os(args, &mut i, flag, inline)?),
            "--record-path" => inner.record_path = Some(take_val_os(args, &mut i, flag, inline)?),
            "--event-type" => {
                inner.event_type = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--source-id" => {
                inner.source_id = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--operation-id" => {
                inner.operation_id = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--occurred-at" => {
                inner.occurred_at = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--verification-status" => {
                inner.verification_status =
                    Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--outcome" => {
                inner.outcome = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--outcome-status" => {
                inner.outcome_status = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--decision" => {
                inner.decision = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--blocker" => {
                inner.blocker = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--next-action" => {
                inner.next_action = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            "--operation" => {
                inner.operation = Some(take_val_str(args, &mut i, flag, inline)?.to_string())
            }
            other => {
                return Err(format!(
                    "error: unrecognized argument '{other}' for observe"
                ));
            }
        }
        i += 1;
    }
    if inner.source_type.trim().is_empty() {
        return Err("error: observe requires --source-type".to_string());
    }
    Ok(JournalCommand::Observe(ObserveOverridesWithRepo {
        repo,
        inner,
    }))
}

fn parse_tick(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut local_only = false;
    let mut digest = false;
    let mut autodistill = false;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "tick")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--local-only" => local_only = true,
            "--digest" => digest = true,
            "--autodistill" => autodistill = true,
            other => return Err(format!("error: unrecognized argument '{other}' for tick")),
        }
        i += 1;
    }
    Ok(JournalCommand::Tick {
        repo,
        local_only,
        digest,
        autodistill,
    })
}

fn parse_seal(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut apply = false;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "seal")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--apply" => apply = true,
            other => return Err(format!("error: unrecognized argument '{other}' for seal")),
        }
        i += 1;
    }
    Ok(JournalCommand::Seal { repo, apply })
}

fn parse_show(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut json = false;
    let mut today = false;
    let mut week = false;
    let mut period: Option<String> = None;
    let mut blockers = false;
    let mut decisions = false;
    let mut since: Option<String> = None;
    let mut recent: Option<usize> = None;
    let mut grep: Option<String> = None;
    let mut kinds: Vec<String> = Vec::new();
    let mut limit: usize = crate::search::DEFAULT_LIMIT;

    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "show")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--json" => json = true,
            "--today" => today = true,
            "--week" => week = true,
            "--period" => period = Some(take_val_str(args, &mut i, flag, inline)?.to_string()),
            "--blockers" => blockers = true,
            "--decisions" => decisions = true,
            "--since" => since = Some(take_val_str(args, &mut i, flag, inline)?.to_string()),
            "--recent" => {
                let raw = take_val_str(args, &mut i, flag, inline)?;
                let n: usize = raw.parse().map_err(|_| {
                    format!("error: --recent expects positive integer, got '{raw}'")
                })?;
                if n == 0 {
                    return Err("error: --recent must be >= 1".to_string());
                }
                recent = Some(n);
            }
            "--grep" => grep = Some(take_val_str(args, &mut i, flag, inline)?.to_string()),
            "--kind" => kinds.push(take_val_str(args, &mut i, flag, inline)?.to_string()),
            "--limit" => {
                let raw = take_val_str(args, &mut i, flag, inline)?;
                limit = raw
                    .parse()
                    .map_err(|_| format!("error: --limit expects positive integer, got '{raw}'"))?;
            }
            other => return Err(format!("error: unrecognized argument '{other}' for show")),
        }
        i += 1;
    }

    let mode_count = usize::from(today)
        + usize::from(week)
        + usize::from(period.is_some())
        + usize::from(blockers)
        + usize::from(decisions)
        + usize::from(recent.is_some())
        + usize::from(grep.is_some());
    if mode_count > 1 {
        return Err("error: show modes (--today, --week, --period, --blockers, --decisions, --recent, --grep) are mutually exclusive".to_string());
    }
    if since.is_some() && !decisions {
        return Err("error: --since applies only to show --decisions".to_string());
    }
    if !kinds.is_empty() && grep.is_none() {
        return Err("error: --kind applies only to show --grep".to_string());
    }

    let mode = if let Some(q) = grep {
        ShowMode::Grep {
            query: q,
            kinds,
            limit,
        }
    } else if week {
        ShowMode::Week
    } else if let Some(p) = period {
        ShowMode::Period(p)
    } else if blockers {
        ShowMode::Blockers
    } else if decisions {
        ShowMode::Decisions { since }
    } else if let Some(n) = recent {
        ShowMode::Recent(n)
    } else {
        ShowMode::Today
    };

    Ok(JournalCommand::Show { repo, mode, json })
}

fn parse_resolve(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut how = None;
    let mut event_id = None;
    let mut i = 0usize;
    while i < args.len() {
        if !arg_starts_with_dash(&args[i]) {
            let eid = os_str_to_utf8(&args[i], "EVENT_ID")?;
            if event_id.is_some() {
                return Err(format!(
                    "error: unexpected positional argument '{eid}' for resolve"
                ));
            }
            event_id = Some(eid.to_string());
            i += 1;
            continue;
        }
        let (flag, inline) = split_flag(&args[i], "resolve")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--how" => how = Some(take_val_str(args, &mut i, flag, inline)?.to_string()),
            other => {
                return Err(format!(
                    "error: unrecognized argument '{other}' for resolve"
                ));
            }
        }
        i += 1;
    }
    let eid = event_id.ok_or_else(|| "error: resolve requires EVENT_ID".to_string())?;
    let how_s = how.ok_or_else(|| "error: resolve requires --how".to_string())?;
    Ok(JournalCommand::Resolve {
        repo,
        event_id: eid,
        how: how_s,
    })
}

fn parse_sync(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut force = false;
    let mut today = false;
    let mut recent_days = None;
    let mut budget = 180.0f64;
    let mut max_per_tick = None;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "sync")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--force" => force = true,
            "--today" => today = true,
            "--recent-days" => {
                let raw = take_val_str(args, &mut i, flag, inline)?;
                let d: i64 = raw
                    .parse()
                    .map_err(|_| format!("error: --recent-days expects integer, got '{raw}'"))?;
                if d < 0 {
                    return Err("error: --recent-days must be >= 0".to_string());
                }
                recent_days = Some(d);
            }
            "--budget" => {
                let raw = take_val_str(args, &mut i, flag, inline)?;
                let b: f64 = raw
                    .parse()
                    .map_err(|_| format!("error: --budget expects number, got '{raw}'"))?;
                if !b.is_finite() || b <= 0.0 {
                    return Err("error: --budget must be positive".to_string());
                }
                budget = b;
            }
            "--max-per-tick" => {
                let raw = take_val_str(args, &mut i, flag, inline)?;
                let n: usize = raw
                    .parse()
                    .map_err(|_| format!("error: --max-per-tick expects integer, got '{raw}'"))?;
                if n == 0 {
                    return Err("error: --max-per-tick must be >= 1".to_string());
                }
                max_per_tick = Some(n);
            }
            other => return Err(format!("error: unrecognized argument '{other}' for sync")),
        }
        i += 1;
    }
    Ok(JournalCommand::Sync {
        repo,
        force,
        today,
        recent_days,
        budget,
        max_per_tick,
    })
}

fn parse_distill(args: &[OsString]) -> Result<JournalCommand, String> {
    let mut repo = None;
    let mut dry_run = false;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = split_flag(&args[i], "distill")?;
        match flag {
            "-h" | "--help" => return Ok(JournalCommand::Help),
            "--repo" => repo = Some(take_val_os(args, &mut i, flag, inline)?),
            "--dry-run" => dry_run = true,
            other => {
                return Err(format!(
                    "error: unrecognized argument '{other}' for distill"
                ));
            }
        }
        i += 1;
    }
    Ok(JournalCommand::Distill { repo, dry_run })
}
