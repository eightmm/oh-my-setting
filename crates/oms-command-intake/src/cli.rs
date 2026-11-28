//! CLI option parser and command dispatch for `oms intake`.

use crate::crypto::SecretStore;
use crate::model::intake_path;
use crate::reply::{ReplyConfig, execute_reply};
use crate::routes::{self, Route};
use crate::serve::{
    DEFAULT_LISTEN_ADDR, PreparedServer, ServeConfig, parse_loopback_listen, validate_route_room,
};
use crate::source::{Source, compiled_sources, find_source_arc};
use oms_command_room::Services as RoomServices;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const HELP_HEADER: &str = "\
OMS external tracker intake: receive signed webhooks into an OMS Room and send outbound-scanned replies.

Usage:
  oms intake serve --source SOURCE --repo PATH --room ROOM --participant PART --to MAIN \\
    [--listen 127.0.0.1:PORT] [source binding flags]
  oms intake serve --source SOURCE --routes [--listen 127.0.0.1:PORT] [source binding flags]
  oms intake route add --source SOURCE --team TEAM --repo PATH --room ROOM --participant PART \\
    --to MAIN
  oms intake route remove --source SOURCE --team TEAM
  oms intake route list [--json]
  oms intake reply --source SOURCE [--repo PATH] --room ROOM --participant PART --session ID \\
    --kind KIND --text-file FILE [--reply-to MSG] [--json]
  oms intake sources [--json]

Subcommands:
  serve                   Bind loopback HTTP receiver at POST /intake/<source> and deliver
                          verified WorkRequest questions into an existing OMS Room
  reply                   Scan a reply file with oms-scrub, reconcile or create the external
                          delivery with a deterministic ID, and record the Room status receipt
  route                   Add, remove or list route-table entries sending one external team's
                          sessions to an existing OMS Room (TEAM is a team key or id, or '*'
                          for the fallback route)
  sources                 List compiled source adapters, environment variables, and binding flags

Options (serve):
  --source SOURCE         Compiled intake adapter name (see `oms intake sources`)
  --repo PATH             Repository owning the existing OMS Room
  --room ROOM             Target OMS Room id
  --participant PART      Enrolled Room participant acting as the intake sender
  --to MAIN               Enrolled Main participant receiving addressed Room questions
  --routes                Deliver each session to the Room its team's route names, from the
                          route table (instead of --repo/--room/--participant/--to); sessions
                          of an unrouted team get a fixed reply and no Room message
  --listen ADDR           Loopback bind address (default 127.0.0.1:8787; port 0 selects ephemeral)
  [source binding flags]  Adapter-required binding flags (inspect with `oms intake sources`)

Options (reply):
  --source SOURCE         Compiled intake adapter name
  --repo PATH             Repository owning the existing OMS Room (defaults to current directory)
  --room ROOM             Target OMS Room id where the delivery status receipt is recorded
  --participant PART      Enrolled Room participant recording the receipt
  --session ID            External session identifier
  --kind KIND             Adapter reply kind (inspect with `oms intake sources`)
  --text-file FILE        UTF-8 body file scanned by oms-scrub before network egress
  --reply-to MSG          Optional Room message id included in the deterministic caller key
  --json                  Emit structured JSON receipt output
  -h, --help              Show this help message and exit

Notes:
  - Adapter secrets are read from validated environment variables only (non-empty, no whitespace
    or control characters; never passed on argv).
  - Terminate public HTTPS in a user-owned tunnel or reverse proxy forwarding to loopback
    POST /intake/<source>.
  - The route table is $XDG_CONFIG_HOME/oh-my-setting/intake-routes.json (or
    ~/.config/oh-my-setting/intake-routes.json, or the absolute OMS_INTAKE_ROUTES_FILE), a
    private 0600 file; `serve --routes` reads it once at start.

Compiled sources (run `oms intake sources` for adapter env vars, binding flags, and reply kinds):
";

