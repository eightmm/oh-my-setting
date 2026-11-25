//! Managed capture creation/publication. The transaction holds the existing
//! landings lock; it never adopts files discovered by name or matching bytes.
use crate::{
    Result, V,
    capture::{self, CaptureIndex},
    checkpoint, digest, encoded, obj, path_value, storage, text,
};
use std::{
    ffi::OsString,
    fs::{self, DirBuilder},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt},
    },
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub struct Producer {
    pub(crate) index: CaptureIndex,
    _guard: oms_lock::Guard,
}
#[derive(Debug)]
pub enum AllocationError {
    Quota(String),
    Invalid(String),
}
impl AllocationError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Quota(_) => 3,
            Self::Invalid(_) => 2,
        }
    }
}
impl std::fmt::Display for AllocationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quota(message) | Self::Invalid(message) => formatter.write_str(message),
        }
    }
}
impl std::error::Error for AllocationError {}
impl From<String> for AllocationError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}
impl From<&str> for AllocationError {
    fn from(message: &str) -> Self {
        Self::Invalid(message.to_owned())
    }
}
impl Producer {
    /// Lock ordering: landings precedes marker-set, plan, lifecycle and index.
    /// A caller already holding landings must use its existing producer, never
    /// reopen a nested transaction. Managed creation is parent-only.
    pub fn open(repo: &Path, interrupted: &AtomicBool) -> Result<Self> {
        parent_guard()?;
        let repo = fs::canonicalize(repo).map_err(|e| e.to_string())?;
        let guard = storage::acquire(&repo.join(".oms/landings.jsonl"), interrupted)?;
        storage::ensure_parent(&repo, Path::new(".oms/landings.jsonl"))?;
        let index = CaptureIndex::load(&repo)?;
        Ok(Self {
            index,
            _guard: guard,
        })
    }
    pub fn index(&self) -> &CaptureIndex {
        &self.index
    }
    pub fn allocate(
        &mut self,
        source: &Path,
        landing: &str,
        expected: &str,
        execution_repo: &Path,
        interrupted: &AtomicBool,
    ) -> std::result::Result<PathBuf, AllocationError> {
        checkpoint(interrupted)?;
        if !capture::landing_id(landing) {
            return Err("invalid native landing identity".into());
        }
        let raw = match self.index.indexed_bytes(source)? {
            Some(raw) => raw,
            None => storage::regular(source, storage::MAX_OBJECT, 1)?.0,
        };
        if digest(&raw) != expected {
            return Err("source changed before quota reservation".into());
        }
        if self
            .index
            .reservations
            .iter()
            .any(|v| v.get("landing_id").text() == Some(landing))
        {
            return Err("native landing already reserved; use its existing recovery path".into());
        }
        let (count, size) = self.usage()?;
        let count_limit = limit("OMS_LANDING_CAPTURE_COUNT", 2048)?;
        let bytes_limit = limit("OMS_LANDING_CAPTURE_BYTES", 128 * 1024 * 1024)?;
        let needed = 2 * raw.len() as u64;
        if count + 1 > count_limit || size.checked_add(needed).is_none_or(|v| v > bytes_limit) {
            return Err(AllocationError::Quota(format!(
                "managed landing capture quota exhausted (count {count}/{count_limit}, bytes {size}/{bytes_limit}, need {needed} bytes); run `oms gc --apply` or raise OMS_LANDING_CAPTURE_COUNT / OMS_LANDING_CAPTURE_BYTES"
            )));
        }
        let execution = fs::canonicalize(execution_repo).map_err(|e| e.to_string())?;
        let device = fs::metadata(&self.index.repo)
            .map_err(|e| e.to_string())?
            .dev();
        let cache_root =
            capture_cache_root(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"));
        let parent = select_capture_parent(
            &self.index.repo,
            &execution,
            device,
            &std::env::temp_dir(),
            cache_root.as_deref(),
        )?;
        let generation = storage::random_hex(16)?;
        let directory = parent.join(format!("oms-landing-capture-{generation}"));
        let item = obj([
            ("version", 1usize.into()),
            ("generation", generation.into()),
            ("landing_id", landing.into()),
            (
                "namespace",
                digest(self.index.repo.as_os_str().as_bytes()).into(),
            ),
            ("directory", path_value(&directory)),
            ("bytes", V::Num(needed.to_string())),
            ("sha256", expected.into()),
        ]);
        // The reservation is durable before any allocation. A kill at every
        // later step leaves a charged, unsealed generation, never free space.
        self.index
            .append_event("capture-reserved", &item, obj([]))?;
        checkpoint(interrupted)?;
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|e| e.to_string())?;
        let info = storage::private_directory(&directory)?;
        let mut certificate = obj([
            ("reservation", item.clone()),
            ("directory", storage::identity(&info)),
            ("objects", obj([])),
            ("terminal", V::Null),
        ]);
        storage::create_private(&directory, Path::new("owner.json"), &encoded(&certificate)?)?;
        storage::sync_directory(&parent)?;
        checkpoint(interrupted)?;
        let target = directory.join(format!("{landing}.patch"));
        let payload_info = storage::create_private(
            &directory,
            Path::new(target.file_name().ok_or("capture has no name")?),
            &raw,
        )?;
        let anchor = directory.join("capture.anchor");
        storage::link(
            &target,
            &directory,
            Path::new("capture.anchor"),
            &payload_info,
        )?;
        let object = obj([
            ("identity", storage::identity(&payload_info)),
            ("size", raw.len().into()),
            ("path", path_value(&target)),
            ("anchor", path_value(&anchor)),
        ]);
        certificate.set("objects", obj([("capture", object)]));
        let before = self.index.certificate(&item)?;
        checkpoint(interrupted)?;
        before.save(&certificate)?;
        Ok(target)
    }
    pub fn publish(
        &mut self,
        capture: &Path,
        target: &Path,
        interrupted: &AtomicBool,
    ) -> Result<()> {
        checkpoint(interrupted)?;
        let item = self.index.find(Some(capture), None)?;
        let certificate = self.index.certificate(item)?;
        let raw = self
            .index
            .indexed_bytes(capture)?
            .ok_or("managed admission input ownership unavailable")?;
        let target = storage::absolute(&self.index.repo.join(target))?;
        let relative = target
            .strip_prefix(&self.index.repo)
            .map_err(|_| "landing publication must stay inside the physical repository")?;
        storage::ensure_parent(&self.index.repo, relative)?;
        let root = PathBuf::from(text(item, "directory")?);
        let anchor = root.join("publication.anchor");
        let anchor_info = storage::create_private(&root, Path::new("publication.anchor"), &raw)?;
        let object = obj([
            ("identity", storage::identity(&anchor_info)),
            ("size", raw.len().into()),
            ("path", path_value(&target)),
            ("anchor", path_value(&anchor)),
        ]);
        let mut value = certificate.value.clone();
        let mut objects = value.get("objects").clone();
        objects.set("publication", object);
        value.set("objects", objects);
        certificate.save(&value)?;
        checkpoint(interrupted)?;
        self.index.check()?;
        storage::link(&anchor, &self.index.repo, relative, &anchor_info)
    }
    /// Only an exact terminal proof seals a generation. Lifecycle completion,
    /// report admission, and landing completion are distinct events.
    pub fn seal(&mut self, landing: &str, reason: &str, interrupted: &AtomicBool) -> Result<()> {
        checkpoint(interrupted)?;
        if !self
            .index
            .reservations
            .iter()
            .any(|v| v.get("landing_id").text() == Some(landing))
        {
            self.index.check()?;
            return Ok(());
        }
        let item = self.index.find(None, Some(landing))?.clone();
        let certificate = self.index.certificate(&item)?;
        let proof = terminal_proof(&self.index, &item, &certificate.value, reason)?;
        let sealed = certificate.value.get("terminal");
        if sealed.truth()
            && sealed.get("proof") == &proof
            && self.index.rows.iter().any(|row| {
                encoded(row).is_ok_and(|v| sealed.get("event_sha256").text() == Some(&digest(&v)))
            })
        {
            return Ok(());
        }
        let V::Obj(objects) = certificate.value.get("objects") else {
            return Err("unknown capture object inventory".into());
        };
        for object in objects.values() {
            capture::object_bytes(&item, object, 2)?;
            let info = fs::symlink_metadata(Path::new(text(object, "path")?))
                .map_err(|e| e.to_string())?;
            if !info.is_file() || storage::identity(&info) != *object.get("identity") {
                return Err("cannot seal replaced capture".into());
            }
        }
        checkpoint(interrupted)?;
        let row = self.index.append_event(
            "capture-terminal",
            &item,
            obj([
                ("reason", reason.into()),
                ("native_terminal_proof", proof.clone()),
            ]),
        )?;
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?;
        let mut value = certificate.value.clone();
        value.set(
            "terminal",
            obj([
                ("event_sha256", digest(&encoded(&row)?).into()),
                ("time", V::Num(elapsed.as_secs_f64().to_string())),
                ("reason", reason.into()),
                ("proof", proof),
            ]),
        );
        certificate.save(&value)
    }
    pub fn usage(&self) -> Result<(u64, u64)> {
        let mut count = 0u64;
        let mut bytes = 0u64;
        for row in &self.index.rows {
            if matches!(
                row.get("event").text(),
                Some("capture-release-ready" | "capture-released")
            ) && !self
                .index
                .reservations
                .iter()
                .any(|v| v.get("generation") == row.get("managed_capture").get("generation"))
            {
                return Err("release has no native reservation".into());
            }
        }
        for item in &self.index.reservations {
            let outcome: Result<Option<u64>> = (|| {
                let ready = release_ready(&self.index, item)?;
                let released = self
                    .index
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| {
                        r.get("event").text() == Some("capture-released")
                            && !matches!(r.get("release_missing"), V::Bool(true))
                            && r.get("managed_capture").get("generation") == item.get("generation")
                    })
                    .collect::<Vec<_>>();
                for (position, row) in &released {
                    let (ready_position, ready) =
                        ready.ok_or("invalid native capture release evidence")?;
                    if row.get("schema") != &1usize.into()
                        || row.get("managed_capture") != item
                        || row.get("landing_id") != item.get("landing_id")
                        || row.get("release_ready_sha256").text() != Some(&digest(&encoded(ready)?))
                        || *position <= ready_position
                    {
                        return Err("invalid native capture release evidence".into());
                    }
                }
                let root = Path::new(text(item, "directory")?);
                if !capture::exists(root)? {
                    if !released.is_empty()
                        || self.index.rows.iter().any(|r| {
                            r.get("event").text() == Some("capture-released")
                                && matches!(r.get("release_missing"), V::Bool(true))
                                && r.get("schema") == &1usize.into()
                                && r.get("managed_capture") == item
                                && r.get("landing_id") == item.get("landing_id")
                        })
                    {
                        return Ok(None);
                    }
                    let public = self
                        .index
                        .repo
                        .join(".oms/landing-patches")
                        .join(format!("{}.patch", text(item, "landing_id")?));
                    let namespace = storage::Namespace::snapshot(&self.index.repo, &public)?;
                    if capture::exists(&public)? {
                        let info = fs::symlink_metadata(&public).map_err(|e| e.to_string())?;
                        if !info.is_file()
                            || item.get("bytes").py().parse::<u64>().ok()
                                != info.size().checked_mul(2)
                        {
                            return Err("publication contradicts quota reservation".into());
                        }
                    }
                    namespace.check()?;
                } else {
                    let certificate = self.index.certificate(item)?;
                    let V::Obj(objects) = certificate.value.get("objects") else {
                        return Err("unknown quota object inventory".into());
                    };
                    if objects.is_empty()
                        || objects
                            .keys()
                            .any(|k| !matches!(k.as_slice(), b"capture" | b"publication"))
                    {
                        return Err("unproved quota object inventory".into());
                    }
                    for object in objects.values() {
                        if !object.get("size").integer(false)
                            || object
                                .get("size")
                                .py()
                                .parse::<u64>()
                                .ok()
                                .and_then(|v| v.checked_mul(2))
                                != item.get("bytes").py().parse::<u64>().ok()
                        {
                            return Err("quota reservation contradicts actual object size".into());
                        }
                        let links = fs::symlink_metadata(text(object, "anchor")?)
                            .map_err(|e| e.to_string())?
                            .nlink();
                        if !matches!(links, 1 | 2) {
                            return Err("unknown quota anchor links".into());
                        }
                        capture::object_bytes(item, object, links)?;
                    }
                    certificate.check()?;
                }
                Ok(Some(
                    item.get("bytes")
                        .py()
                        .parse::<u64>()
                        .map_err(|e| e.to_string())?,
                ))
            })();
            let charge = match outcome {
                Ok(None) => continue,
                Ok(Some(charge)) => charge,
                Err(_) => 2 * storage::MAX_OBJECT as u64,
            };
            count = count.checked_add(1).ok_or("quota count overflow")?;
            bytes = bytes.checked_add(charge).ok_or("quota bytes overflow")?;
        }
        self.index.check()?;
        Ok((count, bytes))
    }
}

