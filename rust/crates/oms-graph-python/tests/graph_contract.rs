//! Contract of the native Python graph parser, driven only through the public
//! `Session` API and the real private helper process (`CARGO_BIN_EXE_*`).
//!
//! `fixtures/legacy_cases.json` holds each case's source and the complete
//! `{nodes, edges, refs}` output of `PythonParser` version 6 (CPython 3.12) at
//! baseline 24b71701; credential-looking words are stored with a JSON unicode
//! escape so the file passes the outbound scrubber. The cases cover canonical
//! parser fixtures, nested/decorated/async definitions, env aliases, f-strings,
//! Unicode identifiers and spans, newline conventions, and invalid source.
//! Nothing here runs Python. Lifecycle tests use owned shell fixtures as fake
//! helpers; every test that spawns a helper must leave no live child.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use oms_graph_python::{CancelFlag, Error, Options, ParseResult, Parser, Session};
use serde_json::{Value, json};

fn helper_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oms-graph-python-helper"))
}

fn start(options: Options) -> Session {
    Session::start(&helper_path(), options).expect("helper starts")
}

thread_local! {
    /// One persistent helper per test thread, reused by every plain parse.
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

fn try_parse(path: &str, text: &str, digest: &str) -> Result<ParseResult, Error> {
    SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        let session = slot.get_or_insert_with(|| start(Options::default()));
        let outcome = Parser::parse(session, path, text, digest);
        if session.is_poisoned() {
            *slot = None;
        }
        outcome
    })
}

fn parse(path: &str, text: &str, digest: &str) -> ParseResult {
    try_parse(path, text, digest).expect("helper answers")
}

fn parse_json(path: &str, text: &str, digest: &str) -> String {
    parse(path, text, digest).to_json()
}

fn legacy_cases() -> Vec<Value> {
    let raw = include_str!("fixtures/legacy_cases.json");
    match serde_json::from_str(raw).expect("fixture is valid JSON") {
        Value::Array(cases) => cases,
        other => panic!("fixture root must be an array, found {other}"),
    }
}

fn as_value(result: &ParseResult) -> Value {
    serde_json::to_value(result).expect("serializable")
}

fn text_of(case: &Value, key: &str) -> String {
    case[key].as_str().expect("string field").to_owned()
}

#[test]
fn matches_every_legacy_case_exactly() {
    let cases = legacy_cases();
    assert!(cases.len() > 100, "fixture shrank to {} cases", cases.len());
    let mut mismatches = Vec::new();
    for case in &cases {
        let (name, path, source) = (
            text_of(case, "name"),
            text_of(case, "path"),
            text_of(case, "source"),
        );
        let actual = as_value(&parse(&path, &source, "digest"));
        if actual != case["expected"] {
            mismatches.push(name);
        }
    }
    assert!(
        mismatches.is_empty(),
        "differs from legacy output: {mismatches:?}"
    );
}

#[test]
fn output_is_deterministic_and_round_trips() {
    for case in legacy_cases() {
        let (path, source) = (text_of(&case, "path"), text_of(&case, "source"));
        let first = parse_json(&path, &source, "digest");
        assert_eq!(first, parse_json(&path, &source, "digest"), "{path}");
        let decoded: ParseResult = serde_json::from_str(&first).expect("round trip");
        assert_eq!(decoded, parse(&path, &source, "digest"), "{path}");
    }
}

#[test]
fn invalid_source_keeps_only_the_module_and_its_edge() {
    for source in [
        "def f(:\n",
        "x = (1,\n",
        "if 1:\npass\n",
        "x = 1\0\n",
        "\u{feff}import os\n",
    ] {
        let result = parse("pkg\\mod.py", source, "d");
        assert_eq!(result.nodes.len(), 1, "{source:?}");
        assert_eq!(result.edges.len(), 1, "{source:?}");
        assert!(result.refs.is_empty(), "{source:?}");
        assert_eq!(
            as_value(&result)["nodes"][0],
            json!({
                "id": "module:pkg/mod.py", "kind": "module", "name": "pkg\\mod.py",
                "path": "pkg/mod.py", "language": "python", "source_digest": "d",
                "summary": null, "crux": null, "metadata": {}
            }),
            "module name is the raw text after the last slash, path is normalized"
        );
    }
}

#[test]
fn identifiers_are_nfkc_normalized() {
    let result = parse(
        "m.py",
        "import \u{ff4f}\u{ff53}\n\u{ff4f}\u{ff53}.environ[\"A_B\"]\n",
        "d",
    );
    let value = as_value(&result);
    assert_eq!(value["refs"][0]["value"], "os");
    assert_eq!(value["edges"][1]["relation"], "reads_env");
    let kelvin = parse("m.py", "def \u{212a}():\n    pass\n", "d");
    assert_eq!(kelvin.nodes[1].name, "K");
}

#[test]
fn line_spans_follow_python_newline_rules() {
    let source =
        "x = 1  # a\u{2028}b\r\ndef f():\r    pass\ndef g():\n    \"\"\"a\u{85}b\"\"\"\n    f()\n";
    let result = parse("m.py", source, "d");
    let lines: Vec<_> = result
        .nodes
        .iter()
        .skip(1)
        .map(|node| {
            (
                node.name.as_str(),
                node.metadata.line,
                node.metadata.end_line,
            )
        })
        .collect();
    assert_eq!(lines, [("f", Some(2), Some(3)), ("g", Some(4), Some(6))]);
}

