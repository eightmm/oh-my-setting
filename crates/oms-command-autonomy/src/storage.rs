use crate::{Error, Result, Services, checkpoint, require, value};
use oms_command_plan::storage::Store;
use oms_command_plan::{Action, Context as PlanContext, Plan, Request};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, MutexGuard, PoisonError};

#[cfg(test)]
mod snapshot_race_tests {
    use super::*;
    use oms_command_plan::services::{GroupMembership, GroupResolver};
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Condvar;
    use std::time::Duration;

    static STATE: Mutex<(bool, bool)> = Mutex::new((false, false));
    static WAKE: Condvar = Condvar::new();
    thread_local! { static READER: Cell<u8> = const { Cell::new(0) }; }

    fn timeout() -> Duration {
        Duration::from_secs(
            std::env::var("PLAN_READ_TEST_TIMEOUT")
                .or_else(|_| std::env::var("OMS_TEST_CALL_TIMEOUT"))
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
        )
    }

    fn group(gid: u32) -> oms_command_plan::Result<GroupMembership> {
        READER.with(|reader| match reader.get() {
            1 => reader.set(2),
            2 => {
                reader.set(0);
                let mut state = STATE.lock().unwrap();
                state.0 = true;
                WAKE.notify_all();
                let (state, _) = WAKE.wait_timeout_while(state, timeout(), |s| !s.1).unwrap();
                assert!(state.1, "publisher did not release reader");
            }
            _ => {}
        });
        let uid = rustix::process::getuid().as_raw();
        Ok(GroupMembership {
            gid,
            explicit_member_uids: vec![uid],
            primary_member_uids: vec![uid],
        })
    }
    struct Host;
    impl oms_command_plan::Services for Host {
        fn group_membership_resolver(&self) -> Option<GroupResolver> {
            Some(group)
        }
    }
    impl Services for Host {}

    #[test]
    fn worker_plan_value_snapshot_is_guarded_against_publication() {
        let ambient = |key: &std::ffi::OsStr| {
            let key = key.to_string_lossy();
            key.starts_with("OMS_") || key.starts_with("OH_MY_SETTING_")
        };
        if std::env::vars_os().any(|(key, _)| ambient(&key)) {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            if let Some(timeout) = std::env::var_os("OMS_TEST_CALL_TIMEOUT") {
                child.env("PLAN_READ_TEST_TIMEOUT", timeout);
            }
            for (key, _) in std::env::vars_os().filter(|(key, _)| ambient(key)) {
                child.env_remove(key);
            }
            let out = child.args(["--exact", "storage::snapshot_race_tests::worker_plan_value_snapshot_is_guarded_against_publication", "--nocapture"]).output().unwrap();
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let path = std::env::temp_dir().join(value::random("oms-plan-read-").unwrap());
        std::fs::create_dir(&path).unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&path)
                .status()
                .unwrap()
                .success()
        );
        let host = Host;
        let stop = AtomicBool::new(false);
        let command = |args: &[&str]| {
            let words = args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            let (code, _, err) = oms_command_plan::run_with_services(&words, &path, &stop, &host);
            assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        };
        command(&["init", "--goal", "race"]);
        command(&[
            "add",
            "--id",
            "t1",
            "--title",
            "t1",
            "--allowed",
            "a/",
            "--verify",
            "true",
        ]);
        for relative in ["", ".oms", ".oms/plan"] {
            std::fs::set_permissions(path.join(relative), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        std::fs::set_permissions(path.join(ACTIVE), std::fs::Permissions::from_mode(0o660))
            .unwrap();
        let gates = Gates::default();
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut ctx = Context::new(path.clone(), path.clone(), &host, &stop).unwrap();
                ctx.gates = Some(&gates);
                READER.with(|reader| reader.set(1));
                ctx.plan_value()
            });
            let state = STATE.lock().unwrap();
            let (state, _) = WAKE.wait_timeout_while(state, timeout(), |s| !s.0).unwrap();
            assert!(state.0, "worker did not open Plan");
            drop(state);
            let lock = oms_lock::Lock::for_file(
                &path.join(ACTIVE),
                oms_lock::Settings::from_env().unwrap(),
            )
            .unwrap()
            .try_acquire();
            let guarded = matches!(lock, Err(oms_lock::Error::Busy));
            drop(lock);
            if guarded {
                STATE.lock().unwrap().1 = true;
                WAKE.notify_all();
            }
            command(&["claim", "--id", "t1", "--provider", "codex"]);
            if !guarded {
                STATE.lock().unwrap().1 = true;
                WAKE.notify_all();
            }
            let read = reader.join().unwrap();
            assert!(guarded, "worker Plan read did not hold writer lock");
            assert!(read.is_ok());
        });
        std::fs::remove_dir_all(path).unwrap();
    }
}

pub const MAX_FILE: usize = 1024 * 1024;
pub const MAX_LEDGER: usize = 16 * 1024 * 1024;
pub const ACTIVE: &str = ".oms/plan/tasks.json";
pub const PROGRESS: &str = ".oms/plan/progress.jsonl";
pub const RECEIPT: &str = ".oms/plan/autopilot-run.json";

