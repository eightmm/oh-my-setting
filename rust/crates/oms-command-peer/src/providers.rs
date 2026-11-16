//! Provider identity, route selection, bounded native envelopes and account
//! selection. A model family is distinct from the executable transport.
//! Custom transports use only marked adapters and require explicit write opt-in.
//! Aider, Antigravity and custom read calls require disposable repository state;
//! private prompt and environment files are removed when execution returns.
use crate::{
    Context, Error, Result, V, fail, number,
    process::{Completed, Invocation},
    storage, text,
};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    io::Write,
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
    let value = canonical_provider(raw);
    if !provider_id(value) || (builtin_binary(value).is_none() && !discovered(value)) {
        return fail("unsupported provider name");
    }
    Ok(value.into())
}
fn canonical_provider(raw: &str) -> &str {
    match raw {
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
    }
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
fn provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.contains("..")
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
fn builtin_binary(provider: &str) -> Option<&str> {
    Some(match provider {
        "antigravity" => "agy",
        "cursor" => "cursor-agent",
        "deepseek" => "dsh",
        "kiro" => "kiro-cli",
        "continue" => "cn",
        "codex" | "claude" | "grok" | "gemini" | "qwen" | "opencode" | "vibe" | "pi"
        | "copilot" | "droid" | "aider" | "muse" | "kimi" | "goose" | "cline" | "kilo"
        | "openhands" | "auggie" => provider,
        _ => return None,
    })
}
fn executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .ok()
        .is_some_and(|m| m.is_file() && m.mode() & 0o111 != 0)
}
fn on_path(name: &OsStr) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|p| executable(&p.join(name)))
}
fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}
fn binary_path(provider: &str) -> PathBuf {
    let provider = canonical_provider(provider);
    if !provider_id(provider) {
        return PathBuf::new();
    }
    if let Some(name) = builtin_binary(provider) {
        return if provider == "opencode" && !on_path(name.as_ref()) && on_path("opencode2".as_ref())
        {
            "opencode2".into()
        } else {
            name.into()
        };
    }
    let name = format!("oms-agent-adapter-{provider}");
    let base = nonempty_env("OMS_PROVIDER_ADAPTER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            nonempty_env("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(nonempty_env("HOME").unwrap_or_else(|| "/nonexistent".into()))
                        .join(".config")
                })
                .join("oh-my-setting/provider-adapters")
        });
    let direct = base.join(&name);
    if executable(&direct) {
        // A relative adapter directory is relative to the owner, not the child worktree.
        std::path::absolute(&direct).unwrap_or(direct)
    } else {
        name.into()
    }
}
pub fn binary(provider: &str) -> String {
    binary_path(provider).to_string_lossy().into_owned()
}
pub fn discovered(provider: &str) -> bool {
    let path = binary_path(provider);
    !path.as_os_str().is_empty()
        && if path.is_absolute() {
            executable(&path)
        } else {
            on_path(path.as_os_str())
        }
}
fn requires_read_isolation(provider: &str) -> bool {
    matches!(provider, "antigravity" | "aider") || builtin_binary(provider).is_none()
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
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SeatHealth {
    pub ok: u64,
    pub err: u64,
    pub latency_ms: u64,
    pub cooldown_until: u64,
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
    select_account_adaptive(
        accounts,
        spread,
        explicit,
        model,
        &std::collections::BTreeMap::new(),
        0,
    )
}

pub fn select_account_adaptive(
    accounts: &[Account],
    spread: bool,
    explicit: bool,
    model: &str,
    health: &std::collections::BTreeMap<String, SeatHealth>,
    now: u64,
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
        let ha = health.get(&a.id);
        let hb = health.get(&b.id);
        let cool_a = ha.is_some_and(|h| h.cooldown_until > now);
        let cool_b = hb.is_some_and(|h| h.cooldown_until > now);
        let pen_a = ha.map_or(0, |h| h.err.saturating_sub(h.ok));
        let pen_b = hb.map_or(0, |h| h.err.saturating_sub(h.ok));
        let ua = a.used.unwrap_or(100.0);
        let ub = b.used.unwrap_or(100.0);
        cool_a
            .cmp(&cool_b)
            .then_with(|| pen_a.cmp(&pen_b))
            .then_with(|| ua.total_cmp(&ub))
            .then_with(|| {
                if spread && !explicit {
                    a.running.cmp(&b.running)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                let la = ha.map_or(u64::MAX, |h| {
                    if h.latency_ms == 0 {
                        u64::MAX
                    } else {
                        h.latency_ms
                    }
                });
                let lb = hb.map_or(u64::MAX, |h| {
                    if h.latency_ms == 0 {
                        u64::MAX
                    } else {
                        h.latency_ms
                    }
                });
                la.cmp(&lb)
            })
    });
    eligible.first().map(|a| (*a).clone())
}

fn load_seat_health(repo: &Path) -> std::collections::BTreeMap<String, SeatHealth> {
    let Ok(bytes) = storage::read(repo, Path::new(".oms/seat-health.json"), true) else {
        return std::collections::BTreeMap::new();
    };
    if bytes.is_empty() || bytes.len() > 262_144 {
        return std::collections::BTreeMap::new();
    }
    let Ok(doc) = V::parse(&bytes) else {
        return std::collections::BTreeMap::new();
    };
    let mut map = std::collections::BTreeMap::new();
    if let V::Obj(fields) = doc.get("accounts") {
        for (key, val) in fields {
            let Ok(name) = std::str::from_utf8(key) else {
                continue;
            };
            map.insert(
                name.to_owned(),
                SeatHealth {
                    ok: number(val.get("ok")).map_or(0, |n| n.max(0.0) as u64),
                    err: number(val.get("err")).map_or(0, |n| n.max(0.0) as u64),
                    latency_ms: number(val.get("latency_ms")).map_or(0, |n| n.max(0.0) as u64),
                    cooldown_until: number(val.get("cooldown_until"))
                        .map_or(0, |n| n.max(0.0) as u64),
                },
            );
        }
    }
    map
}

