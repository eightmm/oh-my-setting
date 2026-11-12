use oms_output_capture::{capture_metadata, capture_reference};
use std::path::Path;

#[test]
fn reference_and_metadata_contract() {
    let repo = Path::new("/repo");
    let name = "20250102T030405Z-42.log";
    let sha = "0123456789abcdef".repeat(4);
    let expected = format!(
        "output:{name}:{sha}:00bfa7e74fbd9816176e8b434cd8a5e498c4a6f243e44746b877ec0ce27a0555"
    );
    let reference = capture_reference(repo, name, &sha).unwrap();
    assert_eq!(reference, expected);
    assert_eq!(
        capture_metadata(repo, name, &sha).unwrap(),
        serde_json::json!({
            "capture_ref": reference, "capture_sha256": sha, "read_tool": "oms_output_read",
            "read_arguments": {"repo": "/repo", "capture_ref": expected}
        })
    );
    for invalid in [
        "",
        "20250102t030405Z-42.log",
        "20250102T030405Z-.log",
        "20250102T030405Z-42.log/x",
    ] {
        assert_eq!(
            capture_reference(repo, invalid, &sha).unwrap_err(),
            "invalid capture filename"
        );
    }
    for invalid in ["", "A", &"A".repeat(64), &"0".repeat(63)] {
        assert_eq!(
            capture_reference(repo, name, invalid).unwrap_err(),
            "invalid capture SHA-256"
        );
    }
}

use oms_context::parse_json;
use oms_output_capture::{project_snapshot, serialize_compact, validate_request};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn request(data: &[u8], extra: Value) -> oms_output_capture::ValidatedRequest {
    let sha = format!("{:x}", Sha256::digest(data));
    let reference = capture_reference(Path::new("/repo"), "20250102T030405Z-42.log", &sha).unwrap();
    let mut args = json!({"capture_ref": reference});
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    validate_request(
        Path::new("/repo"),
        &parse_json(serde_json::to_string(&args).unwrap().as_bytes()).unwrap(),
    )
    .unwrap()
}

#[test]
fn page_search_and_batch() {
    let data = b"a\xff\xf0\x9f\x98\x80xyz xyz xyz xyz xyz xyz xyz xyz xyz";
    let page = project_snapshot(&request(data, json!({"limit":4})), data).unwrap();
    assert_eq!(page["content"], "a\u{fffd}");
    assert_eq!(page["end"], 4);
    assert_eq!(page["next_read_arguments"]["offset"], 4);
    let compact = project_snapshot(
        &request(data, json!({"compact":true,"offset":4,"limit":4})),
        data,
    )
    .unwrap();
    assert_eq!(compact["content"], "😀");
    assert_eq!(compact["next_read"], json!({"offset":8,"limit":4}));
    let result = project_snapshot(&request(data, json!({"query":"xyz"})), data).unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 8);
    assert_eq!(result["has_more"], true);
    assert_eq!(result["next_search_arguments"]["offset"], 39);
    let batch = project_snapshot(
        &request(
            data,
            json!({"compact":true,"queries":["missing","xyz","a"]}),
        ),
        data,
    )
    .unwrap();
    assert_eq!(batch["searches"][0]["end"], 0);
    assert_eq!(batch["searches"][1]["matches"].as_array().unwrap().len(), 4);
    assert_eq!(batch["searches"][2]["matches"].as_array().unwrap().len(), 1);
    assert_eq!(batch["has_more"], true);
    assert!(serialize_compact(&batch).is_ok());
    assert_eq!(
        project_snapshot(&request(data, json!({"offset":2})), data)
            .unwrap_err()
            .code,
        "invalid_range"
    );
    assert_eq!(
        project_snapshot(&request(data, json!({})), b"changed")
            .unwrap_err()
            .code,
        "capture_hash_changed"
    );
}

