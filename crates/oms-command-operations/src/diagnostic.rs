//! Syntax diagnostics follow JSONDecoder after the lossless parser rejects a text row.
struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}
type Failure = (&'static str, usize);
impl Parser<'_> {
    fn ws(&mut self) {
        while self.pos < self.bytes.len() && b" \t\r\n".contains(&self.bytes[self.pos]) {
            self.pos += 1;
        }
    }
    fn string(&mut self) -> Result<(), Failure> {
        let start = self.pos;
        self.pos += 1;
        while let Some(&b) = self.bytes.get(self.pos) {
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(());
                }
                b'\\' => {
                    self.pos += 1;
                    match self.bytes.get(self.pos) {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.pos += 1
                        }
                        Some(b'u') => {
                            let at = self.pos;
                            self.pos += 1;
                            for _ in 0..4 {
                                if !self
                                    .bytes
                                    .get(self.pos)
                                    .is_some_and(|b| b.is_ascii_hexdigit())
                                {
                                    return Err(("Invalid \\uXXXX escape", at));
                                }
                                self.pos += 1;
                            }
                        }
                        _ => return Err(("Invalid \\escape", self.pos.saturating_sub(1))),
                    }
                }
                0..=31 => return Err(("Invalid control character at", self.pos)),
                _ => self.pos += 1,
            }
        }
        Err(("Unterminated string starting at", start))
    }
    fn value(&mut self, depth: usize) -> Result<(), Failure> {
        self.ws();
        let start = self.pos;
        if depth > 1024 {
            return Err(("JSON nesting exceeds the supported limit", start));
        }
        match self.bytes.get(self.pos) {
            Some(b'"') => self.string(),
            Some(b'{') => {
                self.pos += 1;
                self.ws();
                if self.bytes.get(self.pos) == Some(&b'}') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.ws();
                    if self.bytes.get(self.pos) != Some(&b'"') {
                        return Err((
                            "Expecting property name enclosed in double quotes",
                            self.pos,
                        ));
                    }
                    self.string()?;
                    self.ws();
                    if self.bytes.get(self.pos) != Some(&b':') {
                        return Err(("Expecting ':' delimiter", self.pos));
                    }
                    self.pos += 1;
                    self.value(depth + 1)?;
                    self.ws();
                    match self.bytes.get(self.pos) {
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(());
                        }
                        Some(b',') => self.pos += 1,
                        _ => return Err(("Expecting ',' delimiter", self.pos)),
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                self.ws();
                if self.bytes.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.value(depth + 1)?;
                    self.ws();
                    match self.bytes.get(self.pos) {
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(());
                        }
                        Some(b',') => self.pos += 1,
                        _ => return Err(("Expecting ',' delimiter", self.pos)),
                    }
                }
            }
            Some(b't' | b'f' | b'n' | b'N' | b'I') => {
                for word in [b"true".as_slice(), b"false", b"null", b"NaN", b"Infinity"] {
                    if self.bytes[self.pos..].starts_with(word) {
                        self.pos += word.len();
                        return Ok(());
                    }
                }
                Err(("Expecting value", start))
            }
            Some(b'-' | b'0'..=b'9') => {
                if self.bytes[self.pos..].starts_with(b"-Infinity") {
                    self.pos += 9;
                    return Ok(());
                }
                let mut i = self.pos;
                if self.bytes[i] == b'-' {
                    i += 1;
                }
                match self.bytes.get(i) {
                    Some(b'0') => i += 1,
                    Some(b'1'..=b'9') => {
                        i += 1;
                        while self.bytes.get(i).is_some_and(|b| b.is_ascii_digit()) {
                            i += 1;
                        }
                    }
                    _ => return Err(("Expecting value", start)),
                }
                if self.bytes.get(i) == Some(&b'.')
                    && self.bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit())
                {
                    i += 2;
                    while self.bytes.get(i).is_some_and(|b| b.is_ascii_digit()) {
                        i += 1;
                    }
                }
                if self.bytes.get(i).is_some_and(|b| matches!(b, b'e' | b'E')) {
                    let mut j = i + 1;
                    if self.bytes.get(j).is_some_and(|b| matches!(b, b'+' | b'-')) {
                        j += 1;
                    }
                    if self.bytes.get(j).is_some_and(|b| b.is_ascii_digit()) {
                        j += 1;
                        while self.bytes.get(j).is_some_and(|b| b.is_ascii_digit()) {
                            j += 1;
                        }
                        i = j;
                    }
                }
                self.pos = i;
                Ok(())
            }
            _ => Err(("Expecting value", start)),
        }
    }
}
pub fn json(text: &str) -> String {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    let failure = if text.starts_with('\u{feff}') {
        ("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0)
    } else {
        match parser.value(0) {
            Err(e) => e,
            Ok(()) => {
                parser.ws();
                if parser.pos < parser.bytes.len() {
                    ("Extra data", parser.pos)
                } else {
                    ("invalid JSON", parser.pos)
                }
            }
        }
    };
    let (message, byte) = failure;
    let byte = byte.min(text.len());
    let prefix = &text[..text.floor_char_boundary(byte)];
    let position = prefix.chars().count();
    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("{message}: line {line} column {column} (char {position})")
}
