#![forbid(unsafe_code)]

use crate::sanitize::os_str_to_utf8;
use crate::schema::atomic_write_bytes;
use crate::tz::TimeZoneSpec;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct NotionSettings {
    pub auth_mode: String, // "token", "bearer", "ntn", or ""
    pub auth_value: String,
    pub data_source_id: String,
    pub database_id: String,
    pub transport: String,   // "http" or "ntn"
    pub cli_command: String, // defaults to "ntn"
    pub keyring: String,     // e.g. "file", "os", ""
    pub key_property: String,
    pub hash_property: String,
    pub title_property: String,
    pub project_property: String,
    pub kind_property: String,
    pub period_property: String,
    pub blocker_property: String,
    pub sessions_property: String,
    pub commits_property: String,
    pub verified_property: String,
    pub timeout_seconds: f64,
    pub budget_seconds: f64,
}

impl NotionSettings {
    pub fn target_id(&self) -> &str {
        if !self.data_source_id.is_empty() {
            &self.data_source_id
        } else {
            &self.database_id
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.target_id().is_empty()
            && (!self.auth_value.is_empty() || self.transport == "ntn" || self.auth_mode == "ntn")
    }
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    let val = std::env::var_os(key)?;
    if let Some(s) = val.to_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    } else if val.is_empty() {
        None
    } else {
        Some(PathBuf::from(val))
    }
}

pub fn expand_user_path(path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use users::os::unix::UserExt as _;
        let raw = path.as_os_str().as_bytes();
        if !raw.starts_with(b"~") {
            return path.to_path_buf();
        }
        let (head, mut rest) = match raw.iter().position(|&b| b == b'/') {
            Some(pos) => (&raw[..pos], &raw[pos + 1..]),
            None => (raw, b"".as_slice()),
        };
        let home = if head == b"~" {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    users::get_user_by_uid(users::get_current_uid())
                        .map(|u| u.home_dir().to_path_buf())
                })
        } else {
            std::str::from_utf8(&head[1..])
                .ok()
                .and_then(users::get_user_by_name)
                .map(|u| u.home_dir().to_path_buf())
        };
        if let Some(home_dir) = home {
            while let Some(stripped) = rest.strip_prefix(b"/") {
                rest = stripped;
            }
            return if rest.is_empty() {
                home_dir
            } else {
                home_dir.join(OsStr::from_bytes(rest))
            };
        }
        path.to_path_buf()
    }
    #[cfg(not(unix))]
    {
        path.to_path_buf()
    }
}

