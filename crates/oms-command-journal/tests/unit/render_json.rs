use super::*;

#[test]
fn attempt_sorting_keeps_arbitrary_integers_and_python_zero_rules() {
    let mut attempts = [
        "18446744073709551616000000000000000000000",
        "0",
        "-18446744073709551616000000000000000000000",
        "-1",
        "1.9",
    ]
    .map(|raw| attempt_order(&crate::json::from_str(raw).unwrap()));
    attempts.sort_by(compare_attempts);
    assert_eq!(
        attempts[0],
        (true, "18446744073709551616000000000000000000000".into())
    );
    assert_eq!(attempts[1], (true, "1".into()));
    assert_eq!(attempts[2], (false, "0".into()));
    assert_eq!(attempts[3], (false, "1".into()));
    assert_eq!(
        attempts[4],
        (false, "18446744073709551616000000000000000000000".into())
    );
    for raw in [
        r#""""#, r#""bad""#, r#""1_""#, r#""--1""#, "null", "-0", "-0.0", "NaN",
    ] {
        assert_eq!(
            attempt_order(&crate::json::from_str(raw).unwrap()),
            (false, "0".into())
        );
    }
    let limit = "9".repeat(4300);
    assert_eq!(
        attempt_order(&crate::json::from_str(&limit).unwrap()).1,
        limit
    );
}

#[test]
fn latest_ci_attempt_does_not_reset_beyond_i128() {
    let large = format!("1{}", "0".repeat(100));
    let event = |attempt: &str, occurred: &str, id: &str| {
        crate::json::from_str(&format!(r#"{{"event_id":"{id}","occurred_at":"{occurred}","source":{{"type":"ci-status"}},"provenance":{{"code_commit":"abc","run_id":1,"attempt":{attempt}}}}}"#)).unwrap()
    };
    let events = vec![
        event(&large, "2026-07-31T01:00:00Z", "large"),
        event("2", "2026-07-31T02:00:00Z", "small"),
    ];
    let latest = latest_ci_events(&events);
    assert_eq!(latest.len(), 1);
    assert_eq!(string(latest[0], "event_id"), "large");
}