pub fn help_text() -> String {
    use std::fmt::Write as _;
    let mut out = String::from(HELP_HEADER);
    for source in compiled_sources() {
        let _ = writeln!(out, "  {:<22}  {}", source.name(), source.description());
    }
    out
}

const CORE_SERVE_VALUES: &[&str] = &[
    "--source",
    "--repo",
    "--room",
    "--participant",
    "--to",
    "--listen",
];
const CORE_REPLY_VALUES: &[&str] = &[
    "--source",
    "--repo",
    "--room",
    "--participant",
    "--session",
    "--kind",
    "--text-file",
    "--reply-to",
];
const JSON_FLAGS: &[&str] = &["--json"];
const SERVE_FLAGS: &[&str] = &["--routes"];
const SINGLE_TARGET_VALUES: &[&str] = &["--repo", "--room", "--participant", "--to"];
const ROUTE_ADD_VALUES: &[&str] = &[
    "--source",
    "--team",
    "--repo",
    "--room",
    "--participant",
    "--to",
];

enum ParsedCommand {
    Help,
    Sources {
        json: bool,
    },
    Serve {
        source: Arc<dyn Source>,
        /// `None` reads the route table (`--routes`).
        target: Option<Route>,
        binding_flags: BTreeMap<String, String>,
        listen: String,
    },
    RouteAdd(Route),
    RouteRemove {
        source: String,
        team: String,
    },
    RouteList {
        json: bool,
    },
    Reply {
        source: Arc<dyn Source>,
        repo: PathBuf,
        room: String,
        participant: String,
        session: String,
        kind: String,
        text_file: PathBuf,
        reply_to: Option<String>,
        json: bool,
    },
}

fn sanitize_option_name(raw: &[u8]) -> String {
    let head = match raw.iter().position(|b| *b == b'=') {
        Some(idx) => &raw[..idx],
        None => raw,
    };
    let mut out = String::new();
    for ch in String::from_utf8_lossy(head).chars().take(64) {
        if ch.is_control() {
            out.push('?');
        } else {
            out.push(ch);
        }
    }
    out
}

fn resolve_path(raw: &Path, cwd: &Path) -> PathBuf {
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    }
}

fn require_utf8(values: &BTreeMap<String, OsString>, key: &str) -> Result<String, String> {
    let val = values
        .get(key)
        .ok_or_else(|| format!("missing required option {key}"))?;
    let text = val
        .to_str()
        .ok_or_else(|| format!("{key} must be valid UTF-8"))?;
    if text.is_empty() {
        return Err(format!("{key} must not be empty"));
    }
    Ok(text.to_owned())
}

fn require_path(
    values: &BTreeMap<String, OsString>,
    key: &str,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let val = values
        .get(key)
        .ok_or_else(|| format!("missing required option {key}"))?;
    if val.is_empty() {
        return Err(format!("{key} must not be empty"));
    }
    Ok(resolve_path(Path::new(val), cwd))
}

fn collect_options(
    args: &[OsString],
    action: &str,
    allowed_flags: &[&str],
) -> Result<(BTreeMap<String, OsString>, BTreeSet<String>), String> {
    let mut values: BTreeMap<String, OsString> = BTreeMap::new();
    let mut flags: BTreeSet<String> = BTreeSet::new();

    let mut i = 1;
    while i < args.len() {
        let raw = args[i].as_bytes();
        if !raw.starts_with(b"--") {
            return Err(format!("unexpected positional argument in {action}"));
        }
        let eq_pos = raw.iter().position(|b| *b == b'=');
        let name_bytes = &raw[..eq_pos.unwrap_or(raw.len())];
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            return Err("invalid option encoding".into());
        };
        let safe_name = sanitize_option_name(name.as_bytes());
        if allowed_flags.contains(&name) {
            if eq_pos.is_some() {
                return Err(format!("{safe_name} does not accept a value"));
            }
            if !flags.insert(name.to_owned()) {
                return Err(format!("duplicate option {safe_name}"));
            }
            i += 1;
            continue;
        }
        if values.contains_key(name) {
            return Err(format!("duplicate option {safe_name}"));
        }
        let val = if let Some(at) = eq_pos {
            OsString::from_vec(raw[at + 1..].to_vec())
        } else {
            i += 1;
            let Some(next) = args.get(i) else {
                return Err(format!("{safe_name} requires a value"));
            };
            if next.as_bytes().starts_with(b"-") {
                return Err(format!("{safe_name} requires a value"));
            }
            next.clone()
        };
        values.insert(name.to_owned(), val);
        i += 1;
    }

    Ok((values, flags))
}

