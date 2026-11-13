use oms_context::{compare_manifests, inspect_manifest};
use serde_json::{Value, json};
use std::path::PathBuf;

fn corpus(kind: &str) -> Value {
    let root = std::env::var_os("OMS_CONTRACT_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .find(|p| {
                    p.join("migration/contracts/context-inspect-v1.json")
                        .is_file()
                })
                .expect("repository root")
                .to_path_buf()
        });
    let path = root.join(format!("migration/contracts/context-{kind}-v1.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn vectors() -> Value {
    corpus("inspect")
}
fn raw(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}
#[test]
fn frozen_vectors() {
    for (kind, entry, count) in [
        ("inspect", "inspect_manifest", 40),
        ("compare", "compare_manifests", 12),
    ] {
        let data = corpus(kind);
        assert_eq!(data["fixture_schema"], 1);
        assert_eq!(data["reference"]["entrypoint"], entry);
        assert_eq!(
            data["reference"]["commit"],
            "74f87f654a314961420be2aa8ab6dfc1999151e1"
        );
        assert_eq!(
            data["reference"]["source"],
            "scripts/lib/oms_runtime/context_inspect.py"
        );
        assert_eq!(
            data["reference"]["source_sha256"],
            "1b3bad09517f4f6abd1269481b95986ffb62e84c811e6dfb4555691663742da8"
        );
        assert_eq!(data["reference"]["arguments"], json!({"limit":10}));
        let cases = data["cases"].as_array().unwrap();
        assert_eq!(cases.len(), count);
        let mut names = std::collections::HashSet::new();
        for case in cases {
            let name = case["name"].as_str().unwrap();
            assert!(names.insert(name));
            assert_ne!(
                case.get("expected").is_some(),
                case.get("expected_error").is_some(),
                "{name}"
            );
            let mut args = data["reference"]["arguments"].as_object().unwrap().clone();
            if let Some(overrides) = case.get("arguments") {
                args.extend(overrides.as_object().unwrap().clone());
            }
            let result = oms_context::inspection_limit(&args["limit"]).and_then(|limit| {
                if kind == "inspect" {
                    inspect_manifest(&raw(&case["input"]), limit)
                } else {
                    compare_manifests(&raw(&case["left"]), &raw(&case["right"]), limit)
                }
            });
            if let Some(expected) = case.get("expected") {
                assert_eq!(result.unwrap(), *expected, "{name}");
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    case["expected_error"].as_str().unwrap(),
                    "{name}"
                );
            }
        }
    }
}
#[test]
fn json_limit_boundary() {
    for value in [
        json!(null),
        json!(true),
        json!(false),
        json!(0),
        json!(101),
        json!(-1),
        json!(1.0),
        json!("1"),
        json!([]),
    ] {
        assert_eq!(
            oms_context::inspection_limit(&value).unwrap_err(),
            "limit must be an integer from 1 to 100"
        );
    }
    for n in [1, 100] {
        assert_eq!(oms_context::inspection_limit(&json!(n)).unwrap(), n);
    }
}
#[test]
fn compare_and_validation() {
    let data = vectors()["cases"][0]["input"].clone();
    assert_eq!(
        compare_manifests(&raw(&data), &raw(&data), 10).unwrap()["recorded_scope_matches"],
        true
    );
    let mut other = data.clone();
    other["phase"] = json!("review");
    other["selected"][0]["reason"] = json!("different");
    let report = compare_manifests(&raw(&data), &raw(&other), 1).unwrap();
    assert_eq!(report["scope_mismatch_reasons"], json!(["different-phase"]));
    assert_eq!(
        report["changed"]["items"][0]["changed_fields"],
        json!(["reason"])
    );
    assert!(inspect_manifest(&raw(&data), 0).is_err());
    assert!(inspect_manifest(&raw(&data), 101).is_err());
    assert!(inspect_manifest(&raw(&data), 100).is_ok());
    other["budget_bytes"] = json!(true);
    assert_eq!(
        inspect_manifest(&raw(&other), 10).unwrap_err(),
        "invalid context manifest: budget_bytes"
    );
}

