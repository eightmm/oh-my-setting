use super::*;

pub fn patch_paths(text: &str) -> Result<Vec<String>> {
    let mut paths = vec![];
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("diff --git ") else {
            continue;
        };
        if rest.starts_with('"') || !rest.contains(" b/") {
            return Err(Error::new(format!(
                "patch names a path this step cannot stage exactly: {}",
                rest.chars().take(120).collect::<String>()
            )));
        }
        let (left, right) = rest.split_once(" b/").unwrap();
        let Some(left) = left.strip_prefix("a/") else {
            return Err(Error::new(format!(
                "patch header is not in git format: {}",
                rest.chars().take(120).collect::<String>()
            )));
        };
        for path in [left.trim(), right.trim()] {
            if !path.is_empty() && !paths.iter().any(|p| p == path) {
                paths.push(path.to_owned());
            }
        }
    }
    if paths.is_empty() {
        return Err(Error::new("patch names no paths"));
    }
    Ok(paths)
}
pub fn bound_with_host(
    repo: &Path,
    binding: &str,
    run: &str,
    message: &str,
    cancel: &AtomicBool,
    services: &(dyn oms_command_plan::Services + Sync),
    host: &dyn crate::HostServices,
) -> Result<Value> {
    if std::env::var_os("OMS_HARNESS_CHILD").as_deref() == Some(std::ffi::OsStr::new("1")) {
        return Err(Error::new(
            "a harness child may only use read-only graph actions",
        ));
    }
    let selected = if run.is_empty() {
        events::latest(repo)?
    } else {
        run.into()
    };
    if selected.is_empty() {
        return Err(Error::new(
            "no execution graph run to read the binding from",
        ));
    }
    let (_, state, facts) = runner::view(repo, &selected, cancel, services)?;
    let task = super::bound(&state, binding);
    if task.is_empty() {
        return Err(Error::new(format!(
            "run {selected} holds no task binding named {}",
            binding.chars().take(60).collect::<String>()
        )));
    }
    let task_view = runner::task_view(repo, &task, cancel, services)?;
    if st(&task_view, "state") != "done" {
        return Err(Error::new(format!(
            "task {task} is {}, not done; nothing landed to commit",
            task_view.get("state").or("-").py()
        )));
    }
    if !facts.get(&format!("receipt.land.{task}.present")).truth() {
        return Err(Error::new(format!(
            "task {task} has no landing receipt; the graph commits only what patch-land applied"
        )));
    }
    let patch_ref = st(&task_view, "patch");
    if patch_ref.is_empty() {
        return Err(Error::new(format!(
            "task {task} has no readable stored patch"
        )));
    }
    let path = util::at(repo, patch_ref);
    let store = crate::util::open_store(repo)?;
    let raw = store
        .read(&path, 256 * 1024 * 1024)
        .map_err(|_| Error::new(format!("task {task} has no readable stored patch")))?;
    let paths = patch_paths(&String::from_utf8_lossy(&raw))?;
    let title = if !message.is_empty() {
        message.to_owned()
    } else {
        task_view.get("title").or(task.as_str()).py()
    };
    let subject = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(72)
        .collect::<String>();
    let head = util::git(repo, &["rev-parse", "HEAD"], cancel);
    let args = vec![
        std::ffi::OsString::from("--repo"),
        repo.as_os_str().to_owned(),
        std::ffi::OsString::from("--commit-task"),
        std::ffi::OsString::from(&task),
        std::ffi::OsString::from("--commit-message"),
        std::ffi::OsString::from(&subject),
    ];
    let (code, stdout, stderr, _) = host
        .invoke_command("goal-drive", &args, repo, &[], None, cancel)
        .map_err(|e| {
            Error::new(format!(
                "exact commit could not complete; retry to reconcile: {}",
                e.message
            ))
        })?;
    if code != 0 {
        let body = if stderr.is_empty() { stdout } else { stderr };
        let detail = String::from_utf8_lossy(&body)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(500)
            .collect::<String>();
        return Err(Error::new(format!(
            "exact commit refused: {}",
            util::scrub(&detail)
        )));
    }
    let new_head = util::git(repo, &["rev-parse", "HEAD"], cancel);
    if new_head.is_empty() {
        return Err(Error::new("commit HEAD is unreadable"));
    }
    Ok(obj([
        ("schema", 1_i64.into()),
        ("run_id", selected.into()),
        ("binding", binding.into()),
        ("task_id", task.into()),
        ("commit", new_head.clone().into()),
        ("paths", list(paths)),
        (
            "status",
            if head == new_head {
                "clean"
            } else {
                "committed"
            }
            .into(),
        ),
    ]))
}
