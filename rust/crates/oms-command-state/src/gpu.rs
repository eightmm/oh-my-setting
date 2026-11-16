//! Bounded local GPU inventory and availability gates.
use crate::json::{self, number, object, string};
use crate::{
    Context, Options, Output, Result, Value, checkpoint, error, json_output, require, success,
};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn inventory(bin: &OsStr, ctx: &Context<'_>, limit: Option<Instant>) -> Result<Value> {
    let mut child = match Command::new(bin)
        .args([
            "--query-gpu=index,name,memory.total,memory.used,memory.free,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(object([
                ("available", Value::Bool(false)),
                ("gpus", Value::Array(vec![])),
            ]));
        }
        Err(e) => return Err(error(1, format!("GPU inventory launch failed: {e}"))),
    };
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| error(1, "GPU inventory stdout unavailable"))?;
        let flags = fcntl_getfl(&stdout).map_err(|e| error(1, e))?;
        fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).map_err(|e| error(1, e))?;
        let deadline = limit.map_or(Instant::now() + Duration::from_secs(5), |limit| {
            limit.min(Instant::now() + Duration::from_secs(5))
        });
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        let mut exited = None;
        let mut eof = false;
        loop {
            checkpoint(ctx.interrupted)?;
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => {
                        bytes.extend_from_slice(&buf[..n]);
                        require(bytes.len() <= 1024 * 1024, "GPU inventory exceeds limit")?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(error(1, e)),
                }
            }
            if exited.is_none() {
                exited = child.try_wait().map_err(|e| error(1, e))?;
            }
            if eof && let Some(status) = exited {
                require(status.success(), "GPU inventory command failed")?;
                break;
            }
            require(Instant::now() < deadline, "GPU inventory command timed out")?;
            std::thread::sleep(Duration::from_millis(10));
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| error(1, "invalid GPU inventory encoding"))?;
        let mut gpus = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let fields: Vec<_> = line.split(',').map(str::trim).collect();
            require(fields.len() == 6, "invalid GPU inventory row")?;
            let parse = |i: usize| -> Result<usize> {
                fields[i]
                    .parse()
                    .map_err(|_| error(1, "invalid GPU inventory number"))
            };
            gpus.push(object([
                ("index", number(parse(0)?)),
                ("name", string(fields[1])),
                ("memory_total_mib", number(parse(2)?)),
                ("memory_used_mib", number(parse(3)?)),
                ("memory_free_mib", number(parse(4)?)),
                ("utilization_gpu_percent", number(parse(5)?)),
            ]));
        }
        Ok(object([
            ("available", Value::Bool(true)),
            ("gpus", Value::Array(gpus)),
        ]))
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

pub(crate) fn run(action: &str, args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let bin = std::env::var_os("OMS_NVIDIA_SMI_BIN").unwrap_or_else(|| "nvidia-smi".into());
    run_with_bin(action, args, ctx, &bin)
}

fn run_with_bin(action: &str, args: &[OsString], ctx: &Context<'_>, bin: &OsStr) -> Result<Output> {
    let opts = Options::parse(
        args,
        &["min-free-mib", "gpu", "timeout-secs", "poll-ms"],
        &["json"],
    )?;
    require(opts.pos.is_empty(), "unexpected positional argument")?;
    if action == "gpu-status" {
        require(opts.values.is_empty(), "gpu-status takes no value options")?;
    } else {
        require(
            !opts.get("min-free-mib").is_empty(),
            "--min-free-mib is required",
        )?;
    }
    let minimum = opts.number("min-free-mib", 0)?;
    let gpu = if opts.get("gpu").is_empty() {
        None
    } else {
        Some(opts.number("gpu", 0)?)
    };
    let timeout = opts.number("timeout-secs", 300)?;
    let poll = opts.number("poll-ms", 1000)?;
    require(
        poll > 0 && poll <= 60_000,
        "--poll-ms must be between 1 and 60000",
    )?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout as u64))
        .ok_or_else(|| error(2, "timeout out of range"))?;
    loop {
        checkpoint(ctx.interrupted)?;
        let report = inventory(
            bin,
            ctx,
            (action == "gpu-wait" && timeout > 0).then_some(deadline),
        )?;
        let matched = report.get("gpus").array().unwrap_or(&[]).iter().any(|row| {
            gpu.is_none_or(|idx| row.get("index").int() == Ok(idx as i128))
                && row.get("memory_free_mib").int().unwrap_or(-1) >= minimum as i128
        });
        if action == "gpu-status" || matched {
            return if opts.flag("json") {
                json_output(0, &report)
            } else {
                Ok(success(format!(
                    "{}\n",
                    String::from_utf8_lossy(&json::pretty(&report)?)
                )))
            };
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(error(1, "GPU wait timed out"));
        }
        let sleep = Duration::from_millis(poll as u64).min(deadline - now);
        let until = now + sleep;
        while Instant::now() < until {
            checkpoint(ctx.interrupted)?;
            std::thread::sleep(
                Duration::from_millis(50).min(until.saturating_duration_since(Instant::now())),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(script: &str) -> Self {
            let root = std::env::temp_dir().join(format!("oms-gpu-test-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let bin = root.join("nvidia-smi");
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn inventory_is_bounded_and_parses_fixture() {
        let fixture = Fixture::new("#!/bin/sh\nprintf '0, Test GPU, 8192, 1024, 7168, 12\\n'\n");
        let interrupted = AtomicBool::new(false);
        let ctx = Context {
            runtime: None,
            cwd: fixture.0.clone(),
            repo: fixture.0.clone(),
            state_repo: fixture.0.clone(),
            interrupted: &interrupted,
        };
        let report = inventory(fixture.0.join("nvidia-smi").as_os_str(), &ctx, None).unwrap();
        assert!(report.get("available").truth());
        let rows = report.get("gpus").array().unwrap();
        assert_eq!(rows[0].get("memory_free_mib").int().unwrap(), 7168);
        let absent = inventory(fixture.0.join("absent").as_os_str(), &ctx, None).unwrap();
        assert!(!absent.get("available").truth());
        let bin = fixture.0.join("nvidia-smi");
        let ok = run_with_bin(
            "gpu-wait",
            &[
                "--min-free-mib".into(),
                "7000".into(),
                "--gpu".into(),
                "0".into(),
                "--json".into(),
            ],
            &ctx,
            bin.as_os_str(),
        )
        .unwrap();
        assert_eq!(ok.0, 0);
        let timeout = run_with_bin(
            "gpu-wait",
            &[
                "--min-free-mib".into(),
                "9000".into(),
                "--timeout-secs".into(),
                "0".into(),
            ],
            &ctx,
            bin.as_os_str(),
        )
        .unwrap_err();
        assert_eq!(timeout.code, 1);
        interrupted.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            inventory(fixture.0.join("nvidia-smi").as_os_str(), &ctx, None)
                .unwrap_err()
                .code,
            130
        );
    }
}
