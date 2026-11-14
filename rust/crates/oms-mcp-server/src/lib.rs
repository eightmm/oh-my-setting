#![forbid(unsafe_code)]
//! Newline JSON-RPC transport and the fixed OMS MCP surface.
//!
//! The embedding executable supplies trusted source/caller paths and private
//! peer-owner location. Request arguments never select an executable.

#[macro_use]
pub mod value;
mod command;
pub use command::{Captured, CommandExecutor, CommandRequest, SubprocessExecutor};
mod peer;
mod storage;

use crate::value::{Text, Value};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
pub(crate) const OUTPUT_LIMIT: usize = 60_000;
pub(crate) const MAX_ANSWER: usize = 8 * 1024 * 1024;
pub(crate) const PAGE_BYTES: usize = 16 * 1024;
pub(crate) const RUN_ROOT: &str = ".oms/artifacts/mcp";
const MODERN: &str = "2026-07-28";
const FALLBACK: &str = "2025-06-18";
const PROTOCOLS: [&str; 3] = [MODERN, FALLBACK, "2025-03-26"];
const META_PROTOCOL: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const EXTENSION: &str = "io.modelcontextprotocol/tasks";
const GRAPH_URI: &str = "ui://oms/project-graph/v1.html";
const MIME: &str = "text/html;profile=mcp-app";
const CORE: [&str; 12] = [
    "oms_inbox",
    "oms_repo_state",
    "oms_handoffs",
    "oms_handoff_show",
    "oms_journal",
    "oms_peer_start",
    "oms_peer_result",
    "oms_peer_operations",
    "oms_project_graph_render",
    "oms_project_graph_query",
    "oms_project_graph_trace",
    "oms_project_graph_affected",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolProfile {
    Core,
    Full,
}

/// Immutable process configuration; it confers no request or worker authority.
#[derive(Clone, Debug)]
pub struct Config {
    pub profile: ToolProfile,
    pub tasks_extension: bool,
    /// Trusted private executable invoking [`peer_owner`] with its OS arguments.
    /// This is a bundled native helper, never an arbitrary command from a client.
    pub peer_owner: PathBuf,
    /// Absolute native `oms` executable pinned by the host. When set it is the
    /// only program MCP verbs and the peer owner run; no search is attempted.
    pub oms_program: Option<PathBuf>,
}
impl Config {
    pub fn from_env(peer_owner: PathBuf) -> Result<Self, &'static str> {
        let profile = match std::env::var("OMS_MCP_TOOL_PROFILE").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("core") => ToolProfile::Core,
            Ok("full") => ToolProfile::Full,
            _ => return Err("error: OMS_MCP_TOOL_PROFILE must be core or full"),
        };
        Ok(Self {
            profile,
            tasks_extension: std::env::var("OMS_MCP_TASKS_EXTENSION").as_deref() == Ok("1"),
            peer_owner,
            oms_program: None,
        })
    }
}