#[test]
fn validation_boundaries_and_sanitation() {
    let base = vectors()["cases"][0]["input"].clone();
    let mut data = base.clone();
    for (key, value) in [
        ("schema", json!(true)),
        ("budget_bytes", json!(-1)),
        ("budget_bytes", json!(0)),
        ("budget_bytes", json!(9007199254740992u64)),
        ("selected_bytes", json!(true)),
        ("remaining_bytes", json!(99)),
        ("context_debt", json!(1)),
        ("sufficient", json!(1)),
        ("phase", json!("invented")),
        ("bundle_sha256", json!("bad")),
        ("selected", json!({})),
        ("targets", json!(["src.py", "src.py"])),
    ] {
        data[key] = value;
        assert!(inspect_manifest(&raw(&data), 10).is_err(), "{key}");
        data = base.clone();
    }
    for p in [
        "", "../x", "/x", "a//b", "a/./b", "x\\y", "C:x", "x\u{1b}y", "~x",
    ] {
        data["selected"][0]["path"] = json!(p);
        assert!(inspect_manifest(&raw(&data), 10).is_err(), "{p:?}");
    }
    data = base.clone();
    data["selected"][0]["reason"] = json!("a\u{1b}\n\u{202e}é");
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap()["sources"]["items"][0]["reason"],
        "a? ?é"
    );
    let text = String::from_utf8(raw(&base))
        .unwrap()
        .replace("direct target", "a\\ud800b");
    assert_eq!(
        inspect_manifest(text.as_bytes(), 10).unwrap()["sources"]["items"][0]["reason"],
        "a?b"
    );
    data = base.clone();
    data["selected"][0]["reason"] = json!("é".repeat(4096));
    assert!(inspect_manifest(&raw(&data), 10).is_ok());
    data["selected"][0]["reason"] = json!("é".repeat(4097));
    assert!(inspect_manifest(&raw(&data), 10).is_err());
    data = base.clone();
    data["omitted"] = json!(vec![json!({"path":"p", "reason":"x"}); 4097]);
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap_err(),
        "invalid context manifest: omitted"
    );
    data = base.clone();
    data["omitted"] = json!(vec![json!({}); 4096]);
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap_err(),
        "invalid context manifest: source path"
    );
    let negzero = String::from_utf8(raw(&base))
        .unwrap()
        .replace("\"context_debt\":0", "\"context_debt\":-0");
    assert!(inspect_manifest(negzero.as_bytes(), 10).is_ok());
    let decimal = String::from_utf8(raw(&base))
        .unwrap()
        .replace("\"context_debt\":0", "\"context_debt\":0.0");
    assert_eq!(
        inspect_manifest(decimal.as_bytes(), 10).unwrap_err(),
        "invalid context manifest: context_debt"
    );
}

#[test]
fn truncation_and_comparison_provenance() {
    let base = vectors()["cases"][0]["input"].clone();
    let mut partial = base.clone();
    partial["selected"][0]["bytes_selected"] = json!(50);
    partial["selected"][0]["omitted_bytes"] = json!(81);
    partial["selected"][0]["selection"] = json!("head");
    partial["context_debt"] = json!(1);
    partial["missing_required"] = json!(["src.py"]);
    partial["truncated_required"] = json!(["src.py"]);
    partial["sufficient"] = json!(false);
    assert!(inspect_manifest(&raw(&partial), 10).is_ok());
    partial["selected"][0]["omitted_bytes"] = json!(80);
    assert_eq!(
        inspect_manifest(&raw(&partial), 10).unwrap_err(),
        "invalid context manifest: inconsistent source selection"
    );
    let mut a = base.clone();
    let mut b = base.clone();
    a["phase_rules_sha256"] = json!("c".repeat(64));
    assert_eq!(
        compare_manifests(&raw(&a), &raw(&b), 10).unwrap()["phase_rules"]["status"],
        "unknown"
    );
    b["phase_rules_sha256"] = json!("d".repeat(64));
    assert_eq!(
        compare_manifests(&raw(&a), &raw(&b), 10).unwrap()["scope_mismatch_reasons"],
        json!(["different-phase-rules"])
    );
    b["phase_rules_sha256"] = a["phase_rules_sha256"].clone();
    assert_eq!(
        compare_manifests(&raw(&a), &raw(&b), 10).unwrap()["phase_rules"]["status"],
        "same-recorded-digest"
    );
    b["phase_rules_sha256"] = Value::Null;
    assert_eq!(
        inspect_manifest(&raw(&b), 10).unwrap_err(),
        "invalid context manifest: phase rules digest"
    );
    b = base.clone();
    b["contract_state_digest"] = Value::Null;
    assert_eq!(
        compare_manifests(&raw(&b), &raw(&b), 10).unwrap()["scope_mismatch_reasons"],
        json!(["contract-identity-unavailable"])
    );
}

