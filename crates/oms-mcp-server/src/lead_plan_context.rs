//! Explicit export from the existing owned answer reader into a task-bound
//! context pack. Preparing reference data never invokes a Plan mutation.
use super::{LEAD_STATUS, PanelResult};
use crate::storage::{Directory, hash};
use crate::value::Value;
use crate::{RUN_ROOT, Server};
use oms_command_plan::{json as plan_json, reviewed_context};
use rustix::fs::{self, Dir, Mode, OFlags};
use std::path::Path;

const INVALID: &str = "The reviewed Lead excerpt or Plan task changed. Review them again.";
const MAX_CONTEXT_PACKS: usize = 16;
const MAX_OPERATION_ENTRIES: usize = 4096;
const PACK_PREFIX: &str = "lead-plan-context-";
const PACK_LIMIT_REACHED: &str = "This Lead answer already has the maximum 16 prepared context packs. Reuse an existing selection or start a new Lead turn.";

fn count_context_packs(directory: &Directory) -> PanelResult<usize> {
    let scan_fd = fs::openat(
        &directory.fd,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| INVALID)?;
    let mut packs = 0usize;
    let mut seen = 0usize;
    for entry in Dir::read_from(&scan_fd).map_err(|_| INVALID)? {
        let entry = entry.map_err(|_| INVALID)?;
        seen = seen.saturating_add(1);
        if seen > MAX_OPERATION_ENTRIES {
            return Err(INVALID.into());
        }
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if name.starts_with(PACK_PREFIX.as_bytes()) && name.ends_with(b".json") {
            packs += 1;
        }
    }
    Ok(packs)
}

fn command_repo(repo: &Path) -> PanelResult<&str> {
    repo.to_str()
        .filter(|path| !path.chars().any(char::is_control))
        .ok_or_else(|| {
            "The selected project path cannot be represented in an executable command.".into()
        })
}

