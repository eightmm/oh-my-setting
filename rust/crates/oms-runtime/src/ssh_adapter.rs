//! OpenSSH transport for repository bundles and remote commands.
use crate::{Result, error, require, router::Response};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

const USAGE: &str =
    "usage: oms runtime ssh-adapter <probe|sync-bundle|exec> [options] [-- command...]";

fn ssh_control_path(host: &str, port: Option<u16>) -> Result<String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let uid = rustix::process::geteuid().as_raw();
    let suffix = format!(
        "s-{}.sock",
        &sha256(format!("{host}\0{}", port.unwrap_or(22)).as_bytes())[..16]
    );
    let mut candidates = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        let dir = PathBuf::from(runtime).join("oms-ssh");
        if dir.join(&suffix).to_string_lossy().len() <= 80 {
            candidates.push(dir);
        }
    }
    candidates.push(PathBuf::from(format!("/tmp/oms-ssh-{uid}")));
    for dir in candidates {
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => continue,
        }
        let Ok(meta) = std::fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid {
            continue;
        }
        if meta.permissions().mode() & 0o077 != 0
            && std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_err()
        {
            continue;
        }
        let control = dir.join(&suffix).to_string_lossy().into_owned();
        if control.len() <= 80 {
            return Ok(control);
        }
    }
    Err(error(
        "cannot provision user-private 0700 SSH control directory within socket limit",
    ))
}

