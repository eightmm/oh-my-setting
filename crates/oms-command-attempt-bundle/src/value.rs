use oms_context::Json;
use oms_json::{PyString, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum V {
    Null,
    Bool(bool),
    Num(String),
    Str(Vec<u8>),
    Arr(Vec<V>),
    Obj(BTreeMap<Vec<u8>, V>),
}
pub(crate) static NULL: V = V::Null;
impl V {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, String> {
        fn convert(v: &Json) -> V {
            match v {
                Json::Null => V::Null,
                Json::Bool(b) => V::Bool(*b),
                Json::Number(n) => V::Num(if n == "-0" { "0".into() } else { n.clone() }),
                Json::String(s) => V::Str(s.clone()),
                Json::Array(a) => V::Arr(a.iter().map(convert).collect()),
                Json::Object(o) => V::Obj(o.iter().map(|(k, v)| (k.clone(), convert(v))).collect()),
            }
        }
        if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
            return Err("invalid JSON".into());
        }
        oms_context::parse_session_json(bytes)
            .map(|v| convert(&v))
            .map_err(|_| "invalid JSON".into())
    }
    pub(crate) fn get(&self, key: &str) -> &Self {
        match self {
            V::Obj(m) => m.get(key.as_bytes()).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub(crate) fn has(&self, key: &str) -> bool {
        matches!(self, V::Obj(m) if m.contains_key(key.as_bytes()))
    }
    pub(crate) fn set(&mut self, key: &str, value: impl Into<V>) {
        if let V::Obj(m) = self {
            m.insert(key.as_bytes().to_vec(), value.into());
        }
    }
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            V::Str(s) => std::str::from_utf8(s).ok(),
            _ => None,
        }
    }
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        if let V::Str(s) = self { Some(s) } else { None }
    }
    pub(crate) fn array(&self) -> &[V] {
        if let V::Arr(a) = self { a } else { &[] }
    }
    pub(crate) fn truth(&self) -> bool {
        match self {
            V::Null => false,
            V::Bool(b) => *b,
            V::Num(n) => {
                if n.contains(['.', 'e', 'E']) {
                    n.parse::<f64>() != Ok(0.0)
                } else {
                    n.trim_start_matches('-').bytes().any(|b| b != b'0')
                }
            }
            V::Str(s) => !s.is_empty(),
            V::Arr(a) => !a.is_empty(),
            V::Obj(o) => !o.is_empty(),
        }
    }
    pub(crate) fn or(&self, other: impl Into<V>) -> V {
        if self.truth() {
            self.clone()
        } else {
            other.into()
        }
    }
    pub(crate) fn default(&self, key: &str, value: impl Into<V>) -> V {
        if self.has(key) {
            self.get(key).clone()
        } else {
            value.into()
        }
    }
    pub(crate) fn integer(&self, positive: bool) -> bool {
        matches!(self,V::Num(n) if !n.contains(['.','e','E']) && n.bytes().all(|b| b.is_ascii_digit()) && (!positive || n != "0"))
    }
    pub(crate) fn py(&self) -> String {
        match self {
            V::Null => "None".into(),
            V::Bool(true) => "True".into(),
            V::Bool(false) => "False".into(),
            V::Num(n) => {
                if n == "NaN" {
                    "nan".into()
                } else if n == "Infinity" {
                    "inf".into()
                } else if n == "-Infinity" {
                    "-inf".into()
                } else if n.contains(['.', 'e', 'E']) {
                    let f = n.parse::<f64>().unwrap_or(f64::NAN);
                    if f.is_infinite() {
                        if f.is_sign_negative() {
                            "-inf".into()
                        } else {
                            "inf".into()
                        }
                    } else {
                        self.json(false)
                            .ok()
                            .and_then(|b| String::from_utf8(b).ok())
                            .unwrap_or_else(|| n.clone())
                    }
                } else {
                    n.clone()
                }
            }
            V::Str(s) => String::from_utf8_lossy(s).into_owned(),
            V::Arr(a) => format!("[{}]", a.iter().map(V::repr).collect::<Vec<_>>().join(", ")),
            V::Obj(o) => format!(
                "{{{}}}",
                o.iter()
                    .map(|(k, v)| format!("{}: {}", V::Str(k.clone()).repr(), v.repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    pub(crate) fn repr(&self) -> String {
        let V::Str(bytes) = self else {
            return self.py();
        };
        use unicode_general_category::{GeneralCategory as C, get_general_category};
        let quote = if bytes.contains(&b'\'') && !bytes.contains(&b'"') {
            '"'
        } else {
            '\''
        };
        let mut out = String::from(quote);
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            let len = if b < 128 {
                1
            } else if b < 224 {
                2
            } else if b < 240 {
                3
            } else {
                4
            };
            let mut cp = (b & [0, 127, 31, 15, 7][len]) as u32;
            for b in &bytes[i + 1..i + len] {
                cp = (cp << 6) | (b & 63) as u32;
            }
            i += len;
            match char::from_u32(cp) {
                Some('\\') => out.push_str("\\\\"),
                Some('\n') => out.push_str("\\n"),
                Some('\r') => out.push_str("\\r"),
                Some('\t') => out.push_str("\\t"),
                Some(c) if c == quote => {
                    out.push('\\');
                    out.push(c);
                }
                Some(c)
                    if c == ' '
                        || !matches!(
                            get_general_category(c),
                            C::Control
                                | C::Format
                                | C::PrivateUse
                                | C::Surrogate
                                | C::Unassigned
                                | C::LineSeparator
                                | C::ParagraphSeparator
                                | C::SpaceSeparator
                        ) =>
                {
                    out.push(c)
                }
                _ => out.push_str(&if cp <= 255 {
                    format!("\\x{cp:02x}")
                } else if cp <= 65535 {
                    format!("\\u{cp:04x}")
                } else {
                    format!("\\U{cp:08x}")
                }),
            }
        }
        out.push(quote);
        out
    }
    fn codec(&self) -> Result<Value, String> {
        fn string(s: &[u8]) -> Result<PyString, String> {
            let mut points = Vec::new();
            let mut i = 0;
            while i < s.len() {
                let b = s[i];
                let len = if b < 128 {
                    1
                } else if b < 224 {
                    2
                } else if b < 240 {
                    3
                } else {
                    4
                };
                if i + len > s.len() {
                    return Err("invalid string".into());
                }
                let mut cp = (b & [0, 127, 31, 15, 7][len]) as u32;
                for p in &s[i + 1..i + len] {
                    cp = (cp << 6) | (p & 63) as u32;
                }
                points.push(cp);
                i += len;
            }
            PyString::from_codepoints(points).map_err(|_| "invalid string".into())
        }
        Ok(match self {
            V::Null => Value::Null,
            V::Bool(b) => Value::Bool(*b),
            V::Num(n) => {
                if n.contains(['.', 'e', 'E'])
                    || matches!(n.as_str(), "NaN" | "Infinity" | "-Infinity")
                {
                    Value::Float(n.parse().map_err(|_| "invalid number")?)
                } else {
                    Value::integer(n).map_err(|_| "invalid integer")?
                }
            }
            V::Str(s) => Value::String(string(s)?),
            V::Arr(a) => Value::Array(a.iter().map(V::codec).collect::<Result<_, _>>()?),
            V::Obj(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| Ok((string(k)?, v.codec()?)))
                    .collect::<Result<_, String>>()?,
            ),
        })
    }
    pub(crate) fn json(&self, pretty: bool) -> Result<Vec<u8>, String> {
        let v = self.codec()?;
        if pretty {
            oms_json::pretty_json(&v)
        } else {
            oms_json::agent_event_json_bytes(&v)
        }
        .map_err(|_| "cannot encode bundle".into())
    }
    pub(crate) fn default_json(&self, ascii: bool) -> Result<String, String> {
        let raw = String::from_utf8(self.json(false)?).map_err(|_| "cannot encode bundle")?;
        let mut out = String::new();
        let mut quoted = false;
        let mut escaped = false;
        for c in raw.chars() {
            if quoted {
                if ascii && !c.is_ascii() {
                    for n in c.encode_utf16(&mut [0; 2]) {
                        out.push_str(&format!("\\u{n:04x}"));
                    }
                } else {
                    out.push(c);
                }
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    quoted = false;
                }
            } else {
                out.push(c);
                if c == '"' {
                    quoted = true;
                } else if matches!(c, ',' | ':') {
                    out.push(' ');
                }
            }
        }
        Ok(out)
    }
    pub(crate) fn short(&self, limit: usize) -> V {
        match self {
            V::Str(s) => {
                let end = s
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| **b & 0xc0 != 0x80)
                    .nth(limit)
                    .map_or(s.len(), |(i, _)| i);
                V::Str(s[..end].to_vec())
            }
            V::Arr(a) => V::Arr(a.iter().take(16).map(|v| v.short(80)).collect()),
            V::Obj(_) => V::Null,
            _ => self.clone(),
        }
    }
}
impl From<&str> for V {
    fn from(s: &str) -> V {
        V::Str(s.as_bytes().to_vec())
    }
}
impl From<String> for V {
    fn from(s: String) -> V {
        V::Str(s.into_bytes())
    }
}
impl From<bool> for V {
    fn from(b: bool) -> V {
        V::Bool(b)
    }
}
impl From<usize> for V {
    fn from(n: usize) -> V {
        V::Num(n.to_string())
    }
}
impl From<i64> for V {
    fn from(n: i64) -> V {
        V::Num(n.to_string())
    }
}
pub(crate) fn obj(items: impl IntoIterator<Item = (&'static str, V)>) -> V {
    V::Obj(
        items
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}
pub(crate) fn add(a: &str, b: &str) -> String {
    let mut carry = 0;
    let mut out = Vec::new();
    let mut aa = a.bytes().rev();
    let mut bb = b.bytes().rev();
    loop {
        let x = aa.next();
        let y = bb.next();
        if x.is_none() && y.is_none() && carry == 0 {
            break;
        }
        let n = x.map_or(0, |x| x - b'0') + y.map_or(0, |y| y - b'0') + carry;
        out.push(b'0' + n % 10);
        carry = n / 10;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncation_and_python_scalar_rendering() {
        for n in [0, 80, 200] {
            let value = V::from("한".repeat(n + 1));
            assert_eq!(value.short(n), V::from("한".repeat(n)));
        }
        assert_eq!(
            V::parse(br#"[true, null, {"a": "x"}]"#).unwrap().py(),
            "[True, None, {'a': 'x'}]"
        );
        assert_eq!(add(&"9".repeat(100), "1"), format!("1{}", "0".repeat(100)));
    }
}
