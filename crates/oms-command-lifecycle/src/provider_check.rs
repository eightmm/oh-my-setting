use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

pub(crate) fn check(table: &Value, paths: &Paths) -> Result<Value> {
    let functions = table["functions"]
        .as_array()
        .ok_or_else(|| r::Error(2, "provider functions must be an array".into()))?;
    let providers = table["providers"]
        .as_object()
        .ok_or_else(|| r::Error(2, "provider definitions must be an object".into()))?;
    let mut problems = Vec::<String>::new();
    let mut cache = BTreeMap::<String, Option<String>>::new();
    for (provider, spec) in providers {
        let mut cells = match spec.get("profile") {
            None => serde_json::Map::new(),
            Some(profile) => table["profiles"]
                .get(profile.as_str().unwrap_or(""))
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| r::Error(2, "unknown provider capability profile".into()))?,
        };
        if let Some(overrides) = spec["overrides"].as_object() {
            for (key, cell) in overrides {
                let cell = if let Some(name) = cell["$ref"].as_str() {
                    table["refs"].get(name).ok_or_else(|| {
                        r::Error(2, "unknown provider capability reference".into())
                    })?
                } else {
                    cell
                };
                cells.insert(key.clone(), cell.clone());
            }
        }
        for function in functions {
            let name = function["id"]
                .as_str()
                .ok_or_else(|| r::Error(2, "invalid provider function".into()))?;
            let cell = spec
                .get(name)
                .or_else(|| cells.get(name))
                .unwrap_or(&Value::Null);
            let label = format!("{provider}/{name}");
            if !matches!(
                cell["status"].as_str(),
                Some("supported" | "partial" | "unsupported" | "unknown")
            ) {
                problems.push(format!("{label}: invalid status"));
                continue;
            }
            if !matches!(cell["verified"].as_str(), Some("fixture" | "live")) {
                problems.push(format!("{label}: verified must be fixture or live"));
            }
            if !matches!(cell["status"].as_str(), Some("supported" | "partial")) {
                continue;
            }
            if cell["status"] == "partial" && cell["limit"].as_str().is_none_or(str::is_empty) {
                problems.push(format!("{label}: partial needs a limit"));
            }
            for kind in ["impl", "test"] {
                let Some(entries) = cell[kind].as_array().filter(|a| !a.is_empty()) else {
                    problems.push(format!("{label}: missing {kind} evidence"));
                    continue;
                };
                for entry in entries {
                    let Some((file, word)) = entry
                        .as_str()
                        .and_then(|s| s.split_once("::"))
                        .filter(|(_, w)| !w.is_empty())
                    else {
                        problems.push(format!("{label}: invalid {kind} reference"));
                        continue;
                    };
                    let path = Path::new(file);
                    if path.is_absolute()
                        || path
                            .components()
                            .any(|c| !matches!(c, Component::Normal(_)))
                    {
                        problems.push(format!("{label}: unsafe {kind} reference"));
                        continue;
                    }
                    let text = cache.entry(file.into()).or_insert_with(|| {
                        let full = paths.source.join(path);
                        if !r::physical(&full).is_ok_and(|p| p.starts_with(&paths.source)) {
                            return None;
                        }
                        String::from_utf8(r::read(&full, r::LIMIT).ok()?).ok()
                    });
                    let symbol = regex::escape(word);
                    let pattern = if kind == "impl" && file.ends_with(".py") {
                        format!(
                            r"(?m)^\s*(?:async\s+)?(?:def|class)\s+{symbol}\b|(?m)^{symbol}\s*="
                        )
                    } else if kind == "impl" && file.ends_with(".sh") {
                        format!(r"(?m)^\s*(?:function\s+)?{symbol}\s*\(\)")
                    } else {
                        format!(r"(?:^|[^\w-]){symbol}(?:$|[^\w-])")
                    };
                    if !text
                        .as_ref()
                        .is_some_and(|t| regex::Regex::new(&pattern).is_ok_and(|re| re.is_match(t)))
                    {
                        problems.push(format!(
                            "{label}: {kind} reference not found: {file}::{word}"
                        ));
                    }
                }
            }
        }
    }
    Ok(json!({"schema":1,"checked":providers.len(),"problems":problems,"ok":problems.is_empty()}))
}

