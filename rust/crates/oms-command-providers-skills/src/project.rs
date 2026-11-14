use crate::common::*;
use regex::Regex;
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const PRUNE: &[&str] = &[
    ".git",
    ".venv",
    "node_modules",
    "__pycache__",
    "tmp",
    "backups",
];
const NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md", "GEMINI.md", "PROJECT.md"];
const ML_FIELDS: &[&str] = &[
    "Prediction unit",
    "Inference-time information boundary",
    "Entity IDs/standardization",
    "Source snapshot/provenance",
    "Label/target definition",
    "Label units/direction/censoring/replicates",
    "Split policy",
    "Split/group keys",
    "Leakage risks",
    "Train-only fitted transforms",
    "Data manifest",
    "Calibration/applicability-domain plan",
];
const ML_IGNORES: &[&str] = &[
    "data/",
    "outputs/",
    "checkpoints/",
    "wandb/",
    "runs/",
    ".venv/",
    ".oms/",
];
const BEGIN: &str = "# oh-my-setting:private:begin";
const END: &str = "# oh-my-setting:private:end";

pub(crate) fn style(root: &Path) -> Result<&'static str> {
    let imports = Regex::new(r"(^|[\s;])(from|import)\s+(torch|tensorflow|jax|sklearn|pytorch_lightning|lightning)([.\s,]|$)").map_err(|_| error("invalid import matcher"))?;
    let dependencies = Regex::new(r#"(^|["'\s-])(torch|tensorflow|jax|scikit-learn|pytorch-lightning|lightning)(\[[^]]+\])?(["'\s]*([<>=~!:]|$)|["'])"#).map_err(|_| error("invalid dependency matcher"))?;
    for file in walk(root, 3, PRUNE)? {
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if [
            "train.py",
            "pretrain.py",
            "finetune.py",
            "infer.py",
            "inference.py",
        ]
        .contains(&name)
        {
            return Ok("ml");
        }
        let dependency = (name.starts_with("requirements") && name.ends_with(".txt"))
            || (name.starts_with("environment")
                && (name.ends_with(".yaml") || name.ends_with(".yml")))
            || [
                "pyproject.toml",
                "setup.cfg",
                "setup.py",
                "Pipfile",
                "poetry.lock",
                "uv.lock",
            ]
            .contains(&name);
        if (name.ends_with(".py") || dependency)
            && let Ok(bytes) = read(&file, LIMIT)
        {
            let text = String::from_utf8_lossy(&bytes);
            if (name.ends_with(".py") && text.lines().any(|l| imports.is_match(l)))
                || (dependency && text.lines().any(|l| dependencies.is_match(l)))
            {
                return Ok("ml");
            }
        }
    }
    Ok("general")
}
pub(crate) fn detect(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(args, &[], &[])?;
    args.count(1)?;
    Ok(Output::text(format!(
        "{}\n",
        style(&ctx.path(args.positional.first().map(String::as_str).unwrap_or(".")))?
    )))
}

