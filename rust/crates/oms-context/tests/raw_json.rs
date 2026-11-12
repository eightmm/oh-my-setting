use oms_context::{Json, parse_json};

#[test]
fn preserves_numbers_and_surrogate_strings() {
    let parsed = parse_json(br#"{"n":900719925474099312345,"s":"\ud800","b":true}"#).unwrap();
    let Json::Object(ref fields) = parsed else {
        panic!("object")
    };
    assert_eq!(fields[0].1, Json::Number("900719925474099312345".into()));
    assert_eq!(fields[1].1, Json::String(vec![0xed, 0xa0, 0x80]));
    assert_eq!(fields[2].1, Json::Bool(true));
}

#[test]
fn rejects_duplicate_keys_at_any_depth() {
    assert_eq!(
        parse_json(br#"{"x":{"a":1,"\u0061":2}}"#).unwrap_err(),
        "invalid context manifest: duplicate JSON key"
    );
    assert!(parse_json(br#"{"a":1,"a":2}"#).is_err());
    assert!(parse_json(br#"{"a":1"#).is_err());
    assert!(parse_json(b"").is_err());
    assert!(parse_json(br#"{"a":1}"#).is_ok());
    assert!(parse_json(br#"{"\ud800":1,"\ud800":2}"#).is_err());
    assert!(parse_json(br#"[null,0,false,[],{}]"#).is_ok());
    assert!(parse_json(br#"[NaN]"#).is_err());
    assert!(parse_json(br#"{"x":0,"x":1"#).is_err());
    assert_eq!(
        parse_json(br#"{"x":0,"x":1}"#).unwrap_err(),
        "invalid context manifest: duplicate JSON key"
    );
    let mut oversized_duplicate = br#"{"x":0,"x":1}"#.to_vec();
    oversized_duplicate.resize(4 * 1024 * 1024 + 1, b' ');
    assert_eq!(
        parse_json(&oversized_duplicate).unwrap_err(),
        "context manifest exceeds the read limit"
    );
}

#[test]
fn public_parser_limits() {
    let mut input = vec![b' '; 4 * 1024 * 1024];
    input[0] = b'0';
    assert!(parse_json(&input).is_ok());
    input.push(b' ');
    assert!(parse_json(&input).is_err());
    let over = format!("{}0{}", "[".repeat(1025), "]".repeat(1025));
    assert!(parse_json(over.as_bytes()).is_err());
    let edge = format!("{}0{}", "[".repeat(1024), "]".repeat(1024));
    assert!(parse_json(edge.as_bytes()).is_ok());
    let malformed = "[".repeat(1025);
    assert!(parse_json(malformed.as_bytes()).is_err());
}

#[test]
fn drops_constructed_deep_values() {
    let mut value = Json::Null;
    for _ in 0..100_000 {
        value = Json::Array(vec![value]);
    }
    drop(value);
}

#[test]
fn ordered_errors_and_leaf_identity() {
    let duplicate_then_nonfinite = br#"{"a":{"x":0,"x":1},"b":NaN}"#;
    assert_eq!(
        parse_json(duplicate_then_nonfinite).unwrap_err(),
        "invalid context manifest: duplicate JSON key"
    );
    assert_eq!(
        parse_json(br#"[NaN,{"x":0,"x":1}]"#).unwrap_err(),
        "invalid context manifest: non-finite JSON value"
    );
    let parsed = parse_json(br#"{"$serde_json::private::Number":"0","0":"$serde_json::private::Number","\ud800":-0,"n":1e999999}"#).unwrap();
    let Json::Object(ref fields) = parsed else {
        panic!("object")
    };
    assert_eq!(fields[2].0, vec![0xed, 0xa0, 0x80]);
    assert_eq!(fields[2].1, Json::Number("-0".into()));
    assert_eq!(fields[3].1, Json::Number("1e999999".into()));
    for raw in [
        br#"[1true]"#.as_slice(),
        br#"[1NaN]"#,
        br#"{1:2}"#,
        br#"[01]"#,
        br#"[1 2]"#,
        br#"["bad\q"]"#,
    ] {
        assert!(parse_json(raw).is_err());
    }
    assert_eq!(
        parse_json(br#"{"\ud800":0,"\uD800":1}"#).unwrap_err(),
        "invalid context manifest: duplicate JSON key"
    );
    assert!(parse_json(br#"{"x" NaN}"#).is_err());
    assert_eq!(
        parse_json(br#"{"a":{"x":0,"x":1},"b":"\q"}"#).unwrap_err(),
        "invalid context manifest: duplicate JSON key"
    );
    for depth in [0, 128, 129, 1024] {
        let raw = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        assert!(parse_json(raw.as_bytes()).is_ok());
    }
}

#[test]
fn session_bytes_and_python_values() {
    use oms_context::parse_session_json;
    let data =
        br#"{"a":1,"b":NaN,"a":-0,"c":Infinity,"d":-Infinity,"$serde_json::private::Number":"v"}"#;
    let value = parse_session_json(data).unwrap();
    let Json::Object(ref fields) = value else {
        panic!("object")
    };
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[0].0, b"a");
    assert_eq!(fields[0].1, Json::Number("-0".into()));
    assert_eq!(fields[1].1, Json::Number("NaN".into()));
    assert_eq!(fields[2].1, Json::Number("Infinity".into()));
    assert_eq!(fields[3].1, Json::Number("-Infinity".into()));
    assert!(parse_json(data).is_err());
    let source = "{\"x\":\"é😀\"}";
    let expected = parse_session_json(source.as_bytes()).unwrap();
    for be in [false, true] {
        let mut bytes = Vec::new();
        for unit in source.encode_utf16() {
            bytes.extend_from_slice(&if be {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            });
        }
        assert_eq!(parse_session_json(&bytes).unwrap(), expected);
        let bom = if be {
            &[0xfe, 0xff][..]
        } else {
            &[0xff, 0xfe][..]
        };
        let mut with_bom = bom.to_vec();
        with_bom.extend_from_slice(&bytes);
        assert_eq!(parse_session_json(&with_bom).unwrap(), expected);
        let mut bytes = Vec::new();
        for ch in source.chars() {
            let unit = ch as u32;
            bytes.extend_from_slice(&if be {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            });
        }
        assert_eq!(parse_session_json(&bytes).unwrap(), expected);
        let bom = if be {
            &[0, 0, 0xfe, 0xff][..]
        } else {
            &[0xff, 0xfe, 0, 0][..]
        };
        let mut with_bom = bom.to_vec();
        with_bom.extend_from_slice(&bytes);
        assert_eq!(parse_session_json(&with_bom).unwrap(), expected);
    }
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend_from_slice(source.as_bytes());
    assert_eq!(parse_session_json(&bom).unwrap(), expected);
    assert!(parse_json(&bom).is_err());
    assert_eq!(
        parse_session_json(b"\"\xed\xa0\x80\"").unwrap(),
        parse_session_json(br#""\ud800""#).unwrap()
    );
    assert!(parse_session_json(b"\"\xff\"").is_err());
    assert!(parse_session_json("1".repeat(4301).as_bytes()).is_err());
    assert!(parse_session_json("1".repeat(4300).as_bytes()).is_ok());
    assert!(parse_session_json(format!("{}.0", "1".repeat(4301)).as_bytes()).is_ok());
    for spelling in ["1.00", "1e2", "-0.0", "1e999"] {
        assert_eq!(
            parse_session_json(spelling.as_bytes()).unwrap(),
            Json::Number(spelling.into())
        );
    }
    let utf32 = [
        0xff, 0xfe, 0, 0, 0x22, 0, 0, 0, 0, 0xd8, 0, 0, 0, 0xdc, 0, 0, 0x22, 0, 0, 0,
    ];
    assert_eq!(
        parse_session_json(&utf32).unwrap(),
        Json::String(vec![0xed, 0xa0, 0x80, 0xed, 0xb0, 0x80])
    );
    let lone_utf16 = [0xff, 0xfe, 0x22, 0, 0, 0xd8, 0x22, 0];
    assert_eq!(
        parse_session_json(&lone_utf16).unwrap(),
        Json::String(vec![0xed, 0xa0, 0x80])
    );
    assert_eq!(
        parse_session_json(br#"{"\ud800":1,"\uD800":2,"?":3}"#).unwrap(),
        Json::Object(vec![
            (vec![0xed, 0xa0, 0x80], Json::Number("2".into())),
            (b"?".to_vec(), Json::Number("3".into()))
        ])
    );
    assert!(parse_session_json(&[0xff, 0xfe, 0x22]).is_err());
    assert!(parse_session_json(&[0xff, 0xfe, 0, 0, 0, 0, 0x11, 0]).is_err());
    assert!(parse_session_json(&[0x22, 0x5c, 0xed, 0xa0, 0x80, 0x22]).is_err());
    assert_eq!(
        parse_session_json(&vec![b' '; 16 * 1024 * 1024 + 1]).unwrap_err(),
        oms_context::SessionJsonError::Limit
    );
    let mut max = vec![b' '; 16 * 1024 * 1024];
    max[0] = b'0';
    assert_eq!(parse_session_json(&max).unwrap(), Json::Number("0".into()));
    assert_eq!(
        parse_session_json(b"").unwrap_err(),
        oms_context::SessionJsonError::Invalid
    );
    let edge = format!("{}0{}", "[".repeat(1024), "]".repeat(1024));
    assert!(parse_session_json(edge.as_bytes()).is_ok());
    let over = format!("{}0{}", "[".repeat(1025), "]".repeat(1025));
    assert_eq!(
        parse_session_json(over.as_bytes()).unwrap_err(),
        oms_context::SessionJsonError::Invalid
    );
    assert_eq!(
        parse_session_json("[".repeat(1025).as_bytes()).unwrap_err(),
        oms_context::SessionJsonError::Invalid
    );
}

#[test]
fn literal_controls_are_invalid_but_escaped_controls_remain_valid() {
    use oms_context::{compare_manifests, inspect_manifest, parse_session_json};
    for control in 0..0x20u8 {
        for template in [b"\"X\"".as_slice(), b"{\"X\":0}", b"{\"ignored\":\"X\"}"] {
            let raw: Vec<u8> = template
                .iter()
                .map(|b| if *b == b'X' { control } else { *b })
                .collect();
            assert!(parse_json(&raw).is_err(), "{control}");
            assert!(parse_session_json(&raw).is_err(), "{control}");
            assert!(inspect_manifest(&raw, 10).is_err());
            assert!(compare_manifests(&raw, &raw, 10).is_err());
            // Structural zero bytes in UTF-16/32 are valid; literal controls
            // inside the decoded JSON string still are not.
            for width in [2, 4] {
                for be in [false, true] {
                    let mut encoded = Vec::new();
                    for byte in &raw {
                        let bytes = if be {
                            (*byte as u32).to_be_bytes()
                        } else {
                            (*byte as u32).to_le_bytes()
                        };
                        encoded.extend_from_slice(if be {
                            &bytes[4 - width..]
                        } else {
                            &bytes[..width]
                        });
                    }
                    assert!(parse_session_json(&encoded).is_err());
                }
            }
        }
        let escaped = format!("\"\\u{control:04x}\"");
        assert!(parse_json(escaped.as_bytes()).is_ok());
        assert!(parse_session_json(escaped.as_bytes()).is_ok());
        let mut ordered = br#"{"x":{"a":1,"a":2},"later":""#.to_vec();
        ordered.push(control);
        ordered.extend_from_slice(b"\"}");
        assert_eq!(
            parse_json(&ordered).unwrap_err(),
            "invalid context manifest: duplicate JSON key"
        );
    }
}
