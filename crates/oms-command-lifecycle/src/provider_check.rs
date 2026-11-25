use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use oms_command_providers_skills::model_probe;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

// Quoted examples and comments cannot certify a Rust source definition.
fn rust_code(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut code = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while i < bytes.len() && depth != 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'r' {
            let mut quote = i + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if bytes.get(quote) != Some(&b'"') {
                i += 1;
                continue;
            }
            let hashes = &bytes[i + 1..quote];
            i = quote + 1;
            while i < bytes.len() {
                if bytes[i] == b'"' && bytes[i + 1..].starts_with(hashes) {
                    i += 1 + hashes.len();
                    break;
                }
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'\'' {
            let end = if bytes.get(i + 1) == Some(&b'\\') {
                (i + 3..bytes.len()).find(|&j| matches!(bytes[j], b'\'' | b'\n'))
            } else {
                text[i + 1..].chars().next().map(|c| i + 1 + c.len_utf8())
            };
            let Some(end) = end.filter(|&j| bytes.get(j) == Some(&b'\'')) else {
                i += 1;
                continue;
            };
            i = end + 1;
        } else {
            i += 1;
            continue;
        }
        for byte in &mut code[start..i] {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(code).expect("source masking preserves UTF-8")
}

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
                        let text = String::from_utf8(r::read(&full, r::LIMIT).ok()?).ok()?;
                        Some(if file.ends_with(".rs") {
                            rust_code(&text)
                        } else {
                            text
                        })
                    });
                    let symbol = regex::escape(word);
                    let pattern = if file.ends_with(".rs") {
                        format!(
                            r"(?m)^\s*(?:pub(?:\([^)\r\n]*\))?\s+)?(?:(?:async|unsafe|const|extern)\s+)*(?:fn|struct|enum|union|trait|type|const|static)\s+(?:mut\s+)?{symbol}\b"
                        )
                    } else if kind == "impl" && file.ends_with(".py") {
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
        let existing_models_probe = if r::exists(&cache_file) {
            String::from_utf8_lossy(&r::read(&cache_file, r::LIMIT)?)
                .lines()
                .find_map(|line| {
                    line.split_once('=')
                        .filter(|(key, _)| *key == "models_probe")
                        .map(|(_, value)| value.to_owned())
                })
                .filter(|value| !value.is_empty())
        } else {
            None
        };
        let spelling = crate::agy_surface::resolve_binary(paths, binary.into());
        let original = crate::agy_surface::identity(paths, spelling.as_deref())
            .unwrap_or_else(|_| "absent".into());
        let mut probe_status: Option<&str> = None;
        let mut model_efforts = Vec::<String>::new();
        let mut usable = false;
        let mut reachable = false;
        let mut release = None;
        let mut catalog_rows = Vec::<String>::new();
        let mut evidence = model_probe::Evidence::absent();
        if let Some(program) = &found.as_ref().filter(|p| intact(p).unwrap_or(false)) {
            let args = model_probe::version_args(provider)
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            let out = r::command(
                program,
                &args,
                &paths.cwd,
                None,
                flag,
                Duration::from_secs(10),
            )?;
            r::check(flag)?;
            let version_code = out.0;
            if version_code == 0 {
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
            let help = model_probe::help_args(provider)
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            let out = r::command(
                program,
                &help,
                &paths.cwd,
                None,
                flag,
                Duration::from_secs(10),
            )?;
            r::check(flag)?;
            evidence = model_probe::inspect(provider, version_code, out.0, &out.1, &out.2);
            usable = evidence.usable();
            reachable = evidence.reachable;
            if live && usable && reachable {
                if provider == "codex" {
                    probe_status = Some("failed");
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
                                    catalog_rows.push(name.into());
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
                    probe_status = Some("failed");
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
                            catalog_rows = model_probe::clean_catalog_rows(
                                provider,
                                &String::from_utf8_lossy(&out.1),
                            );
                            if !catalog_rows.is_empty() {
                                break;
                            }
                        }
                    }
                    if catalog_rows.is_empty() {
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
                catalog_rows.clear();
                model_efforts.clear();
                probe_status = Some("failed");
                warnings.push(format!(
                    "{provider}: binary changed during capability probe"
                ));
            }
            if !catalog_rows.is_empty() {
                probe_status = Some("ok");
                r::atomic(
                    &cachedir.join(format!("{provider}.models")),
                    format!("{}\n", catalog_rows.join("\n")).as_bytes(),
                )?;
                if !model_efforts.is_empty() {
                    r::atomic(
                        &cachedir.join(format!("{provider}.efforts")),
                        format!("{}\n", model_efforts.join("\n")).as_bytes(),
                    )?;
                }
            }
        }
        let effort_values = if provider == "codex" && usable {
            let efforts_file = cachedir.join(format!("{provider}.efforts"));
            let scales = if r::exists(&efforts_file) {
                model_probe::parse_effort_scales(&String::from_utf8_lossy(&r::read(
                    &efforts_file,
                    r::LIMIT,
                )?))
            } else {
                BTreeMap::new()
            };
            let union = model_probe::union_effort_scales(&scales);
            if union.is_empty() {
                evidence.effort_values.clone()
            } else {
                union
            }
        } else {
            evidence.effort_values.clone()
        };
        let mut fields = evidence.cache_fields(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| r::Error(2, "clock unavailable".into()))?
                .as_secs(),
        );
        fields.insert("effort_values".into(), effort_values.join(" "));
        fields.insert("binary_key".into(), original);
        fields.insert(
            "models_probe".into(),
            probe_status
                .map(str::to_owned)
                .or(existing_models_probe)
                .unwrap_or_else(|| "skipped".into()),
        );
        let snapshot = model_probe::cache_text(&fields);
        r::atomic(&cache_file, snapshot.as_bytes())?;
        if found.is_none() {
            warnings.push(format!("{provider}: provider binary is not installed"));
        } else if !usable || !reachable {
            errors.push(format!("{provider}: bounded version/help probe failed"));
        }
        if usable && reachable && family != "unknown" {
            families.insert(family);
        }
        let catalog_raw = if !catalog_rows.is_empty() {
            catalog_rows.join("\n")
        } else {
            let file = cachedir.join(format!("{provider}.models"));
            if r::exists(&file) {
                String::from_utf8_lossy(&r::read(&file, r::LIMIT)?).into_owned()
            } else {
                String::new()
            }
        };
        let catalog = model_probe::catalog_selectors(provider, &catalog_raw);
        let routable = model_probe::routable_models(&catalog, provider);
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
        rows.push(json!({"provider":provider,"binary":binary,"installed":found.is_some(),"usable":usable,"version":release,"provider_default_reachable":reachable,"family":family,"effort_mechanism":evidence.effort_mechanism,"effort_values":effort_values,"configured_default":configured,"configured_default_routable":standing,"routable":routable}));
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