#[test]
fn validation_and_order() {
    let sha = "0".repeat(64);
    let name = "20250102T030405Z-42.log";
    for path in ["", "rel", "/repo/", "/repo/..", "/repo\0x", "/repo//x"] {
        assert!(capture_reference(Path::new(path), name, &sha).is_err());
    }
    let metadata = capture_metadata(Path::new("/repo"), name, &sha).unwrap();
    let text = serde_json::to_string(&metadata).unwrap();
    assert!(text.find("read_tool").unwrap() < text.find("read_arguments").unwrap());
    assert!(text.find("repo").unwrap() < text.rfind("capture_ref").unwrap());
    let reference = capture_reference(Path::new("/repo"), name, &sha).unwrap();
    for (suffix, code) in [
        (r#", "compact":1"#, "invalid_representation"),
        (r#", "offset":true"#, "invalid_range"),
        (r#", "limit":3"#, "invalid_range"),
        (r#", "query":"\ud800""#, "invalid_query"),
        (r#", "compact":true, "queries":[]"#, "invalid_query"),
    ] {
        let input = format!("{{\"capture_ref\":\"{reference}\"{suffix}}}");
        assert_eq!(
            validate_request(Path::new("/repo"), &parse_json(input.as_bytes()).unwrap())
                .unwrap_err(),
            oms_output_capture::RequestError::Capture(oms_output_capture::CaptureError {
                code,
                message: match code {
                    "invalid_representation" => "compact must be a boolean".into(),
                    "invalid_range" if suffix.contains("offset") =>
                        "offset must be an integer from 0 to 201326592".into(),
                    "invalid_range" => "limit must be an integer from 4 to 16384".into(),
                    "invalid_query" if suffix.contains("queries") =>
                        "queries requires compact=true and 1-8 literals; no query or limit".into(),
                    _ => "each query requires 1-256 UTF-8 bytes".into(),
                }
            })
        );
    }
    assert_eq!(
        serialize_compact(&json!("x".repeat(65535)))
            .unwrap_err()
            .code,
        "response_too_large"
    );
}

#[test]
fn complete_page_and_snapshot_error_contract() {
    use oms_output_capture::{CaptureError, SnapshotFailure};
    let data = b"abcde";
    let req = request(data, json!({"limit":4}));
    let sha = format!("{:x}", Sha256::digest(data));
    let reference = capture_reference(Path::new("/repo"), "20250102T030405Z-42.log", &sha).unwrap();
    let expected = json!({"capture_ref":reference,"capture_sha256":sha,"capture_bytes":5,"content_bytes":5,"offset":0,"encoding":"utf-8 (invalid bytes replaced)","provenance":"stored redacted capture; not verification evidence","content":"abcd","end":4,"has_more":true,"next_read_arguments":{"repo":"/repo","capture_ref":reference,"offset":4,"limit":4}});
    assert_eq!(
        serde_json::to_string(&project_snapshot(&req, data).unwrap()).unwrap(),
        serde_json::to_string(&expected).unwrap()
    );
    for (f, code) in [
        (SnapshotFailure::Missing, "capture_missing"),
        (SnapshotFailure::Unsafe, "unsafe_capture"),
        (SnapshotFailure::Unreadable, "capture_unreadable"),
        (
            SnapshotFailure::InitialOversize { max_bytes: 4 },
            "capture_too_large",
        ),
        (SnapshotFailure::ChangedBeforeRead, "capture_changed"),
        (SnapshotFailure::ChangedDuringRead, "capture_changed"),
        (SnapshotFailure::PathChanged, "unsafe_capture"),
        (SnapshotFailure::RootChanged, "unsafe_capture"),
        (SnapshotFailure::UnsafeOpen, "unsafe_capture"),
    ] {
        let e: CaptureError = f.into();
        assert_eq!(e.code, code);
    }
}

#[test]
fn query_limits_and_nonoverlap() {
    let data = b"aaaaa";
    let result = project_snapshot(&request(data, json!({"query":"aa","offset":1})), data).unwrap();
    assert_eq!(
        result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["offset"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(result["end"], 5);
    assert_eq!(result["has_more"], false);
    let no_hits = project_snapshot(&request(data, json!({"query":"z","offset":2})), data).unwrap();
    assert_eq!(no_hits["end"], 2);
    assert_eq!(no_hits["has_more"], false);
    let sha = format!("{:x}", Sha256::digest(data));
    let reference = capture_reference(Path::new("/repo"), "20250102T030405Z-42.log", &sha).unwrap();
    for (extra, valid) in [
        (json!({"query":"x".repeat(256)}), true),
        (json!({"query":"x".repeat(257)}), false),
        (json!({"query":""}), false),
        (
            json!({"compact":true,"queries":vec!["x".repeat(256);4]}),
            true,
        ),
        (
            json!({"compact":true,"queries":["x".repeat(256),"x".repeat(256),"x".repeat(256),"x".repeat(256),"z".to_string()]}),
            false,
        ),
        (json!({"compact":true,"queries":vec!["x";8]}), true),
        (json!({"compact":true,"queries":vec!["x";9]}), false),
    ] {
        let mut args = json!({"capture_ref":reference});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let parsed = parse_json(serde_json::to_string(&args).unwrap().as_bytes()).unwrap();
        assert_eq!(validate_request(Path::new("/repo"), &parsed).is_ok(), valid);
    }
}

#[test]
fn repo_precondition_is_not_a_client_error() {
    let sha = "0".repeat(64);
    let reference = capture_reference(Path::new("/repo"), "20250102T030405Z-42.log", &sha).unwrap();
    let input = parse_json(
        serde_json::to_vec(&json!({"capture_ref":reference}))
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    assert!(matches!(
        validate_request(Path::new("relative"), &input),
        Err(oms_output_capture::RequestError::InvalidRepository(_))
    ));
}

#[test]
fn stored_limit_and_repository_preconditions() {
    use oms_output_capture::{
        CaptureError, READ_CAP, RequestError, SnapshotFailure, validate_stored_capture,
    };
    use std::os::unix::ffi::OsStrExt;
    let name = "20250102T030405Z-42.log";
    for n in [1, 4 * 1024 * 1024, READ_CAP] {
        assert!(validate_stored_capture(name, n).is_ok());
    }
    for n in [0, READ_CAP + 1] {
        let e = validate_stored_capture(name, n).unwrap_err();
        assert_eq!(
            (e.code, e.message),
            (
                "invalid_limit",
                format!("max_bytes must be an integer from 1 to {READ_CAP}")
            )
        );
    }
    assert_eq!(
        validate_stored_capture("bad", 0).unwrap_err().code,
        "invalid_reference"
    );
    let e: CaptureError = SnapshotFailure::InitialOversize { max_bytes: 7 }.into();
    assert_eq!(e.message, "stored capture exceeds the 7-byte read limit");
    let sha = "0".repeat(64);
    let reference = capture_reference(Path::new("/repo"), name, &sha).unwrap();
    let valid = parse_json(
        serde_json::to_vec(&json!({"capture_ref":reference}))
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    let invalid = parse_json(br#"{"capture_ref":"bad"}"#).unwrap();
    let raw = std::ffi::OsStr::from_bytes(b"/bad\xff");
    for path in [
        Path::new("relative"),
        Path::new("/repo/.."),
        Path::new("/repo/"),
        Path::new(raw),
    ] {
        assert!(matches!(
            validate_request(path, &valid),
            Err(RequestError::InvalidRepository(_))
        ));
        assert!(matches!(
            validate_request(path, &invalid),
            Err(RequestError::Capture(CaptureError {
                code: "invalid_reference",
                ..
            }))
        ));
    }
}

#[test]
fn pinned_reference_projection_corpus() {
    use oms_output_capture::RequestError;
    let fixture: Value = serde_json::from_str(include_str!("projection_fixtures.json")).unwrap();
    let hex = fixture["data_hex"].as_str().unwrap().as_bytes();
    let data: Vec<u8> = hex
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect();
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let input = parse_json(serde_json::to_vec(&case["arguments"]).unwrap().as_slice()).unwrap();
        let result = validate_request(Path::new("/repo"), &input)
            .map_err(|e| match e {
                RequestError::Capture(e) => e,
                RequestError::InvalidRepository(e) => panic!("{e}"),
            })
            .and_then(|r| project_snapshot(&r, &data));
        if let Some(expected) = case.get("payload") {
            assert_eq!(
                serde_json::to_string(&result.unwrap()).unwrap(),
                serde_json::to_string(expected).unwrap(),
                "case {i}"
            );
        } else {
            let e = result.unwrap_err();
            assert_eq!(
                json!({"code":e.code,"message":e.message}),
                case["error"],
                "case {i}"
            );
        }
    }
}
