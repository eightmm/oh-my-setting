//! Project-scoped RPC surface for the desktop app. The host owns repository
//! bindings; tool arguments can only select an alias from that configuration.
use crate::value::Value;
use crate::{CommandRequest, MIME, Server, error, reply};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const URI: &str = "ui://oms/conversation-panel/v4.html";
const V3: &str = "ui://oms/conversation-panel/v3.html";
const V1: &str = "ui://oms/conversation-panel/v1.html";
const V2: &str = "ui://oms/conversation-panel/v2.html";
const OPEN: &str = "oms_open_panel";
const PROJECTS: &str = "oms_list_projects";
const SNAPSHOT: &str = "oms_panel_snapshot";
const START: &str = "oms_start_main";
const START_STATUS: &str = "oms_start_status";
const APPROVALS: &str = "oms_list_approvals";
const DECIDE: &str = "oms_decide_approval";
const BINDING: &str = "Select a host-configured OMS project, or bind this connection to a project with OMS_PANEL_REPO.";
type PanelResult<T> = Result<T, String>;

pub(super) fn runtime_request(request: &Value) -> bool {
    let id = &request["id"];
    (matches!(id, Value::Number(_)) || id.as_str().is_some_and(|s| s.len() <= 1024))
        && request["method"] == "tools/call"
        && matches!(
            request["params"]["name"].as_str(),
            Some(SNAPSHOT | START | START_STATUS | APPROVALS | DECIDE)
        )
}

pub(super) fn definitions() -> Value {
    let make = |name: &str,
                description: &str,
                properties: Value,
                required: &[&str],
                write: bool| {
        json!({"name":name,"description":description,
          "inputSchema":{"type":"object","properties":properties,"required":required.to_vec(),"additionalProperties":false},
          "annotations":{"readOnlyHint":!write,"destructiveHint":write,"idempotentHint":!write,"openWorldHint":name == START},
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
    opener["_meta"]["openai/outputTemplate"] = json!(URI);
    opener["_meta"]["openai/ui"] = json!({"entrypoints":[{"type":"thread"}]});
    json!([
        opener,
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
            DECIDE,
            "Send the user's explicit decision for the exact reviewed Main question.",
            json!({
               "project":project,"id":{"type":"string"},"version":{"type":"integer","minimum":1},
               "digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"decision":{"type":"string","enum":["approve","deny"]}
            }),
            &["id", "version", "digest", "decision"],
            true
        )
    ])
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
            "resources/read" if matches!(params["uri"].as_str(), Some(URI | V1 | V2 | V3)) => {
                json!({"contents":[{
                    "uri":params["uri"],"mimeType":MIME,"text":include_str!("conversation-panel.html"),
                    "_meta":{"ui":{"csp":{"connectDomains":[],"resourceDomains":[]}}}
                }]})
            }
            "resources/read" => return error(id, -32602, "unknown panel resource"),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let allowed: &[&str] = match name {
                    PROJECTS => &[],
                    OPEN | APPROVALS | SNAPSHOT => &["project"],
                    START_STATUS => &["project", "requestId"],
                    START => &["project", "main", "goal", "requestId"],
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
                response(self.panel_call(name, args, &params["_meta"]))
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
                    "message id conflicts",
                    "The question already has a different decision.",
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
        Ok((
            crate::value::from_str(&output.stdout).map_err(|_| "Unreadable OMS response.")?,
            output.code != 0,
        ))
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
            return Ok(
                json!({"projects":projects.into_keys().collect::<Vec<_>>(),"binding":binding}),
            );
        }
        if name == OPEN {
            if args.get("project").is_some() {
                self.panel_repo(args)?;
            }
            return Ok(json!({"panel":"OMS","project":args["project"]}));
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
        let request_room = || -> PanelResult<String> {
            let key = args["requestId"]
                .as_str()
                .filter(|v| uuid(v))
                .ok_or("A lowercase UUID requestId is required.")?;
            Ok(format!("oms-app-{key}"))
        };
        let argv = match name {
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
        match name {
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
            dashboard["degraded"] = json!(true);
        }
        if dashboard.to_string().len() > crate::OUTPUT_LIMIT {
            return Err("OMS panel snapshot exceeds its output limit.".into());
        }
        Ok(dashboard)
    }
}
