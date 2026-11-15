//! Native runtime host adapters and shared bounded Unix account-ownership proofs.
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
