use crate::common::*;
use crate::skills;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new(label: &str) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        for _ in 0..64 {
            let path =
                std::env::temp_dir().join(format!("oms-{label}-{}", crate::https::random_hex(16)?));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(error("cannot allocate private temporary directory"))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn sha(raw: &str) -> Result<&str> {
    if raw.len() != 64
        || !raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Err(error("expected a lowercase SHA-256 digest"))
    } else {
        Ok(raw)
    }
}
pub(crate) fn load(root: &Path, skill: &str) -> Result<Value> {
    skills::name(skill, true)?;
    let row = json(&read(
        &root.join(".oms/skill-store").join(skill).join("lock.json"),
        1024 * 1024,
    )?)?;
    if row["schema"] != 1 || row["name"] != skill || !row["provenance"].is_object() {
        return Err(error("skill lock has an invalid schema"));
    }
    let expected = sha(row["bundle_sha256"]
        .as_str()
        .ok_or_else(|| error("invalid stored bundle digest"))?)?;
    let relative = safe_relative(
        row["revision_path"]
            .as_str()
            .ok_or_else(|| error("invalid stored revision path"))?,
    )?;
    let prefix = PathBuf::from(".oms/skill-store")
        .join(skill)
        .join("revisions");
    if !relative.starts_with(prefix) {
        return Err(error("skill lock revision escapes immutable store"));
    }
    no_links(root, relative)?;
    let files = skills::entries(&root.join(relative))?;
    if skills::bundle_digest(&files) != expected {
        return Err(error("skill lock revision digest does not match"));
    }
    Ok(row)
}
fn git(arguments: &[&str], cwd: &Path, ctx: &Context<'_>) -> Result<String> {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(arguments)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let result = capture(command, ctx.interrupted, 120)?;
    if result.code != 0 || result.out.len() > 65536 {
        return Err(error(
            "git source probe failed or exceeded its output budget",
        ));
    }
    String::from_utf8(result.out)
        .map(|s| s.trim().to_owned())
        .map_err(|_| error("git returned non-UTF-8 output"))
}
fn emit(value: Value, args: &Args) -> Output {
    if args.has("--json") {
        Output::json(value)
    } else {
        Output::text(
            value
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| format!("{k}: {v}\n"))
                .collect::<String>(),
        )
    }
}
pub(crate) fn run(command: &str, args: &Args, root: &Path, ctx: &Context<'_>) -> Result<Output> {
    let store_root = Store::open(root)?;
    if command == "outdated" {
        return outdated(args, root, ctx);
    }
    if command == "eval" {
        return evaluate(args, root, ctx);
    }
    if command == "rollback" {
        args.count(2)?;
        let skill = skills::name(
            args.positional
                .get(1)
                .ok_or_else(|| error("rollback requires a name"))?,
            false,
        )?;
        let target = sha(args.required("--to")?)?;
        let expected = sha(args.required("--expected-current-sha256")?)?;
        let _guard = lock(&root.join(".oms/skill-store/.authority"))?;
        skills::preflight(root, skill)?;
        let mut current = load(root, skill)?;
        if current["bundle_sha256"] != expected {
            return Err(error("current bundle digest changed before rollback"));
        }
        if !args.has("--apply") {
            return Ok(emit(
                json!({"schema":1,"status":"preview","name":skill,"from":current["bundle_sha256"],"to":target}),
                args,
            ));
        }
        let revision = PathBuf::from(".oms/skill-store")
            .join(skill)
            .join("revisions")
            .join(target)
            .join(skill);
        no_links(root, &revision)?;
        if skills::bundle_digest(&skills::entries(&root.join(&revision))?) != target {
            return Err(error(
                "requested rollback revision failed digest verification",
            ));
        }
        current["bundle_sha256"] = json!(target);
        current["revision_path"] = json!(revision);
        current["previous_sha256"] = json!(expected);
        current["updated_at"] = json!(timestamp());
        store_root.write(
            &root.join(".oms/skill-store").join(skill).join("lock.json"),
            &json_bytes(&current)?,
            false,
        )?;
        skills::link(root, ctx)?;
        return Ok(emit(current, args));
    }
    args.count(1)?;
    let source = args.required("--source")?;
    if source.len() > 2048
        || source.starts_with('-')
        || (source.contains("://")
            && source
                .split("://")
                .nth(1)
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("")
                .contains('@'))
    {
        return Err(error("unsafe git source locator"));
    }
    let reference = args.get("--ref").unwrap_or("");
    if reference.starts_with('-') {
        return Err(error("invalid git reference"));
    }
    let local = ctx.path(source);
    let mut scratch = None;
    let (base, provenance) = if local.is_dir() && reference.is_empty() {
        if std::fs::symlink_metadata(&local)?.file_type().is_symlink() {
            return Err(error("bundle source symlinks are refused"));
        }
        let physical = local.canonicalize()?;
        let revision = git(&["rev-parse", "HEAD"], &physical, ctx).unwrap_or_default();
        let provenance = json!({"kind":"local","locator":format!("local-sha256:{}",digest(physical.as_os_str().as_encoded_bytes())),"resolved_revision":revision});
        (physical, provenance)
    } else {
        let directory = Scratch::new("skill-quarantine")?;
        let clone = directory.0.join("source");
        let clone_str = clone
            .to_str()
            .ok_or_else(|| error("temporary path must be UTF-8"))?;
        git(
            &["clone", "--no-checkout", "--", source, clone_str],
            ctx.cwd,
            ctx,
        )?;
        git(
            &[
                "checkout",
                "--detach",
                if reference.is_empty() {
                    "HEAD"
                } else {
                    reference
                },
            ],
            &clone,
            ctx,
        )?;
        let revision = git(&["rev-parse", "HEAD"], &clone, ctx)?;
        let provenance = json!({"kind":"git","locator":source,"requested_ref":reference,"resolved_revision":revision});
        scratch = Some(directory);
        (clone, provenance)
    };
    let subdir = args.get("--subdir").unwrap_or("");
    let base = if subdir.is_empty() {
        base
    } else {
        let relative = safe_relative(subdir)?;
        no_links(&base, relative)?;
        base.join(relative)
    };
    let files = skills::entries(&base)?;
    let body = files
        .iter()
        .find(|(p, _, _)| p == "SKILL.md")
        .map(|(_, b, _)| b)
        .ok_or_else(|| error("bundle is missing SKILL.md"))?;
    let meta = skills::frontmatter(
        std::str::from_utf8(body).map_err(|_| error("SKILL.md is not UTF-8"))?,
    )?;
    let skill = meta
        .get("name")
        .ok_or_else(|| error("skill name is missing"))?;
    skills::name(skill, true)?;
    skills::validate(body, skill, true)?;
    let digest = skills::bundle_digest(&files);
    let report = json!({"schema":1,"status":"preview","name":skill,"description":meta.get("description"),"bundle_sha256":digest,"files":files.len(),"bytes":files.iter().map(|(_,b,_)|b.len()).sum::<usize>(),"provenance":provenance,"risk_summary":skills::risk(&files)});
    if command == "preview" || !args.has("--apply") {
        return Ok(emit(report, args));
    }
    if sha(args.required("--expected-bundle-sha256")?)? != digest {
        return Err(error("bundle bytes changed after preview"));
    }
    let _guard = lock(&root.join(".oms/skill-store/.authority"))?;
    skills::preflight(root, skill)?;
    if root.join(".oms/skills").join(skill).exists() {
        return Err(error(
            "an active local project skill already uses this name",
        ));
    }
    let store = root.join(".oms/skill-store").join(skill);
    let old = optional(&store.join("lock.json"))?;
    let current = if old.is_empty() {
        None
    } else {
        Some(load(root, skill)?)
    };
    if command == "import" && current.is_some() {
        return Err(error("skill is already imported; use update"));
    }
    if command == "update" && current.is_none() {
        return Err(error("skill is not imported; use import"));
    }
    if command == "update"
        && current
            .as_ref()
            .map(|v| v["bundle_sha256"].as_str().unwrap_or(""))
            != Some(sha(args.required("--expected-current-sha256")?)?)
    {
        return Err(error("current bundle digest changed before update"));
    }
    let relative = PathBuf::from(".oms/skill-store")
        .join(skill)
        .join("revisions")
        .join(&digest)
        .join(skill);
    no_links(root, &relative)?;
    let destination = root.join(&relative);
    if destination.exists() {
        if skills::bundle_digest(&skills::entries(&destination)?) != digest {
            return Err(error("immutable revision occupant differs"));
        }
    } else {
        let revision = destination
            .parent()
            .ok_or_else(|| error("missing revision parent"))?;
        let parent = revision
            .parent()
            .ok_or_else(|| error("missing revisions directory"))?;
        std::fs::create_dir_all(parent)?;
        let quarantine = parent.join(format!(".{}", crate::https::random_hex(16)?));
        std::fs::create_dir(&quarantine)?;
        let temporary = quarantine.join(skill);
        let copy_result = (|| {
            use std::os::unix::fs::PermissionsExt;
            for (name, bytes, executable) in &files {
                let path = temporary.join(name);
                store_root.write(&path, bytes, true)?;
                std::fs::set_permissions(
                    &path,
                    std::fs::Permissions::from_mode(if *executable { 0o755 } else { 0o644 }),
                )?;
            }
            if skills::bundle_digest(&skills::entries(&temporary)?) != digest {
                return Err(error("copied bundle digest changed in quarantine"));
            }
            std::fs::rename(&quarantine, revision)?;
            Ok(())
        })();
        if quarantine.exists() {
            let _ = std::fs::remove_dir_all(&quarantine);
        }
        copy_result?;
    }
    let mut row = json!({"schema":1,"name":skill,"bundle_sha256":digest,"revision_path":relative,"provenance":provenance,"updated_at":timestamp()});
    if let Some(current) = current
        && current["bundle_sha256"] != row["bundle_sha256"]
    {
        row["previous_sha256"] = current["bundle_sha256"].clone();
    }
    store_root.write(&store.join("lock.json"), &json_bytes(&row)?, false)?;
    skills::link(root, ctx)?;
    drop(scratch);
    Ok(emit(row, args))
}
fn outdated(args: &Args, root: &Path, ctx: &Context<'_>) -> Result<Output> {
    args.count(1)?;
    let mut bundles = vec![];
    let store = root.join(".oms/skill-store");
    if store.is_dir() {
        for entry in std::fs::read_dir(store)?.take(50) {
            let entry = entry?;
            let skill = entry.file_name().to_string_lossy().into_owned();
            if skills::name(&skill, false).is_err() {
                continue;
            }
            let mut row = json!({"name":skill,"status":"unknown"});
            if let Ok(current) = load(root, &skill) {
                let p = &current["provenance"];
                if p["kind"] == "git" {
                    let reference = p["requested_ref"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("HEAD");
                    let source = p["locator"].as_str().unwrap_or("");
                    if !source.is_empty() && !source.starts_with('-') && !reference.starts_with('-')
                    {
                        if let Ok(result) =
                            git(&["ls-remote", "--", source, reference], ctx.cwd, ctx)
                            && let Some(revision) = result.split_whitespace().next()
                        {
                            row["recorded"] = p["resolved_revision"].clone();
                            row["remote"] = json!(revision);
                            row["status"] = json!(if p["resolved_revision"] == revision {
                                "current"
                            } else {
                                "moved"
                            });
                        }
                    }
                } else {
                    row["reason"] =
                        json!("imported from a local directory; no upstream is recorded");
                }
            }
            bundles.push(row);
        }
    }
    let moved = bundles.iter().filter(|v| v["status"] == "moved").count();
    let current = bundles.iter().filter(|v| v["status"] == "current").count();
    let unknown = bundles.len() - moved - current;
    let report = json!({"schema":1,"kind":"oms-skill-outdated","bundles":bundles,"counts":{"current":current,"moved":moved,"unknown":unknown},"omitted":0});
    let mut out = emit(report, args);
    out.code = i32::from(moved > 0);
    Ok(out)
}
fn command_array(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| error("evaluation command must be a nonempty argv list"))?
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && !s.contains('\0'))
                .map(str::to_owned)
                .ok_or_else(|| error("invalid evaluation command argument"))
        })
        .collect()
}
fn eval_command(
    argv: &[String],
    cwd: &Path,
    root: &Path,
    skill: &str,
    path: &Path,
    arm: &str,
    enabled: &str,
    input: Option<&str>,
    ctx: &Context<'_>,
) -> Result<Output> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env("OMS_SKILL_EVAL_SKILL", skill)
        .env("OMS_SKILL_EVAL_PATH", path)
        .env("OMS_SKILL_EVAL_ARM", arm)
        .env("OMS_SKILL_EVAL_TREATMENT", enabled)
        .env("PYTHONHASHSEED", "0");
    let _ = root;
    if let Some(input) = input {
        // A private file supplies stdin without a producer pipe that can outlive cancellation.
        let scratch = Scratch::new("skill-input")?;
        let path = scratch.0.join("prompt");
        std::fs::write(&path, input.as_bytes())?;
        command.stdin(std::fs::File::open(path)?);
        capture(command, ctx.interrupted, 60)
    } else {
        command.stdin(Stdio::null());
        capture(command, ctx.interrupted, 60)
    }
}
fn evaluate(args: &Args, root: &Path, ctx: &Context<'_>) -> Result<Output> {
    args.count(2)?;
    let skill = skills::name(
        args.positional
            .get(1)
            .ok_or_else(|| error("eval requires a name"))?,
        false,
    )?;
    let path = root.join(".oms/skills").join(skill);
    no_links(root, Path::new(&format!(".oms/skills/{skill}")))?;
    let files = skills::entries(&path)?;
    let digest = skills::bundle_digest(&files);
    let suite_path = ctx.path(args.required("--suite")?);
    let suite_raw = read(&suite_path, 1024 * 1024)?;
    let suite = json(&suite_raw)?;
    if suite["schema"] != 1 || suite["skill"] != skill {
        return Err(error("skill eval suite schema/name does not match"));
    }
    let router = command_array(&suite["router"])?;
    let empty = vec![];
    let triggers = suite
        .get("trigger_cases")
        .map(|v| v.as_array().ok_or_else(|| error("invalid trigger cases")))
        .transpose()?
        .unwrap_or(&empty);
    let tasks = suite
        .get("task_cases")
        .map(|v| v.as_array().ok_or_else(|| error("invalid task cases")))
        .transpose()?
        .unwrap_or(&empty);
    if triggers.len() + tasks.len() > 100 {
        return Err(error("skill eval suite exceeds 100 cases"));
    }
    if (!triggers.is_empty() || !tasks.is_empty()) && !args.has("--allow-host-commands") {
        return Err(error(
            "black-box eval commands require --allow-host-commands",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for case in triggers.iter().chain(tasks.iter()) {
        let id = case["id"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 160
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
            .ok_or_else(|| error("invalid eval case id"))?;
        if !seen.insert(id) {
            return Err(error("duplicate skill eval case id"));
        }
    }
    for case in triggers {
        if !case["should_trigger"].is_boolean()
            || !case["prompt"].as_str().is_some_and(|s| s.len() <= 65536)
        {
            return Err(error("invalid trigger case"));
        }
    }
    for case in tasks {
        command_array(&case["command"])?;
        command_array(&case["verify"])?;
    }
    let candidate = args.get("--candidate").map(|p| ctx.path(p));
    if let Some(candidate) = &candidate {
        let raw = read(&candidate.join("SKILL.md"), 256 * 1024)?;
        skills::validate(&raw, skill, false)?;
    }
    let candidate_digest = candidate
        .as_ref()
        .map(|p| skills::entries(p).map(|v| skills::bundle_digest(&v)))
        .transpose()?;
    let mut matrix = [[0usize; 4]; 2];
    let mut passed = [0usize; 2];
    for (index, arm) in ["baseline", "treatment"].iter().enumerate() {
        let arm_path = if index == 1 {
            candidate.as_deref().unwrap_or(&path)
        } else {
            &path
        };
        let enabled = if index == 0 && candidate.is_none() {
            "0"
        } else {
            "1"
        };
        for case in triggers {
            let output = eval_command(
                &router,
                root,
                root,
                skill,
                arm_path,
                arm,
                enabled,
                case["prompt"].as_str(),
                ctx,
            )?;
            if output.code != 0 {
                return Err(error("router failed during skill evaluation"));
            }
            let row = json(&output.out)?;
            let selected = row["selected"]
                .as_array()
                .filter(|v| v.iter().all(Value::is_string))
                .ok_or_else(|| error("router output selected must be a string list"))?;
            let actual = selected.iter().any(|v| v == skill);
            let expected = case["should_trigger"] == true;
            matrix[index][match (actual, expected) {
                (true, true) => 0,
                (false, false) => 1,
                (true, false) => 2,
                (false, true) => 3,
            }] += 1;
        }
        for case in tasks {
            let scratch = Scratch::new("skill-eval-case")?;
            let execute = command_array(&case["command"])?;
            let verify = command_array(&case["verify"])?;
            if eval_command(
                &execute, &scratch.0, root, skill, arm_path, arm, enabled, None, ctx,
            )?
            .code
                == 0
                && eval_command(
                    &verify, &scratch.0, root, skill, arm_path, arm, enabled, None, ctx,
                )?
                .code
                    == 0
            {
                passed[index] += 1;
            }
        }
    }
    if skills::bundle_digest(&skills::entries(&path)?) != digest
        || read(&suite_path, 1024 * 1024)? != suite_raw
    {
        return Err(error("skill or suite changed during evaluation"));
    }
    if let (Some(path), Some(expected)) = (&candidate, &candidate_digest) {
        if skills::bundle_digest(&skills::entries(path)?) != *expected {
            return Err(error("candidate changed during evaluation"));
        }
    }
    let counters = |n: usize| json!({"true_positive":matrix[n][0],"true_negative":matrix[n][1],"false_positive":matrix[n][2],"false_negative":matrix[n][3]});
    let denominator = matrix[1][0] + matrix[1][2];
    let recall = matrix[1][0] + matrix[1][3];
    let mut result = json!({"schema":1,"skill":skill,"skill_sha256":digest,"suite_sha256":crate::common::digest(&suite_raw),"trigger":{"baseline":counters(0),"treatment":counters(1),"precision":if denominator==0{None}else{Some(matrix[1][0] as f64/denominator as f64)},"recall":if recall==0{None}else{Some(matrix[1][0] as f64/recall as f64)}},"task":{"cases":tasks.len(),"baseline_passed":passed[0],"treatment_passed":passed[1],"pass_delta":passed[1] as i64-passed[0] as i64}});
    if let Some(candidate) = candidate_digest {
        result["comparison"] =
            json!({"mode":"candidate","baseline_sha256":digest,"treatment_sha256":candidate});
    }
    if args.has("--record") {
        let path = root.join(".oms/runtime/skill-evals.jsonl");
        let _guard = lock(&path)?;
        let old = optional(&path)?;
        let mut bytes = old.clone();
        let mut row = result.clone();
        row["created_at"] = json!(timestamp());
        row["evaluation_id"] = json!(format!(
            "seval_{}",
            &crate::common::digest(result.to_string().as_bytes())[..24]
        ));
        writeln!(&mut bytes, "{row}")?;
        write_locked(&path, &bytes, false, Some(&old))?;
    }
    Ok(emit(result, args))
}
