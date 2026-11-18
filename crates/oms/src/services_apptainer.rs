//! Optional unprivileged Apptainer execution; the parent retains process ownership.
use super::{BackendRequest, ChildSpec, RuntimeServices, Value, plan_error, rvalue};
use oms_command_plan::Result;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use users::os::unix::UserExt;

const IMAGE_LIMIT: u64 = 64 * 1024 * 1024 * 1024;
const HASH_WALL: Duration = Duration::from_secs(120);
const FLAGS: &[&str] = &[
    "--containall",
    "--cleanenv",
    "--no-home",
    "--no-eval",
    "--userns",
    "--no-mount",
    "--drop-caps",
    "--no-privs",
    "--net",
    "--network",
    "--bind",
    "--cwd",
    "--memory",
    "--cpus",
    "--pids-limit",
];

pub(super) fn locate(search: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(search)
        .filter(|p| p.is_absolute())
        .find_map(|dir| {
            let path = std::fs::canonicalize(dir.join("apptainer")).ok()?;
            let meta = std::fs::metadata(&path).ok()?;
            (meta.is_file()
                && meta.permissions().mode() & 0o111 != 0
                && meta.permissions().mode() & 0o6000 == 0)
                .then_some(path)
        })
}

fn host_environment() -> Result<Vec<(OsString, OsString)>> {
    let mut env = vec![("LANG".into(), "C".into())];
    if let Some(path) = std::env::var_os("PATH") {
        env.push(("PATH".into(), path));
    }
    if let Some(user) = users::get_user_by_uid(users::get_current_uid()) {
        env.push(("HOME".into(), user.home_dir().as_os_str().into()));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
    // Rootless cgroup requests need the user's systemd session. Accept only a
    // local bus beneath a private owned directory, never remote D-Bus addresses.
    if users::get_current_uid() != 0 || runtime.is_some() || bus.is_some() {
        let runtime = runtime.ok_or_else(|| plan_error("Apptainer resource limits require XDG_RUNTIME_DIR and a local systemd user session"))?;
        let dir = Path::new(&runtime);
        let meta = std::fs::symlink_metadata(dir)
            .map_err(|_| plan_error("Apptainer session directory is unavailable"))?;
        let text = dir
            .to_str()
            .ok_or_else(|| plan_error("invalid Apptainer session directory"))?;
        if !dir.is_absolute()
            || !meta.is_dir()
            || meta.uid() != users::get_current_uid()
            || meta.mode() & 0o077 != 0
            || !text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
            || std::fs::canonicalize(dir).ok().as_deref() != Some(dir)
        {
            return Err(plan_error(
                "Apptainer session directory must be an absolute private directory owned by the current user",
            ));
        }
        let expected = format!("unix:path={text}/bus");
        if bus.as_deref() != Some(OsStr::new(&expected)) {
            return Err(plan_error(
                "Apptainer requires DBUS_SESSION_BUS_ADDRESS=unix:path=<XDG_RUNTIME_DIR>/bus",
            ));
        }
        let socket = std::fs::symlink_metadata(dir.join("bus"))
            .map_err(|_| plan_error("Apptainer user session bus is unavailable"))?;
        if !socket.file_type().is_socket() || socket.uid() != users::get_current_uid() {
            return Err(plan_error(
                "Apptainer user session bus must be a socket owned by the current user",
            ));
        }
        env.extend([
            ("XDG_RUNTIME_DIR".into(), runtime),
            ("DBUS_SESSION_BUS_ADDRESS".into(), expected.into()),
        ]);
    }
    Ok(env)
}

fn generation(meta: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

pub(super) struct Image {
    file: File,
    path: PathBuf,
    observed: Metadata,
    sha256: String,
}
impl Image {
    pub(super) fn open(raw: &OsStr) -> Result<Self> {
        use rustix::fs::{Mode, OFlags, open};
        if raw.is_empty() || raw.to_string_lossy().contains("://") {
            return Err(plan_error(
                "Apptainer requires an explicit local prebuilt SIF image",
            ));
        }
        let named = std::fs::symlink_metadata(raw)
            .map_err(|_| plan_error("Apptainer image is unavailable"))?;
        if !named.is_file() || named.len() == 0 || named.len() > IMAGE_LIMIT {
            return Err(plan_error(
                "Apptainer image must be a nonempty regular file of at most 64 GiB; links and devices refuse",
            ));
        }
        let path = std::fs::canonicalize(raw).map_err(|e| plan_error(e.to_string()))?;
        let fd = open(
            &path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| plan_error(e.to_string()))?;
        let file = File::from(fd);
        let observed = file.metadata().map_err(|e| plan_error(e.to_string()))?;
        if !observed.is_file() || generation(&named) != generation(&observed) {
            return Err(plan_error("Apptainer image changed while opening"));
        }
        Ok(Self {
            file,
            path,
            observed,
            sha256: String::new(),
        })
    }
    pub(super) fn check(&self) -> Result<()> {
        let opened = self
            .file
            .metadata()
            .map_err(|e| plan_error(e.to_string()))?;
        let named = std::fs::symlink_metadata(&self.path).map_err(|e| plan_error(e.to_string()))?;
        if !named.is_file()
            || generation(&opened) != generation(&self.observed)
            || generation(&named) != generation(&self.observed)
        {
            return Err(plan_error(
                "Apptainer image identity changed after observation",
            ));
        }
        Ok(())
    }
    pub(super) fn hash(&mut self, stop: &AtomicBool) -> Result<String> {
        self.check()?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| plan_error(e.to_string()))?;
        let mut input = (&mut self.file).take(self.observed.len() + 1);
        let (mut hash, mut bytes, started) = (Sha256::new(), 0u64, Instant::now());
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if stop.load(Ordering::Acquire) {
                return Err(oms_command_plan::Error {
                    code: 130,
                    message: "interrupted while observing Apptainer image".into(),
                });
            }
            if started.elapsed() >= HASH_WALL {
                return Err(plan_error(
                    "Apptainer image observation exceeded 120 seconds",
                ));
            }
            let count = input
                .read(&mut buffer)
                .map_err(|e| plan_error(e.to_string()))?;
            if count == 0 {
                break;
            }
            bytes += count as u64;
            hash.update(&buffer[..count]);
        }
        self.check()?;
        if bytes != self.observed.len() {
            return Err(plan_error("Apptainer image length changed while hashing"));
        }
        Ok(format!("{:x}", hash.finalize()))
    }
    pub(super) fn verify_after(&mut self) -> Result<()> {
        // Cleanup already happened. Cancellation must not suppress the receipt's
        // final image observation, which remains separately bounded.
        if self.hash(&AtomicBool::new(false))? != self.sha256 {
            return Err(plan_error("Apptainer image bytes changed during execution"));
        }
        Ok(())
    }
    fn receipt(&self) -> Value {
        rvalue::object([
            ("path", rvalue::string(self.path.to_string_lossy())),
            ("bytes", rvalue::number(self.observed.len())),
            (
                "sha256",
                if self.sha256.is_empty() {
                    Value::Null
                } else {
                    rvalue::string(&self.sha256)
                },
            ),
            (
                "identity_kind",
                rvalue::string("observed-file; not an immutable or signed image assertion"),
            ),
        ])
    }
}

