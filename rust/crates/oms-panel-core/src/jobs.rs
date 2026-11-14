//! Owned job generations. Only the reactor that spawned a job can signal it.
use crate::terminals::{MAX_RECORD, PrivateStore};
use crate::{Error, Result, Value, require};
use oms_supervisor::process::{JobId, Progress, Reactor, Request, Scope, StopReason, Streams};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
pub const LOG_LIMIT: usize = 1024 * 1024;
struct Owned {
    job: JobId,
    row: Value,
}
pub struct Jobs {
    store: PrivateStore,
    reactor: Reactor,
    owned: BTreeMap<String, Owned>,
}
impl Jobs {
    /// Requires exclusive Linux subreaper ownership. A second reactor is refused.
    pub fn open(repo: &Path) -> Result<Self> {
        crate::parent_only()?;
        let store = PrivateStore::open(repo, "jobs", false, true)?;
        let reactor = Reactor::dedicated().map_err(Error::new)?;
        Ok(Self {
            store,
            reactor,
            owned: BTreeMap::new(),
        })
    }
    pub fn run_job(&mut self, argv: Vec<OsString>, cwd: &Path, label: &str) -> Result<Value> {
        crate::parent_only()?;
        require(
            !argv.is_empty() && !argv[0].is_empty() && argv.len() <= 256,
            "explicit argv recipe of 1..256 arguments required",
        )?;
        let cwd = std::fs::canonicalize(cwd).map_err(Error::new)?;
        let strings: Vec<_> = argv
            .iter()
            .map(|v| {
                v.to_str()
                    .ok_or_else(|| Error::new("job argv must be UTF-8"))
            })
            .collect::<Result<_>>()?;
        require(
            strings.iter().all(|v| !v.contains('\0')),
            "job argv contains NUL",
        )?;
        let recipe = serde_json::json!({"repo":self.store.repo,"uid":rustix::process::getuid().as_raw(),"cwd":cwd,"argv":strings});
        let recipe_bytes = crate::value::encoded(&recipe)?;
        require(
            recipe_bytes.len() <= MAX_RECORD,
            "job recipe exceeds private record limit",
        )?;
        let row = self.store.allocate("job","job",&serde_json::json!({"label":crate::eventlog::clean(label,120),"generation":1,"state":"starting","recipe_sha256":crate::digest(&recipe_bytes)}))?;
        let ident = row["id"].as_str().unwrap().to_owned();
        {
            let tx = self
                .store
                .store
                .lock_at(Path::new("lock"), Duration::from_secs(3))?;
            tx.write(
                &PathBuf::from(&ident).join("recipe"),
                &recipe_bytes,
                true,
                MAX_RECORD,
            )?;
            tx.write(&PathBuf::from(&ident).join("output"), b"", true, LOG_LIMIT)?;
        }
        let mut request = Request::new(argv);
        request.cwd = Some(cwd);
        request.scope = Scope::WholeTree;
        request.streams = Streams::Merged;
        request.capture.max_bytes = LOG_LIMIT;
        request.clear_environment = true;
        request.environment = crate::terminals::ordinary_environment();
        let job = self
            .reactor
            .start(request)
            .map_err(|e| Error::new(format!("job starting record retained: {e}")))?;
        let mut changed = row.clone();
        changed["state"] = "running".into();
        changed["supervisor_pid"] = std::process::id().into();
        let owner = crate::terminals::process(std::process::id())?;
        changed["supervisor_identity"] = owner.birth.into();
        self.owned.insert(
            ident.clone(),
            Owned {
                job,
                row: row.clone(),
            },
        );
        self.store.update(&ident, &row, &changed)?;
        self.owned.get_mut(&ident).unwrap().row = changed.clone();
        Ok(changed)
    }
    pub fn poll(&mut self, ident: &str) -> Result<Value> {
        let owned = self
            .owned
            .get_mut(ident)
            .ok_or_else(|| Error::new("job generation is not owned by this reactor"))?;
        let progress = self
            .reactor
            .poll(owned.job, Duration::ZERO)
            .map_err(Error::new)?;
        let (bytes, mut row, complete) = match progress {
            Progress::Pending(v) => {
                let mut row = owned.row.clone();
                row["leader_pid"] = v.pid.into();
                (v.stdout.bytes, row, false)
            }
            Progress::Complete(v) => {
                let mut row = owned.row.clone();
                if v.cleanup_complete() && v.all_reaped {
                    row["state"] = "exited".into();
                    row["completion"] = "reaped-all-children".into();
                } else {
                    row["state"] = "unknown".into();
                }
                row["exit"] = v.exit.map(|v| v.shell_code()).into();
                row["log_truncated"] = v.stdout.limited.into();
                (v.stdout.bytes, row, true)
            }
        };
        let tx = self
            .store
            .store
            .lock_at(Path::new("lock"), Duration::from_secs(3))?;
        require(
            self.store.record(ident)? == owned.row,
            "job generation changed",
        )?;
        tx.write(
            &PathBuf::from(ident).join("output"),
            &bytes,
            false,
            LOG_LIMIT,
        )?;
        row["updated_at"] = crate::now().into();
        tx.write_json(
            &PathBuf::from(ident).join("record"),
            &row,
            false,
            MAX_RECORD,
        )?;
        owned.row = row.clone();
        if complete {
            self.owned.remove(ident);
        }
        Ok(row)
    }
    pub fn restart_job(&mut self, ident: &str, generation: u64) -> Result<Value> {
        crate::parent_only()?;
        require(generation >= 1, "exact job generation required")?;
        require(
            !self.owned.contains_key(ident),
            "job is still owned by this reactor",
        )?;
        let tx = self
            .store
            .store
            .lock_at(Path::new("lock"), Duration::from_secs(3))?;
        let old = self.store.record(ident)?;
        let _record = self
            .store
            .store
            .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
        require(
            self.store.record(ident)? == old,
            "job generation changed before record lock",
        )?;
        require(
            old["archived"] != true
                && old["kind"] == "job"
                && old["generation"].as_u64() == Some(generation)
                && archivable(&old),
            "restart requires the confirmed exited generation",
        )?;
        let next = generation
            .checked_add(1)
            .ok_or_else(|| Error::new("job generation exhausted"))?;
        let recipe_bytes = tx.read(&PathBuf::from(ident).join("recipe"), MAX_RECORD, false)?;
        require(
            old["recipe_sha256"] == crate::digest(&recipe_bytes),
            "job recipe digest changed",
        )?;
        let recipe = crate::decode(&recipe_bytes, MAX_RECORD)?;
        require(
            recipe["repo"] == self.store.repo.to_string_lossy().as_ref()
                && recipe["uid"].as_u64() == Some(u64::from(rustix::process::getuid().as_raw())),
            "job recipe binding changed",
        )?;
        let cwd = Path::new(
            recipe["cwd"]
                .as_str()
                .ok_or_else(|| Error::new("job cwd missing"))?,
        );
        let physical = std::fs::canonicalize(cwd).map_err(Error::new)?;
        require(
            cwd.is_absolute() && physical == cwd && physical.is_dir(),
            "job cwd is not canonical",
        )?;
        let values = recipe["argv"]
            .as_array()
            .ok_or_else(|| Error::new("job argv missing"))?;
        require(
            !values.is_empty() && values.len() <= 256,
            "invalid job argv count",
        )?;
        let argv = values
            .iter()
            .map(|v| {
                let value = v
                    .as_str()
                    .ok_or_else(|| Error::new("job argv must be strings"))?;
                require(!value.contains('\0'), "job argv contains NUL")?;
                Ok(OsString::from(value))
            })
            .collect::<Result<Vec<_>>>()?;
        require(!argv[0].is_empty(), "job executable is empty")?;
        let output_path = PathBuf::from(ident).join("output");
        let record_path = PathBuf::from(ident).join("record");
        let previous = tx.read(&output_path, LOG_LIMIT, false)?;
        let mut row = old.clone();
        for key in [
            "exit",
            "completion",
            "leader_pid",
            "supervisor_pid",
            "supervisor_identity",
            "request",
        ] {
            row.as_object_mut().unwrap().remove(key);
        }
        row["generation"] = next.into();
        row["state"] = "starting".into();
        row["log_truncated"] = false.into();
        row["updated_at"] = crate::now().into();
        let owner = crate::terminals::process(std::process::id())?;
        row["supervisor_pid"] = owner.pid.into();
        row["supervisor_identity"] = owner.birth.into();
        let prepare = (|| -> Result<_> {
            tx.write_json(&record_path, &row, false, MAX_RECORD)?;
            tx.write(&output_path, b"", false, LOG_LIMIT)?;
            let mut request = Request::new(argv);
            request.cwd = Some(physical);
            request.scope = Scope::WholeTree;
            request.streams = Streams::Merged;
            request.capture.max_bytes = LOG_LIMIT;
            request.clear_environment = true;
            request.environment = crate::terminals::ordinary_environment();
            self.reactor.start(request).map_err(Error::new)
        })();
        let job = match prepare {
            Ok(job) => job,
            Err(error) => {
                let restored = (|| -> Result<()> {
                    tx.write(&output_path, &previous, false, LOG_LIMIT)?;
                    tx.write_json(&record_path, &old, false, MAX_RECORD)?;
                    Ok(())
                })();
                return Err(match restored {
                    Ok(()) => error,
                    Err(restore) => Error {
                        publication_uncertain: true,
                        ..Error::new(format!(
                            "restart failed: {error}; rollback incomplete: {restore}"
                        ))
                    },
                });
            }
        };
        self.owned.insert(
            ident.into(),
            Owned {
                job,
                row: row.clone(),
            },
        );
        row["state"] = "running".into();
        tx.write_json(&record_path, &row, false, MAX_RECORD)
            .map_err(|e| Error {
                publication_uncertain: true,
                ..e
            })?;
        self.owned.get_mut(ident).unwrap().row = row.clone();
        Ok(row)
    }
    pub fn interrupt_job(&mut self, ident: &str) -> Result<()> {
        self.signal(ident, 2)
    }
    pub fn stop_job(&mut self, ident: &str) -> Result<()> {
        self.signal(ident, 15)
    }
    fn signal(&mut self, ident: &str, number: i32) -> Result<()> {
        crate::parent_only()?;
        let owned = self.owned.get(ident).ok_or_else(|| {
            Error::new(
                "job generation is not owned; process identity alone grants no signal authority",
            )
        })?;
        require(
            self.store.record(ident)? == owned.row,
            "job generation changed",
        )?;
        self.reactor
            .stop(owned.job, StopReason::Signal(number))
            .map_err(Error::new)
    }
    pub fn list_jobs(&self) -> Result<Vec<Value>> {
        let mut rows = self.store.list()?;
        for row in &mut rows {
            if row["state"] != "exited"
                && !self
                    .owned
                    .contains_key(row["id"].as_str().unwrap_or_default())
            {
                row["state"] = "unknown".into();
            }
        }
        Ok(rows)
    }
}
pub fn job_logs(repo: &Path, ident: &str, maximum: usize) -> Result<Value> {
    let store = PrivateStore::open(repo, "jobs", false, false)?;
    store.record(ident)?;
    crate::terminals::preview(
        &store
            .store
            .read(&PathBuf::from(ident).join("output"), LOG_LIMIT, false)?,
        maximum,
    )
}
pub fn archivable(row: &Value) -> bool {
    row["state"] == "exited" && row["completion"] == "reaped-all-children"
}
pub fn forget_job(repo: &Path, ident: &str, generation: u64) -> Result<Value> {
    require(generation >= 1, "exact job generation required")?;
    crate::history::forget_record(
        repo,
        "jobs",
        ident,
        |row| Ok(row["generation"].as_u64() == Some(generation) && archivable(row)),
        &["record", "recipe", "output"],
    )
}
