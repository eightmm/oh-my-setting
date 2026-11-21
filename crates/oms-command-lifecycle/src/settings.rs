use crate::{
    config,
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::Path};

const STATUS: &str = "status_line = [\"model-with-reasoning\", \"context-remaining\", \"five-hour-limit\", \"weekly-limit\", \"git-branch\", \"current-dir\"]";
// Earlier installs wrote this five-item form; it is still OMS-owned.
const LEGACY_STATUS: &str = "status_line = [\"model-with-reasoning\", \"context-remaining\", \"five-hour-limit\", \"weekly-limit\", \"git-branch\"]";
// The factory bare value is displaced inside the managed block and restored on remove.
const DISPLACED: &str = "# displaced: status_line = [\"model\"]";
const FACTORY: &str = "status_line = [\"model\"]";
fn factory(line: &str) -> bool {
    line.trim_end_matches(['\r', '\n'])
        .split('#')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<String>()
        .replace('\'', "\"")
        == FACTORY.split_whitespace().collect::<String>()
}
/// The single-line factory `status_line` directly under `[tui]`.
fn factory_line(lines: &[String]) -> Option<usize> {
    let mut tui = false;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            tui = header(&lines[i..=i], "tui").is_some();
        } else if tui
            && t.split('=')
                .next()
                .is_some_and(|k| k.trim() == "status_line")
        {
            return factory(l).then_some(i);
        }
    }
    None
}
struct Block<'a> {
    begin: String,
    end: String,
    body: &'a str,
}
impl<'a> Block<'a> {
    fn usage(name: &str, body: &'a str) -> Self {
        Self {
            begin: format!("# >>> oh-my-setting managed Codex usage ({name}) >>>"),
            end: format!("# <<< oh-my-setting managed Codex usage ({name}) <<<"),
            body,
        }
    }
    fn span(&self, lines: &[String]) -> Result<Option<(usize, usize)>> {
        let starts = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim_end_matches(['\r', '\n']) == self.begin)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let ends = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim_end_matches(['\r', '\n']) == self.end)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        match (starts.as_slice(), ends.as_slice()) {
            ([], []) => Ok(None),
            ([a], [b]) if a < b => Ok(Some((*a, *b))),
            _ => r::fail("managed Codex setting markers are incomplete or duplicated"),
        }
    }
    fn owned(&self, lines: &[String], a: usize, b: usize) -> bool {
        let inner = lines[a + 1..b]
            .iter()
            .map(|l| l.trim_end_matches(['\r', '\n']))
            .collect::<Vec<_>>()
            .join("\n");
        inner == self.body
            || self.body == STATUS
                && (inner == LEGACY_STATUS || inner == format!("{DISPLACED}\n{STATUS}"))
    }
    fn lines(&self, newline: &str) -> Vec<String> {
        std::iter::once(self.begin.as_str())
            .chain(self.body.lines())
            .chain(std::iter::once(self.end.as_str()))
            .map(|l| format!("{l}{newline}"))
            .collect()
    }
}
fn header(lines: &[String], table: &str) -> Option<usize> {
    lines.iter().position(|l| {
        let l = l.trim();
        let Some(l) = l
            .strip_prefix('[')
            .and_then(|s| s.split_once(']').map(|(s, _)| s))
        else {
            return false;
        };
        l.split('.')
            .map(|s| s.trim().trim_matches(['\'', '"']))
            .collect::<Vec<_>>()
            .join(".")
            == table
    })
}
fn retired_turn_notifier(paths: &Paths, value: &serde_json::Value, line: &str) -> bool {
    let Some([program, script]) = value.as_array().map(Vec::as_slice) else {
        return false;
    };
    let (Some(program), Some(script)) = (program.as_str(), script.as_str()) else {
        return false;
    };
    let program_path = Path::new(program);
    let python = program_path.file_name().and_then(|name| name.to_str());
    program_path.is_absolute()
        && python.is_some_and(|name| {
            name == "python3"
                || name.strip_prefix("python3.").is_some_and(|version| {
                    !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
                })
        })
        && paths
            .source
            .join("scripts/lib/codex_turn_notify.py")
            .to_str()
            == Some(script)
        && line == format!("notify = [{}, {}]", json!(program), json!(script))
}

