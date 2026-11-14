//! Reuse the native lossless value representation and Python-compatible encodings.
use crate::{Result, error};
pub use oms_command_plan::json::*;
pub fn b(value: bool) -> Value {
    Value::Bool(value)
}
pub fn a(items: impl IntoIterator<Item = Value>) -> Value {
    Value::Array(items.into_iter().collect())
}
pub fn rows(value: &Value) -> &[Value] {
    value.array().unwrap_or(&[])
}
pub fn text(value: &Value) -> &str {
    value.str().unwrap_or("")
}
pub fn optional(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        string(value)
    }
}
pub fn finite(value: &Value) -> Option<f64> {
    if let Value::Number(n) = value {
        n.parse::<f64>().ok().filter(|n| n.is_finite())
    } else {
        None
    }
}
pub fn from_native(value: &oms_context::Json) -> Value {
    use oms_context::Json as J;
    match value {
        J::Null => Value::Null,
        J::Bool(v) => b(*v),
        J::Number(v) => Value::Number(v.clone()),
        J::String(v) => Value::String(v.clone()),
        J::Array(v) => a(v.iter().map(from_native)),
        J::Object(v) => Value::Object(v.iter().map(|(k, v)| (k.clone(), from_native(v))).collect()),
    }
}
pub fn from_serde(value: &serde_json::Value) -> Result<Value> {
    parse(&serde_json::to_vec(value).map_err(error)?)
}
pub fn from_encoded(value: &oms_json::Value) -> Result<Value> {
    parse(&oms_json::canonical_json(value).map_err(error)?)
}
pub fn bounded(value: &str, limit: usize) -> String {
    oms_contract_markdown::bounded_line(value, limit)
}
pub fn pick(value: &Value, names: &[&str]) -> Value {
    Value::Object(
        value
            .object()
            .unwrap_or(&[])
            .iter()
            .filter(|(k, _)| names.iter().any(|s| s.as_bytes() == k))
            .cloned()
            .collect(),
    )
}
pub fn without(value: &Value, names: &[&str]) -> Value {
    Value::Object(
        value
            .object()
            .unwrap_or(&[])
            .iter()
            .filter(|(k, _)| !names.iter().any(|s| s.as_bytes() == k))
            .cloned()
            .collect(),
    )
}
pub fn merge(base: &Value, extra: &Value) -> Result<Value> {
    let mut out = base.clone();
    for (k, v) in extra.object()? {
        out.set(std::str::from_utf8(k).map_err(error)?, v.clone())?;
    }
    Ok(out)
}
pub fn sorted_pretty(value: &Value) -> Result<Vec<u8>> {
    pretty(&parse(&canonical(value)?)?)
}
