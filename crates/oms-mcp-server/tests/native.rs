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
        let p = Command::new(std::env::current_exe().unwrap())
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
        Self::from_child(p)
    }
    fn from_child(mut p: Child) -> Self {
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
fn registry_summary(v: &Value) -> Value {
    let mut data = payload(v);
    let valid = |key: &Value| {
        key.as_str().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    };
    assert!(
        data["projectKeys"]
            .as_object()
            .unwrap()
            .iter()
            .all(
                |(alias, key)| data["projects"].as_array().unwrap().contains(&json!(alias))
                    && valid(key)
            )
    );
    if matches!(data["binding"].as_str(), Some("cwd" | "pinned")) {
        assert!(valid(&data["projectKey"]));
    } else {
        assert!(data["projectKey"].is_null());
    }
    data.as_object_mut().unwrap().remove("projectKeys");
    data.as_object_mut().unwrap().remove("projectKey");
    data
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
        assert_eq!(tools.len(), 9);
        assert!(tools.iter().any(
            |t| t["name"] == "oms_panel_work_items" && t["annotations"]["readOnlyHint"] == true
        ));
        let graph_tool = tools
            .iter()
            .find(|t| t["name"] == "oms_panel_project_graph")
            .unwrap();
        assert_eq!(graph_tool["annotations"]["readOnlyHint"], true);
        assert_eq!(graph_tool["annotations"]["destructiveHint"], false);
        assert_eq!(graph_tool["inputSchema"]["additionalProperties"], false);
        assert!(
            graph_tool["inputSchema"]["properties"]
                .get("repo")
                .is_none()
        );
        assert!(
            graph_tool["inputSchema"]["properties"]
                .get("project")
                .is_some()
        );
        assert!(tools.iter().any(|t| t["name"] == "oms_panel_capabilities"
            && t["annotations"]["readOnlyHint"] == true
            && t["annotations"]["destructiveHint"] == false
            && t["annotations"]["idempotentHint"] == true
            && t["annotations"]["openWorldHint"] == false
            && t["inputSchema"]["additionalProperties"] == false));
        let opener = tools
            .iter()
            .find(|t| t["name"] == "oms_open_panel")
            .unwrap();
        assert_eq!(
            opener["description"],
            "Open the OMS panel (expands beside or over the conversation when the host allows it)."
        );
        assert_eq!(
            opener["inputSchema"]["properties"]["project"]["description"],
            "Pass the exact alias from oms_list_projects when the host has a project map; the initial view is included only when project names an exact alias from oms_list_projects on hosts with a project map."
        );
        assert_eq!(
            graph_tool["inputSchema"]["properties"]["project"]["description"],
            "Exact alias from oms_list_projects when the host has a project map."
        );
        assert_eq!(
            opener["_meta"]["openai/ui"]["entrypoints"][0]["type"],
            "global"
        );
        assert_eq!(
            opener["_meta"]["com.google.antigravity/ui"]["entrypoints"],
            json!([{"type":"aux_pane","title":"OMS"},{"type":"global","title":"OMS"}])
        );
        let uri = &opener["_meta"]["ui"]["resourceUri"];
        assert_eq!(uri, "ui://oms/conversation-panel/v32.html");
        assert_eq!(opener["_meta"]["openai/outputTemplate"], *uri);
        let view = c.call("resources/read", json!({"uri":uri}));
        let html = view["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(html.len() < 240 * 1024);
        assert_eq!(
            html.as_bytes(),
            include_bytes!("../src/conversation-panel.html")
        );
        let legacy_uri = "ui://oms/conversation-panel/v31.html";
        let legacy = c.call("resources/read", json!({"uri":legacy_uri}));
        let legacy = &legacy["result"]["contents"][0];
        assert_eq!(legacy["uri"], legacy_uri);
        assert_eq!(legacy["text"], view["result"]["contents"][0]["text"]);
        assert_eq!(legacy["_meta"], view["result"]["contents"][0]["_meta"]);
        assert!(html.contains("ui/request-display-mode"));
        assert!(html.contains("availableDisplayModes:['inline','fullscreen','pip']"));
        assert_eq!(
            view["result"]["contents"][0]["mimeType"],
            "text/html;profile=mcp-app"
        );
        assert_eq!(
            view["result"]["contents"][0]["_meta"]["ui"]["preferredDisplayMode"],
            "fullscreen"
        );
        let unbound_open = c.tool("oms_open_panel", json!({}));
        good(&unbound_open);
        assert!(unbound_open["result"]["_meta"].get("dashboard").is_none());
        let expected_unbound_open =
            json!({"panel":"OMS","project":null,"readOnly":true,"snapshotOmitted":"project"});
        assert_eq!(
            unbound_open["result"]["structuredContent"],
            expected_unbound_open
        );
        assert_eq!(payload(&unbound_open), expected_unbound_open);
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
        "oms_record_reviewed_issue",
        "oms_reviewed_issue_status",
    ] {
        assert_eq!(c.tool(name, json!({}))["error"]["code"], -32602);
    }
    let dir = source.join("test-config/oh-my-setting");
    std::fs::create_dir_all(&dir).unwrap();
    let map = dir.join("desktop-projects.json");
    std::fs::write(&map, json!({"fixture":repo}).to_string()).unwrap();
    std::fs::set_permissions(map, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        registry_summary(&c.tool("oms_list_projects", json!({}))),
        json!({"binding":"catalog","projects":["fixture"],"readOnly":true})
    );
    let obs = json!({"schema":1,"kind":"multi-source-observation","atomic":false,"revision":"obs-v1:0123456789abcdef01234567","observedAt":"2026-11-24T10:00:00Z","completeness":"complete","sources":{"git":"ok","task":"ok","plan":"ok","attempts":"ok","artifacts":"ok","acceptance":"ok","conversation":"unconfirmed"},"evidence":{"scope":"repository","head":"1234567890ab","room":null,"latestArtifact":null,"omitted":0,"trimmed":0}});
    // The transport duplicates compact payloads in text and structuredContent.
    // A dense snapshot still retains structured evidence within the wire budget.
    let scratch_obs = json!({
        "status":"partial",
        "reason":null,
        "sources":[{"id":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","label":"wt-0123456789ab (detached)","head":"detached","branch":null,"status":"ready"}],
        "attempts":[{"id":"fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210","sourceId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","provider":"codex","tool":"peer-delegate","state":"working","updatedAt":"2026-11-24T10:00:00Z"}],
        "unverifiedCheckouts":1,
        "unexaminedCheckouts":0,
        "omittedAttempts":0
    });
    let large = json!({"scope":"repository","goal":"g".repeat(2000),"goalAvailability":"available",
        "managedScratch":scratch_obs,
        "operations":(0..100).map(|n|json!({"id":n,"description":"d".repeat(410)})).collect::<Vec<_>>()});
    fixture_settings(
        &source,
        json!({"stdout":json!({"schema":1,"kind":"oms-panel-snapshot","dashboard":large}).to_string()}),
    );
    for profile in ["mcp-app", "chatgpt-panel"] {
        let mut bound = Client::with_depth(&source, &repo, profile, false, false, "0".as_ref());
        let result = bound.tool("oms_panel_snapshot", json!({"project":"fixture"}));
        assert_eq!(
            result["result"]["structuredContent"]["dashboard"], large,
            "{profile}"
        );
        assert_eq!(
            result["result"]["structuredContent"]["dashboard"]["managedScratch"]["attempts"][0]["state"],
            "working",
            "{profile}"
        );
        assert!(result.to_string().len() < 60_000, "{profile}");
    }
    let issue = json!({"kind":"github-issue","reference":"https://github.com/org/repo/issues/9",
        "revision":"2026-11-24T09:00:00Z","state":"open","bytes":12,"sha256":"a".repeat(64)});
    let mut preview = json!({"schema":1,"kind":"oms-intent-source","trusted":false,"source":issue,
        "text":"not for relay","note":"not for relay"});
    preview["source"]["title"] = json!("not for relay");
    fixture_settings(&source, json!({"stdout":preview.to_string()}));
    for profile in ["mcp-app", "chatgpt-panel"] {
        let mut bound = Client::with_depth(&source, &repo, profile, false, false, "0".as_ref());
        let result = bound.tool(
            "oms_panel_git_status",
            json!({"project":"fixture","issuePreview":9}),
        );
        assert_eq!(payload(&result), json!({"issuePreview":issue}));
        assert!(!result.to_string().contains("not for relay"));
        assert_eq!(
            record(&source)["argv"],
            json!([
                "intent",
                "draft",
                "--repo",
                repo,
                "--from-project-issue",
                "9",
                "--dry-run"
            ])
        );
    }
    for (key, value) in [
        ("reference", json!("https://github.com/org/repo/issues/10")),
        ("reference", json!("https://github.com/org/repo/pull/9")),
        ("state", json!("closed")),
        ("bytes", json!(32769)),
        ("sha256", json!("A".repeat(64))),
        ("revision", json!("2026-02-31T00:00:00Z")),
    ] {
        let mut bad_source = preview.clone();
        bad_source["source"][key] = value;
        fixture_settings(&source, json!({"stdout":bad_source.to_string()}));
        bad(
            &c.tool(
                "oms_panel_git_status",
                json!({"project":"fixture","issuePreview":9}),
            ),
            "Unverified project issue",
        );
    }
    fixture_settings(&source, json!({"exit":1,"stderr":"not for relay"}));
    bad(
        &c.tool(
            "oms_panel_git_status",
            json!({"project":"fixture","issuePreview":9}),
        ),
        "Cannot verify an open issue",
    );
    let snapshot = json!({"schema":1,"kind":"oms-panel-snapshot","dashboard":{"scope":"repository","project":"fixture","updated":"2026-11-24T10:00:00Z","conversationMain":null,"observation":obs}});
    fixture_settings(&source, json!({"stdout":snapshot.to_string()}));
    let reply=c.call("tools/call",json!({"name":"oms_panel_snapshot","arguments":{"project":"fixture"},"_meta":{"threadId":"00000000-0000-4000-8000-000000000001"}}));
    assert_eq!(payload(&reply)["dashboard"]["project"], "fixture");
    assert_eq!(payload(&reply)["dashboard"]["observation"], obs);
    assert_eq!(
        reply["result"]["structuredContent"]["dashboard"]["observation"],
        obs
    );
    assert!(
        !record(&source)["argv"]
            .to_string()
            .contains("conversation-digest")
    );
    assert!(!record(&source)["argv"].to_string().contains("00000000-"));
    let opened = c.tool("oms_open_panel", json!({"project":"fixture"}));
    good(&opened);
    assert_eq!(
        opened["result"]["_meta"]["dashboard"],
        reply["result"]["structuredContent"]["dashboard"]
    );
    assert!(
        opened["result"]["structuredContent"]
            .get("dashboard")
            .is_none()
    );
    let expected_open =
        json!({"panel":"OMS","project":"fixture","readOnly":true,"snapshot":"attached"});
    assert_eq!(opened["result"]["structuredContent"], expected_open);
    let open_text = opened["result"]["content"][0]["text"].as_str().unwrap();
    assert!(!open_text.contains("obs-v1:"));
    assert!(!open_text.contains("conversationMain"));
    assert!(!open_text.contains("dashboard"));
    assert!(payload(&opened).get("dashboard").is_none());
    assert_eq!(payload(&opened), expected_open);
    let unresolvable = c.tool("oms_open_panel", json!({}));
    good(&unresolvable);
    assert!(
        unresolvable["result"]["structuredContent"]
            .get("dashboard")
            .is_none()
    );
    assert!(unresolvable["result"]["_meta"].get("dashboard").is_none());
    let expected_unresolvable =
        json!({"panel":"OMS","project":null,"readOnly":true,"snapshotOmitted":"project"});
    assert_eq!(
        unresolvable["result"]["structuredContent"],
        expected_unresolvable
    );
    assert_eq!(payload(&unresolvable), expected_unresolvable);
    fixture_settings(&source, json!({"exit":2,"stderr":"snapshot failed"}));
    let failed_snapshot_open = c.tool("oms_open_panel", json!({"project":"fixture"}));
    good(&failed_snapshot_open);
    assert!(
        failed_snapshot_open["result"]["structuredContent"]
            .get("dashboard")
            .is_none()
    );
    assert!(
        failed_snapshot_open["result"]["_meta"]
            .get("dashboard")
            .is_none()
    );
    assert!(payload(&failed_snapshot_open).get("dashboard").is_none());
    assert!(payload(&failed_snapshot_open).get("snapshot").is_none());
    assert!(
        payload(&failed_snapshot_open)
            .get("snapshotOmitted")
            .is_none()
    );
    let oversized_snapshot = json!({"schema":1,"kind":"oms-panel-snapshot","dashboard":{"scope":"repository","project":"fixture","goal":"g".repeat(60_000)}});
    fixture_settings(&source, json!({"stdout":oversized_snapshot.to_string()}));
    let oversized_open = c.tool("oms_open_panel", json!({"project":"fixture"}));
    good(&oversized_open);
    assert!(
        oversized_open["result"]["structuredContent"]
            .get("dashboard")
            .is_none()
    );
    assert!(oversized_open["result"]["_meta"].get("dashboard").is_none());
    assert_eq!(
        oversized_open["result"]["structuredContent"],
        json!({"panel":"OMS","project":"fixture","readOnly":true,"snapshotOmitted":"size"})
    );
    assert_eq!(
        payload(&oversized_open),
        json!({"panel":"OMS","project":"fixture","readOnly":true,"snapshotOmitted":"size"})
    );
    // Fail-closed: exit-1 partial response contradicting a complete observation cannot claim complete revision.
    fixture_settings(&source, json!({"exit":1,"stdout":snapshot.to_string()}));
    let degraded = c.tool("oms_panel_snapshot", json!({"project":"fixture"}));
    assert_eq!(payload(&degraded)["dashboard"]["degraded"], true);
    assert_eq!(
        payload(&degraded)["dashboard"]["observation"]["completeness"],
        "partial"
    );
    assert!(payload(&degraded)["dashboard"]["observation"]["revision"].is_null());
    // Genuine partial response from the producer keeps its partial revision intact.
    let mut partial_obs = obs.clone();
    partial_obs["completeness"] = json!("partial");
    partial_obs["sources"]["artifacts"] = json!("unavailable");
    let partial_snapshot = json!({"schema":1,"kind":"oms-panel-snapshot","dashboard":{"scope":"repository","project":"fixture","degraded":true,"conversationMain":null,"observation":partial_obs}});
    fixture_settings(
        &source,
        json!({"exit":1,"stdout":partial_snapshot.to_string()}),
    );
    let partial_reply = c.tool("oms_panel_snapshot", json!({"project":"fixture"}));
    assert_eq!(
        payload(&partial_reply)["dashboard"]["observation"],
        partial_obs
    );
    // Generic read-only host can read the bounded repository project graph by alias.
    let sample_graph = json!({
        "schema": 1,
        "kind": "oms-panel-project-graph",
        "status": "ready",
        "completeness": "complete",
        "provenance": {"source":"git-ls-files","mode":"fallback","indexState":"unindexed","verifiedRelationships":false,"readOnly":true,"wroteIndex":false},
        "scope": {"repository":"repo","focusNode":null,"query":null,"includeTests":false,"depth":2,"limit":40},
        "limits": {"maxNodes":40,"maxEdges":180,"truncated":false},
        "counts": {"shown_nodes":1,"total_nodes":1,"shown_edges":0,"total_edges":0,"confidence":{}},
        "nodes": [{"id":"dir:.","label":"repo","kind":"directory","path":""}],
        "edges": [],
        "selection": null
    });
    fixture_settings(&source, json!({"stdout":sample_graph.to_string()}));
    let graph_reply = c.tool("oms_panel_project_graph", json!({"project":"fixture"}));
    good(&graph_reply);
    assert_eq!(payload(&graph_reply), sample_graph);
    assert_eq!(graph_reply["result"]["structuredContent"], sample_graph);
    assert_eq!(
        record(&source)["argv"],
        json!([
            "graph", "project", "panel", "--repo", repo, "--json", "--limit", "40", "--depth", "2"
        ])
    );
    // The JSON-RPC response itself must fit the host limit, including text fallback.
    let mut big = sample_graph.clone();
    big["nodes"] = json!(
        (0..60)
            .map(|i| json!({"id":format!("file:{i}"),"label":"x".repeat(600)}))
            .collect::<Vec<_>>()
    );
    fixture_settings(&source, json!({"exit":0,"stdout":big.to_string()}));
    let page = c.tool(
        "oms_panel_project_graph",
        json!({"project":"fixture","limit":60}),
    );
    good(&page);
    assert_eq!(page["result"]["structuredContent"], big);
    assert!(
        page["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("60 nodes")
    );
    assert!(page.to_string().len() < 60_000);

    // A long tracked path is escaped in the widget-only declaration, outside structuredContent.
    let path = format!("{}/{}/code.rs", "\"".repeat(224), "\"".repeat(223));
    let node = format!("symbol:{path}::function:read_value");
    let parameters = (0..15)
        .map(|i| format!("parameter_{i}: usize"))
        .collect::<Vec<_>>()
        .join(", ");
    let definition =
        json!({"id":node,"line":1,"endLine":2,"signature":format!("fn read_value({parameters})")});
    let selected = json!({"id":node,"label":"read_value","kind":"function","path":&path[..260]});
    let mut nodes = vec![selected.clone()];
    nodes.extend((0..53).map(|i| {
        let path = format!("src/{i:02}/{}.rs", "x".repeat(128));
        json!({"id":format!("file:{path}"),"label":"f".repeat(110),"kind":"file","path":path,"language":"rust"})
    }));
    let mut graph = json!({"schema":1,"kind":"oms-panel-project-graph","status":"ready","completeness":"complete",
        "provenance":{"source":"cache"},"limits":{"maxNodes":60,"maxEdges":180,"maxBytes":52000},
        "counts":{"shown_nodes":54,"shown_edges":0},"nodes":nodes,"edges":[],
        "selection":{"status":"found","node":selected,"neighbors":[],"breadcrumbs":[]}});
    let mut native = graph.clone();
    native["selection"]["definition"] = definition.clone();
    fixture_settings(&source, json!({"stdout":native.to_string()}));
    let params = json!({"name":"oms_panel_project_graph","arguments":{"project":"fixture","node":node,"limit":60},
        "_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}});
    let wire_call = |c: &mut Client, id: Value| {
        let request = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":params});
        c.raw(format!("{request}\n").as_bytes());
        let line = c.read_line();
        assert!(
            line.ends_with('\n') && line.len() <= 60_000,
            "{} wire bytes",
            line.len()
        );
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["resultType"], "complete");
        reply
    };
    let reply = wire_call(&mut c, json!(2));
    good(&reply);
    assert_eq!(reply["result"]["structuredContent"], graph);
    assert_eq!(reply["result"]["_meta"]["graphDefinition"], definition);
    assert!(
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("54 nodes")
    );

    // Even a summary can exceed the envelope budget with a maximally escaped legal request ID.
    let paths = (0..4)
        .map(|i| format!("{}/{}{i}.rs", "🌳".repeat(40), "🌳".repeat(34)))
        .chain((0..49).map(|i| format!("src/{i}_{}/{}.rs", "z".repeat(220), "x".repeat(78))));
    let mut nodes = paths
        .map(|path| {
            json!({
                "id":format!("file:{path}"),"path":path,"kind":"file","language":"rust",
                "label":path.rsplit('/').next().unwrap()
            })
        })
        .collect::<Vec<_>>();
    nodes.insert(4, selected);
    graph["nodes"] = json!(nodes);
    native = graph.clone();
    native["selection"]["definition"] = definition.clone();
    fixture_settings(&source, json!({"stdout":native.to_string()}));
    let reply = wire_call(&mut c, json!("\u{1}".repeat(1024)));
    good(&reply);
    assert_eq!(reply["result"]["structuredContent"], graph);
    assert_eq!(reply["result"]["_meta"]["graphDefinition"], definition);
    assert_eq!(
        reply["result"]["content"][0]["text"],
        "Project graph available in structuredContent."
    );
}
// Execute only the packaged remote shell command in a synthetic host. Its local
// oms shim opens this fixture's real MCP server with the requested profile.
fn packaged_remote_catalog(source: &Path, repo: &Path, home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("oms");
    std::fs::write(&shim, b"#!/bin/sh\n[ \"$#:$1\" = 1:mcp-server ] || exit 2\nexec \"$FIXTURE_NATIVE\" --stdio \"$FIXTURE_SOURCE\" \"$PWD\" \"$OMS_MCP_TOOL_PROFILE\" 1\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let xdg = home.join("remote config");
    let map = xdg.join("oh-my-setting/desktop-projects.json");
    std::fs::create_dir_all(map.parent().unwrap()).unwrap();
    std::fs::write(&map, json!({"remote":repo}).to_string()).unwrap();
    std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o600)).unwrap();
    let codex = include_str!("../../../plugins/oms-ssh/oms/mcp.json");
    let generic = include_str!("../../../plugins/oms-app/mcp_config.json");
    let open = |manifest: &str, overrides: &[(&str, &str)]| {
        let pkg: Value = serde_json::from_str(manifest).unwrap();
        let argv = pkg["mcpServers"]["oms-panel"]["args"].as_array().unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", argv.last().unwrap().as_str().unwrap()])
            .current_dir(repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("FIXTURE_NATIVE", std::env::current_exe().unwrap())
            .env("FIXTURE_SOURCE", source)
            .envs(overrides.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Client::from_child(child)
    };
    let projects = |c: &mut Client| c.tool("oms_list_projects", json!({}));
    let mut desktop = open(codex, &[]);
    let mut readonly = open(generic, &[]);
    for (client, read_only) in [(&mut desktop, false), (&mut readonly, true)] {
        assert_eq!(
            registry_summary(&projects(client)),
            json!({"projects":["remote"],"binding":"catalog","readOnly":read_only})
        );
        good(&client.tool("oms_open_panel", json!({"project":"remote"})));
    }
    // A stale selection cannot silently retarget the current checkout.
    std::fs::write(&map, json!({"renamed":repo}).to_string()).unwrap();
    for client in [&mut desktop, &mut readonly] {
        bad(
            &client.tool("oms_open_panel", json!({"project":"remote"})),
            "OMS_PANEL_REPO",
        );
    }
    std::fs::remove_file(&map).unwrap();
    bad(&projects(&mut desktop), "Invalid OMS project map");
    assert_eq!(
        registry_summary(&projects(&mut readonly)),
        json!({"projects":[],"binding":"unbound","readOnly":true})
    );
    let mut pinned = open(generic, &[("OMS_PANEL_REPO", repo.to_str().unwrap())]);
    assert_eq!(
        registry_summary(&projects(&mut pinned)),
        json!({"projects":[],"binding":"pinned","readOnly":true})
    );
    let inline = json!({"operator":repo}).to_string();
    let mut catalog = open(generic, &[("OMS_PANEL_PROJECTS", &inline)]);
    assert_eq!(
        registry_summary(&projects(&mut catalog)),
        json!({"projects":["operator"],"binding":"catalog","readOnly":true})
    );
}

fn mcp_app_binding_boundaries() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = Temp::new();
    let source = tmp.source();
    let repo = tmp.repo();
    std::fs::create_dir(repo.join(".oms")).unwrap();
    packaged_remote_catalog(&source, &repo, &tmp.0.join("remote-home"));
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
    let original_key =
        payload(&c.tool("oms_list_projects", json!({})))["projectKeys"]["fixture"].clone();
    assert!(original_key.is_string());
    bind(json!({"fixture":other}));
    let changed_key =
        payload(&c.tool("oms_list_projects", json!({})))["projectKeys"]["fixture"].clone();
    assert!(changed_key.is_string());
    assert_ne!(
        original_key, changed_key,
        "alias remapping must invalidate physical-scope observations"
    );
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
    assert_eq!(
        c.tool(
            "oms_panel_project_graph",
            json!({"project":"fixture","repo":other})
        )["error"]["code"],
        -32602
    );
    bad(
        &c.tool("oms_panel_project_graph", json!({"project":other})),
        "OMS_PANEL_REPO",
    );
    bad(
        &c.tool("oms_panel_project_graph", json!({})),
        "OMS_PANEL_REPO",
    );
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
    let work = json!({"schema":1,"items":[],"omitted":0,"availability":"ready"});
    fixture_settings(&source, json!({"stdout":work.to_string()}));
    assert_eq!(
        payload(&c.tool("oms_panel_work_items", json!({"project":"fixture"}))),
        work
    );
    let argv = record(&source)["argv"].clone();
    assert!(
        argv.as_array()
            .unwrap()
            .ends_with(&[json!("--desktop-work-items"), json!("--json")])
    );
    fixture_settings(&source, json!({"stdout":snapshot.to_string()}));
    // Replacing the alias target with a symlink is refused on the next call.
    let link = source.join("link-checkout");
    std::os::unix::fs::symlink(&repo, &link).unwrap();
    bind(json!({"fixture":link}));
    bad(
        &ask(&mut c, json!({"project":"fixture"}), json!({})),
        "OMS_PANEL_REPO",
    );
    bad(
        &c.tool("oms_panel_project_graph", json!({"project":"fixture"})),
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
    bad(
        &reopened.tool("oms_panel_work_items", json!({"project":"fixture"})),
        "OMS_PANEL_REPO",
    );
    bad(&ask(&mut reopened, json!({}), json!({})), "OMS_PANEL_REPO");
}
fn desktop_reviewed_issue() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    std::fs::create_dir(repo.join(".oms")).unwrap();
    let dir = source.join("test-config/oh-my-setting");
    std::fs::create_dir_all(&dir).unwrap();
    let map = dir.join("desktop-projects.json");
    let bind = |path: &Path| {
        std::fs::write(&map, json!({"demo":path}).to_string()).unwrap();
        std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    bind(&repo);
    let mut c = Client::with_depth(&source, &repo, "chatgpt-panel", false, false, "0".as_ref());
    let record_tool = "oms_record_reviewed_issue";
    let status = "oms_reviewed_issue_status";
    let thread = "01900000-0000-7000-8000-0000000000bb";
    let request = "40000000-0000-4000-8000-000000000001";
    let host = json!({"threadId":thread});
    let pin = "a".repeat(64);
    let address =
        json!({"project":"demo","room":"room-main","owner":"codex-main","requestId":request});
    let mut args = address.clone();
    args["issue"] = json!("Org/repo#7");
    args["sha256"] = json!(pin);
    let receipt = json!({"schema":1,"kind":"oms-reviewed-issue-receipt","status":"recorded","room":"room-main","participant":"codex-main","requestId":request,"issue":"Org/repo#7","sha256":pin});
    let missing = json!({"status":"not_found","requestId":request});
    let params = |name: &str, arguments: &Value, meta: &Value| json!({"name":name,"arguments":arguments,"_meta":meta});
    let tools = c.call("tools/list", json!({}));
    let tool = |name| {
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"].as_str() == Some(name))
            .unwrap()
    };
    for full in [false, true] {
        let mut regular = Client::new(&source, &repo, full, false, false);
        let advertised = regular.call("tools/list", json!({}));
        for name in [record_tool, status] {
            assert!(
                !advertised["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["name"] == name)
            );
            assert_eq!(
                regular.call("tools/call", params(name, &args, &host))["error"]["code"],
                -32602
            );
        }
    }
    assert_eq!(
        tool(record_tool)["annotations"],
        json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true})
    );
    assert_eq!(
        tool(record_tool)["_meta"]["ui"]["visibility"],
        json!(["app"])
    );
    assert_eq!(tool(status)["annotations"]["readOnlyHint"], true);
    assert_eq!(
        tool(status)["_meta"]["ui"]["visibility"],
        json!(["app", "model"])
    );
    bad(
        &c.call("tools/call", params(record_tool, &args, &json!({}))),
        "bound conversation",
    );
    for (key, val) in [
        ("issue", "elsewhere"),
        ("issue", "org/..#7"),
        ("issue", "Org/repo#07"),
        ("owner", "../other"),
        ("room", "--help"),
        ("requestId", "not-a-uuid"),
        ("sha256", "wrong"),
    ] {
        let mut wrong = args.clone();
        wrong[key] = json!(val);
        assert_eq!(
            c.call("tools/call", params(record_tool, &wrong, &host))["result"]["isError"],
            true
        );
    }
    let mut forged = args.clone();
    forged["conversationDigest"] = json!("b".repeat(32));
    assert_eq!(
        c.call("tools/call", params(record_tool, &forged, &host))["error"]["code"],
        -32602
    );
    bad(
        &c.call("tools/call", params(status, &args, &host)),
        "Choose an exact",
    );
    let mut child = Client::with_depth(&source, &repo, "chatgpt-panel", false, false, "1".as_ref());
    bad(
        &child.call("tools/call", params(record_tool, &args, &host)),
        "parent app",
    );
    assert!(!source.join("last-call.json").exists());
    let configure = |data: &Value| fixture_settings(&source, json!({"stdout":data.to_string()}));
    configure(&receipt);
    assert_eq!(
        payload(&c.call("tools/call", params(record_tool, &args, &host))),
        receipt
    );
    let hashed = format!("{:x}", Sha256::digest(thread.as_bytes()))[..32].to_owned();
    assert_eq!(
        record(&source)["argv"],
        json!([
            "room",
            "desktop-issue-record",
            "--repo",
            repo,
            "--id",
            "room-main",
            "--conversation-digest",
            hashed,
            "--request",
            request,
            "--owner",
            "codex-main",
            "--issue",
            "Org/repo#7",
            "--digest",
            pin,
            "--json"
        ])
    );
    assert_eq!(
        payload(&c.call("tools/call", params(status, &address, &host))),
        receipt
    );
    let mut selection = args.clone();
    selection.as_object_mut().unwrap().remove("requestId");
    bad(
        &c.call("tools/call", params(status, &selection, &host)),
        "unrecognized",
    );
    let mut derived = receipt.clone();
    derived["requestId"] = json!("40000000-0000-8000-b000-000000000001");
    let absent = json!({"status":"not_found","requestId":derived["requestId"]});
    configure(&absent);
    assert_eq!(
        payload(&c.call("tools/call", params(status, &selection, &host))),
        absent
    );
    configure(&derived);
    assert_eq!(
        payload(&c.call("tools/call", params(status, &selection, &host))),
        derived
    );
    let argv = record(&source)["argv"].clone();
    assert_eq!(argv[1], "desktop-issue-status");
    assert!(!argv.as_array().unwrap().contains(&json!("--request")));
    let mut stale = derived.clone();
    stale["sha256"] = json!("f".repeat(64));
    configure(&stale);
    bad(
        &c.call("tools/call", params(status, &selection, &host)),
        "unrecognized",
    );
    configure(&missing);
    assert_eq!(
        payload(&c.call("tools/call", params(status, &address, &host))),
        missing
    );
    bad(
        &c.call("tools/call", params(record_tool, &args, &host)),
        "unrecognized",
    );
    for (key, val) in [
        ("room", "foreign"),
        ("participant", "wrong"),
        ("requestId", "other"),
        ("issue", "Org/repo#8"),
        ("sha256", &"b".repeat(64)),
        ("body", "private fixture text"),
    ] {
        let mut wrong = receipt.clone();
        wrong[key] = json!(val);
        configure(&wrong);
        let error = c.call("tools/call", params(record_tool, &args, &host));
        bad(&error, "unrecognized");
        assert!(!error.to_string().contains("private fixture text"));
    }
    fixture_settings(&source, json!({"exit":2,"stderr":"private fixture text"}));
    let error = c.call("tools/call", params(status, &address, &host));
    bad(&error, "could not confirm");
    assert!(!error.to_string().contains("private fixture text"));
    let other = t.0.join("other");
    std::fs::create_dir_all(other.join(".oms")).unwrap();
    fixture_settings(&source, json!({"hold":true,"stdout":receipt.to_string()}));
    let frame = json!({"id":915,"jsonrpc":"2.0","method":"tools/call","params":params(status,&address,&host)});
    std::fs::remove_file(source.join("last-call.json")).unwrap();
    c.raw(format!("{frame}\n").as_bytes());
    until(|| source.join("last-call.json").exists());
    bind(&other);
    std::fs::write(source.join("release"), "").unwrap();
    bad(&c.read(), "OMS_PANEL_REPO");
}

fn worker_output_reader(c: &mut Client, source: &Path, key: &str, thread: &str) {
    let name = "oms_worker_output";
    let args = json!({"projectKey":key,"room":"lead-room","leadAttempt":"lead-fixture","workerAttempt":"worker-fixture"});
    let request = |c: &mut Client, a: &Value| {
        c.call(
            "tools/call",
            json!({"name":name,"arguments":a,"_meta":{"threadId":thread}}),
        )
    };
    let tools = c.call("tools/list", json!({}));
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap();
    assert_eq!(tool["_meta"]["ui"]["visibility"], json!(["app"]));
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    bad(&c.tool(name, args.clone()), "bound conversation");
    for (field, value, want) in [
        ("projectKey", "obsolete", "project"),
        ("workerAttempt", "../other", "Invalid"),
        ("leadAttempt", "worker-fixture", "under its Lead"),
    ] {
        let mut invalid = args.clone();
        invalid[field] = value.into();
        bad(&request(c, &invalid), want);
    }
    let mut extra = args.clone();
    extra["path"] = "private-local-path".into();
    assert_eq!(request(c, &extra)["error"]["code"], -32602);
    let base = json!({"room":args["room"],"leadAttempt":args["leadAttempt"],"workerAttempt":args["workerAttempt"],"status":"complete",
        "source":"native-report","text":"verification finished in private worker","truncated":false,
        "untrusted":"fixture-source-hidden"});
    let configure = |v: &Value| fixture_settings(source, json!({"stdout":v.to_string()}));
    configure(&base);
    let r = request(c, &args);
    good(&r);
    let meta = &r["result"]["_meta"]["workerOutput"];
    assert_eq!(meta["text"], base["text"]);
    assert_eq!(meta["projectKey"], key);
    assert_eq!(meta["source"], "native-report");
    assert!(!r.to_string().contains("fixture-source-hidden"));
    for k in ["content", "structuredContent"] {
        assert!(!r["result"][k].to_string().contains("verification finished"));
    }
    let argv = record(source)["argv"].clone();
    assert_eq!(argv[3], "--desktop-worker-output");
    assert_eq!(argv[8], args["leadAttempt"]);
    assert_eq!(argv[10], args["workerAttempt"]);
    assert_eq!(argv[11], "--conversation-digest");
    assert_eq!(argv[12].as_str().unwrap().len(), 32);
    assert!(!argv.to_string().contains(thread));
    let mut full = args.clone();
    full["answerPage"] = json!("");
    let mut first = base.clone();
    first["text"] = json!("abc");
    first["truncated"] = json!(true);
    first["page"] = json!({"page":1,"offset":0,"bytes":3,"totalBytes":11,"digest":"d".repeat(64),
        "revision":"e".repeat(64),"hasMore":true,"nextPage":2});
    configure(&first);
    let r = request(c, &full);
    good(&r);
    let cursor = r["result"]["_meta"]["workerOutput"]["page"]["nextCursor"].clone();
    assert!(cursor.is_string());
    full["answerPage"] = cursor;
    let mut second = first.clone();
    second["text"] = json!(" \n\t ");
    second["page"]["page"] = json!(2);
    second["page"]["offset"] = json!(3);
    second["page"]["bytes"] = json!(4);
    second["page"]["nextPage"] = json!(3);
    configure(&second);
    let r = request(c, &full);
    good(&r);
    assert_eq!(r["result"]["_meta"]["workerOutput"]["text"], " \n\t ");
    full["answerPage"] = r["result"]["_meta"]["workerOutput"]["page"]["nextCursor"].clone();
    assert!(full["answerPage"].is_string());
    bad(&request(c, &args), "could not be verified");
    second["text"] = json!("defg");
    second["truncated"] = json!(false);
    second["page"]["page"] = json!(3);
    second["page"]["offset"] = json!(7);
    second["page"]["hasMore"] = json!(false);
    second["page"]["nextPage"] = Value::Null;
    configure(&second);
    let r = request(c, &full);
    good(&r);
    assert_eq!(r["result"]["_meta"]["workerOutput"]["text"], "defg");
    assert!(r["result"]["_meta"]["workerOutput"]["page"]["nextCursor"].is_null());
    let mut wrong = full.clone();
    wrong["room"] = json!("other-room");
    let mut foreign = second.clone();
    foreign["room"] = wrong["room"].clone();
    configure(&foreign);
    bad(&request(c, &wrong), "another selection");
    second["page"]["revision"] = json!("f".repeat(64));
    configure(&second);
    bad(&request(c, &full), "changed");
    full["answerPage"] = json!("../answer");
    bad(&request(c, &full), "cursor");
    for status in ["running", "not_recorded", "unavailable"] {
        let mut r = base.clone();
        r["status"] = status.into();
        r["text"] = Value::Null;
        r["source"] = Value::Null;
        configure(&r);
        let answer = request(c, &args);
        good(&answer);
        assert_eq!(payload(&answer)["status"], status);
        assert!(answer["result"]["_meta"]["workerOutput"]["text"].is_null());
    }
    for (k, v) in [
        ("workerAttempt", json!("other")),
        ("room", json!("other")),
        ("leadAttempt", json!("other")),
        ("text", Value::Null),
        ("text", json!("z".repeat(16 * 1024 + 1))),
        ("source", json!("arbitrary-path")),
        ("status", json!("running")),
    ] {
        let mut r = base.clone();
        r[k] = v;
        configure(&r);
        let refused = request(c, &args);
        bad(&refused, "could not be verified");
        assert!(refused["result"]["_meta"]["workerOutput"].is_null());
    }
    let mut withheld = base.clone();
    withheld["text"] = json!(concat!("gh", "p_", "0123456789abcdef0123456789abcdef0123"));
    configure(&withheld);
    let refused = request(c, &args);
    bad(&refused, "withheld");
    assert!(
        !refused
            .to_string()
            .contains(withheld["text"].as_str().unwrap())
    );

    // A secret straddling an 8192-byte page boundary (including beyond the 16 KiB preview)
    // withholds every page of worker output via the shared whole-body scan.
    let clean_worker = format!("{}{}", "w".repeat(16_382), "y".repeat(600));
    fixture_settings(
        source,
        json!({"pagedWorkerBody":clean_worker,"room":args["room"],"leadAttempt":args["leadAttempt"],"workerAttempt":args["workerAttempt"]}),
    );
    let mut paged = args.clone();
    paged["answerPage"] = json!("");
    let p1 = request(c, &paged);
    good(&p1);
    let c2 = p1["result"]["_meta"]["workerOutput"]["page"]["nextCursor"].clone();
    paged["answerPage"] = c2.clone();
    let p2 = request(c, &paged);
    good(&p2);
    let c3 = p2["result"]["_meta"]["workerOutput"]["page"]["nextCursor"].clone();
    assert!(c3.is_string());

    let left = ["gh", "p_LeftHalf012345"].concat();
    let right = "RightHalf67890abcdef01";
    let straddling_first_worker = format!(
        "{} {left}{right} {}",
        "w".repeat(8_192 - 1 - left.len()),
        "y".repeat(8_800)
    );
    let straddling_worker = format!(
        "{} {left}{right} {}",
        "w".repeat(16_384 - 1 - left.len()),
        "y".repeat(560)
    );
    let machine_left = ["/ho", "me/"].concat();
    let machine_right = "runner/private-tree";
    let straddling_machine_worker = format!(
        "{} {machine_left}{machine_right} {}",
        "w".repeat(8_192 - 1 - machine_left.len()),
        "y".repeat(8_800)
    );
    for tainted in [
        &straddling_first_worker,
        &straddling_worker,
        &straddling_machine_worker,
    ] {
        fixture_settings(
            source,
            json!({"pagedWorkerBody":tainted,"room":args["room"],"leadAttempt":args["leadAttempt"],"workerAttempt":args["workerAttempt"]}),
        );
        bad(&request(c, &args), "withheld");
        for cursor in [json!(""), c2.clone(), c3.clone()] {
            let mut req = args.clone();
            req["answerPage"] = cursor;
            let refused = request(c, &req);
            bad(&refused, "withheld");
            let wire = refused.to_string();
            assert!(
                !wire.contains(&left)
                    && !wire.contains(right)
                    && !wire.contains(&machine_left)
                    && !wire.contains(machine_right)
                    && !wire.contains(&"w".repeat(32))
                    && !wire.contains(&"y".repeat(32))
            );
        }
    }
}

fn desktop_lead_conversations() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    std::fs::create_dir(repo.join(".oms")).unwrap();
    let mut c = Client::with_depth(&source, &repo, "chatgpt-panel", false, false, "0".as_ref());
    let read = "oms_lead_conversation_status";
    let open = "oms_open_lead_conversations";
    let owner = "01900000-0000-7000-8000-0000000000bb";
    let args = json!({"room":"lead-room"});
    let call = |c: &mut Client, name: &str, params: &Value, thread: &str| {
        c.call(
            "tools/call",
            json!({"name":name,"arguments":params,"_meta":{"threadId":thread}}),
        )
    };
    let runs = || std::fs::read_dir(repo.join(".oms/artifacts/mcp")).map_or(0, |d| d.count());
    let opener = c.tool("oms_open_panel", json!({"leadConversations":true}));
    assert_eq!(payload(&opener)["leadConversations"], true);
    assert!(
        payload(&c.tool("oms_open_panel", json!({})))
            .get("leadConversations")
            .is_none()
    );
    assert_eq!(
        c.tool("oms_open_panel", json!({"leadConversations":"yes"}))["result"]["isError"],
        true
    );
    assert_eq!(record(&source)["argv"][0], "dashboard");
    std::fs::remove_file(source.join("last-call.json")).unwrap();
    assert_eq!(
        call(
            &mut c,
            open,
            &json!({"room":"lead-room","owner":"other"}),
            owner
        )["error"]["code"],
        -32602
    );
    for (profile, child) in [("mcp-app", false), ("chatgpt-panel", true)] {
        let mut blocked = Client::with_depth(
            &source,
            &repo,
            profile,
            false,
            child,
            if child { "1" } else { "0" }.as_ref(),
        );
        for name in [
            open,
            read,
            "oms_send_lead_message",
            "oms_worker_output",
            "oms_lead_settings",
            "oms_update_lead_settings",
            "oms_prepare_lead_plan_context",
        ] {
            let denied = call(&mut blocked, name, &args, owner);
            if child {
                bad(&denied, "parent app");
            } else {
                assert_eq!(denied["error"]["code"], -32602);
            }
        }
    }
    bad(&call(&mut c, open, &args, ""), "bound conversation");
    assert!(!source.join("last-call.json").exists());
    let setup = |fail_link: bool| {
        fixture_settings(&source, json!({"leadRoom":true,"failClaudeLink":fail_link}))
    };
    setup(true);
    bad(
        &call(&mut c, open, &args, "01900000-0000-7000-8000-0000000000aa"),
        "current Main",
    );
    let initial = payload(&call(&mut c, read, &args, owner));
    assert_eq!(initial["leads"].as_array().unwrap().len(), 3);
    assert!(
        initial["leads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "not_started")
    );
    let key = initial["projectKey"].as_str().unwrap();
    assert_eq!(key.len(), 64);
    // The worker reader and Lead chat must agree on the current physical checkout.
    worker_output_reader(&mut c, &source, key, owner);
    setup(true);

    bad(
        &call(
            &mut c,
            open,
            &json!({"room":"lead-room","projectKey":"f".repeat(64)}),
            owner,
        ),
        "project changed",
    );
    assert_eq!(runs(), 0);
    for providers in [
        json!([]),
        json!(["codex", "codex"]),
        json!(["pi"]),
        json!("codex"),
    ] {
        let invalid = call(
            &mut c,
            open,
            &json!({"room":"lead-room","providers":providers}),
            owner,
        );
        assert_eq!(invalid["result"]["isError"], true);
        assert_eq!(runs(), 0);
    }
    let single = json!({"room":"lead-room","projectKey":key,"providers":["codex"]});
    let first = payload(&call(&mut c, open, &single, owner));
    assert_eq!(first["leads"].as_array().unwrap().len(), 1);
    assert_eq!(first["leads"][0]["provider"], "codex");
    assert_eq!(
        first["leads"][0]["requestId"],
        initial["leads"][0]["requestId"]
    );
    good(&call(&mut c, open, &single, owner));
    assert_eq!(runs(), 1, "selective reopen must reuse its one reservation");
    let opened = payload(&call(
        &mut c,
        open,
        &json!({"room":"lead-room","projectKey":key}),
        owner,
    ));
    assert_eq!(opened["leads"][1]["status"], "unconfirmed");
    assert_eq!(runs(), 3);
    until(|| {
        std::fs::read_dir(repo.join(".oms/artifacts/mcp"))
            .unwrap()
            .all(|e| e.unwrap().path().join("status").exists())
    });
    let status = payload(&call(&mut c, read, &args, owner));
    assert_eq!(status["leads"][0]["status"], "ready");
    assert_eq!(status["leads"][1]["status"], "unconfirmed");
    assert_eq!(status["leads"][2]["status"], "ready");
    assert!(status.get("leadConversation").is_none());
    setup(false);
    let repaired = payload(&call(&mut c, open, &args, owner));
    assert_eq!(repaired["leads"][1]["status"], "ready");
    assert_eq!(repaired, payload(&call(&mut c, open, &args, owner)));
    assert_eq!(
        runs(),
        3,
        "a repeat or a receipt repair started a duplicate"
    );
    let one = call(
        &mut c,
        read,
        &json!({"room":"lead-room","provider":"claude"}),
        owner,
    );
    let detail = &one["result"]["_meta"]["leadConversation"];
    assert_eq!(detail["room"], "lead-room");
    assert_eq!(detail["provider"], "claude");
    assert_eq!(detail["transcript"]["turns"][0]["text"], "fixture answer");
    assert!(
        !one["result"]["structuredContent"]
            .to_string()
            .contains("fixture answer")
    );
    assert!(
        !one["result"]["content"]
            .to_string()
            .contains("fixture answer")
    );
    let receipt: Value =
        serde_json::from_slice(&std::fs::read(source.join("lead-fixture-claude")).unwrap())
            .unwrap();
    let op = receipt["operation"].as_str().unwrap();
    let dir = repo.join(".oms/artifacts/mcp").join(op);
    let thread_log = source.join(format!("thread-{op}"));
    std::fs::write(&thread_log, "closed").unwrap();
    for _ in 0..2 {
        let detail = call(
            &mut c,
            read,
            &json!({"room":"lead-room","provider":"claude"}),
            owner,
        );
        assert_eq!(payload(&detail)["leads"][0]["status"], "unconfirmed");
        assert!(detail["result"]["_meta"]["leadConversation"]["transcript"].is_null());
        assert_eq!(
            payload(&call(&mut c, open, &args, owner))["leads"][1]["status"],
            "unconfirmed"
        );
        assert_eq!(runs(), 3);
        if thread_log.exists() {
            std::fs::remove_file(&thread_log).unwrap();
        }
    }
    std::fs::write(&thread_log, "open").unwrap();
    let metadata = dir.join("meta.json");
    let original = std::fs::read(&metadata).unwrap();
    let mut altered: Value = serde_json::from_slice(&original).unwrap();
    altered.as_object_mut().unwrap().remove("answer_storage");
    std::fs::write(&metadata, altered.to_string()).unwrap();
    assert_eq!(
        payload(&call(&mut c, read, &args, owner))["leads"][1]["status"],
        "unconfirmed"
    );
    std::fs::write(&metadata, original).unwrap();
    std::fs::write(
        dir.join("answers/fixture.md"),
        "## Output\nempty failure\n## Exit\n7\n",
    )
    .unwrap();
    assert_eq!(
        payload(&call(&mut c, read, &args, owner))["leads"][1]["status"],
        "unconfirmed"
    );
    std::fs::write(dir.join("status"), "-15\n").unwrap();
    assert_eq!(
        payload(&call(&mut c, read, &args, owner))["leads"][1]["status"],
        "unavailable"
    );
    assert_eq!(runs(), 3);
    std::fs::write(dir.join("status"), "0\n").unwrap();
    std::fs::write(
        dir.join("answers/fixture.md"),
        "## Output\nfixture answer\n## Exit\n0\n",
    )
    .unwrap();

    let send = "oms_send_lead_message";
    let tools = c.call("tools/list", json!({}));
    let schema = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == send)
        .unwrap();
    assert_eq!(schema["_meta"]["ui"]["visibility"], json!(["app"]));
    assert_eq!(schema["annotations"]["idempotentHint"], true);
    let prepare = "oms_prepare_lead_plan_context";
    let work_items = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "oms_panel_work_items")
        .unwrap();
    assert_eq!(
        work_items["_meta"]["ui"]["visibility"],
        json!(["app", "model"])
    );
    assert_eq!(work_items["annotations"]["readOnlyHint"], true);
    let schema = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == prepare)
        .unwrap();
    assert_eq!(schema["_meta"]["ui"]["visibility"], json!(["app"]));
    assert!(
        schema["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("excerpt"))
    );
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| {
                t["_meta"]["ui"]["visibility"]
                    .as_array()
                    .is_some_and(|v| v.contains(&json!("model")))
            })
            .all(|t| {
                t["name"] != prepare
                    && t["inputSchema"]["properties"].get("excerpt").is_none()
                    && t["inputSchema"]["properties"]
                        .get("excerptSha256")
                        .is_none()
            }),
        "no model-visible tool may accept or verify a guessed Lead excerpt"
    );
    for full in [false, true] {
        let mut regular = Client::new(&source, &repo, full, false, false);
        assert!(
            !regular.call("tools/list", json!({}))["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == prepare)
        );
        assert_eq!(regular.tool(prepare, json!({}))["error"]["code"], -32602);
    }
    let message = |seat: usize, n: usize| {
        json!({"room":"lead-room","projectKey":key,"provider":initial["leads"][seat]["provider"],
            "leadRequestId":initial["leads"][seat]["requestId"],"turnId":format!("10000000-0000-4000-8000-{n:012x}"),
            "text":format!("Private fixture follow-up {n}")})
    };
    let lookup = |c: &mut Client, m: &Value| {
        call(
            c,
            read,
            &json!({"room":m["room"],"provider":m["provider"],"turnId":m["turnId"]}),
            owner,
        )
    };
    let await_reply = |c: &mut Client, m: &Value| {
        until(|| {
            let r = lookup(c, m);
            good(&r);
            let state = payload(&r)["turn"]["status"].clone();
            assert!(
                matches!(
                    state.as_str(),
                    Some("reserved" | "running" | "unconfirmed" | "replied")
                ),
                "{r}"
            );
            state == "replied"
        });
        let res = lookup(c, m);
        assert!(
            !res["result"]["structuredContent"]
                .to_string()
                .contains("Private fixture")
        );
        assert!(
            !res["result"]["content"]
                .to_string()
                .contains("Private fixture")
        );
        let transcript = res["result"]["_meta"]["leadConversation"]["transcript"].clone();
        let turns = transcript["turns"].as_array().unwrap();
        let pair: Vec<_> = turns
            .iter()
            .filter(|t| t["turnId"] == m["turnId"])
            .collect();
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0]["role"], "user");
        assert_eq!(pair[0]["status"], "replied");
        assert_eq!(pair[0]["text"], m["text"]);
        let native = if m["provider"] == "agy" {
            "antigravity"
        } else {
            m["provider"].as_str().unwrap()
        };
        assert_eq!(pair[1]["role"], native);
        assert_eq!(
            pair[1]["text"],
            format!("{native} received {}", m["text"].as_str().unwrap())
        );
        transcript
    };
    for (k, v, error) in [
        ("projectKey", json!("f".repeat(64)), "project changed"),
        (
            "leadRequestId",
            initial["leads"][2]["requestId"].clone(),
            "selected Lead changed",
        ),
        ("text", json!("invalid\u{0}message"), "bytes of text"),
    ] {
        let mut m = message(0, 1);
        m[k] = v;
        bad(&call(&mut c, send, &m, owner), error);
    }
    bad(
        &call(
            &mut c,
            send,
            &message(0, 1),
            "01900000-0000-7000-8000-0000000000aa",
        ),
        "current Main",
    );
    assert_eq!(runs(), 3);

    // The native models call reads host evidence only. Claude currently has
    // policy aliases and no model catalog; a global effort list proves no pair.
    std::fs::create_dir_all(source.join("config")).unwrap();
    std::fs::write(
        source.join("config/panel-routing.json"),
        json!({"schema":2,"tiers":{"deep":[{"provider":"claude","model":"opus","effort":"high"}]}})
            .to_string(),
    )
    .unwrap();
    let preferences = "oms_lead_settings";
    let change = "oms_update_lead_settings";
    let settings_address = |seat: usize| json!({"projectKey":key,"room":"lead-room","leadRequestId":initial["leads"][seat]["requestId"],"provider":initial["leads"][seat]["provider"]});
    let change_to = |seat: usize, revision: &Value, model: Value, effort: Value| {
        let mut req = settings_address(seat);
        req["revision"] = revision.clone();
        req["model"] = model;
        req["effort"] = effort;
        req
    };
    for name in [preferences, change] {
        let schema = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap();
        assert_eq!(schema["_meta"]["ui"]["visibility"], json!(["app"]));
        assert_eq!(schema["annotations"]["readOnlyHint"], name == preferences);
        bad(
            &call(
                &mut c,
                name,
                &settings_address(0),
                "01900000-0000-7000-8000-0000000000aa",
            ),
            "current Main",
        );
        let mut wrong = settings_address(0);
        wrong["leadRequestId"] = initial["leads"][1]["requestId"].clone();
        bad(&call(&mut c, name, &wrong, owner), "selected Lead changed");
        wrong["projectKey"] = json!("f".repeat(64));
        bad(&call(&mut c, name, &wrong, owner), "project changed");
    }
    for seat in 0..3 {
        let view = call(&mut c, preferences, &settings_address(seat), owner);
        good(&view);
        let public = payload(&view);
        let settings = &view["result"]["_meta"]["leadSettings"];
        assert_eq!(settings["selection"], json!({"model":null,"effort":null}));
        assert_eq!(
            settings["scope"],
            json!({"target":"lead-conversation","appliesTo":"next-message",
            "inheritsToWorkers":false,"mode":"read-only-consultation","continuity":"oms-thread"})
        );
        for field in ["account", "failover", "usage"] {
            assert!(settings[field].is_null());
        }
        assert_eq!(public["revision"], settings["revision"]);
        assert!(settings["nativeTitle"].is_null());
        assert_eq!(
            settings["lastServed"],
            if seat == 1 {
                json!({"model":"claude-observed","effort":null})
            } else {
                Value::Null
            }
        );
        if seat == 0 {
            assert!(settings["nativeSession"].is_null());
        } else {
            assert_eq!(
                settings["nativeSession"],
                json!({"source":"provider-result","scope":"invocation","resumable":false,"id":"a1000000-0000-4000-8000-000000000001"})
            );
            for visible in [
                &view["result"]["structuredContent"],
                &view["result"]["content"],
            ] {
                assert!(
                    !visible.to_string().contains("a1000000-")
                        && !visible.to_string().contains("claude-observed")
                );
            }
        }
        if seat == 1 {
            let proof_path = dir.join("answers/native-lead-observation.json");
            let original = std::fs::read(&proof_path).unwrap();
            for (k, v) in [
                ("operation", "another-op"),
                ("provider", "antigravity"),
                ("sessionId", "../other"),
                ("artifact", "../other.md"),
                ("artifactSha256", "incorrect"),
            ] {
                let mut bad_proof: Value = serde_json::from_slice(&original).unwrap();
                bad_proof[k] = json!(v);
                std::fs::write(&proof_path, bad_proof.to_string()).unwrap();
                let res = call(&mut c, preferences, &settings_address(seat), owner);
                good(&res);
                let detail = &res["result"]["_meta"]["leadSettings"];
                assert!(detail["nativeSession"].is_null() && detail["lastServed"].is_null());
            }
            std::fs::remove_file(&proof_path).unwrap();
            let historical = call(&mut c, preferences, &settings_address(seat), owner);
            good(&historical);
            assert!(historical["result"]["_meta"]["leadSettings"]["nativeSession"].is_null());
            std::fs::write(&proof_path, original).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&proof_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let model = ["gpt-6-astra", "opus", "gemini-4-carbon"][seat];
        let row = settings["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["model"] == model)
            .unwrap();
        assert_eq!(row["source"], if seat == 1 { "policy" } else { "catalog" });
        assert_eq!(row["efforts"], json!(["high"]));
        assert_eq!(row["requiresEffort"], seat == 2);
        assert!(!public.to_string().contains(model));
        if seat == 2 {
            let fixed = settings["options"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["model"] == "gemini-pro-orchestrator")
                .unwrap();
            assert_eq!(fixed["fixed"], true);
            for (model, effort) in [
                (model, Value::Null),
                ("gemini-pro-orchestrator", json!("high")),
            ] {
                bad(
                    &call(
                        &mut c,
                        change,
                        &change_to(seat, &public["revision"], json!(model), effort),
                        owner,
                    ),
                    "not available",
                );
            }
        }
        bad(
            &call(
                &mut c,
                change,
                &change_to(seat, &public["revision"], json!(model), json!("ultra")),
                owner,
            ),
            "not available",
        );
        let set = change_to(seat, &public["revision"], json!(model), json!("high"));
        let changed = call(&mut c, change, &set, owner);
        good(&changed);
        assert_ne!(payload(&changed)["revision"], public["revision"]);
        assert_eq!(
            changed["result"]["_meta"]["leadSettings"]["selection"],
            json!({"model":model,"effort":"high"})
        );
        bad(&call(&mut c, change, &set, owner), "settings changed");
        let m = message(seat, seat + 1);
        if seat == 0 {
            fixture_settings(&source, json!({"leadRoom":true,"gate":true}));
        }
        let ack = call(&mut c, send, &m, owner);
        good(&ack);
        assert_eq!(payload(&ack)["turn"]["turnId"], m["turnId"]);
        if seat == 0 {
            until(|| payload(&lookup(&mut c, &m))["turn"]["status"] == "running");
            let pending = payload(&call(&mut c, read, &args, owner));
            assert_eq!(pending["leads"][0]["latestTurn"]["turnId"], m["turnId"]);
            assert_eq!(pending["leads"][0]["latestTurn"]["status"], "running");
            assert!(!pending.to_string().contains("Private fixture"));
            bad(
                &call(
                    &mut c,
                    read,
                    &json!({"room":m["room"],"projectKey":key,"provider":m["provider"],
                "leadRequestId":m["leadRequestId"],"turnId":m["turnId"],"answerPage":""}),
                    owner,
                ),
                "completed answer",
            );
            let other = message(0, 9);
            bad(&call(&mut c, send, &other, owner), "unresolved");
            assert_eq!(
                payload(&lookup(&mut c, &other))["turn"]["status"],
                "not_found"
            );
            good(&call(&mut c, send, &m, owner));
            std::fs::write(source.join("release"), "").unwrap();
            setup(false);
        }
        await_reply(&mut c, &m);
        let latest = payload(&call(&mut c, read, &args, owner));
        assert_eq!(latest["leads"][seat]["latestTurn"]["turnId"], m["turnId"]);
        assert_eq!(latest["leads"][seat]["latestTurn"]["status"], "replied");
        assert!(latest["leads"][seat]["latestTurn"]["lastObservedAt"].is_string());
        if seat == 0 {
            let child = std::fs::read_dir(repo.join(".oms/artifacts/mcp"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    std::fs::read_to_string(p.join("prompt.txt"))
                        .ok()
                        .as_deref()
                        == Some("Private fixture follow-up 1\n")
                })
                .unwrap();
            let artifact = child.join("answers/fixture.md");
            let original = std::fs::read(&artifact).unwrap();
            let long = format!("{}끝", "긴문장🙂".repeat(1500));
            std::fs::write(&artifact, format!("## Output\n{long}\n## Exit\n0\n")).unwrap();
            let mut full = json!({"room":m["room"],"projectKey":key,"provider":m["provider"],
                "leadRequestId":m["leadRequestId"],"turnId":m["turnId"],"answerPage":""});
            let mut text = String::new();
            let mut first_cursor = Value::Null;
            loop {
                let r = call(&mut c, read, &full, owner);
                good(&r);
                let a = &r["result"]["_meta"]["leadConversation"]["answer"];
                assert_eq!(a["offset"], text.len());
                assert!(a["text"].as_str().unwrap().len() <= 8192);
                text.push_str(a["text"].as_str().unwrap());
                assert_eq!(a["handoff"]["turnId"], m["turnId"]);
                for field in ["content", "structuredContent"] {
                    assert!(!r["result"][field].to_string().contains("긴문장"));
                }
                if first_cursor.is_null() {
                    first_cursor = a["nextCursor"].clone();
                }
                if a["hasMore"] == false {
                    break;
                }
                full["answerPage"] = a["nextCursor"].clone();
            }
            assert_eq!(text, long);
            assert_eq!(runs(), 4, "reading complete answers must not dispatch");
            // An explicit Share payload contains the displayed text and both
            // digests. Metadata alone cannot export a private answer.
            let first = {
                let mut read = full.clone();
                read["answerPage"] = json!("");
                read
            };
            let selected = call(&mut c, read, &first, owner)["result"]["_meta"]["leadConversation"]
                ["answer"]
                .clone();
            let excerpt = "긴문장🙂";
            let digest = |s: &str| oms_command_plan::json::sha(s);
            let task = json!({"id":"reviewed-task","title":"Frozen implementation","brief":"Implement src with evidence","state":"ready","task_sha256":"a".repeat(64),"contract_stale":false,"allowed_paths":["src"],"forbidden_paths":[],"verify":"true","depends":[],"role":"implementation-worker","assignment":{"provider":"claude","model":"frozen-model","reasoning_effort":"high"}});
            fixture_settings(&source, json!({"leadRoom":true,"leadContextTask":task}));
            let share = json!({"kind":"oms-selected-finding","authority":"context-only","source":{"kind":"lead-answer","projectKey":key,"room":m["room"],"provider":m["provider"],"participant":selected["handoff"]["participant"],"leadRequestId":m["leadRequestId"],"turnId":m["turnId"],"operation":selected["handoff"]["operation"],"digest":selected["digest"],"pageCursor":""},"selection":{"text":excerpt,"offset":0,"bytes":excerpt.len(),"totalBytes":selected["totalBytes"],"digest":selected["digest"],"excerptSha256":digest(excerpt),"truncated":true,"displayedBytes":selected["bytes"]}});
            let src = &share["source"];
            let selection = &share["selection"];
            let prepare_args = json!({"room":src["room"],"projectKey":src["projectKey"],"provider":src["provider"],"leadRequestId":src["leadRequestId"],"turnId":src["turnId"],"answerPage":src["pageCursor"],"answerSha256":src["digest"],"offset":selection["offset"],"bytes":selection["bytes"],"excerpt":selection["text"],"excerptSha256":selection["excerptSha256"],"taskId":task["id"],"taskSha256":task["task_sha256"],"executionProvider":"claude"});
            let result = call(&mut c, prepare, &prepare_args, owner);
            good(&result);
            let result = payload(&result);
            assert_eq!(result["status"], "prepared");
            assert_eq!(result["findings"]["selection"]["text"], excerpt);
            assert_eq!(result["execution"]["mode"], "managed-plan-run");
            assert_eq!(result["execution"]["nativeLeadInput"], false);
            assert_eq!(result["execution"]["startsTask"], false);
            for command in ["previewCommand", "runCommand"] {
                assert_eq!(result[command]["cwd"], repo.to_str().unwrap());
                assert_eq!(result[command]["argv"][0], "panel");
                assert_eq!(result[command]["argv"][1], "--repo");
                assert_eq!(result[command]["argv"][2], repo.to_str().unwrap());
                assert_eq!(result[command]["argv"][3], "--room");
                assert_eq!(result[command]["argv"][4], m["room"]);
            }
            assert_eq!(result["findings"]["target"]["provider"], "claude");
            assert_eq!(
                result["findings"]["target"]["task_sha256"],
                task["task_sha256"]
            );
            let pack_path = repo.join(result["contextPack"]["path"].as_str().unwrap());
            let pack = std::fs::read(&pack_path).unwrap();
            assert_eq!(
                oms_command_plan::json::sha(&pack),
                result["contextPack"]["sha256"]
            );
            let checked = oms_command_plan::reviewed_context::from_pack(&pack)
                .unwrap()
                .unwrap();
            checked.check_project(&repo).unwrap();
            assert_eq!(checked.value().get("selection").text("text"), excerpt);
            assert_eq!(
                payload(&call(&mut c, prepare, &prepare_args, owner)),
                result,
                "identical selection reuses the context pack"
            );
            assert_eq!(
                runs(),
                4,
                "preparing context never dispatches or creates a transcript store"
            );
            let fixed_mismatch =
                "The reviewed Lead excerpt or Plan task changed. Review them again.";
            for (field, value, expect_fixed) in [
                ("excerpt", Value::Null, true),
                ("excerpt", json!("different caller text"), true),
                ("excerpt", json!(" "), true),
                ("offset", json!(1), true),
                ("offset", json!(2), true),
                ("offset", json!("긴문장".len() + 1), true),
                ("bytes", json!(4097), true),
                ("offset", json!(8192), true),
                ("projectKey", json!("b".repeat(64)), false),
                ("room", json!("another-room"), false),
                ("taskSha256", json!("b".repeat(64)), true),
                ("executionProvider", json!("antigravity"), true),
                ("answerSha256", json!("b".repeat(64)), true),
                ("excerptSha256", json!("b".repeat(64)), true),
            ] {
                let mut bad_args = prepare_args.clone();
                bad_args[field] = value;
                let refused = call(&mut c, prepare, &bad_args, owner);
                assert_eq!(refused["result"]["isError"], true, "{refused}");
                assert!(
                    !refused.to_string().contains("긴문장"),
                    "errors must not disclose actual source text"
                );
                if expect_fixed {
                    assert_eq!(
                        refused["result"]["content"][0]["text"], fixed_mismatch,
                        "mismatch on {field} must return the fixed error without leaking boundaries"
                    );
                }
            }
            for count in 2..=16 {
                let chunk = "긴문장🙂".repeat(count);
                let mut more = prepare_args.clone();
                more["bytes"] = json!(chunk.len());
                more["excerpt"] = json!(chunk);
                more["excerptSha256"] = json!(digest(&chunk));
                good(&call(&mut c, prepare, &more, owner));
            }
            assert_eq!(
                payload(&call(&mut c, prepare, &prepare_args, owner)),
                result,
                "existing pack with same digest is reused even at the per-operation cap"
            );
            let overflow = "긴문장🙂".repeat(17);
            let mut over_args = prepare_args.clone();
            over_args["bytes"] = json!(overflow.len());
            over_args["excerpt"] = json!(overflow);
            over_args["excerptSha256"] = json!(digest(&overflow));
            bad(
                &call(&mut c, prepare, &over_args, owner),
                "maximum 16 prepared context packs",
            );
            let pack_files = std::fs::read_dir(&child)
                .unwrap()
                .filter_map(|e| e.ok().and_then(|d| d.file_name().into_string().ok()))
                .filter(|name| name.starts_with("lead-plan-context-") && name.ends_with(".json"))
                .count();
            assert_eq!(pack_files, 16);
            let mut changed_task = task.clone();
            changed_task["state"] = json!("claimed");
            fixture_settings(
                &source,
                json!({"leadRoom":true,"leadContextTask":changed_task}),
            );
            bad(&call(&mut c, prepare, &prepare_args, owner), "changed");
            fixture_settings(&source, json!({"leadRoom":true,"leadContextTask":task}));
            std::fs::write(&artifact, "## Output\nchanged source\n## Exit\n0\n").unwrap();
            let refused = call(&mut c, prepare, &prepare_args, owner);
            assert_eq!(refused["result"]["isError"], true);
            assert_eq!(refused["result"]["content"][0]["text"], fixed_mismatch);
            assert!(!refused.to_string().contains("changed source"));
            std::fs::write(&artifact, format!("## Output\n{long}\n## Exit\n0\n")).unwrap();
            let mut bad_zero = prepare_args.clone();
            bad_zero["bytes"] = json!(0);
            let refused_zero = call(&mut c, prepare, &bad_zero, owner);
            assert_eq!(refused_zero["result"]["isError"], true);
            assert_eq!(refused_zero["result"]["content"][0]["text"], fixed_mismatch);
            setup(false);
            full["answerPage"] = first_cursor.clone();
            let mut wrong = full.clone();
            wrong["leadRequestId"] = initial["leads"][1]["requestId"].clone();
            bad(&call(&mut c, read, &wrong, owner), "selected Lead changed");
            let mut wrong = full.clone();
            wrong["room"] = json!("another-room");
            assert_eq!(call(&mut c, read, &wrong, owner)["result"]["isError"], true);
            std::fs::write(&artifact, "## Output\nchanged answer\n## Exit\n0\n").unwrap();
            bad(&call(&mut c, read, &full, owner), "changed");
            std::fs::write(&artifact, format!("## Output\n{long}\n## Exit\n0\n")).unwrap();
            // Even an equal answer in a replacement artifact invalidates an observed page.
            std::fs::rename(&artifact, child.join("retired-answer")).unwrap();
            std::fs::write(&artifact, format!("## Output\n{long}\n## Exit\n0\n")).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600)).unwrap();
            bad(&call(&mut c, read, &full, owner), "changed");

            // Secrets or machine-tier paths straddling an 8192-byte page boundary (both at the first
            // shareable page cut 8192 and beyond the preview at 40960) withhold every page and mark
            // latestTurn unavailable with outbound_withheld.
            let left = ["gh", "p_LeftHalf012345"].concat();
            let right = "RightHalf67890abcdef01";
            let straddling_first = format!(
                "{} {left}{right} {}",
                "a".repeat(8_192 - 1 - left.len()),
                "z".repeat(25_000)
            );
            let straddling = format!(
                "{} {left}{right} {}",
                "a".repeat(40_960 - 1 - left.len()),
                "z".repeat(25_000)
            );
            let machine_left = ["/ho", "me/"].concat();
            let machine_right = "runner/private-tree";
            let straddling_machine = format!(
                "{} {machine_left}{machine_right} {}",
                "a".repeat(8_192 - 1 - machine_left.len()),
                "z".repeat(25_000)
            );
            for tainted in [&straddling_first, &straddling, &straddling_machine] {
                std::fs::write(&artifact, format!("## Output\n{tainted}\n## Exit\n0\n")).unwrap();
                std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                for cursor in [json!(""), first_cursor.clone()] {
                    let mut req = full.clone();
                    req["answerPage"] = cursor;
                    let refused = call(&mut c, read, &req, owner);
                    bad(&refused, "withheld");
                    let wire = refused.to_string();
                    assert!(
                        !wire.contains(&left)
                            && !wire.contains(right)
                            && !wire.contains(&machine_left)
                            && !wire.contains(machine_right)
                            && !wire.contains(&"a".repeat(32))
                            && !wire.contains(&"z".repeat(32))
                    );
                }
                let status_row = payload(&call(&mut c, read, &args, owner));
                assert_eq!(status_row["leads"][0]["latestTurn"], Value::Null);
                assert_eq!(
                    status_row["leads"][0]["turnState"],
                    json!({"status":"unavailable","reason":"outbound_withheld"})
                );
            }

            // An answer exceeding MAX_ANSWER fails closed with invalid_answer_range, not outbound_withheld.
            let oversized = "b".repeat(8 * 1024 * 1024 + 64);
            std::fs::write(&artifact, format!("## Output\n{oversized}\n## Exit\n0\n")).unwrap();
            std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600)).unwrap();
            let oversized_row = payload(&call(&mut c, read, &args, owner));
            assert_eq!(oversized_row["leads"][0]["latestTurn"], Value::Null);
            assert_eq!(
                oversized_row["leads"][0]["turnState"],
                json!({"status":"unavailable","reason":"invalid_answer_range"})
            );
            let mut oversized_page = full.clone();
            oversized_page["answerPage"] = json!("");
            bad(
                &call(&mut c, read, &oversized_page, owner),
                "exceeds the size limit",
            );

            std::fs::write(&artifact, original).unwrap();
            std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600)).unwrap();

            // A secret straddling the 4000-byte Lead kickoff cut withholds leadConversation.
            let kickoff_receipt: Value =
                serde_json::from_slice(&std::fs::read(source.join("lead-fixture-codex")).unwrap())
                    .unwrap();
            let kickoff_artifact = repo
                .join(".oms/artifacts/mcp")
                .join(kickoff_receipt["operation"].as_str().unwrap())
                .join("answers/fixture.md");
            let kickoff_original = std::fs::read(&kickoff_artifact).unwrap();
            let kickoff_straddling = format!(
                "{} {left}{right} {}",
                "k".repeat(4000 - 1 - left.len()),
                "q".repeat(300)
            );
            std::fs::write(
                &kickoff_artifact,
                format!("## Output\n{kickoff_straddling}\n## Exit\n0\n"),
            )
            .unwrap();
            std::fs::set_permissions(&kickoff_artifact, std::fs::Permissions::from_mode(0o600))
                .unwrap();
            let refused_kickoff = call(
                &mut c,
                read,
                &json!({"room":"lead-room","provider":"codex"}),
                owner,
            );
            bad(&refused_kickoff, "withheld");
            let kickoff_wire = refused_kickoff.to_string();
            assert!(!kickoff_wire.contains(&left) && !kickoff_wire.contains(right));
            std::fs::write(&kickoff_artifact, kickoff_original).unwrap();
            std::fs::set_permissions(&kickoff_artifact, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let reset = change_to(
            seat,
            &payload(&changed)["revision"],
            Value::Null,
            Value::Null,
        );
        let restored = call(&mut c, change, &reset, owner);
        good(&restored);
        if seat != 0 {
            let observed = &restored["result"]["_meta"]["leadSettings"]["nativeSession"];
            assert_eq!(observed["id"], "a2000000-0000-4000-8000-000000000002");
            let parent: Value = serde_json::from_slice(
                &std::fs::read(source.join(format!(
                    "lead-fixture-{}",
                    if seat == 1 { "claude" } else { "agy" }
                )))
                .unwrap(),
            )
            .unwrap();
            let parent = parent["operation"].as_str().unwrap();
            let child = std::fs::read_dir(repo.join(".oms/artifacts/mcp"))
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .find(|p| {
                    let meta: Value =
                        serde_json::from_slice(&std::fs::read(p.join("meta.json")).unwrap())
                            .unwrap();
                    meta["thread"] == parent && meta["lead_followup"] == true
                })
                .unwrap();
            std::fs::remove_file(child.join("answers/native-lead-observation.json")).unwrap();
            let latest = call(&mut c, preferences, &settings_address(seat), owner);
            good(&latest);
            let latest = &latest["result"]["_meta"]["leadSettings"];
            assert!(
                latest["nativeSession"].is_null() && latest["lastServed"].is_null(),
                "missing child proof must not display the valid kickoff observation"
            );
        }
        assert_ne!(
            payload(&restored)["revision"],
            public["revision"],
            "reset cannot revive a stale revision"
        );
        // Replaying the turn after reset must re-observe its original route.
        good(&call(&mut c, send, &m, owner));
        assert_eq!(
            runs(),
            4 + seat,
            "exact replay or status relaunched a provider"
        );
        let mut changed = m.clone();
        changed["text"] = json!("another body");
        bad(&call(&mut c, send, &changed, owner), "different text");
    }
    let refused = message(1, 10);
    let launch = dir.join("launch.json");
    let saved = std::fs::read(&launch).unwrap();
    std::fs::write(&launch, "{}").unwrap();
    let no_launch = call(&mut c, send, &refused, owner);
    good(&no_launch);
    assert_eq!(payload(&no_launch)["turn"]["status"], "failed");
    std::fs::write(&launch, saved).unwrap();
    good(&call(&mut c, send, &refused, owner));
    assert_eq!(runs(), 6);
    for n in 11..=13 {
        let m = message(1, n);
        good(&call(&mut c, send, &m, owner));
        let page = await_reply(&mut c, &m);
        if n == 13 {
            assert_eq!(page["hasMore"], true);
            let older = call(
                &mut c,
                read,
                &json!({"room":"lead-room","provider":"claude","before":page["cursor"]}),
                owner,
            );
            good(&older);
            let page = &older["result"]["_meta"]["leadConversation"]["transcript"];
            assert_eq!(page["hasMore"], false);
            assert_eq!(page["turns"][0]["text"], "fixture answer");
            assert_eq!(page["turns"][1]["turnId"], message(1, 2)["turnId"]);
            let foreign = call(
                &mut c,
                read,
                &json!({"room":"lead-room","provider":"agy","before":message(1,2)["turnId"]}),
                owner,
            );
            bad(&foreign, "Invalid Lead conversation page");
        }
    }
    assert_eq!(runs(), 9);
    // A partial, wrongly bound child cannot confirm delivery or license a relaunch.
    let damaged = std::fs::read_dir(repo.join(".oms/artifacts/mcp"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            std::fs::read_to_string(p.join("prompt.txt"))
                .ok()
                .as_deref()
                == Some("Private fixture follow-up 13\n")
        })
        .unwrap();
    std::fs::write(damaged.join("meta.json"), "{}").unwrap();
    let uncertain = message(1, 13);
    assert_eq!(
        payload(&lookup(&mut c, &uncertain))["turn"]["status"],
        "unconfirmed"
    );
    good(&call(&mut c, send, &uncertain, owner));
    bad(&call(&mut c, send, &message(1, 14), owner), "unresolved");
    assert_eq!(runs(), 9);
}

