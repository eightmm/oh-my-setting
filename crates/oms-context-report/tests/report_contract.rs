use oms_context_report::render_report;
use serde_json::{Value, json};

#[test]
fn frozen_python_reports_match() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("data/reports.json")).unwrap();
    for case in cases {
        let projection = &case["projection"];
        let before = projection.clone();
        assert_eq!(
            render_report(projection).unwrap(),
            case["expected"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(*projection, before);
    }
}

#[test]
fn unsupported_and_malformed_reports_are_safe_errors() {
    for value in [
        json!({"schema": true}),
        json!({"schema": 2}),
        json!({"schema": 1, "kind": "other"}),
        json!({"schema": 1, "kind": "oms-context-inspection", "phase": "private"}),
    ] {
        assert!(
            !render_report(&value)
                .unwrap_err()
                .to_string()
                .contains("private")
        );
    }
}

#[test]
fn physical_call_report_preserves_unknown_delivery_and_usage() {
    let report = json!({"schema":1,"kind":"oms-context-request-inspection","operation_id":"op-ctx","call_id":"call-ctx",
        "provider":"fixture","dispatch":"prepared","provider_delivery":null,
        "attribution":{"request_bytes":7,"measured_bytes":3,"unattributed_bytes":4,"renderer_version":"fixture-v1",
            "layers":{"task":{"bytes":3,"tokens":null},"framing":{"bytes":0,"tokens":null}}}});
    let text = render_report(&report).unwrap();
    assert!(text.contains("3 attributed; 4 unmeasured"));
    assert!(text.contains("Provider delivery and model use: unknown"));
    assert!(text.contains("Usage and late corrections remain in the canonical provider receipt"));
    assert!(!text.contains("cache hit: 0"));
}