fn parse(args: &[OsString], cwd: &Path) -> Result<ParsedCommand, String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(ParsedCommand::Help);
    }
    let Some(first) = args.first() else {
        return Err("missing subcommand (choose serve, reply, or sources)".into());
    };
    let action = first
        .to_str()
        .ok_or_else(|| "invalid intake subcommand".to_owned())?;
    if !matches!(action, "serve" | "reply" | "sources" | "route") {
        if action.starts_with('-') {
            return Err(format!(
                "unrecognized option before subcommand: {}",
                sanitize_option_name(first.as_bytes())
            ));
        }
        return Err("invalid subcommand (choose serve, reply, route, or sources)".into());
    }
    if action == "route" {
        return parse_route(&args[1..], cwd);
    }

    if action == "sources" {
        let (values, flags) = collect_options(args, action, JSON_FLAGS)?;
        if let Some(unexpected) = values.keys().next() {
            return Err(format!(
                "unrecognized option for sources: {}",
                sanitize_option_name(unexpected.as_bytes())
            ));
        }
        return Ok(ParsedCommand::Sources {
            json: flags.contains("--json"),
        });
    }

    let allowed_flags: &[&str] = if action == "serve" {
        SERVE_FLAGS
    } else {
        JSON_FLAGS
    };
    let (values, flags) = collect_options(args, action, allowed_flags)?;

    let source_name = require_utf8(&values, "--source")?;
    let Some(source) = find_source_arc(&source_name) else {
        return Err(format!(
            "unknown --source '{source_name}' (run `oms intake sources` to list compiled adapters)"
        ));
    };

    if action == "serve" {
        let adapter_flags = source.binding_flags();
        for key in values.keys() {
            let is_core = CORE_SERVE_VALUES.contains(&key.as_str());
            let is_adapter = adapter_flags.iter().any(|f| f.flag == key);
            if !is_core && !is_adapter {
                return Err(format!(
                    "unrecognized option for serve (--source {}): {}",
                    source.name(),
                    sanitize_option_name(key.as_bytes())
                ));
            }
        }
        let target = if flags.contains("--routes") {
            if let Some(key) = SINGLE_TARGET_VALUES
                .iter()
                .find(|k| values.contains_key(**k))
            {
                return Err(format!("--routes cannot be combined with {key}"));
            }
            None
        } else {
            Some(Route::any(
                source.name(),
                require_path(&values, "--repo", cwd)?,
                require_utf8(&values, "--room")?,
                require_utf8(&values, "--participant")?,
                require_utf8(&values, "--to")?,
            ))
        };
        let listen = match values.get("--listen") {
            Some(v) => v
                .to_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "--listen must be valid non-empty UTF-8".to_owned())?
                .to_owned(),
            None => DEFAULT_LISTEN_ADDR.to_owned(),
        };
        let mut binding_flags = BTreeMap::new();
        for spec in adapter_flags {
            if spec.required || values.contains_key(spec.flag) {
                let v = require_utf8(&values, spec.flag)?;
                binding_flags.insert(spec.flag.to_owned(), v);
            }
        }
        source.validate_binding(&binding_flags)?;

        Ok(ParsedCommand::Serve {
            source,
            target,
            binding_flags,
            listen,
        })
    } else {
        for key in values.keys() {
            if !CORE_REPLY_VALUES.contains(&key.as_str()) {
                return Err(format!(
                    "unrecognized option for reply: {}",
                    sanitize_option_name(key.as_bytes())
                ));
            }
        }
        let repo = if values.contains_key("--repo") {
            require_path(&values, "--repo", cwd)?
        } else {
            cwd.to_path_buf()
        };
        let room = require_utf8(&values, "--room")?;
        let participant = require_utf8(&values, "--participant")?;
        let session = require_utf8(&values, "--session")?;
        let kind = require_utf8(&values, "--kind")?;
        let allowed_kinds = source.reply_kinds();
        if !allowed_kinds.contains(&kind.as_str()) {
            return Err(format!(
                "invalid --kind '{kind}' for source '{}' (expected {})",
                source.name(),
                allowed_kinds.join(", ")
            ));
        }
        let text_file = require_path(&values, "--text-file", cwd)?;
        let reply_to = match values.get("--reply-to") {
            Some(v) => Some(
                v.to_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "--reply-to must be valid non-empty UTF-8".to_owned())?
                    .to_owned(),
            ),
            None => None,
        };
        Ok(ParsedCommand::Reply {
            source,
            repo,
            room,
            participant,
            session,
            kind,
            text_file,
            reply_to,
            json: flags.contains("--json"),
        })
    }
}

