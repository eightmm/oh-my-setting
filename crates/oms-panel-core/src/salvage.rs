//! Stopped worktree capture with isolated Git metadata and immutable provenance.
use crate::services::ExecutionRequest;
use crate::{Error, Result, Services, Store, Value, require};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
pub const MAX_EVIDENCE: usize = 8 * 1024 * 1024;
pub const PREFIX: &str = "# oms-salvage: ";
// Bound directory traversal independently from memory retained for one task.
const INVENTORY_RAW_LIMIT: usize = 100_000;
const INVENTORY_RELEVANT_LIMIT: usize = 10_000;
// Bound actual sidecar and patch reads, including oversized-file probes.
const SIDECAR_LIMIT: usize = 1_024;
const SIDECAR_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy)]
struct Limits {
    raw: usize,
    relevant: usize,
    sidecars: usize,
    bytes: usize,
}
const LIMITS: Limits = Limits {
    raw: INVENTORY_RAW_LIMIT,
    relevant: INVENTORY_RELEVANT_LIMIT,
    sidecars: SIDECAR_LIMIT,
    bytes: SIDECAR_BYTES,
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("oms-panel-salvage-{}", crate::value::unique()?));
        std::fs::create_dir(&path).map_err(Error::new)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(Error::new)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn new_file(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(Error::new)?;
    file.write_all(data).map_err(Error::new)
}
fn git_directory(repo: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(repo.join(".git")).map_err(Error::new)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return std::fs::canonicalize(repo.join(".git")).map_err(Error::new);
    }
    let data = Store::open(repo)?.read(Path::new(".git"), 4096, false)?;
    let text = std::str::from_utf8(&data).map_err(Error::new)?.trim();
    let directory = text
        .strip_prefix("gitdir: ")
        .ok_or_else(|| Error::new("invalid worktree gitfile"))?;
    std::fs::canonicalize(repo.join(directory)).map_err(Error::new)
}
fn common(directory: &Path) -> Result<PathBuf> {
    let data = Store::open(directory)?.read(Path::new("commondir"), 4096, true)?;
    let value = std::str::from_utf8(&data).map_err(Error::new)?.trim();
    if value.is_empty() {
        Ok(directory.into())
    } else {
        std::fs::canonicalize(directory.join(value)).map_err(Error::new)
    }
}
/// Validates the managed marker and Git registration without opening worker config.
pub fn managed(repo: &Path, kept: &Path) -> Result<(PathBuf, PathBuf, String)> {
    let physical = std::fs::canonicalize(kept).map_err(Error::new)?;
    require(
        kept == physical,
        "kept worktree path must use its physical spelling",
    )?;
    let residue = oms_reference::repository::harness_residue_parent(&physical)
        .ok_or_else(|| Error::new("not an OMS-managed delegate worktree"))?;
    let root = std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".cache")))
                .map(|v| v.join("oh-my-setting/worktrees"))
        })
        .ok_or_else(|| Error::new("managed worktree root unavailable"))?;
    let root = std::fs::canonicalize(root).map_err(Error::new)?;
    require(
        residue.parent() == Some(root.as_path())
            && residue
                .file_name()
                .is_some_and(|v| v.to_string_lossy().starts_with("oh-my-setting-delegate.")),
        "kept path is outside managed delegate root",
    )?;
    let marker = Store::open(
        physical
            .parent()
            .ok_or_else(|| Error::new("worktree parent unavailable"))?,
    )?
    .read(Path::new(".oh-my-setting-tmp"), 8192, false)?;
    let marker = std::str::from_utf8(&marker).map_err(Error::new)?;
    let mut fields = BTreeMap::new();
    for line in marker.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| Error::new("invalid kept worktree marker"))?;
        require(
            fields.insert(key, value).is_none(),
            "duplicate kept worktree marker field",
        )?;
    }
    let marker_repo = fields
        .get("repo")
        .ok_or_else(|| Error::new("marker repository missing"))?;
    let marker_worktree = fields
        .get("worktree")
        .ok_or_else(|| Error::new("marker worktree missing"))?;
    require(
        fields.get("kind") == Some(&"oh-my-setting-temp")
            && fields.get("temporary") == Some(&"1")
            && std::fs::canonicalize(marker_worktree).map_err(Error::new)? == physical,
        "invalid managed worktree marker",
    )?;
    let expected_common = common(&git_directory(repo)?)?;
    require(
        common(&git_directory(Path::new(marker_repo))?)? == expected_common,
        "managed worktree marker belongs to another repository",
    )?;
    let directory = git_directory(&physical)?;
    require(
        common(&directory)? == expected_common
            && directory.parent() == Some(expected_common.join("worktrees").as_path()),
        "kept worktree registration does not match repository",
    )?;
    let gitdir = Store::open(&directory)?.read(Path::new("gitdir"), 4096, false)?;
    let backlink = std::str::from_utf8(&gitdir).map_err(Error::new)?.trim();
    require(
        std::fs::canonicalize(backlink).map_err(Error::new)? == physical.join(".git"),
        "kept worktree registration backlink changed",
    )?;
    let head = Store::open(&directory)?.read(Path::new("HEAD"), 4096, false)?;
    let head = std::str::from_utf8(&head)
        .map_err(Error::new)?
        .trim()
        .to_owned();
    require(
        crate::hex(&head, 40) || crate::hex(&head, 64),
        "kept worktree must have detached HEAD",
    )?;
    Ok((physical, expected_common, head))
}
struct Git<'a, S: Services + ?Sized> {
    scratch: &'a Path,
    kept: &'a Path,
    common: &'a Path,
    service: &'a S,
    stop: &'a AtomicBool,
}
impl<S: Services + ?Sized> Git<'_, S> {
    fn run(&self, args: &[OsString], data: Option<Vec<u8>>) -> Result<Vec<u8>> {
        let mut argv: Vec<OsString> = vec![
            "git".into(),
            format!("--git-dir={}", self.scratch.join("git").display()).into(),
            format!("--work-tree={}", self.kept.display()).into(),
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
            "-c".into(),
            "core.fsmonitor=".into(),
            "-c".into(),
            "diff.external=".into(),
            "-c".into(),
            "core.attributesFile=/dev/null".into(),
        ];
        argv.extend_from_slice(args);
        let mut request = ExecutionRequest::new(argv, self.scratch);
        request.timeout = Duration::from_secs(60);
        request.output_limit = 256 * 1024 * 1024;
        request.stdin = data;
        request.remove_environment = std::env::vars_os()
            .filter(|(k, _)| k.to_string_lossy().starts_with("GIT_"))
            .map(|(k, _)| k)
            .collect();
        request.environment = vec![
            ("HOME".into(), self.scratch.into()),
            ("XDG_CONFIG_HOME".into(), self.scratch.into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
            ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
            (
                "GIT_ALTERNATE_OBJECT_DIRECTORIES".into(),
                self.common.join("objects").into_os_string(),
            ),
            ("GIT_NO_LAZY_FETCH".into(), "1".into()),
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
        ];
        let result = self.service.execute(&request, self.stop)?;
        result.success()?;
        Ok(result.stdout)
    }
}
#[derive(Clone, Debug)]
pub struct Capture {
    pub patch: Vec<u8>,
    pub files: Vec<Value>,
    pub encoding_warnings: Vec<Value>,
    pub base: String,
}
pub fn capture<S: Services + ?Sized>(
    kept: &Path,
    common: &Path,
    base: &str,
    head: &str,
    service: &S,
    stop: &AtomicBool,
) -> Result<Capture> {
    require(
        (crate::hex(base, 40) || crate::hex(base, 64))
            && (crate::hex(head, 40) || crate::hex(head, 64)),
        "salvage base/head must be full commit SHAs",
    )?;
    let scratch = Scratch::new()?;
    for path in ["git", "git/objects", "git/refs", "git/info"] {
        std::fs::create_dir(scratch.0.join(path)).map_err(Error::new)?;
    }
    new_file(&scratch.0.join("git/HEAD"), format!("{base}\n").as_bytes())?;
    let config = if base.len() == 64 {
        "[core]\nrepositoryformatversion = 1\nbare = false\n[extensions]\nobjectFormat = sha256\n"
    } else {
        "[core]\nrepositoryformatversion = 0\nbare = false\n"
    };
    new_file(&scratch.0.join("git/config"), config.as_bytes())?;
    let exclude = Store::open(common)?.read(Path::new("info/exclude"), MAX_EVIDENCE, true)?;
    new_file(&scratch.0.join("git/info/exclude"), &exclude)?;
    let git = Git {
        scratch: &scratch.0,
        kept,
        common,
        service,
        stop,
    };
    git.run(&crate::services::args(&["read-tree", base]), None)?;
    let mut entries = BTreeMap::new();
    for entry in git
        .run(&crate::services::args(&["ls-files", "--stage", "-z"]), None)?
        .split(|&v| v == 0)
        .filter(|v| !v.is_empty())
    {
        let (header, path) = entry.split_at(
            entry
                .iter()
                .position(|&v| v == b'\t')
                .ok_or_else(|| Error::new("invalid Git index listing"))?,
        );
        let header: Vec<_> = std::str::from_utf8(header)
            .map_err(Error::new)?
            .split_whitespace()
            .collect();
        require(
            header.len() == 3 && header[0] != "160000" && header[2] == "0",
            "submodule or unmerged worktrees require owner inspection",
        )?;
        let path = std::str::from_utf8(&path[1..])
            .map_err(|_| Error::new("non-UTF-8 salvage paths require lossless owner inspection"))?;
        crate::storage::path(Path::new(path))?;
        entries.insert(
            path.to_owned(),
            (header[0].to_owned(), header[1].to_owned()),
        );
    }
    let mut names: std::collections::BTreeSet<_> = entries.keys().cloned().collect();
    let others = git.run(
        &crate::services::args(&["ls-files", "--others", "--exclude-standard", "-z"]),
        None,
    )?;
    for name in others.split(|&v| v == 0).filter(|v| !v.is_empty()) {
        let name = std::str::from_utf8(name).map_err(Error::new)?;
        if name.starts_with(".oms-room-relay.")
            && name.split('/').next().is_some_and(|v| v.len() == 22)
        {
            continue;
        }
        names.insert(name.to_owned());
    }
    if head != base {
        let tree = git.run(&crate::services::args(&["ls-tree", "-r", "-z", head]), None)?;
        for entry in tree.split(|&v| v == 0).filter(|v| !v.is_empty()) {
            let tab = entry
                .iter()
                .position(|&v| v == b'\t')
                .ok_or_else(|| Error::new("invalid committed tree listing"))?;
            require(
                !entry.starts_with(b"160000 "),
                "submodule worktree requires owner inspection",
            )?;
            names.insert(
                std::str::from_utf8(&entry[tab + 1..])
                    .map_err(Error::new)?
                    .to_owned(),
            );
        }
    }
    let root = Store::open(kept)?;
    let mut files = vec![];
    let mut warnings = vec![];
    for name in names {
        crate::checkpoint(stop)?;
        crate::storage::path(Path::new(&name))?;
        let path = kept.join(&name);
        require(
            path.parent()
                .is_some_and(|p| std::fs::canonicalize(p).is_ok_and(|v| v == p)),
            "salvage path has a symlinked or missing parent",
        )?;
        let before = if let Some((_, oid)) = entries.get(&name) {
            git.run(&crate::services::args(&["cat-file", "blob", oid]), None)?
        } else {
            vec![]
        };
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(v) => Some(v),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::new(e)),
        };
        let (after, mode) = match metadata {
            None => (vec![], None),
            Some(info) if info.is_dir() => (vec![], None),
            Some(info) if info.file_type().is_symlink() => (
                std::fs::read_link(&path)
                    .map_err(Error::new)?
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
                Some("120000"),
            ),
            Some(info) if info.is_file() => (
                root.read(Path::new(&name), 256 * 1024 * 1024, false)?,
                Some(if info.mode() & 0o111 != 0 {
                    "100755"
                } else {
                    "100644"
                }),
            ),
            _ => return Err(Error::new("salvage refuses special file")),
        };
        if entries
            .get(&name)
            .is_some_and(|(old, _)| mode == Some(old.as_str()) && before == after)
            || mode.is_none() && !entries.contains_key(&name)
        {
            continue;
        }
        if let Some(mode) = mode {
            let oid = git.run(
                &crate::services::args(&["hash-object", "-w", "--stdin", "--no-filters"]),
                Some(after.clone()),
            )?;
            let oid = std::str::from_utf8(&oid).map_err(Error::new)?.trim();
            git.run(
                &crate::services::args(&["update-index", "--add", "--cacheinfo", mode, oid, &name]),
                None,
            )?;
        } else {
            git.run(
                &crate::services::args(&["update-index", "--force-remove", "--", &name]),
                None,
            )?;
        }
        warnings.extend(encoding_warnings(&name, &before, &after));
        files.push(
            serde_json::json!({"path":name,"before_bytes":before.len(),"after_bytes":after.len()}),
        );
    }
    let patch = git.run(
        &crate::services::args(&[
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            base,
        ]),
        None,
    )?;
    require(
        patch.len() <= crate::results::PATCH_MAX,
        "salvage patch exceeds byte limit",
    )?;
    warnings.extend(service.patch_encoding_warnings(&patch)?);
    Ok(Capture {
        patch,
        files,
        encoding_warnings: warnings,
        base: base.into(),
    })
}
pub fn encoding_warnings(path: &str, before: &[u8], after: &[u8]) -> Vec<Value> {
    let mut warnings = vec![];
    if std::str::from_utf8(before).is_ok() && std::str::from_utf8(after).is_err() {
        warnings.push(serde_json::json!({"path":path,"kind":"utf8-loss","review_required":true}));
    }
    if after.windows(3).filter(|v| *v == b"\xef\xbf\xbd").count()
        > before.windows(3).filter(|v| *v == b"\xef\xbf\xbd").count()
    {
        warnings.push(
            serde_json::json!({"path":path,"kind":"replacement-character","review_required":true}),
        );
    }
    warnings
}
pub fn payload(body: &[u8]) -> Vec<u8> {
    let start = body
        .windows(11)
        .enumerate()
        .find(|(i, v)| *v == b"diff --git " && (*i == 0 || body[*i - 1] == b'\n'))
        .map(|v| v.0)
        .unwrap_or(0);
    let mut value = body[start..].to_vec();
    while value.last() == Some(&b'\n') {
        value.pop();
    }
    value.push(b'\n');
    value
}
pub fn record_provenance(store: &Store, metadata: &Value) -> Result<String> {
    crate::parent_only()?;
    let sha = metadata["payload_sha256"]
        .as_str()
        .ok_or_else(|| Error::new("salvage payload digest missing"))?;
    require(crate::hex(sha, 64), "invalid salvage payload digest")?;
    let path = format!(".oms/salvage-provenance/{sha}.json");
    let tx = store.transaction("panel-salvage", Duration::from_secs(10))?;
    let record = serde_json::json!({"salvaged_from":metadata["salvaged_from"],"encoding_warnings":metadata["encoding_warnings"],"base_sha":metadata["base_sha"]});
    tx.mutate(Path::new(&path), MAX_EVIDENCE, |old| {
        let mut value = if old.is_empty() {
            serde_json::json!({"schema":1,"payload_sha256":sha,"records":[]})
        } else {
            crate::decode(old, MAX_EVIDENCE)?
        };
        require(
            value["schema"] == 1 && value["payload_sha256"] == sha && value["records"].is_array(),
            "invalid controller salvage provenance",
        )?;
        let records = value["records"].as_array_mut().unwrap();
        if !records.contains(&record) {
            records.push(record);
        }
        crate::value::encoded(&value)
    })?;
    Ok(path)
}
#[derive(Clone, Debug)]
pub struct SalvageRequest {
    pub task_id: String,
    pub attempt_id: String,
    pub member: String,
    pub kept: PathBuf,
    pub execution_repo: PathBuf,
    pub base: String,
}
/// Requires the dispatch's private supervisor proof. Repository artifact rows alone
/// never prove that a process tree has stopped, or authorize salvage publication.
pub fn salvage(
    repo: &Path,
    request: &SalvageRequest,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    crate::identifier(&request.task_id)?;
    crate::identifier(&request.attempt_id)?;
    crate::identifier(&request.member)?;
    require(
        crate::hex(&request.base, 40) || crate::hex(&request.base, 64),
        "salvage base must be a full commit SHA",
    )?;
    let attempts = crate::services::query(
        service,
        crate::NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let worker = crate::value::array(&attempts)
        .iter()
        .find(|v| {
            v["attempt_id"] == request.attempt_id
                && v["task_id"] == request.task_id
                && v["refs"]["panel_room_participant"] == request.member
        })
        .ok_or_else(|| Error::new("salvage attempt binding unavailable"))?;
    crate::control::stopped(repo, worker, crate::value::array(&attempts))?;
    let anchor = crate::control::private_record(repo, &request.member, "launch")?;
    let execution = std::fs::canonicalize(&request.execution_repo).map_err(Error::new)?;
    require(
        anchor["execution_repo"]
            .as_str()
            .is_some_and(|v| std::fs::canonicalize(v).ok().as_ref() == Some(&execution)),
        "salvage execution repository differs from private launch",
    )?;
    let rows = crate::results::index_rows(repo)?;
    let indexed = rows.iter().rev().find(|v| {
        v["kind"] == "delegate"
            && v["task_id"] == request.task_id
            && v["attempt_id"] == request.attempt_id
    });
    require(
        recorded_kept(repo, worker, indexed)? == request.kept,
        "requested kept worktree differs from recorded delegate evidence",
    )?;
    let recorded_base = indexed.and_then(|v| v["base_sha"].as_str());
    if let Some(base) = recorded_base {
        require(
            (7..=64).contains(&base.len()) && crate::hex(base, base.len()),
            "invalid recorded salvage base",
        )?;
        require(
            crate::git::text(
                service,
                &request.execution_repo,
                &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
                stop,
            )? == request.base,
            "salvage base differs from delegate launch base",
        )?;
    }
    let missing = match std::fs::symlink_metadata(&request.kept) {
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(Error::new(e)),
    };
    let captured = if missing {
        let location = crate::value::text(&worker["refs"], "panel_location");
        let relative = location
            .strip_prefix("worktree:")
            .ok_or_else(|| Error::new("missing worktree has no delegate location"))?;
        crate::storage::path(Path::new(relative))?;
        require(
            relative.starts_with("oh-my-setting-delegate."),
            "missing worktree is not a recorded delegate",
        )?;
        let root = std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .map(|v| v.join("oh-my-setting/worktrees"))
            })
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|v| v.join(".cache/oh-my-setting/worktrees"))
            })
            .ok_or_else(|| Error::new("delegate root unavailable"))?;
        require(
            std::fs::canonicalize(root)
                .map_err(Error::new)?
                .join(relative)
                == request.kept,
            "missing worktree differs from recorded delegate location",
        )?;
        let body = if let Some(indexed) = indexed.filter(|v| {
            v.get("patch").is_some()
                || v.get("patch_sha256").is_some()
                || v.get("patch_external").is_some()
        }) {
            require(
                recorded_base.is_some(),
                "missing delegate has no recorded launch base",
            )?;
            crate::results::verified_patch(&Store::open(repo)?, indexed)?
        } else {
            vec![]
        };
        let paths = crate::files::patch_paths(&body)?;
        require(
            body.is_empty() || !paths.is_empty(),
            "indexed recovery patch has no file changes",
        )?;
        let warnings = service.patch_encoding_warnings(&body)?;
        Capture {
            patch: body,
            files: paths
                .into_iter()
                .map(|v| serde_json::json!({"path":v,"before_bytes":null,"after_bytes":null}))
                .collect(),
            encoding_warnings: warnings,
            base: request.base.clone(),
        }
    } else {
        let identity = managed(&request.execution_repo, &request.kept)?;
        require(
            recorded_base.is_some() || request.base == identity.2,
            "unindexed salvage must capture against the kept HEAD",
        )?;
        let captured = capture(
            &identity.0,
            &identity.1,
            &request.base,
            &identity.2,
            service,
            stop,
        )?;
        require(
            managed(&request.execution_repo, &request.kept)? == identity,
            "kept worktree identity moved during capture",
        )?;
        require(
            !captured.patch.is_empty() || recorded_base.is_some(),
            "empty kept worktree has no recorded launch base",
        )?;
        captured
    };
    let store = Store::open(repo)?;
    if captured.patch.is_empty() {
        require(
            rows.iter()
                .filter(|v| {
                    v["task_id"] == request.task_id
                        && v["attempt_id"] == request.attempt_id
                        && v["patch"].is_string()
                })
                .all(|v| crate::results::verified_patch(&store, v).is_ok_and(|v| v.is_empty())),
            "indexed recovery evidence prevents empty salvage",
        )?;
        registered_empty(request, service, stop)?;
        require(
            continuation_patch(&store, &request.task_id, &request.attempt_id)?.is_none(),
            "published salvage prevents an empty recovery receipt",
        )?;
        no_unindexed_recovery(&store, request, worker, &rows)?;
        let sha = empty_identity(&request.task_id, &request.attempt_id)?;
        let relative = format!(".oms/salvage-provenance/empty-{sha}.json");
        let proof = serde_json::json!({"schema":1,"payload_sha256":sha,"records":[{"salvaged_from":{"task":request.task_id,"attempt":request.attempt_id,"outcome":"nothing_to_recover","reason":"stopped delegate recovery inventory was empty"},"base_sha":captured.base,"encoding_warnings":[]}]});
        store
            .transaction("panel-salvage", Duration::from_secs(10))?
            .write_json(Path::new(&relative), &proof, true, MAX_EVIDENCE)?;
        return Ok(
            serde_json::json!({"outcome":"nothing_to_recover","task":request.task_id,"attempt":request.attempt_id,"provenance":relative}),
        );
    }
    let mut metadata = serde_json::json!({"schema":1,"kind":"oms-salvaged-patch","base_sha":captured.base,"salvaged_at":salvaged_at()?,
        "salvaged_from":{"task":request.task_id,"attempt":request.attempt_id,"kept_path":request.kept,"stop_reason":"confirmed-stopped"},
        "files":captured.files,"encoding_warnings":captured.encoding_warnings,"payload_sha256":crate::digest(&payload(&captured.patch))});
    if missing {
        metadata["salvaged_from"]["recovered_from_artifact"] = true.into();
        metadata["recovered_artifact"] = indexed.unwrap()["patch"].clone();
    }
    metadata["provenance"] = record_provenance(&store, &metadata)?.into();
    let mut body = PREFIX.as_bytes().to_vec();
    body.extend(crate::value::encoded(&metadata)?);
    body.extend(&captured.patch);
    let path = format!(
        ".oms/artifacts/delegate/salvage-{}-{}.patch",
        request.task_id,
        &crate::value::unique()?[..16]
    );
    let tx = store.transaction("panel-salvage", Duration::from_secs(10))?;
    tx.write(Path::new(&path), &body, true, crate::results::PATCH_MAX)?;
    metadata["patch"] = path.clone().into();
    metadata["patch_sha256"] = crate::digest(&body).into();
    tx.write_json(
        Path::new(&format!("{path}.json")),
        &metadata,
        true,
        MAX_EVIDENCE,
    )
    .map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })?;
    drop(tx);
    let mut registration = oms_command_telemetry::store::AppendArtifactRequest {
        kind: "salvage".into(),
        provider: "panel".into(),
        artifact: Some(PathBuf::from(format!("{path}.json"))),
        patch: Some(PathBuf::from(&path)),
        task_id: Some(request.task_id.clone()),
        inherit_env: false,
        ..Default::default()
    };
    registration
        .env_overrides
        .insert("OMS_INDEX_ATTEMPT_ID".into(), request.attempt_id.clone());
    registration
        .env_overrides
        .insert("OMS_INDEX_BASE_SHA".into(), captured.base);
    oms_command_telemetry::store::append_artifact_index(repo, &registration, stop).map_err(
        |e| Error {
            publication_uncertain: true,
            ..crate::services::telemetry_error(e)
        },
    )?;
    Ok(metadata)
}

