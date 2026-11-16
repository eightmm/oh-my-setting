#![forbid(unsafe_code)]
fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 1 {
        eprintln!("usage: oms-state-queue-worker REQUEST");
        std::process::exit(2);
    }
    std::process::exit(oms_command_state::run_queue_worker(std::path::Path::new(
        &args[0],
    )));
}
