//! Reuse the native lossless value representation and Python-compatible encodings.
use crate::{Result, error};
pub use oms_command_plan::json::*;
/// Runtime state follows common.read_json/json.loads: last duplicate wins,
/// non-finite input remains readable, and output encoders reject non-finite values.
/// Context manifests keep the separate strict native inspector parser.
pub fn parse(raw: &[u8]) -> Result<Value> {
    parse_bounded(raw, crate::storage::MAX_STATE)
}
pub fn parse_bounded(raw: &[u8], limit: usize) -> Result<Value> {
    std::str::from_utf8(raw).map_err(error)?;
    oms_command_plan::json::parse_legacy(raw, limit)
}
/// Convert existing values without serializing/reparsing or imposing a file bound.
pub fn to_native(value: &Value) -> oms_context::Json {
    use oms_context::Json as J;
    match value {
        Value::Null => J::Null,
        Value::Bool(v) => J::Bool(*v),
        Value::Number(v) => J::Number(v.clone()),
        Value::String(v) => J::String(v.clone()),
        Value::Array(v) => J::Array(v.iter().map(to_native).collect()),
        Value::Object(v) => J::Object(v.iter().map(|(k, v)| (k.clone(), to_native(v))).collect()),
    }
}
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
    Ok(match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(v) => b(*v),
        serde_json::Value::Number(v) => Value::Number(v.to_string()),
        serde_json::Value::String(v) => string(v),
        serde_json::Value::Array(v) => a(v.iter().map(from_serde).collect::<Result<Vec<_>>>()?),
        serde_json::Value::Object(v) => Value::Object(
            v.iter()
                .map(|(k, v)| Ok((k.as_bytes().to_vec(), from_serde(v)?)))
                .collect::<Result<_>>()?,
        ),
    })
}
pub fn from_encoded(value: &oms_json::Value) -> Result<Value> {
    fn wtf8(points: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        for &p in points {
            if (0xd800..=0xdfff).contains(&p) {
                out.extend([
                    (0xe0 | (p >> 12)) as u8,
                    (0x80 | ((p >> 6) & 63)) as u8,
                    (0x80 | (p & 63)) as u8,
                ]);
            } else {
                out.extend(
                    char::from_u32(p)
                        .expect("validated native codepoint")
                        .encode_utf8(&mut [0; 4])
                        .as_bytes(),
                );
            }
        }
        out
    }
    use oms_json::Value as E;
    Ok(match value {
        E::Null => Value::Null,
        E::Bool(v) => b(*v),
        // Integer's representation is private; its bounded scalar encoder is the
        // existing authoritative decimal conversion, without an aggregate reparse.
        E::Integer(_) => Value::Number(
            String::from_utf8(oms_json::canonical_json(value).map_err(error)?).map_err(error)?,
        ),
        E::Float(v) => Value::Number(if v.is_nan() {
            "NaN".into()
        } else if *v == f64::INFINITY {
            "Infinity".into()
        } else if *v == f64::NEG_INFINITY {
            "-Infinity".into()
        } else {
            String::from_utf8(oms_json::canonical_json(value).map_err(error)?).map_err(error)?
        }),
        E::String(v) => Value::String(wtf8(v.codepoints())),
        E::Array(v) => a(v.iter().map(from_encoded).collect::<Result<Vec<_>>>()?),
        E::Object(v) => Value::Object(
            v.iter()
                .map(|(k, v)| Ok((wtf8(k.codepoints()), from_encoded(v)?)))
                .collect::<Result<_>>()?,
        ),
    })
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
    fn sorted(value: &Value) -> Result<Value> {
        Ok(match value {
            Value::Array(v) => a(v.iter().map(sorted).collect::<Result<Vec<_>>>()?),
            Value::Object(v) => {
                let mut fields = v
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), sorted(v)?)))
                    .collect::<Result<Vec<_>>>()?;
                fields.sort_by(|(a, _), (b, _)| a.cmp(b));
                Value::Object(fields)
            }
            _ => value.clone(),
        })
    }
    pretty(&sorted(value)?)
}
/// Non-negative budget arithmetic keeps Python-sized integral limits intact.
pub fn subtract_count(limit: &Value, count: usize) -> Result<Value> {
    let Value::Number(raw) = limit else {
        return Err(error("budget limit must be an integer"));
    };
    crate::require(
        !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()),
        "budget limit must be non-negative integral",
    )?;
    let digits = raw.trim_start_matches('0');
    let count = count.to_string();
    if digits.len() < count.len() || digits.len() == count.len() && digits <= count.as_str() {
        return Ok(number(0));
    }
    let mut result = digits.as_bytes().to_vec();
    let mut borrow = 0i16;
    let mut sub = count.bytes().rev();
    for place in result.iter_mut().rev() {
        let d = i16::from(*place - b'0')
            - i16::from(sub.next().map(|b| b - b'0').unwrap_or(0))
            - borrow;
        if d < 0 {
            *place = (d + 10) as u8 + b'0';
            borrow = 1;
        } else {
            *place = d as u8 + b'0';
            borrow = 0;
        }
    }
    let text = String::from_utf8(result).map_err(error)?;
    Ok(Value::Number(text.trim_start_matches('0').into()))
}
