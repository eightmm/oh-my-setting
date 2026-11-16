mod support;
use std::ffi::OsString;
use std::sync::atomic::AtomicBool;

fn args(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

#[test]
fn owns_every_argument_vector() {
    let fixture = support::Temp::new();
    assert_eq!(fixture.call(&[]).0, 0);
    assert!(oms_command_telemetry::owns(&args(&[
        "--repo",
        "telemetry",
        "telemetry"
    ])));
    assert!(oms_command_telemetry::owns(&args(&["telemetry", "--by"])));
    assert!(oms_command_telemetry::owns(&args(&[
        "--repo",
        "telemetry",
        "list"
    ])));
    assert!(oms_command_telemetry::owns(&args(&["--file", "telemetry"])));
    assert!(oms_command_telemetry::owns(&args(&[
        "telemetry",
        "--waste"
    ])));
    assert!(oms_command_telemetry::owns(&args(&[
        "--file",
        "--waste",
        "telemetry"
    ])));
    let result = oms_command_telemetry::run(
        &args(&["list", "--json"]),
        &fixture.0,
        &AtomicBool::new(false),
    );
    assert_eq!(result.0, 0);
}

#[test]
fn help_and_validation_order_do_not_read() {
    let root = support::Temp::new();
    assert_eq!(root.call(&[]).0, 0);
    root.index(b"broken\n");
    for items in [
        vec!["telemetry", "--help"],
        vec!["--help", "telemetry"],
        vec!["telemetry", "-h", "--by", "bad"],
    ] {
        let result = oms_command_telemetry::run(&args(&items), &root.0, &AtomicBool::new(false));
        assert_eq!(result.0, 0);
        assert!(result.2.is_empty());
        assert!(result.1.starts_with(b"Usage: artifact-index.sh"));
    }
    for items in [
        vec!["telemetry", "--waste"],
        vec!["telemetry", "--waste", "--by", "role"],
        vec!["--repo", "no-such-repo", "telemetry", "--waste"],
        vec!["list"],
        vec!["resolve", "telemetry"],
        vec!["--file", "telemetry"],
    ] {
        assert!(oms_command_telemetry::owns(&args(&items)));
        let result = oms_command_telemetry::run(
            &args(&items),
            &root.0.join("missing"),
            &AtomicBool::new(true),
        );
        assert_eq!(result.0, 2);
        assert!(result.1.is_empty());
    }
    assert_eq!(
        oms_command_telemetry::run(&args(&["telemetry"]), &root.0, &AtomicBool::new(true)),
        (2, Vec::new(), b"error: interrupted\n".to_vec())
    );
    root.index(b"{}\n");
    for items in [
        vec!["--since", "", "--until", ""],
        vec!["--top", ""],
        vec!["--reason", ""],
        vec!["0001"],
        vec!["9223372036854775807"],
    ] {
        assert_eq!(root.call(&items).0, 0, "{items:?}");
    }
}
