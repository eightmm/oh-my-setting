use crate::common::*;
use crate::model_probe;
use oms_command_agent_role::native_model::supports_exact_model_override;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::time::{SystemTime, UNIX_EPOCH};

const BUILTINS: &[(&str, &str)] = &[
    ("codex", "codex"),
    ("claude", "claude"),
    ("antigravity", "agy"),
    ("cursor", "cursor-agent"),
    ("grok", "grok"),
    ("gemini", "gemini"),
    ("qwen", "qwen"),
    ("opencode", "opencode"),
    ("deepseek", "dsh"),
    ("vibe", "vibe"),
    ("pi", "pi"),
    ("copilot", "copilot"),
    ("droid", "droid"),
    ("aider", "aider"),
    ("muse", "muse"),
    ("kimi", "kimi"),
    ("kiro", "kiro-cli"),
    ("continue", "cn"),
    ("goose", "goose"),
    ("cline", "cline"),
    ("kilo", "kilo"),
    ("openhands", "openhands"),
    ("auggie", "auggie"),
];
pub(crate) fn normalize(raw: &str) -> Result<String> {
    let name = match raw {
        "agy" => "antigravity",
        "cursor-agent" => "cursor",
        "grok-build" => "grok",
        "gemini-cli" => "gemini",
        "qwen-code" => "qwen",
        "opencode2" => "opencode",
        "dsh" | "deepseek-harness" => "deepseek",
        "mistral-vibe" => "vibe",
        "github-copilot" => "copilot",
        "factory-droid" => "droid",
        "kiro-cli" => "kiro",
        "cn" => "continue",
        _ => raw,
    };
    if BUILTINS.iter().any(|(n, _)| *n == name) {
        return Ok(name.into());
    }
    if !name.is_empty()
        && name.len() <= 64
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && installed(&binary(name)?)
    {
        Ok(name.into())
    } else {
        Err(error("unsupported provider"))
    }
}
fn binary(name: &str) -> Result<String> {
    if name == "opencode" && !installed("opencode") && installed("opencode2") {
        return Ok("opencode2".into());
    }
    if let Some((_, bin)) = BUILTINS.iter().find(|(n, _)| *n == name) {
        return Ok((*bin).into());
    }
    let base = env_path(
        "OMS_PROVIDER_ADAPTER_DIR",
        env_path("XDG_CONFIG_HOME", home()?.join(".config"))
            .join("oh-my-setting/provider-adapters"),
    );
    let direct = base.join(format!("oms-agent-adapter-{name}"));
    if installed(&direct.to_string_lossy()) {
        Ok(direct.to_string_lossy().into_owned())
    } else {
        Ok(format!("oms-agent-adapter-{name}"))
    }
}
fn selection(raw: &str) -> Result<Vec<String>> {
    let mut available = BUILTINS
        .iter()
        .map(|(n, _)| n.to_string())
        .collect::<Vec<_>>();
    let mut dirs =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    dirs.push(env_path(
        "OMS_PROVIDER_ADAPTER_DIR",
        env_path("XDG_CONFIG_HOME", home()?.join(".config"))
            .join("oh-my-setting/provider-adapters"),
    ));
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten().take(4096) {
                if let Some(name) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.strip_prefix("oms-agent-adapter-"))
                    && !available.iter().any(|n| n == name)
                    && normalize(name).is_ok()
                {
                    available.push(name.into());
                }
            }
        }
    }
    if ["default", "auto", "installed", "all", ""].contains(&raw) {
        return Ok(available
            .into_iter()
            .filter(|n| {
                raw == "all"
                    || (matches!(raw, "default" | "")
                        && ["codex", "claude", "antigravity"].contains(&n.as_str()))
                    || binary(n).is_ok_and(|b| installed(&b))
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
        let name = normalize(&value)?;
        if !seen.insert(name.clone()) {
            return Err(error("duplicate provider"));
        }
        out.push(name);
    }
    if out.is_empty() {
        return Err(error("no providers selected"));
    }
    Ok(out)
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
    let providers = selection(args.get("--providers").unwrap_or("default"))?;
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
        let bin = binary(&provider)?;
        let present = installed(&bin);
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
            let evidence =
                model_probe::inspect(&provider, output.code, help.code, &help.out, &help.err);
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
                                Ok(model_probe::clean_catalog_rows(
                                    &provider,
                                    &String::from_utf8_lossy(&out.out),
                                ))
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
                if provider == "codex" {
                    let cached_scales = model_probe::parse_effort_scales(&String::from_utf8_lossy(
                        &optional(&efforts_file)?,
                    ));
                    let union = model_probe::union_effort_scales(&cached_scales);
                    if !union.is_empty() {
                        fields.insert("effort_values".into(), union.join(" "));
                    }
                }
                let bytes = model_probe::cache_text(&fields);
                write_locked(&env_file, bytes.as_bytes(), false, None)?;
            }
        } else if refresh || doctor {
            usable = Some(false);
        }
        let catalog_raw = String::from_utf8_lossy(&optional(&models_file)?).into_owned();
        let catalog = model_probe::catalog_selectors(&provider, &catalog_raw);
        let models = catalog.iter().take(20).cloned().collect::<Vec<_>>();
        let routable = model_probe::routable_models(&catalog, &provider);
        let not_routed = catalog
            .iter()
            .filter(|m| !routable.contains(m))
            .cloned()
            .collect::<Vec<_>>();
        let (scales, selectors) = if provider == "antigravity" {
            model_probe::agy_effort_scales_and_selectors(&catalog_raw)
        } else {
            (
                model_probe::parse_effort_scales(&String::from_utf8_lossy(&optional(
                    &efforts_file,
                )?)),
                BTreeMap::new(),
            )
        };
        let efforts = if provider == "codex" && !scales.is_empty() {
            model_probe::union_effort_scales(&scales)
        } else {
            fields
                .get("effort_values")
                .map(|v| v.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_default()
        };
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
                families.insert(model_probe::provider_family(&provider).to_owned());
            }
            rows.push(json!({"provider":provider,"binary":bin,"installed":present,"usable":usable.unwrap_or(false),"version":version,"provider_default_reachable":reachable,"family":model_probe::provider_family(&provider),"effort_mechanism":fields.get("effort_mechanism"),"effort_values":efforts,"configured_default":configured,"configured_default_routable":configured_standing,"routable":routable}));
        } else {
            rows.push(json!({"provider":provider,"binary":bin,"present":present,"usable":usable,"exact_model_override":supports_exact_model_override(&provider),"catalog_models":catalog,"models":models,"routable":routable,"not_routed":not_routed,"effort_mechanism":fields.get("effort_mechanism"),"effort_values":efforts,"model_effort_scales":scales,"selectors":selectors,"catalog_probe":if ["codex","antigravity","opencode"].contains(&provider.as_str()){fields.get("models_probe").map(String::as_str).unwrap_or("unknown")}else{"unsupported"},"snapshot_age_seconds":snapshot_age,"catalog_age_seconds":age(&models_file)}));
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
