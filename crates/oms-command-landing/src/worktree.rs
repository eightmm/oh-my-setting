//! Managed scratch registration. The marker precedes Git registration and is
//! removed last; `.git` and the registry backpointer must describe one checkout.
use crate::{Context, Error, Output, Result, cli::Options, git::Git, parent, require, storage};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(crate) fn common(git: &Git<'_>) -> Result<PathBuf> {
    let p = PathBuf::from(git.text(&["rev-parse", "--git-common-dir"])?);
    Ok(std::fs::canonicalize(if p.is_absolute() {
        p
    } else {
        git.repo.join(p)
    })?)
}
pub(crate) fn registry_lock(git: &Git<'_>) -> Result<oms_lock::Guard> {
    storage::lock(&common(git)?.join("worktrees"), git.interrupted)
}
fn root() -> Result<PathBuf> {
    let p = oms_reference::repository::Resolver::from_env()?.managed_root;
    require(
        p.is_absolute(),
        "delegate worktree root must be an absolute path",
    )?;
    Ok(p)
}
fn marker(dir: &Path) -> Result<BTreeMap<String, String>> {
    let raw = storage::read(dir, Path::new(".oh-my-setting-tmp"), false)?;
    require(raw.len() <= 16 * 1024, "scratch marker is too large")?;
    let text = std::str::from_utf8(&raw).map_err(|_| Error::new(2, "invalid scratch marker"))?;
    let mut fields = BTreeMap::new();
    for l in text.lines() {
        if let Some((k, v)) = l.split_once('=') {
            require(
                fields.insert(k.into(), v.into()).is_none(),
                "duplicate scratch marker field",
            )?;
        }
    }
    require(
        fields
            .get("kind")
            .is_some_and(|s| s == "oh-my-setting-temp")
            && fields.get("temporary").is_some_and(|s| s == "1"),
        "not a scratch marker",
    )?;
    Ok(fields)
}
fn value<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    fields
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| Error::new(2, "incomplete scratch marker"))
}
fn owner(c: &Context<'_>, o: &Options) -> Result<u32> {
    if o.has("owner-pid") {
        let p = o
            .get("owner-pid")
            .parse::<u32>()
            .map_err(|_| Error::new(2, "--owner-pid must be a positive process id"))?;
        require(
            p > 0
                && p != std::process::id()
                && oms_command_operations::lock::pid_alive(i64::from(p)),
            "owner process is not running or cannot outlive this call",
        )?;
        return Ok(p);
    }
    let mut pid = rustix::process::getppid()
        .map(|p| p.as_raw_nonzero().get() as u32)
        .ok_or_else(|| Error::new(2, "cannot find a session owner"))?;
    let mut last = None;
    for _ in 0..16 {
        if pid <= 1 {
            break;
        }
        let out = crate::git::process(
            c.cwd,
            vec![
                "ps".into(),
                "-o".into(),
                "ppid=".into(),
                "-o".into(),
                "comm=".into(),
                "-p".into(),
                pid.to_string().into(),
            ],
            None,
            std::time::Duration::from_secs(5),
            c.interrupted,
            &[],
        )?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.split_whitespace();
        let next = parts.next().and_then(|s| s.parse::<u32>().ok());
        let name = parts
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_start_matches('-');
        if ![
            "bash", "sh", "zsh", "dash", "ksh", "mksh", "fish", "env", "timeout", "nohup",
        ]
        .contains(&name)
            && !name.is_empty()
        {
            return Ok(pid);
        }
        last = Some(pid);
        if let Some(next) = next {
            pid = next;
        } else {
            break;
        }
    }
    last.ok_or_else(|| Error::new(2, "cannot find a session process to own the worktree"))
}
fn validate(
    c: &Context<'_>,
    base: &Path,
    dir: &Path,
    fields: &BTreeMap<String, String>,
    existing: bool,
) -> Result<(PathBuf, PathBuf)> {
    let physical = std::fs::canonicalize(dir)?;
    require(
        physical.parent() == Some(base)
            && !std::fs::symlink_metadata(dir)?.file_type().is_symlink()
            && std::fs::metadata(dir)?.uid() == rustix::process::getuid().as_raw(),
        "scratch directory is outside the owned delegate root",
    )?;
    let leaf = physical.file_name().unwrap().to_string_lossy();
    require(
        leaf.starts_with("oh-my-setting-scratch."),
        "not a scratch worktree",
    )?;
    let suffix = leaf.rsplit('.').next().unwrap();
    let worktree = PathBuf::from(value(fields, "worktree")?);
    require(
        worktree == physical.join(format!("wt-{suffix}")) || worktree == physical.join("wt"),
        "marker does not describe the scratch worktree",
    )?;
    let repo = Git::root(Path::new(value(fields, "repo")?), c.interrupted)?;
    let git = Git::new(&repo, c.interrupted);
    if existing {
        require(
            std::fs::canonicalize(&worktree)? == worktree
                && !std::fs::symlink_metadata(&worktree)?
                    .file_type()
                    .is_symlink(),
            "unsafe scratch worktree path",
        )?;
        let pointer = storage::read(&worktree, Path::new(".git"), false)?;
        let pointer = std::str::from_utf8(&pointer)
            .map_err(|_| Error::new(2, "invalid .git pointer"))?
            .trim();
        let pointer = pointer
            .strip_prefix("gitdir: ")
            .ok_or_else(|| Error::new(2, "scratch .git must be a registry pointer"))?;
        let gitmeta = std::fs::canonicalize(worktree.join(pointer))?;
        let common_path = common(&git)?;
        require(
            gitmeta.parent() == Some(common_path.join("worktrees").as_path()),
            "scratch gitmeta leaves the worktree registry",
        )?;
        let back = storage::read(&gitmeta, Path::new("gitdir"), false)?;
        require(
            Path::new(
                std::str::from_utf8(&back)
                    .map_err(|_| Error::new(2, "invalid registry backpointer"))?
                    .trim(),
            ) == worktree.join(".git"),
            "marker and Git registration disagree",
        )?;
        require(
            common(&Git::new(&worktree, c.interrupted))? == common_path,
            "scratch belongs to another Git common directory",
        )?;
        require(
            git.text(&["worktree", "list", "--porcelain"])?
                .lines()
                .any(|l| {
                    l.strip_prefix("worktree ")
                        .is_some_and(|p| Path::new(p) == worktree)
                }),
            "scratch is not registered",
        )?;
    }
    Ok((repo, worktree))
}
const HYGIENE_STAMP: &str = ".oms-scratch-hygiene.stamp";
const HYGIENE_INTERVAL_SECS: u64 = 3600;