#[test]
fn nested_ignored_metadata() {
    let base = vectors()["cases"][0]["input"].clone();
    let raw = String::from_utf8(raw(&base)).unwrap();
    let nested = format!(
        "{{\"ignored\":{}0{},{}",
        "[".repeat(800),
        "]".repeat(800),
        &raw[1..]
    );
    assert!(inspect_manifest(nested.as_bytes(), 10).is_ok());
    let nonfinite = nested.replacen("0]", "NaN]", 1);
    assert!(inspect_manifest(nonfinite.as_bytes(), 10).is_err());
    let duplicate = nested.replacen("{\"ignored\":", "{\"ignored\":0,\"ignored\":", 1);
    assert_eq!(
        inspect_manifest(duplicate.as_bytes(), 10).unwrap_err(),
        "invalid context manifest: duplicate JSON key"
    );
}

#[test]
fn nonfinite_metadata_and_type_errors() {
    let base = vectors()["cases"][0]["input"].clone();
    let text = String::from_utf8(raw(&base)).unwrap();
    let ignored = format!(
        "{{\"ignored\":[NaN,Infinity,-Infinity,\"NaN\",\"oms-nonfinite-marker-0\"],{}",
        &text[1..]
    );
    assert!(inspect_manifest(ignored.as_bytes(), 10).is_err());
    for token in ["NaN", "Infinity", "-Infinity"] {
        let input = format!("{{\"ignored\":{token},{}", &text[1..]);
        assert_eq!(
            inspect_manifest(input.as_bytes(), 10).unwrap_err(),
            "invalid context manifest: non-finite JSON value"
        );
    }
    let input = format!("{{\"ignored\":\"NaN Infinity -Infinity\",{}", &text[1..]);
    assert!(inspect_manifest(input.as_bytes(), 10).is_ok());
    let invalid = text.replace("\"budget_bytes\":1000", "\"budget_bytes\":NaN");
    assert_eq!(
        inspect_manifest(invalid.as_bytes(), 10).unwrap_err(),
        "invalid context manifest: non-finite JSON value"
    );
    let invalid = text.replace("\"budget_bytes\":1000", "\"budget_bytes\":1NaN");
    assert!(inspect_manifest(invalid.as_bytes(), 10).is_err());
    let invalid = format!("{{NaN:0,{}", &text[1..]);
    assert!(inspect_manifest(invalid.as_bytes(), 10).is_err());
    let duplicate = format!("{{\"ignored\":NaN,\"ignored\":0,{}", &text[1..]);
    assert_eq!(
        inspect_manifest(duplicate.as_bytes(), 10).unwrap_err(),
        "invalid context manifest: non-finite JSON value"
    );
}

