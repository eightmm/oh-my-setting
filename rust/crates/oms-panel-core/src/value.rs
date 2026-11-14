use crate::{Error, Result, Value, require};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use sha2::{Digest, Sha256};
use std::fmt;
use std::os::unix::ffi::OsStrExt;

struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("finite JSON with unique fields")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .map(Unique)
                    .ok_or_else(|| E::custom("nonfinite JSON"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut input: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(v)) = input.next_element()? {
                    values.push(v);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut input: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((k, Unique(v))) = input.next_entry::<String, Unique>()? {
                    if values.insert(k, v).is_some() {
                        return Err(de::Error::custom("duplicate JSON field"));
                    }
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(V)
    }
}
pub fn decode(bytes: &[u8], maximum: usize) -> Result<Value> {
    require(bytes.len() <= maximum, "panel JSON exceeds byte limit")?;
    serde_json::from_slice::<Unique>(bytes)
        .map(|v| v.0)
        .map_err(|_| Error::new("invalid panel JSON"))
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn now() -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp() as i64, 0)
        .map(|v| v.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
pub(crate) fn encoded(value: &Value) -> Result<Vec<u8>> {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(v) => Value::Object(
                v.iter()
                    .map(|(k, v)| (k.clone(), sorted(v)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(v) => Value::Array(v.iter().map(sorted).collect()),
            v => v.clone(),
        }
    }
    let mut bytes = serde_json::to_vec(&sorted(value)).map_err(Error::new)?;
    bytes.push(b'\n');
    Ok(bytes)
}
pub fn outbound(bytes: &[u8]) -> Result<()> {
    let values: Vec<_> = std::env::vars_os().collect();
    require(
        values.len() <= oms_scrub::MAX_ENV_ENTRIES
            && values
                .iter()
                .map(|(k, v)| k.as_bytes().len() + v.as_bytes().len())
                .sum::<usize>()
                <= oms_scrub::MAX_ENV_BYTES,
        "panel environment exceeds outbound scan limits",
    )?;
    let env: Vec<_> = values
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect();
    let hits =
        oms_scrub::scan(bytes, &env).map_err(|_| Error::new("panel outbound scan failed"))?;
    require(
        hits.is_empty(),
        "panel outbound content refused by scrubber",
    )
}
pub(crate) fn redact(bytes: &[u8]) -> Result<String> {
    let values: Vec<_> = std::env::vars_os().collect();
    require(
        values.len() <= oms_scrub::MAX_ENV_ENTRIES
            && values
                .iter()
                .map(|(k, v)| k.as_bytes().len() + v.as_bytes().len())
                .sum::<usize>()
                <= oms_scrub::MAX_ENV_BYTES,
        "panel environment exceeds redaction limits",
    )?;
    let env: Vec<_> = values
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect();
    let redacted =
        oms_scrub::redact(bytes, &env).map_err(|_| Error::new("panel redaction failed"))?;
    Ok(String::from_utf8_lossy(&redacted.bytes).into_owned())
}
pub(crate) fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}
pub(crate) fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}
pub(crate) fn unique() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| Error::new("native random source unavailable"))?;
    Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
}
