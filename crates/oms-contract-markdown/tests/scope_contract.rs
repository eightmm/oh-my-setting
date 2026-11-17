//! Frozen from CPython 3.12.15 / Unicode 15.0.0 at f2dbaeb93522fb155e87f87c4f784a8d2c488c5d.
//! Reference: scripts/lib/path_scope.py (all 469 lines), scheduler.py (scope functions 1-91).
//! Native tests never start Python. Stream adapters use in-memory readers and
//! writers; file opening, metadata checks and public CLI activation stay outside.
use oms_contract_markdown::scope::*;
use std::io::{self, BufReader, Cursor, Read, Write};

#[test]
fn scope_stream_rule_rows() {
    let classifier = read_rules(
        b"repo\tignored\tmetadata\r\n\r\nallow\t./src/*.rs/\r\ndeny\tsrc/private*.rs".as_slice(),
    )
    .unwrap();
    assert_eq!(classifier.classify("src/a.rs"), Classification::Allowed);
    assert_eq!(
        classifier.classify("src/private.rs"),
        Classification::Forbidden
    );
    assert_eq!(classifier.classify("docs/a.rs"), Classification::Outside);
    for raw in [b"".as_slice(), b"\n\r\n", b"Allow\ta\nunknown\t\t\n"] {
        assert_eq!(
            read_rules(raw).unwrap().classify("anything"),
            Classification::Allowed
        );
    }
    assert_eq!(
        read_rules(b"allow\t \r\n".as_slice())
            .unwrap()
            .classify("anything"),
        Classification::Allowed
    );
    for (raw, message) in [
        (b"allow".as_slice(), "scope rule row is malformed"),
        (b"deny\t", "scope rule row is malformed"),
        (b"allow\ta\tb", "scope rule row is malformed"),
        (
            b"allow\t../a\ndeny\n",
            "scope rule must be a normalized repo-relative path",
        ),
        (
            b"allow\t[z-a]\ndeny\t../a\n",
            "scope rule must be a normalized repo-relative path",
        ),
        (b"allow\t[z-a]\ndeny\n", "scope rule row is malformed"),
        (b"deny\t../a\n\xff", "scope rule file must be UTF-8"),
        (
            b"allow\nignored\0",
            "scope rule file is invalid or too large",
        ),
        (b"\xff\0", "scope rule file is invalid or too large"),
        (
            b"allow\ta\r\r\n",
            "scope rule contains a control or format character",
        ),
    ] {
        assert_eq!(read_rules(raw).unwrap_err().to_string(), message);
    }
    assert!(
        read_rules(b"deny\t[[:bogus:]]\nallow\t[z-a]\n".as_slice())
            .unwrap_err()
            .to_string()
            .contains("descending")
    );
    assert!(read_rules(b"deny\t[z-a]".as_slice()).is_err());
}

#[test]
fn scope_stream_rule_bounds() {
    let mut bytes = b"allow\ta\n".to_vec();
    bytes.resize(MAX_RULE_FILE_BYTES, b'x');
    let mut input = Cursor::new(bytes);
    assert_eq!(
        read_rules(&mut input).unwrap().classify("b"),
        Classification::Outside
    );
    assert_eq!(input.position() as usize, MAX_RULE_FILE_BYTES);
    input.get_mut().extend_from_slice(b"extra unread bytes");
    input.set_position(0);
    assert_eq!(
        read_rules(&mut input).unwrap_err().to_string(),
        "scope rule file is invalid or too large"
    );
    assert_eq!(input.position() as usize, MAX_RULE_FILE_BYTES + 1);

    let mut rows = b"allow\ta\n".repeat(MAX_RULES);
    rows.extend_from_slice(b"ignored\t\t\n");
    assert!(read_rules(rows.as_slice()).is_ok());
    for (extra, message) in [
        (
            b"allow\ta\n".as_slice(),
            "scope rule file has too many rules",
        ),
        (b"deny\t../a\n", "scope rule file has too many rules"),
        (b"deny\t\n", "scope rule row is malformed"),
    ] {
        let mut oversized = rows.clone();
        oversized.extend_from_slice(extra);
        assert_eq!(
            read_rules(oversized.as_slice()).unwrap_err().to_string(),
            message
        );
    }
}

