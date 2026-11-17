use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// Native companions `oms-bootstrap` accepts besides `oms`; any other name is
/// foreign. Keep identical to its inventory.
const COMPANIONS: [&str; 5] = [
    "oms-supervisor",
    "oms-state-queue-worker",
    "oms-mcp-peer-owner",
    "oms-run-bounded-helper",
    "oms-graph-python-helper",
];
/// Release binaries `stage` builds and records: `oms` plus the companions the
/// staged runtime launches. The supervisor is accepted when present, not built.
const BUILT: [&str; 4] = [
    "oms-mcp-peer-owner",
    "oms-run-bounded-helper",
    "oms-state-queue-worker",
    "oms-graph-python-helper",
];

pub(crate) struct Payload {
    pub executable: PathBuf,
    pub helper: Option<PathBuf>,
    /// Every verified companion present in the payload, in `COMPANIONS` order.
    pub companions: Vec<(&'static str, PathBuf)>,
    pub revision: String,
    discovery: oms_bootstrap::Discovery,
    proof: oms_bootstrap::ValidatedPayload,
}
impl Payload {
    pub(crate) fn recheck(&self) -> Result<()> {
        self.proof.recheck(&self.discovery).map_err(|_| {
            r::Error(
                2,
                "native payload changed during lifecycle transaction".into(),
            )
        })
    }
    pub(crate) fn frontdoor(&self, paths: &Paths) -> PathBuf {
        paths
            .data
            .join("libexec")
            .join(&self.revision)
            .join("oms-frontdoor")
    }
}
fn protected(path: &Path, executable: bool) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    let uid = rustix::process::geteuid().as_raw();
    if !m.is_file()
        || m.file_type().is_symlink()
        || m.nlink() != 1
        || !matches!(m.uid(), 0) && m.uid() != uid
        || m.mode() & 0o022 != 0
        || executable && m.mode() & 0o111 == 0
    {
        return r::fail("unsafe native payload file");
    }
    Ok(())
}
/// `oms` is required; the only other names are the shared companion set.
fn names_allowed<'a>(mut names: impl Iterator<Item = &'a str> + Clone) -> Result<()> {
    if !names.clone().any(|k| k == "oms") || names.any(|k| k != "oms" && !COMPANIONS.contains(&k)) {
        return r::fail("invalid native artifact names");
    }
    Ok(())
}
pub(crate) fn verify(paths: &Paths, flag: &AtomicBool) -> Result<Payload> {
    let discovery = oms_bootstrap::Discovery::from_source_root(&paths.source)
        .map_err(|_| r::Error(2, "cannot establish native source authority".into()))?;
    let proof = oms_bootstrap::ValidatedPayload::load(
        &discovery,
        &oms_bootstrap::SourceInventory::embedded(),
    )
    .map_err(|_| r::Error(2, "native payload/source proof failed".into()))?
    .ok_or_else(|| {
        r::Error(
            2,
            "native payload unavailable; run oms doctor --repair".into(),
        )
    })?;
    proof
        .recheck(&discovery)
        .map_err(|_| r::Error(2, "native source changed during validation".into()))?;

    let revision = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return r::fail("invalid native source revision");
    }
    let dir = paths.source.join("target/oms-native").join(&revision);
    let mut ancestor = dir.as_path();
    while ancestor.starts_with(&paths.source) {
        let m = fs::symlink_metadata(ancestor)?;
        if !m.is_dir()
            || m.file_type().is_symlink()
            || m.mode() & 0o022 != 0
            || m.uid() != 0 && m.uid() != rustix::process::geteuid().as_raw()
        {
            return r::fail("unsafe native payload directory");
        }
        if ancestor == paths.source {
            break;
        }
        ancestor = ancestor.parent().unwrap();
    }
    protected(&dir.join("manifest.json"), false)?;
    protected(&dir.join("oms.sha256"), false)?;
    let row = r::parse(&r::read(&dir.join("manifest.json"), 4 * 1024 * 1024)?)?;
    if row["schema"] != 1 || row["source_commit"].as_str() != Some(&revision) {
        return r::fail("native manifest revision mismatch");
    }
    let artifacts = row["artifacts"]
        .as_object()
        .ok_or_else(|| r::Error(2, "missing native artifacts".into()))?;
    names_allowed(artifacts.keys().map(String::as_str))?;
    for (name, expected) in artifacts {
        let expected = expected
            .as_str()
            .filter(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
            .ok_or_else(|| r::Error(2, "invalid native artifact digest".into()))?;
        protected(&dir.join(name), true)?;
        if r::hash(&r::read(&dir.join(name), 1024 * 1024 * 1024)?) != expected {
            return r::fail("native artifact digest mismatch");
        }
    }
    if r::read(&dir.join("oms.sha256"), 65)?
        != format!("{}\n", artifacts["oms"].as_str().unwrap()).as_bytes()
    {
        return r::fail("invalid native executable checksum");
    }
    for entry in fs::read_dir(&dir)? {
        let name = entry?.file_name();
        if !matches!(name.to_str(), Some("manifest.json" | "oms.sha256"))
            && !artifacts.contains_key(name.to_str().unwrap_or(""))
        {
            return r::fail("foreign native payload content");
        }
    }
    let companions = COMPANIONS
        .iter()
        .filter(|name| artifacts.contains_key(**name))
        .map(|name| {
            proof
                .companion(name)
                .map(|path| (*name, path))
                .ok_or_else(|| r::Error(2, "native companion missing from proof".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Payload {
        executable: proof.executable(),
        helper: proof.helper(),
        companions,
        revision,
        discovery,
        proof,
    })
}
pub(crate) fn install(paths: &Paths, payload: &Payload, tx: &mut Transaction) -> Result<()> {
    payload.recheck()?;
    let files: Vec<(&str, &Path)> = std::iter::once(("oms", payload.executable.as_path()))
        .chain(payload.companions.iter().map(|(n, p)| (*n, p.as_path())))
        .collect();
    install_files(
        &paths.data.join("libexec").join(&payload.revision),
        &files,
        tx,
    )?;
    let digest = r::hash(&r::read(&payload.executable, 1024 * 1024 * 1024)?);
    let executable = r::shell(payload.executable.as_os_str())?;
    let scripts_oms = r::shell(paths.source.join("scripts/oms").as_os_str())?;
    let source = r::shell(paths.source.as_os_str())?;
    let wrapper = format!(
        "#!/bin/sh\n# oh-my-setting native lifecycle\nhash_file() {{\n  if command -v sha256sum >/dev/null 2>&1; then\n    out=$(sha256sum < \"$1\") || return 1\n    printf '%s\\n' \"${{out%% *}}\"\n  elif command -v shasum >/dev/null 2>&1; then\n    out=$(shasum -a 256 < \"$1\") || return 1\n    printf '%s\\n' \"${{out%% *}}\"\n  else\n    echo 'error: native payload hash tool unavailable' >&2\n    return 2\n  fi\n}}\nif observed=$(hash_file {executable} 2>/dev/null) && [ \"$observed\" = '{digest}' ]; then\n  exec {executable} --oms-dispatch {source} {} -- \"$@\"\nfi\nrecover=0\ncase \"${{1:-}}\" in\n  uninstall) recover=1 ;;\n  doctor)\n    for arg in \"$@\"; do\n      [ \"$arg\" = '--repair' ] && recover=1\n    done ;;\nesac\nif [ \"$recover\" = 1 ] && [ -x {scripts_oms} ]; then\n  exec {scripts_oms} \"$@\"\nfi\necho 'error: native payload changed; run oms doctor --repair' >&2\nexit 2\n",
        payload.revision
    );
    tx.write(&payload.frontdoor(paths), wrapper.as_bytes(), 0o755)?;
    payload.recheck()
}

/// Refuse to replace differing bytes before queueing any write.
fn install_files(dir: &Path, files: &[(&str, &Path)], tx: &mut Transaction) -> Result<()> {
    let mut pending = Vec::new();
    for (name, source) in files {
        let target = dir.join(name);
        let raw = r::read(source, 1024 * 1024 * 1024)?;
        if !r::exists(&target) {
            pending.push((target, raw));
        } else if r::read(&target, 1024 * 1024 * 1024)? != raw {
            return r::fail(
                "refusing to replace an existing native libexec payload with different bytes",
            );
        }
    }
    for (target, raw) in pending {
        tx.write(&target, &raw, 0o755)?;
    }
    Ok(())
}

/// Quarantine an invalid or dangling `target/oms-native/<rev>` entry so
/// `doctor --repair` can re-stage a clean payload without weakening `install`.
pub(crate) fn repair_cache(paths: &Paths, flag: &AtomicBool) -> Result<()> {
    let Ok(revision) = r::git(&paths.source, &["rev-parse", "HEAD"], flag) else {
        return Ok(());
    };
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return r::fail("invalid source revision for native build");
    }
    let dir = paths.source.join("target/oms-native").join(&revision);
    if fs::symlink_metadata(&dir).is_ok() && verify(paths, flag).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let quarantine = paths
            .source
            .join("target/oms-native")
            .join(format!(".broken-{revision}-{}-{nanos}", std::process::id()));
        fs::rename(&dir, &quarantine)?;
    }
    Ok(())
}

/// Stage a complete build through the same target journal as installed files.
/// A present invalid cache is terminal; a build never repairs it implicitly.
pub(crate) fn stage(paths: &Paths, tx: &mut Transaction, flag: &AtomicBool) -> Result<()> {
    let revision = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return r::fail("invalid source revision for native build");
    }
    let dir = paths.source.join("target/oms-native").join(&revision);
    if r::exists(&dir) {
        verify(paths, flag)?;
        return Ok(());
    }
    let cargo = r::find("cargo", &paths.cwd)
        .ok_or_else(|| r::Error(2, "native payload missing and cargo unavailable".into()))?;
    let result = r::command(
        cargo,
        &[
            "+1.99.1".into(),
            "build".into(),
            "--manifest-path".into(),
            paths.source.join("Cargo.toml").into_os_string(),
            "--locked".into(),
            "--offline".into(),
            "--release".into(),
            "--bin".into(),
            "oms".into(),
            "--bin".into(),
            "oms-mcp-peer-owner".into(),
            "--bin".into(),
            "oms-run-bounded-helper".into(),
            "--bin".into(),
            "oms-state-queue-worker".into(),
            "--bin".into(),
            "oms-graph-python-helper".into(),
            "--target-dir".into(),
            paths.source.join("target/lifecycle-build").into_os_string(),
        ],
        &paths.source,
        None,
        flag,
        std::time::Duration::from_secs(1800),
    )?;
    if result.0 != 0 {
        return Err(r::Error(
            result.0,
            "native source build failed; installation was not published".into(),
        ));
    }
    if r::git(&paths.source, &["rev-parse", "HEAD"], flag)? != revision {
        return r::fail("source revision changed during build");
    }
    let cache = paths.source.join("target");
    secure_dir(&cache)?;
    secure_dir(&cache.join("oms-native"))?;
    publish(
        &dir,
        &paths.source.join("target/lifecycle-build/release"),
        &revision,
        tx,
    )?;
    verify(paths, flag)?.recheck()
}