fn claim_hygiene_slot(base: &Path, now: u64) -> bool {
    let Ok(root) = storage::root(base) else {
        return false;
    };
    let options = oms_safe_fs::MutationOptions {
        read: oms_safe_fs::ByteReadOptions {
            missing_ok: true,
            max_bytes: 64,
        },
        create_parent: false,
        max_output: 64,
        ..oms_safe_fs::MutationOptions::default()
    };
    root.atomic_mutate(
        Path::new(HYGIENE_STAMP),
        options,
        |current| -> std::result::Result<Vec<u8>, ()> {
            let last = std::str::from_utf8(current)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());
            if let Some(last) = last
                && now.saturating_sub(last) < HYGIENE_INTERVAL_SECS
            {
                return Err(());
            }
            Ok(format!("{now}\n").into_bytes())
        },
    )
    .is_ok()
}

fn hygiene_pass(c: &Context<'_>, base: &Path, repo: &Path) -> Vec<u8> {
    if crate::child() || c.interrupted.load(std::sync::atomic::Ordering::Relaxed) {
        return Vec::new();
    }
    if !claim_hygiene_slot(base, crate::timestamp()) {
        return Vec::new();
    }
    let Ok(report) = c.services.cleanup(repo, true, c.interrupted) else {
        return Vec::new();
    };
    let removed = match report.get("removable") {
        crate::V::Num(n) => n.parse::<usize>().unwrap_or(0),
        _ => 0,
    };
    if removed > 0 {
        format!("scratch-worktree: cleaned {removed} item(s)\n").into_bytes()
    } else {
        Vec::new()
    }
}

pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    parent()?;
    let action = o.action("");
    require(
        ["add", "remove", "list"].contains(&action),
        "scratch-worktree requires add, remove or list",
    )?;
    require(!o.has("base") || action == "add", "--base applies to add")?;
    require(
        !o.has("owner-pid") || action == "add",
        "--owner-pid applies to add",
    )?;
    require(
        !o.has("force") || action == "remove",
        "--force applies to remove",
    )?;
    let base = root()?;
    if action == "add" {
        require(o.words.len() == 1, "unexpected scratch-worktree argument")?;
        let repo = c.repo(o)?;
        let git = Git::new(&repo, c.interrupted);
        git.safe()?;
        let sha = git.commit(o.value("base", "HEAD"))?;
        let pid = owner(c, o)?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&base)?;
        require(
            !std::fs::symlink_metadata(&base)?.file_type().is_symlink(),
            "delegate root must not be a symbolic link",
        )?;
        if std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT").is_none() {
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))?;
        }
        let base = std::fs::canonicalize(&base)?;
        let suffix = oms_command_plan::storage::random_hex(8)?;
        let dir = base.join(format!("oh-my-setting-scratch.{suffix}"));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let worktree = dir.join(format!("wt-{suffix}"));
        let mut body = format!(
            "kind=oh-my-setting-temp\npid={pid}\nrepo={}\nworktree={}\ntemporary=1\n",
            repo.display(),
            worktree.display()
        );
        if let Some(start) = oms_command_operations::lock::process_start(pid) {
            body += &format!("process-start={start}\n");
        }
        storage::replace(&dir, Path::new(".oh-my-setting-tmp"), None, body.as_bytes())?;
        let _registry = registry_lock(&git)?;
        if let Err(e) = git.raw(&[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--quiet"),
            OsStr::new("--detach"),
            worktree.as_os_str(),
            OsStr::new(&sha),
        ]) {
            if !worktree.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            return Err(e);
        }
        drop(_registry);
        let stderr = hygiene_pass(c, &base, &repo);
        return Ok(Output {
            code: 0,
            stdout: format!("{}\n", worktree.display()).into_bytes(),
            stderr,
        });
    }
    if !base.exists() {
        return if action == "list" {
            Ok(Output::default())
        } else {
            Err(Error::new(2, "delegate root does not exist"))
        };
    }
    let base = std::fs::canonicalize(base)?;
    if action == "list" {
        require(o.words.len() == 1, "unexpected scratch-worktree argument")?;
        let filter = if o.has("repo") {
            Some(c.repo(o)?)
        } else {
            None
        };
        let mut out = String::new();
        let mut entries: Vec<_> = std::fs::read_dir(&base)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("oh-my-setting-scratch.")
            {
                continue;
            }
            let Ok(fields) = marker(&entry.path()) else {
                continue;
            };
            let Ok(pid) = value(&fields, "pid").unwrap_or("").parse::<u32>() else {
                continue;
            };
            if !oms_command_operations::lock::pid_alive(i64::from(pid)) {
                continue;
            }
            if let Some(start) = fields.get("process-start")
                && oms_command_operations::lock::process_start(pid).is_some_and(|s| s != *start)
            {
                continue;
            }
            if let Ok((repo, worktree)) = validate(c, &base, &entry.path(), &fields, true)
                && filter.as_ref().is_none_or(|p| p == &repo)
            {
                out += &format!(
                    "{}\towner={pid}\trepo={}\n",
                    worktree.display(),
                    repo.display()
                );
            }
        }
        return Ok(Output::text(out));
    }
    require(
        o.words.len() == 2,
        "remove requires one scratch worktree path",
    )?;
    let target = c.cwd.join(&o.words[1]);
    let target = if target.exists() {
        std::fs::canonicalize(target)?
    } else {
        let p = std::fs::canonicalize(target.parent().unwrap())?;
        p.join(target.file_name().unwrap())
    };
    let dir = if target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .starts_with("oh-my-setting-scratch.")
    {
        target.clone()
    } else {
        target.parent().unwrap().to_owned()
    };
    let fields = marker(&dir)?;
    let exists = Path::new(value(&fields, "worktree")?).exists();
    let (repo, worktree) = validate(c, &base, &dir, &fields, exists)?;
    require(
        target == dir || target == worktree,
        "requested path disagrees with its scratch marker",
    )?;
    if o.has("repo") {
        require(
            c.repo(o)? == repo,
            "scratch worktree belongs to another repository",
        )?;
    }
    let extra = std::fs::read_dir(&dir)?
        .flatten()
        .any(|e| e.path() != worktree && e.file_name() != ".oh-my-setting-tmp");
    require(
        !extra || o.has("force"),
        "scratch residue has additional or unrecognized children; refusing",
    )?;
    let git = Git::new(&repo, c.interrupted);
    let _registry = registry_lock(&git)?;
    if exists {
        let mut args = vec![OsStr::new("worktree"), OsStr::new("remove")];
        if o.has("force") {
            args.push(OsStr::new("--force"));
        }
        args.push(worktree.as_os_str());
        git.raw(&args)?;
    } else {
        let common = common(&git)?;
        if common.join("worktrees").exists() {
            for entry in std::fs::read_dir(common.join("worktrees"))?.flatten() {
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let raw = storage::read(&entry.path(), Path::new("gitdir"), true)?;
                if raw.strip_suffix(b"\n").unwrap_or(&raw)
                    == worktree.join(".git").as_os_str().as_encoded_bytes()
                {
                    std::fs::remove_dir_all(entry.path())?;
                }
            }
        }
    }
    if !extra {
        std::fs::remove_file(dir.join(".oh-my-setting-tmp"))?;
        std::fs::remove_dir(&dir)?;
    }
    Ok(Output::text(format!("removed: {}\n", worktree.display())))
}

