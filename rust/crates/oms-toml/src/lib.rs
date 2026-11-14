//! Bounded TOML 1.0 parsing without external dependencies.
//!
//! Dates and times are represented as ISO 8601 strings. Fractional seconds
//! follow Python 3.11 `tomllib`: precision beyond microseconds is truncated.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

pub type Table = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Datetime(String),
    Array(Vec<Value>),
    Table(Table),
}

impl Value {
    pub fn as_table(&self) -> Option<&Table> {
        if let Self::Table(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        if let Self::Array(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_integer(&self) -> Option<i64> {
        if let Self::Integer(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        if let Self::Float(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Boolean(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_table()?.get(key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at line {}, column {}",
            self.message, self.line, self.column
        )
    }
}
impl std::error::Error for Error {}

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 256;

/// Parse UTF-8 TOML, with a 16 MiB input limit and 256 levels of nesting.
pub fn parse_str(input: &str) -> Result<Table, Error> {
    parse_bytes(input.as_bytes())
}

/// Parse strictly validated UTF-8 with the same bounds as [`parse_str`].
pub fn parse_bytes(input: &[u8]) -> Result<Table, Error> {
    parse_bounded(input, MAX_BYTES)
}

/// Parse with an inclusive caller-selected byte limit and 256 nesting levels.
///
/// The size check precedes UTF-8 validation and allocation. A zero limit accepts
/// only empty input. CRLF becomes LF, including inside multiline strings.
pub fn parse_bounded(input: &[u8], max_bytes: usize) -> Result<Table, Error> {
    if input.len() > max_bytes {
        return Err(error_at("", 0, "Input exceeds byte limit"));
    }
    let input = std::str::from_utf8(input).map_err(|error| {
        let prefix = std::str::from_utf8(&input[..error.valid_up_to()]).unwrap_or("");
        error_at(prefix, prefix.len(), "Input is not valid UTF-8")
    })?;
    let normalized = input.replace("\r\n", "\n");
    Parser {
        input: &normalized,
        pos: 0,
    }
    .document()
}

fn error_at(input: &str, pos: usize, message: &str) -> Error {
    let prefix = &input[..pos];
    Error {
        line: prefix.bytes().filter(|&b| b == b'\n').count() + 1,
        column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
        message: message.to_owned(),
    }
}

// Dotted containers remain extensible in their originating table section.
// A new header seals those declarations against later dotted redefinition.
#[derive(Clone, Copy)]
enum Declaration {
    Implicit,
    Header,
    Dotted(usize),
}

struct Namespace {
    entries: BTreeMap<String, Node>,
    declaration: Declaration,
}
impl Namespace {
    fn new(declaration: Declaration) -> Self {
        Self {
            entries: BTreeMap::new(),
            declaration,
        }
    }
    fn into_table(self) -> Table {
        self.entries
            .into_iter()
            .map(|(key, value)| (key, value.into_value()))
            .collect()
    }
}

enum Node {
    Namespace(Namespace),
    Tables(Vec<Namespace>),
    // Parsed inline collections are sealed: no mutable namespace is exposed.
    Sealed(Value),
}
impl Node {
    fn into_value(self) -> Value {
        match self {
            Self::Namespace(table) => Value::Table(table.into_table()),
            Self::Tables(tables) => Value::Array(
                tables
                    .into_iter()
                    .map(|t| Value::Table(t.into_table()))
                    .collect(),
            ),
            Self::Sealed(value) => value,
        }
    }
}

fn descend<'a>(
    table: &'a mut Namespace,
    path: &[String],
) -> Result<&'a mut Namespace, &'static str> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(table);
    };
    let node = table
        .entries
        .entry(first.clone())
        .or_insert_with(|| Node::Namespace(Namespace::new(Declaration::Implicit)));
    let child = match node {
        Node::Namespace(child) => child,
        Node::Tables(children) => children.last_mut().ok_or("Empty array of tables")?,
        Node::Sealed(_) => return Err("Cannot extend a value or inline collection"),
    };
    descend(child, rest)
}

fn insert(
    table: &mut Namespace,
    path: &[String],
    value: Value,
    section: usize,
    inline: bool,
) -> Result<(), &'static str> {
    let (first, rest) = path.split_first().ok_or("Expected key")?;
    if rest.is_empty() {
        if table.entries.contains_key(first) {
            return Err("Duplicate key");
        }
        table.entries.insert(first.clone(), Node::Sealed(value));
        return Ok(());
    }
    let node = table
        .entries
        .entry(first.clone())
        .or_insert_with(|| Node::Namespace(Namespace::new(Declaration::Dotted(section))));
    let Node::Namespace(child) = node else {
        return Err("Cannot extend a value or inline collection");
    };
    if !inline {
        match child.declaration {
            Declaration::Header => return Err("Cannot redefine an explicitly declared table"),
            Declaration::Dotted(previous) if previous != section => {
                return Err("Cannot redefine a dotted-key table");
            }
            _ => child.declaration = Declaration::Dotted(section),
        }
    }
    insert(child, rest, value, section, inline)
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}
impl Parser<'_> {
    fn error(&self, message: &str) -> Error {
        error_at(self.input, self.pos, message)
    }
    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }
    fn starts(&self, text: &str) -> bool {
        self.input[self.pos..].starts_with(text)
    }
    fn horizontal(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }
    fn expect(&mut self, byte: u8, message: &str) -> Result<(), Error> {
        if self.peek() != Some(byte) {
            return Err(self.error(message));
        }
        self.pos += 1;
        Ok(())
    }
    fn comment(&mut self) -> Result<(), Error> {
        if self.peek() == Some(b'#') {
            while let Some(byte) = self.peek() {
                if byte == b'\n' {
                    break;
                }
                if (byte < 0x20 && byte != b'\t') || byte == 0x7f {
                    return Err(self.error("Control character in comment"));
                }
                self.pos += self.input[self.pos..].chars().next().unwrap().len_utf8();
            }
        }
        Ok(())
    }
    fn array_space(&mut self) -> Result<(), Error> {
        loop {
            self.horizontal();
            self.comment()?;
            if self.peek() != Some(b'\n') {
                return Ok(());
            }
            self.pos += 1;
        }
    }
    fn depth(&self, depth: usize) -> Result<(), Error> {
        if depth > MAX_DEPTH {
            Err(self.error("Nesting exceeds depth limit"))
        } else {
            Ok(())
        }
    }
    fn document(&mut self) -> Result<Table, Error> {
        let mut root = Namespace::new(Declaration::Implicit);
        let mut header: Vec<String> = Vec::new();
        let mut section = 0;
        loop {
            self.horizontal();
            match self.peek() {
                None => return Ok(root.into_table()),
                Some(b'\n') => {
                    self.pos += 1;
                    continue;
                }
                Some(b'#') => self.comment()?,
                Some(b'[') => {
                    section += 1;
                    self.pos += 1;
                    let array = self.peek() == Some(b'[');
                    if array {
                        self.pos += 1;
                    }
                    let path = self.key()?;
                    self.expect(b']', "Expected closing table bracket")?;
                    if array {
                        self.expect(b']', "Expected second closing table bracket")?;
                    }
                    let (last, parents) = path.split_last().unwrap();
                    let parent =
                        descend(&mut root, parents).map_err(|message| self.error(message))?;
                    if array {
                        let node = parent
                            .entries
                            .entry(last.clone())
                            .or_insert_with(|| Node::Tables(Vec::new()));
                        let Node::Tables(tables) = node else {
                            return Err(self.error("Cannot append to a value or static array"));
                        };
                        tables.push(Namespace::new(Declaration::Header));
                    } else {
                        let node = parent.entries.entry(last.clone()).or_insert_with(|| {
                            Node::Namespace(Namespace::new(Declaration::Implicit))
                        });
                        let Node::Namespace(table) = node else {
                            return Err(self.error("Cannot declare a value as a table"));
                        };
                        if !matches!(table.declaration, Declaration::Implicit) {
                            return Err(self.error("Table already declared"));
                        }
                        table.declaration = Declaration::Header;
                    }
                    header = path;
                }
                _ => {
                    let path = self.key()?;
                    self.depth(header.len() + path.len())?;
                    self.expect(b'=', "Expected '=' after key")?;
                    self.horizontal();
                    let value = self.value(header.len() + path.len())?;
                    let table =
                        descend(&mut root, &header).map_err(|message| self.error(message))?;
                    insert(table, &path, value, section, false)
                        .map_err(|message| self.error(message))?;
                }
            }
            self.horizontal();
            self.comment()?;
            if self.peek().is_some() {
                self.expect(b'\n', "Expected newline after statement")?;
            }
        }
    }
    fn key(&mut self) -> Result<Vec<String>, Error> {
        let mut path = Vec::new();
        loop {
            self.horizontal();
            let key = match self.peek() {
                Some(b'"' | b'\'') => self.string(false)?,
                _ => {
                    let start = self.pos;
                    while self
                        .peek()
                        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    {
                        self.pos += 1;
                    }
                    if start == self.pos {
                        return Err(self.error("Expected bare or quoted key"));
                    }
                    self.input[start..self.pos].to_owned()
                }
            };
            path.push(key);
            self.depth(path.len())?;
            self.horizontal();
            if self.peek() != Some(b'.') {
                return Ok(path);
            }
            self.pos += 1;
        }
    }
    fn string(&mut self, allow_multiline: bool) -> Result<String, Error> {
        let quote = self.peek().unwrap();
        let triple = if quote == b'"' { "\"\"\"" } else { "'''" };
        let multiline = allow_multiline && self.starts(triple);
        self.pos += if multiline { 3 } else { 1 };
        if multiline && self.peek() == Some(b'\n') {
            self.pos += 1;
        }
        let mut result = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error("Unterminated string"));
            };
            if byte == quote {
                if !multiline {
                    self.pos += 1;
                    return Ok(result);
                }
                if self.starts(triple) {
                    self.pos += 3;
                    for _ in 0..2 {
                        if self.peek() == Some(quote) {
                            result.push(quote as char);
                            self.pos += 1;
                        }
                    }
                    return Ok(result);
                }
            }
            if (byte < 0x20 && byte != b'\t' && !(multiline && byte == b'\n')) || byte == 0x7f {
                return Err(self.error("Unescaped control character in string"));
            }
            if byte == b'\\' && quote == b'"' {
                self.pos += 1;
                if multiline && matches!(self.peek(), Some(b' ' | b'\t' | b'\n')) {
                    self.horizontal();
                    self.expect(b'\n', "Line continuation requires a newline")?;
                    while matches!(self.peek(), Some(b' ' | b'\t' | b'\n')) {
                        self.pos += 1;
                    }
                    continue;
                }
                let escaped = match self.peek() {
                    Some(b'b') => '\u{8}',
                    Some(b't') => '\t',
                    Some(b'n') => '\n',
                    Some(b'f') => '\u{c}',
                    Some(b'r') => '\r',
                    Some(b'"') => '"',
                    Some(b'\\') => '\\',
                    Some(b'u' | b'U') => {
                        let count = if self.peek() == Some(b'u') { 4 } else { 8 };
                        self.pos += 1;
                        let mut code = 0u32;
                        for _ in 0..count {
                            let digit = self
                                .peek()
                                .and_then(|b| (b as char).to_digit(16))
                                .ok_or_else(|| self.error("Invalid Unicode escape"))?;
                            code = code * 16 + digit;
                            self.pos += 1;
                        }
                        let ch = char::from_u32(code)
                            .ok_or_else(|| self.error("Invalid Unicode scalar"))?;
                        result.push(ch);
                        continue;
                    }
                    _ => return Err(self.error("Invalid string escape")),
                };
                self.pos += 1;
                result.push(escaped);
            } else {
                let ch = self.input[self.pos..].chars().next().unwrap();
                self.pos += ch.len_utf8();
                result.push(ch);
            }
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        self.depth(depth)?;
        match self.peek() {
            Some(b'"' | b'\'') => Ok(Value::String(self.string(true)?)),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.inline_table(depth),
            _ => self.atom(),
        }
    }
    fn array(&mut self, depth: usize) -> Result<Value, Error> {
        self.pos += 1;
        let mut values = Vec::new();
        self.array_space()?;
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(values));
        }
        loop {
            values.push(self.value(depth + 1)?);
            self.array_space()?;
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Value::Array(values));
            }
            self.expect(b',', "Expected comma or closing array bracket")?;
            self.array_space()?;
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Value::Array(values));
            }
        }
    }
    fn inline_table(&mut self, depth: usize) -> Result<Value, Error> {
        self.pos += 1;
        let mut table = Namespace::new(Declaration::Implicit);
        self.horizontal();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Table(table.into_table()));
        }
        loop {
            let path = self.key()?;
            self.expect(b'=', "Expected '=' after inline key")?;
            self.horizontal();
            let value = self.value(depth + path.len())?;
            insert(&mut table, &path, value, 0, true).map_err(|message| self.error(message))?;
            self.horizontal();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(Value::Table(table.into_table()));
            }
            self.expect(b',', "Expected comma or closing inline brace")?;
            self.horizontal();
        }
    }
    fn atom(&mut self) -> Result<Value, Error> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'#' | b',' | b']' | b'}'))
        {
            self.pos += self.input[self.pos..].chars().next().unwrap().len_utf8();
        }
        // A single space is also the permitted date/time separator.
        if self.pos - start == 10 && self.peek() == Some(b' ') {
            let bytes = self.input.as_bytes();
            if bytes.get(self.pos + 1).is_some_and(u8::is_ascii_digit)
                && bytes.get(self.pos + 2).is_some_and(u8::is_ascii_digit)
                && bytes.get(self.pos + 3) == Some(&b':')
            {
                self.pos += 1;
                while self.peek().is_some_and(|b| {
                    !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'#' | b',' | b']' | b'}')
                }) {
                    self.pos += self.input[self.pos..].chars().next().unwrap().len_utf8();
                }
            }
        }
        let atom = &self.input[start..self.pos];
        let value = match atom {
            "true" => Some(Value::Boolean(true)),
            "false" => Some(Value::Boolean(false)),
            "inf" | "+inf" => Some(Value::Float(f64::INFINITY)),
            "-inf" => Some(Value::Float(f64::NEG_INFINITY)),
            "nan" | "+nan" => Some(Value::Float(f64::NAN)),
            "-nan" => Some(Value::Float(-f64::NAN)),
            _ => datetime(atom).map(Value::Datetime).or_else(|| number(atom)),
        };
        value.ok_or_else(|| error_at(self.input, start, "Invalid TOML value"))
    }
}

