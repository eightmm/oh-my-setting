#![forbid(unsafe_code)]
use oms_mcp_server::{
    Captured, CommandExecutor, CommandRequest, Config, Server, SubprocessExecutor, ToolProfile,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const META: &str = "io.modelcontextprotocol/protocolVersion";
const CAPS: &str = "io.modelcontextprotocol/clientCapabilities";
const EXT: &str = "io.modelcontextprotocol/tasks";
fn timeout() -> Duration {
    Duration::from_secs(
        std::env::var("OMS_TEST_CALL_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120),
    )
}
fn until(mut test: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout();
    loop {
        if test() {
            break;
        }
        assert!(Instant::now() < deadline, "observed condition timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn modern() -> Value {
    json!({META:"2026-07-28",CAPS:{"extensions":{EXT:{}}}})
}
fn config(full: bool, tasks: bool) -> Config {
    Config {
        profile: if full {
            ToolProfile::Full
        } else {
            ToolProfile::Core
        },
        tasks_extension: tasks,
        peer_owner: PathBuf::from(env!("CARGO_BIN_EXE_oms-mcp-peer-owner")),
        oms_program: None,
    }
}
// Pin the fixture `oms` of a source tree so nothing is searched for.
fn pinned(source: &Path, mut cfg: Config) -> Config {
    cfg.oms_program = Some(source.join("bin/oms"));
    cfg
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "oms-native-mcp-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn source(&self) -> PathBuf {
        let p = self.0.join("source");
        std::fs::create_dir_all(p.join("bin")).unwrap();
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), p.join("bin/oms")).unwrap();
        std::fs::write(p.join("VERSION"), "fixture\n").unwrap();
        p
    }
    fn repo(&self) -> PathBuf {
        let p = self.0.join("repo");
        std::fs::create_dir(&p).unwrap();
        p
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if let Ok(entries) = std::fs::read_dir(self.0.join("repo/.oms/artifacts/mcp")) {
            for entry in entries.filter_map(Result::ok) {
                let dir = entry.path();
                if !dir.join("status").is_file()
                    && dir.join("fixture-ready").is_file()
                    && let Some(pid) = std::fs::read_to_string(dir.join("pid"))
                        .ok()
                        .and_then(|s| s.trim().parse::<i32>().ok())
                        .filter(|p| *p > 0)
                        .and_then(rustix::process::Pid::from_raw)
                {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
            }
        }
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Client {
    child: Child,
    input: std::process::ChildStdin,
    output: BufReader<std::process::ChildStdout>,
    id: u64,
}
impl Client {
    fn new(source: &Path, repo: &Path, full: bool, tasks: bool, child: bool) -> Self {
        Self::with_depth(source, repo, full, tasks, child, std::ffi::OsStr::new("0"))
    }
    fn with_depth(
        source: &Path,
        repo: &Path,
        full: bool,
        tasks: bool,
        child: bool,
        depth: &std::ffi::OsStr,
    ) -> Self {
        let mut p = Command::new(std::env::current_exe().unwrap())
            .env_remove("PYTHONUTF8")
            .env_remove("PYTHONIOENCODING")
            .arg("--stdio")
            .arg(source)
            .arg(repo)
            .arg(if full { "full" } else { "core" })
            .arg(if tasks { "1" } else { "0" })
            .env("OMS_HARNESS_CHILD", if child { "1" } else { "0" })
            .env("OMS_HARNESS_DELEGATE_DEPTH", depth)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Self {
            input: p.stdin.take().unwrap(),
            output: BufReader::new(p.stdout.take().unwrap()),
            child: p,
            id: 0,
        }
    }
    fn raw(&mut self, bytes: &[u8]) {
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }
    fn read_line(&mut self) -> String {
        let duration = timeout();
        let limit = rustix::event::Timespec {
            tv_sec: duration.as_secs() as i64,
            tv_nsec: 0,
        };
        if self.output.buffer().is_empty() {
            let mut fds = [rustix::event::PollFd::new(
                self.output.get_ref(),
                rustix::event::PollFlags::IN,
            )];
            assert!(
                rustix::event::poll(&mut fds, Some(&limit)).unwrap() > 0,
                "stdio call timed out"
            );
        }
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "server exited"
        );
        line
    }
    fn read(&mut self) -> Value {
        serde_json::from_str(&self.read_line()).unwrap()
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let m = json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        self.raw(format!("{m}\n").as_bytes());
        let v = self.read();
        assert_eq!(v["id"], self.id);
        v
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.call("tools/call", json!({"name":name,"arguments":args}))
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn payload(v: &Value) -> Value {
    serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}
fn good(v: &Value) {
    assert_eq!(v["result"]["isError"], false, "{v}");
}
fn bad(v: &Value, part: &str) {
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(part),
        "{v}"
    );
}
fn run_dir(repo: &Path, op: &str, meta: Value, status: Option<&str>) -> PathBuf {
    let p = repo.join(".oms/artifacts/mcp").join(op);
    std::fs::create_dir_all(p.join("answers")).unwrap();
    std::fs::write(p.join("meta.json"), meta.to_string()).unwrap();
    std::fs::write(p.join("prompt.txt"), "question\n").unwrap();
    if let Some(s) = status {
        std::fs::write(p.join("status"), s).unwrap();
    }
    p
}
fn record(source: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(source.join("last-call.json")).unwrap()).unwrap()
}
fn fixture_settings(source: &Path, value: Value) {
    std::fs::write(source.join("fixture.json"), value.to_string()).unwrap();
}

fn environment_profile() {
    use std::os::unix::ffi::OsStringExt;
    for (value, ok) in [
        (None, true),
        (Some(std::ffi::OsString::from("core")), true),
        (Some("full".into()), true),
        (Some("".into()), false),
        (Some("invalid".into()), false),
        (Some(std::ffi::OsString::from_vec(vec![255])), false),
    ] {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.arg("--profile-check")
            .env_remove("OMS_MCP_TOOL_PROFILE");
        if let Some(v) = value {
            cmd.env("OMS_MCP_TOOL_PROFILE", v);
        }
        let result = cmd.output().unwrap();
        assert_eq!(result.status.success(), ok, "{:?}", result);
    }
}
fn environment_authority() {
    use std::os::unix::ffi::OsStringExt;
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    for depth in [
        std::ffi::OsString::from("1"),
        std::ffi::OsString::from("invalid"),
        std::ffi::OsString::from_vec(vec![255]),
    ] {
        let mut c = Client::with_depth(&source, &repo, false, false, false, &depth);
        bad(
            &c.tool("oms_peer_start", json!({"kind":"consult","prompt":"q"})),
            "delegated worker",
        );
    }
    assert!(!repo.join(".oms").exists());
}
fn parser_reference() {
    use oms_context::Json;
    fn same(a: &Json, b: &Json) -> bool {
        match (a, b) {
            (Json::Array(a), Json::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
            }
            (Json::Object(a), Json::Object(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, v)| {
                        b.iter()
                            .find(|(key, _)| key == k)
                            .is_some_and(|(_, other)| same(v, other))
                    })
            }
            _ => a == b,
        }
    }
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, true, false);
    let corpus: Value = serde_json::from_str(include_str!("parser-reference.json")).unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        let request = if let Some(raw) = case["request"].as_str() {
            raw.to_owned()
        } else {
            use sha2::{Digest, Sha256};
            let raw = case["request_chunks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|chunk| {
                    chunk[0]
                        .as_str()
                        .unwrap()
                        .repeat(chunk[1].as_u64().unwrap() as usize)
                })
                .collect::<String>();
            assert_eq!(raw.len(), case["request_bytes"].as_u64().unwrap() as usize);
            assert_eq!(
                format!("{:x}", Sha256::digest(raw.as_bytes())),
                case["request_sha256"]
            );
            raw
        };
        c.raw(format!("{request}\n").as_bytes());
        let expanded;
        let expected = if let Some(expected) = case["response"].as_str() {
            expected
        } else {
            use sha2::{Digest, Sha256};
            expanded = case["response_chunks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|chunk| {
                    chunk[0]
                        .as_str()
                        .unwrap()
                        .repeat(chunk[1].as_u64().unwrap() as usize)
                })
                .collect::<String>();
            assert_eq!(
                expanded.len(),
                case["response_bytes"].as_u64().unwrap() as usize
            );
            assert_eq!(
                format!("{:x}", Sha256::digest(expanded.as_bytes())),
                case["response_sha256"]
            );
            &expanded
        };
        if expected.is_empty() {
            c.call("ping", json!({}));
            continue;
        }
        let actual = c.read_line();
        let expected = oms_context::parse_session_json(expected.as_bytes()).unwrap();
        let actual = oms_context::parse_session_json(actual.as_bytes()).unwrap();
        assert!(
            same(&actual, &expected),
            "canonical case {}: {:?} != {:?}",
            case["name"],
            actual,
            expected
        );
    }
}

