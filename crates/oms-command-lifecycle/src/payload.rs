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
    let release = build(paths, &revision, flag)?;
    let cache = paths.source.join("target");
    secure_dir(&cache)?;
    secure_dir(&cache.join("oms-native"))?;
    publish(&dir, &release, &revision, tx)?;
    verify(paths, flag)?.recheck()
}

fn build(paths: &Paths, revision: &str, flag: &AtomicBool) -> Result<PathBuf> {
    if r::git(&paths.source, &["rev-parse", "HEAD"], flag)? != revision {
        return r::fail("source revision changed before build");
    }
    let cargo = r::find("cargo", &paths.cwd)
        .ok_or_else(|| r::Error(2, "native payload missing and cargo unavailable".into()))?;
    // A caller's temporary override must not mask the incoming checkout's
    // toolchain. Keep Cargo homes and an explicit offline choice intact.
    let result = r::command_without_env(
        cargo,
        &[
            "build".into(),
            "--manifest-path".into(),
            paths.source.join("Cargo.toml").into_os_string(),
            "--locked".into(),
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
        &["RUSTUP_TOOLCHAIN"],
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
    Ok(paths.source.join("target/lifecycle-build/release"))
}

fn prepared(out: crate::Output, phase: &str) -> Result<()> {
    if out.0 == 0 {
        Ok(())
    } else {
        Err(r::Error(
            out.0,
            format!("incoming source {phase} failed; source output withheld"),
        ))
    }
}

/// A cold target is built and self-checked outside the live checkout. Only its
/// frozen payload enters the outer journal; disposable paths never enter it.
pub(crate) fn prepare_incoming(
    paths: &Paths,
    revision: &str,
    tx: &mut Transaction,
    services: &dyn crate::Services,
    flag: &AtomicBool,
) -> Result<bool> {
    prepare_incoming_with_bundle(paths, revision, tx, services, flag, None)
}

/// Explicit bundle imports are authenticated by the caller's trusted archive
/// digest before any of their executable bytes run. This is not a signature or
/// an automatic release trust policy. Incoming source validation stays the same
/// as the source-build path.
pub(crate) fn prepare_incoming_with_bundle(
    paths: &Paths,
    revision: &str,
    tx: &mut Transaction,
    services: &dyn crate::Services,
    flag: &AtomicBool,
    bundle: Option<(&Path, &str)>,
) -> Result<bool> {
    valid_revision(revision)?;
    let imported = bundle
        .map(|(archive, expected)| read_bundle(paths, archive, expected, revision, flag))
        .transpose()?;
    let destination = paths.source.join("target/oms-native").join(revision);
    match fs::symlink_metadata(&destination) {
        // Existing caches, including invalid ones, belong to strict Preflight.
        Ok(_) => {
            if let Some(imported) = &imported {
                imported.recheck(&destination)?;
            }
            return Ok(false);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    r::check(flag)?;
    let original_head = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    let lease = services
        .prepare_incoming_source(&paths.source, revision, flag)
        .map_err(|out| {
            r::Error(
                if out.0 == 0 { 2 } else { out.0 },
                "native incoming source preparation failed; installation unchanged".into(),
            )
        })?;
    let result = (|| {
        let source = r::physical(lease.source_root())?;
        if source == paths.source {
            return r::fail("incoming build must use a separate managed checkout");
        }
        let check = || -> Result<()> {
            lease.recheck(flag).map_err(|out| {
                r::Error(
                    if out.0 == 0 { 2 } else { out.0 },
                    "incoming source ownership changed".into(),
                )
            })?;
            if r::physical(lease.source_root())? != source
                || r::git(&source, &["rev-parse", "HEAD"], flag)? != revision
                || !r::git(
                    &source,
                    &["status", "--porcelain", "--untracked-files=normal"],
                    flag,
                )?
                .is_empty()
            {
                return r::fail("incoming source revision or tracked checkout changed");
            }
            Ok(())
        };
        check()?;
        secure_dir(&source)?;
        let mut scratch = paths.clone();
        scratch.source = source.clone();
        scratch.cwd = source.clone();
        scratch.config = source.join("target/native-preparation");
        let built = match imported {
            Some(built) => built,
            None => {
                let release = build(&scratch, revision, flag)?;
                check()?;
                BuiltPayload::capture(&release, revision)?
            }
        };
        secure_dir(&source.join("target"))?;
        secure_dir(&source.join("target/oms-native"))?;
        let staged = source.join("target/oms-native").join(revision);
        {
            let mut local = Transaction::begin(&scratch)?;
            built.publish_new(&staged, &mut local)?;
            local.commit()?;
        }
        // The old updater's embedded inventory cannot attest changed source.
        // `link --plan` validates with the incoming binary's own inventory.
        built.recheck(&staged)?;
        let out = r::command(
            staged.join("oms"),
            &[
                "--oms-dispatch".into(),
                source.as_os_str().to_owned(),
                revision.into(),
                "--".into(),
                "link".into(),
                "--plan".into(),
            ],
            &source,
            None,
            flag,
            std::time::Duration::from_secs(120),
        )?;
        prepared(out, "self-validation")?;
        check()?;
        built.recheck(&staged)?;
        if r::git(&paths.source, &["rev-parse", "HEAD"], flag)? != original_head
            || !r::git(
                &paths.source,
                &["status", "--porcelain", "--untracked-files=normal"],
                flag,
            )?
            .is_empty()
        {
            return r::fail("live checkout changed during incoming preparation");
        }
        secure_dir(&paths.source.join("target"))?;
        secure_dir(&paths.source.join("target/oms-native"))?;
        // Refuse a cache that appeared while the build was running.
        match fs::symlink_metadata(&destination) {
            Ok(_) => return r::fail("incoming target cache appeared during preparation"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        built.publish_new(&destination, tx)?;
        Ok(true)
    })();
    let cleanup = lease.close(&AtomicBool::new(false));
    match (result, cleanup.0) {
        (Ok(value), 0) => Ok(value),
        (Err(error), 0) => Err(error),
        (Ok(_), _) => Err(r::Error(
            75,
            "incoming source cleanup unconfirmed; ownership record retained".into(),
        )),
        (Err(r::Error(code, message)), _) => Err(r::Error(
            code,
            format!("{message}; incoming source cleanup unconfirmed; ownership record retained"),
        )),
    }
}

fn valid_revision(revision: &str) -> Result<()> {
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return r::fail("invalid native source revision");
    }
    Ok(())
}

/// Bind a prepared receipt to the exact manifest after checking every declared
/// artifact. A manifest hash alone never attests the binary bytes beside it.
pub(crate) fn prepared_digest(paths: &Paths, revision: &str) -> Result<String> {
    valid_revision(revision)?;
    let dir = paths.source.join("target/oms-native").join(revision);
    let mut ancestor = dir.as_path();
    while ancestor.starts_with(&paths.source) {
        let m = fs::symlink_metadata(ancestor)?;
        if !m.is_dir()
            || m.file_type().is_symlink()
            || m.mode() & 0o022 != 0
            || m.uid() != 0 && m.uid() != rustix::process::geteuid().as_raw()
        {
            return r::fail("unsafe prepared native payload directory");
        }
        if ancestor == paths.source {
            break;
        }
        ancestor = ancestor.parent().unwrap();
    }
    let payload = BuiltPayload::read(&dir, revision)?;
    Ok(r::hash(&payload.manifest))
}

fn read_bundle(
    paths: &Paths,
    archive: &Path,
    expected: &str,
    revision: &str,
    flag: &AtomicBool,
) -> Result<BuiltPayload> {
    valid_revision(revision)?;
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return r::fail("native bundle requires a trusted lowercase SHA-256 digest");
    }
    protected(archive, false)?;
    let raw = r::read(archive, 1024 * 1024 * 1024)?;
    if r::hash(&raw) != expected {
        return r::fail("native bundle SHA-256 mismatch; no payload executed");
    }
    r::check(flag)?;
    let work = r::Scratch::new()?;
    // Decompress a private byte snapshot, not a caller path that can change
    // after hashing. The shared extractor bounds output and rejects traversal.
    let snapshot = work.0.join("bundle.tar.gz");
    r::new_file(&snapshot, &raw, 0o600)?;
    let extracted = work.0.join("extracted");
    crate::supply::extract(paths, &snapshot, "tar.gz", &extracted, flag)?;
    let mut entries = fs::read_dir(&extracted)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    if entries != [std::ffi::OsString::from("bundle.json"), "payload".into()] {
        return r::fail("foreign native bundle content");
    }
    let metadata = extracted.join("bundle.json");
    protected(&metadata, false)?;
    let row = r::parse(&r::read(&metadata, 4 * 1024 * 1024)?)?;
    let platform = crate::supply::platform()?;
    if row["schema"] != 1
        || row["kind"] != "oms-native-payload"
        || row["source_commit"].as_str() != Some(revision)
        || row["platform"].as_str() != Some(platform.as_str())
    {
        return r::fail("native bundle schema, revision or host platform mismatch");
    }
    let dir = extracted.join("payload");
    let metadata = fs::symlink_metadata(&dir)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return r::fail("invalid native bundle payload directory");
    }
    let payload = BuiltPayload::read(&dir, revision)?;
    for required in BUILT {
        if !payload.built.iter().any(|(name, _, _)| *name == required) {
            return r::fail("native bundle is missing a required companion");
        }
    }
    Ok(payload)
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
    BuiltPayload::capture(release, revision)?.publish(dir, tx)
}

struct BuiltPayload {
    built: Vec<(&'static str, String, Vec<u8>)>,
    checksum: String,
    manifest: Vec<u8>,
}
impl BuiltPayload {
    fn read(dir: &Path, revision: &str) -> Result<Self> {
        protected(&dir.join("manifest.json"), false)?;
        protected(&dir.join("oms.sha256"), false)?;
        let manifest = r::read(&dir.join("manifest.json"), 4 * 1024 * 1024)?;
        let row = r::parse(&manifest)?;
        if row["schema"] != 1 || row["source_commit"].as_str() != Some(revision) {
            return r::fail("native manifest revision mismatch");
        }
        let artifacts = row["artifacts"]
            .as_object()
            .ok_or_else(|| r::Error(2, "missing native artifacts".into()))?;
        names_allowed(artifacts.keys().map(String::as_str))?;
        let mut built = Vec::new();
        for name in std::iter::once("oms").chain(COMPANIONS) {
            let Some(expected) = artifacts.get(name) else {
                continue;
            };
            let expected = expected.as_str().filter(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            });
            let expected =
                expected.ok_or_else(|| r::Error(2, "invalid native artifact digest".into()))?;
            let file = dir.join(name);
            protected(&file, true)?;
            let raw = r::read(&file, 1024 * 1024 * 1024)?;
            if r::hash(&raw) != expected {
                return r::fail("native artifact digest mismatch");
            }
            built.push((name, expected.to_owned(), raw));
        }
        let checksum = format!("{}\n", built[0].1);
        if r::read(&dir.join("oms.sha256"), 65)? != checksum.as_bytes() {
            return r::fail("invalid native executable checksum");
        }
        let payload = Self {
            built,
            checksum,
            manifest,
        };
        payload.check_names(dir)?;
        Ok(payload)
    }

    fn capture(release: &Path, revision: &str) -> Result<Self> {
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
        Ok(Self {
            built,
            checksum,
            manifest,
        })
    }
    fn files(&self) -> Vec<(&str, &[u8], u32)> {
        let mut files: Vec<(&str, &[u8], u32)> = self
            .built
            .iter()
            .map(|(name, _, raw)| (*name, raw.as_slice(), 0o755))
            .collect();
        files.push(("oms.sha256", self.checksum.as_bytes(), 0o600));
        files.push(("manifest.json", &self.manifest, 0o600));
        files
    }
    fn publish(&self, dir: &Path, tx: &mut Transaction) -> Result<()> {
        tx.files(dir, &self.files())
    }
    fn publish_new(&self, dir: &Path, tx: &mut Transaction) -> Result<()> {
        tx.create_files(dir, &self.files())
    }
    fn recheck(&self, dir: &Path) -> Result<()> {
        let files = self.files();
        for (name, body, _) in &files {
            protected(
                &dir.join(name),
                *name != "manifest.json" && *name != "oms.sha256",
            )?;
            if r::read(&dir.join(name), 1024 * 1024 * 1024)? != *body {
                return r::fail("incoming native payload changed during source validation");
            }
        }
        self.check_names(dir)
    }
    fn check_names(&self, dir: &Path) -> Result<()> {
        let files = self.files();
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name();
            if !files.iter().any(|(expected, _, _)| name == *expected) {
                return r::fail("foreign incoming native payload content");
            }
        }
        Ok(())
    }
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
    fn imported_bundles_bind_archive_host_revision_and_every_companion() {
        let root = r::Scratch::new().unwrap();
        let paths = paths(&root.0);
        let revision = "1".repeat(40);
        let release = root.0.join("release");
        for name in std::iter::once("oms").chain(BUILT) {
            write(&release.join(name), name.as_bytes());
        }
        let directory = root.0.join("package");
        let payload = directory.join("payload");
        let mut tx = Transaction::begin(&paths).unwrap();
        publish(&payload, &release, &revision, &mut tx).unwrap();
        tx.commit().unwrap();
        let metadata = serde_json::json!({
            "schema": 1,
            "kind": "oms-native-payload",
            "source_commit": revision,
            "platform": crate::supply::platform().unwrap(),
        });
        write(
            &directory.join("bundle.json"),
            &r::bytes(&metadata).unwrap(),
        );
        let archive = root.0.join("bundle.tar.gz");
        let pack = || {
            assert!(
                std::process::Command::new(r::find("tar", &root.0).unwrap())
                    .args(["--format=ustar", "-czf"])
                    .arg(&archive)
                    .arg("-C")
                    .arg(&directory)
                    .args(["bundle.json", "payload"])
                    .status()
                    .unwrap()
                    .success()
            );
            r::hash(&fs::read(&archive).unwrap())
        };
        let flag = AtomicBool::new(false);
        let digest = pack();
        let imported = read_bundle(&paths, &archive, &digest, &revision, &flag).unwrap();
        assert_eq!(imported.built.len(), BUILT.len() + 1);
        assert!(read_bundle(&paths, &archive, &"0".repeat(64), &revision, &flag).is_err());
        assert!(read_bundle(&paths, &archive, &digest, &"2".repeat(40), &flag).is_err());
        let mut wrong_platform = metadata.clone();
        wrong_platform["platform"] = "foreign-host".into();
        write(
            &directory.join("bundle.json"),
            &r::bytes(&wrong_platform).unwrap(),
        );
        assert!(read_bundle(&paths, &archive, &pack(), &revision, &flag).is_err());
        write(
            &directory.join("bundle.json"),
            &r::bytes(&metadata).unwrap(),
        );
        let companion = payload.join(BUILT[0]);
        write(&companion, b"changed companion");
        assert!(read_bundle(&paths, &archive, &pack(), &revision, &flag).is_err());
        write(&companion, BUILT[0].as_bytes());
        let saved = fs::read(&companion).unwrap();
        fs::remove_file(&companion).unwrap();
        symlink("oms", &companion).unwrap();
        assert!(read_bundle(&paths, &archive, &pack(), &revision, &flag).is_err());
        fs::remove_file(&companion).unwrap();
        write(&companion, &saved);
        write(&payload.join("foreign"), b"extra");
        assert!(read_bundle(&paths, &archive, &pack(), &revision, &flag).is_err());
        fs::remove_file(payload.join("foreign")).unwrap();

        let cached = paths.source.join("target/oms-native").join(&revision);
        let mut tx = Transaction::begin(&paths).unwrap();
        imported.publish_new(&cached, &mut tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            prepared_digest(&paths, &revision).unwrap(),
            r::hash(&imported.manifest)
        );
        write(&cached.join(BUILT[0]), b"post-prepare corruption");
        assert!(prepared_digest(&paths, &revision).is_err());
        assert!(prepared_digest(&paths, "../other").is_err());
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
        let frozen = BuiltPayload::capture(&release, &rev).unwrap();
        frozen.recheck(&dir).unwrap();
        assert!(frozen.publish_new(&dir, &mut tx).is_err());
        assert_eq!(fs::read(dir.join("oms")).unwrap(), b"oms");
        fs::write(dir.join("oms"), b"changed").unwrap();
        assert!(frozen.recheck(&dir).is_err());
        fs::write(dir.join("oms"), b"oms").unwrap();
        fs::write(dir.join("foreign"), b"foreign").unwrap();
        assert!(frozen.recheck(&dir).is_err());
        fs::remove_file(dir.join("foreign")).unwrap();
        frozen.recheck(&dir).unwrap();
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
