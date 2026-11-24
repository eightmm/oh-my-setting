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

const URI: &str = "ui://oms/conversation-panel/v9.html";
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
const ASSIGN: &str = "oms_assign_provider";
const ASSIGN_RESULT: &str = "oms_assign_result";
const BINDING: &str = "Select a host-configured OMS project, or bind this connection to a project with OMS_PANEL_REPO.";
type PanelResult<T> = Result<T, String>;

pub(super) fn runtime_request(request: &Value) -> bool {
    let id = &request["id"];
    (matches!(id, Value::Number(_)) || id.as_str().is_some_and(|s| s.len() <= 1024))
        && request["method"] == "tools/call"
        && matches!(
            request["params"]["name"].as_str(),
            Some(
                SNAPSHOT
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
                    | ASSIGN
                    | ASSIGN_RESULT
            )
        )
}

fn project_read(name: &str) -> bool {
    matches!(
        name,
        OPEN | PROJECTS | SNAPSHOT | USAGE | GIT | GRAPH | VIEW | CAPABILITIES
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
    let mut opener = make(
        OPEN,
        "Open OMS beside this conversation.",
        scope.clone(),
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
    let all = json!([
        opener,
        chat,
        make(
            USAGE,
            "Read recorded usage for recent OMS attempts and local provider quota windows.",
            scope.clone(),
            &[],
            false
        ),
        make(
            GIT,
            "Read Git, open repository issues, PRs and linked GitHub Projects. Use inventory and nextCursor to page through all open issues or PRs, or kind and number to look up one. Use boardId from a linked project to read board pages.",
            json!({"project":project,"kind":{"type":"string","enum":["issue","pull"]},"number":{"type":"integer","minimum":1,"maximum":9_999_999_999u64},"inventory":{"type":"string","enum":["issues","pullRequests"]},"boardId":{"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9_-]{0,99}$"},"cursor":{"type":"string","minLength":1,"maxLength":500}}),
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
            "Read one OMS panel page. connections checks runtime PATH and harness policy, modelcatalog reads cached model and effort evidence, routes previews expected choices, and attribution shows recorded calls. Null means unverified.",
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
               "task":{"type":"string","minLength":1,"maxLength":4000}
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
fn alias(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
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
            Ok(m) if m.is_dir() || (marker == ".git" && m.is_file()) => return Ok(true),
            Ok(_) => return Err(BINDING.into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(BINDING.into()),
        }
    }
    Ok(false)
}

pub(super) fn response(data: PanelResult<Value>) -> Value {
    match data.and_then(|data| {
        crate::output::outbound(data.to_string().as_bytes(), &Value::Null)
            .map_err(|_| "OMS withheld this response during its outbound scan".to_owned())?;
        Ok(data)
    }) {
        Ok(data) => {
            json!({"content":[{"type":"text","text":data.to_string()}],"structuredContent":data,"isError":false})
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
                    Some(URI | V1 | V2 | V3 | V4 | V5 | V6 | V7 | V8)
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
                    OPEN | APPROVALS | SNAPSHOT | USAGE => &["project"],
                    GIT => &[
                        "project",
                        "kind",
                        "number",
                        "inventory",
                        "boardId",
                        "cursor",
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
                    ASSIGN => &["project", "room", "requestId", "provider", "title", "task"],
                    ASSIGN_RESULT => &["project", "room", "requestId", "after"],
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
                if name == ASSIGN_RESULT {
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
            let mut result =
                json!({"projects":projects.into_keys().collect::<Vec<_>>(),"binding":binding});
            if self.config.profile == ToolProfile::McpApp {
                result["readOnly"] = json!(true);
            }
            return Ok(result);
        }
        if name == OPEN {
            if args.get("project").is_some() {
                self.panel_repo(args)?;
            }
            let mut result = json!({"panel":"OMS","project":args["project"]});
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
        let request_room = || -> PanelResult<String> {
            let key = args["requestId"]
                .as_str()
                .filter(|v| uuid(v))
                .ok_or("A lowercase UUID requestId is required.")?;
            Ok(format!("oms-app-{key}"))
        };
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
            GIT => {
                let mut cmd = vec![
                    "github".into(),
                    "panel-status".into(),
                    "--repo".into(),
                    path,
                    "--json".into(),
                ];
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
            GIT if data["git"].is_object() && data["github"].is_object() => Ok(data),
            GRAPH
                if data["schema"] == 1
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
                Ok(data)
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

    fn assignment_argv(
        action: &str,
        repo: &Path,
        room: &str,
        request: &str,
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
            "--request".into(),
            request.into(),
        ];
        for (flag, value) in extra {
            argv.extend([OsString::from(*flag), OsString::from(*value)]);
        }
        argv.push("--json".into());
        argv
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
                Self::assignment_argv("desktop-assign-status", repo, room, request, &digest, &[]),
                false,
            )?;
            if data["request"] == request && data["status"] == "not_found" {
                return Ok(json!({"room":room,"requestId":request,"status":"not_found"}));
            }
            check(&data)?;
            let mut out = summary(&data, data["status"].as_str().unwrap_or("assigned"));
            out["task"] = data["task"].clone();
            if data["status"] == "started" {
                let op = data["operation"].as_str().unwrap_or_default();
                let dir = crate::storage::Directory::run(repo, op, false)
                    .map_err(|_| "The recorded assignment operation is unavailable.")?;
                let proof = crate::storage::meta(&dir.path);
                if proof["operation"] != op
                    || proof["thread"] != op
                    || proof["kind"] != "ask"
                    || proof["panel_assignment"] != binding
                    || proof["targets"] != json!([data["provider"]])
                {
                    return Err("The recorded operation does not match this assignment.".into());
                }
                let (state, exit) = crate::peer::status(&dir.path);
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
        let (data, _) = self.panel_exec(
            repo,
            Self::assignment_argv(
                "desktop-assign",
                repo,
                room,
                request,
                &digest,
                &[
                    ("--provider", provider),
                    ("--label", title),
                    ("--text", task),
                ],
            ),
            false,
        )?;
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
        if data["fresh"] != true {
            // A recorded intent without a receipt may already have launched; its
            // acknowledgment is unknown, so it is never dispatched again.
            return Ok(summary(
                &data,
                if data["status"] == "started" {
                    "recorded"
                } else {
                    "unconfirmed"
                },
            ));
        }
        let unconfirmed = |data: &Value| summary(data, "unconfirmed");
        let path = repo.to_str().ok_or("Repository path is not UTF-8.")?;
        let launched = self.start_bound_peer(
            &json!({
                "repo":path,"kind":"ask","prompt":task,"providers":provider,
                "debate_rounds":0,"include_memory":false,"include_task":false
            }),
            Some(&binding),
        );
        let Some(launched) = launched
            .ok()
            .and_then(|t| t.as_str().and_then(|t| crate::value::from_str(t).ok()))
            .filter(|v| v["started"] == true && v["thread"] == v["operation"])
        else {
            return Ok(unconfirmed(&data));
        };
        let Some(operation) = launched["operation"]
            .as_str()
            .filter(|v| crate::operation(v))
        else {
            return Ok(unconfirmed(&data));
        };
        let recorded = self.panel_exec(
            repo,
            Self::assignment_argv(
                "desktop-assign-receipt",
                repo,
                room,
                request,
                &digest,
                &[("--operation", operation)],
            ),
            false,
        );
        match recorded {
            Ok((receipt, _)) if check(&receipt).is_ok() && receipt["operation"] == operation => {
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
        if let Some(thread) = meta["threadId"].as_str().filter(|v| uuid(v)) {
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
        if partial {
            let was_degraded = dashboard["degraded"] == true;
            dashboard["degraded"] = json!(true);
            if dashboard["observation"].is_object()
                && (!was_degraded || dashboard["observation"]["completeness"] == "complete")
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
