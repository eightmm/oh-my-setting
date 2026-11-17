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
        let mut disk = self.store.record(ident)?;
        let pending_req = disk
            .as_object_mut()
            .and_then(|m| m.remove("request"))
            .and_then(|v| v.as_str().map(str::to_owned));
        require(disk == owned.row, "job generation changed")?;
        if !complete && let Some(req) = pending_req.as_deref() {
            match req {
                "interrupt" => {
                    let _ = self.reactor.stop(owned.job, StopReason::Signal(2));
                }
                "stop" => {
                    row["state"] = "stopping".into();
                    let _ = self.reactor.stop(owned.job, StopReason::Signal(15));
                }
                _ => return Err(Error::new("invalid job control request")),
            }
        }
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
pub fn public_job(row: &Value) -> Value {
    let archived = row["archived"] == true;
    let raw_state = row["state"].as_str().unwrap_or("unknown");
    let state = if raw_state == "exited" {
        "exited"
    } else if ["starting", "running", "stopping"].contains(&raw_state) && supervisor_alive(row) {
        raw_state
    } else {
        "unknown"
    };
    let exit_code = if state == "exited" {
        row.get("exit")
            .and_then(|v| v.as_i64())
            .or_else(|| row.get("exit_code").and_then(|v| v.as_i64()))
    } else {
        None
    };
    let pending = row.get("request").is_some_and(|v| !v.is_null());
    serde_json::json!({
        "id": row["id"],
        "label": crate::eventlog::clean(row["label"].as_str().unwrap_or("Job"), 120),
        "kind": "job",
        "state": state,
        "generation": row["generation"],
        "archived": archived,
        "exit_code": exit_code,
        "capabilities": {
            "logs": true,
            "interrupt": state == "running" && !pending,
            "stop": state == "running" && !pending,
            "restart": state == "exited" && !archived,
            "forget": state == "exited",
            "archive": state == "exited" && !archived,
            "restore": state == "exited" && archived,
        },
        "log_truncated": row["log_truncated"] == true,
        "log_limit": LOG_LIMIT,
        "log_retention": "latest generation only",
    })
}