#[test]
fn projection_order_debt_and_duplicate_hashes() {
    let base = vectors()["cases"][0]["input"].clone();
    let mut data = base.clone();
    data["selected"] = json!([]);
    data["targets"] = json!([]);
    data["selected_bytes"] = json!(0);
    data["remaining_bytes"] = json!(1000);
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap()["sources"]["total"],
        0
    );
    data = base.clone();
    let mut second = data["selected"][0].clone();
    second["path"] = json!("other.py");
    let mut third = second.clone();
    third["path"] = json!("third.py");
    third["bytes_before"] = json!(200);
    third["bytes_selected"] = json!(200);
    third["sha256"] = json!("c".repeat(64));
    data["selected"]
        .as_array_mut()
        .unwrap()
        .extend([second, third]);
    data["selected_bytes"] = json!(500);
    data["remaining_bytes"] = json!(500);
    let report = inspect_manifest(&raw(&data), 1).unwrap();
    assert_eq!(report["sources"]["items"][0]["path"], "third.py");
    assert_eq!(report["sources"]["omitted"], 2);
    assert_eq!(
        report["repeated_source_digests"]["items"][0]["paths"]["omitted"],
        1
    );
    data["selected"][1]["bytes_before"] = json!(101);
    data["selected"][1]["bytes_selected"] = json!(101);
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap_err(),
        "invalid context manifest: digest has conflicting source sizes"
    );
    data = base.clone();
    data["targets"] = json!(["src.py", "absent"]);
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap_err(),
        "invalid context manifest: target debt is missing"
    );
    data["missing_required"] = json!(["absent"]);
    data["context_debt"] = json!(1);
    data["sufficient"] = json!(false);
    assert!(inspect_manifest(&raw(&data), 10).is_ok());
    data["truncated_required"] = json!(["absent"]);
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap_err(),
        "invalid context manifest: truncated required source"
    );
    data = base.clone();
    data["omitted"] = json!([{"path":"src.py","reason":"x"}]);
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap_err(),
        "invalid context manifest: duplicate omitted source"
    );
    data = base.clone();
    data["omitted"] = json!(
        (0..4096)
            .map(|i| json!({"path":format!("p{i}"),"reason":"x"}))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap()["omissions"]["total"],
        4096
    );
    data = base.clone();
    let max = 9_007_199_254_740_991u64;
    data["budget_bytes"] = json!(max);
    data["selected_bytes"] = json!(max);
    data["remaining_bytes"] = json!(0);
    data["targets"] = json!([]);
    data["selected"] = json!(
        (0..2050)
            .map(|i| json!({
                "path":format!("p{i}"), "reason":"x", "bytes_before":max,
                "bytes_selected":max, "omitted_bytes":0, "selection":"full",
                "sha256":"a".repeat(64)
            }))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        inspect_manifest(&raw(&data), 10).unwrap_err(),
        "invalid context manifest: duplicate source or payload exceeds bundle"
    );
}

#[test]
fn comparison_direction_and_scope() {
    let left = vectors()["cases"][0]["input"].clone();
    let mut right = left.clone();
    let mut added = right["selected"][0].clone();
    added["path"] = json!("added.py");
    added["sha256"] = json!("d".repeat(64));
    added["bytes_before"] = json!(3);
    added["bytes_selected"] = json!(3);
    right["selected"][0]["reason"] = json!("required context");
    right["selected"].as_array_mut().unwrap().push(added);
    right["selected_bytes"] = json!(180);
    right["remaining_bytes"] = json!(820);
    right["phase"] = json!("review");
    right["targets"] = json!(["added.py"]);
    right["contract_state_digest"] = json!("c".repeat(64));
    let v = compare_manifests(&raw(&left), &raw(&right), 10).unwrap();
    assert_eq!(
        v["scope_mismatch_reasons"],
        json!([
            "different-phase",
            "different-targets",
            "different-contract-state"
        ])
    );
    assert_eq!(v["added"]["items"], json!(["added.py"]));
    assert_eq!(v["delta"]["selected_bytes"], 40);
    assert_eq!(
        v["changed"]["items"][0]["changed_fields"],
        json!(["reason"])
    );
    assert_eq!(
        compare_manifests(&raw(&right), &raw(&left), 10).unwrap()["removed"]["items"],
        json!(["added.py"])
    );
}

#[test]
fn input_size_and_depth_limits() {
    let base = vectors()["cases"][0]["input"].clone();
    let bytes = raw(&base);
    let mut exact = bytes.clone();
    exact.resize(4 * 1024 * 1024, b' ');
    assert!(inspect_manifest(&exact, 10).is_ok());
    exact.push(b' ');
    assert!(inspect_manifest(&exact, 10).is_err());
    assert!(compare_manifests(&bytes, &exact, 10).is_err());
    assert!(inspect_manifest(b"", 10).is_err());
    let raw = String::from_utf8(bytes).unwrap();
    let edge = format!(
        "{{\"ignored\":{}0{},{}",
        "[".repeat(1023),
        "]".repeat(1023),
        &raw[1..]
    );
    assert!(inspect_manifest(edge.as_bytes(), 10).is_ok());
    let above = format!(
        "{{\"ignored\":{}0{},{}",
        "[".repeat(1024),
        "]".repeat(1024),
        &raw[1..]
    );
    assert!(inspect_manifest(above.as_bytes(), 10).is_err());
    let deep = format!(
        "{{\"ignored\":{}0{},{}",
        "[".repeat(1100),
        "]".repeat(1100),
        &raw[1..]
    );
    assert!(inspect_manifest(deep.as_bytes(), 10).is_err());
    let malformed = format!("{{\"ignored\":{}", "[".repeat(1100));
    assert!(inspect_manifest(malformed.as_bytes(), 10).is_err());
}
