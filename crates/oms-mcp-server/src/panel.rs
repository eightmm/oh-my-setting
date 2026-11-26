//! Project-scoped RPC surface for the desktop app. The host owns repository
//! bindings; tool arguments can only select an alias from that configuration.
use crate::value::Value;
use crate::{CommandRequest, MIME, Server, ToolProfile, error, reply};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[path = "lead_turns.rs"]
mod lead_turns;

const URI: &str = "ui://oms/conversation-panel/v25.html";
const V24: &str = "ui://oms/conversation-panel/v24.html";
const V23: &str = "ui://oms/conversation-panel/v23.html";
const V22: &str = "ui://oms/conversation-panel/v22.html";
const V21: &str = "ui://oms/conversation-panel/v21.html";
const V20: &str = "ui://oms/conversation-panel/v20.html";
const V19: &str = "ui://oms/conversation-panel/v19.html";
const V18: &str = "ui://oms/conversation-panel/v18.html";
const V17: &str = "ui://oms/conversation-panel/v17.html";
const V16: &str = "ui://oms/conversation-panel/v16.html";
const V15: &str = "ui://oms/conversation-panel/v15.html";
const V14: &str = "ui://oms/conversation-panel/v14.html";
const V13: &str = "ui://oms/conversation-panel/v13.html";
const V12: &str = "ui://oms/conversation-panel/v12.html";
const V11: &str = "ui://oms/conversation-panel/v11.html";
const V10: &str = "ui://oms/conversation-panel/v10.html";
const V9: &str = "ui://oms/conversation-panel/v9.html";
const V8: &str = "ui://oms/conversation-panel/v8.html";
const V7: &str = "ui://oms/conversation-panel/v7.html";
const V6: &str = "ui://oms/conversation-panel/v6.html";
const V5: &str = "ui://oms/conversation-panel/v5.html";
const V4: &str = "ui://oms/conversation-panel/v4.html";
const V3: &str = "ui://oms/conversation-panel/v3.html";
const V1: &str = "ui://oms/conversation-panel/v1.html";
const V2: &str = "ui://oms/conversation-panel/v2.html";
const OPEN: &str = "oms_open_panel";
const PROJECTS: &str = "oms_list_projects";
const SNAPSHOT: &str = "oms_panel_snapshot";
const WORK_ITEMS: &str = "oms_panel_work_items";
const USAGE: &str = "oms_panel_usage";
const CHAT: &str = "oms_panel_chat";
const GIT: &str = "oms_panel_git_status";
const GRAPH: &str = "oms_panel_project_graph";
const VIEW: &str = "oms_panel_tui_view";
const CAPABILITIES: &str = "oms_panel_capabilities";
const VIEWS: &[&str] = &[
    "overview",
    "health",
    "providers",
    "connections",
    "modelcatalog",
    "attribution",
    "teams",
    "sessions",
    "plan",
    "attention",
    "results",
    "metrics",
    "routes",
    "workspaces",
    "jobs",
    "past",
    "debate",
    "capacity",
    "log",
];
const START: &str = "oms_start_main";
const START_STATUS: &str = "oms_start_status";
const APPROVALS: &str = "oms_list_approvals";
const DECIDE: &str = "oms_decide_approval";
const RECORD_ISSUE: &str = "oms_record_reviewed_issue";
const ISSUE_STATUS: &str = "oms_reviewed_issue_status";
const OPEN_LEADS: &str = "oms_open_lead_conversations";
const SEND_LEAD: &str = "oms_send_lead_message";
const LEAD_STATUS: &str = "oms_lead_conversation_status";
const LEAD_PROVIDERS: [&str; 3] = ["codex", "claude", "agy"];
const LEAD_PROMPT: &str = "Acknowledge availability as this Room's provider Lead, then wait for explicit work. Do not inspect or change the repository, use tools, or delegate.";
const ASSIGN: &str = "oms_assign_provider";
const ASSIGN_RESULT: &str = "oms_assign_result";
const BINDING: &str = "Select a host-configured OMS project, or bind this connection to a project with OMS_PANEL_REPO.";
type PanelResult<T> = Result<T, String>;

// The committed handoff fixes the timestamp. Legacy linked requests keep the
// original operation from their receipt; only new bound asks use this address.
fn assignment_address(data: &Value, binding: &str) -> Option<String> {
    let stamp = data["assignedAt"].as_str()?;
    let time = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%SZ").ok()?;
    if stamp.len() != 20 || time.format("%Y-%m-%dT%H:%M:%SZ").to_string() != stamp {
        return None;
    }
    let suffix = crate::storage::hash(format!("assignment-operation/{binding}"));
    Some(format!("ask-{}-{suffix}", time.format("%Y%m%dT%H%M%SZ")))
}

fn assignment_directory(
    repo: &Path,
    data: &Value,
    op: &str,
    binding: &str,
    recovering: bool,
) -> PanelResult<Option<PathBuf>> {
    checked_peer_directory(repo, data, op, op, binding, recovering, None)
}

fn checked_peer_directory(
    repo: &Path,
    data: &Value,
    op: &str,
    thread: &str,
    binding: &str,
    recovering: bool,
    prompt_digest: Option<&str>,
) -> PanelResult<Option<PathBuf>> {
    use crate::storage::Directory;
    let dir = match Directory::run(repo, op, recovering) {
        Ok(dir) => dir,
        Err(_) if recovering => return Ok(None),
        Err(_) => return Err("The recorded assignment operation is unavailable.".into()),
    };
    let proof = if recovering {
        // A reservation and its metadata can precede supervisor startup. Read
        // only the native supervisor's private directory and its fixed receipts.
        let Some(before) = rustix::fs::fstat(&dir.fd)
            .ok()
            .filter(|s| s.st_mode & 0o077 == 0)
        else {
            return Ok(None);
        };
        let root = oms_safe_fs::Root::open(repo)
            .map_err(|_| "The assignment repository is unavailable.")?;
        let read = |name, limit| {
            root.read(
                &format!("{}/{op}/{name}", crate::RUN_ROOT),
                limit,
                oms_safe_fs::Policy::manifest(),
            )
        };
        let number = |bytes: &[u8]| -> Option<i32> {
            let line = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
            let value: i32 = line.parse().ok()?;
            (value.to_string() == line).then_some(value)
        };
        if !read("pid", 32).is_ok_and(|b| number(&b).is_some_and(|v| v > 0)) {
            return Ok(None);
        }
        match read("status", 32) {
            // The supervisor writes an 8-bit exit code or a negated Unix
            // wait-status signal (7 bits, with 0x7f reserved for stops).
            Ok(bytes) if number(&bytes).is_some_and(|v| (-126..=255).contains(&v)) => (),
            Err(oms_safe_fs::Error::Missing) => (),
            _ => return Ok(None),
        }
        let Some(bytes) = read("meta.json", 256 * 1024).ok() else {
            return Ok(None);
        };
        // The generic MCP parser permits duplicate members. A linkage proof
        // must have a single interpretation and still name the opened directory.
        if oms_context::parse_json_bounded(&bytes, 256 * 1024).is_err()
            || !Directory::run(repo, op, true)
                .ok()
                .and_then(|d| rustix::fs::fstat(&d.fd).ok())
                .is_some_and(|s| {
                    s.st_dev == before.st_dev
                        && s.st_ino == before.st_ino
                        && s.st_mode == before.st_mode
                })
        {
            return Ok(None);
        }
        crate::value::from_slice(&bytes).unwrap_or(Value::Null)
    } else {
        crate::storage::meta(&dir.path)
    };
    if proof["operation"] != op
        || proof["thread"] != thread
        || proof["kind"] != "ask"
        || prompt_digest.is_some_and(|digest| {
            proof["lead_followup"] != true || proof["prompt_sha256"] != digest
        })
        || proof["panel_assignment"] != binding
        || (data["purpose"] == "provider-lead" && proof["answer_storage"] != "operation")
        || proof["targets"] != json!([data["provider"]])
    {
        return if recovering {
            Ok(None)
        } else {
            Err("The recorded operation does not match this assignment.".into())
        };
    }
    Ok(Some(dir.path))
}

pub(super) fn runtime_request(request: &Value) -> bool {
    let id = &request["id"];
    (matches!(id, Value::Number(_)) || id.as_str().is_some_and(|s| s.len() <= 1024))
        && request["method"] == "tools/call"
        && matches!(
            request["params"]["name"].as_str(),
            Some(
                SNAPSHOT
                    | WORK_ITEMS
                    | USAGE
                    | CHAT
                    | GIT
                    | GRAPH
                    | VIEW
                    | CAPABILITIES
                    | START
                    | START_STATUS
                    | APPROVALS
                    | DECIDE
                    | RECORD_ISSUE
                    | ISSUE_STATUS
                    | ASSIGN
                    | ASSIGN_RESULT
                    | OPEN_LEADS
                    | LEAD_STATUS
                    | SEND_LEAD
            )
        )
}

fn project_read(name: &str) -> bool {
    matches!(
        name,
        OPEN | PROJECTS | SNAPSHOT | WORK_ITEMS | USAGE | GIT | GRAPH | VIEW | CAPABILITIES
    )
}

