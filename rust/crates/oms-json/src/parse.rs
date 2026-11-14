//! Iterative parsing with independent limits on input bytes, nesting and integers.

use crate::{Integer, PyString, Value};
use std::collections::BTreeMap;

/// Limits apply in both modes. Zero integer digits means no digit limit; zero
/// bytes accepts no document, and zero depth permits only scalar roots.
/// Depth counts arrays and objects, including empty containers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseOptions {
    pub allow_nonfinite: bool,
    pub allow_duplicate_keys: bool,
    pub allow_lone_surrogates: bool,
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_integer_digits: usize,
}

impl ParseOptions {
    pub fn strict() -> Self {
        Self {
            allow_nonfinite: false,
            allow_duplicate_keys: false,
            allow_lone_surrogates: false,
            max_bytes: 64 * 1024 * 1024,
            max_depth: 1024,
            max_integer_digits: 4300,
        }
    }

    pub fn python_compat() -> Self {
        Self {
            allow_nonfinite: true,
            allow_duplicate_keys: true,
            allow_lone_surrogates: true,
            ..Self::strict()
        }
    }
}

/// Parser errors are separate from the existing serialization `crate::Error`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
    ByteLimit,
    DepthLimit,
    IntegerDigitLimit,
    InvalidUtf8,
    UnexpectedEnd,
    InvalidSyntax,
    InvalidNumber,
    InvalidEscape,
    UnescapedControl,
    LoneSurrogate,
    DuplicateKey,
    NonFiniteFloat,
    TrailingData,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ParseError {}

pub fn parse_strict(input: &[u8]) -> Result<Value, ParseError> {
    parse_with_options(input, &ParseOptions::strict())
}

pub fn parse_python(input: &[u8]) -> Result<Value, ParseError> {
    parse_with_options(input, &ParseOptions::python_compat())
}

enum Frame {
    Array(Vec<Value>),
    Object(BTreeMap<PyString, Value>, PyString),
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    options: &'a ParseOptions,
}