fn empty_identity(task: &str, attempt: &str) -> Result<String> {
    Ok(crate::digest(
        &serde_json::to_vec(&[task, attempt]).map_err(Error::new)?,
    ))
}

pub fn recorded_kept(repo: &Path, worker: &Value, indexed: Option<&Value>) -> Result<PathBuf> {
    let mut paths = std::collections::BTreeSet::new();
    let location = crate::value::text(&worker["refs"], "panel_location");
    if let Some(relative) = location.strip_prefix("worktree:") {
        crate::storage::path(Path::new(relative))?;
        require(
            relative.starts_with("oh-my-setting-delegate."),
            "unmanaged delegate location",
        )?;
        let root = std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .map(|v| v.join("oh-my-setting/worktrees"))
            })
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|v| v.join(".cache/oh-my-setting/worktrees"))
            })
            .ok_or_else(|| Error::new("delegate root unavailable"))?;
        paths.insert(
            std::fs::canonicalize(root)
                .map_err(Error::new)?
                .join(relative),
        );
    }
    if let Some(row) = indexed.filter(|v| v["artifact"].is_string()) {
        let body = crate::results::indexed_artifact(&Store::open(repo)?, row)?;
        let body = std::str::from_utf8(&body).map_err(Error::new)?;
        for line in body.lines() {
            if let Some(path) = [
                "worktree kept at ",
                "worktree kept: ",
                "error: worktree kept at ",
                "error: worktree kept: ",
                "- worktree: ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            {
                let path = PathBuf::from(path);
                require(
                    path.is_absolute()
                        && !path
                            .components()
                            .any(|v| matches!(v, std::path::Component::ParentDir)),
                    "invalid recorded kept worktree path",
                )?;
                paths.insert(path);
            }
        }
    }
    require(
        paths.len() == 1,
        "kept worktree evidence is missing or conflicting",
    )?;
    Ok(paths.into_iter().next().unwrap())
}

