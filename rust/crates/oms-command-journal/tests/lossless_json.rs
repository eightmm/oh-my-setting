use oms_command_journal::adapters::{ObserveOverrides, build_source_payload};
use oms_command_journal::cold::{read_verified_cold_events, seal_events};
use oms_command_journal::sanitize::{canonical_json_bytes, sanitize_value};
use oms_command_journal::schema::{ProjectIdentity, normalize_event};
use oms_command_journal::store::JournalStore;
use oms_command_journal::tz::{TimeZoneSpec, parse_rfc3339_utc};
use oms_json::{OrderedValue, ParseError, parse_python_ordered};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oms-journal-json-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn parse(text: &str) -> OrderedValue {
    parse_python_ordered(text.as_bytes()).unwrap()
}
fn event(number: &str) -> OrderedValue {
    parse(&format!(
        r#"{{"event_type":"run","occurred_at":"2026-07-31T02:00:00Z","source":{{"type":"run-ledger"}},"verification_status":"not_verified","provenance":{{"seed":{number}}}}}"#
    ))
}
fn normalize(raw: &OrderedValue) -> OrderedValue {
    let project = ProjectIdentity {
        id: "proj_test".into(),
        name: "demo".into(),
        origin_remote: String::new(),
        github_repository: String::new(),
        repo_root: PathBuf::from("."),
    };
    normalize_event(
        raw,
        &project,
        &TimeZoneSpec::Utc,
        parse_rfc3339_utc("2026-07-31T03:00:00Z").unwrap(),
    )
    .unwrap()
}
fn seed(value: &OrderedValue) -> &OrderedValue {
    value.get("provenance").unwrap().get("seed").unwrap()
}
fn identity(value: &OrderedValue) -> String {
    value
        .get("event_id")
        .unwrap()
        .as_string()
        .unwrap()
        .to_str_checked()
        .unwrap()
}

#[test]
fn normalization_and_canonical_bytes_keep_arbitrary_integers_exact() {
    for decimal in [
        "9223372036854775808".to_owned(),
        "-9223372036854775809".to_owned(),
        "18446744073709551616".to_owned(),
        format!("1{}", "0".repeat(100)),
        "9".repeat(4300),
    ] {
        let normalized = normalize(&event(&decimal));
        assert_eq!(
            seed(&normalized).as_integer().unwrap().as_decimal(),
            decimal
        );
        let encoded = canonical_json_bytes(&normalized).unwrap();
        let replay = parse_python_ordered(&encoded).unwrap();
        assert_eq!(seed(&replay).as_integer().unwrap().as_decimal(), decimal);
        assert_eq!(identity(&normalized), identity(&replay));
    }
    for malformed in ["", "-", "01", "1e", "+1"] {
        assert!(parse_python_ordered(malformed.as_bytes()).is_err());
    }
    assert!(matches!(
        parse_python_ordered("9".repeat(4301).as_bytes()),
        Err(ParseError::IntegerDigitLimit)
    ));
}

#[test]
fn integer_float_and_signed_zero_have_python_identity_semantics() {
    let integer = normalize(&event("1"));
    let float = normalize(&event("1.0"));
    assert!(seed(&integer).as_integer().is_some());
    assert_eq!(seed(&float).as_float(), Some(1.0));
    assert_ne!(identity(&integer), identity(&float));
    let zero = normalize(&event("0"));
    let negative_zero = normalize(&event("-0"));
    assert_eq!(seed(&negative_zero).as_integer().unwrap().as_decimal(), "0");
    assert_eq!(identity(&zero), identity(&negative_zero));
    let float_zero = normalize(&event("-0.0"));
    assert!(seed(&float_zero).as_float().unwrap().is_sign_negative());
    assert_ne!(identity(&zero), identity(&float_zero));
}

