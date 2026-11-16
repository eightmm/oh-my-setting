use crate::value::{Text, Value};
use crate::{OUTPUT_LIMIT, Server, ToolResult, matches, prefix_chars, text};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, kill_process_group};
use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// Keep enough decoded text for the largest UI model plus one character. Counts
// continue after this prefix fills, so JSON refuses rather than returning a cut.
const CAPTURE: usize = 256 * 1024 + 4;
#[derive(Default)]
struct Stream {
    prefix: String,
    pending: Vec<u8>,
    leading_text: String,
    chars: usize,
    bytes: usize,
    leading: usize,
    trailing: usize,
    nonwhite: bool,
}
impl Stream {
    fn decoded(&mut self, s: &str) {
        for c in s.chars() {
            self.bytes = self.bytes.saturating_add(c.len_utf8());
            self.chars = self.chars.saturating_add(1);
            if crate::whitespace(c) {
                self.trailing += 1;
                if !self.nonwhite {
                    self.leading += 1;
                    if self.leading_text.len() < CAPTURE {
                        self.leading_text.push(c);
                    }
                }
            } else {
                self.nonwhite = true;
                self.trailing = 0;
            }
            if self.nonwhite && self.prefix.len() < CAPTURE {
                self.prefix.push(c);
            }
        }
    }
    fn feed(&mut self, bytes: &[u8], eof: bool) {
        self.pending.extend_from_slice(bytes);
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    let s = s.to_owned();
                    self.decoded(&s);
                    self.pending.clear();
                    break;
                }
                Err(e) => {
                    let end = e.valid_up_to();
                    let s = String::from_utf8_lossy(&self.pending[..end]).into_owned();
                    self.decoded(&s);
                    self.pending.drain(..end);
                    if let Some(n) = e.error_len() {
                        self.decoded("�");
                        self.pending.drain(..n);
                    } else {
                        if eof && !self.pending.is_empty() {
                            self.decoded("�");
                            self.pending.clear();
                        }
                        break;
                    }
                }
            }
        }
    }
    fn trimmed_chars(&self) -> usize {
        if self.nonwhite {
            self.chars - self.leading - self.trailing
        } else {
            0
        }
    }
}
/// Bounded capture of one finished command. Text fields keep only the bounded
/// prefix; the counts cover the whole stream so oversize output is refused
/// rather than silently cut.
pub struct Captured {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) stderr_leading: String,
    pub(crate) stdout_chars: usize,
    pub(crate) stdout_bytes: usize,
    pub(crate) combined_chars: usize,
}
impl Captured {
    pub fn code(&self) -> i32 {
        self.code
    }
    pub fn stdout(&self) -> &str {
        &self.stdout
    }
    pub fn stderr(&self) -> &str {
        &self.stderr
    }
    /// Build a capture from complete in-memory output, with the same UTF-8
    /// decoding, bounds and counts as the subprocess executor.
    pub fn from_output(code: i32, stdout: &[u8], stderr: &[u8]) -> Self {
        let (mut out, mut err) = (Stream::default(), Stream::default());
        out.feed(stdout, true);
        err.feed(stderr, true);
        Self::finish(code, out, err)
    }
    fn finish(code: i32, out: Stream, err: Stream) -> Self {
        let stdout_chars = out.trimmed_chars();
        // Python strips the combined output, rather than each stream. The
        // explicit separator exists whenever both raw streams are nonempty.
        let combined_chars = if out.chars == 0 {
            err.trimmed_chars()
        } else if err.chars == 0 {
            stdout_chars
        } else if !out.nonwhite && !err.nonwhite {
            0
        } else {
            out.chars + 1 + err.chars
                - if out.nonwhite {
                    out.leading
                } else {
                    out.chars + 1 + err.leading
                }
                - if err.nonwhite {
                    err.trailing
                } else {
                    err.chars + 1 + out.trailing
                }
        };
        Self {
            code,
            stdout: out.prefix,
            stderr: err.prefix,
            stderr_leading: err.leading_text,
            stdout_chars,
            stdout_bytes: out.bytes,
            combined_chars,
        }
    }
}
/// One fixed OMS verb invocation. `args` excludes the program name.
pub struct CommandRequest<'a> {
    pub args: &'a [OsString],
    pub cwd: &'a Path,
    pub timeout: Duration,
    /// Set by the embedding process; executors must stop promptly when true.
    pub interrupted: &'a AtomicBool,
}
/// Host boundary for every MCP tool that runs an OMS verb. The `oms` front door
/// can supply an in-process router; the default spawns the native `oms` binary.
pub trait CommandExecutor: Send + Sync {
    fn execute(&self, request: &CommandRequest<'_>) -> Result<Captured, String>;
}
const ENTRYPOINT: &str = "oms";
fn trusted(p: &Path) -> bool {
    p.is_absolute() && p.is_file() && rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok()
}
/// Locate the native `oms` for fixed verbs. Only absolute, regular, executable
/// files qualify. An explicit pin is authoritative: when invalid it is refused,
/// never skipped. Otherwise: the running executable when named `oms`; an `oms`
/// beside an absolute private peer owner; then absolute `PATH` entries.
pub(crate) fn resolve_oms(
    oms_program: Option<&Path>,
    peer_owner: &Path,
) -> Result<PathBuf, String> {
    if let Some(pin) = oms_program {
        return if trusted(pin) {
            Ok(pin.to_owned())
        } else {
            Err("error: configured oms program is not an absolute executable file".into())
        };
    }
    if let Ok(exe) = std::env::current_exe()
        && exe.file_name().is_some_and(|n| n == ENTRYPOINT)
        && trusted(&exe)
    {
        return Ok(exe);
    }
    if peer_owner.is_absolute()
        && peer_owner.is_file()
        && let Some(sibling) = peer_owner.parent().map(|d| d.join(ENTRYPOINT))
        && trusted(&sibling)
    {
        return Ok(sibling);
    }
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(ENTRYPOINT))
                .find(|p| trusted(p))
        })
        .ok_or_else(|| "error: native oms executable not found".into())
}
/// Default executor: one subprocess of the resolved `oms` per call.
pub struct SubprocessExecutor {
    program: PathBuf,
}
impl SubprocessExecutor {
    pub fn resolve(oms_program: Option<&Path>, peer_owner: &Path) -> Result<Self, String> {
        resolve_oms(oms_program, peer_owner).map(|program| Self { program })
    }
    pub fn program(&self) -> &Path {
        &self.program
    }
    /// Use an explicit native `oms` executable (absolute, regular, executable).
    pub fn with_program(program: PathBuf) -> Result<Self, String> {
        Self::resolve(Some(&program), Path::new(""))
    }
}
fn terminate(child: &mut Child) {
    if let Some(pid) = Pid::from_raw(child.id() as i32) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}
