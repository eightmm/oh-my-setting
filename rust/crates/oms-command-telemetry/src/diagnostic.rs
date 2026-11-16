// Match the store's path-free Python JSON diagnostics without echoing row data.
struct Scanner<'a> {
    raw: &'a [u8],
    pos: usize,
}
type Failure = (&'static str, usize);
impl Scanner<'_> {
    fn space(&mut self) {
        while self
            .raw
            .get(self.pos)
            .is_some_and(|b| b" \t\n\r".contains(b))
        {
            self.pos += 1;
        }
    }
    fn string(&mut self) -> Result<(), Failure> {
        let start = self.pos;
        self.pos += 1;
        while let Some(&b) = self.raw.get(self.pos) {
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(());
                }
                0..=31 => return Err(("Invalid control character at", self.pos)),
                b'\\' => {
                    self.pos += 1;
                    let Some(&escape) = self.raw.get(self.pos) else {
                        return Err(("Unterminated string starting at", start));
                    };
                    if escape == b'u' {
                        if !self
                            .raw
                            .get(self.pos + 1..self.pos + 5)
                            .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
                        {
                            return Err(("Invalid \\uXXXX escape", self.pos));
                        }
                        self.pos += 5;
                    } else if b"\"\\/bfnrt".contains(&escape) {
                        self.pos += 1;
                    } else {
                        return Err(("Invalid \\escape", self.pos - 1));
                    }
                }
                _ => self.pos += 1,
            }
        }
        Err(("Unterminated string starting at", start))
    }
    fn value(&mut self, depth: usize) -> Result<(), Failure> {
        if depth > 1024 {
            return Err(("Expecting value", self.pos));
        }
        self.space();
        let start = self.pos;
        match self.raw.get(self.pos).copied() {
            Some(b'"') => self.string(),
            Some(b'{') => {
                self.pos += 1;
                self.space();
                if self.raw.get(self.pos) == Some(&b'}') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    if self.raw.get(self.pos) != Some(&b'"') {
                        return Err((
                            "Expecting property name enclosed in double quotes",
                            self.pos,
                        ));
                    }
                    self.string()?;
                    self.space();
                    if self.raw.get(self.pos) != Some(&b':') {
                        return Err(("Expecting ':' delimiter", self.pos));
                    }
                    self.pos += 1;
                    self.value(depth + 1)?;
                    self.space();
                    match self.raw.get(self.pos) {
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(());
                        }
                        Some(b',') => {
                            self.pos += 1;
                            self.space();
                        }
                        _ => return Err(("Expecting ',' delimiter", self.pos)),
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                self.space();
                if self.raw.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.value(depth + 1)?;
                    self.space();
                    match self.raw.get(self.pos) {
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(());
                        }
                        Some(b',') => self.pos += 1,
                        _ => return Err(("Expecting ',' delimiter", self.pos)),
                    }
                }
            }
            _ => {
                for constant in ["NaN", "Infinity", "-Infinity"] {
                    if self.raw[start..].starts_with(constant.as_bytes()) {
                        return Err((constant, start));
                    }
                }
                for word in ["true", "false", "null"] {
                    if self.raw[start..].starts_with(word.as_bytes()) {
                        self.pos += word.len();
                        return Ok(());
                    }
                }
                if self.raw.get(self.pos) == Some(&b'-') {
                    self.pos += 1;
                }
                match self.raw.get(self.pos) {
                    Some(b'0') => self.pos += 1,
                    Some(b'1'..=b'9') => {
                        while self.raw.get(self.pos).is_some_and(u8::is_ascii_digit) {
                            self.pos += 1;
                        }
                    }
                    _ => return Err(("Expecting value", start)),
                }
                if self.raw.get(self.pos) == Some(&b'.')
                    && self.raw.get(self.pos + 1).is_some_and(u8::is_ascii_digit)
                {
                    self.pos += 2;
                    while self.raw.get(self.pos).is_some_and(u8::is_ascii_digit) {
                        self.pos += 1;
                    }
                }
                if self
                    .raw
                    .get(self.pos)
                    .is_some_and(|b| *b == b'e' || *b == b'E')
                {
                    let exponent = self.pos;
                    self.pos += 1;
                    if self
                        .raw
                        .get(self.pos)
                        .is_some_and(|b| *b == b'+' || *b == b'-')
                    {
                        self.pos += 1;
                    }
                    if self.raw.get(self.pos).is_some_and(u8::is_ascii_digit) {
                        while self.raw.get(self.pos).is_some_and(u8::is_ascii_digit) {
                            self.pos += 1;
                        }
                    } else {
                        self.pos = exponent;
                    }
                }
                Ok(())
            }
        }
    }
}
pub(crate) fn json(raw: &[u8]) -> String {
    let mut scanner = Scanner { raw, pos: 0 };
    let failure = match scanner.value(0) {
        Err(f) => Some(f),
        Ok(()) => {
            scanner.space();
            (scanner.pos < raw.len()).then_some(("Extra data", scanner.pos))
        }
    };
    let Some((message, pos)) = failure else {
        return "unsupported JSON nesting or integer length".into();
    };
    if ["NaN", "Infinity", "-Infinity"].contains(&message) {
        return format!("non-finite JSON value: {message}");
    }
    let prefix = std::str::from_utf8(&raw[..pos]).unwrap_or("");
    let character = prefix.chars().count();
    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("{message}: line {line} column {column} (char {character})")
}
pub(crate) fn utf8(error: std::str::Utf8Error, raw: &[u8]) -> String {
    let pos = error.valid_up_to();
    let byte = raw[pos];
    let reason = if error.error_len().is_none() {
        "unexpected end of data"
    } else if !(0xc2..=0xf4).contains(&byte) {
        "invalid start byte"
    } else {
        "invalid continuation byte"
    };
    let n = error.error_len().unwrap_or(raw.len() - pos);
    let location = if n == 1 {
        format!("byte 0x{byte:02x} in position {pos}")
    } else {
        format!("bytes in position {pos}-{}", pos + n - 1)
    };
    format!("'utf-8' codec can't decode {location}: {reason}")
}