pub(crate) fn models(
    paths: &Paths,
    live: bool,
    strict: bool,
    flag: &std::sync::atomic::AtomicBool,
) -> Result<Value> {
    use std::time::Duration;
    r::child_guard()?;
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut families = std::collections::BTreeSet::new();
    let cachedir = std::env::var_os("OMS_CAPABILITY_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or(paths.home.join(".cache"))
                .join("oh-my-setting/capabilities")
        });
    let effort_pattern = regex::Regex::new(r"\(([a-z][a-z0-9]*(?:\s*[,|]\s*[a-z][a-z0-9]*)+)\)")
        .map_err(|_| r::Error(2, "invalid effort pattern".into()))?;
    for (provider, binary, family) in [
        ("codex", "codex", "openai"),
        ("claude", "claude", "anthropic"),
        ("antigravity", "agy", "unknown"),
        ("cursor", "cursor-agent", "unknown"),
        ("grok", "grok", "xai"),
        ("gemini", "gemini", "google"),
        ("qwen", "qwen", "alibaba"),
        ("opencode", "opencode", "unknown"),
        ("deepseek", "dsh", "deepseek"),
        ("vibe", "vibe", "unknown"),
        ("pi", "pi", "unknown"),
        ("copilot", "copilot", "unknown"),
        ("droid", "droid", "unknown"),
        ("aider", "aider", "unknown"),
        ("kimi", "kimi", "moonshot"),
        ("muse", "muse", "meta"),
        ("kiro", "kiro-cli", "unknown"),
        ("continue", "cn", "unknown"),
        ("goose", "goose", "unknown"),
        ("cline", "cline", "unknown"),
        ("kilo", "kilo", "unknown"),
        ("openhands", "openhands", "unknown"),
        ("auggie", "auggie", "unknown"),
    ] {
        let binary = if provider == "opencode"
            && r::find("opencode", &paths.cwd).is_none()
            && r::find("opencode2", &paths.cwd).is_some()
        {
            "opencode2"
        } else {
            binary
        };
        let found = r::find(binary, &paths.cwd);
        if found.is_none() && !["codex", "claude", "antigravity"].contains(&provider) {
            continue;
        }
        let cache_file = cachedir.join(format!("{provider}.env"));
        let _cache_guard = r::lock(&cache_file, &paths.cwd)?;
        let spelling = crate::agy_surface::resolve_binary(paths, binary.into());
        let original = crate::agy_surface::identity(paths, spelling.as_deref())
            .unwrap_or_else(|_| "absent".into());
        let mut probe_status = "skipped";
        let mut model_efforts = Vec::<String>::new();
        let mut usable = false;
        let mut reachable = false;
        let mut release = None;
        let mut catalog = Vec::<String>::new();
        let mut mechanism = "absent";
        let mut effort = Vec::<String>::new();
        if let Some(program) = &found.as_ref().filter(|p| intact(p).unwrap_or(false)) {
            let args: Vec<std::ffi::OsString> = if provider == "grok" {
                vec!["--no-auto-update".into(), "version".into()]
            } else {
                vec!["--version".into()]
            };
            let out = r::command(
                program,
                &args,
                &paths.cwd,
                None,
                flag,
                Duration::from_secs(10),
            )?;
            r::check(flag)?;
            usable = out.0 == 0;
            if usable {
                release = Some(
                    String::from_utf8_lossy(&out.1)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(160)
                        .collect::<String>(),
                );
            }
            let help: Vec<std::ffi::OsString> = match provider {
                "codex" | "muse" => vec!["exec".into(), "--help".into()],
                "kiro" => vec!["chat".into(), "--help".into()],
                "goose" | "kilo" => vec!["run".into(), "--help".into()],
                "grok" => vec!["--no-auto-update".into(), "--help".into()],
                _ => vec!["--help".into()],
            };
            let out = r::command(
                program,
                &help,
                &paths.cwd,
                None,
                flag,
                Duration::from_secs(10),
            )?;
            reachable = out.0 == 0;
            r::check(flag)?;
            let text = String::from_utf8_lossy(&out.1);
            usable |= reachable;
            let control = match provider {
                "claude" | "antigravity" | "grok" | "copilot" | "kiro" => "--effort",
                "pi" | "cline" => "--thinking",
                "droid" | "aider" | "muse" => "--reasoning-effort",
                "kilo" => "--variant",
                _ => "",
            };
            mechanism = if provider == "codex" {
                "config"
            } else if !control.is_empty() && text.contains(control) {
                "flag"
            } else {
                "none"
            };
            if mechanism != "none" {
                effort = match provider {
                    "codex" | "claude" | "antigravity" | "grok" | "aider" | "muse" | "kiro" => {
                        vec!["low", "medium", "high"]
                    }
                    "cline" => vec!["none", "low", "medium", "high", "xhigh"],
                    "pi" | "copilot" | "droid" => vec!["low", "medium", "high", "xhigh", "max"],
                    _ => vec![],
                }
                .into_iter()
                .map(str::to_owned)
                .collect();
                if mechanism == "flag" {
                    let lines = text.lines().collect::<Vec<_>>();
                    let pattern = &effort_pattern;
                    for (i, line) in lines.iter().enumerate() {
                        if line.contains(control) {
                            let window = lines[i..lines.len().min(i + 3)].join(" ");
                            if let Some(found) = pattern.captures(&window) {
                                effort = found[1]
                                    .split([',', '|'])
                                    .map(str::trim)
                                    .map(str::to_owned)
                                    .collect();
                                break;
                            }
                        }
                    }
                }
            }
            if live && usable && reachable {
                if provider == "codex" {
                    probe_status = "failed";
                    match crate::mcp_probe::codex_models(program, &paths.cwd, flag) {
                        Ok(result) => {
                            for row in result.as_array().unwrap() {
                                if row["hidden"] == true {
                                    continue;
                                }
                                if let Some(name) = row["id"]
                                    .as_str()
                                    .or_else(|| row["model"].as_str())
                                    .filter(|s| {
                                        !s.is_empty()
                                            && s.len() <= 256
                                            && !s.chars().any(char::is_control)
                                    })
                                {
                                    catalog.push(name.into());
                                    let scale = row["supportedReasoningEfforts"]
                                        .as_array()
                                        .into_iter()
                                        .flatten()
                                        .filter_map(|e| e["reasoningEffort"].as_str())
                                        .filter(|s| {
                                            s.chars().all(|c| {
                                                c.is_ascii_lowercase() || c.is_ascii_digit()
                                            }) && !s.is_empty()
                                        })
                                        .collect::<Vec<_>>();
                                    if !scale.is_empty() {
                                        model_efforts.push(format!("{name}\t{}", scale.join(" ")));
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            r::check(flag)?;
                            warnings.push(format!(
                                "{provider}: model catalog unavailable; previous catalog retained"
                            ));
                        }
                    }
                } else if ["antigravity", "opencode"].contains(&provider) {
                    probe_status = "failed";
                    let settle = if provider == "antigravity" {
                        let raw = std::env::var("OMS_CAPABILITY_SETTLE_WAIT")
                            .unwrap_or_else(|_| "2".into());
                        let seconds = raw
                            .parse::<f64>()
                            .map_err(|_| r::Error(2, "invalid capability settle wait".into()))?;
                        Duration::try_from_secs_f64(seconds)
                            .map_err(|_| r::Error(2, "invalid capability settle wait".into()))?
                    } else {
                        Duration::ZERO
                    };
                    for _ in 0..2 {
                        let until =
                            std::time::Instant::now()
                                .checked_add(settle)
                                .ok_or_else(|| {
                                    r::Error(2, "capability settle wait exceeds clock range".into())
                                })?;
                        while std::time::Instant::now() < until {
                            r::check(flag)?;
                            std::thread::sleep(
                                until
                                    .saturating_duration_since(std::time::Instant::now())
                                    .min(Duration::from_millis(100)),
                            );
                        }
                        let out = r::command(
                            program,
                            &["models".into()],
                            &paths.cwd,
                            None,
                            flag,
                            Duration::from_secs(30),
                        )?;
                        if out.0 == 0 {
                            catalog = normalize_models(&String::from_utf8_lossy(&out.1));
                            if !catalog.is_empty() {
                                break;
                            }
                        }
                    }
                    if catalog.is_empty() {
                        warnings.push(format!(
                            "{provider}: model catalog unavailable; previous catalog retained"
                        ));
                    }
                }
            }
            let current_spelling = crate::agy_surface::resolve_binary(paths, binary.into());
            if crate::agy_surface::identity(paths, current_spelling.as_deref())
                .ok()
                .as_ref()
                != Some(&original)
            {
                usable = false;
                reachable = false;
                catalog.clear();
                model_efforts.clear();
                probe_status = "failed";
                warnings.push(format!(
                    "{provider}: binary changed during capability probe"
                ));
            }
            if !catalog.is_empty() {
                probe_status = "ok";
                r::atomic(
                    &cachedir.join(format!("{provider}.models")),
                    format!("{}\n", catalog.join("\n")).as_bytes(),
                )?;
                if !model_efforts.is_empty() {
                    r::atomic(
                        &cachedir.join(format!("{provider}.efforts")),
                        format!("{}\n", model_efforts.join("\n")).as_bytes(),
                    )?;
                }
            }
        }
        let snapshot = format!(
            "binary_key={original}\neffort_mechanism={mechanism}\neffort_values={}\nmodels_probe={probe_status}\nprobed_at={}\n",
            effort.join(" "),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| r::Error(2, "clock unavailable".into()))?
                .as_secs()
        );
        r::atomic(&cache_file, snapshot.as_bytes())?;
        if found.is_none() {
            warnings.push(format!("{provider}: provider binary is not installed"));
        } else if !usable || !reachable {
            errors.push(format!("{provider}: bounded version/help probe failed"));
        }
        if usable && reachable && family != "unknown" {
            families.insert(family);
        }
        if catalog.is_empty() {
            let file = cachedir.join(format!("{provider}.models"));
            if r::exists(&file) {
                catalog = normalize_models(&String::from_utf8_lossy(&r::read(&file, r::LIMIT)?));
            }
        }
        let routable = routable(provider, &catalog);
        let configured = if provider == "codex" {
            let file = paths.codex_config()?;
            if r::exists(&file) {
                crate::config::read_toml(&file)
                    .ok()
                    .and_then(|v| v["model"].as_str().map(str::to_owned))
            } else {
                None
            }
        } else {
            None
        };
        let key = |s: &str| {
            s.chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect::<String>()
        };
        let standing = configured.as_ref().and_then(|name| {
            let wanted = key(name);
            if routable.iter().any(|s| key(s) == wanted) {
                Some(true)
            } else if catalog.iter().any(|s| key(s) == wanted) {
                Some(false)
            } else {
                None
            }
        });
        if standing == Some(false) {
            warnings.push(format!("{provider}: configured default is not routable (previous generation or foreign family)"));
        }
        rows.push(json!({"provider":provider,"binary":binary,"installed":found.is_some(),"usable":usable,"version":release,"provider_default_reachable":reachable,"family":family,"effort_mechanism":mechanism,"effort_values":effort,"configured_default":configured,"configured_default_routable":standing,"routable":routable}));
    }
    if strict && families.len() < 2 {
        errors.push("model-family diversity needs at least two usable families".into());
    }
    Ok(
        json!({"schema":2,"ok":errors.is_empty(),"live_models":live,"require_all":false,"strict_diversity":strict,"providers":rows,"warnings":warnings,"errors":errors}),
    )
}

fn intact(binary: &Path) -> Result<bool> {
    let resolved = std::fs::canonicalize(binary)?;
    for path in [binary, resolved.as_path()] {
        let marker = path.with_file_name(format!(
            "{}.oh-my-setting-managed",
            path.file_name().unwrap().to_string_lossy()
        ));
        if r::exists(&marker) {
            let raw = r::read(&marker, 65536)?;
            let actual = r::hash(&crate::transaction::external_read(&resolved)?);
            let legacy = String::from_utf8_lossy(&raw)
                .strip_prefix("sha256=")
                .is_some_and(|s| s.len() == 65 && s.ends_with('\n') && s.trim_end() == actual);
            let native = r::parse(&raw)
                .is_ok_and(|v| v["owner"] == "oh-my-setting" && v["sha256"] == actual);
            if !legacy && !native {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
fn normalize_models(raw: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    raw.lines()
        .filter_map(|s| {
            let s = s.split('\t').next().unwrap_or("").trim();
            (!s.is_empty()
                && s != "Fetching available models..."
                && s.len() <= 256
                && !s.chars().any(char::is_control)
                && seen.insert(s.to_owned()))
            .then(|| s.to_owned())
        })
        .take(500)
        .collect()
}
fn generation(model: &str) -> Option<(u64, u64)> {
    let mut parts = model
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty());
    let major = parts
        .find(|s| s.chars().all(|c| c.is_ascii_digit()))?
        .parse()
        .ok()?;
    let minor = parts
        .next()
        .filter(|s| s.chars().all(|c| c.is_ascii_digit()))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Some((major, minor))
}
fn role(model: &str) -> Option<&str> {
    let (version, family) = model.strip_prefix("gpt-")?.rsplit_once('-')?;
    if !["astra", "sol", "terra", "luna"].contains(&family) {
        return None;
    }
    let pieces = version.split('.').collect::<Vec<_>>();
    (pieces.len() <= 2
        && pieces
            .iter()
            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())))
    .then_some(family)
}
fn family(provider: &str, model: &str) -> &'static str {
    let lower = model.to_ascii_lowercase();
    let inferred =
        if lower.contains("glm") || lower.starts_with("zai/") || lower.starts_with("zhipu") {
            "zhipu"
        } else if lower.contains("grok") || lower.starts_with("xai/") {
            "xai"
        } else if lower.contains("gemini")
            || lower.starts_with("google/")
            || lower.starts_with("vertex")
        {
            "google"
        } else if lower.contains("claude") || lower.starts_with("anthropic/") {
            "anthropic"
        } else if lower.contains("gpt") || lower.starts_with("openai/") {
            "openai"
        } else if lower.contains("qwen") || lower.starts_with("alibaba/") {
            "alibaba"
        } else if lower.contains("deepseek") {
            "deepseek"
        } else if lower.contains("mistral") {
            "mistral"
        } else if lower.contains("moonshot") || lower.contains("kimi") {
            "moonshot"
        } else if lower.contains("minimax") {
            "minimax"
        } else if lower.contains("muse") || lower.starts_with("meta/") {
            "meta"
        } else {
            "unknown"
        };
    match provider {
        "codex" => "openai",
        "muse" => "meta",
        "kimi" => "moonshot",
        "grok" => "xai",
        "gemini" => "google",
        "qwen" => "alibaba",
        "deepseek" => "deepseek",
        "claude" if inferred == "unknown" => "anthropic",
        _ => inferred,
    }
}
fn routable(provider: &str, catalog: &[String]) -> Vec<String> {
    let primary = match provider {
        "codex" => "openai",
        "claude" => "anthropic",
        "grok" => "xai",
        "gemini" => "google",
        "qwen" => "alibaba",
        "deepseek" => "deepseek",
        "vibe" => "mistral",
        "muse" => "meta",
        "kimi" => "moonshot",
        _ => "",
    };
    let tiers = provider == "codex" && catalog.iter().any(|s| role(s).is_some());
    let rows = catalog
        .iter()
        .filter_map(|model| {
            if tiers {
                return Some((
                    model,
                    format!("codex-{}", role(model)?),
                    Some(generation(model)?),
                ));
            }
            let mut group = family(provider, model);
            if group == "unknown" && !primary.is_empty() {
                group = primary;
            }
            if !primary.is_empty() && group != primary {
                return None;
            }
            Some((model, group.to_owned(), generation(model)))
        })
        .collect::<Vec<_>>();
    let newest_major = rows.iter().filter_map(|r| r.2.map(|g| g.0)).max();
    rows.iter()
        .filter(|(_, group, observed)| {
            (!tiers || observed.map(|g| g.0) == newest_major)
                && !rows
                    .iter()
                    .any(|(_, other, newer)| other == group && newer.is_some() && newer > observed)
        })
        .map(|(model, _, _)| (*model).clone())
        .collect()
}
