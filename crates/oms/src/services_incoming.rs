//! Incoming builds borrow the existing managed scratch producer. The lease
//! proves local ownership and registration; lifecycle core owns payload writes.
use super::*;
use oms_command_lifecycle::IncomingSourceLease;
use oms_safe_fs::{ByteReadOptions, Root};
use std::collections::BTreeMap;

type Stamp = (u64, u64, u32, u32, u64, u64, i64, i64);
fn refuse(message: &str) -> Output {
    (2, vec![], format!("error: {message}\n").into_bytes())
}
fn cancellation(out: Output, stop: &AtomicBool) -> Output {
    if stop.load(Ordering::Acquire) {
        (130, vec![], b"incoming preparation interrupted\n".to_vec())
    } else {
        out
    }
}
fn marker(path: &Path) -> Result<(Vec<u8>, Stamp), Output> {
    let inspect = || -> Result<Stamp, Output> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|_| refuse("incoming scratch marker unavailable"))?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.nlink() != 1
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o022 != 0
            || meta.len() > 16 * 1024
        {
            return Err(refuse("unsafe incoming scratch marker"));
        }
        Ok((
            meta.dev(),
            meta.ino(),
            meta.uid(),
            meta.mode(),
            meta.nlink(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
        ))
    };
    let before = inspect()?;
    let root = Root::open(
        path.parent()
            .ok_or_else(|| refuse("incoming scratch parent missing"))?,
    )
    .map_err(|_| refuse("unsafe incoming scratch parent"))?;
    let raw = root
        .read_no_follow(
            Path::new(".oh-my-setting-tmp"),
            ByteReadOptions {
                missing_ok: false,
                max_bytes: 16 * 1024,
            },
        )
        .map_err(|_| refuse("incoming scratch marker read failed"))?;
    if before != inspect()? {
        return Err(refuse("incoming scratch marker changed"));
    }
    Ok((raw, before))
}
fn invoke(host: &CollabServices<'_>, args: &[OsString], stop: &AtomicBool) -> Output {
    if host.native.is_empty() {
        return refuse(NO_LAUNCHER);
    }
    let mut argv = host.native.clone();
    argv.push("scratch-worktree".into());
    argv.extend_from_slice(args);
    let (done, _) = match supervise(
        Job::new(argv, &host.source, Duration::from_secs(60), 64 * 1024),
        stop,
    ) {
        Ok(done) => done,
        Err(error) => {
            return (
                if error == "interrupted" { 130 } else { 2 },
                vec![],
                b"incoming scratch operation unconfirmed\n".to_vec(),
            );
        }
    };
    let code = if done.cleanup_complete() && done.output_complete() {
        stop_code(&done)
    } else {
        75
    };
    (
        code,
        done.stdout.bytes,
        done.stderr.map_or_else(Vec::new, |s| s.bytes),
    )
}