pub(crate) fn tools() -> &'static [Value] {
    static TOOLS: OnceLock<Vec<Value>> = OnceLock::new();
    TOOLS
        .get_or_init(|| {
            crate::value::from_str(include_str!("tools.json"))
                .expect("compiled canonical definitions")
                .as_array()
                .unwrap()
                .clone()
        })
        .as_slice()
}
fn definitions(profile: ToolProfile) -> Value {
    Value::Array(tools().iter().filter(|t| profile == ToolProfile::Full || CORE.contains(&t["name"].as_str().unwrap())).map(|t| {
        let mut d = json!({"name":t["name"], "description":t["description"], "inputSchema":{"type":"object","properties":t["properties"],"required":t.get("required").cloned().unwrap_or_else(|| json!([]))}});
        for key in ["annotations", "title", "outputSchema"] {
            if let Some(v) = t.get(key) { d[key] = v.clone(); }
        }
        if let Some(uri) = t.get("ui_resource") {
            d["_meta"] = json!({"ui":{"resourceUri":uri},"openai/outputTemplate":uri,"openai/toolInvocation/invoking":"Rendering project graph…","openai/toolInvocation/invoked":"Project graph ready."});
        }
        d
    }).collect())
}
fn meta(params: &Value) -> &Value {
    &params["_meta"]
}
fn reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
pub(crate) fn error(id: &Value, code: i32, message: impl Into<String>) -> Value {
    let message: String = message.into();
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
#[derive(Debug)]
pub(crate) enum ToolError {
    Text(Text),
    InvalidField,
}
impl From<String> for ToolError {
    fn from(s: String) -> Self {
        Self::Text(s.into())
    }
}
impl From<&str> for ToolError {
    fn from(s: &str) -> Self {
        Self::Text(s.into())
    }
}
impl From<Text> for ToolError {
    fn from(s: Text) -> Self {
        Self::Text(s)
    }
}
pub(crate) type ToolResult = Result<Text, ToolError>;
pub(crate) const FIELD_ERROR: &str = "invalid or oversized request field";
pub(crate) fn pretty(value: &Value) -> Text {
    value::encode(value, false, true, false, false)
}
pub(crate) fn text<'a>(args: &'a Value, name: &str, limit: usize) -> Result<&'a str, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(s)) => {
            let s = s
                .as_str()
                .ok_or_else(|| format!("error: {name} must be valid UTF-8 text"))?;
            if s.len() > limit {
                return Err(format!("error: {name} exceeds {limit}-byte limit"));
            }
            if s.contains('\0') {
                return Err(format!("error: {name} contains a NUL byte"));
            }
            Ok(s)
        }
        _ => Err(format!("error: {name} must be a string")),
    }
}
pub(crate) fn integer(
    args: &Value,
    name: &str,
    default: i64,
    low: i64,
    high: i64,
) -> Result<i64, String> {
    match args.get(name) {
        None => Ok(default),
        Some(v) if v.as_i64().is_some_and(|n| (low..=high).contains(&n)) => Ok(v.as_i64().unwrap()),
        _ => Err(format!(
            "error: {name} must be an integer from {low} to {high}"
        )),
    }
}
pub(crate) fn matches(pattern: &str, s: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    static PATTERNS: OnceLock<Mutex<HashMap<String, Arc<regex::Regex>>>> = OnceLock::new();
    let mut cache = PATTERNS
        .get_or_init(Mutex::default)
        .lock()
        .expect("pattern cache");
    let compiled = cache
        .entry(pattern.into())
        .or_insert_with(|| {
            Arc::new(regex::Regex::new(&format!("\\A(?:{pattern})\\z")).expect("fixed pattern"))
        })
        .clone();
    drop(cache);
    compiled.is_match(s)
}
pub(crate) fn answer_name(s: &str) -> bool {
    use unicode_general_category::{GeneralCategory::*, get_general_category};
    s.strip_suffix(".md").is_some_and(|stem| {
        (1..=240).contains(&stem.chars().count())
            && stem.chars().all(|c| {
                matches!(c, '.' | '-' | '_')
                    || matches!(
                        get_general_category(c),
                        UppercaseLetter
                            | LowercaseLetter
                            | TitlecaseLetter
                            | ModifierLetter
                            | OtherLetter
                            | DecimalNumber
                            | LetterNumber
                            | OtherNumber
                    )
            })
    })
}
pub(crate) fn operation(s: &str) -> bool {
    matches("[a-z]+-[0-9TZ]+-[0-9a-f]+", s)
}
pub(crate) fn thread_id(s: &str) -> bool {
    matches("[A-Za-z0-9._][A-Za-z0-9._-]{0,127}", s)
}
pub(crate) fn delegated() -> bool {
    std::env::var("OMS_HARNESS_CHILD").as_deref() == Ok("1")
        || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH").is_some_and(|value| value != "0")
}
pub(crate) fn whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
pub(crate) fn prefix_chars(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

/// One persistent session. Tool definitions and trusted version are captured
/// once; mutable authority, receipts and run state are acquired per action.
pub struct Server<'a> {
    pub(crate) source_root: PathBuf,
    pub(crate) caller_cwd: PathBuf,
    pub(crate) interrupted: &'a AtomicBool,
    pub(crate) config: Config,
    pub(crate) executor: Arc<dyn CommandExecutor>,
    protocol: &'static str,
    info: Value,
    definitions: Value,
}
impl<'a> Server<'a> {
    pub fn new(
        source_root: &Path,
        caller_cwd: &Path,
        interrupted: &'a AtomicBool,
        config: Config,
    ) -> io::Result<Self> {
        let executor = Arc::new(
            SubprocessExecutor::resolve(config.oms_program.as_deref(), &config.peer_owner)
                .map_err(io::Error::other)?,
        );
        Self::with_executor(source_root, caller_cwd, interrupted, config, executor)
    }
    /// Like [`Server::new`], with the host supplying how OMS verbs run.
    pub fn with_executor(
        source_root: &Path,
        caller_cwd: &Path,
        interrupted: &'a AtomicBool,
        config: Config,
        executor: Arc<dyn CommandExecutor>,
    ) -> io::Result<Self> {
        let source_root = source_root.canonicalize()?;
        let caller_cwd = caller_cwd.canonicalize()?;
        if !source_root.is_dir() || !caller_cwd.is_dir() {
            return Err(io::Error::other("source and caller must be directories"));
        }
        let version = std::fs::read_to_string(source_root.join("VERSION"))
            .unwrap_or_else(|_| "unknown".into());
        let definitions = definitions(config.profile);
        Ok(Self {
            source_root,
            caller_cwd,
            interrupted,
            config,
            executor,
            protocol: FALLBACK,
            info: json!({"name":"oh-my-setting","version":version.trim_matches(crate::whitespace)}),
            definitions,
        })
    }
    pub(crate) fn repo(&self, args: &Value) -> Result<PathBuf, ToolError> {
        let raw = text(args, "repo", 4096)?;
        let path = if raw.is_empty() {
            self.caller_cwd.clone()
        } else {
            self.caller_cwd.join(raw)
        };
        if !path.is_dir() {
            let label = if raw.is_empty() {
                self.caller_cwd.as_path()
            } else {
                Path::new(raw)
            };
            return Err(Text::join(&[
                "error: no such repository directory: ".into(),
                Text::path(label.as_os_str()),
            ])
            .into());
        }
        Ok(path
            .canonicalize()
            .map_err(|e| format!("error: invalid repository path: {e}"))?)
    }
    fn protocol(&self, params: &Value) -> &'static str {
        PROTOCOLS
            .into_iter()
            .find(|p| Some(*p) == meta(params)[META_PROTOCOL].as_str())
            .unwrap_or(self.protocol)
    }
    fn capabilities(&self, protocol: &str) -> Value {
        let mut c = json!({"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false}});
        if self.config.tasks_extension && protocol == MODERN {
            c["extensions"] = json!({EXTENSION:{}});
        }
        c
    }
    fn task_capable(&self, params: &Value) -> bool {
        self.config.tasks_extension
            && self.protocol(params) == MODERN
            && meta(params)[META_CAPABILITIES]["extensions"]
                .as_object()
                .is_some_and(|e| e.contains_key(EXTENSION))
    }
    fn complete(&self, mut value: Value, params: &Value, cacheable: bool) -> Value {
        if self.protocol(params) == MODERN {
            if value.get("resultType").is_none() {
                value["resultType"] = json!("complete");
            }
            if cacheable {
                value["ttlMs"] = json!(300_000);
                value["cacheScope"] = json!("private");
            }
        }
        value
    }
    /// Handle a parsed record. A null or absent id is a notification, matching
    /// the canonical handler even when the method could otherwise mutate state.
    pub fn handle(&mut self, message: &impl value::ToValue) -> Option<Value> {
        self.handle_value(&message.to_value())
    }
    fn handle_value(&mut self, message: &Value) -> Option<Value> {
        let null = Value::Null;
        if !message.is_object() {
            return Some(error(&null, -32600, "invalid request: expected object"));
        }
        let id = &message["id"];
        if id.is_boolean() || id.is_object() || id.is_array() {
            return Some(error(&null, -32600, "invalid request: invalid id"));
        }
        if id.is_string() && id.as_str().is_none() {
            return Some(error(&null, -32600, "invalid request: invalid id"));
        }
        if id.as_str().is_some_and(|s| s.len() > 1024) {
            return Some(error(&null, -32600, "invalid request: id is too long"));
        }
        if message["method"].is_string() && message["method"].as_str().is_none() {
            return Some(error(id, -32600, "invalid request: invalid method"));
        }
        let Some(method) = message["method"].as_str() else {
            return Some(error(
                id,
                -32600,
                "invalid request: method must be a string",
            ));
        };
        if method.len() > 128 {
            return Some(error(id, -32600, "invalid request: method is too long"));
        }
        if id.is_null() {
            return None;
        }
        let params = &message["params"];
        if let Some(p) = meta(params)[META_PROTOCOL].as_text()
            && !p.as_str().is_some_and(|p| PROTOCOLS.contains(&p))
        {
            let mut v = error(id, -32022, "Unsupported protocol version");
            v["error"]["data"] = json!({"supported":PROTOCOLS,"requested":p.prefix(64)});
            return Some(v);
        }
        let invalid_params = || error(id, -32602, "invalid params: expected object");
        let result = match method {
            "server/discover" => {
                if !params.is_null() && !params.is_object() {
                    return Some(invalid_params());
                }
                json!({"resultType":"complete","supportedVersions":PROTOCOLS,"capabilities":self.capabilities(MODERN),"_meta":{"io.modelcontextprotocol/serverInfo":self.info},"ttlMs":300_000,"cacheScope":"private"})
            }
            "initialize" => {
                if !params.is_null() && !params.is_object() {
                    return Some(invalid_params());
                }
                self.protocol = PROTOCOLS
                    .into_iter()
                    .find(|p| Some(*p) == params["protocolVersion"].as_str())
                    .unwrap_or(FALLBACK);
                self.complete(json!({"protocolVersion":self.protocol,"capabilities":self.capabilities(self.protocol),"serverInfo":self.info}),params,false)
            }
            "ping" => self.complete(json!({}), params, false),
            "tools/list" => self.complete(json!({"tools":self.definitions}), params, true),
            "resources/list" => {
                if !params.is_null() && !params.is_object() {
                    return Some(invalid_params());
                }
                self.complete(json!({"resources":[{"uri":GRAPH_URI,"name":"oms-project-graph","title":"OMS Project Graph","description":"Interactive bounded project graph with editable Codex instruction drafts.","mimeType":MIME}]}),params,true)
            }
            "resources/read" => {
                if !params.is_object() {
                    return Some(invalid_params());
                }
                if params["uri"].as_str() != Some(GRAPH_URI) {
                    return Some(error(
                        id,
                        -32602,
                        format!("unknown resource: {}", storage::repr(&params["uri"])),
                    ));
                }
                self.complete(json!({"contents":[{"uri":GRAPH_URI,"mimeType":MIME,"text":include_str!("project-graph.html"),"_meta":{"ui":{"prefersBorder":true,"csp":{"resourceDomains":["https://cdn.jsdelivr.net"]}}}}]}),params,true)
            }
            "tasks/get" | "tasks/update" | "tasks/cancel" => {
                if !self.config.tasks_extension || self.protocol(params) != MODERN {
                    return Some(error(id, -32601, format!("method not found: {method}")));
                }
                if !params.is_object() {
                    return Some(invalid_params());
                }
                if !self.task_capable(params) {
                    let mut e = error(
                        id,
                        -32021,
                        format!("missing required client capability: {EXTENSION}"),
                    );
                    e["error"]["data"] =
                        json!({"requiredCapabilities":{"extensions":{EXTENSION:{}}}});
                    return Some(e);
                }
                match self.task_method(method, params) {
                    Ok(v) => v,
                    Err((code, e)) => return Some(error(id, code, e)),
                }
            }
            "tools/call" => {
                if !params.is_null() && !params.is_object() {
                    return Some(invalid_params());
                }
                let Some(name) = params["name"].as_str().filter(|n| n.len() <= 128) else {
                    return Some(error(id, -32602, "invalid tool name"));
                };
                let empty = json!({});
                let args = match params.get("arguments") {
                    None | Some(Value::Null) => &empty,
                    Some(v) if v.is_object() => v,
                    _ => return Some(error(id, -32602, "invalid arguments: expected object")),
                };
                let Some(tool) = tools().iter().find(|t| t["name"] == name) else {
                    return Some(error(
                        id,
                        -32602,
                        format!("unknown tool: {}", storage::repr(&json!(name))),
                    ));
                };
                let (text, bad, graph) = if name == "oms_project_graph_render" {
                    match self.graph(args) {
                        Ok((text, graph)) => (text, false, Some(graph)),
                        Err(ToolError::Text(text)) => (text, true, None),
                        Err(ToolError::InvalidField) => {
                            return Some(error(id, -32602, FIELD_ERROR));
                        }
                    }
                } else {
                    let output = match name {
                        "oms_peer_start" => self.start_peer(args),
                        "oms_peer_result" => self.peer_result(args),
                        "oms_peer_operations" => self.peer_operations(args),
                        _ => self.run_tool(tool, args),
                    };
                    match output {
                        Ok(t) => (t, false, None),
                        Err(ToolError::Text(t)) => (t, true, None),
                        Err(ToolError::InvalidField) => {
                            return Some(error(id, -32602, FIELD_ERROR));
                        }
                    }
                };
                if name == "oms_peer_start"
                    && !bad
                    && self.task_capable(params)
                    && self.repo(args).is_ok_and(|r| r == self.caller_cwd)
                    && let Ok(payload) = value::parse(text.as_bytes(), usize::MAX)
                    && let Ok(dir) = self.task_dir(&payload["operation"])
                {
                    match self.task_snapshot(&dir) {
                        Ok(mut task) => {
                            task["resultType"] = json!("task");
                            return Some(reply(id, task));
                        }
                        Err(_) => return Some(error(id, -32602, FIELD_ERROR)),
                    }
                }
                let mut value = json!({"content":[{"type":"text","text":text}],"isError":bad});
                if let Some(g) = graph
                    && !bad
                {
                    value["structuredContent"] = json!({"graph":g});
                }
                self.complete(value, params, false)
            }
            _ => return Some(error(id, -32601, format!("method not found: {method}"))),
        };
        Some(reply(id, result))
    }
}

/// Serve bounded newline records, draining an oversized record before recovery.
/// Blocking readers must be interruptible by the embedding process; the atomic
/// flag is observed between records and while waiting on child operations.
pub fn serve<R: BufRead, W: Write>(
    source_root: &Path,
    caller_cwd: &Path,
    interrupted: &AtomicBool,
    reader: &mut R,
    writer: &mut W,
    config: Config,
) -> io::Result<()> {
    let mut server = Server::new(source_root, caller_cwd, interrupted, config)?;
    serve_server(&mut server, interrupted, reader, writer)
}
/// [`serve`] with a host-supplied [`CommandExecutor`] (for example an
/// in-process router that never spawns a child).
pub fn serve_with_services<R: BufRead, W: Write>(
    source_root: &Path,
    caller_cwd: &Path,
    interrupted: &AtomicBool,
    reader: &mut R,
    writer: &mut W,
    config: Config,
    executor: Arc<dyn CommandExecutor>,
) -> io::Result<()> {
    let mut server = Server::with_executor(source_root, caller_cwd, interrupted, config, executor)?;
    serve_server(&mut server, interrupted, reader, writer)
}
fn serve_server<R: BufRead, W: Write>(
    server: &mut Server<'_>,
    interrupted: &AtomicBool,
    reader: &mut R,
    writer: &mut W,
) -> io::Result<()> {
    loop {
        if interrupted.load(Ordering::Relaxed) {
            break;
        }
        let mut record = Vec::new();
        let mut oversize = false;
        let mut seen = false;
        loop {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            seen = true;
            let count = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buffer.len(), |n| n + 1);
            let ended = buffer[count - 1] == b'\n';
            if !oversize {
                if record.len() + count > MAX_REQUEST_BYTES {
                    oversize = true;
                    record.clear();
                } else {
                    record.extend_from_slice(&buffer[..count]);
                }
            }
            reader.consume(count);
            if ended {
                break;
            }
        }
        if !seen {
            break;
        }
        let response = if oversize {
            Some(error(
                &Value::Null,
                -32600,
                format!("request exceeds {MAX_REQUEST_BYTES}-byte limit"),
            ))
        } else if let Ok(line) = std::str::from_utf8(&record) {
            let line = line.trim_matches(crate::whitespace);
            if line.is_empty() {
                continue;
            }
            match value::parse(line.as_bytes(), MAX_REQUEST_BYTES) {
                Ok(v) => server.handle_value(&v),
                Err(_) => Some(error(&Value::Null, -32700, "parse error")),
            }
        } else {
            Some(error(&Value::Null, -32700, "parse error"))
        };
        if let Some(v) = response {
            writer.write_all(value::encode(&v, true, false, false, false).as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Private native owner entry, for a bundled executable only. Arguments are
/// SOURCE_ROOT, REPO, OPERATION and an optional OMS_PROGRAM (the absolute native
/// `oms` the server resolved); no command strings or launch modes. Without
/// OMS_PROGRAM the owner resolves `oms` itself and fails before publishing `pid`.
pub fn peer_owner(arguments: &[std::ffi::OsString]) -> io::Result<()> {
    peer::owner(arguments)
}