pub fn parse_with_options(input: &[u8], options: &ParseOptions) -> Result<Value, ParseError> {
    if input.len() > options.max_bytes {
        return Err(ParseError::ByteLimit);
    }
    let mut parser = Parser {
        input: std::str::from_utf8(input).map_err(|_| ParseError::InvalidUtf8)?,
        pos: 0,
        options,
    };
    let mut stack = Vec::new();
    loop {
        parser.whitespace();
        let mut value = match parser.peek().ok_or(ParseError::UnexpectedEnd)? {
            b'[' | b'{' => {
                if stack.len() >= options.max_depth {
                    return Err(ParseError::DepthLimit);
                }
                let open = parser.take()?;
                parser.whitespace();
                if open == b'[' {
                    if parser.consume(b']') {
                        Value::Array(Vec::new())
                    } else {
                        stack.push(Frame::Array(Vec::new()));
                        continue;
                    }
                } else if parser.consume(b'}') {
                    Value::Object(BTreeMap::new())
                } else {
                    let key = parser.object_key()?;
                    stack.push(Frame::Object(BTreeMap::new(), key));
                    continue;
                }
            }
            b'"' => Value::String(parser.string()?),
            b'n' => {
                parser.literal(b"null")?;
                Value::Null
            }
            b't' => {
                parser.literal(b"true")?;
                Value::Bool(true)
            }
            b'f' => {
                parser.literal(b"false")?;
                Value::Bool(false)
            }
            b'N' | b'I' => parser.nonfinite()?,
            b'-' if parser.input.as_bytes().get(parser.pos + 1) == Some(&b'I') => {
                parser.nonfinite()?
            }
            b'-' | b'0'..=b'9' => parser.number()?,
            _ => return Err(ParseError::InvalidSyntax),
        };
        // Completed children bubble up through frames; a comma resumes value
        // parsing, while a closing delimiter completes the parent in this loop.
        loop {
            parser.whitespace();
            match stack.pop() {
                None => {
                    return if parser.pos == parser.input.len() {
                        Ok(value)
                    } else {
                        Err(ParseError::TrailingData)
                    };
                }
                Some(Frame::Array(mut items)) => {
                    items.push(value);
                    if parser.consume(b']') {
                        value = Value::Array(items);
                    } else {
                        parser.expect(b',')?;
                        stack.push(Frame::Array(items));
                        break;
                    }
                }
                Some(Frame::Object(mut items, key)) => {
                    if !options.allow_duplicate_keys && items.contains_key(&key) {
                        return Err(ParseError::DuplicateKey);
                    }
                    items.insert(key, value);
                    if parser.consume(b'}') {
                        value = Value::Object(items);
                    } else {
                        parser.expect(b',')?;
                        parser.whitespace();
                        let key = parser.object_key()?;
                        stack.push(Frame::Object(items, key));
                        break;
                    }
                }
            }
        }
    }
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }

    fn take(&mut self) -> Result<u8, ParseError> {
        let byte = self.peek().ok_or(ParseError::UnexpectedEnd)?;
        self.pos += 1;
        Ok(byte)
    }

    fn consume(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), ParseError> {
        if self.take()? == byte {
            Ok(())
        } else {
            Err(ParseError::InvalidSyntax)
        }
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn literal(&mut self, text: &[u8]) -> Result<(), ParseError> {
        for &byte in text {
            self.expect(byte)?;
        }
        Ok(())
    }

    fn object_key(&mut self) -> Result<PyString, ParseError> {
        let key = self.string()?;
        self.whitespace();
        self.expect(b':')?;
        Ok(key)
    }

    fn nonfinite(&mut self) -> Result<Value, ParseError> {
        let value = if self.peek() == Some(b'N') {
            self.literal(b"NaN")?;
            f64::NAN
        } else if self.consume(b'-') {
            self.literal(b"Infinity")?;
            f64::NEG_INFINITY
        } else {
            self.literal(b"Infinity")?;
            f64::INFINITY
        };
        if !self.options.allow_nonfinite {
            return Err(ParseError::NonFiniteFloat);
        }
        Ok(Value::Float(value))
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        self.consume(b'-');
        let digits_start = self.pos;
        match self.take()? {
            b'0' => {
                if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    return Err(ParseError::InvalidNumber);
                }
            }
            b'1'..=b'9' => self.digits(),
            _ => return Err(ParseError::InvalidNumber),
        }
        let digit_len = self.pos - digits_start;
        let mut is_float = false;
        if self.consume(b'.') {
            is_float = true;
            self.required_digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            is_float = true;
            if !self.consume(b'+') {
                self.consume(b'-');
            }
            self.required_digits()?;
        }
        let numeric_text = &self.input[start..self.pos];
        if is_float {
            let number = numeric_text
                .parse::<f64>()
                .map_err(|_| ParseError::InvalidNumber)?;
            if !number.is_finite() && !self.options.allow_nonfinite {
                return Err(ParseError::NonFiniteFloat);
            }
            Ok(Value::Float(number))
        } else {
            if self.options.max_integer_digits != 0 && digit_len > self.options.max_integer_digits {
                return Err(ParseError::IntegerDigitLimit);
            }
            Ok(Value::Integer(Integer(
                if numeric_text == "-0" {
                    "0"
                } else {
                    numeric_text
                }
                .to_owned(),
            )))
        }
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn required_digits(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        self.digits();
        if self.pos == start {
            Err(ParseError::InvalidNumber)
        } else {
            Ok(())
        }
    }

    fn hex_quad(&mut self) -> Result<u32, ParseError> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = (self.take()? as char)
                .to_digit(16)
                .ok_or(ParseError::InvalidEscape)?;
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<PyString, ParseError> {
        self.expect(b'"')?;
        let mut points = Vec::new();
        loop {
            match self.take()? {
                b'"' => return Ok(PyString(points)),
                b'\\' => {
                    let mut point = match self.take()? {
                        b'"' => 0x22,
                        b'\\' => 0x5c,
                        b'/' => 0x2f,
                        b'b' => 8,
                        b'f' => 12,
                        b'n' => 10,
                        b'r' => 13,
                        b't' => 9,
                        b'u' => self.hex_quad()?,
                        _ => return Err(ParseError::InvalidEscape),
                    };
                    if (0xd800..=0xdbff).contains(&point)
                        && self.input.as_bytes().get(self.pos..self.pos + 2) == Some(b"\\u")
                    {
                        let next = self.pos;
                        self.pos += 2;
                        let low = self.hex_quad()?;
                        if (0xdc00..=0xdfff).contains(&low) {
                            point = 0x10000 + ((point - 0xd800) << 10) + low - 0xdc00;
                        } else {
                            // A non-low escape belongs to the next code point.
                            self.pos = next;
                        }
                    }
                    if (0xd800..=0xdfff).contains(&point) && !self.options.allow_lone_surrogates {
                        return Err(ParseError::LoneSurrogate);
                    }
                    points.push(point);
                }
                0..=0x1f => return Err(ParseError::UnescapedControl),
                byte if byte.is_ascii() => points.push(u32::from(byte)),
                _ => {
                    // The complete input was UTF-8 validated before parsing.
                    let scalar = self.input[self.pos - 1..].chars().next().unwrap();
                    self.pos += scalar.len_utf8() - 1;
                    points.push(u32::from(scalar));
                }
            }
        }
    }
}
