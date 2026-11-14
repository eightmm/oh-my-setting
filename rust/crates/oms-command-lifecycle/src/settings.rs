use crate::{
    config,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::Path};

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
        lines[a + 1..b]
            .iter()
            .map(|l| l.trim_end_matches(['\r', '\n']))
            .collect::<Vec<_>>()
            .join("\n")
            == self.body
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
/// Only exact managed blocks are removed; customized and user values survive.
/// Parsing the candidate catches dotted/inline-table conflicts before writing.
pub(crate) fn codex(tx: &mut Transaction, file: &Path, remove: bool) -> Result<()> {
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
        body: "status_line = [\"model-with-reasoning\", \"context-remaining\", \"five-hour-limit\", \"weekly-limit\", \"git-branch\"]",
    };
    let root = Block::usage("root", "background_terminal_max_timeout = 900000");
    let waits = Block::usage(
        "multi_agent_v2",
        "min_wait_timeout_ms = 120000\ndefault_wait_timeout_ms = 300000",
    );
    if remove {
        let mut spans = Vec::new();
        for block in [&hud, &root, &waits] {
            if let Some((a, b)) = block.span(&lines)? {
                if block.owned(&lines, a, b) {
                    spans.push((a, b));
                    if block.begin == hud.begin {
                        if let Some(table) = expected["tui"].as_object_mut() {
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
                    && lines
                        .get(a + 1)
                        .is_some_and(|l| l.trim_end_matches(['\r', '\n']) == hud.body)
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
            lines.drain(a..=b);
        }
    } else {
        // Validate all markers before making any change, including user blocks.
        for block in [&hud, &root, &waits] {
            block.span(&lines)?;
        }
        if hud.span(&lines)?.is_none() && value["tui"].is_null() {
            if let Some(last) = lines.last_mut() {
                if !last.ends_with('\n') {
                    last.push_str(newline)
                }
            }
            lines.push(format!("{newline}[tui]{newline}"));
            lines.extend(hud.lines(newline));
            expected["tui"] =
                config::toml(format!("[tui]\n{}\n", hud.body).as_bytes())?["tui"].clone();
        } else if hud.span(&lines)?.is_none()
            && value["tui"].is_object()
            && value["tui"].get("status_line").is_none()
        {
            if let Some(i) = header(&lines, "tui") {
                if !lines[i].ends_with('\n') {
                    lines[i].push_str(newline);
                }
                lines.splice(i + 1..i + 1, hud.lines(newline));
                expected["tui"]["status_line"] = config::toml(
                    format!("[tui]\n{}\n", hud.body).as_bytes(),
                )?["tui"]["status_line"]
                    .clone();
            }
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
        {
            if let Some(i) = header(&lines, "features.multi_agent_v2") {
                if !lines[i].ends_with('\n') {
                    lines[i].push_str(newline);
                }
                lines.splice(i + 1..i + 1, waits.lines(newline));
                expected["features"]["multi_agent_v2"]["min_wait_timeout_ms"] = json!(120000);
                expected["features"]["multi_agent_v2"]["default_wait_timeout_ms"] = json!(300000);
            }
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