struct Lease<'a, 'h> {
    host: &'a CollabServices<'h>,
    original: PathBuf,
    source: PathBuf,
    revision: String,
    resolver: oms_reference::repository::Resolver,
    shared: PathBuf,
    marker: PathBuf,
    captured: (Vec<u8>, Stamp),
}
impl IncomingSourceLease for Lease<'_, '_> {
    fn source_root(&self) -> &Path {
        &self.source
    }
    fn recheck(&self, stop: &AtomicBool) -> Result<(), Output> {
        let result = (|| {
            if stop.load(Ordering::Acquire) {
                return Err((130, vec![], b"incoming preparation interrupted\n".to_vec()));
            }
            if std::fs::canonicalize(&self.source).ok().as_ref() != Some(&self.source)
                || marker(&self.marker)? != self.captured
                || self
                    .resolver
                    .state_repository(&self.source, true, stop)
                    .ok()
                    .as_ref()
                    != Some(&self.shared)
            {
                return Err(refuse("incoming scratch ownership or registration changed"));
            }
            let raw = std::str::from_utf8(&self.captured.0)
                .map_err(|_| refuse("invalid incoming scratch marker"))?;
            let mut fields = BTreeMap::new();
            for line in raw.lines() {
                let (name, value) = line
                    .split_once('=')
                    .ok_or_else(|| refuse("invalid incoming scratch marker"))?;
                if fields.insert(name, value).is_some() {
                    return Err(refuse("duplicate incoming scratch marker field"));
                }
            }
            let owner = std::process::id().to_string();
            let generation = oms_command_operations::lock::process_start(std::process::id())
                .ok_or_else(|| refuse("incoming owner generation unavailable"))?;
            if fields.get("pid") != Some(&owner.as_str())
                || fields.get("process-start") != Some(&generation.as_str())
                || fields.get("repo").map(Path::new) != Some(self.original.as_path())
                || fields.get("worktree").map(Path::new) != Some(self.source.as_path())
            {
                return Err(refuse("incoming scratch belongs to another owner"));
            }
            if marker(&self.marker)? != self.captured {
                return Err(refuse("incoming scratch marker changed"));
            }
            Ok(())
        })();
        if stop.load(Ordering::Acquire) {
            Err(cancellation(
                refuse("incoming preparation interrupted"),
                stop,
            ))
        } else {
            result
        }
    }
    fn close(self: Box<Self>, stop: &AtomicBool) -> Output {
        if let Err(out) = self.recheck(stop) {
            return out;
        }
        let git = oms_command_plan::repository::Git {
            group_snapshot: None,
            deadline: None,
            repo: &self.source,
            interrupted: stop,
            group_resolver: None,
        };
        if git.text(&["rev-parse", "HEAD"]).ok().as_deref() != Some(self.revision.as_str())
            || !git
                .text(&["status", "--porcelain", "--untracked-files=normal"])
                .is_ok_and(|s| s.is_empty())
        {
            return cancellation(
                refuse("incoming scratch changed; retained for recovery"),
                stop,
            );
        }
        invoke(
            self.host,
            &[
                "remove".into(),
                "--repo".into(),
                self.original.as_os_str().to_owned(),
                "--force".into(),
                self.source.as_os_str().to_owned(),
            ],
            stop,
        )
    }
}

