//! MCP's Python string JSON domain. Parsing belongs to oms-context; this adapter
//! retains surrogate code points through field validation and response encoding.
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::ops::{Index, IndexMut};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

pub const DEPTH: usize = 9997;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Text(Vec<u8>);
impl Text {
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn points(&self) -> impl Iterator<Item = u32> + '_ {
        let mut at = 0;
        std::iter::from_fn(move || {
            let first = *self.0.get(at)?;
            at += 1;
            let count = if first < 128 {
                0
            } else if first < 224 {
                1
            } else if first < 240 {
                2
            } else {
                3
            };
            let mut n = u32::from(first & [127, 31, 15, 7][count]);
            for _ in 0..count {
                n = (n << 6) | u32::from(self.0[at] & 63);
                at += 1;
            }
            Some(n)
        })
    }
    pub fn prefix(&self, count: usize) -> Self {
        Self::from_points(self.points().take(count))
    }
    fn from_points(points: impl IntoIterator<Item = u32>) -> Self {
        let mut out = Vec::new();
        for n in points {
            if let Some(c) = char::from_u32(n) {
                let mut bytes = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut bytes).as_bytes());
            } else {
                out.extend_from_slice(&[
                    0xe0 | (n >> 12) as u8,
                    0x80 | ((n >> 6) & 63) as u8,
                    0x80 | (n & 63) as u8,
                ]);
            }
        }
        Self(out)
    }
    pub fn path(path: &OsStr) -> Self {
        let mut rest = path.as_bytes();
        let mut out = Vec::new();
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(_) => {
                    out.extend_from_slice(rest);
                    break;
                }
                Err(e) => {
                    out.extend_from_slice(&rest[..e.valid_up_to()]);
                    rest = &rest[e.valid_up_to()..];
                    let count = e.error_len().unwrap_or(rest.len());
                    for &b in &rest[..count] {
                        out.extend_from_slice(&[0xed, 0xb0 | ((b >> 6) & 3), 0x80 | (b & 63)]);
                    }
                    rest = &rest[count..];
                }
            }
        }
        Self(out)
    }
    pub(crate) fn join(parts: &[Text]) -> Self {
        let mut bytes = Vec::new();
        for part in parts {
            bytes.extend_from_slice(&part.0);
        }
        Self(bytes)
    }
    pub fn os_string(&self) -> Result<OsString, &'static str> {
        let mut bytes = Vec::new();
        for n in self.points() {
            if (0xdc80..=0xdcff).contains(&n) {
                bytes.push((n - 0xdc00) as u8);
            } else if let Some(c) = char::from_u32(n) {
                let mut b = [0; 4];
                bytes.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            } else {
                return Err("invalid filesystem string");
            }
        }
        Ok(OsString::from_vec(bytes))
    }
}
impl From<&str> for Text {
    fn from(v: &str) -> Self {
        Self(v.as_bytes().to_vec())
    }
}
impl From<String> for Text {
    fn from(v: String) -> Self {
        Self(v.into_bytes())
    }
}
impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(s) = self.as_str() {
            f.write_str(s)
        } else {
            for n in self.points() {
                if let Some(c) = char::from_u32(n) {
                    write!(f, "{c}")?;
                } else {
                    write!(f, "\\u{n:04x}")?;
                }
            }
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Number {
    spelling: String,
    integer: bool,
}
impl Number {
    fn parse(raw: &str) -> Self {
        if !raw.contains(['.', 'e', 'E']) && !matches!(raw, "NaN" | "Infinity" | "-Infinity") {
            return Self {
                spelling: if raw == "-0" { "0".into() } else { raw.into() },
                integer: true,
            };
        }
        let n = match raw {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            _ => raw.parse().expect("validated float"),
        };
        Self {
            spelling: float(n),
            integer: false,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        self.spelling.parse().ok()
    }
    pub fn as_i64(&self) -> Option<i64> {
        self.integer.then(|| self.spelling.parse().ok()).flatten()
    }
    pub fn as_u64(&self) -> Option<u64> {
        self.integer.then(|| self.spelling.parse().ok()).flatten()
    }
    pub fn repr(&self) -> &str {
        match self.spelling.as_str() {
            "NaN" => "nan",
            "Infinity" => "inf",
            "-Infinity" => "-inf",
            s => s,
        }
    }
}
impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.spelling)
    }
}
fn float(n: f64) -> String {
    if !n.is_finite() {
        return if n.is_nan() {
            "NaN"
        } else if n.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    }
    let mut buffer = ryu::Buffer::new();
    let raw = buffer.format_finite(n);
    let (mantissa, exponent) = raw
        .split_once('e')
        .map_or((raw, 0), |(m, e)| (m, e.parse::<i32>().unwrap()));
    let negative = mantissa.starts_with('-');
    let m = mantissa.trim_start_matches('-');
    let digits: String = m.chars().filter(|c| *c != '.').collect();
    let before = m.find('.').unwrap_or(m.len()) as i32;
    let Some(first) = digits.find(|c| c != '0') else {
        return if negative { "-0.0" } else { "0.0" }.into();
    };
    let exp = exponent + before - first as i32 - 1;
    let significant = digits[first..].trim_end_matches('0');
    let sign = if negative { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let mut s = format!("{sign}{}", &significant[..1]);
        if significant.len() > 1 {
            s.push('.');
            s.push_str(&significant[1..]);
        }
        s.push_str(&format!(
            "e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.unsigned_abs()
        ));
        s
    } else {
        let pos = exp + 1;
        let mut s = sign.to_owned();
        if pos <= 0 {
            s.push_str("0.");
            s.push_str(&"0".repeat((-pos) as usize));
            s.push_str(significant);
        } else if pos as usize >= significant.len() {
            s.push_str(significant);
            s.push_str(&"0".repeat(pos as usize - significant.len()));
            s.push_str(".0");
        } else {
            s.push_str(&significant[..pos as usize]);
            s.push('.');
            s.push_str(&significant[pos as usize..]);
        }
        s
    }
}