/// Cargo output: `release/<name>` is a hard link to `deps/<name>-<hash>` and
/// is group-writable under umask 0002. Accept that until `publish` writes the
/// strict single-link copy that `verify` demands; refuse symlinks, foreign
/// owners, world-writable and non-executable files.
fn build_output(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    let uid = rustix::process::geteuid().as_raw();
    if !m.is_file()
        || m.file_type().is_symlink()
        || !matches!(m.uid(), 0) && m.uid() != uid
        || m.mode() & 0o002 != 0
        || m.mode() & 0o111 == 0
    {
        return r::fail("unsafe native build output");
    }
    Ok(())
}
/// Create a missing build-cache directory, or strip group/world write from one
/// cargo made under a permissive umask, so `verify`'s ancestor walk accepts it.
fn secure_dir(path: &Path) -> Result<()> {
    if !r::exists(path) {
        fs::DirBuilder::new().mode(0o755).create(path)?;
    }
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() || m.uid() != rustix::process::geteuid().as_raw() {
        return r::fail("unsafe native build cache directory");
    }
    if m.mode() & 0o022 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(m.mode() & 0o755))?;
    }
    Ok(())
}

/// Hash every built binary and commit them with the manifest in one journaled replace.
fn publish(dir: &Path, release: &Path, revision: &str, tx: &mut Transaction) -> Result<()> {
    let mut built = Vec::new();
    for name in std::iter::once("oms").chain(BUILT) {
        let path = release.join(name);
        build_output(&path)?;
        let raw = crate::transaction::external_read(&path)?;
        built.push((name, r::hash(&raw), raw));
    }
    let checksum = format!("{}\n", built[0].1);
    let artifacts: serde_json::Map<String, serde_json::Value> = built
        .iter()
        .map(|(name, digest, _)| ((*name).to_owned(), digest.clone().into()))
        .collect();
    let row = serde_json::json!({"schema":1,"source_commit":revision,"artifacts":artifacts});
    let manifest = r::bytes(&row)?;
    let mut files: Vec<(&str, &[u8], u32)> = built
        .iter()
        .map(|(name, _, raw)| (*name, raw.as_slice(), 0o755))
        .collect();
    files.push(("oms.sha256", checksum.as_bytes(), 0o600));
    files.push(("manifest.json", &manifest, 0o600));
    tx.files(dir, &files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn paths(root: &Path) -> Paths {
        Paths {
            home: root.join("home"),
            source: root.join("source"),
            receipt: root.join("receipt"),
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            cwd: root.to_path_buf(),
        }
    }
    fn names(list: &[&str]) -> Result<()> {
        names_allowed(list.iter().copied())
    }
    fn write(path: &Path, raw: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, raw).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn artifact_names_are_exactly_oms_plus_the_bootstrap_companions() {
        assert!(names(&["oms"]).is_ok());
        assert!(names(&["oms", "oms-supervisor", "oms-mcp-peer-owner"]).is_ok());
        assert!(names(&["oms"].into_iter().chain(COMPANIONS).collect::<Vec<_>>()).is_ok());
        assert!(names(&[]).is_err(), "oms is required");
        assert!(names(&["oms-supervisor"]).is_err(), "oms is required");
        assert!(names(&["oms", "oms-foreign"]).is_err());
        assert!(names(&["oms", "oms-graph-python-helper", "stray"]).is_err());
        for name in BUILT {
            assert!(COMPANIONS.contains(&name), "{name} must be verifiable");
        }
    }

    #[test]
    fn publish_records_every_binary_and_install_stages_them_in_libexec() {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("oms-payload-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _guard = Dir(root.clone());
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = paths(&root);
        // Mirror `cargo build --release`: release/<name> hard-links deps/<name>-<hash>,
        // both group-writable (umask 0002), so st_nlink is 2 and mode 0o775.
        let release = root.join("release");
        for name in std::iter::once("oms").chain(BUILT) {
            let built = release.join("deps").join(format!("{name}-0f0f"));
            write(&built, name.as_bytes());
            fs::set_permissions(&built, fs::Permissions::from_mode(0o775)).unwrap();
            fs::hard_link(&built, release.join(name)).unwrap();
            assert_eq!(fs::metadata(release.join(name)).unwrap().nlink(), 2);
        }
        let (dir, rev) = (root.join("cache/rev"), "0".repeat(40));
        let other = root.join("cache/other");
        // The cache parent cargo left at 0o775 is tightened, so verify's walk accepts it.
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::set_permissions(root.join("cache"), fs::Permissions::from_mode(0o775)).unwrap();
        secure_dir(&root.join("cache")).unwrap();
        assert_eq!(
            fs::metadata(root.join("cache")).unwrap().mode() & 0o777,
            0o755
        );
        // Missing parents are created 0o755; a symlink or plain file is refused.
        secure_dir(&root.join("fresh")).unwrap();
        assert_eq!(
            fs::metadata(root.join("fresh")).unwrap().mode() & 0o777,
            0o755
        );
        symlink(root.join("cache"), root.join("link")).unwrap();
        assert!(secure_dir(&root.join("link")).is_err());
        fs::write(root.join("file"), b"x").unwrap();
        assert!(secure_dir(&root.join("file")).is_err());
        let mut tx = Transaction::begin(&paths).unwrap();
        publish(&dir, &release, &rev, &mut tx).unwrap();
        let bin = release.join("oms-run-bounded-helper");
        // Missing, symlinked, world-writable and non-executable outputs fail before any write.
        let saved = fs::read(&bin).unwrap();
        fs::remove_file(&bin).unwrap();
        assert!(publish(&other, &release, &rev, &mut tx).is_err());
        symlink(release.join("oms"), &bin).unwrap();
        assert!(publish(&other, &release, &rev, &mut tx).is_err());
        fs::remove_file(&bin).unwrap();
        write(&bin, &saved);
        for mode in [0o757, 0o777, 0o644, 0o000] {
            fs::set_permissions(&bin, fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                publish(&other, &release, &rev, &mut tx).is_err(),
                "{mode:o}"
            );
        }
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(!other.exists());
        let sources: Vec<(&str, PathBuf)> = std::iter::once("oms")
            .chain(BUILT)
            .map(|n| (n, dir.join(n)))
            .collect();
        let files: Vec<(&str, &Path)> = sources.iter().map(|(n, p)| (*n, p.as_path())).collect();
        let lib = root.join("data/libexec").join(&rev);
        tx.commit().unwrap();

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["schema"], 1);
        assert_eq!(manifest["source_commit"], rev.as_str());
        let artifacts = manifest["artifacts"].as_object().unwrap();
        assert_eq!(artifacts.len(), 1 + BUILT.len());
        for name in std::iter::once("oms").chain(BUILT) {
            // The published copy is what verify() demands: single-link, no group/world write.
            protected(&dir.join(name), true).unwrap();
            assert_eq!(artifacts[name], r::hash(name.as_bytes()).as_str());
            assert_eq!(fs::read(dir.join(name)).unwrap(), name.as_bytes());
        }
        assert_eq!(
            fs::read(dir.join("oms.sha256")).unwrap(),
            format!("{}\n", r::hash(b"oms")).as_bytes()
        );
        let mut tx = Transaction::begin(&paths).unwrap();
        install_files(&lib, &files, &mut tx).unwrap();
        tx.commit().unwrap();
        for (name, _) in &files {
            assert_eq!(fs::read(lib.join(name)).unwrap(), name.as_bytes());
            assert_eq!(fs::metadata(lib.join(name)).unwrap().mode() & 0o777, 0o755);
        }
        // Identical bytes are idempotent; differing bytes refuse and write nothing.
        let mut tx = Transaction::begin(&paths).unwrap();
        install_files(&lib, &files, &mut tx).unwrap();
        tx.commit().unwrap();
        fs::remove_file(lib.join("oms-supervisor")).ok();
        fs::remove_file(lib.join("oms-state-queue-worker")).unwrap();
        fs::write(lib.join("oms-mcp-peer-owner"), b"other").unwrap();
        let mut tx = Transaction::begin(&paths).unwrap();
        assert!(install_files(&lib, &files, &mut tx).is_err());
        assert!(!lib.join("oms-state-queue-worker").exists());
    }
}
