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
pub fn wrap_command(
    program: &OsStr,
    args: &[OsString],
    spec: &SandboxSpec,
    mode: SandboxMode,
) -> Result<(OsString, Vec<OsString>, &'static str), String> {
    let original = || {
        (
            program.to_os_string(),
            args.to_vec(),
            "unsandboxed-fallback",
        )
    };
    if mode == SandboxMode::Off {
        return Ok((program.to_os_string(), args.to_vec(), "disabled"));
    }
    #[cfg(target_os = "linux")]
    if let Some(binary) = find_binary("OMS_BWRAP_BIN", "bwrap") {
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
        wrapped.extend([
            OsString::from("--chdir"),
            spec.worktree.as_os_str().into(),
            "--die-with-parent".into(),
        ]);
        if !spec.allow_network {
            wrapped.push("--unshare-net".into());
        }
        wrapped.push("--".into());
        wrapped.push(program.into());
        wrapped.extend_from_slice(args);
        return Ok((binary, wrapped, "bwrap"));
    }
    #[cfg(target_os = "macos")]
    if let Some(binary) = find_binary("OMS_SANDBOX_EXEC_BIN", "/usr/bin/sandbox-exec") {
        let mut wrapped = vec!["-p".into(), sbpl_profile(spec).into(), program.into()];
        wrapped.extend_from_slice(args);
        return Ok((binary, wrapped, "sandbox-exec"));
    }
    if mode == SandboxMode::Strict {
        Err("worker sandbox backend unavailable".into())
    } else {
        Ok(original())
    }
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
            ("cmd".into(), args, "disabled")
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_override_and_strict_failure() {
        if let Some(case) = std::env::var_os("OMS_SANDBOX_TEST_CASE") {
            let spec = SandboxSpec {
                worktree: PathBuf::from("/tmp"),
                extra_rw_dirs: vec![PathBuf::from("/dev"), PathBuf::from("/not-a-real-oms-dir")],
                allow_network: false,
            };
            let result = wrap_command(
                OsStr::new("echo"),
                &["hello".into()],
                &spec,
                SandboxMode::Strict,
            );
            if case == "missing" {
                assert!(result.is_err());
                return;
            }
            let (binary, args, backend) = result.unwrap();
            assert_eq!(binary, OsString::from("/bin/sh"));
            assert_eq!(backend, "bwrap");
            let expected: Vec<OsString> = [
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--tmpfs",
                "/tmp",
                "--bind",
                "/tmp",
                "/tmp",
                "--bind",
                "/dev",
                "/dev",
                "--chdir",
                "/tmp",
                "--die-with-parent",
                "--unshare-net",
                "--",
                "echo",
                "hello",
            ]
            .into_iter()
            .map(Into::into)
            .collect();
            assert_eq!(args, expected);
            return;
        }
        for (case, binary) in [("present", "/bin/sh"), ("missing", "/not-a-real-oms-bin")] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::tests::linux_override_and_strict_failure",
                ])
                .env("OMS_SANDBOX_TEST_CASE", case)
                .env("OMS_BWRAP_BIN", binary)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }
}