#[derive(Debug, Default)]
pub struct Object(Vec<(Text, Value)>);
impl Object {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
    }
    pub fn insert(&mut self, key: Text, value: Value) {
        if let Some((_, v)) = self.0.iter_mut().find(|(k, _)| *k == key) {
            *v = value;
        } else {
            self.0.push((key, value));
        }
    }
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let i = self.0.iter().position(|(k, _)| k.as_str() == Some(key))?;
        Some(self.0.remove(i).1)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&Text, &Value)> {
        self.0.iter().map(|(k, v)| (k, v))
    }
    pub fn keys(&self) -> impl Iterator<Item = &Text> {
        self.0.iter().map(|(k, _)| k)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl Index<&Text> for Object {
    type Output = Value;
    fn index(&self, key: &Text) -> &Value {
        &self.0.iter().find(|(k, _)| k == key).expect("known key").1
    }
}
impl PartialEq for Object {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().all(|(k, v)| {
                other
                    .0
                    .iter()
                    .find(|(key, _)| key == k)
                    .is_some_and(|(_, value)| v == value)
            })
    }
}

#[derive(Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(Text),
    Array(Vec<Value>),
    Object(Object),
}
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        let mut pending = vec![(self, other)];
        while let Some((a, b)) = pending.pop() {
            match (a, b) {
                (Self::Null, Self::Null) => (),
                (Self::Bool(a), Self::Bool(b)) if a == b => (),
                (Self::Number(a), Self::Number(b)) if a == b => (),
                (Self::String(a), Self::String(b)) if a == b => (),
                (Self::Array(a), Self::Array(b)) if a.len() == b.len() => {
                    pending.extend(a.iter().zip(b))
                }
                (Self::Object(a), Self::Object(b)) if a.0.len() == b.0.len() => {
                    for (k, v) in &a.0 {
                        let Some((_, other)) = b.0.iter().find(|(key, _)| key == k) else {
                            return false;
                        };
                        pending.push((v, other));
                    }
                }
                _ => return false,
            }
        }
        true
    }
}
impl Drop for Value {
    fn drop(&mut self) {
        fn drain(v: &mut Value, pending: &mut Vec<Value>) {
            match v {
                Value::Array(a) => pending.append(a),
                Value::Object(o) => pending.extend(o.0.drain(..).map(|(_, v)| v)),
                _ => (),
            }
        }
        let mut pending = Vec::new();
        drain(self, &mut pending);
        while let Some(mut v) = pending.pop() {
            drain(&mut v, &mut pending);
        }
    }
}
impl Clone for Value {
    fn clone(&self) -> Self {
        enum Frame<'a> {
            Node(&'a Value),
            Array(usize),
            Object(Vec<Text>),
        }
        let mut stack = vec![Frame::Node(self)];
        let mut out = Vec::new();
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Node(v) => match v {
                    Self::Null => out.push(Self::Null),
                    Self::Bool(v) => out.push(Self::Bool(*v)),
                    Self::Number(v) => out.push(Self::Number(v.clone())),
                    Self::String(v) => out.push(Self::String(v.clone())),
                    Self::Array(a) => {
                        stack.push(Frame::Array(a.len()));
                        stack.extend(a.iter().rev().map(Frame::Node));
                    }
                    Self::Object(o) => {
                        stack.push(Frame::Object(o.0.iter().map(|(k, _)| k.clone()).collect()));
                        stack.extend(o.0.iter().rev().map(|(_, v)| Frame::Node(v)));
                    }
                },
                Frame::Array(n) => {
                    let a = out.split_off(out.len() - n);
                    out.push(Self::Array(a));
                }
                Frame::Object(keys) => {
                    let values = out.split_off(out.len() - keys.len());
                    out.push(Self::Object(Object(keys.into_iter().zip(values).collect())));
                }
            }
        }
        out.pop().unwrap()
    }
}
impl Value {
    pub fn as_text(&self) -> Option<&Text> {
        if let Self::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        self.as_text()?.as_str()
    }
    pub fn as_i64(&self) -> Option<i64> {
        if let Self::Number(n) = self {
            n.as_i64()
        } else {
            None
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        if let Self::Number(n) = self {
            n.as_u64()
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(n) = self {
            Some(*n)
        } else {
            None
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    pub fn is_boolean(&self) -> bool {
        matches!(self, Self::Bool(_))
    }
    pub fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }
    pub fn is_array(&self) -> bool {
        matches!(self, Self::Array(_))
    }
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        if let Self::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }
    pub fn as_object(&self) -> Option<&Object> {
        if let Self::Object(o) = self {
            Some(o)
        } else {
            None
        }
    }
    pub fn as_object_mut(&mut self) -> Option<&mut Object> {
        if let Self::Object(o) = self {
            Some(o)
        } else {
            None
        }
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }
    pub fn from_serde(root: &serde_json::Value) -> Self {
        enum Frame<'a> {
            Node(&'a serde_json::Value),
            Array(usize),
            Object(Vec<Text>),
        }
        let mut stack = vec![Frame::Node(root)];
        let mut out = Vec::new();
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Node(v) => match v {
                    serde_json::Value::Null => out.push(Self::Null),
                    serde_json::Value::Bool(v) => out.push(Self::Bool(*v)),
                    serde_json::Value::Number(n) => {
                        out.push(Self::Number(Number::parse(&n.to_string())))
                    }
                    serde_json::Value::String(s) => out.push(Self::String(s.as_str().into())),
                    serde_json::Value::Array(a) => {
                        stack.push(Frame::Array(a.len()));
                        stack.extend(a.iter().rev().map(Frame::Node));
                    }
                    serde_json::Value::Object(o) => {
                        stack.push(Frame::Object(o.keys().map(|k| k.as_str().into()).collect()));
                        stack.extend(o.values().rev().map(Frame::Node));
                    }
                },
                Frame::Array(n) => {
                    let a = out.split_off(out.len() - n);
                    out.push(Self::Array(a));
                }
                Frame::Object(keys) => {
                    let a = out.split_off(out.len() - keys.len());
                    out.push(Self::Object(Object(keys.into_iter().zip(a).collect())));
                }
            }
        }
        out.pop().unwrap()
    }
}
static NULL: Value = Value::Null;
impl Index<&str> for Value {
    type Output = Self;
    fn index(&self, key: &str) -> &Self {
        self.get(key).unwrap_or(&NULL)
    }
}
impl Index<usize> for Value {
    type Output = Self;
    fn index(&self, index: usize) -> &Self {
        self.as_array().and_then(|a| a.get(index)).unwrap_or(&NULL)
    }
}
impl IndexMut<&str> for Value {
    fn index_mut(&mut self, key: &str) -> &mut Self {
        if self.is_null() {
            *self = Self::Object(Object::default());
        }
        let o = self.as_object_mut().expect("object");
        if !o.contains_key(key) {
            o.insert(key.into(), Self::Null);
        }
        &mut o
            .0
            .iter_mut()
            .find(|(k, _)| k.as_str() == Some(key))
            .unwrap()
            .1
    }
}
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            encode(self, true, false, false, false)
                .as_str()
                .expect("ASCII JSON"),
        )
    }
}

