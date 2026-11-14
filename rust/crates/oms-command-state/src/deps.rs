//! Dependency cards inspect local metadata without importing packages or fetching URLs.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, error, json, json_output, require, storage, success,
};
use regex::Regex;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) const HELP: &str = "Usage: oms deps show NAME [--repo PATH] [--json]\nRead local lockfiles, declarations and Python distribution metadata. Nothing is imported or fetched.\n";
const ROWS: usize = 40;

pub fn canonical(name: &str) -> String {
    Regex::new("[-_.]+")
        .expect("static expression")
        .replace_all(&name.to_lowercase(), "-")
        .into_owned()
}

fn text(root: &Path, path: &Path) -> Option<String> {
    storage::read(root, path, storage::MAX_FILE, false)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

fn toml_rows(
    data: &str,
    file: &str,
    wanted: &str,
    pins: &mut Vec<Value>,
    declared: &mut Vec<Value>,
) {
    let Ok(table) = oms_toml::parse_str(data) else {
        return;
    };
    if let Some(packages) = table.get("package").and_then(oms_toml::Value::as_array) {
        for row in packages {
            if row
                .get("name")
                .and_then(oms_toml::Value::as_str)
                .is_some_and(|n| canonical(n) == wanted)
                && let Some(version) = row.get("version").and_then(oms_toml::Value::as_str)
            {
                pins.push(object([
                    ("file", string(file)),
                    ("version", string(version)),
                ]));
            }
        }
    }
    python_sections(&table, data, file, wanted, declared);
    if let Some(project) = table.get("project") {
        let mut requirements = Vec::new();
        if let Some(entries) = project
            .get("dependencies")
            .and_then(oms_toml::Value::as_array)
        {
            requirements.extend(entries.iter().filter_map(oms_toml::Value::as_str));
        }
        if let Some(groups) = project
            .get("optional-dependencies")
            .and_then(oms_toml::Value::as_table)
        {
            for entries in groups.values().filter_map(oms_toml::Value::as_array) {
                requirements.extend(entries.iter().filter_map(oms_toml::Value::as_str));
            }
        }
        let re =
            Regex::new(r"^\s*([A-Za-z0-9._-]+)(?:\[[^\]]*\])?\s*(.*)$").expect("static expression");
        for entry in requirements {
            if let Some(m) = re.captures(entry)
                && canonical(&m[1]) == wanted
            {
                declared.push(object([
                    ("file", string(file)),
                    ("spec", string(m[2].trim())),
                    (
                        "line",
                        number(
                            data.lines()
                                .position(|l| l.contains(entry))
                                .map(|n| n + 1)
                                .unwrap_or(1),
                        ),
                    ),
                ]));
            }
        }
    }
}

fn python_sections(
    table: &oms_toml::Table,
    data: &str,
    file: &str,
    wanted: &str,
    declared: &mut Vec<Value>,
) {
    let add = |name: &str, spec: &str, rows: &mut Vec<Value>| {
        if canonical(name) != wanted {
            return;
        }
        let line = data
            .lines()
            .position(|l| l.contains(name))
            .map(|n| n + 1)
            .unwrap_or(1);
        rows.push(object([
            ("file", string(file)),
            ("line", number(line)),
            ("spec", string(spec)),
        ]));
    };
    if let Some(poetry) = table.get("tool").and_then(|v| v.get("poetry")) {
        let mut sections = Vec::new();
        for key in ["dependencies", "dev-dependencies"] {
            if let Some(rows) = poetry.get(key).and_then(oms_toml::Value::as_table) {
                sections.push(rows);
            }
        }
        if let Some(groups) = poetry.get("group").and_then(oms_toml::Value::as_table) {
            for group in groups.values() {
                if let Some(rows) = group
                    .get("dependencies")
                    .and_then(oms_toml::Value::as_table)
                {
                    sections.push(rows);
                }
            }
        }
        for section in sections {
            for (name, value) in section {
                if let Some(spec) = value
                    .as_str()
                    .or_else(|| value.get("version").and_then(oms_toml::Value::as_str))
                {
                    add(name, spec, declared);
                }
            }
        }
    }
    if let Some(groups) = table
        .get("dependency-groups")
        .and_then(oms_toml::Value::as_table)
    {
        let re =
            Regex::new(r"^([A-Za-z0-9._-]+)(?:\[[^\]]*\])?\s*(.*)$").expect("static expression");
        for entries in groups.values().filter_map(oms_toml::Value::as_array) {
            for item in entries.iter().filter_map(oms_toml::Value::as_str) {
                if let Some(m) = re.captures(item) {
                    add(&m[1], &m[2], declared);
                }
            }
        }
    }
}

fn clean_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (scheme, tail) = raw.split_once("://")?;
    if !["http", "https"].contains(&scheme) {
        return None;
    }
    let end = tail.find(['/', '?', '#']).unwrap_or(tail.len());
    let host = tail[..end].rsplit('@').next()?;
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    let path = tail[end..].split(['?', '#']).next().unwrap_or("");
    Some(
        format!("{scheme}://{host}{path}")
            .chars()
            .take(300)
            .collect(),
    )
}