#[test]
fn scope_stream_raw_rows_and_partial_output() {
    let classifier = read_rules(b"allow\tsrc/*\ndeny\tsrc/private*\n".as_slice()).unwrap();
    let paths = b"\n\r\nsrc/a\r\nsrc/private\n./src/a\nsrc\\a\nsrc/\xff\0\t\rsuffix\nlast\r";
    let expected = b"allowed\tsrc/a\nforbidden\tsrc/private\noutside\t./src/a\noutside\tsrc\\a\nallowed\tsrc/\xff\0\t\rsuffix\noutside\tlast\n";
    for capacity in [1, 2, 3, 4096] {
        let input = BufReader::with_capacity(capacity, paths.as_slice());
        let mut output = Vec::new();
        assert_eq!(classifier.classify_stream(input, &mut output).unwrap(), 6);
        assert_eq!(output, expected);
    }
    let classifier = Classifier::new(&["?"], &[]).unwrap();
    for (raw, class) in [
        (b"\xff".as_slice(), "allowed"),
        (b"\xed\xa0\x80", "outside"),
        ("é".as_bytes(), "allowed"),
        (b"\xf0\x80\x80\x80", "outside"),
        ("\u{fffd}".as_bytes(), "allowed"),
    ] {
        let mut output = Vec::new();
        let input = BufReader::with_capacity(1, raw);
        assert_eq!(classifier.classify_stream(input, &mut output).unwrap(), 1);
        let mut expected = format!("{class}\t").into_bytes();
        expected.extend_from_slice(raw);
        expected.push(b'\n');
        assert_eq!(output, expected);
    }
    let classifier = Classifier::new(&[], &[]).unwrap();
    for raw in [b"".as_slice(), b"\n\r\n\r"] {
        let mut output = Vec::new();
        assert_eq!(classifier.classify_stream(raw, &mut output).unwrap(), 0);
        assert!(output.is_empty());
    }
    for path in [
        vec![b'a'; MAX_SCOPE_ITEM_BYTES],
        "é".repeat(2048).into_bytes(),
        {
            let mut raw = vec![255; 1365];
            raw.push(b'a');
            raw
        },
    ] {
        let mut input = path.clone();
        input.extend_from_slice(b"\r\n");
        let mut output = Vec::new();
        assert_eq!(
            classifier
                .classify_stream(input.as_slice(), &mut output)
                .unwrap(),
            1
        );
        let mut expected = b"allowed\t".to_vec();
        expected.extend_from_slice(&path);
        expected.push(b'\n');
        assert_eq!(output, expected);
    }
    for oversized in [vec![b'a'; MAX_SCOPE_ITEM_BYTES + 10000], vec![255; 1366]] {
        let mut input = b"ok\n".to_vec();
        input.extend_from_slice(&oversized);
        let mut input = Cursor::new(input);
        let mut output = Vec::new();
        let err = classifier
            .classify_stream(&mut input, &mut output)
            .unwrap_err();
        assert!(matches!(err, ScopeStreamError::Scope(_)));
        assert!(std::error::Error::source(&err).is_some());
        assert_eq!(
            err.to_string(),
            "changed path exceeds the 4096-byte UTF-8 limit"
        );
        assert_eq!(output, b"allowed\tok\n");
        assert!(input.position() as usize <= 3 + MAX_SCOPE_ITEM_BYTES + 3);
    }
}

#[test]
fn scope_stream_path_count() {
    let classifier = Classifier::new(&[], &[]).unwrap();
    let mut input = b"a\n".repeat(MAX_CLASSIFY_PATHS);
    input.extend_from_slice(b"\n\r\n");
    assert_eq!(
        classifier
            .classify_stream(input.as_slice(), io::sink())
            .unwrap(),
        MAX_CLASSIFY_PATHS
    );
    for extra in [b"a".to_vec(), vec![b'a'; MAX_SCOPE_ITEM_BYTES + 3]] {
        let mut oversized = input.clone();
        oversized.extend_from_slice(&extra);
        let mut output = Vec::new();
        assert_eq!(
            classifier
                .classify_stream(oversized.as_slice(), &mut output)
                .unwrap_err()
                .to_string(),
            "scope path stream has too many entries"
        );
        assert_eq!(output.len(), b"allowed\ta\n".len() * MAX_CLASSIFY_PATHS);
        assert!(
            output
                .as_chunks::<{ b"allowed\ta\n".len() }>()
                .0
                .iter()
                .all(|row| row == b"allowed\ta\n")
        );
    }
}

#[test]
fn scope_stream_io_failures() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("test read failure"))
        }
    }
    assert_eq!(
        read_rules(Broken).unwrap_err().to_string(),
        "cannot read scope rule file"
    );
    assert_eq!(
        read_rules(b"allow\ta\n".as_slice().chain(Broken))
            .unwrap_err()
            .to_string(),
        "cannot read scope rule file"
    );
    let classifier = Classifier::new(&[], &[]).unwrap();
    let input = BufReader::with_capacity(1, b"a\n".as_slice().chain(Broken));
    let mut output = Vec::new();
    let err = classifier.classify_stream(input, &mut output).unwrap_err();
    assert!(matches!(&err, ScopeStreamError::Io(inner) if inner.kind() == io::ErrorKind::Other));
    assert_eq!(err.to_string(), "test read failure");
    assert!(std::error::Error::source(&err).is_some());
    assert_eq!(output, b"allowed\ta\n");

    struct ShortWriter {
        bytes: Vec<u8>,
        remaining: usize,
    }
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "test write failure",
                ));
            }
            let size = bytes.len().min(self.remaining).min(2);
            self.bytes.extend_from_slice(&bytes[..size]);
            self.remaining -= size;
            Ok(size)
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("flushing belongs to the consumer");
        }
    }
    for remaining in [0, 10, 13, 20] {
        let mut output = ShortWriter {
            bytes: Vec::new(),
            remaining,
        };
        let mut input = Cursor::new(b"a\nb\nc\n");
        let err = classifier
            .classify_stream(&mut input, &mut output)
            .unwrap_err();
        assert!(
            matches!(&err, ScopeStreamError::Io(inner) if inner.kind() == io::ErrorKind::BrokenPipe)
        );
        assert_eq!(err.to_string(), "test write failure");
        assert!(std::error::Error::source(&err).is_some());
        assert_eq!(
            output.bytes,
            b"allowed\ta\nallowed\tb\nallowed\tc\n"[..remaining]
        );
        assert_eq!(
            input.position(),
            if remaining < 10 {
                2
            } else if remaining < 20 {
                4
            } else {
                6
            }
        );
    }
    let mut output = ShortWriter {
        bytes: Vec::new(),
        remaining: 30,
    };
    assert_eq!(
        classifier
            .classify_stream(b"a\nb\nc\n".as_slice(), &mut output)
            .unwrap(),
        3
    );
    assert_eq!(output.bytes, b"allowed\ta\nallowed\tb\nallowed\tc\n");
    let err = classifier
        .classify_stream(b"a\n".as_slice(), Cursor::new([]))
        .unwrap_err();
    assert!(
        matches!(&err, ScopeStreamError::Io(inner) if inner.kind() == io::ErrorKind::WriteZero)
    );
}