impl Server<'_> {
    pub(super) fn prepare_lead_plan_context(
        &self,
        repo: &Path,
        args: &Value,
        meta: &Value,
    ) -> PanelResult<Value> {
        let command_repo = command_repo(repo)?;
        let digest = |key: &str| {
            args[key]
                .as_str()
                .filter(|s| crate::matches("[0-9a-f]{64}", s))
                .ok_or(INVALID)
        };
        let expected_answer = digest("answerSha256")?;
        let expected_excerpt = digest("excerptSha256")?;
        let expected_task = digest("taskSha256")?;
        let task_id = args["taskId"]
            .as_str()
            .filter(|s| crate::matches("[a-zA-Z0-9][a-zA-Z0-9._-]{0,159}", s))
            .ok_or(INVALID)?;
        let provider = args["executionProvider"]
            .as_str()
            .filter(|s| matches!(*s, "claude" | "antigravity"))
            .ok_or(INVALID)?;
        let offset = args["offset"]
            .as_u64()
            .filter(|n| *n <= crate::MAX_ANSWER as u64)
            .ok_or(INVALID)? as usize;
        let bytes = args["bytes"]
            .as_u64()
            .filter(|n| (1..=reviewed_context::MAX_EXCERPT as u64).contains(n))
            .ok_or(INVALID)? as usize;
        let mut source_args = json!({});
        for key in [
            "project",
            "projectKey",
            "room",
            "provider",
            "leadRequestId",
            "turnId",
            "answerPage",
        ] {
            if let Some(value) = args.get(key) {
                source_args[key] = value.clone();
            }
        }
        let read_answer = || -> PanelResult<Value> {
            let result = self.lead_conversations(repo, LEAD_STATUS, &source_args, meta)?;
            let page = result["leadConversation"]["answer"].clone();
            if page["digest"] != expected_answer || page["source"] != "recorded-answer" {
                return Err(INVALID.into());
            }
            Ok(page)
        };
        let page = read_answer()?;
        let page_offset = page["offset"].as_u64().ok_or(INVALID)? as usize;
        let start = offset.checked_sub(page_offset).ok_or(INVALID)?;
        let text = page["text"]
            .as_str()
            .and_then(|s| s.get(start..start.checked_add(bytes)?))
            .filter(|s| !s.trim().is_empty())
            .ok_or(INVALID)?;
        if hash(text) != expected_excerpt || args["excerpt"].as_str() != Some(text) {
            return Err(INVALID.into());
        }
        let task = || -> PanelResult<Value> {
            let (task, _) = self.panel_exec(
                repo,
                vec![
                    "agent-plan".into(),
                    "--repo".into(),
                    repo.as_os_str().into(),
                    "evidence-snapshot".into(),
                    "--id".into(),
                    task_id.into(),
                ],
                false,
            )?;
            if task["id"] != task_id
                || task["task_sha256"] != expected_task
                || task["state"] != "ready"
                || task["contract_stale"] != false
                || !task["allowed_paths"]
                    .as_array()
                    .is_some_and(|p| !p.is_empty())
                || !task["verify"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty())
                || !task.get("assignment").is_none_or(|assignment| {
                    assignment
                        .as_object()
                        .is_some_and(|a| a.is_empty() || task["assignment"]["provider"] == provider)
                })
            {
                return Err(INVALID.into());
            }
            Ok(task)
        };
        let selected = task()?;
        let selected = plan_json::parse(selected.to_string().as_bytes()).map_err(|_| INVALID)?;
        let handoff = &page["handoff"];
        let findings = json!({"schema":1,"kind":"reviewed-lead-answer","authority":"context-only",
            "source":{"project_key":args["projectKey"],"room":handoff["room"],"participant":handoff["participant"],"provider":handoff["provider"],"lead_request_id":handoff["leadRequestId"],"turn_id":handoff["turnId"],"operation":handoff["operation"],"answer_sha256":expected_answer,"page_offset":page["offset"],"page_bytes":page["bytes"],"page_cursor_sha256":hash(args["answerPage"].as_str().ok_or(INVALID)?),"answer_bytes":page["totalBytes"]},
            "selection":{"text":text,"offset":offset,"bytes":bytes,"sha256":expected_excerpt},
            "target":{"task_id":task_id,"task_sha256":expected_task,"contract_sha256":reviewed_context::task_contract(&selected).map_err(|_| INVALID)?,"provider":provider}});
        let excerpt = reviewed_context::LeadExcerpt::parse(findings.to_string().as_bytes())
            .map_err(|_| "The selected excerpt is not safe bounded context.")?;
        excerpt.check_project(repo).map_err(|_| INVALID)?;
        excerpt
            .check_task(repo, &selected, provider, true)
            .map_err(|_| INVALID)?;
        let pack_digest = plan_json::hash(excerpt.value()).map_err(|_| INVALID)?;
        let pack = json!({"schema":1,"pack_digest":pack_digest,"files":[],"tests":[],"lead_findings":findings});
        let raw = pack.to_string();
        crate::output::outbound(raw.as_bytes(), &Value::Null)
            .map_err(|_| "OMS withheld this context pack during its outbound scan.")?;
        let sha = hash(&raw);
        let operation = handoff["operation"].as_str().ok_or(INVALID)?;
        let directory = Directory::run(repo, operation, true).map_err(|_| INVALID)?;
        let _guard = directory.lock().map_err(
            |_| "Another context pack is being prepared for this Lead answer. Try again.",
        )?;
        // Recheck both owners after collection, before publishing a bounded
        // snapshot into the already-owned operation. No transcript mirror.
        if read_answer()? != page {
            return Err(INVALID.into());
        }
        task()?;
        let name = format!("{PACK_PREFIX}{sha}.json");
        let path = format!("{RUN_ROOT}/{operation}/{name}");
        let root = oms_safe_fs::Root::open(repo).map_err(|_| INVALID)?;
        match root.read(&path, 262144, oms_safe_fs::Policy::manifest()) {
            Ok(stored) => {
                if stored != raw.as_bytes() {
                    return Err(INVALID.into());
                }
            }
            Err(oms_safe_fs::Error::Missing) => {
                if count_context_packs(&directory)? >= MAX_CONTEXT_PACKS {
                    return Err(PACK_LIMIT_REACHED.into());
                }
                directory
                    .publish_new(&name, raw.as_bytes())
                    .map_err(|_| INVALID)?;
                let stored = root
                    .read(&path, 262144, oms_safe_fs::Policy::manifest())
                    .map_err(|_| INVALID)?;
                if stored != raw.as_bytes() {
                    return Err(INVALID.into());
                }
            }
            Err(_) => return Err(INVALID.into()),
        }
        let argv = json!([
            "panel",
            "--repo",
            command_repo,
            "--room",
            args["room"],
            "--orchestrator",
            provider,
            "--task-id",
            task_id,
            "--context-pack",
            path,
            "--expected-context-pack-sha256",
            sha,
            "--json"
        ]);
        let mut preview = argv.as_array().ok_or(INVALID)?.clone();
        preview.push(json!("--dry-run"));
        Ok(
            json!({"status":"prepared","authority":"context-only","projectKey":args["projectKey"],"room":args["room"],"findings":findings,
            "contextPack":{"path":path,"sha256":sha,"bytes":raw.len(),"packDigest":pack_digest},
            "execution":{"mode":"managed-plan-run","provider":provider,"taskId":task_id,"taskSha256":expected_task,"nativeLeadInput":false,"startsTask":false,"autoLand":false,"publication":"none"},
            "previewCommand":{"program":"oms","cwd":command_repo,"argv":preview},"runCommand":{"program":"oms","cwd":command_repo,"argv":argv}}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn executable_project_path_is_exact_and_never_lossy() {
        assert_eq!(
            command_repo(Path::new("/fixture/selected project")).unwrap(),
            "/fixture/selected project"
        );
        assert!(command_repo(Path::new(OsStr::from_bytes(b"/fixture/invalid-\xff"))).is_err());
        assert!(command_repo(Path::new("/fixture/invalid\nproject")).is_err());
    }
}
