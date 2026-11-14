use crate::filesystem::{Anchor, Opened, Stamp};
use crate::{Discovery, Error, git, hex, inventory, member};
use oms_context::Json;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

mod built {
    include!(concat!(env!("OUT_DIR"), "/source_inventory.rs"));
}

pub type SourceFile<'a> = (&'a [u8], &'a str);
/// Expected bytes from the reviewed build, never an on-disk cache receipt.
/// The closure includes every crate's src, build.rs, manifest and lock, plus
/// the workspace manifests and pinned toolchain. Separate tests/docs trees
/// are excluded; source-local test modules remain part of the closure.
pub struct SourceInventory<'a> {
    files: &'a [SourceFile<'a>],
}
impl SourceInventory<'static> {
    pub fn embedded() -> Self {
        Self {
            files: built::FILES,
        }
    }
}
impl<'a> SourceInventory<'a> {
    /// The caller's trusted build may supply its own inventory using the same
    /// complete closure. This must not be populated from runtime metadata.
    pub fn reviewed(files: &'a [SourceFile<'a>]) -> Self {
        Self { files }
    }
    fn validate(&self, root: &Anchor) -> Result<(String, Vec<(PathBuf, Stamp)>), Error> {
        let actual = inventory::paths(root.path())
            .map_err(|_| Error::integrity("cannot enumerate current native source"))?;
        if actual.len() != self.files.len()
            || actual
                .iter()
                .zip(self.files)
                .any(|(a, (b, _))| a.as_os_str().as_bytes() != *b)
        {
            return Err(Error::integrity(
                "native source inventory differs from the reviewed build",
            ));
        }
        let mut stamps = Vec::new();
        let mut hash = Sha256::new();
        for (label, expected) in self.files {
            if !hex(expected.as_bytes(), 64) {
                return Err(Error::integrity("invalid reviewed native source digest"));
            }
            let path = Path::new(OsStr::from_bytes(label));
            let source = root.file(path, false)?;
            if source.digest(64 * 1024 * 1024)? != *expected {
                return Err(Error::integrity(
                    "native source content differs from the reviewed build",
                ));
            }
            hash.update((label.len() as u64).to_be_bytes());
            hash.update(label);
            hash.update(expected.as_bytes());
            stamps.push((path.to_path_buf(), source.stamp()));
        }
        Ok((format!("{:x}", hash.finalize()), stamps))
    }
}

