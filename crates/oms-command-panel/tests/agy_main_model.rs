//! Antigravity main model normalization, detection and render fallback.
#![forbid(unsafe_code)]

use oms_command_panel::cli::Options;
use oms_command_panel::native_panel::{
    PANEL_ENV_PASSTHROUGH, apply_main_model_env, detect_antigravity_model, dry_run_plan,
    normalize_antigravity_model,
};
use oms_command_panel::render;
use serde_json::json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn n(s: &str) -> Option<String> {
    normalize_antigravity_model(s)
}

#[test]
fn normalizes_display_names_and_slugs() {
    for (raw, want) in [
        ("Orchestrator (Carbon)", "gemini-pro-orchestrator"),
        ("orchestrator (argon)", "gemini-pro-orchestrator-argon"),
        ("Gemini 4 Carbon (High)", "gemini-4-carbon-high"),
        ("Gemini 3.8 Flash (Medium)", "gemini-3.8-flash-medium"),
        ("Gemini 4 Hydrogen (XHigh)", "gemini-4-hydrogen-xhigh"),
        ("Claude Opus 5.5 (High)", "claude-opus-5-5-high"),
        ("GPT-OSS 120B (Medium)", "gpt-oss-120b-medium"),
        ("  gemini-4-carbon-high  ", "gemini-4-carbon-high"),
        ("a", "a"),
        ("Claude Sonnet 4.6", "claude-sonnet-4-6"),
        ("Gemini 4 Carbon", "gemini-4-carbon"),
        ("GPT-OSS 120B", "gpt-oss-120b"),
    ] {
        assert_eq!(n(raw).as_deref(), Some(want), "{raw}");
    }
    let edge = format!("a{}", "b".repeat(159));
    assert_eq!(n(&edge).as_deref(), Some(edge.as_str()));
    for bad in [
        "",
        "   ",
        "a\nb",
        "Gemini 4\u{7} (High)",
        "-lead",
        "Mystery Model (High)",
        "Gemini 4 Carbon ()",
        "Gemini (High)",
        "Claude Opus (High)",
        "None",
        "auto",
        "Default",
        "null",
        "unknown",
        "Gemini",
        "GEMINI-4",
        "Claude-Opus-5",
        "gemini-4 (",
    ] {
        assert_eq!(n(bad), None, "{bad:?}");
    }
    assert_eq!(n(&format!("a{}", "b".repeat(160))), None);
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("oms-agy-model-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Child entry: runs only when spawned by `run_child`, so env stays per-process.
#[test]
fn child_probe() {
    if std::env::var_os("OMS_AGY_PROBE").is_none() {
        return;
    }
    let report = json!({"room":{"id":"r","participants":[
        {"participant":"p","provider":"antigravity","role":"main"}]}});
    let got = render::calls(&report);
    println!(
        "PROBE:{}|{}",
        detect_antigravity_model().unwrap_or_default(),
        got[0]["model"].as_str().unwrap_or("?")
    );
}

fn run_child(envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["child_probe", "--exact", "--nocapture", "--test-threads=1"])
        .env_remove("OMS_MODEL")
        .env_remove("OMS_ANTIGRAVITY_SETTINGS")
        .env("OMS_AGY_PROBE", "1");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .find_map(|l| l.split_once("PROBE:").map(|(_, r)| r))
        .unwrap_or_else(|| panic!("no probe: {text}"))
        .to_owned()
}

#[test]
fn detects_from_env_and_settings_and_renders_fallback() {
    let d = scratch("detect");
    let s = d.join("settings.json");
    let p = s.to_str().unwrap();
    std::fs::write(&s, r#"{"model":"Claude Opus 5.5 (High)"}"#).unwrap();
    assert_eq!(
        run_child(&[("OMS_ANTIGRAVITY_SETTINGS", p)]),
        "claude-opus-5-5-high|claude-opus-5-5-high"
    );
    // OMS_MODEL wins over settings.
    assert_eq!(
        run_child(&[
            ("OMS_ANTIGRAVITY_SETTINGS", p),
            ("OMS_MODEL", "Gemini 4 Carbon (High)")
        ]),
        "gemini-4-carbon-high|gemini-4-carbon-high"
    );
    // Empty OMS_MODEL falls through to settings.
    assert_eq!(
        run_child(&[("OMS_ANTIGRAVITY_SETTINGS", p), ("OMS_MODEL", "")]),
        "claude-opus-5-5-high|claude-opus-5-5-high"
    );
    // Malformed, non-object, missing key, oversized, symlink, missing file.
    for body in [
        "{",
        "[1]",
        r#"{"other":1}"#,
        r#"{"model":5}"#,
        r#"{"model":""}"#,
    ] {
        std::fs::write(&s, body).unwrap();
        assert_eq!(run_child(&[("OMS_ANTIGRAVITY_SETTINGS", p)]), "|", "{body}");
    }
    let big = format!(
        r#"{{"model":"gemini-4","pad":"{}"}}"#,
        "x".repeat(256 * 1024)
    );
    std::fs::write(&s, big).unwrap();
    assert_eq!(run_child(&[("OMS_ANTIGRAVITY_SETTINGS", p)]), "|");
    std::fs::write(&s, r#"{"model":"gemini-4"}"#).unwrap();
    let link = d.join("link.json");
    std::os::unix::fs::symlink(&s, &link).unwrap();
    assert_eq!(
        run_child(&[("OMS_ANTIGRAVITY_SETTINGS", link.to_str().unwrap())]),
        "|"
    );
    let gone = d.join("gone.json");
    assert_eq!(
        run_child(&[("OMS_ANTIGRAVITY_SETTINGS", gone.to_str().unwrap())]),
        "|"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn passthrough_keeps_model_variables() {
    for k in ["OMS_MODEL", "OMS_ANTIGRAVITY_SETTINGS"] {
        assert!(PANEL_ENV_PASSTHROUGH.contains(&k), "{k}");
    }
    let env_map = |cmd: &Command| -> std::collections::BTreeMap<String, Option<String>> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|val| val.to_string_lossy().into_owned()),
                )
            })
            .collect()
    };
    for harness in ["codex", "claude", "pi"] {
        let mut cmd = Command::new("true");
        apply_main_model_env(&mut cmd, harness, Some("gpt-5.3"));
        let envs = env_map(&cmd);
        assert_eq!(envs.get("OMS_MODEL"), Some(&None), "{harness}");
        assert_eq!(
            envs.get("OMS_ANTIGRAVITY_SETTINGS"),
            Some(&None),
            "{harness}"
        );
    }
    let mut agy_set = Command::new("true");
    apply_main_model_env(&mut agy_set, "agy", Some("gemini-4-carbon-high"));
    assert_eq!(
        env_map(&agy_set).get("OMS_MODEL"),
        Some(&Some("gemini-4-carbon-high".into()))
    );
    let mut agy_unset = Command::new("true");
    apply_main_model_env(&mut agy_unset, "agy", None);
    assert_eq!(env_map(&agy_unset).get("OMS_MODEL"), Some(&None));
}

