use crate::common::*;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

fn normalized(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(error("--worktree-parent must be an absolute path"));
    }
    let raw = path.to_str().ok_or_else(|| error("path must be UTF-8"))?;
    if raw
        .chars()
        .any(|c| c.is_control() || "()*\u{2028}\u{2029}".contains(c))
    {
        return Err(error(
            "permission path contains a control or rule metacharacter",
        ));
    }
    let mut result = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::Normal(p) => result.push(p),
            Component::ParentDir => {
                result.pop();
            }
            _ => (),
        }
    }
    Ok(result)
}
fn physical(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path.canonicalize().map_err(Into::into);
    }
    let parent = path
        .parent()
        .ok_or_else(|| error("invalid settings path"))?;
    Ok(physical(parent)?.join(
        path.file_name()
            .ok_or_else(|| error("invalid settings path"))?,
    ))
}
fn allow(cfg: &mut Value) -> Result<&mut Vec<Value>> {
    let map = cfg
        .as_object_mut()
        .ok_or_else(|| error("Antigravity settings must contain a JSON object"))?;
    let permissions = map.entry("permissions").or_insert_with(|| json!({}));
    let map = permissions
        .as_object_mut()
        .ok_or_else(|| error("invalid permissions schema"))?;
    map.entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| error("invalid permissions.allow schema"))
}
fn state(raw: &[u8]) -> Result<Value> {
    if raw.is_empty() {
        return Ok(json!({"schema":1,"rules":[]}));
    }
    let value = json(raw)?;
    if value["schema"] != 1
        || !value["rules"].as_array().is_some_and(|rules| {
            rules
                .iter()
                .all(|r| r.as_str().is_some_and(|s| !s.is_empty()))
        })
    {
        return Err(error(
            "managed permission state has an unsupported schema or invalid rules",
        ));
    }
    Ok(value)
}
fn satisfied(rule: &str, have: &[Value]) -> bool {
    if have.iter().any(|v| v.as_str() == Some(rule)) {
        return true;
    }
    let Some((namespace, target)) = rule.strip_suffix(')').and_then(|s| s.split_once('(')) else {
        return false;
    };
    let wildcard = format!("{namespace}(*)");
    if have.iter().any(|v| v.as_str() == Some(wildcard.as_str())) {
        return true;
    }
    if matches!(namespace, "read_file" | "write_file") && target.starts_with('/') {
        let mut parent = Path::new(target).parent();
        while let Some(p) = parent {
            let candidate = format!("{namespace}({})", p.display());
            if have.iter().any(|v| v.as_str() == Some(candidate.as_str())) {
                return true;
            }
            parent = p.parent();
        }
    }
    false
}
pub(crate) fn run(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        args,
        &[
            "--profile",
            "--allow-command",
            "--worktree-parent",
            "--settings",
        ],
        &[
            "--check",
            "--print",
            "--apply",
            "--remove",
            "--allow-toolchains",
        ],
    )?;
    args.count(0)?;
    let profile = args.get("--profile").unwrap_or("consult");
    if !matches!(profile, "consult" | "delegate") {
        return Err(error("--profile must be consult or delegate"));
    }
    let modes: Vec<_> = ["--check", "--print", "--apply", "--remove"]
        .into_iter()
        .filter(|m| args.has(m))
        .collect();
    if modes.len() > 1 {
        return Err(error("only one permission mode is allowed"));
    }
    let mode = modes.first().copied().unwrap_or("--check");
    if matches!(mode, "--apply" | "--remove") {
        host_mutation()?;
    }
    let home = home()?;
    let settings = args
        .get("--settings")
        .map(|s| ctx.path(s))
        .unwrap_or_else(|| {
            env_path(
                "OMS_ANTIGRAVITY_SETTINGS",
                home.join(".gemini/antigravity-cli/settings.json"),
            )
        });
    let settings = physical(&settings)?;
    let receipt = PathBuf::from(format!(
        "{}.oh-my-setting-permissions.json",
        settings.display()
    ));
    if mode == "--remove" && !settings.exists() && !receipt.exists() {
        return Ok(Output::text(
            "provider-permissions: no Antigravity settings or managed grants; nothing to remove\n",
        ));
    }
    let parent = args
        .get("--worktree-parent")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env_path(
                "OMS_DELEGATE_WORKTREE_ROOT",
                env_path("XDG_CACHE_HOME", home.join(".cache")).join("oh-my-setting/worktrees"),
            )
        });
    let parent = normalized(&parent)?;
    let mut commands: Vec<String> = args.all("--allow-command").map(str::to_owned).collect();
    if (!commands.is_empty() || args.has("--allow-toolchains")) && profile != "delegate" {
        return Err(error(
            "--allow-command and --allow-toolchains require --profile delegate",
        ));
    }
    for command in &commands {
        if !command
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
            || !command
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        {
            return Err(error("--allow-command needs one executable-name token"));
        }
    }
    if args.has("--allow-toolchains") {
        for bin in ["uv", "npm", "npx", "cargo", "pnpm", "yarn", "go", "poetry"] {
            if installed(bin) && !commands.iter().any(|c| c == bin) {
                commands.push(bin.into());
            }
        }
    }
    if mode != "--remove" && !installed("agy") {
        return Ok(Output::text(
            "ok: antigravity is not installed; nothing to grant\nnote: codex and claude take authority per invocation and need no allow-list\n",
        ));
    }
    ctx.check()?;
    let _guard = lock(&settings)?;
    let raw = optional(&settings)?;
    let mut cfg = if raw.is_empty() {
        json!({"permissions":{"allow":[]}})
    } else {
        json(&raw)?
    };
    allow(&mut cfg)?;
    let receipt_raw = optional(&receipt)?;
    let mut ownership = state(&receipt_raw)?;
    if mode == "--remove" {
        if receipt_raw.is_empty() {
            return Ok(Output::text(
                "ok: managed antigravity permissions already absent\n",
            ));
        }
        if let Some(journal) = ownership.get("removal") {
            let current = digest(&raw);
            if journal["after_sha256"].as_str() == Some(current.as_str()) {
                std::fs::remove_file(&receipt)?;
                return Ok(Output::text(
                    "provider-permissions: removed managed Antigravity grants\n",
                ));
            }
            if journal["before_sha256"].as_str() != Some(current.as_str()) {
                return Err(error(
                    "managed permission removal cannot resume because settings changed",
                ));
            }
        }
        let mut removed = Vec::new();
        let rules = ownership["rules"]
            .as_array()
            .ok_or_else(|| error("invalid ownership rules"))?;
        let mut seen = std::collections::BTreeSet::new();
        let list = allow(&mut cfg)?;
        for rule in rules {
            let rule = rule.as_str().ok_or_else(|| error("invalid owned rule"))?;
            if seen.insert(rule)
                && let Some(index) = list.iter().position(|v| v.as_str() == Some(rule))
            {
                list.remove(index);
                removed.push(rule.to_owned());
            }
        }
        if !removed.is_empty() {
            let after = json_bytes(&cfg)?;
            let journal = json!({"before_sha256":digest(&raw),"after_sha256":digest(&after)});
            if ownership.get("removal").is_some_and(|v| v != &journal) {
                return Err(error(
                    "permission removal journal no longer matches settings",
                ));
            }
            ownership["removal"] = journal;
            write_locked(
                &receipt,
                &json_bytes(&ownership)?,
                false,
                Some(&receipt_raw),
            )?;
            write_locked(&settings, &after, false, Some(&raw))?;
        }
        std::fs::remove_file(&receipt)?;
        let mut text = removed
            .iter()
            .map(|r| format!("removed: {r}\n"))
            .collect::<String>();
        text.push_str("provider-permissions: removed managed Antigravity grants\n");
        return Ok(Output::text(text));
    }
    let mut required = vec!["read_file(*)".to_owned(), "command(*)".to_owned()];
    if profile == "delegate" {
        required.push(format!("write_file({})", parent.display()));
        required.extend(commands.iter().map(|c| format!("unsandboxed({c})")));
    }
    let missing: Vec<_> = required
        .into_iter()
        .filter(|r| !satisfied(r, allow(&mut cfg).expect("validated schema")))
        .collect();
    if mode == "--print" {
        return Ok(Output::text(if missing.is_empty() {
            String::new()
        } else {
            format!("{}\n", missing.join("\n"))
        }));
    }
    if mode == "--check" {
        let mut out = if missing.is_empty() {
            Output::text(format!(
                "settings: {}\nok: antigravity headless read permissions\n",
                settings.display()
            ))
        } else {
            Output::text(format!(
                "warn: antigravity will be denied these headlessly: {}\nwarn: add these under permissions.allow in {}\n",
                missing.join(" "),
                settings.display()
            ))
        };
        out.code = i32::from(!missing.is_empty());
        return Ok(out);
    }
    if ownership.get("removal").is_some() {
        return Err(error(
            "managed permission removal is incomplete; rerun --remove first",
        ));
    }
    if profile == "delegate"
        && args.get("--worktree-parent").is_none()
        && std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT").is_none()
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if std::fs::symlink_metadata(&parent).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(error("default delegate root must not be a symbolic link"));
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&parent)?;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
    }
    if !missing.is_empty() {
        if !raw.is_empty() {
            write_locked(
                &PathBuf::from(format!("{}.bak", settings.display())),
                &raw,
                true,
                None,
            )?;
        }
        for rule in &missing {
            allow(&mut cfg)?.push(Value::String(rule.clone()));
        }
        write_locked(&settings, &json_bytes(&cfg)?, false, Some(&raw))?;
        let rules = ownership["rules"]
            .as_array_mut()
            .ok_or_else(|| error("invalid owned rule list"))?;
        for rule in &missing {
            if !rules.iter().any(|r| r.as_str() == Some(rule)) {
                rules.push(Value::String(rule.clone()));
            }
        }
        write_locked(
            &receipt,
            &json_bytes(&ownership)?,
            false,
            Some(&receipt_raw),
        )?;
    }
    Ok(Output::text(format!(
        "{}provider-permissions: ok ({profile})\n",
        missing
            .iter()
            .map(|r| format!("granted: {r}\n"))
            .collect::<String>()
    )))
}