fn digits(text: &str, radix: u32) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.iter().enumerate().all(|(index, &byte)| {
            if byte == b'_' {
                index > 0
                    && index + 1 < bytes.len()
                    && (bytes[index - 1] as char).is_digit(radix)
                    && (bytes[index + 1] as char).is_digit(radix)
            } else {
                (byte as char).is_digit(radix)
            }
        })
}

fn decimal(text: &str) -> bool {
    digits(text, 10) && (text.len() == 1 || !text.starts_with('0'))
}

fn number(text: &str) -> Option<Value> {
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(body) = text.strip_prefix(prefix) {
            if !digits(body, radix) {
                return None;
            }
            return i64::from_str_radix(&body.replace('_', ""), radix)
                .ok()
                .map(Value::Integer);
        }
    }
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(index) => (&body[..index], Some(&body[index + 1..])),
        None => (body, None),
    };
    if let Some(exponent) = exponent {
        if !digits(exponent.strip_prefix(['+', '-']).unwrap_or(exponent), 10) {
            return None;
        }
    }
    let fraction = mantissa.find('.');
    if let Some(index) = fraction {
        if !decimal(&mantissa[..index]) || !digits(&mantissa[index + 1..], 10) {
            return None;
        }
    } else if !decimal(mantissa) {
        return None;
    }
    let clean = text.replace('_', "");
    if fraction.is_some() || exponent.is_some() {
        clean.parse().ok().map(Value::Float)
    } else {
        clean.parse().ok().map(Value::Integer)
    }
}

