//! Frame protocol between a [`crate::Session`] and its private helper process.
//!
//! One frame is a 20 byte header followed by `len` payload bytes:
//!
//! ```text
//! 0..4   magic "OGPY"          4      version (2)
//! 5      kind                  6..8   reserved, zero
//! 8..16  request id, u64 LE    16..20 payload length, u32 LE
//! ```
//!
//! A request (kind 1) carries `max_bytes u32 | path_len u32 | digest_len u32 |
//! path | digest | source` and is answered by exactly one frame with the same
//! id: a result (kind 2, the JSON `ParseResult`) or a refusal (kind 3, an ASCII
//! reason). Every length is checked against a fixed ceiling before a byte of
//! payload is buffered, and every violation ends the conversation: there is no
//! resynchronization.

use std::io::{self, Read, Write};

pub(crate) const MAGIC: [u8; 4] = *b"OGPY";
/// Frame bodies are UTF-8 JSON `ParseResult` values (including `syntax_valid`
/// on version 2) for kind 2 results, or ASCII refusal reasons for kind 3.
pub(crate) const VERSION: u8 = 2;
pub(crate) const HEADER_LEN: usize = 20;
pub(crate) const KIND_REQUEST: u8 = 1;
pub(crate) const KIND_RESULT: u8 = 2;
pub(crate) const KIND_REFUSAL: u8 = 3;

/// Largest source a session will ever send, whatever `max_bytes` asks for.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
/// Path and digest are repo-relative names and hex digests.
pub(crate) const MAX_FIELD_BYTES: usize = 64 * 1024;
const REQUEST_FIXED: usize = 12;
pub(crate) const MAX_REQUEST_PAYLOAD: usize =
    REQUEST_FIXED + 2 * MAX_FIELD_BYTES + MAX_SOURCE_BYTES;
/// Serialized graph ceiling. A result is a few dozen bytes of JSON per source
/// byte at worst (a call every four bytes); 128 MiB covers the default 2 MiB
/// source with a wide margin and bounds the client's buffer.
pub const MAX_RESPONSE_BYTES: usize = 128 * 1024 * 1024;

pub(crate) const REFUSE_SOURCE: &[u8] = b"source_too_large";
pub(crate) const REFUSE_RESPONSE: &[u8] = b"response_too_large";

#[derive(Debug)]
pub(crate) enum FrameError {
    /// The stream ended inside a frame.
    Truncated,
    Io,
    Invalid(&'static str),
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub(crate) kind: u8,
    pub(crate) id: u64,
    pub(crate) len: usize,
}

pub(crate) fn write_frame(
    out: &mut impl Write,
    kind: u8,
    id: u64,
    payload: &[u8],
) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(&MAGIC);
    header[4] = VERSION;
    header[5] = kind;
    header[8..16].copy_from_slice(&id.to_le_bytes());
    header[16..20].copy_from_slice(&len.to_le_bytes());
    out.write_all(&header)?;
    out.write_all(payload)?;
    out.flush()
}

pub(crate) fn decode_header(bytes: &[u8; HEADER_LEN]) -> Result<Header, FrameError> {
    if bytes[0..4] != MAGIC {
        return Err(FrameError::Invalid("bad magic"));
    }
    if bytes[4] != VERSION {
        return Err(FrameError::Invalid("unsupported protocol version"));
    }
    if bytes[6..8] != [0, 0] {
        return Err(FrameError::Invalid("reserved bytes set"));
    }
    let id = u64::from_le_bytes(bytes[8..16].try_into().map_err(|_| FrameError::Truncated)?);
    let len = u32::from_le_bytes(
        bytes[16..20]
            .try_into()
            .map_err(|_| FrameError::Truncated)?,
    );
    Ok(Header {
        kind: bytes[5],
        id,
        len: len as usize,
    })
}

/// Read one frame; `Ok(None)` is a clean end of stream between frames.
pub(crate) fn read_frame(
    input: &mut impl Read,
    max_payload: usize,
) -> Result<Option<(Header, Vec<u8>)>, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        match input.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(FrameError::Io),
        }
    }
    let header = decode_header(&header)?;
    if header.len > max_payload {
        return Err(FrameError::Invalid("frame over the length ceiling"));
    }
    // Grow with what arrives rather than trusting the declared length.
    let mut payload = Vec::new();
    input
        .by_ref()
        .take(header.len as u64)
        .read_to_end(&mut payload)
        .map_err(|_| FrameError::Io)?;
    if payload.len() != header.len {
        return Err(FrameError::Truncated);
    }
    Ok(Some((header, payload)))
}

