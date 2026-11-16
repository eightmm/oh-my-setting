//! Prompt-only summaries omit raw datasets, checkpoints, logs and machine paths.
use crate::{Context, Result, V, now, safety, storage, text};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const PRUNE: &[&str] = &[
    ".git",
    ".oms",
    ".venv",
    "node_modules",
    "__pycache__",
    "tmp",
    "backups",
    "data",
    "datasets",
    "outputs",
    "checkpoints",
    "wandb",
    "runs",
];
fn files(repo: &Path, context: &Context<'_>) -> Result<Vec<PathBuf>> {
    let mut pending = vec![(PathBuf::new(), 0)];
    let mut files = Vec::new();
    let mut entries = 0;
    while let Some((relative, depth)) = pending.pop() {
        for entry in std::fs::read_dir(repo.join(&relative))? {
            crate::check(context.interrupted)?;
            entries += 1;
            if entries > 8192 {
                return crate::fail("ML context discovery exceeds the entry bound");
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name
                .to_str()
                .filter(|name| !name.chars().any(char::is_control))
            else {
                continue;
            };
            let kind = entry.file_type()?;
            if kind.is_dir() && depth < 2 && !PRUNE.contains(&name) {
                pending.push((relative.join(name), depth + 1));
            } else if kind.is_file() {
                files.push(relative.join(name));
            }
        }
    }
    files.sort();
    Ok(files)
}
fn selected_lines(output: &mut String, paths: &[PathBuf], accept: impl Fn(&Path) -> bool) {
    let entries = paths
        .iter()
        .filter(|path| accept(path))
        .take(30)
        .collect::<Vec<_>>();
    if entries.is_empty() {
        output.push_str("None found.\n");
    }
    for path in entries {
        output.push_str(&format!("- {}\n", path.display()));
    }
}
pub(crate) fn build(repo: &Path, context: &Context<'_>) -> Result<String> {
    let paths = files(repo, context)?;
    let imported = regex::Regex::new(r"(^|[\s;])(from|import)\s+(torch|tensorflow|jax|sklearn|pytorch_lightning|lightning)([.\s,]|$)").unwrap();
    let dependency = regex::Regex::new(r#"(?m)(^|["'\s-])(torch|tensorflow|jax|scikit-learn|pytorch-lightning|lightning)(\[[^]]+\])?(["'\s]*([<>=~!:]|$)|["'])"#).unwrap();
    let mut ml = paths.iter().any(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                [
                    "train.py",
                    "pretrain.py",
                    "finetune.py",
                    "infer.py",
                    "inference.py",
                ]
                .contains(&name)
            })
    });
    let mut scanned = 0;
    if !ml {
        for path in &paths {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let python = path.extension().is_some_and(|extension| extension == "py");
            let requirements = name.starts_with("requirements") && name.ends_with(".txt")
                || name.starts_with("environment")
                    && (name.ends_with(".yaml") || name.ends_with(".yml"))
                || [
                    "pyproject.toml",
                    "setup.cfg",
                    "setup.py",
                    "Pipfile",
                    "poetry.lock",
                    "uv.lock",
                ]
                .contains(&name);
            if !python && !requirements {
                continue;
            }
            let raw = storage::read(repo, path, false)?;
            scanned += raw.len();
            if scanned > 64 * 1024 * 1024 {
                return crate::fail("ML context discovery exceeds the read bound");
            }
            let body = String::from_utf8_lossy(&raw);
            if python && imported.is_match(&body) || requirements && dependency.is_match(&body) {
                ml = true;
                break;
            }
        }
    }
    if !ml {
        return Ok(String::new());
    }
    let mut output = format!(
        "# ML Agent Context Digest\n\n- generated: {}\n- project-style: ml\n- repo-path: omitted\n- policy: raw data, checkpoints, private environment, training logs and machine paths are omitted\n\n## Project Spec\n\n",
        now()
    );
    let project = storage::read(repo, Path::new("PROJECT.md"), true)?;
    let path_like = regex::Regex::new(r"(?i)(?:[A-Za-z][A-Za-z0-9+.-]*://|(?:^|\s)(?:(?:\.{1,2}|~)?[/\\]|[A-Za-z]:[/\\])\S*|(?:^|\s)(?:data|datasets|splits|private|outputs|checkpoints|runs|wandb|mnt|home|tmp)[/\\]\S*|\.(?:csv|tsv|json|jsonl|parquet|arrow|h5|hdf5|pkl|pt|pth|ckpt|np[yz])(?:\s|$))").unwrap();
    let allowed = [
        "State",
        "Goal",
        "Type",
        "Scope",
        "Non-goals",
        "Success criteria",
        "Required checks",
        "Baseline/metric",
        "Task type",
        "Prediction unit",
        "Inference-time information boundary",
        "Entity IDs/standardization",
        "Source snapshot/provenance",
        "Label/target definition",
        "Label units/direction/censoring/replicates",
        "Negative provenance",
        "Split policy",
        "Split/group keys",
        "Leakage risks",
        "Train-only fitted transforms",
        "Data manifest",
        "Calibration/applicability-domain plan",
    ];
    let mut count = 0;
    for line in String::from_utf8_lossy(&project).lines() {
        let Some((key, value)) = line
            .strip_prefix("- ")
            .and_then(|line| line.split_once(':'))
        else {
            continue;
        };
        if !allowed.contains(&key) {
            continue;
        }
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let value = if path_like.is_match(&value) {
            "[redacted path-or-URL]"
        } else {
            &value
        };
        let line = format!("- {key}: {value}");
        output.push_str(&line.chars().take(240).collect::<String>());
        output.push('\n');
        count += 1;
        if count >= 30 {
            break;
        }
    }
    if project.is_empty() {
        output.push_str("No PROJECT.md found.\n");
    }
    output.push_str("\n## Data Manifests\n\n");
    let names = storage::names(repo, Path::new(".oms/manifests"), ".json")?;
    if names.is_empty() {
        output.push_str("No data manifests found.\n");
    }
    for name in names.iter().take(10) {
        let raw = storage::read(repo, &Path::new(".oms/manifests").join(name), false)?;
        let Ok(row) = V::parse(&raw) else {
            continue;
        };
        let keys = match row.get("leakage_keys") {
            V::Obj(fields) => fields
                .keys()
                .map(|key| String::from_utf8_lossy(key).into_owned())
                .collect::<Vec<_>>(),
            other => other
                .array()
                .iter()
                .filter_map(V::text)
                .map(str::to_owned)
                .collect(),
        };
        let labels = row
            .get("splits")
            .array()
            .iter()
            .map(|split| text(split, "label"))
            .filter(|label| !label.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        let ident = row
            .get("id_column")
            .text()
            .or_else(|| row.get("id_index").text())
            .unwrap_or("unspecified");
        output.push_str(&format!(
            "- name={} id={} leakage_keys={} splits={}\n",
            text(&row, "name").chars().take(80).collect::<String>(),
            ident,
            if keys.is_empty() {
                "none".into()
            } else {
                keys.join(",")
            },
            if labels.is_empty() { "none" } else { &labels }
        ));
    }
    output.push_str("\n## Likely ML Entry Points\n\n");
    selected_lines(&mut output, &paths, |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                [
                    "train.py",
                    "eval.py",
                    "evaluate.py",
                    "infer.py",
                    "inference.py",
                    "model.py",
                    "dataset.py",
                    "dataloader.py",
                ]
                .contains(&name)
            })
    });
    output.push_str("\n## Likely Config Files\n\n");
    selected_lines(&mut output, &paths, |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| ["config.yaml", "config.yml", "pyproject.toml"].contains(&name))
            || path.starts_with("configs")
                && path.extension().is_some_and(|extension| {
                    ["yaml", "yml", "toml"]
                        .iter()
                        .any(|name| extension == *name)
                })
    });
    output.push_str("\n## Verification Contract\n\n");
    if std::fs::symlink_metadata(repo.join("scripts/check.sh"))
        .is_ok_and(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
    {
        output.push_str("- scripts/check.sh exists and is executable\n");
        let check = storage::read(repo, Path::new("scripts/check.sh"), false)?;
        if String::from_utf8_lossy(&check).contains("ml-smoke)") {
            output.push_str("- preferred ML smoke: bash scripts/check.sh ml-smoke\n");
        }
        output.push_str("- fallback fast check: bash scripts/check.sh fast\n");
    } else {
        output.push_str("No executable scripts/check.sh found.\n");
    }
    output.push_str("\n## Recent Experiments\n\n");
    let ledger = storage::read(repo, Path::new("docs/EXPERIMENTS.jsonl"), true)?;
    if ledger.is_empty() {
        output.push_str("No docs/EXPERIMENTS.jsonl ledger found.\n");
    } else if safety::outbound(&ledger).is_err() {
        output
            .push_str("Experiment ledger omitted because it contains sensitive-looking content.\n");
    } else {
        let tail = ledger
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .rev()
            .take(8)
            .collect::<Vec<_>>();
        let rows = tail
            .iter()
            .rev()
            .filter_map(|line| V::parse(line).ok())
            .collect::<Vec<_>>();
        let done = rows
            .iter()
            .filter(|row| row.has("exit") && row.get("exit") != &V::Null)
            .map(|row| text(row, "id"))
            .collect::<std::collections::BTreeSet<_>>();
        for row in &rows {
            if row.get("exit") == &V::Null && done.contains(text(row, "id")) {
                continue;
            }
            let command = row
                .get("cmd")
                .array()
                .iter()
                .filter_map(V::text)
                .collect::<Vec<_>>()
                .join(" ");
            output.push_str(&format!(
                "- {} exit={} duration_s={} sha={} cmd={} note={}\n",
                text(row, "ts"),
                row.get("exit").py(),
                row.get("duration_s").py(),
                text(row, "git_sha"),
                safety::truncate(&command, 120),
                safety::truncate(text(row, "note"), 100)
            ));
        }
    }
    output.push_str("\n## Omitted By Design\n\n- raw datasets, checkpoints, environment files, training logs and machine paths\n");
    let absolute =
        regex::Regex::new(r"(?:^|[^A-Za-z0-9_.:/-])(?:/[^/\s]+/[^\s]+|[A-Za-z]:[\\/][^\s]+)")
            .unwrap();
    if absolute.is_match(&output) || safety::outbound(output.as_bytes()).is_err() {
        return Ok(String::new());
    }
    if output.len() > 5000 {
        output = format!(
            "{}\n\n... ML context truncated at 5000 bytes.\n",
            safety::truncate(&output, 5000)
        );
    }
    Ok(output)
}
