use oms_work_inspect::{action_inbox, evidence_matrix, render, return_brief};
use serde_json::{Value, json};
fn fixture() -> Value {
    json!({"schema":2,"generated_at":"2026-11-10T00:00:00Z","state_digest":"a".repeat(64),"repo":{"head":"b".repeat(40)},"task":{},"plan":{"present":true,"plan_id":"plan_1","tasks":[{"id":"t1","title":"구현 t1","state":"ready","depends":[],"verify_digest":"c".repeat(64),"verify_present":true}],"actionable":["t1"],"contract":{"bound":true,"satisfied":true}},"criteria":[{"id":"req1","text":"확인할 기준","status":"missing","source":"plan-task","plan_id":"plan_1","plan_task_id":"t1","evidence":[]}],"sources":[{"path":"PROJECT.md","sha256":"d".repeat(64)}],"next_actions":[{"id":"execute_ready_task","priority":80,"authority":"worktree_write","reason":"one task ready","command":"oms plan-run --next"}]})
}
fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
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