fn drain(reader: &mut impl Read, stream: &mut Stream) -> io::Result<bool> {
    let mut bytes = [0u8; 8192];
    // Bound each turn so a continuously writing pipe cannot starve deadlines.
    for _ in 0..16 {
        match reader.read(&mut bytes) {
            Ok(0) => {
                stream.feed(&[], true);
                return Ok(true);
            }
            Ok(n) => stream.feed(&bytes[..n], false),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}
impl CommandExecutor for SubprocessExecutor {
    fn execute(&self, request: &CommandRequest<'_>) -> Result<Captured, String> {
        let (cwd, timeout) = (request.cwd, request.timeout);
        let mut child = Command::new(&self.program)
            .args(request.args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("error: {e}"))?;
        let result = (|| -> io::Result<Captured> {
            let mut stdout = child.stdout.take().unwrap();
            let mut stderr = child.stderr.take().unwrap();
            for fd in [
                &stdout as &dyn std::os::fd::AsFd,
                &stderr as &dyn std::os::fd::AsFd,
            ] {
                fcntl_setfl(fd, fcntl_getfl(fd)? | OFlags::NONBLOCK)?;
            }
            let (mut out, mut err) = (Stream::default(), Stream::default());
            let (mut out_done, mut err_done) = (false, false);
            let deadline = Instant::now() + timeout;
            let code = loop {
                if request.interrupted.load(Ordering::Relaxed) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"));
                }
                if !out_done {
                    out_done = drain(&mut stdout, &mut out)?;
                }
                if !err_done {
                    err_done = drain(&mut stderr, &mut err)?;
                }
                if let Some(status) = child.try_wait()?
                    && out_done
                    && err_done
                {
                    break status.code().unwrap_or(1);
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            Ok(Captured::finish(code, out, err))
        })();
        if result.is_err() {
            terminate(&mut child);
        }
        result.map_err(|e| format!("error: {e}"))
    }
}
impl Server<'_> {
    pub(crate) fn command(
        &self,
        args: &[impl AsRef<std::ffi::OsStr>],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<Captured, String> {
        let args: Vec<OsString> = args.iter().map(|a| a.as_ref().to_owned()).collect();
        self.executor.execute(&CommandRequest {
            args: &args,
            cwd,
            timeout,
            interrupted: self.interrupted,
        })
    }
    pub(crate) fn run_tool(&self, tool: &Value, args: &Value) -> ToolResult {
        let repo = self.repo(args)?;
        let mut argv: Vec<String> = tool["argv"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .map(|v| {
                let s = v.as_str().unwrap();
                s.strip_prefix("scripts/")
                    .and_then(|s| s.strip_suffix(".sh"))
                    .unwrap_or(s)
                    .to_owned()
            })
            .collect();
        if tool["name"] == "oms_journal" && args["view"] == "blockers" {
            argv.push("--blockers".into());
        }
        if let Some(pos) = tool["positional"].as_str() {
            let value = text(args, pos, 4096)?;
            let pattern = tool["positional_pattern"].as_str();
            let requirement = pattern.map_or_else(
                || "a bare digest file name".into(),
                |p| format!("an id matching {p}"),
            );
            if value.is_empty()
                || value.starts_with('-')
                || value.split(['/', '\\']).any(|s| s == "..")
                || pattern.is_some_and(|p| !matches(p, value))
                || (pattern.is_none() && value.contains(['/', '\\']))
            {
                return Err(format!(
                    "error: {} requires {requirement}",
                    tool["name"].as_str().unwrap()
                )
                .into());
            }
            argv.push(value.into());
        }
        self.run_argv(tool, &argv, &repo)
    }
    /// Run one fixed argv and bound its output like every command-backed tool.
    pub(crate) fn run_argv(&self, tool: &Value, argv: &[String], repo: &Path) -> ToolResult {
        let proc = self.command(argv, repo, Duration::from_secs(30))?;
        let count = if proc.code == 0 {
            proc.stdout_chars
        } else {
            proc.combined_chars
        };
        let output = if proc.code == 0 || proc.stderr.is_empty() {
            proc.stdout
        } else if proc.stdout.is_empty() {
            proc.stderr
        } else {
            format!("{}\n{}{}", proc.stdout, proc.stderr_leading, proc.stderr)
        };
        let mut output = output.trim_matches(crate::whitespace).to_owned();
        if output.is_empty() {
            output = format!("exit {}", proc.code);
        }
        if count > OUTPUT_LIMIT {
            if argv.iter().any(|s| s == "--json") || tool["json_output"] == true {
                let hint = if tool["name"] == "oms_journal" {
                    Some("view")
                } else {
                    tool["positional"].as_str()
                };
                let guidance =
                    hint.map_or_else(String::new, |h| format!(" Narrow the {h} argument."));
                return Err(format!(
                    "error: JSON output has {count} characters; limit is {OUTPUT_LIMIT}.{guidance}"
                )
                .into());
            }
            output = format!(
                "{}\n[truncated: {} characters omitted]",
                prefix_chars(&output, OUTPUT_LIMIT),
                count - OUTPUT_LIMIT
            );
        }
        if proc.code == 0 {
            Ok(output.into())
        } else {
            Err(output.into())
        }
    }
    pub(crate) fn graph(&self, args: &Value) -> Result<(Text, Value), crate::ToolError> {
        let repo = self.repo(args)?;
        let task = text(args, "task", 2048)?.trim_matches(crate::whitespace);
        if task.starts_with('-') {
            return Err("error: task cannot begin with '-'".into());
        }
        let argv = if task.is_empty() {
            vec![
                "graph",
                "project",
                "map",
                "--limit",
                "72",
                "--depth",
                "2",
                "--ui-model",
            ]
        } else {
            vec![
                "graph",
                "project",
                "context",
                "--task",
                task,
                "--max-files",
                "12",
                "--ui-model",
            ]
        };
        let p = self.command(
            &argv.into_iter().map(String::from).collect::<Vec<_>>(),
            &repo,
            Duration::from_secs(30),
        )?;
        if p.code != 0 {
            return Err(prefix_chars(
                format!(
                    "{}{}{}{}",
                    p.stdout,
                    if !p.stdout.is_empty() && !p.stderr.is_empty() {
                        "\n"
                    } else {
                        ""
                    },
                    p.stderr_leading,
                    p.stderr
                )
                .trim_matches(crate::whitespace),
                OUTPUT_LIMIT,
            )
            .into());
        }
        if p.stdout_bytes > 256 * 1024 {
            return Err("error: project graph UI model exceeds 262144-byte limit".into());
        }
        let mut model: Value = crate::value::from_str(&p.stdout)
            .map_err(|e| format!("error: project graph UI model is invalid JSON: {e}"))?;
        if model["kind"] != "project" || !model["nodes"].is_array() || !model["edges"].is_array() {
            return Err("error: project graph UI model has an invalid shape".into());
        }
        if model["nodes"].as_array().unwrap().len() > 120
            || model["edges"].as_array().unwrap().len() > 360
        {
            return Err("error: project graph UI model exceeds its node or edge bound".into());
        }
        model["title"] = json!(if task.is_empty() {
            "OMS Project Graph".into()
        } else {
            format!("OMS Context Graph · {}", prefix_chars(task, 72))
        });
        let n = |key| {
            model["counts"]
                .get(key)
                .map_or_else(|| "0".into(), crate::storage::display)
        };
        let s = Text::join(&[
            "Rendered ".into(),
            n("shown_nodes"),
            "/".into(),
            n("total_nodes"),
            " production nodes and ".into(),
            n("shown_edges"),
            "/".into(),
            n("total_edges"),
            " edges. Select a node to edit an instruction draft.".into(),
        ]);
        Ok((s, model))
    }
}