#[test]
fn byte_bounds_controls_and_codepoints() {
    assert_eq!(normalize(&"a".repeat(4096)).unwrap().len(), 4096);
    assert_eq!(normalize(&"é".repeat(2048)).unwrap().len(), 4096);
    assert_eq!(normalize(&"😀".repeat(1024)).unwrap().len(), 4096);
    for raw in [
        "a".repeat(4097),
        "é".repeat(2049),
        "😀".repeat(1025),
        format!("\0{}", "x".repeat(4096)),
    ] {
        assert_eq!(
            normalize(&raw).unwrap_err().to_string(),
            "scope path exceeds the 4096-byte UTF-8 limit"
        );
    }
    assert_eq!(
        normalize_with_label("../bad", "scope rule")
            .unwrap_err()
            .to_string(),
        "scope rule must be a normalized repo-relative path"
    );
    assert_eq!(normalize_codepoints(&[], "scope rule").unwrap(), ".");
    for point in [0, 0x1f, 0x7f, 0x85, 0xad, 0x200d, 0xd800, 0xdcff, 0xdfff] {
        assert_eq!(
            normalize_codepoints(&[point], "scope rule")
                .unwrap_err()
                .to_string(),
            "scope rule contains a control or format character"
        );
    }
    let mut points = vec![0xdcff; 1365];
    assert_eq!(
        normalize_codepoints(&points, "scope rule")
            .unwrap_err()
            .to_string(),
        "scope rule contains a control or format character"
    );
    points.push(97);
    assert_eq!(
        normalize_codepoints(&points, "scope rule")
            .unwrap_err()
            .to_string(),
        "scope rule contains a control or format character"
    );
    points.push(97);
    assert_eq!(
        normalize_codepoints(&points, "scope rule")
            .unwrap_err()
            .to_string(),
        "scope rule exceeds the 4096-byte UTF-8 limit"
    );
    assert_eq!(
        normalize_codepoints(&[0x110000], "scope rule")
            .unwrap_err()
            .to_string(),
        "scope input contains an invalid Unicode codepoint"
    );
    assert_eq!(
        normalize_codepoints(&[0x1f600], "scope rule").unwrap(),
        "😀"
    );
    assert_eq!(unicode_general_category::UNICODE_VERSION, (15, 0, 0));
}

#[test]
fn grammar_budgets_and_validation_order() {
    assert!(validate_patterns(&[]).unwrap().is_empty());
    assert!(!matches_any("x", &[]).unwrap());
    assert!(!has_glob("a\\b}"));
    assert!(has_glob("["));
    assert!(validate_pattern("").unwrap().is_match(""));
    assert!(validate_pattern("[]").unwrap().is_match("[]"));
    assert!(validate_pattern("[a-a]").unwrap().is_match("a"));
    assert!(
        validate_pattern("[a-\u{1060}]")
            .unwrap()
            .is_match("\u{1060}")
    );
    assert!(
        !validate_pattern("[a-\u{1060}]")
            .unwrap()
            .is_match("\u{1061}")
    );
    assert!(
        validate_pattern("[a-\u{860}][a-\u{860}]")
            .unwrap()
            .is_match("aa")
    );
    assert!(validate_pattern("[a-\u{860}][a-\u{861}]").is_err());
    assert!(validate_pattern(&format!("[{}]", "a".repeat(4094))).is_ok());
    // The item-byte bound dominates bracket count; expansion bounds unique sets.
    assert_eq!(
        validate_pattern(&format!("[{}]", "a".repeat(4095)))
            .unwrap_err()
            .to_string(),
        "scope pattern exceeds the 4096-byte UTF-8 limit"
    );
    assert!(validate_pattern(&"*".repeat(2340)).is_ok());
    assert_eq!(
        validate_pattern(&"*".repeat(2341)).unwrap_err().to_string(),
        "scope pattern regex output is too large"
    );
    assert!(validate_pattern(&format!("{}ab", "?".repeat(2730))).is_err());
    assert!(validate_pattern(&"?".repeat(2730)).is_ok());
    assert!(validate_pattern(&format!("[{}]", "[:ascii:]".repeat(32))).is_ok());
    assert_eq!(
        validate_pattern(&format!("[{}]", "[:ascii:]".repeat(33)))
            .unwrap_err()
            .to_string(),
        "scope pattern bracket expansion is too large"
    );
    let exact = format!("{}[[:ascii:]]aaaa", "*".repeat(2306));
    assert!(validate_pattern(&exact).is_ok());
    assert_eq!(
        validate_pattern(&format!("{exact}a"))
            .unwrap_err()
            .to_string(),
        "scope pattern regex output is too large"
    );
    assert_eq!(
        validate_pattern(&"a".repeat(4097)).unwrap_err().to_string(),
        "scope pattern exceeds the 4096-byte UTF-8 limit"
    );
    // Standalone literal matching preserves the reference's validation bypass.
    let large = "a".repeat(4097);
    assert!(matches(&large, &large).unwrap());
    assert!(matches_any(&large, &[&large]).is_err());
    assert!(validate_pattern("\0").unwrap().is_match("\0"));
    assert!(
        classify("x", &["[z-a]"], &["[[:bogus:]]"])
            .unwrap_err()
            .to_string()
            .contains("descending")
    );
    assert!(
        within_envelope("[z-a]", &["[[:bogus:]]"])
            .unwrap_err()
            .to_string()
            .contains("descending")
    );
    let ambiguous = format!("{}z", "*a".repeat(1000));
    assert!(!matches(&"a".repeat(2000), &ambiguous).unwrap());
}

