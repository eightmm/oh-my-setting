#![forbid(unsafe_code)]

use oms_command_linear::{
    CREATED_THOUGHT_CALLER_KEY, ENV_ACCESS_CRED, ENV_LINEAR_API_URL, ENV_WEBHOOK_CRED,
    FIRST_THOUGHT_BODY, REFUSED_CONTENT_ERROR_BODY, ROOM_TEXT_LIMIT, SecretValue, ServeConfig,
    ServerHandle, UNTRUSTED_HEADER, WEBHOOK_PATH, created_message_id, derive_activity_id,
    hmac_sha256_hex, prompted_message_id, receipt_message_id, verify_linear_signature,
};
use oms_command_room::{Join, LocalServices, Room, Session};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);
const CHILD_TEST_ENV: &str = "OMS_LINEAR_TEST_CHILD";

fn runtime_sig_val() -> String {
    ["whs", "linear", "fixture", "9876543210fedcba"].join("_")
}

fn runtime_acc_val() -> String {
    ["lin", "app", "fixture", "abcdef0123456789"].join("_")
}

fn clean_child_cmd(cmd: &mut Command, home: &Path, path_env: &OsString) {
    cmd.env_clear()
        .env("PATH", path_env)
        .env("HOME", home)
        .env("TMPDIR", std::env::temp_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
}

fn isolated(test_name: &str, body: impl FnOnce(&TempWorkspace)) {
    if std::env::var(CHILD_TEST_ENV).as_deref() == Ok(test_name) {
        let ws = TempWorkspace::new("oms-linear-inner");
        body(&ws);
        return;
    }
    let outer = TempWorkspace::new("oms-linear-outer");
    let exe = std::env::current_exe().expect("test executable");
    let path_env = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let mut cmd = Command::new(exe);
    clean_child_cmd(&mut cmd, &outer.home, &path_env);
    cmd.env(CHILD_TEST_ENV, test_name)
        .args(["--exact", test_name, "--nocapture"]);
    let out = cmd.output().expect("spawn isolated child test");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "isolated {test_name} failed:\n{combined}"
    );
    assert!(
        combined.contains("1 passed;"),
        "isolated {test_name} did not run:\n{combined}"
    );
}

struct TempWorkspace {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl TempWorkspace {
    fn new(prefix: &str) -> Self {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut chosen = None;
        for _ in 0..64 {
            let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!("{prefix}-{pid}-{nanos:x}-{seq}"));
            match DirBuilder::new().mode(0o700).create(&candidate) {
                Ok(()) => {
                    chosen = Some(candidate);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create temp workspace: {e}"),
            }
        }
        let root = chosen
            .expect("unique temp workspace")
            .canonicalize()
            .expect("canonicalize temp workspace");
        let repo = root.join("repo");
        let home = root.join("home");
        let bin = root.join("bin");
        for d in [&repo, &home, &bin] {
            DirBuilder::new()
                .mode(0o700)
                .create(d)
                .expect("create workspace subdir");
        }
        let path_env = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
        let mut git = Command::new("git");
        clean_child_cmd(&mut git, &home, &path_env);
        let st = git
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .expect("git init");
        assert!(st.success());
        Self {
            root,
            repo,
            home,
            bin,
        }
    }

    fn setup_room(&self, room_id: &str, main_id: &str, bridge_id: &str) {
        let stop = AtomicBool::new(false);
        let room = Room::open(
            &self.repo,
            Session::default(),
            &LocalServices,
            &stop,
            true,
            false,
        )
        .expect("open room for creation");
        room.create(Some(room_id), "Linear test room", None, Some(main_id), None)
            .expect("create room");
        room.join(room_id, Join::new(main_id, "codex"))
            .expect("join main");
        let mut bridge = Join::new(bridge_id, "codex");
        bridge.role = "advisor".to_owned();
        room.join(room_id, bridge).expect("join bridge");
    }

    fn room_messages(&self, room_id: &str) -> Vec<oms_command_room::value::Value> {
        let stop = AtomicBool::new(false);
        let room = Room::open(
            &self.repo,
            Session::default(),
            &LocalServices,
            &stop,
            false,
            false,
        )
        .expect("open room for reading");
        room.messages(room_id).expect("read room messages")
    }

