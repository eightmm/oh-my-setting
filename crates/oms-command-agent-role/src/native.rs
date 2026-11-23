use crate::{policy, render, sort, write};
use serde_json::{Map, Value, json};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

type Response = (i32, Vec<u8>, Vec<u8>);

const HELP: &str = "Usage: agent-role.sh [options] <list|show|path|resolve|init>\n\nManage named worker role profiles (.oms/roles/<name>.md; global fallback\n~/.oh-my-setting/local/roles/<name>.md; bundled fallback ROOT/roles). Roles are\ninjected into a delegated worker brief with peer-delegate.sh --role NAME.\n\nOptions:\n  --repo PATH   Repo for project-scoped roles (default: PWD, git-root anchored).\n  --name NAME   Role name ([A-Za-z0-9._-]+). Required for show/path/resolve/init.\n  --provider P  claude, codex, or antigravity; required for render.\n  --global      Use the global role store instead of the repo; resolve/show\n                fall back only to bundled roles.\n  --json        With surface: print JSON instead of the table.\n  -h, --help    Show help.\n\nCommands:\n  surface   Read-only report per bundled role: read_only, tool belt, tool\n            count, MCP on/off (from config/role-contracts.json).\n  list      List available role names (repo, global, bundled; deduped).\n  show      Print the resolved role file (repo, then global, then bundled).\n  path      Print where a role file lives for the chosen scope (may not exist).\n  resolve   Print the first existing role file path (repo, global, bundled);\n            exit 3 if none. Used by peer-delegate.sh --role.\n  init      Create a starter role file at the chosen scope if missing.\n  render    Print a provider-native subagent definition for the resolved role\n            to stdout (--provider claude: .claude/agents/<name>.md format;\n            --provider codex: .codex/agents/<name>.toml format; --provider\n            antigravity: .agents/agents/<name>.md format). Name is\n            oms-<role>; Claude and Codex model/effort come from\n            config/panel-routing.json, while Antigravity sets\n            mainAgent: false, subagent: true, and model: inherit. By\n            default, Claude and Antigravity profiles list read-only tools\n            (Antigravity sets commandExecutionPolicy: off) and Codex\n            profiles emit sandbox_mode set to read-only. Only bundled\n            write roles render edit tools, commandExecutionPolicy:\n            sandbox, or workspace-write. Codex native custom agents\n            inherit the parent sandbox and approval policy. Treat\n            rendered settings as guidance and verify effective access in\n            the launcher. Writes nothing and installs nothing.\n";

