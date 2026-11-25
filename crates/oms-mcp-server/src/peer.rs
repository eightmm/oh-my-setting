use crate::storage::{self, Directory};
use crate::value::Value;
use crate::{
    MAX_ANSWER, OUTPUT_LIMIT, PAGE_BYTES, RUN_ROOT, Server, ToolResult, delegated, integer,
    matches, operation, prefix_chars, pretty, text, thread_id,
};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

fn normalize(raw: &str, cwd: &Path) -> Option<String> {
    let name = match raw {
        "agy" => "antigravity",
        "cursor-agent" => "cursor",
        "grok-build" => "grok",
        "gemini-cli" => "gemini",
        "qwen-code" => "qwen",
        "opencode2" => "opencode",
        "dsh" | "deepseek-harness" => "deepseek",
        "mistral-vibe" => "vibe",
        "github-copilot" => "copilot",
        "factory-droid" => "droid",
        "kiro-cli" => "kiro",
        "cn" => "continue",
        "codex" | "claude" | "antigravity" | "cursor" | "grok" | "gemini" | "qwen" | "opencode"
        | "deepseek" | "vibe" | "pi" | "copilot" | "droid" | "aider" | "muse" | "kimi" | "kiro"
        | "continue" | "goose" | "cline" | "kilo" | "openhands" | "auggie" => raw,
        other => {
            if !matches("[a-z0-9][a-z0-9._-]{0,63}", other) || other.contains("..") {
                return None;
            }
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/nonexistent".into()))
                        .join(".config")
                });
            let dir = std::env::var_os("OMS_PROVIDER_ADAPTER_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| config.join("oh-my-setting/provider-adapters"));
            let binary = format!("oms-agent-adapter-{other}");
            let search_path = std::env::var_os("PATH").unwrap_or_default();
            let candidates = std::iter::once(cwd.join(dir).join(&binary))
                .chain(std::env::split_paths(&search_path).map(|p| cwd.join(p).join(&binary)));
            if !candidates
                .into_iter()
                .any(|p| p.is_file() && rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok())
            {
                return None;
            }
            other
        }
    };
    Some(name.into())
}
fn targets(raw: &str, cwd: &Path) -> Result<Vec<String>, String> {
    let mut values = Vec::new();
    for entry in raw
        .split(',')
        .map(|s| s.trim_matches(crate::whitespace))
        .filter(|e| !e.is_empty())
    {
        let (provider, spec) = entry
            .split_once(':')
            .map_or((entry, None), |(a, b)| (a, Some(b)));
        let normalized = normalize(provider, cwd);
        if normalized.is_none()
            || spec.is_some_and(|s| {
                !s.strip_prefix("model=")
                    .is_some_and(|m| matches("[^\\x00-\\x1f\\x7f,]{1,160}", m))
            })
        {
            return Err(format!(
                "error: target must be PROVIDER or PROVIDER:model=NAME with a registered provider: {}",
                storage::repr(&json!(entry))
            ));
        }
        values.push(format!(
            "{}{}",
            normalized.unwrap(),
            spec.map_or_else(String::new, |s| format!(":{s}"))
        ));
    }
    Ok(values)
}
pub(super) use oms_command_operations::peer_operations::{age, status, updated_stamp};

