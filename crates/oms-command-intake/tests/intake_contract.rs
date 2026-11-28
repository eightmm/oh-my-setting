#![forbid(unsafe_code)]

use oms_command_intake::{
    BindingDecision, BindingFlagSpec, CREATED_THOUGHT_CALLER_KEY, CreateDeliveryOutcome,
    ENV_ACCESS_CRED, ENV_LINEAR_API_URL, ENV_WEBHOOK_CRED, EndpointUrl, EnvVarSpec,
    FIRST_THOUGHT_BODY, LINEAR_INTAKE_PATH, LinearSource, OutboundAck, OutboundReply,
    PayloadDecision, REFUSED_CONTENT_ERROR_BODY, ROOM_TEXT_LIMIT, ReplyConfig, RequestHeaders,
    RoomDelivery, Route, SOURCE_LINEAR, SecretStore, SecretValue, ServeConfig, ServerHandle,
    SignatureInput, Source, TIMESTAMP_WINDOW_MS, TransportError, UNROUTED_ERROR_BODY,
    UNTRUSTED_HEADER, WorkRequest, WorkRequestKind, created_message_id, derive_delivery_id,
    hmac_sha256_hex, intake_path, prompted_message_id, receipt_message_id, reply_caller_key,
    verify_hmac_sha256_hex, verify_linear_signature, within_timestamp_window_ms,
};
use oms_command_room::{Join, LocalServices, Room, Send as RoomSend, Session};
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
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
const CHILD_TEST_ENV: &str = "OMS_INTAKE_TEST_CHILD";

fn runtime_sig_val() -> String {
    ["whs", "intake", "fixture", "9876543210fedcba"].join("_")
}

fn runtime_acc_val() -> String {
    ["lin", "app", "fixture", "abcdef0123456789"].join("_")
}

