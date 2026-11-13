use oms_json::{
    PyString, Value, agent_event_json_bytes, canonical_json, pretty_json, projection_cache_digest,
    session_call_key_json_bytes,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// Ambiguity arises at compile time if a recursive trait is added to Value.
macro_rules! assert_not_implemented {
    ($trait:path) => {{
        trait AmbiguousIfImplemented<A> {
            fn marker() {}
        }
        impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
        impl<T: ?Sized + $trait> AmbiguousIfImplemented<u8> for T {}
        let _ = <Value as AmbiguousIfImplemented<_>>::marker;
    }};
}

#[test]
fn value_has_no_recursive_derived_traits() {
    assert_not_implemented!(Clone);
    assert_not_implemented!(std::fmt::Debug);
    assert_not_implemented!(PartialEq);
}

#[test]
fn python_contracts() {
    let mut map = BTreeMap::new();
    map.insert("z".into(), Value::Float(-0.0));
    map.insert("é".into(), Value::String("😀\n\u{0001}".into()));
    map.insert(
        "a".into(),
        Value::Array(vec![
            Value::Bool(true),
            Value::integer("123456789012345678901234567890").unwrap(),
            Value::Float(1e-5),
            Value::Float(1e16),
        ]),
    );
    let value = Value::Object(map);
    let compact = "{\"a\":[true,123456789012345678901234567890,1e-05,1e+16],\"z\":-0.0,\"é\":\"😀\\n\\u0001\"}";
    assert_eq!(canonical_json(&value).unwrap(), compact.as_bytes());
    assert_eq!(agent_event_json_bytes(&value).unwrap(), compact.as_bytes());
    assert_eq!(pretty_json(&value).unwrap(), "{\n  \"a\": [\n    true,\n    123456789012345678901234567890,\n    1e-05,\n    1e+16\n  ],\n  \"z\": -0.0,\n  \"é\": \"😀\\n\\u0001\"\n}\n".as_bytes());
    assert_eq!(
        projection_cache_digest(&value).unwrap(),
        "d70dfdd3978c28090b6db7725d0e56575fe302a3771b2afd18a46881a26e4b6f"
    );
}

#[test]
fn session_policy_matches_python() {
    let mut object = BTreeMap::new();
    object.insert("z".into(), Value::Float(f64::NEG_INFINITY));
    object.insert(
        "a".into(),
        Value::Array(vec![
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
            Value::Float(1.0),
            Value::integer("1").unwrap(),
        ]),
    );
    object.insert(
        PyString::from_codepoints(vec![0xd800]).unwrap(),
        Value::String(PyString::from_codepoints(vec![0x1f600, 0xdc00, 10]).unwrap()),
    );
    let value = Value::Object(object);
    assert_eq!(
        session_call_key_json_bytes(&value).unwrap(),
        "{\"a\":[NaN,Infinity,1.0,1],\"z\":-Infinity,\"?\":\"😀?\\n\"}".as_bytes()
    );
    let bytes = session_call_key_json_bytes(&value).unwrap();
    assert_eq!(bytes.len(), 54);
    assert_eq!(
        format!("{:x}", Sha256::digest(bytes)),
        "84a5d9ad51cfc6a453ab468e92efc7ebe34d53989247d5409213e5cd4b27afa7"
    );
    for encoder in [canonical_json, pretty_json, agent_event_json_bytes] {
        assert!(encoder(&value).is_err());
        assert!(encoder(&Value::Float(f64::NEG_INFINITY)).is_err());
    }
    assert!(projection_cache_digest(&value).is_err());
    let surrogate = Value::String(PyString::from_codepoints(vec![0xd800]).unwrap());
    assert!(agent_event_json_bytes(&surrogate).is_err());
    assert!(pretty_json(&surrogate).is_err());
    assert!(canonical_json(&surrogate).is_err());
    assert_eq!(
        session_call_key_json_bytes(&Value::Object(BTreeMap::new())).unwrap(),
        b"{}"
    );
    assert_eq!(
        session_call_key_json_bytes(&Value::Float(-0.0)).unwrap(),
        b"-0.0"
    );
}

#[test]
fn replacement_length_counts_plain_text() {
    let text = PyString::from_codepoints(vec![0, 0x7f, 0x80, 0x1f600, 0xd800, 0xdc00]).unwrap();
    assert_eq!(
        text.utf8_with_replacement(),
        "\0\u{7f}\u{80}😀??".as_bytes()
    );
    assert_eq!(text.utf8_with_replacement_len(), 10);
    assert_eq!(PyString::from("").utf8_with_replacement_len(), 0);
    assert!(PyString::from_codepoints(vec![0x110000]).is_err());
}

#[test]
fn edges_and_errors() {
    assert!(Value::integer("").is_err());
    assert!(Value::integer("01").is_err());
    assert!(Value::integer("-0").is_err());
    assert_eq!(canonical_json(&Value::integer("0").unwrap()).unwrap(), b"0");
    assert!(canonical_json(&Value::Float(f64::NAN)).is_err());
    assert!(canonical_json(&Value::Float(f64::INFINITY)).is_err());
    let surrogate = Value::String(PyString::from_codepoints(vec![0xd800]).unwrap());
    assert!(canonical_json(&surrogate).is_err());
    assert!(pretty_json(&surrogate).is_err());
    assert_eq!(
        projection_cache_digest(&surrogate).unwrap(),
        "8c0c59dd0d275aadcd462a5fe12eb352cbdfeaf961eae4f85a4660521df7d2f5"
    );
    assert!(PyString::from_codepoints(vec![0x110000]).is_err());
    assert_eq!(pretty_json(&Value::Array(vec![])).unwrap(), b"[]\n");
    assert_eq!(
        projection_cache_digest(&Value::String("\u{7f}".into())).unwrap(),
        "226453a6508483054f1ddc449c26b45077f5a45d993034f0f5f04f3f9801c32c"
    );
}

#[test]
fn integer_encoding_limit() {
    for sign in ["", "-"] {
        let edge = format!("{sign}{}", "9".repeat(4300));
        let over = format!("{sign}{}", "9".repeat(4301));
        let good = Value::integer(&edge).unwrap();
        let bad = Value::integer(&over).unwrap();
        for encode in [
            canonical_json,
            pretty_json,
            agent_event_json_bytes,
            session_call_key_json_bytes,
        ] {
            assert!(encode(&good).unwrap().starts_with(edge.as_bytes()));
            assert_eq!(
                format!("{:?}", encode(&bad).unwrap_err()),
                "IntegerDigitLimit"
            );
        }
        assert!(projection_cache_digest(&good).is_ok());
        assert_eq!(
            format!("{:?}", projection_cache_digest(&bad).unwrap_err()),
            "IntegerDigitLimit"
        );
    }
}

#[test]
fn depth_bound_and_deep_drop() {
    fn nested(depth: usize) -> Value {
        let mut v = Value::Null;
        for i in 0..depth {
            v = if i % 2 == 0 {
                Value::Array(vec![v])
            } else {
                Value::Object([(PyString::from("x"), v)].into())
            };
        }
        v
    }
    let at_limit = nested(1024);
    let over = nested(1025);
    for encode in [
        canonical_json,
        pretty_json,
        agent_event_json_bytes,
        session_call_key_json_bytes,
    ] {
        assert!(encode(&at_limit).is_ok());
        assert_eq!(format!("{:?}", encode(&over).unwrap_err()), "DepthLimit");
    }
    assert!(projection_cache_digest(&at_limit).is_ok());
    assert_eq!(
        format!("{:?}", projection_cache_digest(&over).unwrap_err()),
        "DepthLimit"
    );
    std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(|| {
            let v = nested(50_000);
            assert!(canonical_json(&v).is_err());
            drop(v);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_drop_without_encoding() {
    std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(|| {
            let mut value = Value::Null;
            for i in 0..50_000 {
                value = if i % 2 == 0 {
                    Value::Array(vec![value])
                } else {
                    Value::Object([(PyString::from("x"), value)].into())
                };
            }
            drop(value);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn python_float_corpus() {
    for row in include_str!("python-floats.txt").lines() {
        let (bits, expected) = row.split_once(' ').unwrap();
        let value = f64::from_bits(u64::from_str_radix(bits, 16).unwrap());
        assert_eq!(
            canonical_json(&Value::Float(value)).unwrap(),
            expected.as_bytes(),
            "bits {bits}"
        );
        assert_eq!(
            session_call_key_json_bytes(&Value::Float(value)).unwrap(),
            expected.as_bytes(),
            "bits {bits}"
        );
    }
}

#[test]
fn codepoints_preserve_surrogates() {
    let text = PyString::from_codepoints(vec![0x61, 0xdcff, 0x1f600]).unwrap();
    assert_eq!(text.codepoints(), &[0x61, 0xdcff, 0x1f600]);
    assert!(canonical_json(&Value::String(text)).is_err());
}
