use crate::common::*;
use serde_json::{Value, json};
use std::{collections::BTreeSet, ffi::OsString, path::Path};

fn packet(repo: &Path) -> Vec<u8> {
    oms_safe_fs::Root::open(repo)
        .ok()
        .and_then(|r| {
            r.read_no_follow(
                Path::new(".oms/task/current.md"),
                oms_safe_fs::ByteReadOptions {
                    missing_ok: true,
                    max_bytes: 65536,
                },
            )
            .ok()
        })
        .unwrap_or_default()
}
fn snapshot(repo: &Path, ctx: &Context<'_>) -> String {
    let task = packet(repo);
    let meta = |k: &str| {
        oms_command_task::packet::metadata(&task, k)
            .map(|v| String::from_utf8_lossy(v).trim().to_owned())
            .unwrap_or_else(|| "unknown".into())
    };
    let section = |k: &str| {
        String::from_utf8_lossy(&oms_command_task::packet::section(&task, k)).into_owned()
    };
    let status = oms_command_task::run(&["status".into(), "--json".into()], repo, ctx.interrupted);
    let status = document(&status.1).unwrap_or_else(|| json!({}));
    let mut out = format!(
        "- head: {}\n- task_id: {}\n- task_digest: {}\n- verification: {}\n- assistant_summary: unverified claim\n\n## Current task snapshot (advisory)\n\n",
        head(repo).unwrap_or_else(|| "unknown".into()),
        meta("task_id"),
        if task.is_empty() {
            "unknown".into()
        } else {
            hash(&task)
        },
        text(&status, "verification")
    );
    if task.is_empty() || meta("status") == "closed" {
        out.push_str(
            "No active task snapshot; use the recent request and recheck current state.\n",
        );
    } else {
        for (name, label) in [
            ("Goal", "Goal"),
            ("Current State", "Completed / remaining (recorded)"),
            ("Verification", "Verification evidence (recorded)"),
            ("Next Step", "Next action"),
        ] {
            let value = flat(&section(name));
            out.push_str(&format!(
                "- {label}: {}\n",
                if value.is_empty() {
                    "not recorded".into()
                } else {
                    clip(&value, 700)
                }
            ));
        }
        out.push_str("\nRecheck the current packet and source before acting; this is not fresh verification.\n");
    }
    let plan = load(&repo.join(".oms/plan/tasks.json"));
    if !text(&plan, "goal").is_empty() {
        out.push_str(&format!(
            "\n## Recorded plan (advisory)\n\nGoal: {}\nAcceptance: {}\n",
            clip(&flat(text(&plan, "goal")), 700),
            clip(&flat(text(&plan, "accept")), 700)
        ));
        for task in crate::routing::plan_tasks(&plan).iter().take(40) {
            out.push_str(&format!(
                "- {}: {}\n",
                text(task, "id"),
                text(task, "state")
            ));
        }
    }
    let decisions = section("Decisions");
    if !decisions.trim().is_empty() {
        out.push_str(&format!(
            "\n## Recorded decisions (advisory)\n\n{}\n",
            clip(
                &decisions,
                cap("OMS_HANDOFF_DECISION_BYTES", 1024, 0, 65536) as usize
            )
        ));
    }
    let verify = section("Verify");
    if !verify.trim().is_empty() {
        out.push_str(&format!(
            "\n## Resume contract\n\nRun before building on this handoff:\n\n```bash\n{}\n```\n",
            verify.trim()
        ));
    }
    let rows = read(&repo.join(".oms/artifacts/index.jsonl"), 64 * INPUT_MAX).unwrap_or_default();
    let rows = rows
        .split(|b| *b == b'\n')
        .filter_map(document)
        .collect::<Vec<_>>();
    let resolved = rows
        .iter()
        .filter(|r| r["kind"] == "artifact-resolution")
        .map(|r| text(r, "resolves_event_id"))
        .collect::<BTreeSet<_>>();
    let open = rows
        .iter()
        .filter(|r| {
            r["kind"] == "review-outcome"
                && r["exit"] == 1
                && r["review"]["overall"] == 1
                && !resolved.contains(text(r, "event_id"))
        })
        .rfind(|r| {
            let seats = r["review"]["seats"].as_array().cloned().unwrap_or_default();
            seats.iter().any(|s| s["verdict"] == "pass")
                && seats.iter().any(|s| s["verdict"] == "fail")
        });
    if let Some(row) = open {
        out.push_str("\n## Open dissents\n\nThe last review round ended split. Acknowledge each verdict — agree, override with reasons, or escalate — before landing related work; do not silently re-derive consensus.\n\n");
        for seat in row["review"]["seats"].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "{}: {}\n",
                text(seat, "provider"),
                text(seat, "verdict")
            ));
        }
        let event = text(row, "event_id");
        if !event.is_empty() {
            out.push_str(&format!("event: {event}\nresolve: oms artifact-index resolve --event-id {event} --reason \"<how it was settled>\"\n"));
        }
    } else if rows.iter().all(|r| r["kind"] != "review-outcome")
        && repo.join(".oms/artifacts/review").is_dir()
    {
        out.push_str("\n## Open dissents\n\nUNAVAILABLE: typed review outcomes are absent. Do not read absence as consensus; inspect `oms peer-review verdicts --json .oms/artifacts/review`.\n");
    }
    normalize_machine(&out)
}
fn clean_for_storage(raw: &str) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt;
    let environment = std::env::vars_os().collect::<Vec<_>>();
    let entries = environment
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect::<Vec<_>>();
    match oms_scrub::scan_secret(raw.as_bytes(), &entries) {
        Ok(hits) if hits.is_empty() => Ok(normalize_machine(raw)),
        _ => Err("session content looks sensitive; refusing to write the digest".into()),
    }
}
fn observe(repo: &Path, path: &Path, provider: &str, sid: &str, ctx: &Context<'_>) {
    if off("OMS_WORK_JOURNAL") {
        return;
    }
    let digest = read(path, INPUT_MAX).map(hash).unwrap_or_default();
    let args = vec![
        "observe".into(),
        "--repo".into(),
        repo.as_os_str().into(),
        "--source-type".into(),
        "session-handoff".into(),
        "--source-file".into(),
        path.as_os_str().into(),
        "--source-id".into(),
        format!("{provider}:{sid}:{digest}").into(),
    ];
    let _ = oms_command_journal::run_with_source(&args, repo, ctx.source, ctx.interrupted);
}
pub(crate) fn capture(
    v: &Value,
    repo: &Path,
    provider: &str,
    note: &str,
    skip: bool,
    ctx: &Context<'_>,
) -> Result<bool, String> {
    if ["antigravity", "agy"].contains(&provider) {
        return antigravity(v, repo, note, skip, ctx);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = std::path::PathBuf::from(format!(
        ".oms/handoffs/.hook-capture-{}-{stamp}.tmp",
        std::process::id()
    ));
    let mut args: Vec<OsString> = vec![
        "capture".into(),
        "--agent".into(),
        provider.into(),
        "--cwd".into(),
        payload_cwd(v, repo).as_os_str().into(),
        "--note".into(),
        note.into(),
        "--out".into(),
        staging.as_os_str().into(),
    ];
    if !session(v).is_empty() {
        args.extend(["--session".into(), session(v).into()]);
    }
    if skip {
        args.extend(["--missing-session-policy".into(), "skip".into()]);
    }
    let (r, timeout) = budgeted(ctx, std::time::Duration::from_secs(20), |flag| {
        oms_command_peer::run_with_source("session-handoff", &args, repo, ctx.source, flag)
    });
    if timeout {
        return Err("handoff capture did not finish in time".into());
    }
    if r.0 != 0 {
        return Err(safe_text(&String::from_utf8_lossy(&r.2), 160));
    }
    if r.1.is_empty() {
        return Ok(true);
    }
    let path = repo.join(&staging);
    let returned = std::str::from_utf8(&r.1)
        .map_err(|_| "capture returned invalid path")?
        .trim();
    if Path::new(returned) != path {
        return Err("capture returned an unexpected output path".into());
    }
    let owned = OwnedCapture::new(&path)?;
    let original = read(&path, INPUT_MAX)?;
    let contract = clean_for_storage(&snapshot(repo, ctx))?;
    let old = std::str::from_utf8(&original).map_err(|_| "capture returned invalid text")?;
    let sid = if session(v).is_empty() {
        old.lines()
            .find_map(|l| l.strip_prefix("- session: "))
            .unwrap_or("latest")
    } else {
        session(v)
    };
    let at = old.find("\n\n").map_or(0, |p| p + 2);
    let updated = format!("{}{}\n{}", &old[..at], contract, &old[at..]);
    let slug = sid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(24)
        .collect::<String>()
        .trim_end_matches('-')
        .to_owned();
    let output = repo.join(".oms/handoffs").join(format!(
        "{provider}-{}-{}.md",
        if slug.is_empty() { "latest" } else { &slug },
        now().replace(':', "-")
    ));
    mutate(&output, INPUT_MAX, false, |_| {
        Ok((updated.into_bytes(), ()))
    })?;
    drop(owned);
    observe(repo, &output, provider, sid, ctx);
    Ok(true)
}
struct OwnedCapture {
    path: std::path::PathBuf,
    dev: u64,
    ino: u64,
}
impl OwnedCapture {
    fn new(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(path).map_err(|_| "staged handoff missing")?;
        if !m.is_file() || m.nlink() != 1 {
            return Err("staged handoff unsafe".into());
        }
        Ok(Self {
            path: path.into(),
            dev: m.dev(),
            ino: m.ino(),
        })
    }
}
impl Drop for OwnedCapture {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.dev && m.ino() == self.ino)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn antigravity(
    v: &Value,
    repo: &Path,
    note: &str,
    skip: bool,
    ctx: &Context<'_>,
) -> Result<bool, String> {
    let source = env_path("OMS_GEMINI_HOME")
        .unwrap_or_else(|| home().join(".gemini"))
        .join("antigravity-cli/history.jsonl");
    let raw = match read(&source, 64 * INPUT_MAX) {
        Ok(raw) => raw,
        Err(_) if skip && !source.exists() => return Ok(true),
        Err(e) => return Err(e),
    };
    let cwd = payload_cwd(v, repo).to_string_lossy();
    let mut rows = raw
        .split(|b| *b == b'\n')
        .filter_map(document)
        .filter(|r| text(r, "workspace") == cwd && !text(r, "display").trim().is_empty())
        .collect::<Vec<_>>();
    let sid = if session(v).is_empty() {
        rows.last()
            .map(|r| text(r, "conversationId"))
            .unwrap_or("")
            .to_owned()
    } else {
        session(v).into()
    };
    rows.retain(|r| text(r, "conversationId") == sid);
    let task = snapshot(repo, ctx);
    let mut users = String::new();
    for row in rows
        .iter()
        .rev()
        .take(cap("OMS_HANDOFF_TURNS", 6, 0, 50) as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        users.push_str(&format!(
            "- {}\n",
            clip(text(row, "display").trim(), 400).replace('\n', " ")
        ));
    }
    let goal = rows
        .first()
        .map(|r| clip(text(r, "display").trim(), 600))
        .unwrap_or_else(|| "(no antigravity prompts matched)".into());
    let ts = now();
    let content = clean_for_storage(&format!("{goal}\n{users}\n{note}\n{task}"))?;
    if rows.len() < 2 {
        return Ok(true);
    }
    let _ = content;
    let body = format!(
        "# Session handoff: antigravity {sid}\n\n- captured: {ts}\n- agent: antigravity\n- cwd: {}\n- source: {}\n- note: {note}\n- verification_observed_at: {ts}\n{task}\n## Last assistant summary\n\n(antigravity assistant output not available from history.jsonl)\n\n## Original request (historical)\n\n{goal}\n\n## Recent user turns (last {} of {})\n\n{users}",
        normalize_machine(&cwd),
        normalize_machine(&source.to_string_lossy()),
        rows.len().min(cap("OMS_HANDOFF_TURNS", 6, 0, 50) as usize),
        rows.len()
    );
    let body = normalize_machine(&body);
    let slug = sid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(24)
        .collect::<String>()
        .trim_end_matches('-')
        .to_owned();
    let name = format!(
        "antigravity-{}-{}.md",
        if slug.is_empty() { "latest" } else { &slug },
        ts.replace(':', "-")
    );
    let path = repo.join(".oms/handoffs").join(name);
    ensure_oms(repo)?;
    mutate(&path, INPUT_MAX, false, |_| Ok((body.into_bytes(), ())))?;
    observe(repo, &path, "antigravity", &sid, ctx);
    Ok(true)
}
