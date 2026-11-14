use crate::common::*;
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) fn name(value: &str, prefixed: bool) -> Result<&str> {
    if value.is_empty()
        || !value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || !value
            .bytes()
            .last()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(error("skill names are lowercase kebab-case"));
    }
    if prefixed && !value.starts_with("oms-") {
        return Err(error(
            "harness-forged skills carry the oms- prefix (the frontmatter name must match)",
        ));
    }
    Ok(value)
}
pub(crate) fn frontmatter(text: &str) -> Result<BTreeMap<String, String>> {
    let lines: Vec<_> = text.lines().collect();
    if lines.first().map(|s| s.trim()) != Some("---") {
        return Err(error("missing frontmatter"));
    }
    let end = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, s)| **s == "---")
        .map(|(n, _)| n)
        .ok_or_else(|| error("unterminated frontmatter"))?;
    let mut meta = BTreeMap::new();
    let mut current = String::new();
    for line in &lines[1..end] {
        if !line.starts_with([' ', '\t'])
            && let Some((field, value)) = line.split_once(':')
        {
            if !field
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                continue;
            }
            current = field.into();
            meta.insert(
                current.clone(),
                value.trim().trim_matches(['\'', '"']).to_owned(),
            );
        } else if line.starts_with([' ', '\t'])
            && current == "metadata"
            && let Some((field, value)) = line.trim().split_once(':')
        {
            meta.insert(
                format!("metadata.{}", field.trim()),
                value.trim().to_owned(),
            );
        } else if line.starts_with([' ', '\t'])
            && ["description", "when_to_use"].contains(&current.as_str())
        {
            let value = meta.entry(current.clone()).or_insert_with(String::new);
            if [">", "|", ">-", "|-"].contains(&value.as_str()) {
                value.clear();
            }
            if !value.is_empty() {
                value.push(' ');
            }
            value.push_str(line.trim());
        }
    }
    Ok(meta)
}
pub(crate) fn validate(
    raw: &[u8],
    directory: &str,
    strict: bool,
) -> Result<BTreeMap<String, String>> {
    if raw.len() > 256 * 1024 {
        return Err(error("SKILL.md exceeds the 256 KiB input budget"));
    }
    let text = std::str::from_utf8(raw).map_err(|_| error("SKILL.md is not UTF-8"))?;
    let meta = frontmatter(text)?;
    name(directory, false)?;
    if meta.get("name").map(String::as_str) != Some(directory) {
        return Err(error("name must match directory"));
    }
    let description = meta.get("description").map(String::as_str).unwrap_or("");
    if description.chars().count() < 40 {
        return Err(error("description too thin to route on"));
    }
    if text.lines().count() > 500 {
        return Err(error("over the 500-line body budget"));
    }
    for field in ["verify", "metadata.verify"] {
        if let Some(value) = meta.get(field)
            && !shlex::split(value).is_some_and(|v| !v.is_empty())
        {
            return Err(error("verify is not a parseable command"));
        }
    }
    if strict {
        if directory.len() > 64 || directory.contains("--") {
            return Err(error("name must be 1-64 chars with no consecutive hyphens"));
        }
        if description.chars().count() > 1024 {
            return Err(error("description exceeds the 1024-character budget"));
        }
        if meta
            .get("compatibility")
            .is_some_and(|s| s.chars().count() > 500)
        {
            return Err(error("compatibility exceeds the 500-character budget"));
        }
        for field in meta.keys() {
            if !field.starts_with("metadata.")
                && ![
                    "name",
                    "description",
                    "license",
                    "compatibility",
                    "metadata",
                    "allowed-tools",
                ]
                .contains(&field.as_str())
            {
                return Err(error(
                    "non-portable frontmatter field; put extensions under metadata",
                ));
            }
        }
    }
    if !clean(raw) {
        return Err(error(
            "sensitive content in project skill; scrub it before adding standing context",
        ));
    }
    Ok(meta)
}
pub(crate) fn entries(root: &Path) -> Result<Vec<(String, Vec<u8>, bool)>> {
    use std::os::unix::fs::MetadataExt;
    let mut result = vec![];
    let mut total = 0;
    fn visit(
        root: &Path,
        path: &Path,
        result: &mut Vec<(String, Vec<u8>, bool)>,
        total: &mut usize,
        depth: usize,
        store: &Store,
    ) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        if depth > 64 {
            return Err(error("bundle directory depth exceeds budget"));
        }
        let mut children = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|e| e.file_name());
        if children.len() > 288 {
            return Err(error("bundle directory has too many entries"));
        }
        for child in children {
            let file = child.path();
            let relative = file
                .strip_prefix(root)
                .map_err(|_| error("bundle outside root"))?
                .to_str()
                .ok_or_else(|| error("bundle paths must be UTF-8"))?
                .to_owned();
            let leaf = child.file_name();
            let leaf = leaf.to_str().ok_or_else(|| error("invalid bundle name"))?;
            if leaf == ".git" || leaf.starts_with(".oms-") {
                continue;
            }
            if relative.len() > 512 || relative.contains('\\') {
                return Err(error("bundle path exceeds budget or is unsafe"));
            }
            let lowered = relative.to_ascii_lowercase();
            if lowered.split('/').any(|p| {
                [
                    ".aws", ".ssh", ".env", ".netrc", ".npmrc", ".pypirc", ".pgpass",
                ]
                .contains(&p)
                    || p.starts_with(".env.")
                    || p.contains("credential")
                    || p.contains("secret")
                    || [".key", ".pem", ".p8", ".p12", ".pfx"]
                        .iter()
                        .any(|s| p.ends_with(s))
                    || ["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"]
                        .iter()
                        .any(|s| p.starts_with(s))
            }) {
                return Err(error("bundle contains a sensitive path"));
            }
            let info = std::fs::symlink_metadata(&file)?;
            if info.file_type().is_symlink() {
                return Err(error("symlinks are not allowed in project skills"));
            }
            if info.is_dir() {
                visit(root, &file, result, total, depth + 1, store)?;
                continue;
            }
            if !info.is_file() || info.nlink() != 1 {
                return Err(error("bundle entries must be single-link regular files"));
            }
            let bytes = store.read(&file, 256 * 1024)?;
            *total += bytes.len();
            if *total > 2 * 1024 * 1024 || result.len() >= 256 {
                return Err(error("bundle exceeds the 2 MiB or 256-file budget"));
            }
            if !clean(&bytes) {
                return Err(error("bundle contains sensitive-looking content"));
            }
            result.push((relative, bytes, info.mode() & 0o100 != 0));
        }
        Ok(())
    }
    let info = std::fs::symlink_metadata(root)?;
    if !info.is_dir() || info.mode() & 0o170000 != 0o040000 {
        return Err(error("bundle source must be a real directory"));
    }
    let store = Store::open(root)?;
    visit(root, root, &mut result, &mut total, 0, &store)?;
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}
pub(crate) fn bundle_digest(entries: &[(String, Vec<u8>, bool)]) -> String {
    let mut hash = Sha256::new();
    for (name, bytes, executable) in entries {
        hash.update((name.len() as u32).to_be_bytes());
        hash.update(name.as_bytes());
        hash.update([u8::from(*executable)]);
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}
fn admission(root: &Path, skill: &str, source: &str) -> Result<()> {
    let store = Store::open(root)?;
    let directory = root.join(".oms/skills").join(skill);
    let files = entries(&directory)?;
    let raw = read(&directory.join("SKILL.md"), 256 * 1024)?;
    let mut whole = Sha256::new();
    for (relative, bytes, _) in &files {
        whole.update(relative.as_bytes());
        whole.update([0]);
        whole.update(digest(bytes).as_bytes());
        whole.update(b"\n");
    }
    let mut row = json!({"schema":1,"name":skill,"source":source,"skill_sha256":digest(&raw),"files_sha256":format!("{:x}",whole.finalize()),"admitted_by":std::env::var("USER").unwrap_or_else(|_|"unknown".into()),"ts":timestamp()});
    if !clean(row.to_string().as_bytes()) {
        row["source"] = json!("(redacted)");
    }
    let path = root.join(".oms/skill-admissions.jsonl");
    let _guard = lock(&path)?;
    let old = optional(&path)?;
    let mut bytes = old.clone();
    bytes.extend_from_slice(row.to_string().as_bytes());
    bytes.push(b'\n');
    store.write_locked(&path, &bytes, false, Some(&old))
}
fn local_targets(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut result = BTreeMap::new();
    let base = root.join(".oms/skills");
    if base.is_dir() {
        for entry in std::fs::read_dir(base)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() && self::name(&name, false).is_ok() {
                result.insert(name, entry.path());
            }
        }
    }
    Ok(result)
}
pub(crate) fn targets(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut result = local_targets(root)?;
    let store = root.join(".oms/skill-store");
    if store.is_dir() {
        for entry in std::fs::read_dir(store)? {
            let entry = entry?;
            let skill = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_dir() || name(&skill, false).is_err() {
                continue;
            }
            let raw = optional(&entry.path().join("lock.json"))?;
            if raw.is_empty() {
                continue;
            }
            let row = crate::lifecycle::load(root, &skill)?;
            let path = root.join(
                row["revision_path"]
                    .as_str()
                    .ok_or_else(|| error("invalid revision path"))?,
            );
            if result.insert(skill, path).is_some() {
                return Err(error("imported and local skills share a name"));
            }
        }
    }
    Ok(result)
}
fn owned(root: &Path, path: &Path) -> bool {
    std::fs::read_link(path).is_ok_and(|target| {
        let absolute = path.parent().unwrap_or(root).join(target);
        let mut normalized = PathBuf::new();
        for component in absolute.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => (),
                part => normalized.push(part),
            }
        }
        let normalized = normalized.canonicalize().unwrap_or(normalized);
        normalized.starts_with(root.join(".oms/skills"))
            || normalized.starts_with(root.join(".oms/skill-store"))
    })
}

