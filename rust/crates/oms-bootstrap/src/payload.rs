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

/// Optional native companions a manifest may list besides `oms`. The graph
/// helper is transitional. Any other name is foreign.
const COMPANIONS: [&str; 5] = [
    "oms-supervisor",
    "oms-state-queue-worker",
    "oms-mcp-peer-owner",
    "oms-run-bounded-helper",
    "oms-graph-python-helper",
];

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
    companions: Vec<(&'static str, Opened)>,
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
            .any(|(name, _)| name != b"oms" && !COMPANIONS.iter().any(|c| c.as_bytes() == name))
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
        let mut companions = Vec::new();
        for name in COMPANIONS {
            if let Some(expected) = digest(artifacts, name.as_bytes())? {
                let file = directory.file(Path::new(name), true)?;
                if file.digest(1024 * 1024 * 1024)?.as_bytes() != expected {
                    return Err(Error::integrity("native artifact digest mismatch"));
                }
                companions.push((name, file));
            }
        }
        for entry in rustix::fs::Dir::read_from(directory.last())
            .map_err(|_| Error::integrity("cannot enumerate native payload"))?
        {
            let entry = entry.map_err(|_| Error::integrity("cannot enumerate native payload"))?;
            let name = entry.file_name().to_bytes();
            if !matches!(
                name,
                b"." | b".." | b"oms" | b"manifest.json" | b"oms.sha256"
            ) && !companions.iter().any(|(c, _)| c.as_bytes() == name)
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
            companions,
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
        self.companion("oms-graph-python-helper")
    }
    /// Path of a manifest-listed, digest-checked companion binary.
    pub fn companion(&self, name: &str) -> Option<PathBuf> {
        self.companions
            .iter()
            .find(|(c, _)| *c == name)
            .map(|(_, file)| file.path())
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
        for (_, file) in &self.companions {
            file.check()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    const REV: &str = "0123456789abcdef0123456789abcdef01234567";
    fn sum(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn write(path: &Path, bytes: &[u8], mode: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    /// A checkout anchored only by native markers, plus a cached payload.
    fn fixture(name: &str, listed: &[(&str, &[u8])], extra: &[&str]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("oms-bootstrap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(
            &root.join(".git/HEAD"),
            format!("{REV}\n").as_bytes(),
            0o644,
        );
        write(&root.join("rust/Cargo.toml"), b"[workspace]\n", 0o644);
        write(&root.join("README.md"), b"x", 0o644);
        let dir = root.join("rust/target/oms-native").join(REV);
        let mut artifacts = vec![format!("\"oms\":\"{}\"", sum(b"oms"))];
        write(&dir.join("oms"), b"oms", 0o755);
        for (file, bytes) in listed {
            write(&dir.join(file), bytes, 0o755);
            artifacts.push(format!("\"{file}\":\"{}\"", sum(bytes)));
        }
        for file in extra {
            write(&dir.join(file), b"foreign", 0o755);
        }
        let manifest = format!(
            "{{\"schema\":1,\"source_commit\":\"{REV}\",\"artifacts\":{{{}}}}}",
            artifacts.join(",")
        );
        write(&dir.join("manifest.json"), manifest.as_bytes(), 0o644);
        write(
            &dir.join("oms.sha256"),
            format!("{}\n", sum(b"oms")).as_bytes(),
            0o644,
        );
        root
    }
    fn load(root: &Path) -> Result<Option<ValidatedPayload>, Error> {
        let source = Discovery::from_source_root(root)?;
        let digest = sum(b"x");
        let files: [SourceFile<'_>; 1] = [(b"README.md", digest.as_str())];
        ValidatedPayload::load(&source, &SourceInventory::reviewed(&files))
    }

    #[test]
    fn companions_validate_without_legacy_markers() {
        let root = fixture(
            "ok",
            &[("oms-supervisor", b"s"), ("oms-graph-python-helper", b"g")],
            &[],
        );
        let payload = load(&root).unwrap().unwrap();
        assert!(payload.companion("oms-supervisor").is_some());
        assert!(payload.companion("oms-mcp-peer-owner").is_none());
        assert!(payload.helper().is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn foreign_unlisted_or_corrupt_companions_fail_closed() {
        let root = fixture("foreign", &[], &["oms-supervisor"]);
        assert!(load(&root).is_err(), "unlisted companion is foreign");
        let root2 = fixture("foreign2", &[], &["stray"]);
        assert!(load(&root2).is_err());
        let root3 = fixture("bad", &[("oms-run-bounded-helper", b"r")], &[]);
        let path = root3
            .join("rust/target/oms-native")
            .join(REV)
            .join("oms-run-bounded-helper");
        write(&path, b"tampered", 0o755);
        assert!(load(&root3).is_err(), "digest mismatch");
        write(&path, b"r", 0o757);
        assert!(load(&root3).is_err(), "world-writable");
        let root4 = fixture("name", &[("not-a-companion", b"n")], &[]);
        assert!(load(&root4).is_err(), "unknown manifest name");
        for r in [root, root2, root3, root4] {
            std::fs::remove_dir_all(r).unwrap();
        }
    }
}
