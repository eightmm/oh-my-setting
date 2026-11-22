use crate::common::*;
use crate::model_probe;
use oms_command_agent_role::native_model::{
    agy_catalog_selector, fixed_effort_model, supports_exact_model_override,
};
use oms_command_agent_role::provider_identity::Discovery;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn normalize(raw: &str, cwd: &std::path::Path) -> Result<String> {
    Discovery::from_env(cwd)?
        .normalize(raw)
        .map(str::to_owned)
        .ok_or_else(|| error("unsupported provider"))
}
fn binary(discovery: &Discovery, name: &str) -> Result<String> {
    discovery
        .binary(name)
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| error("unsupported provider"))
}
fn selection(raw: &str, discovery: &Discovery) -> Result<Vec<String>> {
    let available = discovery.providers();
    if ["default", "auto", "installed", "all", ""].contains(&raw) {
        return Ok(available
            .into_iter()
            .filter(|name| {
                raw == "all"
                    || (matches!(raw, "default" | "")
                        && ["codex", "claude", "antigravity"].contains(&name.as_str()))
                    || discovery.discovered(name)
            })
            .collect());
    }
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for item in raw.split(',') {
        let value = item
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>();
        if value.is_empty() {
            continue;
        }
        let name = discovery
            .normalize(&value)
            .ok_or_else(|| error("unsupported provider"))?;
        if !seen.insert(name.to_owned()) {
            return Err(error("duplicate provider"));
        }
        out.push(name.to_owned());
    }
    if out.is_empty() {
        return Err(error("no providers selected"));
    }
    Ok(out)
}
fn family(provider: &str) -> &str {
    match provider {
        "codex" => "openai",
        "claude" => "anthropic",
        "antigravity" | "gemini" => "google",
        "grok" => "xai",
        "qwen" => "alibaba",
        "deepseek" => "deepseek",
        "vibe" => "mistral",
        "muse" => "meta",
        "kimi" => "moonshot",
        _ => "unknown",
    }
}
fn age(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}
fn generations(models: &[String], provider: &str) -> Vec<String> {
    let role = Regex::new(r"^gpt-([0-9]+)(?:\.([0-9]+))?-(astra|sol|terra|luna)$")
        .expect("static Codex family pattern");
    let tiered = provider == "codex" && models.iter().any(|m| role.is_match(m));
    fn generation(model: &str) -> Option<(u64, u64)> {
        let mut first = None;
        let mut second = 0;
        for segment in model
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
        {
            if !segment.bytes().all(|b| b.is_ascii_digit()) {
                if first.is_some() {
                    break;
                }
                continue;
            }
            let value = segment.parse::<u64>().ok()?;
            if first.is_none() {
                first = Some(value);
            } else {
                second = value;
                break;
            }
        }
        first.map(|a| (a, second))
    }
    fn inferred(model: &str) -> &'static str {
        let model = model.to_ascii_lowercase();
        if model.contains("claude")
            || ["opus", "sonnet", "haiku", "fable"]
                .iter()
                .any(|s| model.starts_with(s))
        {
            "anthropic"
        } else if model.contains("gemini") {
            "google"
        } else if model.contains("gpt-")
            || model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4")
        {
            "openai"
        } else if model.contains("grok") {
            "xai"
        } else if model.contains("qwen") {
            "alibaba"
        } else if model.contains("deepseek") {
            "deepseek"
        } else if model.contains("mistral") {
            "mistral"
        } else if model.contains("llama") {
            "meta"
        } else if model.contains("kimi") {
            "moonshot"
        } else {
            "unknown"
        }
    }
    let mut candidates = Vec::new();
    let mut newest = BTreeMap::new();
    let mut major = 0;
    for model in models {
        let model_family = if tiered {
            let Some(capture) = role.captures(model) else {
                continue;
            };
            format!("codex-{}", &capture[3])
        } else {
            let mut detected = inferred(model);
            let primary = family(provider);
            if detected == "unknown" {
                detected = primary;
            }
            if primary != "unknown" && detected != primary {
                continue;
            }
            detected.into()
        };
        let version = generation(model);
        if let Some(version) = version {
            if tiered {
                major = major.max(version.0);
            }
            newest
                .entry(model_family.clone())
                .and_modify(|v: &mut (u64, u64)| *v = (*v).max(version))
                .or_insert(version);
        }
        candidates.push((model, model_family, version));
    }
    candidates
        .into_iter()
        .filter(|(model, family, version)| {
            fixed_effort_model(provider, model).is_some()
                || (newest
                    .get(family)
                    .is_none_or(|latest| version.as_ref() == Some(latest))
                    && (!tiered || version.is_some_and(|v| v.0 == major)))
        })
        .map(|(m, _, _)| m.clone())
        .collect()
}

