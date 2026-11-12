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
