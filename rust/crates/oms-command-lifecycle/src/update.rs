use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde_json::json;
use std::{path::PathBuf, sync::atomic::AtomicBool};

fn state_path(paths: &Paths) -> Result<PathBuf> {
    r::leaf(
        &std::env::var_os("OH_MY_SETTING_AUTO_UPDATE_STATE")
            .map(PathBuf::from)
            .unwrap_or(paths.source.join("local/auto-update.status")),
    )
}
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    if options.tool == "auto-update" {
        match options.action.as_str() {
            "install" => {
                let mut o = options.clone();
                o.tool = "install-autoupdate".into();
                return crate::scheduler::run(paths, &o, flag);
            }
            "remove" => {
                let mut o = options.clone();
                o.tool = "uninstall-autoupdate".into();
                return crate::scheduler::run(paths, &o, flag);
            }
            "status" => {
                let file = state_path(paths)?;
                return if r::exists(&file) {
                    Ok((0, r::read(&file, r::LIMIT)?, Vec::new()))
                } else {
                    Ok(r::text("auto-update: no-run"))
                };
            }
            "attention" => {
                return Ok(r::text(if !r::exists(&state_path(paths)?) {
                    "no-run"
                } else {
                    "unknown"
                }));
            }
            "" | "check" | "apply" => (),
            _ => return r::fail("unknown auto-update mode"),
        }
    }
    let old = paths.require_owner()?;
    let reference = options
        .get("--ref")
        .map(str::to_owned)
        .or_else(|| {
            old.as_ref()
                .and_then(|r| r["ref"].as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| "edge".into());
    if reference.is_empty() || reference.starts_with('-') || reference.contains(['\n', '\r', '\0'])
    {
        return r::fail("invalid update ref");
    }
    let current = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    if options.preview() {
        return r::json(
            &json!({"schema":1,"current":current,"ref":reference,"actions":["fetch target","verify clean checkout and worker/call fences","verify native payload","reconcile recorded components","doctor gate","publish receipt"],"not_previewed":["fetch","provider refresh"]}),
        );
    }
    r::child_guard()?;
    if options.has("--probe-agy-surfaces") {
        return r::fail(
            "native Antigravity surface certification adapter is not yet available; no provider was started",
        );
    }
    let check =
        options.has("--check") || options.tool == "auto-update" && options.action != "apply";
    if !check {
        return r::fail(
            "native transactional checkout/component update adapter is not yet available; checkout and installation unchanged",
        );
    }
    r::git(
        &paths.source,
        &["fetch", "--quiet", "--", "origin", &reference],
        flag,
    )?;
    let target = r::git(
        &paths.source,
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        flag,
    )?;
    let result = json!({"schema":1,"checked_at":r::now(),"current_commit":current,"target_commit":target,"update_available":current!=target,"ref":reference});
    if options.tool == "auto-update" {
        let path = state_path(paths)?;
        let _guard = r::lock(&path, &paths.cwd)?;
        r::atomic(&path, &r::bytes(&result)?)?;
    }
    if options.has("--json") {
        r::json(&result)
    } else {
        Ok(r::text(format!(
            "update: {}",
            if current == target {
                "current"
            } else {
                "available"
            }
        )))
    }
}
