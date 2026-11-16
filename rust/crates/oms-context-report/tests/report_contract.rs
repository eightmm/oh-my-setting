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
