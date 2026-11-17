#![forbid(unsafe_code)]

use serde::de::{Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Mutex;

/// A JSON value whose strings are WTF-8 bytes and whose numbers retain their spelling.
#[derive(Debug, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(Vec<u8>),
    Array(Vec<Json>),
    Object(Vec<(Vec<u8>, Json)>),
}

impl Drop for Json {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        fn drain(value: &mut Json, pending: &mut Vec<Json>) {
            match value {
                Json::Array(values) => pending.append(values),
                Json::Object(values) => pending.extend(values.drain(..).map(|(_, value)| value)),
                _ => {}
            }
        }
        drain(self, &mut pending);
        while let Some(mut value) = pending.pop() {
            drain(&mut value, &mut pending);
        }
    }
}

fn discard_json(value: Json) {
    drop(value);
}

struct Bytes(Vec<u8>);

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ByteVisitor;
        impl Visitor<'_> for ByteVisitor {
            type Value = Bytes;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON string")
            }
            fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<Bytes, E> {
                Ok(Bytes(value.to_vec()))
            }
            fn visit_byte_buf<E: serde::de::Error>(self, value: Vec<u8>) -> Result<Bytes, E> {
                Ok(Bytes(value))
            }
        }
        deserializer.deserialize_bytes(ByteVisitor)
    }
}

const INVALID: &str = "cannot read a valid repo-local context manifest";
const DUPLICATE: &str = "invalid context manifest: duplicate JSON key";
const NONFINITE: &str = "invalid context manifest: non-finite JSON value";