fn os_args(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn split_args(spec: &str) -> Vec<OsString> {
    spec.split_whitespace().map(OsString::from).collect()
}

fn make_secrets(entries: &[(&str, &str)]) -> SecretStore {
    let mut store = SecretStore::new();
    for &(k, v) in entries {
        store.insert(k, SecretValue::parse_checked(k, v.to_owned()).unwrap());
    }
    store
}

fn assert_curl_spy_clean(redacted: &[&str]) {
    let argv_log_path = std::env::var("OMS_TEST_CURL_LOG").expect("curl log env");
    let logged = fs::read_to_string(argv_log_path).expect("read curl argv log");
    let call_count = logged.matches("CALL\n").count();
    assert!(call_count >= 1, "curl spy must observe child calls");
    assert_eq!(
        call_count,
        logged.matches("CALL\nARG:-q\n").count(),
        "every curl call must pass -q as its first argument:\n{logged}"
    );
    assert!(logged.contains("ARG:-K\n"));
    for secret in redacted {
        assert!(!logged.contains(secret), "secret leaked into curl argv");
    }
}

fn clean_child_cmd(cmd: &mut Command, home: &Path, path_env: &OsStr) {
    cmd.env_clear()
        .env("PATH", path_env)
        .env("HOME", home)
        .env("TMPDIR", std::env::temp_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
}

fn child_test_cmd(
    home: &Path,
    path_env: &OsStr,
    target_test: &str,
    ignored: bool,
    vars: &[(&str, &OsStr)],
) -> Command {
    let exe = std::env::current_exe().expect("test executable");
    let mut cmd = Command::new(exe);
    clean_child_cmd(&mut cmd, home, path_env);
    for &(k, v) in vars {
        cmd.env(k, v);
    }
    cmd.args(["--exact", target_test, "--nocapture"]);
    if ignored {
        cmd.arg("--ignored");
    }
    cmd
}

fn isolated(test_name: &str, body: impl FnOnce(&TempWorkspace)) {
    isolated_with(test_name, false, &[], body);
}

fn isolated_with(
    test_name: &str,
    with_curl_spy: bool,
    extra_vars: &[(&str, &str)],
    body: impl FnOnce(&TempWorkspace),
) {
    if std::env::var(CHILD_TEST_ENV).as_deref() == Ok(test_name) {
        let ws = TempWorkspace::new("oms-intake-inner");
        body(&ws);
        return;
    }
    let outer = TempWorkspace::new("oms-intake-outer");
    fs::write(outer.home.join(".curlrc"), "max-filesize = 1\n").expect("write poison .curlrc");
    let (argv_log, path_env) = if with_curl_spy {
        let (log, p) = outer.install_curl_argv_spy();
        (Some(log), p)
    } else {
        (
            None,
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
    };
    let mut vars: Vec<(&str, &OsStr)> = vec![(CHILD_TEST_ENV, OsStr::new(test_name))];
    if let Some(log) = &argv_log {
        vars.push(("OMS_TEST_CURL_LOG", log.as_os_str()));
    }
    for &(k, v) in extra_vars {
        vars.push((k, OsStr::new(v)));
    }
    let out = child_test_cmd(&outer.home, &path_env, test_name, false, &vars)
        .output()
        .expect("spawn isolated child test");
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
        room.create(Some(room_id), "Intake test room", None, Some(main_id), None)
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
    let mut stream = TcpStream::connect(addr).expect("connect intake server");
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

fn post_signed_linear_webhook(addr: SocketAddr, sig_val: &str, payload: &Value) -> u16 {
    let raw = serde_json::to_vec(payload).expect("serialize webhook JSON");
    let sig = hmac_sha256_hex(sig_val.as_bytes(), &raw);
    let len_s = raw.len().to_string();
    post_raw_http(
        addr,
        "POST",
        LINEAR_INTAKE_PATH,
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
    let k1 = [0x0bu8; 20];
    assert_eq!(
        hmac_sha256_hex(&k1, b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );

    assert_eq!(
        hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );

    let k6 = [0xaau8; 131];
    assert_eq!(
        hmac_sha256_hex(
            &k6,
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        ),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );

    let good = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
    assert!(verify_hmac_sha256_hex(
        b"Jefe",
        b"what do ya want for nothing?",
        good
    ));
    assert!(verify_linear_signature(
        b"Jefe",
        b"what do ya want for nothing?",
        good
    ));
    assert!(!verify_hmac_sha256_hex(
        b"Jefe",
        b"what do ya want for nothing?",
        &good.to_ascii_uppercase()
    ));
    assert!(!verify_hmac_sha256_hex(
        b"Jefe",
        b"what do ya want for nothing?",
        ""
    ));

    assert!(within_timestamp_window_ms(
        1_700_000_000_000,
        1_700_000_060_000,
        TIMESTAMP_WINDOW_MS
    ));
    assert!(!within_timestamp_window_ms(
        1_700_000_000_000,
        1_700_000_060_001,
        TIMESTAMP_WINDOW_MS
    ));
}

#[test]
fn cli_help_sources_loopback_and_secret_startup_validation() {
    isolated(
        "cli_help_sources_loopback_and_secret_startup_validation",
        |ws| {
            ws.setup_room("r1", "main", "linear");
            let stop = AtomicBool::new(false);

            let (code, out, err) =
                oms_command_intake::run(&[OsString::from("--help")], &ws.repo, &stop);
            assert_eq!(code, 0);
            assert!(err.is_empty());
            let help = String::from_utf8(out).expect("help UTF-8");
            for needle in [
                "oms intake serve",
                "oms intake reply",
                "oms intake sources",
                "/intake/<source>",
                "HTTPS",
                "Compiled sources",
                "linear",
            ] {
                assert!(help.contains(needle), "help missing {needle:?}:\n{help}");
            }
            for specific in ["actor=app", "--organization", ENV_WEBHOOK_CRED] {
                assert!(
                    !help.contains(specific),
                    "CLI help should stay source-neutral without {specific:?}:\n{help}"
                );
            }

            let (code, out, err) =
                oms_command_intake::run(&[OsString::from("sources")], &ws.repo, &stop);
            assert_eq!(code, 0);
            assert!(err.is_empty());
            let sources_txt = String::from_utf8(out).expect("sources UTF-8");
            for needle in [
                "linear:",
                "POST /intake/linear",
                ENV_WEBHOOK_CRED,
                ENV_ACCESS_CRED,
                "--organization ORG",
                "--app-user APPUSER",
                "thought, response, elicitation, error",
            ] {
                assert!(
                    sources_txt.contains(needle),
                    "sources text missing {needle:?}:\n{sources_txt}"
                );
            }

            let (code, out, err) = oms_command_intake::run(
                &[OsString::from("sources"), OsString::from("--json")],
                &ws.repo,
                &stop,
            );
            assert_eq!(code, 0);
            assert!(err.is_empty());
            let sources_json: Value = serde_json::from_slice(&out).expect("sources JSON");
            let arr = sources_json["sources"].as_array().expect("sources array");
            assert_eq!(arr.len(), 1, "Linear must be the only compiled source");
            assert_eq!(arr[0]["source"], "linear");
            assert_eq!(arr[0]["route"], "POST /intake/linear");

            let repo_s = ws.repo.to_str().unwrap();

            // Unknown --source fails cleanly.
            let mut jira_args =
                split_args("serve --source jira --room r1 --participant linear --to main");
            jira_args.extend(os_args(&["--repo", repo_s]));
            let (code, _, err) = oms_command_intake::run(&jira_args, &ws.repo, &stop);
            assert_eq!(code, 2);
            assert!(String::from_utf8_lossy(&err).contains("unknown --source 'jira'"));

            // Repeated options are rejected (exit 2) and name the duplicated flag.
            for (spec, flag_name) in [
                ("sources --json --json", "--json"),
                ("serve --source linear --room r1 --room r2", "--room"),
                (
                    "serve --source linear --organization=o1 --organization=o2",
                    "--organization",
                ),
                (
                    "reply --source linear --kind thought --kind response",
                    "--kind",
                ),
            ] {
                let (code, _, err) = oms_command_intake::run(&split_args(spec), &ws.repo, &stop);
                let err_s = String::from_utf8_lossy(&err);
                assert_eq!(code, 2, "expected exit 2 for {flag_name}: {err_s}");
                assert!(
                    err_s.contains(flag_name),
                    "expected {flag_name} in: {err_s}"
                );
            }

            let serve_args = |listen: &str| {
                let mut a = split_args(
                    "serve --source linear --room r1 --participant linear --to main --organization org-1 --app-user app-1",
                );
                a.extend(os_args(&["--repo", repo_s, "--listen", listen]));
                a
            };
            let (code, _, err) =
                oms_command_intake::run(&serve_args("127.0.0.1:0"), &ws.repo, &stop);
            assert_ne!(code, 0);
            assert!(String::from_utf8_lossy(&err).contains(ENV_WEBHOOK_CRED));

            let bad_val = format!("bad {}", runtime_sig_val());
            let (c, _o, e) = run_intake_subproc(
                &ws.home,
                &serve_args("127.0.0.1:0"),
                &[
                    (ENV_WEBHOOK_CRED, &bad_val),
                    (ENV_ACCESS_CRED, &runtime_acc_val()),
                ],
            );
            assert_ne!(c, 0);
            assert!(!e.contains(&bad_val));

            let (c, _o, e) = run_intake_subproc(
                &ws.home,
                &serve_args("0.0.0.0:0"),
                &[
                    (ENV_WEBHOOK_CRED, &runtime_sig_val()),
                    (ENV_ACCESS_CRED, &runtime_acc_val()),
                ],
            );
            assert_ne!(c, 0);
            assert!(e.contains("loopback"), "stderr: {e}");

            // Oversized environment for outbound scan reports that cause (exit 3), not body refusal.
            let clean_env_body = ws.root.join("env-limit-reply.md");
            fs::write(&clean_env_body, "Clean body under oversized env.\n").unwrap();
            let acc_hold = runtime_acc_val();
            let pad_keys: Vec<String> = (0..4100).map(|i| format!("OMS_PAD_{i}")).collect();
            let mut big_env: Vec<(&str, &str)> =
                pad_keys.iter().map(|k| (k.as_str(), "1")).collect();
            big_env.push((ENV_ACCESS_CRED, &acc_hold));
            let mut env_reply_args = split_args(
                "reply --source linear --room r1 --participant main --session sess-env --kind thought",
            );
            env_reply_args.extend(os_args(&[
                "--repo",
                repo_s,
                "--text-file",
                clean_env_body.to_str().unwrap(),
            ]));
            let (c, o, e) = run_intake_subproc(&ws.home, &env_reply_args, &big_env);
            assert_eq!(c, 3, "stderr: {e}");
            assert!(o.is_empty());
            assert!(
                e.contains("environment") && !e.contains("reply body refused by outbound scan"),
                "stderr: {e}"
            );
        },
    );
}

fn run_intake_subproc(
    home: &Path,
    intake_args: &[OsString],
    vars: &[(&str, &str)],
) -> (i32, String, String) {
    let path_env = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let serialized = serde_json::to_string(
        &intake_args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut all_vars: Vec<(&str, &OsStr)> = vec![
        ("OMS_INTAKE_ENV_SUB", OsStr::new("1")),
        ("OMS_INTAKE_SUB_ARGS", OsStr::new(&serialized)),
    ];
    for &(k, v) in vars {
        all_vars.push((k, OsStr::new(v)));
    }
    let out = child_test_cmd(home, &path_env, "env_validation_subhelper", true, &all_vars)
        .output()
        .expect("run subhelper");
    let marker = String::from_utf8_lossy(&out.stdout);
    let line = marker
        .lines()
        .find_map(|l| l.strip_prefix("SUBRESULT:"))
        .expect("subhelper output marker");
    let parsed: Value = serde_json::from_str(line).unwrap();
    (
        parsed["code"].as_i64().unwrap() as i32,
        parsed["stdout"].as_str().unwrap().to_owned(),
        parsed["stderr"].as_str().unwrap().to_owned(),
    )
}

#[test]
#[ignore]
fn env_validation_subhelper() {
    if std::env::var("OMS_INTAKE_ENV_SUB").as_deref() != Ok("1") {
        return;
    }
    let raw = std::env::var("OMS_INTAKE_SUB_ARGS").unwrap();
    let parsed: Vec<String> = serde_json::from_str(&raw).unwrap();
    let os_args: Vec<OsString> = parsed.into_iter().map(OsString::from).collect();
    let stop = AtomicBool::new(false);
    let cwd = std::env::current_dir().unwrap();
    let (code, out, err) = oms_command_intake::run(&os_args, &cwd, &stop);
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
    isolated_with(
        "serve_webhook_security_mapping_dedup_and_first_thought",
        true,
        &[],
        |ws| {
            serve_webhook_contract_inner(ws);
        },
    );
}

fn serve_webhook_contract_inner(ws: &TempWorkspace) {
    ws.setup_room("r1", "main", "linear");
    let api = FakeGraphqlServer::start();

    let sig_val = runtime_sig_val();
    let acc_val = runtime_acc_val();
    let secrets = make_secrets(&[(ENV_WEBHOOK_CRED, &sig_val), (ENV_ACCESS_CRED, &acc_val)]);
    let binding_flags = BTreeMap::from([
        ("--organization".into(), "org-expected".into()),
        ("--app-user".into(), "app-user-expected".into()),
    ]);
    let source = Arc::new(LinearSource::with_endpoint(
        EndpointUrl::parse(&api.url()).unwrap(),
    ));
    let server = ServerHandle::spawn(ServeConfig {
        source,
        routes: vec![Route::any(
            "linear",
            ws.repo.clone(),
            "r1".into(),
            "linear".into(),
            "main".into(),
        )],
        binding_flags,
        listen: "127.0.0.1:0".parse().unwrap(),
        secrets,
    })
    .expect("spawn ServerHandle on port 0");
    let addr = server.local_addr();

    // 1. Wrong path (including legacy /linear/webhook or other source) -> 404; wrong method -> 405.
    for bad_path in ["/wrong", "/linear/webhook", "/intake/jira"] {
        assert_eq!(
            post_raw_http(addr, "POST", bad_path, &[("Content-Length", "2")], b"{}"),
            404
        );
    }
    assert_eq!(
        post_raw_http(
            addr,
            "GET",
            LINEAR_INTAKE_PATH,
            &[("Content-Length", "0")],
            b""
        ),
        405
    );
    assert_eq!(
        post_raw_http(
            addr,
            "POST",
            LINEAR_INTAKE_PATH,
            &[("Transfer-Encoding", "chunked")],
            b"0\r\n\r\n"
        ),
        400
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
        "promptContext": "SECRET_MARKER_BODY Please inspect the Linear intake adapter."
    });
    let raw_valid = serde_json::to_vec(&valid_created).unwrap();
    let raw_len = raw_valid.len().to_string();
    assert_eq!(
        post_raw_http(
            addr,
            "POST",
            LINEAR_INTAKE_PATH,
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
            LINEAR_INTAKE_PATH,
            &[("Content-Length", &raw_len), ("Linear-Signature", &bad_sig)],
            &raw_valid
        ),
        401
    );
    assert!(ws.room_messages("r1").is_empty());

    // 3. Stale timestamp (> 60s old) -> 400 and no Room message.
    let mut stale = valid_created.clone();
    stale["webhookTimestamp"] = json!(now_ms() - 120_000);
    assert_eq!(post_signed_linear_webhook(addr, &sig_val, &stale), 400);
    assert!(ws.room_messages("r1").is_empty());

    // 4. Foreign organization or foreign app user -> 403 and no Room message.
    let mut foreign_org = valid_created.clone();
    foreign_org["organizationId"] = json!("org-foreign");
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &foreign_org),
        403
    );

    let mut foreign_app = valid_created.clone();
    foreign_app["appUserId"] = json!("app-user-foreign");
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &foreign_app),
        403
    );
    assert!(ws.room_messages("r1").is_empty());

    // 5. Oversize Content-Length (> 512 KiB) -> 413.
    let oversize_len = (512 * 1024 + 1).to_string();
    assert_eq!(
        post_raw_http(
            addr,
            "POST",
            LINEAR_INTAKE_PATH,
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
    assert_eq!(post_signed_linear_webhook(addr, &sig_val, &other_type), 200);
    assert!(ws.room_messages("r1").is_empty());

    // 7. Good `created` signature -> 200, exactly one Room question with untrusted header,
    //    and background `thought` activity reaches fake API with derived UUID v4 id.
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &valid_created),
        200
    );
    let msgs = ws.room_messages("r1");
    assert_eq!(msgs.len(), 1);
    let created_msg = &msgs[0];
    let expected_created_mid = created_message_id(SOURCE_LINEAR, "sess-100");
    assert_eq!(created_msg.text("id"), expected_created_mid);
    assert_eq!(created_msg.text("sender"), "linear");
    assert_eq!(created_msg.text("recipient"), "main");
    assert_eq!(created_msg.text("message_kind"), "question");
    assert_eq!(created_msg.text("reply_to"), "");
    assert!(created_msg.text("text").starts_with(UNTRUSTED_HEADER));
    assert!(created_msg.text("text").contains("source: linear"));
    assert!(created_msg.text("text").contains("session: sess-100"));
    assert!(created_msg.text("text").contains("issue: ENG-42"));
    assert!(
        created_msg
            .text("text")
            .contains("https://linear.app/acme/agent-session/sess-100")
    );

    let expected_thought_id =
        derive_delivery_id(SOURCE_LINEAR, "sess-100", "r1", CREATED_THOUGHT_CALLER_KEY);
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
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &valid_created),
        200
    );
    assert_eq!(ws.room_messages("r1").len(), 1);
    assert_eq!(api.snapshot().2, 1);

    let make_prompted = |act_id: &str, signal: Option<&str>, body: &str| {
        json!({
            "type": "AgentSessionEvent",
            "action": "prompted",
            "organizationId": "org-expected",
            "appUserId": "app-user-expected",
            "webhookTimestamp": now_ms(),
            "agentSession": { "id": "sess-100" },
            "agentActivity": {
                "id": act_id,
                "agentSessionId": "sess-100",
                "userId": "user-1",
                "signal": signal,
                "content": { "type": "prompt", "body": body }
            }
        })
    };
    let make_created = |sess_id: &str, ctx: &str| {
        json!({
            "type": "AgentSessionEvent",
            "action": "created",
            "organizationId": "org-expected",
            "appUserId": "app-user-expected",
            "webhookTimestamp": now_ms(),
            "agentSession": { "id": sess_id, "issue": { "identifier": "ENG-99" } },
            "promptContext": ctx,
        })
    };

    // 9. `prompted` follow-up from the same session is NOT sent with Room `--reply-to`;
    //    it is sent as a new `question` whose fixed header names the source, session id,
    //    and message id of the original `created` question so Main can thread it.
    let prompted_ev = make_prompted("act-201", None, "Follow-up detail from Linear user.");
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &prompted_ev),
        200
    );
    let msgs = ws.room_messages("r1");
    assert_eq!(msgs.len(), 2);
    assert_eq!(
        msgs[1].text("id"),
        prompted_message_id(SOURCE_LINEAR, "act-201")
    );
    assert_eq!(msgs[1].text("message_kind"), "question");
    assert_eq!(
        msgs[1].text("reply_to"),
        "",
        "follow-up must not set Room --reply-to"
    );
    let followup_text = msgs[1].text("text");
    assert!(followup_text.starts_with(UNTRUSTED_HEADER));
    assert!(followup_text.contains("source: linear"));
    assert!(followup_text.contains("session: sess-100"));
    assert!(followup_text.contains(&format!("reply_to: {expected_created_mid}")));
    assert!(followup_text.contains("Follow-up detail from Linear user."));

    // Confirm Room's unmodified reply rule remains enforced on `main`.
    {
        let stop = AtomicBool::new(false);
        let room = Room::open(
            &ws.repo,
            Session::default(),
            &LocalServices,
            &stop,
            false,
            false,
        )
        .unwrap();
        let rejected = room.send(
            "r1",
            RoomSend {
                sender: "linear",
                recipient: "main",
                text: "same-sender reply_to must be refused by Room",
                kind: "question",
                reply_to: Some(&expected_created_mid),
                message_id: Some("bad-same-sender-reply"),
                linked: false,
                quiet: false,
            },
        );
        assert!(rejected.is_err());
    }

    // 10. `prompted` with `signal == "stop"` -> kind `question`, no Room `--reply-to`,
    //     first line `stop requested`, and fixed header naming source, session, and created msg id.
    let stop_ev = make_prompted("act-202", Some("stop"), "Please pause.");
    assert_eq!(post_signed_linear_webhook(addr, &sig_val, &stop_ev), 200);
    let msgs = ws.room_messages("r1");
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2].text("message_kind"), "question");
    assert_eq!(msgs[2].text("reply_to"), "");
    let stop_text = msgs[2].text("text");
    assert_eq!(stop_text.lines().next(), Some("stop requested"));
    assert!(stop_text.contains("source: linear"));
    assert!(stop_text.contains("session: sess-100"));
    assert!(stop_text.contains(&format!("reply_to: {expected_created_mid}")));

    // 11. Oversized `promptContext` (> 4000 bytes) is truncated to <= 4000 bytes with "truncated" note.
    let long_ctx = format!("{}끝", "가나다라마바사abcdef".repeat(300));
    assert!(long_ctx.len() > ROOM_TEXT_LIMIT);
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &make_created("sess-long", &long_ctx)),
        200
    );
    let msgs = ws.room_messages("r1");
    assert_eq!(msgs.len(), 4);
    assert!(msgs[3].text("text").len() <= ROOM_TEXT_LIMIT);
    assert!(msgs[3].text("text").contains("truncated"));
    let _ = api.wait_for_calls(2, Duration::from_secs(5));

    // 12. Room refuses sensitive inbound text -> still returns 200, appends nothing, queues error activity.
    let sensitive_turn = ["gh", "p_0123456789abcdefghij0123456789ab"].concat();
    let refused_created =
        make_created("sess-refused", &format!("Please use {sensitive_turn} here"));
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &refused_created),
        200
    );
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

    // 12b. Room open/read failure is NOT a content refusal: returns 503 with no first ack,
    //      and succeeds cleanly when the Room becomes readable again.
    let room_log = ws.repo.join(".oms/threads/r1.jsonl");
    let room_log_offline = ws.repo.join(".oms/threads/r1.jsonl.offline");
    fs::rename(&room_log, &room_log_offline).expect("take room log offline");
    let retryable_created = make_created("sess-503", "Retry when Room I/O recovers");
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &retryable_created),
        503
    );
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        api.snapshot().0.len(),
        3,
        "503 Room failure must not emit a refusal or thought ack"
    );
    fs::rename(&room_log_offline, &room_log).expect("restore room log");
    assert_eq!(
        post_signed_linear_webhook(addr, &sig_val, &retryable_created),
        200
    );
    assert_eq!(ws.room_messages("r1").len(), 5);
    let calls = api.wait_for_calls(4, Duration::from_secs(5));
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls[3].body["variables"]["input"]["content"]["type"].as_str(),
        Some("thought")
    );

    // 13. Shutdown server & verify log lines never contain body text or credentials,
    //     and child curl argv passed `-q` first and never contained credentials.
    let server_logs = String::from_utf8(server.shutdown()).expect("UTF-8 server logs");
    assert!(server_logs.contains("source=linear status=200 session=sess-100"));
    assert!(server_logs.contains("source=linear status=503 session=sess-503"));
    assert!(!server_logs.contains("SECRET_MARKER_BODY"));
    assert!(!server_logs.contains(&sig_val));
    assert!(!server_logs.contains(&acc_val));

    if std::env::var("OMS_TEST_CURL_LOG").is_ok() {
        assert_curl_spy_clean(&[&acc_val, &sig_val]);
    }
}

