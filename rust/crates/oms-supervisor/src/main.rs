//! Standalone native entry point; the parent router may use the library adapter.
#![forbid(unsafe_code)]
use std::ffi::OsString;
use std::sync::atomic::AtomicBool;
fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let mut host = oms_supervisor::CommandHost::new(vec![executable.into_os_string()]);
    let code = oms_supervisor::run_streaming(
        &args,
        &cwd,
        &AtomicBool::new(false),
        &mut host,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    while host.requires_foreground_cleanup() {
        if let Err(error) = host.drive() {
            eprintln!("error: foreground cleanup: {error}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // dispatch hands durable, identity-recorded runner processes to the OS
    // reaper. A failed _run-one leaves cleanup_failed identity for reconciliation.
    // Library callers retain CommandHost and drive pending cleanup themselves.
    std::process::exit(code);
}
