//! Advisory preconditions and immutable execution identity; acceptance authority
//! remains in oms-command-plan.
use crate::{Error, Result, require, value};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub fn duration(raw: &str) -> Result<Duration> {
    let re = regex::Regex::new(r"^([0-9]+)([smhd]?)$").unwrap();
    let matched = re
        .captures(raw)
        .ok_or_else(|| Error::new(2, "duration must be positive, such as 15m, and at most 24h"))?;
    let n = matched[1]
        .parse::<u64>()
        .map_err(|_| Error::new(2, "duration overflow"))?;
    let multiplier = match &matched[2] {
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => 1,
    };
    let seconds = n
        .checked_mul(multiplier)
        .filter(|n| (1..=86400).contains(n))
        .ok_or_else(|| Error::new(2, "duration must be positive and at most 24h"))?;
    Ok(Duration::from_secs(seconds))
}
pub fn effort(raw: &str) -> Result<()> {
    require(
        ["auto", "low", "medium", "high", "xhigh", "max", "ultra"].contains(&raw),
        "reasoning effort must be auto, low, medium, high, xhigh, max, or ultra",
    )
}
pub fn model(raw: &str) -> Result<()> {
    require(
        raw.len() <= 160 && value::clean_text(raw) && raw != "provider-default",
        "model must be at most 160 characters without controls or reserved values",
    )
}
pub fn reason(raw: &str) -> Result<()> {
    require(
        !raw.trim().is_empty() && value::clean_text(raw),
        "reason must be nonblank single-line text without controls",
    )
}

pub fn floor_reads(command: &str, allowed: &[&str]) -> Vec<(String, String)> {
    oms_command_plan::floor::incompatible_reads(command, allowed)
}
pub fn missing_base(
    repo: &Path,
    command: &str,
    allowed: &[String],
    interrupted: &AtomicBool,
) -> Result<Option<String>> {
    let words = match oms_command_plan::floor::split(command, true) {
        Ok(words) => words,
        Err(_) => return Ok(None),
    };
    if words.iter().any(|w| ["cd", "pushd"].contains(&w.as_str())) {
        return Ok(None);
    }
    let git = oms_command_plan::repository::Git {
        group_snapshot: None,
        deadline: None,
        repo,
        interrupted,
        group_resolver: None,
    };
    let suffixes = [
        ".sh", ".bash", ".py", ".js", ".mjs", ".cjs", ".ts", ".rb", ".pl", ".lua", ".jl", ".exs",
        ".go",
    ];
    let mut skip = false;
    for word in words {
        if word.chars().all(|c| ";&|".contains(c)) {
            skip = false;
            continue;
        }
        if skip {
            skip = false;
            continue;
        }
        if [
            "-o",
            "--output",
            "--out",
            "--outfile",
            "--report",
            "--junitxml",
            ">",
            ">>",
            "2>",
            "&>",
            "tee",
        ]
        .contains(&word.as_str())
        {
            skip = true;
            continue;
        }
        if word.starts_with('-') || word.contains('=') || word.contains("://") {
            continue;
        }
        let path = word
            .split("::")
            .next()
            .unwrap_or("")
            .trim_start_matches("./")
            .trim_end_matches('/');
        if path.is_empty()
            || path.starts_with(['/', '~'])
            || path.starts_with("..")
            || path.contains("...")
            || path.chars().any(|c| "*?[]$`\\<>|".contains(c))
        {
            continue;
        }
        if !path.contains('/') && !suffixes.iter().any(|s| path.ends_with(s)) {
            continue;
        }
        if !allowed.iter().any(|root| {
            let root = root.trim_start_matches("./").trim_end_matches('/');
            root.is_empty() || root == "." || path == root || path.starts_with(&format!("{root}/"))
        }) {
            continue;
        }
        let object = format!("HEAD:{path}");
        let output = git.output(
            &["cat-file".as_ref(), "-e".as_ref(), object.as_ref()],
            None,
            &[],
            4096,
        )?;
        if output.code != 0 {
            return Ok(Some(path.into()));
        }
    }
    Ok(None)
}