pub(crate) fn run(raw: &[OsString], doctor: bool, ctx: &Context<'_>) -> Result<Output> {
    let flags = if doctor {
        vec![
            "--json",
            "--live-models",
            "--require-all",
            "--strict-diversity",
        ]
    } else {
        vec!["--json", "--refresh"]
    };
    let args = Args::parse(raw, &["--providers", "--timeout"], &flags)?;
    args.count(0)?;
    let refresh = args.has("--refresh") || args.has("--live-models");
    if refresh || doctor {
        host_mutation()?;
    }
    if let Some(timeout) = args.get("--timeout") {
        number(timeout, 1, 3600, "--timeout")?;
    }
    let discovery = Discovery::from_env(ctx.cwd)?;
    let providers = selection(args.get("--providers").unwrap_or("default"), &discovery)?;
    let cache = env_path(
        "OMS_CAPABILITY_DIR",
        env_path("XDG_CACHE_HOME", home()?.join(".cache")).join("oh-my-setting/capabilities"),
    );
    let mut rows = vec![];
    let mut warnings = vec![];
    let mut errors = vec![];
    let mut families = BTreeSet::new();
    let mut text = String::new();
    for provider in providers {
        ctx.check()?;
        let bin = binary(&discovery, &provider)?;
        let present = discovery.discovered(&provider);
        let env_file = cache.join(format!("{provider}.env"));
        let models_file = cache.join(format!("{provider}.models"));
        let efforts_file = cache.join(format!("{provider}.efforts"));
        let mut fields = BTreeMap::new();
        for line in String::from_utf8_lossy(&optional(&env_file)?).lines() {
            if let Some((name, value)) = line.split_once('=') {
                fields.insert(name.to_owned(), value.to_owned());
            }
        }
        let mut usable = None;
        let mut version = None;
        let mut reachable = false;
        if (refresh || doctor) && present {
            let output = ctx.command(&bin, model_probe::version_args(&provider), 10)?;
            version = String::from_utf8_lossy(&output.out)
                .lines()
                .next()
                .map(str::to_owned);
            let help = ctx.command(&bin, model_probe::help_args(&provider), 10)?;
            let evidence = model_probe::inspect(&provider, output.code, help.code, &help.out);
            reachable = evidence.reachable;
            usable = Some(evidence.usable());
            if usable == Some(true) {
                let _guard = lock(&env_file)?;
                fields.extend(
                    evidence.cache_fields(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    ),
                );
                if refresh {
                    let listed = if provider == "codex" {
                        (|| {
                            let mut broker = crate::broker::Broker::start()?;
                            let mut models = vec![];
                            let mut scales = String::new();
                            let mut cursor = Value::Null;
                            for _ in 0..20 {
                                let page = broker
                                    .call("model/list", json!({"cursor":cursor,"limit":100}))?;
                                for row in page["data"].as_array().into_iter().flatten() {
                                    if row["hidden"] == true {
                                        continue;
                                    }
                                    if let Some(name) = row
                                        .get("id")
                                        .or_else(|| row.get("model"))
                                        .and_then(Value::as_str)
                                    {
                                        models.push(name.to_owned());
                                        let scale = row["supportedReasoningEfforts"]
                                            .as_array()
                                            .into_iter()
                                            .flatten()
                                            .filter_map(|v| v["reasoningEffort"].as_str())
                                            .collect::<Vec<_>>();
                                        if !scale.is_empty() {
                                            scales.push_str(&format!(
                                                "{name}\t{}\n",
                                                scale.join(" ")
                                            ));
                                        }
                                    }
                                }
                                cursor = page["nextCursor"].clone();
                                if cursor.is_null() || cursor == "" {
                                    break;
                                }
                            }
                            write_locked(&efforts_file, scales.as_bytes(), false, None)?;
                            Ok(models)
                        })()
                    } else if ["antigravity", "opencode"].contains(&provider.as_str()) {
                        ctx.command(&bin, &["models"], 30).and_then(|out| {
                            if out.code == 0 {
                                Ok(String::from_utf8_lossy(&out.out)
                                    .lines()
                                    .filter(|s| !s.trim().is_empty())
                                    .map(str::to_owned)
                                    .collect())
                            } else {
                                Err(error("catalog probe failed"))
                            }
                        })
                    } else {
                        Ok(vec![])
                    };
                    match listed {
                        Ok(models) if !models.is_empty() => {
                            write_locked(
                                &models_file,
                                format!("{}\n", models.join("\n")).as_bytes(),
                                false,
                                None,
                            )?;
                            fields.insert("models_probe".into(), "ok".into());
                        }
                        Ok(_) => (),
                        Err(_) => {
                            fields.insert("models_probe".into(), "failed".into());
                        }
                    }
                }
                let bytes = model_probe::cache_text(&fields);
                write_locked(&env_file, bytes.as_bytes(), false, None)?;
            }
        } else if refresh || doctor {
            usable = Some(false);
        }
        let catalog_rows = String::from_utf8_lossy(&optional(&models_file)?)
            .lines()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let models = catalog_rows.iter().take(20).cloned().collect::<Vec<_>>();
        // AGY's cache includes human labels. Only the exact selector is
        // a model ID for automatic allocation and catalog matching.
        let catalog = if provider == "antigravity" {
            let mut seen = BTreeSet::new();
            catalog_rows
                .iter()
                .filter_map(|row| agy_catalog_selector(row))
                .filter(|id| seen.insert(*id))
                .map(str::to_owned)
                .collect()
        } else {
            catalog_rows.clone()
        };
        let routable = generations(&catalog, &provider);
        let not_routed = catalog
            .iter()
            .filter(|m| !routable.contains(m))
            .cloned()
            .collect::<Vec<_>>();
        let efforts = fields
            .get("effort_values")
            .map(|v| v.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut scales = BTreeMap::new();
        for line in String::from_utf8_lossy(&optional(&efforts_file)?).lines() {
            if let Some((name, scale)) = line.split_once('\t') {
                scales.insert(
                    name.to_owned(),
                    scale
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                );
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let snapshot_age = fields
            .get("probed_at")
            .and_then(|s| s.parse::<u64>().ok())
            .map(|n| now.saturating_sub(n));
        let configured = if doctor && provider == "codex" {
            let path = env_path(
                "OMS_CODEX_CONFIG",
                env_path("CODEX_HOME", home()?.join(".codex")).join("config.toml"),
            );
            String::from_utf8_lossy(&optional(&path)?)
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once('=')?;
                    if name.trim() != "model" || !value.trim_start().starts_with('"') {
                        return None;
                    }
                    value
                        .trim_start()
                        .strip_prefix('"')?
                        .split('"')
                        .next()
                        .map(str::to_owned)
                })
        } else {
            None
        };
        let configured_standing = configured
            .as_ref()
            .filter(|_| !catalog.is_empty())
            .map(|m| routable.contains(m));
        if configured_standing == Some(false) {
            warnings.push(format!(
                "{provider}: configured default model {} is not routable; routable: {}",
                configured.as_deref().unwrap_or(""),
                routable.join(", ")
            ));
        }
        if doctor {
            if !present {
                let item = format!("{provider}: provider binary '{bin}' is not installed");
                if args.has("--require-all") {
                    errors.push(item);
                } else {
                    warnings.push(item);
                }
            } else if usable != Some(true) {
                errors.push(format!("{provider}: provider binary is present but its bounded version/help probe failed"));
            }
            if usable == Some(true) && reachable {
                families.insert(family(&provider).to_owned());
            }
            rows.push(json!({"provider":provider,"binary":bin,"installed":present,"usable":usable.unwrap_or(false),"version":version,"provider_default_reachable":reachable,"family":family(&provider),"effort_mechanism":fields.get("effort_mechanism"),"effort_values":efforts,"configured_default":configured,"configured_default_routable":configured_standing,"routable":routable}));
        } else {
            rows.push(json!({"provider":provider,"binary":bin,"present":present,"usable":usable,"exact_model_override":supports_exact_model_override(&provider),"catalog_models":catalog,"models":models,"routable":routable,"not_routed":not_routed,"effort_mechanism":fields.get("effort_mechanism"),"effort_values":efforts,"model_effort_scales":scales,"catalog_probe":if ["codex","antigravity","opencode"].contains(&provider.as_str()){fields.get("models_probe").map(String::as_str).unwrap_or("unknown")}else{"unsupported"},"snapshot_age_seconds":snapshot_age,"catalog_age_seconds":age(&models_file)}));
        }
        text.push_str(&format!(
            "## {provider} ({})\nmodels: {}\neffort: {} {}\n",
            match usable {
                Some(true) => "usable",
                Some(false) if present => "broken",
                _ if present => "present",
                _ => "absent",
            },
            if routable.is_empty() {
                "unknown".into()
            } else {
                routable
                    .iter()
                    .take(20)
                    .map(|id| {
                        if provider == "antigravity" {
                            catalog_rows
                                .iter()
                                .find(|row| agy_catalog_selector(row) == Some(id.as_str()))
                                .unwrap_or(id)
                        } else {
                            id
                        }
                    })
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            fields
                .get("effort_mechanism")
                .map(String::as_str)
                .unwrap_or("unknown"),
            efforts.join(" ")
        ));
    }
    families.remove("unknown");
    if doctor && args.has("--strict-diversity") && families.len() < 2 {
        errors.push("model-family diversity needs at least two usable families".into());
    }
    if !args.has("--json") {
        for warning in &warnings {
            text.push_str(&format!("warning: {warning}\n"));
        }
        for item in &errors {
            text.push_str(&format!("error: {item}\n"));
        }
        if doctor {
            text.push_str(if errors.is_empty() {
                "model-doctor: ok\n"
            } else {
                "model-doctor: FAILED\n"
            });
        }
    }
    let report = if doctor {
        json!({"schema":2,"ok":errors.is_empty(),"live_models":args.has("--live-models"),"require_all":args.has("--require-all"),"strict_diversity":args.has("--strict-diversity"),"providers":rows,"warnings":warnings,"errors":errors})
    } else {
        json!({"schema":1,"providers":rows})
    };
    let mut output = if args.has("--json") {
        Output::json(report)
    } else {
        Output::text(text)
    };
    output.code = i32::from(!errors.is_empty());
    Ok(output)
}
