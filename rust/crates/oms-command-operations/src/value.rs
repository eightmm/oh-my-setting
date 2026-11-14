use oms_context::Json;
use oms_json::{PyString, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum V {
    Null,
    Bool(bool),
    Num(String),
    Str(Vec<u8>),
    Arr(Vec<V>),
    Obj(BTreeMap<Vec<u8>, V>),
}
pub static NULL: V = V::Null;
impl V {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
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
        oms_context::parse_experiment_json(bytes, usize::MAX)
            .map(|v| convert(&v))
            .map_err(|_| "invalid JSON".into())
    }
    pub fn get(&self, key: &str) -> &Self {
        match self {
            V::Obj(m) => m.get(key.as_bytes()).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub fn has(&self, key: &str) -> bool {
        matches!(self, V::Obj(m) if m.contains_key(key.as_bytes()))
    }
    pub fn set(&mut self, key: &str, value: impl Into<V>) {
        if let V::Obj(m) = self {
            m.insert(key.as_bytes().to_vec(), value.into());
        }
    }
    pub fn text(&self) -> Option<&str> {
        match self {
            V::Str(s) => std::str::from_utf8(s).ok(),
            _ => None,
        }
    }
    pub fn bytes(&self) -> Option<&[u8]> {
        if let V::Str(s) = self { Some(s) } else { None }
    }
    pub fn array(&self) -> &[V] {
        if let V::Arr(a) = self { a } else { &[] }
    }
    pub fn truth(&self) -> bool {
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
    pub fn or(&self, other: impl Into<V>) -> V {
        if self.truth() {
            self.clone()
        } else {
            other.into()
        }
    }
    pub fn default(&self, key: &str, value: impl Into<V>) -> V {
        if self.has(key) {
            self.get(key).clone()
        } else {
            value.into()
        }
    }
    pub fn integer(&self, positive: bool) -> bool {
        matches!(self,V::Num(n) if !n.contains(['.','e','E']) && n.bytes().all(|b| b.is_ascii_digit()) && (!positive || n != "0"))
    }
    pub fn py(&self) -> String {
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
    pub fn repr(&self) -> String {
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
    pub fn json(&self, pretty: bool) -> Result<Vec<u8>, String> {
        let v = self.codec()?;
        if pretty {
            oms_json::pretty_json(&v)
        } else {
            oms_json::agent_event_json_bytes(&v)
        }
        .map_err(|_| "cannot encode operations JSON".into())
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
pub fn obj(items: impl IntoIterator<Item = (&'static str, V)>) -> V {
    V::Obj(
        items
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}
pub fn add(a: &str, b: &str) -> String {
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

impl PartialEq for V {
    fn eq(&self, other: &Self) -> bool {
        fn numeric(v: &V) -> Option<String> {
            match v {
                V::Bool(b) => Some(usize::from(*b).to_string()),
                V::Num(n) => Some(n.clone()),
                _ => None,
            }
        }
        if let (Some(a), Some(b)) = (numeric(self), numeric(other)) {
            let float = |s: &str| {
                s.contains(['.', 'e', 'E']) || matches!(s, "NaN" | "Infinity" | "-Infinity")
            };
            return match (float(&a), float(&b)) {
                (false, false) => a == b,
                (true, true) => a
                    .parse::<f64>()
                    .ok()
                    .zip(b.parse::<f64>().ok())
                    .is_some_and(|(a, b)| a == b),
                (true, false) | (false, true) => {
                    let (decimal, integer) = if float(&a) { (&a, &b) } else { (&b, &a) };
                    decimal.parse::<f64>().is_ok_and(|f| {
                        f.is_finite() && f.fract() == 0.0 && format!("{f:.0}") == *integer
                    })
                }
            };
        }
        match (self, other) {
            (V::Null, V::Null) => true,
            (V::Str(a), V::Str(b)) => a == b,
            (V::Arr(a), V::Arr(b)) => a == b,
            (V::Obj(a), V::Obj(b)) => a == b,
            _ => false,
        }
    }
}

/// Python's int grammar for CLI and scalar budget inputs, including decimal
/// Unicode digits and underscores between digits. Output is canonical decimal.
pub fn parse_integer(raw: &str) -> Result<V, String> {
    use unicode_general_category::{GeneralCategory, get_general_category};
    fn digit(c: char) -> Option<u8> {
        if c.is_ascii_digit() {
            return Some(c as u8 - b'0');
        }
        if get_general_category(c) != GeneralCategory::DecimalNumber {
            return None;
        }
        let mut first = c as u32;
        while first > 0
            && char::from_u32(first - 1)
                .is_some_and(|c| get_general_category(c) == GeneralCategory::DecimalNumber)
        {
            first -= 1;
        }
        Some(((c as u32 - first) % 10) as u8)
    }
    let raw =
        raw.trim_matches(|c: char| c.is_whitespace() && !(('\u{1c}'..='\u{1f}').contains(&c)));
    let (negative, body) = raw.strip_prefix('-').map_or_else(
        || (false, raw.strip_prefix('+').unwrap_or(raw)),
        |s| (true, s),
    );
    let mut digits = String::new();
    let mut previous = false;
    for c in body.chars() {
        if let Some(n) = digit(c) {
            digits.push((b'0' + n) as char);
            previous = true;
        } else if c == '_' && previous {
            previous = false;
        } else {
            return Err("invalid integer".into());
        }
    }
    if !previous || digits.len() > 4300 {
        return Err("invalid integer".into());
    }
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    Ok(V::Num(if negative && digits != "0" {
        format!("-{digits}")
    } else {
        digits.into()
    }))
}

pub fn integer_value(value: &V) -> Result<V, String> {
    match value {
        V::Bool(b) => Ok(V::Num(usize::from(*b).to_string())),
        V::Num(n)
            if n.contains(['.', 'e', 'E'])
                || matches!(n.as_str(), "NaN" | "Infinity" | "-Infinity") =>
        {
            let f = n.parse::<f64>().map_err(|_| "invalid integer")?;
            if !f.is_finite() {
                return Err("invalid integer".into());
            }
            parse_integer(&format!("{:.0}", f.trunc()))
        }
        V::Num(n) => parse_integer(n),
        V::Str(_) => parse_integer(&value.py()),
        _ => Err("invalid integer".into()),
    }
}

/// Compact Python JSON while retaining input object insertion order, as follow
/// does for manually authored as well as canonical (sorted) event records.
pub fn ordered_json(value: &Json) -> Result<Vec<u8>, String> {
    fn encode(value: &Json, out: &mut Vec<u8>) -> Result<(), String> {
        match value {
            Json::Object(fields) => {
                out.push(b'{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    out.extend(V::Str(key.clone()).json(false)?);
                    out.push(b':');
                    encode(value, out)?;
                }
                out.push(b'}');
            }
            Json::Array(values) => {
                out.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    encode(value, out)?;
                }
                out.push(b']');
            }
            Json::Null => out.extend(b"null"),
            Json::Bool(true) => out.extend(b"true"),
            Json::Bool(false) => out.extend(b"false"),
            Json::String(raw) => out.extend(V::Str(raw.clone()).json(false)?),
            Json::Number(raw) => {
                out.extend(V::Num(if raw == "-0" { "0".into() } else { raw.clone() }).json(false)?)
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    encode(value, &mut out)?;
    Ok(out)
}
