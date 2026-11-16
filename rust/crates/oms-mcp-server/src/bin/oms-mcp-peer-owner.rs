//! Private libexec helper; never install on PATH.
fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if let Err(e) = oms_mcp_server::peer_owner(&arguments) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
