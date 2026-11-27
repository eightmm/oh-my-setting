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

#[path = "lead_plan_context.rs"]
mod lead_plan_context;
#[path = "lead_turns.rs"]
mod lead_turns;
pub(crate) use lead_turns::LeadTurnCache;

const URI: &str = "ui://oms/conversation-panel/v37.html";
const V33: &str = "ui://oms/conversation-panel/v33.html";
const V34: &str = "ui://oms/conversation-panel/v34.html";
const V35: &str = "ui://oms/conversation-panel/v35.html";
const V36: &str = "ui://oms/conversation-panel/v36.html";
const V32: &str = "ui://oms/conversation-panel/v32.html";
const V31: &str = "ui://oms/conversation-panel/v31.html";
const V30: &str = "ui://oms/conversation-panel/v30.html";
const V29: &str = "ui://oms/conversation-panel/v29.html";
const V28: &str = "ui://oms/conversation-panel/v28.html";
const V27: &str = "ui://oms/conversation-panel/v27.html";
const V26: &str = "ui://oms/conversation-panel/v26.html";
const V25: &str = "ui://oms/conversation-panel/v25.html";
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
const WORKER_OUTPUT: &str = "oms_worker_output";
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
const SETTINGS: &str = "oms_lead_settings";
const UPDATE_SETTINGS: &str = "oms_update_lead_settings";
const SEND_LEAD: &str = "oms_send_lead_message";
const STOP_LEAD: &str = "oms_stop_lead_message";
const LEAD_STATUS: &str = "oms_lead_conversation_status";
const PREPARE_LEAD_CONTEXT: &str = "oms_prepare_lead_plan_context";
const LEAD_PROVIDERS: [&str; 3] = ["codex", "claude", "agy"];
pub(crate) const LEAD_PROMPT: &str = "Acknowledge availability as this Room's provider Lead, then wait for explicit work. Do not inspect or change the repository, use tools, or delegate.";
const ASSIGN: &str = "oms_assign_provider";
const ASSIGN_RESULT: &str = "oms_assign_result";
const BINDING: &str = "Select a host-configured OMS project, or bind this connection to a project with OMS_PANEL_REPO.";
type PanelResult<T> = Result<T, String>;

// A cursor is an observation, never authority. The reader re-establishes its
// physical project, Room custody and immutable answer before checking it.
fn output_cursor(scope: &str, revision: &str, position: usize) -> String {
    format!(
        "{position}:{}",
        crate::storage::hash(json!(["oms-output-page-v1", scope, revision, position]).to_string())
    )
}

fn output_position(cursor: &str, maximum: usize) -> PanelResult<usize> {
    if cursor.is_empty() {
        return Ok(0);
    }
    let (position, digest) = cursor
        .split_once(':')
        .ok_or("Invalid output page cursor.")?;
    let number = position
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0 && *n <= maximum)
        .ok_or("Invalid output page cursor.")?;
    if number.to_string() != position || !crate::matches("[0-9a-f]{64}", digest) {
        return Err("Invalid output page cursor.".into());
    }
    Ok(number)
}

fn check_output_cursor(
    cursor: &str,
    scope: &str,
    revision: &str,
    position: usize,
) -> PanelResult<()> {
    if !cursor.is_empty() && cursor != output_cursor(scope, revision, position) {
        return Err(
            "The output changed or this page belongs to another selection. Reopen the result."
                .into(),
        );
    }
    Ok(())
}

