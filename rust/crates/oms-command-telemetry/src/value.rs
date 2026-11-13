use oms_context::Json;
use std::collections::BTreeMap;
use std::ops::{Index, IndexMut};

#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) enum V {
    #[default]
    Null,
    Bool(bool),
    Int(String),
    Float(f64),
    Str(Vec<u8>),
    Array(Vec<V>),
    Object(BTreeMap<Vec<u8>, V>),
}

macro_rules! obj {
    ($($key:literal : $value:expr),* $(,)?) => {
        $crate::value::object([$(( $key, $crate::value::V::from($value) )),*])
    };
}
pub(crate) use obj;

pub(crate) fn object<const N: usize>(items: [(&str, V); N]) -> V {
    V::Object(
        items
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}
impl From<&str> for V {
    fn from(v: &str) -> Self {
        Self::Str(v.as_bytes().to_vec())
    }
}
impl From<String> for V {
    fn from(v: String) -> Self {
        Self::Str(v.into_bytes())
    }
}
impl From<Vec<u8>> for V {
    fn from(v: Vec<u8>) -> Self {
        Self::Str(v)
    }
}
impl From<Vec<V>> for V {
    fn from(v: Vec<V>) -> Self {
        Self::Array(v)
    }
}
impl From<bool> for V {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl From<usize> for V {
    fn from(v: usize) -> Self {
        Self::Int(v.to_string())
    }
}
impl From<i32> for V {
    fn from(v: i32) -> Self {
        Self::Int(v.to_string())
    }
}
impl From<f64> for V {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}
impl<T: Into<V>> From<Option<T>> for V {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Self::Null)
    }
}
impl Index<&str> for V {
    type Output = V;
    fn index(&self, key: &str) -> &V {
        self.get(key).unwrap_or(&V::Null)
    }
}
impl IndexMut<&str> for V {
    fn index_mut(&mut self, key: &str) -> &mut V {
        if let Self::Object(fields) = self {
            fields.entry(key.as_bytes().to_vec()).or_default()
        } else {
            panic!("report field must be an object")
        }
    }
}
impl V {
    pub(crate) fn get(&self, key: &str) -> Option<&Self> {
        if let Self::Object(m) = self {
            m.get(key.as_bytes())
        } else {
            None
        }
    }
    pub(crate) fn text(&self) -> Option<&str> {
        if let Self::Str(s) = self {
            std::str::from_utf8(s).ok()
        } else {
            None
        }
    }
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        if let Self::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub(crate) fn array(&self) -> &[Self] {
        if let Self::Array(a) = self { a } else { &[] }
    }
    pub(crate) fn int(&self) -> Option<&str> {
        if let Self::Int(n) = self {
            if !n.starts_with('-') { Some(n) } else { None }
        } else {
            None
        }
    }
    pub(crate) fn count(&self) -> usize {
        self.int().and_then(|s| s.parse().ok()).unwrap_or(0)
    }
    pub(crate) fn number(&self) -> Option<f64> {
        match self {
            Self::Int(s) => s.parse().ok(),
            Self::Float(n) => Some(*n),
            _ => None,
        }
    }
    pub(crate) fn truth(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Int(s) => s != "0",
            Self::Float(n) => *n != 0.0,
            Self::Str(s) => !s.is_empty(),
            Self::Array(a) => !a.is_empty(),
            Self::Object(m) => !m.is_empty(),
        }
    }
    pub(crate) fn inc(&mut self, key: &str) {
        self.add(key, &Self::from(1));
    }
    pub(crate) fn add(&mut self, key: &str, amount: &Self) {
        let left = self[key].int().unwrap_or("0");
        let right = amount.int().unwrap_or("0");
        self[key] = Self::Int(add_decimal(left, right));
    }
    pub(crate) fn remove(&mut self, key: &str) -> Self {
        if let Self::Object(m) = self {
            m.remove(key.as_bytes()).unwrap_or_default()
        } else {
            Self::Null
        }
    }
}
fn add_decimal(a: &str, b: &str) -> String {
    let mut carry = 0;
    let mut result = Vec::new();
    let mut a = a.bytes().rev();
    let mut b = b.bytes().rev();
    loop {
        let x = a.next();
        let y = b.next();
        if x.is_none() && y.is_none() && carry == 0 {
            break;
        }
        let n = x.map_or(0, |v| v - b'0') + y.map_or(0, |v| v - b'0') + carry;
        result.push(b'0' + n % 10);
        carry = n / 10;
    }
    if result.is_empty() {
        result.push(b'0');
    }
    result.reverse();
    String::from_utf8(result).unwrap()
}