pub(crate) fn preflight(root: &Path, skill: &str) -> Result<()> {
    for base in [".agents/skills", ".claude/skills"] {
        no_links(root, Path::new(base))?;
        let path = root.join(base).join(skill);
        if std::fs::symlink_metadata(&path).is_ok() && !owned(root, &path) {
            return Err(error("foreign project skill entry blocks link"));
        }
    }
    Ok(())
}
pub(crate) fn link(root: &Path, ctx: &Context<'_>) -> Result<usize> {
    let targets = targets(root)?;
    let mut valid = BTreeMap::new();
    for (skill, path) in targets {
        if entries(&path)
            .and_then(|_| read(&path.join("SKILL.md"), 256 * 1024))
            .and_then(|raw| validate(&raw, &skill, false))
            .is_ok()
        {
            preflight(root, &skill)?;
            valid.insert(skill, path);
        }
    }
    for base in [".agents/skills", ".claude/skills"] {
        no_links(root, Path::new(base))?;
        let directory = root.join(base);
        std::fs::create_dir_all(&directory)?;
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            if owned(root, &entry.path())
                && !valid.contains_key(entry.file_name().to_string_lossy().as_ref())
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        for (skill, target) in &valid {
            let destination = directory.join(skill);
            if owned(root, &destination) {
                if std::fs::read_link(&destination)? == *target {
                    continue;
                }
                std::fs::remove_file(&destination)?;
            }
            std::os::unix::fs::symlink(target, &destination)?;
            let args = vec![
                "--repo".into(),
                root.as_os_str().to_owned(),
                "apply".into(),
                "--path".into(),
                format!("{base}/{skill}").into(),
            ];
            crate::project::privacy(&args, ctx)?;
        }
    }
    Ok(valid.len())
}
pub(crate) fn run(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        raw,
        &[
            "--repo",
            "--name",
            "--file",
            "--source",
            "--ref",
            "--subdir",
            "--expected-bundle-sha256",
            "--expected-current-sha256",
            "--to",
            "--suite",
            "--candidate",
        ],
        &["--json", "--apply", "--record", "--allow-host-commands"],
    )?;
    let command = args
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("help");
    let root = ctx.repo(args.get("--repo"))?;
    let store = Store::open(&root)?;
    if [
        "preview", "import", "update", "rollback", "eval", "outdated",
    ]
    .contains(&command)
    {
        return crate::lifecycle::run(command, &args, &root, ctx);
    }
    if ["help", ""].contains(&command) {
        return Ok(Output::text(crate::usage("skill-forge")));
    }
    args.count(2)?;
    if ["add", "admit", "link", "remove"].contains(&command) {
        no_links(&root, Path::new(".oms/skills"))?;
    }
    let _guard = if ["add", "admit", "link", "remove"].contains(&command) {
        Some(lock(&root.join(".oms/skill-store/.authority"))?)
    } else {
        None
    };
    match command {
        "add" => {
            let skill = name(args.required("--name")?, true)?;
            preflight(&root, skill)?;
            let destination = root.join(".oms/skills").join(skill);
            if std::fs::symlink_metadata(&destination).is_ok() {
                return Err(error("project skill already exists"));
            }
            let bytes = if let Some(file) = args.get("--file") {
                read(&ctx.path(file), 256 * 1024)?
            } else {
                let mut data = vec![];
                std::io::stdin()
                    .take(256 * 1024 + 1)
                    .read_to_end(&mut data)?;
                data
            };
            validate(&bytes, skill, true)?;
            no_links(&root, Path::new(".oms/.gitignore"))?;
            store.write(&root.join(".oms/.gitignore"), b"*\n", true)?;
            store.write(&destination.join("SKILL.md"), &bytes, true)?;
            admission(&root, skill, args.get("--file").unwrap_or("stdin"))?;
            link(&root, ctx)?;
            Ok(Output::text(format!(
                "skill-forge: added {skill} ({}/SKILL.md)\n",
                destination.display()
            )))
        }
        "admit" => {
            let skill = name(
                args.positional
                    .get(1)
                    .ok_or_else(|| error("admit requires a skill name"))?,
                false,
            )?;
            admission(&root, skill, args.get("--source").unwrap_or("admit"))?;
            Ok(Output::text(format!(
                "skill-forge: recorded admission of {skill}\n"
            )))
        }
        "link" => Ok(Output::text(format!(
            "skill-forge: {} skill(s) linked\n",
            link(&root, ctx)?
        ))),
        "show" => {
            let skill = name(
                args.positional
                    .get(1)
                    .ok_or_else(|| error("show requires exactly one name"))?,
                false,
            )?;
            let all = targets(&root)?;
            let path = all.get(skill).ok_or_else(|| error("no such skill"))?;
            Ok(Output {
                code: 0,
                out: read(&path.join("SKILL.md"), 256 * 1024)?,
                err: vec![],
            })
        }
        "remove" => {
            let skill = name(
                args.positional
                    .get(1)
                    .ok_or_else(|| error("remove requires exactly one name"))?,
                false,
            )?;
            let all = targets(&root)?;
            if !all.contains_key(skill) {
                return Err(error("no such skill"));
            }
            for base in [".agents/skills", ".claude/skills"] {
                let path = root.join(base).join(skill);
                if owned(&root, &path) {
                    std::fs::remove_file(path)?;
                }
            }
            let local = root.join(".oms/skills").join(skill);
            if local.is_dir() {
                no_links(&root, Path::new(&format!(".oms/skills/{skill}")))?;
                std::fs::remove_dir_all(local)?;
            } else {
                std::fs::remove_file(root.join(".oms/skill-store").join(skill).join("lock.json"))?;
            }
            Ok(Output::text(format!("skill-forge: removed {skill}\n")))
        }
        "list" | "validate" | "status" | "contracts" => {
            let all = targets(&root)?;
            let mut rows = vec![];
            let mut text = String::new();
            let mut invalid = 0;
            let mut contracts = 0;
            let selected = args.positional.get(1).map(String::as_str);
            if command == "validate"
                && let Some(skill) = selected
            {
                name(skill, false)?;
                if !all.contains_key(skill) {
                    return Err(error("no such skill"));
                }
            }
            for (skill, path) in &all {
                if command == "validate" && selected.is_some_and(|s| s != skill) {
                    continue;
                }
                let result = entries(path)
                    .and_then(|_| read(&path.join("SKILL.md"), 256 * 1024))
                    .and_then(|raw| validate(&raw, skill, false));
                let state = if result.is_ok() {
                    "valid"
                } else {
                    invalid += 1;
                    "invalid"
                };
                if let Ok(meta) = &result
                    && let Some(verify) = meta.get("metadata.verify").or_else(|| meta.get("verify"))
                {
                    contracts += 1;
                    if command == "contracts" {
                        text.push_str(&format!("{skill}\t{verify}\n"));
                    }
                }
                if command == "validate" {
                    text.push_str(&format!(
                        "{}: {skill}{}\n",
                        if result.is_ok() { "ok" } else { "invalid" },
                        result
                            .err()
                            .map(|e| format!(": {}", e.message))
                            .unwrap_or_default()
                            .as_str()
                    ));
                }
                if command == "list" {
                    text.push_str(&format!("{state}: {skill}\n"));
                    rows.push(json!({"name":skill,"state":state}));
                }
            }
            if command == "list" && args.has("--json") {
                return Ok(Output::json(json!({"schema":1,"skills":rows})));
            }
            if command == "status" {
                text = if all.is_empty() {
                    "skill-forge: no project skills\n".into()
                } else {
                    format!(
                        "skill-forge: {} project skill(s), {invalid} invalid, {contracts} with a verify contract\n",
                        all.len()
                    )
                };
            }
            let mut out = Output::text(text);
            if ["validate", "status"].contains(&command) && invalid > 0 {
                out.code = 1;
            }
            Ok(out)
        }
        _ => Err(error("unknown skill-forge command")),
    }
}
pub(crate) fn doctor(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    Args::parse(raw, &[], &[])?.count(0)?;
    let home = home()?;
    let mut text = String::from("# oh-my-setting skill doctor\n\n");
    let mut failed = false;
    let mut codex = BTreeMap::new();
    for (label, base) in [
        ("Codex skills", ".codex/skills"),
        ("Shared agent skills", ".agents/skills"),
        ("Claude skills", ".claude/skills"),
        ("Antigravity skills", ".gemini/config/skills"),
    ] {
        let root = home.join(base);
        let mut names = BTreeMap::new();
        if root.is_dir() {
            for entry in std::fs::read_dir(&root)?.take(500) {
                let entry = entry?;
                let path = entry.path().join("SKILL.md");
                if entry.file_type()?.is_symlink() && !entry.path().exists() {
                    failed = true;
                    text.push_str(&format!("fail: dangling skill entry in {label}\n"));
                    continue;
                }
                if let Ok(raw) = read(&path, 65536)
                    && raw.len() <= 65536
                    && let Ok(meta) = frontmatter(&String::from_utf8_lossy(&raw))
                    && let Some(name) = meta.get("name")
                {
                    if names
                        .insert(
                            name.clone(),
                            meta.get("description").cloned().unwrap_or_default(),
                        )
                        .is_some()
                    {
                        failed = true;
                        text.push_str(&format!("fail: duplicate {name} in {label}\n"));
                    }
                }
            }
        }
        if base == ".codex/skills" {
            codex = names.clone();
        }
        if base == ".agents/skills" {
            for name in names.keys() {
                if codex.contains_key(name) {
                    failed = true;
                    text.push_str(&format!(
                        "fail: duplicate {name} in Codex effective skill overlay\n"
                    ));
                }
            }
        }
        let chars: usize = names.values().map(|s| s.chars().count()).sum();
        let budget = std::env::var("OMS_SKILL_LISTING_CHARS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(8000)
            .clamp(500, 1_000_000);
        text.push_str(&format!(
            "{}: {label}: {} skill(s), descriptions total {chars} characters (budget {budget})\n",
            if chars > budget { "warn" } else { "ok" },
            names.len()
        ));
        if base == ".claude/skills" {
            for (name, description) in &names {
                if description.chars().count() > 1536 {
                    text.push_str(&format!(
                        "warn: Claude listing entry exceeds 1,536 characters: {name}\n"
                    ));
                }
            }
        }
    }
    let budget = std::env::var("OMS_SKILL_WORDS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(900);
    let catalog = ctx.source.join("custom-skills");
    let mut listing_bytes = 0usize;
    let mut count = 0;
    if catalog.is_dir() {
        for entry in std::fs::read_dir(catalog)? {
            let entry = entry?;
            let path = entry.path().join("SKILL.md");
            if let Ok(raw) = read(&path, 256 * 1024) {
                let body = String::from_utf8_lossy(&raw);
                let words = body.split_whitespace().count();
                if words > budget {
                    text.push_str(&format!(
                        "warn: {} SKILL.md is {words} words (budget {budget})\n",
                        entry.file_name().to_string_lossy()
                    ));
                }
                if entry.path().join("references").is_dir() && !body.contains("references/") {
                    text.push_str("warn: references/ is not linked by SKILL.md\n");
                }
                if let Ok(meta) = frontmatter(&body) {
                    listing_bytes += meta.get("description").map(String::len).unwrap_or(0);
                    count += 1;
                }
            }
        }
    }
    let rules = read(&ctx.source.join("rules/global-AGENTS.md"), LIMIT)
        .map(|v| v.len())
        .unwrap_or(0);
    text.push_str(&format!("note: always-loaded OMS text is {} bytes: global rules {rules} + {count} skill description(s) {listing_bytes}\n",rules+listing_bytes));
    let root = ctx.repo(None)?;
    let ledger = optional(&root.join(".oms/skill-admissions.jsonl"))?;
    let mut admitted = BTreeMap::new();
    for line in ledger.split(|b| *b == b'\n') {
        if let Ok(row) = json(line)
            && let Some(name) = row["name"].as_str()
        {
            admitted.insert(name.to_owned(), row);
        }
    }
    for (name, path) in local_targets(&root)? {
        match admitted.get(&name) {
            None => text.push_str(&format!(
                "warn: skill {name} unrecorded: no admission row\n"
            )),
            Some(row) => {
                if digest(&read(&path.join("SKILL.md"), 256 * 1024)?)
                    != row["skill_sha256"].as_str().unwrap_or("")
                {
                    text.push_str(&format!("warn: skill {name} changed since admission\n"));
                }
            }
        }
    }
    text.push_str(if failed {
        "skill-doctor: failed\n"
    } else {
        "skill-doctor: ok\n"
    });
    let mut output = Output::text(text);
    output.code = i32::from(failed);
    Ok(output)
}
pub(crate) fn risk(files: &[(String, Vec<u8>, bool)]) -> Value {
    let instructions=Regex::new(r"(?i)\b(curl|wget|sudo|eval|npx)\s|\b(npm|uv|cargo|pip|brew)\s+(install|add)\b|\bgit\s+clone\b").expect("static risk pattern");
    let urls = Regex::new(r#"https?://(?:[^\s/@<>"'`)\]]*@)?([A-Za-z0-9.-]+)"#)
        .expect("static URL pattern");
    let mut scripts = vec![];
    let mut fetch = vec![];
    let mut hidden = vec![];
    let mut comments = vec![];
    let mut hosts = std::collections::BTreeSet::new();
    let mut url_count = 0;
    for (path, raw, executable) in files {
        if *executable
            || [".sh", ".py", ".js", ".mjs", ".ts", ".rb", ".pl", ".ps1"]
                .iter()
                .any(|s| path.ends_with(s))
            || raw.starts_with(b"#!")
        {
            scripts.push(path.clone());
        }
        for (index, line) in String::from_utf8_lossy(raw).lines().enumerate() {
            let at = format!("{path}:{}", index + 1);
            if instructions.is_match(line) {
                fetch.push(at.clone());
            }
            if line.contains("<!--") {
                comments.push(at.clone());
            }
            if line.chars().any(|c|matches!(c as u32,0x200b..=0x200f|0x202a..=0x202e|0x2060..=0x2069|0xfeff|0xe0000..=0xe007f)){hidden.push(at);}
            for cap in urls.captures_iter(line) {
                url_count += 1;
                hosts.insert(cap[1].to_ascii_lowercase());
            }
        }
    }
    fn item(rows: Vec<String>) -> Value {
        json!({"count":rows.len(),"where":rows.into_iter().take(20).collect::<Vec<_>>()})
    }
    json!({"schema":1,"note":"review points, not a verdict: an empty list does not mean the bundle is safe, and import rules are unchanged","scripts":item(scripts),"fetch_or_run_instructions":item(fetch),"hidden_unicode":item(hidden),"html_comments":item(comments),"external_urls":{"count":url_count,"hosts":hosts.into_iter().take(20).collect::<Vec<_>>()},"allowed_tools":""})
}