fn fixed_digits(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        bytes
            .iter()
            .fold(0, |total, b| total * 10 + u32::from(b - b'0')),
    )
}

fn datetime(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let has_date = bytes.len() >= 10 && bytes.get(4) == Some(&b'-') && bytes.get(7) == Some(&b'-');
    let mut pos = 0;
    let mut result = String::new();
    if has_date {
        let year = fixed_digits(&bytes[..4])?;
        let month = fixed_digits(&bytes[5..7])?;
        let day = fixed_digits(&bytes[8..10])?;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if leap {
                    29
                } else {
                    28
                }
            }
            _ => return None,
        };
        if year == 0 || day == 0 || day > days {
            return None;
        }
        result.push_str(&text[..10]);
        if bytes.len() == 10 {
            return Some(result);
        }
        if !matches!(bytes.get(10), Some(b'T' | b't' | b' ')) {
            return None;
        }
        result.push('T');
        pos = 11;
    }
    if bytes.len() < pos + 8 || bytes[pos + 2] != b':' || bytes[pos + 5] != b':' {
        return None;
    }
    let hour = fixed_digits(&bytes[pos..pos + 2])?;
    let minute = fixed_digits(&bytes[pos + 3..pos + 5])?;
    let second = fixed_digits(&bytes[pos + 6..pos + 8])?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    result.push_str(&text[pos..pos + 8]);
    pos += 8;
    if bytes.get(pos) == Some(&b'.') {
        pos += 1;
        let start = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if pos == start {
            return None;
        }
        let count = (pos - start).min(6);
        let micros = fixed_digits(&bytes[start..start + count])? * 10u32.pow(6 - count as u32);
        if micros != 0 {
            result.push_str(&format!(".{micros:06}"));
        }
    }
    if pos == bytes.len() {
        return Some(result);
    }
    if !has_date {
        return None;
    }
    if matches!(bytes.get(pos), Some(b'Z' | b'z')) && pos + 1 == bytes.len() {
        result.push_str("+00:00");
        return Some(result);
    }
    if !matches!(bytes.get(pos), Some(b'+' | b'-'))
        || pos + 6 != bytes.len()
        || bytes[pos + 3] != b':'
    {
        return None;
    }
    let hour = fixed_digits(&bytes[pos + 1..pos + 3])?;
    let minute = fixed_digits(&bytes[pos + 4..pos + 6])?;
    if hour > 23 || minute > 59 {
        return None;
    }
    if hour == 0 && minute == 0 {
        result.push_str("+00:00");
    } else {
        result.push_str(&text[pos..]);
    }
    Some(result)
}

#[cfg(test)]
mod tests;
