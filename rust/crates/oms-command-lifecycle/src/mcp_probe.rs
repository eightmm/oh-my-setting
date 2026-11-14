use crate::runtime::{self as r, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 4 * 1024 * 1024;
struct Session<'a> {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    buffer: Vec<u8>,
    total: usize,
    deadline: Instant,
    flag: &'a AtomicBool,
}
impl Drop for Session<'_> {
    fn drop(&mut self) {
        if let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Session<'_> {
    fn check(&self) -> Result<()> {
        r::check(self.flag)?;
        if Instant::now() >= self.deadline {
            return Err(r::Error(
                124,
                "MCP probe timed out; nothing recorded".into(),
            ));
        }
        Ok(())
    }
    fn send(&mut self, value: Value) -> Result<()> {
        // MCP stdio is newline-delimited compact JSON, never pretty JSON.
        let mut raw = serde_json::to_vec(&value)
            .map_err(|_| r::Error(2, "cannot encode MCP request".into()))?;
        raw.push(b'\n');
        let mut pos = 0;
        while pos < raw.len() {
            self.check()?;
            match self.input.write(&raw[pos..]) {
                Ok(0) => return r::fail("MCP server closed input"),
                Ok(n) => pos += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => return r::fail("MCP server input failed"),
            }
        }
        Ok(())
    }
    fn reply(&mut self, id: u64) -> Result<Value> {
        loop {
            self.check()?;
            if let Some(n) = self.buffer.iter().position(|b| *b == b'\n') {
                let raw = self.buffer.drain(..=n).collect::<Vec<_>>();
                let Ok(value) = r::parse(&raw) else { continue };
                if value.get("id").and_then(Value::as_u64) != Some(id)
                    || value.get("method").is_some()
                {
                    continue;
                }
                if value.get("error").is_some() {
                    return r::fail("MCP server rejected request; response not printed");
                }
                return value
                    .get("result")
                    .filter(|v| v.is_object())
                    .cloned()
                    .ok_or_else(|| r::Error(2, "MCP response has no result object".into()));
            }
            let mut chunk = [0u8; 65536];
            match self.output.read(&mut chunk) {
                Ok(0) => return r::fail("MCP server closed output"),
                Ok(n) => {
                    self.total += n;
                    if self.total > MAX_BYTES {
                        return r::fail("MCP probe exceeded 4 MiB output limit");
                    }
                    self.buffer.extend_from_slice(&chunk[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => return r::fail("MCP server output failed"),
            }
        }
    }
}
pub(crate) fn run(spec: &Value, cwd: &Path, flag: &AtomicBool) -> Result<Value> {
    r::check(flag)?;
    if ["url", "httpUrl", "serverUrl"]
        .iter()
        .any(|k| spec.get(k).is_some())
        || spec
            .get("type")
            .or_else(|| spec.get("transport"))
            .is_some_and(|v| v != "stdio")
        || spec["disabled"] == true
        || spec["enabled"] == false
    {
        return r::fail("MCP probe requires an enabled local stdio server");
    }
    let name = spec["command"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && !s.contains('\0'))
        .ok_or_else(|| r::Error(2, "MCP probe requires a command".into()))?;
    let args = match spec.get("args") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .ok_or_else(|| r::Error(2, "MCP arguments must be an array".into()))?
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| !s.contains('\0'))
                    .ok_or_else(|| r::Error(2, "MCP arguments must be strings".into()))
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let mut command = Command::new(name);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for key in ["PATH", "LANG", "TMPDIR"] {
        if let Some(v) = std::env::var_os(key) {
            command.env(key, v);
        }
    }
    if let Some(vars) = spec.get("env") {
        for (key, value) in vars
            .as_object()
            .ok_or_else(|| r::Error(2, "MCP environment must be an object".into()))?
        {
            let value = value
                .as_str()
                .ok_or_else(|| r::Error(2, "MCP environment values must be strings".into()))?;
            if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                return r::fail("invalid MCP environment entry");
            }
            command.env(key, value);
        }
    }
    let seconds = match std::env::var("OMS_MCP_PROBE_TIMEOUT") {
        Ok(s) => s
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .unwrap_or(15.0)
            .clamp(1.0, 120.0),
        Err(_) => 15.0,
    };
    let mut child = command.spawn().map_err(|_| {
        r::Error(
            2,
            "could not start MCP probe; command details withheld".into(),
        )
    })?;
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let mut io = Session {
        child,
        input,
        output,
        buffer: Vec::new(),
        total: 0,
        deadline: Instant::now() + Duration::from_secs_f64(seconds),
        flag,
    };
    for fd in [
        &io.input as &dyn std::os::fd::AsFd,
        &io.output as &dyn std::os::fd::AsFd,
    ] {
        let flags =
            rustix::fs::fcntl_getfl(fd).map_err(|_| r::Error(2, "cannot bound MCP pipe".into()))?;
        rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(|_| r::Error(2, "cannot bound MCP pipe".into()))?;
    }
    io.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"oms-doctor-mcp-probe","version":"1"}}}))?;
    io.reply(1)?;
    io.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
    let mut tools = BTreeMap::<String, String>::new();
    let mut cursor = None;
    for page in 0..20 {
        let id = page + 2;
        let params = cursor
            .as_ref()
            .map(|c| json!({"cursor":c}))
            .unwrap_or(json!({}));
        io.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":params}))?;
        let result = io.reply(id)?;
        let rows = result["tools"]
            .as_array()
            .ok_or_else(|| r::Error(2, "MCP tools/list returned no array".into()))?;
        for tool in rows {
            let name = tool["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| r::Error(2, "MCP tool definition has no name".into()))?;
            let raw = r::canonical_ascii(tool)?;
            if tools.insert(name.into(), r::hash(&raw)).is_some() {
                return r::fail("MCP server returned a duplicate tool name");
            }
            if tools.len() > 500 {
                return r::fail("MCP server exceeds 500 tools");
            }
        }
        cursor = match result.get("nextCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => return r::fail("invalid MCP continuation cursor"),
        };
        if cursor.is_none() {
            return serde_json::to_value(tools)
                .map_err(|_| r::Error(2, "cannot encode MCP tool hashes".into()));
        }
    }
    r::fail("MCP tools/list exceeds 20 pages")
}