/// A process-local proof, with open authority handles and checked artifacts.
/// It is deliberately not Clone or serializable, and cannot be obtained from
/// an environment variable or a previous process's success status.
pub struct ValidatedPayload {
    directory: Anchor,
    executable: Opened,
    helper: Option<Opened>,
    manifest: Opened,
    checksum: Opened,
    revision: String,
    source_digest: String,
    sources: Vec<(PathBuf, Stamp)>,
}
impl ValidatedPayload {
    /// For the existing --oms-select/--oms-dispatch consumers. The embedding
    /// entry must already be authenticated before reaching this method.
    /// Reuse the returned proof for dispatch; do not call load a second time.
    pub fn private(
        root: &Path,
        revision: &OsStr,
        inventory: &SourceInventory<'_>,
    ) -> Result<(Discovery, Self), Error> {
        if !hex(revision.as_bytes(), 40) {
            return Err(Error::integrity("invalid native source revision"));
        }
        let source = Discovery::from_source_root(root)?;
        let payload = Self::load(&source, inventory)?.ok_or_else(|| {
            Error::integrity("native payload unavailable; run oms doctor --repair")
        })?;
        if payload.revision.as_bytes() != revision.as_bytes() {
            return Err(Error::integrity("native manifest revision mismatch"));
        }
        payload.authenticates_current_process()?;
        Ok((source, payload))
    }
    /// Missing current-revision entries permit the established legacy route;
    /// every error in a present entry is terminal.
    pub fn load(
        source: &Discovery,
        inventory: &SourceInventory<'_>,
    ) -> Result<Option<Self>, Error> {
        source.check()?;
        if !source.root.exists(Path::new("rust"))? {
            return Ok(None);
        }
        let rust = source.root.descend(Path::new("rust"))?;
        if !rust.exists(Path::new("target"))? {
            return Ok(None);
        }
        let target = rust.descend(Path::new("target"))?;
        if !target.exists(Path::new("oms-native"))? {
            return Ok(None);
        }
        let cache = target.descend(Path::new("oms-native"))?;
        let revision = git::revision(&source.root)?;
        if !cache.exists(Path::new(&revision))? {
            return Ok(None);
        }
        let directory = cache.descend(Path::new(&revision))?;
        let manifest = directory.file(Path::new("manifest.json"), false)?;
        let checksum = directory.file(Path::new("oms.sha256"), false)?;
        let bytes = manifest.read(4 * 1024 * 1024)?;
        let row = oms_context::parse_json(&bytes)
            .map_err(|_| Error::integrity("invalid native manifest"))?;
        if member(&row, b"schema") != Some(&Json::Number("1".into()))
            || member(&row, b"source_commit") != Some(&Json::String(revision.as_bytes().to_vec()))
        {
            return Err(Error::integrity("native manifest revision mismatch"));
        }
        let artifacts = member(&row, b"artifacts")
            .ok_or_else(|| Error::integrity("missing native artifacts"))?;
        let Json::Object(entries) = artifacts else {
            return Err(Error::integrity("invalid native artifacts"));
        };
        if entries
            .iter()
            .any(|(name, _)| name != b"oms" && name != b"oms-graph-python-helper")
        {
            return Err(Error::integrity("invalid native artifact name"));
        }
        let expected = digest(artifacts, b"oms")?
            .ok_or_else(|| Error::integrity("missing native executable digest"))?;
        let record = checksum.read(65)?;
        if record.len() != 65 || record.last() != Some(&b'\n') || &record[..64] != expected {
            return Err(Error::integrity("invalid native executable checksum"));
        }
        let executable = directory.file(Path::new("oms"), true)?;
        if executable.digest(1024 * 1024 * 1024)?.as_bytes() != expected {
            return Err(Error::integrity(
                "native executable checksum mismatch; run oms doctor --repair",
            ));
        }
        let helper = if let Some(expected) = digest(artifacts, b"oms-graph-python-helper")? {
            let helper = directory.file(Path::new("oms-graph-python-helper"), true)?;
            if helper.digest(1024 * 1024 * 1024)?.as_bytes() != expected {
                return Err(Error::integrity("native artifact digest mismatch"));
            }
            Some(helper)
        } else {
            None
        };
        for entry in std::fs::read_dir(directory.path())
            .map_err(|_| Error::integrity("cannot enumerate native payload"))?
        {
            let name = entry
                .map_err(|_| Error::integrity("cannot enumerate native payload"))?
                .file_name();
            if !matches!(name.as_bytes(), b"oms" | b"manifest.json" | b"oms.sha256")
                && !(name == "oms-graph-python-helper" && helper.is_some())
            {
                return Err(Error::integrity("foreign native payload content"));
            }
        }
        let (source_digest, sources) = inventory.validate(&source.root)?;
        let payload = Self {
            directory,
            executable,
            helper,
            manifest,
            checksum,
            revision,
            source_digest,
            sources,
        };
        payload.recheck(source)?;
        Ok(Some(payload))
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn executable(&self) -> PathBuf {
        self.executable.path()
    }
    pub fn helper(&self) -> Option<PathBuf> {
        self.helper.as_ref().map(Opened::path)
    }
    /// Rechecks identities, permissions, source stamps and revision without
    /// rereading executable contents. No proof survives an exec boundary.
    pub fn recheck(&self, source: &Discovery) -> Result<(), Error> {
        source.check()?;
        self.directory.check()?;
        self.executable.check()?;
        self.manifest.check()?;
        self.checksum.check()?;
        if let Some(helper) = &self.helper {
            helper.check()?;
        }
        if git::revision(&source.root)? != self.revision {
            return Err(Error::integrity(
                "native source revision changed during validation",
            ));
        }
        let current = inventory::paths(source.source_root())
            .map_err(|_| Error::integrity("cannot enumerate current native source"))?;
        if current.len() != self.sources.len()
            || current.iter().zip(&self.sources).any(|(a, (b, _))| a != b)
        {
            return Err(Error::integrity(
                "native source inventory changed during validation",
            ));
        }
        for (path, stamp) in &self.sources {
            if !source.root.file(path, false)?.matches_stamp(stamp) {
                return Err(Error::integrity("native source changed during validation"));
            }
        }
        Ok(())
    }
    /// An in-process main must prove it is the authenticated payload before
    /// using this proof to run migrated implementations directly.
    pub fn authenticates_current_process(&self) -> Result<(), Error> {
        let current = std::env::current_exe()
            .map_err(|_| Error::integrity("cannot identify native executable"))?;
        if !self.executable.identity_matches(&current) {
            return Err(Error::integrity(
                "native executable does not belong to this checkout",
            ));
        }
        self.executable.check()
    }
    /// Consumes the proof and retains it until the final pre-exec checks.
    pub fn into_exec(self, source: Discovery, args: Vec<OsString>) -> CheckedExec {
        CheckedExec {
            payload: self,
            source,
            args,
        }
    }
}
fn digest<'a>(artifacts: &'a Json, name: &[u8]) -> Result<Option<&'a [u8]>, Error> {
    match member(artifacts, name) {
        None => Ok(None),
        Some(Json::String(value)) if hex(value, 64) => Ok(Some(value)),
        _ => Err(Error::integrity("invalid native digest")),
    }
}

pub struct CheckedExec {
    payload: ValidatedPayload,
    source: Discovery,
    args: Vec<OsString>,
}
impl CheckedExec {
    /// The child still validates its private invocation. Never translate a
    /// completed action's status 75 into permission to retry via a shell.
    pub fn execute(self) -> Result<std::convert::Infallible, Error> {
        self.payload.recheck(&self.source)?;
        let mut command = Command::new(self.payload.executable());
        command
            .arg("--oms-dispatch")
            .arg(self.source.source_root())
            .arg(self.payload.revision())
            .arg("--")
            .args(&self.args);
        for name in GIT_ENV {
            command.env_remove(name);
        }
        for (name, _) in std::env::vars_os() {
            if name.as_bytes().starts_with(b"GIT_CONFIG_") {
                command.env_remove(name);
            }
        }
        let error = command.exec();
        Err(Error::integrity(format!(
            "cannot execute authenticated native payload: {error}"
        )))
    }
}
const GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
];
