//! Opt-in history retrieval measurement over each fix's parent tree.
use super::{BuildOptions, analytics, build, context, index::Graph, load};
use crate::{CommandResult, Error, Result, cli::Args, util::*};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const DEFINITION: &str =
    "macro file recall@8; all changed non-test/non-doc paths, including additions/deletions";
const KIND: &str = "oms-graph-recall-snapshot";

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "oms-context-benchmark.{}",
            oms_command_plan::storage::random_hex(12)?
        ));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn checked(repo: &Path, args: &[&str], cancel: &AtomicBool) -> Result<Vec<u8>> {
    let (exit, out, _) = command(repo, "git", args, cancel)?;
    if exit != 0 {
        return Err(Error::new("benchmark Git query failed"));
    }
    Ok(out)
}
pub fn history_cases(
    repo: &Path,
    count: usize,
    revision: &str,
    cancel: &AtomicBool,
) -> Result<Vec<Value>> {
    if count == 0 {
        return Err(Error::new("--count must be positive"));
    }
    if revision.is_empty() || revision.starts_with('-') || revision.contains('\0') {
        return Err(Error::new("invalid benchmark revision"));
    }
    let raw = checked(
        repo,
        &["log", revision, "--no-merges", "--format=%H%x00%P%x00%s"],
        cancel,
    )?;
    let text = std::str::from_utf8(&raw).map_err(|e| Error::new(e.to_string()))?;
    let mut cases = vec![];
    for line in text.lines() {
        check(cancel)?;
        let fields: Vec<_> = line.splitn(3, '\0').collect();
        if fields.len() != 3 || !fields[2].starts_with("fix:") {
            continue;
        }
        let Some(parent) = fields[1].split_whitespace().next() else {
            continue;
        };
        let paths = checked(
            repo,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                parent,
                fields[0],
            ],
            cancel,
        )?;
        let paths = std::str::from_utf8(&paths).map_err(|e| Error::new(e.to_string()))?;
        let gold: BTreeSet<_> = paths
            .split('\0')
            .filter(|path| !path.is_empty() && gold_path(path))
            .collect();
        if !gold.is_empty() {
            cases.push(json!({"commit":fields[0],"parent":parent,"task":fields[2][4..].trim(),"gold":gold}));
        }
        if cases.len() >= count {
            break;
        }
    }
    Ok(cases)
}
fn gold_path(path: &str) -> bool {
    !super::build::test_path(path)
        && !path.starts_with("doc/")
        && !path.starts_with("docs/")
        && ![".md", ".rst", ".txt", ".adoc"]
            .iter()
            .any(|suffix| path.to_lowercase().ends_with(suffix))
}
fn export_parent(repo: &Path, revision: &str, target: &Path, cancel: &AtomicBool) -> Result<()> {
    // The tree lists object identities, so no archive links are ever extracted.
    let raw = checked(
        repo,
        &["ls-tree", "-r", "-z", "--full-tree", revision],
        cancel,
    )?;
    for row in raw.split(|b| *b == 0).filter(|row| !row.is_empty()) {
        check(cancel)?;
        let row = std::str::from_utf8(row).map_err(|e| Error::new(e.to_string()))?;
        let (header, path) = row
            .split_once('\t')
            .ok_or_else(|| Error::new("invalid Git tree row"))?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.len() != 3 {
            return Err(Error::new("invalid Git tree row"));
        }
        if !safe_path(path) {
            return Err(Error::new("unsafe archive member"));
        }
        if !["100644", "100755"].contains(&fields[0]) || fields[1] != "blob" {
            continue;
        }
        let bytes = checked(repo, &["cat-file", "blob", fields[2]], cancel)?;
        write(&target.join(path), &bytes)?;
    }
    Ok(())
}
pub fn run(
    repo: &Path,
    count: usize,
    revision: &str,
    cache: &Path,
    cancel: &AtomicBool,
    helper: Option<&Path>,
) -> Result<Value> {
    let cases = history_cases(repo, count, revision, cancel)?;
    if cases.len() != count {
        return Err(Error::new(format!(
            "requested {count} qualifying fixes, found {}",
            cases.len()
        )));
    }
    let mut rows = vec![];
    for case in cases {
        let scratch = Scratch::new()?;
        let tree = scratch.0.join("tree");
        std::fs::create_dir(&tree)?;
        // An empty Git marker prevents discovery from escaping a TMPDIR that
        // happens to live inside another checkout. It contains no Git state.
        std::fs::create_dir(tree.join(".git"))?;
        export_parent(repo, s(&case, "parent"), &tree, cancel)?;
        let cached = cache.join(format!("{}.json", s(&case, "parent")));
        let saved = match read_json(&cached, super::build::GRAPH_LIMIT).ok().flatten() {
            Some(saved) if saved["graph"].is_object() && saved["manifest"].is_object() => saved,
            _ => {
                let extraction = cache.join("extraction");
                let manifest = build(&tree, &extraction, &BuildOptions::default(), cancel, helper)?;
                let graph = load(&extraction)?;
                if graph["revision"] != manifest["revision"] {
                    return Err(Error::new(
                        "benchmark extraction changed before its snapshot",
                    ));
                }
                let saved = json!({"graph":graph,"manifest":manifest});
                write_json(&cached, &saved)?;
                saved
            }
        };
        let index = Graph::new(saved["graph"].clone())?;
        let pack = context::select(
            &tree,
            &index,
            s(&case, "task"),
            12,
            &[],
            "",
            &saved["manifest"]["coverage"],
            cancel,
        )?;
        let mut entries = vec![];
        for id in array(&pack, "entries")
            .iter()
            .take(8)
            .filter_map(Value::as_str)
        {
            if let Some(node) = index.nodes.get(id) {
                let path = s(node, "path");
                if !path.is_empty() && !entries.contains(&path.to_owned()) {
                    entries.push(path.to_owned());
                }
            }
        }
        let gold: BTreeSet<_> = array(&case, "gold")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let hits: BTreeSet<_> = entries
            .iter()
            .map(String::as_str)
            .filter(|path| gold.contains(path))
            .collect();
        let mut row = case.clone();
        row["entries"] = json!(entries);
        row["hits"] = json!(hits);
        row["recall_at_8"] = json!(hits.len() as f64 / gold.len() as f64);
        row["pack_bytes"] = json!(bytes(&pack, false)?.len() - 1);
        rows.push(row);
    }
    let revision = String::from_utf8(checked(repo, &["rev-parse", revision], cancel)?)
        .map_err(|e| Error::new(e.to_string()))?;
    Ok(
        json!({"revision":revision.trim(),"definition":DEFINITION,"metrics":analytics::recall_summary(&rows),"rows":rows}),
    )
}
pub fn snapshot(result: &Value) -> Value {
    json!({"schema":1,"kind":KIND,"revision":result["revision"],"definition":result["definition"],"metrics":result["metrics"],"cases":array(result,"rows").iter().map(|row| json!({"commit":row["commit"],"gold":array(row,"gold").len(),"hits":array(row,"hits").len(),"recall_at_8":row["recall_at_8"],"pack_bytes":row["pack_bytes"]})).collect::<Vec<_>>()})
}
pub fn load_snapshot(path: &Path) -> Result<Value> {
    let value = read_json(path, super::build::GRAPH_LIMIT).map_err(|e| {
        Error::new(format!(
            "unreadable recall snapshot {}: {e}",
            path.display()
        ))
    })?;
    value
        .filter(|v| {
            v["schema"] == 1
                && v["kind"] == KIND
                && v["cases"].is_array()
                && v["metrics"].is_object()
        })
        .ok_or_else(|| Error::new(format!("not a recall snapshot: {}", path.display())))
}
pub(crate) fn dispatch(
    args: &Args,
    repo: &Path,
    cwd: &Path,
    cancel: &AtomicBool,
    helper: Option<&Path>,
) -> Result<CommandResult> {
    let paths = args.list("compare");
    if !paths.is_empty() {
        if !args.text("output").is_empty() || !args.text("snapshot").is_empty() {
            return Err(Error::new(
                "--compare runs nothing and takes no --output or --snapshot",
            ));
        }
        let left = load_snapshot(&at(cwd, &paths[0]))?;
        let right = load_snapshot(&at(cwd, &paths[1]))?;
        let report = analytics::recall_compare(&left, &right)?;
        let exit = if report["comparable"] == true && array(&report, "regressed").is_empty() {
            0
        } else {
            1
        };
        return Ok((exit, bytes(&report, args.flag("pretty"))?, vec![]));
    }
    if args.int("count") < 1 {
        return Err(Error::new("--count must be positive"));
    }
    if args.text("output").is_empty() && args.text("snapshot").is_empty() {
        return Err(Error::new("one of --output or --snapshot is required"));
    }
    let scratch = Scratch::new()?;
    let result = run(
        repo,
        args.int("count") as usize,
        args.text("revision"),
        &scratch.0,
        cancel,
        helper,
    )?;
    for (name, value) in [("output", result.clone()), ("snapshot", snapshot(&result))] {
        if !args.text(name).is_empty() {
            write(&at(cwd, args.text(name)), &bytes(&value, true)?)?;
        }
    }
    let mut output = String::new();
    for (index, row) in array(&result, "rows").iter().enumerate() {
        output.push_str(&format!(
            "{}/{} {} hits={}/{}\n",
            index + 1,
            args.int("count"),
            s(row, "commit").chars().take(10).collect::<String>(),
            array(row, "hits").len(),
            array(row, "gold").len()
        ));
    }
    let mut output = output.into_bytes();
    output.extend(bytes(&result["metrics"], false)?);
    Ok((0, output, vec![]))
}