pub fn parent_guard() -> Result<()> {
    if std::env::var_os("OMS_HARNESS_CHILD").as_deref() == Some(std::ffi::OsStr::new("1"))
        || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH").is_some_and(|v| !v.is_empty() && v != "0")
    {
        Err("native capture management is parent-only".into())
    } else {
        Ok(())
    }
}

fn capture_cache_root(xdg_cache_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = xdg_cache_home
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".cache"))
        })?;
    let base = storage::absolute(&base).ok()?;
    Some(base.join("oh-my-setting").join("landing-captures"))
}

fn fits_capture_parent(candidate: &Path, repo: &Path, execution: &Path, device: u64) -> bool {
    !candidate.starts_with(repo)
        && !candidate.starts_with(execution)
        && fs::metadata(candidate).is_ok_and(|v| v.dev() == device)
}

fn select_capture_parent(
    repo: &Path,
    execution: &Path,
    device: u64,
    temp_dir: &Path,
    cache_root: Option<&Path>,
) -> std::result::Result<PathBuf, AllocationError> {
    if let Ok(candidate) = fs::canonicalize(temp_dir)
        && fits_capture_parent(&candidate, repo, execution, device)
    {
        return Ok(candidate);
    }
    if let Some(cache_root) = cache_root {
        if !cache_root.starts_with(repo)
            && !cache_root.starts_with(execution)
            && DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(cache_root)
                .is_ok()
            && let Ok(candidate) = fs::canonicalize(cache_root)
            && fits_capture_parent(&candidate, repo, execution, device)
        {
            return Ok(candidate);
        }
        return Err(format!(
            "no same-filesystem private capture parent outside state repository (tried cache root {})",
            cache_root.display()
        )
        .into());
    }
    Err(
        "no same-filesystem private capture parent outside state repository (cache root unavailable: set XDG_CACHE_HOME or HOME)"
            .into(),
    )
}
fn limit(name: &str, default: u64) -> Result<u64> {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_string());
    if value.is_empty()
        || value.len() > 13
        || value.starts_with('0')
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(format!("invalid {name}"));
    }
    value.parse().map_err(|_| format!("invalid {name}"))
}

