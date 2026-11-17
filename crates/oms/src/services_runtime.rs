//! Native runtime host adapters and shared bounded Unix account-ownership proofs.
#[path = "services_apptainer.rs"]
mod apptainer;
use oms_command_plan::services::GroupMembership;
use oms_supervisor::capture::{CaptureOptions, LimitAction};
use oms_supervisor::process::{Progress, Reactor, Request, Scope, Streams};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ACCOUNT_ENUM_WALL: Duration = Duration::from_secs(10);
const ACCOUNT_ENUM_LIMIT: usize = 16 * 1024 * 1024;
const ACCOUNT_ENUM_ERR_LIMIT: usize = 64 * 1024;
const NSSWITCH_LIMIT: usize = 64 * 1024;

fn plan_error(message: impl Into<String>) -> oms_command_plan::Error {
    oms_command_plan::Error {
        code: 2,
        message: message.into(),
    }
}

/// Resolves group membership for private-group permission checks in
/// `oms_command_plan` and `oms_runtime`, matching Python `_private_group`.
///
/// Every NSS/directory query runs inside a supervised clean-environment child
/// process with an explicit wall deadline and output bound so an unresponsive
/// host directory service can never hang in-process permission checks.
pub(crate) fn resolve_group_membership(gid: u32) -> oms_command_plan::Result<GroupMembership> {
    #[cfg(target_os = "linux")]
    {
        verify_linux_nss_completeness()?;
        let gid_arg = gid.to_string();
        let (group_code, group_raw) =
            run_clean_probe("/usr/bin/getent", &["group", &gid_arg], &[0, 2])?;
        if group_code == 2 || group_raw.iter().all(|b| b.is_ascii_whitespace()) {
            return Err(plan_error("unknown group id"));
        }
        let member_names = parse_colon_group_members(&group_raw, gid)?;
        let (_, accounts_raw) = run_clean_probe("/usr/bin/getent", &["passwd"], &[0])?;
        let (uid_by_name, primary_member_uids) = parse_colon_account_entries(&accounts_raw, gid)?;
        resolve_membership_uids(gid, &member_names, &uid_by_name, primary_member_uids)
    }
    #[cfg(target_os = "macos")]
    {
        let gid_arg = gid.to_string();
        let (_, group_raw) = run_clean_probe(
            "/usr/bin/dscacheutil",
            &["-q", "group", "-a", "gid", &gid_arg],
            &[0],
        )?;
        let member_names = parse_dscacheutil_group_members(&group_raw, gid)?;
        let (_, accounts_raw) = run_clean_probe("/usr/bin/dscacheutil", &["-q", "user"], &[0])?;
        let (uid_by_name, primary_member_uids) =
            parse_dscacheutil_account_entries(&accounts_raw, gid)?;
        resolve_membership_uids(gid, &member_names, &uid_by_name, primary_member_uids)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = gid;
        Err(plan_error(
            "native account ownership is unsupported on this host",
        ))
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn read_bounded_system_file(
    path: &Path,
    limit: usize,
) -> oms_command_plan::Result<Option<Vec<u8>>> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_file() {
                return Err(plan_error(format!(
                    "host account config is not a regular file: {}",
                    path.display()
                )));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(plan_error(format!(
                "cannot stat host account config {}: {e}",
                path.display()
            )));
        }
    }
    let file = std::fs::File::open(path).map_err(|e| {
        plan_error(format!(
            "cannot open host account config {}: {e}",
            path.display()
        ))
    })?;
    let mut raw = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|e| {
            plan_error(format!(
                "cannot read host account config {}: {e}",
                path.display()
            ))
        })?;
    if raw.len() > limit {
        return Err(plan_error(format!(
            "host account config exceeds size limit: {}",
            path.display()
        )));
    }
    Ok(Some(raw))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_nss_source_tokens(rest: &str) -> oms_command_plan::Result<Vec<String>> {
    let mut sources = Vec::new();
    let mut current = String::new();
    let mut in_bracket = false;
    for ch in rest.chars() {
        if in_bracket {
            match ch {
                '[' => return Err(plan_error("malformed nsswitch.conf nested bracket")),
                ']' => in_bracket = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '[' => {
                if !current.is_empty() {
                    return Err(plan_error("malformed nsswitch.conf token before bracket"));
                }
                in_bracket = true;
            }
            ']' => {
                return Err(plan_error(
                    "malformed nsswitch.conf unmatched closing bracket",
                ));
            }
            c if c.is_ascii_whitespace() => {
                if !current.is_empty() {
                    sources.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_bracket {
        return Err(plan_error(
            "malformed nsswitch.conf unclosed action bracket",
        ));
    }
    if !current.is_empty() {
        sources.push(current);
    }
    if sources.is_empty() {
        return Err(plan_error("malformed nsswitch.conf empty source list"));
    }
    Ok(sources)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_nsswitch_databases(raw: &[u8]) -> oms_command_plan::Result<(Vec<String>, Vec<String>)> {
    let text =
        std::str::from_utf8(raw).map_err(|_| plan_error("nsswitch.conf is not valid UTF-8"))?;
    let mut passwd_sources: Option<Vec<String>> = None;
    let mut group_sources: Option<Vec<String>> = None;
    let mut continuation = String::new();
    for line in text.lines() {
        let stripped = line.split('#').next().unwrap_or("").trim();
        if continuation.is_empty() && stripped.is_empty() {
            continue;
        }
        if !continuation.is_empty() {
            continuation.push(' ');
        }
        if let Some(prefix) = stripped.strip_suffix('\\') {
            continuation.push_str(prefix);
            continue;
        }
        continuation.push_str(stripped);
        let logical = std::mem::take(&mut continuation);
        let trimmed = logical.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((db, rest)) = trimmed.split_once(':') else {
            return Err(plan_error("malformed nsswitch.conf line: missing colon"));
        };
        let db_name = db.trim();
        let slot = if db_name == concat!("pass", "wd") {
            &mut passwd_sources
        } else if db_name == "group" {
            &mut group_sources
        } else {
            continue;
        };
        if slot.is_some() {
            return Err(plan_error(format!(
                "duplicate nsswitch.conf database entry: {}",
                db.trim()
            )));
        }
        *slot = Some(parse_nss_source_tokens(rest)?);
    }
    if !continuation.trim().is_empty() {
        return Err(plan_error(
            "unterminated line continuation in nsswitch.conf",
        ));
    }
    Ok((
        passwd_sources.unwrap_or_else(|| vec!["files".to_owned()]),
        group_sources.unwrap_or_else(|| vec!["files".to_owned()]),
    ))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn compat_files_are_local_only(
    passwd_path: &Path,
    group_path: &Path,
) -> oms_command_plan::Result<bool> {
    for path in [passwd_path, group_path] {
        let Some(raw) = read_bounded_system_file(path, ACCOUNT_ENUM_LIMIT)? else {
            continue;
        };
        let text = std::str::from_utf8(&raw)
            .map_err(|_| plan_error(format!("non-UTF-8 account file {}", path.display())))?;
        if text.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with('+') || trimmed.starts_with('-')
        }) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn verify_linux_nss_sources(
    passwd_sources: &[String],
    group_sources: &[String],
    mut check_compat: impl FnMut() -> oms_command_plan::Result<bool>,
    mut check_sss: impl FnMut(&str) -> oms_command_plan::Result<()>,
) -> oms_command_plan::Result<()> {
    let mut compat_checked: Option<bool> = None;
    for (db, sources) in [("passwd", passwd_sources), ("group", group_sources)] {
        for src in sources {
            match src.as_str() {
                "files" | "systemd" => {}
                "compat" => {
                    let ok = match compat_checked {
                        Some(v) => v,
                        None => {
                            let v = check_compat()?;
                            compat_checked = Some(v);
                            v
                        }
                    };
                    if !ok {
                        return Err(plan_error(
                            "NSS compat database uses external +/- entries; private-group proof unavailable",
                        ));
                    }
                }
                "sss" => check_sss(db)?,
                other => {
                    return Err(plan_error(format!(
                        "unverified NSS {db} source '{other}'; complete account enumeration cannot be proven"
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn verify_linux_nss_completeness() -> oms_command_plan::Result<()> {
    let (passwd_sources, group_sources) =
        match read_bounded_system_file(Path::new("/etc/nsswitch.conf"), NSSWITCH_LIMIT)? {
            Some(raw) => parse_nsswitch_databases(&raw)?,
            None => (vec!["files".to_owned()], vec!["files".to_owned()]),
        };
    verify_linux_nss_sources(
        &passwd_sources,
        &group_sources,
        || compat_files_are_local_only(Path::new("/etc/passwd"), Path::new("/etc/group")),
        |db| match std::fs::symlink_metadata("/var/lib/sss/pipes/nss") {
            Ok(_) => {
                let (_, out) = run_clean_probe("/usr/bin/getent", &["-s", "sss", db], &[0])?;
                if out.iter().all(|b| b.is_ascii_whitespace()) {
                    return Err(plan_error(format!(
                        "NSS provider sss for {db} returned no enumerated entries; complete account proof unavailable"
                    )));
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(plan_error(format!("cannot inspect sss NSS pipe: {e}"))),
        },
    )
}

fn resolve_membership_uids(
    gid: u32,
    member_names: &[Vec<u8>],
    uid_by_name: &BTreeMap<Vec<u8>, u32>,
    primary_member_uids: Vec<u32>,
) -> oms_command_plan::Result<GroupMembership> {
    let mut explicit_member_uids = Vec::with_capacity(member_names.len());
    for name in member_names {
        let uid = uid_by_name
            .get(name)
            .copied()
            .ok_or_else(|| plan_error("group member account is unavailable"))?;
        explicit_member_uids.push(uid);
    }
    Ok(GroupMembership {
        gid,
        explicit_member_uids,
        primary_member_uids,
    })
}

fn run_clean_probe(
    program: &str,
    args: &[&str],
    ok_exits: &[i32],
) -> oms_command_plan::Result<(i32, Vec<u8>)> {
    let mut argv = Vec::with_capacity(1 + args.len());
    argv.push(OsString::from(program));
    argv.extend(args.iter().map(OsString::from));

    let mut request = Request::new(argv);
    request.cwd = Some(PathBuf::from("/"));
    request.stdin = Some(Vec::new());
    request.clear_environment = true;
    request.environment = vec![("LC_ALL".into(), "C".into())];
    request.streams = Streams::Separate;
    request.scope = Scope::ProcessGroup;
    request.deadline = Some(
        Instant::now()
            .checked_add(ACCOUNT_ENUM_WALL)
            .ok_or_else(|| plan_error("account probe deadline overflow"))?,
    );
    request.capture = CaptureOptions {
        max_bytes: ACCOUNT_ENUM_LIMIT,
        on_limit: LimitAction::Terminate,
        lines: None,
    };
    request.stderr_capture = Some(CaptureOptions {
        max_bytes: ACCOUNT_ENUM_ERR_LIMIT,
        on_limit: LimitAction::Terminate,
        lines: None,
    });

    let mut reactor = Reactor::new();
    let id = reactor
        .start(request)
        .map_err(|e| plan_error(format!("account enumeration failed to start: {e}")))?;
    loop {
        match reactor
            .poll(id, Duration::from_millis(50))
            .map_err(|e| plan_error(format!("account enumeration poll failed: {e}")))?
        {
            Progress::Pending(_) => {}
            Progress::Complete(done) => {
                if !done.cleanup_complete()
                    || !done.output_complete()
                    || done.stop.is_some()
                    || done.stdout.limited
                    || done.stdout.dropped_bytes > 0
                    || !done.stdout.eof
                {
                    return Err(plan_error("account enumeration did not complete cleanly"));
                }
                let code = done.exit.map_or(125, |e| e.shell_code());
                if !ok_exits.contains(&code) {
                    return Err(plan_error(format!(
                        "account enumeration failed with exit {code}"
                    )));
                }
                return Ok((code, done.stdout.bytes));
            }
        }
    }
}

fn parse_u32_bytes(raw: &[u8], label: &str) -> oms_command_plan::Result<u32> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| plan_error(format!("non-UTF-8 {label} in account database")))?;
    text.trim()
        .parse::<u32>()
        .map_err(|_| plan_error(format!("invalid {label} in account database")))
}

type PasswdAccounts = (BTreeMap<Vec<u8>, u32>, Vec<u32>);

fn insert_account_uid(
    map: &mut BTreeMap<Vec<u8>, u32>,
    name: &[u8],
    uid: u32,
) -> oms_command_plan::Result<()> {
    if name.is_empty() {
        return Err(plan_error("malformed account entry: empty name"));
    }
    if let Some(prev) = map.insert(name.to_vec(), uid)
        && prev != uid
    {
        return Err(plan_error("conflicting account name to uid mapping"));
    }
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_colon_group_members(raw: &[u8], gid: u32) -> oms_command_plan::Result<Vec<Vec<u8>>> {
    let mut matched: Option<Vec<Vec<u8>>> = None;
    for line in raw.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if matched.is_some() {
            return Err(plan_error("ambiguous group entries for gid"));
        }
        let mut fields = line.split(|b| *b == b':');
        let Some(name) = fields.next().filter(|f| !f.is_empty()) else {
            return Err(plan_error("malformed group entry: missing name"));
        };
        let Some(_placeholder) = fields.next() else {
            return Err(plan_error("malformed group entry: missing field"));
        };
        let Some(gid_bytes) = fields.next() else {
            return Err(plan_error("malformed group entry: missing gid"));
        };
        let Some(members_bytes) = fields.next() else {
            return Err(plan_error("malformed group entry: missing member list"));
        };
        if fields.next().is_some() {
            return Err(plan_error("malformed group entry: extra fields"));
        }
        let _ = name;
        if parse_u32_bytes(gid_bytes, "gid")? != gid {
            return Err(plan_error("group entry gid mismatch"));
        }
        let members = members_bytes
            .split(|b| *b == b',')
            .filter(|m| !m.is_empty())
            .map(<[u8]>::to_vec)
            .collect();
        matched = Some(members);
    }
    matched.ok_or_else(|| plan_error("unknown group id"))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_colon_account_entries(raw: &[u8], gid: u32) -> oms_command_plan::Result<PasswdAccounts> {
    let mut by_name = BTreeMap::new();
    let mut primary_uids = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(|b| *b == b':');
        let Some(name) = fields.next() else {
            return Err(plan_error("malformed account entry: missing name"));
        };
        let _placeholder = fields.next();
        let Some(uid_bytes) = fields.next() else {
            return Err(plan_error("malformed account entry: missing uid"));
        };
        let Some(gid_bytes) = fields.next() else {
            return Err(plan_error("malformed account entry: missing gid"));
        };
        let pw_uid = parse_u32_bytes(uid_bytes, "uid")?;
        let pw_gid = parse_u32_bytes(gid_bytes, "gid")?;
        insert_account_uid(&mut by_name, name, pw_uid)?;
        if pw_gid == gid {
            primary_uids.push(pw_uid);
        }
    }
    Ok((by_name, primary_uids))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn trim_ascii_bytes(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_dscacheutil_group_members(raw: &[u8], gid: u32) -> oms_command_plan::Result<Vec<Vec<u8>>> {
    let mut matched: Option<Vec<Vec<u8>>> = None;
    let mut in_stanza = false;
    let mut stanza_gid: Option<u32> = None;
    let mut stanza_members: Vec<Vec<u8>> = Vec::new();

    let flush = |in_stanza: &mut bool,
                 sgid: &mut Option<u32>,
                 members: &mut Vec<Vec<u8>>,
                 out: &mut Option<Vec<Vec<u8>>>|
     -> oms_command_plan::Result<()> {
        if !*in_stanza {
            return Ok(());
        }
        if out.is_some() {
            return Err(plan_error("ambiguous dscacheutil group entries for gid"));
        }
        let g = sgid
            .take()
            .ok_or_else(|| plan_error("malformed dscacheutil group stanza: missing gid"))?;
        if g != gid {
            return Err(plan_error("dscacheutil group gid mismatch"));
        }
        *in_stanza = false;
        *out = Some(std::mem::take(members));
        Ok(())
    };

    for line in raw.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            flush(
                &mut in_stanza,
                &mut stanza_gid,
                &mut stanza_members,
                &mut matched,
            )?;
            continue;
        }
        in_stanza = true;
        let Some(colon) = line.iter().position(|b| *b == b':') else {
            return Err(plan_error("malformed dscacheutil line: missing colon"));
        };
        let key = trim_ascii_bytes(&line[..colon]);
        let val = trim_ascii_bytes(&line[colon + 1..]);
        if key == b"gid" {
            stanza_gid = Some(parse_u32_bytes(val, "gid")?);
        } else if key == b"users" {
            stanza_members.extend(
                val.split(|b| *b == b' ' || *b == b'\t')
                    .filter(|m| !m.is_empty())
                    .map(<[u8]>::to_vec),
            );
        }
    }
    flush(
        &mut in_stanza,
        &mut stanza_gid,
        &mut stanza_members,
        &mut matched,
    )?;
    matched.ok_or_else(|| plan_error("unknown group id"))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_dscacheutil_account_entries(
    raw: &[u8],
    gid: u32,
) -> oms_command_plan::Result<PasswdAccounts> {
    let mut by_name = BTreeMap::new();
    let mut primary_uids = Vec::new();
    let mut in_stanza = false;
    let mut stanza_name: Option<&[u8]> = None;
    let mut stanza_uid: Option<u32> = None;
    let mut stanza_gid: Option<u32> = None;

    let flush = |in_stanza: &mut bool,
                 name: &mut Option<&[u8]>,
                 uid: &mut Option<u32>,
                 sgid: &mut Option<u32>,
                 by_name: &mut BTreeMap<Vec<u8>, u32>,
                 out: &mut Vec<u32>|
     -> oms_command_plan::Result<()> {
        if !*in_stanza {
            return Ok(());
        }
        let n = name
            .take()
            .ok_or_else(|| plan_error("malformed dscacheutil stanza: missing name"))?;
        let u = uid
            .take()
            .ok_or_else(|| plan_error("malformed dscacheutil stanza: missing uid"))?;
        let g = sgid
            .take()
            .ok_or_else(|| plan_error("malformed dscacheutil stanza: missing gid"))?;
        *in_stanza = false;
        insert_account_uid(by_name, n, u)?;
        if g == gid {
            out.push(u);
        }
        Ok(())
    };

    for line in raw.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            flush(
                &mut in_stanza,
                &mut stanza_name,
                &mut stanza_uid,
                &mut stanza_gid,
                &mut by_name,
                &mut primary_uids,
            )?;
            continue;
        }
        in_stanza = true;
        let Some(colon) = line.iter().position(|b| *b == b':') else {
            return Err(plan_error("malformed dscacheutil line: missing colon"));
        };
        let key = trim_ascii_bytes(&line[..colon]);
        let val = trim_ascii_bytes(&line[colon + 1..]);
        if key == b"name" {
            stanza_name = Some(val);
        } else if key == b"uid" {
            stanza_uid = Some(parse_u32_bytes(val, "uid")?);
        } else if key == b"gid" {
            stanza_gid = Some(parse_u32_bytes(val, "gid")?);
        }
    }
    flush(
        &mut in_stanza,
        &mut stanza_name,
        &mut stanza_uid,
        &mut stanza_gid,
        &mut by_name,
        &mut primary_uids,
    )?;
    Ok((by_name, primary_uids))
}

// ---------------------------------------------------------------------------
// RuntimeServices: one host adapter for the runtime, hook, run-ledger and input
// producer boundaries. It only forwards to native producers; it keeps no mutable
// state (the hook and input traits require `Send + Sync`) and never changes
// process-global cwd or environment. Every child runs under `oms_supervisor`
// with a wall deadline and bounded output; argv is never shell-parsed and no
// interpreter or legacy script is invoked.
// ---------------------------------------------------------------------------
use crate::services_control::ControlServices;
use oms_runtime::services::{
    BackendRequest, BackendResult, ExternalOutput, InstallerRequest, LedgerRequest, LedgerResult,
    PromotionRequest, RelatedSource, ReleaseRequest, RepositoryView, RuntimeRoots, TargetRequest,
};
use oms_runtime::value::{self as rvalue, Value};
use oms_supervisor::process::StopReason;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use users::os::unix::UserExt;

const GIT_WALL: Duration = Duration::from_secs(30);
const FETCH_WALL: Duration = Duration::from_secs(600);
const GIT_LIMIT: usize = 4 * 1024 * 1024;
const PROBE_LIMIT: usize = 64 * 1024;
const MANIFEST_PATH: &str = "config/update-channels.json";
const MANIFEST_LIMIT: usize = 8 * 1024 * 1024;
const HELPER_NAME: &str = "oms-run-bounded-helper";
const LAUNCH_FAILED: &str = "launch failed: ";
const UNSAFE_INPUT: &str = "invalid, unavailable, or unsafe local input state";

fn unavailable(what: &str) -> oms_command_plan::Error {
    plan_error(format!("native {what} service unavailable"))
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time (proleptic Gregorian, Hinnant's civil-from-days).
fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

struct ChildSpec<'a> {
    argv: Vec<OsString>,
    cwd: Option<&'a Path>,
    clear_environment: bool,
    environment: Vec<(OsString, OsString)>,
    remove_environment: Vec<OsString>,
    merged: bool,
    /// Keep head and tail of an over-limit stream instead of stopping the child.
    drain: bool,
    stdin: Vec<u8>,
    wall: Duration,
    limit: usize,
}
impl<'a> ChildSpec<'a> {
    fn new(argv: Vec<OsString>, wall: Duration, limit: usize) -> Self {
        Self {
            argv,
            cwd: None,
            clear_environment: false,
            environment: Vec::new(),
            remove_environment: Vec::new(),
            merged: false,
            drain: false,
            stdin: Vec::new(),
            wall,
            limit,
        }
    }
}
#[derive(Debug)]
struct ChildOut {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    elapsed: Duration,
    timed_out: bool,
    cancelled: bool,
    dropped: u64,
}

/// One fully supervised child: exit codes follow the shell convention (124
/// deadline, 125 output limit/supervisor stop, 130 cancellation). A launch
/// failure is an `Err` starting with [`LAUNCH_FAILED`].
fn run_child(
    scope: Scope,
    spec: ChildSpec<'_>,
    interrupted: &AtomicBool,
) -> Result<ChildOut, String> {
    if interrupted.load(Ordering::Acquire) {
        return Err("interrupted".into());
    }
    if spec.argv.is_empty() || spec.wall.is_zero() || spec.limit == 0 {
        return Err("execution requires a command and positive wall and output bounds".into());
    }
    let mut request = Request::new(spec.argv);
    request.cwd = spec.cwd.map(Path::to_owned);
    request.stdin = Some(spec.stdin);
    request.clear_environment = spec.clear_environment;
    request.environment = spec.environment;
    request.remove_environment = spec.remove_environment;
    request.streams = if spec.merged {
        Streams::Merged
    } else {
        Streams::Separate
    };
    request.scope = scope;
    request.deadline = Some(
        Instant::now()
            .checked_add(spec.wall)
            .ok_or("execution deadline overflow")?,
    );
    request.capture = CaptureOptions {
        max_bytes: spec.limit,
        on_limit: if spec.drain {
            LimitAction::Drain
        } else {
            LimitAction::Terminate
        },
        lines: None,
    };
    if !spec.merged {
        request.stderr_capture = Some(request.capture.clone());
    }
    let mut reactor = if scope == Scope::WholeTree {
        Reactor::dedicated().map_err(|e| e.to_string())?
    } else {
        Reactor::new()
    };
    let id = reactor
        .start(request)
        .map_err(|e| format!("{LAUNCH_FAILED}{e}"))?;
    let mut cancelled = false;
    loop {
        if interrupted.load(Ordering::Acquire) && !cancelled {
            reactor
                .stop(id, StopReason::Cancelled)
                .map_err(|e| e.to_string())?;
            cancelled = true;
        }
        match reactor
            .poll(id, Duration::from_millis(50))
            .map_err(|e| e.to_string())?
        {
            Progress::Pending(_) => {}
            Progress::Complete(done) => {
                if !done.cleanup_complete() || (done.stop.is_none() && !done.output_complete()) {
                    return Err("native supervision incomplete; reconcile before retry".into());
                }
                let code = match done.stop {
                    Some(StopReason::Deadline) => 124,
                    Some(StopReason::Cancelled) => 130,
                    Some(_) => 125,
                    None => done.exit.map_or(125, |e| e.shell_code()),
                };
                let dropped =
                    done.stdout.dropped_bytes + done.stderr.as_ref().map_or(0, |s| s.dropped_bytes);
                return Ok(ChildOut {
                    code,
                    stdout: done.stdout.bytes,
                    stderr: done.stderr.map_or_else(Vec::new, |s| s.bytes),
                    elapsed: done.duration,
                    timed_out: done.stop == Some(StopReason::Deadline),
                    cancelled: done.stop == Some(StopReason::Cancelled),
                    dropped,
                });
            }
        }
    }
}

/// Ambient `GIT_*` names to strip from a Git child, except those it sets itself.
fn ambient_git_variables(
    names: impl Iterator<Item = OsString>,
    keep: &[(OsString, OsString)],
) -> Vec<OsString> {
    names
        .filter(|k| k.as_bytes().to_ascii_uppercase().starts_with(b"GIT_"))
        .filter(|k| !keep.iter().any(|(name, _)| name == k))
        .collect()
}

fn staged_launcher(exe: &Path, source: &Path) -> Vec<OsString> {
    if exe.file_name().and_then(|v| v.to_str()) != Some("oms") {
        return Vec::new();
    }
    let Some(rev_dir) = exe.parent() else {
        return Vec::new();
    };
    let Some(rev) = rev_dir.file_name().and_then(|v| v.to_str()) else {
        return Vec::new();
    };
    if rev.len() != 40 || !rev.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Vec::new();
    }
    if rev_dir.parent() != Some(source.join("target/oms-native").as_path()) {
        return Vec::new();
    }
    if !std::fs::symlink_metadata(rev_dir.join("manifest.json"))
        .is_ok_and(|m| m.file_type().is_file())
    {
        return Vec::new();
    }
    vec![
        exe.as_os_str().to_owned(),
        "--oms-dispatch".into(),
        source.as_os_str().to_owned(),
        rev.into(),
        "--".into(),
    ]
}

/// The bounded-run helper shipped beside the executing binary: a regular,
/// non-symlink, executable file; anything else means no helper.
fn bundled_helper(exe: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = exe.parent()?.join(HELPER_NAME);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    (meta.file_type().is_file() && meta.permissions().mode() & 0o111 != 0).then_some(path)
}

fn native_verb(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= 64
        && tool.as_bytes()[0].is_ascii_lowercase()
        && tool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_hex_oid(text: &str) -> bool {
    (text.len() == 40 || text.len() == 64)
        && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Native runtime host adapter. Read-only producers forward to the owning
/// crates; mutations run under the owner's lock; where no compiled native
/// producer exists the method refuses instead of reporting success.
pub(crate) struct RuntimeServices<'a> {
    interrupted: &'a Arc<AtomicBool>,
    control: ControlServices<'a>,
    source: PathBuf,
    scope: Scope,
    native: Vec<OsString>,
    helper: Option<PathBuf>,
    /// `OMS_SSH_BIN` as seen at construction, else `ssh`.
    ssh: OsString,
    apptainer: Option<PathBuf>,
    #[cfg(test)]
    lock_timeout: Option<Duration>,
}

impl<'a> RuntimeServices<'a> {
    /// `source` is the validated native source/install root. Enforces
    /// `Scope::WholeTree` on Linux and `Scope::ProcessGroup` elsewhere.
    pub(crate) fn new(interrupted: &'a Arc<AtomicBool>, source: &Path) -> Result<Self, String> {
        let control = ControlServices::new(interrupted, source)?;
        let source = std::fs::canonicalize(source)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or("native source root must be an existing directory")?;
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .ok()
            .filter(|p| p.is_file())
            .ok_or("cannot resolve the executing native binary")?;
        #[cfg(target_os = "linux")]
        let scope = Scope::WholeTree;
        #[cfg(not(target_os = "linux"))]
        let scope = Scope::ProcessGroup;
        Ok(Self {
            interrupted,
            control,
            native: staged_launcher(&exe, &source),
            helper: bundled_helper(&exe),
            ssh: std::env::var_os("OMS_SSH_BIN")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "ssh".into()),
            apptainer: std::env::var_os("PATH").and_then(|p| apptainer::locate(&p)),
            source,
            scope,
            #[cfg(test)]
            lock_timeout: None,
        })
    }
    /// Test constructor: `ProcessGroup` scope so multi-threaded harnesses can
    /// run children, and an explicit launcher and helper.
    #[cfg(test)]
    fn with_overrides(
        interrupted: &'a Arc<AtomicBool>,
        source: &Path,
        native: Vec<OsString>,
        helper: Option<PathBuf>,
    ) -> Result<Self, String> {
        let mut services = Self::new(interrupted, source)?;
        services.scope = Scope::ProcessGroup;
        services.native = native;
        services.helper = helper;
        Ok(services)
    }

    fn cancel(&self) -> &AtomicBool {
        self.interrupted.as_ref()
    }
    fn child(&self, spec: ChildSpec<'_>) -> oms_command_plan::Result<ChildOut> {
        #[cfg(target_os = "linux")]
        if self.scope == Scope::WholeTree
            && !self.control.supervise_worker().is_empty()
            && std::fs::read_dir("/proc/self/task").is_ok_and(|entries| entries.count() != 1)
        {
            let mut spawn =
                crate::services_control::Spawn::new(self.scope, spec.argv, spec.wall, spec.limit);
            spawn.cwd = spec.cwd;
            spawn.clear_environment = spec.clear_environment;
            spawn.environment = spec.environment;
            spawn.remove_environment = spec.remove_environment;
            spawn.merged = spec.merged;
            spawn.drain = spec.drain;
            spawn.stdin = spec.stdin;
            let child = crate::services_control::run_via_supervise_worker(
                self.control.supervise_worker(),
                &spawn,
                self.cancel(),
            )
            .map_err(plan_error)?;
            return Ok(ChildOut {
                code: child.code,
                stdout: child.stdout,
                stderr: child.stderr,
                elapsed: child.elapsed,
                timed_out: child.timed_out,
                cancelled: child.cancelled,
                dropped: child.dropped,
            });
        }
        run_child(self.scope, spec, self.cancel()).map_err(plan_error)
    }
    /// Bounded Git child with no ambient `GIT_*` redirection or prompts.
    fn git(
        &self,
        repo: &Path,
        args: &[&str],
        wall: Duration,
    ) -> oms_command_plan::Result<ChildOut> {
        self.git_env(repo, args, wall, &[])
    }
    fn git_env(
        &self,
        repo: &Path,
        args: &[&str],
        wall: Duration,
        extra: &[(&str, &str)],
    ) -> oms_command_plan::Result<ChildOut> {
        let mut argv: Vec<OsString> = ["git", "--no-pager", "-c", "core.fsmonitor=false"]
            .iter()
            .map(OsString::from)
            .collect();
        argv.extend(["-c".into(), "core.hooksPath=/dev/null".into()]);
        argv.extend(["-C".into(), repo.as_os_str().to_owned()]);
        argv.extend(args.iter().map(OsString::from));
        let mut spec = ChildSpec::new(argv, wall, GIT_LIMIT);
        let mut environment: Vec<(OsString, OsString)> = vec![
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        environment.extend(
            extra
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v))),
        );
        // The supervisor applies removals after additions, so a variable set
        // here must not also be listed for removal.
        spec.remove_environment =
            ambient_git_variables(std::env::vars_os().map(|(k, _)| k), &environment);
        spec.environment = environment;
        self.child(spec)
    }
    /// Trimmed stdout of a successful Git query; `None` when Git exits non-zero.
    fn git_text(&self, repo: &Path, args: &[&str]) -> oms_command_plan::Result<Option<String>> {
        let out = self.git(repo, args, GIT_WALL)?;
        Ok((out.code == 0).then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()))
    }
    fn group_store(
        &self,
        root: &Path,
    ) -> oms_command_plan::Result<oms_command_plan::storage::Store> {
        Ok(oms_command_plan::storage::Store::open(root)?
            .with_group_resolver(Some(resolve_group_membership)))
    }
    fn expand_tilde(&self, raw: &OsStr) -> oms_command_plan::Result<PathBuf> {
        let bytes = raw.as_bytes();
        if !bytes.starts_with(b"~") {
            return Ok(PathBuf::from(raw));
        }
        let end = bytes.iter().position(|b| *b == b'/').unwrap_or(bytes.len());
        let home = if end == 1 {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    users::get_user_by_uid(users::get_current_uid())
                        .map(|u| u.home_dir().to_path_buf())
                })
        } else {
            oms_runtime::Services::account_home(self, OsStr::from_bytes(&bytes[1..end]))?
        }
        .ok_or_else(|| plan_error("Could not determine home directory."))?;
        let rest = bytes
            .get(end + usize::from(end < bytes.len())..)
            .unwrap_or(&[]);
        Ok(home.join(OsStr::from_bytes(rest)))
    }
}

impl oms_runtime::Services for RuntimeServices<'_> {
    /// Read-only passwd lookup of a named user's home; no file or exec authority.
    fn account_home(&self, username: &OsStr) -> oms_command_plan::Result<Option<PathBuf>> {
        let name = username.as_bytes();
        if name.is_empty() || name.len() > 256 || name.iter().any(|b| *b == 0 || *b == b'/') {
            return Ok(None);
        }
        Ok(users::get_user_by_name(username).map(|u| u.home_dir().to_path_buf()))
    }
    /// Execution root is the physical Git top level; the state root follows the
    /// plan context rules (`OMS_STATE_REPO`, else the verified shared owner).
    fn resolve_roots(
        &self,
        cwd: &Path,
        requested: &OsStr,
    ) -> oms_command_plan::Result<RuntimeRoots> {
        let raw = if requested.is_empty() {
            cwd.as_os_str().to_owned()
        } else {
            requested.to_owned()
        };
        let mut request = oms_command_plan::Request::new(oms_command_plan::Action::Status);
        request.repo = Some(self.expand_tilde(&raw)?);
        let first = oms_command_plan::Context::from_environment_at(&request, cwd, cwd, None)?;
        let hint = if std::env::var_os("OMS_STATE_REPO").is_some_and(|v| !v.is_empty()) {
            None
        } else {
            Some(
                oms_command_room::Services::state_repository(
                    &oms_command_room::LocalServices,
                    &first.execution_repo,
                )
                .map_err(|e| plan_error(e.to_string()))?,
            )
        };
        let context =
            oms_command_plan::Context::from_environment_at(&request, cwd, cwd, hint.as_deref())?;
        Ok(RuntimeRoots {
            execution_repo: context.execution_repo,
            state_repo: context.state_repo,
        })
    }
    fn context_dependencies(
        &self,
        repo: &Path,
        targets: &[PathBuf],
    ) -> oms_command_plan::Result<Vec<RelatedSource>> {
        oms_command_graph::project::context_dependencies_with_groups(
            repo,
            targets,
            self.helper.as_deref(),
            Some(resolve_group_membership),
            self.cancel(),
        )
        .map_err(|e| plan_error(e.to_string()))
    }
    fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        Some(resolve_group_membership)
    }
    /// An unborn or detached HEAD yields an empty head or branch, as the task
    /// verifier does; the worktree fingerprint is the task verifier's own.
    fn repository_view(&self, repo: &Path) -> oms_command_plan::Result<RepositoryView> {
        let head = self
            .git_text(repo, &["rev-parse", "HEAD"])?
            .unwrap_or_default();
        let branch = self
            .git_text(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])?
            .unwrap_or_default();
        let worktree_state =
            oms_command_task::state_fingerprint(repo, self.cancel()).map_err(|e| {
                oms_command_plan::Error {
                    code: e.code,
                    message: String::from_utf8_lossy(&e.message).into_owned(),
                }
            })?;
        Ok(RepositoryView {
            head,
            branch,
            worktree_state,
        })
    }
    fn resolve_revision(
        &self,
        root: &Path,
        revision: &str,
    ) -> oms_command_plan::Result<Option<String>> {
        if revision.is_empty() || revision.starts_with('-') || revision.contains('\0') {
            return Err(plan_error("invalid revision"));
        }
        let spec = format!("{revision}^{{commit}}");
        self.git_text(root, &["rev-parse", "--verify", "--quiet", &spec])
            .map(|v| v.filter(|oid| !oid.is_empty()))
    }
    fn fetch_release(&self, root: &Path) -> oms_command_plan::Result<()> {
        let remote = self.git_text(root, &["remote", "get-url", "origin"])?;
        if remote.is_none_or(|r| r.is_empty()) {
            return Err(plan_error("cannot fetch channel without origin remote"));
        }
        let out = self.git(root, &["fetch", "--prune", "--tags", "origin"], FETCH_WALL)?;
        if out.code == 0 {
            Ok(())
        } else {
            Err(oms_command_plan::Error {
                code: if out.cancelled { 130 } else { 1 },
                message: "failed to fetch origin while resolving channel".into(),
            })
        }
    }
    /// A numeric PID is never enough across hosts: only this host's process
    /// table can prove absence, and any other host is never "gone".
    fn reservation_process_gone(&self, host: &str, pid: u32) -> oms_command_plan::Result<bool> {
        if pid == 0 || host != oms_runtime::Services::host_name(self)? {
            return Ok(false);
        }
        #[cfg(target_os = "linux")]
        {
            match std::fs::metadata(format!("/proc/{pid}")) {
                Ok(_) => Ok(false),
                Err(e) => Ok(e.kind() == std::io::ErrorKind::NotFound),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut spec = ChildSpec::new(
                vec!["/bin/kill".into(), "-0".into(), pid.to_string().into()],
                Duration::from_secs(5),
                PROBE_LIMIT,
            );
            spec.merged = true;
            let out = self.child(spec)?;
            Ok(out.code != 0
                && String::from_utf8_lossy(&out.stdout)
                    .to_ascii_lowercase()
                    .contains("no such process"))
        }
    }
    fn host_name(&self) -> oms_command_plan::Result<String> {
        #[cfg(target_os = "linux")]
        if let Ok(Some(raw)) =
            read_bounded_system_file(Path::new("/proc/sys/kernel/hostname"), 1024)
        {
            let name = String::from_utf8_lossy(&raw).trim().to_owned();
            if !name.is_empty() {
                return Ok(name);
            }
        }
        let mut spec = ChildSpec::new(
            vec!["uname".into(), "-n".into()],
            Duration::from_secs(5),
            PROBE_LIMIT,
        );
        spec.merged = true;
        let out = self.child(spec)?;
        let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if out.code == 0 && !name.is_empty() {
            Ok(name)
        } else {
            Err(unavailable("local host identity"))
        }
    }
    /// Captured packet bytes must equal the on-disk task before and after the
    /// native `task status --json`; the state root is the caller's, never re-derived.
    fn task_status(
        &self,
        _repo: &Path,
        state: &Path,
        packet: &[u8],
    ) -> oms_command_plan::Result<Vec<u8>> {
        let store = self.group_store(state)?;
        let current = || store.optional(TASK_PATH, oms_runtime::storage::MAX_STATE);
        if current()?.as_deref() != Some(packet) {
            return Err(plan_error("task changed during projection"));
        }
        let task_file = state.join(TASK_PATH);
        let args: [OsString; 6] = [
            "--repo".into(),
            state.as_os_str().to_owned(),
            "--file".into(),
            task_file.into_os_string(),
            "status".into(),
            "--json".into(),
        ];
        let (code, stdout, _) = oms_command_task::run_with_services(
            &args,
            state,
            self.cancel(),
            &mut TaskRead(state.to_owned()),
        );
        if code != 0 {
            return Err(plan_error("native task status failed"));
        }
        if current()?.as_deref() != Some(packet) {
            return Err(plan_error("task changed during projection"));
        }
        Ok(stdout)
    }
    fn plan_status(
        &self,
        repo: &Path,
        state: &Path,
        plan: &[u8],
    ) -> oms_command_plan::Result<Vec<u8>> {
        let open = self.plan(repo, state)?;
        if open.load()?.raw.as_deref() != Some(plan) {
            return Err(plan_error("plan changed during projection"));
        }
        let status = open.status_snapshot()?;
        if open.load()?.raw.as_deref() != Some(plan) {
            return Err(plan_error("plan changed during projection"));
        }
        oms_command_plan::json::canonical(&status)
    }
    /// The plan crate's own `ensure-lineage` action: idempotent, writes only
    /// under the plan lock, and refuses a missing or malformed plan.
    fn ensure_plan_lineage(&self, repo: &Path, state: &Path) -> oms_command_plan::Result<()> {
        let open = self.plan(repo, state)?;
        let response = open.execute(&oms_command_plan::Request::new(
            oms_command_plan::Action::EnsureLineage,
        ))?;
        if response.code == 0 {
            Ok(())
        } else {
            Err(plan_error(
                String::from_utf8_lossy(&response.stderr).trim().to_owned(),
            ))
        }
    }
    /// Projects the ledger through `fail-ledger list --unresolved --json`, and
    /// only when that command reads the very bytes the runtime captured.
    fn unresolved_failures(
        &self,
        repo: &Path,
        state: &Path,
        ledger: &[u8],
    ) -> oms_command_plan::Result<Vec<u8>> {
        if std::env::var_os("OMS_FAIL_LEDGER").is_some_and(|v| !v.is_empty()) {
            return Err(plan_error(
                "OMS_FAIL_LEDGER redirects the failure ledger; refusing a projection of unverified bytes",
            ));
        }
        let own = std::fs::canonicalize(repo)
            .map_err(|_| plan_error("cannot resolve physical repository"))?;
        let shared = std::fs::canonicalize(state)
            .map_err(|_| plan_error("cannot resolve physical state repository"))?;
        let configured = std::env::var_os("OMS_STATE_REPO")
            .filter(|v| !v.is_empty())
            .map(|v| std::fs::canonicalize(v).ok());
        // The command stores in its own state root (OMS_STATE_REPO, else the
        // repo) and reads Git HEAD from its cwd repo: both must be the roots
        // the runtime selected, or it would project a different ledger.
        let consistent = match configured {
            Some(root) => root.as_deref() == Some(shared.as_path()),
            None => own == shared,
        };
        if !consistent {
            return Err(plan_error(
                "failure projection needs OMS_STATE_REPO to name the shared state root",
            ));
        }
        let store = self.group_store(&shared)?;
        let current = || store.optional(FAILURE_PATH, oms_runtime::storage::MAX_STREAM);
        if current()?.as_deref() != Some(ledger) {
            return Err(plan_error("failure ledger changed during projection"));
        }
        let (code, stdout, stderr) = oms_command_state::run_with_source(
            "fail-ledger",
            &["list".into(), "--unresolved".into(), "--json".into()],
            &own,
            &self.source,
            self.cancel(),
        );
        if code != 0 {
            return Err(plan_error(format!(
                "failure ledger projection failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        if current()?.as_deref() != Some(ledger) {
            return Err(plan_error("failure ledger changed during projection"));
        }
        Ok(stdout)
    }
    /// Apptainer probes local inputs only; host namespaces remain an execution
    /// requirement. Existing isolated/remote producers remain unavailable.
    fn backend_check(
        &self,
        profile: &str,
        image: &OsStr,
        adapter: &OsStr,
    ) -> oms_command_plan::Result<Value> {
        if profile == "apptainer" {
            if !adapter.is_empty() {
                return Err(plan_error("--adapter does not apply to Apptainer"));
            }
            return apptainer::readiness(self, image);
        }
        let missing: &[&str] = match profile {
            "trusted-local" => &[],
            "isolated" => &["native container supervisor"],
            "remote" => &["native remote adapter supervisor"],
            _ => {
                return Err(plan_error(format!(
                    "unsupported execution profile: {profile}"
                )));
            }
        };
        Ok(rvalue::object([
            ("ready", rvalue::b(missing.is_empty())),
            ("missing", rvalue::a(missing.iter().map(rvalue::string))),
            ("details", rvalue::empty()),
        ]))
    }
    /// Trusted-local and explicit Apptainer use the same process owner. The leader is
    /// reaped and its whole tree cleaned by the supervisor; the log keeps head
    /// and tail within `log_limit`; log then receipt are created exclusively.
    fn supervise(
        &self,
        repo: &Path,
        request: &BackendRequest,
    ) -> oms_command_plan::Result<BackendResult> {
        if !["trusted-local", "apptainer"].contains(&request.profile.as_str()) {
            return Err(unavailable(&format!(
                "{} execution isolation backend",
                request.profile
            )));
        }
        if request.argv.is_empty() || request.wall.is_zero() {
            return Err(plan_error(
                "execution requires a command and a positive wall clock",
            ));
        }
        if !(1024..=64 * 1024 * 1024).contains(&request.log_limit) {
            return Err(plan_error(
                "log limit must be between 1024 and 67108864 bytes",
            ));
        }
        if self.cancel().load(Ordering::Acquire) {
            return Err(oms_command_plan::Error {
                code: 130,
                message: "interrupted".into(),
            });
        }
        let cwd = std::fs::symlink_metadata(&request.cwd)
            .ok()
            .filter(|m| m.is_dir() && !m.file_type().is_symlink())
            .and_then(|_| std::fs::canonicalize(&request.cwd).ok())
            .ok_or_else(|| plan_error("execution directory must be a real directory"))?;
        let mut container = if request.profile == "apptainer" {
            Some(apptainer::prepare(self, repo, request)?)
        } else {
            None
        };
        let operation = format!("exec-{}", oms_command_plan::storage::random_hex(16)?);
        let log_target = request
            .log
            .clone()
            .unwrap_or_else(|| format!(".oms/runtime/logs/{operation}.log").into());
        let receipt_target = request
            .receipt
            .clone()
            .unwrap_or_else(|| format!(".oms/runtime/receipts/{operation}.json").into());
        // Refuse unwritable/unsafe outputs and hold canonical external locks
        // before running the child so no command side effects occur without a record.
        let (log_output, receipt_output) =
            self.prepare_outputs(repo, &log_target, &receipt_target)?;
        let mut environment = Vec::new();
        let mut names: Vec<OsString> = RUN_ENV
            .iter()
            .map(OsString::from)
            .chain(request.inherit_env.iter().cloned())
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        names.retain(|n| seen.insert(n.clone()));
        if container.is_some() {
            names.clear();
        }
        for name in names {
            if let Some(value) = std::env::var_os(&name) {
                let pair = format!("{}={}", name.to_string_lossy(), value.to_string_lossy());
                if oms_runtime::capsule::sensitive_value(&pair) {
                    return Err(plan_error(format!(
                        "refusing sensitive-looking environment value: {}",
                        name.to_string_lossy()
                    )));
                }
                environment.push((name, value));
            }
        }
        environment.push(("OMS_RUNTIME_BACKEND".into(), "1".into()));
        let started_at = utc_stamp(now_secs());
        let argv = container
            .as_ref()
            .map_or_else(|| request.argv.clone(), |launch| launch.argv.clone());
        if let Some(launch) = &container {
            launch.image.check()?;
            environment = launch.environment.clone();
        }
        let mut spec = ChildSpec::new(argv, request.wall, request.log_limit);
        spec.cwd = Some(&cwd);
        spec.merged = true;
        spec.drain = true;
        spec.clear_environment = true;
        spec.environment = environment;
        let (mut code, mut raw, dropped, elapsed, timed_out) =
            match run_child(self.scope, spec, self.cancel()) {
                Ok(out) => (
                    out.code,
                    out.stdout,
                    out.dropped,
                    out.elapsed,
                    out.timed_out,
                ),
                Err(e) if e.starts_with(LAUNCH_FAILED) => (
                    127,
                    format!("runtime launch failed: {}\n", &e[LAUNCH_FAILED.len()..]).into_bytes(),
                    0,
                    Duration::ZERO,
                    false,
                ),
                Err(e) => return Err(plan_error(e)),
            };
        raw.truncate(request.log_limit);
        if code < 0 {
            code = 125;
        }
        let launcher_exit = code;
        if let Some(launch) = &mut container {
            match launch.image.verify_after() {
                Ok(()) => launch.details.set("image_stability", rvalue::string("same observed bytes before and after execution; concurrent open is not pinned"))?,
                Err(error) => {
                    code = 125;
                    launch.details.set("image_stability", rvalue::string("changed-or-unverified; execution is not accepted"))?;
                    launch.details.set("image_error", rvalue::string(error.message))?;
                }
            }
        }
        log_output.publish(&raw)?;
        let description = oms_runtime::backend::describe(&request.profile)?;
        let relative = |target: &Path| -> String {
            let absolute = if target.is_absolute() {
                target.to_owned()
            } else {
                repo.join(target)
            };
            absolute
                .strip_prefix(repo)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| {
                    absolute
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
        };
        let argv_value = rvalue::a(
            request
                .argv
                .iter()
                .map(|s| rvalue::string(s.to_string_lossy())),
        );
        let mut receipt = rvalue::object([
            ("schema", rvalue::number(1)),
            ("operation_id", rvalue::string(&operation)),
            ("profile", rvalue::string(&request.profile)),
            ("started_at", rvalue::string(started_at)),
            ("finished_at", rvalue::string(utc_stamp(now_secs()))),
            (
                "duration_seconds",
                rvalue::number(format!("{:.3}", elapsed.as_secs_f64())),
            ),
            ("exit", rvalue::number(code)),
            ("timed_out", rvalue::b(timed_out)),
            (
                "command_digest",
                rvalue::string(sha_hex(&oms_command_plan::json::canonical(&argv_value)?)),
            ),
            ("output_sha256", rvalue::string(sha_hex(&raw))),
            ("output_bytes", rvalue::number(raw.len())),
            ("output_omitted_bytes", rvalue::number(dropped)),
            ("log", rvalue::string(relative(&log_target))),
            ("declared_capabilities", description.get("declared").clone()),
            ("enforced_capabilities", description.get("enforced").clone()),
            ("unknown_capabilities", description.get("unknown").clone()),
            ("network_requested", rvalue::b(request.allow_network)),
            (
                "resource_limits",
                rvalue::object([
                    ("memory_mb", rvalue::number(request.memory_mb)),
                    ("cpus", rvalue::number(format!("{:?}", request.cpus))),
                    ("pids", rvalue::number(request.pids)),
                    ("tmpfs_mb", rvalue::number(request.tmpfs_mb)),
                    ("enforced", rvalue::b(false)),
                    ("attested_only", rvalue::b(false)),
                ]),
            ),
            ("adapter_attestation", Value::Null),
            ("remote_response_valid", Value::Null),
            ("remote_accepted", Value::Null),
            ("remote_request_digest", Value::Null),
            ("adapter_attestation_valid", Value::Null),
            (
                "sensitive_output_detected",
                rvalue::b(oms_runtime::capsule::sensitive_value(
                    &String::from_utf8_lossy(&raw),
                )),
            ),
        ]);
        if let Some(launch) = container {
            receipt.set("launcher_exit", rvalue::number(launcher_exit))?;
            let mut limits = receipt.get("resource_limits").clone();
            limits.set("enforced", Value::Null)?;
            limits.set(
                "requested_via_apptainer",
                rvalue::list(["memory_mb", "cpus", "pids"]),
            )?;
            limits.set("tmpfs", rvalue::string("unsupported; not enforced"))?;
            receipt.set("resource_limits", limits)?;
            receipt.set("apptainer", launch.details)?;
        }
        let mut bytes = rvalue::sorted_pretty(&receipt)?;
        bytes.push(b'\n');
        receipt_output.publish(&bytes)?;
        receipt.set("receipt", rvalue::string(relative(&receipt_target)))?;
        Ok(BackendResult {
            receipt,
            exit: code,
        })
    }
    /// `oms install-profile`, the lifecycle crate's native selective installer.
    fn install_profiles(
        &self,
        repo: &Path,
        request: &InstallerRequest,
    ) -> oms_command_plan::Result<ExternalOutput> {
        let bad = |s: &str| s.is_empty() || s.starts_with('-') || s.contains('\0');
        if request.profiles.iter().any(|p| bad(p))
            || request.primary_provider.as_deref().is_some_and(bad)
        {
            return Err(plan_error("invalid profile installer argument"));
        }
        let mut args: Vec<OsString> = Vec::new();
        for name in &request.profiles {
            args.extend(["--profile".into(), name.into()]);
        }
        if let Some(provider) = &request.primary_provider {
            args.extend(["--primary-provider".into(), provider.into()]);
        }
        if request.upgrade {
            args.push("--upgrade".into());
        }
        if request.allow_missing {
            args.push("--allow-missing".into());
        }
        if let Some(receipt) = &request.receipt {
            args.extend(["--receipt".into(), repo.join(receipt).into_os_string()]);
        }
        if request.dry_run {
            args.push("--plan".into());
        }
        let (exit, stdout, stderr) = oms_command_lifecycle::run_with_source(
            "install-profile",
            &args,
            repo,
            &self.source,
            self.cancel(),
        );
        Ok(ExternalOutput {
            exit,
            stdout,
            stderr,
        })
    }
    /// The lifecycle crate's transactional `update --ref`, only for a commit
    /// that is already present locally.
    fn apply_release(
        &self,
        root: &Path,
        request: &ReleaseRequest,
    ) -> oms_command_plan::Result<ExternalOutput> {
        if !["stable", "edge"].contains(&request.channel.as_str()) || !is_hex_oid(&request.target) {
            return Err(plan_error(
                "release request needs a channel and full commit id",
            ));
        }
        if oms_runtime::Services::resolve_revision(self, root, &request.target)?.as_deref()
            != Some(request.target.as_str())
        {
            return Err(plan_error("release target is not available locally"));
        }
        let mut args: Vec<OsString> = vec!["--ref".into(), request.target.clone().into()];
        if request.no_tools {
            args.push("--no-tools".into());
        }
        let (exit, stdout, stderr) =
            oms_command_lifecycle::run_with_source("update", &args, root, root, self.cancel());
        Ok(ExternalOutput {
            exit,
            stdout,
            stderr,
        })
    }
    /// Stable promotion: commit present, tracked tree clean, reviewed manifest
    /// digest equal to the bytes on disk, then one compare-and-set publication
    /// (`Store::mutate`: no-follow, locked, fsync) of the new manifest.
    fn promote_release(
        &self,
        root: &Path,
        request: &PromotionRequest,
    ) -> oms_command_plan::Result<Value> {
        let semver = |v: &str| {
            let (core, pre) = v
                .split_once(['-', '+'])
                .map_or((v, None), |(c, p)| (c, Some(p)));
            let parts: Vec<&str> = core.split('.').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                && pre.is_none_or(|p| {
                    !p.is_empty()
                        && p.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                })
        };
        if !is_hex_oid(&request.commit) || !semver(&request.version) {
            return Err(plan_error(
                "stable promotion needs a full lowercase commit id and a semantic version",
            ));
        }
        if !oms_command_plan::json::hex(&request.expected_manifest_digest, 64) {
            return Err(plan_error(
                "stable promotion requires the exact reviewed manifest digest",
            ));
        }
        if oms_runtime::Services::resolve_revision(self, root, &request.commit)?.as_deref()
            != Some(request.commit.as_str())
        {
            return Err(plan_error(format!(
                "stable commit {} is not available in {}; update this install first",
                &request.commit[..12],
                root.display()
            )));
        }
        let dirty = self.git_text(root, &["status", "--porcelain=v1", "--untracked-files=no"])?;
        if dirty.is_none_or(|d| !d.is_empty()) {
            return Err(oms_command_plan::Error {
                code: 3,
                message: "stable promotion requires a clean tracked worktree".into(),
            });
        }
        let digest = |value: &Value| oms_command_plan::json::hash(value);
        let store = self.group_store(root)?;
        let manifest_lock = self.canonical_lock(&store.repo().join(MANIFEST_PATH))?;
        let on_disk = store.optional(MANIFEST_PATH, MANIFEST_LIMIT)?;
        let mut manifest = match &on_disk {
            Some(raw) => oms_command_plan::json::parse(raw)?,
            None => request.manifest.clone(),
        };
        let stale = || oms_command_plan::Error {
            code: 75,
            message: "update channel manifest changed since review".into(),
        };
        let previous = digest(&manifest)?;
        if previous != request.expected_manifest_digest || digest(&request.manifest)? != previous {
            return Err(stale());
        }
        manifest.set("channels", {
            let mut channels = manifest.get("channels").clone();
            channels.set(
                "stable",
                rvalue::object([
                    ("version", rvalue::string(&request.version)),
                    ("ref", rvalue::string(&request.commit)),
                    ("auto_apply", rvalue::b(false)),
                    (
                        "policy",
                        rvalue::string(
                            "pinned reviewed commit; promotion requires exact manifest digest",
                        ),
                    ),
                    ("promoted_at", rvalue::string(utc_stamp(now_secs()))),
                ]),
            )?;
            channels
        })?;
        let mut bytes = rvalue::sorted_pretty(&manifest)?;
        bytes.push(b'\n');
        manifest_lock.check().map_err(|e| oms_command_plan::Error {
            code: 75,
            message: format!("update channel manifest lock authority uncertain: {e}"),
        })?;
        // A missing manifest reads as empty bytes inside the mutation, so a
        // racing first promotion fails the same compare-and-set under the lock.
        store.mutate(
            MANIFEST_PATH,
            Some(on_disk.as_deref().unwrap_or_default()),
            &bytes,
            MANIFEST_LIMIT,
            on_disk.is_none(),
        )?;
        manifest_lock.check().map_err(|e| oms_command_plan::Error {
            code: 75,
            message: format!("update channel manifest lock authority uncertain: {e}"),
        })?;
        manifest_lock
            .release()
            .map_err(|e| oms_command_plan::Error {
                code: 75,
                message: format!("update channel manifest lock release uncertain: {e}"),
            })?;
        Ok(rvalue::object([
            ("schema", rvalue::number(1)),
            ("promoted", rvalue::b(true)),
            ("commit", rvalue::string(&request.commit)),
            ("version", rvalue::string(&request.version)),
            ("manifest", rvalue::string(MANIFEST_PATH)),
            ("previous_manifest_digest", rvalue::string(previous)),
            ("new_manifest_digest", rvalue::string(digest(&manifest)?)),
        ]))
    }
    /// Same-path checkout status/sync; see [`RuntimeServices::inspect_target`].
    fn target_transport(
        &self,
        repo: &Path,
        request: &TargetRequest,
    ) -> oms_command_plan::Result<Value> {
        self.inspect_target(repo, request)
    }
    /// Native run-ledger execution (`oms_command_runs::execute`) with this
    /// adapter's journal, memory and run-link services; the operation id is
    /// carried in the child context, never the process environment.
    fn run_ledger(
        &self,
        repo: &Path,
        request: &LedgerRequest,
    ) -> oms_command_plan::Result<LedgerResult> {
        if request.argv.is_empty() || request.wall.is_zero() {
            return Err(plan_error("run ledger needs a command and a positive wall"));
        }
        let mut context = oms_command_runs::Context::new(repo, self.cancel())
            .map_err(|e| plan_error(e.message))?;
        context.command_timeout = Some(request.wall);
        context.env.insert(
            "OMS_OPERATION_ID".into(),
            request.operation_id.clone().into(),
        );
        let research = TempJson::write(&request.research)?;
        if let Some(file) = &research {
            context.env.insert(
                "OMS_RESEARCH_METADATA_FILE".into(),
                file.path().as_os_str().to_owned(),
            );
            context
                .env
                .insert("OMS_WORK_JOURNAL_EVENT_TYPE".into(), "experiment".into());
        }
        let mut args: Vec<OsString> = vec!["--note".into(), request.note.clone().into()];
        for (flag, path) in [("--file", &request.file), ("--metrics", &request.metrics)] {
            if let Some(path) = path.as_ref().filter(|p| !p.as_os_str().is_empty()) {
                args.extend([flag.into(), path.as_os_str().to_owned()]);
            }
        }
        if request.no_gate {
            args.extend([
                "--no-gate".into(),
                "--reason".into(),
                request.reason.clone().into(),
            ]);
        }
        args.push("--".into());
        args.extend(request.argv.iter().cloned());
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let mut sink = RunsAdapter(self);
        let record =
            oms_command_runs::execute(&args, &context, &mut sink, &mut stdout, &mut stderr)
                .map_err(|e| oms_command_plan::Error {
                    code: e.code,
                    message: e.message,
                })?;
        let ledger_id = match &record.row {
            oms_command_runs::Json::Object(items) => items
                .iter()
                .rev()
                .find(|(k, _)| k == b"id")
                .and_then(|(_, v)| match v {
                    oms_command_runs::Json::String(s) => String::from_utf8(s.clone()).ok(),
                    oms_command_runs::Json::Number(n) => Some(n.clone()),
                    _ => None,
                })
                .unwrap_or_default(),
            _ => String::new(),
        };
        Ok(LedgerResult {
            output: ExternalOutput {
                exit: record.code,
                stdout,
                stderr,
            },
            ledger_id,
        })
    }
    /// Sums the reconciled usage rows of this experiment's ledgered jobs. The
    /// run-usage producer anchors its reads at the process working directory,
    /// which this adapter must not change, so another cwd reports "not counted".
    fn experiment_gpu_usage(&self, repo: &Path, id: &str) -> oms_command_plan::Result<Value> {
        let uncounted = |why: &str| {
            rvalue::object([
                ("used", Value::Null),
                ("note", rvalue::string(format!("not counted: {why}"))),
            ])
        };
        if id.is_empty() {
            return Ok(uncounted(
                "no experiment id to read run-reconcile usage with",
            ));
        }
        let own = std::fs::canonicalize(repo)
            .map_err(|_| plan_error("cannot resolve physical repository"))?;
        if std::env::current_dir()
            .and_then(std::fs::canonicalize)
            .ok()
            .as_deref()
            != Some(own.as_path())
        {
            return Ok(uncounted(
                "the process working directory is not the experiment repository",
            ));
        }
        let ledger = own.join("docs/EXPERIMENTS.jsonl");
        let reconcile = std::env::var_os("OMS_RECONCILE_FILE")
            .filter(|v| !v.is_empty())
            .map_or_else(|| own.join(".oms/runs/reconcile.jsonl"), PathBuf::from);
        if !ledger.exists() || !reconcile.exists() {
            return Ok(uncounted(
                "no ledgered run or reconciled job exists for this experiment",
            ));
        }
        let jobs = oms_command_runs::usage::experiment_jobs(&ledger, id)
            .map_err(|e| plan_error(e.message))?;
        let report = rvalue::from_native(
            &oms_command_runs::usage::summary(&reconcile, Some(&jobs))
                .map_err(|e| plan_error(e.message))?,
        );
        let mut out = rvalue::object([("used", Value::Null)]);
        out.set("jobs_reconciled", report.get("jobs").clone())?;
        out.set("jobs_with_gpu_hours", report.get("gpu_hours_jobs").clone())?;
        if matches!(report.get("gpu_hours"), Value::Null) {
            out.set(
                "note",
                rvalue::string(format!(
                    "not counted: no reconciled job with allocated GPU-hours is ledgered for experiment {id}"
                )),
            )?;
        } else {
            out.set("used", report.get("gpu_hours").clone())?;
            out.set(
                "note",
                rvalue::string(
                    "sum of run-reconcile usage over the reconciled jobs of this experiment; reported, not enforced",
                ),
            )?;
        }
        Ok(out)
    }
}

/// Remote half of the same-path checkout transport, sent as data to
/// `ssh ... sh -s`. All caller values arrive as shell-quoted positional
/// arguments, never as script text; the reply is NUL-delimited after a sentinel.
const REMOTE_SENTINEL: &str = "\nOMS_TARGET_V1\n";
const REMOTE_REASONS: &[&str] = &[
    "missing",
    "not-git",
    "dirty",
    "diverged",
    "behind",
    "foreign-origin",
    "wrong-branch",
    "unpushed",
    "auth",
    "no-network",
    "would-overwrite-ignored",
    "would-overwrite-untracked",
    "legacy-absolute",
    "timeout",
];
const REMOTE_SCRIPT: &str = r#"export GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0 GIT_LITERAL_PATHSPECS=1 LC_ALL=C
# Git and its hooks must not consume subsequent commands from sh -s input.
git() { command git "$@" </dev/null; }
limited_git() {
    if command -v timeout >/dev/null 2>&1; then
        command timeout "$seconds" git "$@" </dev/null
    else
        git "$@"
    fi
}
mode=$1
workdir=$HOME/${2#\~/}
expected=$3
origin=$4
sha=$5
wanted=$6
seconds=$7
reason=
exists=false
is_git=false
matches=false
head=
branch=
clean=false
untracked=false
before=
after=
finish() {
    printf '\nOMS_TARGET_V1\n'
    printf '%s\000' "$reason" "$workdir" "$exists" "$is_git" "$matches" "$head" "$branch" "$clean" "$untracked" "$before" "$after"
    if [ -n "$reason" ]; then exit 3; fi
    exit 0
}
fail() { reason=$1; finish; }
network() {
    message=$("$@" 2>&1) && return 0
    case "$?" in 124|137) fail timeout ;; esac
    case "$message" in
        *'Permission denied'*|*'Authentication'*|*'authentication'*|*'could not read Username'*|*'Host key verification'*|*'Repository not found'*|*'Access denied'*) fail auth ;;
        *'find remote ref'*|*'Remote branch'*'not found'*) fail unpushed ;;
        *) fail no-network ;;
    esac
}
identity() {
    case "$1" in
        git@*:*) host=${1#git@}; path=${host#*:}; host=${host%%:*} ;;
        ssh://git@*/*) host=${1#ssh://git@}; path=${host#*/}; host=${host%%/*} ;;
        ssh://*/*) host=${1#ssh://}; path=${host#*/}; host=${host%%/*} ;;
        https://*/*) host=${1#https://}; path=${host#*/}; host=${host%%/*} ;;
        *) return 1 ;;
    esac
    host=$(printf '%s' "$host" | tr '[:upper:]' '[:lower:]')
    path=${path%/}
    case "$path" in */*) ;; *) return 1 ;; esac
    owner=${path%%/*}
    project=${path#*/}
    if [ "$host" = github.com ]; then project=$(printf '%s' "$project" | tr '[:upper:]' '[:lower:]'); fi
    project=${project%.git}
    case "$host" in ''|*[!a-z0-9._-]*) return 1 ;; esac
    case "$owner" in ''|.|..|*[!a-zA-Z0-9._-]*) return 1 ;; esac
    case "$project" in ''|.|..|*[!a-zA-Z0-9._-]*) return 1 ;; esac
    if [ "$host" = github.com ]; then
        printf '%s/%s/%s' "$host" "$owner" "$project" | tr '[:upper:]' '[:lower:]'
    else
        printf '%s/%s/%s' "$host" "$owner" "$project"
    fi
}
fresh=false
case "$HOME" in /*) ;; *) fail not-git ;; esac
if [ ! -e "$workdir" ] && [ ! -L "$workdir" ]; then
    [ "$mode" = sync ] || fail missing
    mkdir -p -- "${workdir%/*}" || fail not-git
    network limited_git clone --branch "$wanted" -- "$origin" "$workdir"
    fresh=true
fi
exists=true
cd -- "$workdir" 2>/dev/null || fail not-git
workdir=$(pwd -P)
[ "$(git rev-parse --is-inside-work-tree 2>/dev/null)" = true ] || fail not-git
# A directory nested in another checkout is not the registered checkout.
[ "$(git rev-parse --show-prefix 2>/dev/null)" = '' ] || fail not-git
is_git=true
head=$(git rev-parse --verify HEAD 2>/dev/null) || fail not-git
branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null) || branch=
if git diff --quiet --ignore-submodules=none -- && git diff --cached --quiet --ignore-submodules=none --; then clean=true; fi
[ -z "$(git ls-files --others --exclude-standard)" ] || untracked=true
before=$head
[ "$fresh" = false ] || before=
actual=$(git config --get remote.origin.url 2>/dev/null)
actual=$(identity "$actual") || fail foreign-origin
[ "$actual" = "$expected" ] || fail foreign-origin
matches=true
if [ "$mode" = status ]; then finish; fi
[ "$clean" = true ] || fail dirty
[ "$branch" = "$wanted" ] || fail wrong-branch
network limited_git fetch origin "+refs/heads/$wanted:refs/remotes/origin/$wanted"
git merge-base --is-ancestor "$sha" "refs/remotes/origin/$wanted" 2>/dev/null || fail unpushed
if ! git merge-base --is-ancestor "$head" "$sha"; then
    if git merge-base --is-ancestor "$sha" "$head"; then fail behind; fi
    fail diverged
fi
# The local target-tree check excludes newline filenames before SSH. Preserve
# all other filename bytes while converting NUL separators for POSIX read.
collision() {
    if [ -n "$(git ls-files --others --exclude-standard -- "$1")" ]; then exit 11; fi
    exit 10
}
(
git diff --no-renames --name-only --diff-filter=A -z "$head" "$sha" -- |
    tr '\000' '\n' |
    while IFS= read -r path; do
        if [ -e "$path" ] || [ -L "$path" ]; then
            if git cat-file -e "$head:$path" >/dev/null 2>&1; then
                [ -z "$(git ls-files --others -- "$path")" ] || collision "$path"
            else
                collision "$path"
            fi
        fi
        while :; do
            case "$path" in */*) path=${path%/*} ;; *) break ;; esac
            if [ -L "$path" ] || { [ -e "$path" ] && [ ! -d "$path" ]; }; then
                git cat-file -e "$head:$path" >/dev/null 2>&1 || collision "$path"
            fi
        done
    done
)
collision_status=$?
case "$collision_status" in 10) fail would-overwrite-ignored ;; 11) fail would-overwrite-untracked ;; esac
limited_git merge --ff-only "$sha" >/dev/null 2>&1 || {
    case "$?" in 124|137) fail timeout ;; *) fail diverged ;; esac
}
head=$(git rev-parse --verify HEAD) || fail not-git
[ "$head" = "$sha" ] || fail diverged
after=$head
finish
"#;

fn is_target_name(value: &str) -> bool {
    let b = value.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn safe_part(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Repository identity of an SSH or HTTPS origin without credentials, so two
/// spellings of one remote compare equal; `None` for anything else.
fn origin_identity(origin: &str) -> Option<String> {
    if origin.bytes().any(|c| c < 33 || c == 127) {
        return None;
    }
    let (host, owner, project) = if let Some(rest) = origin.strip_prefix("git@") {
        let (host, tail) = rest.split_once(':')?;
        let (owner, project) = tail.split_once('/')?;
        if host.is_empty()
            || host.contains('/')
            || owner.is_empty()
            || project.is_empty()
            || project.contains('/')
        {
            return None;
        }
        (host.to_owned(), owner.to_owned(), project.to_owned())
    } else {
        let (scheme, rest) = origin.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "ssh" | "https") || rest.contains(['?', '#']) {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = match authority.rsplit_once('@') {
            Some((user, host)) if scheme == "ssh" && user == "git" => host,
            Some(_) => return None,
            None => authority,
        };
        if host.is_empty() || host.contains([':', '@', '[', ']']) {
            return None;
        }
        let path = path.strip_suffix('/').unwrap_or(path);
        let (owner, project) = path.split_once('/')?;
        if owner.is_empty() || project.is_empty() || project.contains('/') {
            return None;
        }
        (
            host.to_ascii_lowercase(),
            owner.to_owned(),
            project.to_owned(),
        )
    };
    let host = host.to_ascii_lowercase();
    let github = host == "github.com";
    let project = if github {
        project.to_ascii_lowercase()
    } else {
        project
    };
    let project = project.strip_suffix(".git").unwrap_or(&project).to_owned();
    if !safe_part(&host) || !safe_part(&owner) || !safe_part(&project) {
        return None;
    }
    let identity = format!("{host}/{owner}/{project}");
    Some(if github {
        identity.to_ascii_lowercase()
    } else {
        identity
    })
}

fn network_reason(stderr: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    const AUTH: [&str; 7] = [
        "permission denied",
        "authentication",
        "could not read username",
        "host key",
        "remote host identification",
        "repository not found",
        "access denied",
    ];
    if AUTH.iter().any(|w| text.contains(w)) {
        "auth"
    } else {
        "no-network"
    }
}

/// The remote reply as its 11 fields, or `None` unless it is exactly the
/// sentinel-framed, NUL-delimited, self-consistent shape the script emits:
/// exit 0 iff no reason, known reasons only, booleans, an absolute workdir and
/// empty-or-full object ids.
fn parse_remote_reply(code: i32, stdout: &str) -> Option<[&str; 11]> {
    let (_, payload) = stdout.rsplit_once(REMOTE_SENTINEL)?;
    let fields: Vec<&str> = payload.split('\0').collect();
    if fields.len() != 12 || !fields[11].is_empty() || !matches!(code, 0 | 3) {
        return None;
    }
    let fields: [&str; 11] = fields[..11].try_into().ok()?;
    let [
        reason,
        workdir,
        exists,
        is_git,
        matches,
        head,
        _,
        clean,
        untracked,
        before,
        after,
    ] = fields;
    let truthy = |s: &str| matches!(s, "true" | "false");
    let oid = |s: &str| s.is_empty() || is_hex_oid(&s.to_ascii_lowercase());
    (((reason.is_empty() || REMOTE_REASONS.contains(&reason)) && (code == 0) == reason.is_empty())
        && [exists, is_git, matches, clean, untracked]
            .into_iter()
            .all(truthy)
        && workdir.starts_with('/')
        && [head, before, after].into_iter().all(oid))
    .then_some(fields)
}

/// POSIX shell quoting of one positional value (shlex.quote).
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"@%+=:,./-_".contains(&c))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

impl RuntimeServices<'_> {
    /// Observe or advance one registered same-path checkout over ssh without a
    /// command runner: local Git proves the origin, branch and pushed commit,
    /// then the remote script re-validates path, origin, branch and cleanliness
    /// and fast-forwards only. Every network child is supervised with the
    /// caller's wall clock; a timeout reports `timeout` with unknown state.
    fn inspect_target(
        &self,
        repo: &Path,
        request: &TargetRequest,
    ) -> oms_command_plan::Result<Value> {
        let seconds = request.timeout.as_secs();
        if !(10..=7200).contains(&seconds) || request.timeout.subsec_nanos() != 0 {
            return Err(plan_error("--timeout must be between 10 and 7200 seconds"));
        }
        if !is_target_name(&request.name) || !is_target_name(&request.ssh_alias) {
            return Err(plan_error(
                "target and SSH alias must be 1..64 letters, digits, dots, underscores or hyphens",
            ));
        }
        if request.workdir.is_empty()
            || request.workdir.starts_with('/')
            || request.workdir.bytes().any(|c| c < 32 || c == 127)
        {
            return Err(plan_error(
                "target workdir must be a same-path relative path",
            ));
        }
        let mut result = rvalue::object([
            ("schema", rvalue::number(1)),
            ("target", rvalue::string(&request.name)),
            ("ok", rvalue::b(false)),
            ("reason", rvalue::string("missing")),
        ]);
        if request.sync {
            result.set("before_sha", Value::Null)?;
            result.set("after_sha", Value::Null)?;
        }
        let timed_out = |result: &mut Value| -> oms_command_plan::Result<Value> {
            result.set("reason", rvalue::string("timeout"))?;
            result.set("state_unknown", rvalue::b(true))?;
            Ok(result.clone())
        };
        macro_rules! git {
            ($($arg:expr),+) => {{
                let out = self.git_target(repo, &[$($arg),+], request.timeout)?;
                if out.timed_out {
                    return timed_out(&mut result);
                }
                out
            }};
        }
        let origin_out = git!("config", "--get", "remote.origin.url");
        let origin = String::from_utf8_lossy(&origin_out.stdout)
            .trim()
            .trim_end_matches('\r')
            .to_owned();
        let Some(identity) = origin_identity(&origin) else {
            result.set("reason", rvalue::string("foreign-origin"))?;
            return Ok(result);
        };
        let (mut requested, mut branch) = (String::new(), String::new());
        if request.sync {
            let revision = request.sha.as_deref().unwrap_or("HEAD");
            if request
                .sha
                .as_deref()
                .is_some_and(|s| !is_hex_oid(&s.to_ascii_lowercase()))
            {
                return Err(plan_error(
                    "--sha must be a full 40- or 64-character commit SHA",
                ));
            }
            let fetched = git!("fetch", "origin");
            if fetched.code != 0 {
                result.set("reason", rvalue::string(network_reason(&fetched.stderr)))?;
                return Ok(result);
            }
            let spec = format!("{revision}^{{commit}}");
            let commit = git!("rev-parse", "--verify", &spec);
            if commit.code != 0 {
                result.set("reason", rvalue::string("unpushed"))?;
                result.set(
                    "message",
                    rvalue::string("push first: requested commit is unavailable"),
                )?;
                return Ok(result);
            }
            requested = String::from_utf8_lossy(&commit.stdout)
                .trim()
                .trim_end_matches('\r')
                .to_owned();
            let head = git!("symbolic-ref", "--quiet", "--short", "HEAD");
            branch = String::from_utf8_lossy(&head.stdout)
                .trim()
                .trim_end_matches('\r')
                .to_owned();
            if branch.is_empty() {
                result.set("reason", rvalue::string("wrong-branch"))?;
                return Ok(result);
            }
            let tree = git!("ls-tree", "-r", "-z", "--name-only", &requested);
            if tree.code != 0 {
                return Err(plan_error("cannot inspect requested commit paths"));
            }
            if tree.stdout.contains(&b'\n') {
                return Err(plan_error(
                    "target filenames containing newlines are unsafe for remote sync",
                ));
            }
            let contains = format!("--contains={requested}");
            let reachable = git!(
                "for-each-ref",
                "--format=%(refname)",
                &contains,
                "refs/remotes/origin/"
            );
            if String::from_utf8_lossy(&reachable.stdout).trim().is_empty() {
                result.set("reason", rvalue::string("unpushed"))?;
                result.set(
                    "message",
                    rvalue::string("push first: requested commit is not on origin"),
                )?;
                return Ok(result);
            }
        }
        let values = [
            if request.sync { "sync" } else { "status" },
            &request.workdir,
            &identity,
            &origin,
            &requested,
            &branch,
            &seconds.to_string(),
        ];
        let mut argv: Vec<OsString> = vec![self.ssh.clone()];
        argv.extend(
            [
                "-a",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "BatchMode=yes",
                "--",
            ]
            .iter()
            .map(OsString::from),
        );
        argv.push(request.ssh_alias.clone().into());
        argv.extend(["sh", "-s", "--"].iter().map(OsString::from));
        argv.extend(values.iter().map(|v| OsString::from(shell_quote(v))));
        let mut spec = ChildSpec::new(argv, request.timeout, GIT_LIMIT);
        spec.stdin = REMOTE_SCRIPT.as_bytes().to_vec();
        let remote = match run_child(self.scope, spec, self.cancel()) {
            Ok(out) => out,
            Err(e) if e.starts_with(LAUNCH_FAILED) => {
                result.set("reason", rvalue::string("no-network"))?;
                return Ok(result);
            }
            Err(e) => return Err(plan_error(e)),
        };
        if remote.timed_out {
            return timed_out(&mut result);
        }
        let text = String::from_utf8_lossy(&remote.stdout).into_owned();
        let Some(
            [
                reason,
                workdir,
                exists,
                is_git,
                matches,
                head,
                current,
                clean,
                untracked,
                before,
                after,
            ],
        ) = parse_remote_reply(remote.code, &text)
        else {
            result.set("reason", rvalue::string(network_reason(&remote.stderr)))?;
            return Ok(result);
        };
        let opt = |s: &str| {
            if s.is_empty() {
                Value::Null
            } else {
                rvalue::string(s)
            }
        };
        result.set("ok", rvalue::b(remote.code == 0))?;
        result.set("reason", opt(reason))?;
        result.set("workdir", rvalue::string(workdir))?;
        result.set("exists", rvalue::b(exists == "true"))?;
        result.set("is_git", rvalue::b(is_git == "true"))?;
        result.set("origin_matches", rvalue::b(matches == "true"))?;
        result.set("head", opt(head))?;
        result.set("branch", opt(current))?;
        result.set("clean", rvalue::b(clean == "true"))?;
        result.set("untracked", rvalue::b(untracked == "true"))?;
        if request.sync {
            result.set("before_sha", opt(before))?;
            result.set("after_sha", opt(after))?;
            result.set("requested_branch", rvalue::string(&branch))?;
        }
        match reason {
            "wrong-branch" => result.set(
                "message",
                rvalue::string(format!(
                    "remote branch {} differs from local branch {branch}",
                    if current.is_empty() {
                        "(detached)"
                    } else {
                        current
                    }
                )),
            )?,
            "unpushed" => result.set(
                "message",
                rvalue::string("push first: requested commit is not on the remote origin branch"),
            )?,
            "timeout" => result.set("state_unknown", rvalue::b(true))?,
            _ => {}
        }
        Ok(result)
    }
    /// Local Git for target checks: never prompts for credentials.
    fn git_target(
        &self,
        repo: &Path,
        args: &[&str],
        wall: Duration,
    ) -> oms_command_plan::Result<ChildOut> {
        self.git_env(
            repo,
            args,
            wall,
            &[("SSH_ASKPASS_REQUIRE", "never"), ("GIT_ASKPASS", "false")],
        )
    }
}

const TASK_PATH: &str = ".oms/task/current.md";
const FAILURE_PATH: &str = ".oms/failures.jsonl";
const RUN_ENV: &[&str] = &[
    "PATH", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "TERM", "USER", "SHELL", "HOME",
];

pub(crate) fn resolve_lock_settings() -> Result<oms_lock::Settings, (i32, String)> {
    match oms_lock::Settings::from_env() {
        Ok(s) => Ok(s),
        Err(oms_lock::Error::InvalidSettings) => {
            let get = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
            let uid = users::get_current_uid();
            let home = get("HOME")
                .map(PathBuf::from)
                .or_else(|| users::get_user_by_uid(uid).map(|user| user.home_dir().to_path_buf()));
            let Some(home) = home.filter(|p| p.is_absolute()) else {
                return Err((2, "invalid lock settings".into()));
            };
            let temp_dir = get("TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            let cwd = std::env::current_dir()
                .map_err(|_| (2, "cannot resolve current directory for lock".into()))?;
            Ok(oms_lock::Settings {
                mode: oms_lock::Mode::LegacyAuto,
                timeout: Duration::from_secs(oms_lock::legacy_seconds(
                    get("OMS_LOCK_TIMEOUT").as_deref(),
                    300,
                )),
                poll_max: Duration::from_secs(oms_lock::legacy_seconds(
                    get("OMS_POLL_MAX_SECONDS").as_deref(),
                    10,
                )),
                lock_dir: get("OMS_LOCK_DIR").map(PathBuf::from),
                home,
                temp_dir,
                cwd,
                search_path: get("PATH").unwrap_or_default(),
                force_mkdir: get("OMS_LOCK_FORCE_MKDIR").as_deref()
                    == Some(std::ffi::OsStr::new("1")),
            })
        }
        Err(e) => Err((e.exit_code(), e.to_string())),
    }
}

impl RuntimeServices<'_> {
    fn plan(
        &self,
        repo: &Path,
        state: &Path,
    ) -> oms_command_plan::Result<oms_command_plan::Plan<'_>> {
        let request = oms_command_plan::Request::new(oms_command_plan::Action::Status);
        let mut context = oms_command_plan::Context::from_environment(&request, repo)?;
        context.state_repo = state.to_owned();
        context.plan_file = state.join(".oms/plan/tasks.json");
        oms_command_plan::Plan::open(context, &self.control, self.cancel())
    }
    fn lock_settings(&self) -> oms_command_plan::Result<oms_lock::Settings> {
        #[allow(unused_mut)]
        let mut settings = resolve_lock_settings()
            .map_err(|(code, message)| oms_command_plan::Error { code, message })?;
        #[cfg(test)]
        if let Some(timeout) = self.lock_timeout {
            settings.timeout = timeout;
        }
        Ok(settings)
    }

    fn canonical_lock(&self, target: &Path) -> oms_command_plan::Result<oms_lock::Guard> {
        let settings = self.lock_settings()?;
        let total = settings.timeout;
        let started = Instant::now();
        loop {
            if self.cancel().load(Ordering::Acquire) {
                return Err(oms_command_plan::Error {
                    code: 130,
                    message: "interrupted".into(),
                });
            }
            let remaining = total.saturating_sub(started.elapsed());
            let mut slice = settings.clone();
            slice.timeout = remaining
                .min(Duration::from_secs(1))
                .max(Duration::from_millis(1));
            let lock =
                oms_lock::Lock::for_file(target, slice).map_err(|e| oms_command_plan::Error {
                    code: e.exit_code(),
                    message: e.to_string(),
                })?;
            match lock.acquire(None) {
                Ok(guard) => return Ok(guard),
                Err(oms_lock::Error::Busy) if started.elapsed() < total => continue,
                Err(oms_lock::Error::Busy) => {
                    return Err(oms_command_plan::Error {
                        code: 75,
                        message: format!(
                            "timed out waiting for canonical state lock: {}",
                            target.display()
                        ),
                    });
                }
                Err(e) => {
                    return Err(oms_command_plan::Error {
                        code: e.exit_code(),
                        message: e.to_string(),
                    });
                }
            }
        }
    }

    /// Resolve an explicit or repo-relative output path to its physical anchor and
    /// canonical target before lock acquisition and preflight checks.
    fn resolve_output_spec(
        repo: &Path,
        repo_phys: &Path,
        target: &Path,
    ) -> oms_command_plan::Result<TargetSpec> {
        use std::path::Component;
        let raw_bytes = target.as_os_str().as_bytes();
        if raw_bytes.is_empty()
            || raw_bytes.contains(&0)
            || raw_bytes.ends_with(b"/")
            || target == Path::new(".")
        {
            return Err(plan_error("output path must name a file"));
        }
        if target
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(plan_error("output path must not contain .."));
        }
        let joined = if target.is_absolute() {
            target.to_owned()
        } else if repo.is_absolute() {
            repo.join(target)
        } else {
            repo_phys.join(target)
        };
        let mut normalized = PathBuf::new();
        for component in joined.components() {
            match component {
                Component::ParentDir => {
                    return Err(plan_error("output path must not contain .."));
                }
                Component::CurDir => {}
                Component::RootDir | Component::Normal(_) | Component::Prefix(_) => {
                    normalized.push(component);
                }
            }
        }
        if normalized.file_name().is_none() || normalized == repo || normalized == repo_phys {
            return Err(plan_error("output path must name a file"));
        }
        let in_repo = normalized
            .strip_prefix(repo)
            .ok()
            .filter(|rel| !rel.as_os_str().is_empty())
            .or_else(|| {
                normalized
                    .strip_prefix(repo_phys)
                    .ok()
                    .filter(|rel| !rel.as_os_str().is_empty())
            });
        let (anchor, relative, create_parent) = match in_repo {
            Some(rel) => (repo_phys.to_owned(), rel.to_owned(), true),
            None => {
                let parent = normalized
                    .parent()
                    .ok_or_else(|| plan_error("output path has no parent"))?;
                let name = normalized
                    .file_name()
                    .ok_or_else(|| plan_error("output path has no file name"))?;
                match std::fs::symlink_metadata(parent) {
                    Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
                    Ok(_) => {
                        return Err(plan_error("output parent directory must not be a symlink"));
                    }
                    Err(_) => return Err(plan_error("output directory must already exist")),
                }
                (parent.to_owned(), PathBuf::from(name), false)
            }
        };
        let root = oms_safe_fs::Root::open(&anchor)
            .map_err(|e| plan_error(format!("cannot open output directory: {e:?}")))?;
        let physical = root.physical_root().join(&relative);
        Ok(TargetSpec {
            root,
            relative,
            physical,
            display: normalized,
            create_parent,
        })
    }

    fn prepare_single_output(
        spec: TargetSpec,
        guard: oms_lock::Guard,
    ) -> oms_command_plan::Result<PreparedOutput> {
        use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
        use std::path::Component;

        guard.check().map_err(|e| oms_command_plan::Error {
            code: 75,
            message: format!("output lock authority uncertain: {e}"),
        })?;
        match std::fs::symlink_metadata(&spec.physical) {
            Ok(_) => {
                return Err(plan_error(format!(
                    "output already exists: {}",
                    spec.display.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(plan_error(format!("cannot inspect output path: {e}"))),
        }
        if !spec.create_parent {
            match std::fs::symlink_metadata(spec.root.physical_root()) {
                Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
                Ok(_) => return Err(plan_error("output parent directory must not be a symlink")),
                Err(_) => return Err(plan_error("output directory must already exist")),
            }
        }
        let dir_flags = OFlags::RDONLY
            | OFlags::DIRECTORY
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::CLOEXEC;
        let mut dir_fd = fs::openat(fs::CWD, spec.root.physical_root(), dir_flags, Mode::empty())
            .map_err(|e| plan_error(format!("cannot open output directory: {e}")))?;
        let mut walked_all = true;
        let parent_components: Vec<OsString> = spec
            .relative
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .filter_map(|c| match c {
                Component::Normal(seg) => Some(seg.to_owned()),
                _ => None,
            })
            .collect();
        for (idx, seg) in parent_components.iter().enumerate() {
            match fs::statat(&dir_fd, seg.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(st) => {
                    if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
                        return Err(plan_error(
                            "output parent component is not a real directory",
                        ));
                    }
                    let next_fd = fs::openat(&dir_fd, seg.as_os_str(), dir_flags, Mode::empty())
                        .map_err(|e| plan_error(format!("cannot open output parent: {e}")))?;
                    let next_stat = fs::fstat(&next_fd)
                        .map_err(|e| plan_error(format!("cannot stat output parent: {e}")))?;
                    if next_stat.st_dev != st.st_dev || next_stat.st_ino != st.st_ino {
                        return Err(plan_error("output parent changed during preflight"));
                    }
                    dir_fd = next_fd;
                }
                Err(rustix::io::Errno::NOENT) if spec.create_parent => {
                    walked_all = false;
                    for later_seg in &parent_components[idx + 1..] {
                        match fs::statat(&dir_fd, later_seg.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
                        {
                            Ok(_) | Err(rustix::io::Errno::NOENT) => {}
                            Err(e) => {
                                return Err(plan_error(format!(
                                    "output parent component is invalid: {e}"
                                )));
                            }
                        }
                    }
                    break;
                }
                Err(_) => return Err(plan_error("output directory is unavailable")),
            }
        }
        let leaf = spec
            .relative
            .file_name()
            .ok_or_else(|| plan_error("output path has no file name"))?;
        if walked_all {
            match fs::statat(&dir_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => {
                    return Err(plan_error(format!(
                        "output already exists: {}",
                        spec.display.display()
                    )));
                }
                Err(rustix::io::Errno::NOENT) => {}
                Err(e) => return Err(plan_error(format!("cannot inspect output leaf: {e}"))),
            }
        }
        // Match oms_safe_fs atomic Temp::create length bounds and prove the deepest
        // existing directory accepts exclusive file creation without leaving artifacts.
        let mut probe = OsString::from(".");
        probe.push(leaf);
        probe.push(format!(
            ".tmp.{}.{}.{}",
            std::process::id(),
            u64::MAX,
            oms_command_plan::storage::random_hex(10)?
        ));
        let probe_fd = fs::openat(
            &dir_fd,
            &probe,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| plan_error(format!("output directory is not writable: {e}")))?;
        drop(probe_fd);
        fs::unlinkat(&dir_fd, &probe, AtFlags::empty())
            .map_err(|e| plan_error(format!("cannot clean output preflight probe: {e}")))?;

        Ok(PreparedOutput {
            guard,
            root: spec.root,
            relative: spec.relative,
            absolute: spec.display,
            create_parent: spec.create_parent,
        })
    }

    /// Lock and preflight both destinations before child execution starts.
    fn prepare_outputs(
        &self,
        repo: &Path,
        log_target: &Path,
        receipt_target: &Path,
    ) -> oms_command_plan::Result<(PreparedOutput, PreparedOutput)> {
        let repo_phys = std::fs::canonicalize(repo)
            .map_err(|e| plan_error(format!("cannot resolve repository path: {e}")))?;
        let log_spec = Self::resolve_output_spec(repo, &repo_phys, log_target)?;
        let receipt_spec = Self::resolve_output_spec(repo, &repo_phys, receipt_target)?;
        if log_spec.physical.starts_with(&receipt_spec.physical)
            || receipt_spec.physical.starts_with(&log_spec.physical)
        {
            return Err(plan_error(
                "log and receipt must be distinct non-nested files",
            ));
        }
        let (log_guard, receipt_guard) = if log_spec.physical <= receipt_spec.physical {
            (
                self.canonical_lock(&log_spec.physical)?,
                self.canonical_lock(&receipt_spec.physical)?,
            )
        } else {
            let receipt_guard = self.canonical_lock(&receipt_spec.physical)?;
            let log_guard = self.canonical_lock(&log_spec.physical)?;
            (log_guard, receipt_guard)
        };
        let log_prepared = Self::prepare_single_output(log_spec, log_guard)?;
        let receipt_prepared = Self::prepare_single_output(receipt_spec, receipt_guard)?;
        Ok((log_prepared, receipt_prepared))
    }
}

struct TargetSpec {
    root: oms_safe_fs::Root,
    relative: PathBuf,
    physical: PathBuf,
    display: PathBuf,
    create_parent: bool,
}

struct PreparedOutput {
    guard: oms_lock::Guard,
    root: oms_safe_fs::Root,
    relative: PathBuf,
    absolute: PathBuf,
    create_parent: bool,
}
impl PreparedOutput {
    /// Exclusive locked no-follow creation of one output file.
    fn publish(self, bytes: &[u8]) -> oms_command_plan::Result<()> {
        self.guard.check().map_err(|e| oms_command_plan::Error {
            code: 75,
            message: format!("output lock authority uncertain: {e}"),
        })?;
        self.root
            .atomic_mutate::<oms_command_plan::Error>(
                &self.relative,
                oms_safe_fs::MutationOptions {
                    read: oms_safe_fs::ByteReadOptions {
                        missing_ok: true,
                        max_bytes: 1,
                    },
                    create_parent: self.create_parent,
                    max_output: bytes.len().max(1),
                    expected_exists: Some(false),
                    ..Default::default()
                },
                |_| Ok(bytes.to_vec()),
            )
            .map(drop)
            .map_err(|e| plan_error(format!("cannot create {}: {e:?}", self.absolute.display())))?;
        self.guard.check().map_err(|e| oms_command_plan::Error {
            code: 75,
            message: format!("output lock authority uncertain: {e}"),
        })?;
        self.guard.release().map_err(|e| oms_command_plan::Error {
            code: 75,
            message: format!("output lock release uncertain: {e}"),
        })
    }
}

/// Read-only task adapter bound to the caller's state root: status needs no
/// reference, journal, memory or failure authority.
struct TaskRead(PathBuf);
impl oms_command_task::Services for TaskRead {
    fn state_repository(&mut self, _: &Path, _: &AtomicBool) -> oms_command_task::Result<PathBuf> {
        Ok(self.0.clone())
    }
    fn begin_reference(
        &mut self,
        _: &Path,
        _: &Path,
    ) -> oms_command_task::Result<Box<dyn oms_command_task::services::ReferenceOperation>> {
        Err(oms_command_task::Error::service("task status reference"))
    }
    fn observe_journal(
        &mut self,
        _: oms_command_task::services::JournalObservation<'_>,
    ) -> oms_command_task::Result<()> {
        Err(oms_command_task::Error::service("task status journal"))
    }
    fn promote_memory(
        &mut self,
        _: oms_command_task::services::MemoryOutcome<'_>,
    ) -> oms_command_task::Result<()> {
        Err(oms_command_task::Error::service("task status memory"))
    }
    fn observe_failure(
        &mut self,
        _: oms_command_task::services::FailureObservation<'_>,
    ) -> oms_command_task::Result<Vec<u8>> {
        Err(oms_command_task::Error::service(
            "task status failure ledger",
        ))
    }
    fn observe_verify(
        &mut self,
        _: &Path,
        _: &[u8],
    ) -> oms_command_task::Result<oms_command_task::services::VerifyObservation> {
        Err(oms_command_task::Error::service("task status verification"))
    }
    fn skill_contracts(&mut self, _: &Path) -> oms_command_task::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Err(oms_command_task::Error::service(
            "task status skill contracts",
        ))
    }
}

/// Private 0700 directory holding one 0600 create-new JSON file for a child
/// that reads it by path; both file and directory are removed on drop.
struct TempJson {
    dir: PathBuf,
    file: PathBuf,
}
impl TempJson {
    fn write(value: &Value) -> oms_command_plan::Result<Option<Self>> {
        if matches!(value, Value::Null) || value.object().is_ok_and(<[_]>::is_empty) {
            return Ok(None);
        }
        let encoded = oms_command_plan::json::canonical(value)?;
        Self::write_bytes(&encoded).map(Some)
    }
    fn write_bytes(bytes: &[u8]) -> oms_command_plan::Result<Self> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let dir = std::env::temp_dir().join(format!(
            "oms-tmp-{}-{}",
            std::process::id(),
            oms_command_plan::storage::random_hex(8)?
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| plan_error(format!("cannot create temporary metadata directory: {e}")))?;
        let file_path = dir.join("payload.json");
        let guard = Self {
            dir,
            file: file_path.clone(),
        };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file_path)
            .map_err(|e| plan_error(format!("cannot create temporary metadata file: {e}")))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| plan_error(format!("cannot write temporary metadata file: {e}")))?;
        Ok(guard)
    }
    fn path(&self) -> &Path {
        &self.file
    }
}
impl Drop for TempJson {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// `oms_command_runs::Services` over this adapter's native journal, memory and
/// run-link producers (the trait needs `&mut`; the adapter itself is stateless).
struct RunsAdapter<'r, 'a>(&'r RuntimeServices<'a>);
fn runs_result(output: (i32, Vec<u8>, Vec<u8>)) -> oms_command_runs::Result<()> {
    if output.0 == 0 {
        Ok(())
    } else {
        Err(oms_command_runs::Error {
            code: output.0,
            message: String::from_utf8_lossy(&output.2).trim().to_owned(),
        })
    }
}
fn json_text<'j>(value: &'j oms_command_runs::Json, key: &str) -> Option<&'j [u8]> {
    match value {
        oms_command_runs::Json::Object(items) => items
            .iter()
            .rev()
            .find(|(k, _)| k == key.as_bytes())
            .and_then(|(_, v)| match v {
                oms_command_runs::Json::String(s) => Some(s.as_slice()),
                _ => None,
            }),
        _ => None,
    }
}
impl oms_command_runs::Services for RunsAdapter<'_, '_> {
    fn rlimit_helper(&self) -> Option<PathBuf> {
        self.0.helper.clone()
    }
    fn available(&self, service: oms_command_runs::Service) -> bool {
        matches!(
            service,
            oms_command_runs::Service::Journal
                | oms_command_runs::Service::Memory
                | oms_command_runs::Service::RunLink
                | oms_command_runs::Service::Digest
        )
    }
    fn observe(
        &mut self,
        event: oms_command_runs::Observation<'_>,
    ) -> oms_command_runs::Result<()> {
        if !matches!(event.row, oms_command_runs::Json::Object(_)) {
            return Err(oms_command_runs::Error::usage(
                "observation row must be a JSON object",
            ));
        }
        let encoded =
            oms_command_plan::json::canonical(&rvalue::from_native(event.row)).map_err(|e| {
                oms_command_runs::Error {
                    code: e.code,
                    message: e.message,
                }
            })?;
        let temp = TempJson::write_bytes(&encoded).map_err(|e| oms_command_runs::Error {
            code: e.code,
            message: e.message,
        })?;
        let mut args: Vec<OsString> = vec![
            "observe".into(),
            "--repo".into(),
            event.repo.as_os_str().to_owned(),
            "--source-type".into(),
            event.source_type.into(),
            "--source-file".into(),
            temp.path().as_os_str().to_owned(),
            "--record-path".into(),
            event.record_path.as_os_str().to_owned(),
        ];
        if let Some(kind) = event.event_type {
            args.extend(["--event-type".into(), kind.into()]);
        }
        runs_result(oms_command_journal::run_with_source(
            &args,
            event.repo,
            &self.0.source,
            self.0.cancel(),
        ))
    }
    fn memory(&mut self, note: oms_command_runs::MemoryNote<'_>) -> oms_command_runs::Result<()> {
        runs_result(oms_command_memory::run_with_source(
            &[
                "append".into(),
                "--agent".into(),
                note.agent.to_owned(),
                note.text.into(),
            ],
            note.repo,
            &self.0.source,
            self.0.cancel(),
        ))
    }
    fn link(&mut self, link: oms_command_runs::RunLink<'_>) -> oms_command_runs::Result<()> {
        runs_result(oms_command_state::run_with_source(
            "run",
            &[
                "link".into(),
                "--run-id".into(),
                link.run_id.into(),
                "--tool".into(),
                "run-ledger".into(),
                "--event".into(),
                "record".into(),
                "--path".into(),
                link.record_path.as_os_str().to_owned(),
                "--detail".into(),
                format!("exit {}", link.code).into(),
            ],
            link.repo,
            &self.0.source,
            self.0.cancel(),
        ))
    }
    /// The captured scheduler observation is handed over as is; the digest
    /// renderer never re-queries the scheduler.
    fn digest(
        &mut self,
        request: oms_command_runs::DigestRequest<'_>,
    ) -> oms_command_runs::Result<Vec<u8>> {
        let job = json_text(request.job, "job_id")
            .and_then(|b| std::str::from_utf8(b).ok())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| oms_command_runs::Error::usage("digest requires a job id"))?;
        let accounting = oms_command_job_digest::AccountingSnapshot {
            job_id: job,
            available: request.accounting.available,
            exit_code: request.accounting.exit_code,
            stdout: request.accounting.stdout,
            stderr: request.accounting.stderr,
        };
        let (code, stdout, stderr) = oms_command_job_digest::run_with_accounting(
            &[job.into()],
            request.repo,
            accounting,
            request.interrupted,
        );
        if code == 0 {
            Ok(stdout)
        } else {
            Err(oms_command_runs::Error {
                code,
                message: String::from_utf8_lossy(&stderr).trim().to_owned(),
            })
        }
    }
}
impl oms_command_runs::Services for RuntimeServices<'_> {
    fn rlimit_helper(&self) -> Option<PathBuf> {
        RunsAdapter(self).rlimit_helper()
    }
    fn available(&self, service: oms_command_runs::Service) -> bool {
        RunsAdapter(self).available(service)
    }
    fn observe(
        &mut self,
        event: oms_command_runs::Observation<'_>,
    ) -> oms_command_runs::Result<()> {
        RunsAdapter(self).observe(event)
    }
    fn memory(&mut self, note: oms_command_runs::MemoryNote<'_>) -> oms_command_runs::Result<()> {
        RunsAdapter(self).memory(note)
    }
    fn link(&mut self, link: oms_command_runs::RunLink<'_>) -> oms_command_runs::Result<()> {
        RunsAdapter(self).link(link)
    }
    fn digest(
        &mut self,
        request: oms_command_runs::DigestRequest<'_>,
    ) -> oms_command_runs::Result<Vec<u8>> {
        RunsAdapter(self).digest(request)
    }
}

/// Detached hook work re-enters this very binary through the staged
/// `--oms-dispatch` launcher; without a staged launcher detaching is refused.
impl oms_command_hooks::HostServices for RuntimeServices<'_> {
    fn supports_detach(&self) -> bool {
        !self.native.is_empty()
    }
    fn detach_command(&self, tool: &str, args: &[OsString], cwd: &Path) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        if self.native.is_empty() || !native_verb(tool) {
            return Err(format!("detached launch for '{tool}' is unavailable"));
        }
        let mut command = std::process::Command::new(&self.native[0]);
        command
            .args(&self.native[1..])
            .arg(tool)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|e| format!("detached launch for '{tool}' failed: {e}"))?;
        // Reap the leader when it ends; the child outlives the hook process.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
    fn check_python_syntax(&self, path: &Path, source: &[u8]) -> Option<bool> {
        crate::check_python_syntax_with_companion(path, source)
    }
    // `record_hook_failure` stays the no-op default on purpose: every caller
    // appends the same `hook` row to the fail-ledger itself immediately after
    // this callback, and a second row would double its count (a hook failure
    // below count 2 is "retiring").
}

impl oms_command_input::HostServices for RuntimeServices<'_> {
    /// In-process `room send --kind note`; refuses while a relay/carry route is
    /// configured, because in-process delivery must not be rerouted by it and
    /// this adapter cannot unset process environment.
    fn send_room_note(
        &self,
        repo: &Path,
        room: &str,
        from: &str,
        to: &str,
        text: &str,
        message_id: &str,
    ) -> Result<(), String> {
        for key in [
            "OMS_ROOM_RELAY_QUEUE",
            "OMS_ROOM_RELAY_ALLOWED",
            "OMS_ROOM_CARRY",
            "OMS_ROOM_CARRY_TS",
        ] {
            if std::env::var_os(key).is_some_and(|v| !v.is_empty()) {
                return Err(format!("room append refused: {key} would reroute the note"));
            }
        }
        let args: Vec<OsString> = ["send", "--repo"]
            .iter()
            .map(OsString::from)
            .chain([repo.as_os_str().to_owned()])
            .chain(
                [
                    "--id",
                    room,
                    "--participant",
                    from,
                    "--to",
                    to,
                    "--kind",
                    "note",
                    "--message-id",
                    message_id,
                    "--text",
                    text,
                ]
                .iter()
                .map(OsString::from),
            )
            .collect();
        let (code, _, stderr) = oms_command_room::run(&args, repo, self.cancel());
        if code == 0 {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&stderr).trim().to_owned())
        }
    }
    /// Takes the canonical runtime lock for the queue (`oms_lock`, mkdir mode,
    /// shared with other native and legacy writers), then the queue's own lock.
    fn with_queue_lock(
        &self,
        queue_path: &Path,
        interrupted: &AtomicBool,
        action: &mut dyn FnMut() -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let settings = oms_lock::Settings::from_env().map_err(|_| UNSAFE_INPUT)?;
        self.queue_lock_with(settings, queue_path, interrupted, action)
    }
}

impl RuntimeServices<'_> {
    fn queue_lock_with(
        &self,
        mut settings: oms_lock::Settings,
        queue_path: &Path,
        interrupted: &AtomicBool,
        action: &mut dyn FnMut() -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        settings.mode = oms_lock::Mode::Mkdir;
        let path = oms_command_input::runtime_lock_path(queue_path, settings.lock_dir.as_deref())
            .ok_or(UNSAFE_INPUT)?;
        let timeout = settings.timeout;
        let lock = oms_lock::Lock::at_path(path, settings).map_err(|_| UNSAFE_INPUT)?;
        let started = Instant::now();
        let _guard = loop {
            if interrupted.load(Ordering::Acquire) || self.cancel().load(Ordering::Acquire) {
                return Err(UNSAFE_INPUT);
            }
            match lock.try_acquire() {
                Ok(guard) => break guard,
                Err(oms_lock::Error::Busy) if started.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(oms_lock::Error::Busy) => return Err(oms_command_input::LOCK_TIMEOUT),
                Err(_) => return Err(UNSAFE_INPUT),
            }
        };
        oms_command_input::HostServices::with_queue_lock(
            &oms_command_input::DefaultHostServices,
            queue_path,
            interrupted,
            action,
        )
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    #[test]
    fn colon_group_and_account_parsers_resolve_members_and_fail_closed() {
        let single =
            b"root:x:0:0:root:/root:/bin/sh\nalice:x:1000:1000:Alice,,,:/var/empty:/bin/sh\nbob:x:1001:1001:Bob,,,:/var/empty:/bin/sh\n";
        let (by_name, primary) = parse_colon_account_entries(single, 1000).unwrap();
        let members = parse_colon_group_members(b"alice:x:1000:alice\n", 1000).unwrap();
        let resolved = resolve_membership_uids(1000, &members, &by_name, primary).unwrap();
        assert_eq!(resolved.gid, 1000);
        assert_eq!(resolved.explicit_member_uids, vec![1000]);
        assert_eq!(resolved.primary_member_uids, vec![1000]);

        let shared_members = parse_colon_group_members(b"shared:x:100:bob\n", 100).unwrap();
        let (s_map, s_primary) =
            parse_colon_account_entries(b"alice:x:1000:100:::\nbob:x:1001:100:::\n", 100).unwrap();
        let s_res = resolve_membership_uids(100, &shared_members, &s_map, s_primary).unwrap();
        assert_eq!(s_res.explicit_member_uids, vec![1001]);
        assert_eq!(s_res.primary_member_uids, vec![1000, 1001]);

        assert!(resolve_membership_uids(1000, &[b"ghost".to_vec()], &by_name, vec![1000]).is_err());
        assert!(parse_colon_group_members(b"", 1000).is_err());
        assert!(parse_colon_group_members(b"alice:x:999:\n", 1000).is_err());
        assert!(parse_colon_account_entries(b"broken:x:1000\n", 1000).is_err());
        assert!(
            parse_colon_account_entries(b"alice:x:1000:1000:::\nalice:x:1001:1000:::\n", 1000)
                .is_err()
        );
        assert!(parse_colon_account_entries(b"baduid:x:abc:1000:::\n", 1000).is_err());
    }

    #[test]
    fn dscacheutil_group_and_account_parsers_resolve_members_and_fail_closed() {
        let users = b"name: alice\nuid: 501\ngid: 20\n\nname: bob\nuid: 502\ngid: 20\n\nname: svc\nuid: 88\ngid: 88\n";
        let (by_name, primary) = parse_dscacheutil_account_entries(users, 20).unwrap();
        let members =
            parse_dscacheutil_group_members(b"name: staff\ngid: 20\nusers: alice bob\n", 20)
                .unwrap();
        let resolved = resolve_membership_uids(20, &members, &by_name, primary).unwrap();
        assert_eq!(resolved.explicit_member_uids, vec![501, 502]);
        assert_eq!(resolved.primary_member_uids, vec![501, 502]);

        let empty_members = parse_dscacheutil_group_members(b"name: svc\ngid: 88\n", 88).unwrap();
        assert!(empty_members.is_empty());

        assert!(parse_dscacheutil_group_members(b"", 20).is_err());
        assert!(parse_dscacheutil_group_members(b"name: staff\ngid: 21\n", 20).is_err());
        assert!(
            parse_dscacheutil_group_members(b"name: staff\ngid: 20\nusers alice bob\n", 20)
                .is_err()
        );
        assert!(parse_dscacheutil_account_entries(b"name: incomplete\nuid: 501\n", 20).is_err());
        assert!(parse_dscacheutil_account_entries(b"name: bad\nuid: nope\ngid: 20\n", 20).is_err());
        assert!(parse_dscacheutil_account_entries(b"name: alice\nuid 501\ngid: 20\n", 20).is_err());
    }

    #[test]
    fn linux_nss_completeness_parsers_and_policy_fail_closed() {
        let nss_sample = concat!(
            "# comment\n",
            "pass",
            "wd: files [NOTFOUND",
            "=return] systemd sss\n",
            "group: files systemd\n",
        );
        let (pw, gr) = parse_nsswitch_databases(nss_sample.as_bytes()).unwrap();
        assert_eq!(pw, vec!["files", "systemd", "sss"]);
        assert_eq!(gr, vec!["files", "systemd"]);

        // Allowed local/inactive-sss cases pass.
        assert!(verify_linux_nss_sources(&pw, &gr, || Ok(true), |_| Ok(())).is_ok());

        // Active sss with empty/suppressed enumeration fails closed.
        assert!(
            verify_linux_nss_sources(&pw, &gr, || Ok(true), |_| Err(plan_error("sss disabled")),)
                .is_err()
        );

        // Unverified NSS providers and external compat +/- lines fail closed.
        assert!(
            verify_linux_nss_sources(
                &["files".to_owned(), "ldap".to_owned()],
                &gr,
                || Ok(true),
                |_| Ok(()),
            )
            .is_err()
        );
        assert!(
            verify_linux_nss_sources(&["compat".to_owned()], &gr, || Ok(false), |_| Ok(()),)
                .is_err()
        );
        assert!(
            verify_linux_nss_sources(&["compat".to_owned()], &gr, || Ok(true), |_| Ok(()),).is_ok()
        );

        // Malformed nsswitch inputs fail closed.
        assert!(
            parse_nsswitch_databases(concat!("pass", "wd: files [UNCLOSED\n").as_bytes()).is_err()
        );
        assert!(
            parse_nsswitch_databases(concat!("pass", "wd: files\npass", "wd: files\n").as_bytes())
                .is_err()
        );
        assert!(parse_nsswitch_databases(concat!("pass", "wd:\n").as_bytes()).is_err());
        assert!(parse_nsswitch_databases(b"broken-line\n").is_err());
    }

    #[test]
    fn resolve_group_membership_resolves_current_gid_and_rejects_missing_gid() {
        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();
        if let Ok(membership) = resolve_group_membership(gid) {
            assert_eq!(membership.gid, gid);
            assert!(membership.primary_member_uids.contains(&uid));
        }
        assert!(resolve_group_membership(u32::MAX - 1).is_err());
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use oms_command_hooks::HostServices as _;
    use oms_command_input::HostServices as _;
    use oms_runtime::Services as _;
    use std::sync::atomic::AtomicU64;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "oms-runtime-services-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(std::fs::canonicalize(&path).unwrap())
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn git(&self, args: &[&str]) {
            let ok = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=oms-test",
                    "-c",
                    "user.email=oms-test@localhost",
                ])
                .args(args)
                .current_dir(&self.0)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        fn repo() -> Self {
            let dir = Self::new();
            dir.git(&["init", "-q", "-b", "main"]);
            dir.git(&["commit", "-q", "--allow-empty", "-m", "init"]);
            dir
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn services<'a>(flag: &'a Arc<AtomicBool>, source: &'a Path) -> RuntimeServices<'a> {
        RuntimeServices::with_overrides(flag, source, Vec::new(), None).unwrap()
    }
    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }
    fn backend(argv: &[&str], cwd: &Path, wall: u64, log_limit: usize) -> BackendRequest {
        BackendRequest {
            profile: "trusted-local".into(),
            argv: self::argv(argv),
            cwd: cwd.to_owned(),
            worktree: None,
            image: OsString::new(),
            adapter: OsString::new(),
            allow_network: false,
            inherit_env: Vec::new(),
            wall: Duration::from_secs(wall),
            log: None,
            receipt: None,
            log_limit,
            memory_mb: 4096,
            cpus: 2.0,
            pids: 512,
            tmpfs_mb: 1024,
        }
    }
    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }

    fn apptainer_test_process(name: &str, needs_session: bool) -> bool {
        if std::env::var_os("OMS_APPTAINER_TEST_CHILD").as_deref() == Some(OsStr::new(name)) {
            return false;
        }
        let session = Dir::new();
        let bus = if needs_session {
            match std::os::unix::net::UnixListener::bind(session.path().join("bus")) {
                Ok(bus) => Some(bus),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                    ) =>
                {
                    assert!(
                        !std::env::var("CI").is_ok_and(|v| v == "true" || v == "1"),
                        "Linux CI must exercise the native Apptainer fixture: {error}"
                    );
                    eprintln!(
                        "CAPABILITY-SKIP {name}: local Unix session socket unavailable: {error}; fake-host execution requires capable Linux CI"
                    );
                    return true;
                }
                Err(error) => panic!("session socket fixture: {error}"),
            }
        } else {
            None
        };
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", name, "--nocapture"])
            .env("OMS_APPTAINER_TEST_CHILD", name)
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
            .env_remove("OMS_STATE_REPO")
            .env("OMS_LOCK_DIR", session.path().join("locks"))
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env("OMS_TEST_LITERAL", "$(touch oms-apptainer-evaluated)")
            .env("APPTAINER_BIND", "/unexpected:/unexpected")
            .env("APPTAINERENV_OMS_AMBIENT", "unexpected")
            .env("SINGULARITY_BINDPATH", "/unexpected:/unexpected");
        if bus.is_some() {
            child.env("XDG_RUNTIME_DIR", session.path()).env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/bus", session.path().display()),
            );
        }
        let status = child.status().unwrap();
        assert!(status.success(), "{name}");
        true
    }

    fn fake_apptainer(dir: &Path) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let binary = dir.join("apptainer");
        std::fs::write(&binary, r#"#!/bin/sh
case "$1 ${2-}" in
  '--version ') printf 'apptainer version 1.5.5-fixture\n'; exit 0 ;;
  'exec --help') printf '%s\n' '--containall --cleanenv --no-home --no-eval --userns --no-mount --drop-caps --no-privs --net --network --bind --cwd --memory --cpus --pids-limit'; exit 0 ;;
  'sif header') [ "$(head -c 11 "$3")" = fixture-sif ]; exit $? ;;
esac
[ "$1" = exec ] || exit 90
printf 'arg=%s\n' "$@"
env
while [ "$#" -gt 0 ] && [ "$1" != --cwd ]; do shift; done
[ "$#" -ge 4 ] || exit 91
shift 2
image=$1
shift
printf 'run\n' >> "$image.calls"
case "$1" in
  exit-seven) exit 7 ;;
  mutate-image) printf 'changed\n' >> "$image"; exit 0 ;;
  namespace-refuse) printf 'namespace denied\n' >&2; exit 255 ;;
  wait) printf 'waiting\n' > "$image.waiting"; exec sleep 30 ;;
esac
exit 92
"#).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let image = dir.join("local.sif");
        std::fs::write(&image, b"fixture-sif\n").unwrap();
        (binary, image)
    }

    fn apptainer_request(repo: &Path, image: &Path, command: &str, name: &str) -> BackendRequest {
        let mut request = backend(&[command], repo, 10, 65536);
        request.profile = "apptainer".into();
        request.image = image.as_os_str().to_owned();
        request.log = Some(format!("{name}.log").into());
        request.receipt = Some(format!("{name}.json").into());
        request
    }

    #[test]
    fn utc_stamp_formats_known_instants() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_stamp(1_767_225_600 + 3_661), "2026-01-01T01:01:01Z");
    }

    #[test]
    fn origin_identity_normalizes_and_rejects_credentials_ports_and_odd_shapes() {
        let id = origin_identity;
        assert_eq!(
            id("git@github.com:Owner/Repo.git").as_deref(),
            Some("github.com/owner/repo")
        );
        assert_eq!(
            id("https://github.com/Owner/Repo/").as_deref(),
            Some("github.com/owner/repo")
        );
        assert_eq!(
            id("ssh://git@Git.Example.org/team/Tool.git").as_deref(),
            Some("git.example.org/team/Tool")
        );
        let with_userinfo = format!("https://{}{}@github.com/o/p", "user:", "x");
        assert_eq!(id(&with_userinfo), None, "credentials in the origin");
        for bad in [
            "",
            "https://user@github.com/o/p",
            "https://github.com:8443/o/p",
            "https://github.com/o/p?x=1",
            "https://github.com/o/p#f",
            "ssh://other@host/o/p",
            "http://github.com/o/p",
            "git@github.com:o",
            "git@github.com:o/p/q",
            "https://github.com/o",
            "https://github.com/../p",
            "https://github.com/o/p q",
            "/local/path",
        ] {
            assert_eq!(id(bad), None, "{bad}");
        }
    }

    #[test]
    fn git_env_removal_never_strips_what_the_child_sets() {
        let keep = [(OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0"))];
        let names = [
            "GIT_DIR",
            "git_work_tree",
            "GIT_TERMINAL_PROMPT",
            "HOME",
            "NOT_GIT_X",
        ]
        .map(OsString::from);
        let removed = ambient_git_variables(names.into_iter(), &keep);
        assert_eq!(removed, argv(&["GIT_DIR", "git_work_tree"]));
    }

    #[test]
    fn shell_quote_and_network_reason() {
        assert_eq!(shell_quote("a/b-c.d"), "a/b-c.d");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b'c"), "'a b'\"'\"'c'");
        assert_eq!(network_reason(b"Permission denied (publickey)"), "auth");
        assert_eq!(network_reason(b"Could not read Username"), "auth");
        assert_eq!(network_reason(b"connection refused"), "no-network");
        assert_eq!(network_reason(b""), "no-network");
    }

    #[test]
    fn remote_reply_parser_demands_the_exact_framed_shape() {
        let head = "a".repeat(40);
        let good = |reason: &str| {
            format!(
                "noise{REMOTE_SENTINEL}{reason}\0/srv/w\0true\0true\0true\0{head}\0main\0true\0false\0{head}\0{head}\0"
            )
        };
        let framed = good("");
        let fields = parse_remote_reply(0, &framed).unwrap();
        assert_eq!(fields[1], "/srv/w");
        assert!(parse_remote_reply(3, &good("dirty")).is_some());
        assert!(
            parse_remote_reply(0, &good("dirty")).is_none(),
            "reason with exit 0"
        );
        assert!(
            parse_remote_reply(3, &good("")).is_none(),
            "exit 3 without reason"
        );
        assert!(parse_remote_reply(3, &good("bogus")).is_none());
        assert!(parse_remote_reply(1, &good("")).is_none());
        assert!(parse_remote_reply(0, "no sentinel").is_none());
        assert!(parse_remote_reply(0, &good("").replace("true", "yes")).is_none());
        assert!(parse_remote_reply(0, &good("").replace("/srv/w", "srv/w")).is_none());
        assert!(parse_remote_reply(0, &good("").replace(&head, "zz")).is_none());
        let short = good("").trim_end_matches('\0').to_owned();
        assert!(
            parse_remote_reply(0, &short).is_none(),
            "missing terminator"
        );
        assert!(
            parse_remote_reply(0, &format!("{}\0", good(""))).is_none(),
            "extra field"
        );
    }

    #[test]
    fn child_runner_bounds_exit_timeout_cancel_and_launch() {
        let flag = AtomicBool::new(false);
        let run = |argv: &[&str], wall: u64, limit: usize| {
            run_child(
                Scope::ProcessGroup,
                ChildSpec::new(self::argv(argv), Duration::from_secs(wall), limit),
                &flag,
            )
        };
        assert!(run(&[], 5, 1024).is_err(), "empty argv");
        assert!(run(&["true"], 0, 1024).is_err(), "zero wall");
        assert!(run(&["true"], 5, 0).is_err(), "zero output bound");
        assert_eq!(run(&["sh", "-c", "exit 3"], 5, 1024).unwrap().code, 3);
        let out = run(&["sh", "-c", "printf out; printf err >&2"], 5, 1024).unwrap();
        assert_eq!(
            (out.stdout.as_slice(), out.stderr.as_slice()),
            (&b"out"[..], &b"err"[..])
        );
        let slow = run(&["sleep", "30"], 1, 1024).unwrap();
        assert!(slow.timed_out && slow.code == 124);
        let loud = run(&["sh", "-c", "yes | head -c 100000"], 5, 1024).unwrap();
        assert_eq!(loud.code, 125, "an over-limit stream stops the child");
        let missing = run(&["/nonexistent/oms-test-binary"], 5, 1024).unwrap_err();
        assert!(missing.starts_with(LAUNCH_FAILED), "{missing}");
        flag.store(true, Ordering::Release);
        assert_eq!(run(&["true"], 5, 1024).unwrap_err(), "interrupted");
    }

    #[test]
    fn child_runner_cancels_a_running_child() {
        let flag = Arc::new(AtomicBool::new(false));
        let remote = Arc::clone(&flag);
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            remote.store(true, Ordering::Release);
        });
        let out = run_child(
            Scope::ProcessGroup,
            ChildSpec::new(argv(&["sleep", "30"]), Duration::from_secs(60), 1024),
            &flag,
        )
        .unwrap();
        canceller.join().unwrap();
        assert!(out.cancelled && out.code == 130);
    }

    #[test]
    fn backend_check_reports_honest_readiness() {
        use std::os::unix::fs::PermissionsExt;
        let (flag, source) = (Arc::new(AtomicBool::new(false)), Dir::new());
        let mut rt = services(&flag, source.path());
        let ready = rt
            .backend_check("trusted-local", OsStr::new(""), OsStr::new(""))
            .unwrap();
        assert!(ready.get("ready").truth() && ready.get("missing").strings().unwrap().is_empty());
        for profile in ["isolated", "remote"] {
            let row = rt
                .backend_check(profile, OsStr::new("img"), OsStr::new("adp"))
                .unwrap();
            assert!(!row.get("ready").truth(), "{profile}");
            assert_eq!(row.get("missing").strings().unwrap().len(), 1);
        }
        assert!(
            rt.backend_check("bogus", OsStr::new(""), OsStr::new(""))
                .is_err()
        );
        rt.apptainer = None;
        assert!(
            !rt.backend_check("apptainer", OsStr::new("missing.sif"), OsStr::new(""))
                .unwrap()
                .get("ready")
                .truth()
        );
        if cfg!(target_os = "linux")
            && apptainer_test_process(
                "services_runtime::runtime_tests::backend_check_reports_honest_readiness",
                true,
            )
        {
            return;
        }
        let (binary, image) = fake_apptainer(source.path());
        rt.apptainer = Some(binary.clone());
        let check = |image: &OsStr| {
            rt.backend_check("apptainer", image, OsStr::new(""))
                .unwrap()
        };
        let ready = check(image.as_os_str());
        if !cfg!(target_os = "linux") {
            assert!(!ready.get("ready").truth());
            assert!(ready.get("missing").strings().unwrap()[0].contains("requires Linux"));
            return;
        }
        assert!(ready.get("ready").truth(), "{ready:?}");
        assert!(ready.get("details").get("preflight_only").truth());
        assert_eq!(
            ready.get("details").get("image").get("sha256"),
            &Value::Null
        );
        // An owned pathname alone is insufficient evidence of a private session.
        let session = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o755)).unwrap();
        let public_session = check(image.as_os_str());
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!public_session.get("ready").truth());
        let bus = session.join("bus");
        let saved = session.join("saved-bus");
        std::fs::rename(&bus, &saved).unwrap();
        std::fs::write(&bus, b"not a socket").unwrap();
        let fake_session = check(image.as_os_str());
        std::fs::remove_file(&bus).unwrap();
        std::fs::rename(&saved, &bus).unwrap();
        assert!(!fake_session.get("ready").truth());
        for bad in [
            OsStr::new(""),
            OsStr::new("missing.sif"),
            source.path().as_os_str(),
            OsStr::new("/dev/null"),
        ] {
            assert!(!check(bad).get("ready").truth());
        }
        let link = source.path().join("linked.sif");
        std::os::unix::fs::symlink(&image, &link).unwrap();
        assert!(!check(link.as_os_str()).get("ready").truth());
        let large = source.path().join("large.sif");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(64 * 1024 * 1024 * 1024 + 1)
            .unwrap();
        assert!(!check(large.as_os_str()).get("ready").truth());
        std::fs::write(&image, b"not a SIF").unwrap();
        assert!(!check(image.as_os_str()).get("ready").truth());
        std::fs::write(&image, b"fixture-sif\n").unwrap();
        let script = String::from_utf8(read(&binary))
            .unwrap()
            .replace("--userns", "--unsupported");
        std::fs::write(&binary, script).unwrap();
        assert!(!check(image.as_os_str()).get("ready").truth());
        assert!(
            !source.path().join("local.sif.calls").exists(),
            "preflight must not execute a payload"
        );
    }

    #[test]
    fn apptainer_policy_and_image_observations_are_literal_and_bounded() {
        if apptainer_test_process(
            "services_runtime::runtime_tests::apptainer_policy_and_image_observations_are_literal_and_bounded",
            false,
        ) {
            return;
        }
        let repo = Dir::new();
        let (binary, image) = fake_apptainer(repo.path());
        let mut request = apptainer_request(repo.path(), &image, "exit-seven", "policy");
        request
            .argv
            .extend(argv(&["two words", "$(touch oms-apptainer-evaluated)"]));
        let words = apptainer::command(&binary, repo.path(), None, repo.path(), &image, &request);
        for required in [
            "--containall",
            "--cleanenv",
            "--no-home",
            "--no-eval",
            "--userns",
            "--no-privs",
            "--net",
        ] {
            assert!(words.contains(&required.into()));
        }
        for pair in [
            ["--no-mount", "hostfs,bind-paths,cwd"],
            ["--drop-caps", "ALL"],
            ["--network", "none"],
            ["--memory", "4294967296"],
            ["--cpus", "2"],
            ["--pids-limit", "512"],
        ] {
            assert!(words.windows(2).any(|w| w == argv(&pair)), "{pair:?}");
        }
        let read_only = format!("{}:{}:ro", repo.path().display(), repo.path().display());
        assert!(words.contains(&read_only.into()));
        assert_eq!(&words[words.len() - 3..], request.argv.as_slice());
        request.allow_network = true;
        let host = apptainer::command(
            &binary,
            repo.path(),
            Some(repo.path()),
            repo.path(),
            &image,
            &request,
        );
        assert!(!host.contains(&"--net".into()) && !host.contains(&"--network".into()));
        let writable = format!("{}:{}:rw", repo.path().display(), repo.path().display());
        assert!(host.contains(&writable.into()));
        let explicit = apptainer::container_environment(&argv(&["OMS_TEST_LITERAL"])).unwrap();
        assert_eq!(
            explicit,
            vec![(
                "APPTAINERENV_OMS_TEST_LITERAL".into(),
                "$(touch oms-apptainer-evaluated)".into()
            )]
        );
        assert!(apptainer::container_environment(&[]).unwrap().is_empty());
        for key in [
            "APPTAINER_BIND",
            "SINGULARITYENV_PATH",
            "HOME",
            "BAD=NAME",
            "1BAD",
        ] {
            assert!(apptainer::container_environment(&argv(&[key])).is_err());
        }

        let mut observed = apptainer::Image::open(image.as_os_str()).unwrap();
        assert_eq!(
            observed.hash(&AtomicBool::new(false)).unwrap(),
            sha_hex(b"fixture-sif\n")
        );
        assert_eq!(observed.hash(&AtomicBool::new(true)).unwrap_err().code, 130);
        std::fs::write(&image, b"changed\n").unwrap();
        assert!(observed.check().is_err());
        for bad in [
            OsStr::new(""),
            OsStr::new("docker://example/image"),
            OsStr::new("/dev/null"),
            repo.path().as_os_str(),
        ] {
            assert!(apptainer::Image::open(bad).is_err());
        }
        let link = repo.path().join("linked.sif");
        std::os::unix::fs::symlink(&image, &link).unwrap();
        assert!(apptainer::Image::open(link.as_os_str()).is_err());
        let large = repo.path().join("large.sif");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(64 * 1024 * 1024 * 1024 + 1)
            .unwrap();
        assert!(apptainer::Image::open(large.as_os_str()).is_err());
        if cfg!(target_os = "linux") && users::get_current_uid() != 0 {
            let flag = Arc::new(AtomicBool::new(false));
            let mut rt = services(&flag, repo.path());
            rt.apptainer = Some(binary);
            let missing = rt
                .backend_check("apptainer", image.as_os_str(), OsStr::new(""))
                .unwrap();
            assert!(!missing.get("ready").truth());
            assert!(missing.get("missing").strings().unwrap()[0].contains("XDG_RUNTIME_DIR"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn apptainer_public_run_preserves_policy_argv_and_receipt() {
        if apptainer_test_process(
            "services_runtime::runtime_tests::apptainer_public_run_preserves_policy_argv_and_receipt",
            true,
        ) {
            return;
        }
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let (binary, image) = fake_apptainer(repo.path());
        let mut rt = services(&flag, repo.path());
        rt.apptainer = Some(binary);
        let args = argv(&[
            "backend",
            "run",
            "apptainer",
            "--image",
            "local.sif",
            "--log",
            "run.log",
            "--receipt",
            "run.json",
            "--inherit-env",
            "OMS_TEST_LITERAL",
            "--",
            "exit-seven",
            "two words",
            "$(touch oms-apptainer-evaluated)",
        ]);
        let response = oms_runtime::run(&args, repo.path(), repo.path(), &rt, &flag);
        assert_eq!(
            response.code,
            7,
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        let receipt = rvalue::parse(&read(&repo.path().join("run.json"))).unwrap();
        assert_eq!(receipt.text("profile"), "apptainer");
        assert!(receipt.get("launcher_exit").is_int(7) && receipt.get("exit").is_int(7));
        assert_eq!(receipt.get("resource_limits").get("enforced"), &Value::Null);
        assert_eq!(
            receipt.get("apptainer").get("image").text("sha256"),
            sha_hex(b"fixture-sif\n")
        );
        assert_eq!(receipt.get("apptainer").text("network"), "none-requested");
        let log = String::from_utf8(read(&repo.path().join("run.log"))).unwrap();
        let actual: Vec<_> = log.lines().filter_map(|s| s.strip_prefix("arg=")).collect();
        for flag in [
            "--containall",
            "--cleanenv",
            "--no-home",
            "--no-eval",
            "--userns",
            "--no-privs",
        ] {
            assert!(actual.contains(&flag), "{actual:?}");
        }
        for pair in [
            ["--no-mount", "hostfs,bind-paths,cwd"],
            ["--drop-caps", "ALL"],
            ["--network", "none"],
            ["--memory", "4294967296"],
            ["--cpus", "2"],
            ["--pids-limit", "512"],
        ] {
            assert!(actual.windows(2).any(|p| p == pair), "{pair:?}: {actual:?}");
        }
        let bind = format!("{}:{}:ro", repo.path().display(), repo.path().display());
        assert!(actual.windows(2).any(|p| p == ["--bind", bind.as_str()]));
        assert_eq!(
            &actual[actual.len() - 4..],
            [
                image.to_str().unwrap(),
                "exit-seven",
                "two words",
                "$(touch oms-apptainer-evaluated)"
            ]
        );
        assert!(
            log.lines()
                .any(|s| s == "APPTAINERENV_OMS_TEST_LITERAL=$(touch oms-apptainer-evaluated)")
        );
        for prefix in [
            "APPTAINER_BIND=",
            "SINGULARITY_BINDPATH=",
            "APPTAINERENV_OMS_AMBIENT=",
            "OMS_TEST_LITERAL=",
        ] {
            assert!(!log.lines().any(|s| s.starts_with(prefix)));
        }
        assert!(!repo.path().join("oms-apptainer-evaluated").exists());
        assert_eq!(read(&repo.path().join("local.sif.calls")), b"run\n");
        // Publishing to existing outputs refuses before a second container exec.
        assert_eq!(
            oms_runtime::run(&args, repo.path(), repo.path(), &rt, &flag).code,
            2
        );
        assert_eq!(read(&repo.path().join("local.sif.calls")), b"run\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn apptainer_explicit_mount_network_and_refusal_contracts() {
        if apptainer_test_process(
            "services_runtime::runtime_tests::apptainer_explicit_mount_network_and_refusal_contracts",
            true,
        ) {
            return;
        }
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let (binary, image) = fake_apptainer(repo.path());
        let mut rt = services(&flag, repo.path());
        rt.apptainer = Some(binary);
        let work = repo.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let mut request = apptainer_request(repo.path(), &image, "exit-seven", "work");
        request.worktree = Some("work".into());
        request.cwd = work.clone();
        let result = rt.supervise(repo.path(), &request).unwrap();
        assert_eq!(result.exit, 7);
        let log = String::from_utf8(read(&repo.path().join("work.log"))).unwrap();
        assert!(log.contains(&format!("arg={}:{}:rw\n", work.display(), work.display())));
        assert!(log.contains(&format!(
            "arg={}:{}:ro\n",
            repo.path().display(),
            repo.path().display()
        )));
        let mut host = apptainer_request(repo.path(), &image, "exit-seven", "host");
        host.worktree = Some(".".into());
        host.allow_network = true;
        let result = rt.supervise(repo.path(), &host).unwrap();
        assert_eq!(
            result.receipt.get("apptainer").text("network"),
            "host-requested"
        );
        let log = String::from_utf8(read(&repo.path().join("host.log"))).unwrap();
        assert!(
            !log.lines()
                .any(|s| s == "arg=--net" || s == "arg=--network")
        );
        assert!(log.contains(&format!(
            "arg={}:{}:rw\n",
            repo.path().display(),
            repo.path().display()
        )));
        let mut bad = apptainer_request(repo.path(), &image, "exit-seven", "bad");
        bad.image.clear();
        assert!(
            rt.supervise(repo.path(), &bad)
                .unwrap_err()
                .message
                .contains("explicit local")
        );
        bad.image = image.as_os_str().into();
        for key in [
            "APPTAINER_BIND",
            "SINGULARITYENV_PATH",
            "HOME",
            "BAD=NAME",
            "1BAD",
        ] {
            bad.inherit_env = vec![key.into()];
            assert!(rt.supervise(repo.path(), &bad).is_err(), "{key}");
        }
        bad.inherit_env.clear();
        let ambiguous = repo.path().join("ambiguous,bind");
        std::fs::create_dir(&ambiguous).unwrap();
        bad.worktree = Some(ambiguous.clone());
        bad.cwd = ambiguous;
        assert!(rt.supervise(repo.path(), &bad).is_err());
        assert_eq!(read(&repo.path().join("local.sif.calls")), b"run\nrun\n");
        let refused = apptainer_request(repo.path(), &image, "namespace-refuse", "refused");
        let result = rt.supervise(repo.path(), &refused).unwrap();
        assert_eq!(result.exit, 255);
        assert_eq!(
            result.receipt.get("apptainer").text("network"),
            "none-requested"
        );
        assert_eq!(
            read(&repo.path().join("local.sif.calls")),
            b"run\nrun\nrun\n",
            "no fallback invocation"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn apptainer_supervision_preserves_drift_timeout_and_cancellation_evidence() {
        if apptainer_test_process(
            "services_runtime::runtime_tests::apptainer_supervision_preserves_drift_timeout_and_cancellation_evidence",
            true,
        ) {
            return;
        }
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let (binary, image) = fake_apptainer(repo.path());
        let mut rt = services(&flag, repo.path());
        rt.apptainer = Some(binary);
        let changed = apptainer_request(repo.path(), &image, "mutate-image", "changed");
        let result = rt.supervise(repo.path(), &changed).unwrap();
        assert_eq!(result.exit, 125);
        assert!(result.receipt.get("launcher_exit").is_int(0));
        assert!(
            result
                .receipt
                .get("apptainer")
                .text("image_stability")
                .starts_with("changed-or-unverified")
        );
        assert!(!read(&repo.path().join("changed.log")).is_empty());
        let mut slow = apptainer_request(repo.path(), &image, "wait", "slow");
        slow.wall = Duration::from_secs(1);
        let result = rt.supervise(repo.path(), &slow).unwrap();
        assert_eq!(result.exit, 124);
        assert!(result.receipt.get("timed_out").truth());
        std::fs::remove_file(repo.path().join("local.sif.waiting")).unwrap();
        let waiting = repo.path().join("local.sif.waiting");
        let stop = Arc::clone(&flag);
        let canceller = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            while !waiting.exists() && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let observed = waiting.exists();
            stop.store(true, Ordering::Release);
            observed
        });
        let cancel = apptainer_request(repo.path(), &image, "wait", "cancel");
        let result = rt.supervise(repo.path(), &cancel).unwrap();
        assert!(
            canceller.join().unwrap(),
            "fixture must start before cancellation"
        );
        assert_eq!(result.exit, 130);
        assert!(result.receipt.get("launcher_exit").is_int(130));
        assert!(!result.receipt.get("timed_out").truth());
    }

    #[test]
    fn supervise_records_log_and_receipt_and_propagates_exit() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let mut request = backend(&["sh", "-c", "printf hello; exit 7"], repo.path(), 10, 4096);
        request.log = Some("out/run.log".into());
        request.receipt = Some("out/run.json".into());
        let result = rt.supervise(repo.path(), &request).unwrap();
        assert_eq!(result.exit, 7);
        assert_eq!(read(&repo.path().join("out/run.log")), b"hello");
        let receipt = rvalue::parse(&read(&repo.path().join("out/run.json"))).unwrap();
        assert!(receipt.get("exit").is_int(7) && !receipt.get("timed_out").truth());
        assert!(receipt.get("output_bytes").is_int(5));
        assert_eq!(receipt.text("log"), "out/run.log");
        assert_eq!(result.receipt.text("receipt"), "out/run.json");
        assert_eq!(receipt.text("profile"), "trusted-local");
        assert!(receipt.text("operation_id").starts_with("exec-"));
        // An existing log is never replaced: the second call refuses before the
        // command runs, and the first record stays intact.
        let marker = repo.path().join("ran-twice");
        let mut again = request.clone();
        again.argv = argv(&["sh", "-c", "touch ran-twice"]);
        assert!(rt.supervise(repo.path(), &again).is_err());
        assert!(
            !marker.exists(),
            "a run whose log cannot be created must not happen"
        );
        assert_eq!(read(&repo.path().join("out/run.log")), b"hello");
    }

    #[test]
    fn supervise_log_limit_edges_and_refusals() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let print = |n: usize| format!("head -c {n} /dev/zero | tr '\\0' x");
        let run = |name: &str, bytes: usize| {
            let mut request = backend(&["sh", "-c", &print(bytes)], repo.path(), 10, 1024);
            request.log = Some(format!("{name}.log").into());
            request.receipt = Some(format!("{name}.json").into());
            rt.supervise(repo.path(), &request).unwrap();
            (
                read(&repo.path().join(format!("{name}.log"))).len(),
                rvalue::parse(&read(&repo.path().join(format!("{name}.json")))).unwrap(),
            )
        };
        let (at, receipt) = run("at", 1024);
        assert!(at == 1024 && receipt.get("output_omitted_bytes").is_int(0));
        let (over, receipt) = run("over", 1025);
        assert!(over <= 1024, "{over}");
        assert!(!receipt.get("output_omitted_bytes").is_int(0));
        for bad in [0usize, 1023, 64 * 1024 * 1024 + 1] {
            let request = backend(&["true"], repo.path(), 10, bad);
            assert!(rt.supervise(repo.path(), &request).is_err(), "{bad}");
        }
        assert!(
            rt.supervise(repo.path(), &backend(&[], repo.path(), 10, 1024))
                .is_err()
        );
        assert!(
            rt.supervise(repo.path(), &backend(&["true"], repo.path(), 0, 1024))
                .is_err()
        );
        let mut isolated = backend(&["true"], repo.path(), 10, 1024);
        isolated.profile = "isolated".into();
        assert!(rt.supervise(repo.path(), &isolated).is_err());
        let ran = repo.path().join("ran");
        let touch = ["sh", "-c", "touch ran"];
        let mut escape = backend(&touch, repo.path(), 10, 1024);
        escape.log = Some("../escape.log".into());
        assert!(rt.supervise(repo.path(), &escape).is_err());
        let mut same = backend(&touch, repo.path(), 10, 1024);
        same.log = Some("same".into());
        same.receipt = Some("same".into());
        assert!(rt.supervise(repo.path(), &same).is_err());
        let mut nested = backend(&touch, repo.path(), 10, 1024);
        nested.log = Some("nested-out".into());
        nested.receipt = Some("nested-out/run.json".into());
        assert!(rt.supervise(repo.path(), &nested).is_err());
        let mut nested_rev = backend(&touch, repo.path(), 10, 1024);
        nested_rev.log = Some("nested-out/run.log".into());
        nested_rev.receipt = Some("nested-out".into());
        assert!(rt.supervise(repo.path(), &nested_rev).is_err());
        let mut long_leaf = backend(&touch, repo.path(), 10, 1024);
        long_leaf.log = Some(format!("{}.log", "a".repeat(230)).into());
        long_leaf.receipt = Some("ok-receipt.json".into());
        assert!(rt.supervise(repo.path(), &long_leaf).is_err());
        let mut orphan = backend(&touch, repo.path(), 10, 1024);
        orphan.log = Some("/nonexistent-oms-dir/x.log".into());
        assert!(rt.supervise(repo.path(), &orphan).is_err());
        assert!(!ran.exists(), "no refused request may run its command");
        let file = repo.path().join("plain");
        std::fs::write(&file, b"x").unwrap();
        assert!(
            rt.supervise(repo.path(), &backend(&["true"], &file, 10, 1024))
                .is_err()
        );
    }

    #[test]
    fn supervise_preflight_refuses_symlink_non_dir_and_readonly_parents_before_execution() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let outside = Dir::new();
        let rt = services(&flag, repo.path());
        let ran = repo.path().join("ran-unsafe-parent");
        let touch = ["sh", "-c", "touch ran-unsafe-parent"];

        // 1. Symlink parent component inside repo refuses before execution.
        let real_sub = repo.path().join("real-sub");
        std::fs::create_dir_all(&real_sub).unwrap();
        symlink(&real_sub, repo.path().join("linked-sub")).unwrap();
        let mut req = backend(&touch, repo.path(), 10, 1024);
        req.log = Some("linked-sub/run.log".into());
        assert!(rt.supervise(repo.path(), &req).is_err());

        // 2. Regular-file parent component inside repo refuses before execution.
        std::fs::write(repo.path().join("file-parent"), b"x").unwrap();
        let mut req = backend(&touch, repo.path(), 10, 1024);
        req.log = Some("file-parent/run.log".into());
        assert!(rt.supervise(repo.path(), &req).is_err());

        // 3. Trailing slash (directory spelling) refuses before execution.
        let mut req = backend(&touch, repo.path(), 10, 1024);
        req.log = Some("dir-only/".into());
        assert!(rt.supervise(repo.path(), &req).is_err());

        // 4. Symlink parent outside repo refuses before execution, and symlinked
        // ancestor aliases to the same physical destination refuse before execution.
        let ext_real = outside.path().join("real");
        std::fs::create_dir_all(&ext_real).unwrap();
        let ext_link = outside.path().join("link");
        symlink(&ext_real, &ext_link).unwrap();
        let mut req = backend(&touch, repo.path(), 10, 1024);
        req.log = Some(ext_link.join("out.log"));
        assert!(rt.supervise(repo.path(), &req).is_err());

        let ext_real_sub = ext_real.join("sub");
        std::fs::create_dir_all(&ext_real_sub).unwrap();
        let mut alias_req = backend(&touch, repo.path(), 10, 1024);
        alias_req.log = Some(ext_link.join("sub/shared.out"));
        alias_req.receipt = Some(ext_real_sub.join("shared.out"));
        assert!(rt.supervise(repo.path(), &alias_req).is_err());

        // 5. Read-only (0500) parent inside and outside repo refuses before execution.
        let ro_in = repo.path().join("ro-in");
        std::fs::create_dir_all(&ro_in).unwrap();
        std::fs::set_permissions(&ro_in, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut req_in = backend(&touch, repo.path(), 10, 1024);
        req_in.log = Some("ro-in/nested/run.log".into());
        let in_err = rt.supervise(repo.path(), &req_in);
        std::fs::set_permissions(&ro_in, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(in_err.is_err());

        let ro_out = outside.path().join("ro-out");
        std::fs::create_dir_all(&ro_out).unwrap();
        std::fs::set_permissions(&ro_out, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut req_out = backend(&touch, repo.path(), 10, 1024);
        req_out.log = Some(ro_out.join("run.log"));
        let out_err = rt.supervise(repo.path(), &req_out);
        std::fs::set_permissions(&ro_out, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(out_err.is_err());

        assert!(
            !ran.exists(),
            "refused output preflight must never run the child command"
        );
    }

    #[test]
    fn supervise_times_out_cancels_and_clears_the_environment() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let mut slow = backend(&["sleep", "30"], repo.path(), 1, 1024);
        slow.log = Some("slow.log".into());
        slow.receipt = Some("slow.json".into());
        let result = rt.supervise(repo.path(), &slow).unwrap();
        assert_eq!(result.exit, 124);
        assert!(result.receipt.get("timed_out").truth());
        let mut env = backend(&["env"], repo.path(), 10, 65536);
        env.log = Some("env.log".into());
        env.receipt = Some("env.json".into());
        rt.supervise(repo.path(), &env).unwrap();
        let text = String::from_utf8(read(&repo.path().join("env.log"))).unwrap();
        assert!(text.lines().any(|l| l == "OMS_RUNTIME_BACKEND=1"));
        assert!(
            !text.contains("CARGO_MANIFEST_DIR"),
            "ambient environment leaked"
        );
        let mut launch = backend(&["/nonexistent/oms-test-binary"], repo.path(), 10, 1024);
        launch.log = Some("launch.log".into());
        launch.receipt = Some("launch.json".into());
        assert_eq!(rt.supervise(repo.path(), &launch).unwrap().exit, 127);
        flag.store(true, Ordering::Release);
        let before = backend(&["true"], repo.path(), 10, 1024);
        assert_eq!(rt.supervise(repo.path(), &before).unwrap_err().code, 130);
    }

    #[test]
    fn repository_view_and_revisions_follow_git() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let view = rt.repository_view(repo.path()).unwrap();
        assert!(is_hex_oid(&view.head), "{}", view.head);
        assert_eq!(view.branch, "main");
        assert!(!view.worktree_state.is_empty());
        assert_eq!(
            rt.resolve_revision(repo.path(), "HEAD").unwrap(),
            Some(view.head.clone())
        );
        assert_eq!(
            rt.resolve_revision(repo.path(), "no-such-ref").unwrap(),
            None
        );
        for bad in ["", "--upload-pack=x", "a\0b"] {
            assert!(rt.resolve_revision(repo.path(), bad).is_err(), "{bad:?}");
        }
        repo.git(&["checkout", "-q", "--detach"]);
        assert_eq!(rt.repository_view(repo.path()).unwrap().branch, "");
        // No origin: a channel fetch refuses instead of reporting success.
        assert!(rt.fetch_release(repo.path()).is_err());
    }

    #[test]
    fn promote_release_checks_then_publishes_once() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let head = rt.repository_view(repo.path()).unwrap().head;
        let manifest = rvalue::parse(
            br#"{"schema":1,"channels":{"stable":{"version":"0.4.0","ref":"aa","auto_apply":false,"policy":"p"},"edge":{"version":"edge","ref":"main","auto_apply":false,"policy":"q"}}}"#,
        )
        .unwrap();
        let digest = rvalue::hash(&manifest).unwrap();
        let request = |commit: &str, version: &str, expected: &str| PromotionRequest {
            commit: commit.into(),
            version: version.into(),
            expected_manifest_digest: expected.into(),
            manifest: manifest.clone(),
        };
        for (commit, version, expected) in [
            ("short", "1.2.3", digest.as_str()),
            (head.as_str(), "1.2", digest.as_str()),
            (head.as_str(), "1.2.3-", digest.as_str()),
            (head.as_str(), "1.2.3", "abc"),
            (&"0".repeat(40), "1.2.3", digest.as_str()),
        ] {
            assert!(
                rt.promote_release(repo.path(), &request(commit, version, expected))
                    .is_err()
            );
        }
        assert!(!repo.path().join(MANIFEST_PATH).exists());
        let stale = rt
            .promote_release(repo.path(), &request(&head, "1.2.3", &"f".repeat(64)))
            .unwrap_err();
        assert_eq!(stale.code, 75);
        std::fs::write(repo.path().join("tracked"), b"1").unwrap();
        repo.git(&["add", "tracked"]);
        repo.git(&["commit", "-q", "-m", "tracked"]);
        let head = rt.repository_view(repo.path()).unwrap().head;
        std::fs::write(repo.path().join("tracked"), b"2").unwrap();
        let dirty = rt
            .promote_release(repo.path(), &request(&head, "1.2.3", &digest))
            .unwrap_err();
        assert_eq!(dirty.code, 3);
        repo.git(&["checkout", "-q", "--", "tracked"]);
        let done = rt
            .promote_release(repo.path(), &request(&head, "1.2.3-rc.1", &digest))
            .unwrap();
        assert!(done.get("promoted").truth());
        assert_eq!(done.text("previous_manifest_digest"), digest);
        let written = rvalue::parse(&read(&repo.path().join(MANIFEST_PATH))).unwrap();
        let stable = written.get("channels").get("stable");
        assert_eq!(
            (stable.text("ref"), stable.text("version")),
            (head.as_str(), "1.2.3-rc.1")
        );
        assert_eq!(
            rvalue::hash(&written).unwrap(),
            done.text("new_manifest_digest")
        );
        // The reviewed digest is now stale: a second promotion cannot replay it.
        let replay = rt
            .promote_release(repo.path(), &request(&head, "1.2.4", &digest))
            .unwrap_err();
        assert_eq!(replay.code, 75);
    }

    #[test]
    fn promote_release_acquires_canonical_lock_and_fails_closed_when_busy() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let mut rt = services(&flag, repo.path());
        rt.lock_timeout = Some(Duration::from_millis(150));
        let head = rt.repository_view(repo.path()).unwrap().head;
        let manifest = rvalue::parse(
            br#"{"schema":1,"channels":{"stable":{"version":"0.4.0","ref":"aa","auto_apply":false,"policy":"p"},"edge":{"version":"edge","ref":"main","auto_apply":false,"policy":"q"}}}"#,
        )
        .unwrap();
        let digest = rvalue::hash(&manifest).unwrap();
        let req = PromotionRequest {
            commit: head,
            version: "1.2.3".into(),
            expected_manifest_digest: digest,
            manifest,
        };
        let settings = rt.lock_settings().unwrap();
        let held = oms_lock::Lock::for_file(&repo.path().join(MANIFEST_PATH), settings)
            .unwrap()
            .try_acquire()
            .unwrap();
        let busy = rt.promote_release(repo.path(), &req).unwrap_err();
        assert_eq!(busy.code, 75, "{}", busy.message);
        assert!(
            !repo.path().join(MANIFEST_PATH).exists(),
            "contended promotion must not write manifest"
        );
        drop(held);
        let ok = rt.promote_release(repo.path(), &req).unwrap();
        assert!(ok.get("promoted").truth());
    }

    #[test]
    fn release_and_profile_installers_refuse_malformed_requests() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let head = rt.repository_view(repo.path()).unwrap().head;
        let release = |channel: &str, target: &str| ReleaseRequest {
            channel: channel.into(),
            target: target.into(),
            no_tools: true,
        };
        assert!(
            rt.apply_release(repo.path(), &release("beta", &head))
                .is_err()
        );
        assert!(
            rt.apply_release(repo.path(), &release("stable", "HEAD"))
                .is_err()
        );
        assert!(
            rt.apply_release(repo.path(), &release("stable", &"0".repeat(40)))
                .is_err()
        );
        let install = |profiles: &[&str], provider: Option<&str>| InstallerRequest {
            profiles: profiles.iter().map(|s| (*s).to_owned()).collect(),
            primary_provider: provider.map(str::to_owned),
            upgrade: false,
            allow_missing: false,
            receipt: None,
            dry_run: true,
        };
        assert!(
            rt.install_profiles(repo.path(), &install(&["--evil"], None))
                .is_err()
        );
        assert!(
            rt.install_profiles(repo.path(), &install(&[""], None))
                .is_err()
        );
        assert!(
            rt.install_profiles(repo.path(), &install(&["core"], Some("-x")))
                .is_err()
        );
    }

    #[test]
    fn target_transport_validates_before_any_network() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let request = |sync: bool, sha: Option<&str>, secs: u64| TargetRequest {
            name: "box".into(),
            ssh_alias: "alias".into(),
            workdir: "work/tree".into(),
            sync,
            sha: sha.map(str::to_owned),
            timeout: Duration::from_secs(secs),
        };
        for secs in [0, 9, 7201] {
            assert!(
                rt.target_transport(repo.path(), &request(false, None, secs))
                    .is_err()
            );
        }
        let mut bad = request(false, None, 10);
        bad.ssh_alias = "-oProxyCommand=x".into();
        assert!(rt.target_transport(repo.path(), &bad).is_err());
        let mut bad = request(false, None, 10);
        bad.workdir = "/abs".into();
        assert!(rt.target_transport(repo.path(), &bad).is_err());
        // No origin remote is a foreign origin, never a connection attempt.
        let status = rt
            .target_transport(repo.path(), &request(false, None, 10))
            .unwrap();
        assert_eq!(status.text("reason"), "foreign-origin");
        assert!(!status.get("ok").truth());
        repo.git(&[
            "remote",
            "add",
            "origin",
            "https://example.invalid/owner/project.git",
        ]);
        let malformed = rt.target_transport(repo.path(), &request(true, Some("xyz"), 10));
        assert!(
            malformed.is_err(),
            "a short sha is refused before any fetch"
        );
    }

    #[test]
    fn processes_hosts_and_accounts() {
        let (flag, source) = (Arc::new(AtomicBool::new(false)), Dir::new());
        let rt = services(&flag, source.path());
        let host = rt.host_name().unwrap();
        assert!(!host.is_empty());
        assert!(
            !rt.reservation_process_gone(&host, std::process::id())
                .unwrap()
        );
        assert!(!rt.reservation_process_gone(&host, 0).unwrap());
        assert!(
            !rt.reservation_process_gone("another-host.invalid", 1)
                .unwrap()
        );
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        if cfg!(target_os = "linux") {
            assert!(rt.reservation_process_gone(&host, pid).unwrap());
        }
        assert!(rt.account_home(OsStr::new("root")).unwrap().is_some());
        for bad in ["", "a/b", "a\0b", "oms-no-such-user-xyz"] {
            assert_eq!(rt.account_home(OsStr::new(bad)).unwrap(), None, "{bad:?}");
        }
        assert!(rt.group_resolver().is_some());
    }

    #[test]
    fn resolve_roots_finds_the_git_top_level() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let nested = repo.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let roots = rt.resolve_roots(&nested, OsStr::new("")).unwrap();
        assert_eq!(roots.execution_repo, repo.path());
        let explicit = rt.resolve_roots(repo.path(), nested.as_os_str()).unwrap();
        assert_eq!(explicit.execution_repo, repo.path());
        assert!(
            rt.resolve_roots(repo.path(), OsStr::new("missing-dir"))
                .is_err()
        );
    }

    #[test]
    fn task_and_failure_projections_refuse_changed_bytes() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        std::fs::create_dir_all(repo.path().join(".oms/task")).unwrap();
        std::fs::write(repo.path().join(TASK_PATH), b"# task\n").unwrap();
        let err = rt
            .task_status(repo.path(), repo.path(), b"different")
            .unwrap_err();
        assert!(err.message.contains("changed"), "{}", err.message);
        assert!(rt.plan_status(repo.path(), repo.path(), b"{}").is_err());
        let (code, _, stderr) = oms_command_state::run_with_source(
            "fail-ledger",
            &argv(&["record", "--cmd", "false", "--exit", "1"]),
            repo.path(),
            repo.path(),
            &flag,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
        let ledger = read(&repo.path().join(FAILURE_PATH));
        let value = rvalue::parse(
            &rt.unresolved_failures(repo.path(), repo.path(), &ledger)
                .unwrap(),
        )
        .unwrap();
        assert!(value.get("schema").is_int(1));
        assert_eq!(value.get("failures").array().unwrap().len(), 1);
        assert!(
            rt.unresolved_failures(repo.path(), repo.path(), b"stale")
                .is_err()
        );
        let other = Dir::new();
        assert!(
            rt.unresolved_failures(repo.path(), other.path(), &ledger)
                .is_err()
        );
    }

    #[test]
    fn task_status_reads_state_repo_when_distinct_from_execution_repo() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let state = Dir::repo();
        let rt = services(&flag, repo.path());
        std::fs::create_dir_all(state.path().join(".oms/task")).unwrap();
        let state_packet = oms_command_task::packet::ensure(
            Vec::new(),
            "2026-01-02T03:04:05Z",
            "shared-state-only-task",
            b"test-session",
        );
        std::fs::write(state.path().join(TASK_PATH), &state_packet).unwrap();
        // Put a different valid packet in execution repo to prove task_status reads state, not repo.
        std::fs::create_dir_all(repo.path().join(".oms/task")).unwrap();
        let wrong_packet = oms_command_task::packet::ensure(
            Vec::new(),
            "2026-01-02T03:04:05Z",
            "wrong-repo-task",
            b"other-session",
        );
        std::fs::write(repo.path().join(TASK_PATH), wrong_packet).unwrap();
        let json_bytes = rt
            .task_status(repo.path(), state.path(), &state_packet)
            .unwrap();
        let json_text = String::from_utf8_lossy(&json_bytes);
        assert!(json_text.contains("shared-state-only-task"), "{json_text}");
        assert!(!json_text.contains("wrong-repo-task"), "{json_text}");
    }

    #[test]
    fn experiment_gpu_usage_reports_uncounted_instead_of_guessing() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        for id in ["", "exp-1"] {
            let usage = rt.experiment_gpu_usage(repo.path(), id).unwrap();
            assert!(matches!(usage.get("used"), Value::Null), "{id:?}");
            assert!(usage.text("note").starts_with("not counted"));
        }
        assert!(
            rt.experiment_gpu_usage(&repo.path().join("missing"), "exp-1")
                .is_err()
        );
    }

    #[test]
    fn run_ledger_refuses_empty_requests_and_records_a_run() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let request = |argv: &[&str], wall: u64| LedgerRequest {
            operation_id: "experiment-run-test".into(),
            argv: self::argv(argv),
            metrics: None,
            file: Some("docs/EXPERIMENTS.jsonl".into()),
            note: "unit test".into(),
            research: rvalue::empty(),
            no_gate: true,
            reason: "unit test".into(),
            wall: Duration::from_secs(wall),
        };
        assert!(rt.run_ledger(repo.path(), &request(&[], 10)).is_err());
        assert!(rt.run_ledger(repo.path(), &request(&["true"], 0)).is_err());
        let result = rt
            .run_ledger(repo.path(), &request(&["sh", "-c", "exit 0"], 30))
            .unwrap();
        assert_eq!(
            result.output.exit,
            0,
            "{}",
            String::from_utf8_lossy(&result.output.stderr)
        );
        assert!(!result.ledger_id.is_empty());
        let ledger = String::from_utf8(read(&repo.path().join("docs/EXPERIMENTS.jsonl"))).unwrap();
        assert!(ledger.contains("experiment-run-test"));

        // Second invocation turns the ledger into multi-line JSONL; observation
        // must still pass one JSON object plus --record-path without warnings.
        let mut second_req = request(&["sh", "-c", "true"], 30);
        second_req.operation_id = "experiment-run-test-2".into();
        let second = rt.run_ledger(repo.path(), &second_req).unwrap();
        assert_eq!(
            second.output.exit,
            0,
            "{}",
            String::from_utf8_lossy(&second.output.stderr)
        );
        assert!(!second.ledger_id.is_empty());
        let stderr_text = String::from_utf8_lossy(&second.output.stderr);
        assert!(
            !stderr_text.contains("source record is not JSON") && !stderr_text.contains("warning:"),
            "{stderr_text}"
        );
        let journal =
            String::from_utf8(read(&repo.path().join(".oms/work-journal/events.jsonl"))).unwrap();
        let event_count = journal.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(event_count, 2, "{journal}");
        assert!(journal.contains("docs/EXPERIMENTS.jsonl"), "{journal}");
    }

    #[test]
    fn runs_services_report_native_producers() {
        use oms_command_runs::Service;
        let (flag, source) = (Arc::new(AtomicBool::new(false)), Dir::new());
        let mut rt = services(&flag, source.path());
        for service in [
            Service::Journal,
            Service::Memory,
            Service::RunLink,
            Service::Digest,
        ] {
            assert!(oms_command_runs::Services::available(&rt, service));
        }
        assert_eq!(oms_command_runs::Services::rlimit_helper(&rt), None);
        let missing_job = oms_command_runs::Json::Object(Vec::new());
        let interrupted = AtomicBool::new(false);
        let err = oms_command_runs::Services::digest(
            &mut rt,
            oms_command_runs::DigestRequest {
                repo: source.path(),
                job: &missing_job,
                accounting: oms_command_runs::AccountingSnapshot {
                    format: oms_command_runs::AccountingFormat::SacctAllocationParsable2,
                    columns: "",
                    available: false,
                    exit_code: 127,
                    stdout: b"",
                    stderr: b"",
                },
                interrupted: &interrupted,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, 2);
    }

    #[test]
    fn bundled_helper_must_be_a_regular_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new();
        let exe = dir.path().join("oms");
        assert_eq!(bundled_helper(&exe), None, "absent");
        let helper = dir.path().join(HELPER_NAME);
        std::fs::write(&helper, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(bundled_helper(&exe), None, "not executable");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(bundled_helper(&exe), Some(helper.clone()));
        std::fs::remove_file(&helper).unwrap();
        std::os::unix::fs::symlink("/bin/sh", &helper).unwrap();
        assert_eq!(bundled_helper(&exe), None, "symlink");
    }

    #[test]
    fn detach_runs_through_the_staged_launcher_only() {
        let (flag, dir) = (Arc::new(AtomicBool::new(false)), Dir::new());
        let plain = services(&flag, dir.path());
        assert!(!plain.supports_detach());
        assert!(plain.detach_command("capture", &[], dir.path()).is_err());
        let launcher = argv(&["/bin/sh", "-c", "printf %s \"$1\" > \"$2\"", "oms-launch"]);
        let rt = RuntimeServices::with_overrides(&flag, dir.path(), launcher, None).unwrap();
        assert!(rt.supports_detach());
        let marker = dir.path().join("marker");
        rt.detach_command(
            "session-handoff",
            &[marker.clone().into_os_string()],
            dir.path(),
        )
        .unwrap();
        for _ in 0..100 {
            if marker.exists() && !read(&marker).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(read(&marker), b"session-handoff");
        for bad in ["", "-x", "../x", "Capture", "a b", &"a".repeat(65)] {
            assert!(rt.detach_command(bad, &[], dir.path()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn staged_launcher_needs_the_exact_native_layout() {
        let dir = Dir::new();
        let rev = "0123456789abcdef0123456789abcdef01234567";
        let revdir = dir.path().join("target/oms-native").join(rev);
        std::fs::create_dir_all(&revdir).unwrap();
        let exe = revdir.join("oms");
        assert!(staged_launcher(&exe, dir.path()).is_empty(), "no manifest");
        std::fs::write(revdir.join("manifest.json"), b"{}").unwrap();
        let launcher = staged_launcher(&exe, dir.path());
        assert_eq!(launcher.len(), 5);
        assert_eq!(launcher[1], "--oms-dispatch");
        assert!(staged_launcher(&revdir.join("other"), dir.path()).is_empty());
        assert!(staged_launcher(&exe, &dir.path().join("elsewhere")).is_empty());
    }

    #[test]
    fn room_note_and_queue_lock_fail_closed() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        // Either an ambient relay route (refused) or a room that does not exist.
        let refused = rt
            .send_room_note(repo.path(), "room-1", "a", "b", "hello", "msg-1")
            .unwrap_err();
        assert!(!refused.is_empty(), "a refusal names its reason");
        let interrupted = AtomicBool::new(true);
        let mut ran = false;
        let queue = repo.path().join(".oms/inputs/a/queue.jsonl");
        let result = rt.with_queue_lock(&queue, &interrupted, &mut || {
            ran = true;
            Ok(())
        });
        assert!(
            result.is_err() && !ran,
            "an interrupted wait never runs the action"
        );
    }

    #[test]
    fn queue_lock_runs_once_propagates_and_times_out() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let locks = repo.path().join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        let inputs = repo.path().join(".oms/inputs");
        std::fs::create_dir_all(&inputs).unwrap();
        std::fs::write(repo.path().join(".git/info/exclude"), b"/.oms/\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [repo.path().join(".oms"), inputs.clone(), locks.clone()] {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let queue = inputs.join("queue.jsonl");
        let rt = services(&flag, repo.path());
        let settings = |ms: u64| {
            let mut settings = oms_lock::Settings::from_env().unwrap();
            settings.lock_dir = Some(locks.clone());
            settings.timeout = Duration::from_millis(ms);
            settings
        };
        let idle = AtomicBool::new(false);
        let mut runs = 0;
        rt.queue_lock_with(settings(2000), &queue, &idle, &mut || {
            runs += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(runs, 1);
        let failed = rt.queue_lock_with(settings(2000), &queue, &idle, &mut || Err("boom"));
        assert_eq!(failed, Err("boom"));
        // The first lock was released: a later call still gets it.
        rt.queue_lock_with(settings(2000), &queue, &idle, &mut || Ok(()))
            .unwrap();
        // While another holder keeps the canonical lock, the wait ends in the contract's timeout.
        let path = oms_command_input::runtime_lock_path(&queue, Some(&locks)).unwrap();
        let mut mkdir = settings(2000);
        mkdir.mode = oms_lock::Mode::Mkdir;
        let held = oms_lock::Lock::at_path(path, mkdir).unwrap();
        let _guard = held.try_acquire().unwrap();
        let mut ran = false;
        let timed_out = rt.queue_lock_with(settings(300), &queue, &idle, &mut || {
            ran = true;
            Ok(())
        });
        assert_eq!(timed_out, Err(oms_command_input::LOCK_TIMEOUT));
        assert!(!ran, "the action never runs without the lock");
    }

    #[test]
    fn ensure_plan_lineage_refuses_without_a_plan_and_accepts_an_initialized_one() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let rt = services(&flag, repo.path());
        let missing = rt
            .ensure_plan_lineage(repo.path(), repo.path())
            .unwrap_err();
        // A worker environment refuses every plan mutation as parent-only.
        let parent_only = missing.message.contains("parent-only");
        assert!(
            parent_only || missing.message.contains("no plan"),
            "{}",
            missing.message
        );
        if parent_only {
            // A harness child may not mutate plans; the refusals above are all it can prove.
            assert!(rt.plan_status(repo.path(), repo.path(), b"stale").is_err());
            return;
        }
        let (code, _, stderr) = oms_command_plan::run_with_services(
            &argv(&[
                "--repo",
                &repo.path().to_string_lossy(),
                "init",
                "--goal",
                "g",
                "--accept",
                "a",
            ]),
            repo.path(),
            &flag,
            &rt.control,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
        rt.ensure_plan_lineage(repo.path(), repo.path()).unwrap();
        rt.ensure_plan_lineage(repo.path(), repo.path()).unwrap();
        let raw = read(&repo.path().join(".oms/plan/tasks.json"));
        let status = rt.plan_status(repo.path(), repo.path(), &raw).unwrap();
        assert!(rvalue::parse(&status).is_ok());
        assert!(rt.plan_status(repo.path(), repo.path(), b"stale").is_err());
    }

    #[test]
    fn target_transport_status_runs_the_remote_script_through_ssh() {
        let (flag, repo) = (Arc::new(AtomicBool::new(false)), Dir::repo());
        let home = Dir::new();
        let origin = "https://example.invalid/owner/project.git";
        repo.git(&["remote", "add", "origin", origin]);
        let work = home.path().join("work/tree");
        std::fs::create_dir_all(&work).unwrap();
        let remote = Dir(work.clone());
        remote.git(&["init", "-q", "-b", "main"]);
        remote.git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        remote.git(&["remote", "add", "origin", origin]);
        // A fake ssh: drop the options and alias, then run the remote command
        // locally with the fake remote HOME (stdin carries the script).
        let fake = home.path().join("fake-ssh");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nwhile [ \"$1\" != -- ]; do shift; done\nshift\nshift\nHOME='{}' exec \"$@\"\n",
                home.path().display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut rt = services(&flag, repo.path());
        rt.ssh = fake.into_os_string();
        let request = |workdir: &str| TargetRequest {
            name: "box".into(),
            ssh_alias: "alias".into(),
            workdir: workdir.into(),
            sync: false,
            sha: None,
            timeout: Duration::from_secs(30),
        };
        let head = rt.repository_view(&work).unwrap().head;
        let ok = rt
            .target_transport(repo.path(), &request("work/tree"))
            .unwrap();
        assert!(ok.get("ok").truth(), "{ok:?}");
        assert!(matches!(ok.get("reason"), Value::Null));
        assert_eq!(
            (ok.text("head"), ok.text("branch")),
            (head.as_str(), "main")
        );
        assert!(ok.get("origin_matches").truth() && ok.get("clean").truth());
        assert!(ok.text("workdir").ends_with("work/tree"));
        let missing = rt
            .target_transport(repo.path(), &request("work/absent"))
            .unwrap();
        assert_eq!(
            (missing.get("ok").truth(), missing.text("reason")),
            (false, "missing")
        );
        remote.git(&[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/other/project.git",
        ]);
        let foreign = rt
            .target_transport(repo.path(), &request("work/tree"))
            .unwrap();
        assert_eq!(foreign.text("reason"), "foreign-origin");
        assert!(!foreign.get("ok").truth());
        // A transport that cannot be launched is a network failure, not success.
        rt.ssh = "/nonexistent/oms-test-ssh".into();
        let down = rt
            .target_transport(repo.path(), &request("work/tree"))
            .unwrap();
        assert_eq!(
            (down.get("ok").truth(), down.text("reason")),
            (false, "no-network")
        );
    }
}