pub(crate) fn verified_empty<S: Services + ?Sized>(
    repo: &Path,
    worker: &Value,
    attempts: &[Value],
    service: &S,
    stop: &AtomicBool,
) -> Result<bool> {
    let task = crate::value::text(worker, "task_id");
    let attempt = crate::value::text(worker, "attempt_id");
    let store = Store::open(repo)?;
    if !empty_provenance(&store, &task, &attempt)? {
        return Ok(false);
    }
    crate::control::stopped(repo, worker, attempts)?;
    let member = crate::value::text(&worker["refs"], "panel_room_participant");
    let anchor = crate::control::private_record(repo, &member, "launch")?;
    let execution = PathBuf::from(
        anchor["execution_repo"]
            .as_str()
            .ok_or_else(|| Error::new("empty salvage execution unavailable"))?,
    );
    let rows = crate::results::index_rows(repo)?;
    let indexed = rows
        .iter()
        .rev()
        .find(|v| v["kind"] == "delegate" && v["task_id"] == task && v["attempt_id"] == attempt);
    let kept = recorded_kept(repo, worker, indexed)?;
    let base = if let Some(base) = indexed.and_then(|v| v["base_sha"].as_str()) {
        require(
            (7..=64).contains(&base.len()) && crate::hex(base, base.len()),
            "invalid empty salvage launch base",
        )?;
        crate::git::text(
            service,
            &execution,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
            stop,
        )?
    } else {
        crate::git::text(service, &execution, &["rev-parse", "HEAD"], stop)?
    };
    require(
        rows.iter()
            .filter(|v| {
                v["task_id"] == task && v["attempt_id"] == attempt && v["patch"].is_string()
            })
            .all(|v| crate::results::verified_patch(&store, v).is_ok_and(|v| v.is_empty())),
        "empty salvage has indexed recovery bytes",
    )?;
    match std::fs::symlink_metadata(&kept) {
        Ok(_) => {
            require(
                indexed.and_then(|v| v["base_sha"].as_str()).is_some(),
                "kept worktree has no launch base",
            )?;
            let identity = managed(&execution, &kept)?;
            require(
                capture(&identity.0, &identity.1, &base, &identity.2, service, stop)?
                    .patch
                    .is_empty()
                    && managed(&execution, &kept)? == identity,
                "empty salvage worktree has changed",
            )?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::new(e)),
    }
    let request = SalvageRequest {
        task_id: task,
        attempt_id: attempt,
        member,
        kept,
        execution_repo: execution,
        base,
    };
    registered_empty(&request, service, stop)?;
    no_unindexed_recovery(&store, &request, worker, &rows)?;
    Ok(true)
}

