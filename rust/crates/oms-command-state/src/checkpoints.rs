use crate::json::{list, number, object, string};
use crate::{
    Context, Output, Result, Value, checkpoint, error, json_output, require, storage, strings,
    success,
};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const EXTENSIONS: &[&str] = &[
    ".pt",
    ".pth",
    ".ckpt",
    ".safetensors",
    ".bin",
    ".h5",
    ".keras",
    ".msgpack",
];
const HELP: &str = "Usage: oms run checkpoints [DIR...] [--max N] [--strict] [--json]\n\nRead-only checkpoint audit. Default: checkpoints/ and PROJECT.md - Checkpoints:.\nOnly regular files are counted; symlinks are never followed. No file contents\nare opened. Default --max 2; allowed stems latest and best. Stops at 2000\ndirectories or 50000 checkpoint files. --strict exits 1 for warnings.\n";

fn show(path: &Path, repo: &Path) -> String {
    path.strip_prefix(repo)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn run(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = strings(args)?;
    let mut roots = Vec::new();
    let (mut max, mut strict, mut as_json) = (2usize, false, false);
    let mut repo = ctx.repo.clone();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(success(HELP)),
            "--json" => as_json = true,
            "--strict" => strict = true,
            "--max" | "--repo" => {
                let flag = &args[index];
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| error(2, format!("{flag} requires a value")))?;
                if flag == "--max" {
                    max = value
                        .parse()
                        .map_err(|_| error(2, "--max takes a whole number >= 0"))?;
                } else {
                    repo = ctx
                        .cwd
                        .join(value)
                        .canonicalize()
                        .map_err(|e| error(2, e))?;
                }
            }
            value if value.starts_with('-') => {
                return Err(error(2, format!("unknown argument: {value}")));
            }
            value => roots.push(ctx.cwd.join(value)),
        }
        index += 1;
    }
    let mut notes = Vec::new();
    let mut field = Value::Null;
    if roots.is_empty() {
        roots.push(repo.join("checkpoints"));
        let raw = storage::read(&repo, Path::new("PROJECT.md"), 1024 * 1024, true)?;
        let mut named = Vec::new();
        for line in String::from_utf8_lossy(&raw).lines() {
            if let Some(value) = line.trim().strip_prefix("- Checkpoints:") {
                for value in value
                    .split([',', ';'])
                    .map(|s| s.trim().trim_matches('`').trim())
                    .filter(|s| !s.is_empty())
                {
                    named.push(repo.join(value));
                }
            }
        }
        field = Value::Bool(!named.is_empty());
        if named.is_empty() {
            notes.push(
                "PROJECT.md has no \"- Checkpoints:\" value; scanned checkpoints/ only".to_owned(),
            );
        }
        roots.extend(named);
        roots.retain(|p| match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                notes.push(format!("skipped a symlink: {}", show(p, &repo)));
                false
            }
            Ok(m) => m.is_dir(),
            _ => false,
        });
        if roots.is_empty() {
            notes.push("no checkpoint directory found".into());
        }
    } else {
        for root in &roots {
            let metadata =
                std::fs::symlink_metadata(root).map_err(|_| error(2, "not a directory"))?;
            require(
                !metadata.file_type().is_symlink() && metadata.is_dir(),
                "not a directory, or is a symlink; symlinks are not followed",
            )?;
        }
    }
    let mut stack: Vec<PathBuf> = roots.into_iter().rev().collect();
    let mut seen = BTreeSet::new();
    let (mut dirs, mut files_total, mut bytes_total, mut warnings) = (0, 0usize, 0u64, 0);
    let (mut dirs_hit, mut files_hit) = (false, false);
    let mut runs = Vec::new();
    while let Some(path) = stack.pop() {
        checkpoint(ctx.interrupted)?;
        if dirs >= 2000 {
            dirs_hit = true;
            break;
        }
        dirs += 1;
        let path = path.canonicalize().map_err(|e| error(2, e))?;
        if !seen.insert(path.clone()) {
            continue;
        }
        let entries = match std::fs::read_dir(&path) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mut subdirs = Vec::new();
        let mut files = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                subdirs.push(entry.path());
            } else if kind.is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if EXTENSIONS
                    .iter()
                    .any(|ext| name.to_lowercase().ends_with(ext))
                {
                    if let Ok(metadata) = entry.metadata() {
                        files.push((name, metadata.len()));
                    }
                }
            }
        }
        files.sort();
        if files.len() > 50000 - files_total {
            files.truncate(50000 - files_total);
            files_hit = true;
        }
        files_total += files.len();
        if !files.is_empty() {
            let offenders: Vec<_> = files
                .iter()
                .filter(|(n, _)| {
                    !matches!(
                        Path::new(n)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_lowercase()
                            .as_str(),
                        "latest" | "best"
                    )
                })
                .map(|(n, _)| n.as_str())
                .collect();
            let follows = offenders.is_empty() && files.len() <= max;
            warnings += usize::from(!follows);
            let bytes: u64 = files.iter().map(|(_, n)| n).sum();
            bytes_total += bytes;
            let examples: Vec<_> = if offenders.is_empty() {
                files.iter().take(3).map(|(n, _)| n.as_str()).collect()
            } else {
                offenders.iter().take(3).copied().collect()
            };
            runs.push(object([
                ("path", string(show(&path, &repo))),
                ("files", number(files.len())),
                ("bytes", number(bytes)),
                ("follows_default", Value::Bool(follows)),
                ("examples", list(examples)),
                ("per_epoch_names", Value::Bool(!offenders.is_empty())),
            ]));
        }
        if files_hit {
            break;
        }
        subdirs.sort();
        stack.extend(subdirs.into_iter().rev());
    }
    if dirs_hit {
        notes.push("scan stopped after 2000 directories; later ones were not counted".into());
    }
    if files_hit {
        notes.push("scan stopped after 50000 checkpoint files; later ones were not counted".into());
    }
    let code = i32::from(strict && warnings > 0);
    if as_json {
        return json_output(
            code,
            &object([
                ("schema", number(1)),
                ("kind", string("oms-run-checkpoints")),
                ("max", number(max)),
                ("extensions", list(EXTENSIONS.iter().copied())),
                ("project_checkpoints_field", field),
                (
                    "totals",
                    object([
                        ("run_dirs", number(runs.len())),
                        ("files", number(files_total)),
                        ("bytes", number(bytes_total)),
                        ("warnings", number(warnings)),
                    ]),
                ),
                ("runs", Value::Array(runs)),
                (
                    "limits",
                    object([
                        ("dirs_hit", Value::Bool(dirs_hit)),
                        ("files_hit", Value::Bool(files_hit)),
                    ]),
                ),
                ("notes", list(notes.iter().map(String::as_str))),
                ("deleted", number(0)),
            ]),
        );
    }
    let mut out = String::new();
    for row in &runs {
        out.push_str(&format!("{}: {}  {} checkpoint files  {} bytes{}\n", if row.get("follows_default").truth() { "ok" } else { "warn" },
            row.text("path"), row.get("files").int()?, row.get("bytes").int()?,
            if row.get("follows_default").truth() { "  follows the default (latest + best)" } else { "; the default is latest + best; a project that opted in to keep more can raise --max" }));
    }
    for note in notes {
        out.push_str(&format!("note: {note}\n"));
    }
    out.push_str(&format!("summary: {} run directories, {files_total} checkpoint files, {bytes_total} bytes, {warnings} warnings; nothing was deleted\n", runs.len()));
    Ok((code, out.into_bytes(), Vec::new()))
}
