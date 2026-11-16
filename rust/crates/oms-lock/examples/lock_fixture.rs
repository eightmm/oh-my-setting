//! Development-only bridge for synthetic Bash/native interop proof.
use oms_lock::{Lock, Mode, Settings};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
fn main() {
    rustix::process::setpgid(None, None).unwrap();
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(error.exit_code());
    }
}
fn run() -> Result<(), oms_lock::Error> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err(oms_lock::Error::InvalidTarget);
    }
    let root = PathBuf::from(&args[0]);
    if !root.starts_with(std::env::temp_dir()) || !root.join("synthetic-fixture").is_file() {
        return Err(oms_lock::Error::Unsafe);
    }
    let mode = match args[1].as_bytes() {
        b"flock" => Mode::Flock,
        b"mkdir" => Mode::Mkdir,
        _ => return Err(oms_lock::Error::InvalidSettings),
    };
    let settings = Settings {
        mode,
        timeout: Duration::from_secs(30),
        poll_max: Duration::from_secs(1),
        lock_dir: Some(root.join("locks")),
        home: root.join("home"),
        temp_dir: root.clone(),
        cwd: root.clone(),
        search_path: std::env::var_os("PATH").unwrap_or_default(),
        force_mkdir: false,
    };
    let lock = Lock::for_file(Path::new(&args[2]), settings)?;
    match args[3].as_bytes() {
        b"path" => {
            std::io::stdout()
                .write_all(lock.path().as_os_str().as_bytes())
                .unwrap();
            println!();
        }
        b"try" => {
            lock.try_acquire()?.release()?;
        }
        b"hold" => {
            let guard = lock.acquire(None)?;
            println!("ready");
            std::io::stdout().flush().unwrap();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).unwrap();
            guard.release()?;
        }
        b"increment" => {
            for _ in 0..20 {
                let guard = lock.acquire(None)?;
                if std::fs::create_dir(root.join("critical")).is_err() {
                    return Err(oms_lock::Error::Unsafe);
                }
                let state = Path::new(&args[2]);
                let count = std::fs::read_to_string(state)
                    .unwrap_or_else(|_| "0".into())
                    .trim()
                    .parse::<u32>()
                    .unwrap();
                std::thread::yield_now();
                std::fs::write(state, (count + 1).to_string()).unwrap();
                std::fs::remove_dir(root.join("critical")).unwrap();
                guard.release()?;
            }
        }
        _ => return Err(oms_lock::Error::InvalidTarget),
    }
    Ok(())
}
