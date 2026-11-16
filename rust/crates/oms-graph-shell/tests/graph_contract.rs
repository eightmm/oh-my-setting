use oms_graph_shell::{ParseResult, parse, parse_with_python};
use serde_json::Value;

#[test]
fn legacy_graph_fixtures() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures.json")).unwrap();
    for case in cases {
        let path = case["path"].as_str().unwrap();
        let text = case["text"].as_str().unwrap();
        if text.contains("python3") && !text.ends_with("import x\n") {
            continue;
        }
        let actual = serde_json::to_value(parse(path, text, "digest").unwrap()).unwrap();
        assert_eq!(actual, case["expected"], "input: {text:?}");
    }
}

#[test]
fn python_adapter_maps_complete_fragment_output() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures.json")).unwrap();
    let case = &cases[11];
    let text = case["text"].as_str().unwrap();
    let mut calls = 0;
    let actual = parse_with_python("tests/example.sh", text, "digest", |path, fragment, digest| {
        calls += 1;
        assert_eq!(path, "tests/example.sh");
        assert_eq!(digest, "digest");
        assert_eq!(fragment, "import panel_metrics, oms_graph.project.build as build\nfrom oms_runtime.common import canonical_json as canon\ndef nested():\n    import ignored");
        Ok(serde_json::from_str::<ParseResult>(include_str!("python_fragment.json")).unwrap())
    }).unwrap();
    assert_eq!(calls, 1);
    assert_eq!(serde_json::to_value(actual).unwrap(), case["expected"]);
    assert!(parse("tests/example.sh", text, "digest").is_err());
    assert!(
        parse_with_python("tests/example.sh", text, "digest", |_, _, _| Err(
            "adapter failed".into()
        ))
        .is_err()
    );
}

#[test]
fn python_tabs_and_invalid_source_are_projected() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures.json")).unwrap();
    let parsed: Vec<ParseResult> = serde_json::from_str(include_str!("python_other.json")).unwrap();
    for (fixture, fragment, result) in [
        (12, "import panel_metrics", &parsed[0]),
        (13, "invalid syntax !!!", &parsed[1]),
    ] {
        let case = &cases[fixture];
        let graph = parse_with_python(
            "tests/example.sh",
            case["text"].as_str().unwrap(),
            "digest",
            |_, body, _| {
                assert_eq!(body, fragment);
                Ok(result.clone())
            },
        )
        .unwrap();
        assert_eq!(serde_json::to_value(graph).unwrap(), case["expected"]);
    }
}

#[test]
fn python_environment_edges_use_shell_owner_and_file_lines() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures.json")).unwrap();
    let case = &cases[cases.len() - 2];
    let fragment: ParseResult = serde_json::from_str(include_str!("python_env.json")).unwrap();
    let graph = parse_with_python(
        "tests/example.sh",
        case["text"].as_str().unwrap(),
        "digest",
        |_, _, _| Ok(fragment.clone()),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(graph).unwrap(), case["expected"]);
}

#[test]
fn malformed_python_adapter_lines_fail() {
    for line in [
        serde_json::json!(null),
        serde_json::json!(0),
        serde_json::json!(u64::MAX),
    ] {
        let mut fragment = ParseResult::default();
        fragment
            .edges
            .push(serde_json::json!({"relation":"reads_env","evidence":{"line":line}}));
        assert!(
            parse_with_python("x.sh", "python3 <<PY\nbody\nPY\n", "d", |_, _, _| Ok(
                fragment.clone()
            ))
            .is_err()
        );
    }
}

#[test]
fn python_oversize_heredoc_line_keeps_offsets() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures.json")).unwrap();
    let case = cases.last().unwrap();
    let fragment: ParseResult = serde_json::from_str(include_str!("python_long.json")).unwrap();
    let graph = parse_with_python(
        "tests/example.sh",
        case["text"].as_str().unwrap(),
        "digest",
        |_, body, _| {
            assert_eq!(body, "\nimport panel_metrics");
            Ok(fragment.clone())
        },
    )
    .unwrap();
    assert_eq!(serde_json::to_value(graph).unwrap(), case["expected"]);
}

#[test]
fn function_summaries_drop_sensitive_text() {
    for comment in [
        "Reads cfg/a=/etc/passwd now.",
        "Uses dir/x,/usr/share/example here.",
    ] {
        let input = format!("# {comment}\nf() {{\n}}\n");
        assert!(parse("x.sh", &input, "d").unwrap().nodes[0]["summary"].is_null());
    }
    let label = ["pass", "word"].concat();
    let input = format!("# {label}{}value\nf() {{\n}}\n", '=');
    let parsed = parse("x.sh", &input, "d").unwrap();
    assert!(parsed.nodes[0]["summary"].is_null());
    let input = format!("# {}\nf() {{\n}}\n", ["/ho", "me/person"].concat());
    assert!(parse("x.sh", &input, "d").unwrap().nodes[0]["summary"].is_null());
}
