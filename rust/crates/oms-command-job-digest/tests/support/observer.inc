use std::{
    env, fs,
    io::{Read, Write},
    path::Path,
    process::{self, Command, Stdio},
    thread,
    time::Duration,
};
fn main() {
    let mode = env::var("OMS_DIGEST_MATRIX_MODE").unwrap();
    let root = env::var_os("OMS_DIGEST_MATRIX_ROOT").unwrap();
    let root = Path::new(&root);
    if env::args().nth(1).as_deref() == Some("descendant") {
        fs::write(root.join("descendant"), process::id().to_string()).unwrap();
        loop {
            thread::park_timeout(Duration::from_secs(60));
        }
    }
    let command = env::args_os().next().unwrap();
    let name = Path::new(&command).file_name().unwrap().to_str().unwrap();
    if mode == "stdin-null" {
        let count = std::io::stdin().read(&mut [0; 1]).unwrap();
        fs::write(root.join(format!("stdin-{name}")), count.to_string()).unwrap();
        assert_eq!(count, 0, "{name} inherited caller input");
        if name == "git" {
            return;
        }
    }
    if ["scancel", "sbatch"].contains(&name) {
        fs::write(root.join("forbidden"), b"called").unwrap();
        process::exit(99);
    }
    let queue = name == "squeue";
    if queue {
        fs::write(root.join("queue-called"), b"called").unwrap();
        if mode == "remove-log" {
            fs::remove_file(root.join("run.log")).unwrap();
        }
        if mode == "remove-metrics" {
            fs::remove_file(root.join("metrics")).unwrap();
        }
    }
    if (queue && ["queue-hang", "pipe-hang", "interrupt-queue"].contains(&mode.as_str()))
        || (!queue && ["account-hang", "interrupt-account"].contains(&mode.as_str()))
    {
        let _child = Command::new(env::current_exe().unwrap())
            .arg("descendant")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        if mode == "pipe-hang" {
            while !root.join("descendant").exists() {
                thread::yield_now();
            }
            return;
        }
        loop {
            thread::park_timeout(Duration::from_secs(60));
        }
    }
    if queue {
        match mode.as_str() {
            "invalid-id" => {
                eprintln!("Invalid job id specified");
                process::exit(1);
            }
            "unknown-job" => {
                eprintln!("Unknown job");
                process::exit(1);
            }
            "outage" => {
                eprintln!("controller unavailable");
                process::exit(1);
            }
            "whitespace" => println!(" "),
            "interrupt-sleep" => println!("123 queued"),
            "retry" => {
                let count: u32 = fs::read_to_string(root.join("calls"))
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(0);
                fs::write(root.join("calls"), (count + 1).to_string()).unwrap();
                match count {
                    0 => println!("123 queued"),
                    1 | 2 => {
                        eprintln!("controller unavailable");
                        process::exit(1);
                    }
                    _ => {
                        eprintln!("Invalid job id specified");
                        process::exit(1);
                    }
                }
            }
            _ => {}
        }
    } else {
        match mode.as_str() {
            "account-empty" => println!("header\nrule"),
            "account-fail" => {
                eprintln!("accounting unavailable");
                process::exit(1);
            }
            "account-raw" => {
                std::io::stdout()
                    .write_all(b"header\nrule\nCOMPLETED\xff\n")
                    .unwrap();
            }
            "account-equal" => {
                println!("header\nrule");
                for i in 0..18 {
                    println!("COMPLETED row{i}");
                }
            }
            "account-many" => {
                println!("header\nrule");
                for i in 0..100_000 {
                    println!("COMPLETED row{i}");
                }
            }
            "account-mixed" => {
                println!("header");
                std::io::stdout().flush().unwrap();
                eprintln!("rule");
                std::io::stderr().flush().unwrap();
                println!("COMPLETED");
                std::io::stdout().flush().unwrap();
                eprintln!("warning");
            }
            _ => println!("header\nrule\nCOMPLETED"),
        }
    }
}
