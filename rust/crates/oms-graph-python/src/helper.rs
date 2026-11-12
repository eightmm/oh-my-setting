//! The private helper: the only place untrusted Python source is parsed.
//!
//! It serves framed requests from stdin to stdout until stdin closes (see
//! `protocol`). It spawns nothing, reads no environment, writes nothing but
//! frames, and is meant to be started only by [`crate::Session`] from a packaged
//! `libexec` path. A crash (stack overflow, allocation failure, abort) is
//! contained to this process; the session reports it as a typed error.

use std::io::{self, Write};
use std::thread;

use crate::protocol::{
    KIND_REFUSAL, KIND_REQUEST, KIND_RESULT, MAX_REQUEST_PAYLOAD, MAX_RESPONSE_BYTES,
    MAX_SOURCE_BYTES, REFUSE_RESPONSE, REFUSE_SOURCE, decode_request, read_frame, write_frame,
};

/// Parser stack: deep but valid input recurses in Ruff's parser and AST drop.
/// Measured earlier at about 12 MiB for 10,000 nested levels; deeper input may
/// still overflow it, which only costs this process.
const PARSE_STACK_BYTES: usize = 64 * 1024 * 1024;

const EXIT_PROTOCOL: i32 = 3;
const EXIT_IO: i32 = 4;
const EXIT_USAGE: i32 = 2;
const EXIT_HARDEN: i32 = 5;

/// Entry point of the helper binary; returns its exit code.
#[doc(hidden)]
pub fn run() -> i32 {
    let mut args = std::env::args().skip(1);
    let mut memory_mib = None;
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next().map(|value| value.parse::<u64>())) {
            ("--memory-mib", Some(Ok(value))) => memory_mib = Some(value),
            _ => return EXIT_USAGE,
        }
    }
    // Before any request byte is read: a crash must not hand source bytes to a
    // core collector, and a helper that cannot ensure that does not serve.
    if !disable_core_dumps() {
        return EXIT_HARDEN;
    }
    limit_resources(memory_mib);
    match thread::Builder::new()
        .name("oms-graph-python-helper".to_owned())
        .stack_size(PARSE_STACK_BYTES)
        .spawn(serve)
    {
        Ok(handle) => handle.join().unwrap_or(EXIT_IO),
        Err(_) => EXIT_IO,
    }
}

/// Linux: mark the process non-dumpable and read the attribute back. Unlike
/// `RLIMIT_CORE`, this also stops dumps piped to a crash collector (the limit
/// is ignored for pipes). macOS has no equivalent here: only the core file
/// size limit below applies, and no stronger guarantee is claimed.
#[cfg(target_os = "linux")]
fn disable_core_dumps() -> bool {
    use nix::sys::prctl::{get_dumpable, set_dumpable};
    set_dumpable(false).is_ok() && get_dumpable() == Ok(false)
}

#[cfg(not(target_os = "linux"))]
fn disable_core_dumps() -> bool {
    true
}

/// Best effort: no core files, and an address-space ceiling where the OS
/// enforces one (Linux does; macOS accepts the call but does not enforce it).
#[cfg(unix)]
fn limit_resources(memory_mib: Option<u64>) {
    use nix::sys::resource::{Resource, setrlimit};
    let _ = setrlimit(Resource::RLIMIT_CORE, 0, 0);
    if let Some(mib) = memory_mib {
        let bytes = mib.saturating_mul(1024 * 1024);
        let _ = setrlimit(Resource::RLIMIT_AS, bytes, bytes);
    }
}

#[cfg(not(unix))]
fn limit_resources(_memory_mib: Option<u64>) {}

fn serve() -> i32 {
    let (mut input, mut output) = (io::stdin().lock(), io::stdout().lock());
    loop {
        let (header, payload) = match read_frame(&mut input, MAX_REQUEST_PAYLOAD) {
            Ok(Some(frame)) => frame,
            Ok(None) => return 0,
            Err(_) => return EXIT_PROTOCOL,
        };
        if header.kind != KIND_REQUEST {
            return EXIT_PROTOCOL;
        }
        let Ok(request) = decode_request(&payload) else {
            return EXIT_PROTOCOL;
        };
        let limit = (request.max_bytes as usize).min(MAX_SOURCE_BYTES);
        let reply = if request.source.len() > limit {
            (KIND_REFUSAL, REFUSE_SOURCE.to_vec())
        } else {
            let graph = crate::extract::project(request.path, request.source, request.digest);
            let json = graph.to_json().into_bytes();
            if json.len() > MAX_RESPONSE_BYTES {
                (KIND_REFUSAL, REFUSE_RESPONSE.to_vec())
            } else {
                (KIND_RESULT, json)
            }
        };
        if write_frame(&mut output, reply.0, header.id, &reply.1)
            .and_then(|()| output.flush())
            .is_err()
        {
            return EXIT_IO;
        }
    }
}
