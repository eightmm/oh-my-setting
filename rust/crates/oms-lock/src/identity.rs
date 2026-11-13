//! One fixed process query for hosts without procfs; never a transform chain.
use crate::{Cancellation, Error};
#[cfg(target_os = "linux")]
use std::fs::File;
use std::io::Read;
#[cfg(any(target_os = "macos", test))]
use std::os::unix::process::CommandExt;
#[cfg(any(target_os = "macos", test))]
use std::process::{Child, Command, Stdio};
#[cfg(any(target_os = "macos", test))]
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Copy)]
pub(crate) struct Observation<'a> {
    pub deadline: Instant,
    pub cancellation: Option<&'a Cancellation>,
}
impl Observation<'_> {
    pub fn check(self) -> Result<(), Error> {
        if self.cancellation.is_some_and(Cancellation::is_cancelled) {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(Error::Busy);
        }
        Ok(())
    }
}

pub(crate) fn start(pid: u64, observation: Observation<'_>) -> Result<Option<Vec<u8>>, Error> {
    if pid == 0 || pid > i32::MAX as u64 {
        return Err(Error::Unsafe);
    }
    observation.check()?;
    #[cfg(target_os = "linux")]
    {
        let file = match File::open(format!("/proc/{pid}/stat")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::Io),
        };
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Io)?;
        observation.check()?;
        if bytes.len() > 65536 {
            return Err(Error::Unsafe);
        }
        proc_start(&bytes).map(Some)
    }
    #[cfg(target_os = "macos")]
    {
        ps(pid, observation, std::path::Path::new("/bin/ps"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(Error::UnsupportedIdentity)
    }
}

#[cfg(any(target_os = "linux", test))]
fn proc_start(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let rest = bytes
        .windows(2)
        .rposition(|b| b == b") ")
        .map(|i| &bytes[i + 2..])
        .ok_or(Error::Io)?;
    let value = rest
        .split(|b| b.is_ascii_whitespace())
        .filter(|s| !s.is_empty())
        .nth(19)
        .ok_or(Error::Io)?;
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return Err(Error::Io);
    }
    let mut identity = b"proc:".to_vec();
    identity.extend_from_slice(value);
    Ok(identity)
}

