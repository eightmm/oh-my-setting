//! One parser for intent adoption and plan decomposition.
use crate::{Error, Result, require, value};
use oms_contract_markdown as markdown;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Contract {
    pub raw: Vec<u8>,
    pub sha256: String,
    pub state: String,
    pub goal: String,
    pub acceptance: String,
    pub acceptance_files: Vec<String>,
    pub allowed: Vec<String>,
    pub criteria: BTreeMap<String, String>,
    pub sections: BTreeMap<String, String>,
}
pub fn section<'a>(text: &'a str, wanted: &str) -> Vec<&'a str> {
    let mut inside = false;
    let mut out = vec![];
    for line in text.lines() {
        if line.starts_with("## ") {
            inside = line.trim_end() == format!("## {wanted}");
            continue;
        }
        if inside {
            out.push(line);
        }
    }
    out
}
pub fn field(text: &str, section_name: &str, label: &str) -> Result<Option<String>> {
    let lines = section(text, section_name);
    let prefix = format!("- {label}:");
    let found = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| line.strip_prefix(&prefix).map(|value| (i, value.trim())))
        .collect::<Vec<_>>();
    require(
        found.len() <= 1,
        format!("PROJECT.md has multiple '- {label}:' fields"),
    )?;
    let Some((index, raw)) = found.first() else {
        return Ok(None);
    };
    for line in &lines[index + 1..] {
        if line.starts_with("- ") {
            break;
        }
        require(
            line.trim().is_empty(),
            format!("{label} must stay on one Markdown line"),
        )?;
    }
    Ok(Some((*raw).into()))
}
fn executable(raw: &str) -> Result<String> {
    require(
        value::clean_text(raw),
        "Required checks contains control or format characters",
    )?;
    let ticks = raw.chars().filter(|c| *c == '`').count();
    let command = if ticks == 0 {
        raw.trim()
    } else {
        require(
            ticks == 2 && raw.starts_with('`') && raw.ends_with('`') && raw.len() > 2,
            "Required checks has unmatched/multiple backticks or backtick substitution",
        )?;
        raw[1..raw.len() - 1].trim()
    };
    require(
        !command.is_empty() && !command.ends_with('\\'),
        "PROJECT.md has no executable verification command",
    )?;
    Ok(command.into())
}
pub fn path(raw: &str) -> Result<String> {
    require(
        !raw.is_empty()
            && value::clean_text(raw)
            && !raw.contains('\\')
            && !raw.starts_with('/')
            && !Regex::new(r"^[A-Za-z]:").unwrap().is_match(raw),
        "Required check files must be normalized repo-relative paths",
    )?;
    let raw = raw.trim_start_matches("./").trim_end_matches('/');
    require(
        !raw.is_empty()
            && raw.len() <= 240
            && raw.split('/').all(|s| !matches!(s, "" | "." | "..")),
        "Required check files must be normalized repo-relative paths of at most 240 bytes",
    )?;
    Ok(raw.into())
}
pub fn parse(raw: &[u8], repo: &Path) -> Result<Contract> {
    require(
        !raw.is_empty() && raw.len() <= 1024 * 1024,
        "PROJECT.md is empty or exceeds 1 MiB",
    )?;
    let text = std::str::from_utf8(raw).map_err(|_| Error::new(2, "PROJECT.md must be UTF-8"))?;
    let state = field(text, "Status", "State")?
        .unwrap_or_default()
        .to_lowercase();
    let goal = ["Project", "Goal"]
        .iter()
        .find_map(|s| {
            section(text, s)
                .iter()
                .find_map(|line| line.strip_prefix("- Goal:").map(|s| s.trim().to_owned()))
        })
        .or_else(|| {
            text.lines()
                .find_map(|line| line.strip_prefix("- Goal:").map(|s| s.trim().to_owned()))
        })
        .unwrap_or_else(|| "see PROJECT.md".into());
    let command = field(text, "Verification", "Required checks")?
        .filter(|s| !s.is_empty())
        .or(field(text, "Commands", "Test")?)
        .unwrap_or_default();
    let acceptance = executable(&command)?;
    let mut files = BTreeSet::new();
    if let Some(list) = field(text, "Verification", "Required check files")? {
        let entries = list
            .split([',', ' ', '\t'])
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        require(
            entries.len() <= 64,
            "Required check files accepts at most 64 paths",
        )?;
        for item in entries {
            files.insert(path(item)?);
        }
    }
    if files.is_empty() {
        let direct = Regex::new(r"^(?:bash|sh)[ \t]+((?:scripts/check|tests/(?:run|check|test|tests))\.sh)(?:[ \t]+[-A-Za-z0-9_./:=]+)*$").unwrap();
        if let Some(capture) = direct.captures(&acceptance) {
            files.insert(capture[1].into());
        } else {
            let standard = [
                r"make(?:[ \t]+(?:test|check))?",
                r"(?:npm|pnpm|yarn|bun)[ \t]+(?:test|run[ \t]+test)",
                r"(?:python3?|python)[ \t]+-m[ \t]+pytest(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
                r"pytest(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
                r"cargo[ \t]+test(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
                r"go[ \t]+test(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
                r"(?:mvn|gradle|\./gradlew)[ \t]+test(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
            ];
            require(
                standard.iter().any(|p| {
                    Regex::new(&format!("^(?:{p})$"))
                        .unwrap()
                        .is_match(&acceptance)
                }),
                "custom acceptance requires '- Required check files:' with every verifier input",
            )?;
        }
    }
    let root = oms_safe_fs::Root::open(repo)
        .map_err(|_| Error::new(2, "cannot open verification root"))?;
    for file in &files {
        root.read_path(
            Path::new(file),
            64 * 1024 * 1024,
            oms_safe_fs::Policy::source(),
        )
        .map_err(|_| {
            Error::new(
                2,
                format!("Required check file is missing, unsafe or not regular: {file}"),
            )
        })?;
    }
    let (body, _) = markdown::sections(text);
    let criteria_text = markdown::section_text(
        &body,
        &[
            "acceptance",
            "done",
            "success",
            "completion",
            "완료",
            "성공",
            "수용",
        ],
    );
    let mut criteria = BTreeMap::new();
    for bullet in markdown::bullet_items(&criteria_text, false) {
        let id = markdown::stable_criterion_id("project", &bullet);
        require(!criteria.contains_key(&id), "duplicate criterion id")?;
        criteria.insert(
            id,
            markdown::strip_criterion_marker(&markdown::normalized_line(&bullet)),
        );
    }
    let scope = markdown::section_text(&body, &["scope", "범위"]);
    let mut allowed = Vec::new();
    if let Some(raw) = text.lines().find_map(|line| line.strip_prefix("- Scope:")) {
        allowed = markdown::scope::normalize_scope_default(raw.trim()).unwrap_or_default();
    }
    for line in scope.lines() {
        if let Some(v) = line.trim().strip_prefix("- ") {
            let v = v
                .strip_prefix("Allowed:")
                .unwrap_or(v)
                .trim()
                .trim_matches('`');
            if let Ok(paths) = markdown::scope::normalize_scope_default(v) {
                for item in paths {
                    if !allowed.contains(&item) {
                        allowed.push(item);
                    }
                }
            }
        }
    }
    // Scope defaults are the explicit machine-readable envelope in contracts
    // whose Scope section also contains ordinary explanatory prose.
    if let Some(v) = text
        .lines()
        .find_map(|line| line.strip_prefix("- Scope defaults:"))
    {
        allowed = markdown::scope::normalize_scope_default(v.trim())
            .map_err(|_| Error::new(2, "unsafe Scope defaults"))?;
    }
    let mut sections = BTreeMap::new();
    for (heading, lines) in &body {
        sections.insert(heading.clone(), lines.join("\n"));
    }
    Ok(Contract {
        raw: raw.into(),
        sha256: value::hash(raw),
        state,
        goal,
        acceptance,
        acceptance_files: files.into_iter().collect(),
        allowed,
        criteria,
        sections,
    })
}
impl Contract {
    pub fn confirmed(&self) -> Result<()> {
        require(
            matches!(self.state.as_str(), "confirmed" | "active"),
            "PROJECT.md is not confirmed; confirm the spec before decomposing",
        )
    }
    pub fn planner_budget(&self) -> Result<()> {
        let text = std::str::from_utf8(&self.raw).unwrap_or("");
        for heading in ["Interface and Data", "Decisions"] {
            require(
                section(text, heading).join("\n").len() <= 4096,
                format!(
                    "PROJECT.md section '{heading}' exceeds the 4096-byte planner budget; compact the section without dropping binding decisions"
                ),
            )?;
        }
        Ok(())
    }
    pub fn warnings(&self) -> Vec<String> {
        let mut out = vec![];
        if self.criteria.is_empty() {
            out.push("warning: spec: no success criteria; acceptance alone does not describe the required behavior".into());
        }
        if !section(
            std::str::from_utf8(&self.raw).unwrap_or(""),
            "Open decisions",
        )
        .join("\n")
        .trim()
        .is_empty()
        {
            out.push("warning: spec: open decisions remain; resolve material choices before applying a proposal".into());
        }
        out
    }
    pub fn roadmap(&self) -> Result<Vec<Value>> {
        let text = std::str::from_utf8(&self.raw).unwrap_or("");
        let (body, _) = markdown::sections(text);
        let roadmap = markdown::section_text(&body, &["roadmap", "로드맵"]);
        let item =
            Regex::new(r"^\s*[-*+]\s+\[([ xX])\]\s*([A-Za-z0-9][A-Za-z0-9._-]{0,39})\s*:\s*(.+)$")
                .unwrap();
        let ids = Regex::new(r"\(([^()]*)\)\s*$").unwrap();
        let mut seen = BTreeSet::new();
        let mut out = vec![];
        for line in roadmap.lines() {
            let Some(m) = item.captures(line) else {
                continue;
            };
            if out.len() >= 40 || !seen.insert(m[2].to_owned()) {
                continue;
            }
            let mut title = m[3].trim().to_owned();
            let mut criteria = vec![];
            if let Some(c) = ids.captures(&title) {
                for name in c[1].split(',').map(str::trim) {
                    if value::id(name, 64)
                        && name.as_bytes()[0].is_ascii_alphanumeric()
                        && !criteria.contains(&name.to_owned())
                        && criteria.len() < 30
                    {
                        criteria.push(name.to_owned());
                    }
                }
                title.truncate(c.get(0).unwrap().start());
                title = title.trim().to_owned();
            }
            let unknown = criteria
                .iter()
                .filter(|id| !self.criteria.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>();
            out.push(json!({"id": &m[2], "checked": &m[1] != " ", "title": markdown::bounded_line(&title, 120), "criteria": criteria, "unknown": unknown}));
        }
        Ok(out)
    }
    pub fn milestone(&self, id: &str) -> Result<Value> {
        let row = self
            .roadmap()?
            .into_iter()
            .find(|r| value::text(r, "id") == id)
            .ok_or_else(|| Error::new(2, "no such milestone in PROJECT.md Roadmap"))?;
        require(
            row["criteria"].as_array().is_some_and(|a| !a.is_empty())
                && row["unknown"].as_array().is_some_and(Vec::is_empty),
            "milestone must list criterion ids present in the contract",
        )?;
        Ok(row)
    }
}