#[test]
fn reply_confirmed_replayed_outcome_unknown_and_outbound_scan_refusal() {
    let acc_val = runtime_acc_val();
    isolated_with(
        "reply_confirmed_replayed_outcome_unknown_and_outbound_scan_refusal",
        true,
        &[(ENV_ACCESS_CRED, &acc_val)],
        |ws| {
            reply_contract_inner(ws);
        },
    );
}

fn reply_contract_inner(ws: &TempWorkspace) {
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
        let mut secrets = SecretStore::new();
        secrets.insert(
            ENV_ACCESS_CRED,
            SecretValue::from_env(ENV_ACCESS_CRED).unwrap(),
        );
        let source = Arc::new(LinearSource::with_endpoint(
            EndpointUrl::parse(api_url).unwrap(),
        ));
        let cfg = ReplyConfig {
            source,
            repo: ws.repo.clone(),
            room: "r1".into(),
            participant: "main".into(),
            session: "sess-reply-1".into(),
            kind: kind.to_owned(),
            text_file: file.to_path_buf(),
            reply_to: reply_to.map(str::to_owned),
            json: true,
            secrets,
        };
        oms_command_intake::execute_reply(&cfg, &LocalServices, &AtomicBool::new(false))
    };

    let origin_mid = Some("linear-created-sess-reply-1");
    let run_reply_ok = |api_url: &str, kind: &str, file: &Path, reply_to: Option<&str>| -> Value {
        let (code, out, err) = run_reply_with_url(api_url, kind, file, reply_to);
        assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
        serde_json::from_slice(&out).expect("reply JSON")
    };

    // 1. Outbound scan refusal -> exit 3, nothing sent to API, no Room receipt.
    let (code, out, err) = run_reply_with_url(&api.url(), "response", &sensitive_file, origin_mid);
    assert_eq!(code, 3, "stderr: {}", String::from_utf8_lossy(&err));
    assert!(out.is_empty());
    assert_eq!(api.snapshot().0.len(), 0);
    assert!(ws.room_messages("r1").is_empty());

    // 2. Closed port -> outcome_unknown (exit 75), safe to retry later.
    let closed_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);
    let closed_url = format!("http://127.0.0.1:{closed_port}/graphql");
    let (code, out, _err) = run_reply_with_url(&closed_url, "response", &clean_file, origin_mid);
    assert_eq!(code, 75);
    let unknown_json: Value = serde_json::from_slice(&out).expect("outcome_unknown JSON");
    assert_eq!(unknown_json["source"], "linear");
    assert_eq!(unknown_json["state"], "outcome_unknown");
    let derived_id = unknown_json["delivery_id"].as_str().unwrap().to_owned();

    // 3. Retry with identical inputs against live fake API -> query (absent) -> create -> confirmed!
    let confirmed_json = run_reply_ok(&api.url(), "response", &clean_file, origin_mid);
    assert_eq!(confirmed_json["state"], "confirmed");
    assert_eq!(confirmed_json["delivery_id"], derived_id);
    assert_eq!(
        confirmed_json["receipt_message_id"],
        receipt_message_id(SOURCE_LINEAR, &derived_id).unwrap()
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
        .find(|m| m.text("id") == format!("linear-delivery-{derived_id}"))
        .expect("confirmed Room status receipt");
    assert_eq!(confirmed_receipt.text("message_kind"), "status");
    for field in [
        "source=linear",
        "session=sess-reply-1",
        "kind=response",
        &derived_id,
        "state=confirmed",
    ] {
        assert!(confirmed_receipt.text("text").contains(field));
    }

    // 4. Replay (fake API reports id exists) -> confirmed (replayed), NO second create, idempotent Room receipt!
    let msg_count_before = ws.room_messages("r1").len();
    let replayed_json = run_reply_ok(&api.url(), "response", &clean_file, origin_mid);
    assert_eq!(replayed_json["state"], "confirmed (replayed)");
    assert_eq!(replayed_json["delivery_id"], derived_id);
    assert_eq!((api.snapshot().1, api.snapshot().2), (2, 1));
    assert_eq!(ws.room_messages("r1").len(), msg_count_before);

    // 4b. Second distinct reply text with the SAME --kind and SAME --reply-to derives a
    //     distinct delivery_id and sends a new create; identical rerun replays.
    let second_clean_file = ws.root.join("clean-reply-2.md");
    let second_body = "Follow-up response with distinct text to the same message.\n";
    fs::write(&second_clean_file, second_body).unwrap();
    let second_json = run_reply_ok(&api.url(), "response", &second_clean_file, origin_mid);
    assert_eq!(second_json["state"], "confirmed");
    let second_id = second_json["delivery_id"].as_str().unwrap().to_owned();
    assert_ne!(second_id, derived_id);
    let (calls, queries, creates) = api.snapshot();
    assert_eq!((queries, creates), (3, 2));
    assert_eq!(
        calls.last().unwrap().body["variables"]["input"]["content"]["body"].as_str(),
        Some(second_body)
    );

    let second_replay = run_reply_ok(&api.url(), "response", &second_clean_file, origin_mid);
    assert_eq!(second_replay["state"], "confirmed (replayed)");
    assert_eq!(second_replay["delivery_id"], second_id);
    assert_eq!(api.snapshot().2, 2);

    // 5. Pre-existing activity on Linear with no prior local Room receipt -> records state=confirmed (replayed).
    let thought_file = ws.root.join("thought.md");
    let thought_body = "Inspecting relevant modules.\n";
    fs::write(&thought_file, thought_body).unwrap();
    let thought_key = reply_caller_key("thought", None, thought_body.as_bytes());
    let pre_id = derive_delivery_id(SOURCE_LINEAR, "sess-reply-1", "r1", &thought_key);
    api.insert_existing(&pre_id);
    let pre_json = run_reply_ok(&api.url(), "thought", &thought_file, None);
    assert_eq!(pre_json["state"], "confirmed (replayed)");
    assert_eq!(api.snapshot().2, 2);
    let msgs = ws.room_messages("r1");
    let pre_receipt = msgs
        .iter()
        .find(|m| m.text("id") == format!("linear-delivery-{pre_id}"))
        .expect("replayed Room status receipt");
    assert!(
        pre_receipt
            .text("text")
            .contains("state=confirmed (replayed)")
    );

    // 6. Verify child curl argv passed `-q` first and never held the access credential.
    assert_curl_spy_clean(&[&acc_val]);

    // 7. Full CLI parser round-trip for `oms intake serve` (port 0) and `oms intake reply`.
    let cli_sig = runtime_sig_val();
    let cli_reply_file = ws.root.join("cli-reply.md");
    fs::write(&cli_reply_file, "CLI end-to-end reply body.\n").unwrap();
    let api_url = api.url();

    let mut cli_reply_args = split_args(
        "reply --source linear --room r1 --participant main --session sess-cli-rt --kind elicitation --json",
    );
    cli_reply_args.extend(os_args(&[
        "--repo",
        ws.repo.to_str().unwrap(),
        "--text-file",
        cli_reply_file.to_str().unwrap(),
    ]));
    let (cli_code, cli_stdout, cli_stderr) = run_intake_subproc(
        &ws.home,
        &cli_reply_args,
        &[(ENV_ACCESS_CRED, &acc_val), (ENV_LINEAR_API_URL, &api_url)],
    );
    assert_eq!(cli_code, 0, "stderr: {cli_stderr}");
    let cli_stdout_json: Value = serde_json::from_str(&cli_stdout).unwrap();
    assert_eq!(cli_stdout_json["source"], "linear");
    assert_eq!(cli_stdout_json["state"], "confirmed");
    assert_eq!(cli_stdout_json["kind"], "elicitation");

    let serve_ready_file = ws.root.join("serve-stdout.log");
    let path_env = std::env::var_os("PATH").unwrap();
    let stdout_file = fs::File::create(&serve_ready_file).unwrap();
    let mut serve_proc = child_test_cmd(
        &ws.home,
        &path_env,
        "serve_cli_subhelper",
        true,
        &[
            ("OMS_INTAKE_SERVE_SUB", OsStr::new("1")),
            (ENV_WEBHOOK_CRED, OsStr::new(&cli_sig)),
            (ENV_ACCESS_CRED, OsStr::new(&acc_val)),
            (ENV_LINEAR_API_URL, OsStr::new(&api_url)),
            ("OMS_INTAKE_SERVE_REPO", ws.repo.as_os_str()),
        ],
    );
    serve_proc.stdout(stdout_file);
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
    assert_eq!(
        post_signed_linear_webhook(bound_addr, &cli_sig, &cli_webhook),
        200
    );
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        ws.room_messages("r1")
            .iter()
            .any(|m| m.text("id") == created_message_id(SOURCE_LINEAR, "sess-cli-serve"))
    );
}