fn plan(args: &[&str]) -> serde_json::Value {
    let argv: Vec<OsString> = args.iter().map(OsString::from).collect();
    let options = Options::parse(&argv).unwrap();
    let services = oms_panel_core::SubcommandServices::sibling().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = std::env::current_dir().unwrap();
    let (code, out, _) =
        dry_run_plan(&repo, &source, &options, &services, &AtomicBool::new(false)).unwrap();
    assert_eq!(code, 0);
    serde_json::from_slice(&out).unwrap()
}

/// Child entry for dry-run plans (per-process env).
#[test]
fn child_plan() {
    if std::env::var_os("OMS_AGY_PLAN").is_none() {
        return;
    }
    let mut args = vec!["--launch", "agy", "--dry-run"];
    let explicit = std::env::var("OMS_AGY_EXPLICIT").unwrap_or_default();
    if !explicit.is_empty() {
        args.extend(["--model", &explicit]);
    }
    let p = plan(&args);
    println!("PLAN:{}|{}", p["model"], p["argv"]);
}

#[test]
fn explicit_model_never_falls_back_to_detected() {
    let d = scratch("plan");
    let s = d.join("settings.json");
    std::fs::write(&s, r#"{"model":"Claude Opus 5.5 (High)"}"#).unwrap();
    let run = |explicit: &str| {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["child_plan", "--exact", "--nocapture", "--test-threads=1"])
            .env_remove("OMS_MODEL")
            .env("OMS_AGY_PLAN", "1")
            .env("OMS_AGY_EXPLICIT", explicit)
            .env("OMS_PANEL_MAIN_PERMISSIONS", "native")
            .env("OMS_ANTIGRAVITY_SETTINGS", &s)
            .output()
            .unwrap();
        let t = String::from_utf8_lossy(&out.stdout).into_owned();
        t.lines()
            .find_map(|l| l.split_once("PLAN:").map(|(_, r)| r.to_owned()))
            .unwrap_or_else(|| panic!("no plan: {t}{}", String::from_utf8_lossy(&out.stderr)))
    };
    // No explicit model: detected, recorded, but not passed to agy.
    let none = run("");
    assert!(none.starts_with("\"claude-opus-5-5-high\"|"), "{none}");
    assert!(!none.contains("--model"), "{none}");
    // Explicit unnormalizable model is kept verbatim, never replaced by the detected one.
    let custom = run("_custom");
    assert!(custom.starts_with("\"_custom\"|"), "{custom}");
    assert!(
        custom.contains("--model") && !custom.contains("opus"),
        "{custom}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

struct CatalogServices;

impl oms_panel_core::PeerExecutor for CatalogServices {
    fn execute(
        &self,
        _request: &oms_panel_core::services::ExecutionRequest,
        _interrupted: &AtomicBool,
    ) -> oms_panel_core::Result<oms_panel_core::CommandOutput> {
        Ok(oms_panel_core::CommandOutput::default())
    }
}

impl oms_panel_core::Services for CatalogServices {
    fn command(
        &self,
        command: oms_panel_core::NativeCommand,
        _repo: &Path,
        _args: &[OsString],
        _interrupted: &AtomicBool,
    ) -> oms_panel_core::Result<oms_panel_core::CommandOutput> {
        let stdout = match command {
            oms_panel_core::NativeCommand::ProviderCatalog => {
                let policy = oms_panel_core::routing::Policy::bundled()?;
                let mut rows = Vec::new();
                for provider in ["codex", "claude", "antigravity"] {
                    let models: Vec<String> = policy
                        .tiers
                        .values()
                        .flatten()
                        .chain(policy.council.extra.iter())
                        .filter(|c| c.provider == provider)
                        .map(|c| c.model.clone())
                        .collect();
                    rows.push(json!({"provider": provider, "installed": true, "models": models}));
                }
                serde_json::to_vec(&json!({"providers": rows})).unwrap()
            }
            oms_panel_core::NativeCommand::Events => {
                if std::env::var_os("OMS_AGY_SIMULATE_SUCCESSION").is_some() {
                    serde_json::to_vec(&json!([
                        {
                            "attempt_id": "att-old",
                            "tool": "panel-main",
                            "provider": "antigravity",
                            "state": "done",
                            "terminal": true,
                            "refs": {
                                "panel_role": "main",
                                "panel_owner": "antigravity",
                                "panel_room_id": "room-succ",
                                "panel_room_participant": "p-old",
                                "panel_model": "gemini-4-carbon-high"
                            }
                        },
                        {
                            "attempt_id": "att-cur",
                            "tool": "panel-main",
                            "provider": "antigravity",
                            "state": "working",
                            "terminal": false,
                            "refs": {
                                "panel_role": "main",
                                "panel_owner": "antigravity",
                                "panel_room_id": "room-succ",
                                "panel_room_participant": "p-cur",
                                "panel_model": "claude-opus-5-5-high"
                            }
                        }
                    ]))
                    .unwrap()
                } else {
                    b"[]".to_vec()
                }
            }
            oms_panel_core::NativeCommand::Room
                if std::env::var_os("OMS_AGY_SIMULATE_SUCCESSION").is_some() =>
            {
                serde_json::to_vec(&json!({
                    "id": "room-succ",
                    "participants": [
                        {
                            "participant": "p-old",
                            "attempt": "att-old",
                            "role": "main",
                            "provider": "antigravity",
                            "model": "gemini-4-carbon-high",
                            "joined": true
                        },
                        {
                            "participant": "p-cur",
                            "attempt": "att-cur",
                            "role": "main",
                            "provider": "antigravity",
                            "model": "claude-opus-5-5-high",
                            "joined": true
                        }
                    ]
                }))
                .unwrap()
            }
            _ => return Ok(oms_panel_core::CommandOutput::default()),
        };
        Ok(oms_panel_core::CommandOutput {
            code: 0,
            stdout,
            stderr: Vec::new(),
            cleanup_complete: true,
            output_complete: true,
            limited: false,
        })
    }
}

/// Child entry for `--routes` / `--dispatch --dry-run` under a configurable owner (per-process env).
#[test]
fn child_routes() {
    if std::env::var_os("OMS_AGY_ROUTES").is_none() {
        return;
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if let Ok(cli_owner) = std::env::var("OMS_AGY_CLI_OWNER")
        && !cli_owner.is_empty()
    {
        let mut chosen = serde_json::Map::new();
        for role in ["reviewer", "security-auditor"] {
            let mut args = vec![
                OsString::from("--dispatch"),
                OsString::from(role),
                OsString::from("--access"),
                OsString::from("read"),
                OsString::from("--owner"),
                OsString::from(&cli_owner),
                OsString::from("--dry-run"),
                OsString::from("--prompt"),
                OsString::from("check"),
            ];
            if role == "reviewer" {
                args.extend([OsString::from("--verify"), OsString::from("true")]);
            }
            let (code, stdout, stderr) = oms_command_panel::run_with_services(
                "panel",
                &args,
                &source,
                &source,
                &AtomicBool::new(false),
                &CatalogServices,
            );
            assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
            let body: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
            chosen.insert(role.into(), body["route"].clone());
        }
        println!("ROUTES:{}", serde_json::Value::Object(chosen));
        return;
    }
    let args = [OsString::from("--routes")];
    let (code, stdout, stderr) = oms_command_panel::run_with_services(
        "panel",
        &args,
        &source,
        &source,
        &AtomicBool::new(false),
        &CatalogServices,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    let body: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    println!("ROUTES:{}", body["chosen"]);
}

#[test]
fn routes_resolves_agy_reviewer_and_security_auditor() {
    let d = scratch("routes");
    let s = d.join("settings.json");
    std::fs::write(&s, r#"{"model":"Gemini 4 Carbon (High)"}"#).unwrap();
    let run_routes = |env_owner: &str,
                      cli_owner: Option<&str>,
                      model_env: Option<&str>,
                      settings: &Path,
                      succession_room: Option<&str>| {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["child_routes", "--exact", "--nocapture", "--test-threads=1"])
            .env_remove("OMS_MODEL")
            .env_remove("OMS_AGENT")
            .env_remove("OMS_AGY_CLI_OWNER")
            .env_remove("OMS_ROOM_ID")
            .env_remove("OMS_ROOM_PARTICIPANT")
            .env_remove("OMS_PANEL_MAIN_ATTEMPT")
            .env_remove("OMS_AGY_SIMULATE_SUCCESSION")
            .env("HOME", &d)
            .env("XDG_CONFIG_HOME", d.join("cfg"))
            .env("OMS_AGY_ROUTES", "1")
            .env("OMS_PANEL_OWNER", env_owner)
            .env("OMS_ANTIGRAVITY_SETTINGS", settings);
        if let Some(owner) = cli_owner {
            cmd.env("OMS_AGY_CLI_OWNER", owner);
        }
        if let Some(m) = model_env {
            cmd.env("OMS_MODEL", m);
        }
        if let Some(room) = succession_room {
            cmd.env("OMS_AGY_SIMULATE_SUCCESSION", "1");
            cmd.env("OMS_ROOM_ID", room);
        }
        let out = cmd.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let raw = text
            .lines()
            .find_map(|l| l.split_once("ROUTES:").map(|(_, r)| r))
            .unwrap_or_else(|| panic!("no routes: {text}{}", String::from_utf8_lossy(&out.stderr)));
        serde_json::from_str::<serde_json::Value>(raw).unwrap()
    };

    let chosen = run_routes("antigravity", None, None, &s, None);
    for role in ["reviewer", "security-auditor"] {
        assert!(chosen[role]["error"].is_null(), "{role}: {chosen}");
        assert!(
            matches!(chosen[role]["provider"].as_str(), Some("claude" | "codex")),
            "{role}: {chosen}"
        );
    }

    // Parent Agy's OMS_MODEL must not leak into a Codex main's route selection.
    let codex_from_agy = run_routes(
        "antigravity",
        Some("codex"),
        Some("claude-opus-5-5-high"),
        &s,
        None,
    );
    for role in ["reviewer", "security-auditor"] {
        assert_eq!(
            codex_from_agy[role]["judged_family"].as_str(),
            Some("openai"),
            "{role}: {codex_from_agy}"
        );
        assert!(
            matches!(
                codex_from_agy[role]["provider"].as_str(),
                Some("claude" | "antigravity")
            ),
            "{role}: {codex_from_agy}"
        );
    }

    let codex_dispatch_env = run_routes(
        "codex",
        Some("codex"),
        Some("Gemini 4 Carbon (High)"),
        &s,
        None,
    );
    for role in ["reviewer", "security-auditor"] {
        assert_eq!(
            codex_dispatch_env[role]["judged_family"].as_str(),
            Some("openai"),
            "{role}: {codex_dispatch_env}"
        );
        assert!(
            matches!(
                codex_dispatch_env[role]["provider"].as_str(),
                Some("claude" | "antigravity")
            ),
            "{role}: {codex_dispatch_env}"
        );
    }

    let codex_env = run_routes("codex", None, Some("Gemini 4 Carbon (High)"), &s, None);
    for role in ["reviewer", "security-auditor"] {
        assert!(codex_env[role]["error"].is_null(), "{role}: {codex_env}");
        assert!(
            matches!(
                codex_env[role]["provider"].as_str(),
                Some("claude" | "antigravity")
            ),
            "{role}: {codex_env}"
        );
    }

    // A previous finished Gemini main still joined in the room must not mask a newer active Claude main.
    let succession = run_routes("antigravity", None, None, &s, Some("room-succ"));
    for role in ["reviewer", "security-auditor"] {
        assert!(succession[role]["error"].is_null(), "{role}: {succession}");
        assert_ne!(
            succession[role]["provider"], "claude",
            "{role}: {succession}"
        );
        assert!(
            matches!(
                succession[role]["provider"].as_str(),
                Some("codex" | "antigravity")
            ),
            "{role}: {succession}"
        );
    }

    std::fs::write(&s, r#"{"model":"Claude Opus 5.5 (High)"}"#).unwrap();
    let anthropic = run_routes("antigravity", None, None, &s, None);
    for role in ["reviewer", "security-auditor"] {
        assert_ne!(anthropic[role]["provider"], "claude", "{role}: {anthropic}");
        assert!(
            matches!(
                anthropic[role]["provider"].as_str(),
                Some("codex" | "antigravity")
            ),
            "{role}: {anthropic}"
        );
    }

    let missing = d.join("missing.json");
    let unknown = run_routes("antigravity", None, None, &missing, None);
    assert!(
        unknown["reviewer"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("unverified")),
        "{unknown}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn render_calls_surfaces_native_agy_subagent_under_main() {
    let report = json!({
        "room": {
            "id": "room-r",
            "participants": [
                {
                    "attempt_id": "att-main",
                    "participant": "p-main",
                    "provider": "antigravity",
                    "role": "main",
                    "model": "gemini-4-carbon-high"
                }
            ]
        },
        "attempts": {
            "active_recent": [
                {
                    "attempt_id": "att-sub",
                    "tool": "agy-subagent",
                    "provider": "antigravity",
                    "parent_attempt_id": "att-main",
                    "state": "working",
                    "refs": {
                        "panel_role": "researcher",
                        "panel_provider": "antigravity",
                        "panel_harness": "agy",
                        "panel_strategy": "research",
                        "panel_label": "[agy:research] Codebase Researcher",
                        "panel_model": "gemini-4-carbon-high",
                        "panel_room_id": "room-r"
                    }
                }
            ],
            "recent": []
        }
    });
    let calls = render::calls(&report);
    let sub = calls
        .iter()
        .find(|row| row["attempt"] == "att-sub")
        .unwrap_or_else(|| panic!("missing native agy subagent call: {calls:?}"));
    assert_eq!(sub["tool"], "agy-subagent");
    assert_eq!(sub["provider"], "antigravity");
    assert_eq!(sub["role"], "researcher");
    assert_eq!(sub["state"], "working");
    assert_eq!(sub["parent"], "att-main");
    assert_eq!(sub["title"], "[agy:research] Codebase Researcher");
    assert_eq!(sub["model"], "gemini-4-carbon-high");
}