fn probe(host: &RuntimeServices<'_>, binary: &Path, arguments: &[OsString]) -> Result<String> {
    let mut argv = vec![binary.as_os_str().to_owned()];
    argv.extend_from_slice(arguments);
    let mut spec = ChildSpec::new(argv, Duration::from_secs(10), 64 * 1024);
    spec.clear_environment = true;
    spec.environment = host_environment()?;
    let out = host.child(spec).map_err(|mut error| {
        if host.cancel().load(Ordering::Acquire) {
            error.code = 130;
        }
        error
    })?;
    if out.cancelled {
        return Err(oms_command_plan::Error {
            code: 130,
            message: "interrupted during Apptainer preflight".into(),
        });
    }
    if out.code != 0 || out.timed_out || out.dropped != 0 {
        return Err(plan_error(
            "Apptainer preflight refused or could not complete; no container started",
        ));
    }
    String::from_utf8(out.stdout)
        .map_err(|_| plan_error("Apptainer preflight returned invalid text"))
}

fn preflight(host: &RuntimeServices<'_>, raw: &OsStr) -> Result<(Image, String)> {
    if !cfg!(target_os = "linux") {
        return Err(plan_error(
            "Apptainer execution requires Linux (including WSL2)",
        ));
    }
    let binary = host
        .apptainer
        .as_deref()
        .ok_or_else(|| plan_error("Apptainer executable is unavailable on PATH"))?;
    let meta = std::fs::symlink_metadata(binary)
        .map_err(|_| plan_error("Apptainer executable is unavailable"))?;
    if !meta.is_file()
        || meta.permissions().mode() & 0o111 == 0
        || meta.permissions().mode() & 0o6000 != 0
    {
        return Err(plan_error(
            "Apptainer executable must be regular, executable and without setuid/setgid bits",
        ));
    }
    let image = Image::open(raw)?;
    let version = probe(host, binary, &["--version".into()])?;
    if !version.trim().starts_with("apptainer version ") {
        return Err(plan_error(
            "executable did not identify itself as Apptainer",
        ));
    }
    let help = probe(host, binary, &["exec".into(), "--help".into()])?;
    if FLAGS
        .iter()
        .any(|flag| !help.split_whitespace().any(|word| word == *flag))
    {
        return Err(plan_error(
            "installed Apptainer lacks required containment options",
        ));
    }
    probe(
        host,
        binary,
        &[
            "sif".into(),
            "header".into(),
            image.path.as_os_str().to_owned(),
        ],
    )?;
    image.check()?;
    Ok((image, version.trim().chars().take(160).collect()))
}

