//! Append-only candidate bookkeeping, with per-link supersession and explicit
//! conflicts. Scores are never averaged across tools or promoted to evidence.
use crate::{
    Context, Error, Output, Result, V, cli::Options, digest, now, obj, require, safe, storage,
    string,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

fn valid(s: &str, tail: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || tail.as_bytes().contains(&b))
}
fn text(s: &str, max: usize, label: &str) -> Result<()> {
    require(
        s.chars().count() <= max
            && !s
                .chars()
                .any(|c| (c.is_control() && c != '\t') || c == '\u{7f}'),
        format!("{label} must be plain single-line text of at most {max} characters"),
    )
}
fn load(raw: &[u8]) -> Result<(BTreeMap<String, V>, Vec<V>)> {
    let mut candidates = BTreeMap::new();
    let mut results = vec![];
    for row in storage::parse_rows(raw)? {
        require(row.get("id").text().is_some(), "invalid candidate row")?;
        match row.get("type").text() {
            Some("candidate") => {
                candidates.insert(string(&row, "id"), row);
            }
            Some("result") => results.push(row),
            _ => return Err(Error::new(2, "invalid candidate row type")),
        }
    }
    Ok((candidates, results))
}
fn link(row: &V) -> String {
    let mut links = vec![];
    if row.get("run").truth() {
        links.push(format!(
            "run {} ({})",
            string(row, "run"),
            row.get("run_via").text().unwrap_or("?")
        ));
    }
    if row.get("external").truth() {
        links.push(format!("receipt {}", string(row, "external")));
    }
    if links.is_empty() {
        "unlinked".into()
    } else {
        links.join(", ")
    }
}
#[derive(Clone)]
struct Entry {
    row: V,
    group: (String, String, String, String),
    superseded: bool,
    conflict: bool,
}
fn analyse(rows: &[V]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = vec![];
    let mut latest = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let group = (
            string(row, "id"),
            string(row, "tool"),
            string(row, "metric"),
            string(row, "conditions"),
        );
        let key = (
            group.clone(),
            if row.get("run").truth() || row.get("external").truth() {
                format!("{}\0{}", string(row, "run"), string(row, "external"))
            } else {
                format!("unlinked:{i}")
            },
        );
        if let Some(old) = latest.insert(key, i) {
            entries[old].superseded = true;
        }
        entries.push(Entry {
            row: row.clone(),
            group,
            superseded: false,
            conflict: false,
        });
    }
    let mut values: BTreeMap<_, Vec<f64>> = BTreeMap::new();
    for e in entries.iter().filter(|e| !e.superseded) {
        let v = e.row.get("value").py().parse::<f64>().unwrap_or(f64::NAN);
        let seen = values.entry(e.group.clone()).or_default();
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    for e in &mut entries {
        e.conflict = !e.superseded && values.get(&e.group).is_some_and(|v| v.len() > 1);
    }
    entries
}
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    let repo = c.state(c.cwd)?;
    if matches!(o.action(""), "add" | "result") {
        storage::ensure_ignore(&repo, c.interrupted)?;
    }
    let path = std::env::var_os("OMS_CANDIDATES_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.join(".oms/candidates.jsonl"));
    let path = if path.is_absolute() {
        path
    } else {
        c.cwd.join(path)
    };
    let action = o.action("");
    require(
        ["add", "result", "show", "table"].contains(&action),
        "candidate requires add, result, show or table",
    )?;
    let _lock = if matches!(action, "add" | "result") {
        Some(storage::lock(&path, c.interrupted)?)
    } else {
        None
    };
    let raw = storage::read_external(&path, true)?;
    let (candidates, results) = load(&raw)?;
    if action == "show" {
        require(o.words.len() == 2, "show requires one candidate id")?;
        let id = &o.words[1];
        let candidate = candidates
            .get(id)
            .ok_or_else(|| Error::new(2, format!("no such candidate: {id}")))?;
        let entries: Vec<_> = analyse(&results)
            .into_iter()
            .filter(|e| e.group.0 == *id)
            .collect();
        if o.has("json") {
            let rows = entries
                .iter()
                .map(|e| {
                    let mut row = e.row.clone();
                    row.set("link", link(&row));
                    row.set("superseded", e.superseded);
                    row.set("conflict", e.conflict);
                    row
                })
                .collect();
            return Output::json(&obj([
                ("schema", 1usize.into()),
                ("candidate", candidate.clone()),
                ("results", V::Arr(rows)),
            ]));
        }
        let mut out = format!(
            "{}  ({}, added {})\n",
            id,
            string(candidate, "kind"),
            string(candidate, "ts")
        );
        for r in candidate.get("refs").array() {
            out += &format!("  ref {}\n", r.py());
        }
        if candidate.has("smiles_length") && !candidate.has("smiles") {
            out += "  structure string not stored (digest and length only)\n";
        }
        if candidate.get("note").truth() {
            out += &format!("  note: {}\n", string(candidate, "note"));
        }
        if entries.is_empty() {
            out += "no results recorded\n";
        }
        let pairs: BTreeSet<_> = entries
            .iter()
            .map(|e| (e.group.1.clone(), e.group.2.clone()))
            .collect();
        for (tool, metric) in pairs {
            out += &format!("{tool} / {metric}\n");
            let mine: Vec<_> = entries
                .iter()
                .filter(|e| e.group.1 == tool && e.group.2 == metric)
                .collect();
            for e in &mine {
                out += &format!(
                    "  {}{}  {}  {}",
                    e.row.get("value").py(),
                    if e.row.get("unit").truth() {
                        format!(" {}", string(&e.row, "unit"))
                    } else {
                        String::new()
                    },
                    link(&e.row),
                    string(&e.row, "ts")
                );
                if e.row.get("conditions").truth() {
                    out += &format!("  conditions: {}", string(&e.row, "conditions"));
                }
                if e.row.get("direction").truth() {
                    out += &format!("  ({} is better)", string(&e.row, "direction"));
                }
                if e.superseded {
                    out += "  [superseded by a later row on the same link]";
                }
                out.push('\n');
            }
            let bad: BTreeSet<_> = mine
                .iter()
                .filter(|e| e.conflict)
                .map(|e| e.group.3.clone())
                .collect();
            for conditions in bad {
                let seen: Vec<_> = mine
                    .iter()
                    .filter(|e| !e.superseded && e.group.3 == conditions)
                    .map(|e| format!("{} ({})", e.row.get("value").py(), link(&e.row)))
                    .collect();
                out += &format!(
                    "  conflict: same tool, metric and conditions{}, different values: {}; none picked\n",
                    if conditions.is_empty() {
                        String::new()
                    } else {
                        format!(" ({conditions})")
                    },
                    seen.join(" vs ")
                );
            }
        }
        return Ok(Output::text(out));
    }
    if action == "table" {
        return table(o, &candidates, &results, &path);
    }
    require(o.words.len() == 1, "unexpected candidate argument")?;
    let id = o.get("id");
    require(
        valid(id, "._:-", 64),
        "--id must be a stable identifier of at most 64 characters",
    )?;
    let mut row = obj([
        ("schema", 1usize.into()),
        (
            "type",
            if action == "add" {
                "candidate".into()
            } else {
                "result".into()
            },
        ),
        ("id", id.into()),
        ("ts", now().into()),
    ]);
    let mut summary;
    if action == "add" {
        let kind = o.value("kind", "other");
        require(
            ["ligand", "protein", "complex", "pose", "other"].contains(&kind),
            "invalid candidate kind",
        )?;
        row.set("kind", kind);
        let mut refs = o.all("ref");
        for r in &refs {
            let (label, value) = r
                .split_once('=')
                .ok_or_else(|| Error::new(2, "--ref must look like label=value"))?;
            require(
                valid(label, "_.-", 32)
                    && !value.is_empty()
                    && value.chars().count() <= 200
                    && !value.chars().any(char::is_control),
                "invalid candidate reference",
            )?;
        }
        summary = format!(
            "{id}: {kind} candidate recorded (bookkeeping only; structure and scores are not validated)\n"
        );
        if o.has("smiles") {
            let smiles = if o.get("smiles") == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s.trim().to_owned()
            } else {
                o.get("smiles").into()
            };
            text(&smiles, 4096, "--smiles")?;
            require(
                !smiles.is_empty() && smiles.trim() == smiles,
                "--smiles must be one non-empty line",
            )?;
            refs.push(format!("smiles_sha256={}", digest(smiles.as_bytes())));
            row.set("smiles_length", smiles.chars().count());
            if o.has("store-structure") {
                row.set("smiles", smiles);
                summary += &format!(
                    "note: --store-structure keeps the SMILES string in {}; do not share that file if the structure is unpublished\n",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
            } else {
                summary += "note: only the sha256 and length of the SMILES were stored, not the string (unpublished structures should not sit in a shared log; --store-structure keeps it)\n";
            }
        } else {
            require(
                !o.has("store-structure"),
                "--store-structure needs --smiles",
            )?;
        }
        require(refs.len() <= 16, "at most 16 references per candidate")?;
        if !refs.is_empty() {
            row.set("refs", V::Arr(refs.into_iter().map(V::from).collect()));
        }
        text(o.get("note"), 500, "--note")?;
        if !o.get("note").is_empty() {
            row.set("note", o.get("note"));
        }
    } else {
        require(
            candidates.contains_key(id),
            format!("no such candidate: {id} (register it first: oms candidate add --id {id})"),
        )?;
        require(valid(o.get("tool"), "._/@:+-", 64), "invalid --tool")?;
        require(valid(o.get("metric"), "._/-", 64), "invalid --metric")?;
        require(
            regex::Regex::new(r"^[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$")
                .unwrap()
                .is_match(o.get("value")),
            "--value must be a plain decimal number",
        )?;
        let value: f64 = o
            .get("value")
            .parse()
            .map_err(|_| Error::new(2, "--value must be finite"))?;
        require(value.is_finite(), "--value must be finite")?;
        let numeric = if o
            .get("value")
            .trim_start_matches(['+', '-'])
            .bytes()
            .all(|b| b.is_ascii_digit())
            && o.get("value").trim_start_matches(['+', '-']).len() <= 15
        {
            V::Num(
                o.get("value")
                    .parse::<i64>()
                    .map_err(|_| Error::new(2, "invalid numeric value"))?
                    .to_string(),
            )
        } else {
            V::Num(format!("{value:?}"))
        };
        row.set("tool", o.get("tool"));
        row.set("metric", o.get("metric"));
        row.set("value", numeric);
        require(
            o.has("run") || o.has("external") || o.has("unlinked"),
            "a result needs --run or --external; use --unlinked to record without one",
        )?;
        require(
            !o.has("unlinked") || (!o.has("run") && !o.has("external")),
            "--unlinked cannot be combined with --run or --external",
        )?;
        for (k, max) in [("unit", 32), ("conditions", 500)] {
            text(o.get(k), max, k)?;
            if !o.get(k).is_empty() {
                row.set(k, o.get(k));
            }
        }
        if o.has("run") {
            let run = o.get("run");
            require(valid(run, "._:-", 128), "invalid --run")?;
            let runs = std::env::var_os("OMS_RUNS_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| repo.join(".oms/runs"));
            let via = if storage::read_external(&runs.join(run).join("capsule.json"), true)
                .is_ok_and(|v| !v.is_empty())
            {
                "capsule"
            } else {
                let ledger = std::env::var_os("LEDGER")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| repo.join("docs/EXPERIMENTS.jsonl"));
                let raw = storage::read_external(&ledger, true)?;
                require(
                    raw.split(|b| *b == b'\n')
                        .filter_map(|l| V::parse(l).ok())
                        .any(|v| v.get("id").text() == Some(run)),
                    "--run is neither a run capsule nor a run-ledger row id",
                )?;
                "ledger"
            };
            row.set("run", run);
            row.set("run_via", via);
        }
        if o.has("external") {
            require(valid(o.get("external"), "._:-", 128), "invalid --external")?;
            let rows = storage::rows(&repo, Path::new(".oms/external.jsonl"))?;
            require(
                rows.iter()
                    .any(|r| r.get("id").text() == Some(o.get("external"))),
                "--external is not a receipt in .oms/external.jsonl",
            )?;
            row.set("external", o.get("external"));
        }
        require(
            !(o.has("higher-is-better") && o.has("lower-is-better")),
            "directions are mutually exclusive",
        )?;
        if o.has("higher-is-better") {
            row.set("direction", "higher");
        }
        if o.has("lower-is-better") {
            row.set("direction", "lower");
        }
        summary = format!(
            "{id}: {}/{} = {}{} recorded ({})\n",
            o.get("tool"),
            o.get("metric"),
            row.get("value").py(),
            if o.get("unit").is_empty() {
                String::new()
            } else {
                format!(" {}", o.get("unit"))
            },
            if o.has("unlinked") {
                String::from("unlinked")
            } else {
                [
                    if o.has("run") {
                        format!("run {}", o.get("run"))
                    } else {
                        String::new()
                    },
                    if o.has("external") {
                        format!("receipt {}", o.get("external"))
                    } else {
                        String::new()
                    },
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
            }
        );
    }
    let body = row.json(false)?;
    safe(&body)
        .map_err(|_| Error::new(2, "candidate row looks sensitive; nothing was recorded"))?;
    let mut next = raw.clone();
    next.extend(body);
    next.push(b'\n');
    let mut ancestor = path.parent().unwrap();
    while !ancestor.exists() {
        ancestor = ancestor.parent().unwrap();
    }
    let physical = std::fs::canonicalize(ancestor)?;
    storage::replace(
        &physical,
        path.strip_prefix(ancestor).unwrap(),
        if path.exists() { Some(&raw) } else { None },
        &next,
    )?;
    Ok(Output::text(summary))
}

fn table(
    o: &Options,
    candidates: &BTreeMap<String, V>,
    results: &[V],
    path: &Path,
) -> Result<Output> {
    let entries = analyse(results);
    let metrics: BTreeSet<_> = o.all("metric").into_iter().collect();
    let columns: BTreeSet<_> = entries
        .iter()
        .filter(|e| metrics.is_empty() || metrics.contains(&e.group.2))
        .map(|e| (e.group.1.clone(), e.group.2.clone()))
        .collect();
    let mut directions = BTreeMap::new();
    for col in &columns {
        let seen: BTreeSet<_> = entries
            .iter()
            .filter(|e| !e.superseded && (&e.group.1, &e.group.2) == (&col.0, &col.1))
            .filter_map(|e| e.row.get("direction").text())
            .collect();
        directions.insert(
            col.clone(),
            if seen.len() == 1 {
                seen.first().copied()
            } else {
                None
            },
        );
    }
    let mut ranking = obj([
        ("by", V::Null),
        (
            "reason",
            "pass exactly one --metric with a stated direction to order rows".into(),
        ),
    ]);
    let mut rows: Vec<(String, V)> = vec![];
    for id in candidates.keys() {
        let mut cells = V::Obj(BTreeMap::new());
        for col in &columns {
            let mine: Vec<_> = entries
                .iter()
                .filter(|e| {
                    !e.superseded
                        && e.group.0 == *id
                        && (&e.group.1, &e.group.2) == (&col.0, &col.1)
                })
                .collect();
            if mine.is_empty() {
                continue;
            }
            let mut values: Vec<V> = vec![];
            for e in &mine {
                let v = e.row.get("value");
                if !values
                    .iter()
                    .any(|x| x.py().parse::<f64>().ok() == v.py().parse::<f64>().ok())
                {
                    values.push(v.clone());
                }
            }
            let conflict = mine.iter().any(|e| e.conflict);
            let multiple = values.len() > 1 && !conflict;
            cells.set(
                &format!("{}:{}", col.0, col.1),
                obj([
                    ("values", V::Arr(values)),
                    ("conflict", conflict.into()),
                    ("multiple_conditions", multiple.into()),
                    (
                        "unlinked",
                        mine.iter()
                            .any(|e| !e.row.get("run").truth() && !e.row.get("external").truth())
                            .into(),
                    ),
                ]),
            );
        }
        rows.push((id.clone(), cells));
    }
    if metrics.len() == 1 && columns.len() == 1 {
        let col = columns.first().unwrap();
        if let Some(direction) = directions[col] {
            let name = format!("{}:{}", col.0, col.1);
            ranking.set("by", name.clone());
            ranking.set("direction", direction);
            ranking.set("reason", "");
            let score = |v: &V| {
                let vals = v.get(&name).get("values").array();
                if vals.len() == 1 {
                    vals[0].py().parse::<f64>().ok()
                } else {
                    None
                }
            };
            rows.sort_by(|a, b| match (score(&a.1), score(&b.1)) {
                (Some(a), Some(b)) => {
                    if direction == "lower" {
                        a.total_cmp(&b)
                    } else {
                        b.total_cmp(&a)
                    }
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            });
        } else {
            ranking.set("reason",format!("no single direction stated for {} (use --higher-is-better or --lower-is-better when recording results)",col.1));
        }
    } else if metrics.len() == 1 && columns.len() > 1 {
        ranking.set(
            "reason",
            format!(
                "metric {} is reported by several tools; rows are not ordered across tools",
                metrics.first().unwrap()
            ),
        );
    }
    if o.has("json") {
        return Output::json(&obj([
            ("schema", 1usize.into()),
            (
                "columns",
                V::Arr(
                    columns
                        .iter()
                        .map(|(t, m)| {
                            obj([("tool", t.clone().into()), ("metric", m.clone().into())])
                        })
                        .collect(),
                ),
            ),
            (
                "rows",
                V::Arr(
                    rows.iter()
                        .map(|(id, cells)| {
                            obj([("id", id.clone().into()), ("cells", cells.clone())])
                        })
                        .collect(),
                ),
            ),
            ("ranking", ranking),
        ]));
    }
    let format = o.value("format", "md");
    require(
        ["md", "tsv"].contains(&format),
        "--format must be md or tsv",
    )?;
    if candidates.is_empty() {
        return Ok(Output::text(format!(
            "no candidates ({})\n",
            path.display()
        )));
    }
    let names: Vec<_> = columns.iter().map(|(t, m)| format!("{t}:{m}")).collect();
    let header: Vec<_> = std::iter::once("candidate".into())
        .chain(names.iter().cloned())
        .collect();
    let mut out = if format == "tsv" {
        format!("{}\n", header.join("\t"))
    } else {
        format!(
            "| {} |\n|{}|\n",
            header.join(" | "),
            vec!["---"; header.len()].join("|")
        )
    };
    for (id, cells) in rows {
        let mut parts = vec![id];
        for name in &names {
            let cell = cells.get(name);
            if cell == &V::Null {
                parts.push("-".into());
                continue;
            }
            let mut s = cell
                .get("values")
                .array()
                .iter()
                .map(V::py)
                .collect::<Vec<_>>()
                .join("/");
            if cell.get("conflict").truth() {
                s += " !conflict";
            } else if cell.get("multiple_conditions").truth() {
                s += " ~conditions";
            }
            if cell.get("unlinked").truth() {
                s += " ?unlinked";
            }
            parts.push(s);
        }
        out += &if format == "tsv" {
            format!("{}\n", parts.join("\t"))
        } else {
            format!("| {} |\n", parts.join(" | "))
        };
    }
    if format == "md" {
        if ranking.get("by").truth() {
            out += &format!(
                "\nordered by {} ({} is better); rows without exactly one value follow in id order\n",
                string(&ranking, "by"),
                string(&ranking, "direction")
            );
        } else {
            out += &format!("\nnot ranked: {}\n", string(&ranking, "reason"));
        }
        out += "markers: !conflict same conditions, different values (all shown, none picked); ~conditions values differ by conditions; ?unlinked no run or receipt\n";
    }
    Ok(Output::text(out))
}