pub fn parse(raw: &[u8], limit: usize) -> Result<Value, &'static str> {
    if raw.len() > limit {
        return Err("JSON exceeds the read limit");
    }
    let mut rest = raw;
    while let Err(error) = std::str::from_utf8(rest) {
        rest = &rest[error.valid_up_to()..];
        if rest.len() < 3
            || rest[0] != 0xed
            || !(0xa0..=0xbf).contains(&rest[1])
            || !(0x80..=0xbf).contains(&rest[2])
        {
            return Err("invalid Python string JSON");
        }
        rest = &rest[3..];
    }
    let parsed = oms_context::parse_experiment_json_with_depth(raw, limit, DEPTH)?;
    enum Frame<'a> {
        Node(&'a oms_context::Json),
        Array(usize),
        Object(Vec<Text>),
    }
    let mut stack = vec![Frame::Node(&parsed)];
    let mut out = Vec::new();
    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Node(v) => match v {
                oms_context::Json::Null => out.push(Value::Null),
                oms_context::Json::Bool(v) => out.push(Value::Bool(*v)),
                oms_context::Json::Number(n) => out.push(Value::Number(Number::parse(n))),
                oms_context::Json::String(s) => out.push(Value::String(Text(s.clone()))),
                oms_context::Json::Array(a) => {
                    stack.push(Frame::Array(a.len()));
                    stack.extend(a.iter().rev().map(Frame::Node));
                }
                oms_context::Json::Object(o) => {
                    stack.push(Frame::Object(
                        o.iter().map(|(k, _)| Text(k.clone())).collect(),
                    ));
                    stack.extend(o.iter().rev().map(|(_, v)| Frame::Node(v)));
                }
            },
            Frame::Array(n) => {
                let a = out.split_off(out.len() - n);
                out.push(Value::Array(a));
            }
            Frame::Object(keys) => {
                let a = out.split_off(out.len() - keys.len());
                out.push(Value::Object(Object(keys.into_iter().zip(a).collect())));
            }
        }
    }
    Ok(out.pop().unwrap())
}
fn quoted(out: &mut Vec<u8>, text: &Text, ascii: bool) {
    out.push(b'"');
    for n in text.points() {
        match n {
            34 => out.extend_from_slice(b"\\\""),
            92 => out.extend_from_slice(b"\\\\"),
            8 => out.extend_from_slice(b"\\b"),
            9 => out.extend_from_slice(b"\\t"),
            10 => out.extend_from_slice(b"\\n"),
            12 => out.extend_from_slice(b"\\f"),
            13 => out.extend_from_slice(b"\\r"),
            0..=31 => out.extend_from_slice(format!("\\u{n:04x}").as_bytes()),
            127.. if ascii => {
                if n <= 65535 {
                    out.extend_from_slice(format!("\\u{n:04x}").as_bytes());
                } else {
                    let n = n - 65536;
                    out.extend_from_slice(
                        format!(
                            "\\u{:04x}\\u{:04x}",
                            0xd800 + (n >> 10),
                            0xdc00 + (n & 1023)
                        )
                        .as_bytes(),
                    );
                }
            }
            _ => out.extend_from_slice(&Text::from_points([n]).0),
        }
    }
    out.push(b'"');
}
pub fn encode(value: &Value, ascii: bool, pretty: bool, sorted: bool, spaced: bool) -> Text {
    enum Frame<'a> {
        Node(&'a Value, usize),
        Array(std::slice::Iter<'a, Value>, usize, bool),
        Object(std::vec::IntoIter<(&'a Text, &'a Value)>, usize, bool),
    }
    fn sep(out: &mut Vec<u8>, first: bool, pretty: bool, spaced: bool, depth: usize) {
        if !first {
            out.push(b',');
            if spaced && !pretty {
                out.push(b' ');
            }
        }
        if pretty {
            out.push(b'\n');
            out.extend(std::iter::repeat_n(b' ', depth * 2));
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![Frame::Node(value, 0)];
    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Node(v, depth) => match v {
                Value::Null => out.extend_from_slice(b"null"),
                Value::Bool(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
                Value::Number(n) => out.extend_from_slice(n.spelling.as_bytes()),
                Value::String(s) => quoted(&mut out, s, ascii),
                Value::Array(a) => {
                    out.push(b'[');
                    stack.push(Frame::Array(a.iter(), depth, true));
                }
                Value::Object(o) => {
                    out.push(b'{');
                    let mut fields: Vec<_> = o.iter().collect();
                    if sorted {
                        fields.sort_by(|a, b| a.0.cmp(b.0));
                    }
                    stack.push(Frame::Object(fields.into_iter(), depth, true));
                }
            },
            Frame::Array(mut a, depth, first) => {
                if let Some(v) = a.next() {
                    sep(&mut out, first, pretty, spaced, depth + 1);
                    stack.push(Frame::Array(a, depth, false));
                    stack.push(Frame::Node(v, depth + 1));
                } else {
                    if pretty && !first {
                        out.push(b'\n');
                        out.extend(std::iter::repeat_n(b' ', depth * 2));
                    }
                    out.push(b']');
                }
            }
            Frame::Object(mut o, depth, first) => {
                if let Some((k, v)) = o.next() {
                    sep(&mut out, first, pretty, spaced, depth + 1);
                    quoted(&mut out, k, ascii);
                    out.push(b':');
                    if pretty || spaced {
                        out.push(b' ');
                    }
                    stack.push(Frame::Object(o, depth, false));
                    stack.push(Frame::Node(v, depth + 1));
                } else {
                    if pretty && !first {
                        out.push(b'\n');
                        out.extend(std::iter::repeat_n(b' ', depth * 2));
                    }
                    out.push(b'}');
                }
            }
        }
    }
    Text(out)
}

pub trait ToValue {
    fn to_value(&self) -> Value;
}
impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (*self).to_value()
    }
}
impl ToValue for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}
impl ToValue for serde_json::Value {
    fn to_value(&self) -> Value {
        Value::from_serde(self)
    }
}
impl ToValue for Text {
    fn to_value(&self) -> Value {
        Value::String(self.clone())
    }
}
impl ToValue for str {
    fn to_value(&self) -> Value {
        Value::String(self.into())
    }
}
impl ToValue for String {
    fn to_value(&self) -> Value {
        self.as_str().to_value()
    }
}
impl ToValue for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}
impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, ToValue::to_value)
    }
}
impl<T: ToValue> ToValue for [T] {
    fn to_value(&self) -> Value {
        Value::Array(self.iter().map(ToValue::to_value).collect())
    }
}
impl<T: ToValue> ToValue for Vec<T> {
    fn to_value(&self) -> Value {
        self.as_slice().to_value()
    }
}
impl<T: ToValue, const N: usize> ToValue for [T; N] {
    fn to_value(&self) -> Value {
        self.as_slice().to_value()
    }
}
impl ToValue for Path {
    fn to_value(&self) -> Value {
        Value::String(Text::path(self.as_os_str()))
    }
}
impl ToValue for PathBuf {
    fn to_value(&self) -> Value {
        self.as_path().to_value()
    }
}
macro_rules! numbers {($($ty:ty),*)=>{$(impl ToValue for $ty{fn to_value(&self)->Value{Value::Number(Number::parse(&self.to_string()))}}impl PartialEq<$ty> for Value{fn eq(&self,v:&$ty)->bool{self==&v.to_value()}})*};}
numbers!(i32, i64, u32, u64, usize);
impl ToValue for f64 {
    fn to_value(&self) -> Value {
        Value::Number(Number {
            spelling: float(*self),
            integer: false,
        })
    }
}
impl PartialEq<f64> for Value {
    fn eq(&self, v: &f64) -> bool {
        self == &v.to_value()
    }
}
impl PartialEq<str> for Value {
    fn eq(&self, v: &str) -> bool {
        self.as_str() == Some(v)
    }
}
impl PartialEq<&str> for Value {
    fn eq(&self, v: &&str) -> bool {
        self.as_str() == Some(*v)
    }
}
impl PartialEq<bool> for Value {
    fn eq(&self, v: &bool) -> bool {
        self.as_bool() == Some(*v)
    }
}
impl PartialEq<String> for Value {
    fn eq(&self, v: &String) -> bool {
        self.as_str() == Some(v.as_str())
    }
}

