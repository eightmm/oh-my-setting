#![forbid(unsafe_code)]

use serde::de::{Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;
use std::collections::HashSet;
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
    Text(Vec<u8>),
    Number(String),
    Nonfinite,
}

// Serde validates each leaf; every original string and number becomes an
// arena reference, so no input string can impersonate one.
fn adapt(input: &[u8]) -> Result<(Vec<u8>, Vec<Leaf>, usize), &'static str> {
    if input.len() > 4 * 1024 * 1024 {
        return Err("context manifest exceeds the read limit");
    }
    std::str::from_utf8(input).map_err(|_| INVALID)?;
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
                    Some(Leaf::Text(value))
                }
                _ => None,
            }
        } else if input[i..].starts_with(b"NaN") {
            consumed = 3;
            Some(Leaf::Nonfinite)
        } else if input[i..].starts_with(b"Infinity") {
            consumed = 8;
            Some(Leaf::Nonfinite)
        } else if input[i..].starts_with(b"-Infinity") {
            consumed = 9;
            Some(Leaf::Nonfinite)
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
                if depth > 1024 {
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
            Leaf::Number(n) if !self.key => Ok(Json::Number(n.clone())),
            Leaf::Nonfinite if !self.key => Err(E::custom(NONFINITE)),
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
        })? {
            let Json::String(ref key) = key else {
                return Err(serde::de::Error::custom(INVALID));
            };
            let value = map.next_value_seed(Seed {
                leaves: self.leaves,
                key: false,
            })?;
            fields.push((key.clone(), value));
        }
        let mut seen = HashSet::new();
        if fields.iter().any(|(key, _)| !seen.insert(key)) {
            return Err(serde::de::Error::custom(DUPLICATE));
        }
        Ok(Json::Object(fields))
    }
}
fn decode(adapted: &[u8], leaves: &[Leaf]) -> Result<Json, &'static str> {
    let mut de = serde_json::Deserializer::from_slice(adapted);
    de.disable_recursion_limit();
    let value = Seed { leaves, key: false }
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
    let (adapted, leaves, depth) = adapt(input)?;
    if depth <= 128 {
        return decode(&adapted, &leaves);
    }
    static DEEP_PARSE: Mutex<()> = Mutex::new(());
    let _guard = DEEP_PARSE.lock().unwrap_or_else(|e| e.into_inner());
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(scope, || decode(&adapted, &leaves))
            .map_err(|_| INVALID)?
            .join()
            .map_err(|_| INVALID)?
    })
}

mod manifest;
pub use manifest::{compare_manifests, inspect_manifest};