pub(crate) fn error(code: i32, message: impl AsRef<[u8]>) -> Response {
    let mut err = b"error: ".to_vec();
    err.extend_from_slice(message.as_ref());
    err.push(b'\n');
    (code, Vec::new(), err)
}
fn ok(body: impl Into<Vec<u8>>) -> Response {
    (0, body.into(), Vec::new())
}
fn path_line(path: &Path) -> Vec<u8> {
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    bytes.push(b'\n');
    bytes
}
fn at(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

struct Args {
    repo: PathBuf,
    name: OsString,
    action: &'static str,
    global: bool,
    provider: OsString,
    json: bool,
}
fn parse(args: &[OsString], cwd: &Path) -> Result<Args, Response> {
    let mut parsed = Args {
        repo: cwd.to_path_buf(),
        name: OsString::new(),
        action: "list",
        global: false,
        provider: OsString::new(),
        json: false,
    };
    let mut i = 0;
    while i < args.len() {
        let value = &args[i];
        match value.as_bytes() {
            b"-h" | b"--help" => return Err(ok(HELP.as_bytes())),
            b"--global" => parsed.global = true,
            b"--json" => parsed.json = true,
            b"--repo" | b"--name" | b"--provider" => {
                let missing = match value.as_bytes() {
                    b"--repo" => "--repo requires a path",
                    b"--name" => "--name requires a value",
                    _ => "--provider requires claude or codex or antigravity",
                };
                i += 1;
                let Some(next) = args.get(i) else {
                    return Err(error(2, missing));
                };
                match value.as_bytes() {
                    b"--repo" => parsed.repo = at(cwd, Path::new(next)),
                    b"--name" => parsed.name = next.clone(),
                    _ => parsed.provider = next.clone(),
                }
            }
            b"list" => parsed.action = "list",
            b"show" => parsed.action = "show",
            b"path" => parsed.action = "path",
            b"resolve" => parsed.action = "resolve",
            b"init" => parsed.action = "init",
            b"render" => parsed.action = "render",
            b"surface" => parsed.action = "surface",
            _ => {
                let mut message = b"unknown argument: ".to_vec();
                message.extend_from_slice(value.as_bytes());
                return Err(error(2, message));
            }
        }
        i += 1;
    }
    if matches!(
        parsed.action,
        "show" | "path" | "resolve" | "init" | "render"
    ) {
        if parsed.name.is_empty() {
            return Err(error(2, format!("{} requires --name", parsed.action)));
        }
        if !parsed
            .name
            .as_bytes()
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
        {
            return Err(error(2, "--name must match [A-Za-z0-9._-]+"));
        }
    }
    Ok(parsed)
}

fn project_root(repo: &Path) -> Option<PathBuf> {
    let physical = fs::canonicalize(repo).ok()?;
    if !physical.is_dir() {
        return None;
    }
    if let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(&physical)
        .args(["rev-parse", "--show-toplevel"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        && output.status.success()
    {
        let mut raw = output.stdout;
        while raw.last() == Some(&b'\n') {
            raw.pop();
        }
        if !raw.is_empty() {
            return Some(PathBuf::from(OsString::from_vec(raw)));
        }
    }
    Some(physical)
}
fn global_dir() -> Result<PathBuf, Response> {
    if let Some(value) = std::env::var_os("OH_MY_SETTING_ROLES_DIR").filter(|s| !s.is_empty()) {
        return Ok(value.into());
    }
    std::env::var_os("HOME")
        .map(|p| PathBuf::from(p).join(".oh-my-setting/local/roles"))
        .ok_or_else(|| error(2, "HOME is not set"))
}
fn list(
    dirs: &[PathBuf],
    cwd: &Path,
    interrupted: &AtomicBool,
) -> Result<(Vec<u8>, Vec<u8>), sort::Failure> {
    let mut names = Vec::new();
    for dir in dirs {
        let actual = at(cwd, dir);
        if !fs::symlink_metadata(&actual).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(actual) else {
            continue;
        };
        for entry in entries.flatten() {
            if interrupted.load(Ordering::Relaxed) {
                return Err(sort::Failure::interrupted());
            }
            if !entry.file_type().is_ok_and(|t| t.is_file())
                || !entry.file_name().as_bytes().ends_with(b".md")
            {
                continue;
            }
            let path = dir.join(entry.file_name());
            // find/line-reader semantics also preserve arbitrary non-UTF-8 bytes.
            for line in path
                .as_os_str()
                .as_bytes()
                .split(|b| *b == b'\n')
                .filter(|s| !s.is_empty())
            {
                let base = line.rsplit(|b| *b == b'/').next().unwrap_or(line);
                let name = base.strip_suffix(b".md").unwrap_or(base);
                if name.len() >= sort::MAX_BYTES.saturating_sub(names.len()) {
                    return Err(sort::Failure::input_limit());
                }
                names.extend_from_slice(name);
                names.push(b'\n');
            }
        }
    }
    sort::collate(&names, cwd, interrupted)
}

pub(crate) fn run(
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
) -> Response {
    if interrupted.load(Ordering::Relaxed) {
        return (130, Vec::new(), Vec::new());
    }
    let args = match parse(args, cwd) {
        Ok(a) => a,
        Err(response) => return response,
    };
    if args.action == "init"
        && args.global
        && std::env::var_os("OMS_HARNESS_CHILD").as_deref() == Some(OsStr::new("1"))
    {
        return error(
            2,
            "a harness child cannot mutate parent-owned host or global state; return the request to the parent agent",
        );
    }
    let source = at(cwd, source);
    if args.action == "surface" {
        return match surface(&source, args.json) {
            Ok(body) => ok(body),
            Err(e) => (
                1,
                Vec::new(),
                format!("role-contracts: unreadable: {e}\n").into_bytes(),
            ),
        };
    }
    let project = project_root(&args.repo).map(|p| p.join(".oms/roles"));
    let global = match global_dir() {
        Ok(p) => p,
        Err(e) => return e,
    };
    let bundled = source.join("roles");
    if interrupted.load(Ordering::Relaxed) {
        return (130, Vec::new(), Vec::new());
    }
    if args.action == "list" {
        let mut dirs: Vec<_> = project.iter().cloned().collect();
        dirs.extend([global, bundled]);
        return match list(&dirs, cwd, interrupted) {
            Ok((out, err)) => (0, out, err),
            Err(e) if e.code == 130 => (130, Vec::new(), Vec::new()),
            Err(e) => error(e.code, e.message),
        };
    }
    let dir = if args.global {
        Some(&global)
    } else {
        project.as_ref()
    };
    let name = args.name.to_str().expect("validated ASCII name");
    let filename = format!("{name}.md");
    if matches!(args.action, "init" | "path") {
        let Some(dir) = dir else {
            return error(2, "could not resolve repo for project scope");
        };
        let target = dir.join(&filename);
        if args.action == "path" {
            return ok(path_line(&target));
        }
        return match write::init(&at(cwd, dir), &filename, name, !args.global, interrupted) {
            Ok(()) => {
                let mut body = b"role: created ".to_vec();
                body.extend(path_line(&target));
                ok(body)
            }
            Err(write::Failure::Exists) => {
                let mut message = b"role already exists: ".to_vec();
                message.extend_from_slice(target.as_os_str().as_bytes());
                error(2, message)
            }
            Err(write::Failure::Interrupted) => (130, Vec::new(), Vec::new()),
            Err(write::Failure::Io(e)) => error(1, e.to_string()),
        };
    }
    if args.action == "render"
        && !matches!(
            args.provider.as_bytes(),
            b"claude" | b"codex" | b"antigravity"
        )
    {
        return error(2, "render requires --provider claude|codex|antigravity");
    }
    let mut candidates = Vec::new();
    if !args.global {
        candidates.extend(project.iter());
    }
    candidates.extend([&global, &bundled]);
    let Some(dir) = candidates
        .into_iter()
        .find(|dir| at(cwd, &dir.join(&filename)).is_file())
    else {
        let mut message = format!("no role '{name}' (looked in ").into_bytes();
        if let Some(p) = &project {
            message.extend_from_slice(p.as_os_str().as_bytes());
        }
        message.extend_from_slice(b", ");
        message.extend_from_slice(global.as_os_str().as_bytes());
        message.extend_from_slice(b", and ");
        message.extend_from_slice(bundled.as_os_str().as_bytes());
        message.push(b')');
        return error(3, message);
    };
    let resolved = dir.join(&filename);
    if args.action == "resolve" {
        return ok(path_line(&resolved));
    }
    let bytes = match fs::read(at(cwd, &resolved)) {
        Ok(bytes) => bytes,
        Err(e) => return error(1, e.to_string()),
    };
    if args.action == "show" {
        return ok(bytes);
    }
    let text = match String::from_utf8(bytes) {
        Ok(s) => s.replace("\r\n", "\n").replace('\r', "\n"),
        Err(e) => return error(1, e.to_string()),
    };
    let policy = match policy::load(&source) {
        Ok(p) => p,
        Err(e) => return error(1, e),
    };
    match render::render(
        args.provider.to_str().unwrap(),
        name,
        &text,
        dir == &bundled,
        &policy,
    ) {
        Ok(body) => ok(body.into_bytes()),
        Err(e) => error(1, e),
    }
}

fn surface(source: &Path, as_json: bool) -> Result<Vec<u8>, String> {
    let data: Value = serde_json::from_slice(
        &fs::read(source.join("config/role-contracts.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let contracts = data
        .get("roles")
        .and_then(Value::as_object)
        .ok_or("missing roles object")?;
    let mut names: Vec<_> = contracts.keys().collect();
    names.sort();
    let mut rows = Vec::new();
    let mut lines = vec![format!(
        "{:<22} {:<9} {:<9} {:<7} {:<4} delegate-default",
        "role", "read_only", "belt", "tools", "mcp"
    )];
    for name in names {
        let entry = contracts[name]
            .as_object()
            .ok_or("role contract must be an object")?;
        let read_only = entry.get("read_only").cloned().unwrap_or(Value::Null);
        let read = read_only == Value::Bool(true);
        let belt = if !read {
            "default"
        } else if name == "researcher" {
            "read+web"
        } else {
            "read"
        };
        let tools = if !read {
            Value::Null
        } else {
            json!(if name == "researcher" { 6 } else { 4 })
        };
        let default = if read { "read" } else { "write" };
        let mut row = Map::new();
        row.insert("role".into(), json!(name));
        row.insert("read_only".into(), read_only.clone());
        row.insert(
            "writes".into(),
            entry.get("writes").cloned().unwrap_or(Value::Null),
        );
        row.insert("belt".into(), json!(belt));
        row.insert("tools".into(), tools.clone());
        row.insert("mcp".into(), json!(!read));
        row.insert("delegate_default".into(), json!(default));
        rows.push(Value::Object(row));
        let read_text = if read_only.is_null() {
            "none".to_string()
        } else {
            read_only.to_string().to_lowercase()
        };
        lines.push(format!(
            "{:<22} {:<9} {:<9} {:<7} {:<4} {}",
            name,
            read_text,
            belt,
            if tools.is_null() {
                "default".into()
            } else {
                tools.to_string()
            },
            if read { "off" } else { "on" },
            default
        ));
    }
    if as_json {
        let mut report = Map::new();
        report.insert("schema".into(), json!(1));
        report.insert("roles".into(), Value::Array(rows));
        let body = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        let mut ascii = String::new();
        for ch in body.chars() {
            if ch <= '\u{7e}' {
                ascii.push(ch);
            } else {
                use std::fmt::Write;
                let mut units = [0; 2];
                for unit in ch.encode_utf16(&mut units) {
                    write!(&mut ascii, "\\u{unit:04x}").unwrap();
                }
            }
        }
        ascii.push('\n');
        Ok(ascii.into_bytes())
    } else {
        lines.push("belt/tools/mcp: claude launch at the contract's own access; delegate-default is the access peer-delegate.sh --role gives a bundled role when no access flag is passed (read for read-only roles; OMS_DELEGATE_ROLE_WRITE=1 restores write)".into());
        Ok((lines.join("\n") + "\n").into_bytes())
    }
}
