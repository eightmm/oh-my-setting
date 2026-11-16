use oms_context_session::inspect_injections;
use serde_json::json;

#[test]
fn ledger_accounting_and_omissions() {
    let data = b"{\"action\":\"injection\",\"status\":\"resume\",\"bytes\":900,\"session\":\"s1\",\"source\":\"startup\"}\n{\"action\":\"injection\",\"status\":\"resume\",\"bytes\":1100,\"session\":\"s1\",\"source\":\"compact\"}\n{\"action\":\"injection\",\"status\":\"prompt\",\"bytes\":40,\"session\":\"s2\"}\n{\"action\":\"telemetry\"}\n{\"action\":\"injection\",\"status\":\"resume\",\"bytes\":true}\nnot json\n";
    let v = inspect_injections(data, None, 1).unwrap();
    assert_eq!(
        (
            v["injections"].as_u64(),
            v["bytes"].as_u64(),
            v["invalid_rows"].as_u64()
        ),
        (Some(3), Some(2040), Some(2))
    );
    assert_eq!(
        v["by_hook"],
        json!({"total":2,"omitted":1,"items":[{"hook":"resume","calls":2,"bytes":2000,"largest":1100}]})
    );
    assert_eq!(
        inspect_injections(data, Some("s2"), 10).unwrap()["bytes"],
        40
    );
}

#[test]
fn limits_and_input_validation() {
    assert!(inspect_injections(b"", None, 0).is_err());
    assert!(inspect_injections(b"", None, 101).is_err());
    assert!(inspect_injections(b"", Some("bad\n"), 1).is_err());
    assert_eq!(inspect_injections(b"", None, 100).unwrap()["bytes"], 0);
    let data = b"{\"action\":\"injection\",\"status\":\"x\",\"bytes\":0}\n{\"action\":\"injection\",\"status\":\"x\",\"bytes\":1099511627776}\n{\"action\":\"injection\",\"status\":\"x\",\"bytes\":1099511627777}\n";
    let v = inspect_injections(data, None, 1).unwrap();
    assert_eq!(v["invalid_rows"], 2);
    assert_eq!(v["bytes"], 1099511627776_u64);
}

#[test]
fn oversized_lines_and_ledger() {
    use oms_context_session::{MAX_LEDGER_BYTES, MAX_LINE_BYTES};
    let mut data = vec![b' '; MAX_LINE_BYTES + 1];
    data.extend_from_slice(b"\n{}\n");
    let value = inspect_injections(&data, None, 1).unwrap();
    assert_eq!(value["invalid_rows"], 1);
    let mut data = vec![b' '; MAX_LINE_BYTES];
    assert_eq!(
        inspect_injections(&data, None, 1).unwrap()["invalid_rows"],
        1
    );
    data.resize(MAX_LEDGER_BYTES, b' ');
    assert!(inspect_injections(&data, None, 1).is_ok());
    data.push(b' ');
    assert!(inspect_injections(&data, None, 1).is_err());
}

#[test]
fn excess_keys_are_folded_into_other() {
    let mut data = String::new();
    for n in 0..=oms_context_session::MAX_KEYS + 1 {
        data.push_str(&format!(
            "{{\"action\":\"injection\",\"status\":\"k{n}\",\"bytes\":1}}\n"
        ));
    }
    let value = inspect_injections(data.as_bytes(), None, 1).unwrap();
    assert_eq!(value["injections"], 4098);
    assert_eq!(value["by_hook"]["total"], 4097);
}

#[test]
fn unicode_labels_follow_python_alphanumeric_categories() {
    let data = "{\"action\":\"injection\",\"status\":\"\u{345}\",\"bytes\":1}\n{\"action\":\"injection\",\"status\":\"²\",\"bytes\":1}\n";
    let value = inspect_injections(data.as_bytes(), None, 10).unwrap();
    assert_eq!(value["invalid_rows"], 1);
    assert_eq!(value["injections"], 1);
}

#[test]
fn encoded_rows_and_nonfinite_values() {
    let row = "{\"action\":\"injection\",\"status\":\"ok\",\"bytes\":1,\"extra\":NaN}";
    let utf16: Vec<u8> = row.encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert_eq!(
        inspect_injections(&utf16, None, 10).unwrap()["injections"],
        1
    );
    let data = b"{\"action\":\"injection\",\"status\":\"x\",\"bytes\":Infinity}\n";
    assert_eq!(
        inspect_injections(data, None, 10).unwrap()["invalid_rows"],
        1
    );
}
