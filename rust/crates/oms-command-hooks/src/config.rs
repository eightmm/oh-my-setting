use crate::{Output, common::*};
use oms_toml::{Table, Value as T};
use std::{ffi::OsString, path::Path};

const MAX: usize = 8 * 1024 * 1024;
const STATUS: &str = "status_line = [\"model-with-reasoning\", \"context-remaining\", \"five-hour-limit\", \"weekly-limit\", \"git-branch\"]";
struct Block {
    begin: String,
    end: String,
    lines: Vec<String>,
    notify: bool,
}
impl Block {
    fn hud() -> Self {
        Self {
            begin: "# >>> oh-my-setting managed Codex HUD >>>".into(),
            end: "# <<< oh-my-setting managed Codex HUD <<<".into(),
            lines: vec![STATUS.into()],
            notify: false,
        }
    }
    fn usage(name: &str, lines: &[&str]) -> Self {
        Self {
            begin: format!("# >>> oh-my-setting managed Codex usage ({name}) >>>"),
            end: format!("# <<< oh-my-setting managed Codex usage ({name}) <<<"),
            lines: lines.iter().map(|s| (*s).into()).collect(),
            notify: name == "notify",
        }
    }
    fn fence(&self, nl: &str) -> Vec<String> {
        std::iter::once(&self.begin)
            .chain(&self.lines)
            .chain(std::iter::once(&self.end))
            .map(|s| format!("{s}{nl}"))
            .collect()
    }
}
fn body(s: &str) -> &str {
    s.trim_end_matches(['\r', '\n'])
}
fn table(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('[') && s.split('#').next().unwrap_or("").trim_end().ends_with(']')
}
fn header(s: &str, name: &str) -> bool {
    let s = body(s).split('#').next().unwrap_or("").trim();
    let Some(inner) = s.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    if inner.starts_with('[') {
        return false;
    }
    inner
        .split('.')
        .map(|p| p.trim().trim_matches(['\'', '"']))
        .collect::<Vec<_>>()
        .join(".")
        == name
}
fn span(
    lines: &[String],
    b: &Block,
    hud: bool,
) -> Result<Option<(usize, usize, bool, bool)>, String> {
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, s)| body(s) == b.begin)
        .map(|(i, _)| i)
        .collect();
    let ends: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, s)| body(s) == b.end)
        .map(|(i, _)| i)
        .collect();
    if starts.is_empty() && ends.is_empty() {
        return Ok(None);
    }
    if starts.len() != 1 || ends.len() != 1 || ends[0] <= starts[0] {
        return Err(format!(
            "managed Codex {} markers are incomplete or duplicated",
            if hud { "HUD" } else { "usage" }
        ));
    }
    let (start, end) = (starts[0], ends[0]);
    let inner: Vec<_> = lines[start + 1..end].iter().map(|s| body(s)).collect();
    let displaced = hud
        && inner.first() == Some(&STATUS)
        && inner
            .iter()
            .skip(1)
            .find(|s| !s.trim().is_empty())
            .is_some_and(|s| table(s))
        && lines[end + 1..].iter().all(|s| s.trim().is_empty());
    let owned = if b.notify {
        inner.len() == 1
            && inner[0].starts_with("notify = [")
            && (inner[0].ends_with("codex_turn_notify.py\"]")
                || inner[0].ends_with("\"codex-turn-notify\"]"))
    } else {
        inner == b.lines.iter().map(String::as_str).collect::<Vec<_>>()
    };
    Ok(Some((start, end, owned || displaced, displaced)))
}
fn parse(lines: &[String]) -> Result<Table, String> {
    oms_toml::parse_bounded(lines.concat().as_bytes(), MAX)
        .map_err(|e| format!("config is not valid TOML ({e})"))
}
fn status_key(lines: &[String]) -> (bool, Option<usize>) {
    let mut tui = false;
    let mut root = true;
    let mut first = None;
    for (i, line) in lines.iter().enumerate() {
        let s = body(line).trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        if header(s, "tui") {
            tui = true;
            root = false;
            if first.is_none() {
                first = Some(i);
            }
            continue;
        }
        if table(s) {
            tui = false;
            root = false;
            continue;
        }
        let key = s.split_once('=').map(|(k, _)| k.trim()).unwrap_or("");
        if tui && key.trim_matches(['\'', '"']) == "status_line"
            || root
                && key
                    .split('.')
                    .map(|p| p.trim().trim_matches(['\'', '"']))
                    .collect::<Vec<_>>()
                    == ["tui", "status_line"]
        {
            return (true, first);
        }
    }
    (false, first)
}
fn hud(
    action: &str,
    mut lines: Vec<String>,
    nl: &str,
    dry: bool,
) -> Result<(Vec<String>, Vec<String>, i32), String> {
    let block = Block::hud();
    let marked = span(&lines, &block, true)?;
    let (exists, head) = status_key(&lines);
    if action == "check" {
        let managed = marked.is_some_and(|s| s.2);
        return Ok((
            lines,
            vec![
                if managed {
                    "managed"
                } else if exists {
                    "user"
                } else {
                    "missing"
                }
                .into(),
            ],
            if managed || exists { 0 } else { 1 },
        ));
    }
    if action == "install" {
        if let Some((_, _, managed, _)) = marked {
            return Ok((
                lines,
                vec![
                    if managed {
                        "already current"
                    } else {
                        "preserved customized status line"
                    }
                    .into(),
                ],
                0,
            ));
        }
        if exists {
            return Ok((lines, vec!["preserved user status line".into()], 0));
        }
        let block = block.fence(nl);
        if let Some(i) = head {
            if !lines[i].ends_with(['\n', '\r']) {
                lines[i].push_str(nl);
            }
            lines.splice(i + 1..i + 1, block);
        } else {
            if let Some(last) = lines.last_mut()
                && !last.ends_with(['\n', '\r'])
            {
                last.push_str(nl);
            }
            if lines.last().is_some_and(|s| !s.trim().is_empty()) {
                lines.push(nl.into());
            }
            lines.push(format!("[tui]{nl}"));
            lines.extend(block);
        }
        parse(&lines)?;
        return Ok((
            lines,
            vec![if dry { "would install" } else { "installed" }.into()],
            0,
        ));
    }
    let Some((start, end, managed, displaced)) = marked else {
        return Ok((lines, vec!["already absent".into()], 0));
    };
    if !managed {
        return Ok((lines, vec!["preserved customized status line".into()], 0));
    }
    if displaced {
        lines.remove(end);
        lines.remove(start + 1);
        lines.remove(start);
    } else {
        lines.drain(start..=end);
    }
    Ok((
        lines,
        vec![if dry { "would remove" } else { "removed" }.into()],
        0,
    ))
}
fn root_state(
    lines: &[String],
    parsed: &Table,
    b: &Block,
    key: &str,
) -> Result<&'static str, String> {
    Ok(if let Some((_, _, managed, _)) = span(lines, b, false)? {
        if managed { "managed" } else { "customized" }
    } else if parsed.contains_key(key) {
        "user"
    } else {
        "missing"
    })
}
fn v2_state(lines: &[String], parsed: &Table, b: &Block) -> Result<&'static str, String> {
    if let Some((_, _, managed, _)) = span(lines, b, false)? {
        return Ok(if managed { "managed" } else { "customized" });
    }
    let table = parsed.get("features").and_then(|t| t.get("multi_agent_v2"));
    if table == Some(&T::Boolean(true)) {
        return Ok("boolean");
    }
    let Some(t) = table
        .and_then(T::as_table)
        .filter(|t| t.get("enabled") == Some(&T::Boolean(true)))
    else {
        return Ok("disabled");
    };
    if ["min_wait_timeout_ms", "default_wait_timeout_ms"]
        .iter()
        .any(|k| t.contains_key(*k))
    {
        return Ok("user");
    }
    if t.get("max_wait_timeout_ms")
        .is_some_and(|v| v.as_integer().is_none_or(|n| n < 300000))
    {
        return Ok("conflict");
    }
    Ok(
        if lines.iter().any(|s| header(s, "features.multi_agent_v2")) {
            "missing"
        } else {
            "shape"
        },
    )
}
fn root_note(state: &str) -> &'static str {
    match state {
        "managed" => "already current",
        "customized" => "preserved customized value",
        _ => "preserved user value",
    }
}
fn v2_note(state: &str) -> &'static str {
    match state {
        "managed" => "already current",
        "customized" => "preserved customized values",
        "user" => "preserved user values",
        "disabled" => "skipped (multi_agent_v2 is not enabled in this config)",
        "boolean" => "preserved (enabled as a boolean; a table would conflict with it)",
        "conflict" => "preserved (max_wait_timeout_ms is below the managed default)",
        _ => "preserved (enabled in a form this helper does not edit)",
    }
}
fn usage(
    action: &str,
    mut lines: Vec<String>,
    nl: &str,
    dry: bool,
    notify: Option<&str>,
    ctx: &Context<'_>,
) -> Result<(Vec<String>, Vec<String>, i32), String> {
    let root = Block::usage("root", &["background_terminal_max_timeout = 900000"]);
    let v2 = Block::usage(
        "multi_agent_v2",
        &[
            "min_wait_timeout_ms = 120000",
            "default_wait_timeout_ms = 300000",
        ],
    );
    let mut notifier = Block::usage("notify", &[]);
    if let Some(script) = notify {
        let target = absolute(ctx.cwd, Path::new(script));
        if Path::new(script)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
        {
            return Err("Python notifier scripts are retired".into());
        }
        let command = vec![
            target.to_string_lossy().into_owned(),
            "codex-turn-notify".into(),
        ];
        notifier.lines = vec![format!(
            "notify = {}",
            serde_json::to_string(&command).map_err(|_| "invalid notifier path")?
        )];
    }
    if action == "remove" {
        let mut spans = Vec::new();
        let mut notes = Vec::new();
        let mut ours = std::collections::BTreeSet::new();
        for (b, label) in [
            (&root, "terminal ceiling"),
            (&v2, "multi_agent_v2 waits"),
            (&notifier, "turn notify"),
        ] {
            let marked = span(&lines, b, false)?;
            let note = match marked {
                None => "already absent",
                Some((_, _, false, _)) => "preserved customized value",
                Some((s, e, true, _)) => {
                    spans.push((s, e));
                    ours.extend(s..=e);
                    if dry { "would remove" } else { "removed" }
                }
            };
            notes.push(format!("{label} {note}"));
        }
        spans.sort_unstable();
        for (s, mut e) in spans.into_iter().rev() {
            let top = (0..s).all(|i| ours.contains(&i) || lines[i].trim().is_empty());
            if top && lines.get(e + 1).is_some_and(|l| l.trim().is_empty()) {
                e += 1;
            }
            lines.drain(s..=e);
        }
        return Ok((lines, notes, 0));
    }
    let parsed = parse(&lines)?;
    let r = root_state(&lines, &parsed, &root, "background_terminal_max_timeout")?;
    let w = v2_state(&lines, &parsed, &v2)?;
    let n = root_state(&lines, &parsed, &notifier, "notify")?;
    if action == "check" {
        let shown = if r == "user" {
            let shown = match parsed.get("background_terminal_max_timeout") {
                Some(T::Integer(n)) => n.to_string(),
                Some(T::String(s)) => s.clone(),
                Some(T::Boolean(b)) => if *b { "True" } else { "False" }.into(),
                _ => "None".into(),
            };
            format!("user({shown})")
        } else {
            r.into()
        };
        return Ok((
            lines,
            vec![format!(
                "terminal-ceiling={shown} v2-waits={w} turn-notify={n} scope=config-file effective=unverified"
            )],
            if r == "missing" { 1 } else { 0 },
        ));
    }
    if w == "missing" {
        let i = lines
            .iter()
            .position(|s| header(s, "features.multi_agent_v2"))
            .ok_or("missing multi-agent header")?;
        if !lines[i].ends_with(['\n', '\r']) {
            lines[i].push_str(nl);
        }
        lines.splice(i + 1..i + 1, v2.fence(nl));
    }
    for (state, b) in [
        (r, &root),
        (if notify.is_some() { n } else { "skipped" }, &notifier),
    ] {
        if state == "missing" {
            let mut prefix = b.fence(nl);
            if !lines.is_empty() {
                prefix.push(nl.into());
            }
            lines.splice(0..0, prefix);
        }
    }
    let after = parse(&lines)?;
    if r == "missing" && after.get("background_terminal_max_timeout") != Some(&T::Integer(900000))
        || w == "missing"
            && after
                .get("features")
                .and_then(|v| v.get("multi_agent_v2"))
                .and_then(|v| v.get("default_wait_timeout_ms"))
                != Some(&T::Integer(300000))
    {
        return Err("managed Codex usage keys did not land where intended".into());
    }
    let verb = if dry { "would install" } else { "installed" };
    let mut notes = vec![
        format!(
            "terminal ceiling {}",
            if r == "missing" {
                format!("{verb} ({})", root.lines[0])
            } else {
                root_note(r).into()
            }
        ),
        format!(
            "multi_agent_v2 waits {}",
            if w == "missing" {
                format!("{verb} ({})", v2.lines.join(", "))
            } else {
                v2_note(w).into()
            }
        ),
    ];
    if notify.is_some() {
        notes.push(format!(
            "turn notify {}",
            if n == "missing" {
                format!("{verb} ({})", notifier.lines[0])
            } else {
                root_note(n).into()
            }
        ));
    }
    Ok((lines, notes, 0))
}
pub fn run(name: &str, args: &[OsString], ctx: &Context<'_>) -> Output {
    let mut position = Vec::new();
    let mut dry = false;
    let mut notify = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str() {
            Some("--dry-run") => dry = true,
            Some("--notify-script") if name == "codex-usage-config" => {
                i += 1;
                let Some(v) = args.get(i).and_then(|a| a.to_str()) else {
                    return fail("--notify-script requires a path");
                };
                notify = Some(v);
            }
            Some("--help" | "-h") => {
                return out(format!(
                    "Usage: {name} {{install,remove,check}} PATH [--dry-run]{}\n",
                    if name == "codex-usage-config" {
                        " [--notify-script PATH]"
                    } else {
                        ""
                    }
                ));
            }
            Some(s) if s.starts_with('-') => return fail("unknown config argument"),
            _ => position.push(&args[i]),
        }
        i += 1;
    }
    if position.len() != 2 {
        return fail("config requires an action and path");
    }
    let Some(action) = position[0]
        .to_str()
        .filter(|s| ["install", "remove", "check"].contains(s))
    else {
        return fail("config action must be install, remove or check");
    };
    let path = absolute(ctx.cwd, Path::new(position[1]));
    let load = || match read(&path, MAX) {
        Ok(v) => Ok(v),
        Err(_) if !path.exists() && !path.is_symlink() => Ok(Vec::new()),
        Err(e) => Err(e),
    };
    let original = match load() {
        Ok(v) => v,
        Err(e) => return fail(format!("{}: {e}", path.display())),
    };
    let result = (|| -> Result<_, String> {
        let s = std::str::from_utf8(&original).map_err(|_| "config is not UTF-8")?;
        let nl = if s.contains("\r\n") { "\r\n" } else { "\n" };
        let lines: Vec<String> = s.split_inclusive('\n').map(str::to_owned).collect();
        if action != "remove" {
            parse(&lines)?;
        }
        let (updated, notes, code) = if name == "codex-hud-config" {
            hud(action, lines, nl, dry)?
        } else {
            usage(action, lines, nl, dry, notify, ctx)?
        };
        let bytes = updated.concat().into_bytes();
        if bytes != original && action != "check" && !dry {
            if path.is_symlink() {
                return Err("refusing to replace a symlinked config".into());
            }
            // The outer transaction serializes config and first-touch backup;
            // inner mutations revalidate exact bytes before publication.
            let outer = path.with_extension("oms-config-transaction");
            let _guard = lock(&outer, ctx.cwd)?;
            if !original.is_empty() {
                let backup = path.with_file_name(format!(
                    "{}.oms-bak",
                    path.file_name().unwrap().to_string_lossy()
                ));
                crate::common::backup(&backup, &original, MAX, false)?;
            }
            mutate(&path, MAX, false, |current| {
                if current != original {
                    return Err("config changed during update".into());
                }
                Ok((bytes, ()))
            })?;
        }
        Ok((notes, code))
    })();
    match result {
        Ok((notes, code)) => (
            code,
            notes
                .iter()
                .map(|n| {
                    format!(
                        "{}: {n}\n",
                        if name == "codex-hud-config" {
                            "codex-hud"
                        } else {
                            "codex-usage"
                        }
                    )
                })
                .collect::<String>()
                .into_bytes(),
            vec![],
        ),
        Err(e) => fail(format!("{}: {e}", path.display())),
    }
}