fn tail(dir: &Path) -> String {
    let result = (|| -> io::Result<String> {
        let root = dir
            .ancestors()
            .nth(4)
            .ok_or_else(|| io::Error::other("operation root"))?;
        let rel = dir.strip_prefix(root).map_err(io::Error::other)?;
        let opened = Directory::open(root, rel.to_str().unwrap_or(""), false, false)
            .map_err(io::Error::other)?;
        let fd = rustix::fs::openat(
            &opened.fd,
            "run.log",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        let stat = rustix::fs::fstat(&fd)?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
            return Err(io::Error::other("unsafe log"));
        }
        let mut file = File::from(fd);
        let end = file.seek(SeekFrom::End(0))?;
        let start = end.saturating_sub(64 * 1024);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(64 * 1024).read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        let normalized = storage::normalize_lines(&text);
        let lines: Vec<_> = normalized.lines().collect();
        let last = lines[lines.len().saturating_sub(20)..].join("\n");
        let chars = last.chars().count();
        if chars > 4000 || (start > 0 && lines.len() <= 20) {
            Ok(format!(
                "[truncated]\n{}",
                last.chars()
                    .skip(chars.saturating_sub(4000))
                    .collect::<String>()
            ))
        } else {
            Ok(last)
        }
    })();
    result.unwrap_or_default()
}
fn references(
    dir: &Path,
    caller_cwd: &Path,
    owned_answers: Option<&Path>,
) -> (Vec<String>, String) {
    // Scan the entire log but bound each retained line; history is not tail-capped.
    let result = (|| -> io::Result<(Vec<String>, String)> {
        let repo = dir
            .ancestors()
            .nth(4)
            .ok_or_else(|| io::Error::other("operation root"))?;
        let opened = Directory::run(repo, dir.file_name().unwrap().to_str().unwrap(), false)
            .map_err(io::Error::other)?;
        let fd = rustix::fs::openat(
            &opened.fd,
            "run.log",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        let info = rustix::fs::fstat(&fd)?;
        if rustix::fs::FileType::from_raw_mode(info.st_mode) != rustix::fs::FileType::RegularFile {
            return Err(io::Error::other("unsafe log"));
        }
        let mut reader = BufReader::new(File::from(fd).take(info.st_size.max(0) as u64));
        let mut paths = Vec::new();
        let mut thread = String::new();
        let mut line = Vec::new();
        let mut over = false;
        loop {
            let buf = reader.fill_buf()?;
            if buf.is_empty() {
                if !line.is_empty() && !over {
                    reference_line(&line, &mut paths, &mut thread, caller_cwd, owned_answers);
                }
                break;
            }
            let n = buf
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buf.len(), |n| n + 1);
            let ended = buf[n - 1] == b'\n';
            if !over && line.len() + n <= 16384 {
                line.extend_from_slice(&buf[..n]);
            } else {
                over = true;
                line.clear();
            }
            reader.consume(n);
            if ended {
                if !over {
                    reference_line(&line, &mut paths, &mut thread, caller_cwd, owned_answers);
                }
                line.clear();
                over = false;
            }
        }
        Ok((paths, thread))
    })();
    result.unwrap_or_default()
}
fn reference_line(
    line: &[u8],
    paths: &mut Vec<String>,
    thread: &mut String,
    caller_cwd: &Path,
    owned_answers: Option<&Path>,
) {
    let s = String::from_utf8_lossy(line);
    let s = s.trim_end_matches(['\r', '\n']);
    let p = if let Some(p) = s.strip_prefix("artifact: ") {
        Some(p)
    } else if [
        "ok: ",
        "failed: ",
        "skipped: ",
        "blocked: ",
        "dry-run: ",
        "exported: ",
    ]
    .iter()
    .any(|prefix| s.starts_with(prefix))
    {
        s.split_once(" -> ").map(|(_, p)| p)
    } else {
        // Native peer-ask prints a bare path beside untrusted provider prose.
        // Only the exact direct child spelling is a hint. owned_answer opens it
        // again through the scoped no-follow, single-link reader before use.
        owned_answers.and_then(|dir| {
            use std::os::unix::fs::MetadataExt;
            let name = s.strip_prefix(dir.to_str()?)?.strip_prefix('/')?;
            (crate::answer_name(name)
                && Path::new(s)
                    .symlink_metadata()
                    .is_ok_and(|m| m.is_file() && m.nlink() == 1))
            .then_some(s)
        })
    };
    if let Some(p) = p.filter(|p| !p.is_empty() && !p.starts_with(char::is_whitespace)) {
        let p = p.trim_matches(crate::whitespace);
        if !paths.iter().any(|v| v == p) && caller_cwd.join(p).is_file() {
            paths.push(p.into());
        }
    }
    if let Some(t) = s
        .strip_prefix("thread: ")
        .filter(|t| !t.is_empty() && !t.contains(char::is_whitespace))
    {
        *thread = t.into();
    }
}
fn result_response(mut payload: Value, after: &str) -> ToolResult {
    let mut stable = payload.clone();
    stable.as_object_mut().unwrap().remove("elapsed_seconds");
    let bytes = crate::value::encode(&stable, false, false, true, true);
    let bytes = bytes.as_str().ok_or(crate::ToolError::InvalidField)?;
    let cursor = format!("result:{}", storage::hash(bytes));
    if after == cursor {
        for key in ["answer", "log_tail", "next"] {
            payload.as_object_mut().unwrap().remove(key);
        }
        payload["next"] = json!(
            "Visible result unchanged. Do not immediately recheck; omit after to retrieve the body again."
        );
    }
    payload["unchanged"] = json!(after == cursor);
    payload["cursor"] = json!(cursor);
    Ok(pretty(&payload))
}
impl Server<'_> {
    fn peer_repo(&self, args: &Value) -> Result<(PathBuf, OsString), crate::ToolError> {
        let raw = text(args, "repo", 4096)?;
        // Match Path(raw)'s spelling at the canonical thread/room boundary:
        // collapse separators and dots, but preserve symlinks and parent steps.
        let spelling = if raw.is_empty() {
            String::new()
        } else {
            let root = if raw.starts_with("//") && !raw.starts_with("///") {
                "//"
            } else if raw.starts_with('/') {
                "/"
            } else {
                ""
            };
            let parts = raw
                .split('/')
                .filter(|part| !part.is_empty() && *part != ".")
                .collect::<Vec<_>>()
                .join("/");
            let path = format!("{root}{parts}");
            if path.is_empty() {
                ".".to_owned()
            } else {
                path
            }
        };
        let resolved = self.repo(&json!({"repo":spelling}))?;
        let repo: OsString = if raw.is_empty() {
            resolved.as_os_str().to_owned()
        } else {
            spelling.into()
        };
        Ok((resolved, repo))
    }
    pub(crate) fn thread_exchange(&self, args: &Value, action: &str) -> ToolResult {
        let (_, repo) = self.peer_repo(args)?;
        let thread = text(args, "thread", 128)?;
        if !thread_id(thread) {
            return Err("error: an explicit valid thread id is required".into());
        }
        let after = text(args, "after", 1024)?;
        let new = args
            .get("new_thread")
            .unwrap_or(&Value::Bool(false))
            .as_bool()
            .ok_or("error: new_thread is a boolean for message only")?;
        if new && action != "message" {
            return Err("error: new_thread is a boolean for message only".into());
        }
        if truthy(&args["providers"]) {
            return Err("error: live messages do not invoke or select providers".into());
        }
        let member = text(args, "participant", 160)?;
        let mut argv: Vec<OsString> = vec![
            if member.is_empty() {
                "thread".into()
            } else {
                "room".into()
            },
            "--repo".into(),
            repo,
            "--id".into(),
            thread.into(),
        ];
        let mut command = Vec::<String>::new();
        let mut prompt = "";
        if !member.is_empty() {
            if !thread_id(member) || new {
                return Err("error: use an existing room and a valid joined participant".into());
            }
            argv.extend(["--participant".into(), member.into()]);
            match action {
                "updates" => command.extend(["updates".into(), "--after".into(), after.into()]),
                "ack" => {
                    let ids = args["message_ids"]
                        .as_array()
                        .filter(|a| {
                            !a.is_empty()
                                && a.len() <= 200
                                && a.iter().all(|s| s.as_str().is_some_and(thread_id))
                        })
                        .ok_or("error: room ack needs exact bounded message_ids")?;
                    command.push("ack".into());
                    for id in ids {
                        command.extend(["--message".into(), id.as_str().unwrap().into()]);
                    }
                }
                _ => {
                    prompt = text(args, "prompt", 4000)?;
                    if prompt.trim_matches(crate::whitespace).is_empty() {
                        return Err("error: room message requires a nonempty prompt".into());
                    }
                    command.extend(["send".into(), "--text".into(), prompt.into()]);
                    for (key, flag, default) in [
                        (
                            "to_participant",
                            "--to",
                            if delegated() { "" } else { "all" },
                        ),
                        ("message_kind", "--kind", "note"),
                        ("reply_to", "--reply-to", ""),
                        ("message_id", "--message-id", ""),
                    ] {
                        let raw = text(args, key, 160)?;
                        let value = if raw.is_empty() { default } else { raw };
                        if !value.is_empty() {
                            command.extend([flag.into(), value.into()]);
                        }
                    }
                }
            }
        } else {
            match action {
                "updates" => command.extend(
                    ["updates", "--after", after, "--max-bytes", "8192", "--json"]
                        .map(String::from),
                ),
                "ack" => command.extend(
                    [
                        "ack",
                        "--after",
                        after,
                        "--consumer",
                        text(args, "consumer", 160)?,
                    ]
                    .map(String::from),
                ),
                _ => {
                    prompt = text(args, "prompt", 4000)?;
                    if prompt.trim_matches(crate::whitespace).is_empty() {
                        return Err("error: message requires a nonempty prompt".into());
                    }
                    command
                        .extend(["append", "--role", "note", "--text", prompt].map(String::from));
                }
            }
        }
        if new {
            command = ["new", "--live", "--topic", prompt]
                .map(String::from)
                .to_vec();
        }
        argv.extend(command.into_iter().map(OsString::from));
        // The canonical thread front door receives no cwd override.
        let p = self.command(&argv, &self.caller_cwd, Duration::from_secs(5))?;
        if p.code != 0 {
            return Err(prefix_chars(
                if p.stderr.is_empty() {
                    p.stdout.trim_matches(crate::whitespace)
                } else {
                    p.stderr.trim_matches(crate::whitespace)
                },
                OUTPUT_LIMIT,
            )
            .into());
        }
        if p.stdout_chars > OUTPUT_LIMIT {
            return Err("error: thread JSON output exceeds 60000-character limit".into());
        }
        if action == "updates" || !member.is_empty() {
            Ok(p.stdout.trim_matches(crate::whitespace).into())
        } else {
            Ok(json!({"thread":thread,"status":"recorded","kind":action})
                .to_string()
                .into())
        }
    }
    pub(crate) fn start_peer(&self, args: &Value) -> ToolResult {
        self.start_bound_peer(args, None, false)
    }

    pub(crate) fn start_bound_peer(
        &self,
        args: &Value,
        assignment: Option<(&str, &str)>,
        preserve_current: bool,
    ) -> ToolResult {
        if delegated()
            && !(matches!(args["kind"].as_str(), Some("message" | "ack"))
                && truthy(&args["participant"]))
        {
            return Err("error: a delegated worker cannot start peer consultations; recursive delegation is the owner's decision — report the need in your answer instead".into());
        }
        let (repo, _) = self.peer_repo(args)?;
        let kind = text(args, "kind", 64)?;
        let rounds = integer(args, "debate_rounds", 0, 0, 3).map_err(
            |_| "error: debate_rounds must be an integer 0-3; positive values require kind='ask'",
        )?;
        if rounds > 0 && kind != "ask" {
            return Err(
                "error: debate_rounds must be an integer 0-3; positive values require kind='ask'"
                    .into(),
            );
        }
        let mut context = json!({});
        for key in ["include_memory", "include_task"] {
            if let Some(v) = args.get(key) {
                if !v.is_boolean() {
                    return Err(format!("error: {key} must be a boolean").into());
                }
                if !["consult", "advise", "ask"].contains(&kind) {
                    return Err(
                        format!("error: {key} applies only to consult, advise, or ask").into(),
                    );
                }
                context[key] = v.clone();
            }
        }
        if matches!(kind, "message" | "ack") {
            return self.thread_exchange(args, kind);
        }
        if !["consult", "advise", "ask"].contains(&kind) {
            return Err("error: kind must be one of: advise, ask, consult".into());
        }
        let raw_prompt = args["prompt"]
            .as_text()
            .ok_or("error: prompt must be a string")?;
        let prompt = raw_prompt
            .as_str()
            .ok_or("error: prompt must be valid UTF-8 text")?;
        if prompt.len() > 64 * 1024 {
            return Err("error: prompt exceeds 65536-byte limit".into());
        }
        // NUL is harmless in the file form but cannot enter ask's inline argv.
        if kind == "ask" && prompt.contains('\0') {
            return Err("error: prompt contains a NUL byte".into());
        }
        let prompt = prompt.trim_matches(crate::whitespace);
        if prompt.is_empty() {
            return Err("error: prompt is required: a consultation needs a question".into());
        }
        let targets = targets(text(args, "providers", 16384)?, &self.caller_cwd)?;
        if kind == "advise" && targets.len() > 1 {
            return Err("error: advise asks one advisor; name a single provider".into());
        }
        let mut thread = text(args, "thread", 128)?.to_owned();
        if !thread.is_empty() && !thread_id(&thread) {
            return Err(format!(
                "error: thread must be an id a previous run reported (letters, digits, dot, underscore, dash): {}",
                storage::repr(&json!(thread))
            ).into());
        }
        let new = match args.get("new_thread") {
            None | Some(Value::Null) => false,
            Some(v) => v.as_bool().ok_or("error: new_thread must be a boolean")?,
        };
        if new && kind != "consult" {
            return Err("error: new_thread applies to kind='consult'; ask creates its own thread when no thread is given".into());
        }
        if new && !thread.is_empty() {
            return Err("error: thread and new_thread are exclusive: continue that thread or start a fresh one".into());
        }
        if !self.config.peer_owner.is_absolute() || !self.config.peer_owner.is_file() {
            return Err("error: native MCP peer owner is not configured; the embedding executable must bundle the private owner entry".into());
        }
        // Fail closed before any `.oms/` state exists when no native oms resolves.
        let oms_bin = crate::command::resolve_oms(
            self.config.oms_program.as_deref(),
            &self.config.peer_owner,
        )?;
        let op = if let Some((operation, _)) = assignment {
            operation.to_owned()
        } else {
            let mut random = [0u8; 4];
            File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut random))
                .map_err(|e| format!("error: {e}"))?;
            let stamp =
                chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).format("%Y%m%dT%H%M%SZ");
            format!(
                "{kind}-{stamp}-{}",
                random
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        };
        if kind == "ask" && thread.is_empty() {
            thread = op.clone();
        }
        let dir_path = repo.join(RUN_ROOT).join(&op);
        let started = storage::now();
        let mut meta = json!({"operation":op,"kind":kind,"targets":targets,"started_at":started,"answer_storage":"operation","cancellation_supported":true,"title":prefix_chars(&prompt.split(crate::whitespace).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "),160)});
        if let Some((_, binding)) = assignment {
            meta["panel_assignment"] = json!(binding);
        }
        if !thread.is_empty() {
            meta["thread"] = json!(thread);
        }
        if context.as_object().is_some_and(|m| !m.is_empty()) {
            meta["context_options"] = context.clone();
        }
        // Owner reconstruction uses only validated typed fields, not stored argv.
        let mut launch = json!({"kind":kind,"targets":targets,"thread":thread,"new_thread":new,"debate_rounds":rounds,"context_options":context});
        if preserve_current {
            if assignment.is_none() || kind != "ask" || thread != op {
                return Err("error: invalid non-selecting assignment".into());
            }
            launch["preserve_current"] = json!(true);
        }
        if kind == "ask" {
            let mut argv: Vec<OsString> = vec![
                "thread".into(),
                "--repo".into(),
                repo.as_os_str().to_owned(),
                "--id".into(),
                thread.clone().into(),
            ];
            if repo
                .join(".oms/threads")
                .join(format!("{thread}.jsonl"))
                .exists()
            {
                if assignment.is_some() {
                    return Err("error: assignment thread already exists".into());
                }
                argv.extend(["context", "--require-open"].map(OsString::from));
            } else {
                argv.extend(["new", "--live", "--topic", "Council"].map(OsString::from));
                if preserve_current {
                    argv.push("--no-current".into());
                }
            }
            if self
                .command(&argv, &self.caller_cwd, Duration::from_secs(5))?
                .code
                != 0
            {
                return Err("error: council thread could not be opened".into());
            }
        }
        // Re-read worker context at the mutation boundary; a caller-owned repo
        // or requested participant cannot authorize a model launch.
        if delegated() {
            return Err("error: a delegated worker cannot start peer consultations".into());
        }
        let oms = Directory::open(&repo, ".oms", true, true)?;
        oms.publish_new(".gitignore", b"*\n")?;
        let parent = Directory::open(&repo, RUN_ROOT, true, true)?;
        rustix::fs::mkdirat(
            &parent.fd,
            op.as_str(),
            rustix::fs::Mode::from_raw_mode(0o700),
        )
        .map_err(|e| format!("error: {e}"))?;
        rustix::fs::fsync(&parent.fd).map_err(|e| format!("error: {e}"))?;
        let dir = Directory::run(&repo, &op, true)?;
        dir.atomic("prompt.txt", format!("{prompt}\n").as_bytes())?;
        dir.atomic("meta.json", format!("{meta}\n").as_bytes())?;
        dir.atomic("launch.json", format!("{launch}\n").as_bytes())?;
        let log = dir.exclusive("run.log")?;
        let mut owner = Command::new(&self.config.peer_owner)
            .args([
                self.source_root.as_os_str(),
                repo.as_os_str(),
                std::ffi::OsStr::new(&op),
                oms_bin.as_os_str(),
            ])
            .current_dir(&repo)
            .env(
                "OMS_AGENT_MEMORY_MODE",
                std::env::var_os("OMS_AGENT_MEMORY_MODE").unwrap_or_else(|| "relevant".into()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|e| format!("error: {e}"))?,
            ))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| format!("error: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while storage::read_run(&dir_path, "pid", 4096).is_err() {
            if let Some(s) = owner.try_wait().map_err(|e| format!("error: {e}"))? {
                return Err(
                    format!("error: native peer owner exited before readiness: {s}").into(),
                );
            }
            if self.interrupted.load(Ordering::Relaxed) || Instant::now() >= deadline {
                let _ = owner.kill();
                let _ = owner.wait();
                return Err("error: native peer owner did not become ready".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Reap eventually while permitting the native owner to outlive stdio.
        std::thread::spawn(move || {
            let _ = owner.wait();
        });
        let mut out = json!({"operation":op,"kind":kind,"started":true,"started_at":started,"targets":targets,"artifact_dir":dir_path.join("answers"),"run_dir":dir_path,"log":dir_path.join("run.log"),"result_arguments":{"repo":repo,"operation":op,"wait_seconds":if kind == "ask" {0} else {50}},"next": if kind == "ask" {"Council replies are published as completed turns. Read oms_peer_result with thread_arguments and reuse its cursor at useful task boundaries, not in a tight poll loop. Keep operation reads immediate while exchanging messages: a long wait blocks this serial connection. Post a note via kind=message for the next configured round; all seats receive the same round-start snapshot. Notes never start a round, interrupt a call, or grant new authority. Reads never start more model calls."} else {"The peer is running detached. Do authorized independent work or call oms_peer_result with result_arguments. Reduce wait_seconds if needed to stay below the host timeout. Avoid repeated immediate reads; expiry never restarts the peer."}});
        if context.as_object().is_some_and(|m| !m.is_empty()) {
            out["context_options"] = context;
        }
        if kind == "ask" {
            out["thread"] = json!(thread);
            out["debate_rounds"] = json!(rounds);
            out["thread_arguments"] = json!({"repo":repo,"thread":thread});
        }
        Ok(pretty(&out))
    }
    pub(crate) fn peer_operations(&self, args: &Value) -> ToolResult {
        let repo = self.repo(args)?;
        let mut entries = Vec::<String>::new();
        if Directory::open(&repo, RUN_ROOT, false, false).is_ok() {
            for e in std::fs::read_dir(repo.join(RUN_ROOT)).map_err(|e| format!("error: {e}"))? {
                let e = e.map_err(|e| format!("error: {e}"))?;
                let name = e.file_name().to_string_lossy().into_owned();
                if operation(&name) && Directory::run(&repo, &name, false).is_ok() {
                    entries.push(name);
                }
            }
        }
        entries.sort_by(|a, b| {
            b.split('-')
                .nth(1)
                .cmp(&a.split('-').nth(1))
                .then_with(|| b.cmp(a))
        });
        let mut rows = Vec::new();
        for name in entries.iter().take(20) {
            let dir = repo.join(RUN_ROOT).join(name);
            let meta = storage::meta(&dir);
            let (kind, started) = storage::started(name);
            let (status, code) = status(&dir);
            let mut row = json!({"operation":name,"kind":if truthy(&meta["kind"]) {meta["kind"].clone()} else {json!(kind)},"status":status,"started_at":if truthy(&meta["started_at"]) {meta["started_at"].clone()} else {json!(started)}});
            cancellation(&dir, status, &mut row);
            if let Some(n) = age(&dir) {
                row["age_seconds"] = json!(n);
            }
            if let Some(n) = code {
                row["exit"] = json!(n);
            }
            if let Some(a) = meta["targets"].as_array().filter(|a| !a.is_empty()) {
                row["targets"] = json!(
                    a.iter()
                        .filter(|s| s.is_string())
                        .take(8)
                        .collect::<Vec<_>>()
                );
            }
            let mut context = json!({});
            for key in ["include_memory", "include_task"] {
                if meta["context_options"][key].is_boolean() {
                    context[key] = meta["context_options"][key].clone();
                }
            }
            if context.as_object().is_some_and(|o| !o.is_empty()) {
                row["context_options"] = context;
            }
            if let Some(title) = meta["title"].as_text().filter(|s| !s.is_empty()) {
                row["title"] = json!(title.prefix(160));
            }
            let (_, thread) = references(&dir, &self.caller_cwd, None);
            let thread = if thread.is_empty() {
                meta["thread"].as_text().cloned().unwrap_or_default()
            } else {
                thread.into()
            };
            if !thread.is_empty() {
                row["thread"] = json!(thread);
            }
            rows.push(row);
        }
        let mut payload = json!({"shown":rows.len(),"operations":rows,"total":entries.len(),"next":"Read one with oms_peer_result operation=ID. status='stalled' means the run's process is gone with no exit recorded: that answer is never arriving, so start a new consultation instead of polling."});
        if entries.len() > 20 {
            payload["truncated"] = json!(true);
        }
        Ok(pretty(&payload))
    }
    pub(crate) fn peer_result(&self, args: &Value) -> ToolResult {
        self.peer_result_snapshot(args, None)
    }
    fn peer_result_snapshot(
        &self,
        args: &Value,
        captured: Option<(&Value, &'static str, Option<i64>)>,
    ) -> ToolResult {
        let wait = integer(args, "wait_seconds", 0, 0, 50)
            .map_err(|_| "error: wait_seconds must be an integer from 0 to 50")?;
        let page = args.get("answer_ref").is_some();
        if page {
            let reference = args["answer_ref"].as_str().unwrap_or("");
            let (name, digest) = reference.rsplit_once(':').unwrap_or(("", ""));
            if !crate::answer_name(name) || !matches("[0-9a-f]{64}", digest) {
                return Err("error: answer_ref must be a returned answer reference".into());
            }
            if args.get("thread").is_some() || args.get("after").is_some() || wait > 0 {
                return Err(
                    "error: answer_ref cannot be combined with thread, after, or a completion wait"
                        .into(),
                );
            }
        } else if ["answer_offset", "answer_limit", "answer_query"]
            .iter()
            .any(|k| args.get(k).is_some())
        {
            return Err("error: answer_offset and answer_limit require answer_ref".into());
        }
        if args
            .get("answer_query")
            .is_some_and(|q| q.is_string() && q.as_str().is_none())
        {
            return Err(crate::ToolError::InvalidField);
        }
        if let Some(query) = args.get("answer_query")
            && (!query.as_str().is_some_and(|s| (1..=256).contains(&s.len()))
                || args.get("answer_limit").is_some())
        {
            return Err(
                "error: answer_query requires 1-256 UTF-8 bytes and cannot accompany answer_limit"
                    .into(),
            );
        }
        integer(args, "answer_offset", 0, 0, MAX_ANSWER as i64)?;
        integer(
            args,
            "answer_limit",
            PAGE_BYTES as i64,
            4,
            PAGE_BYTES as i64,
        )?;
        if args.get("thread").is_some() {
            if truthy(&args["operation"]) {
                return Err("error: choose thread or operation, not both".into());
            }
            if wait > 0 {
                return Err("error: wait_seconds requires an operation, not a thread".into());
            }
            return self.thread_exchange(args, "updates");
        }
        let after = args
            .get("after")
            .map_or(Some(""), Value::as_str)
            .ok_or("error: after must be a cursor from an operation result")?;
        if !after.is_empty() && !matches("result:[0-9a-f]{64}", after) {
            return Err("error: after must be a cursor from an operation result".into());
        }
        let repo = self.repo(args)?;
        let op = text(args, "operation", 128)?;
        if !operation(op) {
            return Err("error: operation must be an id returned by oms_peer_start".into());
        }
        let dir = Directory::run(&repo, op, false)
            .map_err(|_| format!("error: unknown operation: {op}"))?
            .path;
        let mut payload = json!({"operation":op,"log":dir.join("run.log")});
        let deadline = Instant::now() + Duration::from_secs(wait as u64);
        let (mut state, mut code) = captured.map_or_else(|| status(&dir), |(_, s, c)| (s, c));
        let meta = captured.map_or_else(|| storage::meta(&dir), |(m, _, _)| m.clone());
        let owned = meta["answer_storage"] == "operation";
        if page {
            if state != "done" || !owned {
                return Err("error: answer ranges require a completed operation with operation-owned artifacts".into());
            }
            return answer_page(&repo, op, args, code.unwrap_or(1));
        }
        while state == "running" && wait > 0 {
            if self.interrupted.load(Ordering::Relaxed) {
                return Err("error: interrupted".into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(remaining.min(Duration::from_millis(100)));
            (state, code) = status(&dir);
        }
        cancellation(&dir, state, &mut payload);
        if let Some(n) = age(&dir).filter(|_| {
            std::fs::symlink_metadata(dir.join("prompt.txt")).is_ok_and(|m| m.file_type().is_file())
        }) {
            payload["elapsed_seconds"] = json!(n);
        }
        payload["status"] = json!(state);
        if state != "done" {
            payload["log_tail"] = json!(tail(&dir));
            payload["next"] = json!(if state == "running" {
                "Still running; this read did not cancel or restart the peer. Do authorized independent work, or reuse this operation with wait_seconds=50 (below your host timeout). Avoid status-only loops."
            } else {
                "The run's process is gone and it recorded no exit, so no answer is coming. The log tail says how far it got; start a new consultation if the question still needs one."
            });
            let out = result_response(payload, after)?;
            return if state == "running" {
                Ok(out)
            } else {
                Err(out.into())
            };
        }
        let answers = owned.then(|| dir.join("answers"));
        let (artifacts, thread) = references(&dir, &self.caller_cwd, answers.as_deref());
        let per = OUTPUT_LIMIT.saturating_sub(2 * artifacts.len().saturating_sub(1))
            / artifacts.len().max(1);
        let mut sections = Vec::<String>::new();
        let mut details = Vec::new();
        let mut errors = Vec::<String>::new();
        for path in &artifacts {
            let name = Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let extracted = if owned {
                if Path::new(path).parent() != Some(dir.join("answers").as_path()) {
                    Err("error: artifact is outside this operation".into())
                } else {
                    storage::owned_answer(&repo, op, name)
                }
            } else {
                // Historical logs carry explicit legacy paths. Acquire through a
                // physical parent, keeping the same completed-section parser.
                self.caller_cwd
                    .join(path)
                    .parent()
                    .ok_or_else(|| "error: invalid artifact path".into())
                    .and_then(|p| storage::read(p, name, MAX_ANSWER))
                    .map(|b| storage::sections(&b))
            };
            let (answer, artifact_exit) = match extracted {
                Ok(a) => a,
                Err(_) => {
                    errors.push(
                        "An artifact is missing, unsafe, changed, or exceeds the 8 MiB read limit."
                            .into(),
                    );
                    continue;
                }
            };
            if answer.is_empty() {
                continue;
            }
            let header =
                if artifacts.len() > 1 || (!artifact_exit.is_empty() && artifact_exit != "0") {
                    format!(
                        "--- {name} (exit {}) ---\n",
                        if artifact_exit.is_empty() {
                            "?"
                        } else {
                            &artifact_exit
                        }
                    )
                } else {
                    String::new()
                };
            let preview = storage::bounded(&answer, per.saturating_sub(header.len()));
            let mut detail = json!({"artifact":path,"exit":artifact_exit,"answer_bytes":answer.len(),"preview_bytes":preview.len(),"answer_sha256":storage::hash(&answer),"truncated":preview != answer});
            if owned && !artifact_exit.is_empty() {
                detail["read_arguments"] = json!({"repo":repo,"operation":op,"answer_ref":storage::reference(op,name,&answer)});
            }
            if header.len() > per {
                detail["inline_omitted"] = json!(true);
            } else if !header.is_empty() || !preview.is_empty() {
                sections.push(format!("{header}{preview}"));
            }
            details.push(detail);
        }
        let answer = sections.join("\n\n");
        if owned && !details.is_empty() {
            payload["answer_details"] = json!(details);
        }
        if details.iter().any(|d| d["truncated"] == true) {
            payload["answer_truncated"] = json!(true);
            payload["answer_details"] = json!(details);
            payload["next"] = json!(
                "Answer previews omit evidence. Use read_arguments when present to retrieve ranges; otherwise read the original artifacts before deciding. answer_sha256 identifies the extracted, normalized UTF-8 answer, not the artifact file or its authority. preview_bytes includes truncation markers. inline_omitted means the seat label could not fit; its exit remains in answer_details."
            );
        }
        payload["exit"] = json!(code);
        payload["artifacts"] = json!(artifacts);
        payload["answer"] = json!(answer);
        if !thread.is_empty() {
            payload["thread"] = json!(thread);
        }
        if code != Some(0) || answer.is_empty() {
            payload["log_tail"] = json!(tail(&dir));
        }
        if answer.is_empty() && code == Some(0) && details.is_empty() {
            payload["note"] = json!(
                "the run exited 0 but no answer text could be extracted from its artifacts; judge by the log tail, not by the clean exit"
            );
        }
        let bad = code != Some(0) || !errors.is_empty();
        if !errors.is_empty() {
            payload["answer_errors"] = json!(errors);
        }
        let out = result_response(payload, after)?;
        if bad { Err(out.into()) } else { Ok(out) }
    }
    pub(crate) fn task_dir(&self, id: &Value) -> Result<PathBuf, String> {
        let id = id
            .as_str()
            .filter(|s| operation(s))
            .ok_or("taskId must be an operation id returned by oms_peer_start")?;
        Directory::run(&self.caller_cwd, id, false)
            .map(|d| d.path)
            .map_err(|_| format!("unknown or unsafe taskId: {id}"))
    }
    pub(crate) fn task_snapshot(&self, dir: &Path) -> Result<Value, String> {
        let meta = storage::meta(dir);
        let id = dir
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid taskId")?;
        let (_, inferred) = storage::started(id);
        let created = meta["started_at"]
            .as_text()
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if inferred.is_empty() {
                    "1970-01-01T00:00:00Z".into()
                } else {
                    inferred.into()
                }
            });
        let updated = crate::value::Text::from(updated_stamp(dir, created.as_str().unwrap_or("")));
        let mut value = json!({"resultType":"complete","taskId":id,"createdAt":created,"lastUpdatedAt":updated,"ttlMs":null,"pollIntervalMs":5000});
        let (state, code) = status(dir);
        if state == "done" && storage::valid_receipt(dir, "cancel-observed") {
            value["status"] = json!("cancelled");
            value["statusMessage"] = json!(
                "Provider cancellation was observed and the operation exited. Partial artifacts are retained."
            );
        } else if state == "running" {
            let cancelled = storage::valid_receipt(dir, "cancel-request");
            value["status"] = json!("working");
            value["cancellation_requested"] = json!(cancelled);
            value["statusMessage"] = json!(if cancelled {
                "Cancellation requested; awaiting provider cleanup."
            } else {
                "Peer consultation is running."
            });
        } else if state == "stalled" {
            value["status"] = json!("failed");
            value["statusMessage"] = json!("Peer process ended without a durable exit status.");
            value["error"] = json!({"code":-32603,"message":"peer operation stalled"});
        } else {
            let result =
                self.peer_result_snapshot(&json!({"operation":id}), Some((&meta, state, code)));
            let (text, bad) = match result {
                Ok(s) => (s, false),
                Err(crate::ToolError::Text(s)) => (s, true),
                Err(crate::ToolError::InvalidField) => return Err(crate::FIELD_ERROR.into()),
            };
            value["status"] = json!("completed");
            value["statusMessage"] = json!(format!(
                "Peer consultation completed with exit {}.",
                code.unwrap_or(1)
            ));
            value["result"] = json!({"resultType":"complete","content":[{"type":"text","text":text}],"isError":bad});
        }
        Ok(value)
    }
    pub(crate) fn task_method(&self, method: &str, params: &Value) -> Result<Value, (i32, String)> {
        let dir = self.task_dir(&params["taskId"]).map_err(|e| (-32602, e))?;
        let snapshot = self.task_snapshot(&dir).map_err(|e| (-32602, e))?;
        if method == "tasks/get" {
            return Ok(snapshot);
        }
        if method == "tasks/update" {
            return Err((-32602, "task has no outstanding input requests".into()));
        }
        if snapshot["status"] != "working" {
            return Err((-32602, "task is already terminal".into()));
        }
        if storage::meta(&dir)["cancellation_supported"] != true {
            return Err((-32602,"this legacy or non-POSIX operation has no supervised cancellation; execution state is unchanged".into()));
        }
        let id = params["taskId"].as_str().unwrap();
        let target = Directory::run(&self.caller_cwd, id, true)
            .map_err(|e| (-32603, format!("could not record cancellation: {e}")))?;
        let row = json!({"schema":1,"kind":"mcp-task-cancel-request","task_id":id,"requested_at":storage::now()});
        target
            .atomic("cancel-request.json", format!("{row}\n").as_bytes())
            .map_err(|e| (-32603, format!("could not record cancellation: {e}")))?;
        Ok(json!({"resultType":"complete"}))
    }
}
fn cancellation(dir: &Path, state: &str, payload: &mut Value) {
    if storage::valid_receipt(dir, "cancel-request") {
        payload["cancellation_requested"] = json!(true);
    }
    if state == "done" && storage::valid_receipt(dir, "cancel-observed") {
        payload["termination_reason"] = json!("cancelled");
    }
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.),
    }
}
fn answer_page(repo: &Path, op: &str, args: &Value, code: i64) -> ToolResult {
    let reference = args["answer_ref"].as_str().unwrap();
    let name = reference.rsplit_once(':').unwrap().0;
    let (answer, artifact_exit) = storage::owned_answer(repo, op, name)?;
    if reference != storage::reference(op, name, &answer) {
        return Err("error: answer reference changed or belongs to another operation; refresh the operation result".into());
    }
    if artifact_exit.is_empty() {
        return Err("error: answer has no completed exit section".into());
    }
    let offset = integer(args, "answer_offset", 0, 0, MAX_ANSWER as i64)? as usize;
    if offset > answer.len() || !answer.is_char_boundary(offset) {
        return Err("error: answer_offset must be a UTF-8 boundary within the answer".into());
    }
    let bad = code != 0 || artifact_exit != "0";
    let mut out = json!({"operation":op,"status":"done","exit":code,"artifact_exit":artifact_exit,"answer_ref":reference,"answer_bytes":answer.len()});
    if let Some(query) = args["answer_query"].as_str() {
        let mut rows = Vec::new();
        let mut pos = offset;
        while rows.len() < 8 {
            let Some(i) = answer[pos..].find(query) else {
                break;
            };
            let i = pos + i;
            let mut start = i.saturating_sub(128);
            while !answer.is_char_boundary(start) {
                start += 1;
            }
            let end = (i + query.len() + 128).min(answer.len());
            let snippet = storage::byte_prefix(&answer[start..], end - start);
            rows.push(json!({"answer_offset":i,"context":snippet,"read_arguments":{"repo":repo,"operation":op,"answer_ref":reference,"answer_offset":start}}));
            pos = i + query.len();
        }
        let more = answer[pos..].contains(query);
        out["matches"] = json!(rows);
        out["has_more"] = json!(more);
        if more {
            out["next_search_arguments"] = json!({"repo":repo,"operation":op,"answer_ref":reference,"answer_query":query,"answer_offset":pos});
        }
    } else {
        let limit = integer(
            args,
            "answer_limit",
            PAGE_BYTES as i64,
            4,
            PAGE_BYTES as i64,
        )? as usize;
        let page = storage::byte_prefix(&answer[offset..], limit);
        let end = offset + page.len();
        out["answer"] = json!(page);
        out["answer_offset"] = json!(offset);
        out["answer_end"] = json!(end);
        out["answer_sha256"] = json!(storage::hash(&answer));
        out["has_more"] = json!(end < answer.len());
        if end < answer.len() {
            out["next_read_arguments"] = json!({"repo":repo,"operation":op,"answer_ref":reference,"answer_offset":end,"answer_limit":limit});
        }
    }
    if bad {
        Err(pretty(&out).into())
    } else {
        Ok(pretty(&out))
    }
}

pub(crate) fn owner(arguments: &[OsString]) -> io::Result<()> {
    if !(3..=4).contains(&arguments.len()) {
        return Err(io::Error::other(
            "private owner requires source, repository, operation and optional oms program",
        ));
    }
    if delegated() {
        return Err(io::Error::other(
            "delegated worker cannot own a peer launch",
        ));
    }
    let source = PathBuf::from(&arguments[0]).canonicalize()?;
    let repo = PathBuf::from(&arguments[1]).canonicalize()?;
    let op = arguments[2]
        .to_str()
        .ok_or_else(|| io::Error::other("invalid operation"))?;
    let dir = Directory::run(&repo, op, true).map_err(io::Error::other)?;
    let _owner_lock = dir.lock().map_err(io::Error::other)?;
    if dir.path.join("status").symlink_metadata().is_ok() {
        return Err(io::Error::other("operation is already terminal"));
    }
    if storage::read_run(&dir.path, "pid", 4096).is_ok() {
        return Err(io::Error::other("operation already has an owner"));
    }
    rustix::process::setsid()?;
    let pin = arguments.get(3).map(PathBuf::from);
    let result = owner_run(pin.as_deref(), &source, &repo, op, &dir);
    if let Err(e) = &result {
        eprintln!("error: native peer owner: {e}");
        let _ = dir.atomic("status", b"1\n");
    }
    result
}
fn owner_run(
    pin: Option<&Path>,
    source: &Path,
    repo: &Path,
    op: &str,
    dir: &Directory,
) -> io::Result<()> {
    fn fail(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
        io::Error::other(e)
    }
    // Resolve before any running-state file: `pid` means the launch is viable.
    let program = crate::command::resolve_oms(pin, &std::env::current_exe().unwrap_or_default())
        .map_err(fail)?;
    let launch: Value = crate::value::from_slice(
        &storage::read_run(&dir.path, "launch.json", 128 * 1024).map_err(fail)?,
    )
    .map_err(fail)?;
    let kind = launch["kind"]
        .as_str()
        .ok_or_else(|| fail("missing launch kind"))?;
    if !["consult", "advise", "ask"].contains(&kind) || !op.starts_with(&format!("{kind}-")) {
        return Err(fail("invalid launch kind"));
    }
    let prompt_bytes = storage::read_run(&dir.path, "prompt.txt", 64 * 1024 + 1).map_err(fail)?;
    let prompt = std::str::from_utf8(&prompt_bytes)
        .map_err(fail)?
        .trim_matches(crate::whitespace);
    if prompt.is_empty() || prompt.len() > 64 * 1024 || (kind == "ask" && prompt.contains('\0')) {
        return Err(fail("invalid stored prompt"));
    }
    let list = launch["targets"]
        .as_array()
        .ok_or_else(|| fail("invalid stored targets"))?;
    let mut selected = Vec::new();
    for value in list {
        let target = value
            .as_str()
            .ok_or_else(|| fail("invalid stored target"))?;
        let canonical = targets(target, repo).map_err(fail)?;
        if canonical.len() != 1 || canonical[0] != target {
            return Err(fail("stored target no longer registered"));
        }
        selected.push(target.to_owned());
    }
    if kind == "advise" && selected.len() > 1 {
        return Err(fail("advise requires one target"));
    }
    let thread = launch["thread"]
        .as_str()
        .ok_or_else(|| fail("invalid stored thread"))?;
    if !thread.is_empty() && !thread_id(thread) {
        return Err(fail("invalid stored thread"));
    }
    let new = launch["new_thread"]
        .as_bool()
        .ok_or_else(|| fail("invalid stored new_thread"))?;
    if new && (kind != "consult" || !thread.is_empty()) {
        return Err(fail("invalid stored thread mode"));
    }
    let rounds = integer(&launch, "debate_rounds", 0, 0, 3).map_err(fail)?;
    if rounds > 0 && kind != "ask" {
        return Err(fail("invalid stored debate rounds"));
    }
    let mut argv: Vec<OsString> = vec![
        if kind == "ask" {
            "peer-ask".into()
        } else {
            kind.into()
        },
        "--repo".into(),
        repo.as_os_str().to_owned(),
        "--artifact-dir".into(),
        dir.path.join("answers").into_os_string(),
    ];
    let context = launch["context_options"]
        .as_object()
        .ok_or_else(|| fail("invalid stored context"))?;
    for key in ["include_memory", "include_task"] {
        if let Some(value) = context.get(key) {
            let enabled = value
                .as_bool()
                .ok_or_else(|| fail("invalid stored context option"))?;
            if key == "include_task" && enabled && kind == "consult" {
                continue;
            }
            argv.push(
                format!(
                    "--{}{}",
                    if enabled { "" } else { "no-" },
                    key.strip_prefix("include_").unwrap()
                )
                .into(),
            );
        }
    }
    if context.keys().any(|k| {
        !k.as_str()
            .is_some_and(|k| ["include_memory", "include_task"].contains(&k))
    }) {
        return Err(fail("unknown stored context option"));
    }
    if !thread.is_empty() {
        argv.extend(["--thread".into(), thread.into()]);
    }
    if let Some(field) = launch.get("preserve_current") {
        let preserve = field
            .as_bool()
            .ok_or_else(|| fail("invalid stored preserve_current"))?;
        if preserve {
            if kind != "ask" || thread != op {
                return Err(fail("invalid stored non-selecting thread"));
            }
            argv.push("--no-current".into());
        }
    }
    if new {
        argv.push("--new-thread".into());
    }
    if kind == "ask" {
        argv.extend(["--prompt".into(), prompt.into()]);
        if rounds > 0 {
            argv.extend(["--debate".into(), rounds.to_string().into()]);
        }
        if !selected.is_empty() {
            argv.extend(["--providers".into(), selected.join(",").into()]);
        }
    } else {
        argv.extend([
            "--prompt-file".into(),
            dir.path.join("prompt.txt").into_os_string(),
        ]);
        for target in selected {
            argv.extend(["--to".into(), target.into()]);
        }
    }
    if delegated() {
        return Err(fail("delegation authority changed before launch"));
    }
    // Cooperative cancellation and per-seat deadlines remain the fixed OMS
    // verb's existing contract. The owner never signals a pid read from disk.
    // A pre-start request prevents the downstream command entirely.
    if storage::valid_receipt(&dir.path, "cancel-request") {
        dir.atomic(
            "cancel-observed.json",
            format!(
                "{}\n",
                json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-observed"})
            )
            .as_bytes(),
        )
        .map_err(fail)?;
        dir.atomic("pid", format!("{}\n", std::process::id()).as_bytes())
            .map_err(fail)?;
        dir.atomic("status", b"125\n").map_err(fail)?;
        return Ok(());
    }
    dir.atomic("pid", format!("{}\n", std::process::id()).as_bytes())
        .map_err(fail)?;
    let mut cmd = Command::new(&program);
    // The staged native CLI validates its manifest, revision and its own executable
    // before entering provider verbs. Preserve this proof in the detached owner.
    if let Some(revision) = program.parent().and_then(Path::file_name)
        && source.join("target/oms-native").join(revision).join("oms") == program
    {
        cmd.arg("--oms-dispatch")
            .arg(source)
            .arg(revision)
            .arg("--");
    }
    let mut child = cmd
        .args(argv)
        .current_dir(repo)
        .env("OMS_PEER_CANCEL_DIR", &dir.path)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let exit = match child.wait() {
        Ok(s) => {
            use std::os::unix::process::ExitStatusExt;
            s.code()
                .map(i64::from)
                .unwrap_or_else(|| -i64::from(s.signal().unwrap_or(1)))
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    dir.atomic("status", format!("{exit}\n").as_bytes())
        .map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oms_command_operations::peer_operations::status_with;
    use rustix::process::Pid;

    #[test]
    fn peer_status_observes_atomic_publication_before_reporting_failure_or_stall() {
        use std::cell::Cell;
        use std::os::unix::fs::{DirBuilderExt, symlink};

        struct Temp(PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("oms-peer-terminal-status-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        let repo = Temp(path);
        let dir = Directory::open(
            &repo.0,
            &format!("{RUN_ROOT}/advise-20261123T215445Z-aabbccdd"),
            true,
            true,
        )
        .unwrap();
        let read = || storage::read_run(&dir.path, "status", 4096);
        let status_path = dir.path.join("status");

        let first = Cell::new(true);
        assert_eq!(
            status_with(
                &dir.path,
                || {
                    let result = read();
                    if first.replace(false) {
                        assert!(result.is_err());
                        assert!(!status_path.exists());
                        dir.atomic("status", b"0\n").unwrap();
                    }
                    result
                },
                |_| panic!("published terminal status should precede the owner check"),
            ),
            ("done", Some(0))
        );

        std::fs::remove_file(&status_path).unwrap();
        assert_eq!(status(&dir.path), ("running", None));
        dir.atomic("pid", b"123\n").unwrap();
        assert_eq!(status_with(&dir.path, read, |_| true), ("stalled", None));
        assert_eq!(
            status_with(&dir.path, read, |pid| {
                assert_eq!(Some(pid), Pid::from_raw(123));
                dir.atomic("status", b"7\n").unwrap();
                true
            }),
            ("done", Some(7))
        );

        dir.atomic("status", b"malformed\n").unwrap();
        assert_eq!(status(&dir.path), ("done", Some(1)));
        std::fs::remove_file(&status_path).unwrap();
        dir.atomic("other", b"0\n").unwrap();
        symlink(dir.path.join("other"), &status_path).unwrap();
        assert!(read().is_err());
        assert_eq!(status(&dir.path), ("done", Some(1)));
    }

    #[test]
    fn canonical_provider_aliases_and_cursor_bytes() {
        let aliases: Value =
            crate::value::from_str(include_str!("../tests/provider-aliases.json")).unwrap();
        for row in aliases.as_array().unwrap() {
            assert_eq!(
                json!(targets(row["input"].as_str().unwrap(), Path::new(".")).unwrap()),
                row["expected"]
            );
        }
        for bad in [
            "--help",
            "codex:effort=high",
            "codex:model=",
            "codex:model=x\ny",
            "claude:model=x\0y",
        ] {
            assert!(targets(bad, Path::new(".")).is_err());
        }
        let cursors: Value =
            crate::value::from_str(include_str!("../tests/result-cursors.json")).unwrap();
        for row in cursors.as_array().unwrap() {
            let result: Value = crate::value::from_slice(
                result_response(row["payload"].clone(), row["after"].as_str().unwrap())
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
            assert_eq!(result, row["expected"]);
        }
    }
}