#[test]
fn sanitize_preserves_insertion_order_before_collisions_and_collection_limits() {
    let raw =
        parse(r#"{"z":0,"b ":1,"b":2,"$serde_json::private::Number":{"n":18446744073709551616}}"#);
    let clean = sanitize_value(&raw, None).unwrap();
    assert_eq!(
        clean.get("b").unwrap().as_integer().unwrap().as_decimal(),
        "2"
    );
    assert!(
        clean
            .get("$serde_json::private::Number")
            .unwrap()
            .get("n")
            .unwrap()
            .as_integer()
            .is_some()
    );
    let in_place = sanitize_value(&parse(r#"{"b":1,"b ":2,"b":3}"#), None).unwrap();
    assert_eq!(
        in_place
            .get("b")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_decimal(),
        "2"
    );
    for count in [0usize, 64, 65] {
        let entries = (0..count)
            .map(|i| format!("\"k{i:02}\":{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let input = parse(&format!("{{{entries}}}"));
        let output = sanitize_value(&input, None).unwrap();
        assert_eq!(output.as_object().unwrap().len(), count.min(64));
        assert!(output.get("k64").is_none());
    }
    // A lexicographically earlier 65th key must not displace the first entry.
    let entries = (0..64)
        .map(|i| format!("\"z{i:02}\":{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let input = parse(&format!("{{{entries},\"a\":999}}"));
    let output = sanitize_value(&input, None).unwrap();
    assert!(output.get("z00").is_some());
    assert!(output.get("a").is_none());
}

#[test]
fn sanitizer_and_copy_are_stack_safe_at_the_parser_depth_boundary() {
    for depth in [0usize, 1, 1024] {
        let text = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        let raw = parse(&text);
        let clean = sanitize_value(&raw, None).unwrap();
        let copy = clean.clone_bounded().unwrap();
        assert!(oms_json::ordered_values_equal(&raw, &copy));
        assert_eq!(canonical_json_bytes(&clean).unwrap(), text.as_bytes());
    }
    let excessive = format!("{}0{}", "[".repeat(1025), "]".repeat(1025));
    assert!(matches!(
        parse_python_ordered(excessive.as_bytes()),
        Err(ParseError::DepthLimit)
    ));
    assert!(parse_python_ordered(b"[[0]").is_err());
}

#[test]
fn nonfinite_values_are_filtered_and_surrogate_keys_are_never_replaced() {
    let raw = parse(r#"[NaN,Infinity,-Infinity,1,1.0]"#);
    let clean = sanitize_value(&raw, None).unwrap();
    assert_eq!(canonical_json_bytes(&clean).unwrap(), b"[1,1.0]");
    for number in ["NaN", "Infinity", "-Infinity", "1e999"] {
        let normalized = normalize(&event(number));
        assert!(normalized.get("provenance").is_none());
    }
    let raw = parse(r#"{"\ud800":"\udcff","\ud83d\ude00":1}"#);
    let clean = sanitize_value(&raw, None).unwrap();
    let entries = clean.as_object().unwrap().as_slice();
    assert_eq!(entries[0].0.codepoints(), &[0xd800]);
    assert_eq!(entries[0].1.as_string().unwrap().codepoints(), &[0xdcff]);
    assert_eq!(entries[1].0.codepoints(), &[0x1f600]);
    assert!(canonical_json_bytes(&clean).is_err());
    let escaped_pair = sanitize_value(&parse(r#"{"\ud83d\ude00":"ok"}"#), None).unwrap();
    assert_eq!(
        canonical_json_bytes(&escaped_pair).unwrap(),
        "{\"😀\":\"ok\"}".as_bytes()
    );
}

#[test]
fn adapter_store_dedupe_sqlite_and_cold_replay_keep_exact_decimal_numbers() {
    let scratch = Scratch::new();
    let source = scratch.0.join("record.json");
    let decimal = format!("1{}", "0".repeat(100));
    fs::write(
        &source,
        format!(r#"{{"id":"large-run","exit":0,"metrics":{{"score":{decimal}}}}}"#),
    )
    .unwrap();
    let now = parse_rfc3339_utc("2026-07-31T03:00:00Z").unwrap();
    let options = ObserveOverrides {
        source_type: "run-ledger".into(),
        source_file: Some(source),
        ..Default::default()
    };
    let payload = build_source_payload(&scratch.0, &options, now).unwrap();
    assert_eq!(
        payload.get("metrics").unwrap().as_array().unwrap()[0]
            .get("value")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_decimal(),
        decimal
    );
    let store = JournalStore::open_for_write(&scratch.0, Some("UTC")).unwrap();
    let (_, created) = store.record_event(&payload, now).unwrap();
    assert!(created);
    let (_, created) = store.record_event(&payload, now).unwrap();
    assert!(!created);
    let replay = store.recent_events(10).unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(
        replay[0].get("metrics").unwrap().as_array().unwrap()[0]
            .get("value")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_decimal(),
        decimal
    );
    drop(store);
    let later = parse_rfc3339_utc("2026-08-01T03:00:00Z").unwrap();
    assert_eq!(
        seal_events(&scratch.0, &TimeZoneSpec::Utc, later, true).unwrap(),
        1
    );
    let (_, cold) = read_verified_cold_events(&scratch.0.join(".oms/work-journal")).unwrap();
    assert_eq!(cold.len(), 1);
    assert_eq!(
        cold[0].get("metrics").unwrap().as_array().unwrap()[0]
            .get("value")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_decimal(),
        decimal
    );
    assert_eq!(
        seal_events(&scratch.0, &TimeZoneSpec::Utc, later, true).unwrap(),
        0
    );
}

#[test]
fn cold_manifest_validation_keeps_null_and_numeric_bounds_explicit() {
    use oms_command_journal::cold::{cold_manifest_path, load_cold_manifest, save_cold_manifest};
    let scratch = Scratch::new();
    let journal = scratch.0.join(".oms/work-journal");
    fs::create_dir_all(journal.join("cold")).unwrap();
    let path = cold_manifest_path(&journal);
    for valid in [
        r#"{"schema_version":1,"segments":[]}"#,
        r#"{"schema_version":1,"segments":[],"pending":null}"#,
    ] {
        fs::write(&path, valid).unwrap();
        let manifest = load_cold_manifest(&journal).unwrap();
        save_cold_manifest(&journal, &manifest).unwrap();
        let replay = parse_python_ordered(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(replay.get("pending").is_some(), valid.contains("pending"));
    }
    for malformed in [
        "",
        "null",
        "{}",
        r#"{"schema_version":1,"segments":null}"#,
        r#"{"schema_version":1,"segments":[],"extra":0}"#,
        r#"{"schema_version":1.0,"segments":[]}"#,
        r#"{"schema_version":1,"segments":[],"pending":{}}"#,
    ] {
        fs::write(&path, malformed).unwrap();
        assert!(load_cold_manifest(&journal).is_err());
    }
    let digest = "a".repeat(64);
    for (count, valid) in [
        ("0", false),
        ("1", true),
        ("18446744073709551616", false),
        ("-1", false),
        ("1.0", false),
        ("true", false),
        ("\"1\"", false),
    ] {
        let document = format!(
            r#"{{"schema_version":1,"segments":[{{"file":"2026-07-{digest}.jsonl","month":"2026-07","count":{count},"first_event_id":18446744073709551616,"last_event_id":-9223372036854775809,"sha256":"{digest}"}}]}}"#
        );
        fs::write(&path, document).unwrap();
        let loaded = load_cold_manifest(&journal);
        assert_eq!(loaded.is_ok(), valid);
        if let Ok(manifest) = loaded {
            save_cold_manifest(&journal, &manifest).unwrap();
            let replay = load_cold_manifest(&journal).unwrap();
            assert_eq!(
                replay.segments[0]
                    .first_event_id
                    .as_integer()
                    .unwrap()
                    .as_decimal(),
                "18446744073709551616"
            );
            assert_eq!(
                replay.segments[0]
                    .last_event_id
                    .as_integer()
                    .unwrap()
                    .as_decimal(),
                "-9223372036854775809"
            );
        }
    }
}

#[test]
fn adapter_numeric_zero_status_keeps_the_original_number_kind() {
    let scratch = Scratch::new();
    let source = scratch.0.join("record.json");
    let options = ObserveOverrides {
        source_type: "run-ledger".into(),
        source_file: Some(source.clone()),
        ..Default::default()
    };
    let now = parse_rfc3339_utc("2026-07-31T03:00:00Z").unwrap();
    for (number, success) in [
        ("0", true),
        ("-0", true),
        ("0.0", true),
        ("-0.0", true),
        ("false", true),
        ("null", false),
        ("1", false),
        ("NaN", false),
        ("Infinity", false),
        ("18446744073709551616", false),
        ("\"0\"", false),
    ] {
        fs::write(
            &source,
            format!(r#"{{"id":"run","exit":{number},"ignored":"\ud800"}}"#),
        )
        .unwrap();
        let payload = build_source_payload(&scratch.0, &options, now).unwrap();
        let actual = payload
            .get("outcome")
            .unwrap()
            .get("status")
            .unwrap()
            .as_string()
            .unwrap()
            .to_str_checked()
            .unwrap();
        assert_eq!(actual == "success", success);
        if number == "0.0" || number == "-0.0" {
            assert!(
                payload
                    .get("provenance")
                    .unwrap()
                    .get("exit_code")
                    .unwrap()
                    .as_float()
                    .is_some()
            );
        }
    }
    fs::write(&source, r#"{"id":"\ud800","exit":0}"#).unwrap();
    assert!(build_source_payload(&scratch.0, &options, now).is_err());
}

#[test]
fn normalization_ignores_unselected_strings_and_refuses_surrogate_coercion() {
    let mut raw = event("1");
    raw.as_object_mut()
        .unwrap()
        .get_mut("source")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("id".into(), parse(r#""safe""#));
    raw.as_object_mut()
        .unwrap()
        .insert("ignored".into(), parse(r#""\ud800""#));
    let normalized = normalize(&raw);
    assert!(normalized.get("ignored").is_none());
    let mut bad = event("1");
    bad.as_object_mut()
        .unwrap()
        .insert("event_type".into(), parse(r#""\ud800""#));
    let project = ProjectIdentity {
        id: "proj_test".into(),
        name: "demo".into(),
        origin_remote: String::new(),
        github_repository: String::new(),
        repo_root: PathBuf::from("."),
    };
    assert!(
        normalize_event(
            &bad,
            &project,
            &TimeZoneSpec::Utc,
            parse_rfc3339_utc("2026-07-31T03:00:00Z").unwrap()
        )
        .is_err()
    );
}

#[test]
fn numeric_source_ids_stay_exact_in_sqlite_filter_columns() {
    let scratch = Scratch::new();
    let store = JournalStore::open_for_write(&scratch.0, Some("UTC")).unwrap();
    let decimal = format!("1{}", "0".repeat(100));
    let mut raw = event("1");
    raw.as_object_mut()
        .unwrap()
        .get_mut("source")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("id".into(), parse(&decimal));
    store
        .record_event(&raw, parse_rfc3339_utc("2026-07-31T03:00:00Z").unwrap())
        .unwrap();
    let connection = rusqlite::Connection::open(&store.db_path).unwrap();
    let source: String = connection
        .query_row("SELECT source_id FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source, decimal);
}

#[test]
fn config_roundtrip_keeps_unknown_large_integer_fields() {
    use oms_command_journal::config::{load_work_journal_config, save_work_journal_config};
    if std::env::var_os("OMS_JOURNAL_JSON_CONFIG_CHILD").is_some() {
        let config = load_work_journal_config().unwrap();
        save_work_journal_config(&config).unwrap();
        return;
    }
    let scratch = Scratch::new();
    let path = scratch.0.join("work-journal.json");
    let decimal = format!("1{}", "0".repeat(100));
    fs::write(
        &path,
        format!(r#"{{"schema_version":1,"notion":{{}},"extra":{decimal}}}"#),
    )
    .unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "config_roundtrip_keeps_unknown_large_integer_fields",
        ])
        .env("OMS_JOURNAL_JSON_CONFIG_CHILD", "1")
        .env("OMS_WORK_JOURNAL_CONFIG", &path)
        .status()
        .unwrap();
    assert!(status.success());
    let replay = parse_python_ordered(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        replay
            .get("extra")
            .unwrap()
            .as_integer()
            .unwrap()
            .as_decimal(),
        decimal
    );
}
