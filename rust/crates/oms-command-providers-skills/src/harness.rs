use crate::broker::{Broker, SubscriptionBackend, subscription_env};
use crate::common::*;
use crate::gateway::Gateway;
use crate::https::CurlTransport;
use serde_json::json;
use std::ffi::{OsStr, OsString};
use std::process::Command;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::Duration;

fn command(
    host: &str,
    model: &str,
    tail: &[OsString],
    effort: Option<&str>,
    ctx: &Context<'_>,
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![host.into()];
    if host == "codex" {
        if let Some(value) = effort {
            argv.extend([
                "-c".into(),
                format!("model_reasoning_effort=\"{value}\"").into(),
            ]);
        }
        argv.extend([
            "--model".into(),
            model.into(),
            "-c".into(),
            "model_provider=\"openai\"".into(),
        ]);
    } else if host == "claude" {
        argv.extend(["--model".into(), model.into()]);
    } else {
        for file in ["pi-codex-extension.mjs", "pi-oms-extension.mjs"] {
            argv.push("--extension".into());
            argv.push(ctx.source.join("scripts/lib").join(file).into_os_string());
        }
        argv.extend([
            "--provider".into(),
            "oms-codex".into(),
            "--model".into(),
            model.into(),
        ]);
        if let Some(value) = effort {
            argv.extend(["--thinking".into(), value.into()]);
        }
    }
    argv.extend_from_slice(tail);
    argv
}
pub(crate) fn run(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    host_mutation()?;
    if std::env::var("OMS_HARNESS_DELEGATE_DEPTH").is_ok_and(|s| s != "0") {
        return Err(error("A harness worker cannot launch another harness"));
    }
    let separator = raw.iter().position(|s| s == "--").unwrap_or(raw.len());
    let args = Args::parse(
        &raw[..separator],
        &["--model", "--effort", "--context-window"],
        &["--check", "--dry-run"],
    )?;
    args.count(1)?;
    let host = args
        .positional
        .first()
        .map(String::as_str)
        .filter(|s| ["codex", "pi", "claude"].contains(s))
        .ok_or_else(|| error("harness must be codex, pi or claude"))?;
    let model = args.required("--model")?;
    if model.len() > 160
        || !model
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || ["provider-default", "latest", "auto"].contains(&model)
    {
        return Err(error("An exact model ID is required"));
    }
    let requested = args.get("--effort");
    if requested.is_some_and(|s| !["low", "medium", "high", "xhigh", "max"].contains(&s)) {
        return Err(error("unsupported reasoning effort"));
    }
    let window = number(
        args.get("--context-window").unwrap_or("200000"),
        1024,
        2_000_000,
        "context-window",
    )?;
    let tail = raw.get(separator + 1..).unwrap_or(&[]);
    let mut denied = vec![
        "--model",
        "-m",
        "--provider",
        concat!("--api-", "key"),
        "--models",
    ];
    if host == "codex" {
        denied.extend([
            "--oss",
            "--local-provider",
            "--profile",
            "-p",
            "--config",
            "-c",
            "login",
            "logout",
        ]);
    }
    if host == "claude" {
        denied.push("--settings");
    }
    for arg in tail {
        if arg == "--" {
            break;
        }
        let s = arg
            .to_str()
            .ok_or_else(|| error("native arguments must be UTF-8"))?;
        let option = s.split('=').next().unwrap_or("");
        if denied.contains(&option)
            || (host == "codex"
                && ["-m", "-c", "-p"]
                    .iter()
                    .any(|p| s.starts_with(p) && s != *p))
        {
            return Err(error(
                "Use the OMS model selection; native routing overrides are unsupported",
            ));
        }
    }
    if args.has("--dry-run") {
        let argv = command(host, model, tail, requested, ctx);
        return Ok(Output::json(
            json!({"harness":host,"model_provider":"codex","auth":"chatgpt-subscription","model":model,"argv":argv.iter().map(|s|s.to_string_lossy()).collect::<Vec<_>>()}),
        ));
    }
    if !installed("codex") || !installed(host) {
        return Err(error(
            "Both Codex CLI and the selected harness must already be installed",
        ));
    }
    let mut broker = Broker::start()?;
    broker.login(false)?;
    let details = broker.model(model)?;
    let supported: Vec<String> = details["supportedReasoningEfforts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["reasoningEffort"].as_str().map(str::to_owned))
        .collect();
    let effort = requested
        .or_else(|| details["defaultReasoningEffort"].as_str())
        .or_else(|| {
            [
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ]
            .into_iter()
            .find(|s| supported.iter().any(|v| v == s))
        })
        .map(str::to_owned);
    if effort
        .as_ref()
        .is_some_and(|s| !supported.is_empty() && !supported.contains(s))
    {
        return Err(error(
            "The selected model does not support this reasoning effort",
        ));
    }
    if args.has("--check") {
        return Ok(Output::json(
            json!({"harness":host,"model_provider":"codex","auth":"chatgpt-subscription","model":model,"catalog_verified":true,"inference_verified":false}),
        ));
    }
    let gateway = if host != "codex" {
        Some(Gateway::start(
            Arc::new(SubscriptionBackend {
                broker: Arc::new(Mutex::new(broker)),
                transport: Arc::new(CurlTransport),
            }),
            model.into(),
            effort.clone(),
            supported,
        )?)
    } else {
        None
    };
    let argv = command(host, model, tail, effort.as_deref(), ctx);
    let mut child = Command::new(&argv[0]);
    child.args(&argv[1..]).current_dir(ctx.cwd);
    subscription_env(&mut child);
    for name in [
        concat!("ANTHROPIC_API_", "KEY"),
        concat!("ANTHROPIC_AUTH_", "TOKEN"),
        "ANTHROPIC_BASE_URL",
        concat!("CLAUDE_CODE_OAUTH_", "TOKEN"),
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    ] {
        child.env_remove(name);
    }
    child.env("OMS_AGENT", host);
    if let Some(gateway) = &gateway {
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            child.env_remove(name);
        }
        child
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env("OMS_HARNESS_BRIDGE", "1");
        if host == "claude" {
            child.env(concat!("ANTHROPIC_AUTH_", "TOKEN"), gateway.capability());
            child.env("ANTHROPIC_BASE_URL", gateway.url());
            for name in [
                "ANTHROPIC_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "CLAUDE_CODE_SUBAGENT_MODEL",
            ] {
                child.env(name, model);
            }
            for name in [
                "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            ] {
                child.env(name, "1");
            }
            child.env("ENABLE_TOOL_SEARCH", "false");
        } else {
            child
                .env("OMS_CODEX_HARNESS_URL", gateway.url())
                .env("OMS_CODEX_HARNESS_CAPABILITY", gateway.capability())
                .env(
                    "OMS_CODEX_HARNESS_MODEL",
                    json!({"id":model,"contextWindow":window}).to_string(),
                );
            if std::env::var_os("OMS_PANEL_MAIN_ATTEMPT").is_some()
                && std::env::var_os("OMS_PANEL_OWNER").as_deref() == Some(OsStr::new("codex"))
            {
                child.env("OMS_AGENT", "codex").env_remove("OMS_HOOK_AGENT");
            }
        }
    }
    let mut process = child.spawn()?;
    let code = loop {
        if ctx.interrupted.load(Ordering::Relaxed) {
            let _ = process.kill();
            let _ = process.wait();
            break 130;
        }
        if let Some(status) = process.try_wait()? {
            break status.code().unwrap_or(1);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(gateway);
    Ok(Output {
        code,
        out: vec![],
        err: vec![],
    })
}