/// Hashes raw tracked bytes as well as Git metadata. An ineligible tree returns
/// None and therefore forces a fresh supervised acceptance run.
pub fn reuse_context(
    repo: &Path,
    controller: &str,
    interrupted: &AtomicBool,
) -> Result<Option<String>> {
    if !value::digest(controller) {
        return Ok(None);
    }
    let store = oms_command_plan::storage::Store::open(repo)?;
    let spec = match store.read("PROJECT.md", 1024 * 1024) {
        Ok(raw) => raw,
        Err(_) => return Ok(None),
    };
    let Ok(spec_text) = std::str::from_utf8(&spec) else {
        return Ok(None);
    };
    let declarations = crate::contract::section(spec_text, "Verification")
        .into_iter()
        .filter_map(|line| line.strip_prefix("- Acceptance reuse: "))
        .collect::<Vec<_>>();
    if declarations != ["same-run"] {
        return Ok(None);
    }
    let git = oms_command_plan::repository::Git {
        group_snapshot: None,
        deadline: None,
        repo,
        interrupted,
        group_resolver: None,
    };
    let raw = git.run(&["ls-files", "-z"])?;
    let names = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .collect::<BTreeSet<_>>();
    let mut budget = 64 * 1024 * 1024;
    let mut entries = vec![];
    let root = oms_safe_fs::Root::open(repo)
        .map_err(|_| Error::new(2, "cannot open tracked source root"))?;
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    for name in names {
        let path = Path::new(std::ffi::OsStr::from_bytes(name));
        let info = match std::fs::symlink_metadata(repo.join(path)) {
            Ok(info) => info,
            Err(_) => return Ok(None),
        };
        let body = if info.is_file() {
            match root.read_path(path, budget, oms_safe_fs::Policy::source()) {
                Ok(raw) => raw,
                Err(_) => return Ok(None),
            }
        } else if info.file_type().is_symlink() {
            match std::fs::read_link(repo.join(path)) {
                Ok(p) => p.as_os_str().as_bytes().to_vec(),
                Err(_) => return Ok(None),
            }
        } else {
            return Ok(None);
        };
        if body.len() > budget {
            return Ok(None);
        }
        budget -= body.len();
        entries.push(json!([value::hash(name), info.mode(), value::hash(&body)]));
    }
    let metadata = [
        "_",
        "SHLVL",
        "PWD",
        "OLDPWD",
        "OMS_GOAL_RUN_ID",
        "OMS_GOAL_CYCLE",
        "OMS_AUTOPILOT_PHASE_WALL",
        "OMS_TS",
    ];
    let mut environment = std::env::vars_os()
        .filter(|(key, _)| !metadata.iter().any(|name| key == name))
        .map(|(k, v)| (value::hash(k.as_bytes()), value::hash(v.as_bytes())))
        .collect::<BTreeMap<_, _>>();
    let count = std::env::var("GIT_CONFIG_COUNT")
        .unwrap_or_else(|_| "0".into())
        .parse::<usize>()
        .ok()
        .filter(|n| *n <= 64);
    let Some(count) = count else {
        return Ok(None);
    };
    environment.remove(&value::hash("GIT_CONFIG_COUNT"));
    let mut config: Vec<(String, String)> = vec![];
    for i in 0..count {
        let key_name = format!("GIT_CONFIG_KEY_{i}");
        let val_name = format!("GIT_CONFIG_VALUE_{i}");
        let (Ok(key), Ok(val)) = (std::env::var(&key_name), std::env::var(&val_name)) else {
            return Ok(None);
        };
        environment.remove(&value::hash(&key_name));
        environment.remove(&value::hash(&val_name));
        if key == "core.hooksPath"
            && val == "/dev/null"
            && config.last() == Some(&(key.clone(), val.clone()))
        {
            continue;
        }
        config.push((key, val));
    }
    Ok(Some(value::hash(value::canonical(
        &json!({"spec": value::hash(spec), "files": entries, "environment": environment, "git_config": config, "runner": controller, "repo": repo}),
    )?)))
}
pub fn reusable(rows: &[Value], expected: &Value, now_seconds: i64) -> Result<Option<String>> {
    if expected["schema"].as_u64() != Some(1)
        || !value::id(value::text(expected, "run_id"), 64)
        || [
            "reuse_context_sha256",
            "plan_sha256",
            "repo_snapshot_sha256",
            "manifest_sha256",
            "accept_sha256_full",
        ]
        .iter()
        .any(|key| !value::digest(value::text(expected, key)))
    {
        return Ok(None);
    }
    let Some(last) = rows
        .iter()
        .rev()
        .find(|r| value::text(r, "kind") == "acceptance")
    else {
        return Ok(None);
    };
    for (key, item) in expected
        .as_object()
        .ok_or_else(|| Error::new(2, "reuse expectation must be an object"))?
    {
        if last.get(key) != Some(item) {
            return Ok(None);
        }
    }
    if value::text(last, "status") != "pass"
        || last["exit"].as_i64() != Some(0)
        || last["timed_out"] != false
        || last["output_limited"] != false
        || !value::text(last, "reason").is_empty()
    {
        return Ok(None);
    }
    let ts = chrono::DateTime::parse_from_rfc3339(value::text(last, "ts"))
        .map_err(|_| Error::new(2, "invalid acceptance timestamp"))?
        .timestamp();
    if !(0..=600).contains(&(now_seconds - ts)) {
        return Ok(None);
    }
    Ok(Some(value::hash(oms_command_plan::json::default_bytes(
        &value::plan(last)?,
        true,
    )?)))
}