pub(super) fn definitions(profile: ToolProfile) -> Value {
    let make = |name: &str,
                description: &str,
                properties: Value,
                required: &[&str],
                write: bool| {
        json!({"name":name,"description":description,
          "inputSchema":{"type":"object","properties":properties,"required":required.to_vec(),"additionalProperties":false},
          "annotations":{"readOnlyHint":!write,"destructiveHint":write,"idempotentHint":!write,"openWorldHint":matches!(name, START | ASSIGN)},
          "_meta":{"ui":{"visibility":["app","model"]}}})
    };
    let project = json!({"type":"string","description":"Exact alias from oms_list_projects when the host has a project map."});
    let scope = json!({"project":project});
    let mut open_scope = scope.clone();
    open_scope["leadConversations"] = json!({"type":"boolean","description":"Explicit intent to open provider Lead conversations. Opening the panel itself only displays it."});
    let mut opener = make(
        OPEN,
        "Open the OMS panel (expands beside or over the conversation when the host allows it).",
        open_scope,
        &[],
        false,
    );
    opener["_meta"]["ui"]["resourceUri"] = json!(URI);
    opener["_meta"]["com.google.antigravity/ui"] =
        json!({"entrypoints":[{"type":"aux_pane","title":"OMS"},{"type":"global","title":"OMS"}]});
    opener["_meta"]["openai/outputTemplate"] = json!(URI);
    opener["_meta"]["openai/ui"] = if profile == ToolProfile::McpApp {
        json!({"entrypoints":[{"type":"global"}]})
    } else {
        json!({"entrypoints":[{"type":"thread"}]})
    };
    let ident = json!({"type":"string","pattern":"^[a-zA-Z0-9][a-zA-Z0-9._-]{0,159}$"});
    let mut chat = make(
        CHAT,
        "Read an exact Room participant’s messages shared with this conversation and its recorded result.",
        json!({"project":project,"room":ident,"participant":ident,"before":{"type":"integer","minimum":1},"section":{"type":"string","enum":["info","events","files","diff","transcript"]},"attempt":ident,"page":{"type":"integer","minimum":1,"maximum":10000}}),
        &["room", "participant"],
        false,
    );
    chat["_meta"]["ui"]["visibility"] = json!(["app"]);
    let mut issue_address = json!({"project":project,"room":ident,"owner":ident,"requestId":{"type":"string","format":"uuid"}});
    issue_address["issue"] = json!({"type":"string","maxLength":151,"description":"Canonical owner/repo#N from the reviewed same-project issue."});
    issue_address["sha256"] = json!({"type":"string","pattern":"^[0-9a-f]{64}$"});
    let mut issue_status = make(
        ISSUE_STATUS,
        "Read a receipt for this conversation’s exact Main by its requestId, or look up the stable requestId by reviewed issue and sha256. This read never records a new receipt.",
        issue_address.clone(),
        &["room", "owner"],
        false,
    );
    issue_status["inputSchema"]["oneOf"] = json!([
        {"required":["requestId"],"not":{"anyOf":[{"required":["issue"]},{"required":["sha256"]}]}},
        {"required":["issue","sha256"],"not":{"required":["requestId"]}}
    ]);
    let mut record_issue = make(
        RECORD_ISSUE,
        "Record the user-selected GitHub issue and reviewed SHA-256 for this conversation’s exact Main. First look up the requestId with oms_reviewed_issue_status for this exact issue and sha256. The first record reads GitHub. This action does not start work.",
        issue_address,
        &["room", "owner", "requestId", "issue", "sha256"],
        true,
    );
    record_issue["annotations"] = json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true});
    record_issue["_meta"]["ui"]["visibility"] = json!(["app"]);
    let mut open_leads = make(
        OPEN_LEADS,
        "Open one bounded OMS readiness conversation for each Codex, Claude and AGY Lead in this chat's verified Room. Reopening reuses the same requests.",
        json!({"project":project,"room":ident,"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$","description":"Optional previously observed physical project key; refuses if the binding changed."}}),
        &["room"],
        true,
    );
    open_leads["annotations"] = json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true});
    let turn_id = json!({"type":"string","format":"uuid","description":"A lowercase UUIDv4 created for this explicit user message. Reuse it only with the identical text."});
    let mut send = make(
        SEND_LEAD,
        "Send a user's explicit message to this Lead's existing OMS conversation. Repeated submissions with the same turn ID never start another call.",
        json!({"project":project,"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$"},"room":ident,"provider":{"type":"string","enum":LEAD_PROVIDERS},"leadRequestId":{"type":"string","format":"uuid"},"turnId":turn_id,"text":{"type":"string","minLength":1,"maxLength":4000}}),
        &[
            "projectKey",
            "room",
            "provider",
            "leadRequestId",
            "turnId",
            "text",
        ],
        true,
    );
    send["annotations"] = json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true});
    send["_meta"]["ui"]["visibility"] = json!(["app"]);
    let all = json!([
        opener,
        chat,
        record_issue,
        issue_status,
        open_leads,
        send,
        make(
            LEAD_STATUS,
            "Read provider readiness in the current Main’s Room. A selected provider’s answer is visible only in the panel.",
            json!({"project":project,"room":ident,"provider":{"type":"string","enum":LEAD_PROVIDERS},"turnId":turn_id,"before":{"type":"string","description":"An older-page cursor returned by this conversation."}}),
            &["room"],
            false
        ),
        make(
            USAGE,
            "Read recorded usage for recent OMS attempts and local provider quota windows.",
            scope.clone(),
            &[],
            false
        ),
        make(
            GIT,
            "Read Git, open repository issues, PRs and linked GitHub Projects. Use inventory and nextCursor to page through all open issues or PRs, or kind and number to look up one. Use boardId for linked board pages (or with candidates=true for ranked whole-board issue candidates), or issuePreview alone to review immutable metadata for one open issue in the selected project.",
            json!({"project":project,"issuePreview":{"type":"integer","minimum":1,"maximum":9_999_999_999u64},"kind":{"type":"string","enum":["issue","pull"]},"number":{"type":"integer","minimum":1,"maximum":9_999_999_999u64},"inventory":{"type":"string","enum":["issues","pullRequests"]},"boardId":{"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9_-]{0,99}$"},"cursor":{"type":"string","minLength":1,"maxLength":500},"candidates":{"type":"boolean"}}),
            &[],
            false
        ),
        make(
            GRAPH,
            "Read a bounded, strictly read-only repository project graph without building or writing an index. Uses the cached project graph when present (distinguishing EXTRACTED vs INFERRED/AMBIGUOUS relationships) or bounded git ls-files structural containment otherwise. Pass node to focus, inspect neighbors, or drill into a directory or file; pass query to filter by name or path.",
            json!({
                "project": project,
                "node": {"type":"string","minLength":1,"maxLength":512,"description":"Optional stable node ID to focus, inspect neighbors, or drill into (for example dir:crates or file:src/lib.rs)."},
                "query": {"type":"string","minLength":1,"maxLength":256,"description":"Optional literal substring filter over node names and paths."},
                "limit": {"type":"integer","minimum":1,"maximum":60,"description":"Maximum nodes to return (1-60, default 40)."},
                "depth": {"type":"integer","minimum":0,"maximum":3,"description":"Maximum traversal or directory depth (0-3, default 2)."},
                "includeTests": {"type":"boolean","description":"Include test nodes when available (default false)."}
            }),
            &[],
            false
        ),
        make(
            VIEW,
            "Read one OMS panel page. connections reports CLI presence, managed outgoing swap policy, host preconditions and configured permission policy, modelcatalog reads cached model and effort evidence, routes previews expected choices, and attribution shows recorded calls. Null means unverified.",
            json!({"project":project,"view":{"type":"string","enum":VIEWS.to_vec()},"page":{"type":"integer","minimum":1,"maximum":1000}}),
            &["view"],
            false
        ),
        make(
            CAPABILITIES,
            "Read the public OMS CLI command catalog (oms list --all) for the selected project host. Pass offset from nextOffset if the catalog spans multiple pages.",
            json!({"project":project,"offset":{"type":"integer","minimum":0,"maximum":10000,"description":"Zero-based command offset when paging a catalog with nextOffset."}}),
            &[],
            false
        ),
        make(
            PROJECTS,
            "List projects configured by the OMS host.",
            json!({}),
            &[],
            false
        ),
        make(
            SNAPSHOT,
            "Read project progress, recorded Main and provider activity.",
            scope.clone(),
            &[],
            false
        ),
        make(
            WORK_ITEMS,
            "Read bounded, strictly read-only task-focused work items for the selected project from the canonical plan and verified result bindings.",
            scope.clone(),
            &[],
            false
        ),
        make(
            START,
            "Start a managed Main with the user's selected provider and goal. Reuse the original requestId for a retry.",
            json!({
               "project":project,"main":{"type":"string","enum":["codex","claude","agy","pi"]},
               "goal":{"type":"string","minLength":1,"maxLength":2000},"requestId":{"type":"string","format":"uuid"}
            }),
            &["main", "goal", "requestId"],
            true
        ),
        make(
            START_STATUS,
            "Recover the original Main launch by requestId, including after it has ended.",
            json!({"project":project,"requestId":{"type":"string","format":"uuid"}}),
            &["requestId"],
            false
        ),
        make(
            APPROVALS,
            "Read the exact scope questions sent by the Main to the app user.",
            scope,
            &[],
            false
        ),
        make(
            ASSIGN,
            "Assign one read-only Claude or AGY provider conversation to this conversation's own Room. Reuse the original requestId for a retry; a retry never starts another provider.",
            json!({
               "project":project,"room":ident,"requestId":{"type":"string","format":"uuid"},
               "provider":{"type":"string","enum":["claude","agy"]},
               "title":{"type":"string","minLength":1,"maxLength":120},
               "task":{"type":"string","minLength":1,"maxLength":4000},
               "repairOnly":{"type":"boolean","description":"Only link an existing verified operation; never create an assignment or start a provider."}
            }),
            &["room", "requestId", "provider", "title", "task"],
            true
        ),
        make(
            ASSIGN_RESULT,
            "Read the assignment record and returned provider conversation for an original requestId.",
            json!({"project":project,"room":ident,"requestId":{"type":"string","format":"uuid"},"after":{"type":"string","maxLength":1024}}),
            &["room", "requestId"],
            false
        ),
        make(
            DECIDE,
            "Send the user's explicit decision for the exact reviewed Main question.",
            json!({
               "project":project,"id":{"type":"string"},"version":{"type":"integer","minimum":1},
               "digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"decision":{"type":"string","enum":["approve","deny"]}
            }),
            &["id", "version", "digest", "decision"],
            true
        )
    ]);
    if profile == ToolProfile::McpApp {
        Value::Array(
            all.as_array()
                .unwrap()
                .iter()
                .filter(|t| t["name"].as_str().is_some_and(project_read))
                .cloned()
                .collect(),
        )
    } else {
        all
    }
}

impl CommandRequest<'_> {
    /// Only this fixed native start route transfers ownership to a tmux daemon.
    pub fn is_desktop_main_start(&self) -> bool {
        let [
            panel,
            repo_flag,
            repo,
            host_flag,
            host,
            launch,
            provider,
            room_flag,
            room,
            task,
            json,
        ] = self.args
        else {
            return false;
        };
        panel == "panel"
            && repo_flag == "--repo"
            && repo == self.cwd.as_os_str()
            && host_flag == "--host"
            && host == "desktop"
            && launch == "--launch"
            && matches!(provider.to_str(), Some("codex" | "claude" | "agy" | "pi"))
            && room_flag == "--room"
            && room
                .to_str()
                .and_then(|value| value.strip_prefix("oms-app-"))
                .is_some_and(uuid)
            && task
                .to_str()
                .and_then(|value| value.strip_prefix("--task="))
                .is_some_and(|goal| {
                    !goal.trim().is_empty()
                        && !goal.chars().any(char::is_control)
                        && goal.chars().count() <= 2000
                })
            && json == "--json"
    }
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit() && !b.is_ascii_uppercase()
            }
        })
}
fn github_slug(slug: &str) -> bool {
    slug.split_once('/').is_some_and(|(owner, repo)| {
        (1..=39).contains(&owner.len())
            && owner.as_bytes()[0].is_ascii_alphanumeric()
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && (1..=100).contains(&repo.len())
            && !matches!(repo, "." | "..")
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    })
}

fn canonical_issue(value: &str) -> bool {
    value.split_once('#').is_some_and(|(slug, n)| {
        github_slug(slug)
            && (1..=10).contains(&n.len())
            && !n.starts_with('0')
            && n.bytes().all(|b| b.is_ascii_digit())
    })
}

fn alias(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}
const RESUME_REFUSAL_COLUMN: (&str, &str) = ("resumeRefusalCode", "재개 거절 코드");

const CONNECTION_EVIDENCE: [(&str, &str); 4] = [
    ("managedOutgoingSwap", "내보내는 Main 교체 계약"),
    ("swapEvidence", "교체 계약 및 호스트 조건"),
    ("configuredMainPermissions", "설정된 Main 권한 모드"),
    ("fullModeOption", "full 모드 옵션 지원"),
];

fn connections(mut data: Value) -> PanelResult<Value> {
    const INVALID: &str = "OMS returned unrecognized Connections evidence.";
    const CELLS: &[(&str, &str)] = &[
        ("harness", "하네스"),
        ("owner", "디스패치 소유자"),
        ("transport", "Main 전송 경로"),
        ("installed", "CLI 설치"),
        ("dependencies", "실행에 필요한 도구"),
        ("dependenciesPresent", "도구 모두 발견"),
        ("resumePolicy", "재개 계약"),
        ("accountSelection", "계정 선택 계약"),
        ("version", "확인된 CLI 버전"),
        ("auth", "인증 준비"),
        ("observedAt", "PATH 조회 시각"),
    ];
    let column_contract = |key: &str| {
        CELLS
            .iter()
            .chain(&CONNECTION_EVIDENCE)
            .chain([&RESUME_REFUSAL_COLUMN])
            .find(|(k, _)| *k == key)
            .map(|(_, label)| *label)
    };
    let columns = data["columns"].as_array().ok_or(INVALID)?;
    let mut seen = std::collections::BTreeSet::new();
    for column in columns {
        let key = column["key"].as_str().ok_or(INVALID)?;
        let label = column_contract(key).ok_or(INVALID)?;
        if !seen.insert(key)
            || column["label"] != label
            || !column.as_object().is_some_and(|o| o.keys().count() == 2)
        {
            return Err(INVALID.into());
        }
    }
    if !data.as_object().is_some_and(|obj| {
        obj.keys().all(|k| {
            matches!(
                k.as_str(),
                Some(
                    "view"
                        | "page"
                        | "nextPage"
                        | "columns"
                        | "rows"
                        | "status"
                        | "total"
                        | "incomplete"
                )
            )
        })
    }) || !matches!(data["status"].as_str(), Some("ready" | "unavailable"))
        || !data["nextPage"].is_null()
        || !(data["total"].is_null() || data["total"].as_u64().is_some_and(|n| n <= 4))
        || !(data["incomplete"].is_null() || data["incomplete"].is_boolean())
    {
        return Err(INVALID.into());
    }
    let page_has_rows = data["status"] == "ready" && data["page"] == 1;
    let total = data["total"].as_u64().unwrap_or(4);
    let Value::Array(rows) = &mut data["rows"] else {
        return Err(INVALID.into());
    };
    if (!page_has_rows && !rows.is_empty()) || rows.len() as u64 > total {
        return Err(INVALID.into());
    }
    let mut harnesses = std::collections::BTreeSet::new();
    let has_rows = !rows.is_empty();
    let mut legacy = false;
    for row in rows {
        let harness = match row["harness"].as_str() {
            Some("codex") => "codex",
            Some("claude") => "claude",
            Some("pi") => "pi",
            Some("agy" | "antigravity") => "agy",
            _ => return Err(INVALID.into()),
        };
        let owner = match harness {
            "pi" => "codex",
            "agy" => "antigravity",
            x => x,
        };
        let deps = if harness == "pi" {
            "pi, codex, oms"
        } else {
            harness
        };
        let selectable = matches!(harness, "codex" | "claude");
        let shape = row.as_object().ok_or(INVALID)?;
        if !harnesses.insert(harness)
            || !shape
                .keys()
                .all(|k| k.as_str().and_then(column_contract).is_some())
            || row["owner"] != owner
            || row["transport"]
                != if harness == "pi" {
                    "CodexGateway"
                } else {
                    "native"
                }
            || row["dependencies"] != deps
            || !row["dependenciesPresent"].is_boolean()
            || !row["installed"].is_boolean()
            || row["resumePolicy"]
                != if selectable {
                    "requires verified session"
                } else {
                    "unsupported"
                }
            || row["accountSelection"].as_bool() != Some(selectable)
            || !row["auth"].is_null()
            || !row["version"].is_null()
            || !(row["observedAt"].is_null() || row["observedAt"].as_u64().is_some())
        {
            return Err(INVALID.into());
        }
        let swap_missing = !shape.contains_key("managedOutgoingSwap");
        let evidence_missing = !shape.contains_key("swapEvidence");
        for (key, _) in CONNECTION_EVIDENCE {
            if row.get(key).is_none() {
                legacy = true;
                row[key] = json!("unknown");
            }
        }
        let (key, _) = RESUME_REFUSAL_COLUMN;
        match row.get(key).and_then(Value::as_str) {
            None if row.get(key).is_none() => row[key] = json!("unknown"),
            Some("unknown") => {}
            Some("not_applicable") if selectable => {}
            Some("binding_unverified") if !selectable => {}
            _ => return Err(INVALID.into()),
        }
        let swap = row["managedOutgoingSwap"].as_str();
        let evidence = row["swapEvidence"].as_str();
        let valid = [
            ("unknown", "unknown"),
            match harness {
                "codex" => ("supported", "requires-verified-idle-pane-exit"),
                "claude" => ("supported", "assisted-exit-linux-proc-available"),
                _ => ("unsupported", "no-managed-outgoing-adapter"),
            },
            if harness == "claude" {
                ("unsupported", "assisted-exit-linux-proc-unavailable")
            } else {
                ("unknown", "unknown")
            },
        ]
        .iter()
        .any(|(s, e)| {
            (swap_missing || swap == Some(s)) && (evidence_missing || evidence == Some(e))
        });
        if !valid
            || !matches!(
                row["configuredMainPermissions"].as_str(),
                Some("native" | "full" | "unknown")
            )
            || !(row["fullModeOption"] == "unknown"
                || row["fullModeOption"]
                    == if harness == "pi" {
                        "unsupported"
                    } else {
                        "supported"
                    })
        {
            return Err(INVALID.into());
        }
    }
    if let Value::Array(columns) = &mut data["columns"] {
        if legacy {
            for (key, label) in CONNECTION_EVIDENCE {
                if !columns.iter().any(|column| column["key"] == key) {
                    columns.push(json!({"key":key,"label":label}));
                }
            }
        }
        let (key, label) = RESUME_REFUSAL_COLUMN;
        if has_rows && !columns.iter().any(|column| column["key"] == key) {
            columns.push(json!({"key":key,"label":label}));
        }
    }
    Ok(data)
}

