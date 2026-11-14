use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) struct Payload {
    pub executable: PathBuf,
    pub helper: Option<PathBuf>,
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
    let dir = paths.source.join("rust/target/oms-native").join(&revision);
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
    if !artifacts.contains_key("oms")
        || artifacts
            .keys()
            .any(|k| !matches!(k.as_str(), "oms" | "oms-graph-python-helper"))
    {
        return r::fail("invalid native artifact names");
    }
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
    Ok(Payload {
        executable: proof.executable(),
        helper: proof.helper(),
        revision,
        discovery,
        proof,
    })
}
pub(crate) fn install(paths: &Paths, payload: &Payload, tx: &mut Transaction) -> Result<()> {
    payload.recheck()?;
    let dir = paths.data.join("libexec").join(&payload.revision);
    for (name, source) in std::iter::once(("oms", &payload.executable)).chain(
        payload
            .helper
            .as_ref()
            .map(|p| ("oms-graph-python-helper", p)),
    ) {
        let target = dir.join(name);
        if r::exists(&target)
            && r::read(&target, 1024 * 1024 * 1024)? != r::read(source, 1024 * 1024 * 1024)?
        {
            return r::fail(
                "refusing to replace an existing native libexec payload with different bytes",
            );
        }
        if !r::exists(&target) {
            tx.write(&target, &r::read(source, 1024 * 1024 * 1024)?, 0o755)?;
        }
    }
    let digest = r::hash(&r::read(&payload.executable, 1024 * 1024 * 1024)?);
    let executable = r::shell(payload.executable.as_os_str())?;
    let wrapper = format!(
        "#!/bin/sh\n# oh-my-setting native lifecycle\nif command -v sha256sum >/dev/null 2>&1; then\n  observed=$(sha256sum < {executable}) || exit 2\n  observed=${{observed%% *}}\nelif command -v shasum >/dev/null 2>&1; then\n  observed=$(shasum -a 256 < {executable}) || exit 2\n  observed=${{observed%% *}}\nelse\n  echo 'error: native payload hash tool unavailable' >&2\n  exit 2\nfi\n[ \"$observed\" = '{digest}' ] || {{ echo 'error: native payload changed; run oms doctor --repair' >&2; exit 2; }}\nexec {executable} --oms-dispatch {} {} -- \"$@\"\n",
        r::shell(paths.source.as_os_str())?,
        payload.revision
    );
    tx.write(&payload.frontdoor(paths), wrapper.as_bytes(), 0o755)?;
    payload.recheck()
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
    let dir = paths.source.join("rust/target/oms-native").join(&revision);
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
            paths.source.join("rust/Cargo.toml").into_os_string(),
            "--locked".into(),
            "--offline".into(),
            "--release".into(),
            "--bin".into(),
            "oms".into(),
            "--target-dir".into(),
            paths
                .source
                .join("rust/target/lifecycle-build")
                .into_os_string(),
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
    let executable = paths.source.join("rust/target/lifecycle-build/release/oms");
    protected(&executable, true)?;
    let raw = r::read(&executable, 1024 * 1024 * 1024)?;
    let digest = r::hash(&raw);
    let row = serde_json::json!({"schema":1,"source_commit":revision,"artifacts":{"oms":digest}});
    tx.files(
        &dir,
        &[
            ("oms", &raw, 0o755),
            ("oms.sha256", format!("{digest}\n").as_bytes(), 0o600),
            ("manifest.json", &r::bytes(&row)?, 0o600),
        ],
    )?;
    verify(paths, flag)?.recheck()
}
