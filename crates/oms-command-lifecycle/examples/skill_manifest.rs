//! Run the bundled skill check against the current developer checkout.

use std::io::{self, Write};
use std::sync::atomic::AtomicBool;

fn main() -> io::Result<()> {
    let root = std::env::current_dir()?;
    let (code, stdout, stderr) = oms_command_lifecycle::run_with_source(
        "install-skills",
        &[],
        &root,
        &root,
        &AtomicBool::new(false),
    );
    io::stdout().write_all(&stdout)?;
    io::stderr().write_all(&stderr)?;
    std::process::exit(code);
}