fn supervisor_alive(row: &Value) -> bool {
    let Some(pid) = row["supervisor_pid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
    else {
        return false;
    };
    let Some(birth) = row["supervisor_identity"]
        .as_str()
        .filter(|v| !v.is_empty())
    else {
        return false;
    };
    crate::terminals::process(pid).is_ok_and(|owner| owner.birth == birth)
}

pub fn list_public_jobs(repo: &Path, archived: bool) -> Result<Vec<Value>> {
    crate::parent_only()?;
    let Some(store) = PrivateStore::open_optional(repo, "jobs", archived)? else {
        return Ok(vec![]);
    };
    let mut rows = Vec::new();
    for ident in store.identifiers()? {
        let _lock = store
            .store
            .lock_at(&PathBuf::from(&ident).join("lock"), Duration::from_secs(3))?;
        let row = store.record(&ident)?;
        require(row["kind"] == "job", "invalid job record kind")?;
        rows.push(public_job(&row));
    }
    Ok(rows)
}

pub fn read_job_logs(
    repo: &Path,
    ident: &str,
    generation: Option<u64>,
    maximum: usize,
) -> Result<Value> {
    crate::parent_only()?;
    let active = PrivateStore::open_optional(repo, "jobs", false)?;
    let archived = PrivateStore::open_optional(repo, "jobs", true)?;
    let store = match (active, archived) {
        (Some(a), _) if a.record(ident).is_ok() => a,
        (_, Some(h)) if h.record(ident).is_ok() => h,
        _ => return Err(Error::new("managed job record unavailable")),
    };
    let _lock = store
        .store
        .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
    let row = store.record(ident)?;
    require(row["kind"] == "job", "invalid job record kind")?;
    let recorded_gen = row["generation"]
        .as_u64()
        .ok_or_else(|| Error::new("invalid job generation"))?;
    if let Some(expected) = generation {
        require(recorded_gen == expected, "stale generation")?;
    }
    let output_rel = PathBuf::from(ident).join("output");
    let full_size = std::fs::symlink_metadata(store.root.join(&output_rel))
        .map_err(Error::new)?
        .len() as usize;
    let raw = store.store.read(&output_rel, LOG_LIMIT, false)?;
    let mut result = crate::terminals::preview(&raw, maximum)?;
    let truncated =
        result["truncated"] == true || row["log_truncated"] == true || full_size > raw.len();
    result["id"] = ident.into();
    result["kind"] = "job".into();
    result["generation"] = recorded_gen.into();
    result["truncated"] = truncated.into();
    result["retention"] = "latest generation only; first 1048576 bytes retained".into();
    Ok(result)
}

pub fn job_logs(repo: &Path, ident: &str, maximum: usize) -> Result<Value> {
    read_job_logs(repo, ident, None, maximum)
}

pub fn request_job_control(
    repo: &Path,
    ident: &str,
    generation: u64,
    action: &str,
) -> Result<Value> {
    crate::parent_only()?;
    require(generation >= 1, "exact job generation required")?;
    require(
        action == "interrupt" || action == "stop",
        "invalid job control action",
    )?;
    let store = PrivateStore::open(repo, "jobs", false, false)?;
    let _lock = store
        .store
        .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
    let row = store.record(ident)?;
    require(
        row["kind"] == "job"
            && row["generation"].as_u64() == Some(generation)
            && row["state"] == "running"
            && supervisor_alive(&row)
            && row.get("request").is_none_or(Value::is_null),
        "job cannot accept control request in current state",
    )?;
    let mut changed = row.clone();
    changed["request"] = action.into();
    store.update(ident, &row, &changed)?;
    Ok(public_job(&changed))
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_job(
    repo: &Path,
    session: &str,
    argv: Vec<OsString>,
    label: &str,
    cwd: Option<&Path>,
    service: &dyn crate::Services,
    stop: &std::sync::atomic::AtomicBool,
    executable: &Path,
) -> Result<Value> {
    crate::parent_only()?;
    require(
        !argv.is_empty()
            && argv.len() <= 256
            && !argv[0].is_empty()
            && argv.iter().all(|a| !a.as_encoded_bytes().contains(&0)),
        "an explicit argv recipe is required",
    )?;
    let binding = crate::terminals::session_proof(service, repo, session, stop)?;
    let canon_repo = std::fs::canonicalize(repo).map_err(Error::new)?;
    let canon_cwd = std::fs::canonicalize(cwd.unwrap_or(&canon_repo)).map_err(Error::new)?;
    let argv_strings: Vec<String> = argv
        .iter()
        .map(|a| {
            a.to_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("job argv must be UTF-8"))
        })
        .collect::<Result<_>>()?;
    let recipe = serde_json::json!({
        "repo": canon_repo.to_string_lossy(),
        "uid": u64::from(rustix::process::getuid().as_raw()),
        "cwd": canon_cwd.to_string_lossy(),
        "argv": argv_strings,
    });
    let recipe_bytes = crate::value::encoded(&recipe)?;
    require(recipe_bytes.len() <= MAX_RECORD, "job recipe is too large")?;
    let recipe_sha = crate::digest(&recipe_bytes);

    let store = PrivateStore::open(repo, "jobs", false, true)?;
    let row = store.allocate(
        "job",
        "job",
        &serde_json::json!({
            "session_proof": binding,
            "label": crate::eventlog::clean(label, 120),
            "generation": 1u64,
            "state": "starting",
            "log_truncated": false,
            "recipe_sha256": recipe_sha,
        }),
    )?;
    let ident = row["id"].as_str().unwrap().to_owned();
    let tx = store
        .store
        .lock_at(&PathBuf::from(&ident).join("lock"), Duration::from_secs(3))?;
    tx.write_json(
        &PathBuf::from(&ident).join("recipe"),
        &recipe,
        true,
        MAX_RECORD,
    )?;
    tx.write(&PathBuf::from(&ident).join("output"), b"", true, LOG_LIMIT)?;
    drop(tx);

    spawn_supervisor_for_generation(&store, &canon_repo, &ident, 1, &row, executable)?;
    wait_started(&store, &ident, 1)
}

pub fn spawn_restart_job(
    repo: &Path,
    session: &str,
    ident: &str,
    generation: u64,
    service: &dyn crate::Services,
    stop: &std::sync::atomic::AtomicBool,
    executable: &Path,
) -> Result<Value> {
    crate::parent_only()?;
    require(generation >= 1, "exact job generation required")?;
    let binding = crate::terminals::session_proof(service, repo, session, stop)?;
    let canon_repo = std::fs::canonicalize(repo).map_err(Error::new)?;
    let store = PrivateStore::open(repo, "jobs", false, false)?;
    let tx = store
        .store
        .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
    let old = store.record(ident)?;
    require(
        old["generation"].as_u64() == Some(generation) && archivable(&old),
        "only an exited generation can restart",
    )?;
    let new_gen = generation + 1;
    let mut updated = old.clone();
    updated["generation"] = new_gen.into();
    updated["state"] = "starting".into();
    updated["log_truncated"] = false.into();
    updated["session_proof"] = serde_json::to_value(binding).map_err(Error::new)?;
    if let Some(m) = updated.as_object_mut() {
        m.remove("exit");
        m.remove("exit_code");
        m.remove("completion");
        m.remove("request");
        m.remove("leader_pid");
    }
    tx.write(&PathBuf::from(ident).join("output"), b"", false, LOG_LIMIT)?;
    tx.write_json(
        &PathBuf::from(ident).join("record"),
        &updated,
        false,
        MAX_RECORD,
    )?;
    drop(tx);

    spawn_supervisor_for_generation(&store, &canon_repo, ident, new_gen, &updated, executable)?;
    wait_started(&store, ident, new_gen)
}

fn spawn_supervisor_for_generation(
    store: &PrivateStore,
    canon_repo: &Path,
    ident: &str,
    generation: u64,
    expected_row: &Value,
    executable: &Path,
) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let (read_gate, write_gate) = rustix::pipe::pipe().map_err(Error::new)?;
    let mut cmd = Command::new(executable);
    cmd.args(["panel", "job", "_supervise", "--repo"])
        .arg(canon_repo)
        .args(["--id", ident, "--generation", &generation.to_string()])
        .stdin(Stdio::from(read_gate))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .envs(crate::terminals::ordinary_environment());
    let mut child = cmd.spawn().map_err(Error::new)?;
    let identity = crate::terminals::process(child.id())?;
    let tx = store
        .store
        .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
    let current = store.record(ident)?;
    require(
        current == *expected_row,
        "job record changed before supervisor approval",
    )?;
    let mut changed = current;
    changed["supervisor_pid"] = child.id().into();
    changed["supervisor_identity"] = identity.birth.into();
    tx.write_json(
        &PathBuf::from(ident).join("record"),
        &changed,
        false,
        MAX_RECORD,
    )?;
    drop(tx);

    let mut gate_writer = std::fs::File::from(write_gate);
    gate_writer.write_all(b"1").map_err(Error::new)?;
    drop(gate_writer);
    let _ = child.try_wait();
    Ok(())
}