fn lead_turn_failure_degrades_per_row() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    std::fs::create_dir(repo.join(".oms")).unwrap();
    fixture_settings(&source, json!({"leadRoom":true}));
    let mut c = Client::with_depth(&source, &repo, "chatgpt-panel", false, false, "0".as_ref());
    let read = "oms_lead_conversation_status";
    let open = "oms_open_lead_conversations";
    let send = "oms_send_lead_message";
    let owner = "01900000-0000-7000-8000-0000000000bb";
    let args = json!({"room":"lead-room"});
    let call = |c: &mut Client, name: &str, params: &Value| {
        c.call(
            "tools/call",
            json!({"name":name,"arguments":params,"_meta":{"threadId":owner}}),
        )
    };
    let initial = payload(&call(&mut c, open, &args));
    let key = initial["projectKey"].as_str().unwrap().to_owned();
    until(|| {
        std::fs::read_dir(repo.join(".oms/artifacts/mcp"))
            .unwrap()
            .all(|e| e.unwrap().path().join("status").exists())
    });
    let ready = payload(&call(&mut c, open, &args));
    assert!(
        ready["leads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "ready")
    );
    let send_turn = |c: &mut Client, seat: usize, n: usize| {
        let turn_id = format!("20000000-0000-4000-8000-{n:012x}");
        let m = json!({
            "room":"lead-room",
            "projectKey":key,
            "provider":ready["leads"][seat]["provider"],
            "leadRequestId":ready["leads"][seat]["requestId"],
            "turnId":turn_id,
            "text":format!("Check seat {n}"),
        });
        good(&call(c, send, &m));
        until(|| {
            let r = call(
                c,
                read,
                &json!({"room":"lead-room","provider":m["provider"],"turnId":m["turnId"]}),
            );
            payload(&r)["turn"]["status"] == "replied"
        });
        turn_id
    };
    let codex_turn = send_turn(&mut c, 0, 1);
    let _claude_turn = send_turn(&mut c, 1, 2);
    let claude_child = std::fs::read_dir(repo.join(".oms/artifacts/mcp"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            std::fs::read_to_string(p.join("prompt.txt"))
                .ok()
                .as_deref()
                == Some("Check seat 2\n")
        })
        .unwrap();
    let leaked = ["gh", "p_", &"9".repeat(36)].concat();
    let claude_artifact = claude_child.join("answers/fixture.md");
    std::fs::write(
        &claude_artifact,
        format!("## Output\nsecret {leaked}\n## Exit\n0\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&claude_artifact, std::fs::Permissions::from_mode(0o600)).unwrap();

    for tool in [read, open] {
        let res = call(&mut c, tool, &args);
        good(&res);
        let wire = res.to_string();
        assert!(!wire.contains(&leaked) && !wire.contains("withheld during"));
        let leads = payload(&res)["leads"].as_array().unwrap().clone();
        assert_eq!(leads.len(), 3);
        assert_eq!(leads[0]["provider"], "codex");
        assert_eq!(leads[0]["status"], "ready");
        assert_eq!(leads[0]["latestTurn"]["turnId"], codex_turn);
        assert_eq!(leads[0]["latestTurn"]["status"], "replied");
        assert!(leads[0].get("turnState").is_none());

        assert_eq!(leads[1]["provider"], "claude");
        assert_eq!(leads[1]["status"], "ready");
        assert_eq!(leads[1]["latestTurn"], Value::Null);
        assert_eq!(
            leads[1]["turnState"],
            json!({"status":"unavailable","reason":"outbound_withheld"})
        );

        assert_eq!(leads[2]["provider"], "agy");
        assert_eq!(leads[2]["status"], "ready");
        assert_eq!(leads[2]["latestTurn"], Value::Null);
        assert!(leads[2].get("turnState").is_none());
    }

    // A corrupt head record on Claude also degrades only Claude's turn row.
    let claude_receipt: Value =
        serde_json::from_slice(&std::fs::read(source.join("lead-fixture-claude")).unwrap())
            .unwrap();
    let claude_kickoff = repo
        .join(".oms/artifacts/mcp")
        .join(claude_receipt["operation"].as_str().unwrap());
    let head_path = claude_kickoff.join("lead-turn-head.json");
    std::fs::write(&head_path, "{corrupt").unwrap();
    std::fs::set_permissions(&head_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let corrupt_res = call(&mut c, read, &args);
    good(&corrupt_res);
    let leads = payload(&corrupt_res)["leads"].as_array().unwrap().clone();
    assert_eq!(leads[0]["latestTurn"]["turnId"], codex_turn);
    assert_eq!(leads[0]["latestTurn"]["status"], "replied");
    assert_eq!(leads[1]["status"], "ready");
    assert_eq!(leads[1]["latestTurn"], Value::Null);
    assert_eq!(
        leads[1]["turnState"],
        json!({"status":"unavailable","reason":"invalid_turn_record"})
    );
    assert_eq!(leads[2]["status"], "ready");
    assert_eq!(leads[2]["latestTurn"], Value::Null);
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
            "status":status,"operation":operation,"assignedAt":"2026-11-24T01:02:03Z"});
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
    fixture_settings(
        &source,
        json!({"exit": 2, "stderr": "room: room_inventory_incomplete"}),
    );
    bad(
        &call(
            &mut c,
            "oms_assign_provider",
            args.clone(),
            json!({"threadId":thread}),
        ),
        "room_inventory_incomplete:",
    );
    assert_eq!(runs(), 0, "incomplete ownership never launches a reviewer");
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
    let assigned = record_for("assigned", Some(false), Value::Null);
    let linked = record_for("started", None, json!(operation));
    let query = json!({"room":"r","requestId":req});
    let repair = {
        let mut a = args.clone();
        a["repairOnly"] = true.into();
        a
    };
    let host = json!({"threadId":thread});
    // A new client has only the Room/request after losing the launch reply.
    drop(c);
    let mut c = Client::with_depth(&source, &repo, "chatgpt-panel", false, false, "0".as_ref());
    fixture_settings(
        &source,
        json!({"room":{"desktop-assign-status":assigned,"desktop-assign":assigned,"desktop-assign-receipt":linked}}),
    );
    let opdir = repo.join(".oms/artifacts/mcp").join(&operation);
    let pidpath = opdir.join("pid");
    let pid = std::fs::read(&pidpath).unwrap();
    for invalid in [None, Some("0\n"), Some("-15\n"), Some("bad")] {
        if let Some(bytes) = invalid {
            std::fs::write(&pidpath, bytes).unwrap();
        } else {
            std::fs::remove_file(&pidpath).unwrap();
        }
        assert_eq!(
            payload(&call(
                &mut c,
                "oms_assign_result",
                query.clone(),
                host.clone()
            ))["status"],
            "assigned"
        );
        assert_eq!(
            payload(&call(
                &mut c,
                "oms_assign_provider",
                repair.clone(),
                host.clone()
            ))["status"],
            "unconfirmed"
        );
        assert_eq!(record(&source)["argv"][1], "desktop-assign-status");
    }
    std::fs::write(&pidpath, &pid).unwrap();
    let meta_path = opdir.join("meta.json");
    let metadata = std::fs::read_to_string(&meta_path).unwrap();
    let duplicate = metadata.replacen('{', "{\"thread\":\"foreign\",", 1);
    std::fs::write(&meta_path, duplicate).unwrap();
    assert_eq!(
        payload(&call(
            &mut c,
            "oms_assign_result",
            query.clone(),
            host.clone()
        ))["status"],
        "assigned"
    );
    std::fs::write(&meta_path, &metadata).unwrap();
    std::fs::remove_file(&pidpath).unwrap();
    std::os::unix::fs::symlink(opdir.join("status"), &pidpath).unwrap();
    assert_eq!(
        payload(&call(
            &mut c,
            "oms_assign_provider",
            repair.clone(),
            host.clone()
        ))["status"],
        "unconfirmed"
    );
    std::fs::remove_file(&pidpath).unwrap();
    std::fs::write(&pidpath, &pid).unwrap();
    let exitpath = opdir.join("status");
    let exit = std::fs::read(&exitpath).unwrap();
    // The supervisor writes code or -signal, including the final newline.
    std::fs::write(&exitpath, "-15\n").unwrap();
    let signalled = call(&mut c, "oms_assign_result", query.clone(), host.clone());
    let assignment = &signalled["result"]["_meta"]["assignment"];
    assert_eq!(assignment["status"], "recovered");
    assert_eq!(assignment["execution"], json!({"state":"done","exit":-15}));
    for invalid in [
        "broken\n", "-127\n", "256\n", "-0\n", "-015\n", "+0\n", "-15",
    ] {
        std::fs::write(&exitpath, invalid).unwrap();
        assert_eq!(
            payload(&call(
                &mut c,
                "oms_assign_result",
                query.clone(),
                host.clone()
            ))["status"],
            "assigned",
            "{invalid:?}"
        );
    }
    std::fs::write(&exitpath, exit).unwrap();
    // Discovery normally returns only the newest twenty; recovery is an exact lookup.
    let newer: Vec<_> = (0..21)
        .map(|i| {
            opdir
                .parent()
                .unwrap()
                .join(format!("ask-20991231T235959Z-{i:08x}"))
        })
        .collect();
    for d in &newer {
        std::fs::create_dir(d).unwrap();
    }
    let recovered = call(&mut c, "oms_assign_result", query.clone(), host.clone());
    assert_eq!(payload(&recovered)["status"], "recovered");
    assert_eq!(
        recovered["result"]["_meta"]["assignment"]["operation"],
        operation
    );
    assert!(payload(&recovered)["operation"].is_null());
    assert_ne!(record(&source)["argv"][1], "desktop-assign-receipt");
    // The same synthetic Room data for another conversation cannot locate an operation.
    assert_eq!(
        payload(&call(
            &mut c,
            "oms_assign_result",
            query.clone(),
            json!({"threadId":"01900000-0000-7000-8000-0000000000cc"})
        ))["status"],
        "assigned"
    );
    for d in newer {
        std::fs::remove_dir(d).unwrap();
    }
    let mut wrong_room = query.clone();
    wrong_room["room"] = json!("another");
    bad(
        &call(&mut c, "oms_assign_result", wrong_room, host.clone()),
        "unrecognized",
    );
    let mut wrong = repair.clone();
    wrong["title"] = "changed".into();
    bad(
        &call(&mut c, "oms_assign_provider", wrong, host.clone()),
        "does not match",
    );
    for original in [&repair, &args] {
        assert_eq!(
            payload(&call(
                &mut c,
                "oms_assign_provider",
                original.clone(),
                host.clone()
            ))["status"],
            "started"
        );
        assert_eq!(record(&source)["argv"][1], "desktop-assign-receipt");
    }
    fixture_settings(&source, json!({"room":{"desktop-assign-status":linked}}));
    assert_eq!(
        payload(&call(
            &mut c,
            "oms_assign_provider",
            repair.clone(),
            host.clone()
        ))["status"],
        "recorded"
    );
    assert_eq!(record(&source)["argv"][1], "desktop-assign-status");
    fixture_settings(
        &source,
        json!({"room":{"desktop-assign-status":json!({"request":req,"status":"not_found"}).to_string()}}),
    );
    bad(
        &call(&mut c, "oms_assign_provider", repair.clone(), host.clone()),
        "unrecognized",
    );
    assert_eq!(record(&source)["argv"][1], "desktop-assign-status");
    assert_eq!(runs(), 1, "read and repair never redispatch");
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

    // Whole-body scan before the 6000-char assignment preview cut: a secret straddling
    // char 6000 or a machine path in bytes omitted by storage::bounded withholds the preview.
    let assign_artifact = opdir.join("answers/fixture.md");
    let assign_original = std::fs::read(&assign_artifact).unwrap();
    let clean_long = format!("{}{}", "a".repeat(6000), "b".repeat(400));
    std::fs::write(
        &assign_artifact,
        format!("## Output\n{clean_long}\n## Exit\n0\n"),
    )
    .unwrap();
    let long_page = call(
        &mut c,
        "oms_panel_chat",
        json!({"room":"r","participant":format!("ask-{req}")}),
        json!({"threadId":thread}),
    );
    good(&long_page);
    assert_eq!(
        long_page["result"]["_meta"]["chat"]["result"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        6000
    );
    assert_eq!(
        long_page["result"]["_meta"]["chat"]["result"]["truncated"],
        true
    );

    let left = ["gh", "p_LeftHalf012345"].concat();
    let right = "RightHalf67890abcdef01";
    let straddling = format!(
        "{} {left}{right} {}",
        "a".repeat(6000 - 1 - left.chars().count()),
        "z".repeat(400)
    );
    std::fs::write(
        &assign_artifact,
        format!("## Output\n{straddling}\n## Exit\n0\n"),
    )
    .unwrap();
    let withheld_cut = call(
        &mut c,
        "oms_panel_chat",
        json!({"room":"r","participant":format!("ask-{req}")}),
        json!({"threadId":thread}),
    );
    bad(
        &withheld_cut,
        "OMS withheld this conversation during its outbound scan.",
    );
    let cut_wire = withheld_cut.to_string();
    assert!(
        !cut_wire.contains(&left)
            && !cut_wire.contains(right)
            && !cut_wire.contains(&"a".repeat(32))
            && !cut_wire.contains(&"z".repeat(32))
    );

    let machine = ["/", "ho", "me/fixture_account/work"].concat();
    let omitted_middle = format!("{} {machine} {}", "m".repeat(45_000), "n".repeat(25_000));
    std::fs::write(
        &assign_artifact,
        format!("## Output\n{omitted_middle}\n## Exit\n0\n"),
    )
    .unwrap();
    let withheld_omitted = call(
        &mut c,
        "oms_panel_chat",
        json!({"room":"r","participant":format!("ask-{req}")}),
        json!({"threadId":thread}),
    );
    bad(
        &withheld_omitted,
        "OMS withheld this conversation during its outbound scan.",
    );
    assert!(!withheld_omitted.to_string().contains(&machine));
    std::fs::write(&assign_artifact, &assign_original).unwrap();

    // Fallback transcript turns also scan the whole joined body before cutting 6000 chars.
    let role_prefix = "claude: ";
    let turn_straddling = format!(
        "{} {left}{right} {}",
        "t".repeat(6000 - role_prefix.chars().count() - 1 - left.chars().count()),
        "u".repeat(200)
    );
    let straddling_updates = json!({
        "schema":1,
        "thread":operation,
        "turns":[{"role":"answer","provider":"claude","quality":"ok","text":turn_straddling}],
        "cursor":"c1",
        "has_more":false
    });
    std::fs::write(&exitpath, "7\n").unwrap();
    fixture_settings(
        &source,
        json!({"stdout":chat.to_string(),"updates":straddling_updates.to_string(),"room":{"desktop-assign-status":record_for("started",None,json!(operation))}}),
    );
    let withheld_turns = call(
        &mut c,
        "oms_panel_chat",
        json!({"room":"r","participant":format!("ask-{req}")}),
        json!({"threadId":thread}),
    );
    bad(
        &withheld_turns,
        "OMS withheld this conversation during its outbound scan.",
    );
    let turns_wire = withheld_turns.to_string();
    assert!(!turns_wire.contains(&left) && !turns_wire.contains(right));
    std::fs::write(&exitpath, "0\n").unwrap();
    fixture_settings(
        &source,
        json!({"stdout":chat.to_string(),"updates":updates.to_string(),"room":{"desktop-assign-status":record_for("started",None,json!(operation))}}),
    );

    // Non-oversize answer artifact failures (missing, incomplete, hard-linked or symlinked
    // `answer_errors`) fall back to the transcript turns instead of returning LIMIT_ERR.
    let assert_transcript_fallback = |c: &mut Client| {
        let fallback = call(
            c,
            "oms_panel_chat",
            json!({"room":"r","participant":format!("ask-{req}")}),
            json!({"threadId":thread}),
        );
        good(&fallback);
        assert_eq!(
            fallback["result"]["_meta"]["chat"]["result"]["text"],
            "claude: callers found"
        );
    };
    let write_private_assign = |bytes: &[u8]| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&assign_artifact, bytes).unwrap();
        std::fs::set_permissions(&assign_artifact, std::fs::Permissions::from_mode(0o600)).unwrap();
    };

    std::fs::remove_file(&assign_artifact).unwrap();
    assert_transcript_fallback(&mut c);

    write_private_assign(b"## Output\nchanged without exit\n");
    assert_transcript_fallback(&mut c);

    write_private_assign(&assign_original);
    let linked_copy = opdir.join("answers/changed-link.md");
    std::fs::hard_link(&assign_artifact, &linked_copy).unwrap();
    assert_transcript_fallback(&mut c);
    std::fs::remove_file(&linked_copy).unwrap();

    std::fs::remove_file(&assign_artifact).unwrap();
    std::os::unix::fs::symlink(opdir.join("status"), &assign_artifact).unwrap();
    assert_transcript_fallback(&mut c);
    std::fs::remove_file(&assign_artifact).unwrap();

    // Confirmed oversize answers still fail closed with LIMIT_ERR rather than falling back.
    for extra in [64usize, 65 * 1024] {
        let oversized_assign = "b".repeat(8 * 1024 * 1024 + extra);
        write_private_assign(format!("## Output\n{oversized_assign}\n## Exit\n0\n").as_bytes());
        let oversized_chat = call(
            &mut c,
            "oms_panel_chat",
            json!({"room":"r","participant":format!("ask-{req}")}),
            json!({"threadId":thread}),
        );
        bad(
            &oversized_chat,
            "OMS desktop detail exceeds the response limit.",
        );
        let wire = oversized_chat.to_string();
        assert!(!wire.contains("callers found") && !wire.contains(&"b".repeat(32)));
    }
    write_private_assign(&assign_original);
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
    // A pre-upgrade receipt still addresses its recorded random operation.
    let legacy = "ask-20250501T010203Z-abcd1234";
    let legacy_dir = opdir.with_file_name(legacy);
    std::fs::rename(&opdir, &legacy_dir).unwrap();
    let mut old_meta: Value = serde_json::from_str(&metadata).unwrap();
    old_meta["operation"] = legacy.into();
    old_meta["thread"] = legacy.into();
    std::fs::write(legacy_dir.join("meta.json"), old_meta.to_string()).unwrap();
    let mut old_record: Value =
        serde_json::from_str(&record_for("started", None, json!(legacy))).unwrap();
    old_record.as_object_mut().unwrap().remove("assignedAt");
    fixture_settings(
        &source,
        json!({"room":{"desktop-assign-status":old_record.to_string()}}),
    );
    let old = call(&mut c, "oms_assign_result", query.clone(), host.clone());
    assert_eq!(old["result"]["_meta"]["assignment"]["operation"], legacy);
    assert_eq!(payload(&old)["status"], "started");
    fixture_settings(&source, json!({"room":{"desktop-assign-status":assigned}}));
    assert_eq!(
        payload(&call(&mut c, "oms_assign_result", query, host))["status"],
        "assigned"
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
    std::fs::create_dir(t.0.join(".git")).unwrap();
    let source = t.source();
    let repo = t.repo();
    let mut c = Client::with_depth(&source, &repo, "chatgpt-panel", true, false, "0".as_ref());
    let tools = c.call("tools/list", json!({}))["result"]["tools"].clone();
    assert_eq!(tools.as_array().unwrap().len(), 25);
    assert_eq!(tools[0]["name"], "oms_open_panel");
    assert_eq!(
        tools[0]["description"],
        "Open the OMS panel (expands beside or over the conversation when the host allows it)."
    );
    assert_eq!(
        tools[0]["_meta"]["openai/ui"]["entrypoints"],
        json!([{"type":"thread"}])
    );
    let uri = &tools[0]["_meta"]["ui"]["resourceUri"];
    assert_eq!(uri, "ui://oms/conversation-panel/v32.html");
    for name in [
        "oms_panel_usage",
        "oms_panel_project_graph",
        "oms_panel_tui_view",
    ] {
        let read = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        assert_eq!(read["_meta"]["ui"]["visibility"], json!(["app", "model"]));
        assert_eq!(read["annotations"]["readOnlyHint"], true);
        assert_eq!(read["annotations"]["openWorldHint"], false);
    }
    // Codex uses the stdio transport configuration to decide whether to reuse a running server.
    for package in [
        include_str!("../../../plugins/oms/mcp.json"),
        include_str!("../../../plugins/oms-ssh/oms/mcp.json"),
    ] {
        let manifest: Value = serde_json::from_str(package).unwrap();
        let revision = manifest["mcpServers"]["oms-panel"]["env"]["OMS_MCP_PANEL_RESOURCE_VERSION"]
            .as_str()
            .unwrap();
        assert_eq!(uri, &format!("ui://oms/conversation-panel/{revision}.html"));
    }
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
    let html = current["text"].as_str().unwrap();
    assert!(html.len() < 240 * 1024);
    assert_eq!(
        html.as_bytes(),
        include_bytes!("../src/conversation-panel.html")
    );
    for marker in [r#"id="header-main""#, r#"id="leads""#, "nativeMainUri"] {
        assert!(html.contains(marker));
    }
    assert!(html.contains("ui/request-display-mode"));
    assert!(html.contains("availableDisplayModes:['inline','fullscreen','pip']"));
    assert_eq!(current["mimeType"], "text/html;profile=mcp-app");
    for historical_uri in [
        json!("ui://oms/conversation-panel/v1.html"),
        json!("ui://oms/conversation-panel/v2.html"),
        json!("ui://oms/conversation-panel/v3.html"),
        json!("ui://oms/conversation-panel/v4.html"),
        json!("ui://oms/conversation-panel/v5.html"),
        json!("ui://oms/conversation-panel/v6.html"),
        json!("ui://oms/conversation-panel/v7.html"),
        json!("ui://oms/conversation-panel/v8.html"),
        json!("ui://oms/conversation-panel/v9.html"),
        json!("ui://oms/conversation-panel/v10.html"),
        json!("ui://oms/conversation-panel/v11.html"),
        json!("ui://oms/conversation-panel/v12.html"),
        json!("ui://oms/conversation-panel/v13.html"),
        json!("ui://oms/conversation-panel/v14.html"),
        json!("ui://oms/conversation-panel/v15.html"),
        json!("ui://oms/conversation-panel/v16.html"),
        json!("ui://oms/conversation-panel/v17.html"),
        json!("ui://oms/conversation-panel/v18.html"),
        json!("ui://oms/conversation-panel/v19.html"),
        json!("ui://oms/conversation-panel/v20.html"),
        json!("ui://oms/conversation-panel/v21.html"),
        json!("ui://oms/conversation-panel/v22.html"),
        json!("ui://oms/conversation-panel/v23.html"),
        json!("ui://oms/conversation-panel/v24.html"),
        json!("ui://oms/conversation-panel/v26.html"),
        json!("ui://oms/conversation-panel/v25.html"),
        json!("ui://oms/conversation-panel/v27.html"),
        json!("ui://oms/conversation-panel/v28.html"),
        json!("ui://oms/conversation-panel/v29.html"),
        json!("ui://oms/conversation-panel/v30.html"),
        json!("ui://oms/conversation-panel/v31.html"),
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
    let unbound_open = c.tool("oms_open_panel", json!({}));
    good(&unbound_open);
    assert!(unbound_open["result"]["_meta"].get("dashboard").is_none());
    assert_eq!(
        unbound_open["result"]["structuredContent"],
        json!({"panel":"OMS","project":null,"snapshotOmitted":"project"})
    );
    assert_eq!(
        payload(&unbound_open),
        json!({"panel":"OMS","project":null,"snapshotOmitted":"project"})
    );
    assert!(c.tool("oms_peer_start", json!({}))["error"].is_object());
    assert!(c.tool("oms_panel_snapshot", json!({"repo":source}))["error"].is_object());
    assert!(
        c.call("tasks/cancel", json!({"_meta":modern(),"taskId":"absent"}))["error"].is_object()
    );
    bad(&c.tool("oms_panel_snapshot", json!({})), "OMS_PANEL_REPO");
    assert_eq!(
        registry_summary(&c.tool("oms_list_projects", json!({}))),
        json!({"projects":[],"binding":"unbound","readOnly":false})
    );
    assert!(!source.join("last-call.json").exists());
    std::fs::create_dir(repo.join(".oms")).unwrap();
    assert_eq!(
        registry_summary(&c.tool("oms_list_projects", json!({}))),
        json!({"projects":[],"binding":"cwd","readOnly":false})
    );
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
    let opened = c.tool("oms_open_panel", json!({"leadConversations":true}));
    good(&opened);
    assert_eq!(opened["result"]["_meta"]["dashboard"], *data);
    assert!(
        opened["result"]["structuredContent"]
            .get("dashboard")
            .is_none()
    );
    assert!(payload(&opened).get("dashboard").is_none());
    assert_eq!(
        opened["result"]["structuredContent"],
        json!({"panel":"OMS","project":null,"leadConversations":true,"snapshot":"attached"})
    );
    assert!(
        !opened["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Finish inspection")
    );
    assert_eq!(
        payload(&opened),
        json!({"panel":"OMS","project":null,"leadConversations":true,"snapshot":"attached"})
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
    let snapshot_pid = record(&source)["pid"].clone();
    let busy_open = c.tool("oms_open_panel", json!({}));
    good(&busy_open);
    assert!(busy_open["result"]["_meta"].get("dashboard").is_none());
    assert_eq!(
        busy_open["result"]["structuredContent"],
        json!({"panel":"OMS","project":null,"snapshotOmitted":"busy"})
    );
    assert_eq!(
        payload(&busy_open),
        json!({"panel":"OMS","project":null,"snapshotOmitted":"busy"})
    );
    assert_eq!(record(&source)["pid"], snapshot_pid);
    for id in 9002..=9009 {
        send(&mut c, id, "oms_list_approvals");
    }
    send(&mut c, 9010, "oms_list_approvals");
    let refused = c.read();
    assert_eq!(refused["id"], 9010);
    assert_eq!(refused["error"]["code"], -32000);
    let full_open = c.tool("oms_open_panel", json!({}));
    good(&full_open);
    assert!(full_open["result"]["_meta"].get("dashboard").is_none());
    assert_eq!(
        full_open["result"]["structuredContent"],
        json!({"panel":"OMS","project":null,"snapshotOmitted":"busy"})
    );
    assert_eq!(
        payload(&full_open),
        json!({"panel":"OMS","project":null,"snapshotOmitted":"busy"})
    );
    assert_eq!(read(&mut c, uri)["text"], current["text"]);
    assert_eq!(record(&source)["argv"][0], "dashboard");
    assert_eq!(record(&source)["pid"], snapshot_pid);
    std::fs::write(source.join("snapshot-release"), "").unwrap();
    let ready = c.read();
    assert_eq!(ready["id"], 9001);
    assert_eq!(payload(&ready)["dashboard"]["project"], "demo");
    for id in 9002..=9009 {
        let approvals = c.read();
        assert_eq!(approvals["id"], id);
        assert_eq!(payload(&approvals)["approvals"], json!([]));
    }
    let idle_open = c.tool("oms_open_panel", json!({}));
    good(&idle_open);
    assert!(
        idle_open["result"]["structuredContent"]
            .get("dashboard")
            .is_none()
    );
    assert!(payload(&idle_open).get("dashboard").is_none());
    assert!(
        !idle_open["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("dashboard")
    );
    assert_eq!(idle_open["result"]["_meta"]["dashboard"]["project"], "demo");
    assert_eq!(
        idle_open["result"]["structuredContent"],
        json!({"panel":"OMS","project":null,"snapshot":"attached"})
    );
    assert_eq!(
        payload(&idle_open),
        json!({"panel":"OMS","project":null,"snapshot":"attached"})
    );
    assert_eq!(record(&source)["argv"][0], "dashboard");
    assert_ne!(record(&source)["pid"], snapshot_pid);
    // An idle OPEN running on the runtime worker also counts as in-flight: a concurrent OPEN or
    // resources/read returns immediately on the input thread without spawning a second dashboard.
    std::fs::remove_file(source.join("snapshot-ready")).unwrap();
    std::fs::remove_file(source.join("snapshot-release")).unwrap();
    send(&mut c, 9020, "oms_open_panel");
    until(|| source.join("snapshot-ready").exists());
    let open_pid = record(&source)["pid"].clone();
    assert_eq!(read(&mut c, uri)["text"], current["text"]);
    let concurrent_open = c.tool("oms_open_panel", json!({}));
    good(&concurrent_open);
    assert!(
        concurrent_open["result"]["_meta"]
            .get("dashboard")
            .is_none()
    );
    assert_eq!(
        concurrent_open["result"]["structuredContent"],
        json!({"panel":"OMS","project":null,"snapshotOmitted":"busy"})
    );
    assert_eq!(
        payload(&concurrent_open),
        json!({"panel":"OMS","project":null,"snapshotOmitted":"busy"})
    );
    assert_eq!(record(&source)["pid"], open_pid);
    std::fs::write(source.join("snapshot-release"), "").unwrap();
    let gated_open = c.read();
    assert_eq!(gated_open["id"], 9020);
    good(&gated_open);
    assert!(
        gated_open["result"]["structuredContent"]
            .get("dashboard")
            .is_none()
    );
    assert!(payload(&gated_open).get("dashboard").is_none());
    assert_eq!(
        gated_open["result"]["_meta"]["dashboard"]["project"],
        "demo"
    );
    assert_eq!(
        gated_open["result"]["structuredContent"],
        json!({"panel":"OMS","project":null,"snapshot":"attached"})
    );
    assert_eq!(
        payload(&gated_open),
        json!({"panel":"OMS","project":null,"snapshot":"attached"})
    );
    let single_open = Command::new(std::env::current_exe().unwrap())
        .arg("--single-open")
        .arg(&source)
        .arg(&repo)
        .env_remove("OMS_PANEL_REPO")
        .env_remove("OMS_PANEL_PROJECTS")
        .env_remove("OMS_ROOM_PARTICIPANT")
        .env_remove("OMS_PANEL_PROJECTS_FILE")
        .env("XDG_CONFIG_HOME", source.join("test-config"))
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .output()
        .unwrap();
    assert!(single_open.status.success(), "{single_open:?}");
    let single_reply: Value = serde_json::from_slice(&single_open.stdout).unwrap();
    good(&single_reply);
    assert!(
        single_reply["result"]["structuredContent"]
            .get("dashboard")
            .is_none()
    );
    assert!(payload(&single_reply).get("dashboard").is_none());
    assert_eq!(
        single_reply["result"]["_meta"]["dashboard"]["project"],
        "demo"
    );
    assert_eq!(
        single_reply["result"]["structuredContent"],
        json!({"panel":"OMS","project":null,"snapshot":"attached"})
    );
    assert_eq!(
        payload(&single_reply),
        json!({"panel":"OMS","project":null,"snapshot":"attached"})
    );
    fixture_settings(&source, json!({"exit":1,"stdout":snapshot.to_string()}));
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = source.join("test-config/oh-my-setting");
        std::fs::create_dir_all(&dir).unwrap();
        let map = dir.join("desktop-projects.json");
        std::fs::write(&map, json!({"demo":repo}).to_string()).unwrap();
        std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o600)).unwrap();
        let unaliased = c.tool("oms_open_panel", json!({}));
        good(&unaliased);
        assert!(unaliased["result"]["_meta"].get("dashboard").is_none());
        assert_eq!(
            unaliased["result"]["structuredContent"],
            json!({"panel":"OMS","project":null,"snapshotOmitted":"project"})
        );
        assert_eq!(
            payload(&unaliased),
            json!({"panel":"OMS","project":null,"snapshotOmitted":"project"})
        );
        let aliased = c.tool("oms_open_panel", json!({"project":"demo"}));
        good(&aliased);
        assert_eq!(aliased["result"]["_meta"]["dashboard"]["project"], "demo");
        assert_eq!(
            aliased["result"]["structuredContent"],
            json!({"panel":"OMS","project":"demo","snapshot":"attached"})
        );
        assert_eq!(
            payload(&aliased),
            json!({"panel":"OMS","project":"demo","snapshot":"attached"})
        );
        std::fs::remove_file(&map).unwrap();
    }
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
    let mut response = detail.clone();
    response["messages"] = json!([]);
    response["evidence"] = json!({"attempt":"bound-attempt","section":"transcript","page":1,
        "source":"indexed-answer","text":"한글","lines":["한글"],"offset":0,"bytes":6,"totalBytes":12,
        "digest":"a".repeat(64),"revision":"b".repeat(64),"nextPage":2,"hasMore":true,"truncated":false});
    fixture_settings(&source, json!({"stdout":response.to_string()}));
    let project_key = payload(&c.tool("oms_list_projects", json!({})))["projectKey"].clone();
    assert_eq!(project_key.as_str().unwrap().len(), 64);
    let mut full = json!({"room":"fixture","participant":"owner","section":"transcript","attempt":"bound-attempt","evidenceCursor":""});
    bad(&evidence(&mut c, full.clone()), "observed project key");
    full["projectKey"] = json!("0".repeat(64));
    bad(&evidence(&mut c, full.clone()), "observed project key");
    full["projectKey"] = project_key.clone();
    let r = evidence(&mut c, full.clone());
    good(&r);
    assert_eq!(r["result"]["structuredContent"]["projectKey"], project_key);
    assert_eq!(r["result"]["_meta"]["chat"]["projectKey"], project_key);
    full["evidenceCursor"] = r["result"]["_meta"]["chat"]["evidence"]["nextCursor"].clone();
    assert!(full["evidenceCursor"].is_string());
    for field in ["content", "structuredContent"] {
        assert!(!r["result"][field].to_string().contains("한글"));
    }
    response["evidence"]["page"] = json!(2);
    response["evidence"]["offset"] = json!(6);
    response["evidence"]["text"] = json!("결과");
    response["evidence"]["hasMore"] = json!(false);
    response["evidence"]["nextPage"] = Value::Null;
    fixture_settings(&source, json!({"stdout":response.to_string()}));
    let r = evidence(&mut c, full.clone());
    good(&r);
    assert_eq!(r["result"]["_meta"]["chat"]["evidence"]["text"], "결과");
    let mut foreign = full.clone();
    foreign["participant"] = json!("other");
    response["participant"] = json!("other");
    fixture_settings(&source, json!({"stdout":response.to_string()}));
    bad(&evidence(&mut c, foreign), "another selection");
    response["participant"] = json!("owner");
    response["evidence"]["revision"] = json!("c".repeat(64));
    fixture_settings(&source, json!({"stdout":response.to_string()}));
    bad(&evidence(&mut c, full.clone()), "changed");
    full["page"] = json!(2);
    bad(&evidence(&mut c, full), "returned cursor");

    // Room transcript/diff full paging withholds every page when a secret straddles byte 8192.
    let clean_evidence = format!("{}{}", "a".repeat(8_190), "z".repeat(400));
    fixture_settings(
        &source,
        json!({"pagedChatEvidence":{"base":detail,"attempt":"bound-attempt","section":"transcript","body":clean_evidence}}),
    );
    let mut ev_req = json!({"room":"fixture","participant":"owner","section":"transcript","attempt":"bound-attempt","projectKey":project_key,"evidenceCursor":""});
    let ev_p1 = evidence(&mut c, ev_req.clone());
    good(&ev_p1);
    let ev_c2 = ev_p1["result"]["_meta"]["chat"]["evidence"]["nextCursor"].clone();
    assert!(ev_c2.is_string());

    let left = ["gh", "p_LeftHalf012345"].concat();
    let right = "RightHalf67890abcdef01";
    let straddling_evidence = format!(
        "{} {left}{right} {}",
        "a".repeat(8_192 - 1 - left.len()),
        "z".repeat(360)
    );
    fixture_settings(
        &source,
        json!({"pagedChatEvidence":{"base":detail,"attempt":"bound-attempt","section":"transcript","body":straddling_evidence}}),
    );
    for cursor in [json!(""), ev_c2] {
        ev_req["evidenceCursor"] = cursor;
        let refused = evidence(&mut c, ev_req.clone());
        bad(&refused, "withheld");
        let wire = refused.to_string();
        assert!(
            !wire.contains(&left)
                && !wire.contains(right)
                && !wire.contains(&"a".repeat(32))
                && !wire.contains(&"z".repeat(32))
        );
    }

    fixture_settings(&source, json!({"stdout":detail.to_string()}));
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
    let cand_fixture = json!({
        "git": {},
        "github": {
            "repository": "example-org/example-repo",
            "observedAt": "2026-11-26T00:00:00Z",
            "candidates": {
                "status": "available",
                "source": "explicit_project",
                "observed_at": "2026-11-26T00:00:00Z",
                "items": [{"rank": 1, "issue": "example-org/example-repo#447", "number": 447}],
                "recommendations": [{"issue": "example-org/example-repo#447", "number": 447, "class": "candidate", "state": "recommended", "reason": "ready_with_priority"}],
                "incomplete": false,
                "reason": null
            }
        }
    });
    fixture_settings(&source, json!({"stdout":cand_fixture.to_string()}));
    let cand_reply = c.tool(
        "oms_panel_git_status",
        json!({"boardId":"PVT_fixture","candidates":true}),
    );
    assert_eq!(
        cand_reply["result"]["structuredContent"]["github"]["candidates"]["source"],
        "explicit_project"
    );
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
            "--candidates"
        ])
    );
    let wide_title = "🧪".repeat(120);
    let wide_items: Vec<_> = (1..=16)
        .map(|n| json!({"rank": n, "issue": format!("example-org/example-repo#{n}"), "number": n, "title": wide_title}))
        .collect();
    let wide_cand = json!({"git": {}, "github": {"candidates": {"status": "available", "source": "explicit_project", "observed_at": "2026-11-26T00:00:00Z", "items": wide_items, "incomplete": true, "reason": "result_limit"}}});
    fixture_settings(&source, json!({"stdout":wide_cand.to_string()}));
    let wide_reply = c.tool(
        "oms_panel_git_status",
        json!({"boardId":"PVT_fixture","candidates":true}),
    );
    assert_eq!(wide_reply["result"]["isError"], false);
    assert_eq!(
        wide_reply["result"]["structuredContent"]["github"]["candidates"]["items"]
            .as_array()
            .unwrap()
            .len(),
        16
    );
    assert!(wide_reply.to_string().len() < 60_000);
    for args in [
        json!({"issuePreview":0}),
        json!({"issuePreview":10000000000i64}),
        json!({"issuePreview":"9"}),
        json!({"issuePreview":9,"cursor":null}),
        json!({"issuePreview":9,"candidates":true}),
        json!({"issuePreview":9,"kind":"issue","number":9}),
        json!({"issuePreview":9,"inventory":"issues"}),
        json!({"issuePreview":9,"boardId":"PVT_1"}),
        json!({"kind":"issue"}),
        json!({"number":2}),
        json!({"kind":"pr","number":2}),
        json!({"kind":"issue","number":"--repo"}),
        json!({"kind":"issue","number":10000000000i64}),
        json!({"inventory":"other"}),
        json!({"boardId":"https://example.invalid/x"}),
        json!({"boardId":"PVT_1","inventory":"issues"}),
        json!({"boardId":"PVT_1","kind":"issue","number":1}),
        json!({"boardId":"PVT_1","candidates":"yes"}),
        json!({"boardId":"PVT_1","candidates":true,"cursor":"abc.foo="}),
        json!({"boardId":"PVT_1","candidates":true,"inventory":"issues"}),
        json!({"boardId":"PVT_1","candidates":true,"kind":"issue","number":1}),
        json!({"candidates":true}),
        json!({"candidates":false}),
        json!({"cursor":"abc"}),
        json!({"inventory":"issues","cursor":"x y"}),
        json!({"inventory":"issues","kind":"issue","number":1}),
    ] {
        assert_eq!(
            c.tool("oms_panel_git_status", args)["result"]["isError"],
            true
        );
    }
    let panel_graph = json!({
        "schema": 1,
        "kind": "oms-panel-project-graph",
        "status": "ready",
        "completeness": "complete",
        "provenance": {"source":"indexed-graph","mode":"indexed","indexState":"present","verifiedRelationships":true,"readOnly":true,"wroteIndex":false},
        "scope": {"repository":"demo","focusNode":"dir:src","query":null,"includeTests":true,"depth":1,"limit":20},
        "limits": {"maxNodes":20,"maxEdges":180,"truncated":false},
        "counts": {"shown_nodes":2,"total_nodes":2,"shown_edges":1,"total_edges":1,"confidence":{"EXTRACTED":1}},
        "nodes": [
            {"id":"file:src/a.rs","label":"a.rs","kind":"file","path":"src/a.rs"},
            {"id":"file:src/b.rs","label":"b.rs","kind":"file","path":"src/b.rs"}
        ],
        "edges": [{"source":"file:src/a.rs","target":"file:src/b.rs","relation":"imports","confidence":"EXTRACTED"}],
        "selection": {"status":"found","requestedId":"dir:src","node":{"id":"dir:src","kind":"directory"},"breadcrumbs":[],"neighbors":[],"omittedNeighbors":0}
    });
    fixture_settings(&source, json!({"stdout":panel_graph.to_string()}));
    assert_eq!(
        payload(&c.tool(
            "oms_panel_project_graph",
            json!({"node":"dir:src","limit":20,"depth":1,"includeTests":true})
        )),
        panel_graph
    );
    assert_eq!(
        record(&source)["argv"],
        json!([
            "graph",
            "project",
            "panel",
            "--repo",
            repo,
            "--json",
            "--limit",
            "20",
            "--depth",
            "1",
            "--include-tests",
            "--node=dir:src"
        ])
    );
    good(&c.tool("oms_panel_project_graph", json!({"query":"panel"})));
    assert_eq!(
        record(&source)["argv"],
        json!([
            "graph",
            "project",
            "panel",
            "--repo",
            repo,
            "--json",
            "--limit",
            "40",
            "--depth",
            "2",
            "--query=panel"
        ])
    );
    assert_eq!(
        c.tool("oms_panel_project_graph", json!({"repo":source}))["error"]["code"],
        -32602
    );
    let before_bad_graph = record(&source);
    for args in [
        json!({"node":"dir:src","query":"src"}),
        json!({"node":"file:../etc/passwd"}),
        json!({"node":"file:/etc/passwd"}),
        json!({"node":"--help"}),
        json!({"query":"   "}),
        json!({"query":"bad\nquery"}),
        json!({"limit":0}),
        json!({"limit":61}),
        json!({"depth":-1}),
        json!({"depth":4}),
        json!({"includeTests":"yes"}),
    ] {
        assert_eq!(
            c.tool("oms_panel_project_graph", args.clone())["result"]["isError"],
            true,
            "expected error for {args}"
        );
    }
    assert_eq!(record(&source), before_bad_graph);
    // Oversized node count from backend is rejected by panel receipt validation.
    let mut oversize_graph = panel_graph.clone();
    oversize_graph["nodes"] = json!(vec![json!({"id":"file:a.rs"}); 61]);
    fixture_settings(&source, json!({"stdout":oversize_graph.to_string()}));
    bad(
        &c.tool("oms_panel_project_graph", json!({})),
        "unrecognized receipt",
    );
    // Outbound secret-shaped text is withheld before leaving the MCP server.
    let mut secret_graph = panel_graph.clone();
    secret_graph["nodes"][0]["label"] =
        json!(concat!("ghp_", "0123456789abcdef0123456789abcdef0123"));
    fixture_settings(&source, json!({"stdout":secret_graph.to_string()}));
    bad(
        &c.tool("oms_panel_project_graph", json!({})),
        "outbound scan",
    );
    // Invalid project repository stderr maps to a safe user-facing message.
    fixture_settings(
        &source,
        json!({"exit":2,"stdout":"","stderr":"graph: invalid project repository: expected .git\n"}),
    );
    bad(
        &c.tool("oms_panel_project_graph", json!({})),
        "not a valid OMS or Git repository",
    );
    let expected_main = json!({"source":"room-session","provider":"codex","harness":"pi","participant":"owner","room":"r","state":"joined","incomplete":false});
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
    for (field, value) in [
        ("harness", "foreign"),
        ("provider", "claude"),
        ("source", "other"),
        ("state", "left"),
        ("participant", ""),
        ("room", "../other"),
    ] {
        let mut forged = expected_main.clone();
        forged[field] = json!(value);
        fixture_settings(
            &source,
            json!({"stdout":snapshot.to_string(),"binding":{"conversationBinding":"matched","conversationMain":forged}}),
        );
        let result = c.call(
            "tools/call",
            json!({"name":"oms_panel_snapshot","arguments":{},"_meta":{"threadId":thread}}),
        );
        let report = payload(&result);
        assert!(report["dashboard"]["conversationMain"]["harness"].is_null());
        assert_eq!(report["dashboard"]["degraded"], true);
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
    bad(
        &worker.tool("oms_panel_project_graph", json!({})),
        "parent app",
    );
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

fn panel_command_catalog() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = Temp::new();
    let source = tmp.source();
    let repo = tmp.repo();
    std::fs::create_dir(repo.join(".oms")).unwrap();
    let name = "oms_panel_capabilities";
    let mut desktop =
        Client::with_depth(&source, &repo, "chatgpt-panel", false, false, "0".as_ref());
    let listing =
        "advise                     First description.\npeer-ask                   Ask a peer.\n";
    fixture_settings(&source, json!({"stdout":listing}));
    let first = desktop.tool(name, json!({}));
    let expected = json!({"schema":1,"total":2,"omitted":0,"commands":[{"name":"advise","description":"First description."},{"name":"peer-ask","description":"Ask a peer."}]});
    assert_eq!(payload(&first), expected);
    assert_eq!(first["result"]["structuredContent"], expected);
    assert_eq!(record(&source)["argv"], json!(["list", "--all"]));
    assert_eq!(record(&source)["cwd"], json!(repo));
    assert_eq!(
        payload(&desktop.tool(name, json!({"offset":2}))),
        json!({"schema":1,"total":2,"omitted":2,"commands":[]})
    );
    let before = std::fs::read(source.join("last-call.json")).unwrap();
    for field in [
        json!({"command":"update"}),
        json!({"path":repo}),
        json!({"repo":repo}),
    ] {
        assert_eq!(desktop.tool(name, field)["error"]["code"], -32602);
    }
    for offset in [
        json!(-1),
        json!(10001),
        json!("2"),
        json!(null),
        json!(true),
        json!(0.5),
    ] {
        bad(
            &desktop.tool(name, json!({"offset":offset})),
            "Invalid command catalog offset",
        );
    }
    let mut worker = Client::with_depth(&source, &repo, "chatgpt-panel", false, true, "1".as_ref());
    bad(&worker.tool(name, json!({})), "parent app");
    let mut generic = Client::with_depth(&source, &repo, "mcp-app", false, false, "0".as_ref());
    bad(&generic.tool(name, json!({})), "OMS_PANEL_REPO");
    let map = source.join("test-config/oh-my-setting/desktop-projects.json");
    std::fs::create_dir_all(map.parent().unwrap()).unwrap();
    std::fs::write(&map, json!({"approved":repo}).to_string()).unwrap();
    std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o600)).unwrap();
    for alias in [json!("unknown"), json!(repo), json!(null)] {
        bad(
            &generic.tool(name, json!({"project":alias})),
            "OMS_PANEL_REPO",
        );
    }
    assert_eq!(
        before,
        std::fs::read(source.join("last-call.json")).unwrap()
    );
    assert_eq!(
        payload(&generic.tool(name, json!({"project":"approved"}))),
        expected
    );
    for full in [false, true] {
        assert_eq!(
            Client::new(&source, &repo, full, false, false).tool(name, json!({}))["error"]["code"],
            -32602
        );
    }
    // Both desktop profiles read the same bounded rows via their own project binding.
    let legacy = json!({"harness":"pi","owner":"codex","transport":"CodexGateway","installed":false,
        "dependencies":"pi, codex, oms","dependenciesPresent":false,"resumePolicy":"unsupported",
        "accountSelection":false,"auth":null,"version":null,"observedAt":10});
    let mut current = legacy.clone();
    current["managedOutgoingSwap"] = json!("unsupported");
    current["swapEvidence"] = json!("no-managed-outgoing-adapter");
    current["configuredMainPermissions"] = json!("full");
    current["fullModeOption"] = json!("unsupported");
    current["resumeRefusalCode"] = json!("binding_unverified");
    for (client, args) in [
        (
            &mut desktop,
            json!({"view":"connections","project":"approved"}),
        ),
        (
            &mut generic,
            json!({"view":"connections","project":"approved"}),
        ),
    ] {
        let mut read = |row: &Value| {
            let page = json!({"view":"connections","page":1,"columns":[{"key":"harness","label":"하네스"}],"rows":[row],"status":"ready"});
            fixture_settings(&source, json!({"stdout":page.to_string()}));
            client.tool("oms_panel_tui_view", args.clone())
        };
        let old = read(&legacy);
        good(&old);
        let old = payload(&old);
        for k in [
            "resumeRefusalCode",
            "managedOutgoingSwap",
            "swapEvidence",
            "configuredMainPermissions",
            "fullModeOption",
        ] {
            assert_eq!(old["rows"][0][k], "unknown");
            assert!(
                old["columns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|column| column["key"] == k)
            );
        }
        let evidence_keys = [
            "managedOutgoingSwap",
            "swapEvidence",
            "configuredMainPermissions",
            "fullModeOption",
        ];
        for omitted in 1..(1 << evidence_keys.len()) {
            let mut partial = current.clone();
            let mut expected = current.clone();
            for (index, key) in evidence_keys.iter().enumerate() {
                if omitted & (1 << index) != 0 {
                    partial.as_object_mut().unwrap().remove(*key);
                    expected[*key] = json!("unknown");
                }
            }
            let response = read(&partial);
            good(&response);
            assert_eq!(payload(&response)["rows"][0], expected);
        }
        for (key, wrong) in [
            ("managedOutgoingSwap", json!("ready")),
            ("swapEvidence", json!("invalid")),
            ("configuredMainPermissions", json!(null)),
            ("fullModeOption", json!(true)),
        ] {
            let mut partial = legacy.clone();
            partial[key] = wrong;
            bad(&read(&partial), "unrecognized Connections evidence");
        }
        let mut previous = current.clone();
        previous
            .as_object_mut()
            .unwrap()
            .remove("resumeRefusalCode");
        let previous = payload(&read(&previous));
        assert_eq!(previous["rows"][0]["resumeRefusalCode"], "unknown");
        assert_eq!(previous["rows"][0]["fullModeOption"], "unsupported");

        let ok = read(&current);
        assert_eq!(payload(&ok)["rows"][0], current);
        assert_eq!(ok["result"]["structuredContent"]["rows"][0], current);
        for (key, wrong) in [
            ("owner", json!("pi")),
            ("harness", json!("other")),
            ("transport", json!("native")),
            ("fullModeOption", json!("supported")),
            ("resumeRefusalCode", json!("not_applicable")),
            ("resumeRefusalCode", json!(null)),
            ("swapEvidence", json!("requires-verified-idle-pane-exit")),
            ("managedOutgoingSwap", json!("ready")),
            ("configuredMainPermissions", json!(["full"])),
            ("foreignPolicy", json!("fixture-private-command")),
        ] {
            let mut row = current.clone();
            row[key] = wrong;
            let denied = read(&row);
            bad(&denied, "unrecognized Connections evidence");
            assert!(!denied.to_string().contains("fixture-private-command"));
        }
        let mut partial = legacy.clone();
        partial["managedOutgoingSwap"] = json!("supported");
        bad(&read(&partial), "unrecognized Connections evidence");
        let mut claude = current.clone();
        for (key, value) in [
            ("harness", "claude"),
            ("owner", "claude"),
            ("transport", "native"),
            ("dependencies", "claude"),
            ("resumePolicy", "requires verified session"),
            ("resumeRefusalCode", "not_applicable"),
            ("fullModeOption", "supported"),
        ] {
            claude[key] = json!(value);
        }
        claude["accountSelection"] = json!(true);
        for (swap, evidence) in [
            ("supported", "assisted-exit-linux-proc-available"),
            ("unsupported", "assisted-exit-linux-proc-unavailable"),
        ] {
            claude["managedOutgoingSwap"] = json!(swap);
            claude["swapEvidence"] = json!(evidence);
            assert_eq!(payload(&read(&claude))["rows"][0], claude);
        }
        let mut agy = current.clone();
        agy["owner"] = json!("antigravity");
        agy["transport"] = json!("native");
        agy["dependencies"] = json!("agy");
        agy["fullModeOption"] = json!("supported");
        for alias in ["agy", "antigravity"] {
            agy["harness"] = json!(alias);
            assert_eq!(payload(&read(&agy))["rows"][0], agy);
        }
        let page = json!({"view":"connections","page":1,"columns":[{"key":"swapEvidence","label":"교체 계약 및 호스트 조건"}],"rows":[current],"status":"ready"});
        for k in ["foreign", "columns", "status"] {
            let mut bad_page = page.clone();
            bad_page[k] = match k {
                "columns" => json!([{"key":"swapEvidence","label":"fixture private label"}]),
                "status" => json!("unavailable"),
                _ => json!("fixture private label"),
            };
            fixture_settings(&source, json!({"stdout":bad_page.to_string()}));
            let denied = client.tool("oms_panel_tui_view", args.clone());
            bad(&denied, "unrecognized Connections evidence");
            assert!(!denied.to_string().contains("fixture private label"));
        }
    }
    std::fs::remove_file(map).unwrap();

    let descriptions = (0..240)
        .map(|i| format!("Description {i} — {}", r#""some text" "#.repeat(30)))
        .collect::<Vec<_>>();
    let rows = descriptions
        .iter()
        .enumerate()
        .map(|(i, d)| format!("command-{i} {d}\n"))
        .collect::<String>();
    fixture_settings(&source, json!({"stdout":rows}));
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let page = desktop.tool(name, json!({"offset":offset}));
        good(&page);
        assert!(page.to_string().len() <= 60_000);
        let data = &page["result"]["structuredContent"];
        let rows = data["commands"].as_array().unwrap();
        assert!(!rows.is_empty());
        assert_eq!(data["omitted"], descriptions.len() - rows.len());
        for row in rows {
            assert_eq!(row["name"], format!("command-{offset}"));
            assert_eq!(row["description"], descriptions[offset]);
            offset += 1;
        }
        pages += 1;
        if data.get("nextOffset").is_none() {
            break;
        }
        assert_eq!(data["nextOffset"], offset);
    }
    assert_eq!(offset, descriptions.len());
    assert!(pages > 1);
    for (stdout, error) in [
        ("".into(), "Unreadable"),
        ("a desc\na again\n".into(), "Unreadable"),
        ("/bin/sh command\n".into(), "Unreadable"),
        ("status bad\u{7}record".into(), "Unreadable"),
        (format!("giant {}", "a".repeat(61000)), "response limit"),
        (
            format!("help {}{}", "gh", ["p_", &"a".repeat(36)].concat()),
            "outbound scan",
        ),
    ] {
        fixture_settings(&source, json!({"stdout":stdout}));
        bad(&desktop.tool(name, json!({})), error);
    }
    fixture_settings(&source, json!({"exit":2,"stderr":"private diagnostic"}));
    let result = desktop.tool(name, json!({}));
    bad(&result, "OMS refused");
    assert!(!result.to_string().contains("private diagnostic"));
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
    let symbol =
        "symbol:crates/oms-command-room/src/desktop.rs::function:impl Room<'_>#2::assignment_owner";
    fixture_settings(
        &source,
        json!({"stdout":json!([{"id":symbol}]).to_string()}),
    );
    let rows = payload(&c.tool(
        "oms_project_graph_query",
        json!({"query":"assignment_owner"}),
    ));
    good(&c.tool("oms_project_graph_trace", json!({"node":rows[0]["id"]})));
    let trace = json!([
        "graph",
        "project",
        "trace",
        "--json",
        "--depth",
        "2",
        "--direction",
        "out",
        symbol
    ]);
    assert_eq!(record(&source)["argv"], trace);
    for node in [
        "--help",
        "file:../escape",
        "symbol:src/../x::run",
        "file:foo\\bar",
        "symbol:a::f\ng",
        "symbol:a::f\u{0085}g",
        "symbol:a::f\u{202e}g",
        "symbol:a::f\u{2060}g",
        "symbol:a::f\u{2028}g",
    ] {
        bad(
            &c.tool("oms_project_graph_trace", json!({"node":node})),
            "requires",
        );
    }
    assert_eq!(
        record(&source)["argv"],
        trace,
        "invalid IDs never reached the command"
    );
    let prefix = "file:";
    let at_limit = format!("{prefix}{}", "한".repeat(169));
    assert_eq!(at_limit.len(), 512);
    good(&c.tool("oms_project_graph_trace", json!({"node":at_limit})));
    bad(
        &c.tool("oms_project_graph_trace", json!({"node":at_limit+"x"})),
        "requires",
    );
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
    let foreign = repo.join("unrelated.md");
    std::fs::write(&foreign, "## Output\nforeign text\n## Exit\n0\n").unwrap();
    let linked = dir.join("answers/advisory.md");
    std::os::unix::fs::symlink(&foreign, &linked).unwrap();
    std::fs::write(
        dir.join("run.log"),
        format!(
            "{}\n{}\n{}\n{}\nartifact: {}\n{}thread: latest\n",
            path.display(),
            foreign.display(),
            linked.display(),
            dir.join("answers/../answers/codex-근거.md").display(),
            path.display(),
            "noise\n".repeat(100000)
        ),
    )
    .unwrap();
    let result = c.tool("oms_peer_result", json!({"operation":op}));
    good(&result);
    let p = payload(&result);
    assert_eq!(p["thread"], "latest");
    assert_eq!(p["artifacts"], json!([path]));
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
    // Bare paths grant no new legacy read authority, even inside answers/.
    std::fs::write(
        dir.join("run.log"),
        format!("{}\n{}\n", legacy.display(), path.display()),
    )
    .unwrap();
    let untrusted = payload(&c.tool("oms_peer_result", json!({"operation":op})));
    assert_eq!(untrusted["artifacts"], json!([]));
    assert_eq!(untrusted["answer"], "");
    std::fs::write(
        dir.join("run.log"),
        format!("artifact: {}\n", legacy.display()),
    )
    .unwrap();
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
        payload(&running)["cancellation"],
        json!({"requested_at":Value::Null,"state":"none"})
    );
    assert_eq!(
        c.call(
            "tasks/get",
            json!({"taskId":op,"_meta":{META:"2026-07-28"}})
        )["error"]["code"],
        -32021
    );
    let get = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}));
    assert_eq!(get["result"]["status"], "working");
    assert_eq!(get["result"]["cancellation_requested"], false);
    assert_eq!(
        get["result"]["cancellation"],
        json!({"requested_at":Value::Null,"state":"none"})
    );
    assert_eq!(
        get["result"]["statusMessage"],
        "Peer consultation is running."
    );
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
    let first_bytes = std::fs::read(dir.join("cancel-request.json")).unwrap();
    let receipt: Value = serde_json::from_slice(&first_bytes).unwrap();
    assert_eq!(receipt["schema"], 1);
    assert_eq!(receipt["task_id"], op);
    let first_at = receipt["requested_at"].as_str().unwrap().to_owned();
    let second = c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}));
    assert_eq!(
        second["result"],
        json!({"resultType":"complete"}),
        "{second}"
    );
    assert_eq!(
        std::fs::read(dir.join("cancel-request.json")).unwrap(),
        first_bytes
    );
    let pinned_at = "2020-01-01T00:00:05Z";
    let pinned_bytes = format!(
        "{{\"kind\":\"mcp-task-cancel-request\",\"requested_at\":\"{pinned_at}\",\"schema\":1,\"task_id\":\"{op}\"}}  \n"
    )
    .into_bytes();
    std::fs::write(dir.join("cancel-request.json"), &pinned_bytes).unwrap();
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["result"],
        json!({"resultType":"complete"})
    );
    assert_eq!(
        std::fs::read(dir.join("cancel-request.json")).unwrap(),
        pinned_bytes
    );
    std::fs::remove_file(dir.join("pid")).unwrap();
    let requested_get =
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(requested_get["cancellation_requested"], true);
    assert_eq!(
        requested_get["cancellation"],
        json!({"requested_at":pinned_at,"state":"requested"})
    );
    assert_eq!(
        requested_get["statusMessage"],
        "Cancellation requested; awaiting provider cleanup."
    );
    let waited = payload(&c.tool("oms_peer_result", json!({"operation":op,"wait_seconds":1})));
    assert_eq!(waited["status"], "running");
    assert_eq!(waited["cancellation_requested"], true);
    assert_eq!(
        waited["cancellation"],
        json!({"requested_at":pinned_at,"state":"requested"})
    );
    assert_eq!(
        payload(&c.tool("oms_peer_operations", json!({})))["operations"][0]["cancellation"],
        json!({"requested_at":pinned_at,"state":"requested"})
    );
    let legacy_bytes =
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-request"}).to_string();
    std::fs::write(dir.join("cancel-request.json"), &legacy_bytes).unwrap();
    let legacy_get = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(legacy_get["cancellation_requested"], true);
    assert_eq!(
        legacy_get["cancellation"],
        json!({"requested_at":Value::Null,"state":"requested"})
    );
    assert_eq!(
        legacy_get["statusMessage"],
        "Cancellation requested; awaiting provider cleanup."
    );
    assert_eq!(
        payload(&c.tool("oms_peer_result", json!({"operation":op})))["cancellation"],
        json!({"requested_at":Value::Null,"state":"requested"})
    );
    assert_eq!(
        payload(&c.tool("oms_peer_operations", json!({})))["operations"][0]["cancellation"],
        json!({"requested_at":Value::Null,"state":"requested"})
    );
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["result"],
        json!({"resultType":"complete"})
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("cancel-request.json")).unwrap(),
        legacy_bytes
    );
    for bytes in [
        "not json".to_owned(),
        json!({"schema":1,"task_id":"foreign","kind":"mcp-task-cancel-request","requested_at":first_at}).to_string(),
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-request","requested_at":Value::Null}).to_string(),
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-request","requested_at":123}).to_string(),
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-request","requested_at":""}).to_string(),
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-request","requested_at":"not-rfc3339"}).to_string(),
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-request","requested_at":"2020-01-01T09:00:05+09:00"}).to_string(),
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-request","requested_at":"2020-01-01T00:00:05-00:00"}).to_string(),
        "x".repeat(4097),
    ] {
        std::fs::write(dir.join("cancel-request.json"), bytes).unwrap();
        let ignored = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
        assert_eq!(ignored["cancellation_requested"], false);
        assert_eq!(
            ignored["cancellation"],
            json!({"requested_at":Value::Null,"state":"none"})
        );
        assert_eq!(ignored["statusMessage"], "Peer consultation is running.");
        let r = payload(&c.tool("oms_peer_result", json!({"operation":op})));
        assert!(r.get("cancellation_requested").is_none());
        assert_eq!(
            r["cancellation"],
            json!({"requested_at":Value::Null,"state":"none"})
        );
    }
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["result"],
        json!({"resultType":"complete"})
    );
    let replaced_bytes = std::fs::read(dir.join("cancel-request.json")).unwrap();
    let replaced_row: Value = serde_json::from_slice(&replaced_bytes).unwrap();
    assert!(
        replaced_row["requested_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["result"],
        json!({"resultType":"complete"})
    );
    assert_eq!(
        std::fs::read(dir.join("cancel-request.json")).unwrap(),
        replaced_bytes
    );
    std::fs::remove_file(dir.join("cancel-request.json")).unwrap();
    std::os::unix::fs::symlink(dir.join("meta.json"), dir.join("cancel-request.json")).unwrap();
    let ignored = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(ignored["cancellation_requested"], false);
    assert_eq!(
        ignored["cancellation"],
        json!({"requested_at":Value::Null,"state":"none"})
    );
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["result"],
        json!({"resultType":"complete"})
    );
    assert_eq!(
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["cancellation"]["state"],
        "requested"
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
    let stalled_none = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(stalled_none["status"], "failed");
    assert_eq!(
        stalled_none["cancellation"],
        json!({"requested_at":Value::Null,"state":"none"})
    );
    assert_eq!(
        stalled_none["statusMessage"],
        "Peer process ended without a durable exit status."
    );
    for (raw_status, expected_exit) in [("bad status", 1), ("124\n", 124), ("1\n", 1), ("0\n", 0)] {
        std::fs::write(dir.join("status"), raw_status).unwrap();
        let snap = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
        assert_eq!(snap["status"], "completed");
        assert_eq!(
            snap["cancellation"],
            json!({"requested_at":Value::Null,"state":"none"})
        );
        assert_eq!(
            snap["statusMessage"],
            format!("Peer consultation completed with exit {expected_exit}.")
        );
    }
    std::fs::remove_file(dir.join("status")).unwrap();
    std::fs::write(dir.join("cancel-request.json"), &pinned_bytes).unwrap();
    let stalled_req = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(stalled_req["status"], "failed");
    assert_eq!(
        stalled_req["cancellation"],
        json!({"requested_at":pinned_at,"state":"unknown"})
    );
    assert_eq!(
        stalled_req["statusMessage"],
        "Cancellation was requested, but the peer ended without a durable exit or observation."
    );
    let stalled_res = payload(&c.tool("oms_peer_result", json!({"operation":op})));
    assert_eq!(stalled_res["cancellation_requested"], true);
    assert_eq!(
        stalled_res["cancellation"],
        json!({"requested_at":pinned_at,"state":"unknown"})
    );
    assert_eq!(
        payload(&c.tool("oms_peer_operations", json!({})))["operations"][0]["cancellation"],
        json!({"requested_at":pinned_at,"state":"unknown"})
    );
    for (non_own, expected_msg) in [
        (
            "bad status",
            "Cancellation was requested, but the peer ended without a durable exit or observation.",
        ),
        (
            "124\n",
            "Cancellation was requested, but the peer ended with exit 124 without a cancellation observation.",
        ),
        (
            "1\n",
            "Cancellation was requested, but the peer ended with exit 1 without a cancellation observation.",
        ),
        (
            "130\n",
            "Cancellation was requested, but the peer ended with exit 130 without a cancellation observation.",
        ),
        (
            "-15\n",
            "Cancellation was requested, but the peer ended with exit -15 without a cancellation observation.",
        ),
    ] {
        std::fs::write(dir.join("status"), non_own).unwrap();
        let res = payload(&c.tool("oms_peer_result", json!({"operation":op})));
        if non_own == "bad status" {
            assert_eq!(res["exit"], 1);
        }
        assert_eq!(
            res["cancellation"],
            json!({"requested_at":pinned_at,"state":"unknown"})
        );
        let snap = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
        assert_eq!(snap["status"], "completed");
        assert_eq!(
            snap["cancellation"],
            json!({"requested_at":pinned_at,"state":"unknown"})
        );
        assert_eq!(snap["statusMessage"], expected_msg);
    }
    std::fs::write(dir.join("status"), "0").unwrap();
    let completed_first =
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(completed_first["status"], "completed");
    assert_eq!(
        completed_first["cancellation"],
        json!({"requested_at":pinned_at,"state":"completed_first"})
    );
    assert_eq!(
        completed_first["statusMessage"],
        "Peer consultation completed with exit 0 before cancellation took effect."
    );
    let embedded: Value = serde_json::from_str(
        completed_first["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(embedded["cancellation_requested"], true);
    assert!(embedded.get("termination_reason").is_none());
    assert_eq!(
        embedded["cancellation"],
        json!({"requested_at":pinned_at,"state":"completed_first"})
    );
    assert_eq!(
        payload(&c.tool("oms_peer_operations", json!({})))["operations"][0]["cancellation"],
        json!({"requested_at":pinned_at,"state":"requested"})
    );
    assert_eq!(
        c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["error"]["code"],
        -32602
    );
    assert_eq!(
        std::fs::read(dir.join("cancel-request.json")).unwrap(),
        pinned_bytes
    );
    std::fs::write(
        dir.join("cancel-observed.json"),
        json!({"schema":1,"task_id":op,"kind":"mcp-task-cancel-observed"}).to_string(),
    )
    .unwrap();
    let confirmed = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(confirmed["status"], "cancelled");
    assert_eq!(
        confirmed["cancellation"],
        json!({"requested_at":pinned_at,"state":"confirmed"})
    );
    assert_eq!(
        confirmed["statusMessage"],
        "Provider cancellation was observed and the operation exited. Partial artifacts are retained."
    );
    let confirmed_res = payload(&c.tool("oms_peer_result", json!({"operation":op})));
    assert_eq!(confirmed_res["termination_reason"], "cancelled");
    assert_eq!(
        confirmed_res["cancellation"],
        json!({"requested_at":pinned_at,"state":"confirmed"})
    );
    std::fs::remove_file(dir.join("status")).unwrap();
    assert_eq!(
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["cancellation"],
        json!({"requested_at":pinned_at,"state":"confirmed"})
    );
    std::fs::write(dir.join("status"), "0").unwrap();
    std::fs::remove_file(dir.join("cancel-request.json")).unwrap();
    let observed_only =
        c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(observed_only["status"], "cancelled");
    assert_eq!(
        observed_only["cancellation"],
        json!({"requested_at":Value::Null,"state":"confirmed"})
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

    let owner_bin = PathBuf::from(env!("CARGO_BIN_EXE_oms-mcp-peer-owner"));
    let owner_op = "consult-20200101T000001Z-11111111";
    let owner_dir = run_dir(
        &repo,
        owner_op,
        json!({"answer_storage":"operation","cancellation_supported":true}),
        None,
    );
    std::fs::write(
        owner_dir.join("launch.json"),
        json!({"kind":"consult","targets":["claude"],"thread":"","new_thread":false,"debate_rounds":0,"context_options":{}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        owner_dir.join("cancel-request.json"),
        json!({"schema":1,"kind":"mcp-task-cancel-request","task_id":owner_op,"requested_at":pinned_at}).to_string(),
    )
    .unwrap();
    std::fs::create_dir(owner_dir.join("cancel-observed.json")).unwrap();
    let owner_log = std::fs::File::create(owner_dir.join("run.log")).unwrap();
    let owner_err = owner_log.try_clone().unwrap();
    let owner_out = Command::new(&owner_bin)
        .args([
            source.as_os_str(),
            repo.as_os_str(),
            owner_op.as_ref(),
            source.join("bin/oms").as_os_str(),
        ])
        .env("PATH", "")
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .stdout(owner_log)
        .stderr(owner_err)
        .status()
        .unwrap();
    assert!(!owner_out.success());
    assert_eq!(
        std::fs::read_to_string(owner_dir.join("status")).unwrap(),
        "1\n"
    );
    let owner_snap =
        c.call("tasks/get", json!({"taskId":owner_op,"_meta":modern()}))["result"].clone();
    assert_eq!(owner_snap["status"], "completed");
    assert_eq!(
        owner_snap["cancellation"],
        json!({"requested_at":pinned_at,"state":"unknown"})
    );
    assert_eq!(
        owner_snap["statusMessage"],
        "Cancellation was requested, but the peer ended with exit 1 without a cancellation observation."
    );
    assert_eq!(
        payload(&c.tool("oms_peer_result", json!({"operation":owner_op})))["cancellation"],
        json!({"requested_at":pinned_at,"state":"unknown"})
    );
    assert_eq!(
        payload(&c.tool("oms_peer_operations", json!({})))["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["operation"] == owner_op)
            .unwrap()["cancellation"],
        json!({"requested_at":pinned_at,"state":"requested"})
    );

    use std::os::unix::fs::PermissionsExt;
    let runner_script = source.join("runner-cancel-receipt-fail");
    std::fs::write(
        &runner_script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}' > \"$OMS_PEER_CANCEL_DIR/cancel-request.json\"\necho 'error: cancellation handled but receipt publication failed: ReadOnly' >&2\nexit 1\n",
            json!({"schema":1,"kind":"mcp-task-cancel-request","task_id":"consult-20200101T000002Z-22222222","requested_at":pinned_at})
        ),
    )
    .unwrap();
    std::fs::set_permissions(&runner_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runner_op = "consult-20200101T000002Z-22222222";
    let runner_dir = run_dir(
        &repo,
        runner_op,
        json!({"answer_storage":"operation","cancellation_supported":true}),
        None,
    );
    std::fs::write(
        runner_dir.join("launch.json"),
        json!({"kind":"consult","targets":["claude"],"thread":"","new_thread":false,"debate_rounds":0,"context_options":{}}).to_string(),
    )
    .unwrap();
    let runner_log = std::fs::File::create(runner_dir.join("run.log")).unwrap();
    let runner_err = runner_log.try_clone().unwrap();
    let runner_out = Command::new(&owner_bin)
        .args([
            source.as_os_str(),
            repo.as_os_str(),
            runner_op.as_ref(),
            runner_script.as_os_str(),
        ])
        .env("PATH", "")
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .stdout(runner_log)
        .stderr(runner_err)
        .status()
        .unwrap();
    assert!(runner_out.success());
    assert_eq!(
        std::fs::read_to_string(runner_dir.join("status")).unwrap(),
        "1\n"
    );
    let runner_snap =
        c.call("tasks/get", json!({"taskId":runner_op,"_meta":modern()}))["result"].clone();
    assert_eq!(runner_snap["status"], "completed");
    assert_eq!(
        runner_snap["cancellation"],
        json!({"requested_at":pinned_at,"state":"unknown"})
    );
    assert_eq!(
        runner_snap["statusMessage"],
        "Cancellation was requested, but the peer ended with exit 1 without a cancellation observation."
    );
    assert_eq!(
        payload(&c.tool("oms_peer_result", json!({"operation":runner_op})))["cancellation"],
        json!({"requested_at":pinned_at,"state":"unknown"})
    );

    let seat_op = "consult-20200101T000003Z-33333333";
    let seat_dir = run_dir(
        &repo,
        seat_op,
        json!({"answer_storage":"operation","cancellation_supported":true}),
        Some("1\n"),
    );
    std::fs::write(
        seat_dir.join("cancel-request.json"),
        json!({"schema":1,"kind":"mcp-task-cancel-request","task_id":seat_op,"requested_at":pinned_at}).to_string(),
    )
    .unwrap();
    let seat_answer = seat_dir.join("answers/claude.md");
    std::fs::write(&seat_answer, "## Output\npartial without exit\n").unwrap();
    std::fs::write(
        seat_dir.join("run.log"),
        format!("{}\n", seat_answer.display()),
    )
    .unwrap();
    assert_eq!(
        c.call("tasks/get", json!({"taskId":seat_op,"_meta":modern()}))["result"]["cancellation"],
        json!({"requested_at":pinned_at,"state":"unknown"})
    );
    std::fs::write(
        &seat_answer,
        "## Output\nprovider failed on its own\n## Exit\n1\n",
    )
    .unwrap();
    let seat_snap =
        c.call("tasks/get", json!({"taskId":seat_op,"_meta":modern()}))["result"].clone();
    assert_eq!(
        seat_snap["cancellation"],
        json!({"requested_at":pinned_at,"state":"completed_first"})
    );
    assert_eq!(
        seat_snap["statusMessage"],
        "Peer consultation completed with exit 1 before cancellation took effect."
    );
    assert_eq!(
        payload(&c.tool("oms_peer_operations", json!({})))["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["operation"] == seat_op)
            .unwrap()["cancellation"],
        json!({"requested_at":pinned_at,"state":"requested"})
    );

    let legacy_guard_op = "consult-20200101T000004Z-44444444";
    let legacy_guard_dir = run_dir(
        &repo,
        legacy_guard_op,
        json!({"answer_storage":"operation","cancellation_supported":true}),
        None,
    );
    std::fs::write(
        legacy_guard_dir.join("launch.json"),
        json!({"kind":"consult","targets":["claude"],"thread":"","new_thread":false,"debate_rounds":0,"context_options":{}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        legacy_guard_dir.join("cancel-request.json"),
        json!({"schema":1,"kind":"mcp-task-cancel-request","task_id":legacy_guard_op}).to_string(),
    )
    .unwrap();
    let _ = std::fs::remove_file(source.join("last-call.json"));
    let guard_log = std::fs::File::create(legacy_guard_dir.join("run.log")).unwrap();
    let guard_err = guard_log.try_clone().unwrap();
    let guard_out = Command::new(&owner_bin)
        .args([
            source.as_os_str(),
            repo.as_os_str(),
            legacy_guard_op.as_ref(),
            source.join("bin/oms").as_os_str(),
        ])
        .env("PATH", "")
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .stdout(guard_log)
        .stderr(guard_err)
        .status()
        .unwrap();
    assert!(guard_out.success());
    assert!(!source.join("last-call.json").exists());
    assert_eq!(
        std::fs::read_to_string(legacy_guard_dir.join("status")).unwrap(),
        "125\n"
    );
    assert!(legacy_guard_dir.join("cancel-observed.json").is_file());
    let guard_snap = c.call(
        "tasks/get",
        json!({"taskId":legacy_guard_op,"_meta":modern()}),
    )["result"]
        .clone();
    assert_eq!(guard_snap["status"], "cancelled");
    assert_eq!(
        guard_snap["cancellation"],
        json!({"requested_at":Value::Null,"state":"confirmed"})
    );
    assert_eq!(
        payload(&c.tool("oms_peer_operations", json!({})))["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["operation"] == legacy_guard_op)
            .unwrap()["cancellation"],
        json!({"requested_at":Value::Null,"state":"confirmed"})
    );
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
    for (idx, initial) in [
        None,
        Some("not json".to_owned()),
        Some(
            json!({
                "schema": 1,
                "task_id": "foreign",
                "kind": "mcp-task-cancel-request",
                "requested_at": "2020-01-01T00:00:05Z"
            })
            .to_string(),
        ),
        Some(
            json!({
                "schema": 1,
                "task_id": "consult-20200101T000003Z-deadbeef",
                "kind": "mcp-task-cancel-request",
                "requested_at": "2020-01-01T09:00:05+09:00"
            })
            .to_string(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let op = format!("consult-20200101T00000{idx}Z-deadbeef");
        let dir = run_dir(&repo, &op, json!({"cancellation_supported":true}), None);
        if let Some(invalid) = initial {
            std::fs::write(dir.join("cancel-request.json"), invalid).unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let s = source.clone();
            let r = repo.clone();
            let op_id = op.clone();
            let run = dir.clone();
            let gate = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let mut c = Client::new(&s, &r, false, true, false);
                gate.wait();
                let v = c.call("tasks/cancel", json!({"taskId":op_id,"_meta":modern()}));
                assert_eq!(v["result"], json!({"resultType":"complete"}), "{v}");
                std::fs::read(run.join("cancel-request.json")).unwrap()
            }));
        }
        let seen: Vec<Vec<u8>> = threads.into_iter().map(|h| h.join().unwrap()).collect();
        let bytes = std::fs::read(dir.join("cancel-request.json")).unwrap();
        for observed in &seen {
            assert_eq!(observed, &bytes);
        }
        let row: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(row["task_id"], op);
        assert!(row["requested_at"].as_str().unwrap().ends_with('Z'));
        let mut c = Client::new(&source, &repo, false, true, false);
        assert_eq!(
            c.call("tasks/cancel", json!({"taskId":op,"_meta":modern()}))["result"],
            json!({"resultType":"complete"})
        );
        assert_eq!(
            std::fs::read(dir.join("cancel-request.json")).unwrap(),
            bytes
        );
        assert_eq!(
            c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"]["cancellation"],
            json!({"requested_at":row["requested_at"],"state":"requested"})
        );
        assert!(dir.join("cancel-request.lock").is_file());
        assert_eq!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
                .count(),
            0
        );
    }
}
fn peer_detached_cancel() {
    let t = Temp::new();
    let source = t.source();
    let repo = t.repo();
    fixture_settings(&source, json!({"gate":true}));
    let mut c = Client::new(&source, &repo, false, true, false);
    let started=c.call("tools/call",json!({"name":"oms_peer_start","arguments":{"kind":"consult","prompt":"hold"},"_meta":modern()}));
    assert_eq!(started["result"]["resultType"], "task");
    assert_eq!(
        started["result"]["cancellation"],
        json!({"requested_at":Value::Null,"state":"none"})
    );
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
    let settled = c.call("tasks/get", json!({"taskId":op,"_meta":modern()}))["result"].clone();
    assert_eq!(settled["status"], "cancelled");
    assert_eq!(settled["cancellation"]["state"], "confirmed");
    assert!(settled["cancellation"]["requested_at"].as_str().is_some());
    assert!(!source.join("release").exists());
    let rows = payload(&c.tool("oms_peer_operations", json!({})));
    assert_eq!(rows["operations"][0]["termination_reason"], "cancelled");
    assert_eq!(
        rows["operations"][0]["cancellation"],
        settled["cancellation"]
    );
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
    if cfg["leadRoom"] == true && args.first().is_some_and(|a| a == "agent-plan") {
        assert_eq!(args.len(), 6);
        assert_eq!(args[1], "--repo");
        assert_eq!(args[3], "evidence-snapshot");
        assert_eq!(args[4], "--id");
        println!("{}", cfg["leadContextTask"]);
        return;
    }
    if cfg["leadRoom"] == true && args.first().is_some_and(|a| a == "models") {
        assert_eq!(args.len(), 4);
        assert_eq!(args[1], "--providers");
        assert_eq!(args[3], "--json");
        let p = args[2].to_str().unwrap();
        let mut row = json!({"provider":p,"present":true,"exact_model_override":true,"catalog_probe":"ok","catalog_age_seconds":42,
            "effort_values":["low","high","ultra"]});
        match p {
            "codex" => {
                row["catalog_models"] = json!(["gpt-6-astra", "unrouted"]);
                row["routable"] = json!(["gpt-6-astra"]);
                row["model_effort_scales"] = json!({"gpt-6-astra":["high"]});
            }
            "claude" => row["catalog_probe"] = "unsupported".into(),
            "antigravity" => {
                row["catalog_models"] = json!([
                    "gemini-4-carbon-high",
                    "gemini-4-carbon-low",
                    "gemini-pro-orchestrator"
                ]);
                row["routable"] = json!(["gemini-4-carbon-high", "gemini-pro-orchestrator"]);
                row["selectors"] = json!({"gemini-4-carbon":{"high":"gemini-4-carbon-high","low":"gemini-4-carbon-low"}});
            }
            _ => panic!("unknown provider in catalog request"),
        }
        println!("{}", json!({"schema":1,"providers":[row]}));
        return;
    }
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
        assert_eq!(
            args.iter().any(|a| a == "--no-current"),
            cfg["leadRoom"] == true
        );
        if cfg["leadRoom"] == true {
            let id = args.windows(2).find(|a| a[0] == "--thread").unwrap()[1]
                .to_str()
                .unwrap();
            assert!(source.join(format!("thread-{id}")).is_file());
        }
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
        let reply = args
            .iter()
            .filter_map(|a| a.to_str())
            .find_map(|a| a.strip_prefix("--prompt="))
            .map_or_else(
                || "fixture answer".to_owned(),
                |prompt| {
                    let field = |name: &str| {
                        args.windows(2).find(|pair| pair[0] == name).unwrap()[1]
                            .to_str()
                            .unwrap()
                    };
                    let thread = field("--thread");
                    let provider = field("--providers");
                    let explicit = matches!(
                        prompt,
                        "Private fixture follow-up 1"
                            | "Private fixture follow-up 2"
                            | "Private fixture follow-up 3"
                    );
                    assert_eq!(args.iter().any(|a| a == "--model"), explicit);
                    assert_eq!(args.iter().any(|a| a == "--reasoning-effort"), explicit);
                    if explicit {
                        assert_eq!(
                            field("--model"),
                            match provider {
                                "codex" => "gpt-6-astra",
                                "claude" => "opus",
                                "antigravity" => "gemini-4-carbon",
                                _ => panic!(),
                            }
                        );
                        assert_eq!(field("--reasoning-effort"), "high");
                    }

                    let owner: Value = serde_json::from_slice(
                        &std::fs::read(source.join(format!(
                            "lead-fixture-{}",
                            if provider == "antigravity" {
                                "agy"
                            } else {
                                provider
                            }
                        )))
                        .unwrap(),
                    )
                    .unwrap();
                    assert_eq!(owner["operation"], thread);
                    assert_ne!(run.file_name().unwrap(), thread);
                    format!("{provider} received {prompt}")
                },
            );
        std::fs::write(&artifact, format!("## Output\n{reply}\n## Exit\n0\n")).unwrap();
        // Native peer storage publishes answer artifacts privately (storage::write).
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600)).unwrap();
        if cfg["leadRoom"] == true {
            // Normal native peer-ask stdout prints the seat path before its response.
            let provider = args.windows(2).find(|a| a[0] == "--providers").unwrap()[1]
                .to_str()
                .unwrap();
            let operation = run.file_name().unwrap().to_str().unwrap();
            let marker = std::env::var("OMS_PEER_LEAD_OBSERVATION").ok();
            if matches!(provider, "claude" | "antigravity") {
                use sha2::{Digest, Sha256};
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(marker.as_deref(), Some(operation));
                let followup = args
                    .iter()
                    .any(|a| a.to_str().is_some_and(|s| s.starts_with("--prompt=")));
                let id = if followup {
                    "a2000000-0000-4000-8000-000000000002"
                } else {
                    "a1000000-0000-4000-8000-000000000001"
                };
                let proof = json!({"schema":1,"operation":operation,"provider":provider,
                    "sessionId":id,"model":(provider=="claude").then_some("claude-observed"),
                    "artifact":"fixture.md","artifactSha256":format!("{:x}",Sha256::digest(std::fs::read(&artifact).unwrap()))});
                let observed = out.join("native-lead-observation.json");
                std::fs::write(&observed, proof.to_string()).unwrap();
                std::fs::set_permissions(observed, std::fs::Permissions::from_mode(0o600)).unwrap();
            } else {
                assert!(marker.is_none());
            }
            println!(
                "{}\n\n{provider}:\nfixture console text\npeer-ask: 1/1 target(s) answered, 1 independent model family(ies)",
                artifact.display()
            );
        } else {
            println!("artifact: {}", artifact.display());
            println!("thread: fixture-thread");
        }
        return;
    }
    if cfg["leadRoom"] == true && args.first().is_some_and(|a| a == "thread") {
        let id = args.windows(2).find(|a| a[0] == "--id").unwrap()[1]
            .to_str()
            .unwrap();
        let thread = source.join(format!("thread-{id}"));
        if args.iter().any(|a| a == "new") {
            assert!(args.iter().any(|a| a == "--live"));
            assert!(args.iter().any(|a| a == "--no-current"));
            assert!(!thread.exists(), "recreated existing thread");
            std::fs::write(&thread, "open").unwrap();
            return;
        }
        if args.iter().any(|a| a == "context") {
            assert!(args.iter().any(|a| a == "--require-open"));
            if std::fs::read_to_string(thread).ok().as_deref() != Some("open") {
                std::process::exit(2);
            }
            println!("Open collaboration note");
            return;
        }
    }
    if cfg["leadRoom"] == true && args.first().is_some_and(|a| a == "room") {
        use sha2::{Digest, Sha256};
        let arg = |flag: &str| {
            args.windows(2)
                .find(|a| a[0] == flag)
                .and_then(|a| a[1].to_str())
        };
        let digest = format!(
            "{:x}",
            Sha256::digest(b"01900000-0000-7000-8000-0000000000bb")
        );
        if arg("--id") != Some("lead-room") || arg("--conversation-digest") != Some(&digest[..32]) {
            std::process::exit(2);
        }
        let providers = ["codex", "claude", "agy"];
        let req = |idx: usize| format!("d0000000-0000-8000-8000-{idx:012}");
        let Some((i, provider)) =
            providers
                .iter()
                .enumerate()
                .find(|(i, p)| match arg("--provider") {
                    Some(selected) => selected == **p,
                    None => arg("--request") == Some(req(*i).as_str()),
                })
        else {
            std::process::exit(2)
        };
        let request = req(i);
        assert!(arg("--request").is_none_or(|id| id == request));
        let path = source.join(format!("lead-fixture-{provider}"));
        let absent = !path.exists();
        let mut row = if absent {
            json!({"request":request,"status":"not_found"})
        } else {
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
        };
        match args[1].to_str().unwrap() {
            "desktop-lead-open" => {
                if absent {
                    let canonical = if *provider == "agy" {
                        "antigravity"
                    } else {
                        provider
                    };
                    row = json!({"request":request,"room":"lead-room","participant":format!("ask-{request}"),
                        "provider":canonical,"role":"worker","purpose":"provider-lead","readOnly":true,
                        "title":(["Codex Lead","Claude Lead","AGY Lead"][i]),"task":"Acknowledge availability as this Room's provider Lead, then wait for explicit work. Do not inspect or change the repository, use tools, or delegate.",
                        "assignedAt":"2026-11-24T01:02:03Z","status":"assigned","thread":null,"operation":null});
                    std::fs::write(&path, row.to_string()).unwrap();
                }
                row["fresh"] = json!(absent);
            }
            "desktop-assign-receipt" => {
                if *provider == "claude" && cfg["failClaudeLink"] == true {
                    std::process::exit(2);
                }
                let op = arg("--operation").unwrap();
                row["operation"] = json!(op);
                row["thread"] = json!(op);
                row["status"] = json!("started");
                std::fs::write(&path, row.to_string()).unwrap();
            }
            "desktop-lead-status" => (),
            other => panic!("unexpected lead command {other}"),
        }
        println!("{row}");
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
    if let Some(body) = cfg["pagedWorkerBody"].as_str()
        && args.iter().any(|v| v == "--desktop-worker-output")
    {
        let page = args
            .windows(2)
            .find(|pair| pair[0] == "--page")
            .and_then(|pair| pair[1].to_str()?.parse::<usize>().ok());
        let selector = page.map_or(
            oms_scrub::PageSelector::Offset(0),
            oms_scrub::PageSelector::Number,
        );
        let out = match oms_scrub::outbound_page(body, 1024 * 1024, selector) {
            Ok(part) => {
                if let Some(page) = page {
                    json!({
                        "room":cfg["room"],
                        "leadAttempt":cfg["leadAttempt"],
                        "workerAttempt":cfg["workerAttempt"],
                        "status":"complete",
                        "source":"indexed-answer",
                        "text":part.text,
                        "truncated":part.has_more,
                        "page":{
                            "page":page,
                            "offset":part.offset,
                            "bytes":part.end - part.offset,
                            "totalBytes":part.total_bytes,
                            "digest":"d".repeat(64),
                            "revision":"e".repeat(64),
                            "hasMore":part.has_more,
                            "nextPage":part.has_more.then_some(page + 1)
                        }
                    })
                } else {
                    let preview_end = body.floor_char_boundary(body.len().min(16 * 1024));
                    json!({
                        "room":cfg["room"],
                        "leadAttempt":cfg["leadAttempt"],
                        "workerAttempt":cfg["workerAttempt"],
                        "status":"complete",
                        "source":"indexed-answer",
                        "text":&body[..preview_end],
                        "truncated":preview_end < body.len()
                    })
                }
            }
            Err(oms_scrub::PageError::OutboundWithheld) => json!({
                "room":cfg["room"],
                "leadAttempt":cfg["leadAttempt"],
                "workerAttempt":cfg["workerAttempt"],
                "status":"outbound_withheld",
                "text":Value::Null,
                "source":Value::Null,
                "truncated":false
            }),
            Err(oms_scrub::PageError::InvalidRange) => std::process::exit(2),
        };
        println!("{out}");
        return;
    }
    if let Some(spec) = cfg.get("pagedChatEvidence").filter(|v| v.is_object())
        && args.iter().any(|v| v == "--evidence-full")
    {
        let page = args
            .windows(2)
            .find(|pair| pair[0] == "--page")
            .and_then(|pair| pair[1].to_str()?.parse::<usize>().ok())
            .unwrap_or(1);
        let body = spec["body"].as_str().unwrap();
        let mut out = spec["base"].clone();
        out["messages"] = json!([]);
        out["evidence"] = match oms_scrub::outbound_page(
            body,
            16 * 1024 * 1024,
            oms_scrub::PageSelector::Number(page),
        ) {
            Ok(part) => json!({
                "attempt":spec["attempt"],
                "section":spec["section"],
                "page":page,
                "source":"indexed-answer",
                "text":part.text,
                "lines":part.text.lines().collect::<Vec<_>>(),
                "offset":part.offset,
                "bytes":part.end - part.offset,
                "totalBytes":part.total_bytes,
                "digest":"a".repeat(64),
                "revision":"b".repeat(64),
                "nextPage":part.has_more.then_some(page + 1),
                "hasMore":part.has_more,
                "truncated":false
            }),
            Err(oms_scrub::PageError::OutboundWithheld) => json!({
                "attempt":spec["attempt"],
                "section":spec["section"],
                "status":"outbound_withheld"
            }),
            Err(oms_scrub::PageError::InvalidRange) => std::process::exit(2),
        };
        println!("{out}");
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
    if args.get(1).is_some_and(|a| a == "--single-open") {
        let flag = AtomicBool::new(false);
        let mut cfg = pinned(Path::new(&args[2]), config(false, false));
        cfg.profile = ToolProfile::ChatGptPanel;
        let mut s = Server::new(Path::new(&args[2]), Path::new(&args[3]), &flag, cfg).unwrap();
        let v = s
            .handle(
                &json!({"id":1,"method":"tools/call","params":{"name":"oms_open_panel","arguments":{}}}),
            )
            .unwrap();
        println!("{v}");
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
    let cases: [(&str, fn()); 38] = [
        ("lossless_values", lossless_values),
        ("raw_peer_paths", raw_peer_paths),
        ("parser_reference", parser_reference),
        ("owner_validation", owner_validation),
        ("environment_profile", environment_profile),
        ("cross_host_project_reads", cross_host_project_reads),
        ("desktop_reviewed_issue", desktop_reviewed_issue),
        ("mcp_app_binding_boundaries", mcp_app_binding_boundaries),
        ("conversation_panel", conversation_panel),
        ("desktop_lead_conversations", desktop_lead_conversations),
        (
            "lead_turn_failure_degrades_per_row",
            lead_turn_failure_degrades_per_row,
        ),
        ("desktop_assignment", desktop_assignment),
        ("panel_command_catalog", panel_command_catalog),
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
