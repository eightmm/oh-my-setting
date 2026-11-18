use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct Error {
    pub code: i32,
    pub message: String,
}
impl Error {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl From<oms_command_plan::Error> for Error {
    fn from(e: oms_command_plan::Error) -> Self {
        Self::new(e.code, e.message)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub fn hash(raw: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(raw.as_ref()))
}
pub fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn commit(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn id(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn parse(raw: &[u8]) -> Result<Value> {
    oms_context::parse_json(raw)
        .map_err(|_| Error::new(2, "invalid, duplicate-key or oversized JSON"))?;
    serde_json::from_slice(raw)
        .map_err(|_| Error::new(2, "JSON is not representable as bounded UTF-8 data"))
}
pub fn pretty(v: &Value) -> Result<Vec<u8>> {
    let mut raw = serde_json::to_vec_pretty(v).map_err(|_| Error::new(2, "cannot encode JSON"))?;
    raw.push(b'\n');
    Ok(raw)
}
pub fn canonical(v: &Value) -> Result<Vec<u8>> {
    Ok(oms_command_plan::json::canonical(&plan(v)?)?)
}
pub fn plan(v: &Value) -> Result<oms_command_plan::json::Value> {
    let raw = serde_json::to_vec(v).map_err(|_| Error::new(2, "cannot encode JSON"))?;
    Ok(oms_command_plan::json::parse(&raw)?)
}
pub fn from_plan(v: &oms_command_plan::json::Value) -> Result<Value> {
    parse(&oms_command_plan::json::canonical(v)?)
}
pub(crate) fn from_shared(v: &oms_json::Value) -> Result<Value> {
    parse(&oms_json::canonical_json(v).map_err(|_| Error::new(2, "cannot encode shared JSON"))?)
}
pub fn keys(v: &Value, required: &[&str], optional: &[&str]) -> Result<()> {
    let map = v
        .as_object()
        .ok_or_else(|| Error::new(2, "expected JSON object"))?;
    crate::require(
        required.iter().all(|k| map.contains_key(*k))
            && map
                .keys()
                .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str())),
        "malformed or unsupported object fields",
    )
}
pub fn integer(v: &Value, low: u64, high: u64, label: &str) -> Result<u64> {
    v.as_u64()
        .filter(|n| (low..=high).contains(n))
        .ok_or_else(|| Error::new(2, format!("{label} must be {low}..{high}")))
}
pub fn now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    chrono::DateTime::from_timestamp(seconds as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
pub fn random(prefix: &str) -> Result<String> {
    Ok(format!(
        "{prefix}{}",
        oms_command_plan::storage::random_hex(16)?
    ))
}
pub fn clean_text(s: &str) -> bool {
    use unicode_general_category::{GeneralCategory as G, get_general_category};
    !s.chars().any(|c| {
        matches!(
            get_general_category(c),
            G::Control | G::Format | G::Surrogate
        )
    })
}