/// Returns unmanaged text and the selected managed block, refusing broken framing.
pub(crate) fn block(text: &str, begin: &str, end: &str) -> Result<(String, Option<String>)> {
    let mut outside = String::new();
    let mut inside = String::new();
    let mut open = false;
    let mut seen = false;
    for line in text.lines() {
        if line == begin {
            if open || seen {
                return Err(error("nested or duplicate managed block begin"));
            }
            open = true;
            seen = true;
            inside.push_str(line);
            inside.push('\n');
        } else if line == end {
            if !open {
                return Err(error("unmatched managed block end"));
            }
            open = false;
            inside.push_str(line);
            inside.push('\n');
        } else if open {
            inside.push_str(line);
            inside.push('\n');
        } else {
            outside.push_str(line);
            outside.push('\n');
        }
    }
    if open {
        return Err(error("missing managed block end"));
    }
    Ok((outside, if seen { Some(inside) } else { None }))
}
fn markers(style: &str) -> (String, String) {
    (
        format!("<!-- oh-my-setting:{style}:begin -->"),
        format!("<!-- oh-my-setting:{style}:end -->"),
    )
}
fn loader(style: &str, source: &Path) -> String {
    let template = source.join(format!("templates/project-{style}-AGENTS.md"));
    let display = if let Ok(home) = home() {
        template
            .strip_prefix(home)
            .map(|p| format!("~/{}", p.display()))
            .unwrap_or_else(|_| template.display().to_string())
    } else {
        template.display().to_string()
    };
    let (begin, end) = markers(style);
    format!(
        "{begin}\n\n# oh-my-setting Loader\n\nRead `PROJECT.md`, then follow `{display}` for shared `{style}` rules.\nProject rules override global defaults. Resolve only open choices that affect this work.\nClear bounded changes may proceed from local evidence.\n\n{end}\n"
    )
}
fn scaffold(style: &str, slurm: bool) -> String {
    let mut text = format!(
        "# PROJECT.md\n\nProject-specific decisions and verification contract.\n\n## Status\n\n- State: draft\n- Open decisions:\n\n## Project\n\n- Type: {style}\n- Goal:\n- Scope:\n- Non-goals:\n\n## Decisions\n\n- Rationale/invariants:\n- Alternatives ruled out:\n- References (repo-relative paths/sections):\n\n## Commands\n\n- Setup:{}\n- Test:\n- Run:\n- Lint/typecheck:\n\n## Paths\n\n- Data:\n- Config:\n- Outputs/logs:\n- Checkpoints:\n\n",
        if style == "ml" { " uv sync" } else { "" }
    );
    if style == "ml" {
        text.push_str("## ML Scientific Contract\n\n");
        for field in ML_FIELDS {
            text.push_str(&format!("- {field}:\n"));
        }
        text.push_str("- Baseline / primary metric:\n- Validation interval (steps/epochs):\n\n");
    }
    text.push_str("## Verification\n\n- Success criteria:\n- Affected checks:\n- Always-run checks:\n- CI scope/runtime:\n- Required checks:\n\n");
    if style == "ml" {
        text.push_str("## Experiment Pre-Registration\n\n- Hypothesis / predicted outcome:\n- Metric + split:\n- Success / abandon threshold:\n\n");
    }
    if slurm {
        text.push_str(
            "## Slurm\n\n- Partition/account:\n- CPU/GPU/memory/time:\n- Logs/checkpoints:\n\n",
        );
    }
    text.push_str("## Notes\n\n- Gotchas (non-obvious decisions an agent would get wrong):\n- Do not touch:\n- Risks:\n");
    text
}
fn env_flag(name: &str, default: bool) -> Result<bool> {
    match std::env::var(name).as_deref() {
        Ok("1") => Ok(true),
        Ok("0") => Ok(false),
        Err(_) => Ok(default),
        _ => Err(error(format!("{name} must be 0 or 1"))),
    }
}
pub(crate) fn apply(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    use std::os::unix::fs::PermissionsExt;
    let args = Args::parse(args, &[], &["--no-private", "--full-docs", "--python-ci"])?;
    let selected = args
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("general");
    if !["auto", "general", "ml", "slurm"].contains(&selected) {
        return Err(error("style must be auto, general, ml or slurm"));
    }
    let dry = env_flag("OH_MY_SETTING_DRY_RUN", false)?;
    let private = !args.has("--no-private") && env_flag("OH_MY_SETTING_PRIVATE_AGENT_FILES", true)?;
    let full = args.has("--full-docs") || env_flag("OH_MY_SETTING_ML_FULL_DOCS", false)?;
    let root = ctx.path(args.positional.get(1).map(String::as_str).unwrap_or("."));
    let root = root.canonicalize()?;
    let store = Store::open(&root)?;
    let base = if selected == "auto" {
        style(&root)?
    } else {
        selected
    };
    let mut slurm = base == "slurm";
    let files = walk(&root, 3, PRUNE)?;
    slurm |= files.iter().any(|p| {
        p.file_name().and_then(|s| s.to_str()).is_some_and(|n| {
            n.ends_with(".sbatch")
                || ((n.starts_with("slurm") || n.starts_with("submit")) && n.ends_with(".sh"))
        })
    }) || files.iter().any(|p| {
        p != &root.join("scripts/apply-project-template.sh")
            && matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("sh" | "sbatch")
            )
            && read(p, LIMIT).is_ok_and(|s| s.windows(7).any(|w| w == b"#SBATCH"))
    });
    slurm |= base == "ml"
        && (["sbatch", "srun", "sinfo"].iter().any(|bin| installed(bin))
            || std::env::var_os("SLURM_CLUSTER_NAME").is_some()
            || std::env::var_os("SLURM_JOB_ID").is_some());
    let mut names: Vec<String> = if args.positional.len() > 2 {
        args.positional[2..].to_vec()
    } else {
        vec!["AGENTS.md".into(), "CLAUDE.md".into()]
    };
    if args.positional.len() <= 2 && root.join("GEMINI.md").is_file() {
        names.push("GEMINI.md".into());
    }
    let mut destinations = names.clone();
    destinations.push("PROJECT.md".into());
    let mut copies: Vec<(PathBuf, PathBuf, bool)> = vec![];
    if base == "ml" {
        destinations.extend(
            [
                ".gitignore",
                ".oms/skills",
                ".agents/skills",
                ".claude/skills",
            ]
            .map(str::to_owned),
        );
        for name in ["ml_smoke.py", "ml_resume_smoke.py", "check.sh"] {
            copies.push((
                ctx.source.join("templates").join(name),
                root.join("scripts").join(name),
                name == "check.sh",
            ));
        }
        if full && ctx.source.join("templates/ml-docs").is_dir() {
            for path in walk(&ctx.source.join("templates/ml-docs"), 1, &[])? {
                if path.extension().is_some_and(|s| s == "md") {
                    copies.push((
                        path.clone(),
                        root.join("docs").join(
                            path.file_name()
                                .ok_or_else(|| error("missing document name"))?,
                        ),
                        false,
                    ));
                }
            }
        }
    }
    if args.has("--python-ci") {
        for path in ["pyproject.toml", "uv.lock", "tests"] {
            if !root.join(path).exists() {
                return Err(error(
                    "--python-ci needs pyproject.toml, uv.lock and tests/; nothing was changed",
                ));
            }
        }
        let check = root.join("scripts/check.sh");
        if check.exists()
            && read(&check, LIMIT)? != read(&ctx.source.join("templates/check.sh"), LIMIT)?
        {
            return Err(error(
                "--python-ci: scripts/check.sh differs; nothing was changed",
            ));
        }
        let workflows = root.join(".github/workflows");
        if workflows.is_dir()
            && std::fs::read_dir(&workflows)?
                .any(|p| p.is_ok_and(|p| p.file_name() != "python-ci.yml"))
        {
            return Err(error(
                "--python-ci: another workflow exists; nothing was changed",
            ));
        }
        copies.push((ctx.source.join("templates/check.sh"), check, true));
        copies.push((
            ctx.source.join("templates/python-ci.yml"),
            workflows.join("python-ci.yml"),
            false,
        ));
    }
    for (_, destination, _) in &copies {
        destinations.push(
            destination
                .strip_prefix(&root)
                .map_err(|_| error("destination outside project"))?
                .to_string_lossy()
                .into_owned(),
        );
    }
    for destination in &destinations {
        no_links(&root, safe_relative(destination)?)?;
    }
    let mut styles = vec![];
    if base != "slurm" {
        styles.push(base);
    }
    if slurm {
        styles.push("slurm");
    }
    for style in &styles {
        read(
            &ctx.source
                .join(format!("templates/project-{style}-AGENTS.md")),
            LIMIT,
        )?;
    }
    let _guard = lock(&root.join(".oms-template-authority"))?;
    let mut changes = vec![];
    for name in &names {
        let destination = root.join(safe_relative(name)?);
        let raw = optional(&destination)?;
        let mut text =
            String::from_utf8(raw.clone()).map_err(|_| error("rule file must be UTF-8"))?;
        if base != "slurm" {
            for stale in ["general", "ml"] {
                if stale != base {
                    let (b, e) = markers(stale);
                    text = block(&text, &b, &e)?.0;
                }
            }
        }
        for style in &styles {
            let (begin, end) = markers(style);
            text = block(&text, &begin, &end)?.0;
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&loader(style, ctx.source));
        }
        changes.push((destination, raw, text.into_bytes()));
    }
    let mut out = String::new();
    for (path, old, bytes) in changes {
        ctx.check()?;
        if !dry {
            store.write_locked(&path, &bytes, false, Some(&old))?;
        }
        out.push_str(&format!(
            "{}update {}: {}\n",
            if dry { "would " } else { "" },
            path.display(),
            styles.join(", ")
        ));
    }
    let spec = root.join("PROJECT.md");
    if !dry {
        store.write(&spec, scaffold(base, slurm).as_bytes(), true)?;
    }
    for (source, destination, executable) in copies {
        if source.is_file() && !destination.exists() {
            if !dry {
                store.write(&destination, &read(&source, LIMIT)?, true)?;
                if executable {
                    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            out.push_str(&format!(
                "{}create {}\n",
                if dry { "would " } else { "" },
                destination.display()
            ));
        }
    }
    if base == "ml" && !dry {
        let ignore = root.join(".gitignore");
        let _guard = lock(&ignore)?;
        let raw = optional(&ignore)?;
        let mut text =
            String::from_utf8(raw.clone()).map_err(|_| error(".gitignore must be UTF-8"))?;
        for name in ML_IGNORES {
            if !text.lines().any(|l| l == *name) {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(name);
                text.push('\n');
            }
        }
        store.write_locked(&ignore, text.as_bytes(), false, Some(&raw))?;
        for name in ["oms-ml-experiment", "oms-dataset-safety"] {
            let source = ctx
                .source
                .join("templates/project-skills")
                .join(name)
                .join("SKILL.md");
            let legacy = name.strip_prefix("oms-").unwrap_or(name);
            if source.is_file()
                && !root.join(".oms/skills").join(name).exists()
                && !root.join(".oms/skills").join(legacy).exists()
            {
                let args = vec![
                    "--repo".into(),
                    root.as_os_str().to_owned(),
                    "add".into(),
                    "--name".into(),
                    name.into(),
                    "--file".into(),
                    source.into_os_string(),
                ];
                crate::skills::run(&args, ctx)?;
            }
        }
    }
    if private {
        let mut args: Vec<OsString> = vec!["--repo".into(), root.into_os_string(), "apply".into()];
        if dry {
            args.push("--dry-run".into());
        }
        for name in names {
            if !NAMES.contains(&name.as_str()) && !name.chars().any(char::is_whitespace) {
                args.extend(["--path".into(), name.into()]);
            }
        }
        match privacy(&args, ctx) {
            Ok(result) => out.push_str(&String::from_utf8_lossy(&result.out)),
            Err(_) => out.push_str(
                "warn: could not hide agent files from git; run: oms project-private status\n",
            ),
        }
    }
    Ok(Output::text(out))
}
pub(crate) fn remove(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(args, &[], &[])?;
    let style = args.positional.first().map(String::as_str).unwrap_or("all");
    if !["all", "general", "ml", "slurm"].contains(&style) {
        return Err(error("style must be all, general, ml or slurm"));
    }
    let root = ctx
        .path(args.positional.get(1).map(String::as_str).unwrap_or("."))
        .canonicalize()?;
    let store = Store::open(&root)?;
    let names = if args.positional.len() > 2 {
        args.positional[2..].to_vec()
    } else {
        NAMES[..3].iter().map(|s| s.to_string()).collect()
    };
    let dry = env_flag("OH_MY_SETTING_DRY_RUN", false)?;
    let _guard = lock(&root.join(".oms-template-authority"))?;
    let mut changes = vec![];
    for name in names {
        no_links(&root, safe_relative(&name)?)?;
        let path = root.join(name);
        let raw = optional(&path)?;
        if raw.is_empty() {
            continue;
        }
        let mut text =
            String::from_utf8(raw.clone()).map_err(|_| error("rule file must be UTF-8"))?;
        for selected in ["general", "ml", "slurm"] {
            if style == "all" || style == selected {
                let (b, e) = markers(selected);
                text = block(&text, &b, &e)?.0;
            }
        }
        changes.push((path, raw, text));
    }
    let mut out = String::new();
    for (path, raw, text) in changes {
        if !dry {
            store.write_locked(&path, text.as_bytes(), false, Some(&raw))?;
        }
        out.push_str(&format!(
            "{}removed {style} block(s) from {}\n",
            if dry { "would " } else { "" },
            path.display()
        ));
    }
    Ok(Output::text(out))
}
pub(crate) fn privacy(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        args,
        &["--repo", "--path"],
        &["--check", "--untrack", "--dry-run", "--json"],
    )?;
    args.count(1)?;
    let action = args
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if !["status", "apply", "remove"].contains(&action) {
        return Err(error("unknown project-private command"));
    }
    if args.has("--json") && action != "status" {
        return Err(error("--json is only supported by status"));
    }
    if args.has("--untrack") && action != "apply" {
        return Err(error("--untrack is only supported by apply"));
    }
    let root = ctx.repo(args.get("--repo"))?;
    let root_str = root
        .to_str()
        .ok_or_else(|| error("repository must be UTF-8"))?;
    let git = ctx.command(
        "git",
        &["-C", root_str, "rev-parse", "--git-common-dir"],
        10,
    )?;
    if git.code != 0 {
        return Ok(if args.has("--json") {
            Output::json(json!({"schema":1,"repo":root,"git":false,"entries":[]}))
        } else {
            Output::text(format!(
                "project-private: {} is not a git repo yet; re-run after git init\n",
                root.display()
            ))
        });
    }
    let common = root
        .join(String::from_utf8_lossy(&git.out).trim())
        .canonicalize()?;
    let exclude = common.join("info/exclude");
    let _guard = lock(&exclude)?;
    let raw = optional(&exclude)?;
    let text = String::from_utf8(raw.clone()).map_err(|_| error("exclude file must be UTF-8"))?;
    let (outside, managed) = block(&text, BEGIN, END)?;
    let mut entries: Vec<String> = managed
        .as_deref()
        .unwrap_or("")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(str::to_owned)
        .collect();
    let mut names: Vec<String> = NAMES.iter().map(|s| s.to_string()).collect();
    for value in args.all("--path") {
        safe_relative(value)?;
        if value.contains("..") || value.chars().any(char::is_whitespace) {
            return Err(error("--path must be relative without whitespace or '..'"));
        }
        if !names.iter().any(|n| n == value) {
            names.push(value.into());
        }
    }
    let dry = args.has("--dry-run") || env_flag("OH_MY_SETTING_DRY_RUN", false)?;
    let mut rows = vec![];
    let mut exposed = 0;
    for name in &names {
        let tracked = ctx
            .command("git", &["-C", root_str, "ls-files", "--", name], 10)?
            .out;
        let state = if !tracked.is_empty() {
            "tracked"
        } else if entries.contains(name) {
            if root.join(name).exists() {
                "hidden"
            } else {
                "reserved"
            }
        } else if root.join(name).exists() {
            "exposed"
        } else {
            "absent"
        };
        if state == "exposed" {
            exposed += 1;
        }
        rows.push(json!({"path":name,"state":state}));
        if action == "apply" {
            if state == "tracked" && !args.has("--untrack") {
                continue;
            }
            if state == "tracked"
                && !dry
                && ctx
                    .command(
                        "git",
                        &["-C", root_str, "rm", "--cached", "--quiet", "--", name],
                        10,
                    )?
                    .code
                    != 0
            {
                return Err(error("git rm --cached failed"));
            }
            if !entries.contains(name) {
                entries.push(name.clone());
            }
        }
    }
    if action == "status" {
        let mut out = if args.has("--json") {
            Output::json(
                json!({"schema":1,"repo":root,"git":true,"exclude_file":exclude,"block":if managed.is_some(){"present"}else{"absent"},"exposed":exposed,"entries":rows}),
            )
        } else {
            Output::text(format!(
                "{}project-private: {}\n",
                rows.iter()
                    .map(|r| format!(
                        "{:<9} {}\n",
                        r["state"].as_str().unwrap_or("unknown"),
                        r["path"].as_str().unwrap_or("")
                    ))
                    .collect::<String>(),
                if exposed > 0 {
                    "agent file(s) visible to git; run: oms project-private apply"
                } else {
                    "no agent files exposed"
                }
            ))
        };
        if args.has("--check") && exposed > 0 {
            out.code = 1;
        }
        return Ok(out);
    }
    let mut updated = outside.trim_end_matches('\n').to_owned();
    if action == "apply" && !entries.is_empty() {
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(&format!("{BEGIN}\n# Local-only agent harness files (oms project-private). Not committed.\n{}\n{END}\n", entries.join("\n")));
    } else if !updated.is_empty() {
        updated.push('\n');
    }
    if !dry {
        write_locked(&exclude, updated.as_bytes(), false, Some(&raw))?;
    }
    Ok(Output::text(format!(
        "{}project-private: {} managed block in {}\n",
        if dry { "would " } else { "" },
        if action == "apply" {
            "updated"
        } else {
            "removed"
        },
        exclude.display()
    )))
}
pub(crate) fn field<'a>(text: &'a str, name: &str) -> &'a str {
    let prefix = format!("- {name}:");
    text.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(str::trim)
        .unwrap_or("")
}
pub(crate) fn doctor(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        args,
        &[],
        &[
            "--strict",
            "--contract",
            "--budget",
            "--prompts",
            "--team",
            "--json",
        ],
    )?;
    args.count(1)?;
    let modes: Vec<_> = ["--contract", "--budget", "--prompts", "--team"]
        .into_iter()
        .filter(|s| args.has(s))
        .collect();
    if modes.len() > 1 {
        return Err(error(
            "--budget, --prompts, --contract and --team are separate reports",
        ));
    }
    if args.has("--json") && !args.has("--budget") && !args.has("--prompts") {
        return Err(error("--json is only for --budget or --prompts"));
    }
    let root = ctx
        .path(
            args.positional
                .first()
                .map(String::as_str)
                .unwrap_or(if args.has("--prompts") {
                    ctx.source
                        .to_str()
                        .ok_or_else(|| error("source path must be UTF-8"))?
                } else {
                    "."
                }),
        )
        .canonicalize()?;
    if !modes.is_empty() {
        return crate::reports::project(&args, &root, ctx);
    }
    let mut messages = vec![];
    let mut failed = false;
    let mut warned = false;
    let mut directories = BTreeSet::from([root.clone()]);
    for path in walk(
        &root,
        6,
        &[
            ".git",
            "node_modules",
            ".oms",
            ".venv",
            "__pycache__",
            ".cache",
        ],
    )? {
        if NAMES[..3].contains(&path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
            && let Some(parent) = path.parent()
        {
            directories.insert(parent.to_path_buf());
        }
    }
    let mut found = false;
    for directory in directories {
        let mut views = vec![];
        for name in &NAMES[..3] {
            views.push((
                *name,
                String::from_utf8_lossy(&optional(&directory.join(name))?).into_owned(),
            ));
        }
        for style in ["general", "ml", "slurm"] {
            let (b, e) = markers(style);
            let blocks: Vec<_> = views
                .iter()
                .map(|(_, text)| block(text, &b, &e).map(|(_, b)| b))
                .collect::<Result<_>>()?;
            if !blocks.iter().any(Option::is_some) {
                continue;
            }
            found = true;
            for (index, (name, _)) in views.iter().enumerate() {
                if *name == "GEMINI.md" && !directory.join(name).exists() {
                    continue;
                }
                if blocks[index].is_none() {
                    failed = true;
                    messages.push(format!(
                        "fail: {style} block missing in {}",
                        directory.join(name).display()
                    ));
                } else if blocks[index].as_ref() != Some(&loader(style, ctx.source)) {
                    failed = true;
                    messages.push(format!(
                        "fail: {style} block is stale in {}",
                        directory.join(name).display()
                    ));
                } else {
                    messages.push(format!(
                        "ok: {style} block matches current template in {}",
                        directory.join(name).display()
                    ));
                }
                if index > 0 && blocks[0].is_some() && blocks[index] != blocks[0] {
                    failed = true;
                    messages.push(format!(
                        "fail: {style} block differs between AGENTS.md and {name}"
                    ));
                }
            }
        }
    }
    if !found {
        failed = true;
        messages.push("fail: no oh-my-setting managed blocks found".into());
    }
    let spec = optional(&root.join("PROJECT.md"))?;
    if spec.is_empty() {
        failed = true;
        messages.push("fail: PROJECT.md missing".into());
    } else {
        let text = String::from_utf8_lossy(&spec);
        let state = field(&text, "State");
        if !matches!(state, "confirmed" | "active") {
            warned = true;
            messages.push(format!(
                "warn: PROJECT.md State is {}",
                if state.is_empty() { "missing" } else { state }
            ));
        } else {
            if ["Setup", "Test", "Run"]
                .iter()
                .all(|f| field(&text, f).is_empty())
            {
                warned = true;
                messages.push("warn: PROJECT.md Commands are empty".into());
            }
            if field(&text, "Success criteria").is_empty() {
                warned = true;
                messages.push("warn: PROJECT.md Success criteria is empty".into());
            }
        }
    }
    let private = privacy(
        &[
            "--repo".into(),
            root.into_os_string(),
            "status".into(),
            "--check".into(),
        ],
        ctx,
    )?;
    if private.code != 0 {
        warned = true;
        messages.push("warn: agent files visible to git; run: oms project-private apply".into());
    }
    let bad = failed || (warned && args.has("--strict"));
    messages.push(
        if bad {
            "project-doctor: failed"
        } else if warned {
            "project-doctor: ok (with warnings)"
        } else {
            "project-doctor: ok"
        }
        .into(),
    );
    let mut out = Output::text(format!("{}\n", messages.join("\n")));
    out.code = i32::from(bad);
    Ok(out)
}
pub(crate) fn init(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(args, &["--repo"], &["--no-private"])?;
    args.count(0)?;
    let root = ctx.repo(args.get("--repo"))?;
    let store = Store::open(&root)?;
    no_links(&root, Path::new(".oms/.gitignore"))?;
    store.write(&root.join(".oms/.gitignore"), b"*\n", true)?;
    let memory_args = vec!["--repo".into(), root.as_os_str().to_owned(), "init".into()];
    let (code, _, err) =
        oms_command_memory::run_with_source(&memory_args, ctx.cwd, ctx.source, ctx.interrupted);
    if code != 0 {
        return Err(Error {
            code,
            message: String::from_utf8_lossy(&err).into_owned(),
        });
    }
    if !args.has("--no-private") && env_flag("OH_MY_SETTING_PRIVATE_AGENT_FILES", true)? {
        privacy(
            &["--repo".into(), root.as_os_str().to_owned(), "apply".into()],
            ctx,
        )?;
    }
    let selected = style(&root)?;
    Ok(Output::text(format!(
        "oms init: {} (style={selected})\n\nNext actions:\n- Run 'oms state' to see task/plan/board/runs at a glance.\n- Check project rules with 'oms project-doctor .'; apply 'oms apply-project-template {selected} .' when missing.\n- Confirm PROJECT.md before broad work.\n- Use 'oms agent-task context' to resume an active task.\n- Distill verified repo lessons into project skills: 'oms skill-forge add --name oms-NAME'.\n",
        root.display()
    )))
}