#[test]
fn supplied_rules_and_raw_batch_limits() {
    let rules = [
        Rule {
            kind: RuleKind::Allow,
            path: "./src/",
        },
        Rule {
            kind: RuleKind::Deny,
            path: "src/private",
        },
    ];
    let classifier = validate_rules(&rules).unwrap();
    assert_eq!(classifier.classify("src/a"), Classification::Allowed);
    assert_eq!(
        classifier.classify("src/private/a"),
        Classification::Forbidden
    );
    assert_eq!(classifier.classify("docs/a"), Classification::Outside);
    let empty = validate_rules(&[]).unwrap();
    assert_eq!(empty.classify("anything"), Classification::Allowed);
    let many = vec![
        Rule {
            kind: RuleKind::Allow,
            path: "a"
        };
        8192
    ];
    assert!(validate_rules(&many).is_ok());
    let mut too_many = many;
    too_many.push(Rule {
        kind: RuleKind::Deny,
        path: "../x",
    });
    assert_eq!(
        validate_rules(&too_many).unwrap_err().to_string(),
        "scope rule file has too many rules"
    );
    let wrong = [
        Rule {
            kind: RuleKind::Allow,
            path: "[z-a]",
        },
        Rule {
            kind: RuleKind::Deny,
            path: "../x",
        },
    ];
    assert_eq!(
        validate_rules(&wrong).unwrap_err().to_string(),
        "scope rule must be a normalized repo-relative path"
    );
    assert!(
        validate_rules(&[Rule {
            kind: RuleKind::Allow,
            path: "[z-a]"
        }])
        .is_err()
    );
    assert!(
        validate_rules(&[Rule {
            kind: RuleKind::Deny,
            path: "[z-a]"
        }])
        .is_err()
    );
    let path_lines: &[&[u8]] = &[b"\n", b"src/a\r\n", b"./src/a\n", b"src\\a\n", b"\xff\n"];
    assert_eq!(
        classifier
            .classify_changed_paths(path_lines.iter().copied())
            .unwrap(),
        vec![
            (Classification::Allowed, b"src/a".as_slice()),
            (Classification::Outside, b"./src/a".as_slice()),
            (Classification::Outside, b"src\\a".as_slice()),
            (Classification::Outside, b"\xff".as_slice()),
        ]
    );
    assert!(empty.classify_changed_paths([]).unwrap().is_empty());
    assert!(
        empty
            .classify_changed_paths(std::iter::repeat_n(b"\r\n".as_slice(), 100001))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        empty
            .classify_changed_paths(std::iter::repeat_n(b"a".as_slice(), 100000))
            .unwrap()
            .len(),
        100000
    );
    assert_eq!(
        empty
            .classify_changed_paths(std::iter::repeat_n(b"a".as_slice(), 100001))
            .unwrap_err()
            .to_string(),
        "scope path stream has too many entries"
    );
    let oversized = vec![b'a'; 4097];
    assert_eq!(
        empty
            .classify_changed_paths(
                std::iter::repeat_n(b"a".as_slice(), 100000)
                    .chain(std::iter::once(oversized.as_slice()))
            )
            .unwrap_err()
            .to_string(),
        "scope path stream has too many entries"
    );
    assert_eq!(
        empty.classify_codepoints(&[]).unwrap(),
        Classification::Allowed
    );
    assert!(empty.classify_codepoints(&[0x110000]).is_err());
    assert_eq!(
        empty.classify_codepoints(&[0; 4096]).unwrap(),
        Classification::Allowed
    );
    assert_eq!(
        empty.classify_changed_path(&vec![b'a'; 4096]).unwrap(),
        Some(Classification::Allowed)
    );
    assert_eq!(
        empty
            .classify_changed_path(&vec![b'a'; 4097])
            .unwrap_err()
            .to_string(),
        "changed path exceeds the 4096-byte UTF-8 limit"
    );
    let mut raw = vec![255; 1365];
    raw.push(b'a');
    assert_eq!(
        empty.classify_changed_path(&raw).unwrap(),
        Some(Classification::Allowed)
    );
    raw.push(b'a');
    assert_eq!(
        empty.classify_changed_path(&raw).unwrap_err().to_string(),
        "changed path exceeds the 4096-byte UTF-8 limit"
    );
    assert!(
        empty
            .classify_changed_paths([vec![b'a'; 4097].as_slice()])
            .is_err()
    );
    assert_eq!(
        normalize_scope_default(&"a".repeat(4097)).unwrap_err(),
        DefaultScopeError::Invalid
    );
}