macro_rules! json {
    (@object $o:ident;) => {};
    (@object $o:ident; , $($rest:tt)*) => {json!(@object $o; $($rest)*);};
    (@object $o:ident; $key:tt : null, $($rest:tt)*) => {$o.insert(($key).into(),$crate::value::Value::Null);json!(@object $o;$($rest)*);};
    (@object $o:ident; $key:tt : {$($inner:tt)*}, $($rest:tt)*) => {$o.insert(($key).into(),json!({$($inner)*}));json!(@object $o;$($rest)*);};
    (@object $o:ident; $key:tt : [$($inner:tt)*], $($rest:tt)*) => {$o.insert(($key).into(),json!([$($inner)*]));json!(@object $o;$($rest)*);};
    (@object $o:ident; $key:tt : $value:expr, $($rest:tt)*) => {$o.insert(($key).into(),$crate::value::ToValue::to_value(&$value));json!(@object $o;$($rest)*);};
    (@array $a:ident;) => {};
    (@array $a:ident; , $($rest:tt)*) => {json!(@array $a;$($rest)*);};
    (@array $a:ident; null, $($rest:tt)*) => {$a.push($crate::value::Value::Null);json!(@array $a;$($rest)*);};
    (@array $a:ident; {$($inner:tt)*}, $($rest:tt)*) => {$a.push(json!({$($inner)*}));json!(@array $a;$($rest)*);};
    (@array $a:ident; [$($inner:tt)*], $($rest:tt)*) => {$a.push(json!([$($inner)*]));json!(@array $a;$($rest)*);};
    (@array $a:ident; $value:expr, $($rest:tt)*) => {$a.push($crate::value::ToValue::to_value(&$value));json!(@array $a;$($rest)*);};
    ({$($inner:tt)*}) => {{#[allow(unused_mut)] let mut o=$crate::value::Object::default();json!(@object o;$($inner)*,);$crate::value::Value::Object(o)}};
    ([$($inner:tt)*]) => {{#[allow(unused_mut)] let mut a=Vec::new();json!(@array a;$($inner)*,);$crate::value::Value::Array(a)}};
    (null) => {$crate::value::Value::Null};
    ($value:expr) => {$crate::value::ToValue::to_value(&$value)};
}

pub fn from_str(raw: &str) -> Result<Value, &'static str> {
    parse(raw.as_bytes(), usize::MAX)
}
pub fn from_slice(raw: &[u8]) -> Result<Value, &'static str> {
    from_str(std::str::from_utf8(raw).map_err(|_| "invalid UTF-8 JSON")?)
}
