//! Pure compatibility helpers used by admission and worker guard diagnostics.
use crate::{Error, Result, V, obj, require, storage, string};
use regex::Regex;
use std::{collections::BTreeSet, path::Path};
fn matches(pattern: &str, text: &str) -> bool {
    Regex::new(pattern)
        .expect("constant pattern")
        .is_match(text)
}
pub fn panel_call_record(relative: &str) -> bool {
    matches(
        r"^artifacts/panel-call-[0-9a-f]{16}(?:-(?:exit|pending))?\.json$",
        relative,
    )
}
pub fn internal_atomic_temp(root: &Path, relative: &str) -> bool {
    let path = Path::new(relative);
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let parent = path.parent().and_then(|p| p.to_str()).unwrap_or("");
    let recognised = match parent {
        "hooks/sessions" | "hooks/panel-activity" => {
            matches(r"^[A-Za-z0-9._-]+\.json\.[a-z0-9_]{8}\.tmp$", name)
        }
        "hooks/relay" => {
            matches(r"^claude\.json\.[a-z0-9_]{8}\.tmp$", name)
                || matches(r"^\.land\.json\.tmp\.[0-9]+\.[0-9]+\.[0-9]+$", name)
        }
        "hooks" => {
            if let Some(date) = name.strip_prefix("state-hint.")
                && chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .is_ok_and(|d| d.format("%Y-%m-%d").to_string() == date)
            {
                return std::fs::symlink_metadata(root.join(path))
                    .is_ok_and(|m| m.is_file() && m.len() == 0);
            }
            matches(r"^\.oms-replace\.[a-z0-9_]{8}$", name)
        }
        "memory" => matches(
            r"^(?:\.oms-replace\.[A-Za-z0-9]{6}|memory\.sqlite3-(?:journal|wal|shm))$",
            name,
        ),
        "work-journal" => matches(r"^index\.sqlite3-(?:journal|wal|shm)$", name),
        "plan" => matches(
            r"^(?:tmp[a-z0-9_]{8}|\.tasks\.[0-9a-f]{64}\.(?:archive|retire-intent)\.json\.tmp)$",
            name,
        ),
        "panel-state/dispatch-logs" => matches(
            r"^(?:call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}\.transcript|\.transcript-[a-z0-9_]{8})$",
            name,
        ),
        _ => false,
    };
    let generic = Regex::new(r"^\.(.+)\.tmp\.[0-9]+(?:\.[0-9a-f]{32})?$").unwrap();
    let recognised=recognised || generic.captures(name).is_some_and(|m|{
        let target=if parent.is_empty(){m[1].to_owned()}else{format!("{parent}/{}",&m[1])};
        [".gitignore","artifacts/index.jsonl"].contains(&target.as_str())
            || matches(r"^artifacts/quarantine/artifact-index-[0-9a-f]{64}\.raw$",&target)
            || matches(r"^hooks/panel-cache/(?:dashboard|attempts|result-records|plan-list|land-status)(?:\.[0-9a-f]{12})?\.json$",&target)
            || matches(r"^artifacts/panel-results/(?:result-[A-Za-z0-9][A-Za-z0-9._-]{0,159}-[0-9a-f]{24}|delivery-[A-Za-z0-9][A-Za-z0-9._-]{0,159}-[0-9a-f]{24}-[0-9a-f]{16}(?:-pending)?)\.json$",&target)
    });
    recognised && std::fs::symlink_metadata(root.join(path)).is_ok_and(|m| m.is_file())
}
pub fn diagnostic_name(raw: &str) -> String {
    let name = raw.replace('\\', "/");
    let sensitive = concat!(
        r"(?i)(^|[._/-])(?:sec",
        "ret|to",
        "ken|creden",
        "tial|pass",
        r"word|\.env)(?:[._/-]|$)"
    );
    if name.starts_with('/')
        || name.split('/').any(|p| p == "..")
        || !matches(r"^[A-Za-z0-9._/@+ -]{1,180}$", &name)
        || matches(sensitive, &name)
    {
        "[redacted path]".into()
    } else {
        name
    }
}
fn lines(path: &Path) -> BTreeSet<String> {
    storage::read_external(path, true)
        .ok()
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
pub fn worker_guard_details(root: &Path, surfaces: &str) -> Vec<String> {
    let surfaces: BTreeSet<_> = surfaces.split(',').map(str::trim).collect();
    let mut out = BTreeSet::new();
    let dirs = if root.join("source").is_dir() {
        vec![(root.to_owned(), ""), (root.join("source"), "source ")]
    } else {
        vec![(root.to_owned(), "")]
    };
    for (dir, prefix) in dirs {
        if surfaces.contains("gitmeta") {
            let before = lines(&dir.join("gitmeta"));
            for (capture, label) in [
                ("gitmeta-admin-now", "admin"),
                ("gitmeta-now", "now"),
                ("guard-report-gitmeta-now", "report"),
            ] {
                for line in before.symmetric_difference(&lines(&dir.join(capture))) {
                    let (kind, value) = line.split_once(' ').unwrap_or((line, ""));
                    let name = if kind == "worktree-dir" {
                        diagnostic_name(value)
                    } else if ["worktree", "worktree-link", "worktree-entry", "module"]
                        .contains(&kind)
                    {
                        diagnostic_name(value.rsplit_once(' ').map_or(value, |(v, _)| v))
                    } else {
                        diagnostic_name(kind)
                    };
                    out.insert(format!("gitmeta {prefix}{name} [{label}]"));
                }
            }
        }
        if surfaces.contains("refs") {
            let before = lines(&dir.join("refs"));
            let mut after = lines(&dir.join("refs-now"));
            if after.is_empty() {
                after = lines(&dir.join("guard-report-refs-now"));
            }
            for line in before.symmetric_difference(&after) {
                let fields: Vec<_> = line.split_whitespace().collect();
                let name = if fields.get(1).is_some_and(|s| s.starts_with("refs/")) {
                    format!("ref {}", diagnostic_name(fields[1]))
                } else if fields.get(1) == Some(&"HEAD-identity") {
                    String::from("HEAD identity")
                } else if fields.first().is_some_and(|s| {
                    (40..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
                }) {
                    String::from("HEAD")
                } else {
                    continue;
                };
                out.insert(format!("refs {prefix}{name}"));
            }
        }
    }
    if surfaces.contains("shared-state") {
        for line in lines(&root.join("state-detail")) {
            let (rel, rest) = line.split_once(' ').unwrap_or((&line, ""));
            let rest: String = rest
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || " ()>-".contains(*c))
                .take(80)
                .collect();
            out.insert(
                format!("shared-state {} {rest}", diagnostic_name(rel))
                    .trim_end()
                    .to_owned(),
            );
        }
    }
    if out.is_empty() {
        return vec!["no path/ref detail available in the preserved manifests".into()];
    }
    let omitted = out.len() > 20;
    let mut rows: Vec<_> = out
        .into_iter()
        .take(if omitted { 19 } else { 20 })
        .collect();
    if omitted {
        rows.push("additional changed paths/refs omitted".into());
    }
    rows
}
pub fn gate_timings(raw: &str, tsv: bool) -> V {
    let mut groups: [Vec<V>; 4] = std::array::from_fn(|_| vec![]);
    let mut slow = vec![];
    let mut slow_stages = vec![];
    let mut exit = V::Null;
    let mut wall = V::Null;
    let patterns = [
        r"^ok: (\S.*?) \((\d+)s\)$",
        r"^check: (\S.*?) FAILED \((\d+)s\)$",
        r"^(?:smoke-timing|slow-test): (\d+)s ([A-Za-z0-9_]+)$",
        r"^slow-stage: (\d+)s (\S.*)$",
        r"^operator-section: (\d+)s ([A-Za-z0-9_./-]+)$",
        r"^operator-shard: (\d+/\d+) (\d+)s$",
        r"^exit=(\d+) wall=(\d+)$",
    ]
    .map(|p| Regex::new(p).unwrap());
    let stamp = Regex::new(r"^\u{feff}?\d{4}-\d\d-\d\dT[0-9:.]+Z ").unwrap();
    for line in raw.lines() {
        if tsv {
            let parts: Vec<_> = line.splitn(3, '\t').collect();
            if parts.len() != 3 || parts[0].parse::<u64>().is_err() {
                continue;
            }
            let index = match parts[1] {
                "stage" => 0,
                "test" => 1,
                "operator-section" => 2,
                "operator-shard" => 3,
                _ => continue,
            };
            let mut row = obj([
                ("name", parts[2].into()),
                ("seconds", V::Num(parts[0].into())),
            ]);
            if index == 0 {
                row.set("status", "ok");
            }
            groups[index].push(row);
            continue;
        }
        let line = stamp.replace(line.trim_start_matches('\u{feff}'), "");
        for (index, re) in patterns.iter().enumerate() {
            if let Some(m) = re.captures(&line) {
                let (name, seconds) = if [0, 1, 5].contains(&index) {
                    (&m[1], &m[2])
                } else {
                    (&m[2], &m[1])
                };
                if index == 6 {
                    exit = V::Num(m[1].into());
                    wall = V::Num(m[2].into());
                    break;
                }
                let mut row = obj([("name", name.into()), ("seconds", V::Num(seconds.into()))]);
                if [0, 1, 3].contains(&index) {
                    row.set("status", if index == 1 { "failed" } else { "ok" });
                }
                match index {
                    0 | 1 => groups[0].push(row),
                    2 => {
                        if line.starts_with("slow-test:") {
                            slow.push(row)
                        } else {
                            groups[1].push(row)
                        }
                    }
                    3 => slow_stages.push(row),
                    4 => groups[2].push(row),
                    5 => groups[3].push(row),
                    _ => {}
                }
                break;
            }
        }
    }
    let seen: BTreeSet<_> = groups[0].iter().map(|r| string(r, "name")).collect();
    groups[0].extend(
        slow_stages
            .into_iter()
            .filter(|r| !seen.contains(&string(r, "name"))),
    );
    if groups[1].is_empty() {
        groups[1] = slow;
    }
    let [stages, tests, sections, shards] = groups;
    obj([
        ("stages", V::Arr(stages)),
        ("tests", V::Arr(tests)),
        ("sections", V::Arr(sections)),
        ("shards", V::Arr(shards)),
        ("exit", exit),
        ("wall", wall),
    ])
}
pub fn recorded_base(receipt: &V) -> Result<Option<&str>> {
    if !receipt.get("push").has("base") {
        return Ok(None);
    }
    let base = receipt
        .get("push")
        .get("base")
        .text()
        .ok_or_else(|| Error::new(2, "invalid receipt push.base"))?;
    require(
        crate::hex(base, 40) || crate::hex(base, 64),
        "invalid receipt push.base",
    )?;
    Ok(Some(base))
}
pub fn range_base<'a>(chain: &'a [String], pushed: &BTreeSet<String>) -> Option<&'a str> {
    chain
        .iter()
        .skip(1)
        .find(|sha| pushed.contains(*sha))
        .or_else(|| chain.get(1))
        .map(String::as_str)
}