/// Independent exact-commit checkout: no source registry/index mutation.
pub(crate) struct Sandbox {
    _temporary: storage::Temp,
    pub path: PathBuf,
}
impl Sandbox {
    pub fn new(
        repo: &Path,
        sha: &str,
        interrupted: &std::sync::atomic::AtomicBool,
    ) -> Result<Self> {
        Git::new(repo, interrupted).transport_safe()?;
        let temporary = storage::Temp::new("oh-my-setting-land")?;
        let path = temporary.path.join("worktree");
        let git = Git::new(temporary.path.as_path(), interrupted);
        git.raw(&[
            OsStr::new("clone"),
            OsStr::new("--quiet"),
            OsStr::new("--no-local"),
            OsStr::new("--no-hardlinks"),
            OsStr::new("--no-checkout"),
            OsStr::new("--"),
            repo.as_os_str(),
            path.as_os_str(),
        ])?;
        let clone = Git::new(&path, interrupted);
        clone.raw(&[
            OsStr::new("fetch"),
            OsStr::new("--quiet"),
            OsStr::new("--no-tags"),
            OsStr::new("--"),
            repo.as_os_str(),
            OsStr::new(sha),
            OsStr::new("+refs/remotes/*:refs/remotes/*"),
        ])?;
        clone.run(&["checkout", "--quiet", "--detach", sha])?;
        Ok(Self {
            _temporary: temporary,
            path,
        })
    }
}
pub(crate) fn cleanup(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(o.words.is_empty(), "unexpected cleanup argument")?;
    if o.has("apply") {
        parent()?;
    }
    let repo = Git::root(c.cwd, c.interrupted).unwrap_or_else(|_| c.cwd.to_owned());
    let report = c.services.cleanup(&repo, o.has("apply"), c.interrupted)?;
    let mut out = format!(
        "# oh-my-setting cleanup\n\nmode: {}\n\n",
        if o.has("apply") { "apply" } else { "dry-run" }
    );
    if let Some(lines) = report.get("text").text() {
        out += lines;
    }
    out += &branch_sweep(c, &repo)?;
    Ok(Output::text(out))
}
pub(crate) fn branch_sweep(c: &Context<'_>, repo: &Path) -> Result<String> {
    let git = Git::new(repo, c.interrupted);
    let base = [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/heads/main",
        "refs/heads/master",
    ]
    .into_iter()
    .find(|b| git.commit(b).is_ok());
    let Some(base) = base else {
        return Ok("branches: no base available\n".into());
    };
    let used = git.text(&["worktree", "list", "--porcelain"])?;
    let mut out = String::new();
    for branch in git
        .text(&["for-each-ref", "--format=%(refname)", "refs/heads/oms/"])?
        .lines()
    {
        let short = branch.trim_start_matches("refs/heads/");
        let status = if used.lines().any(|l| l == format!("branch {branch}")) {
            "in use"
        } else if git.succeeds(&["merge-base", "--is-ancestor", branch, base])? {
            "safe to delete"
        } else if !git
            .text(&["cherry", base, branch])?
            .lines()
            .any(|l| l.starts_with('+'))
        {
            "landed by equivalent commits"
        } else {
            "still unique"
        };
        out += &format!("{short}: {status}\n");
        if ["safe to delete", "landed by equivalent commits"].contains(&status) {
            out += &format!("  git branch -D -- '{}'\n", short.replace('\'', "'\\''"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, obj, run_with_services};
    use std::ffi::OsString;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const CASE: &str = "OMS_WORKTREE_UT_CASE";
    const ROOT: &str = "OMS_WORKTREE_UT_ROOT";

    struct CountingCleanup {
        calls: AtomicUsize,
        removable: usize,
        fail: Option<i32>,
    }
    impl Services for CountingCleanup {
        fn cleanup(&self, _repo: &Path, apply: bool, interrupted: &AtomicBool) -> Result<crate::V> {
            assert!(apply, "automatic hygiene pass must run cleanup with apply");
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(code) = self.fail {
                if code == 130 {
                    interrupted.store(true, Ordering::SeqCst);
                }
                return Err(Error::new(code, "simulated hygiene failure"));
            }
            Ok(obj([
                ("text", "".into()),
                ("removable", self.removable.into()),
                ("kept", 0usize.into()),
            ]))
        }
    }

    fn git_ok(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn setup_repo(root: &Path) -> (PathBuf, PathBuf) {
        let repo = root.join("repo");
        let managed = root.join("managed");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        git_ok(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        git_ok(&repo, &["add", "a.txt"]);
        git_ok(&repo, &["commit", "-qm", "init"]);
        (
            std::fs::canonicalize(repo).unwrap(),
            std::fs::canonicalize(managed).unwrap(),
        )
    }

    fn run_isolated(case: &str, extra_env: &[(&str, &str)]) {
        let root =
            std::env::temp_dir().join(format!("oms-wt-hygiene-ut-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        std::fs::write(
            root.join("gitconfig"),
            "[user]\n\tname = t\n\temail = t@example.invalid\n",
        )
        .unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "worktree::tests::isolated_entry",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("TMPDIR", &root)
        .env("OMS_DELEGATE_WORKTREE_ROOT", root.join("managed"))
        .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("OMS_HARNESS_CHILD", "0")
        .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
        .env(CASE, case)
        .env(ROOT, &root);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "{case}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn add_args() -> Vec<OsString> {
        let ppid = rustix::process::getppid()
            .map(|p| p.as_raw_nonzero().get().to_string())
            .unwrap();
        vec!["add".into(), "--owner-pid".into(), OsString::from(ppid)]
    }

    fn run_case(case: &str, root: &Path) {
        let (repo, managed) = setup_repo(root);
        let flag = AtomicBool::new(false);
        match case {
            "rate-limit" => {
                let services = CountingCleanup {
                    calls: AtomicUsize::new(0),
                    removable: 2,
                    fail: None,
                };
                let args = add_args();
                let (code, out, err) =
                    run_with_services("scratch-worktree", &args, &repo, &repo, &flag, &services);
                assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
                let wt1 = PathBuf::from(String::from_utf8_lossy(&out).trim());
                assert!(wt1.join("a.txt").is_file(), "worktree 1 must exist");
                assert_eq!(String::from_utf8_lossy(&out).lines().count(), 1);
                assert_eq!(
                    String::from_utf8_lossy(&err),
                    "scratch-worktree: cleaned 2 item(s)\n"
                );
                assert_eq!(services.calls.load(Ordering::SeqCst), 1);
                let stamp = managed.join(HYGIENE_STAMP);
                assert!(stamp.is_file(), "stamp file must be created");

                // Second add within the hour is suppressed by the stamp.
                let (code, out, err) =
                    run_with_services("scratch-worktree", &args, &repo, &repo, &flag, &services);
                assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
                let wt2 = PathBuf::from(String::from_utf8_lossy(&out).trim());
                assert!(wt2.join("a.txt").is_file(), "worktree 2 must exist");
                assert!(err.is_empty(), "suppressed pass must emit no stderr");
                assert_eq!(
                    services.calls.load(Ordering::SeqCst),
                    1,
                    "rate-limit stamp must suppress second pass within the hour"
                );

                // Backdating the stamp past one hour allows a new pass; zero removals emit no stderr.
                let expired = crate::timestamp().saturating_sub(HYGIENE_INTERVAL_SECS + 10);
                std::fs::write(&stamp, format!("{expired}\n")).unwrap();
                let quiet = CountingCleanup {
                    calls: AtomicUsize::new(0),
                    removable: 0,
                    fail: None,
                };
                let (code, out, err) =
                    run_with_services("scratch-worktree", &args, &repo, &repo, &flag, &quiet);
                assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
                assert!(PathBuf::from(String::from_utf8_lossy(&out).trim()).is_dir());
                assert!(err.is_empty(), "zero removals must emit no stderr");
                assert_eq!(quiet.calls.load(Ordering::SeqCst), 1);
            }
            "failure-tolerant" => {
                let failing = CountingCleanup {
                    calls: AtomicUsize::new(0),
                    removable: 0,
                    fail: Some(2),
                };
                let args = add_args();
                let (code, out, err) =
                    run_with_services("scratch-worktree", &args, &repo, &repo, &flag, &failing);
                assert_eq!(code, 0, "hygiene failure must not fail add");
                let wt1 = PathBuf::from(String::from_utf8_lossy(&out).trim());
                assert!(wt1.join("a.txt").is_file(), "worktree 1 must exist");
                assert!(err.is_empty(), "hygiene failure must not pollute stderr");
                assert_eq!(failing.calls.load(Ordering::SeqCst), 1);

                // Remove stamp so interruption test also invokes cleanup.
                let _ = std::fs::remove_file(managed.join(HYGIENE_STAMP));
                let interrupting = CountingCleanup {
                    calls: AtomicUsize::new(0),
                    removable: 0,
                    fail: Some(130),
                };
                let (code, out, err) = run_with_services(
                    "scratch-worktree",
                    &args,
                    &repo,
                    &repo,
                    &flag,
                    &interrupting,
                );
                assert_eq!(code, 0, "hygiene interruption must not fail add");
                let wt2 = PathBuf::from(String::from_utf8_lossy(&out).trim());
                assert!(wt2.join("a.txt").is_file(), "worktree 2 must exist");
                assert!(err.is_empty());
                assert_eq!(interrupting.calls.load(Ordering::SeqCst), 1);
            }
            "harness-child-skips" => {
                let services = CountingCleanup {
                    calls: AtomicUsize::new(0),
                    removable: 1,
                    fail: None,
                };
                let context = Context {
                    cwd: &repo,
                    source: &repo,
                    interrupted: &flag,
                    services: &services,
                };
                let err = hygiene_pass(&context, &managed, &repo);
                assert!(err.is_empty());
                assert_eq!(services.calls.load(Ordering::SeqCst), 0);
                assert!(!managed.join(HYGIENE_STAMP).exists());
            }
            other => panic!("unknown case {other}"),
        }
    }

    #[test]
    #[ignore = "re-executed by isolated worktree hygiene cases"]
    fn isolated_entry() {
        let (Some(case), Some(root)) = (std::env::var_os(CASE), std::env::var_os(ROOT)) else {
            return;
        };
        run_case(&case.to_string_lossy(), Path::new(&root));
    }

    #[test]
    fn scratch_hygiene_rate_limit_suppresses_second_pass_within_hour() {
        run_isolated("rate-limit", &[]);
    }

    #[test]
    fn scratch_hygiene_failure_and_interruption_leave_add_successful() {
        run_isolated("failure-tolerant", &[]);
    }

    #[test]
    fn scratch_hygiene_skips_for_harness_child() {
        run_isolated("harness-child-skips", &[("OMS_HARNESS_CHILD", "1")]);
    }
}
