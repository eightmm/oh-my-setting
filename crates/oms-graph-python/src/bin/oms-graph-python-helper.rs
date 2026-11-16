//! Private helper executable of `oms-graph-python`. A libexec build artifact
//! started only by `Session::start` from an explicit packaged path; it is not
//! a command, takes no user input beyond its framed stdin, and has no CLI.

fn main() {
    std::process::exit(oms_graph_python::helper::run());
}