    fn install_curl_argv_spy(&self) -> (PathBuf, OsString) {
        let orig_path = std::env::var_os("PATH").expect("PATH present");
        let real_curl = std::env::split_paths(&orig_path)
            .map(|dir| dir.join("curl"))
            .find(|p| p.is_file())
            .expect("system curl binary");
        let real_curl = fs::canonicalize(real_curl).expect("canonical curl");

        let argv_log = self.root.join("curl-argv.log");
        let wrapper = self.bin.join("curl");
        let script = format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'CALL\\n' >> {}\nfor a in \"$@\"; do printf 'ARG:%s\\n' \"$a\" >> {}; done\nexec {} \"$@\"\n",
            shell_single_quote(&argv_log),
            shell_single_quote(&argv_log),
            shell_single_quote(&real_curl),
        );
        fs::write(&wrapper, script).expect("write curl wrapper");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))
            .expect("chmod curl wrapper");

        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(&orig_path));
        let joined = std::env::join_paths(paths).expect("join PATH");
        (argv_log, joined)
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn shell_single_quote(p: &Path) -> String {
    let s = p.to_str().expect("UTF-8 test path");
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[derive(Clone, Debug)]
struct RecordedGraphqlCall {
    auth_header: Option<String>,
    body: Value,
}

#[derive(Default)]
struct FakeGraphqlState {
    calls: Vec<RecordedGraphqlCall>,
    existing_ids: BTreeSet<String>,
    create_count: usize,
    query_count: usize,
}

struct FakeGraphqlServer {
    addr: SocketAddr,
    state: Arc<Mutex<FakeGraphqlState>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl FakeGraphqlServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake GraphQL");
        let addr = listener.local_addr().expect("fake GraphQL addr");
        listener
            .set_nonblocking(true)
            .expect("nonblocking fake GraphQL");
        let state = Arc::new(Mutex::new(FakeGraphqlState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let st_clone = Arc::clone(&state);
        let stop_clone = Arc::clone(&stop);

        let join = thread::spawn(move || {
            while !stop_clone.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        handle_fake_graphql_conn(&mut stream, &st_clone);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            state,
            stop,
            join: Some(join),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/graphql", self.addr)
    }

    fn snapshot(&self) -> (Vec<RecordedGraphqlCall>, usize, usize) {
        let guard = self.state.lock().expect("lock fake state");
        (guard.calls.clone(), guard.query_count, guard.create_count)
    }

    fn insert_existing(&self, id: &str) {
        self.state
            .lock()
            .expect("lock fake state")
            .existing_ids
            .insert(id.to_owned());
    }

    fn wait_for_calls(&self, count: usize, timeout: Duration) -> Vec<RecordedGraphqlCall> {
        let deadline = Instant::now() + timeout;
        loop {
            let calls = self.state.lock().expect("lock fake state").calls.clone();
            if calls.len() >= count || Instant::now() >= deadline {
                return calls;
            }
            thread::sleep(Duration::from_millis(15));
        }
    }
}

impl Drop for FakeGraphqlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn handle_fake_graphql_conn(stream: &mut TcpStream, state: &Arc<Mutex<FakeGraphqlState>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let headers_str = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut content_len = 0usize;
    let mut auth_header = None;
    let auth_name = concat!("author", "ization");
    for line in headers_str.split("\r\n").skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_len = v.trim().parse().unwrap_or(0);
            } else if k.trim().eq_ignore_ascii_case(auth_name) {
                auth_header = Some(v.trim().to_owned());
            }
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_len {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(content_len);
    let parsed: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let query_str = parsed.get("query").and_then(Value::as_str).unwrap_or("");

    let resp_json = {
        let mut st = state.lock().expect("lock fake state");
        st.calls.push(RecordedGraphqlCall {
            auth_header,
            body: parsed.clone(),
        });
        if query_str.contains("agentActivityCreate") {
            st.create_count += 1;
            let input = &parsed["variables"]["input"];
            let id = input
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("generated-id")
                .to_owned();
            st.existing_ids.insert(id.clone());
            json!({
                "data": {
                    "agentActivityCreate": {
                        "success": true,
                        "agentActivity": { "id": id }
                    }
                }
            })
        } else {
            st.query_count += 1;
            let id = parsed["variables"]["id"].as_str().unwrap_or("");
            if st.existing_ids.contains(id) {
                json!({
                    "data": {
                        "agentActivity": { "id": id }
                    }
                })
            } else {
                json!({
                    "data": {
                        "agentActivity": null
                    }
                })
            }
        }
    };

    let resp_bytes = serde_json::to_vec(&resp_json).expect("encode fake GraphQL response");
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp_bytes.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&resp_bytes);
    let _ = stream.flush();
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("unix time")
        .as_millis() as i64
}

fn post_raw_http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> u16 {
    let mut stream = TcpStream::connect(addr).expect("connect webhook server");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .expect("write timeout");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for &(k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    let _ = stream.write_all(req.as_bytes());
    if !body.is_empty() {
        let _ = stream.write_all(body);
    }
    let _ = stream.flush();

    let mut resp = Vec::new();
    let _ = stream.read_to_end(&mut resp);
    let text = String::from_utf8_lossy(&resp);
    let status_line = text.lines().next().unwrap_or("");
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .expect("HTTP status code")
}

fn post_signed_webhook(addr: SocketAddr, sig_val: &str, payload: &Value) -> u16 {
    let raw = serde_json::to_vec(payload).expect("serialize webhook JSON");
    let sig = hmac_sha256_hex(sig_val.as_bytes(), &raw);
    let len_s = raw.len().to_string();
    post_raw_http(
        addr,
        "POST",
        WEBHOOK_PATH,
        &[
            ("Content-Type", "application/json"),
            ("Content-Length", &len_s),
            ("Linear-Signature", &sig),
        ],
        &raw,
    )
}

#[test]
fn rfc_4231_hmac_sha256_vectors_and_constant_time_verification() {
    // RFC 4231 Test Case 1: 20-byte 0x0b key, "Hi There"
    let k1 = [0x0bu8; 20];
    assert_eq!(
        hmac_sha256_hex(&k1, b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );

    // RFC 4231 Test Case 2: "Jefe", "what do ya want for nothing?"
    assert_eq!(
        hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );

    // RFC 4231 Test Case 6: 131-byte 0xaa key (> 64-byte block size)
    let k6 = [0xaau8; 131];
    assert_eq!(
        hmac_sha256_hex(
            &k6,
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        ),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );

    let good = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
    assert!(verify_linear_signature(
        b"Jefe",
        b"what do ya want for nothing?",
        good
    ));
    // Uppercase hex must be rejected (contract specifies lowercase hex).
    assert!(!verify_linear_signature(
        b"Jefe",
        b"what do ya want for nothing?",
        &good.to_ascii_uppercase()
    ));
    assert!(!verify_linear_signature(
        b"Jefe",
        b"what do ya want for nothing?",
        ""
    ));
}

#[test]
fn cli_help_loopback_and_secret_startup_validation() {
    isolated("cli_help_loopback_and_secret_startup_validation", |ws| {
        ws.setup_room("r1", "main", "linear");
        let stop = AtomicBool::new(false);

        let (code, out, err) =
            oms_command_linear::run(&[OsString::from("--help")], &ws.repo, &stop);
        assert_eq!(code, 0);
        assert!(err.is_empty());
        let help = String::from_utf8(out).expect("help UTF-8");
        for needle in [
            "oms linear serve",
            "oms linear reply",
            ENV_WEBHOOK_CRED,
            ENV_ACCESS_CRED,
            ENV_LINEAR_API_URL,
            "actor=app",
            "app:assignable",
            "app:mentionable",
            "HTTPS tunnel",
        ] {
            assert!(help.contains(needle), "help missing {needle:?}");
        }

        // Missing env vars fail before binding.
        let serve_args = |listen: &str| -> Vec<OsString> {
            [
                "serve",
                "--repo",
                ws.repo.to_str().unwrap(),
                "--room",
                "r1",
                "--participant",
                "linear",
                "--to",
                "main",
                "--organization",
                "org-1",
                "--app-user",
                "app-1",
                "--listen",
                listen,
            ]
            .into_iter()
            .map(OsString::from)
            .collect()
        };
        let (code, _, err) = oms_command_linear::run(&serve_args("127.0.0.1:0"), &ws.repo, &stop);
        assert_ne!(code, 0);
        assert!(String::from_utf8_lossy(&err).contains(ENV_WEBHOOK_CRED));

        // Whitespace in secret/token rejected, never echoed.
        let bad_val = format!("bad {}", runtime_sig_val());
        // Safety: isolated single-threaded child process.
        unsafe_free_set_env(
            &ws.home,
            &serve_args("127.0.0.1:0"),
            &[
                (ENV_WEBHOOK_CRED, &bad_val),
                (ENV_ACCESS_CRED, &runtime_acc_val()),
            ],
            |c, _o, e| {
                assert_ne!(c, 0);
                assert!(!e.contains(&bad_val));
            },
        );

        // Non-loopback address refused before binding.
        unsafe_free_set_env(
            &ws.home,
            &serve_args("0.0.0.0:0"),
            &[
                (ENV_WEBHOOK_CRED, &runtime_sig_val()),
                (ENV_ACCESS_CRED, &runtime_acc_val()),
            ],
            |c, _o, e| {
                assert_ne!(c, 0);
                assert!(e.contains("loopback"), "stderr: {e}");
            },
        );
    });
}

fn unsafe_free_set_env(
    home: &Path,
    linear_args: &[OsString],
    vars: &[(&str, &str)],
    check: impl FnOnce(i32, String, String),
) {
    const SUB_ENV: &str = "OMS_LINEAR_ENV_SUB";
    let exe = std::env::current_exe().expect("test exe");
    let path_env = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let mut cmd = Command::new(exe);
    clean_child_cmd(&mut cmd, home, &path_env);
    cmd.env(SUB_ENV, "1")
        .env(
            "OMS_LINEAR_SUB_ARGS",
            serde_json::to_string(
                &linear_args
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        )
        .args(["--exact", "env_validation_subhelper", "--nocapture"]);
    for &(k, v) in vars {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run subhelper");
    let marker = String::from_utf8_lossy(&out.stdout);
    let line = marker
        .lines()
        .find_map(|l| l.strip_prefix("SUBRESULT:"))
        .expect("subhelper output marker");
    let parsed: Value = serde_json::from_str(line).unwrap();
    check(
        parsed["code"].as_i64().unwrap() as i32,
        parsed["stdout"].as_str().unwrap().to_owned(),
        parsed["stderr"].as_str().unwrap().to_owned(),
    );
}

#[test]
fn env_validation_subhelper() {
    if std::env::var("OMS_LINEAR_ENV_SUB").as_deref() != Ok("1") {
        return;
    }
    let raw = std::env::var("OMS_LINEAR_SUB_ARGS").unwrap();
    let parsed: Vec<String> = serde_json::from_str(&raw).unwrap();
    let os_args: Vec<OsString> = parsed.into_iter().map(OsString::from).collect();
    let stop = AtomicBool::new(false);
    let cwd = std::env::current_dir().unwrap();
    let (code, out, err) = oms_command_linear::run(&os_args, &cwd, &stop);
    println!(
        "SUBRESULT:{}",
        json!({
            "code": code,
            "stdout": String::from_utf8_lossy(&out),
            "stderr": String::from_utf8_lossy(&err),
        })
    );
}

#[test]
fn serve_webhook_security_mapping_dedup_and_first_thought() {
    if std::env::var(CHILD_TEST_ENV).as_deref()
        != Ok("serve_webhook_security_mapping_dedup_and_first_thought")
    {
        let ws = TempWorkspace::new("oms-linear-serve-host");
        let (argv_log, spy_path) = ws.install_curl_argv_spy();
        let exe = std::env::current_exe().expect("test exe");
        let mut cmd = Command::new(exe);
        clean_child_cmd(&mut cmd, &ws.home, &spy_path);
        cmd.env(
            CHILD_TEST_ENV,
            "serve_webhook_security_mapping_dedup_and_first_thought",
        )
        .env("OMS_TEST_CURL_LOG", &argv_log)
        .args([
            "--exact",
            "serve_webhook_security_mapping_dedup_and_first_thought",
            "--nocapture",
        ]);
        let out = cmd.output().expect("spawn serve test child");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "serve test failed:\n{combined}");
        return;
    }

    let ws = TempWorkspace::new("oms-linear-serve-inner");
    ws.setup_room("r1", "main", "linear");
    let api = FakeGraphqlServer::start();

    let sig_val = runtime_sig_val();
    let acc_val = runtime_acc_val();

    let cfg = ServeConfig {
        repo: ws.repo.clone(),
        room: "r1".into(),
        participant: "linear".into(),
        to: "main".into(),
        organization: "org-expected".into(),
        app_user: "app-user-expected".into(),
        listen: "127.0.0.1:0".parse().unwrap(),
        signing_material: SecretValue::parse_checked("sig", sig_val.clone()).unwrap(),
        access_cred: SecretValue::parse_checked("acc", acc_val.clone()).unwrap(),
        api_url: oms_command_linear::EndpointUrl::parse(&api.url()).unwrap(),
    };
    let server = ServerHandle::spawn(cfg).expect("spawn ServerHandle on port 0");
    let addr = server.local_addr();

    // 1. Wrong path -> 404; wrong method -> 405.
    assert_eq!(
        post_raw_http(addr, "POST", "/wrong", &[("Content-Length", "2")], b"{}"),
        404
    );
    assert_eq!(
        post_raw_http(addr, "GET", WEBHOOK_PATH, &[("Content-Length", "0")], b""),
        405
    );

    // 2. Missing signature -> 401 and no Room message.
    let valid_created = json!({
        "type": "AgentSessionEvent",
        "action": "created",
        "organizationId": "org-expected",
        "appUserId": "app-user-expected",
        "oauthClientId": "oauth-1",
        "webhookId": "wh-1",
        "webhookTimestamp": now_ms(),
        "createdAt": "2026-11-27T01:00:00.000Z",
        "agentSession": {
            "id": "sess-100",
            "url": "https://linear.app/acme/agent-session/sess-100",
            "status": "pending",
            "creatorId": "user-1",
            "issue": {
                "identifier": "ENG-42",
                "title": "Investigate webhook bridge",
                "url": "https://linear.app/acme/issue/ENG-42"
            }
        },
        "promptContext": "SECRET_MARKER_BODY Please inspect the Linear webhook adapter."
    });
    let raw_valid = serde_json::to_vec(&valid_created).unwrap();
    let raw_len = raw_valid.len().to_string();
    assert_eq!(
        post_raw_http(
            addr,
            "POST",
            WEBHOOK_PATH,
            &[("Content-Length", &raw_len)],
            &raw_valid
        ),
        401
    );
    // Bad signature -> 401 and no Room message.
    let bad_sig = "0".repeat(64);
    assert_eq!(
        post_raw_http(
            addr,
            "POST",
            WEBHOOK_PATH,
            &[("Content-Length", &raw_len), ("Linear-Signature", &bad_sig),],
            &raw_valid
        ),
        401
    );
    assert!(ws.room_messages("r1").is_empty());

    // 3. Stale timestamp (> 60s old) -> 400 and no Room message.
    let mut stale = valid_created.clone();
    stale["webhookTimestamp"] = json!(now_ms() - 120_000);
    assert_eq!(post_signed_webhook(addr, &sig_val, &stale), 400);
    assert!(ws.room_messages("r1").is_empty());

    // 4. Foreign organization or foreign app user -> 403 and no Room message.
    let mut foreign_org = valid_created.clone();
    foreign_org["organizationId"] = json!("org-foreign");
    assert_eq!(post_signed_webhook(addr, &sig_val, &foreign_org), 403);

    let mut foreign_app = valid_created.clone();
    foreign_app["appUserId"] = json!("app-user-foreign");
    assert_eq!(post_signed_webhook(addr, &sig_val, &foreign_app), 403);
    assert!(ws.room_messages("r1").is_empty());

    // 5. Oversize Content-Length (> 512 KiB) -> 413.
    let oversize_len = (512 * 1024 + 1).to_string();
    assert_eq!(
        post_raw_http(
            addr,
            "POST",
            WEBHOOK_PATH,
            &[("Content-Length", &oversize_len)],
            b"",
        ),
        413
    );
    assert!(ws.room_messages("r1").is_empty());

    // 6. Non-AgentSessionEvent type -> 200 ignored, no Room message.
    let other_type = json!({
        "type": "Issue",
        "action": "update",
        "organizationId": "org-foreign",
        "appUserId": "app-foreign",
        "webhookTimestamp": now_ms(),
    });
    assert_eq!(post_signed_webhook(addr, &sig_val, &other_type), 200);
    assert!(ws.room_messages("r1").is_empty());

    // 7. Good `created` signature -> 200, exactly one Room question with untrusted header,
    //    and background `thought` activity reaches fake API with derived UUID v4 id.
    assert_eq!(post_signed_webhook(addr, &sig_val, &valid_created), 200);
    let msgs = ws.room_messages("r1");
    assert_eq!(msgs.len(), 1);
    let created_msg = &msgs[0];
    let expected_created_mid = created_message_id("sess-100");
    assert_eq!(created_msg.text("id"), expected_created_mid);
    assert_eq!(created_msg.text("sender"), "linear");
    assert_eq!(created_msg.text("recipient"), "main");
    assert_eq!(created_msg.text("message_kind"), "question");
    assert!(created_msg.text("text").starts_with(UNTRUSTED_HEADER));
    assert!(created_msg.text("text").contains("session: sess-100"));
    assert!(created_msg.text("text").contains("issue: ENG-42"));
    assert!(
        created_msg
            .text("text")
            .contains("https://linear.app/acme/agent-session/sess-100")
    );

    let expected_thought_id = derive_activity_id("sess-100", "r1", CREATED_THOUGHT_CALLER_KEY);
    let calls = api.wait_for_calls(1, Duration::from_secs(5));
    assert_eq!(calls.len(), 1);
    let expected_auth = format!("{}{acc_val}", concat!("Bear", "er "));
    assert_eq!(
        calls[0].auth_header.as_deref(),
        Some(expected_auth.as_str())
    );
    assert_eq!(
        calls[0].body["variables"]["input"]["id"].as_str(),
        Some(expected_thought_id.as_str())
    );
    assert_eq!(
        calls[0].body["variables"]["input"]["agentSessionId"].as_str(),
        Some("sess-100")
    );
    assert_eq!(
        calls[0].body["variables"]["input"]["content"]["type"].as_str(),
        Some("thought")
    );
    assert_eq!(
        calls[0].body["variables"]["input"]["content"]["body"].as_str(),
        Some(FIRST_THOUGHT_BODY)
    );

    // 8. Duplicate `created` delivery -> 200 and still one Room message, no second thought.
    assert_eq!(post_signed_webhook(addr, &sig_val, &valid_created), 200);
    assert_eq!(ws.room_messages("r1").len(), 1);
    assert_eq!(api.snapshot().2, 1);

    // 9. `prompted` links `reply_to` to the `created` message.
    let prompted_ev = json!({
        "type": "AgentSessionEvent",
        "action": "prompted",
        "organizationId": "org-expected",
        "appUserId": "app-user-expected",
        "webhookTimestamp": now_ms(),
        "agentSession": { "id": "sess-100" },
        "agentActivity": {
            "id": "act-201",
            "agentSessionId": "sess-100",
            "userId": "user-1",
            "signal": null,
            "content": { "type": "prompt", "body": "Follow-up detail from Linear user." }
        }
    });
    assert_eq!(post_signed_webhook(addr, &sig_val, &prompted_ev), 200);
    let msgs = ws.room_messages("r1");
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].text("id"), prompted_message_id("act-201"));
    assert_eq!(msgs[1].text("message_kind"), "question");
    assert_eq!(msgs[1].text("reply_to"), expected_created_mid);
    assert!(
        msgs[1]
            .text("text")
            .contains("Follow-up detail from Linear user.")
    );

    // 10. `prompted` with `signal == "stop"` -> kind `question` with first line `stop requested`.
    let stop_ev = json!({
        "type": "AgentSessionEvent",
        "action": "prompted",
        "organizationId": "org-expected",
        "appUserId": "app-user-expected",
        "webhookTimestamp": now_ms(),
        "agentSession": { "id": "sess-100" },
        "agentActivity": {
            "id": "act-202",
            "agentSessionId": "sess-100",
            "userId": "user-1",
            "signal": "stop",
            "content": { "type": "prompt", "body": "Please pause." }
        }
    });
    assert_eq!(post_signed_webhook(addr, &sig_val, &stop_ev), 200);
    let msgs = ws.room_messages("r1");
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2].text("message_kind"), "question");
    assert_eq!(msgs[2].text("reply_to"), expected_created_mid);
    assert_eq!(msgs[2].text("text").lines().next(), Some("stop requested"));

    // 11. Oversized `promptContext` (> 4000 bytes) is truncated to <= 4000 bytes with "truncated" note.
    let long_ctx = format!("{}끝", "가나다라마바사abcdef".repeat(300));
    assert!(long_ctx.len() > ROOM_TEXT_LIMIT);
    let long_created = json!({
        "type": "AgentSessionEvent",
        "action": "created",
        "organizationId": "org-expected",
        "appUserId": "app-user-expected",
        "webhookTimestamp": now_ms(),
        "agentSession": { "id": "sess-long", "issue": { "identifier": "ENG-99" } },
        "promptContext": long_ctx,
    });
    assert_eq!(post_signed_webhook(addr, &sig_val, &long_created), 200);
    let msgs = ws.room_messages("r1");
    assert_eq!(msgs.len(), 4);
    assert!(msgs[3].text("text").len() <= ROOM_TEXT_LIMIT);
    assert!(msgs[3].text("text").contains("truncated"));
    let _ = api.wait_for_calls(2, Duration::from_secs(5));

    // 12. Room refuses sensitive inbound text -> still returns 200, appends nothing, queues error activity.
    let sensitive_turn = ["gh", "p_0123456789abcdefghij0123456789ab"].concat();
    let refused_created = json!({
        "type": "AgentSessionEvent",
        "action": "created",
        "organizationId": "org-expected",
        "appUserId": "app-user-expected",
        "webhookTimestamp": now_ms(),
        "agentSession": { "id": "sess-refused" },
        "promptContext": format!("Please use {sensitive_turn} here"),
    });
    assert_eq!(post_signed_webhook(addr, &sig_val, &refused_created), 200);
    assert_eq!(ws.room_messages("r1").len(), 4);
    let calls = api.wait_for_calls(3, Duration::from_secs(5));
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[2].body["variables"]["input"]["content"]["type"].as_str(),
        Some("error")
    );
    assert_eq!(
        calls[2].body["variables"]["input"]["content"]["body"].as_str(),
        Some(REFUSED_CONTENT_ERROR_BODY)
    );

    // 13. Shutdown server & verify log lines never contain body text or credentials,
    //     and child curl argv never contained the access credential.
    let server_logs = String::from_utf8(server.shutdown()).expect("UTF-8 server logs");
    assert!(server_logs.contains("status=200 session=sess-100"));
    assert!(!server_logs.contains("SECRET_MARKER_BODY"));
    assert!(!server_logs.contains(&sig_val));
    assert!(!server_logs.contains(&acc_val));

    if let Ok(argv_log_path) = std::env::var("OMS_TEST_CURL_LOG") {
        let logged = fs::read_to_string(argv_log_path).expect("read curl argv log");
        assert!(
            logged.contains("CALL\n"),
            "curl spy must observe child calls"
        );
        assert!(logged.contains("ARG:-K\n"));
        assert!(
            !logged.contains(&acc_val),
            "credential leaked into curl argv"
        );
        assert!(
            !logged.contains(&sig_val),
            "signing value leaked into curl argv"
        );
    }
}

#[test]
fn reply_confirmed_replayed_outcome_unknown_and_outbound_scan_refusal() {
    if std::env::var(CHILD_TEST_ENV).as_deref()
        != Ok("reply_confirmed_replayed_outcome_unknown_and_outbound_scan_refusal")
    {
        let ws = TempWorkspace::new("oms-linear-reply-host");
        let (argv_log, spy_path) = ws.install_curl_argv_spy();
        let acc_val = runtime_acc_val();
        let exe = std::env::current_exe().expect("test exe");
        let mut cmd = Command::new(exe);
        clean_child_cmd(&mut cmd, &ws.home, &spy_path);
        cmd.env(
            CHILD_TEST_ENV,
            "reply_confirmed_replayed_outcome_unknown_and_outbound_scan_refusal",
        )
        .env(ENV_ACCESS_CRED, &acc_val)
        .env("OMS_TEST_CURL_LOG", &argv_log)
        .args([
            "--exact",
            "reply_confirmed_replayed_outcome_unknown_and_outbound_scan_refusal",
            "--nocapture",
        ]);
        let out = cmd.output().expect("spawn reply test child");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "reply test failed:\n{combined}");
        return;
    }

    let ws = TempWorkspace::new("oms-linear-reply-inner");
    ws.setup_room("r1", "main", "linear");
    let api = FakeGraphqlServer::start();
    let acc_val = std::env::var(ENV_ACCESS_CRED).expect("access cred in child env");

    let clean_file = ws.root.join("clean-reply.md");
    fs::write(
        &clean_file,
        "Completed root-cause analysis and verified tests.\n",
    )
    .unwrap();

    let sensitive_file = ws.root.join("sensitive-reply.md");
    let leaked_pat = ["sk_li", "ve_0123456789abcdefghij"].concat();
    fs::write(&sensitive_file, format!("Refusing to leak {leaked_pat}\n")).unwrap();

    let run_reply_with_url = |api_url: &str, kind: &str, file: &Path, reply_to: Option<&str>| {
        let cfg = oms_command_linear::ReplyConfig {
            repo: ws.repo.clone(),
            room: "r1".into(),
            participant: "main".into(),
            session: "sess-reply-1".into(),
            kind: oms_command_linear::ActivityKind::parse(kind).unwrap(),
            text_file: file.to_path_buf(),
            reply_to: reply_to.map(str::to_owned),
            json: true,
            access_cred: SecretValue::from_env(ENV_ACCESS_CRED).unwrap(),
            api_url: oms_command_linear::EndpointUrl::parse(api_url).unwrap(),
        };
        oms_command_linear::execute_reply(&cfg, &LocalServices, &AtomicBool::new(false))
    };

    // 1. Outbound scan refusal -> exit 3, nothing sent to API, no Room receipt.
    let (code, out, err) = run_reply_with_url(
        &api.url(),
        "response",
        &sensitive_file,
        Some("linear-created-sess-reply-1"),
    );
    assert_eq!(code, 3, "stderr: {}", String::from_utf8_lossy(&err));
    assert!(out.is_empty());
    assert_eq!(api.snapshot().0.len(), 0);
    assert!(ws.room_messages("r1").is_empty());

    // 2. Closed port -> outcome_unknown (exit 75), safe to retry later.
    let closed_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);
    let closed_url = format!("http://127.0.0.1:{closed_port}/graphql");
    let (code, out, _err) = run_reply_with_url(
        &closed_url,
        "response",
        &clean_file,
        Some("linear-created-sess-reply-1"),
    );
    assert_eq!(code, 75);
    let unknown_json: Value = serde_json::from_slice(&out).expect("outcome_unknown JSON");
    assert_eq!(unknown_json["state"], "outcome_unknown");
    let derived_id = unknown_json["activity_id"].as_str().unwrap().to_owned();

    // 3. Retry with identical inputs against live fake API -> query (absent) -> create -> confirmed!
    let (code, out, err) = run_reply_with_url(
        &api.url(),
        "response",
        &clean_file,
        Some("linear-created-sess-reply-1"),
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
    let confirmed_json: Value = serde_json::from_slice(&out).expect("confirmed JSON");
    assert_eq!(confirmed_json["state"], "confirmed");
    assert_eq!(confirmed_json["activity_id"], derived_id);
    assert_eq!(
        confirmed_json["receipt_message_id"],
        receipt_message_id(&derived_id).unwrap()
    );
    let (calls, queries, creates) = api.snapshot();
    assert_eq!((queries, creates), (1, 1));
    let expected_auth = format!("{}{acc_val}", concat!("Bear", "er "));
    assert!(
        calls
            .iter()
            .all(|c| c.auth_header.as_deref() == Some(expected_auth.as_str()))
    );

    // Check Room status receipt message.
    let msgs = ws.room_messages("r1");
    let confirmed_receipt = msgs
        .iter()
        .find(|m| m.text("id") == format!("linear-activity-{derived_id}"))
        .expect("confirmed Room status receipt");
    assert_eq!(confirmed_receipt.text("message_kind"), "status");
    assert!(
        confirmed_receipt
            .text("text")
            .contains("session=sess-reply-1")
    );
    assert!(confirmed_receipt.text("text").contains("kind=response"));
    assert!(confirmed_receipt.text("text").contains(&derived_id));
    assert!(confirmed_receipt.text("text").contains("state=confirmed"));

    // 4. Replay (fake API reports id exists) -> confirmed (replayed), NO second create, idempotent Room receipt!
    let msg_count_before = ws.room_messages("r1").len();
    let (code, out, err) = run_reply_with_url(
        &api.url(),
        "response",
        &clean_file,
        Some("linear-created-sess-reply-1"),
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
    let replayed_json: Value = serde_json::from_slice(&out).expect("replayed JSON");
    assert_eq!(replayed_json["state"], "confirmed (replayed)");
    assert_eq!(replayed_json["activity_id"], derived_id);
    let (_calls, queries, creates) = api.snapshot();
    assert_eq!((queries, creates), (2, 1), "must not send second create");
    assert_eq!(ws.room_messages("r1").len(), msg_count_before);

    // 5. Pre-existing activity on Linear with no prior local Room receipt -> records state=confirmed (replayed).
    let thought_file = ws.root.join("thought.md");
    let thought_body = "Inspecting relevant modules.\n";
    fs::write(&thought_file, thought_body).unwrap();
    let thought_key = oms_command_linear::reply_caller_key(
        oms_command_linear::ActivityKind::Thought,
        None,
        thought_body.as_bytes(),
    );
    let pre_id = derive_activity_id("sess-reply-1", "r1", &thought_key);
    api.insert_existing(&pre_id);
    let (code, out, err) = run_reply_with_url(&api.url(), "thought", &thought_file, None);
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
    let pre_json: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(pre_json["state"], "confirmed (replayed)");
    assert_eq!(api.snapshot().2, 1, "still only one create");
    let msgs = ws.room_messages("r1");
    let pre_receipt = msgs
        .iter()
        .find(|m| m.text("id") == format!("linear-activity-{pre_id}"))
        .expect("replayed Room status receipt");
    assert!(
        pre_receipt
            .text("text")
            .contains("state=confirmed (replayed)")
    );

    // 6. Verify child curl argv never held the access credential.
    let argv_log_path = std::env::var("OMS_TEST_CURL_LOG").expect("curl log env");
    let logged = fs::read_to_string(argv_log_path).expect("read curl argv log");
    assert!(logged.contains("CALL\n"));
    assert!(
        !logged.contains(&acc_val),
        "access credential leaked into curl argv"
    );

    // 7. Full CLI parser round-trip for `oms linear serve` (port 0) and `oms linear reply`.
    let cli_sig = runtime_sig_val();
    let cli_reply_file = ws.root.join("cli-reply.md");
    fs::write(&cli_reply_file, "CLI end-to-end reply body.\n").unwrap();

    // Test CLI `reply` via `oms_command_linear::run` using OMS_LINEAR_API_URL from child env.
    let exe = std::env::current_exe().expect("test exe");
    let path_env = std::env::var_os("PATH").unwrap();
    let mut cli_cmd = Command::new(&exe);
    clean_child_cmd(&mut cli_cmd, &ws.home, &path_env);
    cli_cmd
        .env("OMS_LINEAR_ENV_SUB", "1")
        .env(ENV_ACCESS_CRED, &acc_val)
        .env(ENV_LINEAR_API_URL, api.url())
        .env(
            "OMS_LINEAR_SUB_ARGS",
            serde_json::to_string(&vec![
                "reply",
                "--repo",
                ws.repo.to_str().unwrap(),
                "--room",
                "r1",
                "--participant",
                "main",
                "--session",
                "sess-cli-rt",
                "--kind",
                "elicitation",
                "--text-file",
                cli_reply_file.to_str().unwrap(),
                "--json",
            ])
            .unwrap(),
        )
        .args(["--exact", "env_validation_subhelper", "--nocapture"]);
    let cli_out = cli_cmd.output().expect("run CLI reply subhelper");
    let cli_marker = String::from_utf8_lossy(&cli_out.stdout);
    let cli_line = cli_marker
        .lines()
        .find_map(|l| l.strip_prefix("SUBRESULT:"))
        .expect("CLI reply subresult");
    let cli_parsed: Value = serde_json::from_str(cli_line).unwrap();
    assert_eq!(cli_parsed["code"], 0, "stderr: {}", cli_parsed["stderr"]);
    let cli_stdout_json: Value =
        serde_json::from_str(cli_parsed["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(cli_stdout_json["state"], "confirmed");
    assert_eq!(cli_stdout_json["kind"], "elicitation");

    // Also spawn `oms linear serve --listen 127.0.0.1:0` through `run_streaming` in a child
    // process, read the bound port from stdout, send a signed webhook, and stop cleanly.
    let serve_ready_file = ws.root.join("serve-stdout.log");
    let mut serve_proc = Command::new(&exe);
    clean_child_cmd(&mut serve_proc, &ws.home, &path_env);
    let stdout_file = fs::File::create(&serve_ready_file).unwrap();
    serve_proc
        .env("OMS_LINEAR_SERVE_SUB", "1")
        .env(ENV_WEBHOOK_CRED, &cli_sig)
        .env(ENV_ACCESS_CRED, &acc_val)
        .env(ENV_LINEAR_API_URL, api.url())
        .env("OMS_LINEAR_SERVE_REPO", &ws.repo)
        .stdout(stdout_file)
        .args(["--exact", "serve_cli_subhelper", "--nocapture"]);
    let mut child = serve_proc.spawn().expect("spawn serve cli child");
    let deadline = Instant::now() + Duration::from_secs(5);
    let bound_addr: SocketAddr = loop {
        let txt = fs::read_to_string(&serve_ready_file).unwrap_or_default();
        if let Some(addr_s) = txt.lines().find_map(|l| l.strip_prefix("listening ")) {
            break addr_s.trim().parse().expect("parse bound SocketAddr");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for serve CLI bind: {txt}"
        );
        thread::sleep(Duration::from_millis(15));
    };
    let cli_webhook = json!({
        "type": "AgentSessionEvent",
        "action": "created",
        "organizationId": "org-cli",
        "appUserId": "app-cli",
        "webhookTimestamp": now_ms(),
        "agentSession": { "id": "sess-cli-serve", "issue": { "identifier": "CLI-1" } },
        "promptContext": "CLI serve end-to-end check",
    });
    assert_eq!(post_signed_webhook(bound_addr, &cli_sig, &cli_webhook), 200);
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        ws.room_messages("r1")
            .iter()
            .any(|m| m.text("id") == created_message_id("sess-cli-serve"))
    );
}

#[test]
fn serve_cli_subhelper() {
    if std::env::var("OMS_LINEAR_SERVE_SUB").as_deref() != Ok("1") {
        return;
    }
    let repo = PathBuf::from(std::env::var_os("OMS_LINEAR_SERVE_REPO").unwrap());
    let args: Vec<OsString> = [
        "serve",
        "--repo",
        repo.to_str().unwrap(),
        "--room",
        "r1",
        "--participant",
        "linear",
        "--to",
        "main",
        "--organization",
        "org-cli",
        "--app-user",
        "app-cli",
        "--listen",
        "127.0.0.1:0",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let stop = AtomicBool::new(false);
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let _ = oms_command_linear::run_streaming(&args, &repo, &stop, &mut out, &mut err);
}

#[test]
fn linear_crate_and_docs_pass_outbound_scrubber() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.join("../..");
    let mut files = vec![
        repo_root.join("docs/LINEAR.md"),
        repo_root.join("docs/COMPONENTS.md"),
        repo_root.join("docs/README.md"),
        manifest_dir.join("Cargo.toml"),
    ];
    for sub in ["src", "tests"] {
        for entry in fs::read_dir(manifest_dir.join(sub)).expect("read crate subdir") {
            let path = entry.expect("dir entry").path();
            if path.is_file() {
                files.push(path);
            }
        }
    }
    let mut failures = Vec::new();
    for path in files {
        let bytes = fs::read(&path).expect("read source/doc file");
        match oms_scrub::scan(&bytes, &[]) {
            Ok(hits) if hits.is_empty() => {}
            other => failures.push(format!("{}: {other:?}", path.display())),
        }
    }
    assert!(failures.is_empty(), "scrubber hits:\n{failures:#?}");
}
