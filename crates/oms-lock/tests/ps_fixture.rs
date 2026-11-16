//! Compiled fixed-query oracle; no runtime shell or Python.
use std::io::Write;
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return;
    }
    assert_eq!(args.len(), 4);
    assert_eq!(args[0], "-p");
    assert_eq!(args[2], "-o");
    assert_eq!(args[3], "lstart=");
    assert_eq!(std::env::var("LC_ALL").unwrap(), "C");
    let scenario: u32 = args[1].parse().unwrap();
    let parent = rustix::process::getppid().unwrap().as_raw_nonzero();
    let path = std::env::temp_dir().join(format!("oms-ps-query-{parent}-{scenario}"));
    std::fs::write(path, std::process::id().to_string()).unwrap();
    match scenario {
        1 => print!(" Fri Nov  13 04:51:37 2026\r\n"),
        2 => {}
        3 => print!("{}", " ".repeat(256)),
        4 => print!("{}", "x".repeat(257)),
        5 => std::io::stdout().write_all(&[255]).unwrap(),
        6 => std::process::exit(7),
        7 => {
            std::io::stdout().flush().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        8 => print!("malformed ASCII"),
        _ => panic!("unknown fixture"),
    }
}