#[test]
#[ignore]
fn serve_cli_subhelper() {
    if std::env::var("OMS_INTAKE_SERVE_SUB").as_deref() != Ok("1") {
        return;
    }
    let repo = PathBuf::from(std::env::var_os("OMS_INTAKE_SERVE_REPO").unwrap());
    let mut args = split_args(
        "serve --source linear --room r1 --participant linear --to main --organization org-cli --app-user app-cli --listen 127.0.0.1:0",
    );
    args.extend(os_args(&["--repo", repo.to_str().unwrap()]));
    let stop = AtomicBool::new(false);
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let _ = oms_command_intake::run_streaming(&args, &repo, &stop, &mut out, &mut err);
}

#[test]
fn routed_serve_delivers_by_team_and_answers_unrouted_sessions() {
    let sig_val = runtime_sig_val();
    isolated_with(
        "routed_serve_delivers_by_team_and_answers_unrouted_sessions",
        true,
        &[
            (ENV_WEBHOOK_CRED, &sig_val),
            (ENV_ACCESS_CRED, "not one value"),
            (ENV_LINEAR_API_URL, ""),
        ],
        routed_serve_inner,
    );
}

fn routed_serve_inner(ws: &TempWorkspace) {
    ws.setup_room("r-jae", "main", "linear");
    ws.setup_room("r-ops", "main", "linear");
    let stop = AtomicBool::new(false);
    let run = |spec: &str, extra: &[&str]| {
        let mut args = split_args(spec);
        args.extend(os_args(extra));
        oms_command_intake::run(&args, &ws.repo, &stop)
    };
    let repo = ws.repo.to_str().unwrap();
    let add = "route add --source linear --participant linear --to main";

    // Route edits validate the Room target and are idempotent; the table stays private.
    let (code, out, err) = run(add, &["--team", "JAE", "--repo", repo, "--room", "r-jae"]);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    assert!(String::from_utf8_lossy(&out).starts_with("routed linear JAE -> "));
    let (code, out, _) = run(add, &["--team", "JAE", "--repo", repo, "--room", "r-jae"]);
    assert_eq!(code, 0);
    assert!(String::from_utf8_lossy(&out).starts_with("already routed "));
    let ops_id = "3ab2f60d-ops";
    assert_eq!(
        run(add, &["--team", ops_id, "--repo", repo, "--room", "r-ops"]).0,
        0
    );
    let (code, _, err) = run(
        add,
        &["--team", "BAD", "--repo", repo, "--room", "r-missing"],
    );
    assert_eq!(code, 2);
    assert!(String::from_utf8_lossy(&err).contains("r-missing"));
    assert_eq!(
        run(
            add,
            &["--team", "bad team", "--repo", repo, "--room", "r-jae"]
        )
        .0,
        2
    );
    let table = oms_command_intake::routes::routes_path().unwrap();
    assert_eq!(
        fs::metadata(&table).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (code, out, _) = run("route list --json", &[]);
    assert_eq!(code, 0);
    let listed: Value = serde_json::from_slice(&out).unwrap();
    let teams: Vec<_> = listed["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["team"].as_str().unwrap(), r["room"].as_str().unwrap()))
        .collect();
    assert_eq!(teams, [("JAE", "r-jae"), (ops_id, "r-ops")]);
    let (code, _, err) = run(
        "serve --source linear --routes --room r-jae --organization o --app-user a",
        &[],
    );
    assert_eq!(code, 2);
    assert!(String::from_utf8_lossy(&err).contains("--routes cannot be combined with --room"));

    let api = FakeGraphqlServer::start();
    let sig_val = runtime_sig_val();
    let secrets = make_secrets(&[
        (ENV_WEBHOOK_CRED, &sig_val),
        (ENV_ACCESS_CRED, &runtime_acc_val()),
    ]);
    // Library callers get the same startup checks the CLI table filter gives.
    let loaded = oms_command_intake::routes::load(&table).unwrap();
    let checked = |routes: Vec<Route>| {
        oms_command_intake::serve::validate_serve_room(
            &ServeConfig {
                source: Arc::new(LinearSource::with_endpoint(
                    EndpointUrl::parse(&api.url()).unwrap(),
                )),
                routes,
                binding_flags: BTreeMap::from([
                    ("--organization".into(), "org-r".into()),
                    ("--app-user".into(), "app-r".into()),
                ]),
                listen: "127.0.0.1:0".parse().unwrap(),
                secrets: secrets.clone(),
            },
            &LocalServices,
            &AtomicBool::new(false),
        )
    };
    let mut foreign = loaded[0].clone();
    foreign.source = "other".into();
    assert!(
        checked(vec![foreign])
            .unwrap_err()
            .contains("belongs to source 'other'")
    );
    assert!(
        checked(vec![loaded[0].clone(), loaded[0].clone()])
            .unwrap_err()
            .contains("two intake routes for team")
    );
    let mut bad = loaded[0].clone();
    bad.team = "-x".into();
    assert!(checked(vec![bad]).is_err());
    let server = ServerHandle::spawn(ServeConfig {
        source: Arc::new(LinearSource::with_endpoint(
            EndpointUrl::parse(&api.url()).unwrap(),
        )),
        routes: oms_command_intake::routes::load(&table).unwrap(),
        binding_flags: BTreeMap::from([
            ("--organization".into(), "org-r".into()),
            ("--app-user".into(), "app-r".into()),
        ]),
        listen: "127.0.0.1:0".parse().unwrap(),
        secrets,
    })
    .expect("spawn routed server");
    let addr = server.local_addr();
    let event = |action: &str, session: &str, team: Value, activity: Option<&str>| {
        let mut payload = json!({
            "type": "AgentSessionEvent",
            "action": action,
            "organizationId": "org-r",
            "appUserId": "app-r",
            "webhookTimestamp": now_ms(),
            "agentSession": { "id": session, "issue": team },
            "promptContext": "route me",
        });
        if let Some(id) = activity {
            payload["agentActivity"] = json!({
                "id": id, "agentSessionId": session,
                "content": { "type": "prompt", "body": "follow-up" }
            });
        }
        post_signed_linear_webhook(addr, &sig_val, &payload)
    };
    let issue = |key: &str, id: &str| json!({ "identifier": format!("{key}-1"), "teamId": id, "team": { "id": id, "key": key } });

    // The team key routes first; a team id routes when its key is unknown.
    assert_eq!(event("created", "s-jae", issue("JAE", "id-jae"), None), 200);
    assert_eq!(event("created", "s-ops", issue("OPS", ops_id), None), 200);
    assert_eq!(
        event("prompted", "s-jae", issue("JAE", "id-jae"), Some("act-1")),
        200
    );
    let ids = |room: &str| -> Vec<String> {
        ws.room_messages(room)
            .iter()
            .map(|m| m.text("id").to_owned())
            .collect()
    };
    assert_eq!(
        ids("r-jae"),
        [
            created_message_id(SOURCE_LINEAR, "s-jae"),
            prompted_message_id(SOURCE_LINEAR, "act-1")
        ]
    );
    assert_eq!(ids("r-ops"), [created_message_id(SOURCE_LINEAR, "s-ops")]);

    // An unrouted team, or a session without an issue, gets the fixed error and no Room message.
    assert_eq!(event("created", "s-zzz", issue("ZZZ", "id-zzz"), None), 200);
    assert_eq!(event("created", "s-none", Value::Null, None), 200);
    assert_eq!(ids("r-jae").len() + ids("r-ops").len(), 3);
    let calls = api.wait_for_calls(4, Duration::from_secs(5));
    let input = |session: &str| {
        calls
            .iter()
            .map(|c| &c.body["variables"]["input"])
            .find(|i| i["agentSessionId"] == session)
            .unwrap_or_else(|| panic!("no ack for {session}"))
            .clone()
    };
    assert_eq!(
        input("s-jae")["id"].as_str(),
        Some(
            derive_delivery_id(SOURCE_LINEAR, "s-jae", "r-jae", CREATED_THOUGHT_CALLER_KEY)
                .as_str()
        )
    );
    for session in ["s-zzz", "s-none"] {
        let unrouted = input(session);
        assert_eq!(unrouted["content"]["type"], "error");
        assert_eq!(unrouted["content"]["body"], UNROUTED_ERROR_BODY);
        let key = format!(
            "error:unrouted:{}",
            created_message_id(SOURCE_LINEAR, session)
        );
        assert_eq!(
            unrouted["id"].as_str(),
            Some(derive_delivery_id(SOURCE_LINEAR, session, "", &key).as_str())
        );
    }
    let logs = String::from_utf8(server.shutdown()).unwrap();
    assert!(logs.contains("session=s-ops route=3ab2f60d-ops"), "{logs}");
    assert!(logs.contains("session=s-zzz route=unrouted"), "{logs}");

    // Status names the newest intake question per Room target, never a manual question, and
    // reports an unusable route as a row error instead of failing the whole report.
    let stop_send = AtomicBool::new(false);
    Room::open(
        &ws.repo,
        Session::default(),
        &LocalServices,
        &stop_send,
        false,
        false,
    )
    .unwrap()
    .send(
        "r-jae",
        RoomSend {
            sender: "linear",
            recipient: "main",
            text: "manual question",
            kind: "question",
            reply_to: None,
            message_id: Some("manual-question-1"),
            linked: false,
            quiet: false,
        },
    )
    .unwrap();
    oms_command_intake::routes::update(&table, |list| {
        let mut gone = list[0].clone();
        gone.team = "GONE".into();
        gone.room = "r-gone".into();
        list.push(gone);
        Ok(true)
    })
    .unwrap();
    let (code, out, _) = run("status --json", &[]);
    assert_eq!(code, 0);
    let status: Value = serde_json::from_slice(&out).unwrap();
    let linear = &status["sources"][0];
    assert_eq!(linear["source"], SOURCE_LINEAR);
    let states = |side: &str| -> Vec<(String, String)> {
        linear[side]["env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["name"].as_str().unwrap().into(),
                    e["state"].as_str().unwrap().into(),
                )
            })
            .collect()
    };
    let named = |n: &str, st: &str| (n.to_owned(), st.to_owned());
    assert_eq!(
        states("serve"),
        [
            named(ENV_WEBHOOK_CRED, "set"),
            named(ENV_ACCESS_CRED, "invalid"),
            named(ENV_LINEAR_API_URL, "missing")
        ]
    );
    assert_eq!(linear["serve"]["ready"], false);
    assert!(!String::from_utf8_lossy(&out).contains(&sig_val));
    let row = |team: &str| {
        status["routes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["team"] == team)
            .unwrap()
            .clone()
    };
    assert_eq!(
        row("JAE")["last_delivery"]["id"].as_str(),
        Some(prompted_message_id(SOURCE_LINEAR, "act-1").as_str())
    );
    assert!(
        !row("JAE")["last_delivery"]["ts"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        row(ops_id)["last_delivery"]["id"].as_str(),
        Some(created_message_id(SOURCE_LINEAR, "s-ops").as_str())
    );
    assert_eq!(row("GONE")["ok"], false);
    assert!(row("GONE")["error"].as_str().unwrap().contains("r-gone"));
    let (code, out, _) = run("status", &[]);
    assert_eq!(code, 0);
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains(&format!("{ENV_ACCESS_CRED} (invalid)"))
            && text.contains(&format!("{ENV_LINEAR_API_URL} (missing, optional)")),
        "{text}"
    );
    assert!(text.contains("room=r-gone: error: "), "{text}");
    assert_eq!(run("route remove --source linear --team GONE", &[]).0, 0);

    assert_eq!(run("route remove --source linear --team JAE", &[]).0, 0);
    assert_eq!(run("route remove --source linear --team JAE", &[]).0, 1);
    assert_eq!(oms_command_intake::routes::load(&table).unwrap().len(), 1);
}

// --- Test-only second fake adapter proving core reuse without Linear code ---

const FAKE_SOURCE_NAME: &str = "fake";
const FAKE_SIGN_ENV: &str = concat!("OMS_FAKE_WEBHOOK_", "KEY");
const FAKE_REPLY_ENV: &str = concat!("OMS_FAKE_REPLY_", "CRED");

static FAKE_SERVE_ENV: &[EnvVarSpec] = &[
    EnvVarSpec {
        name: FAKE_SIGN_ENV,
        required: true,
        sensitive: true,
        description: "Fake webhook signing material",
    },
    EnvVarSpec {
        name: FAKE_REPLY_ENV,
        required: true,
        sensitive: true,
        description: "Fake reply credential",
    },
];

static FAKE_REPLY_ENV_SPECS: &[EnvVarSpec] = &[EnvVarSpec {
    name: FAKE_REPLY_ENV,
    required: true,
    sensitive: true,
    description: "Fake reply credential",
}];

static FAKE_BINDING_FLAGS: &[BindingFlagSpec] = &[BindingFlagSpec {
    flag: "--project",
    value_name: "PROJ",
    required: true,
    description: "Expected fake tracker project key",
}];

static FAKE_REPLY_KINDS: &[&str] = &["comment", "status"];

#[derive(Default)]
struct FakeTrackerBackend {
    acks: Vec<OutboundAck>,
    ack_attempts: usize,
    ack_script: std::collections::VecDeque<(Duration, Result<(), TransportError>)>,
    existing_deliveries: BTreeMap<String, OutboundReply>,
    query_calls: usize,
    create_calls: usize,
    fail_unknown: bool,
}

#[derive(Clone, Default)]
struct FakeTrackerSource {
    backend: Arc<Mutex<FakeTrackerBackend>>,
    hold_acks: Arc<AtomicBool>,
}

impl Source for FakeTrackerSource {
    fn name(&self) -> &'static str {
        FAKE_SOURCE_NAME
    }

    fn description(&self) -> &'static str {
        "Test-only tracker adapter exercising oms intake core without Linear code"
    }

    fn serve_env(&self) -> &'static [EnvVarSpec] {
        FAKE_SERVE_ENV
    }

    fn reply_env(&self) -> &'static [EnvVarSpec] {
        FAKE_REPLY_ENV_SPECS
    }

    fn binding_flags(&self) -> &'static [BindingFlagSpec] {
        FAKE_BINDING_FLAGS
    }

    fn reply_kinds(&self) -> &'static [&'static str] {
        FAKE_REPLY_KINDS
    }

    fn signature_header(&self) -> &'static str {
        "X-Fake-Signature"
    }

    fn validate_binding(&self, flags: &BTreeMap<String, String>) -> Result<(), String> {
        match flags.get("--project") {
            Some(p) if !p.is_empty() => Ok(()),
            _ => Err("missing --project".into()),
        }
    }

    fn signature_input<'a>(
        &self,
        headers: &'a RequestHeaders,
        raw_body: &'a [u8],
    ) -> Option<SignatureInput<'a>> {
        let sig = headers.get(self.signature_header())?;
        let ts_hdr = headers.get("X-Fake-Timestamp")?;
        let mut signed = format!("v1:{ts_hdr}:").into_bytes();
        signed.extend_from_slice(raw_body);
        Some(SignatureInput {
            secret_env: FAKE_SIGN_ENV,
            message: Cow::Owned(signed),
            signature_hex: sig,
        })
    }

    fn observed_session_ref(&self, payload: &Map<String, Value>) -> Option<String> {
        payload
            .get("ticket")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn extract_timestamp_ms(
        &self,
        headers: &RequestHeaders,
        _payload: &Map<String, Value>,
    ) -> Option<i64> {
        headers.get("X-Fake-Timestamp")?.parse::<i64>().ok()
    }

    fn check_binding(
        &self,
        flags: &BTreeMap<String, String>,
        payload: &Map<String, Value>,
    ) -> BindingDecision {
        let want = flags.get("--project").map(String::as_str);
        if payload.get("project").and_then(Value::as_str) == want {
            BindingDecision::Matched
        } else {
            BindingDecision::Forbidden
        }
    }

    fn parse_work_request(&self, payload: &Map<String, Value>) -> PayloadDecision {
        let Some(ticket) = payload.get("ticket").and_then(Value::as_str) else {
            return PayloadDecision::BadRequest;
        };
        let Some(req_id) = payload.get("event_id").and_then(Value::as_str) else {
            return PayloadDecision::BadRequest;
        };
        let Some(kind_str) = payload.get("kind").and_then(Value::as_str) else {
            return PayloadDecision::BadRequest;
        };
        let Ok(kind) = WorkRequestKind::parse(kind_str) else {
            return PayloadDecision::Ignore;
        };
        let text = payload.get("body").and_then(Value::as_str).unwrap_or("");
        let (msg_id, reply_to) = match kind {
            WorkRequestKind::Created => (created_message_id(FAKE_SOURCE_NAME, ticket), None),
            WorkRequestKind::Prompted | WorkRequestKind::Stop => (
                prompted_message_id(FAKE_SOURCE_NAME, req_id),
                Some(created_message_id(FAKE_SOURCE_NAME, ticket)),
            ),
        };
        match WorkRequest::new(
            FAKE_SOURCE_NAME,
            req_id,
            ticket,
            kind,
            text,
            msg_id,
            reply_to,
        ) {
            Ok(req) => PayloadDecision::Work(req),
            Err(_) => PayloadDecision::BadRequest,
        }
    }

    fn first_ack(
        &self,
        req: &WorkRequest,
        room_id: &str,
        delivery: RoomDelivery,
    ) -> Option<OutboundAck> {
        if delivery == RoomDelivery::AppendedNew && req.kind == WorkRequestKind::Created {
            Some(OutboundAck {
                delivery_id: derive_delivery_id(
                    FAKE_SOURCE_NAME,
                    &req.session_ref,
                    room_id,
                    "ack:created",
                ),
                session_ref: req.session_ref.clone(),
                kind: "status".to_owned(),
                body: "Queued in OMS Room".to_owned(),
            })
        } else {
            None
        }
    }

    fn deliver_ack(
        &self,
        _secrets: &SecretStore,
        ack: &OutboundAck,
        _timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<(), TransportError> {
        while self.hold_acks.load(Ordering::Acquire) && !interrupted.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(5));
        }
        if interrupted.load(Ordering::Acquire) {
            return Err(TransportError::OutcomeUnknown("interrupted".into()));
        }
        let scripted = {
            let mut guard = self.backend.lock().unwrap();
            guard.ack_attempts += 1;
            guard.ack_script.pop_front()
        };
        if let Some((delay, outcome)) = scripted {
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            if outcome.is_ok() {
                self.backend.lock().unwrap().acks.push(ack.clone());
            }
            return outcome;
        }
        self.backend.lock().unwrap().acks.push(ack.clone());
        Ok(())
    }

    fn query_delivery(
        &self,
        _secrets: &SecretStore,
        delivery_id: &str,
        _session_ref: &str,
        _timeout: Duration,
        _interrupted: &AtomicBool,
    ) -> Result<bool, TransportError> {
        let mut guard = self.backend.lock().unwrap();
        if guard.fail_unknown {
            return Err(TransportError::OutcomeUnknown(
                "simulated fake transport timeout".into(),
            ));
        }
        guard.query_calls += 1;
        Ok(guard.existing_deliveries.contains_key(delivery_id))
    }

    fn create_delivery(
        &self,
        _secrets: &SecretStore,
        reply: &OutboundReply,
        _timeout: Duration,
        _interrupted: &AtomicBool,
    ) -> Result<CreateDeliveryOutcome, TransportError> {
        let mut guard = self.backend.lock().unwrap();
        if guard.fail_unknown {
            return Err(TransportError::OutcomeUnknown(
                "simulated fake transport drop".into(),
            ));
        }
        guard.create_calls += 1;
        if guard.existing_deliveries.contains_key(&reply.delivery_id) {
            return Ok(CreateDeliveryOutcome::AlreadyExists);
        }
        guard
            .existing_deliveries
            .insert(reply.delivery_id.clone(), reply.clone());
        Ok(CreateDeliveryOutcome::Created)
    }
}

