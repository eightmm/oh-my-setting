//! Single-attempt local host requests; ambiguous mutation outcomes never retry.
use crate::{Error, Result, Value, require};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::{
    fs::{FileTypeExt, MetadataExt},
    net::UnixStream,
};
use std::path::{Path, PathBuf};
use std::time::Duration;
pub const MAX_RESPONSE: usize = 1024 * 1024;
pub fn herdr_active() -> bool {
    std::env::var_os("HERDR_ENV").is_some_and(|v| v == "1")
        && std::env::var_os("HERDR_SOCKET_PATH").is_some()
        && !std::env::var("OMS_PANEL_HOST")
            .ok()
            .is_some_and(|v| ["inline", "tmux"].contains(&v.as_str()))
}
#[derive(Clone, Debug)]
pub struct Host {
    pub socket: PathBuf,
}
impl Host {
    pub fn from_env() -> Result<Self> {
        require(
            herdr_active(),
            "Herdr host requires an existing local POSIX Herdr pane",
        )?;
        Ok(Self {
            socket: std::env::var_os("HERDR_SOCKET_PATH")
                .map(PathBuf::from)
                .ok_or_else(|| Error::new("Herdr socket unavailable"))?,
        })
    }
    pub fn socket_digest(&self) -> Result<String> {
        Ok(crate::digest(
            std::fs::canonicalize(&self.socket)
                .map_err(Error::new)?
                .as_os_str()
                .as_encoded_bytes(),
        ))
    }
    pub fn request(&self, method: &str, params: &Value) -> Result<Value> {
        let info = std::fs::symlink_metadata(&self.socket)
            .map_err(|_| Error::new("Herdr socket unavailable"))?;
        require(
            info.file_type().is_socket() && info.uid() == rustix::process::getuid().as_raw(),
            "Herdr socket is not an owned local socket",
        )?;
        let id = crate::value::unique()?;
        let body =
            crate::value::encoded(&serde_json::json!({"id":id,"method":method,"params":params}))?;
        require(
            body.len() <= MAX_RESPONSE,
            "Herdr request exceeds local host limit",
        )?;
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|_| Error::new("Herdr connection unavailable"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(Error::new)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .map_err(Error::new)?;
        let after = std::fs::symlink_metadata(&self.socket).map_err(Error::new)?;
        require(
            info.dev() == after.dev() && info.ino() == after.ino(),
            "Herdr socket changed",
        )?;
        stream.write_all(&body).map_err(|_| Error {
            publication_uncertain: true,
            ..Error::new("Herdr request outcome unknown; do not replay")
        })?;
        let mut bytes = vec![];
        let mut reader = BufReader::new(stream);
        loop {
            let available = reader.fill_buf().map_err(|_| Error {
                publication_uncertain: true,
                ..Error::new("Herdr outcome unknown; do not replay")
            })?;
            require(
                !available.is_empty(),
                "Herdr disconnected; host outcome unknown",
            )?;
            let n = available
                .iter()
                .position(|&v| v == b'\n')
                .map(|v| v + 1)
                .unwrap_or(available.len());
            require(
                bytes.len().saturating_add(n) <= MAX_RESPONSE,
                "Herdr response exceeds local limit",
            )?;
            let done = available[n - 1] == b'\n';
            bytes.extend_from_slice(&available[..n]);
            reader.consume(n);
            if done {
                break;
            }
        }
        let response = crate::decode(&bytes, MAX_RESPONSE)?;
        require(
            response["id"] == id
                && response.get("error").is_none()
                && response["result"].is_object(),
            "Herdr response identity/result unavailable",
        )?;
        Ok(response["result"].clone())
    }
    pub fn pane(&self, ident: &str) -> Result<Value> {
        let valid = regex::Regex::new(r"^[A-Za-z0-9_-]+:p[A-Za-z0-9_-]+$").map_err(Error::new)?;
        require(valid.is_match(ident), "invalid Herdr pane identity")?;
        let result =
            self.request("pane.get", &serde_json::json!({"pane_id":ident}))?["pane"].clone();
        require(
            result["pane_id"] == ident
                && ["terminal_id", "workspace_id", "tab_id"].iter().all(|k| {
                    result[k]
                        .as_str()
                        .is_some_and(|v| !v.is_empty() && v.len() <= 160)
                }),
            "Herdr pane identity unavailable",
        )?;
        Ok(result)
    }
    pub fn current(&self, repo: &Path) -> Result<Value> {
        let pane = self.pane(
            &std::env::var("HERDR_PANE_ID")
                .map_err(|_| Error::new("current Herdr pane unavailable"))?,
        )?;
        let cwd = pane["foreground_cwd"]
            .as_str()
            .or_else(|| pane["cwd"].as_str())
            .ok_or_else(|| Error::new("pane cwd unavailable"))?;
        require(
            std::fs::canonicalize(cwd).map_err(Error::new)?
                == std::fs::canonicalize(repo).map_err(Error::new)?,
            "Herdr pane is outside selected repository",
        )?;
        Ok(pane)
    }
    pub fn focus(&self, pane: &str) -> Result<Value> {
        crate::parent_only()?;
        self.pane(pane)?;
        self.request("pane.focus", &serde_json::json!({"pane_id":pane}))
    }
    pub fn read(&self, pane: &str) -> Result<String> {
        self.pane(pane)?;
        let result = self.request(
            "pane.read",
            &serde_json::json!({"pane_id":pane,"source":"visible","lines":8}),
        )?;
        let read = &result["read"];
        require(read["pane_id"] == pane, "Herdr read pane changed")?;
        crate::value::redact(
            read["text"]
                .as_str()
                .filter(|v| v.len() <= 16384)
                .unwrap_or_default()
                .as_bytes(),
        )
    }
    pub fn zoom(&self, pane: &str, enabled: bool) -> Result<Value> {
        crate::parent_only()?;
        self.pane(pane)?;
        self.request(
            "pane.zoom",
            &serde_json::json!({"pane_id":pane,"mode":if enabled { "on" } else { "off" }}),
        )
    }
}

/// A failed host query is unknown, never proof of absence.
pub fn absent<S: crate::Services + ?Sized>(
    repo: &Path,
    attempt: &Value,
    service: &S,
) -> Result<bool> {
    let refs = &attempt["refs"];
    if refs["panel_host"] == "inline" {
        return Ok(crate::release::identity_gone(refs, "wrapper"));
    }
    if refs["panel_host"] != "tmux" {
        return Ok(false);
    }
    let pane = refs["panel_pane_id"].as_str().unwrap_or_default();
    if !pane
        .strip_prefix('%')
        .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        || !crate::hex(refs["panel_socket_digest"].as_str().unwrap_or_default(), 64)
    {
        return Ok(crate::release::boot_changed(refs, "wrapper"));
    }
    if crate::release::identity_gone(refs, "tmux_server") {
        return Ok(true);
    }
    let stop = std::sync::atomic::AtomicBool::new(false);
    let run = |values: &[&str]| -> Result<String> {
        let mut argv = vec!["tmux".into()];
        argv.extend(crate::services::args(values));
        let mut request = crate::services::ExecutionRequest::new(argv, repo);
        request.timeout = Duration::from_secs(3);
        request.output_limit = MAX_RESPONSE;
        let output = service.execute(&request, &stop)?;
        output.success()?;
        String::from_utf8(output.stdout).map_err(Error::new)
    };
    let current = match run(&["display-message", "-p", "#{pid}\t#{socket_path}"]) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let fields = current.trim().split('\t').collect::<Vec<_>>();
    if fields.len() != 2
        || refs["panel_tmux_server_pid"] != fields[0]
        || refs["panel_socket_digest"]
            != crate::digest(
                std::fs::canonicalize(fields[1])
                    .map_err(Error::new)?
                    .as_os_str()
                    .as_encoded_bytes(),
            )
    {
        return Ok(false);
    }
    // The live generation must match; legacy birth encodings stay unproven.
    let pid = match fields[0].parse::<u32>() {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let identity = match crate::terminals::process(pid) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    if refs["panel_tmux_server_identity"] != identity.birth
        && refs["panel_tmux_server_identity"] != crate::digest(identity.birth.as_bytes())
    {
        return Ok(false);
    }
    let panes = match run(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{@oms_panel_attempt}\t#{@oms_panel_main_attempt}",
    ]) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let windows = match run(&[
        "list-windows",
        "-a",
        "-F",
        "#{@oms_panel_attempt}\t#{@oms_panel_main_attempt}",
    ]) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let id = attempt["attempt_id"].as_str().unwrap_or_default();
    Ok(!id.is_empty()
        && !panes
            .lines()
            .chain(windows.lines())
            .any(|line| line.split('\t').any(|value| value == id || value == pane)))
}
