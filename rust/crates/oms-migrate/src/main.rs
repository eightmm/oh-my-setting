use oms_migrate::{BASELINE, inventory, parse_baseline, regressions, render, repository_root};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "OMS migration preparation (development only)\n\
Usage: oms-migrate inventory [--repo PATH]\n\
       oms-migrate check [--repo PATH] [--baseline PATH] [--require-zero]\n\n\
Inventory reads tracked working-tree files and emits sorted TSV.\n\
Check rejects new/increased legacy Python footprints; reductions are allowed.\n\
The default baseline is migration/python-inventory.tsv, relative to the repo.\n\
--require-zero also rejects remaining rows. No mode rewrites the baseline.\n\
This lexical inventory is not proof that Rust or dynamic commands avoid Python.\n";

fn run() -> Result<bool, String> {
    let args: Vec<_> = env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().map_err(|_| "arguments must be UTF-8"))
        .collect::<Result<_, _>>()?;
    let command = args.first().map(String::as_str).unwrap_or("--help");
    if matches!(command, "--help" | "-h") {
        print!("{USAGE}");
        return Ok(true);
    }
    if command == "--version" {
        println!("oms-migrate {}", env!("CARGO_PKG_VERSION"));
        return Ok(true);
    }
    if !matches!(command, "inventory" | "check") {
        return Err(format!("unknown command: {command:?}\n{USAGE}"));
    }
    let mut repo = PathBuf::from(".");
    let mut baseline = PathBuf::from(BASELINE);
    let mut require_zero = false;
    let mut remaining = args[1..].iter();
    while let Some(option) = remaining.next() {
        match option.as_str() {
            "--repo" => {
                repo = remaining.next().ok_or("--repo requires a path")?.into();
            }
            "--baseline" if command == "check" => {
                baseline = remaining.next().ok_or("--baseline requires a path")?.into();
            }
            "--require-zero" if command == "check" => require_zero = true,
            _ => return Err(format!("unknown option for {command}: {option:?}")),
        }
    }
    let repo = repository_root(&repo)?;
    let current = inventory(&repo)?;
    if command == "inventory" {
        io::stdout()
            .lock()
            .write_all(render(&current).as_bytes())
            .map_err(|error| format!("cannot write inventory: {error}"))?;
        return Ok(true);
    }
    let baseline_path = repo.join(baseline);
    let text = fs::read_to_string(&baseline_path)
        .map_err(|error| format!("cannot read inventory baseline: {error}"))?;
    let expected = parse_baseline(&text)?;
    let failures = regressions(&expected, &current);
    for failure in &failures {
        eprintln!("{failure}");
    }
    if require_zero && !current.is_empty() {
        eprintln!("zero-footprint check failed: {} rows remain", current.len());
    }
    let passed = failures.is_empty() && (!require_zero || current.is_empty());
    println!(
        "legacy Python inventory: {} ({} current paths, {} baseline paths)",
        if passed { "PASS" } else { "FAIL" },
        current.len(),
        expected.len()
    );
    Ok(passed)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}