fn lossless_values() {
    use oms_mcp_server::value::{self, ToValue};
    use std::os::unix::ffi::OsStringExt;
    fn call(c: &mut Client, method: &str, params: Value) -> value::Value {
        c.id += 1;
        c.raw(format!("{}\n", json!({"id":c.id,"method":method,"params":params})).as_bytes());
        value::from_str(&c.read_line()).unwrap()
    }
    let mut bytes = b"raw-".to_vec();
    bytes.extend(128..=255);
    let raw = std::ffi::OsString::from_vec(bytes);
    let text = value::Text::path(&raw);
    assert_eq!(text.os_string().unwrap(), raw);
    let encoded = text.to_value().to_string();
    assert!(encoded.contains("\\udc80") && encoded.contains("\\udcff"));
    assert_eq!(value::from_str(&encoded).unwrap().as_text().unwrap(), &text);
    assert!(value::Text::from("").os_string().unwrap().is_empty());
    assert_eq!(
        value::Text::from("근거🙂").os_string().unwrap(),
        std::ffi::OsString::from("근거🙂")
    );
    let incomplete = std::ffi::OsString::from_vec(vec![0xf0, 0x9f]);
    assert_eq!(
        value::Text::path(&incomplete).os_string().unwrap(),
        incomplete
    );
    let escaped = value::from_str(r#""\ud800""#).unwrap();
    assert!(escaped.as_text().unwrap().os_string().is_err());
    let internal = b"\"\xed\xa0\x80\"";
    assert_eq!(value::parse(internal, internal.len()).unwrap(), escaped);
    assert!(value::from_slice(internal).is_err());
    assert!(value::parse(b"\"\xff\"", 3).is_err());
    assert!(value::parse(b"", 0).is_err());
    assert!(value::parse(b"0", 0).is_err());
    assert!(value::parse(b"0", 1).is_ok());
    assert!(value::from_slice(b"\xef\xbb\xbf{}").is_err());
    assert_eq!(
        value::from_str(r#""\ud800\udc00""#).unwrap().as_str(),
        Some("𐀀")
    );
    assert_eq!(1.0_f64.to_value().as_i64(), None);
    let marker = value::from_str(r#"{"$serde_json::private::Number":"NaN"}"#).unwrap();
    assert!(marker.is_object());
    let deep = format!("{}0{}", "[".repeat(9997), "]".repeat(9997));
    let tree = value::from_str(&deep).unwrap();
    let cloned = tree.clone();
    assert_eq!(tree, cloned);
    assert_eq!(
        value::encode(&cloned, true, false, false, false).as_str(),
        Some(deep.as_str())
    );
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, true, false);
    let reference = value::from_str(include_str!("value-reference.json")).unwrap();
    for case in reference["numbers"].as_array().unwrap() {
        assert_eq!(
            value::from_str(case["request"].as_str().unwrap())
                .unwrap()
                .to_string(),
            case["response"].as_str().unwrap()
        );
    }
    for raw in reference["malformed_numbers"].as_array().unwrap() {
        assert!(value::from_str(raw.as_str().unwrap()).is_err());
    }
    let op = reference["operation"].as_str().unwrap();
    let dir = run_dir(&repo, op, json!({}), None);
    std::fs::write(dir.join("meta.json"), reference["meta"].to_string()).unwrap();
    let response = call(
        &mut c,
        "tools/call",
        json!({"name":"oms_peer_operations","arguments":{}}),
    );
    let payload = value::parse(
        response["result"]["content"][0]["text"]
            .as_text()
            .unwrap()
            .as_bytes(),
        usize::MAX,
    )
    .unwrap();
    let row = &payload["operations"][0];
    for key in ["title", "started_at", "thread", "targets"] {
        assert_eq!(row[key], reference["operation_fields"][key]);
    }
    let task = call(&mut c, "tasks/get", json!({"taskId":op,"_meta":modern()}));
    assert_eq!(task["result"]["createdAt"], reference["task_createdAt"]);
    std::fs::write(dir.join("meta.json"), b"{\"title\":\"\xed\xa0\x80\"}").unwrap();
    let response = call(
        &mut c,
        "tools/call",
        json!({"name":"oms_peer_operations","arguments":{}}),
    );
    let payload = value::parse(
        response["result"]["content"][0]["text"]
            .as_text()
            .unwrap()
            .as_bytes(),
        usize::MAX,
    )
    .unwrap();
    assert!(payload["operations"][0].get("title").is_none());
    fixture_settings(
        &source,
        json!({"stdout":reference["graph_stdout"].as_str().unwrap()}),
    );
    let graph = call(
        &mut c,
        "tools/call",
        json!({"name":"oms_project_graph_render","arguments":{}}),
    );
    assert_eq!(
        graph["result"]["content"][0]["text"],
        reference["graph_text"]
    );
    assert_eq!(
        graph["result"]["structuredContent"]["graph"],
        reference["graph"]
    );
}

fn canonical_protocol_cases() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, true, false);
    let cases: Value = serde_json::from_str(include_str!("protocol-cases.json")).unwrap();
    for (i, case) in cases.as_array().unwrap().iter().enumerate() {
        c.raw(format!("{}\n", case["request"]).as_bytes());
        if case["expected"].is_null() {
            let barrier = c.call("ping", json!({}));
            assert_eq!(barrier["id"], c.id);
        } else {
            assert_eq!(c.read(), case["expected"], "case {i}: {}", case["request"]);
        }
    }
}
fn discovery_protocols() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, true, false);
    let listed = c.call("tools/list", json!({"_meta":modern()}));
    let golden: Value = serde_json::from_str(include_str!("definitions.json")).unwrap();
    assert_eq!(listed["result"]["tools"], golden);
    assert_eq!(golden.as_array().unwrap().len(), 27);
    assert_eq!(listed["result"]["ttlMs"], 300000);
    let d = c.call("server/discover", json!({}));
    assert_eq!(
        d["result"]["supportedVersions"],
        json!(["2026-07-28", "2025-06-18", "2025-03-26"])
    );
    assert!(d["result"]["capabilities"]["extensions"].get(EXT).is_some());
    assert!(
        c.call("ping", json!({}))["result"]
            .get("resultType")
            .is_none()
    );
    for p in ["1900", "2025-03-26", "2026-07-28"] {
        let r = c.call("initialize", json!({"protocolVersion":p}));
        assert_eq!(
            r["result"]["protocolVersion"],
            if p == "1900" { "2025-06-18" } else { p }
        );
    }
    assert_eq!(
        c.call("ping", json!({}))["result"]["resultType"],
        "complete"
    );
    assert!(
        c.call("ping", json!({"_meta":{META:"2025-03-26"}}))["result"]
            .get("resultType")
            .is_none()
    );
    assert_eq!(
        c.call("ping", json!({"_meta":{META:"future"}}))["error"]["code"],
        -32022
    );
    for method in [
        "initialize",
        "server/discover",
        "resources/list",
        "resources/read",
    ] {
        assert_eq!(c.call(method, json!([]))["error"]["code"], -32602);
    }
    assert_eq!(
        c.call("resources/list", json!({}))["result"]["resources"][0]["uri"],
        "ui://oms/project-graph/v1.html"
    );
    let resource = c.call(
        "resources/read",
        json!({"uri":"ui://oms/project-graph/v1.html","_meta":modern()}),
    );
    let html = resource["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(html.contains("Instruction draft") && html.contains("sendFollowUpMessage"));
    assert_eq!(resource["result"]["ttlMs"], 300000);
    assert_eq!(
        c.call("resources/read", json!({"uri":"file:any"}))["error"]["code"],
        -32602
    );
    for method in ["tasks/result", "resources/templates/list", "unknown"] {
        assert_eq!(c.call(method, json!({}))["error"]["code"], -32601);
    }
    assert_eq!(
        c.call("tools/call", json!({"name":false}))["error"]["code"],
        -32602
    );
    assert_eq!(
        c.call("tools/call", json!({"name":"oms_inbox","arguments":[]}))["error"]["code"],
        -32602
    );
    assert_eq!(c.tool("unknown", json!({}))["error"]["code"], -32602);
    let mut core = Client::new(&source, &repo, false, false, false);
    assert_eq!(
        core.call("tools/list", Value::Null)["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
    good(&core.tool("oms_task_state", json!({})));
    assert_eq!(
        core.call("tasks/get", json!({"_meta":modern()}))["error"]["code"],
        -32601
    );
}
fn transport_bounds() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, false, false, false);
    c.raw(b"\n \r\n{\"method\":\"ping\"}\n{\"method\":\"ping\",\"id\":null}\n");
    assert_eq!(c.call("ping", json!({}))["result"], json!({}));
    for (input, code) in [
        (b"bad\n".as_slice(), -32700),
        (b"\xff\n".as_slice(), -32700),
        (b"[]\n".as_slice(), -32600),
        (b"{\"id\":true,\"method\":\"ping\"}\n".as_slice(), -32600),
        (b"{\"id\":1,\"method\":false}\n".as_slice(), -32600),
    ] {
        c.raw(input);
        assert_eq!(c.read()["error"]["code"], code);
    }
    c.raw(b"{\"id\":1,\"method\":\"ping\",\"params\":\"\\ud800\"}\n");
    assert_eq!(c.read()["result"], json!({}));
    c.raw(b"\x1c{\"id\":991,\"method\":\"ping\"}\x1f\n");
    assert_eq!(c.read()["id"], 991);
    let exact = format!(
        "{}{{\"id\":99,\"method\":\"ping\"}}\n",
        " ".repeat(oms_mcp_server::MAX_REQUEST_BYTES - 26)
    );
    assert_eq!(exact.len(), oms_mcp_server::MAX_REQUEST_BYTES);
    c.raw(exact.as_bytes());
    assert_eq!(c.read()["id"], 99);
    let over = format!("{}\n", "x".repeat(oms_mcp_server::MAX_REQUEST_BYTES * 2));
    c.raw(over.as_bytes());
    assert_eq!(c.read()["error"]["code"], -32600);
    let deep = format!("{}0{}\n", "[".repeat(2000), "]".repeat(2000));
    c.raw(deep.as_bytes());
    assert_eq!(c.read()["error"]["code"], -32600);
    assert_eq!(c.call("ping", Value::Null)["result"], json!({}));
    c.raw(format!("{}\n", json!({"id":"i".repeat(1025),"method":"ping"})).as_bytes());
    assert_eq!(c.read()["error"]["code"], -32600);
    c.raw(format!("{}\n", json!({"id":1,"method":"m".repeat(129)})).as_bytes());
    assert_eq!(c.read()["error"]["code"], -32600);
    let flag = AtomicBool::new(false);
    let mut output = Vec::new();
    oms_mcp_server::serve(
        &source,
        &repo,
        &flag,
        &mut std::io::Cursor::new(b"{\"id\":992,\"method\":\"ping\"}"),
        &mut output,
        pinned(&source, config(false, false)),
    )
    .unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&output).unwrap()["id"], 992);

    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let raw_repo = t.0.join(std::ffi::OsString::from_vec(b"raw-\xff".to_vec()));
    std::fs::create_dir(&raw_repo).unwrap();
    let alias = repo.join("raw-alias");
    std::os::unix::fs::symlink(&raw_repo, &alias).unwrap();
    let mut raw = Client::new(&source, &raw_repo, false, true, false);
    assert_eq!(raw.call("ping", json!({}))["result"], json!({}));
    good(&raw.tool("oms_inbox", json!({})));
    assert_eq!(
        record(&source)["cwd_bytes"],
        json!(raw_repo.as_os_str().as_bytes())
    );
    good(&c.tool("oms_inbox", json!({"repo":alias})));
    assert_eq!(
        record(&source)["cwd_bytes"],
        json!(raw_repo.as_os_str().as_bytes())
    );
    good(&raw.tool("oms_peer_operations", json!({})));
    assert!(!raw_repo.join(".oms").exists());
    let op = "ask-20260927T000000Z-aabbccdd";
    let dir = run_dir(&raw_repo, op, json!({"cancellation_supported":true}), None);
    assert_eq!(
        raw.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["status"],
        "working"
    );
    assert_eq!(
        raw.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["result"],
        json!({"resultType":"complete"})
    );
    let receipt: Value =
        serde_json::from_slice(&std::fs::read(dir.join("cancel-request.json")).unwrap()).unwrap();
    assert_eq!(receipt["task_id"], op);
    std::fs::write(dir.join("status"), "0").unwrap();
    let completed = raw.call("tasks/get", json!({"taskId":op,"_meta":modern()}));
    assert_eq!(completed["error"]["code"], -32602);
    assert_eq!(
        completed["error"]["message"],
        "invalid or oversized request field"
    );
}
fn raw_peer_paths() {
    use oms_mcp_server::value::{self, ToValue};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let t = Temp::new();
    let original = t.source();
    let source =
        t.0.join(std::ffi::OsString::from_vec(b"source-\xfe".to_vec()));
    std::fs::rename(original, &source).unwrap();
    let repo =
        t.0.join(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    std::fs::create_dir(&repo).unwrap();
    let mut c = Client::new(&source, &repo, true, true, false);
    for kind in ["consult", "advise", "ask"] {
        c.id += 1;
        c.raw(format!("{}\n",json!({"id":c.id,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"kind":kind,"prompt":"raw path question"}}})).as_bytes());
        let response = value::from_str(&c.read_line()).unwrap();
        assert_eq!(response["result"]["isError"], false, "{response}");
        let payload = value::parse(
            response["result"]["content"][0]["text"]
                .as_text()
                .unwrap()
                .as_bytes(),
            usize::MAX,
        )
        .unwrap();
        let operation = payload["operation"].as_str().unwrap();
        let dir = repo.join(".oms/artifacts/mcp").join(operation);
        assert_eq!(
            payload["run_dir"].as_text().unwrap().os_string().unwrap(),
            dir.as_os_str()
        );
        assert_eq!(
            payload["result_arguments"]["repo"]
                .as_text()
                .unwrap()
                .os_string()
                .unwrap(),
            repo.as_os_str()
        );
        until(|| dir.join("status").exists());
        assert_eq!(std::fs::read_to_string(dir.join("status")).unwrap(), "0\n");
        assert!(dir.join("answers/fixture.md").is_file());
        let call = record(&source);
        let args = call["argv"].as_array().unwrap();
        assert_eq!(args[0], if kind == "ask" { "peer-ask" } else { kind });
        assert_eq!(call["argv_bytes"][2], json!(repo.as_os_str().as_bytes()));
        assert_eq!(
            call["argv_bytes"][4],
            json!(dir.join("answers").as_os_str().as_bytes())
        );
        assert_eq!(call["cwd_bytes"], json!(repo.as_os_str().as_bytes()));
        let meta: Value =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["answer_storage"], "operation");
        // Canonical cursor hashing strictly encodes its path-bearing snapshot.
        let result = c.tool("oms_peer_result", json!({"operation":operation}));
        assert_eq!(result["error"]["code"], -32602);
        assert_eq!(
            result["error"]["message"],
            "invalid or oversized request field"
        );
    }
    std::fs::write(source.join("fixture-owner"), "complete before readiness").unwrap();
    let mut modern_client = Client::new(&source, &repo, true, true, false);
    let result=modern_client.call("tools/call",json!({"name":"oms_peer_start","arguments":{"kind":"consult","prompt":"completed before snapshot"},"_meta":modern()}));
    assert_eq!(result["error"]["code"], -32602, "{result}");
    assert_eq!(
        result["error"]["message"],
        "invalid or oversized request field"
    );
    good(&c.tool("oms_peer_result", json!({"thread":"existing"})));
    assert_eq!(
        record(&source)["argv_bytes"][2],
        json!(repo.as_os_str().as_bytes())
    );
    let raw_repo = repo.to_value().to_string();
    c.raw(format!("{{\"id\":999,\"method\":\"tools/call\",\"params\":{{\"name\":\"oms_inbox\",\"arguments\":{{\"repo\":{raw_repo}}}}}}}\n").as_bytes());
    bad(&c.read(), "repo must be valid UTF-8 text");
}