fn valid_work_items_receipt(data: &Value) -> bool {
    let keys = [
        "id",
        "title",
        "state",
        "owner",
        "depends",
        "issue",
        "result",
        "verification",
        "revision",
    ];
    let id = |s: &str| {
        (1..=96).contains(&s.len())
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    };
    let nullable = |v: &Value, max: usize| {
        v.is_null()
            || v.as_str()
                .is_some_and(|s| s.chars().count() <= max && !s.chars().any(char::is_control))
    };
    let optional = ["claimExpired", "issueCache", "prReviewCandidate"];
    data.as_object().is_some_and(|obj| obj.keys().count() == 4)
        && data["schema"] == 1
        && data["omitted"].as_i64().is_some_and(|n| n >= 0)
        && matches!(
            data["availability"].as_str(),
            Some("ready" | "partial" | "unavailable")
        )
        && data["items"].as_array().is_some_and(|items| {
            items.len() <= 64
                && items.iter().all(|row| {
                    row.as_object().is_some_and(|obj| {
                        keys.iter().all(|k| obj.contains_key(k))
                            && obj.keys().all(|k| {
                                k.as_str()
                                    .is_some_and(|k| keys.contains(&k) || optional.contains(&k))
                            })
                    }) && row["id"].as_str().is_some_and(id)
                        && nullable(&row["title"], 160)
                        && nullable(&row["issue"], 140)
                        && nullable(&row["state"], 32)
                        && nullable(&row["result"], 32)
                        && nullable(&row["verification"], 32)
                        && match row.get("claimExpired") {
                            Some(expired) => expired.as_bool() == Some(row["state"] == "stale"),
                            None => row["state"] != "stale",
                        }
                        && row.get("issueCache").is_none_or(|cache| {
                            row["issue"].is_string() && valid_issue_cache(cache)
                        })
                        && row.get("prReviewCandidate").is_none_or(|candidate| {
                            row["state"] == "review" && valid_pr_candidate(candidate)
                        })
                        && (row["owner"].is_null() || row["owner"].as_str().is_some_and(id))
                        && (row["revision"].is_null()
                            || row["revision"].as_str().is_some_and(|s| {
                                s.len() == 64
                                    && s.bytes()
                                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                            }))
                        && (row["depends"].is_null()
                            || row["depends"].as_array().is_some_and(|deps| {
                                deps.len() <= 32 && deps.iter().all(|v| v.as_str().is_some_and(id))
                            }))
                })
        })
}

fn valid_issue_cache(cache: &Value) -> bool {
    cache.as_object().is_some_and(|obj| {
        obj.keys().count() == 3
            && ["state", "freshness", "reason"]
                .iter()
                .all(|k| obj.contains_key(k))
    }) && match cache["freshness"].as_str() {
        Some("fresh" | "stale") => {
            matches!(cache["state"].as_str(), Some("open" | "closed")) && cache["reason"].is_null()
        }
        Some("unavailable") => {
            cache["state"].is_null()
                && matches!(
                    cache["reason"].as_str(),
                    Some("cache missing" | "cache unreadable" | "no matching cache entry")
                )
        }
        _ => false,
    }
}

fn valid_pr_candidate(candidate: &Value) -> bool {
    let reference = candidate["reference"]
        .as_str()
        .and_then(|s| s.split_once('#'))
        .is_some_and(|(slug, n)| {
            let segment = |s: &str| {
                !matches!(s, "" | "." | "..")
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            };
            slug.len() <= 128
                && slug
                    .split_once('/')
                    .is_some_and(|(o, r)| segment(o) && segment(r))
                && (1..=10).contains(&n.len())
                && !n.starts_with('0')
                && n.bytes().all(|b| b.is_ascii_digit())
        });
    candidate
        .as_object()
        .is_some_and(|obj| obj.keys().count() == 4)
        && reference
        && matches!(candidate["freshness"].as_str(), Some("fresh" | "stale"))
        && matches!(
            (
                candidate["state"].as_str(),
                candidate["disposition"].as_str()
            ),
            (Some("merged"), Some("completed-external")) | (Some("closed"), Some("superseded"))
        )
}

fn valid_graph_node_id(value: &str) -> bool {
    if value == "dir:." {
        return true;
    }
    let Some((kind, suffix)) = value.split_once(':') else {
        return false;
    };
    let path = if kind == "symbol" {
        let Some((path, name)) = suffix.split_once("::") else {
            return false;
        };
        if name.is_empty() || name.contains('/') {
            return false;
        }
        path
    } else if matches!(kind, "dir" | "file" | "test") {
        suffix
    } else {
        return false;
    };
    value.len()<=512 && !value.contains('\\') && !value.chars().any(|c|c.is_control() || matches!(c,'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'|'\u{feff}'))
        && path.split('/').all(|c| !matches!(c,""|"."|".."))
}

fn configured_projects() -> PanelResult<BTreeMap<String, PathBuf>> {
    let invalid = "Invalid OMS project map. Configure a nonempty object of project aliases and absolute checkout paths on the host.";
    let map_file = std::env::var_os("OMS_PANEL_PROJECTS_FILE");
    let inline = std::env::var_os("OMS_PANEL_PROJECTS");
    let pin = std::env::var_os("OMS_PANEL_REPO");
    if [map_file.is_some(), inline.is_some(), pin.is_some()]
        .into_iter()
        .filter(|v| *v)
        .count()
        > 1
    {
        return Err(invalid.into());
    }
    let raw = if let Some(v) = inline {
        v.into_string().map_err(|_| invalid)?
    } else if pin.is_some() {
        return Ok(BTreeMap::new());
    } else {
        let explicit = map_file.is_some();
        let path = if let Some(path) = map_file {
            PathBuf::from(path)
        } else if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            PathBuf::from(config).join("oh-my-setting/desktop-projects.json")
        } else if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(".config/oh-my-setting/desktop-projects.json")
        } else {
            return Ok(BTreeMap::new());
        };
        if !path.is_absolute() {
            return Err(invalid.into());
        }
        let mut f = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                rustix::fs::OFlags::NOFOLLOW.bits() as i32
                    | rustix::fs::OFlags::NONBLOCK.bits() as i32,
            )
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if !explicit && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(_) => return Err(invalid.into()),
        };
        let meta = f.metadata().map_err(|_| invalid)?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o022 != 0
        {
            return Err(invalid.into());
        }
        let mut raw = String::new();
        (&mut f)
            .take(16385)
            .read_to_string(&mut raw)
            .map_err(|_| invalid)?;
        raw
    };
    if raw.len() > 16384 {
        return Err(invalid.into());
    }
    let map: BTreeMap<String, PathBuf> = serde_json::from_str(&raw).map_err(|_| invalid)?;
    if map.is_empty() || map.len() > 128 || map.iter().any(|(id, p)| !alias(id) || !p.is_absolute())
    {
        return Err(invalid.into());
    }
    Ok(map)
}

