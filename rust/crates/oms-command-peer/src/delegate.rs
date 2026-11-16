use crate::{
    Context, Error, Output, Result, V,
    cli::Args,
    fail, json, now, obj, out, peer, process,
    providers::{self, Target},
    safety, storage,
};
use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

struct CargoSlot {
    _guard: oms_lock::Guard,
    target_dir: PathBuf,
}

fn acquire_cargo_slot(repo: &Path, provider: &str) -> Option<CargoSlot> {
    if !(repo.join("Cargo.toml").is_file() || repo.join("rust/Cargo.toml").is_file()) {
        return None;
    }
    if std::env::var_os("OMS_DELEGATE_ISOLATED_BUILD").as_deref() == Some(OsStr::new("1"))
        || std::env::var_os("OMS_CARGO_SLOT_CACHE").as_deref() == Some(OsStr::new("0"))
        || std::env::var_os("CARGO_TARGET_DIR").is_some_and(|v| !v.is_empty())
    {
        return None;
    }
    let key = storage::hash(repo.to_string_lossy().as_bytes());
    let base = if provider == "codex" {
        std::env::temp_dir()
            .join(format!(
                "oms-cargo-slots-{}/cargo-slots",
                rustix::process::geteuid().as_raw()
            ))
            .join(&key[..16])
    } else {
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".cache"))
            })?;
        cache_root
            .join("oh-my-setting/cargo-slots")
            .join(&key[..16])
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(&base).ok()?;
    let mut settings = oms_lock::Settings::from_env().ok()?;
    settings.cwd = repo.to_owned();
    for slot in 0..8 {
        let slot_dir = base.join(format!("slot-{slot}"));
        let _ = builder.create(&slot_dir);
        let lock = oms_lock::Lock::for_file(&slot_dir.join("slot.lock"), settings.clone()).ok()?;
        if let Ok(guard) = lock.try_acquire() {
            let target_dir = slot_dir.join("target");
            if builder.create(&target_dir).is_ok() {
                return Some(CargoSlot {
                    _guard: guard,
                    target_dir,
                });
            }
        }
    }
    None
}

