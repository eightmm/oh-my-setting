use oms_context_session::{inspect_session, inspect_timeline};

#[test]
fn transcript_projections() {
    let data = b"{\"timestamp\":\"2026-11-11T10:00:00Z\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"a\",\"name\":\"Read\",\"input\":{\"file_path\":\"src/a\"}}]}}\n{\"timestamp\":\"2026-11-11T10:00:01Z\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"a\",\"content\":\"hi\"}]}}\n{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"b\",\"name\":\"Read\",\"input\":{\"file_path\":\"src/a\"}},{\"type\":\"tool_result\",\"tool_use_id\":\"b\",\"is_error\":true,\"content\":\"no\"}]}}\ninvalid\n";
    let session = inspect_session(data, 10, |path| Some(path.to_owned())).unwrap();
    assert_eq!(session["result_bytes"], 4);
    assert_eq!(session["file_reads"]["repeated"]["items"][0]["reads"], 2);
    assert_eq!(session["unread_lines"], 1);
    let timeline = inspect_timeline(data, 1).unwrap();
    assert_eq!(timeline["steps"][0]["step"], 2);
    assert_eq!(timeline["steps"][0]["repeat_of"], 1);
    assert_eq!(timeline["first_error"]["step"], 2);
    assert_eq!(
        inspect_timeline(data, 2).unwrap()["steps"][0]["duration_ms"],
        1000
    );
    assert!(inspect_timeline(data, 0).is_err());
    assert!(inspect_timeline(data, 201).is_err());
    assert!(inspect_session(data, 0, |_| None).is_err());
    assert!(inspect_session(data, 101, |_| None).is_err());
    assert!(inspect_timeline(b"", 200).is_ok());
    assert!(inspect_session(b"", 100, |_| None).is_ok());
}

#[test]
fn timeline_caps_calls_and_counts_unmatched_results() {
    let mut data = Vec::new();
    for _ in 0..=oms_context_session::MAX_STEPS {
        data.extend_from_slice(
            b"{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"a\"}]}}\n",
        );
    }
    data.extend_from_slice(
        b"{\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"missing\"}]}}\n",
    );
    let value = inspect_timeline(&data, 1).unwrap();
    assert_eq!(value["steps_total"], 20000);
    assert_eq!(value["steps_not_recorded"], 1);
    assert_eq!(value["unmatched_results"], 1);
}

#[test]
fn timestamp_writer_forms_and_invalid_dates() {
    for (start, end, expected) in [
        (
            "2026-11-11T10:00:00 +00:00",
            "2026-11-11T10:00:01Z",
            Some(1000),
        ),
        ("2026-11-11T10:00:00.Z", "2026-11-11T10:00:01Z", Some(1000)),
        (
            "8059-07-04T21:55:15.841918+00:00",
            "8059-07-04T21:55:15.938416+00:00",
            Some(96),
        ),
        ("20261111T100000+0000", "20261111T100001+0000", Some(1000)),
        ("2026-11-11T10:00Z", "2026-11-11T10:01Z", Some(60000)),
        (
            "2026-11-11T10:00:00,5Z",
            "2026-11-11T10:00:01,5Z",
            Some(1000),
        ),
        ("2026-11-11T10:00:00z", "2026-11-11T10:00:01Z", None),
        ("2026-11-11T10:00:60Z", "2026-11-11T10:01:01Z", None),
        ("2026-1-1T10:00:00Z", "2026-01-01T10:00:01Z", None),
        (
            "2026-W46-3T10:00:00+00:00",
            "2026-11-11T10:00:01Z",
            Some(1000),
        ),
        ("2026-11-11T10:00+01:60", "2026-11-11T08:00:01Z", Some(1000)),
        (
            "2026-11-11T10:00:00+00:00:00.5",
            "2026-11-11T10:00:01Z",
            Some(1000),
        ),
        ("20261111Z100000", "2026-11-11T10:00:01Z", None),
        ("2026-11-11T10:00:00+2400", "2026-11-11T10:00:01Z", None),
    ] {
        let data = format!(
            "{{\"timestamp\":\"{start}\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"a\"}}]}}}}\n{{\"timestamp\":\"{end}\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"a\"}}]}}}}\n"
        );
        assert_eq!(
            inspect_timeline(data.as_bytes(), 1).unwrap()["steps"][0]["duration_ms"].as_i64(),
            expected,
            "{start}"
        );
    }
}

#[test]
fn resolver_output_must_be_safe_relative_label() {
    let data = b"{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"a\",\"name\":\"Read\",\"input\":{\"file_path\":\"x\"}},{\"type\":\"tool_use\",\"id\":\"b\",\"name\":\"Read\",\"input\":{\"file_path\":\"x\"}}]}}\n";
    for bad in ["/outside", "../outside", "a/../../outside", "", "bad\npath"] {
        let v = inspect_session(data, 10, |_| Some(bad.to_owned())).unwrap();
        assert_eq!(v["file_reads"]["outside_repository"], 2, "{bad:?}");
        assert!(!v.to_string().contains(bad) || bad.is_empty());
    }
}

