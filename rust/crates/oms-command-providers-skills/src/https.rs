//! Fixed-origin HTTPS transport. Subscription headers never enter argv or child environment.
use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};

pub struct HttpsResponse {
    pub status: u16,
    pub stream: Box<dyn BufRead + Send>,
}
/// Implementations must send exclusively to the Codex subscription origin and
/// must not expose request headers through diagnostics, process arguments or environment.
pub trait HttpsTransport: Send + Sync {
    fn open(&self, headers: &[(String, String)], body: &[u8]) -> io::Result<HttpsResponse>;
}
pub struct CurlTransport;
fn failure() -> io::Error {
    io::Error::other("Codex HTTPS transport failed")
}
pub(crate) fn random_hex(bytes: usize) -> io::Result<String> {
    let mut raw = vec![0u8; bytes];
    File::open("/dev/urandom")?.read_exact(&mut raw)?;
    let result = raw.iter().map(|b| format!("{b:02x}")).collect();
    raw.fill(0);
    Ok(result)
}
struct RequestDirectory {
    path: PathBuf,
    fd: OwnedFd,
    identity: (u64, u64),
    uid: u32,
    file: Option<(u64, u64)>,
}
#[allow(clippy::unnecessary_cast)]
impl RequestDirectory {
    fn create() -> io::Result<Self> {
        for _ in 0..64 {
            let path = std::env::temp_dir().join(format!("oms-codex-curl-{}", random_hex(24)?));
            match rfs::mkdir(&path, Mode::from_raw_mode(0o700)) {
                Ok(()) => (),
                Err(rustix::io::Errno::EXIST) => continue,
                Err(_) => return Err(failure()),
            }
            let fd = rfs::open(
                &path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| failure())?;
            let a = rfs::fstat(&fd).map_err(|_| failure())?;
            let b = rfs::lstat(&path).map_err(|_| failure())?;
            let uid = rustix::process::geteuid().as_raw();
            if FileType::from_raw_mode(a.st_mode) != FileType::Directory
                || a.st_uid != uid
                || a.st_mode & 0o777 != 0o700
                || a.st_dev != b.st_dev
                || a.st_ino != b.st_ino
            {
                return Err(failure());
            }
            return Ok(Self {
                path,
                fd,
                identity: (a.st_dev as u64, a.st_ino as u64),
                uid,
                file: None,
            });
        }
        Err(failure())
    }
    fn write(&mut self, body: &mut [u8]) -> io::Result<()> {
        let result = (|| {
            let fd = rfs::openat(
                &self.fd,
                "cfg",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| failure())?;
            let info = rfs::fstat(&fd).map_err(|_| failure())?;
            self.file = Some((info.st_dev as u64, info.st_ino as u64));
            if FileType::from_raw_mode(info.st_mode) != FileType::RegularFile
                || info.st_uid != self.uid
                || info.st_mode & 0o777 != 0o600
                || info.st_nlink != 1
            {
                return Err(failure());
            }
            let mut file = File::from(fd);
            file.write_all(body)?;
            file.flush()?;
            let named =
                rfs::statat(&self.fd, "cfg", AtFlags::SYMLINK_NOFOLLOW).map_err(|_| failure())?;
            if named.st_dev != info.st_dev || named.st_ino != info.st_ino || named.st_nlink != 1 {
                return Err(failure());
            }
            Ok(())
        })();
        body.fill(0);
        result
    }
}
impl Drop for RequestDirectory {
    #[allow(clippy::unnecessary_cast)]
    fn drop(&mut self) {
        let Ok(info) = rfs::fstat(&self.fd) else {
            return;
        };
        if info.st_uid != self.uid || (info.st_dev as u64, info.st_ino as u64) != self.identity {
            return;
        }
        if let (Some(expected), Ok(info)) = (
            self.file,
            rfs::statat(&self.fd, "cfg", AtFlags::SYMLINK_NOFOLLOW),
        ) && info.st_uid == self.uid
            && info.st_nlink == 1
            && FileType::from_raw_mode(info.st_mode) == FileType::RegularFile
            && (info.st_dev as u64, info.st_ino as u64) == expected
        {
            let _ = rfs::unlinkat(&self.fd, "cfg", AtFlags::empty());
        }
        if rfs::lstat(&self.path)
            .is_ok_and(|info| (info.st_dev as u64, info.st_ino as u64) == self.identity)
        {
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}
struct CurlStream {
    child: Child,
    reader: BufReader<ChildStdout>,
    _directory: RequestDirectory,
}
impl Read for CurlStream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.reader.read(b)
    }
}
impl BufRead for CurlStream {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.reader.fill_buf()
    }
    fn consume(&mut self, n: usize) {
        self.reader.consume(n);
    }
}
impl Drop for CurlStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn quoted(value: &str) -> io::Result<String> {
    if value.chars().any(|c| c.is_control()) {
        return Err(failure());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
impl HttpsTransport for CurlTransport {
    fn open(&self, headers: &[(String, String)], body: &[u8]) -> io::Result<HttpsResponse> {
        if body.is_empty() || body.len() > 16 * 1024 * 1024 {
            return Err(failure());
        }
        let mut directory = RequestDirectory::create()?;
        let mut config = Vec::new();
        config.extend_from_slice(b"url = \"https://chatgpt.com/backend-api/codex/responses\"\n");
        for (name, value) in headers {
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                config.fill(0);
                return Err(failure());
            }
            match quoted(&format!("{name}: {value}")) {
                Ok(line) => {
                    config.extend_from_slice(b"header = ");
                    config.extend_from_slice(line.as_bytes());
                    config.push(b'\n');
                    let mut bytes = line.into_bytes();
                    bytes.fill(0);
                }
                Err(e) => {
                    config.fill(0);
                    return Err(e);
                }
            }
        }
        directory.write(&mut config)?;
        let mut command = Command::new("/usr/bin/curl");
        command
            .args([
                "-q",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--max-time",
                "90",
                "--connect-timeout",
                "20",
                "-sS",
                "-N",
                "--include",
                "--request",
                "POST",
                "--data-binary",
                "@-",
                "-K",
            ])
            .arg(directory.path.join("cfg"))
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| failure())?;
        let mut input = child.stdin.take().ok_or_else(failure)?;
        if input.write_all(body).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure());
        }
        drop(input);
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failure());
            }
        };
        let mut stream = CurlStream {
            child,
            reader: BufReader::new(stdout),
            _directory: directory,
        };
        let status = loop {
            let mut line = String::new();
            let mut total = 0usize;
            if stream.by_ref().take(8193).read_line(&mut line)? == 0 || line.len() > 8192 {
                return Err(failure());
            }
            total += line.len();
            if !line.starts_with("HTTP/") {
                return Err(failure());
            }
            let status = line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<u16>().ok())
                .filter(|n| (100..600).contains(n))
                .ok_or_else(failure)?;
            loop {
                line.clear();
                let count = stream.by_ref().take(8193).read_line(&mut line)?;
                total += count;
                if count == 0 || total > 65536 {
                    return Err(failure());
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }
            if !(100..200).contains(&status) {
                break status;
            }
        };
        Ok(HttpsResponse {
            status,
            stream: Box::new(stream),
        })
    }
}