pub(super) fn prepare<'a>(
    host: &'a CollabServices<'_>,
    original: &Path,
    revision: &str,
    stop: &AtomicBool,
) -> Result<Box<dyn IncomingSourceLease + 'a>, Output> {
    let result = (|| -> Result<Box<dyn IncomingSourceLease + 'a>, Output> {
        if stop.load(Ordering::Acquire) {
            return Err(cancellation(
                refuse("incoming preparation interrupted"),
                stop,
            ));
        }
        oms_command_peer::safety::parent()
            .map_err(|_| refuse("incoming source preparation requires a parent"))?;
        if !is_revision(revision) {
            return Err(refuse("invalid incoming source revision"));
        }
        let original = std::fs::canonicalize(original)
            .map_err(|_| refuse("incoming source root unavailable"))?;
        let mut resolver = oms_reference::repository::Resolver::from_env()
            .map_err(|_| refuse("incoming managed root unavailable"))?;
        resolver.child = false;
        resolver.inherited_room = None;
        resolver.execution_root = None;
        let shared = resolver
            .state_repository(&original, true, stop)
            .map_err(|_| refuse("incoming repository ownership unavailable"))?;
        let out = invoke(
            host,
            &[
                "add".into(),
                "--repo".into(),
                original.as_os_str().to_owned(),
                "--base".into(),
                revision.into(),
                "--owner-pid".into(),
                std::process::id().to_string().into(),
            ],
            stop,
        );
        if out.0 != 0 {
            return Err(out);
        }
        let path = std::str::from_utf8(&out.1)
            .map_err(|_| refuse("incoming scratch response is not UTF-8"))?
            .trim_end_matches(['\r', '\n']);
        if path.is_empty() || path.contains(['\r', '\n', '\0']) {
            return Err(refuse("invalid incoming scratch response"));
        }
        let source = PathBuf::from(path);
        if !source.is_absolute() || source == original {
            return Err(refuse("incoming scratch must be a separate absolute path"));
        }
        let marker_path = source
            .parent()
            .ok_or_else(|| refuse("incoming scratch marker unavailable"))?
            .join(".oh-my-setting-tmp");
        let captured = marker(&marker_path)?;
        let lease = Lease {
            host,
            original,
            source,
            revision: revision.into(),
            resolver,
            shared,
            marker: marker_path,
            captured,
        };
        lease.recheck(stop)?;
        Ok(Box::new(lease))
    })();
    result.map_err(|out| cancellation(out, stop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::process::Command;

    #[test]
    #[ignore = "subprocess native scratch producer; invoked by its owning fixture"]
    fn scratch_producer() {
        let args = std::env::args_os()
            .skip_while(|arg| arg != "--")
            .skip(1)
            .collect::<Vec<_>>();
        if args.first().is_none_or(|arg| arg != "scratch-worktree") {
            std::process::exit(2);
        }
        let cwd = std::env::current_dir().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let control = ControlServices::new(&stop, &cwd).unwrap();
        let out = oms_command_landing::run_with_services(
            "scratch-worktree",
            &args[1..],
            &cwd,
            &cwd,
            &stop,
            &control,
        );
        std::io::stderr().write_all(&out.2).unwrap();
        std::io::stdout()
            .write_all(b"OMS_INCOMING_OUTPUT:")
            .unwrap();
        std::io::stdout().write_all(&out.1).unwrap();
        std::io::stdout().flush().unwrap();
        std::process::exit(out.0);
    }

    #[test]
    fn managed_incoming_lease_tracks_native_owner_and_preserves_changed_work() {
        const NAME: &str = "services_collab::incoming::tests::managed_incoming_lease_tracks_native_owner_and_preserves_changed_work";
        let Some(root) = std::env::var_os("OMS_INCOMING_LEASE_FIXTURE") else {
            let root =
                std::env::temp_dir().join(format!("oms-incoming-lease-{}", std::process::id()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            struct Cleanup(PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let f = Cleanup(std::fs::canonicalize(root).unwrap());
            for name in ["home", "cache", "config", "data", "state", "bin"] {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(f.0.join(name))
                    .unwrap();
            }
            let path = std::env::var_os("PATH").unwrap();
            let real_git = std::env::split_paths(&path)
                .map(|p| p.join("git"))
                .find(|p| p.is_file())
                .unwrap();
            let wrapper = br##"#!/bin/sh
root="$OMS_INCOMING_LEASE_FIXTURE"
if [ -f "$root/hold-git" ]; then
  read phase < "$root/hold-git"
  if [ "$phase" = resolver ] || [ "$1" = --no-pager ]; then
    : > "$root/git-entered"
    while [ ! -e "$root/git-release" ]; do sleep 0.01; done
  fi
fi
exec "$OMS_INCOMING_REAL_GIT" "$@"
"##;
            std::fs::write(f.0.join("bin/git"), wrapper).unwrap();
            std::fs::set_permissions(f.0.join("bin/git"), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let path = std::env::join_paths(
                std::iter::once(f.0.join("bin")).chain(std::env::split_paths(&path)),
            )
            .unwrap();
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env_clear()
                .env("PATH", path)
                .env(
                    "OMS_INCOMING_REAL_GIT",
                    std::fs::canonicalize(real_git).unwrap(),
                )
                .env("HOME", f.0.join("home"))
                .env("XDG_CACHE_HOME", f.0.join("cache"))
                .env("XDG_CONFIG_HOME", f.0.join("config"))
                .env("XDG_DATA_HOME", f.0.join("data"))
                .env("XDG_STATE_HOME", f.0.join("state"))
                .env("OMS_DELEGATE_WORKTREE_ROOT", f.0.join("worktrees"))
                .env("OMS_LOCK_DIR", f.0.join("locks"))
                .env("TMPDIR", &f.0)
                .env("OMS_INCOMING_LEASE_FIXTURE", &f.0)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        };
        let root = std::fs::canonicalize(root).unwrap();
        let source = root.join("source");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&source)
            .unwrap();
        let git = |repo: &Path, args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&source, &["init", "-q"]);
        git(
            &source,
            &["config", "user.email", "fixture@example.invalid"],
        );
        git(&source, &["config", "user.name", "fixture"]);
        std::fs::write(source.join("VERSION"), b"before").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-qm", "before"]);
        let before = git(&source, &["rev-parse", "HEAD"]);
        std::fs::write(source.join("VERSION"), b"incoming").unwrap();
        git(&source, &["commit", "-qam", "incoming"]);
        let revision = git(&source, &["rev-parse", "HEAD"]);
        let stop = Arc::new(AtomicBool::new(false));
        let script = r#"captured=$("$0" --ignored --exact services_collab::incoming::tests::scratch_producer --nocapture -- "$@") || exit $?
case "$captured" in *OMS_INCOMING_OUTPUT:*) printf '%s\n' "${captured##*OMS_INCOMING_OUTPUT:}" ;; *) exit 2 ;; esac"#;
        let host = CollabServices::new(&stop, &source)
            .unwrap()
            .with_native(vec![
                "/bin/sh".into(),
                "-c".into(),
                script.into(),
                std::env::current_exe().unwrap().into_os_string(),
            ]);
        let make = || match prepare(&host, &source, &revision, &stop) {
            Ok(lease) => lease,
            Err(out) => panic!("{}: {}", out.0, String::from_utf8_lossy(&out.2)),
        };
        let lease = make();
        let path = lease.source_root().to_owned();
        assert_eq!(git(&path, &["rev-parse", "HEAD"]), revision);
        assert_eq!(lease.recheck(&AtomicBool::new(true)).unwrap_err().0, 130);
        lease.recheck(&stop).unwrap();
        let out = lease.close(&stop);
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        assert!(!path.exists());
        for phase in ["recheck", "prepare", "close"] {
            let lease = make();
            std::fs::write(
                root.join("hold-git"),
                if phase == "close" {
                    "close\n"
                } else {
                    "resolver\n"
                },
            )
            .unwrap();
            let code = std::thread::scope(|scope| {
                let signal = scope.spawn(|| {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !root.join("git-entered").exists() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    stop.store(true, Ordering::Release);
                    std::fs::write(root.join("git-release"), b"release").unwrap();
                    assert!(
                        root.join("git-entered").exists(),
                        "{phase}: Git gate not reached"
                    );
                });
                let code = match phase {
                    "recheck" => lease.recheck(&stop).unwrap_err().0,
                    "prepare" => match prepare(&host, &source, &revision, &stop) {
                        Err(out) => out.0,
                        Ok(_) => panic!("interruption accepted"),
                    },
                    "close" => lease.close(&stop).0,
                    _ => unreachable!(),
                };
                signal.join().unwrap();
                code
            });
            assert_eq!(
                code, 130,
                "{phase}: mid-query cancellation must retain its status"
            );
            stop.store(false, Ordering::Release);
            for file in ["hold-git", "git-entered", "git-release"] {
                std::fs::remove_file(root.join(file)).unwrap();
            }
        }
        for case in ["owner", "untracked", "tracked", "revision", "registration"] {
            let lease = make();
            let path = lease.source_root().to_owned();
            match case {
                "owner" => {
                    let marker = path.parent().unwrap().join(".oh-my-setting-tmp");
                    let raw = std::fs::read_to_string(&marker).unwrap();
                    std::fs::write(
                        marker,
                        raw.replace(&format!("pid={}\n", std::process::id()), "pid=1\n"),
                    )
                    .unwrap();
                    assert!(lease.recheck(&stop).is_err());
                }
                "untracked" => std::fs::write(path.join("user-work"), b"preserve").unwrap(),
                "tracked" => std::fs::write(path.join("VERSION"), b"preserve").unwrap(),
                "revision" => {
                    git(&path, &["checkout", "--detach", &before]);
                }
                "registration" => {
                    std::fs::write(
                        path.join(".git"),
                        "gitdir: /missing-incoming-registration\n",
                    )
                    .unwrap();
                    assert!(lease.recheck(&stop).is_err());
                }
                _ => unreachable!(),
            }
            assert_eq!(lease.close(&stop).0, 2, "{case}");
            assert!(
                path.is_dir(),
                "{case}: unproven or changed work must remain"
            );
        }
        assert_eq!(git(&source, &["rev-parse", "HEAD"]), revision);
        assert!(git(&source, &["status", "--porcelain"]).is_empty());
        let host = CollabServices::new(&stop, &source).unwrap();
        assert!(prepare(&host, &source, &revision, &stop).is_err());
        // A permissive marker is not accepted merely because its bytes parse.
        let marker = root.join("permissive/.oh-my-setting-tmp");
        std::fs::create_dir(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"kind=oh-my-setting-temp\n").unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(super::marker(&marker).is_err());
    }
}
