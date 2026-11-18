//! A local checkpoint cache, never a remote snapshot or delivery receipt.
use super::*;
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

pub(super) const DEFAULT_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const MIN_CHUNK_BYTES: u64 = 64 * 1024;
pub(super) const MAX_CHUNK_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn check_cancelled(interrupted: &AtomicBool) -> Result<()> {
    if interrupted.load(Ordering::Relaxed) {
        Err(status(
            130,
            "collection interrupted; verified chunks retained",
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct Options {
    pub(super) size: u64,
    pub(super) chunk_bytes: u64,
}
impl Options {
    fn chunks(self) -> u64 {
        self.size.div_ceil(self.chunk_bytes).max(1)
    }
    fn chunk_len(self, index: u64) -> u64 {
        self.size
            .saturating_sub(index * self.chunk_bytes)
            .min(self.chunk_bytes)
    }
}

fn lock_settings(cwd: &Path) -> Result<oms_lock::Settings> {
    let mut settings = oms_lock::Settings::from_env().map_err(error)?;
    settings.mode = oms_lock::Mode::Mkdir;
    settings.cwd = std::fs::canonicalize(cwd).map_err(error)?;
    Ok(settings)
}

pub(super) fn destination_lock(cwd: &Path, dest: &Path) -> Result<oms_lock::Guard> {
    let settings = lock_settings(cwd)?;
    let raw = settings
        .lock_dir
        .clone()
        .unwrap_or_else(|| settings.home.join(".cache/oh-my-setting/locks"));
    let bytes = raw.as_os_str().as_bytes();
    let root = if bytes == b"~" || bytes.starts_with(b"~/") {
        settings
            .home
            .join(OsStr::from_bytes(bytes.get(2..).unwrap_or(&[])))
    } else {
        settings.cwd.join(raw)
    };
    let key = sha256(dest.as_os_str().as_bytes());
    oms_lock::Lock::at_path(root.join(format!("ssh-collect-{key}.lock")), settings)
        .and_then(|lock| lock.try_acquire())
        .map_err(|e| status(e.exit_code(), format!("collection destination lock: {e}")))
}

fn private_dir(path: &Path) -> Result<()> {
    let created = match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(error(e)),
    };
    let meta = std::fs::symlink_metadata(path).map_err(error)?;
    require(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.uid() == rustix::process::geteuid().as_raw()
            && meta.mode() & 0o077 == 0,
        "collection cache must be an owned private non-symlink directory",
    )?;
    if created {
        sync_directory(&open_directory(path)?).map_err(error)?;
        sync_directory(&open_directory(path.parent().expect("cache parent"))?).map_err(error)?;
    }
    Ok(())
}

fn open_directory(path: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32)
        .open(path)
        .map_err(error)
}

fn sync_directory(file: &std::fs::File) -> std::result::Result<(), rustix::io::Errno> {
    match rustix::fs::fsync(file) {
        Ok(()) | Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOTSUP) => Ok(()),
        Err(e) => Err(e),
    }
}

struct Cache {
    dir: PathBuf,
    root: oms_safe_fs::Root,
}
impl Cache {
    fn private_file(&self, name: &str) -> Result<bool> {
        match std::fs::symlink_metadata(self.dir.join(name)) {
            Ok(meta) => {
                require(
                    meta.is_file()
                        && !meta.file_type().is_symlink()
                        && meta.nlink() == 1
                        && meta.uid() == rustix::process::geteuid().as_raw()
                        && meta.mode() & 0o077 == 0,
                    "collection checkpoint must be an owned private regular single-link file",
                )?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(error(e)),
        }
    }
    fn read(&self, name: &str, max_bytes: usize) -> Result<Option<Vec<u8>>> {
        if !self.private_file(name)? {
            return Ok(None);
        }
        let bytes = self
            .root
            .read_no_follow(
                Path::new(name),
                oms_safe_fs::ByteReadOptions {
                    missing_ok: false,
                    max_bytes,
                },
            )
            .map_err(|e| error(format!("collection checkpoint read: {e:?}")))?;
        require(
            self.private_file(name)?,
            "collection checkpoint disappeared",
        )?;
        Ok(Some(bytes))
    }
    fn write(&self, name: &str, bytes: &[u8], max_bytes: usize) -> Result<()> {
        self.private_file(name)?;
        self.root
            .atomic_mutate(
                Path::new(name),
                oms_safe_fs::MutationOptions {
                    read: oms_safe_fs::ByteReadOptions {
                        missing_ok: true,
                        max_bytes,
                    },
                    create_parent: false,
                    max_output: max_bytes,
                    preserve_existing: false,
                    expected_exists: None,
                },
                |_| Ok::<_, String>(bytes.to_vec()),
            )
            .map_err(|e| error(format!("collection checkpoint publication: {e:?}")))?;
        require(
            self.private_file(name)?,
            "collection checkpoint publication missing",
        )
    }
    fn chunk(&self, index: u64, options: Options, row: &serde_json::Value) -> Result<Vec<u8>> {
        let len = options.chunk_len(index);
        require(
            keys(row, &["bytes", "sha256"]) && row["bytes"].as_u64() == Some(len),
            "corrupt collection chunk metadata",
        )?;
        let digest = row["sha256"]
            .as_str()
            .ok_or_else(|| error("invalid chunk digest"))?;
        validate_sha256_hex(digest, "cached chunk SHA256")?;
        let bytes = self
            .read(&chunk_name(index), len as usize)?
            .ok_or_else(|| error("recorded collection chunk is missing"))?;
        require(
            bytes.len() as u64 == len && sha256(&bytes) == digest,
            "cached collection chunk size or digest mismatch",
        )?;
        Ok(bytes)
    }
}

fn keys(value: &serde_json::Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() == expected.len() && expected.iter().all(|key| map.contains_key(*key))
    })
}
fn chunk_name(index: u64) -> String {
    format!("{index:05}.chunk")
}