fn record_seat_health(
    repo: &Path,
    seat: &str,
    ok: bool,
    latency_ms: u64,
    capacity_hit: bool,
    interrupted: &std::sync::atomic::AtomicBool,
) {
    if seat.is_empty() || !repo.join(".oms").is_dir() {
        return;
    }
    let rel = Path::new(".oms/seat-health.json");
    let mut map = load_seat_health(repo);
    let entry = map.entry(seat.to_owned()).or_default();
    if ok {
        entry.ok = entry.ok.saturating_add(1);
        entry.cooldown_until = 0;
        entry.latency_ms = if entry.latency_ms == 0 {
            latency_ms.max(1)
        } else {
            (entry.latency_ms * 3 + latency_ms.max(1)) / 4
        };
    } else {
        entry.err = entry.err.saturating_add(1);
        if capacity_hit {
            entry.cooldown_until = crate::seconds().saturating_add(300);
        }
    }
    let rows = map
        .into_iter()
        .map(|(k, h)| {
            (
                k,
                crate::obj([
                    ("ok", V::Num(h.ok.to_string())),
                    ("err", V::Num(h.err.to_string())),
                    ("latency_ms", V::Num(h.latency_ms.to_string())),
                    ("cooldown_until", V::Num(h.cooldown_until.to_string())),
                ]),
            )
        })
        .collect::<Vec<_>>();
    let doc = crate::obj([
        ("schema", V::Num("1".into())),
        (
            "accounts",
            V::Obj(rows.into_iter().map(|(k, v)| (k.into_bytes(), v)).collect()),
        ),
    ]);
    if let Ok(encoded) = crate::json(&doc) {
        let _ = storage::write(repo, rel, encoded, true, interrupted);
    }
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
    let health = load_seat_health(repo);
    match select_account_adaptive(
        &accounts,
        policy == "spread",
        route.explicit,
        &route.model,
        &health,
        crate::seconds(),
    ) {
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
struct ProviderScratch(PathBuf);
impl ProviderScratch {
    fn new(provider: &str, outside: Option<&Path>) -> Result<Self> {
        let mut base = std::env::temp_dir().canonicalize()?;
        if let Some(cwd) = outside {
            let cwd = cwd.canonicalize()?;
            if base.starts_with(&cwd) {
                base = Path::new("/tmp").canonicalize()?;
            }
            if base.starts_with(cwd) {
                return fail("private provider prompt state must be outside the worktree");
            }
        }
        let path = base.join(crate::id(&format!("oms-{provider}-"))?);
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let scratch = Self(path);
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))?;
        Ok(scratch)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.0.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        Ok(path)
    }
}
impl Drop for ProviderScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Transport {
    argv: Vec<OsString>,
    input: Option<Vec<u8>>,
    environment: Vec<(OsString, OsString)>,
    scratch: Option<ProviderScratch>,
}
impl Transport {
    fn args(&mut self, values: &[&str]) {
        self.argv.extend(values.iter().map(OsString::from));
    }
    fn option(&mut self, name: &str, value: impl AsRef<OsStr>) {
        self.argv.extend([name.into(), value.as_ref().into()]);
    }
    fn model(&mut self, route: &Route) {
        if route.model != "provider-default" {
            self.option("--model", &route.model);
        }
    }
    fn effort(&mut self, route: &Route, flag: &str) {
        if !route.effort.is_empty() {
            self.option(flag, &route.effort);
        }
    }
    fn prompt_file(&mut self, provider: &str, prompt: &[u8]) -> Result<PathBuf> {
        let scratch = ProviderScratch::new(provider, None)?;
        let path = scratch.file("prompt.md", prompt)?;
        self.scratch = Some(scratch);
        Ok(path)
    }
}
fn transport(
    route: &Route,
    prompt: &[u8],
    cwd: &Path,
    write: bool,
    timeout: u64,
) -> Result<Transport> {
    let provider = route.provider.as_str();
    let mut cmd = Transport {
        argv: vec![binary_path(provider).into_os_string()],
        input: Some(prompt.to_vec()),
        environment: Vec::new(),
        scratch: None,
    };
    let argv_prompt = if matches!(
        provider,
        "grok"
            | "opencode"
            | "deepseek"
            | "vibe"
            | "pi"
            | "copilot"
            | "muse"
            | "kimi"
            | "kiro"
            | "continue"
            | "goose"
            | "cline"
            | "kilo"
            | "openhands"
            | "auggie"
    ) || provider == "antigravity" && prompt.len() <= 120000
    {
        if prompt.len() > 120000 {
            return fail("provider argv prompt exceeds 120000-byte transport limit");
        }
        let value = std::str::from_utf8(prompt)
            .map_err(|_| Error::new(2, "provider argv prompt must be UTF-8"))?;
        if value.contains('\0') {
            return fail("provider argv prompt contains NUL");
        }
        value
    } else {
        ""
    };
    if matches!(
        provider,
        "deepseek"
            | "vibe"
            | "pi"
            | "copilot"
            | "droid"
            | "aider"
            | "muse"
            | "kimi"
            | "kiro"
            | "continue"
            | "goose"
            | "cline"
            | "kilo"
            | "openhands"
            | "auggie"
    ) {
        cmd.input = None;
    }
    match provider {
        "codex" => {
            cmd.args(&["exec"]);
            cmd.model(route);
            if !route.effort.is_empty() {
                cmd.option("-c", format!("model_reasoning_effort=\"{}\"", route.effort));
            }
            cmd.args(&["--json"]);
            if !write {
                cmd.args(&["-c", "web_search=\"disabled\"", "-c", "features.apps=false"]);
            }
            cmd.args(&[
                "--sandbox",
                if write {
                    "workspace-write"
                } else {
                    "read-only"
                },
            ]);
            if !write {
                cmd.args(&["--skip-git-repo-check"]);
            }
            if let Some(resume) = nonempty_env("OMS_RESUME_SESSION") {
                cmd.args(&["resume", "--all"]);
                cmd.argv.push(resume);
            }
            cmd.args(&["-"]);
        }
        "claude" => {
            if let Some(resume) = nonempty_env("OMS_RESUME_SESSION") {
                cmd.option("--resume", resume);
            }
            if !write {
                cmd.args(&["--permission-mode", "plan"]);
            }
            cmd.model(route);
            cmd.effort(route, "--effort");
            if !write {
                cmd.args(&[
                    "--strict-mcp-config",
                    "--setting-sources",
                    "",
                    "--tools",
                    "Read,Grep,Glob,Bash",
                ]);
                let plans = format!(
                    "{{\"plansDirectory\":\".oms/tmp/claude-plans-{}\"}}",
                    crate::id("")?
                );
                cmd.option("--settings", plans);
            }
            cmd.args(&["--output-format", "stream-json", "--verbose", "-p"]);
        }
        "antigravity" => {
            cmd.model(route);
            cmd.option("--add-dir", cwd);
            cmd.effort(route, "--effort");
            cmd.args(&["--sandbox"]);
            cmd.option("--print-timeout", format!("{timeout}s"));
            cmd.args(&["--output-format", "json"]);
            if prompt.len() > 120000 {
                if std::env::var("OMS_PEER_INTERACTIVE").as_deref() == Ok("1") {
                    return fail("provider argv prompt exceeds 120000-byte transport limit");
                }
                let scratch = ProviderScratch::new("agy-prompt", Some(cwd))?;
                let path = scratch.file("prompt.md", prompt)?;
                cmd.option("--add-dir", &scratch.0);
                cmd.option(
                    "--print",
                    format!(
                        "Read {} in full and follow it as your complete instructions.",
                        path.display()
                    ),
                );
                cmd.scratch = Some(scratch);
            } else {
                cmd.option("--print", argv_prompt);
            }
        }
        "cursor" => {
            cmd.args(&["--print", "--output-format", "text"]);
            cmd.option("--workspace", cwd);
            cmd.args(&["--sandbox", "enabled"]);
            cmd.model(route);
            cmd.args(if write {
                &["--force"]
            } else {
                &["--mode", "ask"]
            });
        }
        "grok" => {
            cmd.args(&["--no-auto-update"]);
            cmd.option("--cwd", cwd);
            cmd.args(&["--output-format", "plain"]);
            cmd.model(route);
            cmd.effort(route, "--effort");
            cmd.args(if write {
                &[
                    "--always-approve",
                    "--sandbox",
                    "workspace",
                    "--deny",
                    "Bash(git push*)",
                ]
            } else {
                &[
                    "--permission-mode",
                    "dontAsk",
                    "--allow",
                    "Read",
                    "--allow",
                    "Grep",
                    "--allow",
                    "Bash(git *)",
                    "--deny",
                    "Edit",
                    "--deny",
                    "WebFetch",
                    "--deny",
                    "WebSearch",
                    "--sandbox",
                    "strict",
                    "--no-memory",
                    "--no-subagents",
                    "--disable-web-search",
                ]
            });
            cmd.option("-p", argv_prompt);
        }
        "gemini" | "qwen" => {
            cmd.args(&["--output-format", "text", "--sandbox"]);
            cmd.model(route);
            if provider == "qwen" && !write {
                cmd.args(&["--safe-mode"]);
            }
            cmd.args(&["--approval-mode", if write { "yolo" } else { "plan" }]);
        }
        "opencode" => {
            cmd.args(&["run", "--format", "default"]);
            cmd.model(route);
            cmd.args(if write {
                &["--agent", "build", "--auto"]
            } else {
                &["--agent", "plan"]
            });
            cmd.argv.push(argv_prompt.into());
        }
        "deepseek" => {
            cmd.environment.extend([
                (
                    "DSH_PERMISSION_MODE".into(),
                    if write {
                        "workspace-write"
                    } else {
                        "read-only"
                    }
                    .into(),
                ),
                ("DSH_TELEMETRY_DISABLED".into(), "1".into()),
            ]);
            cmd.args(&["--profile", "headless", argv_prompt]);
        }
        "vibe" => {
            cmd.args(&[
                "--trust",
                "--max-turns",
                "24",
                "--output",
                "text",
                "--agent",
                if write { "accept-edits" } else { "plan" },
                "--enabled-tools",
                "read_file",
                "--enabled-tools",
                "grep",
            ]);
            if write {
                cmd.args(&["--enabled-tools", "write_file", "--enabled-tools", "edit"]);
            }
            cmd.option("--prompt", argv_prompt);
        }
        "pi" => {
            cmd.environment
                .push(("PI_SKIP_VERSION_CHECK".into(), "1".into()));
            cmd.args(&[
                "--no-session",
                "--no-approve",
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--mode",
                "json",
            ]);
            cmd.model(route);
            cmd.effort(route, "--thinking");
            cmd.args(&[
                "--tools",
                if write {
                    "read,bash,edit,write,grep,find,ls"
                } else {
                    "read,grep,find,ls"
                },
            ]);
            cmd.option("--print", argv_prompt);
        }
        "copilot" => {
            cmd.args(&[
                "-s",
                "--no-ask-user",
                "--disable-builtin-mcps",
                "--disallow-temp-dir",
            ]);
            cmd.model(route);
            cmd.effort(route, "--effort");
            cmd.args(&["--available-tools"]);
            cmd.args(if write {
                &[
                    "view,grep,glob,edit,create,apply_patch",
                    "--allow-tool",
                    "read,write",
                    "--deny-tool",
                    "shell",
                    "--deny-tool",
                    "url",
                    "--deny-tool",
                    "memory",
                ]
            } else {
                &[
                    "view,grep,glob",
                    "--deny-tool",
                    "write",
                    "--deny-tool",
                    "shell",
                    "--deny-tool",
                    "url",
                    "--deny-tool",
                    "memory",
                ]
            });
            cmd.option("-p", argv_prompt);
        }
        "droid" => {
            cmd.args(&["exec"]);
            cmd.option("--cwd", cwd);
            cmd.args(&["--output-format", "text", "--disable-builtin-skills"]);
            cmd.model(route);
            cmd.effort(route, "--reasoning-effort");
            if write {
                cmd.args(&["--auto", "low"]);
            }
            let path = cmd.prompt_file(provider, prompt)?;
            cmd.option("-f", path);
        }
        "aider" => {
            let path = cmd.prompt_file(provider, prompt)?;
            let scratch = cmd.scratch.as_ref().expect("prompt scratch");
            let empty = scratch.file("empty.env", b"")?;
            let input = scratch.0.join("input.history");
            let chat = scratch.0.join("chat.history");
            cmd.args(&[
                "--no-auto-commits",
                "--no-dirty-commits",
                "--no-gitignore",
                "--no-suggest-shell-commands",
                "--no-auto-lint",
                "--no-auto-test",
                "--no-check-update",
                "--no-detect-urls",
                "--no-pretty",
                "--no-stream",
                "--no-analytics",
                "--no-restore-chat-history",
            ]);
            cmd.option("--env-file", empty);
            cmd.option("--input-history-file", input);
            cmd.option("--chat-history-file", chat);
            cmd.model(route);
            cmd.effort(route, "--reasoning-effort");
            cmd.args(if write {
                &["--chat-mode", "code", "--yes-always", "--no-dry-run"]
            } else {
                &["--chat-mode", "ask", "--dry-run"]
            });
            cmd.option("--message-file", path);
        }
        "muse" => {
            cmd.args(&["exec"]);
            cmd.model(route);
            cmd.effort(route, "--reasoning-effort");
            if write {
                cmd.args(&["--disable-approval"]);
            }
            cmd.argv.push(argv_prompt.into());
        }
        "kimi" => {
            cmd.model(route);
            cmd.option("-p", argv_prompt);
        }
        "kiro" => {
            cmd.args(&["chat", "--v3", "--no-interactive"]);
            cmd.model(route);
            cmd.effort(route, "--effort");
            if write {
                cmd.args(&["--trust-all-tools"]);
            }
            cmd.argv.push(argv_prompt.into());
        }
        "continue" => {
            cmd.option("-p", argv_prompt);
            cmd.model(route);
            cmd.args(if write { &["--auto"] } else { &["--readonly"] });
        }
        "goose" => {
            cmd.environment.push(("GOOSE_MODE".into(), "auto".into()));
            cmd.args(&["run", "--no-session"]);
            cmd.model(route);
            cmd.option("-t", argv_prompt);
        }
        "cline" => {
            cmd.model(route);
            cmd.effort(route, "--thinking");
            cmd.args(&["--auto-approve", "true", argv_prompt]);
        }
        "kilo" => {
            cmd.args(&["run", "--auto"]);
            cmd.model(route);
            cmd.effort(route, "--variant");
            cmd.argv.push(argv_prompt.into());
        }
        "openhands" => cmd.args(&["--headless", "-t", argv_prompt]),
        "auggie" => {
            cmd.environment
                .push(("AUGMENT_DISABLE_AUTO_UPDATE".into(), "1".into()));
            cmd.args(&["--print"]);
            cmd.model(route);
            cmd.args(&[
                "--permission",
                "edit:allow",
                "--permission",
                "write:allow",
                "--permission",
                "terminal:allow",
                argv_prompt,
            ]);
        }
        _ => {
            let path = cmd.prompt_file("adapter", prompt)?;
            cmd.args(&["run", "--access", if write { "write" } else { "read" }]);
            cmd.option("--workdir", cwd);
            cmd.option("--prompt-file", path);
            cmd.model(route);
            cmd.effort(route, "--effort");
        }
    }
    Ok(cmd)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_env(
    route: &Route,
    prompt: &[u8],
    cwd: &Path,
    write: bool,
    timeout: u64,
    account_repo: &Path,
    extra_env: &[(OsString, OsString)],
    context: &Context<'_>,
) -> Result<Completed> {
    crate::safety::outbound(prompt)?;
    if !discovered(&route.provider) {
        return Err(Error::new(127, "provider command not found"));
    }
    if write
        && builtin_binary(&route.provider).is_none()
        && !std::env::var("OMS_PROVIDER_WRITE_ADAPTERS")
            .unwrap_or_default()
            .split(',')
            .any(|name| name.split_whitespace().collect::<String>() == route.provider)
    {
        return fail("custom provider does not support write access without explicit opt-in");
    }
    if route.model != "provider-default"
        && matches!(route.provider.as_str(), "deepseek" | "vibe" | "openhands")
    {
        return fail("provider has no documented per-invocation model override");
    }
    // Every read caller that can snapshot a repository already passes a disposable
    // worktree. Fail closed when a caller falls back to its original checkout.
    if !write
        && requires_read_isolation(&route.provider)
        && cwd.canonicalize()? == account_repo.canonicalize()?
    {
        return fail("provider read access requires a disposable repository worktree");
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
    let transport = transport(route, prompt, cwd, write, timeout)?;
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
        environment.extend(transport.environment.clone());
        environment.extend(extra_env.iter().cloned());
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
        let mut exec_argv = transport.argv.clone();
        let sandbox_mode = match std::env::var("OMS_WORKER_SANDBOX").as_deref() {
            Ok("strict") => Some(oms_supervisor::sandbox::SandboxMode::Strict),
            Ok("auto") => Some(oms_supervisor::sandbox::SandboxMode::Auto),
            _ => None,
        };
        if let Some(mode) = sandbox_mode
            && !(mode == oms_supervisor::sandbox::SandboxMode::Auto && route.provider == "codex")
            && let Some((program, rest)) = exec_argv.split_first()
        {
            let mut extra_rw_dirs = Vec::new();
            for (k, v) in &environment {
                if k == "CARGO_TARGET_DIR" || k == "CODEX_HOME" {
                    let p = PathBuf::from(v);
                    if p.is_dir() {
                        extra_rw_dirs.push(p);
                    }
                }
            }
            if let Some(home) = std::env::var_os("HOME") {
                for sub in [".claude", ".config", ".cache"] {
                    let p = PathBuf::from(&home).join(sub);
                    if p.is_dir() {
                        extra_rw_dirs.push(p);
                    }
                }
            }
            let spec = oms_supervisor::sandbox::SandboxSpec {
                worktree: cwd.to_owned(),
                extra_rw_dirs,
                allow_network: true,
            };
            let (wrapped_prog, wrapped_args, _) =
                oms_supervisor::sandbox::wrap_command(program, rest, &spec, mode)
                    .map_err(|e| Error::new(2, e))?;
            exec_argv = std::iter::once(wrapped_prog).chain(wrapped_args).collect();
        }
        let started = std::time::Instant::now();
        let mut result = context.executor.execute(
            Invocation {
                argv: exec_argv,
                cwd: cwd.to_owned(),
                input: transport.input.clone(),
                environment,
                remove_environment: remove_environment.clone(),
                timeout: Duration::from_secs(timeout),
                max_bytes: 64 * 1024 * 1024,
                merge: false,
            },
            context.interrupted,
        )?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
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
        let seat_key = account.as_deref().unwrap_or(&route.provider);
        let cap_hit = capacity(&result);
        record_seat_health(
            account_repo,
            seat_key,
            result.code == 0 && result.complete && !cap_hit,
            elapsed_ms,
            cap_hit,
            context.interrupted,
        );
        if overflow == 0
            && cap_hit
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
    execute_with_fallback_env(
        route,
        fallback,
        prompt,
        cwd,
        write,
        timeout,
        account_repo,
        &[],
        context,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_with_fallback_env(
    route: &mut Route,
    fallback: &str,
    prompt: &[u8],
    cwd: &Path,
    write: bool,
    timeout: u64,
    account_repo: &Path,
    extra_env: &[(OsString, OsString)],
    context: &Context<'_>,
) -> Result<Completed> {
    let result = execute_env(
        route,
        prompt,
        cwd,
        write,
        timeout,
        account_repo,
        extra_env,
        context,
    )?;
    if !fallback.is_empty() && fallback != route.model && capacity(&result) {
        model_name(fallback)?;
        route.model = fallback.into();
        route.explicit = true;
        execute_env(
            route,
            prompt,
            cwd,
            write,
            timeout,
            account_repo,
            extra_env,
            context,
        )
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
    use crate::process::Executor;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::Command,
        sync::{Mutex, atomic::AtomicBool},
    };

    fn isolated_transport(name: &str, mode: &str, body: impl FnOnce(&Path, &Path)) {
        if std::env::var("TRANSPORT_CASE").as_deref() == Ok(name) {
            if std::env::var("TRANSPORT_MODE").as_deref() == Ok(mode) {
                let root = PathBuf::from(std::env::var_os("HOME").unwrap());
                body(&root.join("repo"), &root.join("isolated"));
            }
            return;
        }
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = std::env::temp_dir().join(format!(
            "oms-native-transport-{}-{name}-{mode}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Fixture(root);
        for dir in ["repo", "isolated", "tmp", "bin"] {
            fs::create_dir(fixture.0.join(dir)).unwrap();
        }
        for bin in [
            "codex",
            "claude",
            "agy",
            "cursor-agent",
            "grok",
            "gemini",
            "qwen",
            "opencode",
            "dsh",
            "vibe",
            "pi",
            "copilot",
            "droid",
            "aider",
            "muse",
            "kimi",
            "kiro-cli",
            "cn",
            "goose",
            "cline",
            "kilo",
            "openhands",
            "auggie",
            "oms-agent-adapter-fixture",
            "oms-agent-adapter-blocked",
            "phantom",
        ] {
            let path = fixture.0.join("bin").join(bin);
            fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .args([
                "--exact",
                &format!("providers::bridge_route_tests::{name}"),
                "--nocapture",
            ])
            .env("TRANSPORT_CASE", name)
            .env("TRANSPORT_MODE", mode)
            .env("HOME", &fixture.0)
            .env("PATH", fixture.0.join("bin"))
            .env(
                "OMS_RESUME_SESSION",
                if mode == "resume" {
                    "fixture-session"
                } else {
                    ""
                },
            )
            .env(
                "TMPDIR",
                fixture
                    .0
                    .join(if mode == "inside" { "isolated" } else { "tmp" }),
            )
            .env(
                "OMS_PEER_INTERACTIVE",
                if mode == "interactive" { "1" } else { "0" },
            )
            .env("OMS_PROVIDER_WRITE_ADAPTERS", "other, fixture ,")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
    }
    #[derive(Default)]
    struct Capture(Mutex<Vec<Invocation>>);
    impl Executor for Capture {
        fn execute(&self, request: Invocation, _: &AtomicBool) -> Result<Completed> {
            let git = request.argv[0] == "git";
            if !git {
                for pair in request.argv.windows(2) {
                    if ["--prompt-file", "--message-file", "-f", "--env-file"]
                        .iter()
                        .any(|flag| pair[0] == *flag)
                    {
                        let path = Path::new(&pair[1]);
                        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
                        assert_eq!(
                            fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
                            0o700
                        );
                    }
                }
                self.0.lock().unwrap().push(request);
            }
            Ok(Completed {
                code: i32::from(git),
                stdout: b"fixture answer\n".to_vec(),
                stderr: Vec::new(),
                complete: true,
                elapsed: Duration::ZERO,
            })
        }
    }
    fn direct(
        route: &Route,
        prompt: &[u8],
        write: bool,
        repo: &Path,
        cwd: &Path,
        capture: &Capture,
    ) -> Result<Completed> {
        let stop = AtomicBool::new(false);
        let context = Context {
            cwd,
            source: repo,
            interrupted: &stop,
            executor: capture,
        };
        execute_env(route, prompt, cwd, write, 12, repo, &[], &context)
    }
    #[test]
    fn native_write_transports() {
        for mode in ["normal", "resume"] {
            isolated_transport("native_write_transports", mode, |repo, cwd| {
                let cases = [
                    (
                        "codex",
                        "exec|{model}|{effort}|--json|--sandbox|workspace-write|-",
                        "-c|model_reasoning_effort=\"high\"",
                    ),
                    (
                        "claude",
                        "{model}|{effort}|--output-format|stream-json|--verbose|-p",
                        "--effort|high",
                    ),
                    (
                        "antigravity",
                        "{model}|--add-dir|{cwd}|{effort}|--sandbox|--print-timeout|12s|--output-format|json|--print|complete fixture",
                        "--effort|high",
                    ),
                    (
                        "cursor",
                        "--print|--output-format|text|--workspace|{cwd}|--sandbox|enabled|{model}|--force",
                        "",
                    ),
                    (
                        "grok",
                        "--no-auto-update|--cwd|{cwd}|--output-format|plain|{model}|{effort}|--always-approve|--sandbox|workspace|--deny|Bash(git push*)|-p|complete fixture",
                        "--effort|high",
                    ),
                    (
                        "gemini",
                        "--output-format|text|--sandbox|{model}|--approval-mode|yolo",
                        "",
                    ),
                    (
                        "qwen",
                        "--output-format|text|--sandbox|{model}|--approval-mode|yolo",
                        "",
                    ),
                    (
                        "opencode",
                        "run|--format|default|{model}|--agent|build|--auto|complete fixture",
                        "",
                    ),
                    ("deepseek", "--profile|headless|complete fixture", ""),
                    (
                        "vibe",
                        "--trust|--max-turns|24|--output|text|--agent|accept-edits|--enabled-tools|read_file|--enabled-tools|grep|--enabled-tools|write_file|--enabled-tools|edit|--prompt|complete fixture",
                        "",
                    ),
                    (
                        "pi",
                        "--no-session|--no-approve|--no-extensions|--no-skills|--no-prompt-templates|--mode|json|{model}|{effort}|--tools|read,bash,edit,write,grep,find,ls|--print|complete fixture",
                        "--thinking|high",
                    ),
                    (
                        "copilot",
                        "-s|--no-ask-user|--disable-builtin-mcps|--disallow-temp-dir|{model}|{effort}|--available-tools|view,grep,glob,edit,create,apply_patch|--allow-tool|read,write|--deny-tool|shell|--deny-tool|url|--deny-tool|memory|-p|complete fixture",
                        "--effort|high",
                    ),
                    (
                        "droid",
                        "exec|--cwd|{cwd}|--output-format|text|--disable-builtin-skills|{model}|{effort}|--auto|low|-f|{file}",
                        "--reasoning-effort|high",
                    ),
                    (
                        "aider",
                        "--no-auto-commits|--no-dirty-commits|--no-gitignore|--no-suggest-shell-commands|--no-auto-lint|--no-auto-test|--no-check-update|--no-detect-urls|--no-pretty|--no-stream|--no-analytics|--no-restore-chat-history|--env-file|{env}|--input-history-file|{input}|--chat-history-file|{chat}|{model}|{effort}|--chat-mode|code|--yes-always|--no-dry-run|--message-file|{file}",
                        "--reasoning-effort|high",
                    ),
                    (
                        "muse",
                        "exec|{model}|{effort}|--disable-approval|complete fixture",
                        "--reasoning-effort|high",
                    ),
                    ("kimi", "{model}|-p|complete fixture", ""),
                    (
                        "kiro",
                        "chat|--v3|--no-interactive|{model}|{effort}|--trust-all-tools|complete fixture",
                        "--effort|high",
                    ),
                    ("continue", "-p|complete fixture|{model}|--auto", ""),
                    ("goose", "run|--no-session|{model}|-t|complete fixture", ""),
                    (
                        "cline",
                        "{model}|{effort}|--auto-approve|true|complete fixture",
                        "--thinking|high",
                    ),
                    (
                        "kilo",
                        "run|--auto|{model}|{effort}|complete fixture",
                        "--variant|high",
                    ),
                    ("openhands", "--headless|-t|complete fixture", ""),
                    (
                        "auggie",
                        "--print|{model}|--permission|edit:allow|--permission|write:allow|--permission|terminal:allow|complete fixture",
                        "",
                    ),
                    (
                        "fixture",
                        "run|--access|write|--workdir|{cwd}|--prompt-file|{file}|{model}|{effort}",
                        "--effort|high",
                    ),
                ];
                for (provider, pattern, flag) in cases {
                    for exact in [false, true] {
                        let native_model = ["deepseek", "vibe", "openhands"].contains(&provider);
                        let route = Route {
                            provider: provider.into(),
                            model: if exact && !native_model {
                                "fixture-model"
                            } else {
                                "provider-default"
                            }
                            .into(),
                            effort: if exact { "high" } else { "" }.into(),
                            explicit: true,
                        };
                        let capture = Capture::default();
                        assert_eq!(
                            direct(&route, b"complete fixture", true, repo, cwd, &capture)
                                .unwrap()
                                .code,
                            0
                        );
                        let calls = capture.0.lock().unwrap();
                        assert_eq!(calls.len(), 1);
                        let request = &calls[0];
                        let file = request
                            .argv
                            .windows(2)
                            .find(|p| {
                                ["--prompt-file", "--message-file", "-f"]
                                    .iter()
                                    .any(|f| p[0] == *f)
                            })
                            .map(|p| PathBuf::from(&p[1]));
                        let scratch = file
                            .as_ref()
                            .and_then(|p| p.parent())
                            .unwrap_or(Path::new(""));
                        let mut expanded = pattern
                            .replace(
                                "{model}",
                                if exact && !native_model {
                                    "--model|fixture-model"
                                } else {
                                    ""
                                },
                            )
                            .replace("{effort}", if exact { flag } else { "" });
                        if mode == "resume" {
                            if provider == "codex" {
                                expanded = expanded.strip_suffix('-').unwrap().to_owned()
                                    + "resume|--all|fixture-session|-";
                            } else if provider == "claude" {
                                expanded = "--resume|fixture-session|".to_owned() + &expanded;
                            }
                        }
                        let expected: Vec<OsString> = expanded
                            .split('|')
                            .filter(|s| !s.is_empty())
                            .map(|s| match s {
                                "{cwd}" => cwd.as_os_str().into(),
                                "{file}" => file.as_ref().unwrap().as_os_str().into(),
                                "{env}" => scratch.join("empty.env").into_os_string(),
                                "{input}" => scratch.join("input.history").into_os_string(),
                                "{chat}" => scratch.join("chat.history").into_os_string(),
                                _ => s.into(),
                            })
                            .collect();
                        assert_eq!(request.argv[1..], expected, "{provider} exact={exact}");
                        if file.is_some() {
                            assert!(!scratch.exists());
                        }
                        for (key, value) in match provider {
                            "deepseek" => vec![
                                ("DSH_PERMISSION_MODE", "workspace-write"),
                                ("DSH_TELEMETRY_DISABLED", "1"),
                            ],
                            "pi" => vec![("PI_SKIP_VERSION_CHECK", "1")],
                            "goose" => vec![("GOOSE_MODE", "auto")],
                            "auggie" => vec![("AUGMENT_DISABLE_AUTO_UPDATE", "1")],
                            _ => vec![],
                        } {
                            assert!(
                                request.environment.contains(&(key.into(), value.into())),
                                "{provider}"
                            );
                        }
                    }
                }
                for provider in ["deepseek", "vibe", "openhands", "blocked", "phantom"] {
                    let route = Route {
                        provider: provider.into(),
                        model: "fixture-model".into(),
                        effort: String::new(),
                        explicit: true,
                    };
                    let capture = Capture::default();
                    assert!(
                        direct(&route, b"inspect", true, repo, cwd, &capture).is_err(),
                        "{provider}"
                    );
                    assert!(capture.0.lock().unwrap().is_empty());
                }
                for provider in ["antigravity", "aider", "fixture"] {
                    let route = Route {
                        provider: provider.into(),
                        model: "provider-default".into(),
                        effort: String::new(),
                        explicit: false,
                    };
                    let capture = Capture::default();
                    assert!(direct(&route, b"inspect", false, repo, repo, &capture).is_err());
                    let alias = cwd.join(format!("{provider}-alias"));
                    std::os::unix::fs::symlink(repo, &alias).unwrap();
                    assert!(direct(&route, b"inspect", false, repo, &alias, &capture).is_err());
                    assert!(capture.0.lock().unwrap().is_empty());
                }
                for provider in ["droid", "aider", "fixture"] {
                    let route = Route {
                        provider: provider.into(),
                        model: "provider-default".into(),
                        effort: String::new(),
                        explicit: false,
                    };
                    let capture = Capture::default();
                    std::thread::scope(|scope| {
                        for _ in 0..2 {
                            scope.spawn(|| {
                                direct(&route, b"inspect", true, repo, cwd, &capture).unwrap();
                            });
                        }
                    });
                    let calls = capture.0.lock().unwrap();
                    let dirs: BTreeSet<_> = calls
                        .iter()
                        .flat_map(|call| call.argv.windows(2))
                        .filter(|pair| {
                            ["-f", "--message-file", "--prompt-file"]
                                .iter()
                                .any(|flag| pair[0] == *flag)
                        })
                        .map(|pair| PathBuf::from(&pair[1]).parent().unwrap().to_owned())
                        .collect();
                    assert_eq!(dirs.len(), 2);
                    assert!(dirs.iter().all(|path| !path.exists()));
                }
            });
        }
    }
    #[test]
    fn native_prompt_limits_and_cleanup() {
        for mode in ["normal", "interactive", "inside"] {
            isolated_transport("native_prompt_limits_and_cleanup", mode, |repo, cwd| {
                for provider in [
                    "grok",
                    "opencode",
                    "deepseek",
                    "vibe",
                    "pi",
                    "copilot",
                    "muse",
                    "kimi",
                    "kiro",
                    "continue",
                    "goose",
                    "cline",
                    "kilo",
                    "openhands",
                    "auggie",
                ] {
                    let route = Route {
                        provider: provider.into(),
                        model: "provider-default".into(),
                        effort: String::new(),
                        explicit: false,
                    };
                    for prompt in [
                        vec![],
                        vec![b'x'; 120000],
                        vec![b'x'; 120001],
                        vec![255],
                        b"a\0b".to_vec(),
                    ] {
                        let capture = Capture::default();
                        let result = direct(&route, &prompt, true, repo, cwd, &capture);
                        let valid = prompt.len() <= 120000
                            && std::str::from_utf8(&prompt).is_ok()
                            && !prompt.contains(&0);
                        assert_eq!(result.is_ok(), valid, "{provider}, {} bytes", prompt.len());
                        assert_eq!(capture.0.lock().unwrap().len(), usize::from(valid));
                    }
                }
                let route = Route {
                    provider: "antigravity".into(),
                    model: "provider-default".into(),
                    effort: String::new(),
                    explicit: false,
                };
                for size in [0, 120000, 120001] {
                    let capture = Capture::default();
                    let result = direct(&route, &vec![b'x'; size], true, repo, cwd, &capture);
                    assert_eq!(result.is_ok(), mode != "interactive" || size <= 120000);
                    let calls = capture.0.lock().unwrap();
                    if let Some(call) = calls.first() {
                        let dirs: Vec<_> = call
                            .argv
                            .windows(2)
                            .filter(|p| p[0] == "--add-dir")
                            .map(|p| PathBuf::from(&p[1]))
                            .collect();
                        if size > 120000 {
                            assert_eq!(dirs.len(), 2);
                            assert!(!dirs[1].starts_with(cwd));
                            assert!(!dirs[1].exists());
                        } else {
                            assert_eq!(dirs, vec![cwd.to_owned()]);
                        }
                    }
                }
            });
        }
    }
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

    #[test]
    fn adaptive_routing_prefers_healthy_low_latency_accounts_and_strict_sandbox() {
        let accounts = vec![
            Account {
                id: "slow-or-cooled".into(),
                home: PathBuf::from("/tmp/a1"),
                used: Some(20.0),
                running: 0,
                usable: true,
                models: None,
            },
            Account {
                id: "fast-healthy".into(),
                home: PathBuf::from("/tmp/a2"),
                used: Some(20.0),
                running: 0,
                usable: true,
                models: None,
            },
        ];
        let mut health = std::collections::BTreeMap::new();
        health.insert(
            "slow-or-cooled".into(),
            SeatHealth {
                ok: 1,
                err: 0,
                latency_ms: 900,
                cooldown_until: 2000,
            },
        );
        health.insert(
            "fast-healthy".into(),
            SeatHealth {
                ok: 3,
                err: 0,
                latency_ms: 120,
                cooldown_until: 0,
            },
        );
        let picked =
            select_account_adaptive(&accounts, true, false, "provider-default", &health, 1000)
                .unwrap();
        assert_eq!(picked.id, "fast-healthy");

        // When cooldown has expired, lower latency still wins ties at identical quota.
        let picked_after =
            select_account_adaptive(&accounts, true, false, "provider-default", &health, 3000)
                .unwrap();
        assert_eq!(picked_after.id, "fast-healthy");

        if std::env::var_os("SANDBOX_STRICT_PROBE").as_deref() == Some(OsStr::new("1")) {
            let dir = std::env::temp_dir().join(format!("oms-sbx-{}", std::process::id()));
            let _ = fs::create_dir_all(&dir);
            let route = Route {
                provider: "claude".into(),
                model: "provider-default".into(),
                effort: String::new(),
                explicit: false,
            };
            let capture = Capture::default();
            let err = direct(&route, b"probe", true, &dir, &dir, &capture).unwrap_err();
            let _ = fs::remove_dir_all(&dir);
            assert!(err.message.contains("sandbox backend"), "{}", err.message);
            return;
        }
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "providers::bridge_route_tests::adaptive_routing_prefers_healthy_low_latency_accounts_and_strict_sandbox",
            ])
            .env("SANDBOX_STRICT_PROBE", "1")
            .env("OMS_WORKER_SANDBOX", "strict")
            .env("OMS_BWRAP_BIN", "/nonexistent-oms-bwrap-bin")
            .env("OMS_SANDBOX_EXEC_BIN", "/nonexistent-oms-sandbox-exec")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
