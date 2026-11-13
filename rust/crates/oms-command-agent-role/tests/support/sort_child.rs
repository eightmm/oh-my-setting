use std::{
    env,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};

fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    assert_eq!(args, ["-u"]);
    let root = PathBuf::from(env::var_os("OMS_ROLE_SORT_FIXTURE_ROOT").unwrap());
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("sort.calls"))
        .unwrap()
        .write_all(b"-u\n")
        .unwrap();
    fs::write(root.join("sort.pid"), std::process::id().to_string()).unwrap();
    match env::var("OMS_ROLE_SORT_FIXTURE_MODE").unwrap().as_str() {
        "fail" => {
            eprintln!("fixture sort failure");
            std::process::exit(7);
        }
        "hang" => loop {
            std::thread::park();
        },
        "stdout-over" => {
            std::io::stdout()
                .write_all(&vec![b'x'; 1024 * 1024 + 1])
                .unwrap();
        }
        "stderr-over" => {
            std::io::stderr()
                .write_all(&vec![b'e'; 64 * 1024 + 1])
                .unwrap();
        }
        "malformed" => {
            std::io::stdout().write_all(b"unterminated").unwrap();
        }
        "nul" => {
            std::io::stdout().write_all(b"bad\0name\n").unwrap();
        }
        "empty" => {}
        "closed" => {
            std::io::stdout().write_all(b"closed\n").unwrap();
        }
        mode => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            let mut names: Vec<_> = input.split_inclusive(|b| *b == b'\n').collect();
            names.sort();
            names.dedup();
            for name in names {
                std::io::stdout().write_all(name).unwrap();
            }
            if mode == "stderr-edge" {
                std::io::stderr().write_all(&vec![b'e'; 64 * 1024]).unwrap();
            } else {
                std::io::stderr().write_all(b"fixture warning\n").unwrap();
            }
        }
    }
}