pub fn run(args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> Response {
    execute(args, cwd, interrupted).unwrap_or_else(Response::failure)
}
fn execute(args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> Result<Response> {
    if args.is_empty()
        || args.iter().any(|a| a == "--help" || a == "-h") && !args.iter().any(|a| a == "--")
    {
        return Ok(Response::text(USAGE));
    }
    let action = args[0].to_str().ok_or_else(|| error("invalid action"))?;
    require(
        ["probe", "sync-bundle", "exec"].contains(&action),
        "unknown ssh-adapter action",
    )?;
    require(
        action == "probe"
            || std::env::var_os("OMS_HARNESS_CHILD").as_deref() != Some(OsStr::new("1")),
        "a harness child may only use read-only runtime actions",
    )?;
    let mut host = None;
    let mut port = None;
    let mut repo = None;
    let mut remote = None;
    let mut base = None;
    let mut pretty = false;
    let mut tail = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].to_str().ok_or_else(|| error("invalid option"))?;
        if flag == "--" {
            tail = args[i + 1..].to_vec();
            break;
        }
        if flag == "--pretty" {
            pretty = true;
            i += 1;
            continue;
        }
        if flag == "--json" && action != "exec" {
            i += 1;
            continue;
        }
        let slot = match flag {
            "--host" => &mut host,
            "--port" => &mut port,
            "--repo" if action != "probe" => &mut repo,
            "--remote-dir" if action != "probe" => &mut remote,
            "--base" if action == "sync-bundle" => &mut base,
            _ => return Err(error(format!("unknown option: {flag}"))),
        };
        i += 1;
        let value = args
            .get(i)
            .ok_or_else(|| error(format!("{flag} requires a value")))?;
        require(slot.is_none(), format!("duplicate option: {flag}"))?;
        *slot = Some(
            value
                .to_str()
                .ok_or_else(|| error("option requires Unicode text"))?
                .to_owned(),
        );
        i += 1;
    }
    require(
        action == "exec" || tail.is_empty(),
        "unexpected command tail",
    )?;
    let host = host.ok_or_else(|| error("--host required"))?;
    require(
        !host.is_empty()
            && !host.starts_with('-')
            && host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-:@[]".contains(&c)),
        "invalid SSH host",
    )?;
    let port = port
        .map(|p| {
            p.parse::<u16>()
                .ok()
                .filter(|n| *n != 0)
                .ok_or_else(|| error("invalid SSH port"))
        })
        .transpose()?;
    let control = ssh_control_path(&host, port)?;
    assert!(control.len() <= 80);
    require(!interrupted.load(Ordering::Relaxed), "interrupted")?;
    let mut ssh = Command::new(std::env::var_os("OMS_SSH_BIN").unwrap_or_else(|| "ssh".into()));
    ssh.args([
        "-o",
        "ControlMaster=auto",
        "-o",
        "ControlPersist=120",
        "-o",
        &format!("ControlPath={control}"),
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
    ]);
    if let Some(port) = port {
        ssh.args(["-p", &port.to_string()]);
    }
    ssh.args(["--", &host]);
    let mut value = serde_json::json!({"schema":1,"action":action,"host":host});
    let code;
    if action == "probe" {
        let out = ssh.arg("printf ok").output().map_err(error)?;
        code = exit_code(out.status);
        value["control_path"] = control.into();
        value["exit_code"] = code.into();
        value["reachable"] = (code == 0).into();
        value["stdout"] = String::from_utf8_lossy(&out.stdout).into_owned().into();
        value["stderr"] = String::from_utf8_lossy(&out.stderr).into_owned().into();
    } else {
        let repo = cwd.join(repo.ok_or_else(|| error("--repo required"))?);
        let remote = remote
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("--remote-dir required"))?;
        value["remote_dir"] = remote.clone().into();
        let head = git(&repo, &["rev-parse", "--verify", "HEAD"])?;
        let head = if head.status.success() {
            String::from_utf8_lossy(&head.stdout).trim().to_owned()
        } else {
            String::new()
        };
        value["head_sha"] = head.clone().into();
        if action == "sync-bundle" {
            require(!head.is_empty(), "repository has no HEAD")?;
            let range;
            let reference = match base.as_deref() {
                None => "HEAD",
                Some(base) => {
                    require(!base.starts_with('-'), "invalid bundle ref")?;
                    let resolved = git(
                        &repo,
                        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
                    )?;
                    require(resolved.status.success(), "cannot resolve --base commit")?;
                    let base_sha = String::from_utf8_lossy(&resolved.stdout).trim().to_owned();
                    value["base_sha"] = base_sha.clone().into();
                    if base_sha == head {
                        "HEAD"
                    } else {
                        range = format!("{base_sha}..HEAD");
                        &range
                    }
                }
            };
            let temp = TempDir::new()?;
            let bundle = temp.0.join("repo.bundle");
            let result = Command::new("git")
                .current_dir(&repo)
                .arg("bundle")
                .arg("create")
                .arg(&bundle)
                .arg(reference)
                .output()
                .map_err(error)?;
            require(
                result.status.success(),
                format!(
                    "git bundle failed: {}",
                    String::from_utf8_lossy(&result.stderr)
                ),
            )?;
            let file = std::fs::File::open(&bundle).map_err(error)?;
            value["bundle_bytes"] = file.metadata().map_err(error)?.len().into();
            let script = format!(
                "set -eu; mkdir -p -- {d}; cd -- {d}; t=$(mktemp .oms-bundle.XXXXXX); trap 'rm -f -- \"$t\"' EXIT HUP INT TERM; cat > \"$t\"; if [ ! -d .git ]; then git init; fi; test -z \"$(git status --porcelain)\"; git fetch \"$t\" HEAD; git checkout --no-overwrite-ignore --detach FETCH_HEAD; test \"$(git rev-parse HEAD)\" = {h}",
                d = remote_path(&remote),
                h = quote(&head)
            );
            // The bundle is outside the checkout so its presence cannot make it dirty.
            let script = script.replace(
                "mktemp .oms-bundle.XXXXXX",
                "mktemp \"${TMPDIR:-/tmp}/oms-bundle.XXXXXX\"",
            );
            let out = ssh
                .arg(format!("sh -lc {}", quote(&script)))
                .stdin(Stdio::from(file))
                .output()
                .map_err(error)?;
            code = exit_code(out.status);
            value["exit_code"] = code.into();
        } else {
            require(!tail.is_empty(), "command required after --")?;
            let command = tail
                .iter()
                .map(|v| {
                    v.to_str()
                        .map(quote)
                        .ok_or_else(|| error("command requires Unicode text"))
                })
                .collect::<Result<Vec<_>>>()?
                .join(" ");
            let script = format!("cd -- {} && {}", remote_path(&remote), command);
            let remote_command = format!("sh -lc {}", quote(&script));
            value["command_sha256"] = sha256(remote_command.as_bytes()).into();
            value["control_path"] = control.into();
            value["started_at"] = now()?.into();
            let out = ssh.arg(&remote_command).output().map_err(error)?;
            value["completed_at"] = now()?.into();
            code = exit_code(out.status);
            value["exit_code"] = code.into();
            value["stdout"] = String::from_utf8_lossy(&out.stdout).into_owned().into();
            value["stderr"] = String::from_utf8_lossy(&out.stderr).into_owned().into();
        }
    }
    let mut stdout = if pretty {
        serde_json::to_vec_pretty(&value)
    } else {
        serde_json::to_vec(&value)
    }
    .map_err(error)?;
    stdout.push(b'\n');
    Ok(Response {
        code,
        stdout,
        stderr: vec![],
    })
}
/// Shell word for a remote directory; a leading `~` expands on the remote host.
fn remote_path(s: &str) -> String {
    if s == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = s.strip_prefix("~/") {
        if rest.is_empty() {
            "\"$HOME\"".into()
        } else {
            format!("\"$HOME\"/{}", quote(rest))
        }
    } else {
        quote(s)
    }
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(128)
}
fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(error)
}
fn now() -> Result<String> {
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(error)?;
    Ok(
        chrono::DateTime::from_timestamp(t.as_secs() as i64, t.subsec_nanos())
            .ok_or_else(|| error("invalid time"))?
            .to_rfc3339(),
    )
}
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Result<Self> {
        for n in 0..1000 {
            let p = std::env::temp_dir().join(format!("oms-bundle-{}-{n}", std::process::id()));
            use std::os::unix::fs::DirBuilderExt;
            match std::fs::DirBuilder::new().mode(0o700).create(&p) {
                Ok(()) => return Ok(Self(p)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(error(e)),
            }
        }
        Err(error("cannot create temporary directory"))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let a = w[i - 15];
            let b = w[i - 2];
            w[i] = w[i - 16]
                .wrapping_add(a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3))
                .wrapping_add(w[i - 7])
                .wrapping_add(b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10));
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut z] = h;
        for i in 0..64 {
            let t1 = z
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            z = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, z]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn remote_path_expands_leading_tilde() {
        assert_eq!(remote_path("~"), "\"$HOME\"");
        assert_eq!(remote_path("~/foo/bar"), "\"$HOME\"/'foo/bar'");
        assert_eq!(remote_path("/a b"), "'/a b'");
        assert_eq!(remote_path("a~/b"), "'a~/b'");
    }

    fn invoke(args: &[&str], cwd: &Path) -> Response {
        run(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            cwd,
            &AtomicBool::new(false),
        )
    }
    #[test]
    fn digest_and_validation() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        for help in ["-h", "--help"] {
            assert_eq!(invoke(&[help], Path::new(".")).code, 0);
        }
        assert_ne!(
            invoke(&["probe", "--host", "-oProxyCommand=bad"], Path::new(".")).code,
            0
        );
        assert_ne!(
            invoke(&["probe", "--host", "host", "--port", "0"], Path::new(".")).code,
            0
        );
    }
    #[test]
    fn transport() {
        if std::env::var_os("OMS_TEST_SSH_CHILD").is_none() {
            let temp = TempDir::new().unwrap();
            let fake = temp.0.join("ssh");
            std::fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$OMS_TEST_ARGS\"\nfor last do :; done\ncase \"$last\" in *oms-bundle*) cat > \"$OMS_TEST_BUNDLE\"; exec sh -c \"$last\" < \"$OMS_TEST_BUNDLE\";; *) exec sh -c \"$last\";; esac\n").unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "ssh_adapter::tests::transport", "--nocapture"])
                .env("OMS_TEST_SSH_CHILD", "1")
                .env("OMS_SSH_BIN", &fake)
                .env("OMS_TEST_ARGS", temp.0.join("args"))
                .env("OMS_TEST_BUNDLE", temp.0.join("bundle"))
                .env("OMS_TEST_DIR", &temp.0)
                .env_remove("OMS_HARNESS_CHILD")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("OMS_TEST_DIR").unwrap());
        let repo = root.join("repo");
        let remote = root.join("remote space");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.name", "Test"][..],
            &["config", "user.email", "test@example.invalid"][..],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(repo.join("file"), "content").unwrap();
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "file"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-qm", "initial"])
                .status()
                .unwrap()
                .success()
        );
        let long_host = "a".repeat(300);
        let response = invoke(&["probe", "--host", &long_host, "--json"], &root);
        assert_eq!(response.code, 0);
        let v: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        assert_eq!(v["stdout"], "ok");
        let ctrl = v["control_path"].as_str().unwrap();
        assert!(ctrl.len() <= 80);
        let ctrl_dir = Path::new(ctrl).parent().unwrap();
        assert_eq!(
            std::fs::symlink_metadata(ctrl_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let args = std::fs::read_to_string(root.join("args")).unwrap();
        for option in [
            "ControlMaster=auto",
            "ControlPersist=120",
            "ControlPath=",
            "BatchMode=yes",
            "ConnectTimeout=10",
        ] {
            assert!(args.contains(option));
        }
        let repo = repo.to_str().unwrap();
        let remote_str = remote.to_str().unwrap();
        let response = invoke(
            &[
                "sync-bundle",
                "--repo",
                repo,
                "--host",
                "example",
                "--remote-dir",
                remote_str,
            ],
            &root,
        );
        assert_eq!(
            response.code,
            0,
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        assert_eq!(v["head_sha"].as_str().unwrap().len(), 40);
        assert!(v["bundle_bytes"].as_u64().unwrap() > 0);
        assert!(
            std::fs::read(root.join("bundle"))
                .unwrap()
                .starts_with(b"# v2 git bundle")
        );
        assert_eq!(
            std::fs::read_to_string(remote.join("file")).unwrap(),
            "content"
        );
        let first = String::from_utf8(git(Path::new(repo), &["rev-parse", "HEAD"]).unwrap().stdout)
            .unwrap();
        let first = first.trim();
        std::fs::write(Path::new(repo).join("file"), "second").unwrap();
        assert!(
            git(Path::new(repo), &["commit", "-qam", "second"])
                .unwrap()
                .status
                .success()
        );
        let response = invoke(
            &[
                "sync-bundle",
                "--repo",
                repo,
                "--host",
                "example",
                "--remote-dir",
                remote_str,
                "--base",
                "HEAD~1",
            ],
            &root,
        );
        assert_eq!(response.code, 0);
        let v: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        assert_eq!(v["base_sha"], first);
        assert_ne!(v["head_sha"], first);
        assert_eq!(
            std::fs::read_to_string(remote.join("file")).unwrap(),
            "second"
        );
        let response = invoke(
            &[
                "exec",
                "--repo",
                repo,
                "--host",
                "example",
                "--remote-dir",
                remote_str,
                "--",
                "sh",
                "-c",
                "printf hello; exit 7",
            ],
            &root,
        );
        assert_eq!(response.code, 7);
        let v: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["stdout"], "hello");
        assert_eq!(v["exit_code"], 7);
        let logged = std::fs::read_to_string(root.join("args")).unwrap();
        assert_eq!(
            v["command_sha256"],
            sha256(logged.lines().last().unwrap().as_bytes())
        );
        for key in ["started_at", "completed_at"] {
            assert!(chrono::DateTime::parse_from_rfc3339(v[key].as_str().unwrap()).is_ok());
        }
    }
}
