//! CLI option parser and command dispatch for `oms linear`.

use crate::crypto::SecretValue;
use crate::model::ActivityKind;
use crate::reply::{ReplyConfig, execute_reply};
use crate::serve::{DEFAULT_LISTEN_ADDR, PreparedServer, ServeConfig, parse_loopback_listen};
use crate::transport::EndpointUrl;
use oms_command_room::Services as RoomServices;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const ENV_WEBHOOK_CRED: &str = concat!("OMS_LINEAR_WEBHOOK_", "SECRET");
pub const ENV_ACCESS_CRED: &str = concat!("OMS_LINEAR_ACCESS_", "TOKEN");

pub const HELP: &str = "\
OMS Linear agent session bridge: receive Linear webhooks into an OMS Room and send activities back.

Usage:
  oms linear serve --repo PATH --room ROOM --participant PART --to MAIN \\
    --organization ORG --app-user APPUSER [--listen 127.0.0.1:PORT]
  oms linear reply --repo PATH --room ROOM --participant PART --session ID \\
    --kind thought|response|elicitation|error --text-file FILE [--reply-to MSG] [--json]

Subcommands:
  serve                   Bind loopback HTTP receiver at POST /linear/webhook and deliver
                          verified AgentSessionEvent questions into an existing OMS Room
  reply                   Scan a reply file with oms-scrub, reconcile or create the Linear
                          agent activity with a deterministic UUID v4, and record the receipt

Options (serve):
  --repo PATH             Repository owning the existing OMS Room
  --room ROOM             Target OMS Room id
  --participant PART      Enrolled Room participant acting as the Linear bridge sender
  --to MAIN               Enrolled Main participant receiving addressed Room questions
  --organization ORG      Expected Linear organizationId (foreign installations get 403)
  --app-user APPUSER      Expected Linear appUserId for this OAuth app installation
  --listen ADDR           Loopback bind address (default 127.0.0.1:8787; port 0 selects ephemeral)

Options (reply):
  --repo PATH             Repository owning the existing OMS Room
  --room ROOM             Target OMS Room id where the delivery status receipt is recorded
  --participant PART      Enrolled Room participant recording the receipt
  --session ID            Linear agentSession id
  --kind KIND             thought | response | elicitation | error
  --text-file FILE        UTF-8 body file scanned by oms-scrub before network egress
  --reply-to MSG          Optional Room message id used as the deterministic caller key
  --json                  Emit structured JSON receipt output
  -h, --help              Show this help message and exit

Environment variables (validated at startup, non-empty, no whitespace; never put on argv):
  OMS_LINEAR_WEBHOOK_SECRET   Signing value verifying the raw body Linear-Signature header (serve)
  OMS_LINEAR_ACCESS_TOKEN     OAuth access value sent via exclusive 0600 curl config (serve, reply)
  OMS_LINEAR_API_URL          Test-only GraphQL endpoint override (https or loopback http only)

User-owned external prerequisites:
  - A public HTTPS tunnel terminating TLS and forwarding to loopback POST /linear/webhook.
  - A Linear OAuth app installed with actor=app and scopes app:assignable, app:mentionable.
";

const SERVE_VALUES: &[&str] = &[
    "--repo",
    "--room",
    "--participant",
    "--to",
    "--organization",
    "--app-user",
    "--listen",
];
const REPLY_VALUES: &[&str] = &[
    "--repo",
    "--room",
    "--participant",
    "--session",
    "--kind",
    "--text-file",
    "--reply-to",
];
const REPLY_FLAGS: &[&str] = &["--json"];

enum ParsedCommand {
    Help,
    Serve {
        repo: PathBuf,
        room: String,
        participant: String,
        to: String,
        organization: String,
        app_user: String,
        listen: String,
    },
    Reply {
        repo: PathBuf,
        room: String,
        participant: String,
        session: String,
        kind: ActivityKind,
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

fn parse(args: &[OsString], cwd: &Path) -> Result<ParsedCommand, String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(ParsedCommand::Help);
    }
    let Some(first) = args.first() else {
        return Err("missing subcommand (choose serve or reply)".into());
    };
    let action = first
        .to_str()
        .ok_or_else(|| "invalid linear subcommand".to_owned())?;
    if !matches!(action, "serve" | "reply") {
        if action.starts_with('-') {
            return Err(format!(
                "unrecognized option before subcommand: {}",
                sanitize_option_name(first.as_bytes())
            ));
        }
        return Err("invalid subcommand (choose serve or reply)".into());
    }

    let allowed_values = if action == "serve" {
        SERVE_VALUES
    } else {
        REPLY_VALUES
    };
    let allowed_flags: &[&str] = if action == "serve" { &[] } else { REPLY_FLAGS };

    let mut values: BTreeMap<String, OsString> = BTreeMap::new();
    let mut flags: BTreeSet<String> = BTreeSet::new();

