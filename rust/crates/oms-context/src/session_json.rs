use crate::{Json, parse_mode};

/// A malformed row, a row over the supported bound, or an internal parser failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionJsonError {
    /// The row is malformed, improperly encoded, or exceeds parser depth or integer limits.
    Invalid,
    /// The raw row exceeds 16 MiB.
    Limit,
    /// The bounded deep-parser worker could not be started or failed.
    Infrastructure,
}

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf16(bool),
    Utf32(bool),
}

fn encoding(input: &[u8]) -> (Encoding, usize) {
    if input.starts_with(&[0, 0, 0xfe, 0xff]) {
        return (Encoding::Utf32(true), 4);
    }
    if input.starts_with(&[0xff, 0xfe, 0, 0]) {
        return (Encoding::Utf32(false), 4);
    }
    if input.starts_with(&[0xfe, 0xff]) {
        return (Encoding::Utf16(true), 2);
    }
    if input.starts_with(&[0xff, 0xfe]) {
        return (Encoding::Utf16(false), 2);
    }
    if input.starts_with(&[0xef, 0xbb, 0xbf]) {
        return (Encoding::Utf8, 3);
    }
    if input.len() >= 4 {
        if input[0] == 0 {
            return if input[1] == 0 {
                (Encoding::Utf32(true), 0)
            } else {
                (Encoding::Utf16(true), 0)
            };
        }
        if input[1] == 0 {
            return if input[2] != 0 || input[3] != 0 {
                (Encoding::Utf16(false), 0)
            } else {
                (Encoding::Utf32(false), 0)
            };
        }
    } else if input.len() == 2 {
        if input[0] == 0 {
            return (Encoding::Utf16(true), 0);
        }
        if input[1] == 0 {
            return (Encoding::Utf16(false), 0);
        }
    }
    (Encoding::Utf8, 0)
}

fn push_codepoint(out: &mut Vec<u8>, n: u32) -> Result<(), SessionJsonError> {
    if (0xd800..=0xdfff).contains(&n) {
        out.push(0xe0 | (n >> 12) as u8);
        out.push(0x80 | ((n >> 6) & 63) as u8);
        out.push(0x80 | (n & 63) as u8);
    } else {
        let ch = char::from_u32(n).ok_or(SessionJsonError::Invalid)?;
        let mut bytes = [0; 4];
        out.extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
    }
    Ok(())
}

fn transcode(input: &[u8]) -> Result<Vec<u8>, SessionJsonError> {
    let (enc, skip) = encoding(input);
    let input = &input[skip..];
    let mut out = Vec::with_capacity(input.len());
    match enc {
        Encoding::Utf8 => {
            let mut rest = input;
            while !rest.is_empty() {
                match std::str::from_utf8(rest) {
                    Ok(_) => {
                        out.extend_from_slice(rest);
                        break;
                    }
                    Err(e) => {
                        out.extend_from_slice(&rest[..e.valid_up_to()]);
                        rest = &rest[e.valid_up_to()..];
                        if rest.len() < 3
                            || rest[0] != 0xed
                            || !(0xa0..=0xbf).contains(&rest[1])
                            || !(0x80..=0xbf).contains(&rest[2])
                        {
                            return Err(SessionJsonError::Invalid);
                        }
                        out.extend_from_slice(&rest[..3]);
                        rest = &rest[3..];
                    }
                }
            }
        }
        Encoding::Utf16(be) => {
            let (chunks, remainder) = input.as_chunks::<2>();
            let units: Vec<u16> = chunks
                .iter()
                .map(|p| {
                    if be {
                        u16::from_be_bytes([p[0], p[1]])
                    } else {
                        u16::from_le_bytes([p[0], p[1]])
                    }
                })
                .collect();
            if !remainder.is_empty() {
                return Err(SessionJsonError::Invalid);
            }
            let mut i = 0;
            while i < units.len() {
                let high = units[i] as u32;
                i += 1;
                if (0xd800..=0xdbff).contains(&high)
                    && i < units.len()
                    && (0xdc00..=0xdfff).contains(&(units[i] as u32))
                {
                    let low = units[i] as u32;
                    i += 1;
                    push_codepoint(&mut out, 0x10000 + ((high - 0xd800) << 10) + low - 0xdc00)?;
                } else {
                    push_codepoint(&mut out, high)?;
                }
            }
        }
        Encoding::Utf32(be) => {
            let (chunks, remainder) = input.as_chunks::<4>();
            for p in chunks {
                let b = [p[0], p[1], p[2], p[3]];
                push_codepoint(
                    &mut out,
                    if be {
                        u32::from_be_bytes(b)
                    } else {
                        u32::from_le_bytes(b)
                    },
                )?;
            }
            if !remainder.is_empty() {
                return Err(SessionJsonError::Invalid);
            }
        }
    }
    Ok(out)
}

/// Parse one bounded session JSON row. Strings contain WTF-8, numbers retain
/// their JSON spelling (including non-finite constants), and duplicate keys
/// retain their first position and last value. Input uses Python JSON byte
/// encoding detection with surrogate-pass decoding; errors never include row
/// text. Nested values are limited to 1,024 containers.
pub fn parse_session_json(raw: &[u8]) -> Result<Json, SessionJsonError> {
    if raw.len() > 16 * 1024 * 1024 {
        return Err(SessionJsonError::Limit);
    }
    let input = transcode(raw)?;
    parse_mode(&input, true, 48 * 1024 * 1024).map_err(|e| {
        if e == "parser infrastructure failure" {
            SessionJsonError::Infrastructure
        } else {
            SessionJsonError::Invalid
        }
    })
}