fn no_unindexed_recovery(
    store: &Store,
    request: &SalvageRequest,
    worker: &Value,
    rows: &[Value],
) -> Result<()> {
    let provenance = Path::new(".oms/salvage-provenance");
    match std::fs::read_dir(store.repo().join(provenance)) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(Error::new)?;
                let name = entry.file_name();
                if name
                    .to_str()
                    .is_none_or(|v| !v.ends_with(".json") || v.starts_with("empty-"))
                {
                    continue;
                }
                let Ok(Some(value)) = store.json(&provenance.join(name), MAX_EVIDENCE, false)
                else {
                    continue;
                };
                require(
                    !crate::value::array(&value["records"]).iter().any(|v| {
                        v["salvaged_from"]["task"] == request.task_id
                            && v["salvaged_from"]["attempt"] == request.attempt_id
                    }),
                    "retained recovery provenance prevents empty salvage",
                )?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::new(e)),
    }
    if rows.iter().any(|v| {
        v["task_id"] == request.task_id
            && v["attempt_id"] == request.attempt_id
            && v["patch"].is_string()
    }) {
        return Ok(());
    }
    let provider = worker["refs"]["panel_provider"]
        .as_str()
        .or_else(|| worker["provider"].as_str())
        .unwrap_or_default();
    require(
        !provider.is_empty()
            && provider
                .bytes()
                .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || b"_-".contains(&v)),
        "unindexed delegate provider unavailable",
    )?;
    let slug = regex::Regex::new("[^a-z0-9]+")
        .map_err(Error::new)?
        .replace_all(&request.task_id.to_ascii_lowercase(), "-")
        .trim_matches('-')
        .chars()
        .take(48)
        .collect::<String>();
    let pattern = regex::Regex::new(&format!(
        "^{}-{}-(\\d{{8}}T\\d{{6}}Z)-\\d+\\.patch$",
        regex::escape(provider),
        regex::escape(&slug)
    ))
    .map_err(Error::new)?;
    let created = chrono::NaiveDateTime::parse_from_str(
        &crate::value::text(worker, "created_at"),
        "%Y-%m-%dT%H:%M:%SZ",
    )
    .map_err(|_| Error::new("unindexed delegate creation evidence unavailable"))?;
    let directory = Path::new(".oms/artifacts/delegate");
    match std::fs::read_dir(store.repo().join(directory)) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(Error::new)?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let Some(captures) = pattern.captures(name) else {
                    continue;
                };
                let stamp = chrono::NaiveDateTime::parse_from_str(&captures[1], "%Y%m%dT%H%M%SZ")
                    .map_err(Error::new)?;
                let path = directory.join(name);
                if (stamp - created).num_seconds().unsigned_abs() > 300
                    || rows.iter().any(|v| {
                        v["attempt_id"] != request.attempt_id
                            && v["patch"] == path.to_string_lossy().as_ref()
                    })
                {
                    continue;
                }
                require(
                    store
                        .read(&path, crate::results::PATCH_MAX, false)?
                        .is_empty(),
                    "unindexed delegate patch prevents empty salvage",
                )?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::new(e)),
    }
    Ok(())
}
pub fn empty_provenance(store: &Store, task: &str, attempt: &str) -> Result<bool> {
    let sha = empty_identity(task, attempt)?;
    let path = format!(".oms/salvage-provenance/empty-{sha}.json");
    let Some(value) = store.json(Path::new(&path), MAX_EVIDENCE, true)? else {
        return Ok(false);
    };
    require(
        value["schema"] == 1 && value["payload_sha256"] == sha && value["records"].is_array(),
        "invalid empty salvage provenance",
    )?;
    Ok(crate::value::array(&value["records"]).iter().any(|v| {
        v["salvaged_from"]["task"] == task
            && v["salvaged_from"]["attempt"] == attempt
            && v["salvaged_from"]["outcome"] == "nothing_to_recover"
    }))
}
fn header(body: &[u8]) -> Result<Value> {
    let tail = body
        .strip_prefix(PREFIX.as_bytes())
        .ok_or_else(|| Error::new("salvage header absent"))?;
    let end = tail
        .iter()
        .position(|v| *v == b'\n')
        .ok_or_else(|| Error::new("salvage header incomplete"))?;
    crate::decode(&tail[..end], MAX_EVIDENCE)
}
fn budgeted_read(
    store: &Store,
    path: &Path,
    per_record: usize,
    remaining: &mut usize,
) -> Result<Vec<u8>> {
    // The no-follow reader probes one extra byte to detect oversized inputs.
    // Reserve that byte from the aggregate budget even when the read fails.
    require(*remaining > 0, "salvage evidence exceeds bounded byte scan")?;
    let maximum = per_record.min(*remaining - 1);
    let bytes = store.read(path, maximum, false)?;
    *remaining -= bytes.len();
    Ok(bytes)
}
/// Select only paired, digest-checked patches with matching provenance. A patch
/// without its sidecar, or provenance without its patch, remains unconfirmed.
pub fn continuation_patch(store: &Store, task: &str, attempt: &str) -> Result<Option<Value>> {
    continuation_within(store, task, attempt, LIMITS)
}
fn continuation_within(
    store: &Store,
    task: &str,
    attempt: &str,
    limits: Limits,
) -> Result<Option<Value>> {
    crate::identifier(task)?;
    crate::identifier(attempt)?;
    let directory = store.repo().join(".oms/artifacts/delegate");
    let entries = match std::fs::read_dir(&directory) {
        Ok(v) => Some(v),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(Error::new(e)),
    };
    let prefix = format!("salvage-{task}-");
    let mut names = std::collections::BTreeSet::new();
    for (count, entry) in entries.into_iter().flatten().enumerate() {
        require(
            count < limits.raw,
            "salvage inventory exceeds bounded raw scan",
        )?;
        let entry = entry.map_err(Error::new)?;
        let name = entry.file_name();
        if !name.as_bytes().starts_with(prefix.as_bytes()) {
            continue;
        }
        require(
            names.len() < limits.relevant,
            "salvage inventory exceeds bounded relevant scan",
        )?;
        let name = name
            .into_string()
            .map_err(|_| Error::new("non-UTF-8 salvage name"))?;
        names.insert(name);
    }
    let mut selected = Vec::new();
    let mut covered = std::collections::BTreeSet::new();
    for name in &names {
        if !name.starts_with(&prefix) || !name.ends_with(".patch") {
            continue;
        }
        require(
            names.contains(&format!("{name}.json")),
            "salvage publication incomplete: patch has no sidecar",
        )?;
    }
    let sidecars: Vec<&String> = names
        .iter()
        .filter(|n| n.ends_with(".patch.json"))
        .collect();
    require(
        sidecars.len() <= limits.sidecars,
        "salvage inventory exceeds bounded sidecar scan",
    )?;
    let mut remaining = limits.bytes;
    for name in sidecars {
        let path = format!(".oms/artifacts/delegate/{}", &name[..name.len() - 5]);
        let raw = budgeted_read(
            store,
            Path::new(&format!("{path}.json")),
            MAX_EVIDENCE,
            &mut remaining,
        )?;
        let meta = crate::decode(&raw, MAX_EVIDENCE)?;
        let owner = crate::value::text(&meta["salvaged_from"], "task");
        let foreign = owner != task
            && owner.starts_with(&format!("{task}-"))
            && crate::identifier(&owner).is_ok()
            && name.starts_with(&format!("salvage-{owner}-"));
        require(
            owner == task || foreign,
            "salvage task differs from filename",
        )?;
        if !foreign && meta["salvaged_from"]["attempt"] != attempt {
            continue;
        }
        require(
            meta["patch"] == path
                && crate::hex(meta["patch_sha256"].as_str().unwrap_or_default(), 64),
            "invalid indexed patch identity",
        )?;
        let body = budgeted_read(
            store,
            Path::new(&path),
            crate::results::PATCH_MAX,
            &mut remaining,
        )?;
        require(
            crate::digest(&body) == meta["patch_sha256"],
            "indexed patch digest changed",
        )?;
        let header = header(&body)?;
        for key in [
            "salvaged_from",
            "salvaged_at",
            "payload_sha256",
            "base_sha",
            "encoding_warnings",
        ] {
            require(
                header[key] == meta[key],
                "salvage header differs from sidecar",
            )?;
        }
        let sha = crate::digest(&payload(&body));
        require(
            meta["payload_sha256"] == sha,
            "salvage payload digest differs",
        )?;
        let proof_path = format!(".oms/salvage-provenance/{sha}.json");
        let proof = store
            .json(Path::new(&proof_path), MAX_EVIDENCE, false)?
            .ok_or_else(|| Error::new("salvage provenance absent"))?;
        require(
            proof["schema"] == 1
                && proof["payload_sha256"] == sha
                && crate::value::array(&proof["records"]).iter().any(|v| {
                    v["salvaged_from"] == meta["salvaged_from"] && v["base_sha"] == meta["base_sha"]
                }),
            "salvage provenance does not bind this patch",
        )?;
        if !foreign {
            covered.insert(sha);
            selected.push(meta);
        }
    }
    let proof_dir = store.repo().join(".oms/salvage-provenance");
    if proof_dir.exists() {
        for (count, entry) in std::fs::read_dir(proof_dir)
            .map_err(Error::new)?
            .enumerate()
        {
            require(count < 10000, "salvage provenance exceeds bounded scan")?;
            let name = entry.map_err(Error::new)?.file_name();
            let relative = PathBuf::from(".oms/salvage-provenance").join(&name);
            let value = store
                .json(&relative, MAX_EVIDENCE, false)?
                .ok_or_else(|| Error::new("salvage provenance disappeared"))?;
            let relevant = crate::value::array(&value["records"]).iter().any(|v| {
                v["salvaged_from"]["task"] == task
                    && v["salvaged_from"]["attempt"] == attempt
                    && v["salvaged_from"]["outcome"] != "nothing_to_recover"
            });
            if relevant {
                require(
                    value["payload_sha256"]
                        .as_str()
                        .is_some_and(|v| covered.contains(v)),
                    "salvage publication incomplete: provenance has no paired patch",
                )?;
            }
        }
    }
    selected.sort_by_key(|v| {
        (
            crate::value::text(v, "salvaged_at"),
            crate::value::text(v, "patch"),
        )
    });
    Ok(selected.pop())
}