pub(crate) struct Request<'a> {
    pub(crate) max_bytes: u32,
    pub(crate) path: &'a str,
    pub(crate) digest: &'a str,
    pub(crate) source: &'a str,
}

pub(crate) fn encode_request(request: &Request<'_>) -> Vec<u8> {
    let mut payload = Vec::with_capacity(
        REQUEST_FIXED + request.path.len() + request.digest.len() + request.source.len(),
    );
    payload.extend_from_slice(&request.max_bytes.to_le_bytes());
    payload.extend_from_slice(&(request.path.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(request.digest.len() as u32).to_le_bytes());
    payload.extend_from_slice(request.path.as_bytes());
    payload.extend_from_slice(request.digest.as_bytes());
    payload.extend_from_slice(request.source.as_bytes());
    payload
}

pub(crate) fn decode_request(payload: &[u8]) -> Result<Request<'_>, &'static str> {
    let field = |at: usize| -> Result<u32, &'static str> {
        let bytes = payload.get(at..at + 4).ok_or("short request")?;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(|_| "short request")?,
        ))
    };
    let (max_bytes, path_len, digest_len) = (field(0)?, field(4)? as usize, field(8)? as usize);
    if path_len > MAX_FIELD_BYTES || digest_len > MAX_FIELD_BYTES {
        return Err("field over the length ceiling");
    }
    let body = &payload[REQUEST_FIXED..];
    let path = body.get(..path_len).ok_or("short request")?;
    let digest = body
        .get(path_len..path_len + digest_len)
        .ok_or("short request")?;
    let source = &body[path_len + digest_len..];
    Ok(Request {
        max_bytes,
        path: std::str::from_utf8(path).map_err(|_| "path is not UTF-8")?,
        digest: std::str::from_utf8(digest).map_err(|_| "digest is not UTF-8")?,
        source: std::str::from_utf8(source).map_err(|_| "source is not UTF-8")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: u8, id: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, kind, id, payload).unwrap();
        out
    }

    #[test]
    fn frames_round_trip_and_clean_eof_is_none() {
        let bytes = frame(KIND_RESULT, 7, b"{}");
        let mut input = bytes.as_slice();
        let (header, payload) = read_frame(&mut input, 16).unwrap().unwrap();
        assert_eq!(
            header,
            Header {
                kind: KIND_RESULT,
                id: 7,
                len: 2
            }
        );
        assert_eq!(payload, b"{}");
        assert!(read_frame(&mut input, 16).unwrap().is_none());
        assert!(read_frame(&mut &b""[..], 16).unwrap().is_none());
    }

    #[test]
    fn every_malformed_frame_is_refused() {
        let good = frame(KIND_RESULT, 1, b"abcd");
        let cases: [(&str, Vec<u8>); 6] = [
            ("truncated header", good[..10].to_vec()),
            ("truncated payload", good[..good.len() - 1].to_vec()),
            ("magic", [b"XXXX".as_slice(), &good[4..]].concat()),
            ("version", {
                let mut bad = good.clone();
                bad[4] = VERSION.wrapping_add(1);
                bad
            }),
            ("reserved", {
                let mut bad = good.clone();
                bad[6] = 1;
                bad
            }),
            ("oversize", frame(KIND_RESULT, 1, &[0u8; 17])),
        ];
        for (name, bytes) in cases {
            assert!(read_frame(&mut bytes.as_slice(), 16).is_err(), "{name}");
        }
        // The length ceiling is exclusive of the payload at the limit.
        assert!(read_frame(&mut frame(KIND_RESULT, 1, &[0u8; 16]).as_slice(), 16).is_ok());
    }

    #[test]
    fn requests_round_trip_and_bad_fields_are_refused() {
        let request = Request {
            max_bytes: 9,
            path: "a/b.py",
            digest: "d",
            source: "x = 1\n",
        };
        let payload = encode_request(&request);
        let decoded = decode_request(&payload).unwrap();
        assert_eq!(
            (
                decoded.max_bytes,
                decoded.path,
                decoded.digest,
                decoded.source
            ),
            (9, "a/b.py", "d", "x = 1\n")
        );
        assert!(decode_request(&payload[..11]).is_err());
        assert!(decode_request(&payload[..13]).is_err());
        let mut long = payload.clone();
        long[4..8].copy_from_slice(&(MAX_FIELD_BYTES as u32 + 1).to_le_bytes());
        assert!(decode_request(&long).is_err());
        let mut bad_utf8 = payload.clone();
        *bad_utf8.last_mut().unwrap() = 0xff;
        assert!(decode_request(&bad_utf8).is_err());
    }
}