pub(crate) fn codex_models(binary: &Path, cwd: &Path, flag: &AtomicBool) -> Result<Value> {
    let mut command = Command::new(binary);
    command
        .arg("app-server")
        .current_dir(cwd)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let mut io = Session {
        child,
        input,
        output,
        buffer: Vec::new(),
        total: 0,
        deadline: Instant::now() + Duration::from_secs(40),
        flag,
    };
    for fd in [
        &io.input as &dyn std::os::fd::AsFd,
        &io.output as &dyn std::os::fd::AsFd,
    ] {
        let flags = rustix::fs::fcntl_getfl(fd)
            .map_err(|_| r::Error(2, "cannot bound catalog pipe".into()))?;
        rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(|_| r::Error(2, "cannot bound catalog pipe".into()))?;
    }
    io.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"oh-my-setting","title":"oh-my-setting","version":"1"}}}))?;
    io.reply(1)?;
    io.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
    let mut rows = Vec::new();
    let mut cursor = None;
    for page in 0..20 {
        let id = page + 2;
        let params = cursor
            .as_ref()
            .map(|c| json!({"cursor":c}))
            .unwrap_or(json!({}));
        io.send(json!({"jsonrpc":"2.0","id":id,"method":"model/list","params":params}))?;
        let result = io.reply(id)?;
        for row in result["data"]
            .as_array()
            .ok_or_else(|| r::Error(2, "invalid model catalog".into()))?
        {
            if row["hidden"] == true {
                continue;
            }
            let name = row["id"]
                .as_str()
                .or_else(|| row["model"].as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| r::Error(2, "model catalog has no identity".into()))?;
            rows.push(
                json!({"id":name,"supportedReasoningEfforts":row["supportedReasoningEfforts"]}),
            );
            if rows.len() > 500 {
                return r::fail("model catalog exceeds 500 entries");
            }
        }
        cursor = result["nextCursor"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if cursor.is_none() {
            return Ok(json!(rows));
        }
    }
    r::fail("model catalog exceeds 20 pages")
}

pub(crate) fn cached(spec: &Value, cwd: &Path, flag: &AtomicBool) -> Result<Value> {
    let Some(dir) = std::env::var_os("OMS_DOCTOR_PROBE_DIR") else {
        return run(spec, cwd, flag);
    };
    let dir = std::path::PathBuf::from(dir);
    if dir.is_symlink() || !dir.is_absolute() {
        return r::fail("doctor probe cache must be a physical absolute directory");
    }
    r::private_dir(&dir)?;
    let executable = spec["command"]
        .as_str()
        .and_then(|s| r::find(s, cwd))
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            json!([m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec()])
        });
    let material = r::bytes(
        &json!({"spec":spec,"cwd":cwd,"executable":executable,"bound":std::env::var("OMS_MCP_PROBE_TIMEOUT").ok()}),
    )?;
    let file = dir.join(format!("doctor-mcp-probe-{}.json", r::hash(&material)));
    let _guard = r::lock(&file, cwd)?;
    if r::exists(&file) {
        let row = r::value(&file)?;
        if row["schema"] != 1
            || row["tools"].as_object().is_none_or(|m| {
                m.len() > 500
                    || m.values().any(|v| {
                        v.as_str().is_none_or(|s| {
                            s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                    })
            })
        {
            return r::fail("invalid MCP probe cache");
        }
        return Ok(row["tools"].clone());
    }
    let tools = run(spec, cwd, flag)?;
    r::atomic(&file, &r::bytes(&json!({"schema":1,"tools":tools}))?)?;
    Ok(tools)
}