fn checkout(dir: &Path) -> PanelResult<bool> {
    for marker in [".oms", ".git"] {
        match dir.join(marker).symlink_metadata() {
            Ok(m) if marker == ".oms" && m.is_dir() => return Ok(true),
            Ok(m) if marker == ".git" && (m.is_dir() || m.is_file()) => {
                if oms_safe_fs::has_git_marker(dir) {
                    return Ok(true);
                }
                if m.is_dir() {
                    match dir.join(".git/HEAD").symlink_metadata() {
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        _ => return Err(BINDING.into()),
                    }
                }
            }
            Ok(_) => return Err(BINDING.into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(BINDING.into()),
        }
    }
    Ok(false)
}

/// JSON-RPC envelope around a tool result: a request id of up to 1024 bytes may escape to six
/// bytes per byte, plus the fixed `jsonrpc`/`id`/`result` keys.
const FRAME_RESERVE: usize = 1024 * 6 + 256;
pub(super) fn response(data: PanelResult<Value>) -> Value {
    match data.and_then(|data| {
        crate::output::outbound(data.to_string().as_bytes(), &Value::Null)
            .map_err(|_| "OMS withheld this response during its outbound scan".to_owned())?;
        Ok(data)
    }) {
        Ok(mut data) => {
            // Selected declarations are for the widget. Sharing a node with the
            // conversation carries only its existing scoped reference.
            let definition =
                if data["kind"] == "oms-panel-project-graph" && data["selection"].is_object() {
                    data["selection"]
                        .as_object_mut()
                        .and_then(|s| s.remove("definition"))
                } else {
                    None
                };
            let mut text = data.to_string();
            let is_git_detail = data["git"].is_object() && data["github"].is_object();
            if (data["dashboard"]["managedScratch"].is_object() || is_git_detail)
                && crate::value::encode(
                    &json!({"content":[{"type":"text","text":&text}],"structuredContent":&data,"isError":false}),
                    true,
                    false,
                    false,
                    false,
                )
                .len()
                    + FRAME_RESERVE
                    > crate::OUTPUT_LIMIT
            {
                text = if is_git_detail {
                    "Git and GitHub status available in structuredContent.".into()
                } else {
                    "Project dashboard. Canonical status and managed scratch lifecycle observations are in structuredContent.".into()
                };
                if crate::value::encode(
                    &json!({"content":[{"type":"text","text":&text}],"structuredContent":&data,"isError":false}),
                    true,
                    false,
                    false,
                    false,
                )
                .len()
                    + FRAME_RESERVE
                    > crate::OUTPUT_LIMIT
                {
                    return response(Err(if is_git_detail {
                        "OMS desktop detail exceeds the response limit.".into()
                    } else {
                        "OMS panel snapshot exceeds its output limit.".into()
                    }));
                }
            }
            let mut result = json!({"content":[{"type":"text","text":text}],"structuredContent":data,"isError":false});
            if let Some(detail) = definition {
                result["_meta"]["graphDefinition"] = detail;
            }
            result
        }
        Err(message) => json!({"isError":true,"content":[{"type":"text","text":message}]}),
    }
}

impl Server<'_> {
    fn panel_repo(&self, args: &Value) -> PanelResult<PathBuf> {
        let map = configured_projects()?;
        let pin = if map.is_empty() {
            if args.get("project").is_some() {
                return Err(BINDING.into());
            }
            std::env::var_os("OMS_PANEL_REPO").map(PathBuf::from)
        } else {
            let id = args["project"].as_str().ok_or(BINDING)?;
            Some(map.get(id).ok_or(BINDING)?.clone())
        };
        // A generic stdio process may start in the plugin installation directory.
        // Only an enrolled host project or an exact host pin binds its public reads.
        if self.config.profile == ToolProfile::McpApp && pin.is_none() {
            return Err(BINDING.into());
        }
        let repo = if let Some(pin) = pin {
            if !pin.is_absolute()
                || pin.canonicalize().map_err(|_| BINDING)? != pin
                || !checkout(&pin)?
            {
                return Err(BINDING.into());
            }
            pin
        } else {
            // Desktop app versions can start stdio inside the installed plugin
            // cache. Climbing to that Git checkout would operate on the plugin.
            if self.caller_cwd.ancestors().any(|dir| {
                dir.ends_with("plugins/cache")
                    || dir.ends_with(".agents/plugins")
                    || dir.join("plugin.json").is_file()
                    || dir.join(".codex-plugin/plugin.json").is_file()
            }) {
                return Err(BINDING.into());
            }
            let mut found = None;
            for dir in self.caller_cwd.ancestors() {
                if checkout(dir)? {
                    found = Some(dir.to_owned());
                    break;
                }
            }
            found.ok_or(BINDING)?
        };
        if crate::delegated() && repo != self.caller_cwd {
            return Err(BINDING.into());
        }
        // The CLI may resolve an OMS scratch worktree to its shared parent, or
        // inherit an execution/state root. An alias grants only its bound root.
        let resolver = oms_reference::repository::Resolver::from_env().map_err(|_| BINDING)?;
        if resolver
            .state_repository(&repo, false, self.interrupted)
            .map_err(|_| BINDING)?
            != repo
            || resolver
                .native_repository(&repo, self.interrupted)
                .map_err(|_| BINDING)?
                != repo
            || std::env::var_os("OMS_STATE_REPO")
                .filter(|p| !p.is_empty())
                .is_some_and(|path| Path::new(&path).canonicalize().ok().as_deref() != Some(&repo))
        {
            return Err(BINDING.into());
        }
        Ok(repo)
    }

    pub(super) fn panel_request(&self, id: &Value, method: &str, params: &Value) -> Value {
        if !params.is_null() && !params.is_object() {
            return error(id, -32602, "expected object");
        }
        let result = match method {
            "resources/templates/list" => json!({"resourceTemplates":[]}),
            "resources/list" => {
                json!({"resources":[{"uri":URI,"name":"oms-conversation-panel","title":"OMS","mimeType":MIME}]})
            }
            "resources/read"
                if matches!(
                    params["uri"].as_str(),
                    Some(
                        URI | V1
                            | V2
                            | V3
                            | V4
                            | V5
                            | V6
                            | V7
                            | V8
                            | V9
                            | V10
                            | V11
                            | V12
                            | V13
                            | V14
                            | V15
                            | V16
                            | V17
                            | V18
                            | V19
                            | V20
                            | V21
                            | V22
                            | V23
                            | V24
                    )
                ) =>
            {
                json!({"contents":[{
                    "uri":params["uri"],"mimeType":MIME,"text":include_str!("conversation-panel.html"),
                    "_meta":{"ui":{"preferredDisplayMode":"fullscreen","csp":{"connectDomains":[],"resourceDomains":[]}}}
                }]})
            }
            "resources/read" => return error(id, -32602, "unknown panel resource"),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                if self.config.profile == ToolProfile::McpApp && !project_read(name) {
                    return error(id, -32602, "unknown project read tool");
                }
                let allowed: &[&str] = match name {
                    PROJECTS => &[],
                    OPEN => &["project", "leadConversations"],
                    OPEN_LEADS => &["project", "room", "projectKey"],
                    LEAD_STATUS => &["project", "room", "provider", "turnId", "before"],
                    SEND_LEAD => &[
                        "project",
                        "projectKey",
                        "room",
                        "provider",
                        "leadRequestId",
                        "turnId",
                        "text",
                    ],
                    APPROVALS | SNAPSHOT | WORK_ITEMS | USAGE => &["project"],
                    GIT => &[
                        "project",
                        "issuePreview",
                        "kind",
                        "number",
                        "inventory",
                        "boardId",
                        "cursor",
                        "candidates",
                    ],
                    GRAPH => &["project", "node", "query", "limit", "depth", "includeTests"],
                    CHAT => &[
                        "project",
                        "room",
                        "participant",
                        "before",
                        "section",
                        "attempt",
                        "page",
                    ],
                    VIEW => &["project", "view", "page"],
                    CAPABILITIES => &["project", "offset"],
                    START_STATUS => &["project", "requestId"],
                    START => &["project", "main", "goal", "requestId"],
                    ASSIGN => &[
                        "project",
                        "room",
                        "requestId",
                        "provider",
                        "title",
                        "task",
                        "repairOnly",
                    ],
                    ASSIGN_RESULT => &["project", "room", "requestId", "after"],
                    RECORD_ISSUE => &["project", "room", "owner", "requestId", "issue", "sha256"],
                    ISSUE_STATUS => &["project", "room", "owner", "requestId", "issue", "sha256"],
                    DECIDE => &["project", "id", "version", "digest", "decision"],
                    _ => return error(id, -32602, "unknown desktop tool"),
                };
                let empty = json!({});
                let args = params.get("arguments").unwrap_or(&empty);
                if !args.as_object().is_some_and(|o| {
                    o.keys()
                        .all(|k| k.as_str().is_some_and(|s| allowed.contains(&s)))
                }) {
                    return error(id, -32602, "unknown or invalid desktop tool arguments");
                }
                // Other hosts use their own session IDs. Only the Codex adapter supplies
                // the per-call binding used to enter a private OMS Room.
                let meta = if self.config.profile == ToolProfile::McpApp {
                    &Value::Null
                } else {
                    &params["_meta"]
                };
                let result = self.panel_call(name, args, meta);
                if name == GRAPH {
                    return self.graph_reply(id, params, result);
                }
                if matches!(name, LEAD_STATUS | SEND_LEAD) {
                    match result {
                        Ok(mut data) => {
                            let private = data
                                .as_object_mut()
                                .and_then(|o| o.remove("leadConversation"));
                            let mut result = response(Ok(data));
                            if let Some(private) = private {
                                if crate::output::outbound(
                                    private.to_string().as_bytes(),
                                    &Value::Null,
                                )
                                .is_err()
                                {
                                    return reply(id,response(Err("OMS withheld the Lead conversation during its outbound scan.".into())));
                                }
                                result["_meta"]["leadConversation"] = private;
                            }
                            result
                        }
                        Err(e) => response(Err(e)),
                    }
                } else if name == ASSIGN_RESULT {
                    match result {
                        Ok(data)
                            if crate::output::outbound(
                                data.to_string().as_bytes(),
                                &Value::Null,
                            )
                            .is_ok() =>
                        {
                            let mut result = response(Ok(
                                json!({"room":data["room"],"requestId":data["requestId"],"status":data["status"]}),
                            ));
                            result["_meta"]["assignment"] = data;
                            result
                        }
                        Ok(_) => response(Err(
                            "OMS withheld this assignment during its outbound scan.".into(),
                        )),
                        Err(e) => response(Err(e)),
                    }
                } else if name == CHAT {
                    match result {
                        Ok(mut data) => {
                            if data["self"] == true
                                && data["provider"] == "codex"
                                && let Some(thread) =
                                    params["_meta"]["threadId"].as_str().filter(|v| uuid(v))
                            {
                                data["nativeUri"] = json!(format!("codex://threads/{thread}"));
                            }
                            let summary = json!({"room":data["room"],"participant":data["participant"],"source":data["source"],"count":data["messages"].as_array().map(Vec::len)});
                            let mut result = response(Ok(summary));
                            if crate::output::outbound(data.to_string().as_bytes(), &Value::Null)
                                .is_err()
                            {
                                return reply(
                                    id,
                                    response(Err(
                                        "OMS withheld this conversation during its outbound scan."
                                            .into(),
                                    )),
                                );
                            }
                            result["_meta"]["chat"] = data;
                            result
                        }
                        Err(e) => response(Err(e)),
                    }
                } else {
                    response(result)
                }
            }
            _ => return error(id, -32601, "method not found"),
        };
        reply(
            id,
            self.complete(result, params, method.starts_with("resources/")),
        )
    }

    fn graph_reply(&self, id: &Value, params: &Value, data: PanelResult<Value>) -> Value {
        let mut frame = reply(id, self.complete(response(data), params, false));
        // write_response uses this ASCII encoding and appends one newline.
        let fits = |frame: &Value| {
            crate::value::encode(frame, true, false, false, false)
                .as_bytes()
                .len()
                < crate::OUTPUT_LIMIT
        };
        if frame["result"]["isError"] != false || fits(&frame) {
            return frame;
        }
        let graph = &frame["result"]["structuredContent"];
        let nodes = graph["nodes"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let ids = nodes
            .iter()
            .take(4)
            .filter_map(|n| n["id"].as_str())
            .map(|id| id.chars().take(80).collect::<String>())
            .collect::<Vec<_>>()
            .join(", ");
        let text = format!(
            "Project graph ({}; {}): {} nodes, {} relationships on this page. First nodes: {}. Use node or query to inspect. Full page in structuredContent.",
            graph["status"].as_str().unwrap_or("unknown"),
            graph["provenance"]["source"].as_str().unwrap_or("unknown"),
            nodes.len(),
            graph["edges"].as_array().map_or(0, Vec::len),
            ids
        );
        for summary in [
            text.as_str(),
            "Project graph available in structuredContent.",
        ] {
            frame["result"]["content"] = json!([{"type":"text","text":summary}]);
            if fits(&frame) {
                return frame;
            }
        }
        reply(
            id,
            self.complete(
                response(Err(
                    "OMS project graph exceeds the response limit. Request fewer nodes.".into(),
                )),
                params,
                false,
            ),
        )
    }

    fn panel_exec(
        &self,
        repo: &Path,
        args: Vec<OsString>,
        allow_partial: bool,
    ) -> PanelResult<(Value, bool)> {
        let (stdout, partial) = self.panel_output(repo, args, allow_partial)?;
        Ok((
            crate::value::from_str(&stdout).map_err(|_| "Unreadable OMS response.")?,
            partial,
        ))
    }

    fn panel_output(
        &self,
        repo: &Path,
        args: Vec<OsString>,
        allow_partial: bool,
    ) -> PanelResult<(String, bool)> {
        let request = CommandRequest {
            args: &args,
            cwd: repo,
            timeout: Duration::from_secs(30),
            interrupted: self.interrupted,
        };
        let output = self.executor.execute(&request).map_err(|_| "OMS could not confirm the request outcome. Refresh project status before retrying.")?;
        if output.code != 0 && !(allow_partial && output.code == 1) {
            // Never return captured diagnostic prefixes. In particular the
            // capture may end in the middle of a credential or private path.
            let known = [
                (
                    "Room chat unavailable for this conversation",
                    "The current conversation is no longer the confirmed Main of that Room. Refresh the panel.",
                ),
                (
                    "attempt unavailable for this Room participant",
                    "This attempt is unavailable for the selected Room participant.",
                ),
                (
                    "evidence binding changed during collection",
                    "The selected attempt changed. Refresh the participant reader.",
                ),
                (
                    "room id already exists",
                    "This request is already reserved. Use oms_start_status with its original requestId.",
                ),
                (
                    "live or unverified",
                    "A Main is already active or its previous state is unverified.",
                ),
                (
                    "tmux on",
                    "The OMS execution host requires tmux to launch a Main.",
                ),
                (
                    "CLI on PATH",
                    "The selected Main is unavailable on this execution host.",
                ),
                (
                    "desktop Main startup is unconfirmed",
                    "The Main start was submitted. Use oms_start_status with its original requestId to check its lifecycle.",
                ),
                (
                    "stale or mismatched",
                    "The approval question changed. Refresh the approval list.",
                ),
                (
                    "room_inventory_incomplete",
                    "room_inventory_incomplete: OMS could not verify unique assignment ownership within the Room inventory limits.",
                ),
                (
                    "conversation is not the owner",
                    "The current conversation is not the confirmed Main of that Room.",
                ),
                (
                    "assignment needs",
                    "The assignment needs a supported provider, title, task and request UUID.",
                ),
                (
                    "message id conflicts",
                    "That question or request already has different recorded content.",
                ),
                (
                    "invalid project repository",
                    "The selected project is not a valid OMS or Git repository.",
                ),
            ];
            return Err(known
                .iter()
                .find(|(needle, _)| output.stderr.contains(needle))
                .map_or(
                    "OMS refused the operation. Refresh the project and check the host runtime.",
                    |(_, safe)| *safe,
                )
                .into());
        }
        if output.stdout_bytes > output.stdout.len() {
            return Err("OMS response exceeds the desktop capture limit.".into());
        }
        Ok((output.stdout, output.code != 0))
    }

    fn issue_preview(&self, repo: &Path, args: &Value) -> PanelResult<Value> {
        if [
            "kind",
            "number",
            "inventory",
            "boardId",
            "cursor",
            "candidates",
        ]
        .iter()
        .any(|key| args.get(key).is_some())
        {
            return Err("Select only one issue to review.".into());
        }
        let number = args["issuePreview"]
            .as_u64()
            .filter(|n| (1..=9_999_999_999).contains(n))
            .ok_or("Invalid project issue number.")?;
        // The native reader resolves the project's declared repository and origin itself.
        let argv = vec![
            "intent".into(),
            "draft".into(),
            "--repo".into(),
            repo.as_os_str().to_owned(),
            "--from-project-issue".into(),
            number.to_string().into(),
            "--dry-run".into(),
        ];
        let (data, _) = self
            .panel_exec(repo, argv, false)
            .map_err(|_| "Cannot verify an open issue in the selected project.")?;
        if self.panel_repo(args)? != repo {
            return Err(BINDING.into());
        }
        let source = &data["source"];
        let url = source["reference"].as_str().unwrap_or("");
        let slug = url
            .strip_prefix("https://github.com/")
            .and_then(|s| s.strip_suffix(&format!("/issues/{number}")))
            .unwrap_or("");
        let revision = source["revision"].as_str().unwrap_or("");
        let sha = source["sha256"].as_str().unwrap_or("");
        if data["schema"] != 1
            || data["kind"] != "oms-intent-source"
            || data["trusted"] != false
            || source["kind"] != "github-issue"
            || source["state"] != "open"
            || !github_slug(slug)
            || revision.len() != 20
            || !revision.ends_with('Z')
            || chrono::DateTime::parse_from_rfc3339(revision).is_err()
            || revision.as_bytes()[10] != b'T'
            || sha.len() != 64
            || !sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !source["bytes"]
                .as_u64()
                .is_some_and(|n| (1..=32768).contains(&n))
        {
            return Err("Unverified project issue metadata.".into());
        }
        Ok(
            json!({"issuePreview":{"kind":"github-issue","reference":url,"state":"open",
            "revision":revision,"sha256":sha,"bytes":source["bytes"]}}),
        )
    }

    fn panel_call(&self, name: &str, args: &Value, meta: &Value) -> PanelResult<Value> {
        if name == PROJECTS {
            if crate::delegated() {
                return Err("A delegated worker cannot enumerate desktop projects.".into());
            }
            let projects = configured_projects()?;
            let binding = if !projects.is_empty() {
                "catalog"
            } else if std::env::var_os("OMS_PANEL_REPO").is_some() {
                self.panel_repo(args)?;
                "pinned"
            } else if self.panel_repo(args).is_ok() {
                "cwd"
            } else {
                "unbound"
            };
            return Ok(json!({
                "projects": projects.into_keys().collect::<Vec<_>>(),
                "binding": binding,
                "readOnly": self.config.profile == ToolProfile::McpApp,
            }));
        }
        if name == OPEN {
            if args.get("project").is_some() {
                self.panel_repo(args)?;
            }
            let mut result = json!({"panel":"OMS","project":args["project"]});
            if let Some(flag) = args.get("leadConversations") {
                let requested = flag
                    .as_bool()
                    .ok_or("leadConversations must be a boolean.")?;
                if requested {
                    result["leadConversations"] = json!(true);
                }
            }
            if self.config.profile == ToolProfile::McpApp {
                result["readOnly"] = json!(true);
            }
            return Ok(result);
        }
        let repo = self.panel_repo(args)?;
        let path = repo.as_os_str().to_owned();
        if name == SNAPSHOT {
            return Ok(json!({"dashboard":self.panel_snapshot(&repo, meta)?}));
        }
        if crate::delegated()
            || std::env::var_os("OMS_ROOM_PARTICIPANT").is_some_and(|v| !v.is_empty())
        {
            return Err("This desktop control requires the parent app connection.".into());
        }
        if name == CAPABILITIES {
            return self.panel_capabilities(&repo, args);
        }
        if name == GIT && args.get("issuePreview").is_some() {
            return self.issue_preview(&repo, args);
        }
        let request_room = || -> PanelResult<String> {
            let key = args["requestId"]
                .as_str()
                .filter(|v| uuid(v))
                .ok_or("A lowercase UUID requestId is required.")?;
            Ok(format!("oms-app-{key}"))
        };
        if matches!(name, OPEN_LEADS | LEAD_STATUS | SEND_LEAD) {
            return self.lead_conversations(&repo, name, args, meta);
        }
        if matches!(name, RECORD_ISSUE | ISSUE_STATUS) {
            return self.reviewed_issue(&repo, name, args, meta);
        }
        if name == ASSIGN || name == ASSIGN_RESULT {
            return self.assignment(&repo, name, args, meta);
        }
        let argv = match name {
            CHAT => {
                let thread = meta["threadId"]
                    .as_str()
                    .filter(|v| uuid(v))
                    .ok_or("Open this reader from the Room owner's bound conversation.")?;
                let ident = |key: &str| -> PanelResult<OsString> {
                    args[key]
                        .as_str()
                        .filter(|v| {
                            (1..=160).contains(&v.len())
                                && v.as_bytes()[0].is_ascii_alphanumeric()
                                && v.bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                        })
                        .map(OsString::from)
                        .ok_or_else(|| "Invalid Room or participant.".into())
                };
                let mut cmd = vec![
                    "panel".into(),
                    "--repo".into(),
                    path,
                    "--desktop-chat".into(),
                    "--json".into(),
                    "--room".into(),
                    ident("room")?,
                    "--participant".into(),
                    ident("participant")?,
                    "--conversation-digest".into(),
                    crate::storage::hash(thread)[..32].into(),
                ];
                if let Some(section) = args.get("section") {
                    let section = section
                        .as_str()
                        .filter(|s| {
                            matches!(*s, "info" | "events" | "files" | "diff" | "transcript")
                        })
                        .ok_or("Unsupported evidence section.")?;
                    if args.get("before").is_some() {
                        return Err("Evidence uses page, not a Room cursor.".into());
                    }
                    cmd.extend([
                        "--evidence-section".into(),
                        section.into(),
                        "--evidence-attempt".into(),
                        ident("attempt")?,
                    ]);
                    if let Some(p) = args.get("page") {
                        let p = p
                            .as_i64()
                            .filter(|n| (1..=10000).contains(n))
                            .ok_or("Invalid evidence page.")?;
                        cmd.extend(["--page".into(), p.to_string().into()]);
                    }
                } else if args.get("page").is_some() || args.get("attempt").is_some() {
                    return Err("Select an evidence section for this attempt.".into());
                }
                if let Some(before) = args.get("before") {
                    let before = before
                        .as_i64()
                        .filter(|n| *n > 0)
                        .ok_or("Invalid chat cursor.")?;
                    cmd.extend(["--before".into(), before.to_string().into()]);
                }
                cmd
            }
            USAGE => vec![
                "panel".into(),
                "--repo".into(),
                path,
                "--desktop-usage".into(),
                "--json".into(),
            ],
            WORK_ITEMS => vec![
                "panel".into(),
                "--repo".into(),
                path,
                "--desktop-work-items".into(),
                "--json".into(),
            ],
            GIT => {
                let mut cmd = vec![
                    "github".into(),
                    "panel-status".into(),
                    "--repo".into(),
                    path,
                    "--json".into(),
                ];
                let candidates = match args.get("candidates") {
                    None => None,
                    Some(v) => Some(v.as_bool().ok_or("Invalid candidates flag.")?),
                };
                if candidates.is_some() && args.get("boardId").is_none() {
                    return Err("Board candidates require boardId.".into());
                }
                if candidates == Some(true) && args.get("cursor").is_some() {
                    return Err("Board candidates cannot combine with cursor.".into());
                }
                if args.get("inventory").is_some()
                    || args.get("boardId").is_some()
                    || args.get("cursor").is_some()
                {
                    if args.get("kind").is_some() || args.get("number").is_some() {
                        return Err("Choose an inventory or an exact reference.".into());
                    }
                    if let Some(id) = args.get("boardId") {
                        if args.get("inventory").is_some() {
                            return Err("Select one GitHub collection.".into());
                        }
                        let id = id
                            .as_str()
                            .filter(|s| {
                                s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                                    && s.len() <= 100
                                    && s.bytes().all(|b| {
                                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
                                    })
                            })
                            .ok_or("Invalid linked board ID.")?;
                        cmd.extend(["--project-id".into(), id.into()]);
                        if candidates == Some(true) {
                            cmd.push("--candidates".into());
                        }
                    } else {
                        let collection = args["inventory"]
                            .as_str()
                            .filter(|s| matches!(*s, "issues" | "pullRequests"))
                            .ok_or("Invalid GitHub collection.")?;
                        cmd.extend(["--inventory".into(), collection.into()]);
                    }
                    if let Some(cursor) = args.get("cursor") {
                        let value = cursor
                            .as_str()
                            .filter(|s| {
                                !s.is_empty()
                                    && s.len() <= 500
                                    && s.bytes().all(|b| {
                                        b.is_ascii_alphanumeric() || b"+/=._-".contains(&b)
                                    })
                            })
                            .ok_or("Invalid GitHub cursor.")?;
                        cmd.extend(["--cursor".into(), value.into()]);
                    }
                } else if args.get("kind").is_some() || args.get("number").is_some() {
                    let flag = match args["kind"].as_str() {
                        Some("issue") => "--issue",
                        Some("pull") => "--pull",
                        _ => return Err("Select an issue or pull request and its number.".into()),
                    };
                    let number = args["number"]
                        .as_i64()
                        .filter(|n| (1..=9_999_999_999).contains(n))
                        .ok_or("Invalid issue or pull request number.")?;
                    cmd.extend([flag.into(), number.to_string().into()]);
                }
                cmd
            }
            GRAPH => {
                let node = match args.get("node") {
                    None | Some(Value::Null) => None,
                    Some(v) => {
                        let id = v
                            .as_str()
                            .filter(|s| valid_graph_node_id(s))
                            .ok_or("Invalid graph node id.")?;
                        Some(id)
                    }
                };
                let query = match args.get("query") {
                    None | Some(Value::Null) => None,
                    Some(v) => {
                        let q = v
                            .as_str()
                            .map(str::trim)
                            .filter(|s| {
                                !s.is_empty()
                                    && s.len() <= 256
                                    && !s.chars().any(char::is_control)
                                    && !s.contains('\0')
                            })
                            .ok_or("Invalid graph query.")?;
                        crate::output::outbound(q.as_bytes(), &Value::Null)
                            .map_err(|_| "Query withheld by the OMS outbound scan.")?;
                        Some(q)
                    }
                };
                if node.is_some() && query.is_some() {
                    return Err("Choose either node or query, not both.".into());
                }
                let limit = match args.get("limit") {
                    None | Some(Value::Null) => 40,
                    Some(v) => v
                        .as_i64()
                        .filter(|n| (1..=60).contains(n))
                        .ok_or("Invalid graph limit.")?,
                };
                let depth = match args.get("depth") {
                    None | Some(Value::Null) => 2,
                    Some(v) => v
                        .as_i64()
                        .filter(|n| (0..=3).contains(n))
                        .ok_or("Invalid graph depth.")?,
                };
                let include_tests = match args.get("includeTests") {
                    None | Some(Value::Null) => false,
                    Some(v) => v.as_bool().ok_or("Invalid includeTests flag.")?,
                };
                let mut cmd = vec![
                    "graph".into(),
                    "project".into(),
                    "panel".into(),
                    "--repo".into(),
                    path,
                    "--json".into(),
                    "--limit".into(),
                    limit.to_string().into(),
                    "--depth".into(),
                    depth.to_string().into(),
                ];
                if include_tests {
                    cmd.push("--include-tests".into());
                }
                if let Some(id) = node {
                    cmd.push(format!("--node={id}").into());
                }
                if let Some(q) = query {
                    cmd.push(format!("--query={q}").into());
                }
                cmd
            }
            VIEW => {
                let view = args["view"]
                    .as_str()
                    .filter(|name| VIEWS.contains(name))
                    .ok_or("Unsupported OMS view.")?;
                let page = match args.get("page") {
                    Some(v) => v
                        .as_i64()
                        .filter(|n| (1..=1000).contains(n))
                        .ok_or("Invalid OMS page.")?,
                    None => 1,
                };
                let mut cmd = vec![
                    "panel".into(),
                    "--repo".into(),
                    path,
                    "--desktop-view".into(),
                    view.into(),
                    "--page".into(),
                    page.to_string().into(),
                    "--json".into(),
                ];
                if let Some(thread) = meta["threadId"].as_str().filter(|v| uuid(v)) {
                    cmd.extend([
                        "--conversation-digest".into(),
                        crate::storage::hash(thread)[..32].into(),
                    ]);
                }
                cmd
            }
            START_STATUS => vec![
                "room".into(),
                "desktop-start-status".into(),
                "--repo".into(),
                path,
                "--id".into(),
                request_room()?.into(),
                "--json".into(),
            ],
            START => {
                let goal = args["goal"]
                    .as_str()
                    .filter(|v| {
                        !v.trim().is_empty()
                            && !v.chars().any(char::is_control)
                            && v.chars().count() <= 2000
                    })
                    .ok_or("Expected a one-line goal of 1–2000 characters.")?;
                crate::output::outbound(goal.as_bytes(), &Value::Null)
                    .map_err(|_| "Goal withheld by the OMS outbound scan.")?;
                let main = args["main"]
                    .as_str()
                    .filter(|v| matches!(*v, "codex" | "claude" | "agy" | "pi"))
                    .ok_or("Unsupported Main provider.")?;
                vec![
                    "panel".into(),
                    "--repo".into(),
                    path,
                    "--host".into(),
                    "desktop".into(),
                    "--launch".into(),
                    main.into(),
                    "--room".into(),
                    request_room()?.into(),
                    format!("--task={goal}").into(),
                    "--json".into(),
                ]
            }
            APPROVALS => vec![
                "room".into(),
                "desktop-requests".into(),
                "--repo".into(),
                path,
                "--json".into(),
            ],
            DECIDE => {
                let (room, msg) = args["id"]
                    .as_str()
                    .and_then(|s| s.split_once('/'))
                    .filter(|(a, b)| {
                        [a, b].iter().all(|v| {
                            v.len() <= 160
                                && !v.is_empty()
                                && v.as_bytes()[0].is_ascii_alphanumeric()
                                && v.bytes()
                                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
                        })
                    })
                    .ok_or("Invalid question id.")?;
                let version = args["version"]
                    .as_i64()
                    .filter(|v| *v > 0)
                    .ok_or("Invalid question version.")?
                    .to_string();
                let digest = args["digest"]
                    .as_str()
                    .filter(|v| {
                        v.len() == 64
                            && v.bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    })
                    .ok_or("Invalid scope digest.")?;
                let decision = args["decision"]
                    .as_str()
                    .filter(|v| matches!(*v, "approve" | "deny"))
                    .ok_or("Invalid decision.")?;
                vec![
                    "room".into(),
                    "desktop-decide".into(),
                    "--repo".into(),
                    path,
                    "--id".into(),
                    room.into(),
                    "--message".into(),
                    msg.into(),
                    "--version".into(),
                    version.into(),
                    "--digest".into(),
                    digest.into(),
                    "--decision".into(),
                    decision.into(),
                    "--json".into(),
                ]
            }
            _ => unreachable!(),
        };
        let (data, _) = self.panel_exec(&repo, argv, false)?;
        if data.to_string().len() > crate::OUTPUT_LIMIT {
            return Err("OMS desktop detail exceeds the response limit.".into());
        }
        match name {
            CHAT if data["room"] == args["room"]
                && data["participant"] == args["participant"]
                && data["source"] == "room"
                && data["messages"].is_array() =>
            {
                let mut data = data;
                self.attach_assignment_result(&repo, args, meta, &mut data);
                Ok(data)
            }
            USAGE if data["providers"].is_array() && data["attempts"].is_array() => Ok(data),
            WORK_ITEMS => {
                if self.panel_repo(args)? != repo {
                    return Err(BINDING.into());
                }
                if !valid_work_items_receipt(&data) {
                    return Err(
                        "OMS returned an unrecognized receipt. Refresh project status before retrying."
                            .into(),
                    );
                }
                let encoded = data.to_string();
                if encoded.len() + json!(&encoded).to_string().len() + 1024 > crate::OUTPUT_LIMIT {
                    return Err("OMS desktop detail exceeds the response limit.".into());
                }
                Ok(data)
            }
            GIT if data["git"].is_object() && data["github"].is_object() => Ok(data),
            GRAPH
                if data.to_string().len() <= 52_000
                    && data["schema"] == 1
                    && data["kind"] == "oms-panel-project-graph"
                    && matches!(
                        data["status"].as_str(),
                        Some("ready" | "partial" | "empty" | "not_found" | "unknown")
                    )
                    && data["provenance"].is_object()
                    && data["limits"].is_object()
                    && data["counts"].is_object()
                    && data["nodes"].is_array()
                    && data["edges"].is_array()
                    && data["nodes"].as_array().is_some_and(|n| n.len() <= 60)
                    && data["edges"].as_array().is_some_and(|e| e.len() <= 180) =>
            {
                Ok(data)
            }
            VIEW if data["view"] == args["view"]
                && data["page"].as_i64() == Some(args["page"].as_i64().unwrap_or(1))
                && data["columns"].is_array()
                && data["rows"].is_array() =>
            {
                if data["view"] == "connections" {
                    connections(data)
                } else {
                    Ok(data)
                }
            }
            START
                if data["status"] == "started"
                    && data["attempt"].as_str().is_some_and(|id| !id.is_empty())
                    && data["provider"] == args["main"]
                    && data["room"]
                        == format!("oms-app-{}", args["requestId"].as_str().unwrap_or("")) =>
            {
                Ok(
                    json!({"status":"started","provider":data["provider"],"room":data["room"],"attempt":data["attempt"],"requestId":args["requestId"]}),
                )
            }
            START_STATUS
                if data["requestId"] == args["requestId"]
                    && match data["status"].as_str() {
                        Some("not_found") => data["room"].is_null() && data["attempt"].is_null(),
                        Some("reserved") => {
                            data["room"] == request_room()? && data["attempt"].is_null()
                        }
                        Some("recorded") => {
                            data["room"] == request_room()?
                                && data["attempt"].as_str().is_some_and(|v| !v.is_empty())
                                && data["terminal"].is_boolean()
                        }
                        _ => false,
                    } =>
            {
                Ok(data)
            }
            APPROVALS if data["approvals"].is_array() => Ok(data),
            DECIDE
                if data["decision"] == args["decision"]
                    && data["id"] == args["id"]
                    && data["digest"] == args["digest"] =>
            {
                Ok(data)
            }
            _ => Err(
                "OMS returned an unrecognized receipt. Refresh project status before retrying."
                    .into(),
            ),
        }
    }

    fn reviewed_issue(
        &self,
        repo: &Path,
        name: &str,
        args: &Value,
        meta: &Value,
    ) -> PanelResult<Value> {
        use std::os::unix::fs::MetadataExt;
        if self.config.profile != ToolProfile::ChatGptPanel {
            return Err("A bound desktop Main is required.".into());
        }
        let thread = meta["threadId"]
            .as_str()
            .filter(|v| uuid(v))
            .ok_or("Open this control from the Room owner's bound conversation.")?;
        let consumer = crate::storage::hash(thread);
        let id = |key: &str| {
            args[key]
                .as_str()
                .filter(|s| crate::matches("[a-zA-Z0-9][a-zA-Z0-9._-]{0,159}", s))
        };
        let room = id("room").ok_or("Invalid Room.")?;
        let owner = id("owner").ok_or("Invalid Main participant.")?;
        let request = args
            .get("requestId")
            .map(|v| {
                v.as_str()
                    .filter(|s| uuid(s))
                    .ok_or("A lowercase UUID requestId is required.")
            })
            .transpose()?;
        let selected = args.get("issue").is_some() || args.get("sha256").is_some();
        if (name == RECORD_ISSUE && (!selected || request.is_none()))
            || (name == ISSUE_STATUS && selected == request.is_some())
        {
            return Err(
                "Choose an exact receipt ID or a reviewed issue and SHA-256 for lookup.".into(),
            );
        }
        let sha = |v: &Value| {
            v.as_str().is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        };
        let mut extra = vec![("--owner", owner)];
        if selected {
            let issue = args["issue"]
                .as_str()
                .filter(|s| canonical_issue(s))
                .ok_or("Select an exact canonical GitHub issue.")?;
            if !sha(&args["sha256"]) {
                return Err("A reviewed SHA-256 is required.".into());
            }
            extra.extend([
                ("--issue", issue),
                ("--digest", args["sha256"].as_str().unwrap()),
            ]);
        }
        let action = if name == RECORD_ISSUE {
            "desktop-issue-record"
        } else {
            "desktop-issue-status"
        };
        let identity = || {
            std::fs::metadata(repo)
                .map(|m| (m.dev(), m.ino()))
                .map_err(|_| BINDING.to_owned())
        };
        let before = identity()?;
        let argv = Self::assignment_argv(action, repo, room, request, &consumer[..32], &extra);
        let (data,_)=self.panel_exec(repo,argv,false).map_err(|_|"OMS could not confirm the reviewed issue receipt. Check its status with the reviewed issue and SHA-256 or the original request ID.")?;
        if self.panel_repo(args)? != repo || identity()? != before {
            return Err(BINDING.into());
        }
        let actual = data["requestId"]
            .as_str()
            .filter(|s| uuid(s))
            .filter(|s| match request {
                Some(expected) => expected == *s,
                None => {
                    s.as_bytes()[14] == b'8'
                        && matches!(s.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
                }
            })
            .ok_or("OMS returned an unrecognized reviewed issue receipt.")?;
        let count = data.as_object().map(|v| v.keys().count());
        if name == ISSUE_STATUS && count == Some(2) && data["status"] == "not_found" {
            return Ok(json!({"status":"not_found","requestId":actual}));
        }
        if count != Some(8)
            || data["schema"] != 1
            || data["kind"] != "oms-reviewed-issue-receipt"
            || data["status"] != "recorded"
            || data["room"] != room
            || data["participant"] != owner
            || !data["issue"].as_str().is_some_and(canonical_issue)
            || !sha(&data["sha256"])
            || selected && (data["issue"] != args["issue"] || data["sha256"] != args["sha256"])
        {
            return Err("OMS returned an unrecognized reviewed issue receipt.".into());
        }
        Ok(
            json!({"schema":1,"kind":"oms-reviewed-issue-receipt","status":"recorded","room":room,"participant":owner,
            "requestId":actual,"issue":data["issue"],"sha256":data["sha256"]}),
        )
    }

    fn assignment_argv(
        action: &str,
        repo: &Path,
        room: &str,
        request: Option<&str>,
        digest: &str,
        extra: &[(&str, &str)],
    ) -> Vec<OsString> {
        let mut argv: Vec<OsString> = vec![
            "room".into(),
            action.into(),
            "--repo".into(),
            repo.as_os_str().into(),
            "--id".into(),
            room.into(),
            "--conversation-digest".into(),
            digest.into(),
        ];
        if let Some(id) = request {
            argv.extend(["--request".into(), id.into()]);
        }
        for (flag, value) in extra {
            argv.extend([OsString::from(*flag), OsString::from(*value)]);
        }
        argv.push("--json".into());
        argv
    }

    fn lead_conversations(
        &self,
        repo: &Path,
        name: &str,
        args: &Value,
        meta: &Value,
    ) -> PanelResult<Value> {
        let thread = meta["threadId"]
            .as_str()
            .filter(|v| uuid(v))
            .ok_or("Open this control from the Room owner's bound conversation.")?;
        let room = args["room"]
            .as_str()
            .filter(|v| crate::matches("[a-zA-Z0-9][a-zA-Z0-9._-]{0,159}", v))
            .ok_or("Invalid Room.")?;
        let selected = args
            .get("provider")
            .map(|v| {
                v.as_str()
                    .filter(|p| LEAD_PROVIDERS.contains(p))
                    .ok_or("Unsupported Lead provider.")
            })
            .transpose()?;
        lead_turns::validate(name, args, selected)?;
        let digest = crate::storage::hash(thread);
        let project_key = || -> PanelResult<String> {
            if self.panel_repo(args)? != repo {
                return Err(BINDING.into());
            }
            let stat = fs::symlink_metadata(repo).map_err(|_| BINDING.to_owned())?;
            if !stat.is_dir() {
                return Err(BINDING.into());
            }
            let mut identity = b"oms-lead-project-v1\0".to_vec();
            identity.extend_from_slice(repo.as_os_str().as_encoded_bytes());
            identity.extend_from_slice(&stat.dev().to_be_bytes());
            identity.extend_from_slice(&stat.ino().to_be_bytes());
            Ok(crate::storage::hash(identity))
        };
        let key = project_key()?;
        if args
            .get("projectKey")
            .is_some_and(|v| v.as_str() != Some(key.as_str()))
        {
            return Err("The selected project changed. Reopen the panel for that project.".into());
        }
        let bound = || {
            if project_key()? == key {
                Ok(())
            } else {
                Err(BINDING.to_owned())
            }
        };
        let mut rows = Vec::new();
        let mut private = None;
        let mut turn = None;
        let mut has_authority = false;
        for provider in LEAD_PROVIDERS
            .into_iter()
            .filter(|p| selected.is_none_or(|s| s == *p))
        {
            bound()?;
            let run = |action, request: Option<&str>, extra: &[(&str, &str)]| {
                bound()?;
                let argv = Self::assignment_argv(action, repo, room, request, &digest[..32], extra);
                self.panel_exec(repo, argv, false).map(|(data, _)| data)
            };
            let provider_arg = [("--provider", provider)];
            let mut row =
                json!({"provider":provider,"requestId":Value::Null,"status":"unavailable"});
            // The Room store resolves the address against the live Main and its current
            // enrollment. The app cannot choose a different request, participant or peer.
            let Ok(prior) = run("desktop-lead-status", None, &provider_arg) else {
                rows.push(row);
                continue;
            };
            let Some(request) = prior["request"].as_str().filter(|id| {
                uuid(id)
                    && id.as_bytes()[14] == b'8'
                    && matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
            }) else {
                rows.push(row);
                continue;
            };
            let canonical = if provider == "agy" {
                "antigravity"
            } else {
                provider
            };
            let valid = |data: &Value| {
                data["request"] == request
                    && data["room"] == room
                    && data["participant"] == format!("ask-{request}")
                    && data["provider"] == canonical
                    && data["purpose"] == "provider-lead"
                    && data["role"] == "worker"
                    && data["readOnly"] == true
                    && data["task"] == LEAD_PROMPT
                    && ((data["status"] == "assigned"
                        && data["thread"].is_null()
                        && data["operation"].is_null())
                        || (data["status"] == "started"
                            && data["operation"].as_str().is_some_and(crate::operation)
                            && data["operation"] == data["thread"]))
            };
            if prior["status"] != "not_found" && !valid(&prior) {
                rows.push(row);
                continue;
            }
            has_authority = true;
            row["requestId"] = json!(request);
            let resolve = || -> PanelResult<(&'static str, Value)> {
                let write = name == OPEN_LEADS;
                let data = if write && prior["status"] == "not_found" {
                    run("desktop-lead-open", Some(request), &provider_arg)?
                } else {
                    prior.clone()
                };
                if !write && data["status"] == "not_found" {
                    return Ok(("not_started", Value::Null));
                }
                if !valid(&data) {
                    return Err("Invalid Lead reservation.".into());
                }
                let binding =
                    crate::storage::hash(json!([&digest[..32], room, request]).to_string());
                let operation =
                    assignment_address(&data, &binding).ok_or("Missing Lead operation address.")?;
                if data["status"] == "started" && data["operation"] != operation {
                    return Err("Invalid Lead operation.".into());
                }
                let mut linked = data["status"] == "started";
                if !linked && write {
                    // A stale reservation may recover its existing operation, never start it again.
                    if prior["status"] == "not_found" && data["fresh"] == true {
                        let proof = run("desktop-lead-status", Some(request), &provider_arg)?;
                        if !valid(&proof)
                            || proof["assignedAt"] != data["assignedAt"]
                            || proof["status"] != "assigned"
                        {
                            return Err("Lead ownership or reservation changed.".into());
                        }
                        bound()?;
                        if !self.start_assignment_peer(
                            repo,
                            provider,
                            LEAD_PROMPT,
                            &operation,
                            &binding,
                            true,
                        )? {
                            return Ok(("unconfirmed", Value::Null));
                        }
                    } else if assignment_directory(repo, &data, &operation, &binding, true)?
                        .is_none()
                    {
                        return Ok(("unconfirmed", Value::Null));
                    }
                    let receipt = run(
                        "desktop-assign-receipt",
                        Some(request),
                        &[("--operation", operation.as_str())],
                    )?;
                    linked = valid(&receipt)
                        && receipt["assignedAt"] == data["assignedAt"]
                        && receipt["operation"] == operation;
                }
                if !linked {
                    return Ok(("unconfirmed", Value::Null));
                }
                let Some(dir) = assignment_directory(repo, &data, &operation, &binding, true)?
                else {
                    return Ok(("unconfirmed", Value::Null));
                };
                let (state, exit) = crate::peer::status(&dir);
                if state == "running" {
                    return Ok(("starting", Value::Null));
                }
                if state != "done" || exit != Some(0) {
                    return Ok(("unavailable", Value::Null));
                }
                // The supervisor exit alone cannot attest a provider response. Inspect
                // its own single-seat answer, keeping every byte out of model content.
                let proof = self
                    .peer_result(&json!({"repo":repo,"operation":operation}))
                    .ok()
                    .and_then(|r| r.as_str().and_then(|s| crate::value::from_str(s).ok()));
                let Some(answer) = proof
                    .as_ref()
                    .filter(|p| {
                        p["operation"] == operation
                            && p["exit"] == 0
                            && p["answer_details"]
                                .as_array()
                                .is_some_and(|d| d.len() == 1 && d[0]["exit"] == "0")
                    })
                    .and_then(|p| p["answer"].as_str())
                    .filter(|text| !text.trim().is_empty())
                else {
                    return Ok(("unconfirmed", Value::Null));
                };
                let transcript = if selected.is_some() {
                    // This is the original kickoff answer only. Other user turns need
                    // their own protected receipt and exact child operation proof.
                    json!({"turns":[{"role":canonical,"text":crate::storage::byte_prefix(answer,4000),"truncated":answer.len()>4000,"at":Value::Null}],"cursor":Value::Null,"hasMore":false})
                } else {
                    Value::Null
                };
                let live = run("desktop-lead-status", Some(request), &provider_arg)?;
                if !valid(&live)
                    || live["operation"] != operation
                    || live["assignedAt"] != data["assignedAt"]
                {
                    return Err("Lead receipt changed.".into());
                }
                // An operation can outlive its OMS thread. The native thread reader
                // validates the log's exact ID and closed marker. A live thread
                // created by start_bound_peer has its opening collaboration note.
                bound()?;
                let path = repo.to_str().ok_or("Repository path is not UTF-8.")?;
                let witness = self.command(
                    &[
                        "thread",
                        "--repo",
                        path,
                        "--id",
                        operation.as_str(),
                        "context",
                        "--require-open",
                        "--notes-only",
                        "--turns",
                        "1",
                        "--max-bytes",
                        "256",
                    ],
                    repo,
                    Duration::from_secs(5),
                )?;
                if witness.code != 0 || witness.stdout.is_empty() {
                    return Ok(("unconfirmed", Value::Null));
                }
                Ok(("ready", transcript))
            };
            let (status, mut transcript) = resolve().unwrap_or(("unconfirmed", Value::Null));
            if name == SEND_LEAD && args["leadRequestId"] != request {
                return Err("The selected Lead changed. Reopen its conversation.".into());
            }
            if (name == SEND_LEAD || args.get("turnId").is_some() || args.get("before").is_some())
                && status != "ready"
            {
                return Err("The original Lead conversation could not be verified.".into());
            }
            if selected.is_some() && status == "ready" {
                let kickoff = prior["operation"]
                    .as_str()
                    .ok_or("The original Lead receipt is unavailable.")?;
                let binding =
                    crate::storage::hash(json!([&digest[..32], room, request]).to_string());
                let conversation =
                    lead_turns::Conversation::open(repo, kickoff, &binding, &prior, provider)?;
                let current = || {
                    let live = run("desktop-lead-status", Some(request), &provider_arg)?;
                    if !valid(&live)
                        || live["operation"] != kickoff
                        || live["assignedAt"] != prior["assignedAt"]
                    {
                        return Err("The original Lead receipt changed.".into());
                    }
                    Ok(())
                };
                turn = conversation.exchange(self, name, args, &mut transcript, current)?;
            }
            row["status"] = json!(status);
            if selected.is_some() {
                private = Some(
                    json!({"room":room,"provider":provider,"requestId":request,"status":status,"transcript":transcript}),
                );
            }
            rows.push(row);
        }
        bound()?;
        if !has_authority {
            return Err("OMS could not verify the current Main in the selected Room.".into());
        }
        let mut result = json!({"room":room,"projectKey":key,"leads":rows});
        if let Some(turn) = turn {
            result["turn"] = turn;
        }
        if let Some(private) = private {
            result["leadConversation"] = private;
        }
        Ok(result)
    }

    fn start_assignment_peer(
        &self,
        repo: &Path,
        provider: &str,
        task: &str,
        operation: &str,
        binding: &str,
        preserve_current: bool,
    ) -> PanelResult<bool> {
        let path = repo.to_str().ok_or("Repository path is not UTF-8.")?;
        Ok(self
            .start_bound_peer(
                &json!({"repo":path,"kind":"ask","prompt":task,"providers":provider,
            "debate_rounds":0,"include_memory":false,"include_task":false}),
                Some((operation, binding)),
                preserve_current,
            )
            .ok()
            .and_then(|t| t.as_str().and_then(|t| crate::value::from_str(t).ok()))
            .is_some_and(|v| {
                v["started"] == true && v["operation"] == operation && v["thread"] == operation
            }))
    }

    /// Provider assignment for the Room owned by this exact bound conversation.
    /// Authority is the host `threadId` plus the Room's own joined-owner proof;
    /// the widget supplies only a Room name and a request id.
    fn assignment(
        &self,
        repo: &Path,
        name: &str,
        args: &Value,
        meta: &Value,
    ) -> PanelResult<Value> {
        let thread = meta["threadId"]
            .as_str()
            .filter(|v| uuid(v))
            .ok_or("Open this control from the Room owner's bound conversation.")?;
        let digest = crate::storage::hash(thread)[..32].to_owned();
        let room = args["room"]
            .as_str()
            .filter(|v| {
                (1..=160).contains(&v.len())
                    && v.as_bytes()[0].is_ascii_alphanumeric()
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
            .ok_or("Invalid Room.")?;
        let request = args["requestId"]
            .as_str()
            .filter(|v| uuid(v))
            .ok_or("A lowercase UUID requestId is required.")?;
        let binding = crate::storage::hash(json!([digest, room, request]).to_string());
        let check = |data: &Value| -> PanelResult<()> {
            if data["request"] == request
                && data["room"] == room
                && data["participant"] == format!("ask-{request}")
                && data["role"] == "researcher"
                && data["readOnly"] == true
                && matches!(data["provider"].as_str(), Some("claude" | "antigravity"))
                && match data["status"].as_str() {
                    Some("assigned") => data["operation"].is_null() && data["thread"].is_null(),
                    Some("started") => {
                        data["operation"] == data["thread"]
                            && data["operation"].as_str().is_some_and(crate::operation)
                    }
                    _ => false,
                }
            {
                Ok(())
            } else {
                Err(
                    "OMS returned an unrecognized assignment record. Refresh before retrying."
                        .into(),
                )
            }
        };
        let summary = |data: &Value, status: &str| {
            json!({"status":status,"requestId":request,"room":data["room"],"participant":data["participant"],
              "provider":data["provider"],"role":"researcher","readOnly":true,"title":data["title"],
              "thread":data["thread"],"operation":data["operation"]})
        };
        if name == ASSIGN_RESULT {
            let after = args
                .get("after")
                .map_or(Some(""), Value::as_str)
                .ok_or("Invalid cursor.")?;
            let (data, _) = self.panel_exec(
                repo,
                Self::assignment_argv(
                    "desktop-assign-status",
                    repo,
                    room,
                    Some(request),
                    &digest,
                    &[],
                ),
                false,
            )?;
            if data["request"] == request && data["status"] == "not_found" {
                return Ok(json!({"room":room,"requestId":request,"status":"not_found"}));
            }
            check(&data)?;
            let mut out = summary(&data, data["status"].as_str().unwrap_or("assigned"));
            out["task"] = data["task"].clone();
            let recovered = data["status"] == "assigned";
            let operation = if recovered {
                assignment_address(&data, &binding)
            } else {
                data["operation"].as_str().map(str::to_owned)
            };
            if let Some(op) = operation.as_deref()
                && let Some(dir) = assignment_directory(repo, &data, op, &binding, recovered)?
            {
                if recovered {
                    out["status"] = json!("recovered");
                    out["operation"] = json!(op);
                    out["thread"] = json!(op);
                }
                let (state, exit) = crate::peer::status(&dir);
                out["execution"] = json!({"state":state,"exit":exit});
                out["transcript"] = self.provider_thread(
                    repo,
                    op,
                    data["provider"].as_str().unwrap_or_default(),
                    after,
                );
            }
            return Ok(out);
        }
        let provider = args["provider"]
            .as_str()
            .filter(|v| matches!(*v, "claude" | "agy"))
            .ok_or("Unsupported read-only provider.")?;
        let clean_line = |key: &str, limit: usize| -> PanelResult<&str> {
            args[key]
                .as_str()
                .filter(|v| {
                    !v.trim().is_empty()
                        && v.len() <= limit
                        && (key == "task" || !v.chars().any(char::is_control))
                })
                .ok_or_else(|| format!("Invalid {key}."))
        };
        let title = clean_line("title", 120)?;
        let task = clean_line("task", 4000)?;
        for field in [title, task] {
            crate::output::outbound(field.as_bytes(), &Value::Null)
                .map_err(|_| "Assignment withheld by the OMS outbound scan.")?;
        }
        let repair = match args.get("repairOnly") {
            None => false,
            Some(v) => v.as_bool().ok_or("Invalid repairOnly flag.")?,
        };
        // A repair must remain unable to create an intent even if the Room
        // changed since the widget observed the unlinked operation.
        let argv = if repair {
            Self::assignment_argv(
                "desktop-assign-status",
                repo,
                room,
                Some(request),
                &digest,
                &[],
            )
        } else {
            Self::assignment_argv(
                "desktop-assign",
                repo,
                room,
                Some(request),
                &digest,
                &[
                    ("--provider", provider),
                    ("--label", title),
                    ("--text", task),
                ],
            )
        };
        let (data, _) = self.panel_exec(repo, argv, false)?;
        check(&data)?;
        if data["provider"]
            != if provider == "agy" {
                "antigravity"
            } else {
                "claude"
            }
            || data["task"] != task
            || data["title"] != title
        {
            return Err("The request does not match the recorded assignment.".into());
        }
        if data["status"] == "started" {
            if repair {
                assignment_directory(
                    repo,
                    &data,
                    data["operation"].as_str().unwrap_or_default(),
                    &binding,
                    false,
                )?;
            }
            return Ok(summary(&data, "recorded"));
        }
        let unconfirmed = |data: &Value| summary(data, "unconfirmed");
        let Some(operation) = assignment_address(&data, &binding) else {
            return Ok(unconfirmed(&data));
        };
        if !repair && data["fresh"] == true {
            if !self.start_assignment_peer(repo, provider, task, &operation, &binding, false)? {
                return Ok(unconfirmed(&data));
            }
        } else if (!repair && data["fresh"] != false)
            || assignment_directory(repo, &data, &operation, &binding, true)?.is_none()
        {
            return Ok(unconfirmed(&data));
        }
        let recorded = self.panel_exec(
            repo,
            Self::assignment_argv(
                "desktop-assign-receipt",
                repo,
                room,
                Some(request),
                &digest,
                &[("--operation", &operation)],
            ),
            false,
        );
        match recorded {
            Ok((receipt, _))
                if check(&receipt).is_ok()
                    && receipt["operation"] == operation
                    && ["provider", "title", "task", "assignedAt"]
                        .iter()
                        .all(|k| receipt[*k] == data[*k]) =>
            {
                Ok(summary(&receipt, "started"))
            }
            _ => {
                let mut out = unconfirmed(&data);
                out["operation"] = json!(operation);
                Ok(out)
            }
        }
    }

    /// Bounded, read-only turns of the exact thread the Room receipt names.
    fn provider_thread(&self, repo: &Path, thread: &str, provider: &str, after: &str) -> Value {
        let Some(path) = repo.to_str() else {
            return Value::Null;
        };
        self.peer_result(&json!({"repo":path,"thread":thread,"after":after}))
            .ok()
            .and_then(|t| t.as_str().and_then(|t| crate::value::from_str(t).ok()))
            .filter(|v| v["thread"] == thread)
            .and_then(|v| {
                let rows = v["turns"].as_array()?;
                let turns = rows
                    .iter()
                    .filter(|r| {
                        r["role"] == "answer"
                            && r["provider"] == provider
                            && r["quality"] == "ok"
                            && r["text"].is_string()
                    })
                    .map(|r| json!({"role":provider,"text":r["text"],"at":r["ts"]}))
                    .collect::<Vec<_>>();
                Some(json!({"turns":turns,"cursor":v["cursor"],"hasMore":v["has_more"]}))
            })
            .unwrap_or(Value::Null)
    }

    /// Show an assigned provider's returned turns in the reader's existing
    /// recorded-result slot; Room messages keep their own `room` source.
    fn attach_assignment_result(&self, repo: &Path, args: &Value, meta: &Value, data: &mut Value) {
        let Some(request) = args["participant"]
            .as_str()
            .and_then(|p| p.strip_prefix("ask-"))
            .filter(|r| uuid(r))
        else {
            return;
        };
        if !data["result"].is_null() {
            return;
        }
        let Ok(result) = self.assignment(
            repo,
            ASSIGN_RESULT,
            &json!({"room":args["room"],"requestId":request}),
            meta,
        ) else {
            return;
        };
        data["assignment"] = json!({"state":result["status"],"execution":result["execution"],"title":result["title"]});
        if result["status"] == "recovered" {
            data["assignment"]["task"] = result["task"].clone();
        }
        if result["execution"]["state"] == "done"
            && result["execution"]["exit"] == 0
            && let Some(path) = repo.to_str()
            && let Ok(raw) = self.peer_result(&json!({"repo":path,"operation":result["operation"]}))
            && let Some(proof) = raw.as_str().and_then(|v| crate::value::from_str(v).ok())
            && proof["answer_details"]
                .as_array()
                .is_some_and(|a| a.len() == 1 && a[0]["exit"] == "0")
            && let Some(answer) = proof["answer"].as_str().filter(|s| !s.is_empty())
        {
            let cut: String = answer.chars().take(6000).collect();
            data["result"] = json!({"kind":"read-only provider","attempt":result["operation"],"text":cut,"truncated":cut.len()<answer.len() || proof["answer_truncated"]==true, "at":Value::Null});
            return;
        }
        let turns = result["transcript"]["turns"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if turns.is_empty() {
            return;
        }
        let text = turns
            .iter()
            .map(|t| {
                format!(
                    "{}: {}",
                    t["role"].as_str().unwrap_or("provider"),
                    t["text"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let cut: String = text.chars().take(6000).collect();
        data["result"] = json!({"kind":"read-only provider","attempt":result["operation"],
          "text":cut,"truncated":cut.len() < text.len(),"at":Value::Null});
    }

    pub(super) fn panel_snapshot(&self, repo: &Path, meta: &Value) -> PanelResult<Value> {
        let mut argv = vec![
            "dashboard".into(),
            "--repo".into(),
            repo.as_os_str().into(),
            "--json".into(),
            "--panel-snapshot".into(),
        ];
        // Codex supplies threadId on each call, outside model/widget arguments.
        // Only its native consumer hash leaves this process; it grants no authority.
        let thread = meta["threadId"].as_str().filter(|v| uuid(v));
        if let Some(thread) = thread {
            argv.extend([
                "--conversation-digest".into(),
                crate::storage::hash(thread)[..32].into(),
            ]);
        }
        let (report, partial) = self.panel_exec(repo, argv, true)?;
        if report["kind"] != "oms-panel-snapshot"
            || report["schema"] != 1
            || !report["dashboard"].is_object()
        {
            return Err("Unsupported OMS panel snapshot version.".into());
        }
        let mut dashboard = report["dashboard"].clone();
        let main = &dashboard["conversationMain"];
        let room_id = |v: &Value| {
            v.as_str().is_some_and(|s| {
                (1..=160).contains(&s.len())
                    && !s.starts_with(['-', '.'])
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        };
        let harness_unverified = !main["harness"].is_null()
            && !(thread.is_some()
                && dashboard["conversationBinding"] == "matched"
                && main["source"] == "room-session"
                && main["provider"] == "codex"
                && main["state"] == "joined"
                && room_id(&main["room"])
                && room_id(&main["participant"])
                && matches!(main["harness"].as_str(), Some("codex" | "pi")));
        if harness_unverified {
            dashboard["conversationMain"]["harness"] = Value::Null;
        }
        if partial || harness_unverified {
            let was_degraded = dashboard["degraded"] == true;
            dashboard["degraded"] = json!(true);
            if dashboard["observation"].is_object()
                && (harness_unverified
                    || !was_degraded
                    || dashboard["observation"]["completeness"] == "complete")
            {
                if dashboard["observation"]["completeness"] == "complete" {
                    dashboard["observation"]["completeness"] = json!("partial");
                }
                dashboard["observation"]["revision"] = Value::Null;
            }
        }
        if dashboard.to_string().len() > crate::OUTPUT_LIMIT {
            return Err("OMS panel snapshot exceeds its output limit.".into());
        }
        Ok(dashboard)
    }

    fn panel_capabilities(&self, repo: &Path, args: &Value) -> PanelResult<Value> {
        let offset = match args.get("offset") {
            None => 0usize,
            Some(v) => v
                .as_i64()
                .filter(|n| (0..=10_000).contains(n))
                .map(|n| n as usize)
                .ok_or("Invalid command catalog offset.")?,
        };
        let (stdout, _) = self.panel_output(repo, vec!["list".into(), "--all".into()], false)?;
        crate::output::outbound(stdout.as_bytes(), &Value::Null)
            .map_err(|_| "OMS withheld this response during its outbound scan".to_owned())?;
        command_catalog(&stdout, offset)
    }
}

fn command_name(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.ends_with('-')
        && !value.contains("--")
}

fn command_catalog(stdout: &str, offset: usize) -> PanelResult<Value> {
    let mut names = std::collections::BTreeSet::new();
    let mut commands = Vec::new();
    // Reserve space for the result envelope and its numeric fields. Each item
    // also appears escaped inside MCP's text content for non-widget clients.
    let mut bytes = 512;
    let mut full = false;
    for (index, line) in stdout.lines().enumerate() {
        let (name, description) = line.split_once(' ').ok_or("Unreadable OMS response.")?;
        let description = description.trim_start_matches(' ');
        if !command_name(name)
            || description.is_empty()
            || description.chars().any(char::is_control)
            || !names.insert(name)
        {
            return Err("Unreadable OMS response.".into());
        }
        if names.len() > 10_000 {
            return Err("OMS command catalog exceeds the pagination limit.".into());
        }
        if index < offset || full {
            continue;
        }
        let row = json!({"name":name,"description":description});
        let json = row.to_string();
        let size = json.len() + json!(json).to_string().len() + 2;
        if bytes + size > crate::OUTPUT_LIMIT {
            if commands.is_empty() {
                return Err("OMS desktop detail exceeds the response limit.".into());
            }
            full = true;
        } else {
            bytes += size;
            commands.push(row);
        }
    }
    let total = names.len();
    if total == 0 {
        return Err("Unreadable OMS response.".into());
    }
    let count = commands.len();
    let mut page = json!({"schema":1,"commands":commands,"total":total,"omitted":total-count});
    if full {
        page["nextOffset"] = json!(offset + count);
    }
    Ok(page)
}

#[cfg(test)]
mod work_items_tests {
    use super::*;

    #[test]
    fn mcp_work_snapshot_keeps_expired_and_fenced_plan_states_distinct() {
        let task = |id, state, expired| {
            json!({
                "id":id,"title":"Fixture task","state":state,"claimExpired":expired,
                "owner":null,"depends":[],"issue":null,"result":null,"verification":null,
                "revision":null
            })
        };
        let mut review = task("review", "review", false);
        review["issue"] = json!("example-org/example-repo#19");
        review["issueCache"] =
            json!({"state":null,"freshness":"unavailable","reason":"no matching cache entry"});
        review["prReviewCandidate"] = json!({
            "reference":"example-org/example-repo#19","state":"merged","freshness":"fresh",
            "disposition":"completed-external"
        });
        let snapshot = json!({
            "schema":1,"omitted":0,"availability":"ready","items":[
                task("expired", "stale", true),
                task("live", "running", false),
                task("unproven", "claimed", false),
                review
            ]
        });
        assert!(valid_work_items_receipt(&snapshot));
        let rendered = response(Ok(snapshot.clone()));
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["structuredContent"], snapshot);

        let change_item = |index, edit: fn(&mut Value)| {
            let mut inconsistent = snapshot.clone();
            let Value::Array(items) = &mut inconsistent["items"] else {
                unreachable!();
            };
            edit(&mut items[index]);
            inconsistent
        };
        let inconsistent = change_item(2, |item| item["state"] = json!("stale"));
        assert!(!valid_work_items_receipt(&inconsistent));
        let inconsistent = change_item(3, |item| {
            item["issueCache"].as_object_mut().unwrap().remove("reason");
        });
        assert!(!valid_work_items_receipt(&inconsistent));
        let inconsistent = change_item(3, |item| {
            item["prReviewCandidate"]["disposition"] = json!("superseded");
        });
        assert!(!valid_work_items_receipt(&inconsistent));
    }
}