/// Only exact managed blocks are removed; customized and user values survive.
/// Parsing the candidate catches dotted/inline-table conflicts before writing.
pub(crate) fn codex(paths: &Paths, tx: &mut Transaction, file: &Path, remove: bool) -> Result<()> {
    if remove && !r::exists(file) {
        return Ok(());
    }
    let raw = if r::exists(file) {
        r::read(file, 8 * 1024 * 1024)?
    } else {
        Vec::new()
    };
    let value = config::toml(&raw)?;
    let mut expected = value.clone();
    let text = std::str::from_utf8(&raw)
        .map_err(|_| r::Error(2, "Codex settings are not UTF-8".into()))?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines = text
        .split_inclusive('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let hud = Block {
        begin: "# >>> oh-my-setting managed Codex HUD >>>".into(),
        end: "# <<< oh-my-setting managed Codex HUD <<<".into(),
        body: STATUS,
    };
    let root = Block::usage("root", "background_terminal_max_timeout = 900000");
    let waits = Block::usage(
        "multi_agent_v2",
        "min_wait_timeout_ms = 120000\ndefault_wait_timeout_ms = 300000",
    );
    let launcher = paths.home.join(".local/bin/oms");
    let notify = Block::usage("notify", "");
    // The shell installer wrote one JSON-encoded notify line at the TOML root.
    // Partial or edited blocks preserve the user's existing value.
    if let Some((a, b)) = notify.span(&lines).ok().flatten()
        && b == a + 2
        && !lines[..a]
            .iter()
            .any(|line| line.trim_start().starts_with('['))
        && let Some(launcher) = launcher.to_str()
    {
        let turn_command = json!([launcher, "codex-turn-notify"]);
        let turn_line = format!("notify = {turn_command}");
        let line = lines[a + 1].trim_end_matches(['\r', '\n']);
        let legacy = retired_turn_notifier(paths, &value["notify"], line);
        if remove && (legacy || value["notify"] == turn_command && line == turn_line) {
            lines.drain(a..=b);
            expected.as_object_mut().unwrap().remove("notify");
        } else if !remove && legacy {
            lines[a + 1] = format!("{turn_line}{newline}");
            expected["notify"] = turn_command;
        }
    }
    if remove {
        let mut spans = Vec::new();
        let mut restore = Vec::new();
        for block in [&hud, &root, &waits] {
            if let Some((a, b)) = block.span(&lines)? {
                if block.owned(&lines, a, b) {
                    spans.push((a, b));
                    if block.begin == hud.begin {
                        if lines
                            .get(a + 1)
                            .is_some_and(|l| l.trim_end_matches(['\r', '\n']) == DISPLACED)
                        {
                            restore.push(a);
                            expected["tui"]["status_line"] = json!(["model"]);
                        } else if let Some(table) = expected["tui"].as_object_mut() {
                            table.remove("status_line");
                        }
                    } else if block.begin == root.begin {
                        expected
                            .as_object_mut()
                            .unwrap()
                            .remove("background_terminal_max_timeout");
                    } else if let Some(table) =
                        expected["features"]["multi_agent_v2"].as_object_mut()
                    {
                        table.remove("min_wait_timeout_ms");
                        table.remove("default_wait_timeout_ms");
                    }
                } else if block.begin == hud.begin
                    && lines.get(a + 1).is_some_and(|l| {
                        [STATUS, LEGACY_STATUS].contains(&l.trim_end_matches(['\r', '\n']))
                    })
                    && lines[a + 2..b]
                        .iter()
                        .find(|l| !l.trim().is_empty())
                        .is_some_and(|l| l.trim_start().starts_with('['))
                    && lines[b + 1..].iter().all(|l| l.trim().is_empty())
                {
                    spans.extend([(a, a + 1), (b, b)]);
                    if let Some(table) = expected["tui"].as_object_mut() {
                        table.remove("status_line");
                    }
                }
            }
        }
        spans.sort_unstable();
        for (a, b) in spans.into_iter().rev() {
            if restore.contains(&a) {
                lines.splice(a..=b, [format!("{FACTORY}{newline}")]);
            } else {
                lines.drain(a..=b);
            }
        }
    } else {
        // Validate all markers before making any change, including user blocks.
        for block in [&hud, &root, &waits] {
            block.span(&lines)?;
        }
        let upgraded = || -> Result<serde_json::Value> {
            Ok(
                config::toml(format!("[tui]\n{STATUS}\n").as_bytes())?["tui"]["status_line"]
                    .clone(),
            )
        };
        if let Some((a, b)) = hud.span(&lines)?
            && b == a + 2
            && lines[a + 1].trim_end_matches(['\r', '\n']) == LEGACY_STATUS
        {
            lines[a + 1] = format!("{STATUS}{newline}");
            expected["tui"]["status_line"] = upgraded()?;
        } else if hud.span(&lines)?.is_none()
            && value["tui"]["status_line"] == json!(["model"])
            && let Some(i) = factory_line(&lines)
        {
            let mut block = hud.lines(newline);
            block.insert(1, format!("{DISPLACED}{newline}"));
            lines.splice(i..=i, block);
            expected["tui"]["status_line"] = upgraded()?;
        } else if hud.span(&lines)?.is_none() && value["tui"].is_null() {
            if let Some(last) = lines.last_mut()
                && !last.ends_with('\n')
            {
                last.push_str(newline)
            }
            lines.push(format!("{newline}[tui]{newline}"));
            lines.extend(hud.lines(newline));
            expected["tui"] =
                config::toml(format!("[tui]\n{}\n", hud.body).as_bytes())?["tui"].clone();
        } else if hud.span(&lines)?.is_none()
            && value["tui"].is_object()
            && value["tui"].get("status_line").is_none()
            && let Some(i) = header(&lines, "tui")
        {
            if !lines[i].ends_with('\n') {
                lines[i].push_str(newline);
            }
            lines.splice(i + 1..i + 1, hud.lines(newline));
            expected["tui"]["status_line"] =
                config::toml(format!("[tui]\n{}\n", hud.body).as_bytes())?["tui"]["status_line"]
                    .clone();
        }
        let table = &value["features"]["multi_agent_v2"];
        if waits.span(&lines)?.is_none()
            && table.is_object()
            && table.get("enabled").is_none_or(|v| v == true)
            && table.get("min_wait_timeout_ms").is_none()
            && table.get("default_wait_timeout_ms").is_none()
            && table
                .get("max_wait_timeout_ms")
                .is_none_or(|v| v.as_i64().is_some_and(|n| n >= 300000))
            && let Some(i) = header(&lines, "features.multi_agent_v2")
        {
            if !lines[i].ends_with('\n') {
                lines[i].push_str(newline);
            }
            lines.splice(i + 1..i + 1, waits.lines(newline));
            expected["features"]["multi_agent_v2"]["min_wait_timeout_ms"] = json!(120000);
            expected["features"]["multi_agent_v2"]["default_wait_timeout_ms"] = json!(300000);
        }
        if root.span(&lines)?.is_none() && value.get("background_terminal_max_timeout").is_none() {
            lines.splice(0..0, root.lines(newline));
            expected["background_terminal_max_timeout"] = json!(900000);
        }
    }
    let body = lines.concat();
    if config::toml(body.as_bytes())? != expected {
        return r::fail(
            "managed Codex settings changed an unrelated value; configuration retained",
        );
    }
    if body.as_bytes() != raw {
        let backup = file.with_file_name(format!(
            "{}.oms-bak",
            file.file_name().unwrap().to_string_lossy()
        ));
        if r::exists(file) && !r::exists(&backup) {
            tx.copy(&backup, file)?;
        }
        let mode = if r::exists(file) {
            std::fs::symlink_metadata(file)?.permissions().mode() & 0o777
        } else {
            0o600
        };
        tx.write(file, body.as_bytes(), mode)?;
    }
    Ok(())
}
