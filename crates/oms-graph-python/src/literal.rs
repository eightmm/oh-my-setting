//! Docstring text with escaped lone surrogates kept distinct.
//!
//! Python decodes `"\ud800"` to a lone surrogate; Rust strings cannot hold
//! one and Ruff substitutes U+FFFD, which is indistinguishable from a literal
//! U+FFFD in the source. The graph does not scrub category C characters in
//! general: an actual U+FFFD (and any other character) is kept as written, and
//! only the surrogate, which no Rust string can carry, is shown as `?`. The two
//! must therefore be told apart, and units keep the surrogate's own value so no
//! character can collide with it. Only literals that actually contain a
//! surrogate escape are re-decoded here, from the source range Ruff reports for
//! each string part; every other literal keeps Ruff's decoded value. Raw
//! strings have no escapes and are never re-decoded.

use ruff_python_ast::{ExprStringLiteral, StringLiteral};
use ruff_python_parser::parse_expression;
use ruff_text_size::Ranged;

/// Docstring code point units: surrogates keep their own value (U+D800..=U+DFFF),
/// so nothing a source can contain collides with them.
pub(crate) fn doc_units(literal: &ExprStringLiteral, source: &str) -> Vec<u32> {
    let mut units = Vec::new();
    for part in literal.value.iter() {
        let own = escaped_body(part, source)
            .and_then(decode)
            .filter(|own| agrees(own, &part.value));
        match own {
            Some(own) => units.extend(own),
            None => units.extend(part.value.chars().map(u32::from)),
        }
    }
    units
}

/// The text between the quotes of a non-raw part whose source holds a
/// surrogate escape; `None` for every other part.
fn escaped_body<'s>(part: &StringLiteral, source: &'s str) -> Option<&'s str> {
    let literal = source.get(part.range().start().to_usize()..part.range().end().to_usize())?;
    let prefix_len = literal.find(['"', '\''])?;
    if literal[..prefix_len].contains(['r', 'R']) {
        return None;
    }
    let rest = &literal[prefix_len..];
    let quote_len = if rest.starts_with("\"\"\"") || rest.starts_with("'''") {
        3
    } else {
        1
    };
    let body = rest.get(quote_len..rest.len().checked_sub(quote_len)?)?;
    has_surrogate_escape(body).then_some(body)
}

fn hex(digits: &str) -> Option<u32> {
    if digits.chars().all(|c| c.is_ascii_hexdigit()) {
        u32::from_str_radix(digits, 16).ok()
    } else {
        None
    }
}

/// The scalar value of a `\uXXXX` or `\UXXXXXXXX` escape at the start of
/// `rest` (the text after the backslash), with its source length.
fn unicode_escape(rest: &str) -> Option<(u32, usize)> {
    let width = match rest.as_bytes().first()? {
        b'u' => 4,
        b'U' => 8,
        _ => return None,
    };
    Some((hex(rest.get(1..=width)?)?, width + 1))
}

/// How many bytes of `rest` (after a backslash) the escape occupies.
fn escape_len(rest: &str) -> usize {
    let mut chars = rest.chars();
    let Some(first) = chars.next() else { return 0 };
    match first {
        'u' | 'U' => unicode_escape(rest).map_or(1, |(_, len)| len),
        'x' => rest.get(1..3).filter(|d| hex(d).is_some()).map_or(1, |_| 3),
        'N' if rest[1..].starts_with('{') => rest.find('}').map_or(1, |end| end + 1),
        '0'..='7' => rest
            .bytes()
            .take(3)
            .take_while(|b| (b'0'..=b'7').contains(b))
            .count(),
        '\r' if rest[1..].starts_with('\n') => 2,
        other => other.len_utf8(),
    }
}

fn has_surrogate_escape(body: &str) -> bool {
    let mut index = 0;
    while let Some(offset) = body[index..].find('\\') {
        let rest = &body[index + offset + 1..];
        if unicode_escape(rest).is_some_and(|(value, _)| (0xD800..=0xDFFF).contains(&value)) {
            return true;
        }
        index += offset + 1 + escape_len(rest).max(1);
    }
    false
}

/// Python's escape decoding of a non-raw `str` body.
fn decode(body: &str) -> Option<Vec<u32>> {
    let mut out: Vec<u32> = Vec::new();
    let push_str = |out: &mut Vec<u32>, text: &str| out.extend(text.chars().map(u32::from));
    let mut index = 0;
    while let Some(offset) = body[index..].find('\\') {
        push_str(&mut out, &body[index..index + offset]);
        let rest = &body[index + offset + 1..];
        let len = escape_len(rest);
        let escape = &rest[..len];
        let first = escape.chars().next()?;
        match first {
            '\n' | '\r' => {}
            '\\' | '\'' | '"' => out.push(u32::from(first)),
            'a' => out.push(0x7),
            'b' => out.push(0x8),
            'f' => out.push(0xc),
            'n' => out.push(0xa),
            'r' => out.push(0xd),
            't' => out.push(0x9),
            'v' => out.push(0xb),
            '0'..='7' => out.push(u32::from(char::from_u32(
                u32::from_str_radix(escape, 8).ok()?,
            )?)),
            'x' if len == 3 => out.push(u32::from(char::from_u32(hex(&escape[1..])?)?)),
            'u' | 'U' if len > 1 => {
                let (value, _) = unicode_escape(escape)?;
                if !(0xD800..=0xDFFF).contains(&value) {
                    char::from_u32(value)?;
                }
                out.push(value);
            }
            'N' if len > 1 => push_str(&mut out, &named(&escape[2..len - 1])?),
            _ => {
                out.push(u32::from('\\'));
                push_str(&mut out, escape);
            }
        }
        index += offset + 1 + len;
    }
    push_str(&mut out, &body[index..]);
    Some(out)
}

/// `\N{name}` resolved by Ruff itself.
fn named(name: &str) -> Option<String> {
    if name.contains(['"', '\\', '\n']) {
        return None;
    }
    let parsed = parse_expression(&format!("\"\\N{{{name}}}\"")).ok()?;
    match parsed.syntax().body.as_ref() {
        ruff_python_ast::Expr::StringLiteral(literal) => Some(literal.value.to_str().to_owned()),
        _ => None,
    }
}

/// The re-decoded text must equal Ruff's value once surrogates become U+FFFD
/// and line breaks are normalized; otherwise Ruff's value is used instead.
fn agrees(own: &[u32], ruff: &str) -> bool {
    let normalize = |text: &str| text.replace("\r\n", "\n").replace('\r', "\n");
    let own: String = own
        .iter()
        .map(|&unit| char::from_u32(unit).unwrap_or('\u{fffd}'))
        .collect();
    normalize(&own) == normalize(ruff)
}