/// In-process serialization for `plan-run --parallel`. `plan` covers one plan
/// state call and `landing` the single integration writer; neither is ever
/// held across worker execution. A poisoned gate still guards: a panicking
/// worker must not take its siblings down.
#[derive(Default)]
pub struct Gates {
    plan: Mutex<()>,
    landing: Mutex<()>,
}
impl Gates {
    pub fn plan(&self) -> MutexGuard<'_, ()> {
        self.plan.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub fn landing(&self) -> MutexGuard<'_, ()> {
        self.landing.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct Context<'a> {
    pub repo: PathBuf,
    pub state: PathBuf,
    pub source: Store,
    pub store: Store,
    pub services: &'a dyn Services,
    pub interrupted: &'a AtomicBool,
    pub owner_id: String,
    pub run_id: String,
    pub cycle: Option<u64>,
    pub peer_wall: Option<crate::planning::ProviderWall>,
    pub gates: Option<&'a Gates>,
}
impl<'a> Context<'a> {
    pub fn new(
        repo: PathBuf,
        state: PathBuf,
        services: &'a dyn Services,
        interrupted: &'a AtomicBool,
    ) -> Result<Self> {
        let groups = services.group_membership_resolver();
        Ok(Self {
            source: Store::open(&repo)?.with_group_resolver(groups),
            store: Store::open(&state)?.with_group_resolver(groups),
            repo,
            state,
            services,
            interrupted,
            owner_id: String::new(),
            run_id: String::new(),
            cycle: None,
            peer_wall: None,
            gates: None,
        })
    }
    pub fn parent(&self) -> Result<()> {
        checkpoint(self.interrupted)?;
        let args = vec!["init".into()];
        oms_command_task::consumers::parent_guard(&args)
            .map_err(|e| Error::new(e.code, e.to_string()))
    }
    pub fn plan(&self) -> Result<Plan<'_>> {
        let mut req = Request::new(Action::Show);
        req.repo = Some(self.repo.clone());
        let mut context = PlanContext::from_environment(&req, &self.repo)?;
        context.execution_repo = self.repo.clone();
        context.state_repo = self.state.clone();
        context.plan_file = self.state.join(ACTIVE);
        if !self.owner_id.is_empty() {
            context.owner_id = self.owner_id.clone();
        }
        if !self.run_id.is_empty() {
            context.run_id = self.run_id.clone();
            context.cycle = self.cycle;
        }
        Ok(Plan::open(context, self.services, self.interrupted)?)
    }
    pub fn action(&self, req: &Request) -> Result<crate::Output> {
        checkpoint(self.interrupted)?;
        let _gate = self.gates.map(Gates::plan);
        let out = self.plan()?.execute(req)?;
        Ok(crate::Output {
            code: out.code,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
    pub fn ready_snapshot(&self) -> Result<crate::Output> {
        checkpoint(self.interrupted)?;
        let _gate = self.gates.map(Gates::plan);
        let out = self.plan()?.ready_with_locked_snapshot()?;
        Ok(crate::Output {
            code: out.code,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
    pub fn plan_value(&self) -> Result<Value> {
        let _gate = self.gates.map(Gates::plan);
        let plan = self.plan()?;
        let snapshot = if self.gates.is_some() {
            plan.load_with_locked_snapshot()?
        } else {
            plan.load()?
        };
        value::from_plan(&snapshot.plan)
    }
    pub fn git(&self, args: &[&str]) -> Result<String> {
        Ok(self.plan()?.git().text(args)?)
    }
    pub fn bytes(&self, path: &Path, limit: usize) -> Result<Vec<u8>> {
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.repo.join(path)
        };
        if full.starts_with(&self.repo) {
            return Ok(self.source.read(full, limit)?);
        }
        if full.starts_with(&self.state) {
            return Ok(self.store.read(full, limit)?);
        }
        let parent = full
            .parent()
            .ok_or_else(|| Error::new(2, "file has no parent"))?;
        let parent = std::fs::canonicalize(parent)
            .map_err(|_| Error::new(2, "cannot resolve file parent"))?;
        let name = full
            .file_name()
            .ok_or_else(|| Error::new(2, "file has no name"))?;
        let root =
            oms_safe_fs::Root::open(parent).map_err(|_| Error::new(2, "unsafe file parent"))?;
        root.read_path(Path::new(name), limit, oms_safe_fs::Policy::manifest())
            .map_err(|_| Error::new(2, "file must be owned, bounded and regular without links"))
    }
    pub fn rows(&self, path: &str) -> Result<Vec<Value>> {
        let raw = self.store.optional(path, MAX_LEDGER)?.unwrap_or_default();
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        require(
            raw.ends_with(b"\n"),
            "incomplete ledger row; reconcile before retry",
        )?;
        raw.split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let v = value::parse(line)?;
                require(v.is_object(), "ledger row is not an object")?;
                Ok(v)
            })
            .collect()
    }
    pub fn append(&self, path: &str, v: &Value) -> Result<()> {
        // Store::append uses the existing native durable JSONL writer. Its
        // parent must exist; this harmless empty CAS creates only that parent.
        if self.store.optional(path, MAX_LEDGER)?.is_none() {
            self.store.publish_once(path, b"", 0)?;
        }
        self.store.append(path, &value::plan(v)?)?;
        Ok(())
    }
    pub fn lock(&self, path: &str) -> Result<oms_lock::Guard> {
        checkpoint(self.interrupted)?;
        let settings = oms_lock::Settings::from_env().map_err(|e| Error::new(2, e.to_string()))?;
        oms_lock::Lock::for_file(&self.state.join(path), settings)
            .and_then(|lock| lock.acquire(None))
            .map_err(|e| Error::new(2, e.to_string()))
    }
    pub fn clean(&self) -> Result<()> {
        self.plan()?.git().guard()?;
        require(
            self.git(&["status", "--porcelain", "--untracked-files=normal"])?
                .is_empty(),
            "working tree must be clean",
        )
    }
}