fn wait_started(store: &PrivateStore, ident: &str, generation: u64) -> Result<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let tx = store
            .store
            .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
        let row = store.record(ident)?;
        require(
            row["generation"].as_u64() == Some(generation),
            "job generation changed",
        )?;
        drop(tx);
        if row["state"] != "starting" {
            return Ok(public_job(&row));
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err(Error::new(
        "job launch not confirmed; inspect its state before retrying",
    ))
}

pub fn supervise_job(repo: &Path, ident: &str, generation: u64) -> Result<()> {
    use std::io::Read;
    let _ = rustix::process::setsid();
    let mut gate = [0u8; 1];
    let n = std::io::stdin().read(&mut gate).map_err(Error::new)?;
    if n != 1 || gate[0] != b'1' {
        return Ok(());
    }
    let mut jobs = Jobs::open(repo)?;
    let tx = jobs
        .store
        .store
        .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
    let row = jobs.store.record(ident)?;
    require(
        row["generation"].as_u64() == Some(generation)
            && row["state"] == "starting"
            && row["supervisor_pid"].as_u64() == Some(u64::from(std::process::id())),
        "supervisor claim mismatch",
    )?;
    let recipe = jobs
        .store
        .store
        .json(&PathBuf::from(ident).join("recipe"), MAX_RECORD, false)?
        .ok_or_else(|| Error::new("job recipe missing"))?;
    let cwd = PathBuf::from(
        recipe["cwd"]
            .as_str()
            .ok_or_else(|| Error::new("job recipe cwd invalid"))?,
    );
    let argv: Vec<OsString> = crate::value::array(&recipe["argv"])
        .iter()
        .filter_map(|v| v.as_str().map(OsString::from))
        .collect();
    require(!argv.is_empty(), "job recipe argv empty")?;

    let mut request = Request::new(argv);
    request.cwd = Some(cwd);
    request.scope = Scope::WholeTree;
    request.streams = Streams::Merged;
    request.capture.max_bytes = LOG_LIMIT;
    request.clear_environment = true;
    request.environment = crate::terminals::ordinary_environment();
    let started = jobs.reactor.start(request);
    let job = match started {
        Ok(job) => job,
        Err(_) => {
            let mut exited = row;
            exited["state"] = "exited".into();
            exited["completion"] = "reaped-all-children".into();
            exited["exit"] = 127.into();
            tx.write_json(
                &PathBuf::from(ident).join("record"),
                &exited,
                false,
                MAX_RECORD,
            )?;
            return Ok(());
        }
    };
    let mut running = row;
    running["state"] = "running".into();
    tx.write_json(
        &PathBuf::from(ident).join("record"),
        &running,
        false,
        MAX_RECORD,
    )?;
    jobs.owned.insert(ident.into(), Owned { job, row: running });
    drop(tx);

    loop {
        let state = jobs.poll(ident)?;
        if state["state"] == "exited" || state["state"] == "unknown" {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

pub fn archivable(row: &Value) -> bool {
    row["state"] == "exited" && row["completion"] == "reaped-all-children"
}

pub fn archive_job(repo: &Path, ident: &str, generation: Option<u64>) -> Result<Value> {
    crate::history::archive(repo, "jobs", ident, |row| {
        Ok(generation.is_none_or(|g| row["generation"].as_u64() == Some(g)) && archivable(row))
    })
}

pub fn restore_job(repo: &Path, ident: &str, generation: Option<u64>) -> Result<Value> {
    crate::history::restore(repo, "jobs", ident, |row| {
        Ok(generation.is_none_or(|g| row["generation"].as_u64() == Some(g)) && archivable(row))
    })
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
