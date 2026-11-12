use oms_graph::{Error, Selection, parse, select};
use oms_graph_python::{Error as PythonError, ParseResult, Parser};
use serde_json::json;

#[derive(Default)]
struct Fake {
    calls: Vec<(String, String, String)>,
    fail: bool,
}
impl Parser for Fake {
    fn parse(&mut self, path: &str, text: &str, digest: &str) -> Result<ParseResult, PythonError> {
        self.calls.push((path.into(), text.into(), digest.into()));
        if self.fail {
            Err(PythonError::Poisoned)
        } else {
            Ok(ParseResult::default())
        }
    }
}

#[test]
fn selection_and_config() {
    for (name, selection, version) in [
        ("x.PY", Selection::Python, Some(("python", 6))),
        ("x.BASH", Selection::Shell, Some(("shell", 8))),
        ("x.sh", Selection::Shell, Some(("shell", 8))),
        ("x.MARKDOWN", Selection::Markdown, Some(("markdown", 2))),
        (
            "x.mar\u{212a}down",
            Selection::Markdown,
            Some(("markdown", 2)),
        ),
        (
            "x.MAR\u{212a}DOWN",
            Selection::Markdown,
            Some(("markdown", 2)),
        ),
        ("x.md", Selection::Markdown, Some(("markdown", 2))),
        ("x.JSON", Selection::Config, Some(("config", 1))),
        ("x.yaml", Selection::Config, Some(("config", 1))),
        ("x.yml", Selection::Config, Some(("config", 1))),
        ("x.toml", Selection::Config, Some(("config", 1))),
        ("x.ipynb", Selection::UnsupportedNotebook, None),
        ("x.pyi", Selection::Unknown, None),
        ("x.zsh", Selection::Unknown, None),
        ("x.fish", Selection::Unknown, None),
        ("", Selection::Unknown, None),
    ] {
        assert_eq!(select(name), selection);
        assert_eq!(selection.language_version(), version);
    }
    let result = parse("/a\\b.json", "malformed", "d", &mut Fake::default()).unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({"nodes":[{"id":"config:a/b.json","kind":"config","name":"a\\b.json","path":"/a/b.json","language":"config","source_digest":"d","summary":null,"crux":null,"metadata":{}}],"edges":[],"refs":[]})
    );
}

#[test]
fn markdown_contract() {
    let mut fake = Fake::default();
    let result = parse(
        "doc.md",
        "# title\n\nFirst line\n# skip\nsecond sentence. More\n\nignored",
        "d",
        &mut fake,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({"nodes":[{"id":"document:doc.md","kind":"document","name":"doc.md","path":"doc.md","language":"markdown","source_digest":"d","summary":"First line second sentence.","crux":null,"metadata":{}}],"edges":[],"refs":[]})
    );
    for text in ["", "# heading\n```\n~~~\n---\n<!-- comment\n"] {
        assert_eq!(
            parse("a.md", text, "", &mut fake).unwrap().nodes[0]["summary"],
            json!(null)
        );
    }
    let sensitive = ["pass", "word", "=x"].concat();
    assert_eq!(
        parse("a.md", &sensitive, "", &mut fake).unwrap().nodes[0]["summary"],
        json!(null)
    );
    let long = "é".repeat(241);
    assert_eq!(
        parse("a.md", &long, "", &mut fake).unwrap().nodes[0]["summary"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        240
    );
}

#[test]
fn line_splits_and_paths() {
    let separators = [
        "\n", "\r\n", "\r", "\u{b}", "\u{c}", "\u{85}", "\u{1c}", "\u{1d}", "\u{1e}", "\u{2028}",
        "\u{2029}",
    ];
    for sep in separators {
        let text = format!("# a{sep}a/b.{sep}a/b{sep}");
        let result = parse("a.md", &text, "", &mut Fake::default()).unwrap();
        assert_eq!(
            result
                .refs
                .iter()
                .map(|r| (r["value"].as_str().unwrap(), r["line"].as_u64().unwrap()))
                .collect::<Vec<_>>(),
            vec![("a/b", 2), ("a/b", 3)]
        );
    }
    let text = "x/a /a/b .a/b $a/b \\a/b -a/b éa/b (a/b), a/b.; a/b";
    let refs = parse("a.md", text, "", &mut Fake::default()).unwrap().refs;
    assert_eq!(
        refs.iter()
            .map(|r| r["value"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["x/a", "a/b", "a/b", "a/b", "a/b"]
    );
}

#[test]
fn shared_parser_and_errors() {
    let mut fake = Fake::default();
    parse("x.py", "x", "d", &mut fake).unwrap();
    parse("x.sh", "python3 - <<EOF\nprint(1)\nEOF\n", "e", &mut fake).unwrap();
    assert_eq!(
        fake.calls,
        vec![
            ("x.py".into(), "x".into(), "d".into()),
            ("x.sh".into(), "print(1)".into(), "e".into())
        ]
    );
    fake.fail = true;
    assert!(matches!(
        parse("x.py", "", "", &mut fake),
        Err(Error::Python(PythonError::Poisoned))
    ));
    assert!(matches!(
        parse("x.sh", "python3 - <<EOF\nx\nEOF\n", "", &mut fake),
        Err(Error::Python(PythonError::Poisoned))
    ));
    assert!(matches!(
        parse("x.ipynb", "", "", &mut fake),
        Err(Error::Unsupported(Selection::UnsupportedNotebook))
    ));
}

#[test]
fn canonical_reference_corpus() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("data/reference.json")).unwrap();
    for case in corpus.as_array().unwrap() {
        let actual = parse(
            case["path"].as_str().unwrap(),
            case["text"].as_str().unwrap(),
            case["digest"].as_str().unwrap(),
            &mut Fake::default(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            case["expected"],
            "case: {:?}",
            case["path"]
        );
    }
}