fn fixed_argv() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, false, false);
    let rows: Value = serde_json::from_str(include_str!("fixed-argv.json")).unwrap();
    for row in rows.as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        good(&c.tool(name, row["arguments"].clone()));
        let r = record(&source);
        assert_eq!(r["argv"], row["argv"], "{name}");
        assert_eq!(r["cwd"], repo.to_string_lossy().as_ref());
        assert_eq!(r["utf8"], "");
    }
    good(&c.tool("oms_journal", json!({"view":"blockers"})));
    assert_eq!(
        record(&source)["argv"],
        json!(["journal", "show", "--json", "--blockers"])
    );
    let invalid = [
        json!({"file":"../escape"}),
        json!({"file":"--help"}),
        json!({"file":"a/b"}),
        json!({"file":"a\\b"}),
        json!({"file":3}),
        json!({"file":"a\0"}),
        json!({"file":"a".repeat(4097)}),
    ];
    for a in invalid {
        bad(&c.tool("oms_handoff_show", a), "error:");
    }
    for a in ["--help", "../escape", "a/../b", "a\nb", ""] {
        bad(
            &c.tool("oms_project_graph_query", json!({"query":a})),
            "requires",
        );
    }
    bad(&c.tool("oms_inbox", json!({"repo":17})), "repo must");
    bad(&c.tool("oms_inbox", json!({"repo":"missing"})), "no such");
}
fn output_bounds() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, false, false);
    for n in [0, 60000, 60001] {
        fixture_settings(&source, json!({"repeat":n,"character":"근"}));
        let r = c.tool("oms_inbox", json!({}));
        if n > 60000 {
            bad(&r, "JSON output has 60001 characters");
        } else {
            good(&r);
        }
    }
    fixture_settings(&source, json!({"repeat":60001,"character":"x"}));
    let raw = c.tool("oms_handoff_show", json!({"file":"digest.md"}));
    good(&raw);
    assert!(
        raw["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with("[truncated: 1 characters omitted]")
    );
    fixture_settings(
        &source,
        json!({"stdout":"partial","stderr":"failure","exit":7}),
    );
    let r = c.tool("oms_inbox", json!({}));
    bad(&r, "partial\nfailure");
    fixture_settings(&source, json!({"stdout":"","exit":0}));
    assert_eq!(
        c.tool("oms_inbox", json!({}))["result"]["content"][0]["text"],
        "exit 0"
    );
    fixture_settings(&source, json!({"leading":300000,"stdout":"ok"}));
    let r = c.tool("oms_inbox", json!({}));
    good(&r);
    assert_eq!(r["result"]["content"][0]["text"], "ok");
}
fn graph_render() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, false, false);
    fixture_settings(
        &source,
        json!({"stdout":json!({"kind":"project","nodes":[],"edges":[],"counts":{"shown_nodes":0,"total_nodes":0,"shown_edges":0,"total_edges":0}}).to_string()}),
    );
    good(&c.tool("oms_project_graph_render", json!({})));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "graph",
            "project",
            "map",
            "--limit",
            "72",
            "--depth",
            "2",
            "--ui-model"
        ])
    );
    let r = c.tool("oms_project_graph_render", json!({"task":"inspect entry"}));
    good(&r);
    assert_eq!(
        r["result"]["structuredContent"]["graph"]["title"],
        "OMS Context Graph · inspect entry"
    );
    assert_eq!(
        record(&source)["argv"],
        json!([
            "graph",
            "project",
            "context",
            "--task",
            "inspect entry",
            "--max-files",
            "12",
            "--ui-model"
        ])
    );
    for value in [
        "garbage".to_owned(),
        "[]".to_owned(),
        json!({"kind":"project","nodes":vec![json!({});121],"edges":[]}).to_string(),
        "x".repeat(262145),
    ] {
        fixture_settings(&source, json!({"stdout":value}));
        bad(&c.tool("oms_project_graph_render", json!({})), "error:");
    }
    fixture_settings(&source, json!({"invalid_ui":100000}));
    bad(
        &c.tool("oms_project_graph_render", json!({})),
        "262144-byte limit",
    );
    bad(
        &c.tool("oms_project_graph_render", json!({"task":"--help"})),
        "cannot begin",
    );
    bad(
        &c.tool("oms_project_graph_render", json!({"task":"x".repeat(2049)})),
        "2048-byte",
    );
}
fn peer_reading() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, false, true, false);
    let op = "ask-20260927T000000Z-aabbccdd";
    let dir = run_dir(
        &repo,
        op,
        json!({"answer_storage":"operation","cancellation_supported":true}),
        Some("0\n"),
    );
    let answer = format!(
        "HEAD\n{}MIDDLE EVIDENCE\n{}\nTAIL",
        "근거🙂\n".repeat(20000),
        "x".repeat(20000)
    );
    let original = format!("## Prompt\nPRIVATE PROMPT\n## Output\n{answer}\n## Exit\n0\n")
        .replace('\n', "\r\n");
    let path = dir.join("answers/codex-근거.md");
    std::fs::write(&path, &original).unwrap();
    std::fs::write(
        dir.join("run.log"),
        format!(
            "artifact: {}\n{}thread: latest\n",
            path.display(),
            "noise\n".repeat(100000)
        ),
    )
    .unwrap();
    let result = c.tool("oms_peer_result", json!({"operation":op}));
    good(&result);
    let p = payload(&result);
    assert_eq!(p["thread"], "latest");
    assert_eq!(p["answer_truncated"], true);
    assert!(p["answer"].as_str().unwrap().contains("TAIL"));
    assert!(!p["answer"].as_str().unwrap().contains("MIDDLE EVIDENCE"));
    let same = c.tool(
        "oms_peer_result",
        json!({"operation":op,"after":p["cursor"]}),
    );
    good(&same);
    assert_eq!(payload(&same)["unchanged"], true);
    assert!(payload(&same).get("answer").is_none());
    let read = p["answer_details"][0]["read_arguments"].clone();
    let mut next = read.clone();
    let mut rebuilt = String::new();
    loop {
        let r = c.tool("oms_peer_result", next);
        good(&r);
        let page = payload(&r);
        rebuilt.push_str(page["answer"].as_str().unwrap());
        if let Some(n) = page.get("next_read_arguments") {
            next = n.clone();
        } else {
            break;
        }
    }
    assert_eq!(rebuilt, answer);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let mut query = read.clone();
    query["answer_query"] = json!("MIDDLE EVIDENCE");
    let found = payload(&c.tool("oms_peer_result", query));
    assert_eq!(found["matches"].as_array().unwrap().len(), 1);
    assert_eq!(found["has_more"], false);
    let mut query = read.clone();
    query["answer_query"] = json!("근거🙂");
    let found = payload(&c.tool("oms_peer_result", query));
    assert_eq!(found["matches"].as_array().unwrap().len(), 8);
    assert_eq!(found["has_more"], true);
    good(&c.tool("oms_peer_result", found["next_search_arguments"].clone()));
    let mut end = read.clone();
    end["answer_offset"] = json!(answer.len());
    assert_eq!(payload(&c.tool("oms_peer_result", end))["answer"], "");
    for extra in [
        json!({"answer_offset":6}),
        json!({"answer_offset":true}),
        json!({"answer_offset":-1}),
        json!({"answer_offset":8388609}),
        json!({"answer_limit":3}),
        json!({"answer_limit":16385}),
        json!({"answer_limit":null}),
        json!({"answer_query":""}),
        json!({"answer_query":"🙂".repeat(65)}),
        json!({"answer_query":"HEAD","answer_limit":16}),
        json!({"after":""}),
        json!({"thread":"x"}),
        json!({"wait_seconds":1}),
        json!({"answer_ref":"../escape"}),
    ] {
        let mut a = read.clone();
        a.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        bad(&c.tool("oms_peer_result", a), "error:");
    }
    let mut edge = read.clone();
    edge["answer_offset"] = json!(5);
    edge["answer_limit"] = json!(4);
    let edge = payload(&c.tool("oms_peer_result", edge));
    assert_eq!(edge["answer"], "근");
    assert_eq!(edge["answer_end"], 8);
    std::fs::write(&path, original.replace("MIDDLE EVIDENCE", "CHANGED PROOF!")).unwrap();
    bad(
        &c.tool("oms_peer_result", read.clone()),
        "reference changed",
    );
    std::fs::write(&path, original.replace("\r\n", "\n")).unwrap();
    good(&c.tool("oms_peer_result", read.clone()));
    let other = run_dir(
        &repo,
        "ask-20260927T000000Z-eeff0011",
        json!({"answer_storage":"operation"}),
        Some("0"),
    );
    let other_file = other.join("answers/codex-근거.md");
    std::fs::write(&other_file, &original).unwrap();
    let mut cross = read.clone();
    cross["operation"] = json!("ask-20260927T000000Z-eeff0011");
    bad(&c.tool("oms_peer_result", cross), "another operation");
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&other_file, &path).unwrap();
    bad(&c.tool("oms_peer_result", read.clone()), "unsafe");
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&other_file, &path).unwrap();
    bad(&c.tool("oms_peer_result", read.clone()), "unsafe");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, &original).unwrap();
    std::fs::rename(dir.join("answers"), dir.join("saved-answers")).unwrap();
    std::os::unix::fs::symlink(other.join("answers"), dir.join("answers")).unwrap();
    bad(&c.tool("oms_peer_result", read.clone()), "unsafe");
    std::fs::remove_file(dir.join("answers")).unwrap();
    std::fs::rename(dir.join("saved-answers"), dir.join("answers")).unwrap();
    std::fs::write(&path, original.replace("## Exit\r\n0", "## Exit\r\n1")).unwrap();
    bad(&c.tool("oms_peer_result", read.clone()), "artifact_exit");
    std::fs::write(&path, vec![b'x'; 8388609]).unwrap();
    bad(&c.tool("oms_peer_result", read.clone()), "oversized");
    bad(
        &c.tool("oms_peer_result", json!({"operation":op})),
        "answer_errors",
    );
    std::fs::remove_file(dir.join("status")).unwrap();
    bad(
        &c.tool("oms_peer_result", read.clone()),
        "completed operation",
    );

    // Legacy artifacts have no answer_storage marker. A native safety refusal
    // must not turn into answer text on a successful operation.
    std::fs::write(dir.join("meta.json"), "{}").unwrap();
    std::fs::write(dir.join("status"), "0").unwrap();
    let legacy = repo.join("legacy.md");
    std::fs::write(
        dir.join("run.log"),
        format!("artifact: {}\n", legacy.display()),
    )
    .unwrap();
    for bytes in [
        Vec::new(),
        b"malformed".to_vec(),
        b"## Output\nanswer\n## Exit\n0\n".to_vec(),
    ] {
        std::fs::write(&legacy, bytes).unwrap();
        good(&c.tool("oms_peer_result", json!({"operation":op})));
    }
    let framing = "## Output\n\n## Exit\n0\n";
    let edge = format!(
        "## Output\n{}\n## Exit\n0\n",
        "x".repeat(8388608 - framing.len())
    );
    assert_eq!(edge.len(), 8388608);
    std::fs::write(&legacy, &edge).unwrap();
    let result = c.tool("oms_peer_result", json!({"operation":op}));
    good(&result);
    assert_eq!(payload(&result)["answer_truncated"], true);
    std::fs::write(&legacy, format!("{edge}x")).unwrap();
    let result = c.tool("oms_peer_result", json!({"operation":op}));
    bad(&result, "answer_errors");
    assert_eq!(payload(&result)["answer"], "");
    let readable = repo.join("readable.md");
    std::fs::write(&readable, "## Output\nsafe sibling\n## Exit\n0\n").unwrap();
    std::fs::write(
        dir.join("run.log"),
        format!(
            "artifact: {}\nartifact: {}\n",
            legacy.display(),
            readable.display()
        ),
    )
    .unwrap();
    let partial = c.tool("oms_peer_result", json!({"operation":op}));
    bad(&partial, "answer_errors");
    assert!(
        payload(&partial)["answer"]
            .as_str()
            .unwrap()
            .contains("safe sibling")
    );
    std::fs::remove_file(&legacy).unwrap();
    std::os::unix::fs::symlink(other_file, &legacy).unwrap();
    bad(
        &c.tool("oms_peer_result", json!({"operation":op})),
        "answer_errors",
    );

    // These are deliberate native bounds on otherwise unbounded legacy reads.
    std::fs::write(dir.join("run.log"), "").unwrap();
    let ordinary = json!({"operation":op});
    for size in [0, 9, 16384, 16385] {
        let line = if size == 0 {
            String::new()
        } else {
            format!("thread: {}\n", "t".repeat(size - 9))
        };
        assert_eq!(line.len(), size);
        std::fs::write(dir.join("run.log"), line).unwrap();
        let result = c.tool("oms_peer_result", ordinary.clone());
        good(&result);
        assert_eq!(payload(&result).get("thread").is_some(), size == 16384);
    }
    std::fs::write(dir.join("run.log"), "thread: invalid thread\n").unwrap();
    assert!(
        payload(&c.tool("oms_peer_result", ordinary.clone()))
            .get("thread")
            .is_none()
    );
    for size in [0, 2, 262144, 262145] {
        let data = if size == 0 {
            String::new()
        } else {
            format!("{{}}{}", " ".repeat(size - 2))
        };
        std::fs::write(dir.join("meta.json"), data).unwrap();
        good(&c.tool("oms_peer_result", ordinary.clone()));
    }
    let marker = json!({"thread":"metadata-thread"}).to_string();
    for size in [262144, 262145] {
        std::fs::write(
            dir.join("meta.json"),
            format!("{marker}{}", " ".repeat(size - marker.len())),
        )
        .unwrap();
        std::fs::write(dir.join("run.log"), "").unwrap();
        let result = payload(&c.tool("oms_peer_operations", json!({})));
        let row = result["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["operation"] == op)
            .unwrap();
        assert_eq!(row.get("thread").is_some(), size == 262144);
    }
    std::fs::write(dir.join("meta.json"), "{malformed").unwrap();
    good(&c.tool("oms_peer_result", ordinary.clone()));
    for (data, code) in [
        (String::new(), 1),
        ("malformed".into(), 1),
        ("0".into(), 0),
        (format!("{}0", " ".repeat(4095)), 0),
        (format!("{}0", " ".repeat(4096)), 1),
    ] {
        std::fs::write(dir.join("status"), data).unwrap();
        assert_eq!(
            payload(&c.tool("oms_peer_result", ordinary.clone()))["exit"],
            code
        );
    }
    std::fs::remove_file(dir.join("status")).unwrap();
    let pid = std::process::id().to_string();
    for data in [
        String::new(),
        "0".into(),
        "malformed".into(),
        pid.clone(),
        format!("{}{pid}", " ".repeat(4096 - pid.len())),
        format!("{}{pid}", " ".repeat(4097 - pid.len())),
    ] {
        std::fs::write(dir.join("pid"), data).unwrap();
        assert_eq!(
            payload(&c.tool("oms_peer_result", ordinary.clone()))["status"],
            "running"
        );
    }
    for size in [0, 60000, 60001] {
        fixture_settings(&source, json!({"repeat":size}));
        let result = c.tool("oms_peer_result", json!({"thread":"existing"}));
        if size > 60000 {
            bad(&result, "thread JSON output exceeds 60000-character limit");
        } else {
            good(&result);
            assert_eq!(
                result["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .len(),
                size
            );
        }
    }
    fixture_settings(&source, json!({"stdout":"malformed"}));
    good(&c.tool("oms_peer_result", json!({"thread":"existing"})));

    fixture_settings(&source, json!({}));
    let launch = json!({"kind":"consult","targets":[],"thread":"","new_thread":false,"debate_rounds":0,"context_options":{}}).to_string();
    for (index, (data, success)) in [
        (String::new(), false),
        ("{malformed".into(), false),
        (launch.clone(), true),
        (
            format!("{launch}{}", " ".repeat(131072 - launch.len())),
            true,
        ),
        (
            format!("{launch}{}", " ".repeat(131073 - launch.len())),
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let op = format!("consult-20200101T000000Z-{index:08x}");
        let dir = run_dir(&repo, &op, json!({}), None);
        std::fs::write(dir.join("launch.json"), data).unwrap();
        let _ = std::fs::remove_file(source.join("last-call.json"));
        let output = Command::new(env!("CARGO_BIN_EXE_oms-mcp-peer-owner"))
            .args([
                source.as_os_str(),
                repo.as_os_str(),
                std::ffi::OsStr::new(&op),
                source.join("bin/oms").as_os_str(),
            ])
            .env("PATH", "")
            .env("OMS_HARNESS_CHILD", "0")
            .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
            .output()
            .unwrap();
        assert_eq!(output.status.success(), success);
        assert_eq!(
            std::fs::read_to_string(dir.join("status")).unwrap(),
            if success { "0\n" } else { "1\n" }
        );
        assert_eq!(source.join("last-call.json").exists(), success);
    }
}
fn peer_status_tasks() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, false, true, false);
    good(&c.tool("oms_peer_operations", json!({})));
    assert!(!repo.join(".oms").exists());
    let op = "consult-20200101T000000Z-deadbeef";
    let dir = run_dir(
        &repo,
        op,
        json!({"cancellation_supported":true,"title":"question","targets":["claude"]}),
        None,
    );
    std::fs::write(dir.join("run.log"), "working\n").unwrap();
    for value in ["-1", "0", "bad", "999999999999999999999"] {
        std::fs::write(dir.join("pid"), value).unwrap();
        let r = c.tool("oms_peer_result", json!({"operation":op}));
        good(&r);
        assert_eq!(payload(&r)["status"], "running");
    }
    let running = c.tool("oms_peer_result", json!({"operation":op}));
    good(&running);
    assert_eq!(payload(&running)["status"], "running");
    assert_eq!(
        c.call(
            "tasks/get",
            json!({"taskId":op,"_meta":{META:"2026-07-28"}})
        )["error"]["code"],
        -32021
    );
    let get = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}));
    assert_eq!(get["result"]["status"], "working");
    assert_eq!(
        c.call("tasks/update", json!({"taskId":op,"_meta":modern()}))["error"]["code"],
        -32602
    );
    let cancel = c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}));
    assert_eq!(
        cancel["result"],
        json!({"resultType":"complete"}),
        "{cancel}"
    );
    let receipt: Value =
        serde_json::from_slice(&std::fs::read(dir.join("cancel-request.json")).unwrap()).unwrap();
    assert_eq!(receipt["schema"], 1);
    assert_eq!(receipt["task_id"], op);
    assert_eq!(
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["cancellation_requested"],
        true
    );
    for bytes in [
        "not json".to_owned(),
        json!({"schema":1,"task_id":"foreign","kind":"mcp-task-cancel-request"}).to_string(),
        "x".repeat(4097),
    ] {
        std::fs::write(dir.join("cancel-request.json"), bytes).unwrap();
        assert_eq!(
            c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["cancellation_requested"],
            false
        );
    }
    std::fs::remove_file(dir.join("cancel-request.json")).unwrap();
    std::os::unix::fs::symlink(dir.join("meta.json"), dir.join("cancel-request.json")).unwrap();
    assert_eq!(
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["cancellation_requested"],
        false
    );
    std::fs::remove_file(dir.join("cancel-request.json")).unwrap();
    let dead = Command::new(std::env::current_exe().unwrap())
        .arg("--exit")
        .spawn()
        .unwrap();
    let pid = dead.id();
    let mut dead = dead;
    dead.wait().unwrap();
    std::fs::write(dir.join("pid"), format!("{pid}\n")).unwrap();
    let r = c.tool("oms_peer_result", json!({"operation":op}));
    bad(&r, "stalled");
    assert_eq!(
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["status"],
        "failed"
    );
    std::fs::write(dir.join("status"), "bad status").unwrap();
    assert_eq!(
        payload(&c.tool("oms_peer_result", json!({"operation":op})))["exit"],
        1
    );
    std::fs::write(dir.join("status"), "0").unwrap();
    assert_eq!(
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["status"],
        "completed"
    );
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["error"]["code"],
        -32602
    );
    std::fs::write(
        dir.join("cancel-observed.json"),
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-observed"}).to_string(),
    )
    .unwrap();
    assert_eq!(
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["status"],
        "cancelled"
    );
    let legacy = "ask-20190101T000000Z-0badc0de";
    run_dir(&repo, legacy, json!({}), Some("0"));
    let listed = payload(&c.tool("oms_peer_operations", json!({})));
    assert_eq!(listed["total"], 2);
    assert_eq!(
        listed["operations"][1]["started_at"],
        "2019-01-01T00:00:00Z"
    );
    let unsafe_id = "ask-20200101T000000Z-aabbccdd";
    std::os::unix::fs::symlink(&dir, repo.join(".oms/artifacts/mcp").join(unsafe_id)).unwrap();
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":unsafe_id,"_meta":modern()}))["error"]["code"],
        -32602
    );
    for badid in ["../x", "--help", ""] {
        bad(
            &c.tool("oms_peer_result", json!({"operation":badid})),
            "operation must",
        );
        assert_eq!(
            c.call("tasks/get", json!({"taskId":badid,"_meta":modern()}))["error"]["code"],
            -32602
        );
    }
    for wait in [json!(true), json!(-1), json!(51), json!("1"), Value::Null] {
        bad(
            &c.tool(
                "oms_peer_result",
                json!({"operation":op,"wait_seconds":wait}),
            ),
            "wait_seconds",
        );
    }
    for after in [json!(true), Value::Null, json!("result:bad")] {
        bad(
            &c.tool("oms_peer_result", json!({"operation":op,"after":after})),
            "after must",
        );
    }
}
fn peer_start_threads() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, false, true, false);
    for (args, message) in [
        (json!({"kind":"consult","prompt":" "}), "prompt is required"),
        (
            json!({"kind":"consult","prompt":"q","providers":"--help"}),
            "registered provider",
        ),
        (
            json!({"kind":"advise","prompt":"q","providers":"codex,claude"}),
            "single provider",
        ),
        (
            json!({"kind":"advise","prompt":"q","new_thread":true}),
            "new_thread applies",
        ),
        (
            json!({"kind":"consult","prompt":"q","thread":"--help"}),
            "thread must",
        ),
        (
            json!({"kind":"consult","prompt":"q","thread":"active","new_thread":true}),
            "exclusive",
        ),
        (json!({"kind":"unknown","prompt":"q"}), "kind must"),
        (
            json!({"kind":"ask","prompt":"x".repeat(65537)}),
            "65536-byte",
        ),
    ] {
        bad(&c.tool("oms_peer_start", args), message);
    }
    for value in [json!(true), json!(-1), json!(4), Value::Null, json!("1")] {
        bad(
            &c.tool(
                "oms_peer_start",
                json!({"kind":"ask","prompt":"q","debate_rounds":value}),
            ),
            "debate_rounds",
        );
    }
    for key in ["include_memory", "include_task"] {
        for value in [Value::Null, json!(1), json!("true")] {
            let mut a = json!({"kind":"consult","prompt":"q"});
            a[key] = value;
            bad(&c.tool("oms_peer_start", a), "boolean");
        }
    }
    assert!(!repo.join(".oms").exists());
    for (kind, target) in [
        ("consult", "dsh"),
        ("advise", "github-copilot:model=fixture"),
        ("ask", "cn"),
    ] {
        let started=c.tool("oms_peer_start",json!({"kind":kind,"prompt":"q","providers":target,"include_task":true,"include_memory":false}));
        good(&started);
        let p = payload(&started);
        let op = p["operation"].as_str().unwrap();
        let r = c.tool("oms_peer_result", json!({"operation":op,"wait_seconds":50}));
        good(&r);
        assert!(
            payload(&r)["answer"]
                .as_str()
                .unwrap()
                .contains("fixture answer")
        );
        let argv = record(&source)["argv"].as_array().unwrap().clone();
        assert_eq!(argv[0], if kind == "ask" { "peer-ask" } else { kind });
        assert!(argv.contains(&json!("--no-memory")));
        assert_eq!(argv.contains(&json!("--task")), kind != "consult");
    }
    good(&c.tool(
        "oms_peer_start",
        json!({"kind":"message","thread":"active","prompt":"message"}),
    ));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "thread", "--repo", repo, "--id", "active", "append", "--role", "note", "--text",
            "message"
        ])
    );
    good(&c.tool(
        "oms_peer_start",
        json!({"kind":"ack","thread":"active","after":"cursor","consumer":"reader"}),
    ));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "thread",
            "--repo",
            repo,
            "--id",
            "active",
            "ack",
            "--after",
            "cursor",
            "--consumer",
            "reader"
        ])
    );
    good(&c.tool(
        "oms_peer_result",
        json!({"thread":"active","after":"cursor"}),
    ));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "thread",
            "--repo",
            repo,
            "--id",
            "active",
            "updates",
            "--after",
            "cursor",
            "--max-bytes",
            "8192",
            "--json"
        ])
    );
    good(&c.tool("oms_peer_start",json!({"kind":"message","thread":"active","participant":"member","prompt":"room note","to_participant":"owner","message_kind":"status","message_id":"msg-one"})));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "room",
            "--repo",
            repo,
            "--id",
            "active",
            "--participant",
            "member",
            "send",
            "--text",
            "room note",
            "--to",
            "owner",
            "--kind",
            "status",
            "--message-id",
            "msg-one"
        ])
    );
    good(&c.tool(
        "oms_peer_start",
        json!({"kind":"ack","thread":"active","participant":"member","message_ids":["msg-one"]}),
    ));
    assert!(
        record(&source)["argv"]
            .as_array()
            .unwrap()
            .contains(&json!("--message"))
    );
    for ids in [json!([]), json!(["--help"]), json!(vec!["one"; 201])] {
        bad(
            &c.tool(
                "oms_peer_start",
                json!({"kind":"ack","thread":"active","participant":"member","message_ids":ids}),
            ),
            "message_ids",
        );
    }
    let mut worker = Client::new(&source, &repo, false, false, true);
    bad(
        &worker.tool("oms_peer_start", json!({"kind":"consult","prompt":"q"})),
        "delegated worker",
    );
    bad(
        &worker.tool(
            "oms_peer_start",
            json!({"kind":"message","thread":"active","prompt":"q"}),
        ),
        "delegated worker",
    );
    good(&worker.tool("oms_peer_start",json!({"kind":"message","thread":"active","participant":"member","prompt":"note","to_participant":"owner"})));
}
fn peer_exchange_repo_argv() {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let t = Temp::new();
    let source = t.source();
    let repo = t.repo().canonicalize().unwrap();
    let physical = repo.join("physical/child");
    std::fs::create_dir_all(&physical).unwrap();
    let alias = repo.join("alias");
    std::os::unix::fs::symlink(&physical, &alias).unwrap();
    let spaced = repo.join("space λ");
    std::fs::create_dir(&spaced).unwrap();
    let raw_repo = t.0.join(OsString::from_vec(b"raw-\xff".to_vec()));
    std::fs::create_dir(&raw_repo).unwrap();
    std::os::unix::fs::symlink(&raw_repo, repo.join("raw-alias")).unwrap();
    std::fs::write(repo.join("file"), "fixture").unwrap();
    let mut c = Client::new(&source, &repo, false, true, false);
    let exchanges = [
        ("oms_peer_start", json!({"kind":"message","prompt":"note"})),
        (
            "oms_peer_start",
            json!({"kind":"message","prompt":"topic","new_thread":true}),
        ),
        (
            "oms_peer_start",
            json!({"kind":"ack","after":"cursor","consumer":"reader"}),
        ),
        ("oms_peer_result", json!({"after":"cursor"})),
        (
            "oms_peer_start",
            json!({"kind":"message","participant":"member","prompt":"note"}),
        ),
        (
            "oms_peer_start",
            json!({"kind":"ack","participant":"member","message_ids":["msg-one"]}),
        ),
        (
            "oms_peer_result",
            json!({"participant":"member","after":"cursor"}),
        ),
    ];
    let mut paths = vec![
        (None, repo.as_os_str().to_owned()),
        (Some(Value::Null), repo.as_os_str().to_owned()),
        (Some(json!("")), repo.as_os_str().to_owned()),
        (Some(json!(".")), OsString::from(".")),
        (Some(json!("././")), OsString::from(".")),
        (Some(json!("alias")), OsString::from("alias")),
        (Some(json!("./alias//./")), OsString::from("alias")),
        (Some(json!("alias/../")), OsString::from("alias/..")),
        (Some(json!("space λ/")), OsString::from("space λ")),
        (Some(json!("raw-alias")), OsString::from("raw-alias")),
        (Some(json!(alias)), alias.as_os_str().to_owned()),
        (Some(json!("/")), OsString::from("/")),
        (Some(json!("//")), OsString::from("//")),
        (Some(json!("///")), OsString::from("/")),
        (Some(json!("/".repeat(4095))), OsString::from("/")),
        (Some(json!("/".repeat(4096))), OsString::from("/")),
    ];
    let suffix = alias.strip_prefix("/").unwrap().to_str().unwrap();
    for (prefix, root) in [("/", "/"), ("//", "//"), ("///", "/")] {
        paths.push((
            Some(json!(format!("{prefix}{suffix}//./"))),
            OsString::from(format!("{root}{suffix}")),
        ));
    }
    for (raw, expected) in paths {
        for (tool, exchange) in &exchanges {
            let mut args = exchange.clone();
            args["thread"] = json!("active");
            if let Some(raw) = &raw {
                args["repo"] = raw.clone();
            }
            good(&c.tool(tool, args.clone()));
            let call = record(&source);
            assert_eq!(call["argv_bytes"][2], json!(expected.as_bytes()), "{args}");
            assert_eq!(call["cwd_bytes"], json!(repo.as_os_str().as_bytes()));
            assert_eq!(
                call["argv"][0],
                if args["participant"].is_string() {
                    "room"
                } else {
                    "thread"
                }
            );
        }
    }
    let before = std::fs::read(source.join("last-call.json")).unwrap();
    for (raw, message) in [
        (json!(0), "repo must be a string"),
        (json!(false), "repo must be a string"),
        (json!("\0"), "repo contains a NUL byte"),
        (json!("missing"), "no such repository directory"),
        (json!("file"), "no such repository directory"),
        (json!("x".repeat(4097)), "repo exceeds 4096-byte limit"),
        (json!("/".repeat(4097)), "repo exceeds 4096-byte limit"),
    ] {
        for (tool, exchange) in &exchanges {
            let mut args = exchange.clone();
            args["thread"] = json!("active");
            args["repo"] = raw.clone();
            bad(&c.tool(tool, args), message);
            assert_eq!(
                std::fs::read(source.join("last-call.json")).unwrap(),
                before
            );
        }
    }
    let raw_repo = raw_repo.canonicalize().unwrap();
    let mut raw = Client::new(&source, &raw_repo, false, true, false);
    for (tool, exchange) in &exchanges {
        let mut args = exchange.clone();
        args["thread"] = json!("active");
        good(&raw.tool(tool, args));
        assert_eq!(
            record(&source)["argv_bytes"][2],
            json!(raw_repo.as_os_str().as_bytes())
        );
        assert_eq!(
            record(&source)["cwd_bytes"],
            json!(raw_repo.as_os_str().as_bytes())
        );
    }
    std::fs::write(
        source.join("fixture.json"),
        json!({"exit":7,"stderr":"exchange failed"}).to_string(),
    )
    .unwrap();
    bad(
        &c.tool("oms_peer_result", json!({"repo":"alias","thread":"active"})),
        "exchange failed",
    );
    assert_eq!(record(&source)["argv"][2], "alias");
    assert!(!repo.join(".oms").exists());
    assert!(!raw_repo.join(".oms").exists());
}
fn peer_argv_goldens() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, false, false);
    let cases: Value = serde_json::from_str(include_str!("peer-argv.json")).unwrap();
    for row in cases.as_array().unwrap() {
        let mut a = json!({"kind":row["kind"],"prompt":"Context probe","providers":"codex:model=fixture","thread":"active","debate_rounds":row["rounds"]});
        a.as_object_mut()
            .unwrap()
            .extend(row["context"].as_object().unwrap().clone());
        let r = c.tool("oms_peer_start", a);
        good(&r);
        let p = payload(&r);
        let mut wait = p["result_arguments"].clone();
        wait["wait_seconds"] = json!(50);
        good(&c.tool("oms_peer_result", wait));
        let expected: Vec<String> = row["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                v.as_str()
                    .unwrap()
                    .replace("__REPO__", repo.to_str().unwrap())
                    .replace("__RUN__", p["run_dir"].as_str().unwrap())
            })
            .collect();
        assert_eq!(record(&source)["argv"], json!(expected));
    }
}
fn cancellation_concurrency() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let op = "consult-20200101T000000Z-deadbeef";
    let dir = run_dir(&repo, op, json!({"cancellation_supported":true}), None);
    let mut threads = Vec::new();
    for _ in 0..4 {
        let s = source.clone();
        let r = repo.clone();
        threads.push(std::thread::spawn(move || {
            let mut c = Client::new(&s, &r, false, true, false);
            let v = c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}));
            assert_eq!(v["result"], json!({"resultType":"complete"}), "{v}");
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let row: Value =
        serde_json::from_slice(&std::fs::read(dir.join("cancel-request.json")).unwrap()).unwrap();
    assert_eq!(row["task_id"], op);
    assert_eq!(
        std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .count(),
        0
    );
}
fn peer_detached_cancel() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    fixture_settings(&source, json!({"gate":true}));
    let mut c = Client::new(&source, &repo, false, true, false);
    let started=c.call("tools/call",json!({"name":"oms_peer_start","arguments":{"kind":"consult","prompt":"hold"},"_meta":modern()}));
    assert_eq!(started["result"]["resultType"], "task");
    let op = started["result"]["taskId"].as_str().unwrap().to_owned();
    let dir = repo.join(".oms/artifacts/mcp").join(&op);
    until(|| dir.join("fixture-ready").is_file());
    drop(c);
    let mut c = Client::new(&source, &repo, false, true, false);
    assert_eq!(
        payload(&c.tool("oms_peer_result", json!({"operation":op})))["status"],
        "running"
    );
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["result"]["resultType"],
        "complete"
    );
    until(|| dir.join("status").is_file());
    assert_eq!(
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["status"],
        "cancelled"
    );
    assert!(!source.join("release").exists());
    let rows = payload(&c.tool("oms_peer_operations", json!({})));
    assert_eq!(rows["operations"][0]["termination_reason"], "cancelled");
    let target = dir.clone();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let s = source.clone();
        let r = repo.clone();
        let op = op.clone();
        handles.push(std::thread::spawn(move || {
            let mut c = Client::new(&s, &r, false, true, false);
            good(&c.tool("oms_peer_result", json!({"operation":op})));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert!(target.join("status").is_file());
}

fn boundary_limits() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, false, false);
    for (name, key, limit) in [
        ("oms_project_graph_query", "query", 512),
        ("oms_project_graph_trace", "node", 512),
        ("oms_project_graph_api", "path", 1024),
        ("oms_project_graph_affected", "base", 256),
    ] {
        let mut a = json!({});
        a[key] = json!("a".repeat(limit));
        good(&c.tool(name, a.clone()));
        a[key] = json!("a".repeat(limit + 1));
        bad(&c.tool(name, a), "requires");
    }
    for prompt in ["", "\u{1c}"] {
        bad(
            &c.tool("oms_peer_start", json!({"kind":"consult","prompt":prompt})),
            "prompt is required",
        );
    }
    let started=c.tool("oms_peer_start",json!({"kind":"consult","prompt":"q".repeat(65536),"providers":format!("codex:model={}","m".repeat(160))}));
    good(&started);
    let p = payload(&started);
    good(&c.tool("oms_peer_result", p["result_arguments"].clone()));
    bad(&c.tool("oms_peer_start",json!({"kind":"consult","prompt":"q","providers":format!("codex:model={}","m".repeat(161))})),"registered provider");
    good(&c.tool(
        "oms_peer_start",
        json!({"kind":"message","thread":"t".repeat(128),"prompt":"q".repeat(4000)}),
    ));
    bad(
        &c.tool(
            "oms_peer_start",
            json!({"kind":"message","thread":"t".repeat(129),"prompt":"q"}),
        ),
        "128-byte",
    );
    bad(
        &c.tool(
            "oms_peer_start",
            json!({"kind":"message","thread":"active","prompt":"q".repeat(4001)}),
        ),
        "4000-byte",
    );
    good(&c.tool("oms_peer_start",json!({"kind":"ack","thread":"active","participant":"member","message_ids":vec!["msg-one";200]})));
    let model = json!({"kind":"project","nodes":vec![json!({});120],"edges":vec![json!({});360]});
    fixture_settings(&source, json!({"stdout":model.to_string()}));
    good(&c.tool("oms_project_graph_render", json!({"task":"t".repeat(2048)})));
    fixture_settings(
        &source,
        json!({"stdout":json!({"kind":"project","nodes":[],"edges":vec![json!({});361]}).to_string()}),
    );
    bad(
        &c.tool("oms_project_graph_render", json!({})),
        "node or edge bound",
    );
    let base = json!({"kind":"project","nodes":[],"edges":[]}).to_string();
    let exact = format!("{base}{}", " ".repeat(262144 - base.len()));
    fixture_settings(&source, json!({"stdout":exact}));
    good(&c.tool("oms_project_graph_render", json!({})));
    let empty = t.0.join("limits-repo");
    std::fs::create_dir(&empty).unwrap();
    let mut c = Client::new(&source, &empty, false, false, false);
    for i in 0..20 {
        run_dir(
            &empty,
            &format!("ask-20200101T000000Z-{i:08x}"),
            json!({}),
            Some("0"),
        );
    }
    let p = payload(&c.tool("oms_peer_operations", json!({})));
    assert_eq!(p["shown"], 20);
    assert!(p.get("truncated").is_none());
    run_dir(
        &empty,
        "ask-20200101T000000Z-ffffffff",
        json!({}),
        Some("0"),
    );
    let p = payload(&c.tool("oms_peer_operations", json!({})));
    assert_eq!(p["shown"], 20);
    assert_eq!(p["total"], 21);
    assert_eq!(p["truncated"], true);
    let op = "ask-20200101T000000Z-ffffffff";
    let dir = empty.join(".oms/artifacts/mcp").join(op);
    std::fs::write(
        dir.join("meta.json"),
        json!({"answer_storage":"operation"}).to_string(),
    )
    .unwrap();
    let artifact = dir.join("answers/exact.md");
    let framing = "## Output\n\n## Exit\n0\n";
    let data = format!(
        "## Output\n{}\n## Exit\n0\n",
        "x".repeat(8388608 - framing.len())
    );
    assert_eq!(data.len(), 8388608);
    std::fs::write(&artifact, data).unwrap();
    std::fs::write(
        dir.join("run.log"),
        format!("artifact: {}\n", artifact.display()),
    )
    .unwrap();
    let p = payload(&c.tool("oms_peer_result", json!({"operation":op})));
    assert_eq!(p["answer_truncated"], true);
    assert!(p.get("answer_errors").is_none());
    let mut query = p["answer_details"][0]["read_arguments"].clone();
    query["answer_query"] = json!("x".repeat(256));
    good(&c.tool("oms_peer_result", query));
}
fn owner_validation() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_oms-mcp-peer-owner"));
    assert!(!Command::new(&binary).output().unwrap().status.success());
    for (i,launch)in[json!({}),json!({"kind":"write"}),json!({"kind":"consult","targets":["--help"],"thread":"","new_thread":false,"debate_rounds":0,"context_options":{}}),json!({"kind":"consult","targets":[],"thread":"","new_thread":false,"debate_rounds":0,"context_options":{"include_task":1}})].into_iter().enumerate(){let op=format!("consult-20200101T000000Z-{i:08x}");let dir=run_dir(&repo,&op,json!({}),None);std::fs::write(dir.join("launch.json"),launch.to_string()).unwrap();let output=Command::new(&binary).args([source.as_os_str(),repo.as_os_str(),std::ffi::OsStr::new(&op),source.join("bin/oms").as_os_str()]).env("PATH","").env("OMS_HARNESS_CHILD","0").env("OMS_HARNESS_DELEGATE_DEPTH","0").output().unwrap();assert!(!output.status.success());assert_eq!(std::fs::read_to_string(dir.join("status")).unwrap(),"1\n");assert!(!source.join("last-call.json").exists());}
    fixture_settings(&source, json!({"gate":true}));
    let mut c = Client::new(&source, &repo, false, true, false);
    let started = c.tool("oms_peer_start", json!({"kind":"consult","prompt":"hold"}));
    good(&started);
    let p = payload(&started);
    let op = p["operation"].as_str().unwrap();
    let dir = repo.join(".oms/artifacts/mcp").join(op);
    until(|| dir.join("fixture-ready").is_file());
    let duplicate = Command::new(&binary)
        .args([
            source.as_os_str(),
            repo.as_os_str(),
            std::ffi::OsStr::new(op),
            source.join("bin/oms").as_os_str(),
        ])
        .env("PATH", "")
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(!dir.join("status").exists());
    let cancel = c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}));
    assert_eq!(cancel["result"]["resultType"], "complete");
    until(|| dir.join("status").is_file());
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--missing-owner")
        .arg(&source)
        .arg(&repo)
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .output()
        .unwrap();
    assert!(output.status.success());
    bad(
        &serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        "not configured",
    );
}
struct Recorder(std::sync::Mutex<Vec<(Vec<String>, PathBuf)>>);
impl CommandExecutor for Recorder {
    fn execute(&self, r: &CommandRequest<'_>) -> Result<Captured, String> {
        let argv = r
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        self.0.lock().unwrap().push((argv, r.cwd.to_owned()));
        Ok(Captured::from_output(0, b"{\"ok\":true}\n", b""))
    }
}
// Every fixed-argv tool must reach the injected executor: the source root has
// no launcher and no program is spawned, so any subprocess attempt fails.
fn executor_injection() {
    let t = Temp::new();
    let source = t.0.join("bare");
    std::fs::create_dir(&source).unwrap();
    let repo = t.repo();
    let flag = AtomicBool::new(false);
    let rec = std::sync::Arc::new(Recorder(Default::default()));
    let mut server =
        Server::with_executor(&source, &repo, &flag, config(true, false), rec.clone()).unwrap();
    let rows: Value = serde_json::from_str(include_str!("fixed-argv.json")).unwrap();
    for (i, row) in rows.as_array().unwrap().iter().enumerate() {
        let out = server
            .handle(&json!({"id":i,"method":"tools/call","params":{"name":row["name"],"arguments":row["arguments"]}}))
            .unwrap();
        let out: Value = serde_json::from_str(&out.to_string()).unwrap();
        good(&out);
        let seen = rec.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(json!(seen.0), row["argv"], "{}", row["name"]);
        assert_eq!(seen.1, repo.canonicalize().unwrap());
    }
    assert_eq!(rec.0.lock().unwrap().len(), rows.as_array().unwrap().len());
}
fn clear_python(c: &mut Command) -> &mut Command {
    c.env_remove("PYTHONUTF8").env_remove("PYTHONIOENCODING")
}
fn exec_file(dir: &Path, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("oms");
    std::fs::copy(std::env::current_exe().unwrap(), &p).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p
}
// Pinned native program: no Python setting reaches it even when inherited, and
// an invalid pin is refused rather than skipped to another candidate.
fn native_program_has_no_python_env() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let exe = std::env::current_exe().unwrap();
    let out = clear_python(
        Command::new(&exe)
            .args(["--resolve-run"])
            .arg(&source)
            .arg(&repo),
    )
    .output()
    .unwrap();
    assert!(out.status.success(), "{out:?}");
    let r: Value =
        serde_json::from_slice(&std::fs::read(source.join("last-call.json")).unwrap()).unwrap();
    assert_eq!(r["utf8"], "");
    let flag = AtomicBool::new(false);
    let mut cfg = config(false, false);
    for bad in ["relative/oms", "missing/oms"] {
        cfg.oms_program = Some(PathBuf::from(bad));
        assert!(
            Server::new(&source, &repo, &flag, cfg.clone()).is_err(),
            "{bad}"
        );
    }
    let plain = t.0.join("plain");
    std::fs::write(&plain, "x").unwrap();
    cfg.oms_program = Some(plain);
    assert!(Server::new(&source, &repo, &flag, cfg.clone()).is_err());
    assert!(SubprocessExecutor::with_program(PathBuf::from("oms")).is_err());
}
// Resolution runs in a child with a controlled PATH and cwd (no globals here).
fn resolve_child(cwd: &Path, path: &str, pin: Option<&Path>, owner: &Path) -> String {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.arg("--resolve")
        .arg(owner)
        .current_dir(cwd)
        .env("PATH", path);
    if let Some(p) = pin {
        c.arg(p);
    }
    let out = clear_python(&mut c).output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
fn resolution_trust() {
    let t = Temp::new();
    let work = t.0.join("work");
    std::fs::create_dir(&work).unwrap();
    let repo_oms = exec_file(&work, 0o755);
    let owner_dir = t.0.join("owner");
    std::fs::create_dir(&owner_dir).unwrap();
    let owner = owner_dir.join("oms-mcp-peer-owner");
    std::fs::write(&owner, "x").unwrap();
    // Workspace shape: an `oms` sits beside the owner, but the pin wins.
    let sibling = exec_file(&owner_dir, 0o755);
    let pin = exec_file(&t.0.join("pin"), 0o755);
    assert_eq!(
        resolve_child(&work, "", Some(&pin), &owner),
        format!("{}\n", pin.display())
    );
    assert_eq!(
        resolve_child(&work, "", None, &owner),
        format!("{}\n", sibling.display())
    );
    // Relative owner and relative PATH entries never reach the repo's `oms`.
    let rel_owner = resolve_child(&work, "", None, Path::new("oms-mcp-peer-owner"));
    assert!(rel_owner.starts_with("error"), "{rel_owner}");
    for path in [".", "", "relative"] {
        let got = resolve_child(&work, path, None, Path::new("rel/owner"));
        assert!(got.starts_with("error"), "{path:?}: {got}");
    }
    assert!(repo_oms.is_file());
    // A 0644 `oms` earlier on PATH is skipped for a later executable one.
    let (noexec, good_dir) = (t.0.join("a"), t.0.join("b"));
    exec_file(&noexec, 0o644);
    let later = exec_file(&good_dir, 0o755);
    let path = format!("{}:{}", noexec.display(), good_dir.display());
    assert_eq!(
        resolve_child(&work, &path, None, Path::new("rel/owner")),
        format!("{}\n", later.display())
    );
    // Nothing usable: a clear error, never a bare `oms` fallback.
    let only = format!("{}", noexec.display());
    let got = resolve_child(&work, &only, None, Path::new("rel/owner"));
    assert!(got.contains("native oms executable not found"), "{got}");
    // A pin that is not executable is refused even with a good sibling.
    let bad = exec_file(&t.0.join("bad"), 0o644);
    assert!(resolve_child(&work, "", Some(&bad), &owner).starts_with("error"));
}
// Server::new with only PATH (no pin, no launcher) resolves and runs a native
// `oms` with no Python variables.
fn path_resolution_runs_native() {
    let t = Temp::new();
    let source = t.0.join("bare");
    std::fs::create_dir(&source).unwrap();
    let repo = t.repo();
    let bin = t.0.join("pathbin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), bin.join("oms")).unwrap();
    let out = clear_python(
        Command::new(std::env::current_exe().unwrap())
            .arg("--path-run")
            .arg(&source)
            .arg(&repo)
            .env("PATH", &bin),
    )
    .output()
    .unwrap();
    assert!(out.status.success(), "{out:?}");
}
// Unresolvable oms: oms_peer_start refuses before any `.oms/` state exists, and
// a directly launched owner never publishes `pid`.
fn start_refusal_before_mutation() {
    let t = Temp::new();
    let source = t.0.join("bare");
    std::fs::create_dir(&source).unwrap();
    let repo = t.repo();
    let exe = std::env::current_exe().unwrap();
    let empty = t.0.join("emptybin");
    std::fs::create_dir(&empty).unwrap();
    for (mode, path) in [("bad", ""), ("none", empty.to_str().unwrap())] {
        let out = clear_python(
            Command::new(&exe)
                .args(["--start-refused"])
                .arg(&source)
                .arg(&repo)
                .arg(mode),
        )
        .env("PATH", path)
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .output()
        .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains("\"isError\":true"),
            "{mode}: {out:?}"
        );
        assert!(text.contains("oms"), "{text}");
        assert!(
            !repo.join(".oms").exists(),
            "{mode}: state created before refusal"
        );
    }
    let (source, repo) = (t.source(), t.0.join("repo2"));
    std::fs::create_dir(&repo).unwrap();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_oms-mcp-peer-owner"));
    let valid = json!({"kind":"consult","targets":[],"thread":"","new_thread":false,"debate_rounds":0,"context_options":{}});
    // An unpinned owner may legitimately find a sibling `oms` in a workspace build,
    // so only invalid pins are discriminating here.
    for (i, pin) in [Some("relative/oms"), Some("/nonexistent/oms")]
        .into_iter()
        .enumerate()
    {
        let op = format!("consult-20200101T000000Z-{:08x}", 0xa0 + i);
        let dir = run_dir(&repo, &op, json!({}), None);
        std::fs::write(dir.join("launch.json"), valid.to_string()).unwrap();
        let mut c = Command::new(&binary);
        c.args([
            source.as_os_str(),
            repo.as_os_str(),
            std::ffi::OsStr::new(&op),
        ]);
        if let Some(p) = pin {
            c.arg(p);
        }
        let out = c
            .env("PATH", "")
            .env("OMS_HARNESS_CHILD", "0")
            .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
            .output()
            .unwrap();
        assert!(!out.status.success(), "{out:?}");
        assert!(
            !dir.join("pid").exists(),
            "pid published before oms resolved"
        );
        assert_eq!(std::fs::read_to_string(dir.join("status")).unwrap(), "1\n");
    }
}
// With no pin and no sibling, the server forwards its own resolved `oms`.
fn start_forwards_resolved_oms() {
    let t = Temp::new();
    let source = t.0.join("bare");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("fixture-owner"), "x").unwrap();
    let repo = t.repo();
    let bin = t.0.join("pathbin");
    let program = exec_file(&bin, 0o755);
    let out = clear_python(
        Command::new(std::env::current_exe().unwrap())
            .arg("--forward-run")
            .arg(&source)
            .arg(&repo),
    )
    .env("PATH", &bin)
    .env("OMS_HARNESS_CHILD", "0")
    .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
    .output()
    .unwrap();
    assert!(out.status.success(), "{out:?}");
    let runs = repo.join(".oms/artifacts/mcp");
    let op = std::fs::read_dir(&runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let recorded = std::fs::read_to_string(op.join("owner-args")).unwrap();
    assert_eq!(
        recorded.lines().nth(3),
        Some(program.to_str().unwrap()),
        "{recorded}"
    );
}
fn fake_oms() {
    let source = PathBuf::from(std::env::args_os().next().unwrap())
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let cfg: Value = std::fs::read(source.join("fixture.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({}));
    use std::os::unix::ffi::OsStrExt;
    let cwd = std::env::current_dir().unwrap();
    let record = json!({"argv":args.iter().map(|a|a.to_str()).collect::<Vec<_>>(),"argv_bytes":args.iter().map(|a|a.as_bytes()).collect::<Vec<_>>(),"cwd":cwd.to_str(),"cwd_bytes":cwd.as_os_str().as_bytes(),"utf8":std::env::var("PYTHONUTF8").unwrap_or_default(),"child":std::env::var("OMS_HARNESS_CHILD").unwrap_or_default(),"pid":std::process::id()});
    let tmp = source.join(format!(".call-{}", std::process::id()));
    std::fs::write(&tmp, record.to_string()).unwrap();
    std::fs::rename(tmp, source.join("last-call.json")).unwrap();
    if args.first().is_some_and(|s| {
        s.to_str()
            .is_some_and(|s| ["consult", "advise", "peer-ask"].contains(&s))
    }) {
        let out =
            PathBuf::from(&args[args.iter().position(|s| s == "--artifact-dir").unwrap() + 1]);
        std::fs::create_dir_all(&out).unwrap();
        let run = out.parent().unwrap();
        if cfg["gate"] == true {
            std::fs::write(run.join("fixture-ready"), "ready").unwrap();
            until(|| source.join("release").exists() || run.join("cancel-request.json").exists());
            if run.join("cancel-request.json").exists() {
                let op = run.file_name().unwrap().to_string_lossy();
                std::fs::write(
                    run.join("cancel-observed.json"),
                    json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-observed"}).to_string(),
                )
                .unwrap();
            }
        }
        let artifact = out.join("fixture.md");
        std::fs::write(&artifact, "## Output\nfixture answer\n## Exit\n0\n").unwrap();
        println!("artifact: {}", artifact.display());
        println!("thread: fixture-thread");
        return;
    }
    if cfg["hold"] == true {
        until(|| source.join("release").exists());
    }
    if let Some(n) = cfg["invalid_ui"].as_u64() {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(b"{\"kind\":\"project\",\"nodes\":[],\"edges\":[],\"label\":\"")
            .unwrap();
        stdout.write_all(&vec![255; n as usize]).unwrap();
        stdout.write_all(b"\"}").unwrap();
        return;
    }
    let leading = cfg["leading"].as_u64().unwrap_or(0) as usize;
    print!("{}", " ".repeat(leading));
    if let Some(n) = cfg["repeat"].as_u64() {
        print!(
            "{}",
            cfg["character"].as_str().unwrap_or("x").repeat(n as usize)
        );
    } else {
        print!(
            "{}",
            cfg.get("stdout")
                .and_then(Value::as_str)
                .unwrap_or("{\"fixture\":true}")
        );
    }
    if let Some(e) = cfg["stderr"].as_str() {
        eprint!("{e}");
    }
    std::io::stdout().flush().unwrap();
    std::process::exit(cfg["exit"].as_i64().unwrap_or(0) as i32);
}
fn command_interruption() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    fixture_settings(&source, json!({"hold":true}));
    let flag = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let mut s =
                Server::new(&source, &repo, &flag, pinned(&source, config(false, false))).unwrap();
            s.handle(&json!({"id":1,"method":"tools/call","params":{"name":"oms_inbox"}}))
                .unwrap()
        });
        until(|| source.join("last-call.json").is_file());
        let pid = record(&source)["pid"].as_i64().unwrap() as i32;
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let r = waiter.join().unwrap();
        bad(
            &serde_json::from_str(&r.to_string()).unwrap(),
            "interrupted",
        );
        assert_eq!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()),
            Err(rustix::io::Errno::SRCH)
        );
    });
}
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if Path::new(&args[0]).file_name().is_some_and(|n| n == "oms") {
        fake_oms();
        return;
    }
    if (4..=5).contains(&args.len()) && Path::new(&args[1]).join("fixture-owner").is_file() {
        let dir = Path::new(&args[2])
            .join(".oms/artifacts/mcp")
            .join(&args[3]);
        let owner_args: Vec<_> = args[1..].iter().map(|a| a.to_string_lossy()).collect();
        std::fs::write(dir.join("owner-args"), owner_args.join("\n")).unwrap();
        std::fs::write(dir.join("status"), "0\n").unwrap();
        std::fs::write(dir.join("pid"), format!("{}\n", std::process::id())).unwrap();
        return;
    }
    if args.get(1).is_some_and(|a| a == "--forward-run") {
        // ARGS: --forward-run SOURCE REPO; no pin, so the server resolves `oms`.
        let flag = AtomicBool::new(false);
        let mut cfg = config(false, true);
        cfg.peer_owner = std::env::current_exe().unwrap();
        let mut s = Server::new(Path::new(&args[2]), Path::new(&args[3]), &flag, cfg).unwrap();
        let v = s.handle(&json!({"id":1,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"kind":"consult","prompt":"q"}}})).unwrap();
        println!("{v}");
        return;
    }
    if args.get(1).is_some_and(|a| a == "--start-refused") {
        // ARGS: --start-refused SOURCE REPO MODE; hosts embed their own executor.
        let flag = AtomicBool::new(false);
        let mut cfg = config(false, true);
        // deps/ holds no sibling `oms`, even after a workspace build.
        cfg.peer_owner = std::env::current_exe().unwrap();
        cfg.oms_program = (args[4] == "bad").then(|| PathBuf::from("relative/oms"));
        let rec = std::sync::Arc::new(Recorder(Default::default()));
        let mut s =
            Server::with_executor(Path::new(&args[2]), Path::new(&args[3]), &flag, cfg, rec)
                .unwrap();
        let v = s.handle(&json!({"id":1,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"kind":"ask","prompt":"q"}}})).unwrap();
        println!("{v}");
        return;
    }
    if args.get(1).is_some_and(|a| a == "--missing-owner") {
        let mut cfg = pinned(Path::new(&args[2]), config(false, false));
        cfg.peer_owner = PathBuf::from("missing");
        let flag = AtomicBool::new(false);
        let mut server = Server::new(Path::new(&args[2]), Path::new(&args[3]), &flag, cfg).unwrap();
        println!("{}",server.handle(&json!({"id":1,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"kind":"consult","prompt":"q"}}})).unwrap());
        return;
    }
    if args.get(1).is_some_and(|a| a == "--profile-check") {
        if Config::from_env(PathBuf::from("owner")).is_err() {
            std::process::exit(2);
        }
        return;
    }
    if args.get(1).is_some_and(|a| a == "--resolve") {
        // ARGS: --resolve OWNER [PIN]
        let pin = args.get(3).map(PathBuf::from);
        match oms_mcp_server::SubprocessExecutor::resolve(pin.as_deref(), Path::new(&args[2])) {
            Ok(e) => println!("{}", e.program().display()),
            Err(e) => print!("{e}"),
        }
        return;
    }
    if args.get(1).is_some_and(|a| a == "--resolve-run") {
        let flag = AtomicBool::new(false);
        let mut s = Server::new(
            Path::new(&args[2]),
            Path::new(&args[3]),
            &flag,
            pinned(Path::new(&args[2]), config(false, false)),
        )
        .unwrap();
        let v = s
            .handle(
                &json!({"id":1,"method":"tools/call","params":{"name":"oms_inbox","arguments":{}}}),
            )
            .unwrap();
        good(&serde_json::from_str(&v.to_string()).unwrap());
        return;
    }
    if args.get(1).is_some_and(|a| a == "--path-run") {
        let flag = AtomicBool::new(false);
        let mut cfg = config(false, false);
        // No sibling `oms` (a workspace build puts one beside the owner).
        cfg.peer_owner = PathBuf::from("/nonexistent/owner");
        let mut s = Server::new(Path::new(&args[2]), Path::new(&args[3]), &flag, cfg).unwrap();
        let v = s
            .handle(
                &json!({"id":1,"method":"tools/call","params":{"name":"oms_inbox","arguments":{}}}),
            )
            .unwrap();
        good(&serde_json::from_str(&v.to_string()).unwrap());
        let rec = Path::new(&args[2]).parent().unwrap().join("last-call.json");
        let r: Value = serde_json::from_slice(&std::fs::read(rec).unwrap()).unwrap();
        assert_eq!(r["utf8"], "");
        return;
    }
    if args.get(1).is_some_and(|a| a == "--exit") {
        return;
    }
    if args.get(1).is_some_and(|a| a == "--stdio") {
        let flag = AtomicBool::new(false);
        let full = args[4] == "full";
        let tasks = args[5] == "1";
        let mut fixture_config = pinned(Path::new(&args[2]), config(full, tasks));
        if Path::new(&args[2]).join("fixture-owner").is_file() {
            fixture_config.peer_owner = std::env::current_exe().unwrap();
        }
        oms_mcp_server::serve(
            Path::new(&args[2]),
            Path::new(&args[3]),
            &flag,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            fixture_config,
        )
        .unwrap();
        return;
    }
    let cases: [(&str, fn()); 27] = [
        ("lossless_values", lossless_values),
        ("raw_peer_paths", raw_peer_paths),
        ("parser_reference", parser_reference),
        ("owner_validation", owner_validation),
        ("environment_profile", environment_profile),
        ("environment_authority", environment_authority),
        ("boundary_limits", boundary_limits),
        ("canonical_protocol_cases", canonical_protocol_cases),
        ("peer_argv_goldens", peer_argv_goldens),
        ("cancellation_concurrency", cancellation_concurrency),
        ("command_interruption", command_interruption),
        ("discovery_protocols", discovery_protocols),
        ("transport_bounds", transport_bounds),
        ("fixed_argv", fixed_argv),
        ("output_bounds", output_bounds),
        ("graph_render", graph_render),
        ("peer_reading", peer_reading),
        ("peer_status_tasks", peer_status_tasks),
        ("peer_start_threads", peer_start_threads),
        ("peer_exchange_repo_argv", peer_exchange_repo_argv),
        ("peer_detached_cancel", peer_detached_cancel),
        ("executor_injection", executor_injection),
        (
            "native_program_has_no_python_env",
            native_program_has_no_python_env,
        ),
        ("resolution_trust", resolution_trust),
        (
            "start_refusal_before_mutation",
            start_refusal_before_mutation,
        ),
        ("start_forwards_resolved_oms", start_forwards_resolved_oms),
        ("path_resolution_runs_native", path_resolution_runs_native),
    ];
    let mut failed = 0;
    for (name, test) in cases {
        if args.get(1).is_some_and(|s| s != "--case") || args.get(2).is_some_and(|s| s != name) {
            continue;
        }
        let result = std::panic::catch_unwind(test);
        if result.is_err() {
            eprintln!("FAIL {name}");
            failed += 1;
        } else {
            println!("PASS {name}");
        }
    }
    assert_eq!(failed, 0, "native functional scenarios failed");
    // Exercise the embedding ABI without process-global cwd mutation.
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let flag = AtomicBool::new(false);
    let mut server =
        Server::new(&source, &repo, &flag, pinned(&source, config(false, false))).unwrap();
    assert!(server.handle(&json!({"method":"ping"})).is_none());
}
