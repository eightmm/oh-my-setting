use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxMode {
    Auto,
    Strict,
    Off,
}
impl SandboxMode {
    pub fn from_env() -> Self {
        match std::env::var("OMS_WORKER_SANDBOX").as_deref() {
            Ok("strict") => Self::Strict,
            Ok("off" | "0") => Self::Off,
            _ => Self::Auto,
        }
    }
}
#[derive(Clone, Debug)]
pub struct SandboxSpec {
    pub worktree: PathBuf,
    pub extra_rw_dirs: Vec<PathBuf>,
    pub allow_network: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WrappedCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub remove_env: Vec<OsString>,
    pub backend: &'static str,
}
impl WrappedCommand {
    pub fn command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.args);
        for key in &self.remove_env {
            cmd.env_remove(key);
        }
        cmd
    }
}

/// Directories a write-mode worker in a linked `git worktree` must be able to
/// write: its private gitdir and the shared common dir (objects, refs).
pub fn git_writable_dirs(worktree: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(worktree.join(".git")) else {
        return Vec::new();
    };
    let Some(raw) = text.lines().find_map(|l| l.strip_prefix("gitdir:")) else {
        return Vec::new();
    };
    let Ok(gitdir) = worktree.join(raw.trim()).canonicalize() else {
        return Vec::new();
    };
    let mut dirs = vec![gitdir.clone()];
    if let Ok(common) = std::fs::read_to_string(gitdir.join("commondir"))
        && let Ok(common) = gitdir.join(common.trim()).canonicalize()
        && !dirs.contains(&common)
    {
        dirs.push(common);
    }
    dirs
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}
fn find_binary(override_name: &str, default: &str) -> Option<OsString> {
    if let Some(value) = std::env::var_os(override_name) {
        return executable(Path::new(&value)).then_some(value);
    }
    if default.contains('/') {
        return executable(Path::new(default)).then(|| default.into());
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(default))
        .find(|p| executable(p))
        .map(PathBuf::into_os_string)
}
#[allow(dead_code)]
fn sbpl_profile(spec: &SandboxSpec) -> String {
    fn escaped(path: &Path) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }
    let mut profile = String::from(
        "(version 1)(allow default)(deny file-write*)(allow file-write* (subpath \"/private/tmp\") (subpath \"/tmp\")",
    );
    for path in
        std::iter::once(&spec.worktree).chain(spec.extra_rw_dirs.iter().filter(|p| p.is_dir()))
    {
        profile.push_str(&format!(" (subpath \"{}\")", escaped(path)));
    }
    profile.push(')');
    if !spec.allow_network {
        profile.push_str("(deny network*)");
    }
    profile
}
pub fn find_bwrap() -> Option<OsString> {
    find_binary("OMS_BWRAP_BIN", "bwrap")
}
fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}
pub fn in_worktree(dir: &Path, worktree: &Path) -> bool {
    match (dir.canonicalize(), worktree.canonicalize()) {
        (Ok(canon_dir), Ok(canon_wt)) => canon_dir.starts_with(&canon_wt),
        _ => dir.starts_with(worktree),
    }
}
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}
fn push_hidden_dir(hidden: &mut Vec<PathBuf>, dir: PathBuf) {
    let tmp_canon = Path::new("/tmp").canonicalize().ok();
    let canon = dir.canonicalize().ok();
    if dir == Path::new("/")
        || dir == Path::new("/tmp")
        || (tmp_canon.is_some() && canon == tmp_canon)
        || !dir.is_dir()
    {
        return;
    }
    let already = hidden.iter().any(|existing| {
        existing == &dir
            || canon
                .as_ref()
                .is_some_and(|c| existing.canonicalize().ok().as_ref() == Some(c))
    });
    if !already {
        hidden.push(dir);
    }
}
pub fn bwrap_args(program: &OsStr, args: &[OsString], spec: &SandboxSpec) -> Vec<OsString> {
    let mut wrapped: Vec<OsString> = [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    for path in
        std::iter::once(&spec.worktree).chain(spec.extra_rw_dirs.iter().filter(|p| p.is_dir()))
    {
        wrapped.push("--bind".into());
        wrapped.push(path.as_os_str().into());
        wrapped.push(path.as_os_str().into());
    }
    let mut hidden = Vec::new();
    if let Some(home) = nonempty_env("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        push_hidden_dir(&mut hidden, home.join(".ssh"));
    }
    if let Some(runtime) = nonempty_env("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        push_hidden_dir(&mut hidden, runtime.join("oms-ssh"));
    }
    let mut masked_socket = None;
    if let Some(sock) = nonempty_env("SSH_AUTH_SOCK")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        && let Some(agent_dir) = sock.parent()
        && !in_worktree(agent_dir, &spec.worktree)
    {
        // A socket placed directly in a shared directory (home, the runtime dir, an ancestor of a
        // writable bind) is masked on its own; an empty tmpfs there would hide unrelated state.
        let shared = ["HOME", "XDG_RUNTIME_DIR"]
            .into_iter()
            .filter_map(nonempty_env)
            .map(PathBuf::from)
            .any(|dir| same_dir(&dir, agent_dir))
            || std::iter::once(&spec.worktree)
                .chain(spec.extra_rw_dirs.iter())
                .any(|dir| in_worktree(dir, agent_dir));
        if !shared {
            push_hidden_dir(&mut hidden, agent_dir.to_path_buf());
        } else if !in_worktree(&sock, Path::new("/tmp")) && std::fs::symlink_metadata(&sock).is_ok()
        {
            masked_socket = Some(sock);
        }
    }
    for dir in hidden {
        wrapped.push("--tmpfs".into());
        wrapped.push(dir.into_os_string());
    }
    if let Some(sock) = masked_socket {
        wrapped.push("--ro-bind".into());
        wrapped.push("/dev/null".into());
        wrapped.push(sock.into_os_string());
    }
    wrapped.extend([
        OsString::from("--unsetenv"),
        "SSH_AUTH_SOCK".into(),
        "--chdir".into(),
        spec.worktree.as_os_str().into(),
        "--die-with-parent".into(),
    ]);
    if !spec.allow_network {
        wrapped.push("--unshare-net".into());
    }
    wrapped.push("--".into());
    wrapped.push(program.into());
    wrapped.extend_from_slice(args);
    wrapped
}
pub fn wrap_command(
    program: &OsStr,
    args: &[OsString],
    spec: &SandboxSpec,
    mode: SandboxMode,
) -> Result<WrappedCommand, String> {
    let fallback = || WrappedCommand {
        program: program.to_os_string(),
        args: args.to_vec(),
        remove_env: vec!["SSH_AUTH_SOCK".into()],
        backend: "unsandboxed-fallback",
    };
    if mode == SandboxMode::Off {
        return Ok(WrappedCommand {
            program: program.to_os_string(),
            args: args.to_vec(),
            remove_env: Vec::new(),
            backend: "disabled",
        });
    }
    #[cfg(target_os = "linux")]
    if let Some(binary) = find_bwrap() {
        return Ok(WrappedCommand {
            program: binary,
            args: bwrap_args(program, args, spec),
            remove_env: vec!["SSH_AUTH_SOCK".into()],
            backend: "bwrap",
        });
    }
    #[cfg(target_os = "macos")]
    if let Some(binary) = find_binary("OMS_SANDBOX_EXEC_BIN", "/usr/bin/sandbox-exec") {
        let mut wrapped = vec!["-p".into(), sbpl_profile(spec).into(), program.into()];
        wrapped.extend_from_slice(args);
        return Ok(WrappedCommand {
            program: binary,
            args: wrapped,
            remove_env: vec!["SSH_AUTH_SOCK".into()],
            backend: "sandbox-exec",
        });
    }
    if mode == SandboxMode::Strict {
        Err("worker sandbox backend unavailable".into())
    } else {
        Ok(fallback())
    }
}

/// A verifier executes an acknowledged checkout without seeing host homes,
/// sockets, or processes. Unlike ordinary workers this profile has no host
/// root bind and no fallback. The caller supplies the trusted read-only tools.
#[cfg(target_os = "linux")]
pub fn isolated_checkout(
    program: &OsStr,
    args: &[OsString],
    checkout: &Path,
    read_only: &[(PathBuf, PathBuf)],
) -> Result<(OsString, Vec<OsString>), String> {
    let binary = ["/usr/bin/bwrap", "/usr/local/bin/bwrap"]
        .into_iter()
        .find(|path| executable(Path::new(path)))
        .ok_or("system bubblewrap is unavailable")?;
    let mut wrapped: Vec<OsString> = [
        "--unshare-pid",
        "--unshare-net",
        "--unshare-ipc",
        "--new-session",
        "--json-status-fd",
        "1",
        "--die-with-parent",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
        "--bind",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    wrapped.extend([checkout.as_os_str().into(), checkout.as_os_str().into()]);
    for (source, target) in read_only {
        wrapped.extend([
            "--ro-bind".into(),
            source.as_os_str().into(),
            target.as_os_str().into(),
        ]);
    }
    wrapped.extend([
        "--chdir".into(),
        checkout.as_os_str().into(),
        "--".into(),
        // Bubblewrap closes the status descriptor before starting its child.
        // Give the verifier a writable stdout without mixing it with status.
        "/bin/sh".into(),
        "-c".into(),
        "exec \"$@\" >&2".into(),
        "oms-batch-verifier".into(),
        program.into(),
    ]);
    wrapped.extend_from_slice(args);
    Ok((binary.into(), wrapped))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn off_and_profile_escaping() {
        let spec = SandboxSpec {
            worktree: PathBuf::from("/tmp/a\\b\"c"),
            extra_rw_dirs: vec![],
            allow_network: false,
        };
        let profile = sbpl_profile(&spec);
        assert!(profile.contains(r#"(subpath "/tmp/a\\b\"c")"#));
        assert!(profile.contains("(deny network*)"));
        let args = vec![OsString::from("arg")];
        assert_eq!(
            wrap_command(OsStr::new("cmd"), &args, &spec, SandboxMode::Off).unwrap(),
            WrappedCommand {
                program: "cmd".into(),
                args,
                remove_env: Vec::new(),
                backend: "disabled",
            }
        );
    }
    #[test]
    fn git_writable_dirs_resolves_linked_worktree() {
        let root = std::env::temp_dir().join(format!("oms-gwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let gitdir = root.join("main/.git/worktrees/w");
        let work = root.join("w");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(work.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        let dirs = git_writable_dirs(&work);
        let common = root.join("main/.git").canonicalize().unwrap();
        assert_eq!(dirs, vec![gitdir.canonicalize().unwrap(), common]);
        assert!(git_writable_dirs(&root).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_override_and_strict_failure() {
        use std::os::unix::fs::PermissionsExt;
        if let Some(case) = std::env::var_os("OMS_SANDBOX_TEST_CASE") {
            let root = PathBuf::from(std::env::var_os("OMS_SANDBOX_TEST_ROOT").unwrap());
            let home = root.join("home");
            let openssh = home.join(".ssh");
            let runtime = root.join("runtime");
            let control = runtime.join("oms-ssh");
            let worktree = root.join("wt");
            let outside_agent = root.join("agent");
            let claude_dir = home.join(".claude");
            let codex_dir = home.join(".codex");
            let gemini_dir = home.join(".gemini");
            let spec = SandboxSpec {
                worktree: worktree.clone(),
                extra_rw_dirs: vec![
                    claude_dir.clone(),
                    codex_dir.clone(),
                    gemini_dir.clone(),
                    root.join("absent-rw-dir"),
                ],
                allow_network: false,
            };
            if case == "missing" {
                let result = wrap_command(
                    OsStr::new("echo"),
                    &["hello".into()],
                    &spec,
                    SandboxMode::Strict,
                );
                assert!(result.is_err());
                return;
            }
            if case == "fallback" {
                let wrapped = wrap_command(
                    OsStr::new("/bin/sh"),
                    &["-c".into(), "test -z \"${SSH_AUTH_SOCK+x}\"".into()],
                    &spec,
                    SandboxMode::Auto,
                )
                .unwrap();
                assert_eq!(wrapped.backend, "unsandboxed-fallback");
                assert_eq!(wrapped.program, OsString::from("/bin/sh"));
                assert_eq!(wrapped.remove_env, vec![OsString::from("SSH_AUTH_SOCK")]);
                assert!(std::env::var_os("SSH_AUTH_SOCK").is_some());
                assert!(wrapped.command().status().unwrap().success());
                return;
            }
            let wrapped = wrap_command(
                OsStr::new("echo"),
                &["hello".into()],
                &spec,
                SandboxMode::Strict,
            )
            .unwrap();
            assert_eq!(wrapped.backend, "bwrap");
            assert_eq!(wrapped.remove_env, vec![OsString::from("SSH_AUTH_SOCK")]);
            let mut expected: Vec<OsString> = [
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--tmpfs",
                "/tmp",
            ]
            .into_iter()
            .map(Into::into)
            .collect();
            for rw in [&worktree, &claude_dir, &codex_dir, &gemini_dir] {
                expected.extend([
                    OsString::from("--bind"),
                    rw.as_os_str().into(),
                    rw.as_os_str().into(),
                ]);
            }
            expected.extend([
                OsString::from("--tmpfs"),
                openssh.as_os_str().into(),
                "--tmpfs".into(),
                control.as_os_str().into(),
            ]);
            if case == "present" {
                expected.extend([OsString::from("--tmpfs"), outside_agent.as_os_str().into()]);
            }
            // A socket directly in the runtime dir is masked alone; /tmp is already an empty tmpfs.
            let runtime_sock = runtime.join("agent.sock");
            if case == "runtime" && !in_worktree(&runtime_sock, Path::new("/tmp")) {
                expected.extend([
                    OsString::from("--ro-bind"),
                    "/dev/null".into(),
                    runtime_sock.as_os_str().into(),
                ]);
            }
            expected.extend([
                OsString::from("--unsetenv"),
                "SSH_AUTH_SOCK".into(),
                "--chdir".into(),
                worktree.as_os_str().into(),
                "--die-with-parent".into(),
                "--unshare-net".into(),
                "--".into(),
                "echo".into(),
                "hello".into(),
            ]);
            assert_eq!(wrapped.args, expected);
            if case == "inside" || case == "inside-sub" {
                let inside_sub = worktree.join("sub");
                assert!(
                    !wrapped.args.windows(2).any(|pair| {
                        pair[0] == "--tmpfs" && (pair[1] == worktree || pair[1] == inside_sub)
                    }),
                    "socket inside worktree must not be double-mounted: {:?}",
                    wrapped.args
                );
            }
            let captured = root.join("captured-args");
            let _ = std::fs::remove_file(&captured);
            assert!(
                wrapped
                    .command()
                    .env("OMS_CAPTURE_FILE", &captured)
                    .status()
                    .unwrap()
                    .success()
            );
            let lines: Vec<OsString> = std::fs::read_to_string(&captured)
                .unwrap()
                .lines()
                .map(Into::into)
                .collect();
            assert_eq!(lines, expected);
            return;
        }
        let root = std::env::temp_dir().join(format!("oms-sbx-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        for dir in [
            home.join(".ssh"),
            home.join(".claude"),
            home.join(".codex"),
            home.join(".gemini"),
            root.join("runtime").join("oms-ssh"),
            root.join("wt").join("sub"),
            root.join("agent"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let capture_bin = root.join("fake-bwrap");
        std::fs::write(
            &capture_bin,
            "#!/bin/sh\n: > \"$OMS_CAPTURE_FILE\"\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$OMS_CAPTURE_FILE\"; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&capture_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outside_sock = root.join("agent").join("agent.sock");
        let runtime_sock = root.join("runtime").join("agent.sock");
        std::fs::write(&runtime_sock, b"").unwrap();
        let inside_root_sock = root.join("wt").join("agent.sock");
        let inside_sub_sock = root.join("wt").join("sub").join("agent.sock");
        let missing_bin = root.join("not-a-real-oms-bin");
        for (case, binary, sock) in [
            ("present", &capture_bin, &outside_sock),
            ("runtime", &capture_bin, &runtime_sock),
            ("inside", &capture_bin, &inside_root_sock),
            ("inside-sub", &capture_bin, &inside_sub_sock),
            ("missing", &missing_bin, &outside_sock),
            ("fallback", &missing_bin, &outside_sock),
        ] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::tests::linux_override_and_strict_failure",
                ])
                .env("OMS_SANDBOX_TEST_CASE", case)
                .env("OMS_SANDBOX_TEST_ROOT", &root)
                .env("HOME", &home)
                .env("XDG_RUNTIME_DIR", root.join("runtime"))
                .env("SSH_AUTH_SOCK", sock)
                .env("OMS_BWRAP_BIN", binary)
                .env("OMS_SANDBOX_EXEC_BIN", &missing_bin)
                .status()
                .unwrap();
            assert!(status.success(), "case {case} failed");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn real_bwrap_hides_ssh_paths_and_agent() {
        if std::env::var_os("OMS_SANDBOX_REAL_BWRAP").is_some() {
            let Some(binary) = find_bwrap() else {
                println!("skipping real bwrap probe: bwrap is missing");
                return;
            };
            let preflight = std::process::Command::new(&binary)
                .args([
                    "--ro-bind",
                    "/",
                    "/",
                    "--die-with-parent",
                    "--",
                    "/bin/true",
                ])
                .output()
                .unwrap();
            if !preflight.status.success() {
                println!(
                    "skipping real bwrap probe: bwrap cannot create sandbox in this environment ({})",
                    String::from_utf8_lossy(&preflight.stderr).trim()
                );
                return;
            }
            let base = Path::new("/var").join("tmp");
            let parent = if base.is_dir() {
                base
            } else {
                std::env::temp_dir()
            };
            let root = parent.join(format!("oms-real-bwrap-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let home = root.join("home");
            let openssh = home.join(".ssh");
            let claude = home.join(".claude");
            let runtime = root.join("runtime");
            let control = runtime.join("oms-ssh");
            let agent_dir = root.join("agent");
            let worktree = root.join("wt");
            for dir in [&openssh, &claude, &control, &agent_dir, &worktree] {
                std::fs::create_dir_all(dir).unwrap();
            }
            let ssh_item = openssh.join("id_probe");
            let ctl_item = control.join("s-probe.sock");
            let agent_sock = agent_dir.join("agent.sock");
            let claude_item = claude.join("session");
            for file in [&ssh_item, &ctl_item, &agent_sock, &claude_item] {
                std::fs::write(file, b"probe\n").unwrap();
            }
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::tests::real_bwrap_hides_ssh_paths_and_agent_inner",
                    "--nocapture",
                ])
                .env_remove("OMS_BWRAP_BIN")
                .env("OMS_SANDBOX_REAL_ROOT", &root)
                .env("HOME", &home)
                .env("XDG_RUNTIME_DIR", &runtime)
                .env("SSH_AUTH_SOCK", &agent_sock)
                .status()
                .unwrap();
            let _ = std::fs::remove_dir_all(&root);
            assert!(status.success());
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sandbox::tests::real_bwrap_hides_ssh_paths_and_agent",
                "--nocapture",
            ])
            .env_remove("OMS_BWRAP_BIN")
            .env("OMS_SANDBOX_REAL_BWRAP", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn real_bwrap_hides_ssh_paths_and_agent_inner() {
        let Some(raw) = std::env::var_os("OMS_SANDBOX_REAL_ROOT") else {
            return;
        };
        let root = PathBuf::from(raw);
        let home = root.join("home");
        let ssh_item = home.join(".ssh").join("id_probe");
        let claude = home.join(".claude");
        let claude_item = claude.join("session");
        let ctl_item = root.join("runtime").join("oms-ssh").join("s-probe.sock");
        let agent_sock = root.join("agent").join("agent.sock");
        let worktree = root.join("wt");
        let spec = SandboxSpec {
            worktree,
            extra_rw_dirs: vec![claude],
            allow_network: true,
        };
        let wrapped = wrap_command(
            OsStr::new("/bin/sh"),
            &[
                "-c".into(),
                "test ! -e \"$1\" && test ! -e \"$2\" && test ! -e \"$3\" && test -z \"${SSH_AUTH_SOCK:-}\" && test -f \"$4\"".into(),
                "sh".into(),
                ssh_item.into_os_string(),
                ctl_item.into_os_string(),
                agent_sock.into_os_string(),
                claude_item.into_os_string(),
            ],
            &spec,
            SandboxMode::Strict,
        )
        .unwrap();
        assert_eq!(wrapped.backend, "bwrap");
        assert!(wrapped.command().status().unwrap().success());
    }
}