pub(crate) fn terminal_proof(index: &CaptureIndex, item: &V, value: &V, reason: &str) -> Result<V> {
    let own = index
        .rows
        .iter()
        .filter(|r| r.get("landing_id") == item.get("landing_id"))
        .collect::<Vec<_>>();
    let intents = own
        .iter()
        .copied()
        .filter(|r| r.get("event").text() == Some("intent"))
        .collect::<Vec<_>>();
    let publication = value.get("objects").get("publication");
    match reason {
        "publication-without-intent" => {
            if !intents.is_empty() || !publication.truth() {
                return Err("no-intent publication history is contradictory".into());
            }
            let proof = capture::admission_proof_value(index, item, value, 0)?;
            return Ok(obj([
                ("phase", reason.into()),
                ("admission_sha256", digest(&encoded(&proof)?).into()),
            ]));
        }
        "admission-rejected" => {
            if !intents.is_empty() || publication.truth() {
                return Err("rejected input was already published".into());
            }
            let proof = capture::admission_proof_value(index, item, value, 1)?;
            return Ok(obj([
                ("phase", reason.into()),
                ("admission_sha256", digest(&encoded(&proof)?).into()),
            ]));
        }
        "before-admission" => {
            if !intents.is_empty() || publication.truth() {
                return Err("pre-publication seal contradicts landing history".into());
            }
            return Ok(obj([
                ("phase", "pre-publication".into()),
                ("reason", reason.into()),
            ]));
        }
        _ => {}
    }
    if !matches!(reason, "complete" | "abandoned") || intents.len() != 1 || !publication.truth() {
        return Err("exact native terminal landing identity unavailable".into());
    }
    let intent = intents[0];
    let fields = [
        "landing_id",
        "patch",
        "patch_sha",
        "base_sha",
        "task",
        "lease",
        "plan_receipt_sha",
        "plan_done_receipt_sha",
        "approval",
        "approval_version",
    ];
    if intent.get("schema") != &1usize.into()
        || fields.iter().any(|k| intent.get(k).text().is_none())
    {
        return Err("malformed native landing intent".into());
    }
    let mut canonical = obj([("schema", 1usize.into())]);
    for field in &fields {
        canonical.set(field, intent.get(field).clone());
    }
    // Landing intent receipts deliberately use UTF-8 and no final newline.
    let receipt = digest(&canonical.json(false)?);
    let terminals = own
        .iter()
        .copied()
        .filter(|r| matches!(r.get("event").text(), Some("complete" | "abandoned")))
        .collect::<Vec<_>>();
    if intent.get("patch") != publication.get("path")
        || intent.get("patch_sha") != item.get("sha256")
        || intent.get("receipt_sha").text() != Some(&receipt)
        || terminals.is_empty()
        || terminals.iter().any(|r| {
            r.get("event").text() != Some(reason)
                || r.get("schema") != &1usize.into()
                || r.get("receipt_sha").text() != Some(&receipt)
                || r.default("plan_id", "") != intent.default("plan_id", "")
                || fields.iter().any(|k| r.get(k) != intent.get(k))
        })
    {
        return Err("native terminal receipt is missing or contradictory".into());
    }
    index.check()?;
    Ok(obj([
        ("phase", "landing".into()),
        ("intent_sha256", digest(&encoded(intent)?).into()),
        (
            "terminal_sha256",
            digest(&encoded(terminals.last().ok_or("terminal unavailable")?)?).into(),
        ),
        ("receipt_sha256", receipt.into()),
    ]))
}