pub(crate) struct Worktree {
    pub path: PathBuf,
    pub base: String,
    pub owner: PathBuf,
}
impl Worktree {
    pub(crate) fn create(repo: &Path, base: &str, context: &Context<'_>) -> Result<Self> {
        if base.is_empty() || base.starts_with('-') {
            return fail("invalid worktree base");
        }
        let resolved = process::git(
            context,
            repo,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )?;
        let base = String::from_utf8_lossy(&resolved).trim().to_owned();
        let name = crate::id("peer-")?;
        let directory = Path::new(".oms/tmp").join(&name);
        let owner_path = directory.join("owner.json");
        let _owner_guard = storage::lock(repo, &owner_path, context.interrupted)?;
        storage::write_locked(
            repo,
            &owner_path,
            &_owner_guard,
            json(&obj([
                ("schema", 1usize.into()),
                ("base", base.clone().into()),
                ("id", name.into()),
            ]))?,
            false,
            context.interrupted,
        )?;
        let path = repo.join(directory).join("tree");
        let value = path
            .to_str()
            .ok_or_else(|| Error::new(2, "worktree path is not UTF-8"))?;
        process::git(
            context,
            repo,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                "--detach",
                value,
                &base,
            ],
        )?;
        Ok(Self {
            path,
            base,
            owner: repo.into(),
        })
    }
    pub(crate) fn identity(&self, context: &Context<'_>) -> Result<()> {
        let found = process::git(context, &self.path, &["rev-parse", "--show-toplevel"])?;
        if Path::new(String::from_utf8_lossy(&found).trim()).canonicalize()?
            != self.path.canonicalize()?
        {
            return fail("worker worktree identity changed; preserved for inspection");
        }
        let head = process::git(context, &self.path, &["rev-parse", "HEAD"])?;
        if String::from_utf8_lossy(&head).trim() != self.base {
            return fail("worker changed HEAD; worktree preserved for inspection");
        }
        Ok(())
    }
    pub(crate) fn remove(self, context: &Context<'_>) -> Result<()> {
        self.identity(context)?;
        let path = self
            .path
            .to_str()
            .ok_or_else(|| Error::new(2, "invalid worktree path"))?;
        process::git(
            context,
            &self.owner,
            &["worktree", "remove", "--force", path],
        )?;
        self.reclaim_wrapper();
        Ok(())
    }
    /// Best effort after a clean removal: drop `owner.json` and the emptied
    /// `.oms/tmp/peer-<id>` wrapper using descriptor-anchored `O_NOFOLLOW`
    /// handles. Any symlink or unexpected entry leaves the wrapper untouched.
    fn reclaim_wrapper(&self) {
        use rustix::fs::{
            AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat, unlinkat,
        };
        let Some(wrapper) = self.path.parent() else {
            return;
        };
        let Some(id) = wrapper.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        if !id.starts_with("peer-")
            || wrapper.parent() != Some(self.owner.join(".oms/tmp").as_path())
        {
            return;
        }
        let Ok(oms) = open(
            &self.owner,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .and_then(|root_fd| {
            openat(
                &root_fd,
                ".oms",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
        })
        .and_then(|oms_fd| {
            openat(
                &oms_fd,
                "tmp",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
        }) else {
            return;
        };
        let Ok(before) = statat(&oms, id, AtFlags::SYMLINK_NOFOLLOW) else {
            return;
        };
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
            return;
        }
        let Ok(wrapper_fd) = openat(
            &oms,
            id,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            return;
        };
        let Ok(opened) = fstat(&wrapper_fd) else {
            return;
        };
        if opened.st_dev != before.st_dev || opened.st_ino != before.st_ino {
            return;
        }
        let Ok(stream) = Dir::read_from(&wrapper_fd) else {
            return;
        };
        let mut names = Vec::new();
        for entry in stream {
            let Ok(entry) = entry else { return };
            let name = entry.file_name();
            if [b".".as_slice(), b"..".as_slice()].contains(&name.to_bytes()) {
                continue;
            }
            names.push(name.to_owned());
        }
        let owner_present = names.iter().any(|n| n.as_bytes() == b"owner.json");
        if names.len() > usize::from(owner_present) {
            return;
        }
        if owner_present {
            let Ok(st) = statat(&wrapper_fd, c"owner.json", AtFlags::SYMLINK_NOFOLLOW) else {
                return;
            };
            if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
                || unlinkat(&wrapper_fd, c"owner.json", AtFlags::empty()).is_err()
            {
                return;
            }
        }
        drop(wrapper_fd);
        let _ = unlinkat(&oms, id, AtFlags::REMOVEDIR);
    }
}

pub(crate) fn transition(
    repo: &Path,
    attempt: &str,
    state: &str,
    refs: V,
    context: &Context<'_>,
) -> Result<()> {
    let event = oms_command_operations::new_event(
        attempt,
        0usize.into(),
        "attempt.state_changed",
        obj([
            ("from_state", V::Null),
            ("to_state", state.into()),
            (
                "actor",
                obj([("kind", "harness".into()), ("name", "peer".into())]),
            ),
            ("refs", refs),
        ]),
    )?;
    oms_command_operations::append_lifecycle(repo, event, context.interrupted)?;
    Ok(())
}
#[allow(dead_code)]
pub(crate) fn verify(
    command: &str,
    cwd: &Path,
    context: &Context<'_>,
) -> Result<process::Completed> {
    verify_env(command, cwd, &[], context)
}

pub(crate) fn verify_env(
    command: &str,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    context: &Context<'_>,
) -> Result<process::Completed> {
    process::invoke_env(
        context,
        cwd,
        vec![
            "bash".into(),
            "--noprofile".into(),
            "--norc".into(),
            "-c".into(),
            command.into(),
        ],
        None,
        environment.to_vec(),
        1800,
        true,
    )
}
pub(crate) fn verification(args: &Args, repo: &Path, read: bool) -> Result<Option<String>> {
    if args.has("verify") && args.has("no-verify") {
        return fail("--verify and --no-verify are mutually exclusive");
    }
    if args.has("verify") {
        return Ok(Some(args.required("verify")?.into()));
    }
    if read || args.has("no-verify") {
        return Ok(None);
    }
    let bytes = storage::read(repo, Path::new("scripts/check.sh"), true)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let body = String::from_utf8_lossy(&bytes);
    if body.lines().any(|l| {
        l.trim_start().starts_with("fast)") || l.contains(" fast ") && l.contains("Usage:")
    }) {
        Ok(Some("bash scripts/check.sh fast".into()))
    } else {
        fail("check.sh has no declared fast mode; give --verify CMD or --no-verify")
    }
}
fn build_graph_context(
    repo: &Path,
    question: &str,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let is_valid_helper = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| {
            meta.file_type().is_file()
                && (meta.permissions().mode() & 0o111 != 0)
                && (meta.permissions().mode() & 0o022 == 0)
        })
    };
    let helper = std::env::current_exe().ok().and_then(|exe| {
        let sibling = exe
            .parent()
            .map(|dir| dir.join("oms-graph-python-helper"))
            .filter(|p| is_valid_helper(p));
        sibling.or_else(|| {
            exe.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("oms-graph-python-helper"))
                .filter(|p| is_valid_helper(p))
        })
    });
    let sig_args = |no_refresh: bool| -> Vec<OsString> {
        let mut argv: Vec<OsString> = vec![
            "project".into(),
            "map".into(),
            "--signatures".into(),
            "--task".into(),
            question.into(),
            "--max-bytes".into(),
            "12000".into(),
            "--repo".into(),
            repo.as_os_str().to_owned(),
        ];
        if no_refresh {
            argv.push("--no-refresh".into());
        }
        argv
    };
    let (code, stdout, _) =
        oms_command_graph::run_with_helper(&sig_args(false), repo, interrupted, helper.as_deref());
    if code == 0 && !stdout.is_empty() {
        return String::from_utf8(stdout).ok();
    }
    let build_args: Vec<OsString> = vec![
        "project".into(),
        "build".into(),
        "--exclude".into(),
        "*.py".into(),
        "--repo".into(),
        repo.as_os_str().to_owned(),
    ];
    if oms_command_graph::run_with_helper(&build_args, repo, interrupted, None).0 == 0 {
        let (code, stdout, _) =
            oms_command_graph::run_with_helper(&sig_args(true), repo, interrupted, None);
        if code == 0 && !stdout.is_empty() {
            return String::from_utf8(stdout).ok();
        }
    }
    let brief_args: Vec<OsString> = vec![
        "project".into(),
        "map".into(),
        "--brief".into(),
        "--no-refresh".into(),
        "--repo".into(),
        repo.as_os_str().to_owned(),
    ];
    let (code, stdout, _) =
        oms_command_graph::run_with_helper(&brief_args, repo, interrupted, None);
    if code == 0 && !stdout.is_empty() {
        String::from_utf8(stdout).ok()
    } else {
        None
    }
}