fn environments(repo: &Path) -> Vec<(String, PathBuf)> {
    let mut bases = vec![
        ("project .venv".to_owned(), repo.join(".venv")),
        ("project venv".to_owned(), repo.join("venv")),
    ];
    for name in ["VIRTUAL_ENV", "CONDA_PREFIX"] {
        if let Some(path) = std::env::var_os(name).filter(|p| !p.is_empty()) {
            bases.push((name.into(), PathBuf::from(path)));
        }
    }
    let mut found = Vec::new();
    for (label, base) in bases {
        let Ok(base) = base.canonicalize() else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(base.join("lib")) else {
            continue;
        };
        for entry in entries.take(256).flatten() {
            if !entry.file_name().to_string_lossy().starts_with("python") {
                continue;
            }
            let site = entry.path().join("site-packages");
            if let Ok(site) = site.canonicalize()
                && !found.iter().any(|(_, p)| p == &site)
            {
                found.push((label.clone(), site));
            }
        }
    }
    found
}

pub fn card(repo: &Path, name: &str) -> Result<Value> {
    require(
        Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
            .expect("static expression")
            .is_match(name),
        "invalid dependency name",
    )?;
    let wanted = canonical(name);
    let mut pinned = Vec::new();
    let mut declared = Vec::new();
    let mut installed = Vec::new();
    let mut notes = Vec::new();
    let mut entries = std::fs::read_dir(repo)
        .map_err(|e| error(2, e))?
        .take(4097)
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| error(2, e))?;
    require(
        entries.len() <= 4096,
        "dependency directory entry limit exceeded",
    )?;
    entries.sort_by_key(|e| e.file_name());
    let requirements =
        Regex::new(r"^\s*([A-Za-z0-9._-]+)(?:\[[^\]]*\])?\s*([<>=!~].*?)\s*(?:#.*)?$")
            .expect("static expression");
    let conda = Regex::new(
        r"^\s*-\s*(?:[A-Za-z0-9_.-]+::)?([A-Za-z0-9._-]+)\s*={1,2}\s*([A-Za-z0-9.!+_*-]+)",
    )
    .expect("static expression");
    let pin_re =
        Regex::new(r"^={2,3}\s*([A-Za-z0-9][A-Za-z0-9.!+_*-]*)").expect("static expression");
    for entry in entries {
        let file = entry.file_name().to_string_lossy().into_owned();
        let lower = file.to_lowercase();
        let relevant = [
            "pyproject.toml",
            "uv.lock",
            "poetry.lock",
            "pdm.lock",
            "pipfile.lock",
        ]
        .contains(&lower.as_str())
            || lower.ends_with(".txt")
                && (lower.starts_with("requirements") || lower.starts_with("constraints"))
            || lower.starts_with("environment")
                && (lower.ends_with(".yml") || lower.ends_with(".yaml"));
        if !relevant {
            continue;
        }
        let Some(data) = text(repo, Path::new(&file)) else {
            continue;
        };
        match lower.as_str() {
            "uv.lock" | "poetry.lock" | "pdm.lock" | "pyproject.toml" => {
                toml_rows(&data, &file, &wanted, &mut pinned, &mut declared)
            }
            "pipfile.lock" => {
                if let Ok(value) = json::parse(data.as_bytes()) {
                    for section in ["default", "develop"] {
                        if let Ok(rows) = value.get(section).object() {
                            for (name, row) in rows {
                                if canonical(&String::from_utf8_lossy(name)) == wanted
                                    && row.get("version").str().is_ok()
                                {
                                    pinned.push(object([
                                        ("file", string(&file)),
                                        ("section", string(section)),
                                        (
                                            "version",
                                            string(row.text("version").trim_start_matches('=')),
                                        ),
                                    ]));
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                for (i, line) in data.lines().enumerate() {
                    if let Some(m) = requirements.captures(line)
                        && canonical(&m[1]) == wanted
                    {
                        let spec = m[2].trim();
                        let pin = pin_re.captures(spec).map(|m| m[1].to_owned());
                        if let Some(version) = pin {
                            pinned.push(object([
                                ("file", string(&file)),
                                ("line", number(i + 1)),
                                ("version", string(version.trim_start_matches('='))),
                            ]));
                        } else {
                            declared.push(object([
                                ("file", string(&file)),
                                ("line", number(i + 1)),
                                ("spec", string(spec)),
                            ]));
                        }
                    } else if let Some(m) = conda.captures(line)
                        && canonical(&m[1]) == wanted
                    {
                        pinned.push(object([
                            ("file", string(&file)),
                            ("line", number(i + 1)),
                            ("version", string(&m[2])),
                        ]));
                    }
                }
            }
        }
    }
    for (label, site) in environments(repo) {
        let Ok(entries) = std::fs::read_dir(&site) else {
            continue;
        };
        for entry in entries.take(16384).flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name
                .strip_suffix(".dist-info")
                .and_then(|n| n.rsplit_once('-').map(|p| p.0))
            else {
                continue;
            };
            if canonical(stem) != wanted {
                continue;
            }
            let Some(data) = text(&site, &PathBuf::from(name).join("METADATA")) else {
                continue;
            };
            let mut row = object([
                ("environment", string(&label)),
                ("version", string("")),
                ("summary", string("")),
                ("requires_python", string("")),
                (
                    "location",
                    string(
                        site.strip_prefix(repo)
                            .map(|p| format!("./{}", p.display()))
                            .unwrap_or_else(|_| "external environment".into()),
                    ),
                ),
            ]);
            let mut urls = Vec::new();
            for line in data.lines().take_while(|l| !l.is_empty()) {
                if let Some((key, value)) = line.split_once(':') {
                    let value = value.trim();
                    match key {
                        "Version" => row.set(
                            "version",
                            string(value.chars().take(80).collect::<String>()),
                        )?,
                        "Summary" => row.set(
                            "summary",
                            string(value.chars().take(200).collect::<String>()),
                        )?,
                        "Requires-Python" => row.set(
                            "requires_python",
                            string(value.chars().take(80).collect::<String>()),
                        )?,
                        "Home-page" | "Project-URL" => {
                            let (kind, url) = if key == "Home-page" {
                                (key, value)
                            } else {
                                value.split_once(',').unwrap_or(("", ""))
                            };
                            if let Some(url) = clean_url(url)
                                && urls.len() < 12
                            {
                                urls.push(object([
                                    ("kind", string(kind.trim())),
                                    ("url", string(url)),
                                ]));
                            }
                        }
                        _ => {}
                    }
                }
            }
            row.set("urls", Value::Array(urls))?;
            installed.push(row);
        }
    }
    pinned.truncate(ROWS);
    declared.truncate(ROWS);
    installed.truncate(ROWS);
    let versions: std::collections::BTreeSet<_> =
        pinned.iter().map(|r| r.text("version")).collect();
    if versions.len() > 1 {
        notes.push(string(format!(
            "lockfiles disagree: {}",
            versions.iter().copied().collect::<Vec<_>>().join(", ")
        )));
    }
    if installed.is_empty() {
        notes.push(string("no installed copy found in ./.venv, ./venv, VIRTUAL_ENV or CONDA_PREFIX (Python packages only; nothing was imported)"));
    } else if !versions.is_empty()
        && !installed
            .iter()
            .any(|r| versions.contains(r.text("version")))
    {
        let have: std::collections::BTreeSet<_> =
            installed.iter().map(|r| r.text("version")).collect();
        notes.push(string(format!(
            "installed {} is not a pinned version ({})",
            have.into_iter().collect::<Vec<_>>().join(", "),
            versions.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(object([
        ("schema", number(1)),
        ("kind", string("oms-dependency-card")),
        ("name", string(name)),
        ("canonical", string(wanted)),
        ("pinned", Value::Array(pinned)),
        ("declared", Value::Array(declared)),
        ("installed", Value::Array(installed)),
        ("notes", Value::Array(notes)),
        ("fetched", Value::Bool(false)),
    ]))
}

pub(crate) fn run(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let opts = Options::parse(args, &["repo"], &["json"])?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    require(opts.pos.len() == 2 && opts.pos[0] == "show", HELP)?;
    let value = card(&opts.repo(ctx)?, &opts.pos[1])?;
    let code = i32::from(
        ["pinned", "declared", "installed"]
            .iter()
            .all(|k| value.get(k).array().is_ok_and(|v| v.is_empty())),
    );
    if opts.flag("json") {
        return json_output(code, &value);
    }
    let mut out = format!(
        "dependency {}: what this project pins and has installed (nothing fetched, nothing imported)\n",
        opts.pos[1]
    );
    for group in ["pinned", "declared", "installed"] {
        for row in value.get(group).array()? {
            let line = row
                .get("line")
                .int()
                .ok()
                .map(|n| format!(":{n}"))
                .unwrap_or_default();
            if group == "installed" {
                out.push_str(&format!(
                    "  installed  {}  ({}, {})\n",
                    row.text("version"),
                    row.text("environment"),
                    row.text("location")
                ));
                if !row.text("summary").is_empty() {
                    out.push_str(&format!("    {}\n", row.text("summary")));
                }
            } else {
                out.push_str(&format!(
                    "  {group:10} {}  ({}{line})\n",
                    if group == "pinned" {
                        row.text("version")
                    } else {
                        row.text("spec")
                    },
                    row.text("file")
                ));
            }
            if let Ok(urls) = row.get("urls").array() {
                for url in urls {
                    out.push_str(&format!("    {}: {}\n", url.text("kind"), url.text("url")));
                }
            }
        }
    }
    for note in value.get("notes").strings()? {
        out.push_str(&format!("  note: {note}\n"));
    }
    Ok((code, out.into_bytes(), vec![]))
}
