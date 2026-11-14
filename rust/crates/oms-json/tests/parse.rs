use oms_json::{
    Error, OrderedObject, OrderedValue, ParseError, ParseOptions, PyString, canonical_json,
    ordered_values_equal, parse_ordered_with_options, parse_python, parse_python_ordered,
    parse_strict, parse_strict_ordered, parse_with_options, values_equal,
};

fn error(input: &[u8], options: &ParseOptions) -> ParseError {
    match parse_with_options(input, options) {
        Err(error) => error,
        Ok(_) => panic!("unexpectedly parsed {input:?}"),
    }
}

#[test]
fn strict_round_trips_and_integer_grammar() {
    for input in [
        "null",
        "true",
        "false",
        "0",
        "-1",
        "9223372036854775808",
        "-9223372036854775809",
        "18446744073709551616",
        "[]",
        "{}",
        r#"{"a":[null,true,false,1,1.0,-0.0],"é":"😀"}"#,
    ] {
        let value = parse_strict(input.as_bytes()).unwrap();
        assert_eq!(canonical_json(&value).unwrap(), input.as_bytes());
    }
    assert_eq!(
        parse_strict(b"-0")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_decimal(),
        "0"
    );
    for input in ["-0.0", "-0e0", "-0E+1"] {
        assert!(
            parse_strict(input.as_bytes())
                .unwrap()
                .as_float()
                .unwrap()
                .is_sign_negative()
        );
    }
    for input in ["1.0", "1e0", "1E+0", "1e-0"] {
        assert_eq!(
            parse_strict(input.as_bytes()).unwrap().as_float(),
            Some(1.0)
        );
    }
    let value = parse_strict(b" \t\r\n[1, {\"a\" : 2}]\n ").unwrap();
    assert_eq!(canonical_json(&value).unwrap(), br#"[1,{"a":2}]"#);
}

#[test]
fn malformed_json_is_rejected_in_both_modes() {
    for options in [ParseOptions::strict(), ParseOptions::python_compat()] {
        for input in [
            "",
            " ",
            "+1",
            "01",
            "-01",
            "--1",
            "-",
            ".1",
            "1.",
            "1e",
            "1e+",
            "1e-",
            "1e+-1",
            "1.2.3",
            "[",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "[1 2]",
            "{a:1}",
            "{\"a\" 1}",
            "[}",
            "{]",
            "true false",
            "nullx",
            "NULL",
            "nan",
            "inf",
            "-NaN",
            "\u{feff}null",
            "\u{a0}null",
            "\"unterminated",
            "\"\\x00\"",
            "\"\\uXYZ0\"",
            "\"\\u123\"",
            "\"\\u",
            "\"\\",
        ] {
            assert!(
                parse_with_options(input.as_bytes(), &options).is_err(),
                "{input:?}"
            );
        }
        for byte in 0..32 {
            assert_eq!(
                error(&[b'"', byte, b'"'], &options),
                ParseError::UnescapedControl
            );
        }
        for input in [&b"\"\xff\""[..], &b"\"\xed\xa0\x80\""[..], &b"null\xff"[..]] {
            assert_eq!(error(input, &options), ParseError::InvalidUtf8);
        }
        assert_eq!(error(b"null null", &options), ParseError::TrailingData);
        for input in ["NULL", "None", "[Inf]", "[-Inf]", "-Invalid"] {
            assert_eq!(
                error(input.as_bytes(), &options),
                ParseError::InvalidSyntax,
                "{input:?}"
            );
        }
        for input in ["N", "Na", "I", "Inf", "-I", "-Inf"] {
            assert_eq!(
                error(input.as_bytes(), &options),
                ParseError::UnexpectedEnd,
                "{input:?}"
            );
        }
    }
}

#[test]
fn python_modes_are_independent() {
    let strict = ParseOptions::strict();
    for input in ["NaN", "Infinity", "-Infinity", "1e9999", "-1e9999"] {
        assert_eq!(error(input.as_bytes(), &strict), ParseError::NonFiniteFloat);
        assert!(
            !parse_python(input.as_bytes())
                .unwrap()
                .as_float()
                .unwrap()
                .is_finite()
        );
    }
    assert!(parse_python(b"NaN").unwrap().as_float().unwrap().is_nan());
    assert_eq!(
        parse_python(b"-Infinity").unwrap().as_float(),
        Some(f64::NEG_INFINITY)
    );
    assert_eq!(parse_strict(b"1e-9999").unwrap().as_float(), Some(0.0));
    let duplicate = br#"{"a":1,"\u0061":{"x":2}}"#;
    assert_eq!(error(duplicate, &strict), ParseError::DuplicateKey);
    let value = parse_python(duplicate).unwrap();
    assert_eq!(
        value
            .get("a")
            .unwrap()
            .get("x")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_i64(),
        Some(2)
    );
    let mut options = strict;
    options.allow_duplicate_keys = true;
    assert!(parse_with_options(duplicate, &options).is_ok());
    assert_eq!(error(b"NaN", &options), ParseError::NonFiniteFloat);
    options.allow_nonfinite = true;
    options.allow_duplicate_keys = false;
    assert!(parse_with_options(b"NaN", &options).is_ok());
    assert_eq!(error(duplicate, &options), ParseError::DuplicateKey);
}

#[test]
fn escaped_strings_and_surrogate_pairs() {
    let value = parse_strict(br#""\"\\\/\b\f\n\r\t\u0000\u007f\u00e9\uD83D\uDE00""#).unwrap();
    assert_eq!(
        value.as_string().unwrap().to_str_checked().as_deref(),
        Some("\"\\/\u{8}\u{c}\n\r\t\0\u{7f}é😀")
    );
    for input in [
        r#""\ud800""#,
        r#""\udfff""#,
        r#""\ud800\u0061""#,
        r#""\ud800\ud800""#,
    ] {
        assert_eq!(
            error(input.as_bytes(), &ParseOptions::strict()),
            ParseError::LoneSurrogate
        );
        assert!(
            parse_python(input.as_bytes())
                .unwrap()
                .as_string()
                .unwrap()
                .to_str_checked()
                .is_none()
        );
    }
    let value = parse_python(br#""\ud800\ud83d\ude00\udfff\u0061""#).unwrap();
    assert_eq!(
        value.as_string().unwrap().codepoints(),
        &[0xd800, 0x1f600, 0xdfff, 0x61]
    );
    let value = parse_strict(br#""\ud800\udc00\udbff\udfff""#).unwrap();
    assert_eq!(
        value.as_string().unwrap().codepoints(),
        &[0x10000, 0x10ffff]
    );
    let mut options = ParseOptions::strict();
    options.allow_lone_surrogates = true;
    assert!(parse_with_options(br#"{"\ud800":null}"#, &options).is_ok());
    assert_eq!(
        error(br#"{"\ud800":null}"#, &ParseOptions::strict()),
        ParseError::LoneSurrogate
    );
    for input in [r#""\ud800\uZZZZ""#, r#""\ud800\u123""#] {
        assert!(parse_python(input.as_bytes()).is_err());
    }
}

#[test]
fn byte_depth_and_digit_limit_edges() {
    let mut options = ParseOptions::strict();
    options.max_bytes = 0;
    assert_eq!(error(b"", &options), ParseError::UnexpectedEnd);
    assert_eq!(error(b"0", &options), ParseError::ByteLimit);
    options.max_bytes = 1;
    assert!(parse_with_options(b"0", &options).is_ok());
    assert_eq!(error(b"0 ", &options), ParseError::ByteLimit);
    options.max_bytes = 4;
    assert!(parse_with_options("\"é\"".as_bytes(), &options).is_ok());
    options.max_bytes = 3;
    assert_eq!(error("\"é\"".as_bytes(), &options), ParseError::ByteLimit);
    options = ParseOptions::strict();
    options.max_depth = 0;
    assert!(parse_with_options(b"null", &options).is_ok());
    for input in ["[]", "{}"] {
        assert_eq!(error(input.as_bytes(), &options), ParseError::DepthLimit);
    }
    options.max_depth = 1;
    for input in ["[]", "{}", "[0]", "{\"x\":0}"] {
        assert!(parse_with_options(input.as_bytes(), &options).is_ok());
    }
    for input in ["[[]]", "{\"x\":{}}", "[{\"x\":0}]"] {
        assert_eq!(error(input.as_bytes(), &options), ParseError::DepthLimit);
    }
    options = ParseOptions::strict();
    options.max_integer_digits = 1;
    for input in ["0", "-0", "9", "-9", "99.0", "99e0"] {
        assert!(parse_with_options(input.as_bytes(), &options).is_ok());
    }
    for input in ["10", "-10"] {
        assert_eq!(
            error(input.as_bytes(), &options),
            ParseError::IntegerDigitLimit
        );
    }
    for count in [4300, 4301] {
        for sign in ["", "-"] {
            let input = format!("{sign}{}", "9".repeat(count));
            if count == 4300 {
                assert!(parse_strict(input.as_bytes()).is_ok());
            } else {
                assert_eq!(
                    error(input.as_bytes(), &ParseOptions::strict()),
                    ParseError::IntegerDigitLimit
                );
            }
            options.max_integer_digits = 0;
            let parsed = parse_with_options(input.as_bytes(), &options).unwrap();
            assert_eq!(parsed.as_integer().unwrap().as_decimal(), input);
        }
    }
    options.max_bytes = 1;
    assert_eq!(error(b"99", &options), ParseError::ByteLimit);
}

#[test]
fn iterative_parsing_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(|| {
            for depth in [1024, 1025] {
                let input = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
                if depth == 1024 {
                    let value = parse_strict(input.as_bytes()).unwrap();
                    assert!(values_equal(&value, &value.clone_bounded().unwrap()));
                } else {
                    assert_eq!(
                        error(input.as_bytes(), &ParseOptions::strict()),
                        ParseError::DepthLimit
                    );
                }
            }
            let mut options = ParseOptions::strict();
            options.max_depth = 20_000;
            let input = format!("{}null{}", "{\"x\":".repeat(20_000), "}".repeat(20_000));
            drop(parse_with_options(input.as_bytes(), &options).unwrap());
            assert!(parse_with_options(&input.as_bytes()[..input.len() - 1], &options).is_err());

            for depth in [1024, 1025] {
                let ordered_input = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
                if depth == 1024 {
                    let ordered = parse_strict_ordered(ordered_input.as_bytes()).unwrap();
                    assert!(ordered_values_equal(
                        &ordered,
                        &ordered.clone_bounded().unwrap()
                    ));
                    assert!(ordered.into_value().is_ok());
                } else {
                    assert_eq!(
                        match parse_ordered_with_options(
                            ordered_input.as_bytes(),
                            &ParseOptions::strict()
                        ) {
                            Err(err) => err,
                            Ok(_) => panic!("expected depth limit"),
                        },
                        ParseError::DepthLimit
                    );
                }
            }
            drop(parse_ordered_with_options(input.as_bytes(), &options).unwrap());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn ordered_parsing_preserves_insertion_order_and_python_duplicate_semantics() {
    let parsed = parse_python_ordered(
        br#"{"b ":1,"b":2,"big":9223372036854775808,"a":{"z":1,"a":2},"b ":99}"#,
    )
    .unwrap();
    let keys: Vec<String> = parsed
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, _)| k.to_str_checked().unwrap())
        .collect();
    assert_eq!(keys, vec!["b ", "b", "big", "a"]);
    assert_eq!(
        parsed
            .as_object()
            .unwrap()
            .get("b ")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_i64(),
        Some(99)
    );
    assert_eq!(
        parsed
            .get("big")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_decimal(),
        "9223372036854775808"
    );

    let inner_keys: Vec<String> = parsed
        .get("a")
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, _)| k.to_str_checked().unwrap())
        .collect();
    assert_eq!(inner_keys, vec!["z", "a"]);

    assert_eq!(
        match parse_strict_ordered(br#"{"a":1,"a":2}"#) {
            Err(err) => err,
            Ok(_) => panic!("strict ordered must reject duplicate keys"),
        },
        ParseError::DuplicateKey
    );

    let mut pairs = Vec::new();
    for i in (1..=65).rev() {
        pairs.push(format!("\"k{i:02}\":{i}"));
    }
    let wide = format!("{{{}}}", pairs.join(","));
    let parsed_wide = parse_python_ordered(wide.as_bytes()).unwrap();
    let first64: Vec<String> = parsed_wide
        .as_object()
        .unwrap()
        .iter()
        .take(64)
        .map(|(k, _)| k.to_str_checked().unwrap())
        .collect();
    let expected64: Vec<String> = (2..=65).rev().map(|i| format!("k{i:02}")).collect();
    assert_eq!(first64, expected64);

    let left = parse_strict_ordered(br#"{"a":1,"b":2}"#).unwrap();
    let right = parse_strict_ordered(br#"{"b":2,"a":1}"#).unwrap();
    assert!(!ordered_values_equal(&left, &right));
    assert!(values_equal(
        &left.clone_bounded().unwrap().into_value().unwrap(),
        &right.clone_bounded().unwrap().into_value().unwrap()
    ));
    assert_eq!(
        canonical_json(&left.into_value().unwrap()).unwrap(),
        br#"{"a":1,"b":2}"#
    );

    let mut obj = OrderedObject::new();
    assert!(
        obj.insert(PyString::from("first"), OrderedValue::integer("1").unwrap())
            .is_none()
    );
    assert!(
        obj.insert(PyString::from("second"), OrderedValue::Bool(true))
            .is_none()
    );
    let prev = obj
        .insert(PyString::from("first"), OrderedValue::integer("3").unwrap())
        .unwrap();
    assert_eq!(prev.as_integer().unwrap().as_i64(), Some(1));
    assert_eq!(obj.len(), 2);
    assert_eq!(
        obj.iter()
            .map(|(k, _)| k.to_str_checked().unwrap())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );

    let mut deep = OrderedValue::Null;
    for _ in 0..1025 {
        deep = OrderedValue::Array(vec![deep]);
    }
    assert!(matches!(deep.clone_bounded(), Err(Error::DepthLimit)));
    assert!(matches!(deep.into_value(), Err(Error::DepthLimit)));
}