pub(crate) fn parse(raw: &[u8], finite_constants: bool) -> Result<V, ()> {
    if raw.starts_with(&[0xef, 0xbb, 0xbf]) || raw.contains(&0) || std::str::from_utf8(raw).is_err()
    {
        return Err(());
    }
    if finite_constants {
        // Validate constants before duplicate-key reduction: even an overwritten
        // NaN must be refused by the index store's parse_constant policy.
        let mut quoted = false;
        let mut escaped = false;
        for (i, b) in raw.iter().enumerate() {
            if quoted {
                if escaped {
                    escaped = false;
                } else if *b == b'\\' {
                    escaped = true;
                } else if *b == b'"' {
                    quoted = false;
                }
            } else if *b == b'"' {
                quoted = true;
            } else if raw[i..].starts_with(b"NaN")
                || raw[i..].starts_with(b"Infinity")
                || raw[i..].starts_with(b"-Infinity")
            {
                return Err(());
            }
        }
    }
    let parsed = oms_context::parse_session_json(raw).map_err(|_| ())?;
    fn convert(v: &Json, finite: bool) -> Result<V, ()> {
        Ok(match v {
            Json::Null => V::Null,
            Json::Bool(b) => V::Bool(*b),
            Json::String(s) => V::Str(s.clone()),
            Json::Number(n) if ["NaN", "Infinity", "-Infinity"].contains(&n.as_str()) => {
                if finite {
                    return Err(());
                }
                V::Float(n.parse().map_err(|_| ())?)
            }
            Json::Number(n) if !n.contains(['.', 'e', 'E']) => {
                V::Int(if n == "-0" { "0".into() } else { n.clone() })
            }
            Json::Number(n) => V::Float(n.parse().map_err(|_| ())?),
            Json::Array(a) => V::Array(
                a.iter()
                    .map(|v| convert(v, finite))
                    .collect::<Result<_, _>>()?,
            ),
            Json::Object(m) => V::Object(
                m.iter()
                    .map(|(k, v)| Ok((k.clone(), convert(v, finite)?)))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
    convert(&parsed, finite_constants)
}

pub(crate) fn encode(value: &V) -> Result<Vec<u8>, String> {
    use oms_json::{PyString, Value};
    fn convert(v: &V) -> Result<Value, String> {
        Ok(match v {
            V::Null => Value::Null,
            V::Bool(b) => Value::Bool(*b),
            V::Int(n) => Value::integer(n).map_err(|_| "artifact telemetry: invalid integer")?,
            V::Float(n) => Value::Float(*n),
            V::Str(s) => Value::String(PyString::from(
                std::str::from_utf8(s)
                    .map_err(|_| "artifact telemetry: cannot encode surrogate text")?,
            )),
            V::Array(a) => Value::Array(a.iter().map(convert).collect::<Result<_, _>>()?),
            V::Object(m) => {
                Value::Object(
                    m.iter()
                        .map(|(k, v)| {
                            Ok((
                                PyString::from(std::str::from_utf8(k).map_err(
                                    |_| "artifact telemetry: cannot encode surrogate text",
                                )?),
                                convert(v)?,
                            ))
                        })
                        .collect::<Result<_, String>>()?,
                )
            }
        })
    }
    let compact = oms_json::session_call_key_json_bytes(&convert(value)?)
        .map_err(|_| "artifact telemetry: cannot encode report")?;
    // Python json.dumps uses a space after separators, except inside strings.
    let mut out = Vec::with_capacity(compact.len());
    let mut quoted = false;
    let mut escaped = false;
    for b in compact {
        out.push(b);
        if quoted {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                quoted = false;
            }
        } else if b == b'"' {
            quoted = true;
        } else if b == b',' || b == b':' {
            out.push(b' ');
        }
    }
    out.push(b'\n');
    Ok(out)
}
pub(crate) fn display(v: &V) -> String {
    match v {
        V::Null => "None".into(),
        V::Int(n) => n.clone(),
        V::Float(_) => {
            let s = encode(v).unwrap();
            String::from_utf8(s[..s.len() - 1].to_vec()).unwrap()
        }
        V::Str(s) => String::from_utf8_lossy(s).into(),
        _ => "".into(),
    }
}
pub(crate) fn rounded(n: f64, places: usize) -> f64 {
    if n.is_finite() {
        format!("{n:.places$}").parse().unwrap_or(n)
    } else {
        n
    }
}

// Decimal long division avoids converting large recorded integers to infinity
// before division. Every binary64 midpoint terminates within 1,075 fractional
// decimal places; the extra digit records a nonzero discarded remainder.
pub(crate) fn ratio(numerator: &V, denominator: &V) -> Result<f64, String> {
    let a = numerator.int().unwrap();
    let b = denominator.int().unwrap();
    if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
        return Ok(f64::from(a) / f64::from(b));
    }
    fn subtract(a: &str, b: &str) -> String {
        let mut result = Vec::with_capacity(a.len());
        let mut borrow = 0i16;
        let mut rhs = b.bytes().rev();
        for x in a.bytes().rev() {
            let n = i16::from(x - b'0') - rhs.next().map_or(0, |v| i16::from(v - b'0')) - borrow;
            borrow = i16::from(n < 0);
            result.push(b'0' + (if n < 0 { n + 10 } else { n }) as u8);
        }
        result.reverse();
        let result = String::from_utf8(result).unwrap();
        let digits = result.trim_start_matches('0');
        if digits.is_empty() {
            "0".into()
        } else {
            digits.into()
        }
    }
    fn next(remainder: &mut String, digit: u8, divisor: &str) -> u8 {
        if remainder == "0" {
            remainder.clear();
        }
        remainder.push(char::from(digit));
        let mut n = 0;
        while remainder.len() > divisor.len()
            || remainder.len() == divisor.len() && remainder.as_str() >= divisor
        {
            *remainder = subtract(remainder, divisor);
            n += 1;
        }
        n
    }
    let mut remainder = String::from("0");
    let mut output = String::new();
    for d in a.bytes() {
        output.push(char::from(b'0' + next(&mut remainder, d, b)));
    }
    output.push('.');
    for _ in 0..1100 {
        if remainder == "0" {
            break;
        }
        output.push(char::from(b'0' + next(&mut remainder, b'0', b)));
    }
    if remainder != "0" {
        output.push('1');
    }
    let value = output
        .parse::<f64>()
        .map_err(|_| "artifact telemetry: recorded number is out of range")?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err("artifact telemetry: recorded number is out of range".into())
    }
}

pub(crate) fn equal(left: &V, right: &V) -> bool {
    fn numeric(left: &V, right: &V) -> Option<bool> {
        match (left, right) {
            (V::Bool(a), V::Int(b)) | (V::Int(b), V::Bool(a)) => {
                Some(b == if *a { "1" } else { "0" })
            }
            (V::Bool(a), V::Float(b)) | (V::Float(b), V::Bool(a)) => {
                Some(*b == if *a { 1.0 } else { 0.0 })
            }
            (V::Int(a), V::Float(b)) | (V::Float(b), V::Int(a)) => Some(
                b.is_finite()
                    && b.fract() == 0.0
                    && (if *b == 0.0 {
                        a == "0"
                    } else {
                        a == &format!("{b:.0}")
                    }),
            ),
            _ => None,
        }
    }
    if let Some(equal) = numeric(left, right) {
        return equal;
    }
    match (left, right) {
        (V::Array(a), V::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (V::Object(a), V::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|other| equal(v, other)))
        }
        _ => left == right,
    }
}
