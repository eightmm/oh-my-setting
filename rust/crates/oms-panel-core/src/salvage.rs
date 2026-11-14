//! Stopped worktree capture with isolated Git metadata and immutable provenance.
use crate::services::ExecutionRequest;
use crate::{Error, Result, Services, Store, Value, require};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
pub const MAX_EVIDENCE: usize = 8 * 1024 * 1024;
pub const PREFIX: &str = "# oms-salvage: ";
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
struct Git<'a> {
    scratch: &'a Path,
    kept: &'a Path,
    common: &'a Path,
    service: &'a dyn Services,
    stop: &'a AtomicBool,
}
impl Git<'_> {
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
pub fn capture(
    kept: &Path,
    common: &Path,
    base: &str,
    head: &str,
    service: &dyn Services,
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
    let control = crate::terminals::controller_store(repo, false)?;
    let receipt = control
        .json(
            &PathBuf::from(&request.member).join(format!("{}-supervisor.json", request.member)),
            65536,
            false,
        )?
        .ok_or_else(|| Error::new("private supervisor receipt unavailable"))?;
    let receipt = receipt.get("record").unwrap_or(&receipt);
    require(
        receipt["cleanup_complete"] == true && receipt["member"] == request.member,
        "supervisor cleanup is unproven",
    )?;
    let identity = managed(&request.execution_repo, &request.kept)?;
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
        !captured.patch.is_empty(),
        "no patch bytes recovered; empty salvage requires independent evidence reconciliation",
    )?;
    let store = Store::open(repo)?;
    let mut metadata = serde_json::json!({"schema":1,"kind":"oms-salvaged-patch","base_sha":captured.base,"salvaged_at":crate::now(),
        "salvaged_from":{"task":request.task_id,"attempt":request.attempt_id,"kept_path":request.kept,"stop_reason":"confirmed-stopped"},
        "files":captured.files,"encoding_warnings":captured.encoding_warnings,"payload_sha256":crate::digest(&payload(&captured.patch))});
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
    Ok(metadata)
}