    let mut i = 1;
    while i < args.len() {
        let raw = args[i].as_bytes();
        if raw == b"-h" || raw == b"--help" {
            return Ok(ParsedCommand::Help);
        }
        if !raw.starts_with(b"--") {
            return Err(format!("unexpected positional argument in {action}"));
        }
        let eq_pos = raw.iter().position(|b| *b == b'=');
        let name_bytes = &raw[..eq_pos.unwrap_or(raw.len())];
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            return Err("invalid option encoding".into());
        };
        if allowed_flags.contains(&name) {
            if eq_pos.is_some() {
                return Err(format!("{name} does not accept a value"));
            }
            flags.insert(name.to_owned());
            i += 1;
            continue;
        }
        if !allowed_values.contains(&name) {
            return Err(format!(
                "unrecognized option for {action}: {}",
                sanitize_option_name(raw)
            ));
        }
        let val = if let Some(at) = eq_pos {
            OsString::from_vec(raw[at + 1..].to_vec())
        } else {
            i += 1;
            let Some(next) = args.get(i) else {
                return Err(format!("{name} requires a value"));
            };
            if next.as_bytes().starts_with(b"-") {
                return Err(format!("{name} requires a value"));
            }
            next.clone()
        };
        values.insert(name.to_owned(), val);
        i += 1;
    }

    if action == "serve" {
        let repo = require_path(&values, "--repo", cwd)?;
        let room = require_utf8(&values, "--room")?;
        let participant = require_utf8(&values, "--participant")?;
        let to = require_utf8(&values, "--to")?;
        let organization = require_utf8(&values, "--organization")?;
        let app_user = require_utf8(&values, "--app-user")?;
        let listen = match values.get("--listen") {
            Some(v) => v
                .to_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "--listen must be valid non-empty UTF-8".to_owned())?
                .to_owned(),
            None => DEFAULT_LISTEN_ADDR.to_owned(),
        };
        Ok(ParsedCommand::Serve {
            repo,
            room,
            participant,
            to,
            organization,
            app_user,
            listen,
        })
    } else {
        let repo = require_path(&values, "--repo", cwd)?;
        let room = require_utf8(&values, "--room")?;
        let participant = require_utf8(&values, "--participant")?;
        let session = require_utf8(&values, "--session")?;
        let kind = ActivityKind::parse(&require_utf8(&values, "--kind")?)?;
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
            let _ = writeln!(stderr, "error: linear: {msg}");
            return 2;
        }
    };

    match parsed {
        ParsedCommand::Help => {
            let _ = stdout.write_all(HELP.as_bytes());
            let _ = stdout.flush();
            0
        }
        ParsedCommand::Serve {
            repo,
            room,
            participant,
            to,
            organization,
            app_user,
            listen,
        } => {
            let signing_material = match SecretValue::from_env(ENV_WEBHOOK_CRED) {
                Ok(v) => v,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: linear: {msg}");
                    return 2;
                }
            };
            let access_cred = match SecretValue::from_env(ENV_ACCESS_CRED) {
                Ok(v) => v,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: linear: {msg}");
                    return 2;
                }
            };
            let api_url = match EndpointUrl::from_env() {
                Ok(u) => u,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: linear: {msg}");
                    return 2;
                }
            };
            let listen_addr = match parse_loopback_listen(&listen) {
                Ok(a) => a,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: linear: {msg}");
                    return 2;
                }
            };
            let cfg = ServeConfig {
                repo,
                room,
                participant,
                to,
                organization,
                app_user,
                listen: listen_addr,
                signing_material,
                access_cred,
                api_url,
            };
            let prepared = match PreparedServer::bind(cfg, room_services, interrupted) {
                Ok(s) => s,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: linear: {msg}");
                    return 2;
                }
            };
            let _ = writeln!(stdout, "listening {}", prepared.local_addr());
            let _ = stdout.flush();
            match prepared.serve_until_stopped(room_services, interrupted, stderr) {
                Ok(()) => 0,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: linear: {msg}");
                    1
                }
            }
        }
        ParsedCommand::Reply {
            repo,
            room,
            participant,
            session,
            kind,
            text_file,
            reply_to,
            json,
        } => {
            let access_cred = match SecretValue::from_env(ENV_ACCESS_CRED) {
                Ok(v) => v,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: linear: {msg}");
                    return 2;
                }
            };
            let api_url = match EndpointUrl::from_env() {
                Ok(u) => u,
                Err(msg) => {
                    let _ = writeln!(stderr, "error: linear: {msg}");
                    return 2;
                }
            };
            let cfg = ReplyConfig {
                repo,
                room,
                participant,
                session,
                kind,
                text_file,
                reply_to,
                json,
                access_cred,
                api_url,
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