#[test]
fn session_values_preserve_python_semantics() {
    let data = br#"{"message":{"content":[{"type":"tool_use","id":"\ud800","name":"X","input":{"n":1.00}},{"type":"tool_use","id":"\ud801","name":"X","input":{"n":1e0}},{"type":"tool_result","tool_use_id":"\ud800","content":"\ud800"}]}}
"#;
    let session = inspect_session(data, 10, |_| None).unwrap();
    assert_eq!(session["unread_lines"], 0);
    assert_eq!(session["unmatched_results"], 0);
    assert_eq!(session["result_bytes"], 1);
    let timeline = inspect_timeline(data, 10).unwrap();
    assert_eq!(timeline["steps"][0]["argument_bytes"], 9);
    assert_eq!(timeline["steps"][1]["repeat_of"], 1);
    assert_eq!(timeline["steps"][0]["result_bytes"], 1);
    assert!(timeline["steps"][1]["result_bytes"].is_null());
}

#[test]
fn numeric_and_private_map_arguments() {
    let data = br#"{"message":{"content":[{"type":"tool_use","id":"a","input":{"$serde_json::private::Number":"1","a":-0}},{"type":"tool_use","id":"b","input":{"a":0,"$serde_json::private::Number":"1"}},{"type":"tool_use","id":"c","input":[NaN,Infinity,-Infinity,1e999]}]}}"#;
    let v = inspect_timeline(data, 10).unwrap();
    assert_eq!(v["unread_lines"], 0);
    assert_eq!(v["steps"][0]["argument_bytes"], 42);
    assert_eq!(v["steps"][1]["repeat_of"], 1);
    assert_eq!(v["steps"][2]["argument_bytes"], 33);
    assert_eq!(
        inspect_session(data, 10, |_| None).unwrap()["tool_calls"],
        3
    );
}

#[test]
fn encoded_rows_and_integer_limit() {
    let row = r#"{"message":{"content":[{"type":"tool_use","id":"a","input":1.00},{"type":"tool_result","tool_use_id":"a","content":"ok"}]}}"#;
    let mut utf32 = vec![0, 0, 0xfe, 0xff];
    for c in row.chars() {
        utf32.extend_from_slice(&(c as u32).to_be_bytes());
    }
    assert_eq!(
        inspect_session(&utf32, 1, |_| None).unwrap()["result_bytes"],
        2
    );
    assert_eq!(
        inspect_timeline(&utf32, 1).unwrap()["steps"][0]["argument_bytes"],
        3
    );
    let valid = format!(
        "{{\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"a\",\"input\":{}}}]}}}}",
        "1".repeat(4300)
    );
    let invalid = format!(
        "{{\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"a\",\"input\":{}}}]}}}}",
        "1".repeat(4301)
    );
    assert_eq!(
        inspect_timeline(valid.as_bytes(), 1).unwrap()["steps"][0]["argument_bytes"],
        4300
    );
    assert_eq!(
        inspect_timeline(invalid.as_bytes(), 1).unwrap()["unread_lines"],
        1
    );
    assert_eq!(
        inspect_session(b"{", 1, |_| None).unwrap()["unread_lines"],
        1
    );
}

#[test]
fn surrogate_key_order_and_float_normalization() {
    let data = br#"{"message":{"content":[{"type":"tool_use","id":"a","input":{"?":3,"\ud800":2,"\ud801":1}},{"type":"tool_use","id":"b","input":{"\ud801":2,"?":3,"\ud800":1}},{"type":"tool_use","id":"c","input":1e2},{"type":"tool_use","id":"d","input":100.0}]}}"#;
    let value = inspect_timeline(data, 10).unwrap();
    assert_eq!(value["steps"][0]["argument_bytes"], 19);
    assert!(value["steps"][1]["repeat_of"].is_null());
    assert_eq!(value["steps"][2]["argument_bytes"], 5);
    assert_eq!(value["steps"][3]["repeat_of"], 3);
}

#[test]
fn deeply_nested_arguments_are_bounded() {
    let make = |depth: usize| {
        format!(
            "{{\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"a\",\"input\":{}0{}}}]}}}}",
            "[".repeat(depth),
            "]".repeat(depth)
        )
    };
    let accepted = make(1000);
    let rejected = make(1025);
    let value = inspect_timeline(accepted.as_bytes(), 1).unwrap();
    assert_eq!(value["steps"][0]["argument_bytes"], 2001);
    assert_eq!(
        inspect_timeline(rejected.as_bytes(), 1).unwrap()["unread_lines"],
        1
    );
}
