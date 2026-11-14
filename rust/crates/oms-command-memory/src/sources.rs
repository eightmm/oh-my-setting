use crate::{store, *};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
pub(crate) fn display_os(path: &Path, repo: &Path) -> std::ffi::OsString {
    let home = store::home();
    let mut bases = vec![(repo, ""), (&home, "~/")];
    bases.sort_by_key(|(p, _)| std::cmp::Reverse(p.as_os_str().len()));
    for (base, prefix) in bases {
        if !base.as_os_str().is_empty() {
            if let Ok(rest) = path.strip_prefix(base) {
                if rest.as_os_str().is_empty() {
                    return if prefix.is_empty() {
                        ".".into()
                    } else {
                        prefix.into()
                    };
                }
                let mut out = std::ffi::OsString::from(prefix);
                out.push(rest);
                return out;
            }
        }
    }
    let mut out = std::ffi::OsString::from("<outside home>/");
    out.push(path.file_name().unwrap_or_default());
    out
}
pub(crate) fn display(path: &Path, repo: &Path) -> String {
    os::diagnostic(&display_os(path, repo))
}
pub(crate) fn project_key(repo: &Path) -> String {
    os::points(repo.as_os_str())
        .codepoints()
        .iter()
        .map(|p| {
            char::from_u32(*p)
                .filter(char::is_ascii_alphanumeric)
                .unwrap_or('-')
        })
        .collect()
}
pub(crate) fn claude_index(repo: &Path) -> PathBuf {
    store::home()
        .join(".claude/projects")
        .join(project_key(repo))
        .join("memory/MEMORY.md")
}
fn directory(provider: &str, role: &str, path: &Path, repo: &Path, link: bool) -> Option<Value> {
    if !path.is_dir() {
        return None;
    }
    fn walk(
        path: &Path,
        count: &mut usize,
        bytes: &mut u64,
        newest: &mut Option<i64>,
        seen: &mut usize,
        capped: &mut bool,
    ) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        let mut dirs = Vec::new();
        for e in entries {
            let Ok(info) = fs::metadata(e.path()) else {
                *seen += 1;
                if *seen > 2000 {
                    *capped = true;
                    return;
                }
                continue;
            };
            if info.is_dir() {
                if !e.file_type().is_ok_and(|t| t.is_symlink()) {
                    dirs.push(e.path());
                }
                continue;
            }
            *seen += 1;
            if *seen > 2000 {
                *capped = true;
                return;
            }
            *count += 1;
            *bytes += info.len();
            *newest = Some(newest.map_or(info.mtime(), |v| v.max(info.mtime())));
        }
        for p in dirs {
            walk(&p, count, bytes, newest, seen, capped);
            if *capped {
                return;
            }
        }
    }
    let (mut count, mut bytes, mut newest, mut seen, mut capped) = (0, 0, None, 0, false);
    walk(
        path,
        &mut count,
        &mut bytes,
        &mut newest,
        &mut seen,
        &mut capped,
    );
    let mut row = json!({"provider":provider,"role":role,"path":format!("{}/",display(path,repo)),"kind":"directory","bytes":bytes,"files":count,"modified":newest.map(stamp)});
    if link {
        row["link"] = true.into();
    }
    if capped {
        row["capped"] = true.into();
    }
    Some(row)
}
fn file(provider: &str, role: &str, path: &Path, repo: &Path) -> Option<Value> {
    let info = fs::symlink_metadata(path).ok()?;
    let link = info.file_type().is_symlink();
    let mut row = json!({"provider":provider,"role":role,"path":display(path,repo),"kind":"file"});
    let info = if link {
        row["link"] = true.into();
        match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => {
                row["kind"] = "broken-link".into();
                row["bytes"] = 0.into();
                row["files"] = 0.into();
                row["modified"] = Value::Null;
                return Some(row);
            }
        }
    } else {
        info
    };
    if info.is_dir() {
        return directory(provider, role, path, repo, link);
    }
    if !info.is_file() {
        return None;
    }
    row["bytes"] = info.len().into();
    row["files"] = 1.into();
    row["modified"] = stamp(info.mtime()).into();
    Some(row)
}
pub(crate) fn run(repo: &Path, as_json: bool) -> Result<Output> {
    let home = store::home();
    let codex = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let claude = home.join(".claude");
    let gemini = home.join(".gemini");
    let wanted = vec![
        ("claude", "instructions", claude.join("CLAUDE.md")),
        ("claude", "instructions", claude.join("rules")),
        ("claude", "instructions", repo.join("CLAUDE.md")),
        ("claude", "instructions", repo.join(".claude/CLAUDE.md")),
        ("claude", "instructions", repo.join("CLAUDE.local.md")),
        ("claude", "instructions", repo.join(".claude/rules")),
        (
            "claude",
            "auto-memory",
            claude
                .join("projects")
                .join(project_key(repo))
                .join("memory"),
        ),
        ("claude", "subagent-memory", claude.join("agent-memory")),
        (
            "claude",
            "subagent-memory",
            repo.join(".claude/agent-memory"),
        ),
        (
            "claude",
            "subagent-memory",
            repo.join(".claude/agent-memory-local"),
        ),
        ("codex", "instructions", codex.join("AGENTS.md")),
        ("codex", "instructions", codex.join("AGENTS.override.md")),
        ("codex", "instructions", repo.join("AGENTS.md")),
        ("codex", "instructions", repo.join("AGENTS.override.md")),
        ("codex", "memories", codex.join("memories_v2")),
        ("codex", "memories", codex.join("memories")),
        ("antigravity", "instructions", gemini.join("AGENTS.md")),
        ("antigravity", "instructions", gemini.join("GEMINI.md")),
        ("antigravity", "instructions", repo.join("GEMINI.md")),
        ("oms", "memory", repo.join(".oms/memory")),
    ];
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    let mut labels = Vec::new();
    let mut overrides = Vec::new();
    for (provider, role, path) in &wanted {
        if !seen.insert((*provider, path.clone())) {
            continue;
        }
        if let Some(row) = file(provider, role, path, repo) {
            let mut label = display_os(path, repo);
            if row["kind"] == "directory" {
                label.push("/");
            }
            overrides.push((format!("/sources/{}/path", rows.len()), label.clone()));
            labels.push(label);
            rows.push(row);
        }
    }
    let name = repo.file_name().unwrap_or_default();
    overrides.push(("/repo".into(), name.to_owned()));
    if as_json {
        return Ok((
            0,
            os::json_line(
                &json!({"schema":1,"kind":"oms-memory-sources","repo":"","contents_read":false,"enabled":"not reported","sources":rows}),
                &overrides,
            )?,
            Vec::new(),
        ));
    }
    let mut out = b"memory sources for ".to_vec();
    out.extend(name.as_encoded_bytes());
    out.extend(
        format!(
            ": {} present; names, sizes and counts only (no file is opened)\n",
            rows.len()
        )
        .as_bytes(),
    );
    let mut provider = String::new();
    let width = labels
        .iter()
        .map(|label| os::points(label).codepoints().len())
        .max()
        .unwrap_or(0);
    for (r, label) in rows.into_iter().zip(labels) {
        let p = text(&r, "provider");
        if p != provider {
            out.extend(format!("{p}\n").as_bytes());
            provider = p;
        }
        let mut notes = Vec::new();
        if r["kind"] == "directory" {
            notes.push(format!(
                "{} file(s){}",
                r["files"],
                if r["capped"] == true { "+" } else { "" }
            ));
        }
        if r["link"] == true {
            notes.push(if r["kind"] == "broken-link" {
                "broken link".into()
            } else {
                "link".into()
            });
        }
        let bytes = r["bytes"].as_u64().unwrap_or(0);
        let size = if bytes < 1024 {
            format!("{bytes} B")
        } else if bytes < 1048576 {
            format!("{:.1} KB", bytes as f64 / 1024.)
        } else {
            format!("{:.1} MB", bytes as f64 / 1048576.)
        };
        out.extend(format!("  {:<15}  ", text(&r, "role")).as_bytes());
        out.extend(label.as_encoded_bytes());
        out.extend(vec![
            b' ';
            width.saturating_sub(
                os::points(&label).codepoints().len()
            )
        ]);
        out.extend(
            format!(
                "  {:>9}  {}{}\n",
                size,
                r["modified"].as_str().unwrap_or("-"),
                if notes.is_empty() {
                    String::new()
                } else {
                    format!("  {}", notes.join(", "))
                }
            )
            .as_bytes(),
        );
    }
    out.extend(b"not reported: whether a provider has its memory turned on, and what any file says; the Claude auto-memory location is derived from the repository path\n");
    Ok((0, out, Vec::new()))
}