#[test]
fn reference_normalization() {
    for (raw, expected, message) in [
        ("", ".", ""),
        (" ", ".", ""),
        ("/", ".", ""),
        ("//", ".", ""),
        ("///", ".", ""),
        ("./", ".", ""),
        ("././", ".", ""),
        ("././src\\x///", "src/x", ""),
        ("a\\b", "a/b", ""),
        ("a/b/", "a/b", ""),
        (
            "a//b",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        (
            "a/./b",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        (
            "a/../b",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        (
            "..",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        (
            "../x",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        (
            "/abs",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        (
            "\\\\host\\share",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        (
            "C:relative",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        (
            "z:",
            "",
            "scope path must be a normalized repo-relative path",
        ),
        ("1:x", "1:x", ""),
        ("\u{e9}:x", "\u{e9}:x", ""),
        ("a b", "a b", ""),
        ("\u{a0}a\u{2003}", "a", ""),
        ("a\u{2028}b", "a\u{2028}b", ""),
        (
            "\u{d55c}\u{ae00}/\u{d30c}\u{c77c}",
            "\u{d55c}\u{ae00}/\u{d30c}\u{c77c}",
            "",
        ),
        (
            "\u{0}",
            "",
            "scope path contains a control or format character",
        ),
        (
            "\t x ",
            "",
            "scope path contains a control or format character",
        ),
        (
            "\u{1c}a",
            "",
            "scope path contains a control or format character",
        ),
        (
            "a\u{200d}b",
            "",
            "scope path contains a control or format character",
        ),
        (
            "\u{ad}",
            "",
            "scope path contains a control or format character",
        ),
        (
            "\u{61c}",
            "",
            "scope path contains a control or format character",
        ),
        (
            "\u{2066}",
            "",
            "scope path contains a control or format character",
        ),
        (
            "\u{e0001}",
            "",
            "scope path contains a control or format character",
        ),
        (
            "\u{1343f}",
            "",
            "scope path contains a control or format character",
        ),
        ("\u{378}", "\u{378}", ""),
        ("\u{10ffff}", "\u{10ffff}", ""),
    ] {
        if message.is_empty() {
            assert_eq!(normalize(raw).unwrap(), expected, "{raw:?}");
        } else {
            assert_eq!(normalize(raw).unwrap_err().to_string(), message, "{raw:?}");
        }
    }
}

#[test]
fn reference_pattern_matrix() {
    let paths = [
        "",
        ".",
        "src",
        "src/a",
        "src/a.py",
        "src/sub/a.py",
        "src/.a.py",
        "src/a.py/child",
        "src/a.py\n",
        "docs/a.md",
        "a",
        "b",
        "c",
        "-",
        "]",
        "[",
        "^",
        "1",
        "9",
        "_",
        ":",
        "\\",
        " ",
        "\t",
        "\n",
        "a\nb",
        "ab",
        "acb",
        "a/b",
        "abc",
        "axbyc",
        "[]",
        "[!]",
        "\u{e9}",
        "\u{e9}/x",
        "\u{d55c}",
        "\u{fffd}",
        "\u{200d}",
        "\u{0}",
        "&",
        "|",
        "~",
        "#",
        "a-b",
        "[a",
        "a.py",
    ];
    for (pattern, bits) in [
        ("", "1000000000000000000000000000000000000000000000"),
        (".", "1111111111111111111111111111111111111111111111"),
        ("src", "0011111110000000000000000000000000000000000000"),
        ("src/*.py", "0000111000000000000000000000000000000000000000"),
        ("**/*.py", "0000111000000000000000000000000000000000000000"),
        ("src/**", "0001111110000000000000000000000000000000000000"),
        ("*.py", "0000111000000000000000000000000000000000000001"),
        ("?", "0100000000111111111111111000000001011111111000"),
        ("*", "1111111111111111111111111111111111111111111111"),
        ("**", "1111111111111111111111111111111111111111111111"),
        ("a**b", "0000000000000000000000000111100000000000000100"),
        ("*a*b*c", "0000000000000000000000000000011000000000000000"),
        ("[]", "0000000000000000000000000000000100000000000000"),
        ("[!]", "0000000000000000000000000000000010000000000000"),
        ("[a", "0000000000000000000000000000000000000000000010"),
        ("[", "0000000000000001000000000000000000000000000000"),
        ("[[]", "0000000000000001000000000000000000000000000000"),
        ("[]a]", "0000000000100010000000000000000000000000000000"),
        ("[!]]", "0100000000111101111111111000000001011111111000"),
        ("[^]]", "0100000000111101111111111000000001011111111000"),
        ("[a-cx-z]", "0000000000111000000000000000000000000000000000"),
        ("[a-]", "0000000000100100000000000000000000000000000000"),
        ("[-a]", "0000000000100100000000000000000000000000000000"),
        ("[a-b-c]", "0000000000111100000000000000000000000000000000"),
        (
            "[[:digit:]]",
            "0000000000000000011000000000000000000000000000",
        ),
        (
            "[[:digit:]",
            "0000000000000000000000000000000000000000000000",
        ),
        (
            "[[:alpha:]_-]",
            "0000000000111100000100000000000000000000000000",
        ),
        (
            "[![:space:]]",
            "0100000000111111111111000000000001011111111000",
        ),
        (
            "[[:word:]]",
            "0000000000111000011100000000000000000000000000",
        ),
        (
            "[a[:digit:]b]",
            "0000000000110000011000000000000000000000000000",
        ),
        ("[!a]", "0100000000011111111111111000000001011111111000"),
        ("[[.a", "0000000000000000000000000000000000000000000000"),
        ("[[=a", "0000000000000000000000000000000000000000000000"),
        ("\\*", "0000000000000000000001000000000000000000000000"),
        ("a?b", "0000000000000000000000000101100000000000000100"),
        ("\n*", "0000000000000000000000001000000000000000000000"),
        ("[\\^]", "0000000000000000100001000000000000000000000000"),
        ("[&|~#]", "0000000000000000000000000000000000000001111000"),
        (
            "[\u{e9}-\u{ea}]",
            "0000000000000000000000000000000001000000000000",
        ),
        (
            "[^\u{e9}]",
            "0100000000111111111111111000000000011111111000",
        ),
        (
            "[\u{0}-\u{1f}]",
            "0000000000000000000000011000000000000010000000",
        ),
        ("a*b", "0000000000000000000000000111100000000000000100"),
        ("*?", "0111111111111111111111111111111111111111111111"),
        ("?*", "0111111111111111111111111111111111111111111111"),
        ("[a-a]", "0000000000100000000000000000000000000000000000"),
        ("[.-/]", "0100000000000000000000000000000000000000000000"),
        ("\u{e9}/*", "0000000000000000000000000000000000100000000000"),
    ] {
        let compiled = validate_pattern(pattern).unwrap();
        assert_eq!(compiled.as_str(), pattern);
        for (path, bit) in paths.iter().zip(bits.bytes()) {
            assert_eq!(
                matches(path, pattern).unwrap(),
                bit == b'1',
                "{path:?} / {pattern:?}"
            );
            assert_eq!(compiled.is_match(path), bit == b'1');
        }
    }
}

#[test]
fn reference_posix_classes() {
    for (name, members) in [
        (
            "alnum",
            "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
        ),
        (
            "alpha",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
        ),
        (
            "ascii",
            "\u{0}\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}\u{8}\t\n\u{b}\u{c}\r\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{14}\u{15}\u{16}\u{17}\u{18}\u{19}\u{1a}\u{1b}\u{1c}\u{1d}\u{1e}\u{1f} !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~\u{7f}",
        ),
        ("blank", "\t "),
        (
            "cntrl",
            "\u{0}\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}\u{8}\t\n\u{b}\u{c}\r\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{14}\u{15}\u{16}\u{17}\u{18}\u{19}\u{1a}\u{1b}\u{1c}\u{1d}\u{1e}\u{1f}\u{7f}",
        ),
        ("digit", "0123456789"),
        (
            "graph",
            "!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~",
        ),
        ("lower", "abcdefghijklmnopqrstuvwxyz"),
        (
            "print",
            " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~",
        ),
        ("punct", "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"),
        ("space", "\t\n\u{b}\u{c}\r "),
        ("upper", "ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
        (
            "word",
            "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz",
        ),
        ("xdigit", "0123456789ABCDEFabcdef"),
    ] {
        let positive = validate_pattern(&format!("[[:{name}:]]")).unwrap();
        let negative = validate_pattern(&format!("[![:{name}:]]")).unwrap();
        for point in 0..=127u8 {
            let c = char::from(point);
            assert_eq!(
                positive.is_match(&c.to_string()),
                members.contains(c),
                "{name} {point}"
            );
            assert_eq!(negative.is_match(&c.to_string()), !members.contains(c));
        }
        assert!(!positive.is_match("é"));
        assert!(negative.is_match("é"));
    }
}

#[test]
fn reference_validation_errors() {
    assert_eq!(
        matches("x", "[[:x]y:]").unwrap_err().to_string(),
        "scope pattern has an unsupported POSIX class"
    );
    for (pattern, message) in [
        (
            "[[:bogus:]]",
            "scope pattern has an unsupported POSIX class",
        ),
        ("[[:bogus:]", "scope pattern has an unsupported POSIX class"),
        ("[z-a]", "scope pattern has a descending bracket range"),
        (
            "[[.a.]]",
            "scope pattern has an unsupported collating expression",
        ),
        (
            "[[=a=]]",
            "scope pattern has an unsupported collating expression",
        ),
        ("[a-\u{1061}]", "scope pattern bracket range is too large"),
        (
            "[a-\u{1060}a-\u{1060}]",
            "scope pattern bracket expansion is too large",
        ),
        (
            "[a-\u{1060}][a-\u{1060}]",
            "scope pattern total bracket expansion is too large",
        ),
    ] {
        assert_eq!(validate_pattern(pattern).unwrap_err().to_string(), message);
    }
    assert!(validate_patterns(&["src", "[z-a]"]).is_err());
    assert!(matches_any("src/a", &["src", "[z-a]"]).is_err());
    assert!(within_envelope("src/a", &["src", "[z-a]"]).is_err());
    assert!(within_envelope("[z-a]", &["[z-a]"]).is_err());
    assert!(classify("src/a", &["[z-a]"], &["."]).is_err());
    assert!(classify("src/a", &["."], &["[z-a]"]).is_err());
}

#[test]
fn reference_lists_and_defaults() {
    assert_eq!(
        normalize_list("").unwrap_err().to_string(),
        "scope list is empty"
    );
    assert_eq!(
        normalize_list(", \t").unwrap_err().to_string(),
        "scope list is empty"
    );
    assert_eq!(normalize_list("b,./a,a").unwrap(), vec!["a", "b"]);
    assert_eq!(
        normalize_list("src\u{2003}docs").unwrap(),
        vec!["docs", "src"]
    );
    assert_eq!(
        normalize_list("\u{e9},\u{d55c},\u{e9}").unwrap(),
        vec!["\u{e9}", "\u{d55c}"]
    );
    assert_eq!(normalize_list("a\u{1c}b").unwrap(), vec!["a", "b"]);
    assert_eq!(normalize_list("a\u{85}b").unwrap(), vec!["a", "b"]);
    assert_eq!(
        normalize_list("[z-a]").unwrap_err().to_string(),
        "scope pattern has a descending bracket range"
    );
    assert_eq!(
        normalize_list("a//b").unwrap_err().to_string(),
        "scope path must be a normalized repo-relative path"
    );
    assert_eq!(normalize_scope_default("").unwrap_err().exit_code(), 3);
    assert_eq!(normalize_scope_default(" \t").unwrap_err().exit_code(), 3);
    assert_eq!(normalize_scope_default("a,").unwrap_err().exit_code(), 2);
    assert_eq!(normalize_scope_default(".").unwrap_err().exit_code(), 4);
    assert_eq!(normalize_scope_default("./").unwrap_err().exit_code(), 4);
    assert_eq!(normalize_scope_default("/").unwrap_err().exit_code(), 4);
    assert_eq!(normalize_scope_default("a,.").unwrap_err().exit_code(), 4);
    assert_eq!(
        normalize_scope_default("`src`, docs").unwrap(),
        vec!["docs", "src"]
    );
    assert_eq!(normalize_scope_default("a b").unwrap_err().exit_code(), 5);
    assert_eq!(normalize_scope_default("a()").unwrap_err().exit_code(), 5);
    assert_eq!(normalize_scope_default("a*").unwrap_err().exit_code(), 5);
    assert_eq!(normalize_scope_default("``").unwrap_err().exit_code(), 5);
    assert_eq!(normalize_scope_default("`a").unwrap_err().exit_code(), 5);
    assert_eq!(normalize_scope_default("a`").unwrap_err().exit_code(), 5);
    assert_eq!(
        normalize_scope_default("C:relative")
            .unwrap_err()
            .exit_code(),
        2
    );
    assert_eq!(normalize_scope_default("a//b").unwrap_err().exit_code(), 2);
    assert_eq!(
        normalize_scope_default("\u{b2},\u{2163},\u{11f02}").unwrap(),
        vec!["\u{b2}", "\u{2163}", "\u{11f02}"]
    );
    assert_eq!(
        normalize_scope_default("\u{1c89}").unwrap_err().exit_code(),
        5
    );
    assert_eq!(
        normalize_scope_default("\u{10d40}")
            .unwrap_err()
            .exit_code(),
        5
    );
    assert_eq!(
        normalize_scope_default("\u{301}").unwrap_err().exit_code(),
        5
    );
    assert_eq!(
        normalize_scope_default("\u{200d}").unwrap_err().exit_code(),
        5
    );
    assert_eq!(normalize_scope_default("a:b").unwrap_err().exit_code(), 2);
    assert_eq!(normalize_scope_default("a_b/c-d").unwrap(), vec!["a_b/c-d"]);
}

#[test]
fn reference_envelopes_and_classification() {
    assert!(within_envelope("src/*.py", &["src"]).unwrap());
    assert!(within_envelope("src/a.py", &["src/*.py"]).unwrap());
    assert!(within_envelope("src/[ab].py", &["src/[ab].py"]).unwrap());
    assert!(!within_envelope("src/[a].py", &["src/???.py"]).unwrap());
    assert!(!within_envelope("src/a", &[]).unwrap());
    assert!(within_envelope("src", &["."]).unwrap());
    assert!(!within_envelope("src/*", &["*"]).unwrap());
    assert!(within_envelope("a/b", &["a"]).unwrap());
    assert!(!within_envelope("a", &["a/b"]).unwrap());
    assert!(within_envelope("a", &["?"]).unwrap());
    assert_eq!(
        classify("src/private.py", &["src/*.py"], &["src/private.py"])
            .unwrap()
            .as_str(),
        "forbidden"
    );
    assert_eq!(
        classify("src/sub/a.py", &["src/*.py"], &[])
            .unwrap()
            .as_str(),
        "allowed"
    );
    assert_eq!(
        classify("src/a.txt", &["src/*.py"], &[]).unwrap().as_str(),
        "outside"
    );
    assert_eq!(classify("else", &[], &[]).unwrap().as_str(), "allowed");
    assert_eq!(classify("a", &[], &["a"]).unwrap().as_str(), "forbidden");
    assert_eq!(
        classify("./src/a", &["src"], &[]).unwrap().as_str(),
        "outside"
    );
    assert_eq!(
        classify("src\\a", &["src"], &[]).unwrap().as_str(),
        "outside"
    );
    assert_eq!(
        classify("src/a\n", &["src"], &[]).unwrap().as_str(),
        "allowed"
    );
}

#[test]
fn reference_overlap() {
    assert!(scopes_overlap(Some(&["src"]), Some(&["src"])));
    assert!(scopes_overlap(Some(&["src"]), Some(&["src/app"])));
    assert!(scopes_overlap(Some(&["src/app"]), Some(&["src"])));
    assert!(!scopes_overlap(Some(&["src"]), Some(&["docs"])));
    assert!(!scopes_overlap(
        Some(&["delegated.txt"]),
        Some(&["delegated2.txt"])
    ));
    assert!(scopes_overlap(Some(&["src/*.py"]), Some(&["src"])));
    assert!(scopes_overlap(Some(&["src"]), Some(&["src/*.py"])));
    assert!(scopes_overlap(
        Some(&["src/lib/*.py"]),
        Some(&["src/lib/a.py"])
    ));
    assert!(!scopes_overlap(Some(&["src/*.py"]), Some(&["docs"])));
    assert!(scopes_overlap(Some(&["src/*.py"]), Some(&["src/**"])));
    assert!(!scopes_overlap(Some(&["src/*.py"]), Some(&["docs/*.md"])));
    assert!(scopes_overlap(Some(&["*.py"]), Some(&["docs/*.md"])));
    assert!(scopes_overlap(Some(&[]), Some(&["src"])));
    assert!(scopes_overlap(Some(&["src"]), Some(&[])));
    assert!(scopes_overlap(Some(&[]), Some(&[])));
    assert!(scopes_overlap(Some(&["../etc"]), Some(&["src"])));
    assert!(scopes_overlap(Some(&["/abs/path"]), Some(&["src"])));
    assert!(scopes_overlap(Some(&["src/li*b/x"]), Some(&["src/lip/a"])));
    assert!(!scopes_overlap(Some(&["src/li*b/x"]), Some(&["docs/x"])));
    assert!(scopes_overlap(Some(&["./src/"]), Some(&["src/a"])));
    assert!(scopes_overlap(Some(&["."]), Some(&["docs"])));
    assert!(scopes_overlap(Some(&[""]), Some(&["docs"])));
    assert!(scopes_overlap(Some(&["src/[a]"]), Some(&["src/[b]"])));
    assert!(scopes_overlap(Some(&["src", "docs"]), Some(&["docs/x"])));
    assert!(scopes_overlap(
        Some(&["\u{e9}/*.rs"]),
        Some(&["\u{e9}/a.txt"])
    ));
    assert!(scopes_overlap(None, Some(&["src"])));
    assert!(scopes_overlap(Some(&["src"]), None));
    assert!(scopes_overlap(None, None));
    // Only these two fixtures differ: the f2dbaeb93522 Python scheduler returns
    // false after comparing glob prefixes without grammar validation; native
    // validation conservatively returns true for these malformed globs.
    assert!(scopes_overlap(Some(&["src/[z-a]"]), Some(&["docs/*"])));
    assert!(scopes_overlap(
        Some(&["src/[[:bogus:]]"]),
        Some(&["docs/*"])
    ));

    // This mixed literal/glob case and the invalid inputs below already return
    // true in the f2dbaeb93522 Python scheduler as well as natively.
    assert!(scopes_overlap(Some(&["src/[z-a]"]), Some(&["docs"])));
    for invalid in [
        "a\u{200d}".to_owned(),
        "a".repeat(4097),
        format!("src/{}", "*".repeat(2341)),
        "src/[a-\u{1060}][a-\u{1060}]".to_owned(),
    ] {
        assert!(scopes_overlap(Some(&["src", &invalid]), Some(&["docs"])));
    }
}

#[test]
fn reference_raw_bytes() {
    for (raw, expected) in [
        (b"".as_slice(), b"".as_slice()),
        (b"\n".as_slice(), b"".as_slice()),
        (b"\r\n".as_slice(), b"".as_slice()),
        (b"a".as_slice(), b"a".as_slice()),
        (b"a\n".as_slice(), b"a".as_slice()),
        (b"a\r".as_slice(), b"a".as_slice()),
        (b"a\r\n".as_slice(), b"a".as_slice()),
        (b"a\n\n".as_slice(), b"a\n".as_slice()),
        (b"a\r\r\n".as_slice(), b"a\r".as_slice()),
        (b"a\n\r".as_slice(), b"a\n".as_slice()),
        (b"./a\\b/\r\n".as_slice(), b"./a\\b/".as_slice()),
        (
            b"\xff\0\xed\xa0\x80\r\n".as_slice(),
            b"\xff\0\xed\xa0\x80".as_slice(),
        ),
    ] {
        let cleaned = changed_line(raw);
        assert_eq!(cleaned, expected);
        assert_eq!(cleaned.as_ptr(), raw.as_ptr());
    }
    assert!(std::str::from_utf8(changed_line(b"\xff\r\n")).is_err());
    let classifier = Classifier::new(&["?"], &["[[:cntrl:]]"]).unwrap();
    assert_eq!(classifier.classify_changed_path(&[]).unwrap(), None);
    assert_eq!(classifier.classify_changed_path(&[10]).unwrap(), None);
    assert_eq!(classifier.classify_changed_path(&[13, 10]).unwrap(), None);
    assert_eq!(
        classifier
            .classify_changed_path(&[255])
            .unwrap()
            .unwrap()
            .as_str(),
        "allowed"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[237, 160, 128])
            .unwrap()
            .unwrap()
            .as_str(),
        "outside"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[195, 169])
            .unwrap()
            .unwrap()
            .as_str(),
        "allowed"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[240, 144, 128, 128])
            .unwrap()
            .unwrap()
            .as_str(),
        "allowed"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[240, 128, 128, 128])
            .unwrap()
            .unwrap()
            .as_str(),
        "outside"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[192, 175])
            .unwrap()
            .unwrap()
            .as_str(),
        "outside"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[226, 130])
            .unwrap()
            .unwrap()
            .as_str(),
        "outside"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[239, 191, 189])
            .unwrap()
            .unwrap()
            .as_str(),
        "allowed"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[97, 0, 98])
            .unwrap()
            .unwrap()
            .as_str(),
        "outside"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[46, 47, 97])
            .unwrap()
            .unwrap()
            .as_str(),
        "outside"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[97, 92, 98])
            .unwrap()
            .unwrap()
            .as_str(),
        "outside"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[0])
            .unwrap()
            .unwrap()
            .as_str(),
        "forbidden"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[9])
            .unwrap()
            .unwrap()
            .as_str(),
        "forbidden"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[32])
            .unwrap()
            .unwrap()
            .as_str(),
        "allowed"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[226, 128, 141])
            .unwrap()
            .unwrap()
            .as_str(),
        "allowed"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[255, 13, 10])
            .unwrap()
            .unwrap()
            .as_str(),
        "allowed"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[10, 10])
            .unwrap()
            .unwrap()
            .as_str(),
        "forbidden"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[13, 13])
            .unwrap()
            .unwrap()
            .as_str(),
        "forbidden"
    );
    assert_eq!(
        classifier
            .classify_changed_path(&[244, 144, 128, 128])
            .unwrap()
            .unwrap()
            .as_str(),
        "outside"
    );
    let expected = "ffffffffffffffffffffffffffffffffaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaafaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for (b, class) in (0..=255u8).zip(expected.bytes()) {
        // Single-line classification strips CR/LF; codepoint classification does not.
        let point = if b < 128 {
            u32::from(b)
        } else {
            0xdc00 + u32::from(b)
        };
        let actual = classifier.classify_codepoints(&[point]).unwrap();
        assert_eq!(actual.as_str().as_bytes()[0], class);
    }
    let replacement = Classifier::new(&["\u{fffd}"], &[]).unwrap();
    assert_eq!(
        replacement.classify_changed_path(&[255]).unwrap(),
        Some(Classification::Outside)
    );
    assert_eq!(
        replacement
            .classify_changed_path("\u{fffd}".as_bytes())
            .unwrap(),
        Some(Classification::Allowed)
    );
    let spanning = Classifier::new(&["[\u{d7ff}-\u{e000}]"], &[]).unwrap();
    assert_eq!(
        spanning.classify_codepoints(&[0xdcff]).unwrap(),
        Classification::Allowed
    );
}
