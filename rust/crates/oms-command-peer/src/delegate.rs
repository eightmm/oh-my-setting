use crate::{
    Context, Error, Output, Result, V,
    cli::Args,
    fail, json, now, obj, out, peer, process,
    providers::{self, Target},
    safety, storage,
};
use std::path::{Path, PathBuf};

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
        storage::write(
            repo,
            &directory.join("owner.json"),
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
        Ok(())
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
pub(crate) fn verify(
    command: &str,
    cwd: &Path,
    context: &Context<'_>,
) -> Result<process::Completed> {
    process::invoke(
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
pub(crate) fn run(args: &Args, context: &Context<'_>) -> Result<Output> {
    safety::parent()?;
    if args.has("interactive") {
        return fail("interactive control requires a streaming input adapter; no worktree created");
    }
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
    if args.has("context-manifest")
        || args.has("context-pack")
        || args.has("graph-context") && !args.has("no-graph-context")
    {
        return fail("typed context compilation is not integrated; no worktree created");
    }
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
    let worktree = Worktree::create(&repo, &base, context)?;
    let attempt = oms_command_operations::create_attempt(
        &repo,
        &oms_command_operations::StartRequest {
            provider: target.provider.clone(),
            tool: "peer-delegate".into(),
            task_id: args.get("task-id").into(),
            refs: obj([
                ("artifact", artifact.to_string_lossy().into_owned().into()),
                ("patch", patch.to_string_lossy().into_owned().into()),
                (
                    "worktree",
                    worktree.path.to_string_lossy().into_owned().into(),
                ),
            ]),
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
    for round in 0..=repair {
        crate::check(context.interrupted)?;
        worktree.identity(context)?;
        let result = providers::execute_with_fallback(
            &mut route,
            args.get("fallback-model"),
            prompt.as_bytes(),
            &worktree.path,
            !read,
            peer::timeout(args, "peer-delegate")?,
            context,
        );
        let result = match result {
            Ok(r) => r,
            Err(error) => {
                code = error.code;
                body.extend_from_slice(format!("\n## Output\n\n{}\n", error.message).as_bytes());
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
            let checked = verify(command, &worktree.path, context)?;
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
            "{}\n{}\n",
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
    if !body.contains('/')
        && !regex::Regex::new(r"\b[\w-]+\.(rs|py|sh|md|json|toml)\b")
            .unwrap()
            .is_match(body)
    {
        issues.push("file pointers");
    }
    issues
}
