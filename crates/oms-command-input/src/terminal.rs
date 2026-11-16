//! Explicit, read-only tmux pane capture. No key or command is ever sent to a pane.
use crate::json::{J, object, os_string, string};
use crate::{MAX_ATTACHMENT, Result, UNSAFE, storage};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

const TMUX_MISSING: &str = "tmux is required for explicit terminal capture";

fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| {
            path.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}
fn tmux(args: &[&str], limit: usize, interrupted: &AtomicBool) -> Result<String> {
    let binary = which("tmux").ok_or(TMUX_MISSING)?;
    let mut command = Command::new(binary);
    command.args(args);
    let bytes = storage::run_bounded(
        command,
        limit,
        interrupted,
        5,
        "local capture is unavailable",
        "local capture exceeded its byte bound",
        "local capture failed",
    )?;
    String::from_utf8(bytes).map_err(|_| UNSAFE)
}
#[derive(PartialEq)]
pub struct Pane {
    pane: String,
    pid: String,
    cwd: PathBuf,
}
pub fn pane_state(pane: &str, interrupted: &AtomicBool) -> Result<Pane> {
    let b = pane.as_bytes();
    if b.len() < 2 || b[0] != b'%' || !b[1..].iter().all(u8::is_ascii_digit) {
        return Err("terminal attachments require an exact tmux pane ID");
    }
    let raw = tmux(
        &[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_id}\t#{pane_pid}\t#{pane_current_path}\t#{pane_dead}",
        ],
        8192,
        interrupted,
    )?;
    let fields: Vec<&str> = raw.trim_end_matches(['\r', '\n']).split('\t').collect();
    if fields.len() != 4
        || fields[0] != pane
        || fields[1].is_empty()
        || !fields[1].bytes().all(|c| c.is_ascii_digit())
        || fields[3] != "0"
    {
        return Err("terminal pane identity is unavailable or not live");
    }
    let cwd = Path::new(&OsString::from(fields[2]))
        .canonicalize()
        .map_err(|_| UNSAFE)?;
    Ok(Pane {
        pane: pane.to_owned(),
        pid: fields[1].to_owned(),
        cwd,
    })
}
/// The pane identity recorded in a terminal attachment, for drift checks.
pub fn matches_record(source: &J, interrupted: &AtomicBool) -> Result<()> {
    let text = |key| crate::json::str_of(source, key).ok_or(UNSAFE);
    let current = pane_state(text("pane")?, interrupted)?;
    let recorded_cwd = crate::json::os_path(crate::json::get(source, "cwd").ok_or(UNSAFE)?)?;
    if current.pane != text("pane")? || current.pid != text("pid")? || current.cwd != recorded_cwd {
        return Err("terminal pane identity or cwd changed; capture a new attachment");
    }
    Ok(())
}
pub fn capture(
    work: &Path,
    pane: &str,
    lines: i64,
    interrupted: &AtomicBool,
) -> Result<(J, String)> {
    if !(1..=200).contains(&lines) {
        return Err("terminal capture must request 1 to 200 lines");
    }
    let before = pane_state(pane, interrupted)?;
    if !before.cwd.starts_with(work) {
        return Err("terminal pane cwd is outside the selected repository");
    }
    let text = tmux(
        &["capture-pane", "-p", "-t", pane, "-S", &format!("-{lines}")],
        MAX_ATTACHMENT,
        interrupted,
    )?;
    if pane_state(pane, interrupted)? != before {
        return Err("terminal pane identity or cwd changed during capture");
    }
    let all = crate::split_lines(&text);
    let selected = all[all.len().saturating_sub(lines as usize)..].concat();
    let source = object([
        ("pane", string(pane)),
        ("pid", string(&before.pid)),
        ("cwd", os_string(before.cwd.as_os_str())),
        ("verified", J::Bool(true)),
        ("worktree", os_string(work.as_os_str())),
        ("lines", crate::json::number(lines)),
        (
            "note",
            string("Explicit historical terminal snapshot; no command was executed or injected."),
        ),
    ]);
    Ok((source, selected))
}
