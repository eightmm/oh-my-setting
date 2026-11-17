//! Token pre-scan run before parsing: the limits CPython's tokenizer applies
//! and Ruff does not (bracket depth, indentation depth, over-long decimal
//! literals). Source over them is a syntax error, as for the legacy parser.
//!
//! It deliberately does not bound expression depth: host survival on hostile
//! input is the helper process boundary's job (`session.rs`), not a token
//! heuristic's.

use ruff_python_ast::token as lexemes;
use ruff_python_parser::{Mode, lexer::lex};

use lexemes::TokenKind;

/// CPython tokenizer limits (`MAXLEVEL`, `MAXINDENT`).
const MAX_BRACKET_DEPTH: usize = 200;
const MAX_INDENT_LEVELS: usize = 99;

/// CPython 3.12 refuses a decimal integer literal over 4300 digits
/// (`int_max_str_digits`); other bases, all-zero literals, underscores and
/// non-integer literals are exempt.
const MAX_DECIMAL_DIGITS: usize = 4300;

fn decimal_literal_too_long(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.first() == Some(&b'0') {
        return false;
    }
    bytes.iter().filter(|b| b.is_ascii_digit()).count() > MAX_DECIMAL_DIGITS
}

/// Whether CPython would refuse `text` before parsing it.
pub(crate) fn exceeds_cpython_limits(text: &str) -> bool {
    use TokenKind as K;
    let mut lexer = lex(text, Mode::Module);
    let (mut brackets, mut indents) = (0usize, 0usize);
    loop {
        match lexer.next_token() {
            K::EndOfFile => return false,
            K::Int => {
                let range = lexer.current_range();
                let literal = text
                    .get(range.start().to_usize()..range.end().to_usize())
                    .unwrap_or_default();
                if decimal_literal_too_long(literal) {
                    return true;
                }
            }
            K::Lpar | K::Lsqb | K::Lbrace => {
                if brackets >= MAX_BRACKET_DEPTH {
                    return true;
                }
                brackets += 1;
            }
            K::Rpar | K::Rsqb | K::Rbrace => brackets = brackets.saturating_sub(1),
            K::Indent => {
                if indents >= MAX_INDENT_LEVELS {
                    return true;
                }
                indents += 1;
            }
            K::Dedent => indents = indents.saturating_sub(1),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpython_bracket_and_indent_limits_are_inclusive_edges() {
        let nested = |depth: usize| format!("{}{}\n", "(".repeat(depth), ")".repeat(depth));
        assert!(!exceeds_cpython_limits(&nested(MAX_BRACKET_DEPTH)));
        assert!(exceeds_cpython_limits(&nested(MAX_BRACKET_DEPTH + 1)));
        assert!(!exceeds_cpython_limits(""));
        assert!(!exceeds_cpython_limits(")))\n"));
        let indented = |levels: usize| {
            let mut source = String::new();
            for level in 0..levels {
                source.push_str(&format!("{}if 1:\n", " ".repeat(level)));
            }
            source.push_str(&format!("{}g()\n", " ".repeat(levels)));
            source
        };
        assert!(!exceeds_cpython_limits(&indented(MAX_INDENT_LEVELS)));
        assert!(exceeds_cpython_limits(&indented(MAX_INDENT_LEVELS + 1)));
    }

    #[test]
    fn decimal_literals_over_4300_digits_are_refused() {
        let digits = |n: usize| format!("x = {}\n", "1".repeat(n));
        assert!(!exceeds_cpython_limits(&digits(MAX_DECIMAL_DIGITS)));
        assert!(exceeds_cpython_limits(&digits(MAX_DECIMAL_DIGITS + 1)));
        assert!(!exceeds_cpython_limits("x = 1\n"));
        let separated = |n: usize| format!("x = {}1\n", "1_".repeat(n - 1));
        assert!(!exceeds_cpython_limits(&separated(MAX_DECIMAL_DIGITS)));
        assert!(exceeds_cpython_limits(&separated(MAX_DECIMAL_DIGITS + 1)));
        for exempt in [
            format!("x = 0x{}\n", "1".repeat(5_000)),
            format!("x = 0O{}\n", "1".repeat(5_000)),
            format!("x = 0b{}\n", "1".repeat(5_000)),
            format!("x = {}\n", "0".repeat(5_000)),
            format!("x = {}.0\n", "1".repeat(5_000)),
            format!("x = {}j\n", "1".repeat(5_000)),
            format!("x = 1{}e1\n", "1".repeat(5_000)),
        ] {
            assert!(!exceeds_cpython_limits(&exempt), "{}", &exempt[..12]);
        }
        assert!(exceeds_cpython_limits(&format!(
            "def f():\n    return [{}]\n",
            "1".repeat(4_301)
        )));
    }
}