enum Leaf {
    Invalid,
    Text(Vec<u8>),
    Number(String),
    Nonfinite(&'static str),
}

// Serde validates each leaf; every original string and number becomes an
// arena reference, so no input string can impersonate one.
fn adapt(
    input: &[u8],
    max_bytes: usize,
    session: bool,
    depth_limit: usize,
) -> Result<(Vec<u8>, Vec<Leaf>, usize), &'static str> {
    if input.len() > max_bytes {
        return Err("context manifest exceeds the read limit");
    }
    if !session {
        std::str::from_utf8(input).map_err(|_| INVALID)?;
    }
    let mut output = Vec::with_capacity(input.len());
    let mut leaves = Vec::new();
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        let mut consumed = 0;
        let leaf = if b == b'"' {
            let mut stream = serde_json::Deserializer::from_slice(&input[i..]).into_iter::<Bytes>();
            match stream.next() {
                Some(Ok(Bytes(value))) => {
                    consumed = stream.byte_offset();
                    // deserialize_bytes preserves surrogate code points but also accepts
                    // literal C0 bytes; JSON only permits escaped controls.
                    Some(if input[i..i + consumed].iter().any(|b| *b < 0x20) {
                        Leaf::Invalid
                    } else {
                        Leaf::Text(value)
                    })
                }
                _ => None,
            }
        } else if input[i..].starts_with(b"NaN") {
            consumed = 3;
            Some(Leaf::Nonfinite("NaN"))
        } else if input[i..].starts_with(b"Infinity") {
            consumed = 8;
            Some(Leaf::Nonfinite("Infinity"))
        } else if input[i..].starts_with(b"-Infinity") {
            consumed = 9;
            Some(Leaf::Nonfinite("-Infinity"))
        } else if matches!(b, b'-' | b'0'..=b'9') {
            let mut stream =
                serde_json::Deserializer::from_slice(&input[i..]).into_iter::<&RawValue>();
            match stream.next() {
                Some(Ok(value)) => {
                    consumed = stream.byte_offset();
                    Some(Leaf::Number(value.get().to_owned()))
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(leaf) = leaf {
            output.push(b'"');
            output.extend_from_slice(leaves.len().to_string().as_bytes());
            output.push(b'"');
            leaves.push(leaf);
            i += consumed;
        } else if b == b'"' || matches!(b, b'-' | b'0'..=b'9') {
            // Preserve the remaining malformed text for serde to diagnose in order.
            output.extend_from_slice(&input[i..]);
            break;
        } else {
            if matches!(b, b'[' | b'{') {
                depth += 1;
                max_depth = max_depth.max(depth);
                if depth > depth_limit {
                    return Err(INVALID);
                }
            }
            if matches!(b, b']' | b'}') {
                depth = depth.saturating_sub(1);
            }
            output.push(b);
            i += 1;
        }
    }
    Ok((output, leaves, max_depth))
}

struct Seed<'a> {
    leaves: &'a [Leaf],
    key: bool,
    session: bool,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Json;
    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Json, D::Error> {
        de.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Json;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an adapted JSON value")
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Json, E> {
        let leaf = value
            .parse::<usize>()
            .ok()
            .and_then(|i| self.leaves.get(i))
            .ok_or_else(|| E::custom(INVALID))?;
        match leaf {
            Leaf::Text(bytes) => Ok(Json::String(bytes.clone())),
            Leaf::Number(n) if !self.key => {
                if self.session
                    && !n.contains(['.', 'e', 'E'])
                    && n.trim_start_matches('-').len() > 4300
                {
                    return Err(E::custom(INVALID));
                }
                Ok(Json::Number(n.clone()))
            }
            Leaf::Nonfinite(n) if !self.key && self.session => Ok(Json::Number((*n).into())),
            Leaf::Nonfinite(_) if !self.key => Err(E::custom(NONFINITE)),
            _ => Err(E::custom(INVALID)),
        }
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Json, E> {
        if self.key {
            Err(E::custom(INVALID))
        } else {
            Ok(Json::Bool(value))
        }
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Json, E> {
        if self.key {
            Err(E::custom(INVALID))
        } else {
            Ok(Json::Null)
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Seed {
            leaves: self.leaves,
            key: false,
            session: self.session,
        })? {
            values.push(value);
        }
        Ok(Json::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut fields = Vec::new();
        while let Some(key) = map.next_key_seed(Seed {
            leaves: self.leaves,
            key: true,
            session: self.session,
        })? {
            let Json::String(ref key) = key else {
                return Err(serde::de::Error::custom(INVALID));
            };
            let value = map.next_value_seed(Seed {
                leaves: self.leaves,
                key: false,
                session: self.session,
            })?;
            fields.push((key.clone(), value));
        }
        if self.session {
            let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
            let mut unique: Vec<(Vec<u8>, Json)> = Vec::new();
            for (key, value) in fields {
                if let Some(position) = index.get(&key) {
                    unique[*position].1 = value;
                } else {
                    index.insert(key.clone(), unique.len());
                    unique.push((key, value));
                }
            }
            Ok(Json::Object(unique))
        } else {
            let mut seen = HashSet::new();
            if fields.iter().any(|(key, _)| !seen.insert(key)) {
                return Err(serde::de::Error::custom(DUPLICATE));
            }
            Ok(Json::Object(fields))
        }
    }
}
fn decode(adapted: &[u8], leaves: &[Leaf], session: bool) -> Result<Json, &'static str> {
    let mut de = serde_json::Deserializer::from_slice(adapted);
    de.disable_recursion_limit();
    let value = Seed {
        leaves,
        key: false,
        session,
    }
    .deserialize(&mut de)
    .map_err(|e| {
        let e = e.to_string();
        if e.starts_with(DUPLICATE) {
            DUPLICATE
        } else if e.starts_with(NONFINITE) {
            NONFINITE
        } else {
            INVALID
        }
    })?;
    de.end().map_err(|_| INVALID)?;
    Ok(value)
}
/// Parse bounded raw JSON, retaining numeric spellings and WTF-8 strings.
pub fn parse_json(input: &[u8]) -> Result<Json, &'static str> {
    parse_json_bounded(input, 4 * 1024 * 1024)
}
/// Parse bounded raw JSON with an explicit byte limit, retaining numeric spellings and WTF-8 strings.
pub fn parse_json_bounded(input: &[u8], limit: usize) -> Result<Json, &'static str> {
    parse_mode(input, false, limit).map_err(|e| {
        if e == "parser infrastructure failure" {
            INVALID
        } else {
            e
        }
    })
}
/// Parse Python string JSON without byte encoding detection.
/// Duplicate object keys keep their last value and non-finite constants are accepted.
pub fn parse_experiment_json(input: &[u8], limit: usize) -> Result<Json, &'static str> {
    parse_mode(input, true, limit)
}
/// Parse the Python string dialect with an explicit container-depth budget.
/// Existing entrypoints retain 1024. Zero allows scalars only; the maximum
/// supported budget is 16384, keeping decoder stack reservations bounded.
pub fn parse_experiment_json_with_depth(
    input: &[u8],
    limit: usize,
    max_depth: usize,
) -> Result<Json, &'static str> {
    if max_depth > 16384 {
        return Err(INVALID);
    }
    parse_mode_depth(input, true, limit, max_depth)
}
fn parse_mode(input: &[u8], session: bool, max_bytes: usize) -> Result<Json, &'static str> {
    parse_mode_depth(input, session, max_bytes, 1024)
}
fn parse_mode_depth(
    input: &[u8],
    session: bool,
    max_bytes: usize,
    depth_limit: usize,
) -> Result<Json, &'static str> {
    let (adapted, leaves, depth) = adapt(input, max_bytes, session, depth_limit)?;
    if depth <= 128 {
        return decode(&adapted, &leaves, session);
    }
    static DEEP_PARSE: Mutex<()> = Mutex::new(());
    let _guard = DEEP_PARSE.lock().unwrap_or_else(|e| e.into_inner());
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size((16 * 1024 * 1024).max(depth * 16 * 1024))
            .spawn_scoped(scope, || decode(&adapted, &leaves, session))
            .map_err(|_| "parser infrastructure failure")?
            .join()
            .map_err(|_| "parser infrastructure failure")?
    })
}

mod manifest;
mod session_json;
pub use manifest::{compare_manifests, inspect_manifest, inspection_limit_value};
pub use session_json::{SessionJsonError, parse_session_json};
