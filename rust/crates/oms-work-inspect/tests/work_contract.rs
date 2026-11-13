use oms_work_inspect::{action_inbox, evidence_matrix, render, return_brief};
use serde_json::{Value, json};
fn fixture() -> Value {
    json!({"schema":2,"generated_at":"2026-11-10T00:00:00Z","state_digest":"a".repeat(64),"repo":{"head":"b".repeat(40)},"task":{},"plan":{"present":true,"plan_id":"plan_1","tasks":[{"id":"t1","title":"구현 t1","state":"ready","depends":[],"verify_digest":"c".repeat(64),"verify_present":true}],"actionable":["t1"],"contract":{"bound":true,"satisfied":true}},"criteria":[{"id":"req1","text":"확인할 기준","status":"missing","source":"plan-task","plan_id":"plan_1","plan_task_id":"t1","evidence":[]}],"sources":[{"path":"PROJECT.md","sha256":"d".repeat(64)}],"next_actions":[{"id":"execute_ready_task","priority":80,"authority":"worktree_write","reason":"one task ready","command":"oms plan-run --next"}]})
}
fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn status_fixture(done: u64, cancelled: u64, unfinished: u64) -> Value {
    let total = done + cancelled + unfinished;
    json!({
        "schema": 1, "present": true, "task_count": total, "nonempty": total > 0,
        "all_done": total > 0 && done == total,
        "all_closed": total > 0 && unfinished == 0, "has_unfinished": unfinished > 0,
        "verified_count": done, "cancelled_count": cancelled,
        "closed_count": done + cancelled, "unfinished_count": unfinished,
        "by_state": {"done": done, "cancelled": cancelled, "ready": unfinished},
        "actionable": [], "stale": [], "stale_review": [],
        "contract": {"bound": false, "satisfied": false, "project_present": false,
            "project_healthy": false, "project_state": "", "blocker": "",
            "expected_spec_sha256": "", "current_spec_sha256": ""}
    })
}

#[test]
fn task_status_snapshot_contract() {
    use oms_work_inspect::task_status_snapshot;
    let valid = json!({"schema": 1, "present": true, "status": "future-status",
        "verification": "future-verification", "stale": false, "extension": [42, "한글"]});
    let raw = bytes(&valid);
    assert_eq!(
        task_status_snapshot(&raw).unwrap(),
        oms_context::parse_json(&raw).unwrap()
    );
    for key in ["schema", "present", "status", "verification", "stale"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(task_status_snapshot(&bytes(&missing)).is_err(), "{key}");
        let bad_values = match key {
            "schema" => vec![json!(0), json!(2), json!("1"), Value::Null],
            "present" => vec![json!(false), json!(1), Value::Null],
            "stale" => vec![json!(0), json!("false"), Value::Null],
            _ => vec![json!(0), json!(false), Value::Null],
        };
        for bad in bad_values {
            let mut invalid = valid.clone();
            invalid[key] = bad;
            assert_eq!(
                task_status_snapshot(&bytes(&invalid))
                    .unwrap_err()
                    .to_string(),
                "canonical task status projection is unavailable or invalid",
                "{key}"
            );
        }
    }
    let mut empty = valid.clone();
    empty["status"] = json!("");
    empty["verification"] = json!("");
    empty["stale"] = json!(true);
    assert!(task_status_snapshot(&bytes(&empty)).is_ok());
    for raw in [b"".as_slice(), b"{", b"[]", b"null", b"{}", b"0"] {
        assert!(task_status_snapshot(raw).is_err());
    }
}

#[test]
fn plan_status_snapshot_field_contract() {
    use oms_work_inspect::plan_status_snapshot;
    let valid = status_fixture(0, 0, 1);
    for (key, value) in valid.as_object().unwrap() {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(
            plan_status_snapshot(&bytes(&missing), &["t1"]).is_err(),
            "missing {key}"
        );
        let mut invalid = valid.clone();
        invalid[key] = Value::Null;
        assert!(
            plan_status_snapshot(&bytes(&invalid), &["t1"]).is_err(),
            "null {key}"
        );
        if value.is_u64() && key != "schema" {
            for bad in [json!(-1), json!(true), json!(1.0), json!("1")] {
                invalid[key] = bad;
                assert!(
                    plan_status_snapshot(&bytes(&invalid), &["t1"]).is_err(),
                    "{key}"
                );
            }
        } else if value.is_boolean() {
            invalid[key] = json!(1);
            assert!(
                plan_status_snapshot(&bytes(&invalid), &["t1"]).is_err(),
                "{key}"
            );
        }
    }
    for (key, value) in valid["contract"].as_object().unwrap() {
        for bad in [
            Value::Null,
            if value.is_boolean() {
                json!(0)
            } else {
                json!(false)
            },
        ] {
            let mut invalid = valid.clone();
            invalid["contract"][key] = bad;
            assert!(
                plan_status_snapshot(&bytes(&invalid), &["t1"]).is_err(),
                "{key}"
            );
        }
        let mut missing = valid.clone();
        missing["contract"].as_object_mut().unwrap().remove(key);
        assert!(
            plan_status_snapshot(&bytes(&missing), &["t1"]).is_err(),
            "{key}"
        );
    }
    for bad in [
        json!(["t1", "t1"]),
        json!([null]),
        json!([1]),
        json!([true]),
    ] {
        let mut invalid = valid.clone();
        invalid["actionable"] = bad;
        assert_eq!(
            plan_status_snapshot(&bytes(&invalid), &["t1"])
                .unwrap_err()
                .to_string(),
            "canonical plan status projection is unavailable or invalid"
        );
    }
    for raw in [b"".as_slice(), b"{", b"[]", b"null", b"{}", b"0"] {
        assert!(plan_status_snapshot(raw, &[]).is_err());
    }
}