pub(super) fn readiness(host: &RuntimeServices<'_>, raw: &OsStr) -> Result<Value> {
    match preflight(host, raw) {
        Ok((image, version)) => Ok(rvalue::object([
            ("ready", rvalue::b(true)),
            ("missing", rvalue::a([])),
            (
                "details",
                rvalue::object([
                    ("version", rvalue::string(version)),
                    ("image", image.receipt()),
                    ("preflight_only", rvalue::b(true)),
                    (
                        "namespace_support",
                        rvalue::string("unproven until execution; no privileged fallback"),
                    ),
                ]),
            ),
        ])),
        Err(error) if host.cancel().load(Ordering::Acquire) => {
            Err(oms_command_plan::Error { code: 130, ..error })
        }
        Err(error) => Ok(rvalue::object([
            ("ready", rvalue::b(false)),
            ("missing", rvalue::list([error.message.as_str()])),
            ("details", rvalue::empty()),
        ])),
    }
}

fn bind_directory(path: &Path) -> Result<PathBuf> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| plan_error(e.to_string()))?;
    if !meta.is_dir() {
        return Err(plan_error("Apptainer bind source must be a real directory"));
    }
    let path = std::fs::canonicalize(path).map_err(|e| plan_error(e.to_string()))?;
    let text = path
        .to_str()
        .ok_or_else(|| plan_error("Apptainer bind paths must be UTF-8"))?;
    if text.contains([':', ',', '\n', '\r', '\0']) || path.parent().is_none() {
        return Err(plan_error(
            "Apptainer refuses ambiguous bind delimiters or a filesystem root",
        ));
    }
    if users::get_user_by_uid(users::get_current_uid()).is_some_and(|user| {
        std::fs::canonicalize(user.home_dir())
            .unwrap_or_else(|_| user.home_dir().to_owned())
            .starts_with(&path)
    }) {
        return Err(plan_error(
            "Apptainer refuses binding the host home or its ancestors",
        ));
    }
    Ok(path)
}

