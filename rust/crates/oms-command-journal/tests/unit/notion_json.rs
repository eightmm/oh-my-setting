use super::*;

#[test]
fn redaction_preserves_python_codepoints_and_exact_numbers() {
    let raw =
        crate::json::from_str(r#"{"hide\ud800":{"n":18446744073709551616,"s":"hide\udcff"}}"#)
            .unwrap();
    assert!(contains_text(&raw, "hide"));
    assert!(!contains_text(&raw, "missing"));
    assert!(contains_text(&raw, ""));
    let unchanged = redact_json(&raw, "");
    assert!(oms_json::ordered_values_equal(&raw, &unchanged));
    let clean = redact_json(&raw, "hide");
    let entries = clean.as_object().unwrap().as_slice();
    assert_eq!(entries[0].0.codepoints().last(), Some(&0xd800));
    assert!(!contains_text(&clean, "hide"));
    assert!(contains_text(&clean, "[REDACTED]"));
    assert_eq!(
        entries[0]
            .1
            .get("n")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_decimal(),
        "18446744073709551616"
    );
    assert_eq!(
        entries[0]
            .1
            .get("s")
            .unwrap()
            .as_string()
            .unwrap()
            .codepoints()
            .last(),
        Some(&0xdcff)
    );
}

#[test]
fn deep_redaction_uses_bounded_cloning_and_iterative_walks() {
    let source = format!("{}\"hide\"{}", "[".repeat(1024), "]".repeat(1024));
    let raw = crate::json::from_str(&source).unwrap();
    assert!(contains_text(&raw, "hide"));
    let clean = redact_json(&raw, "hide");
    assert!(!contains_text(&clean, "hide"));
    assert!(contains_text(&clean, "[REDACTED]"));
}

#[test]
fn retry_counter_stays_exact_while_scheduling_is_capped() {
    let (first, schedule) = increment_retry_attempts(None).unwrap();
    assert_eq!(first.as_integer().unwrap().as_decimal(), "1");
    assert_eq!(schedule, 1);
    for (before, after, schedule) in [
        ("0", "1", 1),
        ("-1", "1", 1),
        ("1", "2", 2),
        ("18446744073709551615", "18446744073709551616", u64::MAX),
        ("999999999999999999999", "1000000000000000000000", u64::MAX),
    ] {
        let previous = Value::integer(before).unwrap();
        let (exact, actual) = increment_retry_attempts(previous.as_integer()).unwrap();
        assert_eq!(exact.as_integer().unwrap().as_decimal(), after);
        assert_eq!(actual, schedule);
    }
    let previous = Value::integer(&format!("1{}", "0".repeat(4299))).unwrap();
    let (next, _) = increment_retry_attempts(previous.as_integer()).unwrap();
    assert_eq!(next.as_integer().unwrap().digit_len(), 4300);
    let previous = Value::integer(&"9".repeat(4300)).unwrap();
    assert!(increment_retry_attempts(previous.as_integer()).is_err());
    assert!(crate::json::from_str("01").is_err());
}

#[test]
fn request_property_numbers_do_not_pass_through_serde() {
    let schema = TargetSchema {
        is_data_source: true,
        target_id: "target".into(),
        key_prop: "key".into(),
        hash_prop: "hash".into(),
        title_prop: "title".into(),
        project_prop: None,
        kind_prop: None,
        period_prop: None,
        blocker_prop: None,
        sessions_prop: None,
        commits_prop: Some(("commits".into(), "number".into())),
        verified_prop: None,
    };
    for number in ["18446744073709551616", "-9223372036854775809", "1.0"] {
        let aggregates = crate::json::from_str(&format!(r#"{{"commits":{number}}}"#)).unwrap();
        let properties = build_page_properties(
            &schema,
            "key",
            "title",
            None,
            "daily",
            "2026-07-31",
            "project",
            false,
            &aggregates,
        );
        let encoded = crate::sanitize::canonical_json_bytes(&properties).unwrap();
        let replay = crate::json::from_slice(&encoded).unwrap();
        let actual = replay.at("commits").at("number");
        assert!(oms_json::ordered_values_equal(
            actual,
            aggregates.at("commits")
        ));
    }
}