#[cfg(any(target_os = "macos", test))]
struct QueryChild(Child);
#[cfg(any(target_os = "macos", test))]
impl Drop for QueryChild {
    fn drop(&mut self) {
        // kill + wait on every error/timeout also prevents unreaped children.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
#[cfg(any(target_os = "macos", test))]
fn ps(
    pid: u64,
    observation: Observation<'_>,
    executable: &std::path::Path,
) -> Result<Option<Vec<u8>>, Error> {
    if pid == 0 || pid > i32::MAX as u64 {
        return Err(Error::Unsafe);
    }
    observation.check()?;
    let mut child = QueryChild(
        Command::new(executable)
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|_| Error::Io)?,
    );
    let mut stdout = child.0.stdout.take().ok_or(Error::Io)?;
    let flags = rustix::fs::fcntl_getfl(&stdout).map_err(|_| Error::Io)?;
    rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|_| Error::Io)?;
    let deadline = observation
        .deadline
        .min(Instant::now() + Duration::from_secs(2));
    let mut bytes = Vec::new();
    let mut eof = false;
    loop {
        observation.check()?;
        if Instant::now() >= deadline {
            return Err(Error::Busy);
        }
        let mut buffer = [0u8; 257];
        match stdout.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(size) => {
                bytes.extend_from_slice(&buffer[..size]);
                if bytes.len() > 256 {
                    return Err(Error::Unsafe);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(Error::Io),
        }
        if let Some(status) = child.0.try_wait().map_err(|_| Error::Io)?
            && eof
        {
            if !status.success() {
                return Err(Error::Io);
            }
            return normalize_ps(&bytes);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[cfg(any(target_os = "macos", test))]
fn normalize_ps(bytes: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    let line: Vec<u8> = bytes
        .split(|b| *b == b'\n')
        .next()
        .unwrap_or_default()
        .iter()
        .copied()
        .filter(|b| *b != b'\r')
        .collect();
    if line
        .iter()
        .any(|b| !b.is_ascii() || (b.is_ascii_control() && *b != b'\t'))
    {
        return Err(Error::Unsafe);
    }
    let fields: Vec<_> = line
        .split(|b| matches!(b, b' ' | b'\t'))
        .filter(|s| !s.is_empty())
        .collect();
    if fields.is_empty() {
        return Ok(None);
    }
    let number = |bytes: &[u8]| -> Option<u32> {
        if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(bytes).ok()?.parse().ok()
    };
    let days: &[&[u8]] = &[b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
    let months: &[&[u8]] = &[
        b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov",
        b"Dec",
    ];
    if fields.len() != 5
        || !days.contains(&fields[0])
        || !months.contains(&fields[1])
        || !number(fields[2]).is_some_and(|day| (1..=31).contains(&day))
        || fields[3].len() != 8
        || fields[4].len() != 4
        || number(fields[4]).is_none()
    {
        return Err(Error::Unsafe);
    }
    let time: Vec<_> = fields[3].split(|b| *b == b':').collect();
    if time.len() != 3
        || !number(time[0]).is_some_and(|v| v < 24)
        || !number(time[1]).is_some_and(|v| v < 60)
        || !number(time[2]).is_some_and(|v| v <= 60)
    {
        return Err(Error::Unsafe);
    }
    let mut result = b"ps:".to_vec();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            result.push(b' ');
        }
        result.extend_from_slice(field);
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proc_parentheses_and_ps_byte_normalization() {
        let mut stat = b"123 (space ) name) ".to_vec();
        stat.extend_from_slice(b"S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 21\n");
        assert_eq!(proc_start(&stat).unwrap(), b"proc:12345");
        assert_eq!(proc_start(b"malformed"), Err(Error::Io));
        assert_eq!(
            normalize_ps(b"  Fri Nov  13\t04:51:37 2026\r\nignored\n")
                .unwrap()
                .unwrap(),
            b"ps:Fri Nov 13 04:51:37 2026"
        );
        assert_eq!(normalize_ps(b" \t\r\n").unwrap(), None);
        assert_eq!(normalize_ps(b"\0"), Err(Error::Unsafe));
        assert_eq!(normalize_ps(b"\xff"), Err(Error::Unsafe));
        assert_eq!(normalize_ps(b"garbage"), Err(Error::Unsafe));
        assert_eq!(
            normalize_ps(b"Fri Nov 13 25:00:00 2026"),
            Err(Error::Unsafe)
        );
    }
    #[test]
    fn compiled_query_bounds_deadline_and_reaping() {
        let executable = std::env::current_exe().unwrap();
        let fixture = std::fs::read_dir(executable.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ps_fixture-")
                    && path.extension().is_none()
            })
            .unwrap();
        for scenario in 1..=8 {
            let observation = Observation {
                deadline: Instant::now() + Duration::from_millis(250),
                cancellation: None,
            };
            let result = ps(scenario, observation, &fixture);
            match scenario {
                1 => assert_eq!(result.unwrap().unwrap(), b"ps:Fri Nov 13 04:51:37 2026"),
                2 | 3 => assert_eq!(result, Ok(None)),
                4 | 5 | 8 => assert_eq!(result, Err(Error::Unsafe)),
                6 => assert_eq!(result, Err(Error::Io)),
                7 => assert_eq!(result, Err(Error::Busy)),
                _ => unreachable!(),
            }
            let path = std::env::temp_dir()
                .join(format!("oms-ps-query-{}-{scenario}", std::process::id()));
            let pid: i32 = std::fs::read_to_string(&path).unwrap().parse().unwrap();
            std::fs::remove_file(path).unwrap();
            assert!(matches!(
                rustix::process::waitpid(
                    rustix::process::Pid::from_raw(pid),
                    rustix::process::WaitOptions::NOHANG
                ),
                Err(rustix::io::Errno::CHILD)
            ));
        }
        let cancellation = Cancellation::default();
        std::thread::scope(|scope| {
            let observation = Observation {
                deadline: Instant::now() + Duration::from_secs(2),
                cancellation: Some(&cancellation),
            };
            let fixture = &fixture;
            let waiter = scope.spawn(move || ps(7, observation, fixture));
            let path = std::env::temp_dir().join(format!("oms-ps-query-{}-7", std::process::id()));
            let deadline = Instant::now()
                + Duration::from_secs(crate::legacy_seconds(
                    std::env::var_os("OMS_TEST_CALL_TIMEOUT").as_deref(),
                    30,
                ));
            while !path.exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            cancellation.cancel();
            assert_eq!(waiter.join().unwrap(), Err(Error::Cancelled));
            let pid: i32 = std::fs::read_to_string(&path).unwrap().parse().unwrap();
            std::fs::remove_file(path).unwrap();
            assert!(matches!(
                rustix::process::waitpid(
                    rustix::process::Pid::from_raw(pid),
                    rustix::process::WaitOptions::NOHANG
                ),
                Err(rustix::io::Errno::CHILD)
            ));
        });
    }
    #[test]
    fn real_fixed_ps_query_has_legacy_shape() {
        let observation = Observation {
            deadline: Instant::now() + Duration::from_secs(2),
            cancellation: None,
        };
        let result = ps(
            std::process::id() as u64,
            observation,
            std::path::Path::new("/bin/ps"),
        )
        .unwrap()
        .unwrap();
        assert!(result.starts_with(b"ps:"));
        assert_eq!(result.split(|b| *b == b' ').count(), 5);
        assert_eq!(
            ps(0, observation, std::path::Path::new("/bin/ps")),
            Err(Error::Unsafe)
        );
        assert_eq!(
            ps(
                i32::MAX as u64 + 1,
                observation,
                std::path::Path::new("/bin/ps")
            ),
            Err(Error::Unsafe)
        );
    }
}