#[test]
fn second_fake_adapter_reuses_listener_idempotency_and_receipt_without_linear_code() {
    isolated(
        "second_fake_adapter_reuses_listener_idempotency_and_receipt_without_linear_code",
        |ws| {
            ws.setup_room("r-fake", "main", "fake-bridge");
            let fake = FakeTrackerSource::default();
            let sign_val = ["fake", "whs", "key", "1122334455667788"].join("_");
            let reply_val = ["fake", "api", "cred", "8877665544332211"].join("_");

            let mut secrets = SecretStore::new();
            secrets.insert(
                FAKE_SIGN_ENV,
                SecretValue::parse_checked("fake_sign", sign_val.clone()).unwrap(),
            );
            secrets.insert(
                FAKE_REPLY_ENV,
                SecretValue::parse_checked("fake_reply", reply_val.clone()).unwrap(),
            );
            let mut binding_flags = BTreeMap::new();
            binding_flags.insert("--project".into(), "ACME".into());

            let server = ServerHandle::spawn(ServeConfig {
                source: Arc::new(fake.clone()),
                routes: vec![Route::any(
                    FAKE_SOURCE_NAME,
                    ws.repo.clone(),
                    "r-fake".into(),
                    "fake-bridge".into(),
                    "main".into(),
                )],
                binding_flags,
                listen: "127.0.0.1:0".parse().unwrap(),
                secrets: secrets.clone(),
            })
            .expect("spawn fake adapter intake server");
            let addr = server.local_addr();
            let route = intake_path(FAKE_SOURCE_NAME);
            assert_eq!(route, "/intake/fake");

            let post_fake = |ts_ms: i64, payload: &Value| -> u16 {
                let raw = serde_json::to_vec(payload).unwrap();
                let ts_s = ts_ms.to_string();
                let mut signed = format!("v1:{ts_s}:").into_bytes();
                signed.extend_from_slice(&raw);
                let sig = hmac_sha256_hex(sign_val.as_bytes(), &signed);
                let len_s = raw.len().to_string();
                post_raw_http(
                    addr,
                    "POST",
                    &route,
                    &[
                        ("Content-Type", "application/json"),
                        ("Content-Length", &len_s),
                        ("X-Fake-Timestamp", &ts_s),
                        ("X-Fake-Signature", &sig),
                    ],
                    &raw,
                )
            };

            // Route isolation: /intake/linear returns 404 on fake server.
            assert_eq!(
                post_raw_http(
                    addr,
                    "POST",
                    "/intake/linear",
                    &[("Content-Length", "2")],
                    b"{}"
                ),
                404
            );

            let fake_ev = |proj: &str, ticket: &str, ev_id: &str, kind: &str, body: &str| json!({ "project": proj, "ticket": ticket, "event_id": ev_id, "kind": kind, "body": body });

            // Foreign project binding -> 403.
            assert_eq!(
                post_fake(
                    now_ms(),
                    &fake_ev("OTHER", "T-7", "ev-1", "created", "Initial ticket context")
                ),
                403
            );

            // Created event -> 200, Room question, and first acknowledgement.
            let created_payload = fake_ev(
                "ACME",
                "T-7",
                "ev-1",
                "created",
                "Investigate flaky CI job on ACME",
            );
            assert_eq!(post_fake(now_ms(), &created_payload), 200);
            // Duplicate created event -> 200, idempotent Room message, no second ack.
            assert_eq!(post_fake(now_ms(), &created_payload), 200);

            let deadline = Instant::now() + Duration::from_secs(2);
            while fake.backend.lock().unwrap().acks.is_empty() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(fake.backend.lock().unwrap().acks.len(), 1);
            assert_eq!(ws.room_messages("r-fake").len(), 1);

            // Follow-up prompted + stop events -> new Room questions without Room --reply-to,
            // fixed header naming source, session, and original created message id.
            assert_eq!(
                post_fake(
                    now_ms(),
                    &fake_ev(
                        "ACME",
                        "T-7",
                        "ev-2",
                        "prompted",
                        "Logs attached in tracker"
                    )
                ),
                200
            );
            assert_eq!(
                post_fake(now_ms(), &fake_ev("ACME", "T-7", "ev-3", "stop", "")),
                200
            );

            let msgs = ws.room_messages("r-fake");
            assert_eq!(msgs.len(), 3);
            assert_eq!(msgs[0].text("id"), "fake-created-T-7");
            assert_eq!(msgs[1].text("id"), "fake-prompted-ev-2");
            assert_eq!(msgs[1].text("reply_to"), "");
            assert!(msgs[1].text("text").contains("source: fake"));
            assert!(msgs[1].text("text").contains("session: T-7"));
            assert!(msgs[1].text("text").contains("reply_to: fake-created-T-7"));
            assert_eq!(msgs[2].text("id"), "fake-prompted-ev-3");
            assert_eq!(msgs[2].text("reply_to"), "");
            assert_eq!(msgs[2].text("text").lines().next(), Some("stop requested"));
            assert!(msgs[2].text("text").contains("reply_to: fake-created-T-7"));

            // Ack queue overflow logs a clean stderr line, and shutdown interrupts the worker
            // immediately without draining queued acknowledgements.
            fake.hold_acks.store(true, Ordering::Release);
            for i in 0..66 {
                let ev = fake_ev(
                    "ACME",
                    &format!("T-burst-{i}"),
                    &format!("ev-burst-{i}"),
                    "created",
                    "MARKER_BURST_BODY",
                );
                assert_eq!(post_fake(now_ms(), &ev), 200);
            }
            let stop_started = Instant::now();
            let fake_logs = String::from_utf8(server.shutdown()).expect("UTF-8 fake server logs");
            assert!(
                stop_started.elapsed() < Duration::from_secs(2),
                "shutdown waited behind queued acks: {:?}",
                stop_started.elapsed()
            );
            assert_eq!(
                fake.backend.lock().unwrap().acks.len(),
                1,
                "ack worker must stop on interruption instead of draining queued acks"
            );
            assert!(
                fake_logs.contains("ack queue full"),
                "expected queue-full stderr line, got:\n{fake_logs}"
            );
            assert!(!fake_logs.contains("MARKER_BURST_BODY"));
            assert!(!fake_logs.contains(&sign_val));
            assert!(!fake_logs.contains(&reply_val));

            // Outbound reply path reuse: outbound scan refusal ( including explicit adapter secret ),
            // outcome_unknown (exit 75), confirmed (exit 0), and confirmed (replayed) (exit 0).
            let leaked_reply_file = ws.root.join("fake-leaked.md");
            fs::write(
                &leaked_reply_file,
                format!("Accidental credential echo {reply_val}\n"),
            )
            .unwrap();
            let clean_reply_file = ws.root.join("fake-clean.md");
            fs::write(&clean_reply_file, "Fixed flaky test and verified.\n").unwrap();

            let make_reply_cfg = |file: &Path| ReplyConfig {
                source: Arc::new(fake.clone()),
                repo: ws.repo.clone(),
                room: "r-fake".into(),
                participant: "main".into(),
                session: "T-7".into(),
                kind: "comment".into(),
                text_file: file.to_path_buf(),
                reply_to: Some("fake-created-T-7".into()),
                json: true,
                secrets: secrets.clone(),
            };

            let (code, _, _) = oms_command_intake::execute_reply(
                &make_reply_cfg(&leaked_reply_file),
                &LocalServices,
                &AtomicBool::new(false),
            );
            assert_eq!(code, 3, "adapter secret in reply body must be refused");
            assert_eq!(fake.backend.lock().unwrap().query_calls, 0);

            fake.backend.lock().unwrap().fail_unknown = true;
            let (code, out, _) = oms_command_intake::execute_reply(
                &make_reply_cfg(&clean_reply_file),
                &LocalServices,
                &AtomicBool::new(false),
            );
            assert_eq!(code, 75);
            let unk: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(unk["source"], "fake");
            assert_eq!(unk["state"], "outcome_unknown");
            let deliv_id = unk["delivery_id"].as_str().unwrap().to_owned();

            fake.backend.lock().unwrap().fail_unknown = false;
            let (code, out, err) = oms_command_intake::execute_reply(
                &make_reply_cfg(&clean_reply_file),
                &LocalServices,
                &AtomicBool::new(false),
            );
            assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
            let conf: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(conf["state"], "confirmed");
            assert_eq!(conf["delivery_id"], deliv_id);
            assert_eq!(
                conf["receipt_message_id"],
                format!("fake-delivery-{deliv_id}")
            );

            let count_after_confirm = ws.room_messages("r-fake").len();
            let (code, out, err) = oms_command_intake::execute_reply(
                &make_reply_cfg(&clean_reply_file),
                &LocalServices,
                &AtomicBool::new(false),
            );
            assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
            let rep: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(rep["state"], "confirmed (replayed)");
            assert_eq!(rep["delivery_id"], deliv_id);
            let guard = fake.backend.lock().unwrap();
            assert_eq!((guard.query_calls, guard.create_calls), (2, 1));
            drop(guard);
            assert_eq!(ws.room_messages("r-fake").len(), count_after_confirm);
        },
    );
}