fn parse_route(args: &[OsString], cwd: &Path) -> Result<ParsedCommand, String> {
    let verb = args
        .first()
        .and_then(|a| a.to_str())
        .ok_or_else(|| "missing route action (choose add, remove, or list)".to_owned())?;
    let allowed: &[&str] = if verb == "list" { JSON_FLAGS } else { &[] };
    let (values, flags) = collect_options(args, &format!("route {verb}"), allowed)?;
    let expected: &[&str] = match verb {
        "add" => ROUTE_ADD_VALUES,
        "remove" => &["--source", "--team"],
        "list" => &[],
        _ => return Err("invalid route action (choose add, remove, or list)".into()),
    };
    if let Some(key) = values.keys().find(|k| !expected.contains(&k.as_str())) {
        return Err(format!(
            "unrecognized option for route {verb}: {}",
            sanitize_option_name(key.as_bytes())
        ));
    }
    if verb == "list" {
        return Ok(ParsedCommand::RouteList {
            json: flags.contains("--json"),
        });
    }
    let source = require_utf8(&values, "--source")?;
    if find_source_arc(&source).is_none() {
        return Err(format!(
            "unknown --source '{source}' (run `oms intake sources` to list compiled adapters)"
        ));
    }
    let team = require_utf8(&values, "--team")?;
    routes::validate_team(&team)?;
    if verb == "remove" {
        return Ok(ParsedCommand::RouteRemove { source, team });
    }
    let repo = require_path(&values, "--repo", cwd)?;
    let repo = std::fs::canonicalize(&repo)
        .map_err(|e| format!("--repo {} unavailable: {e}", repo.display()))?;
    Ok(ParsedCommand::RouteAdd(Route {
        source,
        team,
        repo,
        room: require_utf8(&values, "--room")?,
        participant: require_utf8(&values, "--participant")?,
        to: require_utf8(&values, "--to")?,
    }))
}

fn render_routes(list: &[Route], json_mode: bool) -> Vec<u8> {
    if json_mode {
        let rows: Vec<_> = list
            .iter()
            .map(|r| {
                json!({
                    "source": r.source,
                    "team": r.team,
                    "repo": r.repo.to_string_lossy(),
                    "room": r.room,
                    "participant": r.participant,
                    "to": r.to,
                })
            })
            .collect();
        let mut out = serde_json::to_vec(&json!({ "routes": rows })).unwrap_or_default();
        out.push(b'\n');
        return out;
    }
    if list.is_empty() {
        return b"no intake routes\n".to_vec();
    }
    let mut out = String::new();
    for r in list {
        use std::fmt::Write as _;
        let _ = writeln!(
            out,
            "{} {} -> {} room={} participant={} to={}",
            r.source,
            r.team,
            r.repo.display(),
            r.room,
            r.participant,
            r.to
        );
    }
    out.into_bytes()
}

