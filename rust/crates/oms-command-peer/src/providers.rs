//! Provider identity, route selection, bounded native envelopes and account
//! selection. A model family is distinct from the executable transport.
use crate::{
    Context, Error, Result, V, fail, number,
    process::{Completed, Invocation},
    storage, text,
};
use std::os::unix::fs::MetadataExt;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Target {
    pub provider: String,
    pub model: String,
    pub optional: bool,
}
impl Target {
    pub fn parse(raw: &str) -> Result<Self> {
        let (raw, optional) = raw
            .strip_suffix(":optional")
            .map_or((raw, false), |s| (s, true));
        let (provider, model) = raw.split_once(":model=").unwrap_or((raw, ""));
        let provider = normalize(provider)?;
        if !model.is_empty() {
            model_name(model)?;
        }
        Ok(Self {
            provider,
            model: model.into(),
            optional,
        })
    }
}
pub fn normalize(raw: &str) -> Result<String> {
    let value = match raw {
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
    if value.is_empty()
        || value.len() > 64
        || value.contains("..")
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return fail("unsupported provider name");
    }
    Ok(value.into())
}
pub fn model_name(value: &str) -> Result<()> {
    if value.len() > 256
        || value.starts_with('-')
        || value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\')
    {
        fail("invalid model name")
    } else {
        Ok(())
    }
}
pub fn family(provider: &str, model: &str) -> String {
    let lower = model.to_lowercase();
    for (needle, family) in [
        ("gpt", "openai"),
        ("claude", "anthropic"),
        ("gemini", "google"),
        ("grok", "xai"),
        ("deepseek", "deepseek"),
        ("qwen", "alibaba"),
    ] {
        if lower.contains(needle) {
            return family.into();
        }
    }
    match provider {
        "codex" => "openai",
        "claude" => "anthropic",
        "antigravity" | "gemini" => "google",
        _ => provider,
    }
    .into()
}
pub fn binary(provider: &str) -> String {
    match provider {
        "antigravity" => "agy",
        "cursor" => "cursor-agent",
        "deepseek" => "dsh",
        "kiro" => "kiro-cli",
        "continue" => "cn",
        _ => provider,
    }
    .into()
}
pub fn discovered(provider: &str) -> bool {
    let binary = binary(provider);
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|p| {
        let p = p.join(&binary);
        std::fs::metadata(p)
            .ok()
            .is_some_and(|m| m.is_file() && m.mode() & 0o111 != 0)
    })
}
pub(crate) fn config(context: &Context<'_>, name: &str) -> Result<V> {
    config_from(context.source, name)
}
fn config_from(source: &Path, name: &str) -> Result<V> {
    let bytes = storage::read(source, Path::new("config").join(name).as_path(), false)?;
    V::parse(&bytes).map_err(Error::from)
}
#[derive(Clone, Debug)]
pub struct Route {
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub explicit: bool,
}
pub(crate) fn route(
    context: &Context<'_>,
    target: &Target,
    model: &str,
    effort: &str,
    role: &str,
) -> Result<Route> {
    route_with_source(context.source, target, model, effort, role)
}
pub fn resolve_route(
    source: &Path,
    provider: &str,
    model: &str,
    effort: &str,
    role: &str,
) -> Result<Route> {
    route_with_source(source, &Target::parse(provider)?, model, effort, role)
}
fn route_with_source(
    source: &Path,
    target: &Target,
    model: &str,
    effort: &str,
    role: &str,
) -> Result<Route> {
    if !["", "auto", "low", "medium", "high", "xhigh", "max", "ultra"].contains(&effort) {
        return fail("invalid reasoning effort");
    }
    let mut selected = if !model.is_empty() {
        model.to_owned()
    } else {
        target.model.clone()
    };
    let explicit = !selected.is_empty();
    let mut resolved = if effort == "auto" {
        String::new()
    } else {
        effort.into()
    };
    if selected.is_empty() {
        let policy = config_from(source, "panel-routing.json")?;
        let tier = policy
            .get("roles")
            .get(role)
            .get("tier")
            .text()
            .unwrap_or("deep");
        if let Some(row) = policy
            .get("tiers")
            .get(tier)
            .array()
            .iter()
            .find(|r| text(r, "provider") == target.provider && !r.get("public_only").truth())
        {
            selected = text(row, "model").into();
            if resolved.is_empty() {
                resolved = text(row, "effort").into();
            }
        } else {
            selected = "provider-default".into();
        }
    }
    model_name(&selected)?;
    if target.provider == "claude"
        && !resolved.is_empty()
        && !matches!(resolved.as_str(), "low" | "medium" | "high" | "max")
    {
        return fail("Claude does not support the requested effort");
    }
    Ok(Route {
        provider: target.provider.clone(),
        model: selected,
        effort: resolved,
        explicit,
    })
}
#[derive(Clone, Debug)]
pub struct Account {
    pub id: String,
    pub home: PathBuf,
    pub used: Option<f64>,
    pub running: usize,
    pub usable: bool,
    pub models: Option<BTreeSet<String>>,
}
pub fn select_account(
    accounts: &[Account],
    spread: bool,
    explicit: bool,
    model: &str,
) -> Option<Account> {
    let mut eligible = accounts
        .iter()
        .filter(|a| {
            a.usable
                && a.used.is_none_or(|v| v < 90.0)
                && (model == "provider-default"
                    || a.models.as_ref().is_none_or(|m| m.contains(model)))
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|a, b| {
        a.used
            .unwrap_or(100.0)
            .total_cmp(&b.used.unwrap_or(100.0))
            .then_with(|| {
                if spread && !explicit {
                    a.running.cmp(&b.running)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
    });
    eligible.first().map(|a| (*a).clone())
}
fn unique_keys(value: &oms_context::Json) -> bool {
    match value {
        oms_context::Json::Object(fields) => {
            let mut keys = BTreeSet::new();
            fields.iter().all(|(k, v)| keys.insert(k) && unique_keys(v))
        }
        oms_context::Json::Array(a) => a.iter().all(unique_keys),
        _ => true,
    }
}
fn keys_are(value: &V, allowed: &[&str]) -> bool {
    matches!(value,V::Obj(fields) if fields.keys().all(|k|allowed.iter().any(|s|k==s.as_bytes())))
}
fn quota(home: &Path) -> Option<f64> {
    let mut pending = vec![(home.join("sessions"), 0)];
    let mut newest = None;
    let mut seen = 0;
    while let Some((dir, depth)) = pending.pop() {
        if depth > 3 {
            continue;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries {
            let entry = entry.ok()?;
            seen += 1;
            if seen > 8192 {
                return None;
            }
            let kind = entry.file_type().ok()?;
            if kind.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl") {
                let modified = entry.metadata().ok()?.modified().ok()?;
                if newest.as_ref().is_none_or(|(stamp, _)| modified > *stamp) {
                    newest = Some((modified, entry.path()));
                }
            }
        }
    }
    let (_, path) = newest?;
    let bytes = storage::file(&path).ok()?;
    for line in bytes.split(|b| *b == b'\n').rev() {
        let Ok(row) = V::parse(line) else { continue };
        let limits = if row.has("rate_limits") {
            row.get("rate_limits")
        } else {
            row.get("payload").get("rate_limits")
        };
        if matches!(limits, V::Null) {
            continue;
        }
        if let Ok(observed) = chrono::DateTime::parse_from_rfc3339(text(&row, "timestamp")) {
            let age = crate::seconds() as i64 - observed.timestamp();
            if !(0..=604800).contains(&age) {
                return None;
            }
        } else {
            return None;
        }
        for key in ["primary", "secondary"] {
            let window = limits.get(key);
            if number(window.get("window_minutes")) != Some(10080.0) {
                continue;
            }
            let used = number(window.get("used_percent"))?;
            let reset = number(window.get("resets_at"))?;
            if !(0.0..=100.0).contains(&used) || reset <= crate::seconds() as f64 {
                return None;
            }
            return Some(used);
        }
        return None;
    }
    None
}
type AccountEnv = (Vec<(OsString, OsString)>, Option<String>);

fn account_environment(
    route: &Route,
    excluded: &BTreeSet<String>,
    repo: &Path,
) -> Result<AccountEnv> {
    if route.provider != "codex" || std::env::var_os("OMS_RESUME_SESSION").is_some() {
        return Ok((Vec::new(), None));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(2, "account home is unavailable"))?;
    if let Some(current) = std::env::var_os("CODEX_HOME")
        && PathBuf::from(current).canonicalize().ok() != home.join(".codex").canonicalize().ok()
    {
        return Ok((Vec::new(), None));
    }
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let path = root.join("oh-my-setting/provider-accounts.json");
    let bytes = match storage::file(&path) {
        Ok(bytes) => bytes,
        Err(_) if !path.exists() => return Ok((Vec::new(), None)),
        Err(e) => return Err(e),
    };
    let parsed = oms_context::parse_json(&bytes)
        .map_err(|_| Error::new(2, "invalid provider accounts configuration"))?;
    if !unique_keys(&parsed) {
        return fail("duplicate provider accounts configuration key");
    }
    let config = V::parse(&bytes)?;
    if !keys_are(&config, &["schema", "providers"])
        || !keys_are(config.get("providers"), &["codex"])
    {
        return fail("unknown or unsupported provider accounts configuration keys");
    }
    if config.get("schema") != &1usize.into() {
        return fail("unsupported provider accounts schema");
    }
    let settings = config.get("providers").get("codex");
    if matches!(settings, V::Null) {
        return Ok((Vec::new(), None));
    }
    if !keys_are(settings, &["policy", "accounts"]) {
        return fail("unknown account policy keys");
    }
    let policy = text(settings, "policy");
    if !["", "ordered", "spread"].contains(&policy) {
        return fail("unsupported provider accounts policy");
    }
    let entries = settings.get("accounts").array();
    if entries.is_empty() {
        return fail("provider accounts must be a non-empty list");
    }
    let running = if policy == "spread" && !route.explicit {
        running_accounts(repo)?
    } else {
        std::collections::BTreeMap::new()
    };
    let mut accounts = Vec::new();
    let mut ids = BTreeSet::new();
    let mut homes = BTreeSet::new();
    let id_re = cached_regex!(r"\A[a-z0-9][a-z0-9_-]{0,63}\z");
    for entry in entries {
        if !keys_are(entry, &["id", "home"]) || !entry.has("id") || !entry.has("home") {
            return fail("unknown or missing provider account fields");
        }
        let ident = text(entry, "id");
        let raw = Path::new(text(entry, "home"));
        if !id_re.is_match(ident) || !raw.is_absolute() {
            return fail("invalid provider account id or home");
        }
        let location = raw.canonicalize().unwrap_or_else(|_| raw.to_owned());
        if !ids.insert(ident.to_owned()) || !homes.insert(location.clone()) {
            return fail("duplicate provider account id or canonical home");
        }
        let usable = std::fs::metadata(&location).ok().is_some_and(|m| {
            m.is_dir() && m.uid() == rustix::process::getuid().as_raw() && m.mode() & 0o022 == 0
        }) && !excluded.contains(ident);
        let models = storage::read(&location, Path::new("models_cache.json"), true)
            .ok()
            .filter(|b| b.len() <= 4 * 1024 * 1024)
            .and_then(|b| V::parse(&b).ok())
            .and_then(|v| {
                let mut values = BTreeSet::new();
                if !matches!(v.get("models"), V::Arr(_)) {
                    return None;
                }
                for r in v.get("models").array() {
                    let slug = text(r, "slug");
                    if slug.is_empty() {
                        return None;
                    }
                    values.insert(slug.to_owned());
                }
                Some(values)
            });
        accounts.push(Account {
            id: ident.into(),
            used: quota(&location),
            running: running.get(ident).copied().unwrap_or(0),
            home: location,
            usable,
            models,
        });
    }
    match select_account(&accounts, policy == "spread", route.explicit, &route.model) {
        Some(account) => Ok((
            vec![("CODEX_HOME".into(), account.home.into_os_string())],
            Some(account.id),
        )),
        None if route.explicit => Ok((Vec::new(), Some("default".into()))),
        None => fail("no eligible Codex account"),
    }
}
pub fn answer_quality(raw: &str) -> &'static str {
    let mut from = 0;
    for (at, line) in raw.lines().enumerate() {
        if line == "## Output" {
            from = at + 1;
        }
    }
    let timeout = cached_regex!(
        r"^\[agy\] print timeout after .+ with turn in progress; returning partial output$",
    );
    if raw
        .lines()
        .skip(from)
        .any(|line| timeout.is_match(line.trim()))
    {
        return "truncated";
    }
    let stop = cached_regex!(
        r"^stop-reason: provider=\S+ reason=(\S+) subtype=\S+ is_error=([01])(?: category=\S+)?$",
    );
    for line in raw.lines().skip(from) {
        if let Some(capture) = stop.captures(line.trim()) {
            if &capture[2] == "1" || &capture[1] == "refusal" {
                return "blocked";
            }
            if [concat!("max_", "tok", "ens"), "stream_truncated"].contains(&&capture[1]) {
                return "truncated";
            }
            break;
        }
    }
    let noise = cached_regex!(concat!(
        r"^\s*(model-route:|model-result:|usage detail:|",
        r"tok",
        r"ens used|\[REDACTED|OpenAI Codex|\[untrusted |\[end untrusted |",
        r"workdir:|model:|provider:|approval:|sandbox:|reasoning effort:|",
        r"reasoning summaries:|session id:|-{3,}$|user$|DRY RUN)"
    ));
    let headings = cached_regex!(
        r"(?i)^(?:#{1,6}\s*)?(?:Answer|Alternatives|Tradeoffs|Evidence|Counterargument|Risks|Recommendation|Verification|Changed from previous round|Remaining disagreements):\s*$"
    );
    let refusal = cached_regex!(
        r"(?i)^(jetski: no output produced\b|add an allow-rule\b|alternatively, re-run with\b|.*\bcommand not found\b|(error|fatal)[: ].*\b(permission|not authenticated|not logged in|unauthorized|invalid api key|quota|rate limit)\b)"
    );
    let mut skip_next = false;
    let mut content = false;
    let mut only_headings = true;
    let mut only_refusals = true;
    for line in raw.lines() {
        if skip_next {
            skip_next = false;
            continue;
        }
        let trimmed = line.trim();
        if [
            "served model",
            "configured model",
            concat!("tok", "ens used"),
            "cost usd",
        ]
        .contains(&trimmed)
        {
            skip_next = true;
            continue;
        }
        if !trimmed.is_empty() && !noise.is_match(line) && !stop.is_match(trimmed) {
            content = true;
            only_headings = only_headings && headings.is_match(trimmed);
            only_refusals = only_refusals && refusal.is_match(trimmed);
        }
    }
    if !content || only_headings {
        "empty"
    } else if only_refusals {
        "blocked"
    } else {
        "ok"
    }
}
pub(crate) fn decode_output(provider: &str, raw: &[u8]) -> (String, bool) {
    let input = String::from_utf8_lossy(raw);
    let mut output = Vec::new();
    let mut recognized = false;
    let mut terminal = false;
    let mut stopped_early = false;
    for line in input.lines() {
        let Ok(row) = V::parse(line.as_bytes()) else {
            continue;
        };
        let kind = text(&row, "type");
        for reason in [
            row.get("stop_reason"),
            row.get("finish_reason"),
            row.get("message").get("stop_reason"),
            row.get("event").get("delta").get("stop_reason"),
        ] {
            if reason.text().is_some_and(|s| {
                matches!(s, "length" | "max_output" | "refusal" | "error" | "aborted")
                    || s.starts_with("max_")
            }) {
                stopped_early = true;
            }
        }
        if provider == "codex" {
            if matches!(
                kind,
                "thread.started" | "turn.started" | "item.started" | "error"
            ) {
                recognized = true;
            }
            if kind == "turn.started" {
                terminal = false;
            }
            if kind == "item.completed" {
                let item = row.get("item");
                if text(item, "type") == "agent_message" {
                    output.push(text(item, "text").to_owned());
                    recognized = true;
                }
            }
            if kind == "turn.completed" {
                terminal = true;
                recognized = true;
            }
            if kind == "turn.failed" {
                recognized = true;
                output.push(text(row.get("error"), "message").to_owned());
            }
        } else if provider == "claude" {
            if matches!(kind, "system" | "stream_event" | "user") {
                recognized = true;
            }
            if kind == "assistant" {
                for part in row.get("message").get("content").array() {
                    if text(part, "type") == "text" {
                        output.push(text(part, "text").to_owned());
                        recognized = true;
                    }
                }
            }
            if kind == "result" {
                recognized = true;
                terminal = !row.get("is_error").truth()
                    && !matches!(
                        text(&row, "subtype"),
                        "error_max_turns" | "error_during_execution"
                    );
                let result = text(&row, "result");
                if !result.is_empty() && output.last().is_none_or(|v| v != result) {
                    output.push(result.into());
                }
            }
        } else if provider == "pi" {
            if matches!(kind, "message_start" | "message_update") {
                recognized = true;
                terminal = false;
            }
            if kind == "message_end" || kind == "agent_end" {
                recognized = true;
            }
            let last = if kind == "agent_end" {
                row.get("messages")
                    .array()
                    .iter()
                    .rev()
                    .find(|m| text(m, "role") == "assistant")
            } else if kind == "message_end" && text(row.get("message"), "role") == "assistant" {
                Some(row.get("message"))
            } else {
                None
            };
            if let Some(message) = last {
                let answer = message
                    .get("content")
                    .array()
                    .iter()
                    .filter(|m| text(m, "type") == "text")
                    .map(|m| text(m, "text"))
                    .collect::<String>();
                if !answer.is_empty() && output.last() != Some(&answer) {
                    output.push(answer);
                }
                terminal = !matches!(
                    text(message, "stopReason"),
                    "error" | "aborted" | "length" | "max_output"
                ) && !text(message, "stopReason").starts_with("max_");
                if !terminal {
                    output.push(text(message, "errorMessage").into());
                }
            }
        } else if matches!(provider, "antigravity" | "gemini" | "qwen") {
            let value = row
                .get("response")
                .text()
                .or_else(|| row.get("result").text());
            if let Some(value) = value {
                recognized = true;
                terminal = true;
                output.push(value.into());
            }
        }
    }
    if recognized {
        (output.join("\n\n"), terminal && !stopped_early)
    } else {
        (input.into_owned(), true)
    }
}
pub(crate) fn execute(
    route: &Route,
    prompt: &[u8],
    cwd: &Path,
    write: bool,
    timeout: u64,
    account_repo: &Path,
    context: &Context<'_>,
) -> Result<Completed> {
    crate::safety::outbound(prompt)?;
    if !discovered(&route.provider) {
        return Err(Error::new(127, "provider command not found"));
    }
    if !write
        && [
            "muse",
            "kimi",
            "kiro",
            "goose",
            "cline",
            "kilo",
            "openhands",
            "auggie",
        ]
        .contains(&route.provider.as_str())
    {
        return fail("provider does not support read access through this transport");
    }
    let mut argv: Vec<OsString> = match route.provider.as_str() {
        "codex" => vec![
            "codex".into(),
            "exec".into(),
            "--json".into(),
            "--sandbox".into(),
            if write {
                "workspace-write"
            } else {
                "read-only"
            }
            .into(),
        ],
        "claude" => vec![
            "claude".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "-p".into(),
        ],
        "antigravity" => vec![
            "agy".into(),
            "--add-dir".into(),
            cwd.as_os_str().into(),
            "--sandbox".into(),
            "--print-timeout".into(),
            format!("{timeout}s").into(),
            "--output-format".into(),
            "json".into(),
        ],
        "pi" => [
            "pi",
            "--no-session",
            "--no-approve",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--mode",
            "json",
            "--tools",
            if write {
                "read,bash,edit,write,grep,find,ls"
            } else {
                "read,grep,find,ls"
            },
        ]
        .into_iter()
        .map(OsString::from)
        .collect(),
        "gemini" | "qwen" => vec![
            binary(&route.provider).into(),
            "--output-format".into(),
            "text".into(),
            "--sandbox".into(),
            "--approval-mode".into(),
            if write { "yolo" } else { "plan" }.into(),
        ],
        _ => return fail("native transport for this registered provider is not implemented"),
    };
    if route.model != "provider-default" {
        argv.extend(["--model".into(), route.model.clone().into()]);
    }
    if !route.effort.is_empty() {
        match route.provider.as_str() {
            "codex" => argv.extend([
                "-c".into(),
                format!("model_reasoning_effort=\"{}\"", route.effort).into(),
            ]),
            "pi" => argv.extend(["--thinking".into(), route.effort.clone().into()]),
            "claude" | "antigravity" => {
                argv.extend(["--effort".into(), route.effort.clone().into()])
            }
            _ => {}
        }
    }
    if !write {
        match route.provider.as_str() {
            "codex" => argv.extend([
                "--skip-git-repo-check".into(),
                "-c".into(),
                "web_search=\"disabled\"".into(),
                "-c".into(),
                "features.apps=false".into(),
            ]),
            "claude" => {
                let plans = format!(
                    "{{\"plansDirectory\":\".oms/tmp/claude-plans-{}\"}}",
                    crate::id("")?
                );
                argv.extend(["--settings".into(), plans.into()]);
                argv.extend([
                    "--permission-mode".into(),
                    "plan".into(),
                    "--strict-mcp-config".into(),
                    "--setting-sources".into(),
                    "".into(),
                    "--tools".into(),
                    "Read,Grep,Glob,Bash".into(),
                ]);
            }
            "qwen" => argv.push("--safe-mode".into()),
            _ => {}
        }
    }
    let input = if route.provider == "antigravity" || route.provider == "pi" {
        if prompt.len() > 120000 {
            return fail("provider argv prompt exceeds 120000-byte transport limit");
        }
        argv.extend([
            "--print".into(),
            String::from_utf8_lossy(prompt).into_owned().into(),
        ]);
        None
    } else {
        if route.provider == "codex" {
            argv.push("-".into());
        }
        Some(prompt.to_vec())
    };
    let remove_environment: Vec<OsString> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| {
            k.to_str().is_some_and(|s| {
                s.starts_with("GIT_")
                    || s.starts_with("OMS_APPROVAL_")
                    || s.starts_with("OMS_LANDING_")
                    || s.starts_with("OMS_LEASE_")
            })
        })
        .collect();
    let mut excluded = BTreeSet::new();
    for overflow in 0..2 {
        let (mut environment, account) = account_environment(route, &excluded, account_repo)?;
        environment.extend([
            ("OMS_HARNESS_CHILD".into(), "1".into()),
            ("OMS_HARNESS_DELEGATE_DEPTH".into(), "1".into()),
        ]);
        if route.provider == "pi" {
            environment.push(("PI_SKIP_VERSION_CHECK".into(), "1".into()));
        }
        if let Ok(base) = crate::process::git(context, cwd, &["rev-parse", "HEAD"]) {
            environment.push((
                "OMS_BASE_SHA".into(),
                String::from_utf8_lossy(&base).trim().into(),
            ));
        }
        let configured = configured_model(
            route,
            environment
                .iter()
                .find(|(key, _)| key == "CODEX_HOME")
                .map(|(_, value)| value.as_os_str()),
        );
        let mut result = context.executor.execute(
            Invocation {
                argv: argv.clone(),
                cwd: cwd.to_owned(),
                input: input.clone(),
                environment,
                remove_environment: remove_environment.clone(),
                timeout: Duration::from_secs(timeout),
                max_bytes: 64 * 1024 * 1024,
                merge: false,
            },
            context.interrupted,
        )?;
        let (decoded, terminal) = decode_output(&route.provider, &result.stdout);
        result.stdout = decoded.into_bytes();
        if !result.stdout.is_empty()
            && let Some(model) = configured
        {
            result
                .stdout
                .extend_from_slice(format!("\nconfigured model\n{model}\n").as_bytes());
        }
        result.complete &= terminal;
        if overflow == 0
            && capacity(&result)
            && let Some(account) = account.filter(|s| s != "default")
        {
            excluded.insert(account);
            continue;
        }
        return Ok(result);
    }
    fail("account capacity overflow exhausted")
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_with_fallback(
    route: &mut Route,
    fallback: &str,
    prompt: &[u8],
    cwd: &Path,
    write: bool,
    timeout: u64,
    account_repo: &Path,
    context: &Context<'_>,
) -> Result<Completed> {
    let result = execute(route, prompt, cwd, write, timeout, account_repo, context)?;
    if !fallback.is_empty() && fallback != route.model && capacity(&result) {
        model_name(fallback)?;
        route.model = fallback.into();
        route.explicit = true;
        execute(route, prompt, cwd, write, timeout, account_repo, context)
    } else {
        Ok(result)
    }
}

pub(crate) fn capacity(result: &Completed) -> bool {
    let body = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    )
    .to_lowercase();
    (result.code != 0 || !result.complete)
        && [
            "quota",
            "rate limit",
            "rate_limit",
            "capacity",
            "usage limit",
        ]
        .iter()
        .any(|word| body.contains(word))
}

fn configured_model(route: &Route, profile: Option<&std::ffi::OsStr>) -> Option<String> {
    if route.provider != "codex" || route.model != "provider-default" {
        return None;
    }
    let location = std::env::var_os("OMS_CODEX_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            profile
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".codex")
                })
                .join("config.toml")
        });
    let parent = location
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let raw = storage::root(parent)
        .ok()?
        .read_no_follow(
            Path::new(location.file_name()?),
            oms_safe_fs::ByteReadOptions {
                missing_ok: false,
                max_bytes: 4 * 1024 * 1024,
            },
        )
        .ok()?;
    let config = oms_toml::parse_bounded(&raw, 4 * 1024 * 1024).ok()?;
    let model = config.get("model")?.as_str()?;
    if !cached_regex!(r"\A[A-Za-z0-9][A-Za-z0-9._:+/-]{0,159}\z").is_match(model) {
        return None;
    }
    Some(model.to_owned())
}
fn running_accounts(repo: &Path) -> Result<std::collections::BTreeMap<String, usize>> {
    let state =
        oms_command_room::Services::state_repository(&oms_command_room::LocalServices, repo)
            .map_err(|e| Error::new(2, e.to_string()))?;
    let (_, rows) = oms_command_operations::load_projection(&state)?;
    let mut counts = std::collections::BTreeMap::new();
    for row in rows {
        let refs = row.get("refs");
        let provider = if text(refs, "panel_provider").is_empty() {
            text(&row, "provider")
        } else {
            text(refs, "panel_provider")
        };
        if provider != "codex"
            || !["worker", "researcher", "advisor", "reviewer"].contains(&text(refs, "panel_role"))
            || ![
                "queued",
                "starting",
                "working",
                "running",
                "waiting_input",
                "waiting_approval",
                "verifying",
            ]
            .contains(&text(&row, "state"))
        {
            continue;
        }
        let account = text(refs, "panel_account").to_owned();
        if !account.is_empty() {
            *counts.entry(account).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod bridge_route_tests {
    use super::*;
    #[test]
    fn route_uses_policy_and_target() {
        let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."));
        let route = resolve_route(source, "agy", "custom", "high", "worker").unwrap();
        assert_eq!(route.provider, "antigravity");
        assert_eq!(route.model, "custom");
        assert!(route.explicit);
        let automatic = resolve_route(source, "codex", "", "auto", "worker").unwrap();
        assert!(!automatic.model.is_empty());
        assert!(!automatic.explicit);
        assert!(resolve_route(source, "", "", "", "worker").is_err());
        assert!(resolve_route(source, "codex", "", "bad", "worker").is_err());
    }
}
