//! Parent-owned integration points. Implementations must not grant model authority.
use crate::value::Value;
use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// Original operation-log creation order, never sorted by update time. A read
/// guard pins the producer's consistent projection until `check` and drop.
/// Lock order for a room writer is room append lock, then operations read guard.
pub trait OperationsRead {
    fn attempts(&self) -> &[Attempt];
    fn check(&self) -> Result<()>;
}
#[derive(Clone, Debug)]
pub struct Attempt {
    pub attempt_id: String,
    pub tool: String,
    pub provider: String,
    pub state: String,
    pub terminal: bool,
    pub reason_code: Option<String>,
    pub created_at: String,
    pub refs: Value,
}
impl Attempt {
    /// Do not coerce absent/malformed terminal flags into a release proof.
    pub fn from_projection_row(row: &Value) -> Result<Self> {
        let field = |name: &str| {
            row[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("invalid native operations projection field"))
        };
        let refs = row
            .get("refs")
            .cloned()
            .unwrap_or_else(|| crate::value::obj([]));
        if refs.as_object().is_none() {
            return Err(Error::new("invalid native operations references"));
        }
        let reason_code = if row["reason_code"].is_null() {
            None
        } else {
            Some(field("reason_code")?)
        };
        Ok(Self {
            attempt_id: field("attempt_id")?,
            tool: field("tool")?,
            provider: field("provider")?,
            state: field("state")?,
            terminal: row["terminal"]
                .as_bool()
                .ok_or_else(|| Error::new("invalid native operations terminal flag"))?,
            reason_code,
            created_at: row
                .get("created_at")
                .map(|_| field("created_at"))
                .transpose()?
                .unwrap_or_default(),
            refs,
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum App {
    Codex,
    Claude,
}
impl App {
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}
#[derive(Debug)]
pub struct DeliveryRequest<'a> {
    pub repository: &'a Path,
    pub app: App,
    pub text: &'a str,
    pub consumer: Option<&'a str>,
    pub allow_wakeup: bool,
}
/// Preserve the producer's public receipt fields. Room validates its status,
/// uncertainty and optional turn identity before appending its own receipt.
pub struct DeliveryReceipt {
    pub status: String,
    pub delivery_unknown: bool,
    pub turn_id: Option<String>,
    pub fields: Value,
}
/// Services are borrowed once by the command/core, not discovered by executing
/// another runtime. Missing integrations refuse before external delivery.
pub trait Services: Sync {
    fn state_repository(&self, path: &Path) -> Result<PathBuf>;
    fn canonical_provider(&self, provider: &str) -> Result<String>;
    fn current_claude_session(&self) -> Result<String> {
        Err(Error::new("current native identity service is unavailable"))
    }
    fn operations<'a>(&'a self, _repo: &Path) -> Result<Box<dyn OperationsRead + 'a>> {
        Err(Error::new(
            "native operations projection service is unavailable",
        ))
    }
    fn delivery_available(&self, _app: App) -> bool {
        false
    }
    fn deliver(&self, _request: DeliveryRequest<'_>) -> Result<DeliveryReceipt> {
        Err(Error::new("app delivery service is unavailable"))
    }
}
/// Local-only native defaults. Managed scratch/shared-state resolution belongs
/// to the parent's resolver; recognized scratch roots refuse rather than split
/// a canonical room across two repositories.
pub struct LocalServices;
impl Services for LocalServices {
    fn state_repository(&self, path: &Path) -> Result<PathBuf> {
        let local = work_repository(path)?;
        if let Some(parent) = local.parent() {
            let name = parent.file_name().and_then(|v| v.to_str()).unwrap_or("");
            let suffix = name.rsplit('.').next().unwrap_or("");
            let leaf = local.file_name().and_then(|v| v.to_str()).unwrap_or("");
            if name.starts_with("oh-my-setting-scratch.")
                && (leaf == "wt" || leaf == format!("wt-{suffix}"))
                && !crate::Session::from_env().child
            {
                return Err(Error::new(
                    "managed scratch repository resolution service is required",
                ));
            }
        }
        Ok(local)
    }
    fn canonical_provider(&self, provider: &str) -> Result<String> {
        let provider = match provider {
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
            v => v,
        };
        if [
            "codex",
            "claude",
            "antigravity",
            "cursor",
            "grok",
            "gemini",
            "qwen",
            "opencode",
            "deepseek",
            "vibe",
            "pi",
            "copilot",
            "droid",
            "aider",
            "muse",
            "kimi",
            "kiro",
            "continue",
            "goose",
            "cline",
            "kilo",
            "openhands",
            "auggie",
        ]
        .contains(&provider)
        {
            return Ok(provider.to_owned());
        }
        // Custom adapters require an explicitly registered executable; no CLI is run.
        if provider.is_empty()
            || provider.len() > 64
            || provider.contains("..")
            || !provider.as_bytes()[0].is_ascii_lowercase()
                && !provider.as_bytes()[0].is_ascii_digit()
            || !provider
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        {
            return Err(Error::new("join needs a registered provider or adapter"));
        }
        let filename = format!("oms-agent-adapter-{provider}");
        let adapter = std::env::var_os("OMS_PROVIDER_ADAPTER_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
                    .map(|p| p.join("oh-my-setting/provider-adapters"))
            });
        let paths = adapter
            .into_iter()
            .map(|p| p.join(&filename))
            .chain(
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                    .map(|p| p.join(&filename)),
            )
            .collect::<Vec<_>>();
        if paths.iter().any(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file())
                && rustix::fs::accessat(
                    rustix::fs::CWD,
                    p,
                    rustix::fs::Access::EXEC_OK,
                    rustix::fs::AtFlags::EACCESS,
                )
                .is_ok()
        }) {
            Ok(provider.to_owned())
        } else {
            Err(Error::new("join needs a registered provider or adapter"))
        }
    }
}
/// Fixed read-only Git query. Paths remain OS strings; repository-local config
/// cannot change the arguments. Inherited Git redirection is refused.
pub fn work_repository(path: &Path) -> Result<PathBuf> {
    use std::io::Read;
    use std::os::unix::ffi::OsStringExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    for k in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        if std::env::var_os(k).is_some() {
            return Err(Error::new(
                "inherited Git redirection is unsupported for room resolution",
            ));
        }
    }
    let path = std::fs::canonicalize(path).map_err(|_| Error::new("repository is unavailable"))?;
    if !path.is_dir() {
        return Err(Error::new("repository must be a directory"));
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::new("native Git repository query unavailable"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::new(
                    "native Git repository query failed or timed out",
                ));
            }
        }
    };
    if !status.success() {
        return Ok(path);
    }
    let mut out = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| Error::new("native Git query has no output"))?
        .take(65537)
        .read_to_end(&mut out)
        .map_err(|_| Error::new("native Git query read failed"))?;
    if out.len() > 65536 {
        return Err(Error::new("native Git repository query is oversized"));
    }
    while out.last() == Some(&b'\n') {
        out.pop();
    }
    out.retain(|b| *b != b'\r');
    std::fs::canonicalize(PathBuf::from(std::ffi::OsString::from_vec(out)))
        .map_err(|_| Error::new("native Git repository root unavailable"))
}