#[test]
fn plan_status_snapshot_counts_and_membership() {
    use oms_work_inspect::plan_status_snapshot;
    for (done, cancelled, unfinished) in [
        (0, 0, 0),
        (1, 0, 0),
        (0, 1, 0),
        (0, 0, 1),
        (1, 1, 1),
        (9, 1, 0),
    ] {
        let mut valid = status_fixture(done, cancelled, unfinished);
        let ids = vec!["t1"; (done + cancelled + unfinished) as usize];
        // State histograms are not reconstructed: the canonical producer owns them.
        valid["by_state"]["future-state"] = json!({"opaque": true});
        valid["stale"] = json!([{"id": "t1", "age": 5}]);
        valid["stale_review"] = json!(["future-format"]);
        let raw = bytes(&valid);
        assert_eq!(
            plan_status_snapshot(&raw, &ids).unwrap(),
            oms_context::parse_json(&raw).unwrap()
        );
        for key in ["all_done", "all_closed", "has_unfinished"] {
            let mut invalid = valid.clone();
            invalid[key] = json!(!valid[key].as_bool().unwrap());
            assert_eq!(
                plan_status_snapshot(&bytes(&invalid), &ids)
                    .unwrap_err()
                    .to_string(),
                "canonical plan status counts are inconsistent",
                "{key}"
            );
        }
        for key in [
            "task_count",
            "verified_count",
            "cancelled_count",
            "closed_count",
            "unfinished_count",
        ] {
            let mut invalid = valid.clone();
            invalid[key] = json!(valid[key].as_u64().unwrap() + 1);
            assert_eq!(
                plan_status_snapshot(&bytes(&invalid), &ids)
                    .unwrap_err()
                    .to_string(),
                "canonical plan status counts are inconsistent",
                "{key}"
            );
        }
    }
    let mut valid = status_fixture(0, 0, 1);
    valid["by_state"] = json!({});
    valid["actionable"] = json!(["t1"]);
    assert!(plan_status_snapshot(&bytes(&valid), &["t1"]).is_ok());
    assert_eq!(
        plan_status_snapshot(&bytes(&valid), &[])
            .unwrap_err()
            .to_string(),
        "canonical plan status disagrees with the physical plan task set"
    );
    valid["nonempty"] = json!(false);
    assert_eq!(
        plan_status_snapshot(&bytes(&valid), &["t1"])
            .unwrap_err()
            .to_string(),
        "canonical plan status disagrees with the physical plan task set"
    );
    valid["nonempty"] = json!(true);
    assert_eq!(
        plan_status_snapshot(&bytes(&valid), &["t2"])
            .unwrap_err()
            .to_string(),
        "canonical plan status names an unknown actionable task"
    );
    for bad in [Value::Null, json!("0"), json!(-1), json!(0.5), json!(1)] {
        valid["by_state"]["done"] = bad;
        assert_eq!(
            plan_status_snapshot(&bytes(&valid), &["t1"])
                .unwrap_err()
                .to_string(),
            "canonical plan status counts are inconsistent"
        );
    }
    let mut huge = String::from_utf8(bytes(&status_fixture(1, 1, 0))).unwrap();
    for key in ["task_count", "closed_count"] {
        huge = huge.replace(
            &format!("\"{key}\":2"),
            &format!("\"{key}\":20000000000000000000000000000000000000000"),
        );
    }
    huge = huge.replace(":1", ":10000000000000000000000000000000000000000");
    // Restore schema after replacing count literals.
    huge = huge.replace(
        "\"schema\":10000000000000000000000000000000000000000",
        "\"schema\":1",
    );
    assert_eq!(
        plan_status_snapshot(huge.as_bytes(), &["t1", "t2"])
            .unwrap_err()
            .to_string(),
        "canonical plan status disagrees with the physical plan task set"
    );
}