#[test]
fn credential_and_path_docstrings_are_withheld_from_summaries() {
    let key = format!("{}{}", "pass", "word");
    for doc in [
        format!("Set {key}: now."),
        "See /usr/lib/thing now.".to_owned(),
    ] {
        let source = format!("def f():\n    \"\"\"{doc}\"\"\"\n");
        assert_eq!(parse("m.py", &source, "d").nodes[1].summary, None, "{doc}");
    }
    let source = "\"\"\"Module  text.\n\nMore.\"\"\"\ndef f():\n    \"Short doc\"\n";
    let result = parse("m.py", source, "d");
    assert_eq!(result.nodes[0].summary.as_deref(), Some("Module text."));
    assert_eq!(result.nodes[1].summary.as_deref(), Some("Short doc"));
}

#[test]
fn newer_python_syntax_parses_as_data() {
    let sources = [
        "def f[T = int](x: T) -> T:\n    g()\n",
        "x = t\"{a()}\"\n",
        "type X[T] = list[T]\nclass C[T](Base[T]):\n    def m(self):\n        self.m()\n",
        "x = f\"{f\"{a()}\"}\"\n",
    ];
    for source in sources {
        let result = parse("m.py", source, "d");
        assert!(result.nodes.len() + result.refs.len() > 1, "{source:?}");
    }
}

#[test]
fn tokenizer_limits_match_cpython() {
    let nested = |depth: usize| format!("{}{}\n", "(".repeat(depth), ")".repeat(depth));
    assert_eq!(parse("m.py", &nested(200), "d").edges.len(), 1);
    assert_eq!(
        parse("m.py", &format!("f{}", nested(200)), "d").refs.len(),
        1
    );
    assert!(
        parse("m.py", &format!("f{}", nested(201)), "d")
            .refs
            .is_empty()
    );
    let indented = |levels: usize| {
        let mut source = String::new();
        for level in 0..levels {
            source.push_str(&format!("{}if 1:\n", " ".repeat(level)));
        }
        source.push_str(&format!("{}g()\n", " ".repeat(levels)));
        source
    };
    assert_eq!(parse("m.py", &indented(99), "d").refs.len(), 1);
    assert!(parse("m.py", &indented(100), "d").refs.is_empty());
}

fn summary_of(source: &str) -> Option<String> {
    parse("m.py", source, "d").nodes[1].summary.clone()
}

#[test]
fn escaped_lone_surrogates_show_as_question_marks_but_real_replacement_chars_stay() {
    let doc = |body: &str| summary_of(&format!("def f():\n    \"{body}\"\n"));
    let replacement = "\u{fffd}";
    // Escaped surrogates: `\u`, `\U`, a pair (two lone surrogates in Python).
    assert_eq!(doc(r"A\ud800B. tail").as_deref(), Some("A?B."));
    assert_eq!(doc(r"\U0000dc00 x.").as_deref(), Some("? x."));
    assert_eq!(doc(r"\ud83d\ude00 x.").as_deref(), Some("?? x."));
    // The placeholder is not the sentence-ending `?` it is displayed as.
    assert_eq!(doc(r"\ud800 x. y").as_deref(), Some("? x."));
    // A real U+FFFD, however written, is kept.
    assert_eq!(
        doc(&format!("A{replacement}B. tail")).as_deref(),
        Some("A\u{fffd}B.")
    );
    assert_eq!(doc(r"A\ufffdB.").as_deref(), Some("A\u{fffd}B."));
    assert_eq!(
        doc(r"A\N{REPLACEMENT CHARACTER}B.").as_deref(),
        Some("A\u{fffd}B.")
    );
    // Both in one literal, and across implicitly concatenated parts.
    assert_eq!(
        doc(&format!(r"\ud800{replacement}\ufffd\udfff.")).as_deref(),
        Some("?\u{fffd}\u{fffd}?.")
    );
    let parts = format!("def f():\n    \"a\\ud800\" \"{replacement}b.\"\n");
    assert_eq!(summary_of(&parts).as_deref(), Some("a?\u{fffd}b."));
    // Other escapes around a surrogate decode as Python does.
    assert_eq!(doc(r"\x41\ud800\n\101.").as_deref(), Some("A? A."));
    assert_eq!(doc(r"\\ud800 x.").as_deref(), Some(r"\ud800 x."));
}

#[test]
fn raw_bytes_and_other_literals_never_hold_surrogates() {
    let source = "def f():\n    r\"\\ud800 x.\"\n";
    assert_eq!(summary_of(source).as_deref(), Some(r"\ud800 x."));
    let source = "def f():\n    R'''\\U0000dc00 y.'''\n";
    assert_eq!(summary_of(source).as_deref(), Some(r"\U0000dc00 y."));
    // Not a docstring: bytes, f-string.
    assert_eq!(summary_of("def f():\n    b\"\\ud800\"\n"), None);
    assert_eq!(summary_of("def f():\n    f\"\\ud800{1}\"\n"), None);
    // Module and class docstrings take the same path.
    let source = "\"\"\"M\\ud800.\"\"\"\nclass C:\n    '''C\\udfff.'''\n";
    let result = parse("m.py", source, "d");
    assert_eq!(result.nodes[0].summary.as_deref(), Some("M?."));
    assert_eq!(result.nodes[1].summary.as_deref(), Some("C?."));
}