fn salvaged_at() -> Result<String> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(Error::new)?;
    let stamp = chrono::DateTime::<chrono::Utc>::from_timestamp(
        i64::try_from(duration.as_secs()).map_err(Error::new)?,
        duration.subsec_nanos(),
    )
    .ok_or_else(|| Error::new("salvage timestamp unavailable"))?;
    Ok(stamp.format("%Y-%m-%dT%H:%M:%S.%6fZ").to_string())
}

fn registered_empty<S: Services + ?Sized>(
    request: &SalvageRequest,
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    let worktrees = crate::git::text(
        service,
        &request.execution_repo,
        &["worktree", "list", "--porcelain"],
        stop,
    )?;
    for line in worktrees
        .lines()
        .filter_map(|v| v.strip_prefix("worktree "))
    {
        let path = Path::new(line);
        if path == request.kept
            || path.file_name() != request.kept.file_name()
            || path.parent().and_then(Path::file_name)
                != request.kept.parent().and_then(Path::file_name)
        {
            continue;
        }
        let identity = managed(&request.execution_repo, path)?;
        require(
            capture(
                &identity.0,
                &identity.1,
                &request.base,
                &identity.2,
                service,
                stop,
            )?
            .patch
            .is_empty()
                && managed(&request.execution_repo, path)? == identity,
            "registered worktree retains recovery data",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    fn delegate_dir(scratch: &Scratch) -> PathBuf {
        let directory = scratch.0.join(".oms/artifacts/delegate");
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }
    fn touch(directory: &Path, name: &str) {
        std::fs::File::create(directory.join(name)).unwrap();
    }
    fn failure(store: &Store, limits: Limits) -> String {
        continuation_within(store, "target", "attempt", limits)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn continuation_scans_flat_inventory_above_relevant_limit() {
        let scratch = Scratch::new().unwrap();
        let directory = delegate_dir(&scratch);
        for i in 0..=INVENTORY_RELEVANT_LIMIT {
            touch(&directory, &format!("delegate-{i}.patch"));
        }
        let store = Store::open(&scratch.0).unwrap();
        assert!(
            continuation_patch(&store, "target", "attempt")
                .unwrap()
                .is_none()
        );
        touch(&directory, "salvage-target-incomplete.patch");
        assert!(failure(&store, LIMITS).contains("patch has no sidecar"));
    }

    #[test]
    fn continuation_filters_unrelated_names_but_rejects_invalid_relevant_utf8() {
        let scratch = Scratch::new().unwrap();
        let directory = delegate_dir(&scratch);
        let make = |name: &[u8]| {
            std::fs::File::create(directory.join(OsString::from_vec(name.to_vec()))).is_ok()
        };
        // Filesystems such as APFS refuse non-UTF-8 names; nothing to test there.
        if cfg!(target_os = "macos") {
            return;
        }
        assert!(make(b"other-\xff"));
        let store = Store::open(&scratch.0).unwrap();
        assert!(
            continuation_patch(&store, "target", "attempt")
                .unwrap()
                .is_none()
        );
        assert!(make(b"salvage-target-\xff"));
        assert!(failure(&store, LIMITS).contains("non-UTF-8 salvage name"));
    }

    #[test]
    fn continuation_limits_bound_scan_and_sidecar_bytes() {
        let scratch = Scratch::new().unwrap();
        let directory = delegate_dir(&scratch);
        touch(&directory, "unrelated-1");
        touch(&directory, "unrelated-2");
        let store = Store::open(&scratch.0).unwrap();
        let tiny = Limits { raw: 1, ..LIMITS };
        assert!(failure(&store, tiny).contains("bounded raw scan"));
        touch(&directory, "salvage-target-1");
        touch(&directory, "salvage-target-2");
        let tiny = Limits {
            relevant: 1,
            ..LIMITS
        };
        assert!(failure(&store, tiny).contains("bounded relevant scan"));
        std::fs::remove_file(directory.join("salvage-target-1")).unwrap();
        std::fs::remove_file(directory.join("salvage-target-2")).unwrap();
        publish(&directory, "salvage-target-1.patch", "target", "target");
        publish(&directory, "salvage-target-2.patch", "target", "target");
        let tiny = Limits {
            sidecars: 1,
            ..LIMITS
        };
        assert!(failure(&store, tiny).contains("bounded sidecar scan"));
        // Each read is capped by the remaining total, not by a prior stat.
        let tiny = Limits { bytes: 8, ..LIMITS };
        assert!(failure(&store, tiny).contains("refused"));
    }

    #[test]
    fn continuation_rejects_unpaired_relevant_provenance() {
        let scratch = Scratch::new().unwrap();
        let directory = scratch.0.join(".oms/salvage-provenance");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("proof.json"), br#"{"payload_sha256":"abc","records":[{"salvaged_from":{"task":"target","attempt":"attempt"}}]}"#).unwrap();
        let store = Store::open(&scratch.0).unwrap();
        assert!(failure(&store, LIMITS).contains("provenance has no paired patch"));
    }

    fn publish(directory: &Path, name: &str, task: &str, claimed: &str) {
        let from = serde_json::json!({"task":task,"attempt":"x"});
        let payload = format!("diff --git a/{task} b/{task}\n");
        let sha = crate::digest(payload.as_bytes());
        let head = serde_json::json!({"salvaged_from":from,"salvaged_at":"now","payload_sha256":sha,"base_sha":"base","encoding_warnings":[]});
        let body = format!("{PREFIX}{head}\n{}", payload);
        std::fs::write(directory.join(name), &body).unwrap();
        let path = format!(".oms/artifacts/delegate/{name}");
        let mut meta = head.clone();
        meta["salvaged_from"]["task"] = claimed.into();
        meta["patch"] = path.into();
        meta["patch_sha256"] = crate::digest(body.as_bytes()).into();
        std::fs::write(directory.join(format!("{name}.json")), meta.to_string()).unwrap();
        let proof_dir = directory
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("salvage-provenance");
        std::fs::create_dir_all(&proof_dir).unwrap();
        let proof = serde_json::json!({"schema":1,"payload_sha256":sha,"records":[{"salvaged_from":from,"base_sha":"base"}]});
        std::fs::write(proof_dir.join(format!("{sha}.json")), proof.to_string()).unwrap();
    }

    #[test]
    fn continuation_skips_only_proven_longer_task_pair() {
        let scratch = Scratch::new().unwrap();
        let directory = scratch.0.join(".oms/artifacts/delegate");
        std::fs::create_dir_all(&directory).unwrap();
        publish(&directory, "salvage-a-b-1.patch", "a-b", "a-b");
        let store = Store::open(&scratch.0).unwrap();
        assert!(
            continuation_patch(&store, "a", "attempt")
                .unwrap()
                .is_none()
        );
        // Header names a different task than the sidecar: tampered, not foreign.
        publish(&directory, "salvage-a-b-2.patch", "other", "a-b");
        assert!(
            continuation_patch(&store, "a", "attempt")
                .unwrap_err()
                .to_string()
                .contains("header differs")
        );
        // A sidecar claiming an unrelated task is never skipped.
        publish(&directory, "salvage-a-b-2.patch", "zzz", "zzz");
        assert!(
            continuation_patch(&store, "a", "attempt")
                .unwrap_err()
                .to_string()
                .contains("task differs")
        );
    }
    #[test]
    fn continuation_bounds_patch_bytes_for_foreign_prefix() {
        let scratch = Scratch::new().unwrap();
        let directory = delegate_dir(&scratch);
        publish(
            &directory,
            "salvage-target-long-1.patch",
            "target-long",
            "target-long",
        );
        let sidecar = std::fs::metadata(directory.join("salvage-target-long-1.patch.json"))
            .unwrap()
            .len() as usize;
        let store = Store::open(&scratch.0).unwrap();
        let limits = Limits {
            bytes: sidecar + 4,
            ..LIMITS
        };
        assert!(failure(&store, limits).contains("refused"));
    }
    #[test]
    fn continuation_bounds_aggregate_patch_reads() {
        let scratch = Scratch::new().unwrap();
        let directory = delegate_dir(&scratch);
        publish(
            &directory,
            "salvage-target-long-1.patch",
            "target-long",
            "target-long",
        );
        publish(
            &directory,
            "salvage-target-longer-2.patch",
            "target-longer",
            "target-longer",
        );
        let bytes = |name: &str| std::fs::metadata(directory.join(name)).unwrap().len() as usize;
        let budget = bytes("salvage-target-long-1.patch.json")
            + bytes("salvage-target-long-1.patch")
            + bytes("salvage-target-longer-2.patch.json")
            + bytes("salvage-target-longer-2.patch")
            - 1;
        let store = Store::open(&scratch.0).unwrap();
        assert!(
            failure(
                &store,
                Limits {
                    bytes: budget,
                    ..LIMITS
                }
            )
            .contains("refused")
        );
        assert!(
            continuation_within(
                &store,
                "target",
                "attempt",
                Limits {
                    bytes: budget + 2,
                    ..LIMITS
                }
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn continuation_rejects_foreign_pair_without_provenance() {
        let scratch = Scratch::new().unwrap();
        let directory = delegate_dir(&scratch);
        publish(
            &directory,
            "salvage-target-long-1.patch",
            "target-long",
            "target-long",
        );
        std::fs::remove_dir_all(scratch.0.join(".oms/salvage-provenance")).unwrap();
        let store = Store::open(&scratch.0).unwrap();
        assert!(failure(&store, LIMITS).contains("refused"));
    }
}
