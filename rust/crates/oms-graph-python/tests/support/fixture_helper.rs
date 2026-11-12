//! Private TEST fixture, not part of the product: a native stand-in for the
//! parser helper that misbehaves in one chosen way, so the session's failure
//! handling is exercised without any shell or interpreter.
//!
//! The scenario is the file name it is started under (`fixture-<scenario>`),
//! because a session starts its helper with a fixed argument list and an empty
//! environment. Tests create such symlinks in a temporary directory. This
//! binary is built only so `CARGO_BIN_EXE_oms-graph-python-fixture` exists; it
//! must never be installed, packaged or placed on a PATH.

use std::io::Write;
use std::process::exit;
use std::time::Duration;

fn frame(version: u8, kind: u8, id: u64, declared_len: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = b"OGPY".to_vec();
    out.extend([version, kind, 0, 0]);
    out.extend(id.to_le_bytes());
    out.extend(declared_len.to_le_bytes());
    out.extend(payload);
    out
}

fn emit(bytes: &[u8]) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(bytes);
    let _ = stdout.flush();
}

/// Read one request frame and answer it with an empty graph, after `delay`.
fn reply_empty(delay: Duration) {
    use std::io::Read;
    let mut stdin = std::io::stdin().lock();
    let mut header = [0u8; 20];
    if stdin.read_exact(&mut header).is_err() {
        exit(65);
    }
    let id = u64::from_le_bytes(header[8..16].try_into().unwrap_or_default());
    let len = u32::from_le_bytes(header[16..20].try_into().unwrap_or_default()) as u64;
    let _ = std::io::copy(&mut stdin.by_ref().take(len), &mut std::io::sink());
    std::thread::sleep(delay);
    let json = br#"{"nodes":[],"edges":[],"refs":[]}"#;
    emit(&frame(1, 2, id, json.len() as u32, json));
}

/// Stay alive, ignoring input, until the session kills this process.
fn park() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

fn main() {
    let name = std::env::args().next().unwrap_or_default();
    let scenario = name.rsplit('/').next().unwrap_or_default().to_owned();
    match scenario.strip_prefix("fixture-").unwrap_or_default() {
        "bad-version" => emit(&frame(2, 2, 1, 0, b"")),
        "oversize" => emit(&frame(1, 2, 1, u32::MAX, b"")),
        "wrong-id" => emit(&frame(1, 2, 9, 2, b"{}")),
        "malformed-result" => emit(&frame(1, 2, 1, 2, b"{}")),
        "unexpected-kind" => emit(&frame(1, 7, 1, 2, b"{}")),
        "garbage" => emit(b"not a frame at all, just text"),
        "truncated-header" => {
            emit(b"OGPY\x01\x02");
            exit(0);
        }
        "truncated-payload" => {
            emit(&frame(1, 2, 1, 2, b"{"));
            exit(0);
        }
        "exit-0" => exit(0),
        "exit-7" => exit(7),
        "abort" => std::process::abort(),
        "env-empty" => exit(if std::env::vars_os().next().is_none() {
            0
        } else {
            9
        }),
        "fast-reply" => reply_empty(Duration::ZERO),
        "slow-reply" => reply_empty(Duration::from_millis(400)),
        "hang" => park(),
        _ => exit(64),
    }
    park();
}