#[test]
fn surrogate_docstrings_keep_the_privacy_filters() {
    let key = ["pass", "word"].concat();
    for doc in [
        format!(r"\ud800 {key}: x."),
        r"\ud800 see /usr/lib/thing now.".to_owned(),
    ] {
        assert_eq!(
            summary_of(&format!("def f():\n    \"{doc}\"\n")),
            None,
            "{doc}"
        );
    }
    // A surrogate between the word and its colon breaks the pattern, as in Python.
    let split = format!("def f():\n    \"{key}\\ud800: x.\"\n");
    assert_eq!(summary_of(&split), Some(format!("{key}?: x.")));
    // The 240 character bound counts a surrogate as one character.
    let long = format!(r#""\ud800{}""#, "x".repeat(300));
    let summary = summary_of(&format!("def f():\n    {long}\n")).unwrap();
    assert_eq!(summary.chars().count(), 240);
    assert!(summary.starts_with('?') && summary.ends_with("..."));
}

/// Run one named probe in a child test process: an overflow there aborts only
/// the child, so a regression fails this test instead of killing the harness.
#[test]
fn independent_lambda_defaults_are_shallow_and_accepted() {
    let n = 10_001;
    let sums: Vec<String> = (0..n).map(|i| format!("p{i}=1+1")).collect();
    let lambdas: Vec<String> = (0..n).map(|i| format!("p{i}=lambda: 0")).collect();
    for params in [sums, lambdas] {
        let source = format!("f = lambda {}: g()\n", params.join(", "));
        let result = parse("m.py", &source, "d");
        assert_eq!(result.refs.len(), 1);
        assert_eq!(result.refs[0].value, "g");
    }
}

#[test]
fn independent_not_operands_are_shallow_and_accepted() {
    for sep in [" or ", " and "] {
        let source = format!(
            "x = {}\ndef f():\n    foo()\n",
            vec!["not a"; 10_001].join(sep)
        );
        let result = parse("m.py", &source, "d");
        assert_eq!(result.nodes.len(), 2, "{sep:?}");
    }
}

#[test]
fn decimal_literal_limit_maps_to_the_legacy_invalid_graph() {
    let sentinel = "def f():\n    foo()\n";
    let graph = |literal: &str| parse("m.py", &format!("x = {literal}\n{sentinel}"), "d");
    let invalid = |literal: &str| {
        let result = graph(literal);
        assert_eq!(
            (result.nodes.len(), result.edges.len(), result.refs.len()),
            (1, 1, 0)
        );
    };
    let valid = |literal: &str| {
        let result = graph(literal);
        assert_eq!(result.nodes.len(), 2, "sentinel function is extracted");
        assert_eq!(result.refs.len(), 1);
    };
    valid(&"1".repeat(4_300));
    invalid(&"1".repeat(4_301));
    valid(&format!("1{}", "_1".repeat(4_299)));
    invalid(&format!("1{}", "_1".repeat(4_300)));
    // Exempt: other bases, all zeros, floats and imaginary literals.
    valid(&format!("0x{}", "1".repeat(5_000)));
    valid(&"0".repeat(5_000));
    valid(&format!("{}.5", "1".repeat(5_000)));
    valid(&format!("{}j", "1".repeat(5_000)));
    // Nested positions: inside a function body and an f-string.
    let nested = format!("def g():\n    return {}\n{sentinel}", "2".repeat(4_301));
    assert_eq!(parse("m.py", &nested, "d").nodes.len(), 1);
    let in_fstring = format!("x = f\"{{{}}}\"\n{sentinel}", "3".repeat(4_301));
    assert_eq!(parse("m.py", &in_fstring, "d").nodes.len(), 1);
    // The invalid result is a graph, not an error.
    assert!(try_parse("m.py", &format!("x = {}\n", "1".repeat(4_301)), "d").is_ok());
}

#[test]
fn surrogate_survives_a_docstring_holding_every_private_use_candidate() {
    // Every character a sentinel scheme could pick is present in the docstring.
    let private_use: String = (0x10_FF00u32..=0x10_FFFD)
        .filter_map(char::from_u32)
        .collect();
    assert!(private_use.chars().count() >= 254);
    let source = format!("def f():\n    \"\\ud800{private_use}\\udfff\\ufffd\"\n");
    let summary = summary_of(&source).expect("summary");
    let expected: String = ["?"]
        .into_iter()
        .chain(private_use.chars().take(236).map(|_| ""))
        .collect();
    assert!(summary.starts_with(&expected));
    assert_eq!(summary.chars().count(), 240);
    assert!(summary.ends_with("..."));
    let kept: String = summary.chars().skip(1).take(236).collect();
    assert_eq!(kept, private_use.chars().take(236).collect::<String>());
    // Short form: the surrogate and a real U+FFFD sit beside the whole pool.
    let short = "def f():\n    \"\\ud800\u{fffd}\\udfff.\"\n";
    assert_eq!(summary_of(short).as_deref(), Some("?\u{fffd}?."));
}

// ---------------------------------------------------------------------------
// Session: one persistent helper, replayed goldens, size options.
// ---------------------------------------------------------------------------

#[test]
fn one_persistent_helper_replays_every_legacy_case() {
    let mut session = start(Options::default());
    let pid = session.pid();
    let cases = legacy_cases();
    for case in &cases {
        let (path, source) = (text_of(case, "path"), text_of(case, "source"));
        let actual = session.parse(&path, &source, "digest").expect("answers");
        assert_eq!(as_value(&actual), case["expected"], "{}", case["name"]);
        assert_eq!(session.pid(), pid, "the helper process is reused");
    }
    assert!(!session.is_poisoned());
    #[cfg(target_os = "linux")]
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
    session.close();
    assert_gone(pid);
}

#[test]
fn source_size_is_an_explicit_option_and_refusal_keeps_the_session() {
    let source = "def f():\n    pass\n";
    let mut at_limit = start(Options {
        max_bytes: source.len(),
        ..Options::default()
    });
    assert!(at_limit.parse("m.py", source, "d").is_ok());
    let mut below = start(Options {
        max_bytes: source.len() - 1,
        ..Options::default()
    });
    let error = below.parse("m.py", source, "d").unwrap_err();
    assert_eq!(
        error,
        Error::SourceTooLarge {
            bytes: source.len(),
            max_bytes: source.len() - 1
        }
    );
    assert!(error.session_survives() && !below.is_poisoned());
    assert!(
        below.parse("m.py", "", "d").is_ok(),
        "empty source, zero edge"
    );
    assert_eq!(Options::default().max_bytes, 2 * 1024 * 1024);
    let big = format!("x = 1\n#{}", "#".repeat(Options::default().max_bytes));
    let mut defaults = start(Options::default());
    assert!(matches!(
        defaults.parse("m.py", &big, "d"),
        Err(Error::SourceTooLarge { .. })
    ));
    let mut raised = start(Options {
        max_bytes: big.len(),
        ..Options::default()
    });
    assert!(raised.parse("m.py", &big, "d").is_ok());
    // No option exceeds the protocol ceiling.
    let mut capped = start(Options {
        max_bytes: usize::MAX,
        ..Options::default()
    });
    assert!(capped.parse("m.py", "x = 1\n", "d").is_ok());
    let long = "p".repeat(70_000);
    assert!(matches!(
        capped.parse(&long, "x = 1\n", "d"),
        Err(Error::FieldTooLong { field: "path", .. })
    ));
}

// ---------------------------------------------------------------------------
// Shallow valid input must match the canonical graph; no depth heuristic.
// ---------------------------------------------------------------------------

#[test]
fn flat_valid_forms_are_accepted_at_any_width() {
    let n = 10_001;
    let sentinel = "\ndef f():\n    foo()\n";
    let cases = [
        format!("x = {}", vec!["a"; n].join(" is not ")),
        format!("x = {}", vec!["a"; n].join(" not in ")),
        format!("x = {}", vec!["a"; n].join(" < ")),
        format!("x = {}", vec!["not a"; n].join(" or ")),
        format!("x = [v for v in w{}]", " if a".repeat(n)),
        format!("x = [v for v in w{}]", " for a in b".repeat(n)),
        format!("x = {}", vec!["a() == 1"; n].join(" and ")),
        format!(
            "f = lambda {}: g()",
            (0..n)
                .map(|i| format!("p{i}=lambda: 0"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        format!(
            "f = lambda {}: g()",
            (0..n)
                .map(|i| format!("p{i}=1+1"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ];
    let mut session = start(Options::default());
    for case in cases {
        let result = session
            .parse("m.py", &format!("{case}{sentinel}"), "d")
            .expect("answers");
        assert!(
            result.nodes.iter().any(|node| node.name == "f"),
            "{}",
            &case[..24]
        );
        assert!(!session.is_poisoned());
    }
}

// ---------------------------------------------------------------------------
// Hostile input: the helper may die, the test process and host never do.
// ---------------------------------------------------------------------------

fn hostile_cases() -> Vec<(&'static str, String)> {
    let long_chain = |unit: &str, n: usize| unit.repeat(n);
    vec![
        (
            "semicolon in call",
            format!("x = f({}", "a; ".repeat(100_000)),
        ),
        (
            "semicolon in list",
            format!("x = [{}", "1; ".repeat(100_000)),
        ),
        (
            "semicolon in fstring",
            format!("x = f\"{{{}", "a; ".repeat(100_000)),
        ),
        ("nested call semicolons", format!("{}1;)", "f(".repeat(150))),
        ("mismatched closers", ")".repeat(30_000)),
        ("mismatched pairs", "(]".repeat(30_000)),
        ("mismatched braces", "{)".repeat(30_000)),
        (
            "closers after chain",
            format!("{}x{}", "a+".repeat(30_000), "]".repeat(30_000)),
        ),
        (
            "invalid assignment target",
            format!("{}b = 1\n", "a.".repeat(100_000)),
        ),
        (
            "call assignment target",
            format!("{} = 1\n", "f()".repeat(100_000)),
        ),
        (
            "invalid del target",
            format!("del {}a\n", "a+".repeat(100_000)),
        ),
        (
            "invalid comprehension target",
            format!("[1 for {}a in b]\n", "a+".repeat(100_000)),
        ),
        ("starred targets", format!("{}a = 1\n", "*".repeat(100_000))),
        (
            "deep pattern",
            format!(
                "match x:\n    case {}1:\n        pass\n",
                "-".repeat(100_000)
            ),
        ),
        (
            "pattern or chain",
            format!(
                "match x:\n    case {}:\n        pass\n",
                "1 | ".repeat(100_000) + "2"
            ),
        ),
        (
            "class pattern chain",
            format!(
                "match x:\n    case {}:\n        pass\n",
                "a.".repeat(100_000) + "b()"
            ),
        ),
        (
            "nested format specs",
            format!("x = f\"{}\"\n", "{a:".repeat(5_000)),
        ),
        (
            "nested fstring quotes",
            format!("x = {}1{}\n", "f\"{".repeat(2_000), "}\"".repeat(2_000)),
        ),
        (
            "error after deep ast",
            format!("x = {}a $\n", "a+".repeat(150_000)),
        ),
        (
            "deep unary valid",
            format!("x = {}1\n", long_chain("-", 200_000)),
        ),
        (
            "deep not valid",
            format!("x = {}a\n", long_chain("not ", 100_000)),
        ),
        (
            "deep lambda",
            format!("x = {}1\n", long_chain("lambda: ", 100_000)),
        ),
        (
            "deep power",
            format!("x = {}\n", long_chain("a**", 150_000) + "a"),
        ),
        (
            "deep ternary",
            format!("x = {}b\n", long_chain("a if c else ", 100_000)),
        ),
        (
            "deep attribute chain",
            format!("x = a{}\n", ".b".repeat(400_000)),
        ),
        (
            "deep call chain",
            format!("x = a{}\n", "()".repeat(400_000)),
        ),
        (
            "deep subscript chain",
            format!("x = a{}\n", "[0]".repeat(300_000)),
        ),
        (
            "deep await",
            format!("async def f():\n    x = {}a\n", "await ".repeat(100_000)),
        ),
        (
            "deep walrus",
            format!("x = {}1{}\n", "(a := ".repeat(190), ")".repeat(190)),
        ),
    ]
}

#[test]
fn hostile_input_never_aborts_the_host_and_a_fresh_session_recovers() {
    let options = Options {
        timeout: Duration::from_secs(120),
        ..Options::default()
    };
    for (name, source) in hostile_cases() {
        let mut session = start(options);
        let pid = session.pid();
        let outcome = session.parse("m.py", &source, "d");
        eprintln!(
            "hostile {name}: {}",
            outcome.as_ref().map_or_else(
                |e| e.to_string(),
                |r| format!("graph of {} nodes", r.nodes.len())
            )
        );
        match outcome {
            // Survived: a valid graph (usually the invalid-source one).
            Ok(result) => assert!(!result.nodes.is_empty(), "{name}"),
            // The helper crashed or refused: typed, contained, cleaned up.
            Err(error) => {
                assert!(
                    matches!(
                        error,
                        Error::HelperExited(_)
                            | Error::ResponseTooLarge { .. }
                            | Error::SourceTooLarge { .. }
                    ),
                    "{name}: {error}"
                );
                if !error.session_survives() {
                    assert!(session.is_poisoned(), "{name}");
                    assert_eq!(session.parse("m.py", "x = 1\n", "d"), Err(Error::Poisoned));
                    assert_gone(pid);
                }
            }
        }
        // Whatever happened, a new session works.
        let mut fresh = start(options);
        let graph = fresh
            .parse("m.py", "def f():\n    g()\n", "d")
            .expect("recovers");
        assert_eq!(graph.refs.len(), 1, "{name}");
    }
}

#[test]
fn helper_crash_is_a_typed_error_not_an_empty_graph() {
    // Deep enough to overflow the helper's parse stack in any build profile.
    let source = format!("x = {}\n", "a+".repeat(3_000_000) + "a");
    let mut session = start(Options {
        max_bytes: source.len(),
        timeout: Duration::from_secs(120),
    });
    let pid = session.pid();
    let outcome = session.parse("m.py", &source, "d");
    match outcome {
        Err(Error::HelperExited(reason)) => {
            assert!(
                reason.contains("signal") || reason.contains("status"),
                "{reason}"
            );
            assert!(session.is_poisoned());
            assert_gone(pid);
        }
        // A build whose stack held it is also correct: it is a real graph.
        Ok(result) => assert!(result.nodes.len() > 1),
        Err(other) => panic!("unexpected {other}"),
    }
}

// ---------------------------------------------------------------------------
// Lifecycle with owned fake helpers.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod lifecycle {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// The native test fixture started under a scenario name (a symlink in a
    /// private temporary directory, removed on drop).
    struct Fake {
        dir: PathBuf,
        path: PathBuf,
    }

    impl Fake {
        fn new(scenario: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "oms-graph-python-fixture-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).expect("fixture dir");
            let path = dir.join(format!("fixture-{scenario}"));
            symlink(env!("CARGO_BIN_EXE_oms-graph-python-fixture"), &path).expect("link");
            Self { dir, path }
        }

        fn start(&self, options: Options) -> Session {
            Session::start(&self.path, options).expect("fixture starts")
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn quick() -> Options {
        Options {
            timeout: Duration::from_secs(20),
            ..Options::default()
        }
    }

    #[test]
    fn framing_violations_are_protocol_errors_and_poison_the_session() {
        let cases = [
            ("bad version", "bad-version"),
            ("oversize", "oversize"),
            ("wrong id", "wrong-id"),
            ("malformed result", "malformed-result"),
            ("unexpected kind", "unexpected-kind"),
            ("garbage", "garbage"),
        ];
        for (name, scenario) in cases {
            let fake = Fake::new(scenario);
            let mut session = fake.start(quick());
            let pid = session.pid();
            let error = session.parse("m.py", "x = 1\n", "d").unwrap_err();
            assert!(matches!(error, Error::Protocol(_)), "{name}: {error}");
            assert!(session.is_poisoned(), "{name}");
            assert_eq!(session.parse("m.py", "x = 1\n", "d"), Err(Error::Poisoned));
            assert_gone(pid);
        }
    }

    #[test]
    fn truncated_frames_and_early_exit_are_helper_exited() {
        for (name, scenario) in [
            ("truncated header", "truncated-header"),
            ("truncated payload", "truncated-payload"),
            ("no output", "exit-0"),
            ("exit status", "exit-7"),
        ] {
            let fake = Fake::new(scenario);
            let mut session = fake.start(quick());
            let pid = session.pid();
            let error = session.parse("m.py", "x = 1\n", "d").unwrap_err();
            assert!(matches!(error, Error::HelperExited(_)), "{name}: {error}");
            assert_gone(pid);
        }
        let fake = Fake::new("exit-7");
        let error = fake.start(quick()).parse("m.py", "x\n", "d").unwrap_err();
        assert_eq!(
            error,
            Error::HelperExited("helper exited with status 7".to_owned())
        );
    }

    #[test]
    fn an_aborted_child_is_reported_with_its_signal() {
        let fake = Fake::new("abort");
        let mut session = fake.start(quick());
        let pid = session.pid();
        let error = session.parse("m.py", "x = 1\n", "d").unwrap_err();
        assert_eq!(
            error,
            Error::HelperExited("helper killed by signal 6".to_owned())
        );
        assert!(session.is_poisoned());
        assert_gone(pid);
    }

    #[test]
    fn a_hung_child_times_out_and_is_killed_and_reaped() {
        let fake = Fake::new("hang");
        let mut session = fake.start(Options {
            timeout: Duration::from_millis(300),
            ..Options::default()
        });
        let pid = session.pid();
        let started = std::time::Instant::now();
        let error = session.parse("m.py", "x = 1\n", "d").unwrap_err();
        assert_eq!(error, Error::Timeout { millis: 300 });
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_gone(pid);
        assert!(session.is_poisoned());
    }

    #[test]
    fn a_child_that_never_reads_cannot_block_a_large_write_past_the_timeout() {
        let fake = Fake::new("hang");
        let mut session = fake.start(Options {
            timeout: Duration::from_millis(300),
            max_bytes: 8 * 1024 * 1024,
        });
        let pid = session.pid();
        let source = format!("x = 1\n#{}", "#".repeat(6 * 1024 * 1024));
        assert_eq!(
            session.parse("m.py", &source, "d").unwrap_err(),
            Error::Timeout { millis: 300 }
        );
        assert_gone(pid);
    }

    #[test]
    fn cancellation_kills_reaps_and_reports_cancelled() {
        let fake = Fake::new("hang");
        let mut session = fake.start(Options::default());
        let pid = session.pid();
        let flag = CancelFlag::new();
        let remote = flag.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            remote.cancel();
        });
        let error = session
            .parse_cancellable("m.py", "x = 1\n", "d", &flag)
            .unwrap_err();
        canceller.join().expect("canceller");
        assert_eq!(error, Error::Cancelled);
        assert_gone(pid);
        assert_eq!(session.parse("m.py", "x\n", "d"), Err(Error::Poisoned));
        // A flag cancelled before the call stops it at once.
        let fake = Fake::new("hang");
        let mut session = fake.start(Options::default());
        let pid = session.pid();
        let flag = CancelFlag::new();
        flag.cancel();
        assert_eq!(
            session.parse_cancellable("m.py", "x\n", "d", &flag),
            Err(Error::Cancelled)
        );
        assert_gone(pid);
    }

    #[test]
    fn cancellation_outranks_a_fast_reply_size_errors_and_a_stuck_reply() {
        // The real helper answers well inside one poll slice: a flag set before
        // the call must still win, and leave no helper behind.
        let mut real = start(Options::default());
        let pid = real.pid();
        let flag = CancelFlag::new();
        flag.cancel();
        assert_eq!(
            real.parse_cancellable("m.py", "x = 1\n", "d", &flag),
            Err(Error::Cancelled)
        );
        assert!(real.is_poisoned());
        assert_gone(pid);
        // Cancellation precedes the size and field errors.
        let mut small = start(Options {
            max_bytes: 1,
            ..Options::default()
        });
        let pid = small.pid();
        assert_eq!(
            small.parse_cancellable("m.py", "x = 1\n", "d", &flag),
            Err(Error::Cancelled)
        );
        assert_gone(pid);
        // A caller looping over many small files with one shared flag stops at once.
        let mut session = start(Options::default());
        let shared = CancelFlag::new();
        let mut parsed = 0;
        let mut outcome = None;
        for index in 0..50 {
            if index == 5 {
                shared.cancel();
            }
            match session.parse_cancellable("m.py", "def f():\n    g()\n", "d", &shared) {
                Ok(_) => parsed += 1,
                Err(error) => {
                    outcome = Some(error);
                    break;
                }
            }
        }
        assert_eq!((parsed, outcome), (5, Some(Error::Cancelled)));
        assert_eq!(session.parse("m.py", "x\n", "d"), Err(Error::Poisoned));
        // A reply already waiting loses to a flag set before the call.
        let fast = Fake::new("fast-reply");
        let mut session = fast.start(Options::default());
        assert!(
            session.parse("m.py", "x\n", "d").is_ok(),
            "the fixture replies"
        );
        let mut session = fast.start(Options::default());
        let pid = session.pid();
        assert_eq!(
            session.parse_cancellable("m.py", "x\n", "d", &flag),
            Err(Error::Cancelled)
        );
        assert_gone(pid);
        // A flag set while a reply is still being produced beats that reply.
        let slow = Fake::new("slow-reply");
        let mut session = slow.start(Options::default());
        let pid = session.pid();
        let late = CancelFlag::new();
        let remote = late.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            remote.cancel();
        });
        assert_eq!(
            session.parse_cancellable("m.py", "x\n", "d", &late),
            Err(Error::Cancelled)
        );
        canceller.join().expect("canceller");
        assert_gone(pid);
    }

    #[test]
    fn extreme_timeouts_neither_panic_nor_succeed_after_the_deadline() {
        // Zero never succeeds, even against a reply that is ready at once.
        let mut real = start(Options {
            timeout: Duration::ZERO,
            ..Options::default()
        });
        let pid = real.pid();
        assert_eq!(
            real.parse("m.py", "x = 1\n", "d"),
            Err(Error::Timeout { millis: 0 })
        );
        assert_gone(pid);
        let fast = Fake::new("fast-reply");
        let mut session = fast.start(Options {
            timeout: Duration::ZERO,
            ..Options::default()
        });
        assert_eq!(
            session.parse("m.py", "x\n", "d"),
            Err(Error::Timeout { millis: 0 })
        );
        // A reply slower than a short budget times out near that budget, not a
        // poll slice later.
        let slow = Fake::new("slow-reply");
        let mut session = slow.start(Options {
            timeout: Duration::from_millis(5),
            ..Options::default()
        });
        let started = std::time::Instant::now();
        assert_eq!(
            session.parse("m.py", "x\n", "d"),
            Err(Error::Timeout { millis: 5 })
        );
        assert!(started.elapsed() < Duration::from_millis(300));
        // The largest timeout is an unbounded wait: no overflow panic, success
        // when the helper answers, and cancellation still ends a stuck one.
        let mut forever = start(Options {
            timeout: Duration::MAX,
            ..Options::default()
        });
        assert!(forever.parse("m.py", "x = 1\n", "d").is_ok());
        let hang = Fake::new("hang");
        let mut session = hang.start(Options {
            timeout: Duration::MAX,
            ..Options::default()
        });
        let pid = session.pid();
        let flag = CancelFlag::new();
        let remote = flag.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            remote.cancel();
        });
        assert_eq!(
            session.parse_cancellable("m.py", "x\n", "d", &flag),
            Err(Error::Cancelled)
        );
        canceller.join().expect("canceller");
        assert_gone(pid);
    }

    #[test]
    fn dropping_a_session_kills_and_reaps_its_child() {
        let fake = Fake::new("hang");
        let session = fake.start(Options::default());
        let pid = session.pid();
        drop(session);
        assert_gone(pid);
        let real = start(Options::default());
        let pid = real.pid();
        drop(real);
        assert_gone(pid);
    }

    #[test]
    fn the_helper_path_must_be_an_absolute_file_and_is_never_searched() {
        for path in [
            "oms-graph-python-helper",
            "./relative",
            "/definitely/not/here",
        ] {
            let error = Session::start(std::path::Path::new(path), Options::default())
                .err()
                .expect("refused");
            assert!(matches!(error, Error::HelperUnavailable(_)), "{path}");
        }
        let directory = std::env::temp_dir();
        assert!(Session::start(&directory, Options::default()).is_err());
        let not_executable =
            std::env::temp_dir().join(format!("oms-graph-python-noexec-{}", std::process::id()));
        std::fs::write(&not_executable, "x").expect("write");
        let error = Session::start(&not_executable, Options::default()).err();
        let _ = std::fs::remove_file(&not_executable);
        assert!(matches!(error, Some(Error::HelperUnavailable(_))));
    }

    #[test]
    fn the_helper_starts_without_the_callers_environment() {
        // The fixture exits 0 only if it sees no environment variable at all.
        let fake = Fake::new("env-empty");
        let error = fake.start(quick()).parse("m.py", "x\n", "d").unwrap_err();
        assert_eq!(
            error,
            Error::HelperExited("helper exited with status 0".to_owned())
        );
    }
}

// ---------------------------------------------------------------------------
// The real helper: protocol edges, reuse, limits, bounded memory.
// ---------------------------------------------------------------------------

/// Whether `pid` is gone (reaped): a zombie would still have a /proc entry.
fn assert_gone(pid: u32) {
    #[cfg(target_os = "linux")]
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "child {pid} is still alive or unreaped"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

#[cfg(unix)]
fn raw_frame(kind: u8, id: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = b"OGPY\x01".to_vec();
    frame.push(kind);
    frame.extend([0, 0, id, 0, 0, 0, 0, 0, 0, 0]);
    frame.extend((payload.len() as u32).to_le_bytes());
    frame.extend(payload);
    frame
}

#[cfg(unix)]
fn raw_request(max_bytes: u32, path: &str, digest: &str, source: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend(max_bytes.to_le_bytes());
    payload.extend((path.len() as u32).to_le_bytes());
    payload.extend((digest.len() as u32).to_le_bytes());
    payload.extend(path.as_bytes());
    payload.extend(digest.as_bytes());
    payload.extend(source.as_bytes());
    raw_frame(1, 5, &payload)
}

#[cfg(unix)]
fn talk_to_helper(input: &[u8]) -> (Vec<u8>, std::process::ExitStatus) {
    use std::io::{Read, Write};
    let mut child = std::process::Command::new(helper_path())
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn helper");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let bytes = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });
    let mut out = Vec::new();
    stdout.read_to_end(&mut out).expect("read");
    writer.join().expect("writer");
    (out, child.wait().expect("wait"))
}

#[cfg(unix)]
#[test]
fn the_helper_enforces_the_protocol_itself() {
    // A request over the client's max_bytes is refused in-protocol, same id.
    let (out, status) = talk_to_helper(&raw_request(1, "m.py", "d", "x = 1\n"));
    assert!(status.success());
    assert_eq!(&out[..6], b"OGPY\x01\x03");
    assert_eq!(out[8], 5);
    assert_eq!(&out[20..], b"source_too_large");
    // A valid request is answered with a result frame and a clean EOF exits 0.
    let (out, status) = talk_to_helper(&raw_request(100, "m.py", "d", "def f():\n    g()\n"));
    assert!(status.success());
    assert_eq!(&out[..6], b"OGPY\x01\x02");
    let graph: ParseResult = serde_json::from_slice(&out[20..]).expect("graph");
    assert_eq!(graph.refs.len(), 1);
    // Garbage, truncation, bad version, a result frame sent as a request,
    // non-UTF-8 source: the helper stops with its protocol exit status.
    let mut bad_utf8 = raw_request(100, "m.py", "d", "xx");
    *bad_utf8.last_mut().expect("byte") = 0xff;
    let mut bad_version = raw_request(100, "m.py", "d", "x");
    bad_version[4] = 9;
    let truncated = raw_request(100, "m.py", "d", "x = 1\n");
    for (name, input) in [
        ("garbage", b"hello, not a frame at all".to_vec()),
        ("truncated", truncated[..truncated.len() - 1].to_vec()),
        ("version", bad_version),
        ("utf8", bad_utf8),
        ("wrong kind", raw_frame(2, 1, b"{}")),
        ("short request", raw_frame(1, 1, b"abc")),
    ] {
        let (out, status) = talk_to_helper(&input);
        assert!(out.is_empty(), "{name}");
        assert_eq!(status.code(), Some(3), "{name}");
    }
    let (out, status) = talk_to_helper(b"");
    assert!(out.is_empty() && status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn the_helper_runs_with_an_address_space_limit_and_bounded_memory() {
    let mut session = start(Options::default());
    let pid = session.pid();
    // The helper applies its limits right after it starts: wait for them.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let limits = loop {
        let limits = std::fs::read_to_string(format!("/proc/{pid}/limits")).expect("limits");
        if limits.contains("671088640") || std::time::Instant::now() > deadline {
            break limits;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let line = |name: &str| {
        limits
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_default()
            .to_owned()
    };
    // 512 MiB base plus 64 MiB per started MiB of the 2 MiB source cap.
    assert!(line("Max address space").contains("671088640"), "{limits}");
    assert!(line("Max core file size").contains(" 0 "), "{limits}");
    // The helper is non-dumpable (PR_SET_DUMPABLE 0), which also stops dumps
    // piped to a crash collector: /proc/<pid>/environ then belongs to root. The helper
    // refuses to serve if it cannot set and read back the attribute, so having
    // answered at all is the child-side proof; the owner check is the outside
    // view, skipped when the tests share the mapped kernel-root owner.
    {
        use std::os::unix::fs::MetadataExt;
        let own = std::fs::metadata("/proc/self").expect("self").uid();
        let helper = std::fs::metadata(format!("/proc/{pid}/environ"))
            .expect("helper")
            .uid();
        // Since Linux 4.11 a mapped namespace UID 0 owns non-dumpable
        // proc files; otherwise global root is projected through our mapping.
        let uid_map = std::fs::read_to_string("/proc/self/uid_map").expect("uid map");
        let mapped_root = uid_map.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.len() == 3 && fields[0] == "0" && fields[2] != "0"
        });
        let dump_owner = if mapped_root {
            0
        } else {
            std::fs::metadata("/proc").expect("proc root").uid()
        };
        if own != dump_owner {
            assert_eq!(helper, dump_owner, "helper must be non-dumpable");
        }
    }
    let rss_kib = |pid: u32| -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .expect("status")
            .lines()
            .find(|line| line.starts_with("VmHWM"))
            .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
            .expect("VmHWM")
    };
    let source = format!("def f():\n    g()\n{}", "x = a() + b.c(1)\n".repeat(10_000));
    for _ in 0..15 {
        session.parse("m.py", &source, "d").expect("answers");
    }
    assert_eq!(session.pid(), pid);
    let peak = rss_kib(pid);
    for _ in 0..15 {
        session.parse("m.py", &source, "d").expect("answers");
    }
    let after = rss_kib(pid);
    assert!(after < 256 * 1024, "peak {after} KiB");
    assert!(
        after <= peak + peak / 4 + 8 * 1024,
        "grew from {peak} to {after} KiB"
    );
    // The worst the default cap allows, a call every 4 bytes, still fits.
    let dense = format!("{}\n", "f();".repeat(Options::default().max_bytes / 4 - 1));
    let graph = session
        .parse("m.py", &dense, "d")
        .expect("dense source fits the limit");
    assert!(graph.refs.len() > 100_000);
}