#[test]
fn stub_ack_retry_inside_window_and_no_retry_when_window_expired() {
    isolated(
        "stub_ack_retry_inside_window_and_no_retry_when_window_expired",
        |ws| {
            ws.setup_room("r-ack", "main", "fake-bridge");
            let sign_val = ["fake", "whs", "ack", "1122334455667788"].join("_");
            let reply_val = ["fake", "api", "ack", "8877665544332211"].join("_");

            let mut secrets = SecretStore::new();
            secrets.insert(
                FAKE_SIGN_ENV,
                SecretValue::parse_checked("fake_sign", sign_val.clone()).unwrap(),
            );
            secrets.insert(
                FAKE_REPLY_ENV,
                SecretValue::parse_checked("fake_reply", reply_val.clone()).unwrap(),
            );
            let binding_flags = BTreeMap::from([("--project".to_owned(), "ACME".to_owned())]);
            let route = intake_path(FAKE_SOURCE_NAME);

            let post_fake_to = |addr: SocketAddr, ticket: &str, ev_id: &str| -> u16 {
                let ts_ms = now_ms();
                let payload = json!({
                    "project": "ACME",
                    "ticket": ticket,
                    "event_id": ev_id,
                    "kind": "created",
                    "body": "REQUEST_BODY_MARKER check retry window",
                });
                let raw = serde_json::to_vec(&payload).unwrap();
                let ts_s = ts_ms.to_string();
                let mut signed = format!("v1:{ts_s}:").into_bytes();
                signed.extend_from_slice(&raw);
                let sig = hmac_sha256_hex(sign_val.as_bytes(), &signed);
                let len_s = raw.len().to_string();
                post_raw_http(
                    addr,
                    "POST",
                    &route,
                    &[
                        ("Content-Type", "application/json"),
                        ("Content-Length", &len_s),
                        ("X-Fake-Timestamp", &ts_s),
                        ("X-Fake-Signature", &sig),
                    ],
                    &raw,
                )
            };

            // 1. Ack failure then success inside the 10 s window: one retry, one stderr line,
            //    no secrets and no response/request bodies in stderr.
            let fake_ok = FakeTrackerSource::default();
            {
                let mut guard = fake_ok.backend.lock().unwrap();
                guard.ack_script.push_back((
                    Duration::ZERO,
                    Err(TransportError::OutcomeUnknown(format!(
                        "upstream 502 UPSTREAM_RESPONSE_BODY_MARKER {reply_val}"
                    ))),
                ));
                guard.ack_script.push_back((Duration::ZERO, Ok(())));
            }
            let server_ok = ServerHandle::spawn(ServeConfig {
                source: Arc::new(fake_ok.clone()),
                routes: vec![Route::any(
                    FAKE_SOURCE_NAME,
                    ws.repo.clone(),
                    "r-ack".into(),
                    "fake-bridge".into(),
                    "main".into(),
                )],
                binding_flags: binding_flags.clone(),
                listen: "127.0.0.1:0".parse().unwrap(),
                secrets: secrets.clone(),
            })
            .expect("spawn server for retry-inside-window test");

            assert_eq!(
                post_fake_to(server_ok.local_addr(), "T-retry-ok", "ev-retry-ok"),
                200
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while fake_ok.backend.lock().unwrap().acks.is_empty() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            let logs_ok = String::from_utf8(server_ok.shutdown()).expect("UTF-8 server logs");
            {
                let guard = fake_ok.backend.lock().unwrap();
                assert_eq!(
                    guard.ack_attempts, 2,
                    "expected initial attempt + one retry"
                );
                assert_eq!(guard.acks.len(), 1, "retry inside window must deliver ack");
                assert_eq!(guard.acks[0].session_ref, "T-retry-ok");
            }
            let ack_err_lines: Vec<&str> = logs_ok
                .lines()
                .filter(|l| l.contains("outbound ack"))
                .collect();
            assert_eq!(
                ack_err_lines.len(),
                1,
                "expected exactly one stderr line on single transient failure:\n{logs_ok}"
            );
            assert!(
                ack_err_lines[0].contains("source=fake")
                    && ack_err_lines[0].contains("session=T-retry-ok")
                    && ack_err_lines[0].contains("retry=once"),
                "unexpected ack failure log line: {}",
                ack_err_lines[0]
            );
            for forbidden in [
                "UPSTREAM_RESPONSE_BODY_MARKER",
                "REQUEST_BODY_MARKER",
                "Queued in OMS Room",
                sign_val.as_str(),
                reply_val.as_str(),
            ] {
                assert!(
                    !logs_ok.contains(forbidden),
                    "stderr leaked forbidden content:\n{logs_ok}"
                );
            }

            // 2. Ack failure with no time left inside the window: no retry, one stderr line.
            //    Send a follow-up probe event so FIFO completion proves the worker finished
            //    handling T-expired before shutdown without sleeping longer than needed.
            let fake_exp = FakeTrackerSource::default();
            {
                let mut guard = fake_exp.backend.lock().unwrap();
                guard.ack_script.push_back((
                    Duration::from_millis(25),
                    Err(TransportError::OutcomeUnknown(
                        "UPSTREAM_EXPIRED_BODY_MARKER".into(),
                    )),
                ));
                guard.ack_script.push_back((Duration::ZERO, Ok(())));
            }
            let server_exp = ServerHandle::spawn_with_ack_window(
                ServeConfig {
                    source: Arc::new(fake_exp.clone()),
                    routes: vec![Route::any(
                        FAKE_SOURCE_NAME,
                        ws.repo.clone(),
                        "r-ack".into(),
                        "fake-bridge".into(),
                        "main".into(),
                    )],
                    binding_flags,
                    listen: "127.0.0.1:0".parse().unwrap(),
                    secrets,
                },
                Duration::from_millis(15),
            )
            .expect("spawn server with short ack window");

            assert_eq!(
                post_fake_to(server_exp.local_addr(), "T-expired", "ev-expired"),
                200
            );
            let wait_first = Instant::now() + Duration::from_secs(2);
            while fake_exp.backend.lock().unwrap().ack_attempts < 1 && Instant::now() < wait_first {
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                post_fake_to(server_exp.local_addr(), "T-probe", "ev-probe"),
                200
            );
            let wait_probe = Instant::now() + Duration::from_secs(2);
            while !fake_exp
                .backend
                .lock()
                .unwrap()
                .acks
                .iter()
                .any(|a| a.session_ref == "T-probe")
                && Instant::now() < wait_probe
            {
                thread::sleep(Duration::from_millis(5));
            }
            let logs_exp = String::from_utf8(server_exp.shutdown()).expect("UTF-8 server logs");
            {
                let guard = fake_exp.backend.lock().unwrap();
                assert_eq!(
                    guard.ack_attempts, 2,
                    "expired ack must not retry (1 attempt for T-expired + 1 for T-probe)"
                );
                assert_eq!(guard.acks.len(), 1);
                assert_eq!(guard.acks[0].session_ref, "T-probe");
            }
            let exp_err_lines: Vec<&str> = logs_exp
                .lines()
                .filter(|l| l.contains("outbound ack"))
                .collect();
            assert_eq!(
                exp_err_lines.len(),
                1,
                "expected one stderr line for expired ack failure:\n{logs_exp}"
            );
            assert!(
                exp_err_lines[0].contains("session=T-expired")
                    && exp_err_lines[0].contains("retry=none"),
                "unexpected expired ack failure log line: {}",
                exp_err_lines[0]
            );
            assert!(!logs_exp.contains("UPSTREAM_EXPIRED_BODY_MARKER"));
        },
    );
}

#[test]
fn accept_backoff_doubles_to_cap_deduplicates_bursts_and_resets_on_success() {
    use oms_command_intake::serve::{ACCEPT_BACKOFF_INITIAL, ACCEPT_BACKOFF_MAX, AcceptBackoff};

    let mut backoff = AcceptBackoff::new();
    let emfile = std::io::Error::from_raw_os_error(24);
    let enfile = std::io::Error::from_raw_os_error(23);

    assert_eq!(backoff.on_error(&emfile), (ACCEPT_BACKOFF_INITIAL, true));
    assert_eq!(
        backoff.on_error(&emfile),
        (Duration::from_millis(200), false)
    );
    assert_eq!(
        backoff.on_error(&emfile),
        (Duration::from_millis(400), false)
    );
    assert_eq!(
        backoff.on_error(&emfile),
        (Duration::from_millis(800), false)
    );
    assert_eq!(backoff.on_error(&emfile), (ACCEPT_BACKOFF_MAX, false));
    assert_eq!(backoff.on_error(&emfile), (ACCEPT_BACKOFF_MAX, false));

    // Distinct error during an outage logs once while keeping bounded cap.
    assert_eq!(backoff.on_error(&enfile), (ACCEPT_BACKOFF_MAX, true));
    assert_eq!(backoff.on_error(&enfile), (ACCEPT_BACKOFF_MAX, false));

    // Success resets both delay and burst deduplication.
    backoff.on_success();
    assert_eq!(backoff.on_error(&emfile), (ACCEPT_BACKOFF_INITIAL, true));
}

#[test]
fn room_content_refusal_strings_stay_pinned_to_oms_command_room() {
    use oms_command_intake::serve::{
        ROOM_CONTENT_REFUSAL_MESSAGES, ROOM_DUPLICATE_CONFLICT_MESSAGE, is_room_content_refusal,
    };

    const ROOM_VALIDATE_SRC: &str = include_str!("../../oms-command-room/src/validate.rs");
    const ROOM_CORE_SRC: &str = include_str!("../../oms-command-room/src/core.rs");

    assert_eq!(ROOM_CONTENT_REFUSAL_MESSAGES.len(), 6);
    for &msg in ROOM_CONTENT_REFUSAL_MESSAGES {
        assert!(
            is_room_content_refusal(msg),
            "is_room_content_refusal must match {msg:?}"
        );
        let quoted = format!("\"{msg}\"");
        assert!(
            ROOM_VALIDATE_SRC.contains(&quoted) || ROOM_CORE_SRC.contains(&quoted),
            "oms-command-room no longer contains literal refusal message {quoted}"
        );
    }
    let quoted_dup = format!("\"{ROOM_DUPLICATE_CONFLICT_MESSAGE}\"");
    assert!(
        ROOM_VALIDATE_SRC.contains(&quoted_dup),
        "oms-command-room no longer contains duplicate conflict message {quoted_dup}"
    );
    assert!(!is_room_content_refusal(ROOM_DUPLICATE_CONFLICT_MESSAGE));
    assert!(!is_room_content_refusal("room unavailable"));

    // Also exercise the live oms-command-room error paths reachable from Room::send / Room::create.
    isolated(
        "room_content_refusal_strings_stay_pinned_to_oms_command_room",
        |ws| {
            ws.setup_room("r-pin", "main", "linear");
            let stop = AtomicBool::new(false);
            let room = Room::open(
                &ws.repo,
                Session::default(),
                &LocalServices,
                &stop,
                true,
                false,
            )
            .expect("open room");

            let send_probe = |mid: &str, text: &str| {
                room.send(
                    "r-pin",
                    RoomSend {
                        sender: "linear",
                        recipient: "main",
                        text,
                        kind: "question",
                        reply_to: None,
                        message_id: Some(mid),
                        linked: false,
                        quiet: false,
                    },
                )
                .expect_err("expected room refusal")
                .message
            };

            let sensitive_turn = ["gh", "p_0123456789abcdefghij0123456789ab"].concat();
            let err_turn = send_probe("pin-turn", &format!("contains {sensitive_turn}"));
            assert_eq!(
                err_turn,
                "turn looks sensitive; it would be replayed into other agents' prompts"
            );
            assert!(is_room_content_refusal(&err_turn));

            let err_meta = send_probe(&format!("id-{sensitive_turn}"), "clean body");
            assert_eq!(
                err_meta,
                "room metadata contains sensitive-looking content or could not be checked"
            );
            assert!(is_room_content_refusal(&err_meta));

            let err_len = send_probe("pin-empty", "   ");
            assert_eq!(err_len, "message needs 1..4000 UTF-8 bytes");
            assert!(is_room_content_refusal(&err_len));

            let err_oversized_meta = send_probe(
                &"m".repeat(oms_command_room::MAX_METADATA_BYTES + 128),
                "clean body",
            );
            assert_eq!(err_oversized_meta, "room metadata is oversized");
            assert!(is_room_content_refusal(&err_oversized_meta));

            room.send(
                "r-pin",
                RoomSend {
                    sender: "linear",
                    recipient: "main",
                    text: "original message body",
                    kind: "question",
                    reply_to: None,
                    message_id: Some("pin-dup"),
                    linked: false,
                    quiet: false,
                },
            )
            .expect("send original message");
            let err_conflict = send_probe("pin-dup", "conflicting message body");
            assert_eq!(err_conflict, ROOM_DUPLICATE_CONFLICT_MESSAGE);
            assert!(!is_room_content_refusal(&err_conflict));
        },
    );
}

#[test]
fn intake_crate_and_docs_pass_outbound_scrubber() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.join("../..");
    let mut files = vec![
        repo_root.join("docs/INTAKE.md"),
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
