use crate::runtime::{self as r, Result};
use serde_json::Value;
use std::path::Path;

pub(crate) fn toml(raw: &[u8]) -> Result<Value> {
    fn convert(v: oms_toml::Value) -> Result<Value> {
        use oms_toml::Value as T;
        Ok(match v {
            T::String(s) | T::Datetime(s) => Value::String(s),
            T::Integer(n) => n.into(),
            T::Float(n) => serde_json::Number::from_f64(n)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            T::Boolean(b) => b.into(),
            T::Array(a) => Value::Array(a.into_iter().map(convert).collect::<Result<_>>()?),
            T::Table(t) => Value::Object(
                t.into_iter()
                    .map(|(k, v)| Ok((k, convert(v)?)))
                    .collect::<Result<_>>()?,
            ),
        })
    }
    let table = oms_toml::parse_bounded(raw, r::LIMIT)
        .map_err(|_| r::Error(2, "invalid or oversized TOML configuration".into()))?;
    convert(oms_toml::Value::Table(table))
}

pub(crate) fn read_toml(path: &Path) -> Result<Value> {
    toml(&r::read(path, r::LIMIT)?)
}
