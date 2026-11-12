use oms_failure::{catalog, classify, classify_codepoints, codes};
use oms_json::{PyString, canonical_json};

type Case = (
    &'static [u32],
    Option<i64>,
    &'static str,
    &'static str,
    bool,
    bool,
    bool,
    &'static str,
    &'static [u32],
);
type CatalogRow = (&'static str, bool, bool, bool, &'static str);
include!("fixture.rs.inc");

#[test]
fn canonical_reference_corpus() {
    for &(text, exit, explicit, code, retry, fallback, escalation, recovery, matched) in CASES {
        let got = classify_codepoints(text, exit, explicit).unwrap();
        assert_eq!(
            (
                got.code,
                got.properties.retryable,
                got.properties.fallback_allowed,
                got.properties.context_escalation_allowed,
                got.properties.recovery,
                got.matched,
                got.exit
            ),
            (
                code,
                retry,
                fallback,
                escalation,
                recovery,
                PyString::from_codepoints(matched.to_vec()).unwrap(),
                exit
            ),
            "input {text:?}"
        );
    }
}
#[test]
fn catalog_and_order_are_canonical() {
    let actual = catalog();
    assert_eq!(actual.len(), CATALOG.len());
    for &(code, retry, fallback, escalation, recovery) in CATALOG {
        let p = actual[code];
        assert_eq!(
            (
                p.retryable,
                p.fallback_allowed,
                p.context_escalation_allowed,
                p.recovery
            ),
            (retry, fallback, escalation, recovery)
        );
    }
    let expected: Vec<_> = CATALOG
        .iter()
        .map(|r| r.0)
        .chain(["unknown_failure"])
        .collect();
    assert_eq!(codes(), expected);
    assert!(!actual.contains_key("unknown_failure"));
}
#[test]
fn json_fields_and_invalid_codepoint() {
    let got = canonical_json(&classify("", None, "").to_json_value()).unwrap();
    assert_eq!(
        String::from_utf8(got).unwrap(),
        r#"{"code":"unknown_failure","context_escalation_allowed":false,"exit":null,"fallback_allowed":false,"matched":"","recovery":"preserve_and_escalate","retryable":false}"#
    );
    assert!(classify_codepoints(&[0x110000], None, "").is_err());
    assert!(
        canonical_json(
            &classify_codepoints(
                &[
                    99, 114, 101, 100, 101, 110, 116, 105, 97, 108, 32, 0xd800, 32, 109, 105, 115,
                    115, 105, 110, 103
                ],
                None,
                ""
            )
            .unwrap()
            .to_json_value()
        )
        .is_err()
    );
}
