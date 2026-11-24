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
        Self(p.canonicalize().unwrap())
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
        Self::with_depth(
            source,
            repo,
            if full { "full" } else { "core" },
            tasks,
            child,
            std::ffi::OsStr::new("0"),
        )
    }
    fn with_depth(
        source: &Path,
        repo: &Path,
        profile: &str,
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
            .arg(profile)
            .env_remove("OMS_PANEL_REPO")
            .env_remove("OMS_PANEL_PROJECTS")
            .env_remove("OMS_ROOM_PARTICIPANT")
            .env_remove("OMS_PANEL_PROJECTS_FILE")
            .env("XDG_CONFIG_HOME", source.join("test-config"))
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
        (Some("chatgpt-panel".into()), true),
        (Some("mcp-app".into()), true),
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
fn cross_host_project_reads() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = Temp::new();
    let source = tmp.source();
    let repo = tmp.repo();
    std::fs::create_dir(repo.join(".oms")).unwrap();
    let mut c = Client::with_depth(&source, &repo, "mcp-app", true, false, "0".as_ref());
    for (host, extensions) in [
        ("generic-tools", json!({})),
        (
            "claude",
            json!({"io.modelcontextprotocol/ui":{"mimeTypes":["text/html;profile=mcp-app"]}}),
        ),
        (
            "antigravity",
            json!({"io.modelcontextprotocol/ui":{},"com.google.antigravity/ui":{"entrypoints":["aux_pane","global"]}}),
        ),
    ] {
        let init=c.call("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{"extensions":extensions},"clientInfo":{"name":host,"version":"1"}}));
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("oms_panel_snapshot")
        );
        let listed = c.call("tools/list", json!({}));
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);
        let opener = tools
            .iter()
            .find(|t| t["name"] == "oms_open_panel")
            .unwrap();
        assert_eq!(
            opener["_meta"]["openai/ui"]["entrypoints"][0]["type"],
            "global"
        );
        assert_eq!(
            opener["_meta"]["com.google.antigravity/ui"]["entrypoints"],
            json!([{"type":"aux_pane","title":"OMS"},{"type":"global","title":"OMS"}])
        );
        let uri = &opener["_meta"]["ui"]["resourceUri"];
        let view = c.call("resources/read", json!({"uri":uri}));
        assert!(
            view["result"]["contents"][0]["text"].as_str()
                == Some(include_str!("../src/conversation-panel.html"))
        );
        assert_eq!(
            view["result"]["contents"][0]["mimeType"],
            "text/html;profile=mcp-app"
        );
        assert_eq!(
            view["result"]["contents"][0]["_meta"]["ui"]["preferredDisplayMode"],
            "fullscreen"
        );
        assert_eq!(
            payload(&c.tool("oms_open_panel", json!({})))["readOnly"],
            true
        );
    }
    assert_eq!(
        payload(&c.tool("oms_list_projects", json!({})))["binding"],
        "unbound"
    );
    bad(&c.tool("oms_panel_snapshot", json!({})), "OMS_PANEL_REPO");
    for name in [
        "oms_panel_chat",
        "oms_start_main",
        "oms_decide_approval",
        "oms_list_approvals",
        "oms_assign_provider",
        "oms_assign_result",
    ] {
        assert_eq!(c.tool(name, json!({}))["error"]["code"], -32602);
    }
    let dir = source.join("test-config/oh-my-setting");
    std::fs::create_dir_all(&dir).unwrap();
    let map = dir.join("desktop-projects.json");
    std::fs::write(&map, json!({"fixture":repo}).to_string()).unwrap();
    std::fs::set_permissions(map, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        payload(&c.tool("oms_list_projects", json!({}))),
        json!({"binding":"catalog","projects":["fixture"],"readOnly":true})
    );
    let snapshot = json!({"schema":1,"kind":"oms-panel-snapshot","dashboard":{"scope":"repository","project":"fixture","conversationMain":null}});
    fixture_settings(&source, json!({"stdout":snapshot.to_string()}));
    let reply=c.call("tools/call",json!({"name":"oms_panel_snapshot","arguments":{"project":"fixture"},"_meta":{"threadId":"00000000-0000-4000-8000-000000000001"}}));
    assert_eq!(payload(&reply)["dashboard"]["project"], "fixture");
    assert!(
        !record(&source)["argv"]
            .to_string()
            .contains("conversation-digest")
    );
    assert!(!record(&source)["argv"].to_string().contains("00000000-"));
}
fn mcp_app_binding_boundaries() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = Temp::new();
    let source = tmp.source();
    let repo = tmp.repo();
    std::fs::create_dir(repo.join(".oms")).unwrap();
    let other = source.join("other-checkout");
    std::fs::create_dir_all(other.join(".oms")).unwrap();
    let dir = source.join("test-config/oh-my-setting");
    std::fs::create_dir_all(&dir).unwrap();
    let map = dir.join("desktop-projects.json");
    let bind = |value: Value| {
        std::fs::write(&map, value.to_string()).unwrap();
        std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    let snapshot = json!({"schema":1,"kind":"oms-panel-snapshot","dashboard":{"scope":"repository","project":"fixture"}});
    fixture_settings(&source, json!({"stdout":snapshot.to_string()}));
    let ask = |c: &mut Client, args: Value, meta: Value| {
        c.call(
            "tools/call",
            json!({"name":"oms_panel_snapshot","arguments":args,"_meta":meta}),
        )
    };
    let mut c = Client::with_depth(&source, &repo, "mcp-app", true, false, "0".as_ref());
    bind(json!({"fixture":repo}));
    let foreign =
        json!({"threadId":"00000000-0000-4000-8000-000000000001","room":"r","participant":"p"});
    good(&ask(&mut c, json!({"project":"fixture"}), foreign.clone()));
    let argv = record(&source)["argv"].to_string();
    assert!(argv.contains(repo.to_str().unwrap()) && !argv.contains("00000000-"));
    // Client-supplied paths, Room and participant names are never authority.
    for args in [
        json!({"project":"fixture","repo":other}),
        json!({"project":"fixture","participant":"p"}),
        json!({"project":"fixture","room":"r"}),
    ] {
        assert_eq!(ask(&mut c, args, json!({}))["error"]["code"], -32602);
    }
    // An alias absent from the catalog (and a raw path) is refused.
    bad(
        &ask(&mut c, json!({"project":other}), json!({})),
        "OMS_PANEL_REPO",
    );
    bad(&ask(&mut c, json!({}), json!({})), "OMS_PANEL_REPO");
    // Mutating or Room tools are refused by name and leave no new command record.
    let before = std::fs::read(source.join("last-call.json")).unwrap();
    for name in [
        "oms_start_main",
        "oms_decide_approval",
        "oms_panel_chat",
        "oms_peer_start",
        "oms_list_approvals",
    ] {
        let r = c.call(
            "tools/call",
            json!({"name":name,"arguments":{"project":"fixture"},"_meta":foreign}),
        );
        assert_eq!(r["error"]["code"], -32602, "{name}");
    }
    assert_eq!(
        std::fs::read(source.join("last-call.json")).unwrap(),
        before
    );
    // Replacing the alias target with a symlink is refused on the next call.
    let link = source.join("link-checkout");
    std::os::unix::fs::symlink(&repo, &link).unwrap();
    bind(json!({"fixture":link}));
    bad(
        &ask(&mut c, json!({"project":"fixture"}), json!({})),
        "OMS_PANEL_REPO",
    );
    // Repointing the alias grants exactly the newly bound checkout.
    bind(json!({"fixture":other}));
    good(&ask(&mut c, json!({"project":"fixture"}), json!({})));
    assert!(
        record(&source)["argv"]
            .to_string()
            .contains(other.to_str().unwrap())
    );
    // Group/world-writable, empty or sparse catalogs fail closed.
    bind(json!({"fixture":repo}));
    std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o666)).unwrap();
    bad(
        &ask(&mut c, json!({"project":"fixture"}), json!({})),
        "Invalid OMS project map",
    );
    for value in [
        json!({}),
        json!({"fixture":"relative/path"}),
        json!({"bad alias":repo}),
    ] {
        bind(value);
        bad(
            &ask(&mut c, json!({"project":"fixture"}), json!({})),
            "Invalid OMS project map",
        );
    }
    // Revocation between calls, then reopening, does not restore access.
    bind(json!({"fixture":repo}));
    good(&ask(&mut c, json!({"project":"fixture"}), json!({})));
    bind(json!({"other":other}));
    bad(
        &ask(&mut c, json!({"project":"fixture"}), json!({})),
        "OMS_PANEL_REPO",
    );
    std::fs::remove_file(&map).unwrap();
    let mut reopened = Client::with_depth(&source, &repo, "mcp-app", true, false, "0".as_ref());
    bad(
        &ask(&mut reopened, json!({"project":"fixture"}), json!({})),
        "OMS_PANEL_REPO",
    );
    bad(&ask(&mut reopened, json!({}), json!({})), "OMS_PANEL_REPO");
}
// Provider assignment: the Room store is proven in room_contract; here the
// MCP side must launch exactly once per requestId and trust only the host binding.
fn desktop_assignment() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    std::fs::create_dir(repo.join(".oms")).unwrap();
    let mut c = Client::with_depth(&source, &repo, "chatgpt-panel", false, false, "0".as_ref());
    let thread = "01900000-0000-7000-8000-0000000000bb";
    let req = "4d6acd01-bd18-4567-bf69-ea05fb048b27";
    let record_for = |status: &str, fresh: Option<bool>, operation: Value| {
        let mut r = json!({"request":req,"room":"r","participant":format!("ask-{req}"),"provider":"claude","role":"researcher",
            "title":"Find callers","task":"inspect the callers","readOnly":true,"thread":operation,
            "status":status,"operation":operation});
        if let Some(f) = fresh {
            r["fresh"] = json!(f);
        }
        r.to_string()
    };
    let call = |c: &mut Client, name: &str, args: Value, meta: Value| {
        c.call(
            "tools/call",
            json!({"name":name,"arguments":args,"_meta":meta}),
        )
    };
    let args = json!({"room":"r","requestId":req,"provider":"claude","title":"Find callers","task":"inspect the callers"});
    let runs = || std::fs::read_dir(repo.join(".oms/artifacts/mcp")).map_or(0, |d| d.count());
    // No host conversation binding, widget-chosen authority, or extra arguments: nothing runs.
    bad(
        &call(&mut c, "oms_assign_provider", args.clone(), json!({})),
        "bound conversation",
    );
    bad(
        &call(
            &mut c,
            "oms_assign_provider",
            args.clone(),
            json!({"threadId":"bad"}),
        ),
        "bound conversation",
    );
    let mut extra = args.clone();
    extra["thread"] = json!("other");
    assert_eq!(
        call(
            &mut c,
            "oms_assign_provider",
            extra,
            json!({"threadId":thread})
        )["error"]["code"],
        -32602
    );
    let mut bad_provider = args.clone();
    bad_provider["provider"] = json!("codex,claude");
    bad(
        &call(
            &mut c,
            "oms_assign_provider",
            bad_provider,
            json!({"threadId":thread}),
        ),
        "provider",
    );
    assert!(!source.join("last-call.json").exists());
    assert_eq!(runs(), 0);
    // A fresh intent launches once and records the returned operation.
    // The fixture cannot know the generated operation id, so its receipt names another one.
    fixture_settings(
        &source,
        json!({"room":{
        "desktop-assign":record_for("assigned",Some(true),Value::Null),
        "desktop-assign-receipt":record_for("started",None,json!("placeholder"))}}),
    );
    let first = call(
        &mut c,
        "oms_assign_provider",
        args.clone(),
        json!({"threadId":thread}),
    );
    good(&first);
    // A mismatching receipt is never trusted: the launch stays unconfirmed.
    assert_eq!(payload(&first)["status"], "unconfirmed");
    assert_eq!(runs(), 1, "the provider was launched exactly once");
    let operation = std::fs::read_dir(repo.join(".oms/artifacts/mcp"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    assert_eq!(payload(&first)["operation"], operation);
    // Let the detached fixture peer finish so it cannot overwrite the command record.
    until(|| {
        repo.join(".oms/artifacts/mcp")
            .join(&operation)
            .join("status")
            .is_file()
    });
    // A retry observes the recorded intent and never starts another provider.
    for (status, expected) in [("assigned", "unconfirmed"), ("started", "recorded")] {
        let op = if status == "started" {
            json!(operation)
        } else {
            Value::Null
        };
        fixture_settings(
            &source,
            json!({"room":{"desktop-assign":record_for(status,Some(false),op)}}),
        );
        let again = call(
            &mut c,
            "oms_assign_provider",
            args.clone(),
            json!({"threadId":thread}),
        );
        good(&again);
        assert_eq!(payload(&again)["status"], expected);
        assert_eq!(payload(&again)["role"], "researcher");
        assert_eq!(payload(&again)["readOnly"], true);
        assert_eq!(runs(), 1, "retry never redispatches");
        assert_eq!(record(&source)["argv"][1], "desktop-assign");
        let digest = record(&source)["argv"]
            .as_array()
            .unwrap()
            .iter()
            .position(|v| v == "--conversation-digest")
            .unwrap();
        assert_ne!(
            record(&source)["argv"][digest + 1],
            thread,
            "only a digest leaves the process"
        );
    }
    // A Room record for another request, role or thread is never trusted.
    let mut forged: Value =
        serde_json::from_str(&record_for("started", Some(false), json!(operation))).unwrap();
    forged["role"] = json!("main");
    fixture_settings(
        &source,
        json!({"room":{"desktop-assign":forged.to_string()}}),
    );
    bad(
        &call(
            &mut c,
            "oms_assign_provider",
            args.clone(),
            json!({"threadId":thread}),
        ),
        "unrecognized",
    );
    assert_eq!(runs(), 1);
    // The result reader takes the thread only from the Room receipt and exposes its turns.
    let updates = json!({"schema":1,"thread":operation,"turns":[{"role":"topic","text":"internal topic"},{"role":"answer","provider":"claude","quality":"ok","text":"callers found"},{"role":"answer","provider":"agy","quality":"ok","text":"foreign seat"}],"cursor":"c1","has_more":false});
    fixture_settings(
        &source,
        json!({"updates":updates.to_string(),"room":{"desktop-assign-status":record_for("started",None,json!(operation))}}),
    );
    let read = call(
        &mut c,
        "oms_assign_result",
        json!({"room":"r","requestId":req}),
        json!({"threadId":thread}),
    );
    good(&read);
    assert_eq!(
        read["result"]["_meta"]["assignment"]["transcript"]["turns"][0]["text"],
        "callers found"
    );
    assert_eq!(read["result"]["_meta"]["assignment"]["role"], "researcher");
    assert_eq!(payload(&read)["status"], "started");
    assert!(payload(&read)["transcript"].is_null());
    assert!(
        !read["result"]["content"]
            .to_string()
            .contains("fixture answer")
    );
    assert!(!read.to_string().contains("foreign seat"));
    // The existing participant reader shows the same turns in its recorded-result slot.
    let chat = json!({"room":"r","participant":format!("ask-{req}"),"provider":"claude","source":"room","self":false,"messages":[],"nextBefore":null});
    fixture_settings(
        &source,
        json!({"stdout":chat.to_string(),"updates":updates.to_string(),"room":{"desktop-assign-status":record_for("started",None,json!(operation))}}),
    );
    let page = call(
        &mut c,
        "oms_panel_chat",
        json!({"room":"r","participant":format!("ask-{req}")}),
        json!({"threadId":thread}),
    );
    good(&page);
    assert!(page["result"]["content"][0]["text"].as_str().is_some());
    assert!(
        page["result"]["_meta"]["chat"]["result"]["text"]
            .as_str()
            .unwrap()
            .contains("fixture answer")
    );
    bad(
        &call(
            &mut c,
            "oms_assign_result",
            json!({"room":"r","requestId":req}),
            json!({}),
        ),
        "bound conversation",
    );
    let foreign_req = "5d6acd01-bd18-4567-bf69-ea05fb048b27";
    let mut foreign: Value =
        serde_json::from_str(&record_for("started", None, json!(operation))).unwrap();
    foreign["request"] = json!(foreign_req);
    foreign["participant"] = json!(format!("ask-{foreign_req}"));
    fixture_settings(
        &source,
        json!({"updates":updates.to_string(),"room":{"desktop-assign-status":foreign.to_string()}}),
    );
    bad(
        &call(
            &mut c,
            "oms_assign_result",
            json!({"room":"r","requestId":foreign_req}),
            json!({"threadId":thread}),
        ),
        "does not match",
    );
    // Delegated workers and the read-only app profile have no assignment authority.
    let before = runs();
    let mut worker = Client::with_depth(&source, &repo, "chatgpt-panel", false, true, "1".as_ref());
    bad(
        &call(
            &mut worker,
            "oms_assign_provider",
            args.clone(),
            json!({"threadId":thread}),
        ),
        "parent app",
    );
    let mut app = Client::with_depth(&source, &repo, "mcp-app", true, false, "0".as_ref());
    assert_eq!(
        call(
            &mut app,
            "oms_assign_provider",
            args,
            json!({"threadId":thread})
        )["error"]["code"],
        -32602
    );
    assert_eq!(runs(), before);
}
fn conversation_panel() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::with_depth(&source, &repo, "chatgpt-panel", true, false, "0".as_ref());
    let tools = c.call("tools/list", json!({}))["result"]["tools"].clone();
    assert_eq!(tools.as_array().unwrap().len(), 13);
    assert_eq!(
        tools[0]["_meta"]["openai/ui"]["entrypoints"],
        json!([{"type":"thread"}])
    );
    let uri = &tools[0]["_meta"]["ui"]["resourceUri"];
    assert_eq!(uri, "ui://oms/conversation-panel/v8.html");
    assert_eq!(tools[0]["_meta"]["openai/outputTemplate"], *uri);
    assert_eq!(
        c.call("resources/templates/list", json!({}))["result"]["resourceTemplates"],
        json!([])
    );
    let resources = c.call("resources/list", json!({}))["result"]["resources"].clone();
    assert_eq!(resources.as_array().unwrap().len(), 1);
    assert_eq!(resources[0]["uri"], *uri);
    let read = |c: &mut Client, uri: &Value| {
        c.call("resources/read", json!({"uri":uri}))["result"]["contents"][0].clone()
    };
    let current = read(&mut c, uri);
    assert_eq!(current["uri"], *uri);
    assert!(current["text"].as_str() == Some(include_str!("../src/conversation-panel.html")));
    assert_eq!(current["mimeType"], "text/html;profile=mcp-app");
    for historical_uri in [
        json!("ui://oms/conversation-panel/v1.html"),
        json!("ui://oms/conversation-panel/v2.html"),
        json!("ui://oms/conversation-panel/v3.html"),
        json!("ui://oms/conversation-panel/v4.html"),
        json!("ui://oms/conversation-panel/v5.html"),
        json!("ui://oms/conversation-panel/v6.html"),
        json!("ui://oms/conversation-panel/v7.html"),
    ] {
        let historical = read(&mut c, &historical_uri);
        assert_eq!(historical["uri"], historical_uri);
        assert_eq!(historical["text"], current["text"]);
        assert_eq!(historical["mimeType"], current["mimeType"]);
    }
    assert_eq!(
        c.call(
            "resources/read",
            json!({"uri":"ui://oms/conversation-panel/unrecognized.html"})
        )["error"]["code"],
        -32602
    );
    assert!(c.tool("oms_open_panel", json!({}))["result"].is_object());
    assert!(c.tool("oms_peer_start", json!({}))["error"].is_object());
    assert!(c.tool("oms_panel_snapshot", json!({"repo":source}))["error"].is_object());
    assert!(
        c.call("tasks/cancel", json!({"_meta":modern(),"taskId":"absent"}))["error"].is_object()
    );
    bad(&c.tool("oms_panel_snapshot", json!({})), "OMS_PANEL_REPO");
    assert!(!source.join("last-call.json").exists());
    std::fs::create_dir(repo.join(".oms")).unwrap();
    let snapshot = json!({"schema":1,"kind":"oms-panel-snapshot","dashboard":{"scope":"repository","currentRoom":null,"project":"demo","goal":"Finish inspection","active":null,"activeAttempts":[],"attention":[{"summary":"Review pending","count":2}]}});
    fixture_settings(&source, json!({"exit":1,"stdout":snapshot.to_string()}));
    let result = c.tool("oms_panel_snapshot", json!({}));
    let data = &result["result"]["structuredContent"]["dashboard"];
    assert_eq!(payload(&result)["dashboard"], *data);
    assert_eq!(data["degraded"], true);
    assert_eq!(
        record(&source)["argv"],
        json!(["dashboard", "--repo", repo, "--json", "--panel-snapshot"])
    );
    // Keep one runtime command blocked while the host opens and reads its tab.
    // A queued project call must stay behind the snapshot and emit its own ID.
    fixture_settings(
        &source,
        json!({"snapshotGate":true,"stdout":snapshot.to_string()}),
    );
    let send = |client: &mut Client, id: i64, name: &str| {
        client.raw(format!("{}\n", json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}})).as_bytes());
    };
    send(&mut c, 9001, "oms_panel_snapshot");
    until(|| source.join("snapshot-ready").exists());
    for id in 9002..=9009 {
        send(&mut c, id, "oms_list_approvals");
    }
    send(&mut c, 9010, "oms_list_approvals");
    let refused = c.read();
    assert_eq!(refused["id"], 9010);
    assert_eq!(refused["error"]["code"], -32000);
    assert_eq!(
        c.tool("oms_open_panel", json!({}))["result"]["structuredContent"]["panel"],
        "OMS"
    );
    assert_eq!(read(&mut c, uri)["text"], current["text"]);
    assert_eq!(record(&source)["argv"][0], "dashboard");
    std::fs::write(source.join("snapshot-release"), "").unwrap();
    let ready = c.read();
    assert_eq!(ready["id"], 9001);
    assert_eq!(payload(&ready)["dashboard"]["project"], "demo");
    for id in 9002..=9009 {
        let approvals = c.read();
        assert_eq!(approvals["id"], id);
        assert_eq!(payload(&approvals)["approvals"], json!([]));
    }
    fixture_settings(&source, json!({"exit":1,"stdout":snapshot.to_string()}));
    for full in [false, true] {
        let mut regular = Client::new(&source, &repo, full, false, false);
        assert!(
            regular.call("tools/list", json!({}))["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "oms_panel_snapshot"
                    && t["annotations"]["readOnlyHint"] == true
                    && t.get("_meta").is_none())
        );
        assert_eq!(
            regular.tool("oms_panel_snapshot", json!({"repo":repo}))["result"],
            result["result"]
        );
        assert_eq!(
            regular.tool("oms_panel_snapshot", json!({}))["result"],
            result["result"]
        );
        assert!(
            regular.tool("oms_panel_snapshot", json!({"project":"other"}))["error"].is_object()
        );
        assert!(regular.tool("oms_start_main", json!({}))["error"].is_object());
    }
    let thread = "01900000-0000-7000-8000-000000000001";
    let detail = json!({"room":"fixture","participant":"owner","provider":"codex","self":true,"source":"room","messages":[{"text":"dedicated widget reader body"}]});
    fixture_settings(&source, json!({"stdout":detail.to_string()}));
    assert!(
        tools
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "oms_panel_chat"
                && v["_meta"]["ui"]["visibility"] == json!(["app"]))
    );
    bad(
        &c.tool(
            "oms_panel_chat",
            json!({"room":"fixture","participant":"owner"}),
        ),
        "bound conversation",
    );
    let result=c.call("tools/call",json!({"name":"oms_panel_chat","arguments":{"room":"fixture","participant":"owner","before":42},"_meta":{"threadId":thread}}));
    assert_eq!(
        result["result"]["_meta"]["chat"]["messages"],
        detail["messages"]
    );
    assert_eq!(
        result["result"]["_meta"]["chat"]["nativeUri"],
        format!("codex://threads/{thread}")
    );
    for k in ["content", "structuredContent"] {
        let public = result["result"][k].to_string();
        assert!(!public.contains("dedicated widget reader body") && !public.contains(thread));
    }
    let argv = record(&source)["argv"].clone();
    assert!(!argv.to_string().contains(thread));
    assert_eq!(argv[3], "--desktop-chat");
    assert_eq!(argv[10].as_str().unwrap().len(), 32);
    let evidence = |client: &mut Client, args: Value| {
        client.call(
            "tools/call",
            json!({"name":"oms_panel_chat","arguments":args,"_meta":{"threadId":thread}}),
        )
    };
    good(&evidence(
        &mut c,
        json!({"room":"fixture","participant":"owner","section":"files","attempt":"bound-attempt","page":2}),
    ));
    let recorded = record(&source);
    assert_eq!(
        &recorded["argv"].as_array().unwrap()[11..],
        [
            "--evidence-section",
            "files",
            "--evidence-attempt",
            "bound-attempt",
            "--page",
            "2"
        ]
        .map(Value::from)
    );
    bad(
        &evidence(
            &mut c,
            json!({"room":"fixture","participant":"owner","section":"raw","attempt":"bound-attempt"}),
        ),
        "Unsupported",
    );
    bad(
        &evidence(
            &mut c,
            json!({"room":"fixture","participant":"owner","section":"files","attempt":"../other"}),
        ),
        "Invalid",
    );
    bad(
        &evidence(
            &mut c,
            json!({"room":"fixture","participant":"owner","section":"files","attempt":"bound-attempt","before":5}),
        ),
        "cursor",
    );

    assert!(
        c.tool(
            "oms_panel_chat",
            json!({"room":"fixture","participant":"owner","threadId":thread})
        )["error"]
            .is_object()
    );
    let view = json!({"view":"plan","page":2,"columns":[],"rows":[],"status":"ready"});
    fixture_settings(&source, json!({"stdout":view.to_string()}));
    assert_eq!(
        payload(&c.tool("oms_panel_tui_view", json!({"view":"plan","page":2}))),
        view
    );
    for name in ["connections", "modelcatalog", "attribution", "routes"] {
        let page = json!({"view":name,"page":1,"columns":[],"rows":[],"status":"ready"});
        fixture_settings(&source, json!({"stdout":page.to_string()}));
        assert_eq!(
            payload(&c.tool("oms_panel_tui_view", json!({"view":name}))),
            page
        );
        assert_eq!(
            record(&source)["argv"],
            json!([
                "panel",
                "--repo",
                repo,
                "--desktop-view",
                name,
                "--page",
                "1",
                "--json"
            ])
        );
    }
    bad(
        &c.tool("oms_panel_tui_view", json!({"view":"--launch"})),
        "Unsupported",
    );
    bad(
        &c.tool("oms_panel_tui_view", json!({"view":"plan","page":-1})),
        "Invalid",
    );
    for (name, body, argv) in [
        (
            "oms_panel_usage",
            json!({"providers":[],"attempts":[]}),
            json!(["panel", "--repo", repo, "--desktop-usage", "--json"]),
        ),
        (
            "oms_panel_git_status",
            json!({"git":{},"github":{}}),
            json!(["github", "panel-status", "--repo", repo, "--json"]),
        ),
    ] {
        fixture_settings(&source, json!({"stdout":body.to_string()}));
        assert_eq!(payload(&c.tool(name, json!({}))), body);
        assert_eq!(record(&source)["argv"], argv);
    }
    let lookup = json!({"git":{},"github":{"lookup":{"status":"closed","item":null}}});
    fixture_settings(&source, json!({"stdout":lookup.to_string()}));
    for (kind, flag) in [("issue", "--issue"), ("pull", "--pull")] {
        assert_eq!(
            payload(&c.tool("oms_panel_git_status", json!({"kind":kind,"number":447}))),
            lookup
        );
        assert_eq!(
            record(&source)["argv"],
            json!([
                "github",
                "panel-status",
                "--repo",
                repo,
                "--json",
                flag,
                "447"
            ])
        );
    }
    fixture_settings(
        &source,
        json!({"stdout":json!({"git":{},"github":{"inventory":{"kind":"issues","items":[],"nextCursor":null}}}).to_string()}),
    );
    let page = c.tool(
        "oms_panel_git_status",
        json!({"inventory":"issues","cursor":"abc.foo="}),
    );
    assert_eq!(payload(&page)["github"]["inventory"]["kind"], "issues");
    assert_eq!(
        record(&source)["argv"],
        json!([
            "github",
            "panel-status",
            "--repo",
            repo,
            "--json",
            "--inventory",
            "issues",
            "--cursor",
            "abc.foo="
        ])
    );
    good(&c.tool(
        "oms_panel_git_status",
        json!({"boardId":"PVT_fixture","cursor":"abc.foo="}),
    ));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "github",
            "panel-status",
            "--repo",
            repo,
            "--json",
            "--project-id",
            "PVT_fixture",
            "--cursor",
            "abc.foo="
        ])
    );
    for args in [
        json!({"kind":"issue"}),
        json!({"number":2}),
        json!({"kind":"pr","number":2}),
        json!({"kind":"issue","number":"--repo"}),
        json!({"kind":"issue","number":10000000000i64}),
        json!({"inventory":"other"}),
        json!({"boardId":"https://example.invalid/x"}),
        json!({"boardId":"PVT_1","inventory":"issues"}),
        json!({"boardId":"PVT_1","kind":"issue","number":1}),
        json!({"cursor":"abc"}),
        json!({"inventory":"issues","cursor":"x y"}),
        json!({"inventory":"issues","kind":"issue","number":1}),
    ] {
        assert_eq!(
            c.tool("oms_panel_git_status", args)["result"]["isError"],
            true
        );
    }
    let expected_main = json!({"source":"room-session","provider":"codex","participant":"owner","room":"r","state":"joined","incomplete":false});
    fixture_settings(
        &source,
        json!({"stdout":snapshot.to_string(),"binding":{"conversationBinding":"matched","conversationMain":expected_main}}),
    );
    for mut client in [
        Client::new(&source, &repo, false, false, false),
        Client::with_depth(&source, &repo, "chatgpt-panel", false, false, "0".as_ref()),
    ] {
        let snapshot = |client: &mut Client, metadata: Value| {
            client.call(
                "tools/call",
                json!({"name":"oms_panel_snapshot","arguments":{},"_meta":metadata}),
            )
        };
        let observed = snapshot(&mut client, json!({"threadId":thread}));
        assert_eq!(
            payload(&observed)["dashboard"]["conversationMain"],
            expected_main
        );
        assert!(!observed.to_string().contains(thread));
        use sha2::{Digest, Sha256};
        let digest = format!("{:x}", Sha256::digest(thread.as_bytes()));
        assert_eq!(
            record(&source)["argv"],
            json!([
                "dashboard",
                "--repo",
                repo,
                "--json",
                "--panel-snapshot",
                "--conversation-digest",
                &digest[..32]
            ])
        );
        for invalid in [
            json!({"sessionId":thread}),
            json!({"threadId":"bad"}),
            json!({}),
        ] {
            let unbound = snapshot(&mut client, invalid);
            assert!(payload(&unbound)["dashboard"]["conversationMain"].is_null());
            assert_eq!(
                record(&source)["argv"],
                json!(["dashboard", "--repo", repo, "--json", "--panel-snapshot"])
            );
        }
    }
    fixture_settings(&source, json!({"stdout":snapshot.to_string()}));
    let partial = c.call(
        "tools/call",
        json!({"name":"oms_panel_snapshot","_meta":{"threadId":thread}}),
    );
    assert_eq!(payload(&partial)["dashboard"]["project"], "demo");
    assert_eq!(
        payload(&partial)["dashboard"]["conversationBinding"],
        "unconfirmed"
    );
    let mut worker = Client::with_depth(&source, &repo, "core", false, false, "1".as_ref());
    bad(
        &worker.tool("oms_panel_snapshot", json!({"repo":source})),
        "own repository",
    );
    let start = json!({"main":"codex","goal":"--help; inspect scope","requestId":"4d6acd01-bd18-4567-bf69-ea05fb048b27"});
    let room = "oms-app-4d6acd01-bd18-4567-bf69-ea05fb048b27";
    fixture_settings(
        &source,
        json!({"stdout":json!({"status":"started","provider":"codex","room":room,"attempt":"native-main"}).to_string()}),
    );
    let receipt = payload(&c.tool("oms_start_main", start.clone()));
    assert_eq!(receipt["attempt"], "native-main");
    assert_eq!(receipt["requestId"], start["requestId"]);
    assert_eq!(
        record(&source)["argv"],
        json!([
            "panel",
            "--repo",
            repo,
            "--host",
            "desktop",
            "--launch",
            "codex",
            "--room",
            room,
            "--task=--help; inspect scope",
            "--json"
        ])
    );
    let argv: Vec<std::ffi::OsString> = record(&source)["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().into())
        .collect();
    let interrupted = AtomicBool::new(false);
    let recognized = |args: &[std::ffi::OsString]| {
        CommandRequest {
            args,
            cwd: &repo,
            timeout: Duration::from_secs(1),
            interrupted: &interrupted,
        }
        .is_desktop_main_start()
    };
    assert!(recognized(&argv));
    for (i, replacement) in [
        (0, "other"),
        (2, "/"),
        (4, "inline"),
        (6, "unknown"),
        (8, "room-other"),
        (9, "--help"),
    ] {
        let mut changed = argv.clone();
        changed[i] = replacement.into();
        assert!(!recognized(&changed));
    }
    let before = record(&source);
    let mut worker = Client::with_depth(&source, &repo, "chatgpt-panel", false, true, "1".as_ref());
    bad(&worker.tool("oms_start_main", start.clone()), "parent app");
    bad(&worker.tool("oms_list_projects", json!({})), "delegated");
    bad(
        &c.tool(
            "oms_start_main",
            json!({"main":"codex","goal":"x\ry","requestId":start["requestId"]}),
        ),
        "one-line goal",
    );
    assert_eq!(record(&source), before);
    let map = source.join("test-config/oh-my-setting/desktop-projects.json");
    std::fs::create_dir_all(map.parent().unwrap()).unwrap();
    std::fs::write(&map, json!({"demo":repo}).to_string()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        payload(&c.tool("oms_list_projects", json!({})))["projects"],
        json!(["demo"])
    );
    bad(&c.tool("oms_panel_snapshot", json!({})), "project");
    bad(
        &c.tool("oms_panel_snapshot", json!({"project":"../demo"})),
        "project",
    );
    let mut routed = start;
    routed["project"] = json!("demo");
    good(&c.tool("oms_start_main", routed.clone()));
    let recovered = json!({"requestId":routed["requestId"],"room":room,"status":"recorded","attempt":"native-main","state":"done","provider":"codex","terminal":true});
    fixture_settings(&source, json!({"stdout":recovered.to_string()}));
    assert_eq!(
        payload(&c.tool(
            "oms_start_status",
            json!({"project":"demo","requestId":routed["requestId"]})
        )),
        recovered
    );
    assert_eq!(
        record(&source)["argv"],
        json!([
            "room",
            "desktop-start-status",
            "--repo",
            repo,
            "--id",
            room,
            "--json"
        ])
    );
    std::fs::write(&map, "{}").unwrap();
    bad(
        &c.tool("oms_list_projects", json!({})),
        "Invalid OMS project map",
    );
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
        let mut c = Client::with_depth(&source, &repo, "core", false, false, &depth);
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
    assert_eq!(golden.as_array().unwrap().len(), 33);
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
        14
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
    let raw_repo = t.0.join(if cfg!(target_os = "macos") {
        std::ffi::OsString::from("raw-space")
    } else {
        std::ffi::OsString::from_vec(b"raw-\xff".to_vec())
    });
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
    if cfg!(target_os = "macos") {
        assert_eq!(completed["result"]["status"], "completed", "{completed}");
    } else {
        assert_eq!(completed["error"]["code"], -32602);
        assert_eq!(
            completed["error"]["message"],
            "invalid or oversized request field"
        );
    }
}
fn raw_peer_paths() {
    use oms_mcp_server::value::{self, ToValue};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let t = Temp::new();
    let original = t.source();
    let source = t.0.join(if cfg!(target_os = "macos") {
        std::ffi::OsString::from("source-space")
    } else {
        std::ffi::OsString::from_vec(b"source-\xfe".to_vec())
    });
    std::fs::rename(original, &source).unwrap();
    let repo = t.0.join(if cfg!(target_os = "macos") {
        std::ffi::OsString::from("repo-space")
    } else {
        std::ffi::OsString::from_vec(b"repo-\xff".to_vec())
    });
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
        if cfg!(target_os = "macos") {
            good(&result);
        } else {
            assert_eq!(result["error"]["code"], -32602);
            assert_eq!(
                result["error"]["message"],
                "invalid or oversized request field"
            );
        }
    }
    std::fs::write(source.join("fixture-owner"), "complete before readiness").unwrap();
    let mut modern_client = Client::new(&source, &repo, true, true, false);
    let result = modern_client.call(
        "tools/call",
        json!({"name":"oms_peer_start","arguments":{"kind":"consult","prompt":"completed before snapshot"},"_meta":modern()}),
    );
    if cfg!(target_os = "macos") {
        assert_eq!(result["result"]["resultType"], "task", "{result}");
        assert_eq!(result["result"]["status"], "completed", "{result}");
    } else {
        assert_eq!(result["error"]["code"], -32602, "{result}");
        assert_eq!(
            result["error"]["message"],
            "invalid or oversized request field"
        );
    }
    good(&c.tool("oms_peer_result", json!({"thread":"existing"})));
    assert_eq!(
        record(&source)["argv_bytes"][2],
        json!(repo.as_os_str().as_bytes())
    );
    let non_utf8_repo = if cfg!(target_os = "macos") {
        repo.join(std::ffi::OsString::from_vec(b"raw-\xff".to_vec()))
    } else {
        repo.clone()
    };
    let raw_repo = non_utf8_repo.to_value().to_string();
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
fn output_capture_reads() {
    use oms_output_capture::{COMPACT_RESPONSE_BYTES, READ_CAP};
    use std::os::unix::fs::symlink;

    // Exercise the actual digest producer, not a separately reconstructed reference.
    fn retained(repo: &Path, name: &str, data: &[u8]) -> (oms_output::OutputDigest, Value) {
        let dir = repo.join(".oms/artifacts/output-digest");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), data).unwrap();
        let root = oms_safe_fs::Root::open(repo).unwrap();
        let mut digest = oms_output::build(data, &oms_output::Options::default()).unwrap();
        digest.retained_capture(&root, name).unwrap();
        let metadata = digest.json();
        assert_eq!(metadata["raw"]["read_tool"], "oms_output_read");
        let args = metadata["raw"]["read_arguments"].clone();
        (digest, args)
    }
    fn rejected(response: &Value, code: &str) {
        bad(response, code);
        let error = payload(response);
        assert_eq!(error["error"]["code"], code);
        assert!(error.get("content").is_none());
        assert!(response["result"].get("structuredContent").is_none());
    }
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let name = "20261119T000000Z-1.log";
    let data = "가나다 hello\n".repeat(1600);
    let (digest, args) = retained(&repo, name, data.as_bytes());
    let mut c = Client::new(&source, &repo, false, false, false);
    for full in [false, true] {
        let mut client = Client::new(&source, &repo, full, false, false);
        let listed = client.call("tools/list", json!({}));
        assert!(
            listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "oms_output_read")
        );
        let response = client.tool("oms_output_read", args.clone());
        good(&response); // Returned arguments are accepted verbatim in either profile.
        let page = payload(&response);
        assert_eq!(page["capture_sha256"], digest.raw.sha256);
        assert_eq!(page["capture_bytes"], data.len());
        assert_eq!(page["content"], &data[..16384]);
        assert!(response["result"].get("structuredContent").is_none());
    }
    let mut next = args.clone();
    let mut whole = String::new();
    loop {
        let response = c.tool("oms_output_read", next);
        good(&response);
        let page = payload(&response);
        assert_eq!(page["capture_sha256"], digest.raw.sha256);
        whole.push_str(page["content"].as_str().unwrap());
        if page["has_more"] == false {
            break;
        }
        next = page["next_read_arguments"].clone();
    }
    assert_eq!(whole, data);

    let mut request = args.clone();
    request["limit"] = json!(4);
    let page = payload(&c.tool("oms_output_read", request.clone()));
    assert_eq!(page["content"], "가");
    assert_eq!(page["end"], 3);
    request["offset"] = json!(1);
    rejected(&c.tool("oms_output_read", request), "invalid_range");

    let mut request = args.clone();
    request["query"] = json!("hello");
    let response = c.tool("oms_output_read", request);
    good(&response);
    let search = payload(&response);
    assert_eq!(search["matches"].as_array().unwrap().len(), 8);
    let response = c.tool("oms_output_read", search["next_search_arguments"].clone());
    good(&response);
    assert!(payload(&response)["offset"].as_u64().unwrap() > 0);
    good(&c.tool(
        "oms_output_read",
        search["matches"][0]["read_arguments"].clone(),
    ));

    let mut request = args.clone();
    request["compact"] = json!(true);
    request["queries"] = json!(["hello", "absent"]);
    let response = c.tool("oms_output_read", request.clone());
    good(&response);
    let batch = payload(&response);
    assert_eq!(batch["representation"], "compact-v1");
    assert_eq!(batch["searches"][0]["matches"].as_array().unwrap().len(), 4);
    assert_eq!(batch["searches"][1]["matches"], json!([]));
    let mut next = batch["read_base"].clone();
    next.as_object_mut().unwrap().extend(
        batch["searches"][0]["next_search"]
            .as_object()
            .unwrap()
            .clone(),
    );
    good(&c.tool("oms_output_read", next));
    request["limit"] = json!(4);
    rejected(&c.tool("oms_output_read", request), "invalid_query");
    for (field, value, code) in [
        ("capture_ref", json!("../../outside"), "invalid_reference"),
        ("offset", json!(true), "invalid_range"),
        ("limit", json!(3), "invalid_range"),
        ("compact", json!("yes"), "invalid_representation"),
        ("query", json!(""), "invalid_query"),
        ("queries", json!(["hello"]), "invalid_query"),
    ] {
        let mut request = args.clone();
        request[field] = value;
        rejected(&c.tool("oms_output_read", request), code);
    }

    let mut unsafe_search = args.clone();
    let private_query = ["/ho", "me/private/work"].concat();
    unsafe_search["compact"] = json!(true);
    unsafe_search["queries"] = json!([private_query.clone()]);
    let response = c.tool("oms_output_read", unsafe_search);
    rejected(&response, "capture_withheld");
    assert!(!response.to_string().contains(&private_query));

    let other_repo = t.0.join("other-repo");
    std::fs::create_dir(&other_repo).unwrap();
    let (_, other_args) = retained(&other_repo, name, b"other capture\n");
    let mut foreign = args.clone();
    foreign["repo"] = json!(other_repo);
    rejected(
        &c.tool("oms_output_read", foreign),
        "capture_scope_mismatch",
    );
    good(&c.tool("oms_output_read", other_args.clone())); // Main's explicit repository choice.
    let mut child = Client::new(&source, &repo, false, false, true);
    good(&child.tool("oms_output_read", args.clone()));
    rejected(
        &child.tool("oms_output_read", other_args),
        "capture_repository_denied",
    );

    let control_repo = t.0.join("control\nrepo");
    std::fs::create_dir(&control_repo).unwrap();
    let control_root = oms_safe_fs::Root::open(&control_repo).unwrap();
    let mut control_digest = digest.clone();
    assert!(
        control_digest
            .retained_capture(&control_root, name)
            .is_err()
    );
    let control = oms_output_capture::capture_metadata(
        control_root.physical_root(),
        name,
        &digest.raw.sha256,
    )
    .unwrap();
    rejected(
        &c.tool("oms_output_read", control["read_arguments"].clone()),
        "invalid_repository",
    );
    let mut missing_repo = args.clone();
    missing_repo["repo"] = json!(t.0.join("missing-repo"));
    rejected(
        &c.tool("oms_output_read", missing_repo),
        "invalid_repository",
    );

    let path = repo.join(".oms/artifacts/output-digest").join(name);
    std::fs::write(&path, b"changed").unwrap();
    rejected(
        &c.tool("oms_output_read", args.clone()),
        "capture_hash_changed",
    );
    std::fs::remove_file(&path).unwrap();
    rejected(&c.tool("oms_output_read", args.clone()), "capture_missing");
    std::fs::write(&path, data.as_bytes()).unwrap();
    let linked = t.0.join("capture-link");
    std::fs::hard_link(&path, &linked).unwrap();
    rejected(&c.tool("oms_output_read", args.clone()), "unsafe_capture");
    std::fs::remove_file(&linked).unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink("missing", &path).unwrap();
    rejected(&c.tool("oms_output_read", args.clone()), "unsafe_capture");
    std::fs::remove_file(&path).unwrap();
    std::fs::File::create(&path)
        .unwrap()
        .set_len(READ_CAP as u64 + 1)
        .unwrap();
    rejected(&c.tool("oms_output_read", args), "capture_too_large");

    // An apparently safe page cannot bypass a block or machine path elsewhere.
    for sensitive in [
        concat!(
            "safe page\n-----BEG",
            "IN PRIV",
            "ATE KEY-----\nhidden\n-----END PRIV",
            "ATE KEY-----\n"
        )
        .to_owned(),
        format!("safe page\n{}{}\n", "/ho", "me/private/work"),
    ] {
        let (mut withheld, mut args) = retained(&repo, name, sensitive.as_bytes());
        withheld.withhold("outbound content scan".into());
        let withheld = withheld.json();
        for key in [
            "capture_ref",
            "capture_sha256",
            "read_tool",
            "read_arguments",
        ] {
            assert!(withheld["raw"].get(key).is_none());
        }
        args["limit"] = json!(4);
        let response = c.tool("oms_output_read", args.clone());
        rejected(&response, "capture_withheld");
        assert!(!response.to_string().contains("hidden"));
        args.as_object_mut().unwrap().remove("limit");
        args["query"] = json!("safe");
        rejected(&c.tool("oms_output_read", args), "capture_withheld");
    }

    // The serialized payload budget is inclusive and never truncates JSON.
    let mut bytes = vec![b'a'; 16384];
    let (_, args) = retained(&repo, name, &bytes);
    let response = c.tool("oms_output_read", args);
    good(&response);
    let baseline = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .len();
    let extra = COMPACT_RESPONSE_BYTES - baseline;
    bytes[..extra / 5].fill(1); // Each control byte adds five JSON escape bytes.
    bytes[extra / 5..extra / 5 + extra % 5].fill(b'"');
    let (_, args) = retained(&repo, name, &bytes);
    let response = c.tool("oms_output_read", args);
    good(&response);
    assert_eq!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .len(),
        COMPACT_RESPONSE_BYTES
    );
    bytes[extra / 5 + extra % 5] = b'"';
    let (_, args) = retained(&repo, name, &bytes);
    rejected(&c.tool("oms_output_read", args), "response_too_large");
    // Reads and failures never execute a command.
    assert!(!source.join("last-call.json").exists());
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
    let raw_repo = t.0.join(if cfg!(target_os = "macos") {
        OsString::from("repo-space")
    } else {
        OsString::from_vec(b"raw-\xff".to_vec())
    });
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
    let revision = "1".repeat(40);
    let stages = source.join("target/oms-native");
    let staged = exec_file(&stages.join(&revision), 0o755);
    let op = "ask-20200101T000000Z-eeeeeeee";
    let dir = run_dir(&repo, op, json!({}), None);
    std::fs::write(dir.join("launch.json"),json!({"kind":"ask","targets":["claude"],"thread":op,"new_thread":false,"debate_rounds":0,"context_options":{}}).to_string()).unwrap();
    let output = Command::new(&binary)
        .args([
            source.as_os_str(),
            repo.as_os_str(),
            op.as_ref(),
            staged.as_os_str(),
        ])
        .env("PATH", "")
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let args = record(&stages)["argv"].as_array().unwrap().clone();
    assert_eq!(
        args[..5],
        json!(["--oms-dispatch", source, revision, "--", "peer-ask"])
            .as_array()
            .unwrap()[..]
    );
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
    if cfg["snapshotGate"] == true {
        if args.first().is_some_and(|v| v == "dashboard") {
            std::fs::write(source.join("snapshot-ready"), "").unwrap();
            until(|| source.join("snapshot-release").exists());
        } else if args.get(1).is_some_and(|v| v == "desktop-requests") {
            println!("{}", json!({"approvals":[],"incomplete":false}));
            return;
        }
    }
    if args.first().is_some_and(|v| v == "dashboard")
        && args.iter().any(|v| v == "--conversation-digest")
    {
        let mut report: Value = serde_json::from_str(cfg["stdout"].as_str().unwrap()).unwrap();
        report["dashboard"]["conversationBinding"] = cfg["binding"]["conversationBinding"]
            .as_str()
            .unwrap_or("unconfirmed")
            .into();
        report["dashboard"]["conversationMain"] = cfg["binding"]["conversationMain"].clone();
        println!("{report}");
        return;
    }
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
    if args.first().is_some_and(|v| v == "room")
        && let Some(out) = args
            .get(1)
            .and_then(|v| v.to_str())
            .and_then(|a| cfg["room"][a].as_str())
    {
        print!("{out}");
        return;
    }
    if args.iter().any(|v| v == "updates") && cfg["updates"].is_string() {
        print!("{}", cfg["updates"].as_str().unwrap());
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
fn new_command_tools() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::new(&source, &repo, true, false, false);
    good(&c.tool("oms_memory_recall", json!({"query":"cache layout"})));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "agent-memory",
            "recall",
            "--json",
            "--limit",
            "10",
            "cache layout"
        ])
    );
    good(&c.tool("oms_memory_recall", json!({"query":"x","limit":3})));
    assert_eq!(record(&source)["argv"][4], "3");
    for args in [
        json!({}),
        json!({"query":""}),
        json!({"query":"--all-repos"}),
        json!({"query":"q","limit":0}),
        json!({"query":"q","limit":51}),
        json!({"query":7}),
    ] {
        bad(&c.tool("oms_memory_recall", args), "error:");
    }
    good(&c.tool("oms_state_compact", json!({})));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "state",
            "compact",
            "--json",
            "--dry-run",
            "--keep-recent",
            "25",
            "--older-than-days",
            "7"
        ])
    );
    good(&c.tool(
        "oms_state_compact",
        json!({"apply":true,"keep_recent":7,"older_than_days":3}),
    ));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "state",
            "compact",
            "--json",
            "--keep-recent",
            "7",
            "--older-than-days",
            "3"
        ])
    );
    bad(
        &c.tool("oms_state_compact", json!({"apply":"yes"})),
        "boolean",
    );
    bad(
        &c.tool("oms_state_compact", json!({"keep_recent":0})),
        "keep_recent",
    );
    fixture_settings(&source, json!({"stdout":"{\"code\":\"x\"}"}));
    let r = c.tool("oms_failure_diagnose", json!({"text":"boom","exit":3}));
    good(&r);
    assert_eq!(
        record(&source)["argv"],
        json!(["runtime", "failure", "classify", "--exit=3", "--", "boom"])
    );
    assert_eq!(payload(&r)["mode"], "classify");
    assert_eq!(payload(&r)["output"]["code"], "x");
    fixture_settings(&source, json!({"stdout":"seen before","exit":3}));
    let r = c.tool("oms_failure_diagnose", json!({"cmd":"cargo test"}));
    good(&r);
    assert_eq!(
        record(&source)["argv"],
        json!(["fail-ledger", "check", "--cmd", "cargo test"])
    );
    assert_eq!(payload(&r)["exit"], 3);
    assert_eq!(payload(&r)["output"], "seen before");
    fixture_settings(&source, json!({"stdout":"[]"}));
    good(&c.tool("oms_failure_diagnose", json!({})));
    assert_eq!(
        record(&source)["argv"],
        json!(["fail-ledger", "list", "--json", "--limit", "10"])
    );
    bad(
        &c.tool("oms_failure_diagnose", json!({"cmd":"--help"})),
        "cannot begin",
    );
    fixture_settings(&source, json!({"stdout":"nope","exit":1,"stderr":"broken"}));
    bad(&c.tool("oms_failure_diagnose", json!({"text":"t"})), "nope");
}
fn symbol_slice() {
    use std::os::unix::fs::symlink;
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    let write = |name: &str, body: &[u8]| {
        let p = repo.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    write(
        "src/a.rs",
        b"// fn commented() {}\nuse x;\n\npub struct Plain;\n\nimpl Plain {\n    fn run(&self) -> char {\n        let c = '{';\n        if true {\n            return c;\n        }\n        '}'\n    }\n}\n\nfn tail() {}\n",
    );
    write(
        "tool.py",
        b"import os\n\nclass Box:\n    def get(self):\n        return {1: 2}\n\n    def put(self):\n        pass\n\nx = 1\n",
    );
    write(
        "run.sh",
        b"#!/bin/sh\nhelper() {\n  echo \"}\"\n}\nhelper\n",
    );
    let big: String = (1..=400).map(|n| format!("line {n}\n")).collect();
    write("big.txt", big.as_bytes());
    let huge = format!("fn huge() {{\n{}}}\n", "    x;\n".repeat(300));
    write("huge.rs", huge.as_bytes());
    let mut c = Client::new(&source, &repo, true, false, false);
    let range = |r: &Value| {
        let p = payload(r);
        (
            p["start_line"].as_u64().unwrap(),
            p["end_line"].as_u64().unwrap(),
        )
    };
    let r = c.tool(
        "oms_symbol_slice",
        json!({"path":"src/a.rs","symbol":"run"}),
    );
    good(&r);
    assert_eq!(range(&r), (7, 13), "char-literal braces do not unbalance");
    assert_eq!(payload(&r)["total_lines"], 16);
    assert_eq!(payload(&r)["truncated"], false);
    assert_eq!(
        payload(&r)["lines"][0],
        "     7:     fn run(&self) -> char {"
    );
    for (path, symbol, expected) in [
        ("src/a.rs", "Plain", (4, 4)),
        ("src/a.rs", "tail", (16, 16)),
        ("tool.py", "Box", (3, 8)),
        ("tool.py", "get", (4, 5)),
        ("run.sh", "helper", (2, 4)),
    ] {
        let r = c.tool("oms_symbol_slice", json!({"path":path,"symbol":symbol}));
        good(&r);
        assert_eq!(range(&r), expected, "{path} {symbol}");
    }
    let r = c.tool(
        "oms_symbol_slice",
        json!({"path":"huge.rs","symbol":"huge"}),
    );
    good(&r);
    assert_eq!(range(&r), (1, 250));
    assert_eq!(payload(&r)["truncated"], true);
    assert_eq!(payload(&r)["lines"].as_array().unwrap().len(), 250);
    for (args, expected) in [
        (json!({"path":"big.txt"}), (1, 250)),
        (
            json!({"path":"big.txt","start_line":10,"end_line":5000}),
            (10, 259),
        ),
        (json!({"path":"big.txt","start_line":390}), (390, 400)),
        (
            json!({"path":"big.txt","start_line":2,"end_line":3}),
            (2, 3),
        ),
    ] {
        let r = c.tool("oms_symbol_slice", args.clone());
        good(&r);
        assert_eq!(range(&r), expected, "{args}");
    }
    for (args, part) in [
        (json!({"path":"big.txt","start_line":401}), "beyond"),
        (
            json!({"path":"big.txt","start_line":9,"end_line":8}),
            "precedes",
        ),
        (
            json!({"path":"big.txt","symbol":"a","start_line":1}),
            "excludes",
        ),
        (json!({"path":"big.txt","symbol":"a b"}), "identifier"),
        (
            json!({"path":"src/a.rs","symbol":"commented"}),
            "no definition",
        ),
        (
            json!({"path":"src/a.rs","symbol":"absent"}),
            "no definition",
        ),
        (json!({"path":"big.txt","start_line":0}), "start_line"),
        (json!({}), "path must"),
        (json!({"path":"../escape"}), "path must"),
        (json!({"path":"/etc/hosts"}), "path must"),
        (json!({"path":"src//a.rs"}), "path must"),
        (json!({"path":"src/./a.rs"}), "path must"),
        (json!({"path":"src\\a.rs"}), "path must"),
        (json!({"path":"missing.rs"}), "cannot read"),
        (json!({"path":"src"}), "cannot read"),
    ] {
        bad(&c.tool("oms_symbol_slice", args.clone()), part);
    }
    // Links never resolve: neither a symlinked file, a symlinked directory
    // nor a second hard link to a regular file.
    symlink(repo.join("src/a.rs"), repo.join("link.rs")).unwrap();
    symlink(repo.join("src"), repo.join("dir")).unwrap();
    std::fs::hard_link(repo.join("tool.py"), repo.join("hard.py")).unwrap();
    for path in ["link.rs", "dir/a.rs", "hard.py", "tool.py"] {
        bad(
            &c.tool("oms_symbol_slice", json!({"path":path})),
            "cannot read",
        );
    }
    for path in [
        ".env",
        "a/.env.local",
        ".git/config",
        ".oms/state.json",
        "k/id_rsa",
        "x.pem",
    ] {
        bad(
            &c.tool("oms_symbol_slice", json!({"path":path})),
            "private or secret",
        );
    }
    // Secret-shaped content withholds the whole file, wherever the range is.
    let leaked = ["gh", "p_", &"a".repeat(36)].concat();
    write("tok.txt", format!("clean\n{leaked}\n").as_bytes());
    let header = ["-----BE", "GIN PRIV", "ATE KEY-----"].concat();
    write("blk.txt", format!("{header}\nbody\nbody\nend\n").as_bytes());
    for (path, args) in [
        (
            "tok.txt",
            json!({"path":"tok.txt","start_line":1,"end_line":1}),
        ),
        ("blk.txt", json!({"path":"blk.txt","start_line":3})),
    ] {
        let r = c.tool("oms_symbol_slice", args);
        bad(&r, "secret-shaped");
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains(&leaked) && !text.contains("body"), "{path}");
    }
    let assignment = ["pass", "word = \"", "hunter2hunter2", "\""].concat();
    write("cfg.txt", format!("a\n{assignment}\nb\n").as_bytes());
    good(&c.tool("oms_symbol_slice", json!({"path":"cfg.txt","start_line":3})));
    bad(
        &c.tool("oms_symbol_slice", json!({"path":"cfg.txt","start_line":1})),
        "assignment",
    );
    write("bin.dat", b"ab\0cd");
    bad(
        &c.tool("oms_symbol_slice", json!({"path":"bin.dat"})),
        "binary",
    );
    write("empty.txt", b"");
    bad(
        &c.tool("oms_symbol_slice", json!({"path":"empty.txt"})),
        "empty",
    );
    write("large.txt", &vec![b'a'; 2 * 1024 * 1024 + 1]);
    bad(
        &c.tool("oms_symbol_slice", json!({"path":"large.txt"})),
        "cannot read",
    );
    assert!(
        !source.join("last-call.json").exists(),
        "symbol slices must not spawn oms"
    );
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
        if args[4] == "mcp-app" {
            fixture_config.profile = ToolProfile::McpApp;
        } else if args[4] == "chatgpt-panel" {
            fixture_config.profile = ToolProfile::ChatGptPanel;
        }
        if Path::new(&args[2]).join("fixture-owner").is_file() {
            fixture_config.peer_owner = std::env::current_exe().unwrap();
        }
        Server::new(
            Path::new(&args[2]),
            Path::new(&args[3]),
            &flag,
            fixture_config,
        )
        .unwrap()
        .serve_connection(&mut std::io::stdin().lock(), &mut std::io::stdout())
        .unwrap();
        return;
    }
    let cases: [(&str, fn()); 34] = [
        ("lossless_values", lossless_values),
        ("raw_peer_paths", raw_peer_paths),
        ("parser_reference", parser_reference),
        ("owner_validation", owner_validation),
        ("environment_profile", environment_profile),
        ("cross_host_project_reads", cross_host_project_reads),
        ("mcp_app_binding_boundaries", mcp_app_binding_boundaries),
        ("conversation_panel", conversation_panel),
        ("desktop_assignment", desktop_assignment),
        ("environment_authority", environment_authority),
        ("boundary_limits", boundary_limits),
        ("canonical_protocol_cases", canonical_protocol_cases),
        ("peer_argv_goldens", peer_argv_goldens),
        ("cancellation_concurrency", cancellation_concurrency),
        ("command_interruption", command_interruption),
        ("discovery_protocols", discovery_protocols),
        ("transport_bounds", transport_bounds),
        ("fixed_argv", fixed_argv),
        ("new_command_tools", new_command_tools),
        ("symbol_slice", symbol_slice),
        ("output_bounds", output_bounds),
        ("output_capture_reads", output_capture_reads),
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