fn interactive_turns_from_stdin() -> Result<Vec<String>> {
    use std::io::Read;
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .map_err(|e| Error::new(2, format!("failed reading interactive turns: {e}")))?;
    let mut turns = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('{')
            && let Ok(v) = V::parse(trimmed.as_bytes())
        {
            if v.get("action").text() == Some("finish") {
                break;
            }
            if let Some(prompt) = v.get("prompt").text() {
                if !prompt.trim().is_empty() {
                    turns.push(prompt.trim().to_owned());
                }
                continue;
            }
        }
        turns.push(trimmed.to_owned());
    }
    Ok(turns)
}

fn brief_label(question: &str) -> String {
    for raw in question.lines() {
        let trimmed = raw
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_start_matches("Goal:")
            .trim();
        if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("goal:") {
            let clean: String = trimmed
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect();
            if !clean.is_empty() {
                return clean;
            }
        }
    }
    "peer-delegate".into()
}

pub(crate) fn run(args: &Args, context: &Context<'_>) -> Result<Output> {
    safety::parent()?;
    let interactive = args.has("interactive");
    if args.has("plan-task")
        || args.has("observe-plan-binding")
        || args.has("terminalize-resumed-repair")
    {
        return fail("plan lease integration must be supplied by the owning native plan adapter");
    }
    if args.has("apply") {
        return fail(
            "automatic apply requires the native patch-admission service; no worker started",
        );
    }
    if args.has("resume-session") {
        return fail("native session relocation must be bound before resuming a delegation");
    }
    if args.has("context-manifest") {
        return fail("typed context compilation is not integrated; no worktree created");
    }
    let orientation = if args.has("context-pack") {
        Some(crate::context_pack::orientation(
            &args.path("context-pack", context.cwd),
        )?)
    } else {
        None
    };
    let target = Target::parse(args.required("to")?)?;
    let question = peer::question(args, context)?;
    let repo = storage::repository(context, &args.path("repo", context.cwd))?;
    let directory = peer::directory(args, &repo, "delegate")?;
    let mut read = args.has("read-only");
    if args.has("role")
        && !read
        && std::env::var_os("OMS_DELEGATE_ROLE_WRITE").is_none_or(|v| v != "1")
    {
        let policy = providers::config(context, "role-contracts.json")?;
        read = policy
            .get("roles")
            .get(args.get("role"))
            .get("read_only")
            .truth();
    }
    let command = verification(args, &repo, read)?;
    let repair = if args.has("repair") {
        args.limit("repair", 0, 1, 3)?
    } else {
        0
    };
    if !["standard", "routine"].contains(&args.value("workload", "standard")) {
        return fail("--workload requires standard or routine");
    }
    let scope = args
        .all("scope")
        .into_iter()
        .map(|s| oms_command_room::normalize_scope(&s).map_err(|e| Error::new(2, e.to_string())))
        .collect::<Result<Vec<_>>>()?;
    for pattern in &scope {
        oms_contract_markdown::scope::validate_pattern(pattern)
            .map_err(|e| Error::new(2, e.to_string()))?;
    }
    let brief_issues = lint(&question);
    if args.has("strict-brief") && !brief_issues.is_empty() {
        return fail(format!("brief lacks {}", brief_issues.join(", ")));
    }
    let role = if read { "researcher" } else { "worker" };
    let mut route = providers::route(
        context,
        &target,
        args.get("model"),
        args.value("reasoning-effort", "auto"),
        role,
    )?;
    route.explicit = true;
    let run = peer::run_id()?;
    let artifact = directory.join(format!("{}-delegate-{run}.md", target.provider));
    let patch = directory.join(format!("{}-delegate-{run}.patch", target.provider));
    let base = String::from_utf8_lossy(&process::git(context, &repo, &["rev-parse", "HEAD"])?)
        .trim()
        .to_owned();
    let mut prompt = format!(
        "You are a delegated worker in an isolated repository worktree. Follow repository contracts. Do not run git commit or git push. Do not recursively delegate. The owner retains review, admission and publication.\nGoal:\n{question}\n\nOutput: return patch artifacts and concise changed files, checks, uncertainty and blockers.\nDone when: the authorized task and its required verification are complete, or no work can advance without the owner.\nBase revision: {base}\n"
    );
    if args.has("task") && !args.has("no-task") || !read && !args.has("no-task") {
        let packet = storage::read(&repo, Path::new(".oms/task/current.md"), true)?;
        if !packet.is_empty() {
            prompt.push_str(&safety::untrusted("task", "recorded context", &packet)?);
        }
    }
    if args.has("memory") && !args.has("no-memory") {
        let (code, bytes, _) =
            oms_command_memory::provider_context(&repo, &question, context.interrupted);
        if code != 0 {
            return fail("shared memory context failed");
        }
        prompt.push_str(&safety::untrusted("memory", "recorded context", &bytes)?);
    }
    if args.has("ml-context") && !args.has("no-ml-context") {
        let digest = crate::ml_context::build(&repo, context)?;
        if !digest.is_empty() {
            prompt.push_str(&safety::untrusted(
                "ML project",
                "context digest",
                digest.as_bytes(),
            )?);
        }
    }
    if let Some(orientation) = orientation {
        prompt.push_str(&safety::untrusted(
            "parent graph",
            "orientation metadata",
            orientation.as_bytes(),
        )?);
    } else if args.has("graph-context")
        && !args.has("no-graph-context")
        && !args.has("context-pack")
        && let Some(graph_text) = build_graph_context(&repo, &question, context.interrupted)
    {
        prompt.push_str(&safety::untrusted(
            "project graph",
            "task signatures",
            graph_text.as_bytes(),
        )?);
    }
    if read {
        prompt.push_str(
            "Read-only investigation: do not modify files. Return evidence and findings.\n",
        );
    }
    if !scope.is_empty() {
        prompt.push_str(&format!("Allowed paths: {}\n", scope.join(", ")));
    }
    if args.has("role") {
        let (code, bytes, _) = oms_command_agent_role::run_with_source(
            &[
                "show".into(),
                "--name".into(),
                args.get("role").into(),
                "--repo".into(),
                repo.as_os_str().into(),
            ],
            &repo,
            context.source,
            context.interrupted,
        );
        if code != 0 {
            return fail("role profile could not be resolved");
        }
        prompt.push_str(&String::from_utf8_lossy(&bytes));
    }
    if args.has("review-artifact") {
        prompt.push_str(&safety::untrusted(
            "reviewer",
            "review claims",
            &storage::file(&args.path("review-artifact", context.cwd))?,
        )?);
    }
    safety::outbound(prompt.as_bytes())?;
    let header = format!(
        "# OMS peer-delegate\n\n- provider: {}\n- base_sha: {base}\n- access: {}\n\n## Prompt\n\n{prompt}\n",
        target.provider,
        if read { "read" } else { "write" }
    );
    storage::write(
        &repo,
        &artifact,
        header.as_bytes().to_vec(),
        false,
        context.interrupted,
    )?;
    storage::write(&repo, &patch, Vec::new(), false, context.interrupted)?;
    if peer::dry(args, "peer-delegate") || args.has("export-only") {
        let advice = obj([
            ("advisory", true.into()),
            (
                "verdict",
                if read {
                    "researcher"
                } else if question.len() < 600 {
                    "inline"
                } else if !scope.is_empty() && command.is_some() {
                    "worker"
                } else {
                    "researcher"
                }
                .into(),
            ),
            (
                "facts",
                obj([
                    ("brief_bytes", V::Num(question.len().to_string())),
                    ("has_check", command.is_some().into()),
                ]),
            ),
        ]);
        let mut output = format!(
            "{}\n{}\n",
            repo.join(&artifact).display(),
            repo.join(&patch).display()
        );
        if args.has("json") {
            output.push_str(&format!(
                "advice-json: {}\n",
                String::from_utf8_lossy(&advice.json(false)?)
            ));
        }
        return Ok(out(output.into_bytes()));
    }
    if interactive && std::io::stdin().is_terminal() {
        return fail("--interactive requires NDJSON turn input on stdin; no worktree created");
    }
    let interactive_turns = if interactive {
        let turns = interactive_turns_from_stdin()?;
        if turns.is_empty() {
            vec![String::new()]
        } else {
            turns
        }
    } else {
        Vec::new()
    };
    let worktree = Worktree::create(&repo, &base, context)?;
    let mut attempt_refs = obj([
        ("artifact", artifact.to_string_lossy().into_owned().into()),
        ("patch", patch.to_string_lossy().into_owned().into()),
        (
            "worktree",
            worktree
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
                .into(),
        ),
    ]);
    let mut parent_attempt_id = String::new();
    if std::env::var_os("OMS_PANEL_DISPATCH").as_deref() != Some(OsStr::new("1")) {
        attempt_refs.set(
            "panel_alias",
            V::from(format!("{}-delegate", target.provider)),
        );
        attempt_refs.set(
            "panel_role",
            V::from(if read { "researcher" } else { "delegate" }),
        );
        attempt_refs.set("panel_model", V::from(route.model.as_str()));
        attempt_refs.set("panel_label", V::from(brief_label(&question)));
        if let Ok(room_id) = std::env::var("OMS_ROOM_ID")
            && !room_id.trim().is_empty()
        {
            attempt_refs.set("panel_room_id", V::from(room_id.trim()));
        }
        if let Ok(main_id) = std::env::var("OMS_PANEL_MAIN_ATTEMPT")
            && !main_id.trim().is_empty()
        {
            parent_attempt_id = main_id.trim().to_owned();
        }
    }
    let attempt = oms_command_operations::create_attempt(
        &repo,
        &oms_command_operations::StartRequest {
            provider: target.provider.clone(),
            tool: "peer-delegate".into(),
            task_id: args.get("task-id").into(),
            parent_attempt_id,
            refs: attempt_refs,
            ..Default::default()
        },
        context.interrupted,
    )?;
    transition(&repo, &attempt, "starting", obj([]), context)?;
    transition(&repo, &attempt, "working", obj([]), context)?;
    let mut code = 1;
    let mut body = header.into_bytes();
    let mut captured = Vec::new();
    let mut verified = "not_run";
    let mut needs_keep = args.has("keep-worktree");
    let mut interactive_stdout = String::new();
    let cargo_slot = acquire_cargo_slot(&repo, &target.provider);
    let extra_env: Vec<(OsString, OsString)> = cargo_slot
        .as_ref()
        .map(|slot| {
            vec![(
                "CARGO_TARGET_DIR".into(),
                slot.target_dir.as_os_str().to_owned(),
            )]
        })
        .unwrap_or_default();
    for round in 0..=repair {
        crate::check(context.interrupted)?;
        worktree.identity(context)?;
        if interactive && round == 0 {
            let mut turn_history = String::new();
            let timeout = peer::timeout(args, "peer-delegate")?;
            code = 0;
            for (idx, turn_prompt) in interactive_turns.iter().enumerate() {
                crate::check(context.interrupted)?;
                worktree.identity(context)?;
                let mut full_prompt = prompt.clone();
                if !turn_history.is_empty() {
                    full_prompt.push_str("\n## Prior Interactive Turns\n");
                    full_prompt.push_str(&turn_history);
                }
                if !turn_prompt.is_empty() {
                    full_prompt.push_str(&format!("\n## Turn {} Input\n{turn_prompt}\n", idx + 1));
                }
                safety::outbound(full_prompt.as_bytes())?;
                let res = providers::execute_with_fallback_env(
                    &mut route,
                    args.get("fallback-model"),
                    full_prompt.as_bytes(),
                    &worktree.path,
                    !read,
                    timeout,
                    &repo,
                    &extra_env,
                    context,
                );
                let res = match res {
                    Ok(r) => r,
                    Err(error) => {
                        code = error.code;
                        body.extend_from_slice(
                            format!("\n## Turn {} Error\n\n{}\n", idx + 1, error.message)
                                .as_bytes(),
                        );
                        let line = obj([
                            ("turn", V::Num((idx + 1).to_string())),
                            ("code", V::Num(code.to_string())),
                            ("complete", false.into()),
                            ("error", error.message.into()),
                        ])
                        .json(false)?;
                        interactive_stdout.push_str(&String::from_utf8_lossy(&line));
                        interactive_stdout.push('\n');
                        break;
                    }
                };
                let turn_code = if !res.complete && res.code == 0 {
                    1
                } else {
                    res.code
                };
                let clean = safety::redact(&res.stdout)?;
                let clean_str = String::from_utf8_lossy(&clean).into_owned();
                body.extend_from_slice(
                    format!("\n## Turn {}\n\n{clean_str}\n", idx + 1).as_bytes(),
                );
                turn_history.push_str(&format!(
                    "\nTurn {}: {}\nReply: {}\n",
                    idx + 1,
                    turn_prompt,
                    clean_str.chars().take(4000).collect::<String>()
                ));
                if turn_history.len() > 24 * 1024 {
                    let drop_bytes = turn_history.len() - 24 * 1024;
                    let boundary = turn_history
                        .char_indices()
                        .find(|(pos, _)| *pos >= drop_bytes)
                        .map_or(turn_history.len(), |(pos, _)| pos);
                    turn_history.drain(..boundary);
                }
                let line = obj([
                    ("turn", V::Num((idx + 1).to_string())),
                    ("code", V::Num(turn_code.to_string())),
                    ("complete", res.complete.into()),
                    ("output", clean_str.into()),
                ])
                .json(false)?;
                interactive_stdout.push_str(&String::from_utf8_lossy(&line));
                interactive_stdout.push('\n');
                code = turn_code;
                if turn_code != 0 {
                    break;
                }
            }
        } else {
            let result = providers::execute_with_fallback_env(
                &mut route,
                args.get("fallback-model"),
                prompt.as_bytes(),
                &worktree.path,
                !read,
                peer::timeout(args, "peer-delegate")?,
                &repo,
                &extra_env,
                context,
            );
            let result = match result {
                Ok(r) => r,
                Err(error) => {
                    code = error.code;
                    body.extend_from_slice(
                        format!("\n## Output\n\n{}\n", error.message).as_bytes(),
                    );
                    break;
                }
            };
            code = result.code;
            if !result.complete && code == 0 {
                code = 1;
            }
            body.extend_from_slice(
                format!(
                    "\n## Output\n\n{}\n",
                    String::from_utf8_lossy(&safety::redact(&result.stdout)?)
                )
                .as_bytes(),
            );
        }
        worktree.identity(context)?;
        process::git(
            context,
            &worktree.path,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "add",
                "-A",
                "--",
            ],
        )?;
        let names = process::git(
            context,
            &worktree.path,
            &["diff", "--cached", "--name-only", "-z", &base, "--"],
        )?;
        for name in names.split(|b| *b == 0).filter(|s| !s.is_empty()) {
            let name = std::str::from_utf8(name)
                .map_err(|_| Error::new(2, "changed path is not UTF-8"))?;
            if name.starts_with(".git/")
                || name == ".git"
                || name.starts_with(".oms/")
                || name == ".oms"
                || !scope.is_empty()
                    && !scope
                        .iter()
                        .any(|s| oms_contract_markdown::scope::matches(name, s).unwrap_or(false))
            {
                needs_keep = true;
                code = 1;
                body.extend_from_slice(
                    b"\nWorker changed a protected or out-of-scope path; patch withheld.\n",
                );
                break;
            }
        }
        if code == 0 || !names.is_empty() {
            captured = process::git(
                context,
                &worktree.path,
                &[
                    "-c",
                    "diff.external=",
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--binary",
                    "--src-prefix=a/",
                    "--dst-prefix=b/",
                    "--cached",
                    &base,
                    "--",
                ],
            )?;
        }
        if read && !captured.is_empty() {
            code = 1;
            needs_keep = true;
            body.extend_from_slice(b"\nRead-only worker changed files; worktree retained.\n");
        }
        storage::write(
            &repo,
            &patch,
            if needs_keep && code != 0 {
                Vec::new()
            } else {
                captured.clone()
            },
            true,
            context.interrupted,
        )?;
        if code == 0
            && !captured.is_empty()
            && let Some(command) = &command
        {
            transition(&repo, &attempt, "verifying", obj([]), context)?;
            let checked = verify_env(command, &worktree.path, &extra_env, context)?;
            code = checked.code;
            if !checked.complete && code == 0 {
                code = 1;
            }
            let boundary = worktree.identity(context).is_ok()
                && process::git(
                    context,
                    &worktree.path,
                    &[
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--binary",
                        "--src-prefix=a/",
                        "--dst-prefix=b/",
                        &base,
                        "--",
                    ],
                )
                .is_ok_and(|after| after == captured);
            if !boundary {
                code = 125;
                needs_keep = true;
                body.extend_from_slice(b"\nVerifier changed the pinned tracked state; worktree retained and success withheld.\n");
            }
            verified = if code == 0 { "passed" } else { "failed" };
            let clean = safety::redact(&checked.stdout)?;
            body.extend_from_slice(
                format!(
                    "\n## Verification\n\n{}\n\nexit: {code}\n",
                    String::from_utf8_lossy(&clean)
                )
                .as_bytes(),
            );
            if code != 0 {
                prompt.push_str(&safety::untrusted(
                    "verifier",
                    "verification output",
                    &clean,
                )?);
            }
            if code != 0 && round < repair {
                transition(&repo, &attempt, "working", obj([]), context)?;
            }
        }
        storage::write(&repo, &artifact, body.clone(), true, context.interrupted)?;
        if code == 0 || matches!(code, 3 | 126 | 127 | 130) || needs_keep {
            break;
        }
        prompt.push_str(
            "\nRepair the rejected changes and verification failure within the original scope.\n",
        );
    }
    let no_change = !read && captured.is_empty();
    if no_change
        && !args.has("allow-no-change")
        && std::env::var_os("OMS_PANEL_DISPATCH").is_some_and(|v| v == "1")
    {
        code = 6;
    }
    let state = if code == 0 {
        "review"
    } else if code == 124 {
        "timed_out"
    } else if code == 130 {
        "cancelled"
    } else {
        "failed"
    };
    let refs = obj([
        ("artifact", artifact.to_string_lossy().into_owned().into()),
        ("patch", patch.to_string_lossy().into_owned().into()),
        ("verify_status", verified.into()),
        ("admit", "pending".into()),
    ]);
    transition(&repo, &attempt, state, refs, context)?;
    body.extend_from_slice(format!("\n## Exit\n\n{code}\n\n- outcome: {}\n- verification: {verified}\n- admission: pending\n",if no_change{"no_change"}else if code==0{"review"}else{"failed"}).as_bytes());
    storage::write(&repo, &artifact, body, true, context.interrupted)?;
    peer::index(
        &repo,
        &obj([
            ("schema", 1usize.into()),
            ("kind", "delegate".into()),
            ("ts", now().into()),
            ("artifact_id", crate::id("art_")?.into()),
            ("attempt_id", attempt.into()),
            ("provider", target.provider.into()),
            ("model", route.model.into()),
            ("artifact", artifact.to_string_lossy().into_owned().into()),
            ("patch", patch.to_string_lossy().into_owned().into()),
            ("base_sha", base.into()),
            ("exit", V::Num(code.to_string())),
            ("verification", verified.into()),
            ("state", state.into()),
            ("admit", "pending".into()),
            ("no_change", no_change.into()),
        ]),
        context,
    )?;
    let mut note = String::new();
    if needs_keep {
        note.push_str(&format!("worktree retained: {}\n", worktree.path.display()));
    } else if let Err(e) = worktree.remove(context) {
        note.push_str(&format!("worktree cleanup deferred: {}\n", e.message));
    }
    Ok((
        code,
        format!(
            "{interactive_stdout}{}\n{}\n",
            repo.join(artifact).display(),
            repo.join(patch).display()
        )
        .into_bytes(),
        note.into_bytes(),
    ))
}
fn lint(body: &str) -> Vec<&'static str> {
    if body.len() < 240 {
        return Vec::new();
    }
    let lower = body.to_lowercase();
    let mut issues = Vec::new();
    for (label, words) in [
        ("goal", &["goal:", "objective:", "purpose:"][..]),
        ("scope", &["scope:", "constraints:", "out of scope:"][..]),
        ("output", &["output:", "return ", "deliverable:"][..]),
        (
            "stop condition",
            &["done when:", "stop if:", "success criteria:"][..],
        ),
    ] {
        if !words.iter().any(|w| lower.contains(w)) {
            issues.push(label);
        }
    }
    if !body.contains('/') && !cached_regex!(r"\b[\w-]+\.(rs|py|sh|md|json|toml)\b").is_match(body)
    {
        issues.push("file pointers");
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn git(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oms-peer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        dir
    }

    #[test]
    fn remove_reclaims_wrapper_only_after_clean_removal() {
        let repo = repo("wrapper");
        let stop = AtomicBool::new(false);
        let native = process::Native;
        let context = Context {
            cwd: &repo,
            source: &repo,
            interrupted: &stop,
            executor: &native,
        };
        let tree = Worktree::create(&repo, "HEAD", &context).unwrap();
        let wrapper = tree.path.parent().unwrap().to_owned();
        assert!(wrapper.join("owner.json").is_file());
        // A changed HEAD preserves everything, wrapper included.
        git(
            &tree.path,
            &["commit", "-q", "--allow-empty", "-m", "drift"],
        );
        let drifted = Worktree {
            path: tree.path.clone(),
            base: tree.base.clone(),
            owner: tree.owner.clone(),
        };
        assert!(drifted.remove(&context).is_err());
        assert!(tree.path.is_dir() && wrapper.join("owner.json").is_file());
        git(&tree.path, &["reset", "-q", "--hard", &tree.base]);
        tree.remove(&context).unwrap();
        assert!(!wrapper.exists(), "wrapper dir reclaimed");
        assert!(repo.join(".oms/tmp").is_dir());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn remove_leaves_wrapper_with_unexpected_entries_untouched() {
        struct LockProbingExecutor {
            repo: PathBuf,
            saw_busy_during_worktree_add: AtomicBool,
        }
        impl process::Executor for LockProbingExecutor {
            fn execute(
                &self,
                request: process::Invocation,
                interrupted: &AtomicBool,
            ) -> Result<process::Completed> {
                let words: Vec<String> = request
                    .argv
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                if words.windows(2).any(|w| w == ["worktree", "add"])
                    && let Some(tree_arg) = words.iter().find(|w| w.ends_with("/tree"))
                {
                    let owner = Path::new(tree_arg).parent().unwrap().join("owner.json");
                    assert!(owner.is_file(), "owner.json created under held lock");
                    let mut settings = oms_lock::Settings::from_env().unwrap();
                    settings.cwd = self.repo.clone();
                    let lock = oms_lock::Lock::for_file(&owner, settings).unwrap();
                    assert!(
                        matches!(lock.try_acquire(), Err(oms_lock::Error::Busy)),
                        "owner.json lock must remain held across git worktree add"
                    );
                    self.saw_busy_during_worktree_add
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                process::Native.execute(request, interrupted)
            }
        }

        let repo = repo("extra");
        let stop = AtomicBool::new(false);
        let probing = LockProbingExecutor {
            repo: repo.clone(),
            saw_busy_during_worktree_add: AtomicBool::new(false),
        };
        let context = Context {
            cwd: &repo,
            source: &repo,
            interrupted: &stop,
            executor: &probing,
        };
        let tree = Worktree::create(&repo, "HEAD", &context).unwrap();
        assert!(
            probing
                .saw_busy_during_worktree_add
                .load(std::sync::atomic::Ordering::Acquire)
        );
        let wrapper = tree.path.parent().unwrap().to_owned();
        std::os::unix::fs::symlink(&repo, wrapper.join("link")).unwrap();
        tree.remove(&context).unwrap();
        assert!(wrapper.join("owner.json").is_file(), "owner.json kept");
        assert!(wrapper.join("link").exists());

        // Swapping owner.json with a symlink must leave the target untouched.
        let victim = repo.join("victim.txt");
        std::fs::write(&victim, b"preserve").unwrap();
        let sym_tree = Worktree::create(&repo, "HEAD", &context).unwrap();
        let sym_wrapper = sym_tree.path.parent().unwrap().to_owned();
        std::fs::remove_file(sym_wrapper.join("owner.json")).unwrap();
        std::os::unix::fs::symlink(&victim, sym_wrapper.join("owner.json")).unwrap();
        sym_tree.remove(&context).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"preserve");
        assert!(sym_wrapper.join("owner.json").is_symlink());
        let _ = std::fs::remove_dir_all(&repo);
    }
}