fn output_page(
    page: &Value,
    text: &str,
    cursor: &str,
    position: usize,
    scope: &str,
) -> PanelResult<Value> {
    let err = "The recorded output page could not be verified.";
    let revision = page["revision"]
        .as_str()
        .filter(|s| crate::matches("[0-9a-f]{64}", s))
        .ok_or(err)?;
    let digest = page["digest"]
        .as_str()
        .filter(|s| crate::matches("[0-9a-f]{64}", s))
        .ok_or(err)?;
    let offset = page["offset"]
        .as_u64()
        .filter(|n| *n <= 16 * 1024 * 1024)
        .ok_or(err)?;
    let total = page["totalBytes"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 16 * 1024 * 1024)
        .ok_or(err)?;
    let end = offset + text.len() as u64;
    let more = end < total;
    if text.is_empty()
        || text.len() > 8192
        || end > total
        || (position == 0 && offset != 0)
        || page["bytes"].as_u64() != Some(text.len() as u64)
        || page["hasMore"] != more
        || page["page"].as_u64() != Some(position as u64 + 1)
        || if more {
            page["nextPage"].as_u64() != Some(position as u64 + 2)
        } else {
            !page["nextPage"].is_null()
        }
    {
        return Err(err.into());
    }
    check_output_cursor(cursor, scope, revision, position)?;
    Ok(
        json!({"offset":offset,"bytes":text.len(),"totalBytes":total,"digest":digest,
        "hasMore":more,"nextCursor":more.then(||output_cursor(scope,revision,position+1))}),
    )
}

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
    message: Option<(&str, &Value)>,
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
        || message.is_some_and(|(digest, route)| {
            proof["lead_followup"] != true
                || proof["prompt_sha256"] != digest
                || if route["model"].is_null() {
                    proof.get("lead_route").is_some()
                } else {
                    proof["lead_route"] != *route
                }
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

fn valid_call_id(request: &Value) -> bool {
    let id = &request["id"];
    matches!(id, Value::Number(_)) || id.as_str().is_some_and(|s| s.len() <= 1024)
}

pub(super) fn open_request(request: &Value) -> bool {
    valid_call_id(request)
        && request["method"] == "tools/call"
        && request["params"]["name"].as_str() == Some(OPEN)
}

pub(super) fn runtime_request(request: &Value) -> bool {
    valid_call_id(request)
        && request["method"] == "tools/call"
        && matches!(
            request["params"]["name"].as_str(),
            Some(
                SNAPSHOT
                    | WORK_ITEMS
                    | USAGE
                    | CHAT
                    | WORKER_OUTPUT
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
                    | STOP_LEAD
                    | SETTINGS
                    | UPDATE_SETTINGS
                    | PREPARE_LEAD_CONTEXT
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
          "annotations":{"readOnlyHint":!write,"destructiveHint":write && name != PREPARE_LEAD_CONTEXT,"idempotentHint":!write || name == PREPARE_LEAD_CONTEXT,"openWorldHint":matches!(name, START | ASSIGN)},
          "_meta":{"ui":{"visibility":["app","model"]}}})
    };
    let project = json!({"type":"string","description":"Exact alias from oms_list_projects when the host has a project map."});
    let scope = json!({"project":project});
    let mut open_scope = scope.clone();
    open_scope["project"]["description"] = json!(
        "Pass the exact alias from oms_list_projects when the host has a project map; the initial view is included only when project names an exact alias from oms_list_projects on hosts with a project map."
    );
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
        json!({"project":project,"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$","description":"Required with evidenceCursor: the independently observed physical project key."},"room":ident,"participant":ident,"before":{"type":"integer","minimum":1},"section":{"type":"string","enum":["info","events","files","diff","transcript"]},"attempt":ident,"page":{"type":"integer","minimum":1,"maximum":10000},"evidenceCursor":{"type":"string","maxLength":100,"description":"Empty for the first full transcript or diff page; then use its returned nextCursor."}}),
        &["room", "participant"],
        false,
    );
    chat["_meta"]["ui"]["visibility"] = json!(["app"]);
    let mut worker_output = make(
        WORKER_OUTPUT,
        "Read the recorded output for an exact worker under its Lead in the current Main’s Room.",
        json!({"project":project,"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$"},"room":ident,"leadAttempt":ident,"workerAttempt":ident,"answerPage":{"type":"string","maxLength":100,"description":"Empty for the first full answer page; then use its returned nextCursor."}}),
        &["projectKey", "room", "leadAttempt", "workerAttempt"],
        false,
    );
    worker_output["_meta"]["ui"]["visibility"] = json!(["app"]);
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
        "Open bounded OMS readiness conversations for the selected Leads in this chat's verified Room. Omitted providers selects all three. Reopening reuses the same requests.",
        json!({"project":project,"room":ident,"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$","description":"Optional previously observed physical project key; refuses if the binding changed."},"providers":{"type":"array","items":{"type":"string","enum":LEAD_PROVIDERS},"minItems":1,"maxItems":3,"uniqueItems":true}}),
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
    let mut stop = make(
        STOP_LEAD,
        "Stop this Lead's running reply to the user's message. A finished or already stopped message is left unchanged.",
        json!({"project":project,"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$"},"room":ident,"provider":{"type":"string","enum":LEAD_PROVIDERS},"leadRequestId":{"type":"string","format":"uuid"},"turnId":{"type":"string","format":"uuid"}}),
        &["projectKey", "room", "provider", "leadRequestId", "turnId"],
        true,
    );
    stop["annotations"] = json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false});
    stop["_meta"]["ui"]["visibility"] = json!(["app"]);
    let settings_scope = json!({"project":project,"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$"},"room":ident,"provider":{"type":"string","enum":LEAD_PROVIDERS},"leadRequestId":{"type":"string","format":"uuid"}});
    let mut settings = make(
        SETTINGS,
        "Read this Lead's saved model and effort for future messages, with host-cached or routing policy options.",
        settings_scope.clone(),
        &["projectKey", "room", "provider", "leadRequestId"],
        false,
    );
    let mut choices = settings_scope;
    choices["revision"] = json!({"type":"string","pattern":"^[0-9a-f]{64}$"});
    for field in ["model", "effort"] {
        choices[field] = json!({"type":["string","null"]});
    }
    let mut update = make(
        UPDATE_SETTINGS,
        "Set this Lead's model and effort for future messages at the observed revision. Clearing both restores OMS routing defaults.",
        choices,
        &[
            "projectKey",
            "room",
            "provider",
            "leadRequestId",
            "revision",
            "model",
            "effort",
        ],
        true,
    );
    update["annotations"] = json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false});
    for t in [&mut settings, &mut update] {
        t["_meta"]["ui"]["visibility"] = json!(["app"]);
    }
    let mut prepare = make(
        PREPARE_LEAD_CONTEXT,
        "Prepare a context pack from an explicitly reviewed Lead answer excerpt for one exact ready Plan task. Supply the observed answer page, answer digest, UTF-8 byte range and excerpt digest, plus the reviewed task digest and execution provider. Rechecks source ownership and task state. Returns the exact excerpt, provenance and separate managed Plan preview/run commands; never claims, starts, accepts or lands a task.",
        json!({"project":project,"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$"},"room":ident,"provider":{"type":"string","enum":LEAD_PROVIDERS},"leadRequestId":{"type":"string","format":"uuid"},"turnId":turn_id,"answerPage":{"type":"string","maxLength":100},"answerSha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"offset":{"type":"integer","minimum":0,"maximum":8388608},"bytes":{"type":"integer","minimum":1,"maximum":4096},"excerptSha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"excerpt":{"type":"string","minLength":1,"maxLength":4096,"description":"The exact text already selected and reviewed; metadata alone cannot export private text."},"taskId":ident,"taskSha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"executionProvider":{"type":"string","enum":["claude","antigravity"]}}),
        &[
            "projectKey",
            "room",
            "provider",
            "leadRequestId",
            "turnId",
            "answerPage",
            "answerSha256",
            "offset",
            "bytes",
            "excerptSha256",
            "excerpt",
            "taskId",
            "taskSha256",
            "executionProvider",
        ],
        true,
    );
    prepare["_meta"]["ui"]["visibility"] = json!(["app"]);
    let all = json!([
        opener,
        chat,
        worker_output,
        record_issue,
        issue_status,
        open_leads,
        send,
        stop,
        settings,
        update,
        prepare,
        make(
            LEAD_STATUS,
            "Read provider readiness in the current Main’s Room. A selected provider’s answer is visible only in the panel.",
            json!({"project":project,"room":ident,"provider":{"type":"string","enum":LEAD_PROVIDERS},"turnId":turn_id,"before":{"type":"string","description":"An older-page cursor returned by this conversation."},"projectKey":{"type":"string","pattern":"^[0-9a-f]{64}$"},"leadRequestId":{"type":"string","format":"uuid"},"answerPage":{"type":"string","maxLength":100,"description":"Read a completed turn's full answer. Requires exact projectKey, leadRequestId and turnId. Empty starts; otherwise use nextCursor."}}),
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
            json!({
                "cards": {
                    "type": "boolean",
                    "description": "Include one cached status card per configured catalog alias without running a snapshot on the request path."
                }
            }),
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
            "Read bounded, strictly read-only task-focused work items for the selected project from the canonical plan and verified result bindings. Ready-state items may include a checked planSelection with taskId/taskSha256/provider for explicit reviewed Lead-context preparation; this is a read reference, not approval or scheduling authority.",
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
    let optional = [
        "claimExpired",
        "issueCache",
        "prReviewCandidate",
        "planSelection",
    ];
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
                        && row.get("planSelection").is_none_or(|selection| {
                            row["state"] == "ready"
                                && selection.as_object().is_some_and(|fields| {
                                    fields.keys().count() == 6
                                        && [
                                            "source",
                                            "readOnly",
                                            "planId",
                                            "taskId",
                                            "taskSha256",
                                            "provider",
                                        ]
                                        .iter()
                                        .all(|key| fields.contains_key(key))
                                })
                                && selection["source"] == "canonical-plan"
                                && selection["readOnly"] == true
                                && selection["taskId"] == row["id"]
                                && selection["planId"]
                                    .as_str()
                                    .is_some_and(|s| crate::matches("plan_[0-9a-f]{32}", s))
                                && selection["taskSha256"]
                                    .as_str()
                                    .is_some_and(|s| crate::matches("[0-9a-f]{64}", s))
                                && (selection["provider"].is_null()
                                    || selection["provider"].as_str().is_some_and(id))
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
            let is_project_cards = data["cards"].is_array();
            if (data["dashboard"]["managedScratch"].is_object()
                || is_git_detail
                || is_project_cards)
                && crate::value::encode(
                    &json!({"content":[{"type":"text","text":&text}],"structuredContent":&data,"isError":false}),
                    true,
                    false,
                    false,
                    false,
                )
                .len()
                    + FRAME_RESERVE
                    >= crate::OUTPUT_LIMIT
            {
                text = if is_git_detail {
                    "Git and GitHub status available in structuredContent.".into()
                } else if is_project_cards {
                    "Project catalog and cached cards available in structuredContent.".into()
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
                    >= crate::OUTPUT_LIMIT
                {
                    return response(Err(if is_git_detail || is_project_cards {
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
                            | V25
                            | V26
                            | V27
                            | V28
                            | V29
                            | V30
                            | V31
                            | V32
                            | V33
                            | V34
                            | V35
                            | V36
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
                    PROJECTS => &["cards"],
                    OPEN => &["project", "leadConversations"],
                    OPEN_LEADS => &["project", "room", "projectKey", "providers"],
                    PREPARE_LEAD_CONTEXT => &[
                        "project",
                        "projectKey",
                        "room",
                        "provider",
                        "leadRequestId",
                        "turnId",
                        "answerPage",
                        "answerSha256",
                        "offset",
                        "bytes",
                        "excerptSha256",
                        "excerpt",
                        "taskId",
                        "taskSha256",
                        "executionProvider",
                    ],
                    LEAD_STATUS => &[
                        "project",
                        "room",
                        "provider",
                        "turnId",
                        "before",
                        "projectKey",
                        "leadRequestId",
                        "answerPage",
                    ],
                    SETTINGS => &["project", "room", "provider", "projectKey", "leadRequestId"],
                    UPDATE_SETTINGS => &[
                        "project",
                        "room",
                        "provider",
                        "projectKey",
                        "leadRequestId",
                        "revision",
                        "model",
                        "effort",
                    ],
                    SEND_LEAD => &[
                        "project",
                        "projectKey",
                        "room",
                        "provider",
                        "leadRequestId",
                        "turnId",
                        "text",
                    ],
                    STOP_LEAD => &[
                        "project",
                        "projectKey",
                        "room",
                        "provider",
                        "leadRequestId",
                        "turnId",
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
                    WORKER_OUTPUT => &[
                        "project",
                        "projectKey",
                        "room",
                        "leadAttempt",
                        "workerAttempt",
                        "answerPage",
                    ],
                    CHAT => &[
                        "project",
                        "projectKey",
                        "room",
                        "participant",
                        "before",
                        "section",
                        "attempt",
                        "page",
                        "evidenceCursor",
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
                if matches!(
                    name,
                    LEAD_STATUS | SEND_LEAD | STOP_LEAD | SETTINGS | UPDATE_SETTINGS
                ) {
                    let field = if matches!(name, SETTINGS | UPDATE_SETTINGS) {
                        "leadSettings"
                    } else {
                        "leadConversation"
                    };
                    match result {
                        Ok(mut data) => {
                            let private = data.as_object_mut().and_then(|o| o.remove(field));
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
                                result["_meta"][field] = private;
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
                } else if name == WORKER_OUTPUT {
                    match result {
                        Ok(data) => {
                            let public = json!({"room":data["room"],"projectKey":data["projectKey"],
                                "leadAttempt":data["leadAttempt"],"workerAttempt":data["workerAttempt"],"status":data["status"]});
                            let mut result = response(Ok(public));
                            result["_meta"]["workerOutput"] = data;
                            result
                        }
                        Err(err) => response(Err(err)),
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
                            let mut summary = json!({"room":data["room"],"participant":data["participant"],"source":data["source"],"count":data["messages"].as_array().map(Vec::len)});
                            if let Some(key) = data.get("projectKey") {
                                summary["projectKey"] = key.clone();
                            }
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
                } else if name == OPEN {
                    match result {
                        Ok(mut data)
                            if crate::output::outbound(
                                data.to_string().as_bytes(),
                                &Value::Null,
                            )
                            .is_ok() =>
                        {
                            let dashboard =
                                data.as_object_mut().and_then(|o| o.remove("dashboard"));
                            if dashboard.is_some() {
                                data["snapshot"] = json!("attached");
                            }
                            let mut result = response(Ok(data));
                            if let Some(dashboard) = dashboard {
                                result["_meta"]["dashboard"] = dashboard;
                            }
                            result
                        }
                        Ok(_) => response(Err(
                            "OMS withheld this response during its outbound scan".into(),
                        )),
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
        execute_panel_output(
            self.executor.as_ref(),
            self.interrupted,
            repo,
            args,
            allow_partial,
            Duration::from_secs(30),
        )
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
            let include_cards = match args.get("cards") {
                None => false,
                Some(v) => v.as_bool().ok_or("cards must be a boolean.")?,
            };
            if include_cards {
                return self.list_projects_with_cards(args);
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
            let mut keys = json!({});
            for alias in projects.keys() {
                let selection = json!({"project":alias});
                if let Ok(repo) = self.panel_repo(&selection)
                    && let Ok(key) = self.project_key(&repo, &selection)
                {
                    keys[alias.as_str()] = json!(key);
                }
            }
            let key = if matches!(binding, "pinned" | "cwd") {
                self.panel_repo(args)
                    .and_then(|repo| self.project_key(&repo, args))
                    .ok()
            } else {
                None
            };
            return Ok(json!({
                "projects": projects.into_keys().collect::<Vec<_>>(),
                "projectKeys":keys,"projectKey":key,
                "binding": binding,
                "readOnly": self.config.profile == ToolProfile::McpApp,
            }));
        }
        if name == OPEN {
            let resolved = if args.get("project").is_some() {
                Some(self.panel_repo(args)?)
            } else {
                None
            };
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
            if !self.open_snapshot {
                result["snapshotOmitted"] = json!("busy");
            } else if let Some(repo) = resolved.or_else(|| self.panel_repo(args).ok()) {
                if let Ok(dashboard) = self.panel_snapshot_projection(&repo, meta) {
                    let encoded = dashboard.to_string();
                    if encoded.len() > crate::OUTPUT_LIMIT {
                        result["snapshotOmitted"] = json!("size");
                    } else if crate::output::outbound(encoded.as_bytes(), &Value::Null).is_ok() {
                        result["dashboard"] = dashboard;
                    }
                }
            } else {
                result["snapshotOmitted"] = json!("project");
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
        if name == WORKER_OUTPUT {
            return self.worker_output(&repo, args, meta);
        }
        if name == PREPARE_LEAD_CONTEXT {
            return self.prepare_lead_plan_context(&repo, args, meta);
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
        if matches!(
            name,
            OPEN_LEADS | LEAD_STATUS | SEND_LEAD | STOP_LEAD | SETTINGS | UPDATE_SETTINGS
        ) {
            return self.lead_conversations(&repo, name, args, meta);
        }
        if matches!(name, RECORD_ISSUE | ISSUE_STATUS) {
            return self.reviewed_issue(&repo, name, args, meta);
        }
        if name == ASSIGN || name == ASSIGN_RESULT {
            return self.assignment(&repo, name, args, meta);
        }
        let evidence_page = if name == CHAT {
            args.get("evidenceCursor").map(|v| -> PanelResult<_> {
                if !matches!(args["section"].as_str(),Some("transcript"|"diff")) || args.get("page").is_some() {
                    return Err("Full evidence paging requires a transcript or diff and its returned cursor.".into());
                }
                let cursor = v.as_str().ok_or("Invalid output page cursor.")?;
                let position = output_position(cursor,9999)?;
                let key = self.project_key(&repo,args)?;
                if args["projectKey"] != key {
                    return Err("The selected project changed. Full evidence requires its observed project key.".into());
                }
                let scope = crate::storage::hash(json!([key,meta["threadId"],args["room"],args["participant"],args["attempt"],args["section"]]).to_string());
                Ok((cursor,position,key,scope))
            }).transpose()?
        } else {
            None
        };
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
                    if let Some((_, position, _, _)) = &evidence_page {
                        cmd.extend(["--page".into(), (position + 1).to_string().into()]);
                        cmd.push("--evidence-full".into());
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
                if let Some((cursor, position, key, scope)) = evidence_page {
                    if self.project_key(&repo, args)? != key
                        || data["evidence"]["attempt"] != args["attempt"]
                        || data["evidence"]["section"] != args["section"]
                    {
                        return Err("The evidence selection changed. Reopen the result.".into());
                    }
                    if data["evidence"]["status"] == "outbound_withheld" {
                        return Err("OMS withheld this evidence during its outbound scan.".into());
                    }
                    if data["evidence"].get("status").is_some() {
                        return Err("Full retained evidence is unavailable.".into());
                    }
                    data["projectKey"] = json!(key);
                    let text = data["evidence"]["text"]
                        .as_str()
                        .ok_or("Full retained evidence is unavailable.")?;
                    oms_scrub::outbound_page(
                        text,
                        oms_scrub::OUTBOUND_PAGE_BYTES,
                        oms_scrub::PageSelector::Offset(0),
                    )
                    .map_err(|e| match e {
                        oms_scrub::PageError::OutboundWithheld => {
                            "OMS withheld this evidence during its outbound scan."
                        }
                        oms_scrub::PageError::InvalidRange => {
                            "The recorded output page could not be verified."
                        }
                    })?;
                    let page = output_page(&data["evidence"], text, cursor, position, &scope)?;
                    for field in [
                        "offset",
                        "bytes",
                        "totalBytes",
                        "digest",
                        "nextCursor",
                        "hasMore",
                    ] {
                        data["evidence"][field] = page[field].clone();
                    }
                    if let Some(evidence) = data["evidence"].as_object_mut() {
                        evidence.remove("revision");
                    }
                } else if args.get("section").is_some() {
                    if data["evidence"]["status"] == "outbound_withheld" {
                        return Err("OMS withheld this evidence during its outbound scan.".into());
                    }
                    if data["evidence"].get("status").is_some() {
                        return Err("Full retained evidence is unavailable.".into());
                    }
                } else {
                    self.attach_assignment_result(&repo, args, meta, &mut data)?;
                }
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

    fn project_key(&self, repo: &Path, args: &Value) -> PanelResult<String> {
        if self.panel_repo(args)? != repo {
            return Err(BINDING.into());
        }
        oms_command_plan::reviewed_context::project_key(repo).map_err(|_| BINDING.to_owned())
    }

    fn worker_output(&self, repo: &Path, args: &Value, meta: &Value) -> PanelResult<Value> {
        let thread = meta["threadId"]
            .as_str()
            .filter(|v| uuid(v))
            .ok_or("Open this reader from the Room owner's bound conversation.")?;
        let id = |field: &str| {
            args[field]
                .as_str()
                .filter(|s| crate::matches("[a-zA-Z0-9][a-zA-Z0-9._-]{0,159}", s))
                .ok_or("Invalid Room or attempt.")
        };
        let (room, lead, worker) = (id("room")?, id("leadAttempt")?, id("workerAttempt")?);
        if lead == worker {
            return Err("Select a worker under its Lead.".into());
        }
        crate::output::outbound(format!("{room}\n{lead}\n{worker}").as_bytes(), &Value::Null)
            .map_err(|_| "OMS withheld this worker identity during its outbound scan.")?;
        let key = self.project_key(repo, args)?;
        if args["projectKey"] != key {
            return Err("The selected project changed. Reopen the panel for that project.".into());
        }
        let digest = crate::storage::hash(thread);
        let mut argv = vec![
            "panel".into(),
            "--repo".into(),
            repo.as_os_str().into(),
            "--desktop-worker-output".into(),
            "--json".into(),
            "--room".into(),
            room.into(),
            "--lead-attempt".into(),
            lead.into(),
            "--worker-attempt".into(),
            worker.into(),
            "--conversation-digest".into(),
            digest[..32].into(),
        ];
        let page = args
            .get("answerPage")
            .map(|v| -> PanelResult<(&str, usize)> {
                let cursor = v.as_str().ok_or("Invalid output page cursor.")?;
                Ok((cursor, output_position(cursor, 9999)?))
            })
            .transpose()?;
        if let Some((_, position)) = page {
            argv.extend(["--page".into(), (position + 1).to_string().into()]);
        }
        let (raw, _) = self.panel_output(repo, argv, false)?;
        let err = "The recorded worker output could not be verified.";
        oms_context::parse_json_bounded(raw.as_bytes(), 64 * 1024).map_err(|_| err)?;
        let data: Value = crate::value::from_str(&raw).map_err(|_| err)?;
        if self.project_key(repo, args)? != key {
            return Err(BINDING.into());
        }
        if data["room"] != room
            || data["leadAttempt"] != lead
            || data["workerAttempt"] != worker
            || !data["truncated"].is_boolean()
        {
            return Err(err.into());
        }
        match data["status"].as_str() {
            Some("outbound_withheld")
                if data.get("text").is_some_and(Value::is_null)
                    && data.get("source").is_some_and(Value::is_null)
                    && data["truncated"] == false
                    && data.get("page").is_none() =>
            {
                return Err("OMS withheld this worker output during its outbound scan.".into());
            }
            Some("complete")
                if matches!(
                    data["source"].as_str(),
                    Some("native-report" | "indexed-answer")
                ) =>
            {
                let text = data["text"]
                    .as_str()
                    .filter(|v| {
                        (page.is_some() || !v.trim().is_empty())
                            && v.len() <= 16 * 1024
                            && !v
                                .chars()
                                .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
                    })
                    .ok_or(err)?;
                let bound = if page.is_some() {
                    oms_scrub::OUTBOUND_PAGE_BYTES
                } else {
                    crate::PAGE_BYTES
                };
                oms_scrub::outbound_page(text, bound, oms_scrub::PageSelector::Offset(0)).map_err(
                    |e| match e {
                        oms_scrub::PageError::OutboundWithheld => {
                            "OMS withheld this worker output during its outbound scan."
                        }
                        oms_scrub::PageError::InvalidRange => err,
                    },
                )?;
            }
            Some("running" | "not_recorded" | "unavailable")
                if data.get("text").is_some_and(Value::is_null)
                    && data.get("source").is_some_and(Value::is_null)
                    && data["truncated"] == false => {}
            _ => return Err(err.into()),
        }
        let mut result = json!({"projectKey":key,"room":room,"leadAttempt":lead,"workerAttempt":worker,"status":data["status"],
                "source":data["source"],"text":data["text"],"truncated":data["truncated"]});
        if let Some((cursor, position)) = page {
            if data["status"] != "complete" {
                return Err("The selected worker has no verified completed answer.".into());
            }
            let scope = crate::storage::hash(json!([key, digest, room, lead, worker]).to_string());
            result["page"] = output_page(
                &data["page"],
                data["text"].as_str().ok_or(err)?,
                cursor,
                position,
                &scope,
            )?;
        }
        Ok(result)
    }

    fn lead_thread_open(&self, repo: &Path, operation: &str) -> PanelResult<bool> {
        let path = repo.to_str().ok_or("Repository path is not UTF-8.")?;
        let witness = self.command(
            &[
                "thread",
                "--repo",
                path,
                "--id",
                operation,
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

        Ok(witness.code == 0 && !witness.stdout.is_empty())
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
        let subset = args
            .get("providers")
            .map(|value| {
                let values = value
                    .as_array()
                    .filter(|v| !v.is_empty() && v.len() <= 3)
                    .ok_or("Select one to three distinct Lead providers.")?;
                let mut providers = Vec::new();
                for value in values {
                    let provider = value
                        .as_str()
                        .filter(|p| LEAD_PROVIDERS.contains(p))
                        .ok_or("Unsupported Lead provider.")?;
                    if providers.contains(&provider) {
                        return Err("Select one to three distinct Lead providers.");
                    }
                    providers.push(provider);
                }
                Ok(providers)
            })
            .transpose()?;
        let full_answer = args.get("answerPage").is_some();
        let settings = matches!(name, SETTINGS | UPDATE_SETTINGS);
        if settings
            && (selected.is_none()
                || !args["projectKey"].is_string()
                || !args["leadRequestId"].as_str().is_some_and(uuid))
        {
            return Err("Select the exact project and current Lead conversation.".into());
        }
        lead_turns::validate(name, args, selected)?;
        let digest = crate::storage::hash(thread);
        let project_key = || self.project_key(repo, args);
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
            .filter(|p| subset.as_ref().is_none_or(|s| s.contains(p)))
        {
            bound()?;
            let run = |action, request: Option<&str>, extra: &[(&str, &str)]| {
                bound()?;
                let argv = Self::assignment_argv(action, repo, room, request, &digest[..32], extra);
                self.panel_exec(repo, argv, false).map(|(data, _)| data)
            };
            let provider_arg = [("--provider", provider)];
            let mut row = json!({"provider":provider,"requestId":Value::Null,"status":"unavailable","latestTurn":Value::Null});
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
            let resolve = || -> PanelResult<(&'static str, Option<String>)> {
                let write = name == OPEN_LEADS;
                let data = if write && prior["status"] == "not_found" {
                    run("desktop-lead-open", Some(request), &provider_arg)?
                } else {
                    prior.clone()
                };
                if !write && data["status"] == "not_found" {
                    return Ok(("not_started", None));
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
                            return Ok(("unconfirmed", None));
                        }
                    } else if assignment_directory(repo, &data, &operation, &binding, true)?
                        .is_none()
                    {
                        return Ok(("unconfirmed", None));
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
                    return Ok(("unconfirmed", None));
                }
                let Some(dir) = assignment_directory(repo, &data, &operation, &binding, true)?
                else {
                    return Ok(("unconfirmed", None));
                };
                let (state, exit) = crate::peer::status(&dir);
                if state == "running" {
                    return Ok(("starting", None));
                }
                if state != "done" || exit != Some(0) {
                    return Ok(("unavailable", None));
                }
                // The supervisor exit alone cannot attest a provider response. Inspect
                // its own single-seat answer, keeping every byte out of model content.
                let proof = self
                    .peer_result(&json!({"repo":repo,"operation":operation}))
                    .ok()
                    .and_then(|r| r.as_str().and_then(|s| crate::value::from_str(s).ok()));
                let Some(answer) = proof
                    .as_ref()
                    .filter(|p| p["operation"] == operation && p["exit"] == 0)
                    .and_then(|p| p["answer_details"].as_array())
                    .and_then(|d| match d.as_slice() {
                        [detail]
                            if detail["exit"] == "0"
                                && detail["read_arguments"]["operation"] == operation =>
                        {
                            detail["read_arguments"]["answer_ref"].as_str()
                        }
                        _ => None,
                    })
                    .and_then(|reference| {
                        let (name, _) = reference.rsplit_once(':')?;
                        let (text, exit) =
                            crate::storage::owned_answer(repo, &operation, name).ok()?;
                        (exit == "0"
                            && reference == crate::storage::reference(&operation, name, &text)
                            && !text.trim().is_empty())
                        .then_some(text)
                    })
                else {
                    return Ok(("unconfirmed", None));
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
                if !self.lead_thread_open(repo, &operation)? {
                    return Ok(("unconfirmed", None));
                }
                let kickoff_answer =
                    (selected.is_some() && !settings && !full_answer).then_some(answer);
                Ok(("ready", kickoff_answer))
            };
            let (status, kickoff_answer) = resolve().unwrap_or(("unconfirmed", None));
            if (settings || matches!(name, SEND_LEAD | STOP_LEAD) || full_answer)
                && args["leadRequestId"] != request
            {
                return Err("The selected Lead changed. Reopen its conversation.".into());
            }
            if (settings
                || matches!(name, SEND_LEAD | STOP_LEAD)
                || args.get("turnId").is_some()
                || args.get("before").is_some())
                && status != "ready"
            {
                return Err("The original Lead conversation could not be verified.".into());
            }
            let mut transcript = if let Some(answer) = kickoff_answer {
                oms_scrub::outbound_page(
                    &answer,
                    crate::MAX_ANSWER,
                    oms_scrub::PageSelector::Offset(0),
                )
                .map_err(|err| match err {
                    oms_scrub::PageError::OutboundWithheld => {
                        "OMS withheld the Lead conversation during its outbound scan."
                    }
                    oms_scrub::PageError::InvalidRange => {
                        "The original Lead conversation could not be verified."
                    }
                })?;
                // This is the original kickoff answer only. Other user turns need
                // their own protected receipt and exact child operation proof.
                json!({"turns":[{"role":canonical,"text":crate::storage::byte_prefix(&answer,4000),"truncated":answer.len()>4000,"at":Value::Null}],"cursor":Value::Null,"hasMore":false})
            } else {
                Value::Null
            };
            if status == "ready" && prior["operation"].is_string() {
                let kickoff = prior["operation"]
                    .as_str()
                    .ok_or("The original Lead receipt is unavailable.")?;
                let binding =
                    crate::storage::hash(json!([&digest[..32], room, request]).to_string());
                if selected.is_some() {
                    let conversation =
                        lead_turns::Conversation::open(repo, kickoff, &binding, &prior, provider)?;
                    let current = || {
                        let live = run("desktop-lead-status", Some(request), &provider_arg)?;
                        if !valid(&live)
                            || live["operation"] != kickoff
                            || live["assignedAt"] != prior["assignedAt"]
                            || ((settings || full_answer)
                                && !self.lead_thread_open(repo, kickoff)?)
                        {
                            return Err("The original Lead receipt changed.".into());
                        }
                        Ok(())
                    };
                    if settings {
                        let mut detail =
                            conversation.settings(self, name == UPDATE_SETTINGS, args, &current)?;
                        let mut summary = json!({"projectKey":key,"room":room,"provider":provider,"leadRequestId":request,"revision":detail["revision"]});
                        for key in ["projectKey", "room", "provider", "leadRequestId"] {
                            detail[key] = summary[key].clone();
                        }
                        summary["leadSettings"] = detail;
                        bound()?;
                        return Ok(summary);
                    }
                    if full_answer {
                        let answer = conversation.answer_page(self, args, current)?;
                        transcript = json!({"turns":[],"cursor":Value::Null,"hasMore":false});
                        private = Some(
                            json!({"room":room,"provider":provider,"requestId":request,"status":status,
                            "transcript":transcript,"answer":answer}),
                        );
                    } else {
                        turn = conversation.exchange(self, name, args, &mut transcript, current)?;
                    }
                    match conversation.latest(self, true, bound) {
                        Ok(latest) => row["latestTurn"] = latest,
                        Err(err) => {
                            row["latestTurn"] = Value::Null;
                            row["turnState"] = err.marker();
                        }
                    }
                } else {
                    match lead_turns::Conversation::open(repo, kickoff, &binding, &prior, provider)
                        .map_err(|_| lead_turns::TurnLookupError::InvalidRecord)
                        .and_then(|conversation| conversation.latest(self, false, bound))
                    {
                        Ok(latest) => row["latestTurn"] = latest,
                        Err(err) => {
                            row["latestTurn"] = Value::Null;
                            row["turnState"] = err.marker();
                        }
                    }
                }
            }
            row["status"] = json!(status);
            if selected.is_some() && private.is_none() {
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
    fn attach_assignment_result(
        &self,
        repo: &Path,
        args: &Value,
        meta: &Value,
        data: &mut Value,
    ) -> PanelResult<()> {
        const WITHHELD: &str = "OMS withheld this conversation during its outbound scan.";
        const LIMIT_ERR: &str = "OMS desktop detail exceeds the response limit.";
        let scan_body = |body: &str| -> PanelResult<()> {
            oms_scrub::outbound_page(body, crate::MAX_ANSWER, oms_scrub::PageSelector::Offset(0))
                .map(|_| ())
                .map_err(|err| match err {
                    oms_scrub::PageError::OutboundWithheld => WITHHELD.to_owned(),
                    oms_scrub::PageError::InvalidRange => LIMIT_ERR.to_owned(),
                })
        };
        let Some(request) = args["participant"]
            .as_str()
            .and_then(|p| p.strip_prefix("ask-"))
            .filter(|r| uuid(r))
        else {
            return Ok(());
        };
        if !data["result"].is_null() {
            return Ok(());
        }
        let Ok(result) = self.assignment(
            repo,
            ASSIGN_RESULT,
            &json!({"room":args["room"],"requestId":request}),
            meta,
        ) else {
            return Ok(());
        };
        data["assignment"] = json!({"state":result["status"],"execution":result["execution"],"title":result["title"]});
        if result["status"] == "recovered" {
            data["assignment"]["task"] = result["task"].clone();
        }
        if result["execution"]["state"] == "done"
            && result["execution"]["exit"] == 0
            && let Some(op) = result["operation"].as_str()
            && let Some(path) = repo.to_str()
        {
            match self.peer_result(&json!({"repo":path,"operation":op})) {
                Ok(raw) => {
                    if let Some(proof) = raw.as_str().and_then(|v| crate::value::from_str(v).ok())
                        && proof["operation"] == op
                        && proof["exit"] == 0
                        && let Some([detail]) =
                            proof["answer_details"].as_array().map(Vec::as_slice)
                        && detail["exit"] == "0"
                    {
                        let full = if detail["read_arguments"]["operation"] == op
                            && let Some(reference) = detail["read_arguments"]["answer_ref"].as_str()
                        {
                            reference.rsplit_once(':').and_then(|(name, _)| {
                                let (text, exit) =
                                    crate::storage::owned_answer(repo, op, name).ok()?;
                                (exit == "0"
                                    && reference == crate::storage::reference(op, name, &text))
                                .then_some(text)
                            })
                        } else if detail["read_arguments"].is_null()
                            && let Some(artifact) = detail["artifact"].as_str()
                        {
                            Path::new(artifact)
                                .file_name()
                                .and_then(|s| s.to_str())
                                .and_then(|name| {
                                    let parent = repo.join(artifact).parent()?.to_path_buf();
                                    let bytes =
                                        crate::storage::read(&parent, name, crate::MAX_ANSWER)
                                            .ok()?;
                                    let (text, exit) = crate::storage::sections(&bytes);
                                    (exit == "0"
                                        && text.len() <= crate::MAX_ANSWER
                                        && detail["answer_sha256"].as_str()
                                            == Some(crate::storage::hash(&text).as_str()))
                                    .then_some(text)
                                })
                        } else {
                            None
                        };
                        if let Some(full) = full.filter(|s| !s.is_empty()) {
                            scan_body(&full)?;
                            let cut: String = full.chars().take(6000).collect();
                            data["result"] = json!({"kind":"read-only provider","attempt":result["operation"],"text":cut,"truncated":cut.len()<full.len(),"at":Value::Null});
                            return Ok(());
                        }
                    }
                }
                Err(crate::ToolError::Text(raw))
                    if lead_turns::confirmed_oversize_answer(repo, op, &raw) =>
                {
                    return Err(LIMIT_ERR.into());
                }
                Err(_) => {}
            }
        }
        let turns = result["transcript"]["turns"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if turns.is_empty() {
            return Ok(());
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
        scan_body(&text)?;
        let cut: String = text.chars().take(6000).collect();
        data["result"] = json!({"kind":"read-only provider","attempt":result["operation"],
          "text":cut,"truncated":cut.len() < text.len(),"at":Value::Null});
        Ok(())
    }

    pub(super) fn panel_snapshot(&self, repo: &Path, meta: &Value) -> PanelResult<Value> {
        let dashboard = self.panel_snapshot_projection(repo, meta)?;
        if dashboard.to_string().len() > crate::OUTPUT_LIMIT {
            return Err("OMS panel snapshot exceeds its output limit.".into());
        }
        self.write_through_project_card(repo, &dashboard);
        Ok(dashboard)
    }

    fn panel_snapshot_projection(&self, repo: &Path, meta: &Value) -> PanelResult<Value> {
        execute_panel_snapshot(
            self.executor.as_ref(),
            self.interrupted,
            repo,
            meta,
            Duration::from_secs(30),
        )
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

    fn configured_projects_for_cards(&self) -> PanelResult<BTreeMap<String, PathBuf>> {
        #[cfg(test)]
        if let Some(projects) = &self.project_cards.0.inner.projects_override {
            return Ok(projects.clone());
        }
        configured_projects()
    }

    fn card_cache_dir(&self) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(dir) = &self.project_cards.0.inner.cache_override {
            return Some(dir.clone());
        }
        default_card_cache_root()
    }

    fn stat_only_catalog_repo(&self, pin: &Path) -> Option<PathBuf> {
        if !pin.is_absolute()
            || pin.canonicalize().ok().as_deref() != Some(pin)
            || checkout(pin).ok() != Some(true)
            || (crate::delegated() && pin != self.caller_cwd)
            || !stat_only_env_and_root_ok(pin, crate::delegated())
        {
            return None;
        }
        Some(pin.to_owned())
    }

    fn stat_only_panel_repo(&self, args: &Value) -> PanelResult<PathBuf> {
        let map = self.configured_projects_for_cards()?;
        let pin = if map.is_empty() {
            if args.get("project").is_some() {
                return Err(BINDING.into());
            }
            std::env::var_os("OMS_PANEL_REPO").map(PathBuf::from)
        } else {
            let id = args["project"].as_str().ok_or(BINDING)?;
            Some(map.get(id).ok_or(BINDING)?.clone())
        };
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
        if (crate::delegated() && repo != self.caller_cwd)
            || !stat_only_env_and_root_ok(&repo, crate::delegated())
        {
            return Err(BINDING.into());
        }
        Ok(repo)
    }

    fn write_through_project_card(&self, repo: &Path, dashboard: &Value) {
        let Ok(projects) = self.configured_projects_for_cards() else {
            return;
        };
        if projects.is_empty() {
            return;
        }
        let matches_alias = projects
            .values()
            .any(|path| path == repo || path.canonicalize().ok().as_deref() == Some(repo));
        if !matches_alias {
            return;
        }
        let Some(cache_dir) = self.card_cache_dir() else {
            return;
        };
        let card = StoredProjectCard::from_dashboard(dashboard);
        let _ = write_cached_card(&cache_dir, repo, &card);
    }

    fn list_projects_with_cards(&self, args: &Value) -> PanelResult<Value> {
        let projects = self.configured_projects_for_cards()?;
        let binding = if !projects.is_empty() {
            "catalog"
        } else if std::env::var_os("OMS_PANEL_REPO").is_some() {
            self.stat_only_panel_repo(args)?;
            "pinned"
        } else if self.stat_only_panel_repo(args).is_ok() {
            "cwd"
        } else {
            "unbound"
        };
        let mut keys = json!({});
        let mut resolved_repos: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();
        for (alias_name, configured_path) in &projects {
            let valid_repo = self.stat_only_catalog_repo(configured_path);
            if let Some(repo) = &valid_repo
                && let Ok(key) = oms_command_plan::reviewed_context::project_key(repo)
            {
                keys[alias_name.as_str()] = json!(key);
            }
            resolved_repos.insert(alias_name.clone(), valid_repo);
        }
        let key = if matches!(binding, "pinned" | "cwd") {
            self.stat_only_panel_repo(args)
                .and_then(|repo| {
                    oms_command_plan::reviewed_context::project_key(&repo)
                        .map_err(|_| BINDING.to_owned())
                })
                .ok()
        } else {
            None
        };
        let alias_list = projects.keys().cloned().collect::<Vec<_>>();
        let mut out = json!({
            "projects": alias_list,
            "projectKeys": keys,
            "projectKey": key,
            "binding": binding,
            "readOnly": self.config.profile == ToolProfile::McpApp,
        });
        if binding != "catalog" {
            return Ok(out);
        }
        let cache_dir = self.card_cache_dir();
        let now = std::time::SystemTime::now();
        let base_bytes = out.to_string().len();
        let count = projects.len().max(1);
        let available_for_cards = crate::OUTPUT_LIMIT
            .saturating_sub(FRAME_RESERVE)
            .saturating_sub(base_bytes)
            .saturating_sub(256);
        let per_card_cap = (available_for_cards / count)
            .saturating_sub(2)
            .min(CARD_MAX_BYTES);
        let mut cards = Vec::with_capacity(projects.len());
        let mut to_refresh = Vec::new();
        for alias_name in projects.keys() {
            let Some(Some(repo)) = resolved_repos.get(alias_name) else {
                cards.push(render_project_card(
                    alias_name,
                    "unavailable",
                    None,
                    per_card_cap,
                ));
                continue;
            };
            match cache_dir
                .as_deref()
                .and_then(|dir| read_cached_card(dir, repo))
            {
                Some((stored, card_mtime)) => {
                    let stale = card_is_stale(repo, card_mtime, now);
                    let state = if stale { "stale" } else { "fresh" };
                    if stale {
                        to_refresh.push((alias_name.clone(), repo.clone(), cache_dir.clone()));
                    }
                    cards.push(render_project_card(
                        alias_name,
                        state,
                        Some(&stored),
                        per_card_cap,
                    ));
                }
                None => {
                    to_refresh.push((alias_name.clone(), repo.clone(), cache_dir.clone()));
                    cards.push(render_project_card(
                        alias_name,
                        "missing",
                        None,
                        per_card_cap,
                    ));
                }
            }
        }
        if !to_refresh.is_empty() {
            self.project_cards.enqueue_batch(to_refresh);
        }
        out["cards"] = Value::Array(cards);
        Ok(out)
    }
}

const CARD_MAX_BYTES: usize = 400;
const CARD_GOAL_MAX_BYTES: usize = 120;
const CARD_NEXT_MAX_BYTES: usize = 100;
const CARD_BRANCH_MAX_BYTES: usize = 80;
const CARD_ROLE_MAX_BYTES: usize = 24;
const CARD_CREW_MAX_ITEMS: usize = 3;
const CARD_CACHE_MAX_BYTES: usize = 4096;
const CARD_FRESH_WINDOW: Duration = Duration::from_secs(10 * 60);
const CARD_REFRESH_DEADLINE: Duration = Duration::from_secs(15);
const MAX_CONCURRENT_CARD_REFRESHES: usize = 2;
const CARD_REFRESH_BACKOFF: Duration = Duration::from_secs(5 * 60);

fn execute_panel_output(
    executor: &dyn crate::CommandExecutor,
    interrupted: &std::sync::atomic::AtomicBool,
    repo: &Path,
    args: Vec<OsString>,
    allow_partial: bool,
    timeout: Duration,
) -> PanelResult<(String, bool)> {
    let request = CommandRequest {
        args: &args,
        cwd: repo,
        timeout,
        interrupted,
    };
    let output = executor.execute(&request).map_err(
        |_| "OMS could not confirm the request outcome. Refresh project status before retrying.",
    )?;
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

fn execute_panel_snapshot(
    executor: &dyn crate::CommandExecutor,
    interrupted: &std::sync::atomic::AtomicBool,
    repo: &Path,
    meta: &Value,
    timeout: Duration,
) -> PanelResult<Value> {
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
    let (stdout, partial) = execute_panel_output(executor, interrupted, repo, argv, true, timeout)?;
    let report = crate::value::from_str(&stdout).map_err(|_| "Unreadable OMS response.")?;
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
    Ok(dashboard)
}

fn stat_only_env_and_root_ok(repo: &Path, is_delegated: bool) -> bool {
    let matches_env_dir = |var_name: &str| {
        std::env::var_os(var_name)
            .filter(|v| !v.is_empty())
            .is_none_or(|v| Path::new(&v).canonicalize().ok().as_deref() == Some(repo))
    };
    if !matches_env_dir("OMS_STATE_REPO")
        || !matches_env_dir(oms_reference::repository::EXECUTION_ENV)
    {
        return false;
    }
    if std::env::var_os("OMS_ROOM_ID").is_some_and(|v| !v.is_empty())
        && !matches_env_dir("OMS_ROOM_REPO")
    {
        return false;
    }
    if !is_delegated
        && let Some(parent) = oms_reference::repository::managed_scratch_parent(repo)
        && parent.join(".oh-my-setting-tmp").symlink_metadata().is_ok()
    {
        return false;
    }
    if !oms_safe_fs::has_git_marker(repo)
        && repo.ancestors().skip(1).any(oms_safe_fs::has_git_marker)
    {
        return false;
    }
    true
}

fn stat_only_checkout_ok(repo: &Path) -> bool {
    repo.is_absolute()
        && repo.canonicalize().ok().as_deref() == Some(repo)
        && checkout(repo).ok() == Some(true)
        && stat_only_env_and_root_ok(repo, false)
}

fn default_card_cache_root() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        let base = PathBuf::from(xdg);
        return base
            .is_absolute()
            .then(|| base.join("oh-my-setting/project-cards"));
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
    let base = PathBuf::from(home);
    base.is_absolute()
        .then(|| base.join(".cache/oh-my-setting/project-cards"))
}

fn card_cache_file_name(physical_repo: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    format!(
        "{}.json",
        crate::storage::hash(physical_repo.as_os_str().as_bytes())
    )
}

fn open_card_cache_dir(cache_dir: &Path, create: bool) -> Option<std::os::fd::OwnedFd> {
    use rustix::fs::{FileType, Mode, OFlags};
    use std::os::unix::fs::DirBuilderExt;

    if !cache_dir.is_absolute() {
        return None;
    }
    let parent = cache_dir.parent()?;
    if create {
        if !parent.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .ok()?;
        }
        if !parent.symlink_metadata().ok()?.file_type().is_dir() {
            return None;
        }
        match rustix::fs::mkdir(cache_dir, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => (),
            Err(_) => return None,
        }
    } else if !parent.symlink_metadata().ok()?.file_type().is_dir() {
        return None;
    }
    let fd = rustix::fs::open(
        cache_dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let st = rustix::fs::fstat(&fd).ok()?;
    if FileType::from_raw_mode(st.st_mode) != FileType::Directory
        || st.st_uid != rustix::process::geteuid().as_raw()
    {
        return None;
    }
    if st.st_mode & 0o777 != 0o700 {
        if create {
            rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o700)).ok()?;
            let updated = rustix::fs::fstat(&fd).ok()?;
            if updated.st_mode & 0o077 != 0 {
                return None;
            }
        } else if st.st_mode & 0o077 != 0 {
            return None;
        }
    }
    Some(fd)
}

fn clean_card_line(raw: &str, max_bytes: usize) -> String {
    let mut normalized = String::with_capacity(raw.len().min(max_bytes + 16));
    let mut prev_newline_cr = false;
    for ch in raw.chars() {
        if ch == '\r' {
            normalized.push(' ');
            prev_newline_cr = true;
            continue;
        }
        if ch == '\n' {
            if !prev_newline_cr {
                normalized.push(' ');
            }
            prev_newline_cr = false;
            continue;
        }
        prev_newline_cr = false;
        if ch == '\t' {
            normalized.push(' ');
        } else if !ch.is_control() {
            normalized.push(ch);
        }
    }
    let trimmed = normalized.trim();
    crate::storage::byte_prefix(trimmed, max_bytes)
        .trim_end()
        .to_owned()
}

fn valid_card_iso(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.chars().any(char::is_control)
        && (value.ends_with('Z') || value.ends_with("+00:00"))
        && value.as_bytes().get(10) == Some(&b'T')
        && chrono::DateTime::parse_from_rfc3339(value)
            .is_ok_and(|dt| dt.offset().local_minus_utc() == 0)
}

fn normalize_card_provider(raw: Option<&str>) -> Option<&'static str> {
    match raw {
        Some("codex") => Some("codex"),
        Some("claude") => Some("claude"),
        Some("antigravity" | "agy") => Some("antigravity"),
        _ => None,
    }
}

fn crew_id_from_attempt(attempt_id: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in attempt_id.as_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")[..12].to_owned()
}

fn valid_crew_id(value: &str) -> bool {
    value.len() == 12
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn compare_rfc3339_or_str(a: &str, b: &str) -> std::cmp::Ordering {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(da), Ok(db)) => da.cmp(&db).then_with(|| a.cmp(b)),
        _ => a.cmp(b),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredCrewMember {
    provider: Option<String>,
    role: Option<String>,
    id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredProjectCard {
    observed_at: Option<String>,
    branch: Option<String>,
    dirty: Option<bool>,
    goal: String,
    plan: Option<(u64, u64)>,
    attention: u64,
    next: String,
    active: u64,
    main: Option<String>,
    crew: Vec<StoredCrewMember>,
    crew_total: u64,
    degraded: bool,
    withheld: bool,
}

impl StoredProjectCard {
    fn from_dashboard(dashboard: &Value) -> Self {
        let observed_at = dashboard["updated"]
            .as_str()
            .filter(|s| valid_card_iso(s))
            .or_else(|| {
                dashboard["observation"]["observedAt"]
                    .as_str()
                    .filter(|s| valid_card_iso(s))
            })
            .map(str::to_owned);
        let raw_branch = dashboard["branch"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                dashboard["git"]["branch"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
            });
        let raw_goal = dashboard["goal"].as_str().unwrap_or("");
        let raw_next = if dashboard["nextStep"].is_object() {
            dashboard["nextStep"]["summary"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| dashboard["nextStep"]["id"].as_str())
                .unwrap_or("")
        } else {
            dashboard["nextStep"].as_str().unwrap_or("")
        };
        let active_attempts = dashboard["activeAttempts"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut latest_panel_main: Option<&Value> = None;
        let mut worker_rows: Vec<(&str, &Value)> = Vec::new();
        let mut role_withheld = false;
        for row in active_attempts {
            if !row.is_object() {
                continue;
            }
            if row["tool"].as_str() == Some("panel-main") {
                latest_panel_main = Some(match latest_panel_main {
                    None => row,
                    Some(prev) => {
                        let prev_ts = prev["updated"].as_str().filter(|s| !s.trim().is_empty());
                        let cur_ts = row["updated"].as_str().filter(|s| !s.trim().is_empty());
                        let newer = match (prev_ts, cur_ts) {
                            (None, Some(_)) => true,
                            (Some(p), Some(c)) => compare_rfc3339_or_str(c, p).is_gt(),
                            _ => false,
                        };
                        if newer { row } else { prev }
                    }
                });
                continue;
            }
            if let Some(raw_role) = row["role"].as_str()
                && !raw_role.is_empty()
                && crate::output::outbound(raw_role.as_bytes(), &Value::Null).is_err()
            {
                role_withheld = true;
            }
            if let Some(attempt_id) = row["id"].as_str().filter(|s| !s.trim().is_empty()) {
                worker_rows.push((attempt_id, row));
            }
        }
        let raw_main_provider = latest_panel_main
            .map(|row| row["provider"].as_str())
            .unwrap_or_else(|| dashboard["conversationMain"]["provider"].as_str());
        let source_withheld = role_withheld
            || [raw_branch.unwrap_or(""), raw_goal, raw_next]
                .into_iter()
                .any(|s| {
                    !s.is_empty() && crate::output::outbound(s.as_bytes(), &Value::Null).is_err()
                });
        let branch = if source_withheld {
            None
        } else {
            raw_branch
                .map(|s| clean_card_line(s, CARD_BRANCH_MAX_BYTES))
                .filter(|s| !s.is_empty())
        };
        let dirty = dashboard["git"]["dirty"].as_bool();
        let goal = if source_withheld {
            String::new()
        } else {
            clean_card_line(raw_goal, CARD_GOAL_MAX_BYTES)
        };
        let plan = if dashboard["planProgress"].is_object() {
            let total = dashboard["planProgress"]["total"].as_u64();
            let done = dashboard["planProgress"]["verified"]
                .as_u64()
                .or_else(|| dashboard["planProgress"]["done"].as_u64());
            match (done, total) {
                (Some(done), Some(total)) if done <= total => Some((done, total)),
                _ => None,
            }
        } else {
            None
        };
        let attention = dashboard["attention"]
            .as_array()
            .map_or(0, |items| items.len() as u64)
            .saturating_add(dashboard["attentionTrimmed"].as_u64().unwrap_or(0));
        let next = if source_withheld {
            String::new()
        } else {
            clean_card_line(raw_next, CARD_NEXT_MAX_BYTES)
        };
        let active = dashboard["active"].as_u64().unwrap_or_else(|| {
            dashboard["activeAttempts"]
                .as_array()
                .map_or(0, |items| items.len() as u64)
        });
        let main = if source_withheld {
            None
        } else {
            normalize_card_provider(raw_main_provider).map(str::to_owned)
        };
        worker_rows.sort_by(|(id_a, row_a), (id_b, row_b)| {
            let ca = row_a["created"].as_str().filter(|s| !s.trim().is_empty());
            let cb = row_b["created"].as_str().filter(|s| !s.trim().is_empty());
            let created_cmp = match (ca, cb) {
                (Some(a), Some(b)) => compare_rfc3339_or_str(a, b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            };
            created_cmp.then_with(|| id_a.cmp(id_b))
        });
        // The snapshot keeps only a window of active rows; count the omitted ones as workers too.
        let omitted = dashboard["active"]
            .as_u64()
            .map_or(0, |all| all.saturating_sub(active_attempts.len() as u64))
            .max(dashboard["activeAttemptsTrimmed"].as_u64().unwrap_or(0));
        let crew_total = if source_withheld {
            0
        } else {
            (worker_rows.len() as u64).saturating_add(omitted)
        };
        let crew = if source_withheld {
            Vec::new()
        } else {
            worker_rows
                .into_iter()
                .take(CARD_CREW_MAX_ITEMS)
                .map(|(attempt_id, row)| {
                    let provider =
                        normalize_card_provider(row["provider"].as_str()).map(str::to_owned);
                    let cleaned_role =
                        clean_card_line(row["role"].as_str().unwrap_or(""), CARD_ROLE_MAX_BYTES);
                    let role = (!cleaned_role.is_empty()).then_some(cleaned_role);
                    StoredCrewMember {
                        provider,
                        role,
                        id: crew_id_from_attempt(attempt_id),
                    }
                })
                .collect()
        };
        let degraded = dashboard["degraded"] == true;
        Self {
            observed_at,
            branch,
            dirty,
            goal,
            plan,
            attention,
            next,
            active,
            main,
            crew,
            crew_total,
            degraded,
            withheld: source_withheld,
        }
    }

    fn to_cache_bytes(&self) -> Vec<u8> {
        let plan = self
            .plan
            .map(|(done, total)| json!({"done": done, "total": total}))
            .unwrap_or(Value::Null);
        let crew = self
            .crew
            .iter()
            .map(|member| {
                json!({
                    "provider": member.provider,
                    "role": member.role,
                    "id": member.id,
                })
            })
            .collect::<Vec<_>>();
        let mut record = json!({
            "schema": 1,
            "observedAt": self.observed_at,
            "branch": self.branch,
            "dirty": self.dirty,
            "goal": self.goal,
            "plan": plan,
            "attention": self.attention,
            "next": self.next,
            "active": self.active,
            "main": self.main,
            "crew": crew,
            "crewTotal": self.crew_total,
            "degraded": self.degraded,
        });
        if self.withheld {
            record["withheld"] = json!(true);
        }
        record.to_string().into_bytes()
    }

    fn from_cache_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > CARD_CACHE_MAX_BYTES {
            return None;
        }
        oms_context::parse_json_bounded(bytes, CARD_CACHE_MAX_BYTES).ok()?;
        let v = crate::value::from_slice(bytes).ok()?;
        if !v.is_object() || v.get("schema").is_some_and(|s| *s != 1) {
            return None;
        }
        let mut raw_withheld = false;
        let check_outbound = |raw: &str, flag: &mut bool| {
            if !raw.is_empty() && crate::output::outbound(raw.as_bytes(), &Value::Null).is_err() {
                *flag = true;
            }
        };
        let observed_at = match v.get("observedAt") {
            None | Some(Value::Null) => None,
            Some(s) => Some(s.as_str().filter(|t| valid_card_iso(t))?.to_owned()),
        };
        let branch = match v.get("branch") {
            None | Some(Value::Null) => None,
            Some(s) => {
                let raw = s.as_str().filter(|t| !t.chars().any(char::is_control))?;
                check_outbound(raw, &mut raw_withheld);
                let cleaned = clean_card_line(raw, CARD_BRANCH_MAX_BYTES);
                (!cleaned.is_empty()).then_some(cleaned)
            }
        };
        let dirty = match v.get("dirty") {
            None | Some(Value::Null) => None,
            Some(b) => Some(b.as_bool()?),
        };
        let goal = match v.get("goal") {
            None | Some(Value::Null) => String::new(),
            Some(s) => {
                let raw = s.as_str().filter(|t| !t.chars().any(char::is_control))?;
                check_outbound(raw, &mut raw_withheld);
                clean_card_line(raw, CARD_GOAL_MAX_BYTES)
            }
        };
        let plan = match v.get("plan") {
            None | Some(Value::Null) => None,
            Some(p) => {
                let obj = p.as_object()?;
                if obj.keys().count() != 2 {
                    return None;
                }
                let done = p["done"].as_u64()?;
                let total = p["total"].as_u64()?;
                if done > total {
                    return None;
                }
                Some((done, total))
            }
        };
        let attention = v.get("attention")?.as_u64()?;
        let next = match v.get("next") {
            None | Some(Value::Null) => String::new(),
            Some(s) => {
                let raw = s.as_str().filter(|t| !t.chars().any(char::is_control))?;
                check_outbound(raw, &mut raw_withheld);
                clean_card_line(raw, CARD_NEXT_MAX_BYTES)
            }
        };
        let active = v.get("active")?.as_u64()?;
        let main = match v.get("main") {
            None | Some(Value::Null) => None,
            Some(s) => Some(normalize_card_provider(Some(s.as_str()?))?.to_owned()),
        };
        let crew = match v.get("crew") {
            None => Vec::new(),
            Some(arr) => {
                let items = arr.as_array()?;
                if items.len() > CARD_CREW_MAX_ITEMS {
                    return None;
                }
                let mut parsed = Vec::with_capacity(items.len());
                for item in items {
                    if !item.is_object() {
                        return None;
                    }
                    let provider = match item.get("provider") {
                        None | Some(Value::Null) => None,
                        Some(s) => Some(normalize_card_provider(Some(s.as_str()?))?.to_owned()),
                    };
                    let role = match item.get("role") {
                        None | Some(Value::Null) => None,
                        Some(s) => {
                            let raw = s.as_str().filter(|t| !t.chars().any(char::is_control))?;
                            check_outbound(raw, &mut raw_withheld);
                            let cleaned = clean_card_line(raw, CARD_ROLE_MAX_BYTES);
                            (!cleaned.is_empty()).then_some(cleaned)
                        }
                    };
                    let id = item
                        .get("id")?
                        .as_str()
                        .filter(|s| valid_crew_id(s))?
                        .to_owned();
                    parsed.push(StoredCrewMember { provider, role, id });
                }
                parsed
            }
        };
        let crew_total = match v.get("crewTotal") {
            None => crew.len() as u64,
            Some(n) => {
                let total = n.as_u64()?;
                if total < crew.len() as u64 {
                    return None;
                }
                total
            }
        };
        let degraded = v.get("degraded")?.as_bool()?;
        let withheld = match v.get("withheld") {
            None => raw_withheld,
            Some(flag) => flag.as_bool()? || raw_withheld,
        };
        Some(Self {
            observed_at,
            branch,
            dirty,
            goal,
            plan,
            attention,
            next,
            active,
            main,
            crew,
            crew_total,
            degraded,
            withheld,
        })
    }
}

fn write_cached_card(
    cache_dir: &Path,
    physical_repo: &Path,
    card: &StoredProjectCard,
) -> Result<(), ()> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    let dir_fd = open_card_cache_dir(cache_dir, true).ok_or(())?;
    let name = card_cache_file_name(physical_repo);
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let temp = format!(
        ".{name}.{}.{}.part",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let data = card.to_cache_bytes();
    let outcome = (|| -> Result<(), ()> {
        let fd = rustix::fs::openat(
            &dir_fd,
            &temp,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| ())?;
        rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o600)).map_err(|_| ())?;
        let mut file = std::fs::File::from(fd);
        file.write_all(&data).map_err(|_| ())?;
        file.sync_all().map_err(|_| ())?;
        rustix::fs::renameat(&dir_fd, &temp, &dir_fd, &name).map_err(|_| ())?;
        rustix::fs::fsync(&dir_fd).map_err(|_| ())?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = rustix::fs::unlinkat(&dir_fd, &temp, AtFlags::empty());
    }
    outcome
}

fn read_cached_card(
    cache_dir: &Path,
    physical_repo: &Path,
) -> Option<(StoredProjectCard, std::time::SystemTime)> {
    use rustix::fs::{FileType, Mode, OFlags};

    let dir_fd = open_card_cache_dir(cache_dir, false)?;
    let name = card_cache_file_name(physical_repo);
    let fd = rustix::fs::openat(
        &dir_fd,
        &name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let st = rustix::fs::fstat(&fd).ok()?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
        || st.st_nlink != 1
        || st.st_uid != rustix::process::geteuid().as_raw()
        || st.st_mode & 0o077 != 0
        || st.st_size <= 0
        || (st.st_size as usize) > CARD_CACHE_MAX_BYTES
    {
        return None;
    }
    let mut file = std::fs::File::from(fd);
    let mtime = file.metadata().ok()?.modified().ok()?;
    let mut buf = Vec::with_capacity(st.st_size as usize);
    (&mut file)
        .take((CARD_CACHE_MAX_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() > CARD_CACHE_MAX_BYTES {
        return None;
    }
    let card = StoredProjectCard::from_cache_bytes(&buf)?;
    Some((card, mtime))
}

fn card_is_stale(
    physical_repo: &Path,
    card_mtime: std::time::SystemTime,
    now: std::time::SystemTime,
) -> bool {
    if now
        .duration_since(card_mtime)
        .is_ok_and(|age| age > CARD_FRESH_WINDOW)
    {
        return true;
    }
    for marker in [
        ".git/HEAD",
        ".git/index",
        ".oms/plan",
        ".oms/plan/tasks.json",
    ] {
        if physical_repo
            .join(marker)
            .symlink_metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified > card_mtime)
        {
            return true;
        }
    }
    false
}

fn render_project_card(
    alias_name: &str,
    state: &str,
    stored: Option<&StoredProjectCard>,
    max_bytes: usize,
) -> Value {
    let withheld = || json!({"alias": alias_name, "state": "withheld"});
    if stored.is_some_and(|c| c.withheld) {
        return withheld();
    }
    let cap = max_bytes.min(CARD_MAX_BYTES);
    let mut observed_at = stored.and_then(|c| c.observed_at.clone());
    let mut branch = stored.and_then(|c| c.branch.clone());
    let dirty = stored.and_then(|c| c.dirty);
    let mut goal = stored
        .map(|c| clean_card_line(&c.goal, CARD_GOAL_MAX_BYTES))
        .unwrap_or_default();
    let mut plan = stored.and_then(|c| c.plan);
    let attention = stored.map_or(0, |c| c.attention);
    let mut next = stored
        .map(|c| clean_card_line(&c.next, CARD_NEXT_MAX_BYTES))
        .unwrap_or_default();
    let active = stored.map_or(0, |c| c.active);
    let main = stored.and_then(|c| c.main.clone());
    let mut crew: Vec<StoredCrewMember> = stored
        .map(|c| {
            c.crew
                .iter()
                .take(CARD_CREW_MAX_ITEMS)
                .map(|m| StoredCrewMember {
                    provider: m.provider.clone(),
                    role: m
                        .role
                        .as_deref()
                        .map(|r| clean_card_line(r, CARD_ROLE_MAX_BYTES))
                        .filter(|r| !r.is_empty()),
                    id: m.id.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let crew_total = stored.map_or(0, |c| c.crew_total);
    let degraded = stored.is_some_and(|c| c.degraded);

    let assemble = |observed_at: &Option<String>,
                    branch: &Option<String>,
                    goal: &str,
                    plan: Option<(u64, u64)>,
                    next: &str,
                    crew: &[StoredCrewMember]| {
        let plan_val = plan
            .map(|(done, total)| json!({"done": done, "total": total}))
            .unwrap_or(Value::Null);
        let crew_val = crew
            .iter()
            .map(|member| {
                json!({
                    "provider": member.provider,
                    "role": member.role,
                    "id": member.id,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "alias": alias_name,
            "state": state,
            "observedAt": observed_at,
            "branch": branch,
            "dirty": dirty,
            "goal": goal,
            "plan": plan_val,
            "attention": attention,
            "next": next,
            "active": active,
            "main": main,
            "crew": crew_val,
            "crewTotal": crew_total,
            "degraded": degraded,
        })
    };

    let initial = assemble(&observed_at, &branch, &goal, plan, &next, &crew);
    let initial_bytes = initial.to_string();
    if crate::output::outbound(initial_bytes.as_bytes(), &Value::Null).is_err() {
        return withheld();
    }
    if initial_bytes.len() <= cap {
        return initial;
    }

    loop {
        let branch_len = branch.as_ref().map_or(0, String::len);
        if goal.len() >= next.len() && goal.len() >= branch_len && !goal.is_empty() {
            goal.pop();
        } else if next.len() >= branch_len && !next.is_empty() {
            next.pop();
        } else if let Some(b) = &mut branch {
            b.pop();
            if b.is_empty() {
                branch = None;
            }
        } else if !crew.is_empty() {
            crew.pop();
        } else if plan.is_some() {
            plan = None;
        } else if observed_at.is_some() {
            observed_at = None;
        } else {
            break;
        }
        let candidate = assemble(&observed_at, &branch, &goal, plan, &next, &crew);
        let encoded = candidate.to_string();
        if encoded.len() <= cap {
            if crate::output::outbound(encoded.as_bytes(), &Value::Null).is_err() {
                return withheld();
            }
            return candidate;
        }
    }
    // Long aliases can leave no room even for an empty card; keep the response valid instead.
    let minimal = json!({"alias": alias_name, "state": state, "trimmed": true});
    if minimal.to_string().len() <= cap
        && crate::output::outbound(minimal.to_string().as_bytes(), &Value::Null).is_ok()
    {
        return minimal;
    }
    withheld()
}

#[derive(Clone)]
pub(crate) struct ProjectCardRefresher(std::sync::Arc<CardRefresherOwner>);

struct CardRefresherOwner {
    inner: std::sync::Arc<CardRefresherInner>,
}

struct CardRefresherInner {
    executor: std::sync::Arc<dyn crate::CommandExecutor>,
    stopped: std::sync::atomic::AtomicBool,
    deadline: Duration,
    #[cfg(test)]
    cache_override: Option<PathBuf>,
    #[cfg(test)]
    projects_override: Option<BTreeMap<String, PathBuf>>,
    state: std::sync::Mutex<CardRefresherState>,
}

#[derive(Default)]
struct CardRefresherState {
    pending: std::collections::VecDeque<(String, PathBuf, Option<PathBuf>)>,
    in_flight: std::collections::BTreeSet<String>,
    failed: BTreeMap<String, std::time::Instant>,
    slot_busy: [bool; MAX_CONCURRENT_CARD_REFRESHES],
    slots: [Option<std::thread::JoinHandle<()>>; MAX_CONCURRENT_CARD_REFRESHES],
}

impl Drop for CardRefresherOwner {
    fn drop(&mut self) {
        self.inner
            .stopped
            .store(true, std::sync::atomic::Ordering::Release);
        let handles = {
            let mut state = self.inner.state.lock().expect("card refresher lock");
            state.pending.clear();
            std::mem::take(&mut state.slots)
        };
        for handle in handles.into_iter().flatten() {
            let _ = handle.join();
        }
    }
}

impl ProjectCardRefresher {
    pub(crate) fn new(executor: std::sync::Arc<dyn crate::CommandExecutor>) -> Self {
        Self(std::sync::Arc::new(CardRefresherOwner {
            inner: std::sync::Arc::new(CardRefresherInner {
                executor,
                stopped: std::sync::atomic::AtomicBool::new(false),
                deadline: CARD_REFRESH_DEADLINE,
                #[cfg(test)]
                cache_override: None,
                #[cfg(test)]
                projects_override: None,
                state: std::sync::Mutex::new(CardRefresherState::default()),
            }),
        }))
    }

    #[cfg(test)]
    fn with_test_overrides(
        executor: std::sync::Arc<dyn crate::CommandExecutor>,
        deadline: Duration,
        cache_override: PathBuf,
        projects_override: BTreeMap<String, PathBuf>,
    ) -> Self {
        Self(std::sync::Arc::new(CardRefresherOwner {
            inner: std::sync::Arc::new(CardRefresherInner {
                executor,
                stopped: std::sync::atomic::AtomicBool::new(false),
                deadline,
                cache_override: Some(cache_override),
                projects_override: Some(projects_override),
                state: std::sync::Mutex::new(CardRefresherState::default()),
            }),
        }))
    }

    #[cfg(test)]
    fn is_idle(&self) -> bool {
        let state = self.0.inner.state.lock().expect("card refresher lock");
        state.pending.is_empty()
            && state.in_flight.is_empty()
            && state.slot_busy.iter().all(|busy| !*busy)
    }

    fn enqueue_batch(&self, items: Vec<(String, PathBuf, Option<PathBuf>)>) {
        use std::sync::atomic::Ordering;

        if self.0.inner.stopped.load(Ordering::Acquire) {
            return;
        }
        let mut stale_handles = Vec::new();
        {
            let mut state = self.0.inner.state.lock().expect("card refresher lock");
            if self.0.inner.stopped.load(Ordering::Acquire) {
                return;
            }
            for (alias_name, repo, cache_dir) in items {
                if state.in_flight.contains(&alias_name)
                    || state
                        .failed
                        .get(&alias_name)
                        .is_some_and(|at| at.elapsed() < CARD_REFRESH_BACKOFF)
                {
                    continue;
                }
                if let Some(existing) = state
                    .pending
                    .iter_mut()
                    .find(|(queued, _, _)| queued == &alias_name)
                {
                    existing.1 = repo;
                    existing.2 = cache_dir;
                } else if state.pending.len() < 128 {
                    state.pending.push_back((alias_name, repo, cache_dir));
                }
            }
            while !state.pending.is_empty() {
                let Some(slot) = state.slot_busy.iter().position(|busy| !*busy) else {
                    break;
                };
                let Some(first) = state.pending.pop_front() else {
                    break;
                };
                state.in_flight.insert(first.0.clone());
                state.slot_busy[slot] = true;
                if let Some(prev) = state.slots[slot].take() {
                    stale_handles.push(prev);
                }
                let inner = std::sync::Arc::clone(&self.0.inner);
                state.slots[slot] = Some(std::thread::spawn(move || {
                    run_card_refresh_worker(inner, slot, first);
                }));
            }
        }
        for handle in stale_handles {
            let _ = handle.join();
        }
    }
}

fn run_card_refresh_worker(
    inner: std::sync::Arc<CardRefresherInner>,
    slot: usize,
    mut current: (String, PathBuf, Option<PathBuf>),
) {
    use std::sync::atomic::Ordering;

    loop {
        let (alias_name, repo, cache_dir) = current;
        let mut refreshed = false;
        if !inner.stopped.load(Ordering::Acquire)
            && let Some(cache_dir) = cache_dir.as_deref()
            && stat_only_checkout_ok(&repo)
            && let Ok(dashboard) = execute_panel_snapshot(
                inner.executor.as_ref(),
                &inner.stopped,
                &repo,
                &Value::Null,
                inner.deadline,
            )
            && dashboard.to_string().len() <= crate::OUTPUT_LIMIT
            && !inner.stopped.load(Ordering::Acquire)
            && stat_only_checkout_ok(&repo)
        {
            let card = StoredProjectCard::from_dashboard(&dashboard);
            refreshed = write_cached_card(cache_dir, &repo, &card).is_ok();
        }
        let mut state = inner.state.lock().expect("card refresher lock");
        state.in_flight.remove(&alias_name);
        // A failing project waits out a backoff instead of rerunning on every poll.
        if refreshed {
            state.failed.remove(&alias_name);
        } else {
            state
                .failed
                .insert(alias_name.clone(), std::time::Instant::now());
        }
        if inner.stopped.load(Ordering::Acquire) {
            state.pending.clear();
            state.slot_busy[slot] = false;
            return;
        }
        match state.pending.pop_front() {
            Some(next) => {
                state.in_flight.insert(next.0.clone());
                current = next;
            }
            None => {
                state.slot_busy[slot] = false;
                return;
            }
        }
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
        let mut selection = task("selected", "ready", false);
        selection["planSelection"] = json!({"source":"canonical-plan","readOnly":true,"planId":format!("plan_{}","a".repeat(32)),"taskId":"selected","taskSha256":"b".repeat(64),"provider":"claude"});
        let selection_snapshot =
            json!({"schema":1,"omitted":0,"availability":"ready","items":[selection]});
        assert!(valid_work_items_receipt(&selection_snapshot));
        for field in ["taskId", "readOnly", "source", "taskSha256"] {
            let mut changed = selection_snapshot.clone();
            let Value::Array(items) = &mut changed["items"] else {
                unreachable!();
            };
            items[0]["planSelection"][field] = json!("invalid");
            assert!(!valid_work_items_receipt(&changed));
        }
        let inconsistent = change_item(3, |item| {
            item["issueCache"].as_object_mut().unwrap().remove("reason");
        });
        assert!(!valid_work_items_receipt(&inconsistent));
        let inconsistent = change_item(3, |item| {
            item["prReviewCandidate"]["disposition"] = json!("superseded");
        });
        assert!(!valid_work_items_receipt(&inconsistent));
    }

    struct PanelCardTemp(PathBuf);
    impl PanelCardTemp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "oms-panel-card-unit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }
        fn checkout(&self, name: &str) -> PathBuf {
            let repo = self.0.join(name);
            std::fs::create_dir_all(repo.join(".oms/plan")).unwrap();
            std::fs::create_dir_all(repo.join(".git")).unwrap();
            std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(repo.join(".git/index"), "idx\n").unwrap();
            std::fs::write(repo.join(".oms/plan/tasks.json"), "{}\n").unwrap();
            repo
        }
    }
    impl Drop for PanelCardTemp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn set_mtime(path: &Path, when: std::time::SystemTime) {
        let since_epoch = when
            .duration_since(std::time::UNIX_EPOCH)
            .expect("post-epoch time");
        let ts = rustix::fs::Timespec {
            tv_sec: since_epoch.as_secs() as i64,
            tv_nsec: since_epoch.subsec_nanos() as _,
        };
        let stamps = rustix::fs::Timestamps {
            last_access: ts,
            last_modification: ts,
        };
        rustix::fs::utimensat(
            rustix::fs::CWD,
            path,
            &stamps,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();
    }

    #[test]
    fn project_card_counts_omitted_workers_and_fits_long_aliases() {
        let rows: Vec<Value> = (0..8)
            .map(|i| json!({"id": format!("att-{i}"), "tool": "peer-delegate", "provider": "codex", "created": format!("2026-11-27T05:5{i}:00Z")}))
            .collect();
        let dashboard = json!({"active": 12, "activeAttempts": rows});
        let card = StoredProjectCard::from_dashboard(&dashboard);
        assert_eq!(card.crew.len(), CARD_CREW_MAX_ITEMS);
        assert_eq!(card.crew_total, 12);
        let trimmed = json!({"activeAttempts": [], "activeAttemptsTrimmed": 4});
        assert_eq!(StoredProjectCard::from_dashboard(&trimmed).crew_total, 4);

        let long_alias = format!("a{}", "b".repeat(79));
        let rendered = render_project_card(&long_alias, "fresh", Some(&card), 150);
        assert!(rendered.to_string().len() <= 150, "{rendered}");
        assert_eq!(rendered["alias"], long_alias.as_str());
        assert_eq!(rendered["trimmed"], true);
    }

    #[test]
    fn project_card_derivation_char_truncation_cache_safety_and_withhold() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = PanelCardTemp::new();
        let cache_dir = tmp.0.join("cache/oh-my-setting/project-cards");
        let alpha = tmp.checkout("alpha");
        let beta = tmp.checkout("beta");

        let multibyte_goal = "패널홈요약".repeat(25);
        let multibyte_next = "검증단계확인".repeat(20);
        let multibyte_role = "코드리뷰담당워커역할이름길게테스트";
        let dashboard = json!({
            "updated": "2026-11-27T06:00:00Z",
            "branch": "feat/project-home",
            "git": {"available": true, "branch": "feat/project-home", "dirty": true},
            "goal": multibyte_goal,
            "planProgress": {"verified": 3, "total": 7},
            "attention": [{"code": "a"}, {"code": "b"}],
            "attentionTrimmed": 1,
            "nextStep": {"id": "verify", "summary": multibyte_next, "scope": "repository"},
            "active": 5,
            "conversationMain": {"provider": "codex"},
            "activeAttempts": [
                {"id": "main-older", "tool": "panel-main", "provider": "codex", "updated": "2026-11-27T05:55:00Z"},
                {"id": "main-newer", "tool": "panel-main", "provider": "agy", "updated": "2026-11-27T05:59:00Z"},
                {"id": "", "tool": "peer-delegate", "provider": "claude", "role": "ignored", "created": "2026-11-27T05:50:00Z"},
                {"id": "att-late", "tool": "peer-delegate", "provider": "claude", "role": "late-worker", "created": "2026-11-27T05:58:00Z"},
                {"id": "att-tie-b", "tool": "peer-delegate", "provider": "agy", "role": multibyte_role, "created": "2026-11-27T05:56:00Z"},
                {"id": "att-early", "tool": "peer-delegate", "provider": "codex", "role": "  architect\n\t ", "created": "2026-11-27T05:54:00Z"},
                {"id": "att-tie-a", "tool": "agent-supervisor", "provider": "pi", "role": "   ", "created": "2026-11-27T05:56:00Z"},
            ],
            "degraded": false,
        });
        let stored = StoredProjectCard::from_dashboard(&dashboard);
        let stored_again = StoredProjectCard::from_dashboard(&dashboard);
        assert_eq!(stored, stored_again);
        assert_eq!(stored.observed_at.as_deref(), Some("2026-11-27T06:00:00Z"));
        assert_eq!(stored.branch.as_deref(), Some("feat/project-home"));
        assert_eq!(stored.dirty, Some(true));
        assert!(stored.goal.len() <= CARD_GOAL_MAX_BYTES);
        assert!(stored.next.len() <= CARD_NEXT_MAX_BYTES);
        assert_eq!(stored.plan, Some((3, 7)));
        assert_eq!(stored.attention, 3);
        assert_eq!(stored.active, 5);
        assert_eq!(stored.main.as_deref(), Some("antigravity"));
        assert_eq!(stored.crew_total, 4);
        assert_eq!(stored.crew.len(), 3);
        assert_eq!(stored.crew[0].provider.as_deref(), Some("codex"));
        assert_eq!(stored.crew[0].role.as_deref(), Some("architect"));
        assert_eq!(stored.crew[1].provider, None);
        assert_eq!(stored.crew[1].role, None);
        assert_eq!(stored.crew[2].provider.as_deref(), Some("antigravity"));
        assert!(
            stored.crew[2]
                .role
                .as_deref()
                .is_some_and(|r| !r.is_empty() && r.len() <= CARD_ROLE_MAX_BYTES)
        );
        for (member, raw_id) in stored
            .crew
            .iter()
            .zip(["att-early", "att-tie-a", "att-tie-b"])
        {
            assert!(valid_crew_id(&member.id), "invalid crew id {}", member.id);
            assert_ne!(member.id, raw_id);
            assert_eq!(member.id, crew_id_from_attempt(raw_id));
        }
        // Standard FNV-1a 64-bit check vector ("foobar" -> 0x85944171f73967e8).
        assert_eq!(crew_id_from_attempt("foobar"), "85944171f739");
        assert!(!stored.degraded);

        // Fallback to conversationMain when no panel-main row exists, plus agy and unknown providers.
        let conv_fallback = StoredProjectCard::from_dashboard(&json!({
            "conversationMain": {"provider": "claude"},
            "activeAttempts": [{"id": "w1", "tool": "peer-delegate", "provider": "codex"}],
            "degraded": false,
        }));
        assert_eq!(conv_fallback.main.as_deref(), Some("claude"));
        let conv_agy = StoredProjectCard::from_dashboard(&json!({
            "conversationMain": {"provider": "agy"},
            "degraded": false,
        }));
        assert_eq!(conv_agy.main.as_deref(), Some("antigravity"));
        let unknown_panel_main = StoredProjectCard::from_dashboard(&json!({
            "conversationMain": {"provider": "codex"},
            "activeAttempts": [{"id": "m1", "tool": "panel-main", "provider": "pi", "updated": "2026-11-27T06:00:00Z"}],
            "degraded": false,
        }));
        assert_eq!(unknown_panel_main.main, None);
        let unknown_conv_main = StoredProjectCard::from_dashboard(&json!({
            "conversationMain": {"provider": "cursor"},
            "degraded": false,
        }));
        assert_eq!(unknown_conv_main.main, None);

        // Missing/unavailable cards still carry main: null, crew: [], crewTotal: 0 within cap.
        let missing_rendered = render_project_card("empty", "missing", None, CARD_MAX_BYTES);
        assert_eq!(missing_rendered["main"], Value::Null);
        assert_eq!(missing_rendered["crew"], json!([]));
        assert_eq!(missing_rendered["crewTotal"], 0);

        // Shortening order: goal/next/branch shrink before crew; crew drops from the end only when
        // still over cap, while crewTotal retains full count and main is never dropped.
        let ascii_crew_stored = StoredProjectCard::from_dashboard(&json!({
            "updated": "2026-11-27T06:00:00Z",
            "branch": "feature/very-long-branch-name-for-budget-test",
            "git": {"available": true, "branch": "feature/very-long-branch-name-for-budget-test", "dirty": false},
            "goal": "g".repeat(200),
            "planProgress": {"verified": 1, "total": 3},
            "nextStep": {"id": "n", "summary": "n".repeat(160), "scope": "repository"},
            "active": 4,
            "activeAttempts": [
                {"id": "m1", "tool": "panel-main", "provider": "claude", "updated": "2026-11-27T06:00:00Z"},
                {"id": "c1", "tool": "peer-delegate", "provider": "codex", "role": "impl", "created": "2026-11-27T05:50:00Z"},
                {"id": "c2", "tool": "peer-delegate", "provider": "claude", "role": "review", "created": "2026-11-27T05:51:00Z"},
                {"id": "c3", "tool": "peer-delegate", "provider": "agy", "role": "verify", "created": "2026-11-27T05:52:00Z"}
            ],
            "degraded": false,
        }));
        let fit_with_all_crew =
            render_project_card("p0", "fresh", Some(&ascii_crew_stored), CARD_MAX_BYTES);
        assert!(fit_with_all_crew.to_string().len() <= CARD_MAX_BYTES);
        assert_eq!(fit_with_all_crew["crew"].as_array().unwrap().len(), 3);
        assert_eq!(fit_with_all_crew["crewTotal"], 3);
        assert_eq!(fit_with_all_crew["main"], "claude");

        let drop_one_crew = render_project_card("p0", "fresh", Some(&ascii_crew_stored), 350);
        assert!(drop_one_crew.to_string().len() <= 350);
        assert_eq!(drop_one_crew["goal"], "");
        assert_eq!(drop_one_crew["next"], "");
        assert_eq!(drop_one_crew["branch"], Value::Null);
        assert_eq!(drop_one_crew["crew"].as_array().unwrap().len(), 2);
        assert_eq!(drop_one_crew["crew"][0]["id"], ascii_crew_stored.crew[0].id);
        assert_eq!(drop_one_crew["crew"][1]["id"], ascii_crew_stored.crew[1].id);
        assert_eq!(drop_one_crew["crewTotal"], 3);
        assert_eq!(drop_one_crew["main"], "claude");

        let short_alias_rendered =
            render_project_card("p0", "fresh", Some(&stored), CARD_MAX_BYTES);
        assert!(short_alias_rendered.to_string().len() <= CARD_MAX_BYTES);
        assert_eq!(short_alias_rendered["crew"].as_array().unwrap().len(), 2);
        assert_eq!(short_alias_rendered["crewTotal"], 4);
        assert_eq!(short_alias_rendered["main"], "antigravity");

        let long_alias = "a".repeat(80);
        let rendered = render_project_card(&long_alias, "fresh", Some(&stored), CARD_MAX_BYTES);
        let wire = rendered.to_string();
        assert!(
            wire.len() <= CARD_MAX_BYTES,
            "wire len {}: {wire}",
            wire.len()
        );
        assert_eq!(rendered["alias"], long_alias);
        assert_eq!(rendered["state"], "fresh");
        assert_eq!(rendered["plan"], json!({"done": 3, "total": 7}));
        assert_eq!(rendered["attention"], 3);
        assert_eq!(rendered["active"], 5);
        assert_eq!(rendered["main"], "antigravity");
        assert_eq!(rendered["crewTotal"], 4);
        let trimmed_crew = rendered["crew"].as_array().unwrap();
        assert!(!trimmed_crew.is_empty() && trimmed_crew.len() < 3);
        assert_eq!(trimmed_crew[0]["id"], stored.crew[0].id);
        assert_eq!(rendered["dirty"], true);
        assert_eq!(rendered["degraded"], false);
        assert!(rendered["goal"].as_str().unwrap().len() <= CARD_GOAL_MAX_BYTES);
        assert!(rendered["next"].as_str().unwrap().len() <= CARD_NEXT_MAX_BYTES);

        // Cache mode 0700 / 0600, no-follow, size cap, and nothing written in checkout.
        let before_alpha: Vec<_> = std::fs::read_dir(&alpha)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        write_cached_card(&cache_dir, &alpha, &stored).unwrap();
        let dir_mode = std::fs::symlink_metadata(&cache_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let file_path = cache_dir.join(card_cache_file_name(&alpha));
        let file_mode = std::fs::symlink_metadata(&file_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        let after_alpha: Vec<_> = std::fs::read_dir(&alpha)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(before_alpha, after_alpha);
        let (loaded, _) = read_cached_card(&cache_dir, &alpha).unwrap();
        assert_eq!(loaded, stored);

        // Old cache record without main, crew, or crewTotal loads with defaults.
        let legacy_record = json!({
            "schema": 1,
            "observedAt": "2026-11-27T06:00:00Z",
            "branch": "main",
            "dirty": false,
            "goal": "Legacy cached goal",
            "plan": {"done": 1, "total": 2},
            "attention": 0,
            "next": "",
            "active": 1,
            "degraded": false,
        });
        let legacy_loaded =
            StoredProjectCard::from_cache_bytes(legacy_record.to_string().as_bytes()).unwrap();
        assert_eq!(legacy_loaded.main, None);
        assert!(legacy_loaded.crew.is_empty());
        assert_eq!(legacy_loaded.crew_total, 0);
        let legacy_rendered =
            render_project_card("legacy", "fresh", Some(&legacy_loaded), CARD_MAX_BYTES);
        assert_eq!(legacy_rendered["main"], Value::Null);
        assert_eq!(legacy_rendered["crew"], json!([]));
        assert_eq!(legacy_rendered["crewTotal"], 0);

        // Symlinked or oversized cache files are rejected fail-closed.
        let beta_file = cache_dir.join(card_cache_file_name(&beta));
        std::os::unix::fs::symlink(&file_path, &beta_file).unwrap();
        assert!(read_cached_card(&cache_dir, &beta).is_none());
        std::fs::remove_file(&beta_file).unwrap();
        std::fs::write(&beta_file, vec![b' '; CARD_CACHE_MAX_BYTES + 1]).unwrap();
        std::fs::set_permissions(&beta_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_cached_card(&cache_dir, &beta).is_none());
        std::fs::remove_file(&beta_file).unwrap();

        // Outbound scan per card: tripping text in goal or worker role withholds only that card
        // without main, crew, or crewTotal.
        let tripped = ["gh", "p_", &"z".repeat(36)].concat();
        let bad_dashboard = json!({
            "updated": "2026-11-27T06:00:00Z",
            "goal": format!("Inspect {tripped}"),
            "conversationMain": {"provider": "codex"},
            "activeAttempts": [{"id": "w1", "tool": "peer-delegate", "provider": "claude", "role": "reviewer"}],
            "degraded": false,
        });
        let bad_stored = StoredProjectCard::from_dashboard(&bad_dashboard);
        assert!(
            !String::from_utf8_lossy(&bad_stored.to_cache_bytes()).contains(&tripped),
            "cache bytes must not retain outbound-tripping text"
        );
        assert_eq!(
            render_project_card("bad", "fresh", Some(&bad_stored), CARD_MAX_BYTES),
            json!({"alias": "bad", "state": "withheld"})
        );
        let bad_role_dashboard = json!({
            "updated": "2026-11-27T06:00:00Z",
            "goal": "Clean goal",
            "conversationMain": {"provider": "claude"},
            "activeAttempts": [
                {"id": "w-secret", "tool": "peer-delegate", "provider": "codex", "role": format!("role-{tripped}")}
            ],
            "degraded": false,
        });
        let bad_role_stored = StoredProjectCard::from_dashboard(&bad_role_dashboard);
        assert!(
            !String::from_utf8_lossy(&bad_role_stored.to_cache_bytes()).contains(&tripped[..16]),
            "cache bytes must not retain truncated secret role prefix"
        );
        assert_eq!(
            render_project_card("bad", "fresh", Some(&bad_role_stored), CARD_MAX_BYTES),
            json!({"alias": "bad", "state": "withheld"})
        );
        // Also verify read-time per-card scan when raw cached JSON contains tripping text.
        let raw_bad = json!({
            "schema": 1,
            "observedAt": "2026-11-27T06:00:00Z",
            "branch": "main",
            "dirty": false,
            "goal": tripped,
            "plan": null,
            "attention": 0,
            "next": "",
            "active": 0,
            "degraded": false,
        });
        let parsed_raw_bad =
            StoredProjectCard::from_cache_bytes(raw_bad.to_string().as_bytes()).unwrap();
        assert_eq!(
            render_project_card("bad", "fresh", Some(&parsed_raw_bad), CARD_MAX_BYTES),
            json!({"alias": "bad", "state": "withheld"})
        );
    }

    #[test]
    fn stat_freshness_concurrency_deadline_and_failed_refresh_retention() {
        use std::sync::{
            Arc, Condvar, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };

        struct GateExecutor {
            request_thread: std::thread::ThreadId,
            active: AtomicUsize,
            max_active: AtomicUsize,
            calls: AtomicUsize,
            fail_mode: AtomicBool,
            hold: Mutex<bool>,
            cv: Condvar,
            seen_deadlines: Mutex<Vec<Duration>>,
        }
        impl crate::CommandExecutor for GateExecutor {
            fn execute(&self, request: &CommandRequest<'_>) -> Result<crate::Captured, String> {
                assert_ne!(
                    std::thread::current().id(),
                    self.request_thread,
                    "snapshot refresh must never run on the request thread"
                );
                self.seen_deadlines.lock().unwrap().push(request.timeout);
                let now_active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
                self.max_active.fetch_max(now_active, Ordering::AcqRel);
                self.calls.fetch_add(1, Ordering::AcqRel);
                {
                    let mut held = self.hold.lock().unwrap();
                    while *held && !request.interrupted.load(Ordering::Acquire) {
                        held = self.cv.wait(held).unwrap();
                    }
                }
                self.active.fetch_sub(1, Ordering::AcqRel);
                if self.fail_mode.load(Ordering::Acquire) {
                    let leaked = ["gh", "p_", &"q".repeat(36)].concat();
                    return Ok(crate::Captured::from_output(2, b"", leaked.as_bytes()));
                }
                let snap = serde_json::json!({
                    "schema": 1,
                    "kind": "oms-panel-snapshot",
                    "dashboard": {
                        "scope": "repository",
                        "updated": "2026-11-27T06:05:00Z",
                        "branch": "main",
                        "git": {"available": true, "branch": "main", "dirty": false},
                        "goal": "Refreshed in background",
                        "planProgress": {"verified": 1, "total": 2},
                        "attention": [],
                        "nextStep": null,
                        "active": 0,
                        "degraded": false
                    }
                });
                Ok(crate::Captured::from_output(
                    0,
                    snap.to_string().as_bytes(),
                    b"",
                ))
            }
        }

        let tmp = PanelCardTemp::new();
        let source = tmp.0.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("VERSION"), "test\n").unwrap();
        let cache_dir = tmp.0.join("cache/oh-my-setting/project-cards");

        let mut projects = BTreeMap::new();
        for idx in 0..4 {
            projects.insert(format!("p{idx}"), tmp.checkout(&format!("p{idx}")));
        }
        projects.insert("gone".into(), tmp.0.join("nonexistent"));

        let exec = Arc::new(GateExecutor {
            request_thread: std::thread::current().id(),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            fail_mode: AtomicBool::new(false),
            hold: Mutex::new(true),
            cv: Condvar::new(),
            seen_deadlines: Mutex::new(Vec::new()),
        });
        let interrupted = AtomicBool::new(false);
        let cfg = crate::Config {
            profile: ToolProfile::McpApp,
            tasks_extension: false,
            peer_owner: source.join("owner"),
            oms_program: None,
        };
        let mut server = Server::with_executor(
            &source,
            &tmp.0,
            &interrupted,
            cfg,
            Arc::clone(&exec) as Arc<dyn crate::CommandExecutor>,
        )
        .unwrap();
        let custom_deadline = Duration::from_secs(9);
        server.project_cards = ProjectCardRefresher::with_test_overrides(
            Arc::clone(&exec) as Arc<dyn crate::CommandExecutor>,
            custom_deadline,
            cache_dir.clone(),
            projects.clone(),
        );

        // Request returns immediately while refreshes are held, with missing + unavailable.
        let first = server
            .handle(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "oms_list_projects", "arguments": {"cards": true}}
            }))
            .unwrap();
        let cards = first["result"]["structuredContent"]["cards"]
            .as_array()
            .unwrap();
        assert_eq!(cards.len(), 5);
        assert_eq!(cards[0]["state"], "unavailable"); // "gone" sorts before "p0"
        for card in &cards[1..] {
            assert_eq!(card["state"], "missing");
        }
        // Repeated call while in-flight deduplicates per alias and caps concurrency at 2.
        let _ = server
            .handle(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "oms_list_projects", "arguments": {"cards": true}}
            }))
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while exec.active.load(Ordering::Acquire) < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(exec.max_active.load(Ordering::Acquire), 2);

        // Release workers and wait for all 4 valid aliases to become fresh.
        *exec.hold.lock().unwrap() = false;
        exec.cv.notify_all();
        let wait_until = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let reply = server
                .handle(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {"name": "oms_list_projects", "arguments": {"cards": true}}
                }))
                .unwrap();
            let current = reply["result"]["structuredContent"]["cards"]
                .as_array()
                .unwrap();
            if current[1..].iter().all(|c| c["state"] == "fresh") && server.project_cards.is_idle()
            {
                break;
            }
            assert!(std::time::Instant::now() < wait_until, "refresh timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(exec.max_active.load(Ordering::Acquire) <= 2);
        assert_eq!(exec.calls.load(Ordering::Acquire), 4);
        assert!(
            exec.seen_deadlines
                .lock()
                .unwrap()
                .iter()
                .all(|d| *d == custom_deadline)
        );

        // Fresh call queues zero refreshes.
        let calls_before = exec.calls.load(Ordering::Acquire);
        let fresh_reply = server
            .handle(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {"name": "oms_list_projects", "arguments": {"cards": true}}
            }))
            .unwrap();
        assert_eq!(
            fresh_reply["result"]["structuredContent"]["cards"][1]["goal"],
            "Refreshed in background"
        );
        assert_eq!(exec.calls.load(Ordering::Acquire), calls_before);

        // Stat-only staleness: 10-minute age, .git/HEAD, .git/index, and .oms/plan.
        let now = std::time::SystemTime::now();
        let p0_cache = cache_dir.join(card_cache_file_name(&projects["p0"]));
        set_mtime(&p0_cache, now - Duration::from_secs(601));
        set_mtime(
            &projects["p1"].join(".git/HEAD"),
            now + Duration::from_secs(5),
        );
        set_mtime(
            &projects["p2"].join(".git/index"),
            now + Duration::from_secs(5),
        );
        set_mtime(
            &projects["p3"].join(".oms/plan"),
            now + Duration::from_secs(5),
        );

        // Fail refreshes and verify old cards remain intact with nothing secret written.
        exec.fail_mode.store(true, Ordering::Release);
        let stale_reply = server
            .handle(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "oms_list_projects", "arguments": {"cards": true}}
            }))
            .unwrap();
        let stale_cards = stale_reply["result"]["structuredContent"]["cards"]
            .as_array()
            .unwrap();
        for card in &stale_cards[1..] {
            assert_eq!(card["state"], "stale");
            assert_eq!(card["goal"], "Refreshed in background");
        }
        let fail_wait = std::time::Instant::now() + Duration::from_secs(5);
        while exec.calls.load(Ordering::Acquire) < calls_before + 4
            || !server.project_cards.is_idle()
        {
            assert!(std::time::Instant::now() < fail_wait);
            std::thread::sleep(Duration::from_millis(5));
        }
        for idx in 0..4 {
            let file = cache_dir.join(card_cache_file_name(&projects[&format!("p{idx}")]));
            let body = std::fs::read_to_string(&file).unwrap();
            assert!(body.contains("Refreshed in background"));
            assert!(!body.contains(&"q".repeat(16)));
        }
    }
}
