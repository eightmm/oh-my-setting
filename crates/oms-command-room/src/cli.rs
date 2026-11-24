use crate::value::{Value, encode, obj};
use crate::{
    App, Error, Join, Relay, Result, Room, Send, Services, Session, checkpoint, relay_request,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const ACTIONS: &[&str] = &[
    "new",
    "join",
    "bind",
    "scope",
    "describe",
    "leave",
    "close",
    "send",
    "updates",
    "ack",
    "show",
    "list",
    "publish",
    "rollover",
    "relay-server",
    "desktop-start-status",
    "desktop-requests",
    "desktop-decide",
    "desktop-assign",
    "desktop-assign-status",
    "desktop-assign-receipt",
];
const VALUES: &[&str] = &[
    "--repo",
    "--id",
    "--participant",
    "--provider",
    "--role",
    "--model",
    "--label",
    "--native-session",
    "--replaces",
    "--owns",
    "--parent",
    "--topic",
    "--task-id",
    "--to",
    "--text",
    "--text-file",
    "--kind",
    "--reply-to",
    "--message-id",
    "--after",
    "--message",
    "--app",
    "--digest",
    "--version",
    "--decision",
    "--conversation-digest",
    "--request",
    "--operation",
    "--queue",
    "--worktree",
];
const FLAGS: &[&str] = &["--clear", "--json", "--exact", "--retry", "--allow-wakeup"];
pub const HELP: &str = "OMS work rooms: shared threads, addressed messages and explicit consumption.\n\nUsage: oms room ACTION [OPTIONS]\nActions: new join bind scope describe leave close send updates ack show list publish rollover relay-server desktop-start-status desktop-requests desktop-decide desktop-assign desktop-assign-status desktop-assign-receipt\n  --repo PATH             State repository (defaults to inherited room or current repository)\n  --id ID                 Room id (defaults to inherited room)\n  --participant ID        Exact participant (defaults to inherited participant)\n  --provider PROVIDER     Registered provider or adapter (default OMS_AGENT or codex)\n  --role ROLE             main, worker, advisor, reviewer, researcher\n  --model MODEL           Declared model\n  --label TEXT            Bounded plain participant label\n  --native-session ID     Native session id, or current inside Claude; stored only as a hash\n  --replaces ID           join only: atomically resume an exact terminal Main binding\n  --owns PATH             Repeatable declared relative scope, not a write grant\n  --clear                 scope only: clear declared scopes\n  --parent ID             Parent participant\n  --topic TEXT            New room title (default Shared work)\n  --task-id ID            Task identifier for new\n  --to ID                 Exact recipient or all; a worker defaults to its parent\n  --text TEXT             Inline message of 1..4000 UTF-8 bytes\n  --text-file PATH        Message file of at most 4000 bytes\n  --kind KIND             question, answer, note, decision, handoff, status\n  --reply-to ID           Exact message being answered\n  --message-id ID         Stable message identity for idempotent retry\n  --after CURSOR          Opaque updates cursor; never an acknowledgment\n  --message ID            Repeatable exact consumed message id for ack\n  --json                  JSON output\n  --exact                 show only: read this log itself, not its successor\n  --retry                 publish only: explicitly retry an uncertain snapshot; may duplicate output\n  --app APP               publish target codex or claude; default Codex fixed receiver\n  --allow-wakeup          Claude publish only: authorize native inbox delivery which can spend model usage\n  -h, --help              Show this help\n";
struct Options {
    action: String,
    values: BTreeMap<String, Vec<OsString>>,
    flags: BTreeSet<String>,
    explicit: BTreeSet<String>,
}
impl Options {
    fn path(&self, k: &str) -> Option<PathBuf> {
        self.values.get(k).and_then(|v| v.last()).map(PathBuf::from)
    }
    fn text(&self, k: &str) -> Result<Option<&str>> {
        self.values
            .get(k)
            .and_then(|v| v.last())
            .map(|v| {
                v.to_str()
                    .ok_or_else(|| Error::new(format!("{k} must be UTF-8 text")))
            })
            .transpose()
    }
    fn texts(&self, k: &str) -> Result<Vec<String>> {
        self.values
            .get(k)
            .map(|v| {
                v.iter()
                    .map(|v| {
                        v.to_str()
                            .map(str::to_owned)
                            .ok_or_else(|| Error::new(format!("{k} must be UTF-8 text")))
                    })
                    .collect()
            })
            .unwrap_or(Ok(vec![]))
    }
    fn flag(&self, k: &str) -> bool {
        self.flags.contains(k)
    }
}
fn parse(args: &[OsString]) -> Result<Options> {
    let mut options = Options {
        action: String::new(),
        values: BTreeMap::new(),
        flags: BTreeSet::new(),
        explicit: BTreeSet::new(),
    };
    let mut i = 0;
    let mut positional = false;
    while i < args.len() {
        let raw = args[i].as_bytes();
        if raw == b"--" && !positional {
            positional = true;
            i += 1;
            continue;
        }
        if raw.starts_with(b"--") && !positional {
            let equal = raw.iter().position(|b| *b == b'=');
            let name = std::str::from_utf8(&raw[..equal.unwrap_or(raw.len())])
                .map_err(|_| Error::new("invalid room option"))?;
            let choices = VALUES
                .iter()
                .chain(FLAGS)
                .filter(|k| **k == name || k.starts_with(name))
                .copied()
                .collect::<Vec<_>>();
            let name = if VALUES.contains(&name) || FLAGS.contains(&name) {
                name
            } else if choices.len() == 1 {
                choices[0]
            } else {
                return Err(Error::new(format!(
                    "unrecognized or ambiguous room option: {name}"
                )));
            };
            options.explicit.insert(name.to_owned());
            if FLAGS.contains(&name) {
                if equal.is_some() {
                    return Err(Error::new(format!("{name} does not accept a value")));
                }
                options.flags.insert(name.to_owned());
                i += 1;
                continue;
            }
            let value = if let Some(at) = equal {
                OsString::from_vec(raw[at + 1..].to_vec())
            } else {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| Error::new(format!("{name} requires a value")))?;
                if v.as_bytes().starts_with(b"-") {
                    return Err(Error::new(format!("{name} requires a value")));
                }
                v.clone()
            };
            let values = options.values.entry(name.to_owned()).or_default();
            if !matches!(name, "--owns" | "--message") {
                values.clear();
            }
            values.push(value);
        } else {
            let word = args[i]
                .to_str()
                .ok_or_else(|| Error::new("invalid room action"))?;
            if !options.action.is_empty() || !ACTIONS.contains(&word) {
                return Err(Error::new(format!("invalid room action: {word}")));
            }
            options.action = word.to_owned();
        }
        i += 1;
    }
    if options.action.is_empty() {
        return Err(Error::new("a room action is required"));
    }
    if options.action == "describe"
        && options.explicit.iter().any(|k| {
            !["--repo", "--id", "--participant", "--label", "--json"].contains(&k.as_str())
        })
    {
        return Err(Error::new(
            "describe accepts only --repo, --id, --participant, --label and --json",
        ));
    }
    if let Some(role) = options.text("--role")?
        && !crate::model::ROLES.contains(&role)
    {
        return Err(Error::new("invalid room role"));
    }
    if let Some(kind) = options.text("--kind")?
        && !crate::model::KINDS.contains(&kind)
    {
        return Err(Error::new("invalid message kind"));
    }
    if let Some(app) = options.text("--app")?
        && !["codex", "claude"].contains(&app)
    {
        return Err(Error::new("invalid app delivery options"));
    }
    let action = options.action.as_str();
    if options.values.contains_key("--replaces") && action != "join" {
        return Err(Error::new("--replaces applies to join only"));
    }
    if action == "scope" && (options.values.contains_key("--owns") == options.flag("--clear")) {
        return Err(Error::new(
            "scope needs --owns PATH (repeatable) or --clear",
        ));
    }
    if options.flag("--clear") && action != "scope" {
        return Err(Error::new("--clear applies to scope only"));
    }
    if action == "describe" && options.text("--label")?.is_none() {
        return Err(Error::new("describe requires --label"));
    }
    if options.flag("--retry") && action != "publish" {
        return Err(Error::new("--retry applies to publish only"));
    }
    if options.values.contains_key("--app") && action != "publish"
        || options.flag("--allow-wakeup")
            && (action != "publish" || options.text("--app")? != Some("claude"))
    {
        return Err(Error::new(
            "--app/--allow-wakeup apply to app publishing only",
        ));
    }
    if options.text("--app")? == Some("claude") && options.text("--to")?.unwrap_or("all") == "all" {
        return Err(Error::new("Claude app publishing needs --to PARTICIPANT"));
    }
    if options.flag("--exact") && action != "show" {
        return Err(Error::new("--exact applies to show only"));
    }
    Ok(options)
}
pub fn run(
    args: &[OsString],
    default_repo: &Path,
    stop: &AtomicBool,
    services: &dyn Services,
) -> (i32, Vec<u8>, Vec<u8>) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return (0, HELP.as_bytes().to_vec(), vec![]);
    }
    let options = match parse(args) {
        Ok(v) => v,
        Err(e) => return fail(e, vec![]),
    };
    let mut notices = Vec::new();
    let result = execute(&options, default_repo, stop, services, &mut notices);
    match result {
        Ok((value, relay)) => {
            if options.action == "relay-server" {
                return (0, vec![], notice_bytes(notices));
            }
            let output = if relay && !options.flag("--json") {
                format!("{}\n", value["status"].as_str().unwrap_or("ok")).into_bytes()
            } else {
                match encode(&value, !relay, false) {
                    Ok(mut out) => {
                        out.push(b'\n');
                        out
                    }
                    Err(e) => return fail(e, notices),
                }
            };
            (0, output, notice_bytes(notices))
        }
        Err(e) => fail(e, notices),
    }
}
fn fail(error: Error, mut notices: Vec<String>) -> (i32, Vec<u8>, Vec<u8>) {
    notices.push(format!(
        "error: room: {}",
        crate::model::clean(&error.message, 300)
    ));
    (2, vec![], notice_bytes(notices))
}
fn notice_bytes(notices: Vec<String>) -> Vec<u8> {
    if notices.is_empty() {
        vec![]
    } else {
        format!("{}\n", notices.join("\n")).into_bytes()
    }
}
fn execute(
    o: &Options,
    default_repo: &Path,
    stop: &AtomicBool,
    services: &dyn Services,
    notices: &mut Vec<String>,
) -> Result<(Value, bool)> {
    checkpoint(stop)?;
    let session = Session::from_env();
    let path = o
        .path("--repo")
        .or_else(|| session.repository.clone())
        .unwrap_or_else(|| default_repo.to_owned());
    let mut foreign = false;
    if o.values.contains_key("--repo")
        && matches!(o.action.as_str(), "list" | "show")
        && session.room.is_some()
        && let Some(bound) = &session.repository
    {
        let explicit = std::fs::canonicalize(services.state_repository(&path)?)
            .map_err(|_| Error::new("explicit repository unavailable"))?;
        let inherited = std::fs::canonicalize(services.state_repository(bound)?)
            .map_err(|_| Error::new("inherited room repository unavailable"))?;
        foreign = explicit != inherited;
    }
    let id = o.text("--id")?.or({
        if foreign {
            None
        } else {
            session.room.as_deref()
        }
    });
    let who = o.text("--participant")?.or({
        if foreign {
            None
        } else {
            session.participant.as_deref()
        }
    });
    if !matches!(o.action.as_str(), "new" | "list" | "desktop-requests") && id.is_none() {
        return Err(Error::new("this operation needs a room id"));
    }
    if [
        "join", "bind", "scope", "describe", "leave", "close", "send", "updates", "ack",
    ]
    .contains(&o.action.as_str())
        && who.is_none()
    {
        return Err(Error::new("this operation needs a participant"));
    }
    let room_id = id.unwrap_or("");
    let participant = who.unwrap_or("");
    if let Some(queue) = &session.relay_queue
        && matches!(o.action.as_str(), "send" | "ack")
    {
        let mut request = obj([
            ("action", o.action.clone().into()),
            ("room", room_id.into()),
            ("participant", participant.into()),
        ]);
        if o.action == "send" {
            let text = o.text("--text")?.unwrap_or("");
            if o.values.contains_key("--text-file") || text.trim().is_empty() || text.len() > 4000 {
                return Err(Error::new(
                    "relay send needs inline text of 1..4000 UTF-8 bytes",
                ));
            }
            request.set("to", o.text("--to")?.map(str::to_owned));
            request.set("kind", o.text("--kind")?.unwrap_or("note"));
            request.set("text", text);
            for (option, field) in [("--reply-to", "reply_to"), ("--message-id", "message_id")] {
                if let Some(v) = o.text(option)? {
                    request.set(field, v);
                }
            }
        } else {
            let mids = o.texts("--message")?;
            if mids.is_empty() {
                return Err(Error::new("ack requires the exact message ids consumed"));
            }
            request.set("messages", mids);
        }
        return Ok((relay_request(queue, &request, stop)?, true));
    }
    if o.action == "new" && session.child {
        return Err(Error::new("a worker cannot create a room"));
    }
    let core = Room::open(
        &path,
        session.clone(),
        services,
        stop,
        o.action == "new",
        foreign,
    )?;
    let execute = (|| {
        if o.action == "relay-server" {
            let queue = o.path("--queue").ok_or_else(|| {
                Error::new("relay-server requires --queue, --worktree, --id and --participant")
            })?;
            let worktree = o.path("--worktree").ok_or_else(|| {
                Error::new("relay-server requires --queue, --worktree, --id and --participant")
            })?;
            if room_id.is_empty() || participant.is_empty() {
                return Err(Error::new(
                    "relay-server requires --queue, --worktree, --id and --participant",
                ));
            }
            let mut server = Relay::open(&core, &queue, room_id, participant, &worktree)?;
            server.serve_forever(Duration::from_millis(100))?;
            return Ok(obj([]));
        }
        let resolved = if !room_id.is_empty()
            && !matches!(
                o.action.as_str(),
                "new"
                    | "rollover"
                    | "desktop-decide"
                    | "desktop-start-status"
                    | "desktop-assign"
                    | "desktop-assign-status"
                    | "desktop-assign-receipt"
            )
            && !o.flag("--exact")
        {
            core.resolve(crate::identifier(room_id, "room")?)?
        } else {
            room_id.to_owned()
        };
        let id = resolved.as_str();
        match o.action.as_str() {
            "new" => Ok(obj([(
                "id",
                core.create(
                    o.text("--id")?.or(id_if_nonempty(room_id)),
                    o.text("--topic")?.unwrap_or("Shared work"),
                    o.text("--task-id")?,
                    who,
                    None,
                )?
                .into(),
            )])),
            "join" => {
                let provider = std::env::var("OMS_AGENT").unwrap_or_else(|_| "codex".to_owned());
                let mut join = Join::new(participant, o.text("--provider")?.unwrap_or(&provider));
                join.role = o.text("--role")?.unwrap_or("main").to_owned();
                join.model = o.text("--model")?.map(str::to_owned);
                join.label = o.text("--label")?.map(str::to_owned);
                join.native_session = o.text("--native-session")?.map(str::to_owned);
                join.replaces = o.text("--replaces")?.map(str::to_owned);
                join.owns = o.texts("--owns")?;
                join.parent = o.text("--parent")?.map(str::to_owned);
                core.join(id, join)
            }
            "bind" => core.bind(
                id,
                participant,
                o.text("--native-session")?
                    .ok_or_else(|| Error::new("invalid native session"))?,
                None,
            ),
            "scope" => core.scope(id, participant, &o.texts("--owns")?),
            "describe" => core.describe(id, participant, o.text("--label")?.unwrap_or("")),
            "leave" => core.leave(id, participant),
            "close" => core.close(id, participant),
            "send" => {
                let recipient = if let Some(to) = o.text("--to")? {
                    to.to_owned()
                } else if session.child {
                    let state = crate::project(&core.records(id)?)?;
                    let p =
                        crate::model::member(&state, session.participant.as_deref().unwrap_or(""))?;
                    if p.text("parent").is_empty() {
                        "all".to_owned()
                    } else {
                        p.text("parent").to_owned()
                    }
                } else {
                    "all".to_owned()
                };
                let inline = o.text("--text")?.filter(|v| !v.is_empty());
                let file = o.path("--text-file").filter(|v| !v.as_os_str().is_empty());
                if inline.is_some() == file.is_some() {
                    return Err(Error::new("send needs exactly one text or text-file"));
                }
                let text = if let Some(path) = file {
                    let canonical = std::fs::canonicalize(&path)
                        .map_err(|_| Error::new("message file unavailable"))?;
                    let parent = canonical
                        .parent()
                        .ok_or_else(|| Error::new("message file has no parent"))?;
                    let root = oms_safe_fs::Root::open(parent)
                        .map_err(|_| Error::new("message file root unavailable"))?;
                    let raw = root
                        .read_no_follow(
                            Path::new(
                                canonical
                                    .file_name()
                                    .ok_or_else(|| Error::new("invalid message file"))?,
                            ),
                            oms_safe_fs::ByteReadOptions {
                                missing_ok: false,
                                max_bytes: 4000,
                            },
                        )
                        .map_err(|_| Error::new("message file must be at most 4000 bytes"))?;
                    String::from_utf8(raw).map_err(|_| Error::new("message file must be UTF-8"))?
                } else {
                    inline.unwrap_or("").to_owned()
                };
                core.send(
                    id,
                    Send {
                        sender: participant,
                        recipient: &recipient,
                        text: &text,
                        kind: o.text("--kind")?.unwrap_or("note"),
                        reply_to: o.text("--reply-to")?,
                        message_id: o.text("--message-id")?,
                        linked: true,
                        quiet: false,
                    },
                )
            }
            "updates" => core.updates(id, participant, o.text("--after")?.unwrap_or(""), 8192, 50),
            "ack" => {
                let mids = o.texts("--message")?;
                if mids.is_empty() {
                    return Err(Error::new("ack requires the exact message ids consumed"));
                }
                core.acknowledge(id, participant, &mids)?;
                Ok(obj([
                    ("status", "consumed".into()),
                    ("approval", false.into()),
                ]))
            }
            "rollover" => Ok(obj([
                ("id", core.rollover(room_id, who)?.into()),
                ("rolled", room_id.into()),
            ])),
            "desktop-start-status" => core.desktop_start_status(id),
            "desktop-requests" => core.desktop_requests(),
            "desktop-assign" => core.desktop_assign(
                id,
                o.text("--conversation-digest")?.unwrap_or(""),
                o.text("--request")?.unwrap_or(""),
                o.text("--provider")?.unwrap_or(""),
                o.text("--label")?.unwrap_or(""),
                o.text("--text")?.unwrap_or(""),
            ),
            "desktop-assign-status" => core.desktop_assign_status(
                id,
                o.text("--conversation-digest")?.unwrap_or(""),
                o.text("--request")?.unwrap_or(""),
            ),
            "desktop-assign-receipt" => core.desktop_assign_receipt(
                id,
                o.text("--conversation-digest")?.unwrap_or(""),
                o.text("--request")?.unwrap_or(""),
                o.text("--operation")?.unwrap_or(""),
            ),
            "desktop-decide" => core.desktop_decide(
                id,
                o.text("--message")?.unwrap_or(""),
                o.text("--version")?.unwrap_or(""),
                o.text("--digest")?.unwrap_or(""),
                o.text("--decision")?.unwrap_or(""),
            ),
            "list" => core.list(),
            "show" => core.status(id, o.flag("--exact")),
            "publish" => core.publish(
                id,
                o.flag("--retry"),
                if o.text("--app")? == Some("claude") {
                    App::Claude
                } else {
                    App::Codex
                },
                o.text("--to")?,
                o.flag("--allow-wakeup"),
            ),
            _ => Err(Error::new("invalid room action")),
        }
    })();
    notices.extend(core.take_notices());
    Ok((execute?, false))
}
fn id_if_nonempty(id: &str) -> Option<&str> {
    if id.is_empty() { None } else { Some(id) }
}
