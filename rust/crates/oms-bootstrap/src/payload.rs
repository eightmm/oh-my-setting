use crate::filesystem::{Anchor, Opened};
use crate::snapshot::Snapshot;
use crate::{Discovery, Error, git, hex, member};
use oms_context::Json;
use oms_reference::controller::Controller;
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
/// The closure includes native runtime dependency sources, manifests and build
/// scripts, workspace lock/toolchain, and bundled config/roles/prompts data.
/// Separate tests/docs trees are excluded; source-local tests are included.
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
    fn validate(&self, root: &Anchor) -> Result<(String, Snapshot), Error> {
        Snapshot::capture(root, self.files)
    }
}

/// A process-local proof, with open authority handles and checked artifacts.
/// It is deliberately not Clone or serializable, and cannot be obtained from
/// an environment variable or a previous process's success status.
pub struct ValidatedPayload {
    directory: Anchor,
    directory_generation: std::fs::Metadata,
    executable: Opened,
    controller: Option<Controller>,
    helper: Option<Opened>,
    manifest: Opened,
    checksum: Opened,
    revision: String,
    source_digest: String,
    sources: Snapshot,
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
        let directory_generation = directory.generation()?;
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
        let controller = executable.controller()?;
        let executable_digest = match &controller {
            Some(controller) => controller.digest().map_err(Error::integrity)?.to_owned(),
            None => executable.digest(1024 * 1024 * 1024)?,
        };
        if executable_digest.as_bytes() != expected {
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
        for entry in rustix::fs::Dir::read_from(directory.last())
            .map_err(|_| Error::integrity("cannot enumerate native payload"))?
        {
            let entry = entry.map_err(|_| Error::integrity("cannot enumerate native payload"))?;
            let name = entry.file_name().to_bytes();
            if !matches!(
                name,
                b"." | b".." | b"oms" | b"manifest.json" | b"oms.sha256"
            ) && !(name == b"oms-graph-python-helper" && helper.is_some())
            {
                return Err(Error::integrity("foreign native payload content"));
            }
        }
        let (source_digest, sources) = inventory.validate(&source.root)?;
        let payload = Self {
            directory,
            directory_generation,
            executable,
            controller,
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
        self.directory
            .check_generation(&self.directory_generation, true)?;
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
        if let Some(controller) = &self.controller {
            controller.check().map_err(Error::integrity)?;
        }
        self.sources.check(&source.root)
    }
    /// An in-process main must prove it is the authenticated payload before
    /// using this proof to run migrated implementations directly.
    pub fn authenticates_current_process(&self) -> Result<(), Error> {
        self.controller().map(|_| ())
    }
    /// Share the already hashed, kernel-authenticated command image with the
    /// Reference protocol and other native services. No second capture/hash.
    pub fn controller(&self) -> Result<Controller, Error> {
        let controller = self.controller.as_ref().ok_or_else(|| Error::integrity(
            "native executing-artifact proof unavailable or executable does not belong to this checkout"
        ))?;
        self.executable.check()?;
        controller.check().map_err(Error::integrity)?;
        Ok(controller.clone())
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
