use crate::runtime::{self as r, Result};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

#[derive(Serialize, Deserialize)]
pub(crate) enum Effect {
    Crontab {
        cwd: PathBuf,
        before: Option<Vec<u8>>,
        after: Vec<u8>,
    },
    Systemd {
        cwd: PathBuf,
        name: String,
        enabled: String,
        active: bool,
    },
}
fn run(cwd: &Path, name: &str, args: &[&str], input: Option<&[u8]>) -> Result<crate::Output> {
    let program = r::find(name, cwd)
        .ok_or_else(|| r::Error(2, format!("{name} unavailable for scheduler recovery")))?;
    r::command(
        program,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        cwd,
        input,
        &AtomicBool::new(false),
        Duration::from_secs(30),
    )
}
pub(crate) fn cron(cwd: &Path) -> Result<Option<Vec<u8>>> {
    let out = run(cwd, "crontab", &["-l"], None)?;
    if out.0 == 0 {
        return Ok(Some(out.1));
    }
    if out.0 == 1
        && String::from_utf8_lossy(&out.2)
            .to_lowercase()
            .contains("no crontab")
    {
        return Ok(None);
    }
    r::fail("could not inspect crontab for reversible publication")
}
impl Effect {
    pub fn systemd(cwd: &Path, name: &str) -> Result<Self> {
        if !matches!(name, "autoupdate" | "tick") {
            return r::fail("unknown scheduler unit");
        }
        let unit = format!("oh-my-setting-{name}.timer");
        let out = run(cwd, "systemctl", &["--user", "is-enabled", &unit], None)?;
        let enabled = String::from_utf8_lossy(&out.1).trim().to_owned();
        let enabled = if enabled.is_empty() && String::from_utf8_lossy(&out.2).contains("not exist")
        {
            "not-found".to_owned()
        } else {
            enabled
        };
        if !matches!(
            enabled.as_str(),
            "enabled" | "enabled-runtime" | "disabled" | "not-found" | "static" | "indirect"
        ) {
            return r::fail("cannot preserve unknown scheduler activation state");
        }
        let active = run(
            cwd,
            "systemctl",
            &["--user", "is-active", "--quiet", &unit],
            None,
        )?
        .0 == 0;
        Ok(Self::Systemd {
            cwd: cwd.into(),
            name: name.into(),
            enabled,
            active,
        })
    }
    pub fn restore(&self) -> Result<()> {
        match self {
            Self::Crontab { cwd, before, after } => {
                let current = cron(cwd)?;
                if &current == before {
                    return Ok(());
                }
                if current.as_deref() != Some(after) {
                    return r::fail("crontab changed concurrently; scheduler journal retained");
                }
                let out = if let Some(raw) = before {
                    run(cwd, "crontab", &["-"], Some(raw))?
                } else {
                    run(cwd, "crontab", &["-r"], None)?
                };
                if out.0 != 0 {
                    return r::fail("crontab rollback failed; scheduler journal retained");
                }
            }
            Self::Systemd {
                cwd,
                name,
                enabled,
                active,
            } => {
                if !matches!(name.as_str(), "autoupdate" | "tick") {
                    return r::fail("unsafe scheduler recovery unit");
                }
                let unit = format!("oh-my-setting-{name}.timer");
                if run(cwd, "systemctl", &["--user", "daemon-reload"], None)?.0 != 0 {
                    return r::fail("scheduler rollback reload failed");
                }
                let mut actions = Vec::new();
                match enabled.as_str() {
                    "enabled" => actions.push(vec!["--user", "enable", unit.as_str()]),
                    "enabled-runtime" => {
                        actions.push(vec!["--user", "disable", unit.as_str()]);
                        actions.push(vec!["--user", "enable", "--runtime", unit.as_str()]);
                    }
                    "disabled" | "not-found" | "static" | "indirect" => {
                        actions.push(vec!["--user", "disable", unit.as_str()])
                    }
                    _ => return r::fail("invalid scheduler recovery activation state"),
                }
                actions.push(vec![
                    "--user",
                    if *active { "start" } else { "stop" },
                    unit.as_str(),
                ]);
                for args in actions {
                    let out = run(cwd, "systemctl", &args, None)?;
                    if out.0 != 0 && enabled != "not-found" {
                        return r::fail("scheduler rollback activation failed");
                    }
                }
            }
        }
        Ok(())
    }
}