#[test]
fn status_snapshot_lossless_dialect_and_bounds() {
    use oms_work_inspect::{plan_status_snapshot, task_status_snapshot};
    let task = br#"{"schema":true,"present":true,"status":"old","status":"\ud800","verification":"","stale":false,"future":[NaN,Infinity,-Infinity,123456789012345678901234567890]}"#;
    assert_eq!(
        task_status_snapshot(task).unwrap(),
        oms_context::parse_experiment_json(task, task.len()).unwrap()
    );
    let mut plan = String::from_utf8(bytes(&status_fixture(1, 0, 0))).unwrap();
    plan = plan
        .replace("\"schema\":1", "\"schema\":1e0")
        .replace("\"done\":1", "\"done\":1.0");
    assert!(plan_status_snapshot(plan.as_bytes(), &["t1"]).is_ok());
    plan = plan.replace("\"done\":1.0", "\"done\":true");
    assert!(plan_status_snapshot(plan.as_bytes(), &["t1"]).is_ok());
    for bad in ["NaN", "Infinity", "-Infinity", "1e400", "-0.5"] {
        let invalid = plan.replace("\"done\":true", &format!("\"done\":{bad}"));
        assert_eq!(
            plan_status_snapshot(invalid.as_bytes(), &["t1"])
                .unwrap_err()
                .to_string(),
            "canonical plan status counts are inconsistent"
        );
    }
    let zero = String::from_utf8(bytes(&status_fixture(0, 0, 0))).unwrap();
    let signed_zero = zero
        .replace(":0", ":-0")
        .replace("\"done\":-0", "\"done\":-0.0");
    assert!(plan_status_snapshot(signed_zero.as_bytes(), &[]).is_ok());
    let unicode_id = "한글\n";
    let mut unicode_plan = status_fixture(0, 0, 1);
    unicode_plan["actionable"] = json!([unicode_id]);
    assert!(plan_status_snapshot(&bytes(&unicode_plan), &[unicode_id]).is_ok());
    let limit = 4 * 1024 * 1024;
    let mut at_limit = task.to_vec();
    at_limit.resize(limit, b' ');
    assert!(task_status_snapshot(&at_limit).is_ok());
    at_limit.push(b' ');
    assert!(task_status_snapshot(&at_limit).is_err());
    let base = String::from_utf8(bytes(&status_fixture(0, 0, 0))).unwrap();
    let mut at_limit = base.into_bytes();
    at_limit.resize(limit, b' ');
    assert!(plan_status_snapshot(&at_limit, &[]).is_ok());
    at_limit.push(b' ');
    assert!(plan_status_snapshot(&at_limit, &[]).is_err());
    for (depth, valid) in [(1023, true), (1024, false)] {
        let raw = format!(
            "{{\"schema\":1,\"present\":true,\"status\":\"\",\"verification\":\"\",\"stale\":false,\"future\":{}0{}}}",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        assert_eq!(task_status_snapshot(raw.as_bytes()).is_ok(), valid);
    }
}
#[test]
fn views_and_render() {
    let v = fixture();
    let raw = bytes(&v);
    let m = evidence_matrix(&raw, 20, None).unwrap();
    assert_eq!(
        m.as_value().unwrap()["criteria"]["items"][0]["link"],
        "plan-task-lineage"
    );
    assert_eq!(
        m.as_value().unwrap()["criteria"]["items"][0]["status"],
        "missing"
    );
    assert!(render(&m).unwrap().ends_with('\n'));
    let i = action_inbox(&raw, 20, None).unwrap();
    assert_eq!(
        i.as_value().unwrap()["tasks"]["items"][0]["reasons"][0]["code"],
        "ready"
    );
    let b = return_brief(&raw, Some(&raw), 20).unwrap();
    assert_eq!(b.as_value().unwrap()["recorded_scope_matches"], true);
    assert_eq!(b.as_value().unwrap()["history_complete"], false);
    assert!(render(&b).unwrap().contains("baseline: compared"));
}
#[test]
fn boundary() {
    let mut v = fixture();
    let raw = bytes(&v);
    assert!(evidence_matrix(&raw, 0, None).is_err());
    assert!(evidence_matrix(&raw, 101, None).is_err());
    assert!(evidence_matrix(&raw, 1, Some("absent")).is_err());
    v["schema"] = json!(true);
    assert!(evidence_matrix(&bytes(&v), 1, None).is_err());
    v = fixture();
    let duplicate = v["criteria"][0].clone();
    v["criteria"].as_array_mut().unwrap().push(duplicate);
    assert!(evidence_matrix(&bytes(&v), 1, None).is_err());
}
#[test]
fn legacy_parity() {
    let cases: Value = serde_json::from_str(include_str!("legacy_cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let current = bytes(&case["current"]);
        let before = bytes(&case["before"]);
        let limit = case["limit"].as_u64().unwrap() as usize;
        let reports = [
            evidence_matrix(&current, limit, None).unwrap(),
            action_inbox(&current, limit, None).unwrap(),
            return_brief(&current, Some(&before), limit).unwrap(),
        ];
        for (index, report) in reports.iter().enumerate() {
            assert_eq!(report.as_value().unwrap(), &case["reports"][index]);
            assert_eq!(
                render(report).unwrap(),
                case["texts"][index].as_str().unwrap()
            );
        }
    }
}
#[test]
fn bounds_unicode_and_lineage() {
    let mut v = fixture();
    v["criteria"][0]["text"] = json!("한글\u{fffd}\u{202e}");
    let raw = bytes(&v);
    let report = evidence_matrix(&raw, 100, None).unwrap();
    assert_eq!(
        report.as_value().unwrap()["criteria"]["items"][0]["criterion"],
        "한글�?"
    );
    let mut old = fixture();
    old["generated_at"] = json!("2026-11-11T00:00:00Z");
    let brief = return_brief(&bytes(&v), Some(&bytes(&old)), 1).unwrap();
    assert_eq!(
        brief.as_value().unwrap()["mismatch_reasons"][0],
        "baseline-in-future"
    );
    old["generated_at"] = json!("2026-11-10T09:00:00+09:00");
    assert_eq!(
        return_brief(&bytes(&v), Some(&bytes(&old)), 1)
            .unwrap()
            .as_value()
            .unwrap()["recorded_scope_matches"],
        true
    );
    old["generated_at"] = json!("2026-11-10T00:00:00");
    assert!(return_brief(&bytes(&v), Some(&bytes(&old)), 1).is_err());
    v["criteria"] = json!(vec![v["criteria"][0].clone(); 4097]);
    assert!(action_inbox(&bytes(&v), 1, None).is_err());
}
#[test]
fn escaped_surrogate_is_sanitized() {
    let raw = String::from_utf8(bytes(&fixture()))
        .unwrap()
        .replace("확인할 기준", "\\ud800");
    let report = evidence_matrix(raw.as_bytes(), 1, None).unwrap();
    assert_eq!(
        report.as_value().unwrap()["criteria"]["items"][0]["criterion"],
        "?"
    );
}
#[test]
fn timestamp_and_parse_regressions() {
    let mut v = fixture();
    for stamp in [
        "20261110T0000+0000",
        "2026-W46-2X00:00+00:00",
        "2026-11-10 00:00:00,123456+00:00:30.5",
    ] {
        v["generated_at"] = json!(stamp);
        assert_eq!(
            evidence_matrix(&bytes(&v), 20, None)
                .unwrap()
                .as_value()
                .unwrap()["generated_at"],
            stamp
        );
    }
    for stamp in ["2026-11-10T00:00:005+00:00", "2026-11-10T001+00:00"] {
        v["generated_at"] = json!(stamp);
        assert!(evidence_matrix(&bytes(&v), 20, None).is_ok(), "{stamp}");
    }
    for stamp in [
        "2026-11-10T00:00:00.Z",
        "2026-11-10T00:00:00,+00:00",
        "2026-11-10T12345678+00:00",
        "2026-11-10T00:00:00+01020345",
    ] {
        v["generated_at"] = json!(stamp);
        assert!(evidence_matrix(&bytes(&v), 20, None).is_ok(), "{stamp}");
    }
    for stamp in [
        "2026-W46T00:00+00:00",
        "2026W46T00:00+00:00",
        "20261110T000000+235959.999999",
        "20261110T000000-235959.999999",
        "2026-11-10T00:00:00+00:00:00.5",
        "0001-01-01T01:00+01:00",
        "9999-12-31T22:59:59-01:00",
    ] {
        v["generated_at"] = json!(stamp);
        assert!(evidence_matrix(&bytes(&v), 20, None).is_ok(), "{stamp}");
    }
    for stamp in [
        "2026-11-10T00:00:60Z",
        "+001-01-01T00:00Z",
        "+202-11-10T00:00:00Z",
        " 202-11-10T00:00:00Z",
        "2026- 1-10T00:00:00Z",
        "2026-\t1-10T00:00:00Z",
        "2026-11- 1T00:00:00Z",
        "202611 1T00:00:00Z",
        "2026-W-4-6T00:00:00Z",
        "2026-W4--6T00:00:00Z",
        "+202-W46-2T00:00:00Z",
        "+202W462T00:00:00Z",
        "2026-W+4-2T00:00:00Z",
        "2026W+42T00:00:00Z",
        "2026-11-10T00:00:00.12x+00:00",
        "2026-11-10T00:00:00가+00:00",
        "0000-W01-7T00:00:00Z",
        "0001-01-01T00:00:00+01:00",
        "9999-12-31T23:59:59-01:00",
        "2026-11-10T00:00:00z",
        "2026-11-10T00:00:00",
    ] {
        v["generated_at"] = json!(stamp);
        assert!(evidence_matrix(&bytes(&v), 20, None).is_err());
    }
    v["generated_at"] = json!("2026-11-10\t00:00+0000");
    let report = evidence_matrix(&bytes(&v), 20, None).unwrap();
    assert_eq!(
        report.as_value().unwrap()["generated_at"],
        "2026-11-10\t00:00+0000"
    );
    let mut before = fixture();
    before["generated_at"] = json!("9999-01-01T00:00:00.000002Z");
    v["generated_at"] = json!("9999-01-01T00:00:00.000001Z");
    assert_eq!(
        return_brief(&bytes(&v), Some(&bytes(&before)), 20)
            .unwrap()
            .as_value()
            .unwrap()["mismatch_reasons"][0],
        "baseline-in-future"
    );
    assert!(
        !evidence_matrix(b"{", 20, None)
            .unwrap_err()
            .to_string()
            .contains("context manifest")
    );
}

#[test]
fn surrogate_timestamp_brief() {
    let raw = String::from_utf8(bytes(&fixture()))
        .unwrap()
        .replace("2026-11-10T00:00:00Z", "2026-11-10\\ud80000:00Z");
    let matrix = evidence_matrix(raw.as_bytes(), 20, None).unwrap();
    let inbox = action_inbox(raw.as_bytes(), 20, None).unwrap();
    let brief = return_brief(raw.as_bytes(), Some(raw.as_bytes()), 20).unwrap();
    for report in [&matrix, &inbox, &brief] {
        assert_eq!(
            report.as_value().unwrap_err(),
            oms_work_inspect::ReportEncodingError::LoneSurrogate
        );
        assert!(
            report
                .generated_at_wtf8()
                .windows(3)
                .any(|w| w == [0xed, 0xa0, 0x80])
        );
        assert!(render(report).is_ok());
    }
    assert_eq!(brief.generated_at_wtf8(), brief.baseline_at_wtf8().unwrap());
    let mut ordinary = fixture();
    ordinary["generated_at"] = json!("2026-11-10�00:00Z");
    assert_eq!(
        evidence_matrix(&bytes(&ordinary), 20, None)
            .unwrap()
            .as_value()
            .unwrap()["generated_at"],
        "2026-11-10�00:00Z"
    );
    let long = format!("2026-11-10\\ud80000:00:00.{}Z", "1".repeat(59));
    let raw = String::from_utf8(bytes(&fixture()))
        .unwrap()
        .replace("2026-11-10T00:00:00Z", &long);
    assert!(return_brief(raw.as_bytes(), Some(raw.as_bytes()), 20).is_ok());
    let too_long = long.replace(&"1".repeat(59), &"1".repeat(60));
    let raw = String::from_utf8(bytes(&fixture()))
        .unwrap()
        .replace("2026-11-10T00:00:00Z", &too_long);
    assert!(evidence_matrix(raw.as_bytes(), 20, None).is_err());
}
#[test]
fn timestamp_scanner_legacy_corpus() {
    let cases: Value = serde_json::from_str(r#"[["2026-11-10T00+00:00", true], ["2026-11-10T00-01", true], ["2026-11-10T00+00:00:30.5", true], ["2026-11-10T00+24:00", false], ["2026-11-10T001+00:00", true], ["2026-11-10T001-01", true], ["2026-11-10T001+00:00:30.5", true], ["2026-11-10T001+24:00", false], ["2026-11-10T0011+00:00", true], ["2026-11-10T0011-01", true], ["2026-11-10T0011+00:00:30.5", true], ["2026-11-10T0011+24:00", false], ["2026-11-10T00111+00:00", true], ["2026-11-10T00111-01", true], ["2026-11-10T00111+00:00:30.5", true], ["2026-11-10T00111+24:00", false], ["2026-11-10T001111+00:00", true], ["2026-11-10T001111-01", true], ["2026-11-10T001111+00:00:30.5", true], ["2026-11-10T001111+24:00", false], ["2026-11-10T0011111+00:00", true], ["2026-11-10T0011111-01", true], ["2026-11-10T0011111+00:00:30.5", true], ["2026-11-10T0011111+24:00", false], ["2026-11-10T00:00+00:00", true], ["2026-11-10T00:00-01", true], ["2026-11-10T00:00+00:00:30.5", true], ["2026-11-10T00:00+24:00", false], ["2026-11-10T00:001+00:00", true], ["2026-11-10T00:001-01", true], ["2026-11-10T00:001+00:00:30.5", true], ["2026-11-10T00:001+24:00", false], ["2026-11-10T00:0011+00:00", false], ["2026-11-10T00:0011-01", false], ["2026-11-10T00:0011+00:00:30.5", false], ["2026-11-10T00:0011+24:00", false], ["2026-11-10T00:00:00+00:00", true], ["2026-11-10T00:00:00-01", true], ["2026-11-10T00:00:00+00:00:30.5", true], ["2026-11-10T00:00:00+24:00", false], ["2026-11-10T00:00:001+00:00", true], ["2026-11-10T00:00:001-01", true], ["2026-11-10T00:00:001+00:00:30.5", true], ["2026-11-10T00:00:001+24:00", false], ["2026-11-10T00x+00:00", true], ["2026-11-10T00x-01", true], ["2026-11-10T00x+00:00:30.5", true], ["2026-11-10T00x+24:00", false], ["2026-11-10T00xx+00:00", false], ["2026-11-10T00xx-01", false], ["2026-11-10T00xx+00:00:30.5", false], ["2026-11-10T00xx+24:00", false], ["2026-11-10T00:00:00x+00:00", true], ["2026-11-10T00:00:00x-01", true], ["2026-11-10T00:00:00x+00:00:30.5", true], ["2026-11-10T00:00:00x+24:00", false], ["2026-11-10T00:00:00xy+00:00", false], ["2026-11-10T00:00:00xy-01", false], ["2026-11-10T00:00:00xy+00:00:30.5", false], ["2026-11-10T00:00:00xy+24:00", false], ["2026-11-10T00:00:00.1x+00:00", false], ["2026-11-10T00:00:00.1x-01", false], ["2026-11-10T00:00:00.1x+00:00:30.5", false], ["2026-11-10T00:00:00.1x+24:00", false], ["2026-11-10T00:00:00.1234567+00:00", true], ["2026-11-10T00:00:00.1234567-01", true], ["2026-11-10T00:00:00.1234567+00:00:30.5", true], ["2026-11-10T00:00:00.1234567+24:00", false], ["2026-11-10T23:59:59+00:00", true], ["2026-11-10T23:59:59-01", true], ["2026-11-10T23:59:59+00:00:30.5", true], ["2026-11-10T23:59:59+24:00", false], ["2026-11-10T24:00+00:00", false], ["2026-11-10T24:00-01", false], ["2026-11-10T24:00+00:00:30.5", false], ["2026-11-10T24:00+24:00", false], ["2026-11-10T00:60+00:00", false], ["2026-11-10T00:60-01", false], ["2026-11-10T00:60+00:00:30.5", false], ["2026-11-10T00:60+24:00", false], ["2026-11-10T00:00:60+00:00", false], ["2026-11-10T00:00:60-01", false], ["2026-11-10T00:00:60+00:00:30.5", false], ["2026-11-10T00:00:60+24:00", false], ["2026-11-10T00:00:00\uac00+00:00", false], ["2026-11-10T00:00:00\uac00-01", false], ["2026-11-10T00:00:00\uac00+00:00:30.5", false], ["2026-11-10T00:00:00\uac00+24:00", false], ["20261110T00+00:00", true], ["20261110T00-01", true], ["20261110T00+00:00:30.5", true], ["20261110T00+24:00", false], ["20261110T001+00:00", true], ["20261110T001-01", true], ["20261110T001+00:00:30.5", true], ["20261110T001+24:00", false], ["20261110T0011+00:00", true], ["20261110T0011-01", true], ["20261110T0011+00:00:30.5", true], ["20261110T0011+24:00", false], ["20261110T00111+00:00", true], ["20261110T00111-01", true], ["20261110T00111+00:00:30.5", true], ["20261110T00111+24:00", false], ["20261110T001111+00:00", true], ["20261110T001111-01", true], ["20261110T001111+00:00:30.5", true], ["20261110T001111+24:00", false], ["20261110T0011111+00:00", true], ["20261110T0011111-01", true], ["20261110T0011111+00:00:30.5", true], ["20261110T0011111+24:00", false], ["20261110T00:00+00:00", true], ["20261110T00:00-01", true], ["20261110T00:00+00:00:30.5", true], ["20261110T00:00+24:00", false], ["20261110T00:001+00:00", true], ["20261110T00:001-01", true], ["20261110T00:001+00:00:30.5", true], ["20261110T00:001+24:00", false], ["20261110T00:0011+00:00", false], ["20261110T00:0011-01", false], ["20261110T00:0011+00:00:30.5", false], ["20261110T00:0011+24:00", false], ["20261110T00:00:00+00:00", true], ["20261110T00:00:00-01", true], ["20261110T00:00:00+00:00:30.5", true], ["20261110T00:00:00+24:00", false], ["20261110T00:00:001+00:00", true], ["20261110T00:00:001-01", true], ["20261110T00:00:001+00:00:30.5", true], ["20261110T00:00:001+24:00", false], ["20261110T00x+00:00", true], ["20261110T00x-01", true], ["20261110T00x+00:00:30.5", true], ["20261110T00x+24:00", false], ["20261110T00xx+00:00", false], ["20261110T00xx-01", false], ["20261110T00xx+00:00:30.5", false], ["20261110T00xx+24:00", false], ["20261110T00:00:00x+00:00", true], ["20261110T00:00:00x-01", true], ["20261110T00:00:00x+00:00:30.5", true], ["20261110T00:00:00x+24:00", false], ["20261110T00:00:00xy+00:00", false], ["20261110T00:00:00xy-01", false], ["20261110T00:00:00xy+00:00:30.5", false], ["20261110T00:00:00xy+24:00", false], ["20261110T00:00:00.1x+00:00", false], ["20261110T00:00:00.1x-01", false], ["20261110T00:00:00.1x+00:00:30.5", false], ["20261110T00:00:00.1x+24:00", false], ["20261110T00:00:00.1234567+00:00", true], ["20261110T00:00:00.1234567-01", true], ["20261110T00:00:00.1234567+00:00:30.5", true], ["20261110T00:00:00.1234567+24:00", false], ["20261110T23:59:59+00:00", true], ["20261110T23:59:59-01", true], ["20261110T23:59:59+00:00:30.5", true], ["20261110T23:59:59+24:00", false], ["20261110T24:00+00:00", false], ["20261110T24:00-01", false], ["20261110T24:00+00:00:30.5", false], ["20261110T24:00+24:00", false], ["20261110T00:60+00:00", false], ["20261110T00:60-01", false], ["20261110T00:60+00:00:30.5", false], ["20261110T00:60+24:00", false], ["20261110T00:00:60+00:00", false], ["20261110T00:00:60-01", false], ["20261110T00:00:60+00:00:30.5", false], ["20261110T00:00:60+24:00", false], ["20261110T00:00:00\uac00+00:00", false], ["20261110T00:00:00\uac00-01", false], ["20261110T00:00:00\uac00+00:00:30.5", false], ["20261110T00:00:00\uac00+24:00", false], ["2026-W46-2T00+00:00", true], ["2026-W46-2T00-01", true], ["2026-W46-2T00+00:00:30.5", true], ["2026-W46-2T00+24:00", false], ["2026-W46-2T001+00:00", true], ["2026-W46-2T001-01", true], ["2026-W46-2T001+00:00:30.5", true], ["2026-W46-2T001+24:00", false], ["2026-W46-2T0011+00:00", true], ["2026-W46-2T0011-01", true], ["2026-W46-2T0011+00:00:30.5", true], ["2026-W46-2T0011+24:00", false], ["2026-W46-2T00111+00:00", true], ["2026-W46-2T00111-01", true], ["2026-W46-2T00111+00:00:30.5", true], ["2026-W46-2T00111+24:00", false], ["2026-W46-2T001111+00:00", true], ["2026-W46-2T001111-01", true], ["2026-W46-2T001111+00:00:30.5", true], ["2026-W46-2T001111+24:00", false], ["2026-W46-2T0011111+00:00", true], ["2026-W46-2T0011111-01", true], ["2026-W46-2T0011111+00:00:30.5", true], ["2026-W46-2T0011111+24:00", false], ["2026-W46-2T00:00+00:00", true], ["2026-W46-2T00:00-01", true], ["2026-W46-2T00:00+00:00:30.5", true], ["2026-W46-2T00:00+24:00", false], ["2026-W46-2T00:001+00:00", true], ["2026-W46-2T00:001-01", true], ["2026-W46-2T00:001+00:00:30.5", true], ["2026-W46-2T00:001+24:00", false], ["2026-W46-2T00:0011+00:00", false], ["2026-W46-2T00:0011-01", false], ["2026-W46-2T00:0011+00:00:30.5", false], ["2026-W46-2T00:0011+24:00", false], ["2026-W46-2T00:00:00+00:00", true], ["2026-W46-2T00:00:00-01", true], ["2026-W46-2T00:00:00+00:00:30.5", true], ["2026-W46-2T00:00:00+24:00", false], ["2026-W46-2T00:00:001+00:00", true], ["2026-W46-2T00:00:001-01", true], ["2026-W46-2T00:00:001+00:00:30.5", true], ["2026-W46-2T00:00:001+24:00", false], ["2026-W46-2T00x+00:00", true], ["2026-W46-2T00x-01", true], ["2026-W46-2T00x+00:00:30.5", true], ["2026-W46-2T00x+24:00", false], ["2026-W46-2T00xx+00:00", false], ["2026-W46-2T00xx-01", false], ["2026-W46-2T00xx+00:00:30.5", false], ["2026-W46-2T00xx+24:00", false], ["2026-W46-2T00:00:00x+00:00", true], ["2026-W46-2T00:00:00x-01", true], ["2026-W46-2T00:00:00x+00:00:30.5", true], ["2026-W46-2T00:00:00x+24:00", false], ["2026-W46-2T00:00:00xy+00:00", false], ["2026-W46-2T00:00:00xy-01", false], ["2026-W46-2T00:00:00xy+00:00:30.5", false], ["2026-W46-2T00:00:00xy+24:00", false], ["2026-W46-2T00:00:00.1x+00:00", false], ["2026-W46-2T00:00:00.1x-01", false], ["2026-W46-2T00:00:00.1x+00:00:30.5", false], ["2026-W46-2T00:00:00.1x+24:00", false], ["2026-W46-2T00:00:00.1234567+00:00", true], ["2026-W46-2T00:00:00.1234567-01", true], ["2026-W46-2T00:00:00.1234567+00:00:30.5", true], ["2026-W46-2T00:00:00.1234567+24:00", false], ["2026-W46-2T23:59:59+00:00", true], ["2026-W46-2T23:59:59-01", true], ["2026-W46-2T23:59:59+00:00:30.5", true], ["2026-W46-2T23:59:59+24:00", false], ["2026-W46-2T24:00+00:00", false], ["2026-W46-2T24:00-01", false], ["2026-W46-2T24:00+00:00:30.5", false], ["2026-W46-2T24:00+24:00", false], ["2026-W46-2T00:60+00:00", false], ["2026-W46-2T00:60-01", false], ["2026-W46-2T00:60+00:00:30.5", false], ["2026-W46-2T00:60+24:00", false], ["2026-W46-2T00:00:60+00:00", false], ["2026-W46-2T00:00:60-01", false], ["2026-W46-2T00:00:60+00:00:30.5", false], ["2026-W46-2T00:00:60+24:00", false], ["2026-W46-2T00:00:00\uac00+00:00", false], ["2026-W46-2T00:00:00\uac00-01", false], ["2026-W46-2T00:00:00\uac00+00:00:30.5", false], ["2026-W46-2T00:00:00\uac00+24:00", false], ["2026W462T00+00:00", true], ["2026W462T00-01", true], ["2026W462T00+00:00:30.5", true], ["2026W462T00+24:00", false], ["2026W462T001+00:00", true], ["2026W462T001-01", true], ["2026W462T001+00:00:30.5", true], ["2026W462T001+24:00", false], ["2026W462T0011+00:00", true], ["2026W462T0011-01", true], ["2026W462T0011+00:00:30.5", true], ["2026W462T0011+24:00", false], ["2026W462T00111+00:00", true], ["2026W462T00111-01", true], ["2026W462T00111+00:00:30.5", true], ["2026W462T00111+24:00", false], ["2026W462T001111+00:00", true], ["2026W462T001111-01", true], ["2026W462T001111+00:00:30.5", true], ["2026W462T001111+24:00", false], ["2026W462T0011111+00:00", true], ["2026W462T0011111-01", true], ["2026W462T0011111+00:00:30.5", true], ["2026W462T0011111+24:00", false], ["2026W462T00:00+00:00", true], ["2026W462T00:00-01", true], ["2026W462T00:00+00:00:30.5", true], ["2026W462T00:00+24:00", false], ["2026W462T00:001+00:00", true], ["2026W462T00:001-01", true], ["2026W462T00:001+00:00:30.5", true], ["2026W462T00:001+24:00", false], ["2026W462T00:0011+00:00", false], ["2026W462T00:0011-01", false], ["2026W462T00:0011+00:00:30.5", false], ["2026W462T00:0011+24:00", false], ["2026W462T00:00:00+00:00", true], ["2026W462T00:00:00-01", true], ["2026W462T00:00:00+00:00:30.5", true], ["2026W462T00:00:00+24:00", false], ["2026W462T00:00:001+00:00", true], ["2026W462T00:00:001-01", true], ["2026W462T00:00:001+00:00:30.5", true], ["2026W462T00:00:001+24:00", false], ["2026W462T00x+00:00", true], ["2026W462T00x-01", true], ["2026W462T00x+00:00:30.5", true], ["2026W462T00x+24:00", false], ["2026W462T00xx+00:00", false], ["2026W462T00xx-01", false], ["2026W462T00xx+00:00:30.5", false], ["2026W462T00xx+24:00", false], ["2026W462T00:00:00x+00:00", true], ["2026W462T00:00:00x-01", true], ["2026W462T00:00:00x+00:00:30.5", true], ["2026W462T00:00:00x+24:00", false], ["2026W462T00:00:00xy+00:00", false], ["2026W462T00:00:00xy-01", false], ["2026W462T00:00:00xy+00:00:30.5", false], ["2026W462T00:00:00xy+24:00", false], ["2026W462T00:00:00.1x+00:00", false], ["2026W462T00:00:00.1x-01", false], ["2026W462T00:00:00.1x+00:00:30.5", false], ["2026W462T00:00:00.1x+24:00", false], ["2026W462T00:00:00.1234567+00:00", true], ["2026W462T00:00:00.1234567-01", true], ["2026W462T00:00:00.1234567+00:00:30.5", true], ["2026W462T00:00:00.1234567+24:00", false], ["2026W462T23:59:59+00:00", true], ["2026W462T23:59:59-01", true], ["2026W462T23:59:59+00:00:30.5", true], ["2026W462T23:59:59+24:00", false], ["2026W462T24:00+00:00", false], ["2026W462T24:00-01", false], ["2026W462T24:00+00:00:30.5", false], ["2026W462T24:00+24:00", false], ["2026W462T00:60+00:00", false], ["2026W462T00:60-01", false], ["2026W462T00:60+00:00:30.5", false], ["2026W462T00:60+24:00", false], ["2026W462T00:00:60+00:00", false], ["2026W462T00:00:60-01", false], ["2026W462T00:00:60+00:00:30.5", false], ["2026W462T00:00:60+24:00", false], ["2026W462T00:00:00\uac00+00:00", false], ["2026W462T00:00:00\uac00-01", false], ["2026W462T00:00:00\uac00+00:00:30.5", false], ["2026W462T00:00:00\uac00+24:00", false], ["0000-W01-7T00+00:00", false], ["0000-W01-7T00-01", false], ["0000-W01-7T00+00:00:30.5", false], ["0000-W01-7T00+24:00", false], ["0000-W01-7T001+00:00", false], ["0000-W01-7T001-01", false], ["0000-W01-7T001+00:00:30.5", false], ["0000-W01-7T001+24:00", false], ["0000-W01-7T0011+00:00", false], ["0000-W01-7T0011-01", false], ["0000-W01-7T0011+00:00:30.5", false], ["0000-W01-7T0011+24:00", false], ["0000-W01-7T00111+00:00", false], ["0000-W01-7T00111-01", false], ["0000-W01-7T00111+00:00:30.5", false], ["0000-W01-7T00111+24:00", false], ["0000-W01-7T001111+00:00", false], ["0000-W01-7T001111-01", false], ["0000-W01-7T001111+00:00:30.5", false], ["0000-W01-7T001111+24:00", false], ["0000-W01-7T0011111+00:00", false], ["0000-W01-7T0011111-01", false], ["0000-W01-7T0011111+00:00:30.5", false], ["0000-W01-7T0011111+24:00", false], ["0000-W01-7T00:00+00:00", false], ["0000-W01-7T00:00-01", false], ["0000-W01-7T00:00+00:00:30.5", false], ["0000-W01-7T00:00+24:00", false], ["0000-W01-7T00:001+00:00", false], ["0000-W01-7T00:001-01", false], ["0000-W01-7T00:001+00:00:30.5", false], ["0000-W01-7T00:001+24:00", false], ["0000-W01-7T00:0011+00:00", false], ["0000-W01-7T00:0011-01", false], ["0000-W01-7T00:0011+00:00:30.5", false], ["0000-W01-7T00:0011+24:00", false], ["0000-W01-7T00:00:00+00:00", false], ["0000-W01-7T00:00:00-01", false], ["0000-W01-7T00:00:00+00:00:30.5", false], ["0000-W01-7T00:00:00+24:00", false], ["0000-W01-7T00:00:001+00:00", false], ["0000-W01-7T00:00:001-01", false], ["0000-W01-7T00:00:001+00:00:30.5", false], ["0000-W01-7T00:00:001+24:00", false], ["0000-W01-7T00x+00:00", false], ["0000-W01-7T00x-01", false], ["0000-W01-7T00x+00:00:30.5", false], ["0000-W01-7T00x+24:00", false], ["0000-W01-7T00xx+00:00", false], ["0000-W01-7T00xx-01", false], ["0000-W01-7T00xx+00:00:30.5", false], ["0000-W01-7T00xx+24:00", false], ["0000-W01-7T00:00:00x+00:00", false], ["0000-W01-7T00:00:00x-01", false], ["0000-W01-7T00:00:00x+00:00:30.5", false], ["0000-W01-7T00:00:00x+24:00", false], ["0000-W01-7T00:00:00xy+00:00", false], ["0000-W01-7T00:00:00xy-01", false], ["0000-W01-7T00:00:00xy+00:00:30.5", false], ["0000-W01-7T00:00:00xy+24:00", false], ["0000-W01-7T00:00:00.1x+00:00", false], ["0000-W01-7T00:00:00.1x-01", false], ["0000-W01-7T00:00:00.1x+00:00:30.5", false], ["0000-W01-7T00:00:00.1x+24:00", false], ["0000-W01-7T00:00:00.1234567+00:00", false], ["0000-W01-7T00:00:00.1234567-01", false], ["0000-W01-7T00:00:00.1234567+00:00:30.5", false], ["0000-W01-7T00:00:00.1234567+24:00", false], ["0000-W01-7T23:59:59+00:00", false], ["0000-W01-7T23:59:59-01", false], ["0000-W01-7T23:59:59+00:00:30.5", false], ["0000-W01-7T23:59:59+24:00", false], ["0000-W01-7T24:00+00:00", false], ["0000-W01-7T24:00-01", false], ["0000-W01-7T24:00+00:00:30.5", false], ["0000-W01-7T24:00+24:00", false], ["0000-W01-7T00:60+00:00", false], ["0000-W01-7T00:60-01", false], ["0000-W01-7T00:60+00:00:30.5", false], ["0000-W01-7T00:60+24:00", false], ["0000-W01-7T00:00:60+00:00", false], ["0000-W01-7T00:00:60-01", false], ["0000-W01-7T00:00:60+00:00:30.5", false], ["0000-W01-7T00:00:60+24:00", false], ["0000-W01-7T00:00:00\uac00+00:00", false], ["0000-W01-7T00:00:00\uac00-01", false], ["0000-W01-7T00:00:00\uac00+00:00:30.5", false], ["0000-W01-7T00:00:00\uac00+24:00", false], ["2026-W46T00+00:00", true], ["2026-W46T00-01", true], ["2026-W46T00+00:00:30.5", true], ["2026-W46T00+24:00", false], ["2026-W46T001+00:00", true], ["2026-W46T001-01", true], ["2026-W46T001+00:00:30.5", true], ["2026-W46T001+24:00", false], ["2026-W46T0011+00:00", true], ["2026-W46T0011-01", true], ["2026-W46T0011+00:00:30.5", true], ["2026-W46T0011+24:00", false], ["2026-W46T00111+00:00", true], ["2026-W46T00111-01", true], ["2026-W46T00111+00:00:30.5", true], ["2026-W46T00111+24:00", false], ["2026-W46T001111+00:00", true], ["2026-W46T001111-01", true], ["2026-W46T001111+00:00:30.5", true], ["2026-W46T001111+24:00", false], ["2026-W46T0011111+00:00", true], ["2026-W46T0011111-01", true], ["2026-W46T0011111+00:00:30.5", true], ["2026-W46T0011111+24:00", false], ["2026-W46T00:00+00:00", true], ["2026-W46T00:00-01", true], ["2026-W46T00:00+00:00:30.5", true], ["2026-W46T00:00+24:00", false], ["2026-W46T00:001+00:00", true], ["2026-W46T00:001-01", true], ["2026-W46T00:001+00:00:30.5", true], ["2026-W46T00:001+24:00", false], ["2026-W46T00:0011+00:00", false], ["2026-W46T00:0011-01", false], ["2026-W46T00:0011+00:00:30.5", false], ["2026-W46T00:0011+24:00", false], ["2026-W46T00:00:00+00:00", true], ["2026-W46T00:00:00-01", true], ["2026-W46T00:00:00+00:00:30.5", true], ["2026-W46T00:00:00+24:00", false], ["2026-W46T00:00:001+00:00", true], ["2026-W46T00:00:001-01", true], ["2026-W46T00:00:001+00:00:30.5", true], ["2026-W46T00:00:001+24:00", false], ["2026-W46T00x+00:00", true], ["2026-W46T00x-01", true], ["2026-W46T00x+00:00:30.5", true], ["2026-W46T00x+24:00", false], ["2026-W46T00xx+00:00", false], ["2026-W46T00xx-01", false], ["2026-W46T00xx+00:00:30.5", false], ["2026-W46T00xx+24:00", false], ["2026-W46T00:00:00x+00:00", true], ["2026-W46T00:00:00x-01", true], ["2026-W46T00:00:00x+00:00:30.5", true], ["2026-W46T00:00:00x+24:00", false], ["2026-W46T00:00:00xy+00:00", false], ["2026-W46T00:00:00xy-01", false], ["2026-W46T00:00:00xy+00:00:30.5", false], ["2026-W46T00:00:00xy+24:00", false], ["2026-W46T00:00:00.1x+00:00", false], ["2026-W46T00:00:00.1x-01", false], ["2026-W46T00:00:00.1x+00:00:30.5", false], ["2026-W46T00:00:00.1x+24:00", false], ["2026-W46T00:00:00.1234567+00:00", true], ["2026-W46T00:00:00.1234567-01", true], ["2026-W46T00:00:00.1234567+00:00:30.5", true], ["2026-W46T00:00:00.1234567+24:00", false], ["2026-W46T23:59:59+00:00", true], ["2026-W46T23:59:59-01", true], ["2026-W46T23:59:59+00:00:30.5", true], ["2026-W46T23:59:59+24:00", false], ["2026-W46T24:00+00:00", false], ["2026-W46T24:00-01", false], ["2026-W46T24:00+00:00:30.5", false], ["2026-W46T24:00+24:00", false], ["2026-W46T00:60+00:00", false], ["2026-W46T00:60-01", false], ["2026-W46T00:60+00:00:30.5", false], ["2026-W46T00:60+24:00", false], ["2026-W46T00:00:60+00:00", false], ["2026-W46T00:00:60-01", false], ["2026-W46T00:00:60+00:00:30.5", false], ["2026-W46T00:00:60+24:00", false], ["2026-W46T00:00:00\uac00+00:00", false], ["2026-W46T00:00:00\uac00-01", false], ["2026-W46T00:00:00\uac00+00:00:30.5", false], ["2026-W46T00:00:00\uac00+24:00", false]]"#).unwrap();
    for case in cases.as_array().unwrap() {
        let mut v = fixture();
        v["generated_at"] = case[0].clone();
        assert_eq!(
            evidence_matrix(&bytes(&v), 20, None).is_ok(),
            case[1].as_bool().unwrap(),
            "{}",
            case[0]
        );
    }
}
