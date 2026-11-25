//! Custody proof for an unisolated Antigravity (`agy`) lead pane process.
//!
//! Phase A validates the lead process identity, live working directory,
//! positively allowed `cmdline`/`environ`/`settings.json`, private default
//! `.gemini/antigravity-cli` tree (`0o022 == 0`), and a unique held
//! `FLOCK ADVISORY WRITE` lock under `presence/<uuid>.lock` with a post-scan
//! re-walk confirming no mid-flight swaps or drifts.

#![cfg_attr(not(test), allow(dead_code))]

/// Proven custody of a single live `agy` lead process and its presence lock.
/// No `Debug` derive: `app_root` and `conversation_id` are private facts.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AgyLeadCustody {
    pub(crate) pid: u32,
    pub(crate) birth: String,
    pub(crate) repo: std::path::PathBuf,
    pub(crate) app_root: std::path::PathBuf,
    pub(crate) conversation_id: String,
    pub(crate) lock: (rustix::fs::Dev, u64),
}

const fn unsupported_host_custody() -> Option<AgyLeadCustody> {
    None
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn agy_lead_custody(
    _pid: u32,
    _birth: &str,
    _repo: &std::path::Path,
) -> Option<AgyLeadCustody> {
    unsupported_host_custody()
}

#[cfg(target_os = "linux")]
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use linux::agy_lead_custody;

#[cfg(target_os = "linux")]
mod linux {
    use super::AgyLeadCustody;
    use crate::claude_lead_usage::linux::{DIR, FILE, open_dir, start_time, unchanged};
    use oms_panel_core::{Value, decode, terminals};
    use rustix::fs::{
        self, AtFlags, Dir as RawDir, FileType, Mode, Stat, major, makedev, minor, readlinkat,
    };
    use rustix::io::Errno;
    use rustix::process::{geteuid, getuid};
    use std::cell::RefCell;
    use std::ffi::{CStr, OsStr, OsString};
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Component, Path, PathBuf};
    use std::rc::Rc;

    const PROC_LIMIT: usize = 256 * 1024;
    const SETTINGS_LIMIT: usize = 64 * 1024;
    const FD_LIMIT: usize = 4096;
    const LOCK_FILE_LIMIT: u64 = 4096;
    const FDINFO_LIMIT: usize = 64 * 1024;

    const BOOL_FLAGS: &[&[u8]] = &[
        b"-c",
        b"--continue",
        b"--dangerously-skip-permissions",
        b"--disable-slash-commands",
        b"--sandbox",
    ];
    const VALUE_FLAGS: &[&[u8]] = &[
        b"--model",
        b"--mode",
        b"--effort",
        b"--agent",
        b"-i",
        b"--prompt-interactive",
        b"--log-file",
        b"--conversation",
    ];
    const FORBIDDEN_SETTINGS_TOKENS: &[&str] = &[
        "appdata",
        "datadir",
        "geminidir",
        "configdir",
        "brain",
        "presence",
        "remote",
        "workspace",
        "project",
    ];

    /// `Err` names the first gate that refused, for diagnosis only.
    pub(super) type Stage<T> = Result<T, &'static str>;

    fn gate(ok: bool, stage: &'static str) -> Stage<()> {
        if ok { Ok(()) } else { Err(stage) }
    }

    pub(crate) fn agy_lead_custody(pid: u32, birth: &str, repo: &Path) -> Option<AgyLeadCustody> {
        custody_with(pid, birth, repo, || {}).ok()
    }

    /// Runs the full custody proof chain with a mid-flight test seam between
    /// the initial read/lock scan and post-scan confirmation.
    pub(super) fn custody_with(
        pid: u32,
        birth: &str,
        repo: &Path,
        between: impl FnOnce(),
    ) -> Stage<AgyLeadCustody> {
        custody_traced(pid, birth, repo, between).map(|(custody, _)| custody)
    }

    pub(super) fn custody_traced(
        pid: u32,
        birth: &str,
        repo: &Path,
        between: impl FnOnce(),
    ) -> Stage<(AgyLeadCustody, (usize, usize))> {
        // 1. Pre-validation.
        gate(pid > 0 && pid <= i32::MAX as u32, "pid")?;
        validate_birth(birth)?;
        let (canon_repo, repo_stat) = open_canonical_repo(repo)?;

        // 2. Same-user process & /proc/{pid}.
        let uid = getuid().as_raw();
        gate(geteuid().as_raw() == uid, "process-owner")?;
        let owner = terminals::process(pid).map_err(|_| "process")?;
        gate(owner.pid == pid && owner.uid == uid, "process-owner")?;
        gate(owner.birth == birth, "birth")?;

        let proc_path = PathBuf::from(format!("/proc/{pid}"));
        let proc_fd = open_dir(&proc_path).ok_or("proc")?;
        let proc_stat = fs::fstat(&proc_fd).map_err(|_| "proc")?;
        gate(
            FileType::from_raw_mode(proc_stat.st_mode) == FileType::Directory
                && proc_stat.st_uid == uid,
            "proc-owner",
        )?;

        // 3. Live CWD == canonical repo.
        verify_live_cwd(&proc_fd, &canon_repo, &repo_stat)?;

        // 4. Positively checked cmdline & environ.
        let cmdline_raw = read_proc_file(&proc_fd, c"cmdline", PROC_LIMIT, "cmdline")?;
        validate_cmdline(&cmdline_raw)?;
        let environ_raw = read_proc_file(&proc_fd, c"environ", PROC_LIMIT, "environ")?;
        let home = validate_environ(&environ_raw)?;

        // 5. Positively checked private default app root & config (0o022 == 0).
        let home_dir = Dir::root(&home, uid, "home")?;
        let canon_home = std::fs::canonicalize(&home).map_err(|_| "home")?;
        gate(canon_home.as_path() == home.as_path(), "home")?;

        let gemini_dir = home_dir.dir(".gemini", uid, "gemini-dir")?;
        let config_dir = read_optional_config(&gemini_dir, uid)?;
        let app_dir = gemini_dir.dir("antigravity-cli", uid, "app-root")?;
        let settings = read_optional_settings(&app_dir, uid)?;
        let presence_dir = app_dir.dir("presence", uid, "presence-dir")?;

        let app_root = canon_home.join(".gemini").join("antigravity-cli");
        let presence_path = app_root.join("presence");

        // 6. Unique held WRITE FLOCK presence lock from /proc/{pid}/fd & fdinfo.
        let initial = scan_presence(&proc_fd, &presence_dir, &presence_path, pid, uid, true)?;
        let trail_before = home_dir.trail.borrow().0.len();

        // 7. Mid-flight seam & post-scan confirmation.
        between();

        let rescan = scan_presence(&proc_fd, &presence_dir, &presence_path, pid, uid, false)?;
        let trail_after = home_dir.trail.borrow().0.len();
        gate(rescan == initial, "presence-changed")?;

        match &config_dir {
            None => match fs::statat(&gemini_dir.fd, "config", AtFlags::SYMLINK_NOFOLLOW) {
                Err(Errno::NOENT) => {}
                _ => return Err("config-changed"),
            },
            Some(dir) => {
                let now = fs::fstat(&dir.fd).map_err(|_| "config-changed")?;
                gate(
                    FileType::from_raw_mode(now.st_mode) == FileType::Directory
                        && strict(&now, uid),
                    "config-changed",
                )?;
            }
        }

        match &settings {
            None => match fs::statat(&app_dir.fd, "settings.json", AtFlags::SYMLINK_NOFOLLOW) {
                Err(Errno::NOENT) => {}
                _ => return Err("settings-changed"),
            },
            Some((file, before)) => {
                let now = fs::fstat(file).map_err(|_| "settings-changed")?;
                gate(
                    unchanged(before, &now) && strict(&now, uid) && now.st_nlink == 1,
                    "settings-changed",
                )?;
            }
        }

        same_names(&canon_home, uid, &home_dir.trail.borrow())?;

        let (_, repo_now) = open_canonical_repo(&canon_repo).map_err(|_| "repo-changed")?;
        gate(
            (repo_now.st_dev, repo_now.st_ino) == (repo_stat.st_dev, repo_stat.st_ino),
            "repo-changed",
        )?;

        verify_live_cwd(&proc_fd, &canon_repo, &repo_stat)?;

        let cmdline_now = read_proc_file(&proc_fd, c"cmdline", PROC_LIMIT, "cmdline-changed")?;
        gate(cmdline_now == cmdline_raw, "cmdline-changed")?;
        let environ_now = read_proc_file(&proc_fd, c"environ", PROC_LIMIT, "environ-changed")?;
        gate(environ_now == environ_raw, "environ-changed")?;

        let proc_now = fs::fstat(&proc_fd).map_err(|_| "proc-changed")?;
        gate(
            unchanged(&proc_stat, &proc_now) && proc_now.st_uid == uid,
            "proc-changed",
        )?;
        gate(
            terminals::process(pid).is_ok_and(|now| now == owner),
            "process-changed",
        )?;

        Ok((
            AgyLeadCustody {
                pid,
                birth: owner.birth,
                repo: canon_repo,
                app_root,
                conversation_id: initial.uuid,
                lock: initial.lock,
            },
            (trail_before, trail_after),
        ))
    }

    fn strict(stat: &Stat, uid: u32) -> bool {
        stat.st_uid == uid && stat.st_mode & 0o022 == 0
    }

    /// Every name the reads resolved, with its file flag and `(dev, ino)` for post-scan re-walk.
    #[derive(Default)]
    pub(super) struct Trail(Vec<(Vec<String>, bool, (fs::Dev, u64))>);

    struct Dir {
        fd: OwnedFd,
        names: Vec<String>,
        trail: Rc<RefCell<Trail>>,
    }

    impl Dir {
        fn root(path: &Path, uid: u32, stage: &'static str) -> Stage<Self> {
            let fd = open_dir(path).ok_or(stage)?;
            Self::admit(fd, Vec::new(), Rc::default(), uid, stage)
        }

        fn dir(&self, name: &str, uid: u32, stage: &'static str) -> Stage<Self> {
            let fd = fs::openat(&self.fd, name, DIR, Mode::empty()).map_err(|_| stage)?;
            let mut names = self.names.clone();
            names.push(name.to_owned());
            Self::admit(fd, names, Rc::clone(&self.trail), uid, stage)
        }

        fn admit(
            fd: OwnedFd,
            names: Vec<String>,
            trail: Rc<RefCell<Trail>>,
            uid: u32,
            stage: &'static str,
        ) -> Stage<Self> {
            let stat = fs::fstat(&fd).map_err(|_| stage)?;
            gate(
                FileType::from_raw_mode(stat.st_mode) == FileType::Directory && strict(&stat, uid),
                stage,
            )?;
            trail
                .borrow_mut()
                .0
                .push((names.clone(), false, (stat.st_dev, stat.st_ino)));
            Ok(Self { fd, names, trail })
        }

        fn file(
            &self,
            name: &str,
            limit: u64,
            uid: u32,
            record: bool,
            stage: &'static str,
        ) -> Stage<(File, Stat)> {
            let fd = fs::openat(&self.fd, name, FILE, Mode::empty()).map_err(|_| stage)?;
            let stat = fs::fstat(&fd).map_err(|_| stage)?;
            gate(
                FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                    && stat.st_nlink == 1
                    && strict(&stat, uid)
                    && u64::try_from(stat.st_size).is_ok_and(|size| size <= limit),
                stage,
            )?;
            if record {
                let mut names = self.names.clone();
                names.push(name.to_owned());
                self.trail
                    .borrow_mut()
                    .0
                    .push((names, true, (stat.st_dev, stat.st_ino)));
            }
            Ok((File::from(fd), stat))
        }
    }

    fn same_names(home: &Path, uid: u32, trail: &Trail) -> Stage<()> {
        let root = open_dir(home).ok_or("path-changed")?;
        for (names, is_file, ident) in &trail.0 {
            let mut fd = None;
            for (depth, name) in names.iter().enumerate() {
                let flags = if *is_file && depth + 1 == names.len() {
                    FILE
                } else {
                    DIR
                };
                let next = fs::openat(fd.as_ref().unwrap_or(&root), name, flags, Mode::empty())
                    .map_err(|_| "path-changed")?;
                fd = Some(next);
            }
            let now = fs::fstat(fd.as_ref().unwrap_or(&root)).map_err(|_| "path-changed")?;
            let expected_type = if *is_file {
                FileType::RegularFile
            } else {
                FileType::Directory
            };
            gate(
                FileType::from_raw_mode(now.st_mode) == expected_type
                    && strict(&now, uid)
                    && (!*is_file || now.st_nlink == 1)
                    && (now.st_dev, now.st_ino) == *ident,
                "path-changed",
            )?;
        }
        Ok(())
    }

    fn validate_birth(birth: &str) -> Stage<()> {
        let Some((boot, _)) = birth.rsplit_once(':') else {
            return Err("birth");
        };
        let Some(start) = start_time(birth) else {
            return Err("birth");
        };
        gate(
            !boot.is_empty() && !start.is_empty() && start.bytes().all(|b| b.is_ascii_digit()),
            "birth",
        )
    }

    fn open_canonical_repo(repo: &Path) -> Stage<(PathBuf, Stat)> {
        gate(repo.is_absolute(), "repo")?;
        let repo_fd = open_dir(repo).ok_or("repo")?;
        let repo_stat = fs::fstat(&repo_fd).map_err(|_| "repo")?;
        gate(
            FileType::from_raw_mode(repo_stat.st_mode) == FileType::Directory,
            "repo",
        )?;
        let canon = std::fs::canonicalize(repo).map_err(|_| "repo")?;
        gate(canon.as_path() == repo, "repo")?;
        Ok((canon, repo_stat))
    }

    fn verify_live_cwd(proc_fd: &OwnedFd, repo: &Path, repo_stat: &Stat) -> Stage<()> {
        let cwd_cstr = readlinkat(proc_fd, "cwd", Vec::new()).map_err(|_| "cwd")?;
        let cwd_path = PathBuf::from(OsString::from_vec(cwd_cstr.into_bytes()));
        gate(cwd_path == repo, "cwd")?;
        let cwd_stat = fs::statat(proc_fd, "cwd", AtFlags::empty()).map_err(|_| "cwd")?;
        gate(
            FileType::from_raw_mode(cwd_stat.st_mode) == FileType::Directory
                && cwd_stat.st_nlink > 0
                && (cwd_stat.st_dev, cwd_stat.st_ino) == (repo_stat.st_dev, repo_stat.st_ino),
            "cwd",
        )
    }

    fn read_proc_file(
        dir: &OwnedFd,
        name: &CStr,
        limit: usize,
        stage: &'static str,
    ) -> Stage<Vec<u8>> {
        let fd = fs::openat(dir, name, FILE, Mode::empty()).map_err(|_| stage)?;
        let stat = fs::fstat(&fd).map_err(|_| stage)?;
        gate(
            FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile,
            stage,
        )?;
        let file = File::from(fd);
        let mut buf = Vec::new();
        (&file)
            .take(limit as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|_| stage)?;
        gate(buf.len() <= limit, stage)?;
        Ok(buf)
    }

    pub(super) fn validate_cmdline(raw: &[u8]) -> Stage<()> {
        gate(!raw.is_empty() && raw.len() <= PROC_LIMIT, "cmdline")?;
        let trimmed = raw.strip_suffix(b"\0").ok_or("cmdline")?;
        let mut args = trimmed.split(|&b| b == 0);
        let argv0 = args.next().ok_or("cmdline")?;
        gate(is_valid_argv0(argv0), "cmdline")?;

        while let Some(arg) = args.next() {
            if BOOL_FLAGS.contains(&arg) {
                continue;
            }
            if VALUE_FLAGS.contains(&arg) {
                let val = args.next().ok_or("cmdline")?;
                gate(!val.is_empty(), "cmdline")?;
                continue;
            }
            if let Some(eq) = arg.iter().position(|&b| b == b'=') {
                let flag = &arg[..eq];
                let val = &arg[eq + 1..];
                if flag.starts_with(b"--") && VALUE_FLAGS.contains(&flag) && !val.is_empty() {
                    continue;
                }
            }
            return Err("cmdline");
        }
        Ok(())
    }

    fn is_valid_argv0(argv0: &[u8]) -> bool {
        if argv0 == b"agy" {
            return true;
        }
        if !argv0.starts_with(b"/") || argv0.ends_with(b"/") {
            return false;
        }
        let mut parts = argv0[1..].split(|&b| b == b'/').peekable();
        while let Some(part) = parts.next() {
            if part.is_empty() || part == b"." || part == b".." {
                return false;
            }
            if parts.peek().is_none() {
                return part == b"agy";
            }
        }
        false
    }

    pub(super) fn validate_environ(raw: &[u8]) -> Stage<PathBuf> {
        gate(raw.len() <= PROC_LIMIT, "environ")?;
        let mut home: Option<PathBuf> = None;
        for entry in raw.split(|&b| b == 0) {
            if entry.is_empty() {
                continue;
            }
            let eq = entry.iter().position(|&b| b == b'=').ok_or("environ")?;
            gate(eq > 0, "environ")?;
            let key = &entry[..eq];
            let val = &entry[eq + 1..];
            if key.starts_with(b"ANTIGRAVITY_")
                || key.starts_with(b"AGY_")
                || key == b"OMS_GEMINI_HOME"
                || key == b"OMS_ANTIGRAVITY_SETTINGS"
                || key == b"GEMINI_CLI_HOME"
            {
                return Err("environ-override");
            }
            if key == b"HOME" {
                gate(home.is_none(), "environ")?;
                let path = validate_home_value(val)?;
                home = Some(path);
            }
        }
        home.ok_or("environ")
    }

    fn validate_home_value(val: &[u8]) -> Stage<PathBuf> {
        gate(!val.is_empty() && val.starts_with(b"/"), "environ")?;
        // Path::components collapses internal `/./`, so reject `.` and `..` segments explicitly too.
        gate(
            !val[1..]
                .split(|&b| b == b'/')
                .any(|seg| seg == b"." || seg == b".."),
            "environ",
        )?;
        let path = PathBuf::from(OsString::from_vec(val.to_vec()));
        gate(
            path.is_absolute()
                && path
                    .components()
                    .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
            "environ",
        )?;
        Ok(path)
    }

    fn read_optional_config(gemini_dir: &Dir, uid: u32) -> Stage<Option<Dir>> {
        match fs::statat(&gemini_dir.fd, "config", AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => Ok(None),
            Ok(_) => gemini_dir.dir("config", uid, "gemini-config").map(Some),
            Err(_) => Err("gemini-config"),
        }
    }

    fn read_optional_settings(app_dir: &Dir, uid: u32) -> Stage<Option<(File, Stat)>> {
        match fs::openat(&app_dir.fd, "settings.json", FILE, Mode::empty()) {
            Ok(fd) => {
                let before = fs::fstat(&fd).map_err(|_| "settings")?;
                let size = usize::try_from(before.st_size)
                    .ok()
                    .filter(|&size| size <= SETTINGS_LIMIT)
                    .ok_or("settings")?;
                gate(
                    FileType::from_raw_mode(before.st_mode) == FileType::RegularFile
                        && before.st_nlink == 1
                        && strict(&before, uid),
                    "settings",
                )?;
                let file = File::from(fd);
                let mut buf = Vec::with_capacity(size + 1);
                (&file)
                    .take(size as u64 + 1)
                    .read_to_end(&mut buf)
                    .map_err(|_| "settings")?;
                let after = fs::fstat(&file).map_err(|_| "settings")?;
                gate(buf.len() == size && unchanged(&before, &after), "settings")?;
                let mut names = app_dir.names.clone();
                names.push("settings.json".to_owned());
                app_dir
                    .trail
                    .borrow_mut()
                    .0
                    .push((names, true, (before.st_dev, before.st_ino)));
                let doc = decode(&buf, SETTINGS_LIMIT).map_err(|_| "settings-json")?;
                check_settings_object(&doc)?;
                Ok(Some((file, before)))
            }
            Err(Errno::NOENT) => {
                // Dangling symlinks must be refused, never treated as missing.
                match fs::statat(&app_dir.fd, "settings.json", AtFlags::SYMLINK_NOFOLLOW) {
                    Err(Errno::NOENT) => Ok(None),
                    _ => Err("settings"),
                }
            }
            Err(_) => Err("settings"),
        }
    }

    pub(super) fn check_settings_object(doc: &Value) -> Stage<()> {
        gate(doc.is_object(), "settings-json")?;
        let mut stack = vec![doc];
        while let Some(node) = stack.pop() {
            match node {
                Value::Object(map) => {
                    for (key, value) in map {
                        let norm: String = key
                            .chars()
                            .filter(char::is_ascii_alphanumeric)
                            .map(|c| c.to_ascii_lowercase())
                            .collect();
                        if FORBIDDEN_SETTINGS_TOKENS
                            .iter()
                            .any(|tok| norm.contains(tok))
                        {
                            return Err("settings-override");
                        }
                        stack.push(value);
                    }
                }
                Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
        Ok(())
    }

    #[derive(Clone, PartialEq, Eq)]
    struct PresenceScan {
        fd_name: String,
        uuid: String,
        lock: (fs::Dev, u64),
    }

    pub(super) fn fd_within_budget(seen: usize) -> Stage<()> {
        gate(seen <= FD_LIMIT, "fd-budget")
    }

    fn scan_presence(
        proc_fd: &OwnedFd,
        presence_dir: &Dir,
        presence_path: &Path,
        pid: u32,
        uid: u32,
        record_trail: bool,
    ) -> Stage<PresenceScan> {
        let fd_dir = fs::openat(proc_fd, "fd", DIR, Mode::empty()).map_err(|_| "proc-fd")?;
        let fdinfo_dir =
            fs::openat(proc_fd, "fdinfo", DIR, Mode::empty()).map_err(|_| "proc-fdinfo")?;
        let entries = RawDir::read_from(&fd_dir).map_err(|_| "proc-fd")?;
        let mut seen = 0usize;
        let mut candidate: Option<PresenceScan> = None;

        for (raw_idx, entry) in entries.enumerate() {
            if raw_idx >= FD_LIMIT + 2 {
                return Err("fd-budget");
            }
            let entry = entry.map_err(|_| "proc-fd")?;
            let name_cstr = entry.file_name();
            let name_bytes = name_cstr.to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            seen += 1;
            fd_within_budget(seen)?;
            gate(
                !name_bytes.is_empty() && name_bytes.iter().all(u8::is_ascii_digit),
                "proc-fd",
            )?;
            let fd_name = std::str::from_utf8(name_bytes).map_err(|_| "proc-fd")?;
            let target_cstr = match readlinkat(&fd_dir, name_cstr, Vec::new()) {
                Ok(target) => target,
                Err(Errno::NOENT) => continue,
                Err(_) => return Err("proc-fd"),
            };
            let target = PathBuf::from(OsString::from_vec(target_cstr.into_bytes()));
            let Some(parent) = target.parent() else {
                continue;
            };
            if parent != presence_path {
                if parent.file_name() == Some(OsStr::new("presence")) {
                    return Err("foreign-presence");
                }
                continue;
            }

            let file_name = target
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or("presence-fd")?;
            let uuid = file_name.strip_suffix(".lock").ok_or("presence-fd")?;
            gate(is_uuid_lower_hex(uuid), "presence-fd")?;

            if candidate.is_some() {
                return Err("multiple-presence");
            }

            let fd_stat =
                fs::statat(&fd_dir, name_cstr, AtFlags::empty()).map_err(|_| "presence-fd")?;
            gate(
                FileType::from_raw_mode(fd_stat.st_mode) == FileType::RegularFile
                    && fd_stat.st_nlink == 1
                    && strict(&fd_stat, uid),
                "presence-fd",
            )?;

            let (_, disk_stat) = presence_dir.file(
                file_name,
                LOCK_FILE_LIMIT,
                uid,
                record_trail,
                "presence-file",
            )?;
            gate(
                (disk_stat.st_dev, disk_stat.st_ino) == (fd_stat.st_dev, fd_stat.st_ino),
                "presence-file",
            )?;

            let fdinfo_bytes =
                read_proc_file(&fdinfo_dir, name_cstr, FDINFO_LIMIT, "presence-flock")?;
            let fdinfo_text = std::str::from_utf8(&fdinfo_bytes).map_err(|_| "presence-flock")?;
            verify_unique_write_flock(fdinfo_text, pid, &fd_stat)?;

            candidate = Some(PresenceScan {
                fd_name: fd_name.to_owned(),
                uuid: uuid.to_owned(),
                lock: (fd_stat.st_dev, fd_stat.st_ino),
            });
        }

        candidate.ok_or("no-presence")
    }

    pub(super) fn is_uuid_lower_hex(s: &str) -> bool {
        let bytes = s.as_bytes();
        if bytes.len() != 36 {
            return false;
        }
        bytes.iter().enumerate().all(|(idx, &b)| match idx {
            8 | 13 | 18 | 23 => b == b'-',
            _ => matches!(b, b'0'..=b'9' | b'a'..=b'f'),
        })
    }

    pub(super) fn verify_unique_write_flock(
        fdinfo_text: &str,
        pid: u32,
        fd_stat: &Stat,
    ) -> Stage<()> {
        let mut ino_count = 0usize;
        let mut lock_count = 0usize;

        for line in fdinfo_text.lines() {
            if let Some(rest) = line.strip_prefix("ino:") {
                let val = rest.trim();
                gate(
                    !val.is_empty() && val.bytes().all(|b| b.is_ascii_digit()),
                    "presence-flock",
                )?;
                let ino = val.parse::<u64>().map_err(|_| "presence-flock")?;
                gate(ino == fd_stat.st_ino, "presence-flock")?;
                ino_count += 1;
            } else if let Some(rest) = line.strip_prefix("lock:") {
                lock_count += 1;
                if lock_count > 1 {
                    return Err("presence-flock");
                }
                let tokens: Vec<&str> = rest.split_ascii_whitespace().collect();
                gate(tokens.len() == 8, "presence-flock")?;
                let seq = tokens[0].strip_suffix(':').ok_or("presence-flock")?;
                gate(
                    !seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit()),
                    "presence-flock",
                )?;
                gate(
                    tokens[1] == "FLOCK"
                        && tokens[2] == "ADVISORY"
                        && tokens[3] == "WRITE"
                        && tokens[6] == "0"
                        && tokens[7] == "EOF",
                    "presence-flock",
                )?;
                gate(
                    !tokens[4].is_empty() && tokens[4].bytes().all(|b| b.is_ascii_digit()),
                    "presence-flock",
                )?;
                let owner_pid = tokens[4].parse::<u32>().map_err(|_| "presence-flock")?;
                gate(owner_pid == pid, "presence-flock")?;

                let mut parts = tokens[5].split(':');
                let (Some(maj_s), Some(min_s), Some(ino_s), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return Err("presence-flock");
                };
                gate(
                    is_lower_hex_2_to_8(maj_s) && is_lower_hex_2_to_8(min_s),
                    "presence-flock",
                )?;
                let maj = u32::from_str_radix(maj_s, 16).map_err(|_| "presence-flock")?;
                let min = u32::from_str_radix(min_s, 16).map_err(|_| "presence-flock")?;
                gate(
                    makedev(maj, min) == fd_stat.st_dev
                        && maj == major(fd_stat.st_dev)
                        && min == minor(fd_stat.st_dev),
                    "presence-flock",
                )?;
                gate(
                    !ino_s.is_empty() && ino_s.bytes().all(|b| b.is_ascii_digit()),
                    "presence-flock",
                )?;
                let lock_ino = ino_s.parse::<u64>().map_err(|_| "presence-flock")?;
                gate(lock_ino == fd_stat.st_ino, "presence-flock")?;
            }
        }

        gate(ino_count == 1 && lock_count == 1, "presence-flock")
    }

    fn is_lower_hex_2_to_8(s: &str) -> bool {
        (2..=8).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

#[cfg(test)]
mod stub_tests {
    use super::{AgyLeadCustody, unsupported_host_custody};
    use std::path::PathBuf;

    #[test]
    fn off_linux_stub_is_none_and_custody_implements_clone_and_eq() {
        assert!(unsupported_host_custody().is_none());
        let a = AgyLeadCustody {
            pid: 42,
            birth: "boot:100".to_owned(),
            repo: PathBuf::from("/tmp/repo"),
            app_root: PathBuf::from("/tmp/app"),
            conversation_id: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
            lock: (1, 2),
        };
        let b = a.clone();
        assert!(a == b);
        let mut c = b.clone();
        c.pid = 43;
        assert!(a != c);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::AgyLeadCustody;
    use super::agy_lead_custody;
    use super::linux::{
        check_settings_object, custody_traced, custody_with, fd_within_budget, is_uuid_lower_hex,
        validate_cmdline, validate_environ, verify_unique_write_flock,
    };
    use oms_panel_core::{decode, terminals};
    use rustix::fs::{self as rfs, Stat, major, minor};
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const UUID1: &str = "11111111-2222-3333-4444-555555555555";
    const UUID2: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const OTHER_UUID: &str = "99999999-8888-7777-6666-555555555555";
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    fn mode(path: &Path, perms: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(perms)).unwrap();
    }

    fn write_private(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        mode(path, 0o600);
    }

    fn mkdir_private(path: &Path) {
        fs::create_dir_all(path).unwrap();
        mode(path, 0o700);
    }

    fn copy_tree_exact(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let src = entry.path();
            let dst = to.join(entry.file_name());
            if src.is_dir() {
                copy_tree_exact(&src, &dst);
            } else {
                fs::copy(&src, &dst).unwrap();
                mode(
                    &dst,
                    fs::metadata(&src).unwrap().permissions().mode() & 0o7777,
                );
            }
        }
        mode(
            to,
            fs::metadata(from).unwrap().permissions().mode() & 0o7777,
        );
    }

    struct Fixture {
        base: PathBuf,
        repo: PathBuf,
        other_repo: PathBuf,
        home: PathBuf,
        gemini: PathBuf,
        app_root: PathBuf,
        presence: PathBuf,
        conversations: PathBuf,
    }

    impl Fixture {
        /// Every directory and file receives explicit permissions so host umask never masks failures.
        fn new(label: &str) -> Self {
            let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "oms-agy-custody-{}-{seq}-{label}",
                std::process::id()
            ));
            mkdir_private(&base);
            let repo = base.join("repo");
            let other_repo = base.join("other-repo");
            let home = base.join("home");
            let gemini = home.join(".gemini");
            let app_root = gemini.join("antigravity-cli");
            let presence = app_root.join("presence");
            let conversations = app_root.join("conversations");
            for dir in [
                &repo,
                &other_repo,
                &home,
                &gemini,
                &app_root,
                &presence,
                &conversations,
            ] {
                mkdir_private(dir);
            }
            let fixture = Self {
                base,
                repo,
                other_repo,
                home,
                gemini,
                app_root,
                presence,
                conversations,
            };
            write_private(&fixture.lock_path(UUID1), "");
            write_private(&fixture.lock_path(UUID2), "");
            write_private(&fixture.conversations.join(format!("{OTHER_UUID}.db")), "");
            fixture
        }

        fn lock_path(&self, uuid: &str) -> PathBuf {
            self.presence.join(format!("{uuid}.lock"))
        }

        fn config_path(&self) -> PathBuf {
            self.gemini.join("config")
        }

        fn settings_path(&self) -> PathBuf {
            self.app_root.join("settings.json")
        }

        fn spawn_holder(&self, args: HolderSpec<'_>) -> Holder {
            Holder::spawn(self, args)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    struct HolderSpec<'a> {
        lock_file: Option<&'a Path>,
        extra_lock_file: Option<&'a Path>,
        flock_flag: Option<&'a str>,
        no_fork: bool,
        use_flock: bool,
        argv0: &'a str,
        bash_flags: &'a [&'a str],
        prelude: &'a str,
        cwd: &'a Path,
        include_home: bool,
        extra_env: &'a [(&'a str, &'a str)],
    }

    struct Holder {
        child: Child,
        pid: u32,
        birth: String,
        stdin: Option<ChildStdin>,
        stdout: BufReader<ChildStdout>,
        killed_early: bool,
    }

    impl Holder {
        fn spawn(fixture: &Fixture, spec: HolderSpec<'_>) -> Self {
            let quoted_exec = format!(
                "{}exec -a {} bash{}",
                if spec.prelude.is_empty() {
                    String::new()
                } else {
                    format!("{}; ", spec.prelude)
                },
                shell_quote(spec.argv0),
                spec.bash_flags
                    .iter()
                    .map(|f| format!(" {}", shell_quote(f)))
                    .collect::<String>()
            );

            let default_lock = fixture.lock_path(UUID1);
            let lock_file = spec.lock_file.unwrap_or(&default_lock);
            // A new session has no controlling terminal, so interactive bash
            // skips job control. From a background process group of a
            // terminal it would otherwise stop itself with SIGTTIN.
            let mut cmd = Command::new("setsid");
            if spec.use_flock {
                let c = &mut cmd;
                c.args(["flock", "-n"]);
                if spec.no_fork {
                    c.arg("-F");
                }
                if let Some(flag) = spec.flock_flag {
                    c.arg(flag);
                }
                c.arg(lock_file);
                if let Some(second) = spec.extra_lock_file {
                    c.arg("flock").arg("-n").arg("-F").arg(second);
                }
                c.args(["bash", "-c", &quoted_exec]);
            } else {
                cmd.args(["bash", "-c", &quoted_exec]);
            }

            cmd.env_clear()
                .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                .current_dir(spec.cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if spec.include_home {
                cmd.env("HOME", &fixture.home);
            }
            for (k, v) in spec.extra_env {
                cmd.env(k, v);
            }

            let mut child = cmd.spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let mut reader = BufReader::new(stdout);

            writeln!(stdin, "echo \"READY:$$\"").unwrap();
            stdin.flush().unwrap();

            let mut pid = None;
            loop {
                let mut line = String::new();
                let read = reader.read_line(&mut line).unwrap();
                if read == 0 {
                    break;
                }
                if let Some(pid_str) = line.trim().strip_prefix("READY:") {
                    pid = Some(pid_str.parse::<u32>().unwrap());
                    break;
                }
            }
            let pid = pid.expect("holder failed ready handshake before exit");
            let birth = terminals::process(pid).unwrap().birth;

            Self {
                child,
                pid,
                birth,
                stdin: Some(stdin),
                stdout: reader,
                killed_early: false,
            }
        }

        fn run_cmd(&mut self, command: &str, marker: &str) {
            let stdin = self.stdin.as_mut().unwrap();
            writeln!(stdin, "{command}; echo {marker}").unwrap();
            stdin.flush().unwrap();
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0 && line.trim() == marker, "unexpected {line:?}");
        }

        fn kill_and_wait(&mut self) {
            self.stdin.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.killed_early = true;
        }
    }

    impl Drop for Holder {
        fn drop(&mut self) {
            self.stdin.take();
            if !self.killed_early {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    fn shell_quote(raw: &str) -> String {
        format!("'{}'", raw.replace('\'', "'\"'\"'"))
    }

    fn default_spec(fixture: &Fixture) -> HolderSpec<'_> {
        HolderSpec {
            lock_file: None,
            extra_lock_file: None,
            flock_flag: None,
            no_fork: true,
            use_flock: true,
            argv0: "agy",
            bash_flags: &[],
            prelude: "",
            cwd: &fixture.repo,
            include_home: true,
            extra_env: &[],
        }
    }

    #[test]
    fn happy_path_ignores_stale_lock_and_conversations_db_and_accepts_optional_files() {
        let fixture = Fixture::new("happy");
        let db_path = fixture.conversations.join(format!("{OTHER_UUID}.db"));
        let prelude = format!("exec 8>{}", shell_quote(db_path.to_str().unwrap()));
        let mut spec = default_spec(&fixture);
        spec.prelude = &prelude;
        let holder = fixture.spawn_holder(spec);

        let lock_meta = fs::metadata(fixture.lock_path(UUID1)).unwrap();
        let expected = AgyLeadCustody {
            pid: holder.pid,
            birth: holder.birth.clone(),
            repo: fixture.repo.clone(),
            app_root: fixture.app_root.clone(),
            conversation_id: UUID1.to_owned(),
            lock: (lock_meta.dev(), lock_meta.ino()),
        };

        let got = agy_lead_custody(holder.pid, &holder.birth, &fixture.repo);
        assert!(got.as_ref() == Some(&expected));

        // 0o755 / 0o644 (group/other read without write) satisfies strict 0o022 == 0,
        // and Gate 7 re-scan appends zero duplicate Trail entries.
        mode(&fixture.gemini, 0o755);
        mode(&fixture.lock_path(UUID1), 0o644);
        let (traced_min, (before_min, after_min)) =
            custody_traced(holder.pid, &holder.birth, &fixture.repo, || {}).unwrap();
        assert!(traced_min == expected && before_min == after_min && before_min == 5);
        mode(&fixture.gemini, 0o700);
        mode(&fixture.lock_path(UUID1), 0o600);

        // Optional .gemini/config + benign settings.json also pass without Trail duplication.
        mkdir_private(&fixture.config_path());
        write_private(
            &fixture.settings_path(),
            r#"{"model":"gemini-2.5-pro","nested":{"theme":"dark"}}"#,
        );
        let (got_with_opt, (before_opt, after_opt)) =
            custody_traced(holder.pid, &holder.birth, &fixture.repo, || {}).unwrap();
        assert!(got_with_opt == expected && before_opt == after_opt && before_opt == 7);
    }

    #[test]
    fn normalized_absolute_argv0_is_accepted() {
        let fixture = Fixture::new("abs-argv0");
        let mut spec = default_spec(&fixture);
        spec.argv0 = "/usr/local/bin/agy";
        let holder = fixture.spawn_holder(spec);
        let got = agy_lead_custody(holder.pid, &holder.birth, &fixture.repo);
        assert!(got.is_some());
    }

    #[test]
    fn invalid_pid_birth_repo_cwd_and_midflight_drift_swap_exit_are_refused() {
        let fixture = Fixture::new("proc-cwd");
        {
            let mut holder = fixture.spawn_holder(default_spec(&fixture));

            // Invalid / missing / exited pid.
            assert!(custody_with(0, &holder.birth, &fixture.repo, || {}).is_err());
            assert!(custody_with(u32::MAX, &holder.birth, &fixture.repo, || {}).is_err());
            assert!(
                custody_with(i32::MAX as u32 + 1, &holder.birth, &fixture.repo, || {}).is_err()
            );

            // Malformed and wrong birth.
            for bad_birth in ["", "boot", ":123", "boot:", "boot:12x", "wrong:1"] {
                assert!(
                    custody_with(holder.pid, bad_birth, &fixture.repo, || {}).is_err(),
                    "{bad_birth}"
                );
            }

            // Non-canonical / relative / missing repo.
            assert!(custody_with(holder.pid, &holder.birth, Path::new("repo"), || {}).is_err());
            let repo_link = fixture.base.join("repo-link");
            symlink(&fixture.repo, &repo_link).unwrap();
            assert!(custody_with(holder.pid, &holder.birth, &repo_link, || {}).is_err());

            // Mid-flight live cwd drift.
            let other = fixture.other_repo.clone();
            let pid1 = holder.pid;
            let birth1 = holder.birth.clone();
            let drift = custody_with(pid1, &birth1, &fixture.repo, || {
                holder.run_cmd(
                    &format!("cd {}", shell_quote(other.to_str().unwrap())),
                    "CD",
                );
            });
            assert!(drift.err() == Some("cwd"));
        }

        // Wrong initial cwd.
        {
            let mut wrong_cwd_spec = default_spec(&fixture);
            wrong_cwd_spec.cwd = &fixture.other_repo;
            let wrong_cwd_holder = fixture.spawn_holder(wrong_cwd_spec);
            assert!(
                custody_with(
                    wrong_cwd_holder.pid,
                    &wrong_cwd_holder.birth,
                    &fixture.repo,
                    || {}
                )
                .is_err()
            );
        }

        // Mid-flight repo swap.
        {
            let holder2 = fixture.spawn_holder(default_spec(&fixture));
            let swap = custody_with(holder2.pid, &holder2.birth, &fixture.repo, || {
                let away = fixture.base.join("away-repo");
                fs::rename(&fixture.repo, &away).unwrap();
                fs::rename(&fixture.other_repo, &fixture.repo).unwrap();
            });
            assert!(swap.is_err());
        }
        // Restore repo for next sub-test.
        let away = fixture.base.join("away-repo");
        fs::rename(&fixture.repo, &fixture.other_repo).unwrap();
        fs::rename(&away, &fixture.repo).unwrap();

        // Mid-flight process exit.
        let mut holder3 = fixture.spawn_holder(default_spec(&fixture));
        let pid3 = holder3.pid;
        let birth3 = holder3.birth.clone();
        let exited = custody_with(pid3, &birth3, &fixture.repo, || {
            holder3.kill_and_wait();
        });
        assert!(exited.is_err());
        // Dead pid after exit also fails.
        assert!(agy_lead_custody(pid3, &birth3, &fixture.repo).is_none());
    }

    #[test]
    fn presence_lock_anomalies_and_forked_or_shared_locks_are_refused() {
        let fixture = Fixture::new("locks");
        let lock2 = fixture.lock_path(UUID2);

        // Multiple FLOCK WRITE presence locks.
        {
            let mut multi_spec = default_spec(&fixture);
            multi_spec.extra_lock_file = Some(&lock2);
            let multi = fixture.spawn_holder(multi_spec);
            assert!(
                custody_with(multi.pid, &multi.birth, &fixture.repo, || {}).err()
                    == Some("multiple-presence")
            );
        }

        // FLOCK WRITE + extra unlocked presence fd.
        {
            let prelude_extra = format!("exec 8>{}", shell_quote(lock2.to_str().unwrap()));
            let mut extra_spec = default_spec(&fixture);
            extra_spec.prelude = &prelude_extra;
            let extra = fixture.spawn_holder(extra_spec);
            assert!(custody_with(extra.pid, &extra.birth, &fixture.repo, || {}).is_err());
        }

        // Deleted presence fd.
        {
            let victim = fixture.lock_path(OTHER_UUID);
            write_private(&victim, "");
            let prelude_del = format!(
                "exec 8>{}; rm -f {}",
                shell_quote(victim.to_str().unwrap()),
                shell_quote(victim.to_str().unwrap())
            );
            let mut del_spec = default_spec(&fixture);
            del_spec.prelude = &prelude_del;
            let del = fixture.spawn_holder(del_spec);
            assert!(
                custody_with(del.pid, &del.birth, &fixture.repo, || {}).err()
                    == Some("presence-fd")
            );
        }

        // Non-UUID / non-.lock entry under presence/.
        {
            let bad_name = fixture.presence.join("not-a-uuid.lock");
            write_private(&bad_name, "");
            let prelude_bad = format!("exec 8>{}", shell_quote(bad_name.to_str().unwrap()));
            let mut bad_spec = default_spec(&fixture);
            bad_spec.prelude = &prelude_bad;
            let bad = fixture.spawn_holder(bad_spec);
            assert!(
                custody_with(bad.pid, &bad.birth, &fixture.repo, || {}).err()
                    == Some("presence-fd")
            );
        }

        // Foreign presence/ fd.
        {
            let foreign_dir = fixture.base.join("elsewhere/presence");
            mkdir_private(&foreign_dir);
            let foreign_lock = foreign_dir.join(format!("{UUID1}.lock"));
            write_private(&foreign_lock, "");
            let prelude_foreign = format!("exec 8>{}", shell_quote(foreign_lock.to_str().unwrap()));
            let mut foreign_spec = default_spec(&fixture);
            foreign_spec.prelude = &prelude_foreign;
            let foreign = fixture.spawn_holder(foreign_spec);
            assert!(
                custody_with(foreign.pid, &foreign.birth, &fixture.repo, || {}).err()
                    == Some("foreign-presence")
            );
        }

        // Unlocked .lock fd (no flock).
        {
            let prelude_unlocked = format!(
                "exec 8>{}",
                shell_quote(fixture.lock_path(UUID1).to_str().unwrap())
            );
            let mut unlocked_spec = default_spec(&fixture);
            unlocked_spec.use_flock = false;
            unlocked_spec.prelude = &prelude_unlocked;
            let unlocked = fixture.spawn_holder(unlocked_spec);
            assert!(
                custody_with(unlocked.pid, &unlocked.birth, &fixture.repo, || {}).err()
                    == Some("presence-flock")
            );
        }

        // Shared FLOCK READ (flock -s -n -F).
        {
            let mut shared_spec = default_spec(&fixture);
            shared_spec.flock_flag = Some("-s");
            let shared = fixture.spawn_holder(shared_spec);
            assert!(
                custody_with(shared.pid, &shared.birth, &fixture.repo, || {}).err()
                    == Some("presence-flock")
            );
        }

        // Forked-owner FLOCK (pid mismatch when flock forks without -F).
        {
            let mut forked_spec = default_spec(&fixture);
            forked_spec.no_fork = false;
            let forked = fixture.spawn_holder(forked_spec);
            assert!(
                custody_with(forked.pid, &forked.birth, &fixture.repo, || {}).err()
                    == Some("presence-flock")
            );
        }
    }

    #[test]
    fn fdinfo_flock_parser_rejects_every_non_unique_write_flock_shape() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("oms-agy-fdinfo-{}", std::process::id()));
        mkdir_private(&dir);
        let file = dir.join("probe.lock");
        write_private(&file, "");
        let fd = rfs::open(&file, rfs::OFlags::RDONLY, rfs::Mode::empty()).unwrap();
        let st: Stat = rfs::fstat(&fd).unwrap();
        let _ = fs::remove_dir_all(&dir);

        let maj = major(st.st_dev);
        let min = minor(st.st_dev);
        let ino = st.st_ino;
        let pid = 1234u32;

        let valid = format!(
            "pos:\t0\nflags:\t0100000\nmnt_id:\t32\nino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"
        );
        assert!(verify_unique_write_flock(&valid, pid, &st).is_ok());

        let bad_cases = [
            // Unlocked (no lock: line).
            format!("pos:\t0\nino:\t{ino}\n"),
            // Missing ino: line.
            format!("lock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"),
            // Duplicate ino: line.
            format!(
                "ino:\t{ino}\nino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"
            ),
            // Mismatched ino: line.
            format!(
                "ino:\t{}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n",
                ino + 1
            ),
            // POSIX instead of FLOCK.
            format!(
                "ino:\t{ino}\nlock:\t1: POSIX  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"
            ),
            // READ instead of WRITE.
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  READ {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"
            ),
            // Waiter `->`.
            format!(
                "ino:\t{ino}\nlock:\t1: -> FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"
            ),
            // Partial range.
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 100\n"
            ),
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 1 EOF\n"
            ),
            // Wrong pid.
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {} {maj:02x}:{min:02x}:{ino} 0 EOF\n",
                pid + 1
            ),
            // Wrong major/minor/ino or invalid hex length/case.
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {:02x}:{min:02x}:{ino} 0 EOF\n",
                maj + 1
            ),
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{:02x}:{ino} 0 EOF\n",
                min + 1
            ),
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{} 0 EOF\n",
                ino + 1
            ),
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {:x}:{min:02x}:{ino} 0 EOF\n",
                maj & 0xf
            ),
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} 000000001:{min:02x}:{ino} 0 EOF\n"
            ),
            // Multiple lock: lines.
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\nlock:\t2: FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"
            ),
            // Mandatory instead of advisory.
            format!(
                "ino:\t{ino}\nlock:\t1: FLOCK  MANDATORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"
            ),
            // Bad seq token.
            format!(
                "ino:\t{ino}\nlock:\t1 FLOCK  ADVISORY  WRITE {pid} {maj:02x}:{min:02x}:{ino} 0 EOF\n"
            ),
        ];

        for bad in bad_cases {
            assert!(
                verify_unique_write_flock(&bad, pid, &st).is_err(),
                "accepted bad fdinfo: {bad}"
            );
        }

        // Uppercase hex major/minor rejection on a device ID with letters.
        let mut hex_st = st;
        hex_st.st_dev = rfs::makedev(0xab, 0xcd);
        let lower =
            format!("ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} ab:cd:{ino} 0 EOF\n");
        let upper =
            format!("ino:\t{ino}\nlock:\t1: FLOCK  ADVISORY  WRITE {pid} AB:cd:{ino} 0 EOF\n");
        assert!(verify_unique_write_flock(&lower, pid, &hex_st).is_ok());
        assert!(verify_unique_write_flock(&upper, pid, &hex_st).is_err());
    }

    #[test]
    fn strict_0o022_policy_symlinks_hardlinks_and_midflight_replacements_fail_closed() {
        // Explicit .gemini 0o775 refusal test.
        {
            let fixture = Fixture::new("gemini-775");
            let holder = fixture.spawn_holder(default_spec(&fixture));
            mode(&fixture.gemini, 0o775);
            assert!(
                custody_with(holder.pid, &holder.birth, &fixture.repo, || {}).err()
                    == Some("gemini-dir")
            );
        }

        // 0o020 / 0o002 permission failures across every path element.
        type TargetFn = fn(&Fixture) -> PathBuf;
        let perm_cases: [(&str, TargetFn, u32, &str); 14] = [
            ("home-g", |f| f.home.clone(), 0o720, "home"),
            ("home-o", |f| f.home.clone(), 0o702, "home"),
            ("gemini-g", |f| f.gemini.clone(), 0o720, "gemini-dir"),
            ("gemini-o", |f| f.gemini.clone(), 0o702, "gemini-dir"),
            ("config-g", |f| f.config_path(), 0o720, "gemini-config"),
            ("config-o", |f| f.config_path(), 0o702, "gemini-config"),
            ("app-g", |f| f.app_root.clone(), 0o720, "app-root"),
            ("app-o", |f| f.app_root.clone(), 0o702, "app-root"),
            ("settings-g", |f| f.settings_path(), 0o620, "settings"),
            ("settings-o", |f| f.settings_path(), 0o602, "settings"),
            ("presence-g", |f| f.presence.clone(), 0o720, "presence-dir"),
            ("presence-o", |f| f.presence.clone(), 0o702, "presence-dir"),
            ("lock-g", |f| f.lock_path(UUID1), 0o620, "presence-fd"),
            ("lock-o", |f| f.lock_path(UUID1), 0o602, "presence-fd"),
        ];
        for (label, target_fn, bad_mode, expected_err) in perm_cases {
            let fixture = Fixture::new(label);
            mkdir_private(&fixture.config_path());
            write_private(&fixture.settings_path(), "{}");
            let holder = fixture.spawn_holder(default_spec(&fixture));
            mode(&target_fn(&fixture), bad_mode);
            let got = custody_with(holder.pid, &holder.birth, &fixture.repo, || {});
            assert!(
                got.as_ref().err() == Some(&expected_err),
                "{label}: expected {expected_err}, got {:?}",
                got.as_ref().err()
            );
        }

        // Hardlinks (st_nlink == 2) on settings.json and <uuid>.lock.
        {
            let fixture = Fixture::new("hardlink-settings");
            write_private(&fixture.settings_path(), "{}");
            let holder = fixture.spawn_holder(default_spec(&fixture));
            fs::hard_link(fixture.settings_path(), fixture.base.join("twin-settings")).unwrap();
            assert!(
                custody_with(holder.pid, &holder.birth, &fixture.repo, || {}).err()
                    == Some("settings")
            );
        }
        {
            let fixture = Fixture::new("hardlink-lock");
            let holder = fixture.spawn_holder(default_spec(&fixture));
            fs::hard_link(fixture.lock_path(UUID1), fixture.base.join("twin-lock")).unwrap();
            assert!(custody_with(holder.pid, &holder.birth, &fixture.repo, || {}).is_err());
        }

        // Symlinks (live and dangling) across every path element.
        let symlink_cases: [(&str, TargetFn, bool); 9] = [
            ("sym-home", |f| f.home.clone(), false),
            ("sym-gemini", |f| f.gemini.clone(), false),
            ("sym-config-live", |f| f.config_path(), false),
            ("sym-config-dangling", |f| f.config_path(), true),
            ("sym-app", |f| f.app_root.clone(), false),
            ("sym-settings-live", |f| f.settings_path(), false),
            ("sym-settings-dangling", |f| f.settings_path(), true),
            ("sym-presence", |f| f.presence.clone(), false),
            ("sym-lock", |f| f.lock_path(UUID1), false),
        ];
        for (label, target_fn, dangling) in symlink_cases {
            let fixture = Fixture::new(label);
            mkdir_private(&fixture.config_path());
            write_private(&fixture.settings_path(), "{}");
            let holder = fixture.spawn_holder(default_spec(&fixture));
            let target = target_fn(&fixture);
            let moved = fixture.base.join("real-target");
            fs::rename(&target, &moved).unwrap();
            if dangling {
                symlink(fixture.base.join("nonexistent-target"), &target).unwrap();
            } else {
                symlink(&moved, &target).unwrap();
            }
            assert!(
                custody_with(holder.pid, &holder.birth, &fixture.repo, || {}).is_err(),
                "symlink case {label} should fail"
            );
        }

        // Mid-flight replacements, chmod, hardlink, and optional creation during between().
        type Mutate = fn(&Fixture);
        let midflight_cases: [(&str, bool, Mutate); 8] = [
            ("swap-gemini", true, |f| {
                let away = f.base.join("away-gemini");
                fs::rename(&f.gemini, &away).unwrap();
                copy_tree_exact(&away, &f.gemini);
            }),
            ("swap-config", true, |f| {
                let away = f.base.join("away-config");
                fs::rename(f.config_path(), &away).unwrap();
                copy_tree_exact(&away, &f.config_path());
            }),
            ("swap-app", true, |f| {
                let away = f.base.join("away-app");
                fs::rename(&f.app_root, &away).unwrap();
                copy_tree_exact(&away, &f.app_root);
            }),
            ("swap-presence", true, |f| {
                let away = f.base.join("away-presence");
                fs::rename(&f.presence, &away).unwrap();
                copy_tree_exact(&away, &f.presence);
            }),
            ("chmod-gemini-mid", true, |f| mode(&f.gemini, 0o775)),
            ("hardlink-lock-mid", true, |f| {
                fs::hard_link(f.lock_path(UUID1), f.base.join("mid-twin")).unwrap();
            }),
            ("create-config-mid", false, |f| {
                mkdir_private(&f.config_path());
            }),
            ("create-settings-mid", false, |f| {
                write_private(&f.settings_path(), "{}");
            }),
        ];
        for (label, precreate_opt, mutate) in midflight_cases {
            let fixture = Fixture::new(label);
            if precreate_opt {
                mkdir_private(&fixture.config_path());
                write_private(&fixture.settings_path(), "{}");
            }
            let holder = fixture.spawn_holder(default_spec(&fixture));
            let got = custody_with(holder.pid, &holder.birth, &fixture.repo, || {
                mutate(&fixture)
            });
            assert!(got.is_err(), "midflight case {label} should fail");
        }
    }

    #[test]
    fn env_cmdline_and_settings_overrides_and_malformations_are_refused() {
        let fixture = Fixture::new("overrides");

        // Live process env overrides.
        type EnvSlice<'a> = &'a [(&'a str, &'a str)];
        let env_cases: [(&str, bool, EnvSlice<'_>); 8] = [
            ("antigravity-var", true, &[("ANTIGRAVITY_HOME", "/tmp/x")]),
            ("agy-var", true, &[("AGY_MODEL", "opus")]),
            ("oms-gemini", true, &[("OMS_GEMINI_HOME", "/tmp/x")]),
            (
                "oms-agy-set",
                true,
                &[("OMS_ANTIGRAVITY_SETTINGS", "/tmp/x")],
            ),
            ("gemini-cli-home", true, &[("GEMINI_CLI_HOME", "/tmp/x")]),
            ("missing-home", false, &[]),
            ("relative-home", false, &[("HOME", "rel/home")]),
            ("dotdot-home", false, &[("HOME", "/tmp/../tmp")]),
        ];
        for (label, include_home, extra_env) in env_cases {
            let mut spec = default_spec(&fixture);
            spec.include_home = include_home;
            spec.extra_env = extra_env;
            let holder = fixture.spawn_holder(spec);
            assert!(
                custody_with(holder.pid, &holder.birth, &fixture.repo, || {}).is_err(),
                "env override {label} should fail"
            );
        }

        // Unit tests for validate_environ edge cases (including duplicate HOME & malformed entries).
        assert!(validate_environ(b"HOME=/tmp/h\0PATH=/bin\0").is_ok());
        for bad_env in [
            b"".as_slice(),
            b"HOME=/tmp/a\0HOME=/tmp/b\0",
            b"HOME=\0",
            b"HOME=/tmp/./a\0",
            b"HOME=/tmp/../a\0",
            b"HOME=rel\0",
            b"MALFORMED\0HOME=/tmp/a\0",
            b"=val\0HOME=/tmp/a\0",
            b"HOME=/tmp/a\0ANTIGRAVITY_FOO=\0",
            b"HOME=/tmp/a\0AGY_FOO=1\0",
        ] {
            assert!(validate_environ(bad_env).is_err(), "{bad_env:?}");
        }

        // Live process cmdline rejections.
        let live_bad_cmds: [(&str, &str, &[&str]); 6] = [
            ("wrong-argv0", "not-agy", &[]),
            ("rel-argv0", "./agy", &[]),
            ("dotdot-argv0", "/usr/bin/../bin/agy", &[]),
            ("flag-p", "agy", &["-p"]),
            ("flag-dashdash", "agy", &["--"]),
            ("missing-i-val", "agy", &["-i"]),
        ];
        for (label, argv0, flags) in live_bad_cmds {
            let mut spec = default_spec(&fixture);
            spec.argv0 = argv0;
            spec.bash_flags = flags;
            let holder = fixture.spawn_holder(spec);
            assert!(
                custody_with(holder.pid, &holder.birth, &fixture.repo, || {}).is_err(),
                "live cmdline {label} should fail"
            );
        }

        // Pure cmdline parser unit tests covering all allowed & refused flags.
        assert!(validate_cmdline(b"agy\0").is_ok());
        assert!(validate_cmdline(b"/agy\0").is_ok());
        assert!(validate_cmdline(b"/usr/local/bin/agy\0").is_ok());
        assert!(
            validate_cmdline(
                b"agy\0-c\0--continue\0--dangerously-skip-permissions\0--disable-slash-commands\0--sandbox\0\
                  --model\0opus\0--mode=plan\0--effort\0high\0--agent\0lead\0-i\0hi\0\
                  --prompt-interactive\0--settings\0--log-file=/tmp/l\0--conversation\0id-1\0"
            )
            .is_ok()
        );
        assert!(validate_cmdline(b"agy\0--prompt-interactive=--settings\0").is_ok());

        let bad_cmdlines: [&[u8]; 25] = [
            b"",
            b"agy",
            b"\0",
            b"agy/\0",
            b"/usr//bin/agy\0",
            b"/usr/bin/./agy\0",
            b"agy\0\0",
            b"agy\0--settings\0/tmp/s.json\0",
            b"agy\0--settings=/tmp/s.json\0",
            b"agy\0--remote-agent\0x\0",
            b"agy\0--remote-control\0",
            b"agy\0--project\0/tmp/p\0",
            b"agy\0--new-project\0",
            b"agy\0--add-dir\0/tmp/d\0",
            b"agy\0-p\0hi\0",
            b"agy\0--print\0",
            b"agy\0--prompt\0hi\0",
            b"agy\0--input-format\0json\0",
            b"agy\0--output-format\0json\0",
            b"agy\0--json-schema\0{}\0",
            b"agy\0--\0",
            b"agy\0--unknown-flag\0",
            b"agy\0--model\0",
            b"agy\0--model=\0",
            b"agy\0subcommand\0",
        ];
        for bad in bad_cmdlines {
            assert!(validate_cmdline(bad).is_err(), "{bad:?}");
        }
        assert!(validate_cmdline(b"agy\0-i=hi\0").is_err());
        assert!(validate_cmdline(b"agy\0--continue=true\0").is_err());
        assert!(validate_cmdline(b"agy\0--model\0\0").is_err());

        // settings.json overrides, malformed JSON, non-object JSON, and mid-flight mutation.
        {
            let holder = fixture.spawn_holder(default_spec(&fixture));
            for bad_json in [
                "",
                "{not json",
                "[]",
                "\"str\"",
                "{\"a\":1,\"a\":2}",
                "{\"appDataDir\":\"/tmp/x\"}",
                "{\"app_data_dir\":\"/tmp/x\"}",
                "{\"data-dir\":\"/tmp/x\"}",
                "{\"GEMINI_DIR\":\"/tmp/x\"}",
                "{\"configDir\":\"/tmp/x\"}",
                "{\"brainPath\":\"/tmp/x\"}",
                "{\"presenceDir\":\"/tmp/x\"}",
                "{\"remoteUrl\":\"https://example.test\"}",
                "{\"workspaceRoot\":\"/tmp/x\"}",
                "{\"projectDir\":\"/tmp/x\"}",
                "{\"nested\":{\"presence\":\"/tmp/x\"}}",
            ] {
                write_private(&fixture.settings_path(), bad_json);
                assert!(
                    custody_with(holder.pid, &holder.birth, &fixture.repo, || {}).is_err(),
                    "settings {bad_json} should fail"
                );
            }

            // Settings > 64 KiB fails.
            let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(65 * 1024));
            write_private(&fixture.settings_path(), &big);
            assert!(custody_with(holder.pid, &holder.birth, &fixture.repo, || {}).is_err());

            // Mid-flight settings mutation (in-place write and atomic rename).
            write_private(&fixture.settings_path(), r#"{"model":"opus"}"#);
            let in_place = custody_with(holder.pid, &holder.birth, &fixture.repo, || {
                write_private(&fixture.settings_path(), r#"{"model":"sonnet"}"#);
            });
            assert!(in_place.err() == Some("settings-changed"));

            let swapped = custody_with(holder.pid, &holder.birth, &fixture.repo, || {
                let tmp = fixture.base.join("new-settings.json");
                write_private(&tmp, r#"{"model":"haiku"}"#);
                fs::rename(&tmp, fixture.settings_path()).unwrap();
            });
            assert!(swapped.is_err());
        }

        // Pure unit checks for UUID, fd budget, >256 KiB proc buffers, & settings object checker.
        assert!(is_uuid_lower_hex(UUID1) && is_uuid_lower_hex(UUID2));
        assert!(!is_uuid_lower_hex(&UUID2.to_uppercase()));
        assert!(!is_uuid_lower_hex("11111111-2222-3333-4444-55555555555"));
        assert!(fd_within_budget(4096).is_ok());
        assert!(fd_within_budget(4097).err() == Some("fd-budget"));
        let mut big_cmd = b"agy\0--model\0".to_vec();
        big_cmd.resize(256 * 1024, b'x');
        big_cmd.push(b'\0');
        assert!(validate_cmdline(&big_cmd).is_err());
        let mut big_env = b"HOME=/tmp/h\0BIG=".to_vec();
        big_env.resize(256 * 1024 + 1, b'x');
        assert!(validate_environ(&big_env).is_err());
        let benign = decode(br#"{"model":"opus","items":[{"ok":true}]}"#, 1024).unwrap();
        assert!(check_settings_object(&benign).is_ok());

        // Mid-flight lock file replacement and cmdline replacement.
        let _ = fs::remove_file(fixture.settings_path());
        {
            let holder_lock = fixture.spawn_holder(default_spec(&fixture));
            let swapped_lock =
                custody_with(holder_lock.pid, &holder_lock.birth, &fixture.repo, || {
                    let tmp = fixture.base.join("replacement.lock");
                    write_private(&tmp, "");
                    fs::rename(&tmp, fixture.lock_path(UUID1)).unwrap();
                });
            assert!(swapped_lock.is_err());
        }

        {
            let mut holder_exec = fixture.spawn_holder(default_spec(&fixture));
            let pid_exec = holder_exec.pid;
            let birth_exec = holder_exec.birth.clone();
            let cmd_changed = custody_with(pid_exec, &birth_exec, &fixture.repo, || {
                let stdin = holder_exec.stdin.as_mut().unwrap();
                writeln!(stdin, "exec -a not-agy bash").unwrap();
                stdin.flush().unwrap();
                holder_exec.run_cmd("true", "AFTER_EXEC");
            });
            assert!(cmd_changed.is_err());
        }
    }
}