struct SshChild(std::process::Child);
impl Drop for SshChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>, limit: usize) -> Result<bool> {
    let mut buf = [0u8; 65536];
    loop {
        let bound = buf
            .len()
            .min(limit.saturating_sub(bytes.len()).saturating_add(1));
        match pipe.read(&mut buf[..bound]) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                require(
                    bytes.len() <= limit,
                    "collection chunk response exceeds byte bound",
                )?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(error(e)),
        }
    }
}

fn fetch(req: &CollectRequest<'_>, options: Options, index: u64) -> Result<Vec<u8>> {
    check_cancelled(req.interrupted)?;
    // A second block on the last request detects bytes beyond the promised EOF.
    // POSIX operands only: no GNU-only skip_bytes/fullblock/status extensions.
    let count = if index + 1 == options.chunks() { 2 } else { 1 };
    let script = format!(
        "{}exec dd if=\"$target\" bs={} skip={index} count={count}\n",
        collect_target_script(req.remote, req.remote_path_rel),
        options.chunk_bytes,
    );
    let mut child = SshChild(
        build_ssh(req.host, req.port, req.control)
            .arg(format!("sh -lc {}", quote(&script)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(error)?,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| error("missing ssh stdout"))?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| error("missing ssh stderr"))?;
    for fd in [
        &stdout as &dyn std::os::fd::AsFd,
        &stderr as &dyn std::os::fd::AsFd,
    ] {
        let flags = rustix::fs::fcntl_getfl(fd).map_err(error)?;
        rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK).map_err(error)?;
    }
    let started = Instant::now();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_done, mut err_done) = (false, false);
    let code = loop {
        check_cancelled(req.interrupted)?;
        require(
            started.elapsed() < CHUNK_TIMEOUT,
            "collection chunk timed out; verified chunks retained",
        )?;
        if !out_done {
            out_done = drain(&mut stdout, &mut out, options.chunk_len(index) as usize)?;
        }
        if !err_done {
            err_done = drain(&mut stderr, &mut err, MAX_STDERR_BYTES as usize)?;
        }
        if let Some(code) = child.0.try_wait().map_err(error)?
            && out_done
            && err_done
        {
            break code;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !code.success() {
        return Err(status(
            exit_code(code),
            format!(
                "remote collection chunk failed: {}",
                String::from_utf8_lossy(&err).trim(),
            ),
        ));
    }
    require(
        out.len() as u64 == options.chunk_len(index),
        "remote collection chunk has wrong exact size",
    )?;
    Ok(out)
}

pub(super) fn run(
    req: CollectRequest<'_>,
    options: Options,
    parent: &Path,
    dest: &Path,
    destination_lock: &oms_lock::Guard,
) -> Result<Response> {
    check_cancelled(req.interrupted)?;
    let expected = req.expected_sha256.expect("validated resumable digest");
    let identity = serde_json::json!({
        "host": req.host, "port": req.port.unwrap_or(22),
        "remote_dir": req.remote, "remote_path": req.remote_path_rel,
        "dest": dest.to_str().ok_or_else(|| error("collection destination must be UTF-8"))?,
        "expected_sha256": expected, "expected_size": options.size,
        "chunk_bytes": options.chunk_bytes,
    });
    let transfer_id = sha256(&serde_json::to_vec(&identity).map_err(error)?);
    let cache_parent = parent.join(".oms-collect-cache");
    private_dir(&cache_parent)?;
    let dir = cache_parent.join(&transfer_id);
    private_dir(&dir)?;
    let transfer_lock = oms_lock::Lock::in_root(
        dir.clone(),
        Path::new("transfer.lock"),
        lock_settings(req.cwd)?,
    )
    .and_then(|lock| lock.try_acquire())
    .map_err(|e| status(e.exit_code(), format!("collection transfer lock: {e}")))?;
    let cache = Cache {
        root: oms_safe_fs::Root::open(&dir)
            .map_err(|e| error(format!("collection cache: {e:?}")))?,
        dir,
    };
    let mut manifest = match cache.read("manifest.json", MAX_MANIFEST_BYTES)? {
        Some(bytes) => {
            let row: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
            require(
                keys(
                    &row,
                    &["schema", "kind", "transfer_id", "identity", "chunks"],
                ) && row["schema"] == 1
                    && row["kind"] == "oms-ssh-collect-checkpoint"
                    && row["transfer_id"] == transfer_id
                    && row["identity"] == identity
                    && row["chunks"]
                        .as_object()
                        .is_some_and(|map| map.len() as u64 <= options.chunks()),
                "corrupt or mismatched collection manifest; cache is not a delivery receipt",
            )?;
            row
        }
        None => {
            let row = serde_json::json!({
                "schema": 1, "kind": "oms-ssh-collect-checkpoint",
                "transfer_id": transfer_id, "identity": identity, "chunks": {},
            });
            cache.write(
                "manifest.json",
                &serde_json::to_vec(&row).map_err(error)?,
                MAX_MANIFEST_BYTES,
            )?;
            row
        }
    };
    let (mut reused_chunks, mut reused_bytes) = (0u64, 0u64);
    for (key, row) in manifest["chunks"].as_object().expect("validated chunks") {
        let index = key
            .parse::<u64>()
            .map_err(|_| error("invalid cached chunk index"))?;
        require(
            index < options.chunks() && key == &index.to_string(),
            "invalid cached chunk index",
        )?;
        reused_bytes += cache.chunk(index, options, row)?.len() as u64;
        reused_chunks += 1;
    }
    let mut fetched_bytes = 0u64;
    for index in 0..options.chunks() {
        if manifest["chunks"].get(index.to_string()).is_some() {
            continue;
        }
        destination_lock.check().map_err(error)?;
        transfer_lock.check().map_err(error)?;
        let bytes = fetch(&req, options, index)?;
        // The external read may outlast this lock's custody. Revalidate before
        // changing the checkpoint, even when final delivery will later refuse.
        destination_lock.check().map_err(error)?;
        transfer_lock.check().map_err(error)?;
        check_cancelled(req.interrupted)?;
        let row = serde_json::json!({"bytes": bytes.len(), "sha256": sha256(&bytes)});
        cache.write(
            &chunk_name(index),
            &bytes,
            options.chunk_len(index) as usize,
        )?;
        manifest["chunks"][index.to_string()] = row;
        destination_lock.check().map_err(error)?;
        transfer_lock.check().map_err(error)?;
        cache.write(
            "manifest.json",
            &serde_json::to_vec(&manifest).map_err(error)?,
            MAX_MANIFEST_BYTES,
        )?;
        fetched_bytes += bytes.len() as u64;
    }
    let directory = open_directory(parent)?;
    let parent_meta = directory.metadata().map_err(error)?;
    let (mut temporary, mut guard) = create_private_temp(parent)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    for index in 0..options.chunks() {
        check_cancelled(req.interrupted)?;
        let bytes = cache.chunk(index, options, &manifest["chunks"][index.to_string()])?;
        temporary.write_all(&bytes).map_err(error)?;
        hasher.update(&bytes);
        total += bytes.len() as u64;
    }
    let digest = hasher.finish();
    require(
        total == options.size && digest == expected,
        "collected full size or SHA256 mismatch; destination unchanged",
    )?;
    temporary.sync_all().map_err(error)?;
    destination_lock.check().map_err(error)?;
    transfer_lock.check().map_err(error)?;
    check_cancelled(req.interrupted)?;
    let named_parent = std::fs::symlink_metadata(parent).map_err(error)?;
    require(
        named_parent.is_dir()
            && !named_parent.file_type().is_symlink()
            && named_parent.dev() == parent_meta.dev()
            && named_parent.ino() == parent_meta.ino(),
        "collection destination parent changed",
    )?;
    match std::fs::symlink_metadata(dest) {
        Ok(meta) => require(
            meta.is_file() && !meta.file_type().is_symlink(),
            "destination became a symlink or directory",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(e)),
    }
    let name = guard.path.file_name().expect("private temporary name");
    let named = std::fs::File::from(
        rustix::fs::openat(
            &directory,
            name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(error)?,
    )
    .metadata()
    .map_err(error)?;
    let temporary_meta = temporary.metadata().map_err(error)?;
    require(
        named.dev() == temporary_meta.dev() && named.ino() == temporary_meta.ino(),
        "collection temporary file changed",
    )?;
    rustix::fs::renameat(
        &directory,
        name,
        &directory,
        dest.file_name().expect("destination name"),
    )
    .map_err(error)?;
    guard.keep = true;
    sync_directory(&directory).map_err(|e| {
        error(format!(
            "collection published but directory durability is unknown: {e}"
        ))
    })?;
    json_response(
        0,
        &serde_json::json!({
            "schema": 1, "action": "collect", "host": req.host,
            "remote_dir": req.remote, "remote_path": req.remote_path_rel,
            "dest": dest.to_string_lossy(), "bytes": total, "sha256": digest, "verified": true,
            "resumable": true, "transfer_id": transfer_id, "chunk_bytes": options.chunk_bytes,
            "chunks_reused": reused_chunks, "bytes_reused": reused_bytes,
            "chunks_fetched": options.chunks() - reused_chunks, "bytes_fetched": fetched_bytes,
            "cache_dir": cache.dir.to_string_lossy(),
        }),
        req.pretty,
    )
}