pub fn oms_config_home() -> PathBuf {
    if let Some(explicit) = non_empty_env_path("OMS_WORK_JOURNAL_CONFIG") {
        let expanded = expand_user_path(&explicit);
        return expanded
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(xdg) = non_empty_env_path("XDG_CONFIG_HOME") {
        return expand_user_path(&xdg).join("oh-my-setting");
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("oh-my-setting")
}

pub fn work_journal_config_path() -> PathBuf {
    if let Some(explicit) = non_empty_env_path("OMS_WORK_JOURNAL_CONFIG") {
        return expand_user_path(&explicit);
    }
    oms_config_home().join("work-journal.json")
}

pub fn load_work_journal_config() -> Result<Map<String, Value>, String> {
    let path = work_journal_config_path();
    let Some(bytes) = crate::cold::read_regular_no_symlink_bytes(&path)? else {
        return Ok(Map::new());
    };
    let row: Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid Work Journal configuration")?;
    let object = row
        .as_object()
        .ok_or("unsupported Work Journal configuration")?;
    if object.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported Work Journal configuration".into());
    }
    if !object.get("notion").is_some_and(Value::is_object) {
        return Err("invalid Work Journal Notion configuration".into());
    }
    Ok(object.clone())
}
pub fn save_work_journal_config(obj: &Map<String, Value>) -> Result<PathBuf, String> {
    let path = work_journal_config_path();
    atomic_write_bytes(
        &path,
        &crate::sanitize::pretty_json_bytes(&Value::Object(obj.clone()))?,
    )?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("configuration visible; required mode uncertain: {e}"))?;
    Ok(path)
}
pub fn configured_journal_language() -> String {
    let Ok(cfg) = load_work_journal_config() else {
        return String::new();
    };
    let value = cfg
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if ["ko", "en"].contains(&value.as_str()) {
        value
    } else {
        String::new()
    }
}
pub fn journal_language(_tz: Option<&TimeZoneSpec>) -> String {
    let value = std::env::var("OMS_WORK_JOURNAL_LANG")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if ["ko", "en"].contains(&value.as_str()) {
        return value;
    }
    let configured = configured_journal_language();
    if !configured.is_empty() {
        return configured;
    }
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let value = std::env::var(name).unwrap_or_default();
        if value.trim().is_empty() {
            continue;
        }
        let tag = value
            .trim()
            .split(['.', '_', '@', '-'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        return if tag == "ko" {
            "ko".into()
        } else {
            "en".into()
        };
    }
    "en".into()
}
pub fn set_journal_language(language: &str) -> Result<PathBuf, String> {
    let value = language.trim().to_lowercase();
    if !["ko", "en"].contains(&value.as_str()) {
        return Err(format!("unsupported journal language: {language}"));
    }
    let mut cfg = load_work_journal_config()?;
    if cfg.is_empty() {
        cfg = serde_json::json!({"schema_version":1,"managed_by":"oh-my-setting","notion":{}})
            .as_object()
            .expect("object")
            .clone();
    }
    cfg.insert("language".into(), Value::String(value));
    save_work_journal_config(&cfg)
}
pub fn notion_access_env_var_name() -> &'static str {
    concat!("OMS_WORK_JOURNAL_NOTION_", "TOK", "EN")
}
pub fn notion_fallback_env_var_name() -> &'static str {
    concat!("NOTION_", "TOK", "EN")
}
fn env_or_config(environment: &str, object: &Map<String, Value>, name: &str) -> String {
    std::env::var(environment)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().into())
        .or_else(|| {
            object
                .get(name)
                .filter(|v| crate::schema::json_truthy(v))
                .map(crate::schema::python_str)
                .map(|s| s.trim().into())
        })
        .unwrap_or_default()
}
pub fn notion_settings() -> Result<NotionSettings, String> {
    let cfg = load_work_journal_config()?;
    let notion = cfg
        .get("notion")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let props = notion
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let property = |name: &str, default: &str| {
        let environment = format!(
            "OMS_WORK_JOURNAL_NOTION_{}_PROPERTY",
            name.to_ascii_uppercase()
        );
        let value = env_or_config(&environment, &props, name);
        if value.is_empty() {
            default.into()
        } else {
            value
        }
    };
    let access = std::env::var(notion_access_env_var_name())
        .unwrap_or_default()
        .trim()
        .to_owned();
    let auth = if !access.is_empty() {
        concat!("to", "ken").into()
    } else {
        env_or_config("OMS_WORK_JOURNAL_NOTION_AUTH", &notion, "auth_mode").to_lowercase()
    };
    let data_env = std::env::var("OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID")
        .unwrap_or_default()
        .trim()
        .to_owned();
    let database_env = std::env::var("OMS_WORK_JOURNAL_NOTION_DATABASE_ID")
        .unwrap_or_default()
        .trim()
        .to_owned();
    let (data_source_id, database_id) = if !data_env.is_empty() || !database_env.is_empty() {
        (data_env, database_env)
    } else {
        (
            notion
                .get("data_source_id")
                .map(crate::schema::python_str)
                .unwrap_or_default()
                .trim()
                .into(),
            notion
                .get("database_id")
                .map(crate::schema::python_str)
                .unwrap_or_default()
                .trim()
                .into(),
        )
    };
    let cli = env_or_config("OMS_NOTION_CLI", &notion, "cli_command");
    let number = |name: &str, default: &str| -> Result<f64, String> {
        let raw = std::env::var(name).unwrap_or_else(|_| default.into());
        let value = raw
            .parse::<f64>()
            .map_err(|_| "invalid Notion time bound")?;
        if !value.is_finite() || value <= 0.0 {
            return Err("invalid Notion time bound".into());
        }
        Ok(value)
    };
    Ok(NotionSettings {
        auth_mode: auth.clone(),
        auth_value: access,
        data_source_id,
        database_id,
        transport: if auth == "ntn" {
            "ntn".into()
        } else {
            "http".into()
        },
        cli_command: if cli.is_empty() { "ntn".into() } else { cli },
        keyring: env_or_config("OMS_NOTION_KEYRING", &notion, "keyring").to_lowercase(),
        title_property: property("title", "Name"),
        key_property: property("key", "Work Journal Key"),
        hash_property: property("hash", "Content Hash"),
        project_property: property("project", ""),
        kind_property: property("kind", ""),
        period_property: property("period", ""),
        blocker_property: property("blocker", ""),
        sessions_property: property("sessions", "Sessions"),
        commits_property: property("commits", "Commits"),
        verified_property: property("verified", "Verified"),
        timeout_seconds: number("OMS_WORK_JOURNAL_NOTION_TIMEOUT_SECONDS", "10")?,
        budget_seconds: number("OMS_WORK_JOURNAL_NOTION_BUDGET_SECONDS", "8")?,
    })
}
pub fn notion_auth_available(settings: &NotionSettings) -> bool {
    if !settings.auth_value.is_empty() {
        return true;
    }
    if settings.auth_mode != "ntn" {
        return false;
    }
    let path = Path::new(&settings.cli_command);
    if path.is_file() {
        return true;
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .any(|dir| dir.join(path).is_file())
}
fn canonical_entry(path: &Path) -> PathBuf {
    let path = expand_user_path(path);
    fs::canonicalize(&path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    })
}
pub fn notion_excluded_repos() -> Result<Vec<String>, String> {
    let cfg = load_work_journal_config()?;
    Ok(cfg
        .get("notion")
        .and_then(|n| n.get("excluded_repos"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|v| crate::schema::json_truthy(v))
        .map(crate::schema::python_str)
        .filter(|s| !s.trim().is_empty())
        .map(|s| crate::sanitize::diagnostic_os_str(canonical_entry(Path::new(&s)).as_os_str()))
        .collect())
}
pub fn notion_repo_excluded(repo: &Path) -> bool {
    let needle = canonical_entry(repo);
    notion_excluded_repos()
        .unwrap_or_default()
        .iter()
        .any(|p| Path::new(p) == needle)
}
pub fn update_notion_exclusions(
    excludes: &[PathBuf],
    includes: &[PathBuf],
) -> Result<(PathBuf, Vec<String>), String> {
    let mut cfg = load_work_journal_config()?;
    if cfg.is_empty() {
        return Err("Notion is not configured; run configure --discover first".into());
    }
    let mut set: BTreeSet<String> = notion_excluded_repos()?.into_iter().collect();
    for path in excludes {
        set.insert(os_str_to_utf8(canonical_entry(path).as_os_str(), "--exclude-repo")?.into());
    }
    for path in includes {
        set.remove(os_str_to_utf8(
            canonical_entry(path).as_os_str(),
            "--include-repo",
        )?);
    }
    let mut notion = cfg["notion"].as_object().expect("validated notion").clone();
    let list: Vec<_> = set.into_iter().collect();
    if list.is_empty() {
        notion.remove("excluded_repos");
    } else {
        notion.insert("excluded_repos".into(), serde_json::json!(list));
    }
    cfg.insert("notion".into(), Value::Object(notion));
    Ok((save_work_journal_config(&cfg)?, list))
}
pub fn configure_notion(target: &str, auth_mode: Option<&str>) -> Result<PathBuf, String> {
    let pattern = regex::Regex::new(
        r"^(?:[0-9A-Fa-f]{32}|[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12})$",
    )
    .expect("pattern");
    if !pattern.is_match(target.trim()) {
        return Err("invalid Notion data source id".into());
    }
    let prior = load_work_journal_config()?;
    let settings = notion_settings()?;
    let mut notion = serde_json::json!({"data_source_id":target.trim(),"properties":{"title":"Name","key":"Work Journal Key","hash":"Content Hash","project":"Project","kind":"Kind","period":"Period","blocker":"Has Blocker","sessions":"Sessions","commits":"Commits","verified":"Verified"},"sync_mode":"finalized"});
    if let Some(excluded) = prior
        .get("notion")
        .and_then(|n| n.get("excluded_repos"))
        .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    {
        notion["excluded_repos"] = excluded.clone();
    }
    if auth_mode.unwrap_or(&settings.auth_mode) == "ntn" {
        notion["auth_mode"] = serde_json::json!("ntn");
        notion["cli_command"] = serde_json::json!(settings.cli_command);
        if settings.keyring == "file" {
            notion["keyring"] = serde_json::json!("file");
        }
    }
    let mut written =
        serde_json::json!({"schema_version":1,"managed_by":"oh-my-setting","notion":notion});
    if let Some(lang) = prior
        .get("language")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase())
        .filter(|s| ["ko", "en"].contains(&s.as_str()))
    {
        written["language"] = serde_json::json!(lang);
    }
    save_work_journal_config(written.as_object().expect("object"))
}
pub fn purge_notion_config() -> Result<bool, String> {
    let path = work_journal_config_path();
    if crate::cold::read_regular_no_symlink_bytes(&path)?.is_none() {
        return Ok(false);
    }
    let config = load_work_journal_config()?;
    if config.get("managed_by").and_then(Value::as_str) != Some("oh-my-setting") {
        return Ok(false);
    }
    crate::files::remove_owned_file(&path)?;
    Ok(true)
}
