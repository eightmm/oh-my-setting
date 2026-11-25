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

fn prepare_capture_cache_root(
    cache_root: &Path,
    repo: &Path,
    execution: &Path,
    device: u64,
) -> Option<PathBuf> {
    let oms_dir = cache_root.parent()?;
    let mut existing = cache_root;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(meta) => {
                if (existing == cache_root || existing == oms_dir)
                    && (meta.file_type().is_symlink() || !meta.is_dir())
                {
                    return None;
                }
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(existing.file_name()?.to_owned());
                existing = existing.parent()?;
            }
            Err(_) => return None,
        }
    }
    if existing == cache_root {
        let oms_meta = fs::symlink_metadata(oms_dir).ok()?;
        if oms_meta.file_type().is_symlink() || !oms_meta.is_dir() {
            return None;
        }
    }
    let canonical_existing = fs::canonicalize(existing).ok()?;
    let existing_meta = fs::metadata(&canonical_existing).ok()?;
    if !existing_meta.is_dir() || existing_meta.dev() != device {
        return None;
    }
    let mut prospective = canonical_existing.clone();
    for name in missing.iter().rev() {
        prospective.push(name);
    }
    if prospective.starts_with(repo) || prospective.starts_with(execution) {
        return None;
    }
    let mut current = canonical_existing;
    for name in missing.into_iter().rev() {
        current.push(name);
        match DirBuilder::new().mode(0o700).create(&current) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
        let meta = fs::symlink_metadata(&current).ok()?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return None;
        }
    }
    let leaf_meta = fs::symlink_metadata(cache_root).ok()?;
    if leaf_meta.file_type().is_symlink() || !leaf_meta.is_dir() {
        return None;
    }
    let oms_meta = fs::symlink_metadata(oms_dir).ok()?;
    if oms_meta.file_type().is_symlink() || !oms_meta.is_dir() {
        return None;
    }
    let canonical = fs::canonicalize(&current).ok()?;
    let canonical_meta = fs::symlink_metadata(&canonical).ok()?;
    if !storage::same(&leaf_meta, &canonical_meta)
        || !fits_capture_parent(&canonical, repo, execution, device)
    {
        return None;
    }
    Some(canonical)
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
        if let Some(candidate) = prepare_capture_cache_root(cache_root, repo, execution, device) {
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
    use std::{
        io::ErrorKind,
        os::unix::fs::{PermissionsExt, symlink},
        process::Command,
    };

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

    fn setup_repo(root: &Path, name: &str) -> (PathBuf, PathBuf, String, Vec<u8>) {
        let projects = root.join("projects");
        fs::create_dir_all(&projects).unwrap();
        let repo = projects.join(name);
        fs::create_dir_all(repo.join(".oms")).unwrap();
        let body =
            b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n"
                .to_vec();
        let source = repo.join(".oms/source.patch");
        fs::write(&source, &body).unwrap();
        let expected = digest(&body);
        (repo, source, expected, body)
    }

    enum ChildOutcome {
        Ok(PathBuf),
        Err(i32, String),
    }

    #[allow(clippy::too_many_arguments)]
    fn run_allocate_child(
        root: &Path,
        repo: &Path,
        execution: &Path,
        source: &Path,
        landing: &str,
        expected: &str,
        tmpdir: &Path,
        xdg_cache_home: Option<&Path>,
        home: Option<&Path>,
    ) -> ChildOutcome {
        let locks = root.join("locks");
        fs::create_dir_all(&locks).unwrap();
        let out_file = root.join(format!("result-{landing}.txt"));
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "producer::tests::allocate_child_entry",
            "--test-threads=1",
        ])
        .env_remove("OMS_HARNESS_CHILD")
        .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("HOME")
        .env("TMPDIR", tmpdir)
        .env("OMS_LOCK_DIR", &locks)
        .env("OMS_TEST_ALLOCATE_OUT", &out_file)
        .env("OMS_TEST_REPO", repo)
        .env("OMS_TEST_EXECUTION", execution)
        .env("OMS_TEST_SOURCE", source)
        .env("OMS_TEST_LANDING", landing)
        .env("OMS_TEST_EXPECTED", expected);
        if let Some(xdg) = xdg_cache_home {
            cmd.env("XDG_CACHE_HOME", xdg);
        }
        if let Some(home) = home {
            cmd.env("HOME", home);
        }
        let output = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "child test failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let text = fs::read_to_string(&out_file).unwrap();
        if let Some(path) = text.strip_prefix("OK\n") {
            ChildOutcome::Ok(PathBuf::from(path))
        } else if let Some(rest) = text.strip_prefix("ERR:") {
            let (code, message) = rest.split_once('\n').unwrap();
            ChildOutcome::Err(code.parse().unwrap(), message.to_owned())
        } else {
            panic!("unexpected child output: {text}");
        }
    }

    #[test]
    fn allocate_child_entry() {
        let Some(out_file) = std::env::var_os("OMS_TEST_ALLOCATE_OUT") else {
            return;
        };
        let repo = PathBuf::from(std::env::var_os("OMS_TEST_REPO").unwrap());
        let execution = PathBuf::from(std::env::var_os("OMS_TEST_EXECUTION").unwrap());
        let source = PathBuf::from(std::env::var_os("OMS_TEST_SOURCE").unwrap());
        let landing = std::env::var("OMS_TEST_LANDING").unwrap();
        let expected = std::env::var("OMS_TEST_EXPECTED").unwrap();
        let interrupted = AtomicBool::new(false);
        let mut producer = Producer::open(&repo, &interrupted).unwrap();
        let payload =
            match producer.allocate(&source, &landing, &expected, &execution, &interrupted) {
                Ok(path) => format!("OK\n{}", path.display()),
                Err(err) => format!("ERR:{}\n{err}", err.exit_code()),
            };
        fs::write(out_file, payload).unwrap();
    }

    fn dir_entries(path: &Path) -> Vec<String> {
        let mut names = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn allocate_uses_cache_root_not_repo_parent_when_tmpdir_is_inside_repo() {
        let root = TestRoot::new("cache-fallback");
        let (repo, source, expected, body) = setup_repo(root.path(), "project");
        let projects = repo.parent().unwrap();
        let tmp_in_repo = repo.join("tmp");
        fs::create_dir(&tmp_in_repo).unwrap();

        let xdg_cache = root.path().join("xdg-cache");
        let landing = "land-20261125T000000Z-0000000000000001";
        let capture = match run_allocate_child(
            root.path(),
            &repo,
            &repo,
            &source,
            landing,
            &expected,
            &tmp_in_repo,
            Some(&xdg_cache),
            Some(&root.path().join("unused-home")),
        ) {
            ChildOutcome::Ok(path) => path,
            ChildOutcome::Err(code, msg) => panic!("expected Ok, got Err({code}): {msg}"),
        };

        let expected_cache_root = xdg_cache.join("oh-my-setting").join("landing-captures");
        let generation_dir = capture.parent().unwrap();
        assert_eq!(generation_dir.parent().unwrap(), expected_cache_root);
        let meta = fs::symlink_metadata(&expected_cache_root).unwrap();
        assert!(!meta.file_type().is_symlink() && meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        assert_eq!(dir_entries(projects), vec!["project"]);

        let index = CaptureIndex::load(&repo).unwrap();
        assert_eq!(index.indexed_bytes(&capture).unwrap(), Some(body.clone()));

        // When XDG_CACHE_HOME is unset, fall back to $HOME/.cache/oh-my-setting/landing-captures.
        let fake_home = root.path().join("user-home");
        fs::create_dir(&fake_home).unwrap();
        let landing_home = "land-20261125T000000Z-0000000000000002";
        let capture_home = match run_allocate_child(
            root.path(),
            &repo,
            &repo,
            &source,
            landing_home,
            &expected,
            &tmp_in_repo,
            None,
            Some(&fake_home),
        ) {
            ChildOutcome::Ok(path) => path,
            ChildOutcome::Err(code, msg) => panic!("expected Ok, got Err({code}): {msg}"),
        };

        let expected_home_cache = fake_home
            .join(".cache")
            .join("oh-my-setting")
            .join("landing-captures");
        assert_eq!(
            capture_home.parent().unwrap().parent().unwrap(),
            expected_home_cache
        );
        let home_meta = fs::symlink_metadata(&expected_home_cache).unwrap();
        assert!(!home_meta.file_type().is_symlink() && home_meta.is_dir());
        assert_eq!(home_meta.permissions().mode() & 0o777, 0o700);
        assert_eq!(dir_entries(projects), vec!["project"]);
    }

    #[test]
    fn allocate_errors_when_no_capture_parent_fits() {
        let root = TestRoot::new("no-parent");
        let (repo, source, expected, _body) = setup_repo(root.path(), "project");
        let projects = repo.parent().unwrap();
        let tmp_in_repo = repo.join("tmp");
        fs::create_dir(&tmp_in_repo).unwrap();
        let dummy_home = root.path().join("dummy-home");
        fs::create_dir(&dummy_home).unwrap();

        // Both TMPDIR and XDG_CACHE_HOME inside the repository.
        let cache_in_repo = repo.join("cache");
        let expected_cache_root = cache_in_repo.join("oh-my-setting").join("landing-captures");
        match run_allocate_child(
            root.path(),
            &repo,
            &repo,
            &source,
            "land-20261125T000000Z-0000000000000010",
            &expected,
            &tmp_in_repo,
            Some(&cache_in_repo),
            Some(&dummy_home),
        ) {
            ChildOutcome::Ok(path) => panic!("expected error, got capture at {}", path.display()),
            ChildOutcome::Err(code, msg) => {
                assert_eq!(code, 2);
                assert!(
                    msg.contains(
                        "no same-filesystem private capture parent outside state repository"
                    ),
                    "unexpected error: {msg}"
                );
                assert!(
                    msg.contains(&expected_cache_root.display().to_string()),
                    "error must name cache root {}: {msg}",
                    expected_cache_root.display()
                );
            }
        }
        assert!(!cache_in_repo.exists());
        assert_eq!(dir_entries(projects), vec!["project"]);

        // Cache root is a symlink: must be refused, naming the cache root tried.
        let symlink_xdg = root.path().join("symlink-cache");
        let oms_dir = symlink_xdg.join("oh-my-setting");
        fs::create_dir_all(&oms_dir).unwrap();
        let symlink_target = root.path().join("real-target");
        fs::create_dir(&symlink_target).unwrap();
        let symlink_cache_root = oms_dir.join("landing-captures");
        symlink(&symlink_target, &symlink_cache_root).unwrap();
        match run_allocate_child(
            root.path(),
            &repo,
            &repo,
            &source,
            "land-20261125T000000Z-0000000000000011",
            &expected,
            &tmp_in_repo,
            Some(&symlink_xdg),
            Some(&dummy_home),
        ) {
            ChildOutcome::Ok(path) => panic!("expected symlink refusal, got {}", path.display()),
            ChildOutcome::Err(code, msg) => {
                assert_eq!(code, 2);
                assert!(
                    msg.contains(&symlink_cache_root.display().to_string()),
                    "error must name symlink cache root {}: {msg}",
                    symlink_cache_root.display()
                );
            }
        }
        assert!(dir_entries(&symlink_target).is_empty());
        assert_eq!(dir_entries(projects), vec!["project"]);

        // Intermediate oh-my-setting directory is a symlink: must also be refused.
        let symlink_oms_xdg = root.path().join("symlink-oms");
        fs::create_dir(&symlink_oms_xdg).unwrap();
        symlink(&symlink_target, symlink_oms_xdg.join("oh-my-setting")).unwrap();
        let symlink_oms_cache_root = symlink_oms_xdg
            .join("oh-my-setting")
            .join("landing-captures");
        match run_allocate_child(
            root.path(),
            &repo,
            &repo,
            &source,
            "land-20261125T000000Z-0000000000000012",
            &expected,
            &tmp_in_repo,
            Some(&symlink_oms_xdg),
            Some(&dummy_home),
        ) {
            ChildOutcome::Ok(path) => panic!("expected symlink refusal, got {}", path.display()),
            ChildOutcome::Err(code, msg) => {
                assert_eq!(code, 2);
                assert!(
                    msg.contains(&symlink_oms_cache_root.display().to_string()),
                    "error must name cache root {}: {msg}",
                    symlink_oms_cache_root.display()
                );
            }
        }
        assert!(dir_entries(&symlink_target).is_empty());
        assert_eq!(dir_entries(projects), vec!["project"]);

        // Both TMPDIR and XDG_CACHE_HOME inside a separate execution repository.
        let execution = root.path().join("execution-repo");
        let tmp_in_exec = execution.join("tmp");
        let cache_in_exec = execution.join("cache");
        fs::create_dir_all(&tmp_in_exec).unwrap();
        let expected_exec_cache = cache_in_exec.join("oh-my-setting").join("landing-captures");
        match run_allocate_child(
            root.path(),
            &repo,
            &execution,
            &source,
            "land-20261125T000000Z-0000000000000013",
            &expected,
            &tmp_in_exec,
            Some(&cache_in_exec),
            Some(&dummy_home),
        ) {
            ChildOutcome::Ok(path) => panic!("expected error, got capture at {}", path.display()),
            ChildOutcome::Err(code, msg) => {
                assert_eq!(code, 2);
                assert!(
                    msg.contains(&expected_exec_cache.display().to_string()),
                    "error must name cache root {}: {msg}",
                    expected_exec_cache.display()
                );
            }
        }
        assert!(!cache_in_exec.exists());
        assert_eq!(dir_entries(projects), vec!["project"]);

        // Different filesystem or unavailable cache root fails closed and names cache root when tried.
        let device = fs::metadata(&repo).unwrap().dev();
        let other_cache = root.path().join("other-fs-cache");
        let other_cache_root = other_cache.join("oh-my-setting").join("landing-captures");
        let wrong_dev_err = select_capture_parent(
            &repo,
            &repo,
            device.wrapping_add(1),
            &tmp_in_repo,
            Some(&other_cache_root),
        )
        .unwrap_err();
        assert_eq!(wrong_dev_err.exit_code(), 2);
        assert!(
            wrong_dev_err
                .to_string()
                .contains(&other_cache_root.display().to_string()),
            "wrong-device error must name cache root: {wrong_dev_err}"
        );
        assert!(!other_cache.exists());

        let no_cache_err =
            select_capture_parent(&repo, &repo, device, &tmp_in_repo, None).unwrap_err();
        assert_eq!(no_cache_err.exit_code(), 2);
        assert!(
            no_cache_err
                .to_string()
                .contains("no same-filesystem private capture parent outside state repository"),
            "unexpected error: {no_cache_err}"
        );
    }

    #[test]
    fn existing_capture_remains_readable_wherever_recorded() {
        let root = TestRoot::new("legacy-read");
        let (repo, source, expected, body) = setup_repo(root.path(), "project");
        let legacy_parent = root.path().join("legacy-parent");
        let dummy_home = root.path().join("dummy-home");
        fs::create_dir(&legacy_parent).unwrap();
        fs::create_dir(&dummy_home).unwrap();
        let landing = "land-20261125T000000Z-0000000000000020";
        let capture = match run_allocate_child(
            root.path(),
            &repo,
            &repo,
            &source,
            landing,
            &expected,
            &legacy_parent,
            Some(&root.path().join("other-cache")),
            Some(&dummy_home),
        ) {
            ChildOutcome::Ok(path) => path,
            ChildOutcome::Err(code, msg) => panic!("expected Ok, got Err({code}): {msg}"),
        };
        assert_eq!(capture.parent().unwrap().parent().unwrap(), legacy_parent);
        let index = CaptureIndex::load(&repo).unwrap();
        assert_eq!(index.indexed_bytes(&capture).unwrap(), Some(body));
    }
}