pub(super) struct Launch {
    pub argv: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    pub image: Image,
    pub details: Value,
}
pub(super) fn prepare(
    host: &RuntimeServices<'_>,
    repo: &Path,
    request: &BackendRequest,
) -> Result<Launch> {
    if request.image.is_empty() {
        return Err(plan_error(
            "Apptainer requires an explicit local prebuilt SIF image",
        ));
    }
    if !request.adapter.is_empty() {
        return Err(plan_error("--adapter does not apply to Apptainer"));
    }
    if !(64..=1048576).contains(&request.memory_mb)
        || !request.cpus.is_finite()
        || !(0.1..=1024.0).contains(&request.cpus)
        || !(16..=1000000).contains(&request.pids)
    {
        return Err(plan_error("invalid Apptainer resource request"));
    }
    let repo = bind_directory(repo)?;
    let work = request
        .worktree
        .as_deref()
        .map(|p| {
            bind_directory(&if p.is_absolute() {
                p.to_owned()
            } else {
                repo.join(p)
            })
        })
        .transpose()?;
    let cwd = std::fs::canonicalize(&request.cwd).map_err(|e| plan_error(e.to_string()))?;
    if cwd != *work.as_ref().unwrap_or(&repo)
        || work
            .as_ref()
            .is_some_and(|p| *p != repo && repo.starts_with(p))
    {
        return Err(plan_error(
            "Apptainer cwd must match the selected worktree or read-only repo; a worktree cannot contain the repo",
        ));
    }
    let selected = Path::new(&request.image);
    let selected = if selected.is_absolute() {
        selected.to_owned()
    } else {
        repo.join(selected)
    };
    let (mut image, version) = preflight(host, selected.as_os_str())?;
    image.sha256 = image.hash(host.cancel())?;
    let argv = command(
        host.apptainer.as_ref().unwrap(),
        &repo,
        work.as_deref(),
        &cwd,
        &image.path,
        request,
    );
    let explicit = container_environment(&request.inherit_env)?;
    let inherited = explicit
        .iter()
        .map(|(key, _)| rvalue::string(key.to_string_lossy().trim_start_matches("APPTAINERENV_")))
        .collect::<Vec<_>>();
    let mut environment = host_environment()?;
    environment.extend(explicit);
    environment.extend([
        ("APPTAINERENV_HOME".into(), "/tmp".into()),
        ("APPTAINERENV_OMS_RUNTIME_BACKEND".into(), "1".into()),
    ]);
    let details = rvalue::object([
        ("version", rvalue::string(version)),
        ("image", image.receipt()),
        (
            "repo_mount",
            rvalue::string(if work.as_ref() == Some(&repo) {
                "rw"
            } else {
                "ro"
            }),
        ),
        (
            "worktree",
            work.map_or(Value::Null, |p| rvalue::string(p.to_string_lossy())),
        ),
        (
            "network",
            rvalue::string(if request.allow_network {
                "host-requested"
            } else {
                "none-requested"
            }),
        ),
        (
            "namespace_enforcement",
            rvalue::string("requested; not independently attested"),
        ),
        ("inherited_environment", rvalue::a(inherited)),
        (
            "gpu",
            rvalue::string("not-requested; default contained devices"),
        ),
        (
            "image_stability",
            rvalue::string("pending-post-execution-observation"),
        ),
    ]);
    Ok(Launch {
        argv,
        environment,
        image,
        details,
    })
}

pub(super) fn command(
    binary: &Path,
    repo: &Path,
    work: Option<&Path>,
    cwd: &Path,
    image: &Path,
    request: &BackendRequest,
) -> Vec<OsString> {
    let mut argv = vec![binary.as_os_str().to_owned()];
    argv.extend(
        [
            "exec",
            "--containall",
            "--cleanenv",
            "--no-home",
            "--no-eval",
            "--userns",
            "--no-mount",
            "hostfs,bind-paths,cwd",
            "--drop-caps",
            "ALL",
            "--no-privs",
        ]
        .into_iter()
        .map(OsString::from),
    );
    if !request.allow_network {
        argv.extend(["--net".into(), "--network".into(), "none".into()]);
    }
    argv.extend([
        "--memory".into(),
        (request.memory_mb * 1024 * 1024).to_string().into(),
        "--cpus".into(),
        request.cpus.to_string().into(),
        "--pids-limit".into(),
        request.pids.to_string().into(),
    ]);
    let repo_mode = if work == Some(repo) { "rw" } else { "ro" };
    argv.extend([
        "--bind".into(),
        format!("{}:{}:{repo_mode}", repo.display(), repo.display()).into(),
    ]);
    if let Some(work) = work.filter(|p| *p != repo) {
        argv.extend([
            "--bind".into(),
            format!("{}:{}:rw", work.display(), work.display()).into(),
        ]);
    }
    argv.extend([
        "--cwd".into(),
        cwd.as_os_str().to_owned(),
        image.as_os_str().to_owned(),
    ]);
    argv.extend(request.argv.iter().cloned());
    argv
}

pub(super) fn container_environment(inherit: &[OsString]) -> Result<Vec<(OsString, OsString)>> {
    let mut environment = Vec::new();
    for key in inherit {
        oms_runtime::backend::apptainer_environment_name(key)?;
        if let Some(value) = std::env::var_os(key) {
            let pair = format!("{}={}", key.to_string_lossy(), value.to_string_lossy());
            if oms_runtime::capsule::sensitive_value(&pair) {
                return Err(plan_error(
                    "refusing sensitive-looking container environment value",
                ));
            }
            environment.push((
                format!("APPTAINERENV_{}", key.to_string_lossy()).into(),
                value,
            ));
        }
    }
    Ok(environment)
}
