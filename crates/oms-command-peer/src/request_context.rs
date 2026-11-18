//! Per-call prompt evidence shares the context manifest reader, not usage accounting.
use crate::{Error, Result, storage};
use oms_context::attribution::{Span, describe};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    ops::Deref,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) trait Input {
    fn bytes(&self) -> &[u8];
    fn spans(&self) -> &[Span] {
        &[]
    }
    fn renderer(&self) -> &'static str {
        "oms-primary-prompt-unattributed-v1"
    }
}
impl Input for [u8] {
    fn bytes(&self) -> &[u8] {
        self
    }
}
impl<const N: usize> Input for [u8; N] {
    fn bytes(&self) -> &[u8] {
        self
    }
}
impl Input for Vec<u8> {
    fn bytes(&self) -> &[u8] {
        self
    }
}

#[derive(Clone)]
pub(crate) struct Prompt {
    body: String,
    spans: Vec<Span>,
    prefix: Sha256,
}
impl Prompt {
    pub fn framing(body: impl Into<String>) -> Self {
        let mut value = Self {
            body: String::new(),
            spans: Vec::new(),
            prefix: Sha256::new(),
        };
        value.push_layer("framing", &body.into());
        value
    }
    pub fn unknown(body: String) -> Self {
        let prefix = Sha256::new().chain_update(body.as_bytes());
        Self {
            body,
            spans: Vec::new(),
            prefix,
        }
    }
    pub fn push_str(&mut self, text: &str) {
        self.body.push_str(text);
        self.prefix.update(text.as_bytes());
    }
    pub fn push(&mut self, c: char) {
        self.push_str(c.encode_utf8(&mut [0; 4]));
    }
    pub fn push_layer(&mut self, layer: &'static str, text: &str) {
        self.push_source(layer, text, (layer != "framing").then_some(text.as_bytes()));
    }
    fn push_source(&mut self, layer: &'static str, text: &str, source: Option<&[u8]>) {
        let start = self.body.len();
        self.push_str(text);
        let end = self.body.len();
        if start == end || self.spans.len() >= 4096 {
            return;
        }
        self.spans.push(Span {
            layer,
            start,
            end,
            sha256: storage::hash(text.as_bytes()),
            prefix_sha256: format!("{:x}", self.prefix.clone().finalize()),
            source_sha256: source.map(storage::hash),
            source_bytes: source.map(<[u8]>::len),
            source_range: source
                .filter(|raw| *raw == text.as_bytes())
                .map(|raw| (0, raw.len())),
        });
    }
    pub fn untrusted(
        &mut self,
        layer: &'static str,
        who: &str,
        kind: &str,
        raw: &[u8],
    ) -> Result<()> {
        let [prefix, body, suffix] = crate::safety::untrusted_parts(who, kind, raw)?;
        self.push_layer("framing", &prefix);
        self.push_source(layer, &body, Some(raw));
        self.push_layer("framing", &suffix);
        Ok(())
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.body.as_bytes()
    }
}
impl Deref for Prompt {
    type Target = str;
    fn deref(&self) -> &str {
        &self.body
    }
}
impl fmt::Display for Prompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.body.fmt(f)
    }
}
impl Input for Prompt {
    fn bytes(&self) -> &[u8] {
        self.as_bytes()
    }
    fn spans(&self) -> &[Span] {
        &self.spans
    }
    fn renderer(&self) -> &'static str {
        "oms-peer-prompt-v1"
    }
}

pub(crate) struct Prepared {
    path: PathBuf,
    row: crate::V,
}
fn try_guard(repo: &Path, path: &Path, stop: &AtomicBool) -> Result<oms_lock::Guard> {
    crate::check(stop)?;
    let root = storage::root(repo)?;
    let mut settings = oms_lock::Settings::from_env()
        .map_err(|_| Error::new(2, "invalid context lock settings"))?;
    settings.cwd = root.physical_root().to_owned();
    let lock = oms_lock::Lock::for_file(&root.physical_root().join(path), settings)
        .map_err(|_| Error::new(2, "context lock unavailable"))?;
    lock.try_acquire()
        .map_err(|_| Error::new(75, "context evidence busy or unavailable"))
}
impl Prepared {
    pub fn new<P: Input + ?Sized>(
        repo: &Path,
        prompt: &P,
        identity: &oms_command_telemetry::store::usage::Identity,
        fence: &crate::process::LaunchFence,
        stop: &AtomicBool,
    ) -> Result<Self> {
        fence.check(stop)?;
        let attribution = describe(
            prompt.bytes(),
            prompt.renderer(),
            prompt.spans(),
            storage::hash,
            None,
            None,
        )
        .map_err(|s| Error::new(2, s))?;
        let attribution = crate::V::parse(attribution.to_string().as_bytes())?;
        let path = PathBuf::from(format!(
            ".oms/runtime/context/calls/{}.json",
            identity.call_id
        ));
        let row = crate::obj([
            ("schema", 1usize.into()),
            ("kind", "oms-context-request".into()),
            ("operation_id", identity.operation_id.clone().into()),
            ("call_id", identity.call_id.clone().into()),
            ("provider", identity.provider.clone().into()),
            ("dispatch", "prepared".into()),
            ("provider_delivery", crate::V::Null),
            ("attribution", attribution),
        ]);
        fence.check(stop)?;
        let guard = try_guard(repo, &path, stop)?;
        let raw = row.json(false)?;
        if raw.len() > 4 * 1024 * 1024 {
            return Err(Error::new(2, "context evidence exceeds read bound"));
        }
        storage::transaction_locked(repo, &path, &guard, stop, |old| {
            fence.check(stop)?;
            if !old.is_empty() {
                return Err(Error::new(75, "context request identity already recorded"));
            }
            Ok((raw, ()))
        })?;
        Ok(Self { path, row })
    }
    pub fn returned(&mut self, repo: &Path, success: bool, stop: &AtomicBool) -> Result<()> {
        let prepared = self.row.json(false)?;
        self.row.set(
            "dispatch",
            if success {
                "executor-returned"
            } else {
                "executor-error"
            },
        );
        let returned = self.row.json(false)?;
        let guard = try_guard(repo, &self.path, stop)?;
        storage::transaction_locked(repo, &self.path, &guard, stop, |old| {
            if old != prepared {
                return Err(Error::new(75, "prepared context request manifest changed"));
            }
            Ok((returned, ()))
        })
    }
}