fn render_sources(json_mode: bool) -> Vec<u8> {
    let sources = compiled_sources();
    if json_mode {
        let rows: Vec<_> = sources
            .iter()
            .map(|s| {
                json!({
                    "source": s.name(),
                    "route": format!("POST {}", intake_path(s.name())),
                    "description": s.description(),
                    "serve_env": s.serve_env().iter().map(|e| json!({
                        "name": e.name,
                        "required": e.required,
                        "sensitive": e.sensitive,
                        "description": e.description,
                    })).collect::<Vec<_>>(),
                    "reply_env": s.reply_env().iter().map(|e| json!({
                        "name": e.name,
                        "required": e.required,
                        "sensitive": e.sensitive,
                        "description": e.description,
                    })).collect::<Vec<_>>(),
                    "binding_flags": s.binding_flags().iter().map(|f| json!({
                        "flag": f.flag,
                        "value_name": f.value_name,
                        "required": f.required,
                        "description": f.description,
                    })).collect::<Vec<_>>(),
                    "reply_kinds": s.reply_kinds(),
                })
            })
            .collect();
        let mut out = serde_json::to_vec(&json!({ "sources": rows })).unwrap_or_default();
        out.push(b'\n');
        return out;
    }

    let mut out = String::new();
    for s in sources {
        use std::fmt::Write as _;
        let serve_env = s
            .serve_env()
            .iter()
            .map(|e| {
                if e.required {
                    e.name.to_owned()
                } else {
                    format!("{} (optional)", e.name)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let reply_env = s
            .reply_env()
            .iter()
            .map(|e| {
                if e.required {
                    e.name.to_owned()
                } else {
                    format!("{} (optional)", e.name)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let flags = s
            .binding_flags()
            .iter()
            .map(|f| format!("{} {}", f.flag, f.value_name))
            .collect::<Vec<_>>()
            .join(", ");
        let kinds = s.reply_kinds().join(", ");
        let _ = writeln!(out, "{}: {}", s.name(), s.description());
        let _ = writeln!(out, "  route: POST {}", intake_path(s.name()));
        let _ = writeln!(out, "  serve env: {serve_env}");
        let _ = writeln!(out, "  reply env: {reply_env}");
        let _ = writeln!(out, "  binding flags: {flags}");
        let _ = writeln!(out, "  reply kinds: {kinds}");
    }
    out.into_bytes()
}

pub fn run_streaming_with_services(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    room_services: &dyn RoomServices,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let parsed = match parse(args, default_repo) {
        Ok(cmd) => cmd,
        Err(msg) => {
            let _ = writeln!(stderr, "error: intake: {msg}");
            return 2;
        }
    };

    match parsed {
        ParsedCommand::Help => {
            let rendered = help_text();
            let _ = stdout.write_all(rendered.as_bytes());
            let _ = stdout.flush();
            0
        }
        ParsedCommand::Sources { json } => {
            let rendered = render_sources(json);
            let _ = stdout.write_all(&rendered);
            let _ = stdout.flush();
            0
        }
        ParsedCommand::RouteList { json } => {
            match routes::routes_path().and_then(|p| routes::load(&p)) {
                Ok(list) => {
                    let _ = stdout.write_all(&render_routes(&list, json));
                    let _ = stdout.flush();
                    0
                }
                Err(msg) => {
                    let _ = writeln!(stderr, "error: intake: {msg}");
                    2
                }
            }
        }
        ParsedCommand::RouteAdd(route) => {
            if let Err(msg) = validate_route_room(&route, room_services, interrupted) {
                let _ = writeln!(stderr, "error: intake: {msg}");
                return 2;
            }
            let result = routes::routes_path().and_then(|path| {
                routes::update(&path, |list| {
                    match list
                        .iter_mut()
                        .find(|r| r.source == route.source && r.team == route.team)
                    {
                        Some(existing) if *existing == route => Ok(false),
                        Some(existing) => {
                            *existing = route.clone();
                            Ok(true)
                        }
                        None => {
                            list.push(route.clone());
                            Ok(true)
                        }
                    }
                })
            });
            match result {
                Ok(changed) => {
                    let verb = if changed { "routed" } else { "already routed" };
                    let _ = writeln!(
                        stdout,
                        "{verb} {} {} -> {} room={}",
                        route.source,
                        route.team,
                        route.repo.display(),
                        route.room
                    );
                    0
                }
                Err(msg) => {
                    let _ = writeln!(stderr, "error: intake: {msg}");
                    2
                }
            }
        }
        ParsedCommand::RouteRemove { source, team } => {
            let result = routes::routes_path().and_then(|path| {
                routes::update(&path, |list| {
                    let before = list.len();
                    list.retain(|r| !(r.source == source && r.team == team));
                    Ok(list.len() != before)
                })
            });
            match result {
                Ok(true) => {
                    let _ = writeln!(stdout, "removed route {source} {team}");
                    0
                }
                Ok(false) => {
                    let _ = writeln!(stderr, "error: intake: no route for {source} team {team}");
                    1
                }
                Err(msg) => {
                    let _ = writeln!(stderr, "error: intake: {msg}");
                    2
                }
            }
        }
        ParsedCommand::Serve {
            source,
            target,
            binding_flags,
            listen,
        } => {
            let routes = match target {
                Some(route) => vec![route],
                None => match routes::routes_path().and_then(|p| routes::load(&p)) {
                    Ok(list) => list
                        .into_iter()
                        .filter(|r| r.source == source.name())
                        .collect(),
                    Err(msg) => {
                        let _ = writeln!(stderr, "error: intake: {msg}");
                        return 2;
                    }
                },
            };
            let secrets = match SecretStore::from_env_specs(source.serve_env()) {
                Ok(s) => s,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: intake: {msg}");
                    return 2;
                }
            };
            if let Err(msg) = source.validate_endpoint() {
                let _ = writeln!(stderr, "error: intake: {msg}");
                return 2;
            }
            let listen_addr = match parse_loopback_listen(&listen) {
                Ok(a) => a,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: intake: {msg}");
                    return 2;
                }
            };
            let cfg = ServeConfig {
                source,
                routes,
                binding_flags,
                listen: listen_addr,
                secrets,
            };
            let prepared = match PreparedServer::bind(cfg, room_services, interrupted) {
                Ok(s) => s,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: intake: {msg}");
                    return 2;
                }
            };
            let _ = writeln!(stdout, "listening {}", prepared.local_addr());
            let _ = stdout.flush();
            match prepared.serve_until_stopped(room_services, interrupted, stderr) {
                Ok(()) => 0,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: intake: {msg}");
                    1
                }
            }
        }
        ParsedCommand::Reply {
            source,
            repo,
            room,
            participant,
            session,
            kind,
            text_file,
            reply_to,
            json,
        } => {
            let secrets = match SecretStore::from_env_specs(source.reply_env()) {
                Ok(s) => s,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: intake: {msg}");
                    return 2;
                }
            };
            if let Err(msg) = source.validate_endpoint() {
                let _ = writeln!(stderr, "error: intake: {msg}");
                return 2;
            }
            let cfg = ReplyConfig {
                source,
                repo,
                room,
                participant,
                session,
                kind,
                text_file,
                reply_to,
                json,
                secrets,
            };
            let (code, out, err) = execute_reply(&cfg, room_services, interrupted);
            let _ = stdout.write_all(&out);
            let _ = stderr.write_all(&err);
            let _ = stdout.flush();
            let _ = stderr.flush();
            code
        }
    }
}