/// Validate existing collector receipts for quota accounting. This reader
/// does not collect, infer quiescence, or grant a caller deletion authority.
fn release_ready<'a>(index: &'a CaptureIndex, item: &V) -> Result<Option<(usize, &'a V)>> {
    let candidates = index
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.get("event").text() == Some("capture-release-ready")
                && r.get("managed_capture").get("generation") == item.get("generation")
        })
        .collect::<Vec<_>>();
    for (position, row) in &candidates {
        let proof = row.get("release_proof");
        let terminal = index.rows[..*position]
            .iter()
            .filter(|r| {
                encoded(r).is_ok_and(|raw| {
                    proof.get("terminal_event_sha256").text() == Some(&digest(&raw))
                })
            })
            .collect::<Vec<_>>();
        let value = proof.get("certificate");
        let objects = proof.get("objects");
        if row.get("schema") != &1usize.into()
            || row.get("managed_capture") != item
            || row.get("landing_id") != item.get("landing_id")
            || proof.get("version") != &1usize.into()
            || proof.get("reservation_sha256").text() != Some(&digest(&encoded(item)?))
            || terminal.len() != 1
            || terminal[0].get("event").text() != Some("capture-terminal")
            || terminal[0].get("managed_capture") != item
            || !objects.get("capture").truth()
            || !matches!(objects,V::Obj(fields) if fields.keys().all(|k| matches!(k.as_slice(),b"capture"|b"publication")))
        {
            return Err("invalid native release readiness evidence".into());
        }
        if !matches!(value,V::Obj(fields) if fields.len() == 4 || (fields.len() == 5 && fields.contains_key(b"admission".as_slice())))
            || value.get("reservation") != item
            || proof.get("certificate_sha256").text() != Some(&digest(&encoded(value)?))
            || value.get("objects") != objects
            || !matches!(value.get("directory"),V::Arr(v) if v.len() == 2 && v.iter().all(|v| v.integer(false)))
        {
            return Err("release certificate inventory is unproven".into());
        }
        let sealed = value.get("terminal");
        let time = sealed.get("time").py().parse::<f64>().ok();
        if !matches!(sealed,V::Obj(v) if v.len() == 4)
            || time.is_none_or(|v| !v.is_finite() || v < 0.0)
            || sealed.get("event_sha256").text() != Some(&digest(&encoded(terminal[0])?))
            || terminal[0].get("reason") != sealed.get("reason")
            || terminal[0].get("native_terminal_proof") != sealed.get("proof")
        {
            return Err("release lacks its exact native terminal provenance".into());
        }
        // terminal_proof consumes only immutable certificate bytes and the
        // fresh lifecycle/report evidence. A removed directory is not adopted.
        if sealed.get("proof") != &terminal_proof(index, item, value, text(sealed, "reason")?)? {
            return Err("release lacks its exact native terminal provenance".into());
        }
        let V::Obj(objects) = objects else {
            return Err("invalid release object inventory".into());
        };
        for (name, object) in objects {
            let name = std::str::from_utf8(name).map_err(|_| "invalid release object name")?;
            let expected = if name == "capture" {
                capture::capture_path(item)?
            } else {
                index
                    .repo
                    .join(".oms/landing-patches")
                    .join(format!("{}.patch", text(item, "landing_id")?))
            };
            let anchor = Path::new(text(item, "directory")?).join(format!("{name}.anchor"));
            if !matches!(object,V::Obj(v) if v.len() == 4)
                || object.get("path") != &path_value(&expected)
                || object.get("anchor") != &path_value(&anchor)
                || !matches!(object.get("identity"),V::Arr(v) if v.len() == 2 && v.iter().all(|v| v.integer(false)))
                || !object.get("size").integer(false)
                || object
                    .get("size")
                    .py()
                    .parse::<u64>()
                    .ok()
                    .and_then(|v| v.checked_mul(2))
                    != item.get("bytes").py().parse::<u64>().ok()
            {
                return Err("release object generation inventory is incomplete".into());
            }
        }
    }
    if let Some((_, first)) = candidates.first() {
        let expected = encoded(first.get("release_proof"))?;
        if candidates
            .iter()
            .any(|(_, r)| encoded(r.get("release_proof")).map_or(true, |raw| raw != expected))
        {
            return Err("conflicting native release readiness".into());
        }
    }
    Ok(candidates.last().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::ErrorKind, os::unix::fs::PermissionsExt};

    struct TestRoot(PathBuf);
    impl TestRoot {
        fn new(name: &str) -> Self {
            let tmp = fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = (0u32..)
                .find_map(|i| {
                    let candidate =
                        tmp.join(format!("oms-producer-{name}-{}-{i}", std::process::id()));
                    match DirBuilder::new().mode(0o700).create(&candidate) {
                        Ok(()) => Some(candidate),
                        Err(e) if e.kind() == ErrorKind::AlreadyExists => None,
                        Err(e) => panic!("create test root: {e}"),
                    }
                })
                .unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn capture_cache_root_resolves_xdg_then_home() {
        let xdg = OsString::from("/var/cache/custom");
        let account = OsString::from("/opt/account");
        assert_eq!(
            capture_cache_root(Some(xdg), Some(account.clone())),
            Some(PathBuf::from(
                "/var/cache/custom/oh-my-setting/landing-captures"
            ))
        );
        assert_eq!(
            capture_cache_root(Some(OsString::new()), Some(account.clone())),
            Some(PathBuf::from(
                "/opt/account/.cache/oh-my-setting/landing-captures"
            ))
        );
        assert_eq!(
            capture_cache_root(
                Some(OsString::from("relative/cache")),
                Some(account.clone())
            ),
            Some(PathBuf::from(
                "/opt/account/.cache/oh-my-setting/landing-captures"
            ))
        );
        assert_eq!(
            capture_cache_root(None, Some(account)),
            Some(PathBuf::from(
                "/opt/account/.cache/oh-my-setting/landing-captures"
            ))
        );
        assert_eq!(capture_cache_root(None, None), None);
        assert_eq!(
            capture_cache_root(Some(OsString::new()), Some(OsString::from("rel"))),
            None
        );
    }

    #[test]
    fn select_capture_parent_prefers_tmpdir_then_cache_root() {
        let root = TestRoot::new("select-parent");
        let projects = root.path().join("projects");
        let repo = projects.join("project");
        let execution = root.path().join("execution");
        let outside_tmp = root.path().join("outside-tmp");
        let tmp_in_repo = repo.join("tmp");
        let tmp_in_exec = execution.join("tmp");
        for dir in [&repo, &execution, &outside_tmp, &tmp_in_repo, &tmp_in_exec] {
            fs::create_dir_all(dir).unwrap();
        }
        let device = fs::metadata(&repo).unwrap().dev();

        // Outside TMPDIR wins and leaves the cache root uncreated.
        let unused_cache = root
            .path()
            .join("unused-cache/oh-my-setting/landing-captures");
        assert_eq!(
            select_capture_parent(&repo, &execution, device, &outside_tmp, Some(&unused_cache))
                .unwrap(),
            outside_tmp
        );
        assert!(!unused_cache.exists());

        // TMPDIR inside repo falls back to the cache root, never repo.parent().
        let cache_root = root.path().join("cache/oh-my-setting/landing-captures");
        assert_eq!(
            select_capture_parent(&repo, &repo, device, &tmp_in_repo, Some(&cache_root)).unwrap(),
            cache_root
        );
        assert_eq!(
            fs::symlink_metadata(&cache_root)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(fs::read_dir(&projects).unwrap().count(), 1);

        // TMPDIR inside a separate execution repo also falls back to the cache root.
        assert_eq!(
            select_capture_parent(&repo, &execution, device, &tmp_in_exec, Some(&cache_root))
                .unwrap(),
            cache_root
        );

        // Cache root inside repo or execution repo is rejected without creating it.
        let cache_in_repo = repo.join("cache/oh-my-setting/landing-captures");
        let err = select_capture_parent(&repo, &repo, device, &tmp_in_repo, Some(&cache_in_repo))
            .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains(&cache_in_repo.display().to_string())
        );
        assert!(!repo.join("cache").exists());

        let cache_in_exec = execution.join("cache/oh-my-setting/landing-captures");
        let err = select_capture_parent(
            &repo,
            &execution,
            device,
            &tmp_in_exec,
            Some(&cache_in_exec),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains(&cache_in_exec.display().to_string())
        );
        assert!(!execution.join("cache").exists());

        // Uncreatable cache root (blocked by a regular file) is skipped and fails closed.
        let blocked = root.path().join("blocked");
        fs::write(&blocked, b"file").unwrap();
        let blocked_cache = blocked.join("oh-my-setting/landing-captures");
        let err = select_capture_parent(&repo, &repo, device, &tmp_in_repo, Some(&blocked_cache))
            .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains(&blocked_cache.display().to_string())
        );

        // Different filesystem or unavailable cache root fails closed.
        let wrong_dev = select_capture_parent(
            &repo,
            &repo,
            device.wrapping_add(1),
            &outside_tmp,
            Some(&cache_root),
        )
        .unwrap_err();
        assert_eq!(wrong_dev.exit_code(), 2);
        assert!(
            wrong_dev
                .to_string()
                .contains(&cache_root.display().to_string())
        );

        let no_cache = select_capture_parent(&repo, &repo, device, &tmp_in_repo, None).unwrap_err();
        assert_eq!(no_cache.exit_code(), 2);
        assert!(
            no_cache
                .to_string()
                .contains("no same-filesystem private capture parent outside state repository")
        );
        assert_eq!(fs::read_dir(&projects).unwrap().count(), 1);
    }

    #[test]
    fn existing_capture_remains_readable_wherever_recorded() {
        let root = TestRoot::new("legacy-read");
        let projects = root.path().join("projects");
        let repo = projects.join("project");
        fs::create_dir_all(repo.join(".oms")).unwrap();
        let body = b"diff --git a/a.txt b/a.txt\n-old\n+new\n".to_vec();
        let landing = "land-20261125T000000Z-0000000000000020";
        let generation = "0123456789abcdef0123456789abcdef";
        let directory = projects.join(format!("oms-landing-capture-{generation}"));
        let item = obj([
            ("version", 1usize.into()),
            ("generation", generation.into()),
            ("landing_id", landing.into()),
            ("namespace", digest(repo.as_os_str().as_bytes()).into()),
            ("directory", path_value(&directory)),
            ("bytes", V::Num((2 * body.len()).to_string())),
            ("sha256", digest(&body).into()),
        ]);
        let mut index = CaptureIndex::load(&repo).unwrap();
        index
            .append_event("capture-reserved", &item, obj([]))
            .unwrap();
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let info = storage::private_directory(&directory).unwrap();
        let target = directory.join(format!("{landing}.patch"));
        let payload =
            storage::create_private(&directory, Path::new(target.file_name().unwrap()), &body)
                .unwrap();
        storage::link(&target, &directory, Path::new("capture.anchor"), &payload).unwrap();
        let certificate = obj([
            ("reservation", item),
            ("directory", storage::identity(&info)),
            (
                "objects",
                obj([(
                    "capture",
                    obj([
                        ("identity", storage::identity(&payload)),
                        ("size", body.len().into()),
                        ("path", path_value(&target)),
                        ("anchor", path_value(&directory.join("capture.anchor"))),
                    ]),
                )]),
            ),
            ("terminal", V::Null),
        ]);
        storage::create_private(
            &directory,
            Path::new("owner.json"),
            &encoded(&certificate).unwrap(),
        )
        .unwrap();

        let reloaded = CaptureIndex::load(&repo).unwrap();
        assert_eq!(reloaded.indexed_bytes(&target).unwrap(), Some(body));
    }
}
