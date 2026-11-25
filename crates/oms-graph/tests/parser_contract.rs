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
        (
            "x.PY",
            Selection::Python,
            Some(("python", oms_graph_python::VERSION)),
        ),
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

#[test]
fn polyglot_tokens_are_iterative_and_extract_basic_declarations() {
    for (path, source, symbol, imported) in [
        (
            "app.ts",
            "import x from './dep'; export class Widget {}",
            "Widget",
            "./dep",
        ),
        (
            "app.go",
            "package app\nimport (\n \"example/dep\"\n)\ntype Thing struct {}",
            "Thing",
            "example/dep",
        ),
        (
            "app.cpp",
            "#include \"dep.h\"\nnamespace demo {}",
            "demo",
            "dep.h",
        ),
    ] {
        let result = parse(path, source, "d", &mut Fake::default()).unwrap();
        assert!(result.nodes.iter().any(|n| n["name"] == symbol));
        assert!(result.refs.iter().any(|r| r["value"] == imported));
    }
    let source = format!(
        "{}pub fn deep() {{}}{}",
        "{".repeat(100_000),
        "}".repeat(100_000)
    );
    assert!(
        parse("deep.rs", &source, "d", &mut Fake::default())
            .unwrap()
            .nodes
            .iter()
            .any(|n| n["name"] == "deep")
    );
    let named = format!(
        "{}fn bottom() {{}}{}",
        "mod nested {".repeat(1000),
        "}".repeat(1000)
    );
    let graph = parse("named.rs", &named, "d", &mut Fake::default()).unwrap();
    assert_eq!(graph.nodes.len(), 1001); // modules and bottom
    let bottom = graph.nodes.iter().find(|n| n["name"] == "bottom").unwrap();
    assert_eq!(bottom["metadata"]["scope_truncated"], true);
    assert!(bottom["metadata"]["qualname"].as_str().unwrap().len() < 256);
    let long = format!("impl {} {{ fn bounded() {{}} }}", "LongType".repeat(1000));
    let graph = parse("long.rs", &long, "d", &mut Fake::default()).unwrap();
    let implementation = graph.nodes.iter().find(|n| n["kind"] == "symbol").unwrap();
    assert_eq!(implementation["metadata"]["signature_truncated"], true);
    assert_eq!(implementation["metadata"]["scope_truncated"], true);
    assert!(
        implementation["metadata"]["signature"]
            .as_str()
            .unwrap()
            .len()
            < 2060
    );
}

#[test]
fn cargo_paths_and_rust_test_attributes() {
    let cargo = parse("crates/app/Cargo.toml", "[package]\nname = \"app\"\n[dependencies]\ncore = { path = \"../core\" }\nbad = { path = \"../../../outside\" }\n[workspace]\nmembers = [\n \"../core\",\n]\n", "d", &mut Fake::default()).unwrap();
    assert_eq!(cargo.nodes[0]["metadata"]["package"], "app");
    assert!(
        cargo
            .refs
            .iter()
            .any(|r| r["value"] == "crates/core/Cargo.toml")
    );
    assert!(
        cargo
            .refs
            .iter()
            .any(|r| r["value"] == "crates/core/src/lib.rs")
    );
    assert!(
        !cargo
            .refs
            .iter()
            .any(|r| r["value"].as_str().unwrap().contains("outside"))
    );
    let rust = parse("crates/app/src/lib.rs", "mod inner;\nuse crate::inner::thing;\n#[test]\nfn first() {}\n#[tokio::test]\nasync fn second() {}\nfn normal() {}", "d", &mut Fake::default()).unwrap();
    for name in ["first", "second"] {
        assert!(rust.nodes.iter().any(|n| n["name"] == name
            && n["kind"] == "function"
            && n["metadata"]["test"] == true));
    }
    assert!(
        rust.refs
            .iter()
            .any(|r| r["value"] == "crates/app/src/inner.rs")
    );
    assert!(
        rust.nodes
            .iter()
            .any(|n| n["name"] == "normal" && n["metadata"].get("test").is_none())
    );
}

#[test]
fn rust_scopes_preserve_same_named_methods_signatures_and_body_spans() {
    let source = r####"mod first {
    struct Item;
    impl Item {
        pub async fn new<'a>(value: &'a str) -> Self {
            let literal = r###"fn fabricated() { }"###;
            Item
        }
    }
    impl<T> Factory<T> for Item {
        fn new() -> Self { Item }
    }
    trait Maker {
        fn new() -> Self;
    }
}
mod second { fn new() {} }
fn r#type() { let brace = '}'; }
"####;
    let parsed = parse("src/lib.rs", source, "digest", &mut Fake::default()).unwrap();
    let methods: Vec<_> = parsed.nodes.iter().filter(|n| n["name"] == "new").collect();
    assert_eq!(methods.len(), 4);
    let ids: std::collections::BTreeSet<_> =
        methods.iter().map(|n| n["id"].as_str().unwrap()).collect();
    let names: std::collections::BTreeSet<_> = methods
        .iter()
        .map(|n| n["metadata"]["qualname"].as_str().unwrap())
        .collect();
    assert_eq!((ids.len(), names.len()), (4, 4));
    assert_eq!(methods[0]["metadata"]["line"], 4);
    assert_eq!(methods[0]["metadata"]["end_line"], 7);
    assert_eq!(methods[0]["metadata"]["span_complete"], true);
    assert!(
        methods[0]["metadata"]["signature"]
            .as_str()
            .unwrap()
            .contains("async fn new<'a>")
    );
    assert!(!parsed.nodes.iter().any(|n| n["name"] == "fabricated"));
    assert!(parsed.nodes.iter().any(|n| n["name"] == "r#type"));
    let partial = parse(
        "src/partial.rs",
        "impl Broken { fn missing() {",
        "d",
        &mut Fake::default(),
    )
    .unwrap();
    assert!(
        partial
            .nodes
            .iter()
            .any(|n| n["name"] == "missing" && n["metadata"]["span_complete"] == false)
    );
}

fn rust_calls(source: &str) -> Vec<(String, String, u64)> {
    let parsed = parse("src/lib.rs", source, "digest", &mut Fake::default()).unwrap();
    parsed
        .edges
        .iter()
        .filter(|e| e["relation"] == "calls")
        .map(|e| {
            assert_eq!(e["confidence"], "INFERRED");
            assert_eq!(e["evidence"]["path"], "src/lib.rs");
            assert_eq!(e["evidence"]["source_digest"], "digest");
            let name = |key: &str| {
                let id = e[key].as_str().unwrap();
                assert!(parsed.nodes.iter().any(|n| n["id"] == id), "{id}");
                id.strip_prefix("symbol:src/lib.rs::function:")
                    .unwrap()
                    .to_owned()
            };
            (
                name("source"),
                name("target"),
                e["evidence"]["line"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn rust_root_forwarding_calls_require_positive_scope_and_uniqueness() {
    let chain = r#"#![forbid(unsafe_code)]
use std::path::Path;
pub fn run(
    tool: &str,
    cwd: &Path,
) -> (i32, Vec<u8>) {
    run_with_source(tool, cwd, cwd)
}
#[must_use]
pub(crate) fn run_with_source(tool: &str, cwd: &Path, source: &Path) -> (i32, Vec<u8>) {
    return run_with_services(
        tool, cwd, source, &Unavailable,
    );
}
pub fn run_with_services(tool: &str, cwd: &Path, source: &Path, s: &dyn Services) -> (i32, Vec<u8>) {
    (0, Vec::new())
}
trait Services {}
struct Unavailable;
impl Services for Unavailable {}
"#;
    let edge = |from: &str, to: &str, line: u64| (from.to_owned(), to.to_owned(), line);
    assert_eq!(
        rust_calls(chain),
        vec![
            edge("run", "run_with_source", 7),
            edge("run_with_source", "run_with_services", 11),
        ]
    );
    // Prove the shared negative fixture shape is accepted before varying it.
    assert_eq!(
        rust_calls(
            "fn a(mut x: u8) { b(x, &mut x, 1, \"s\"); }\nfn b(_: u8, _: &mut u8, _: u8, _: &str) {}"
        ),
        vec![edge("a", "b", 1)]
    );
    for declaration in [
        "struct K { field: u8 }",
        "type K = u8;",
        "trait K {}",
        "enum K { V }",
        "use other::K as Other; mod other { pub struct K; }",
        "mod inner { const K: u8 = 0; }",
    ] {
        let source = format!("fn run() {{ K() }} fn K() {{}} {declaration}");
        assert_eq!(rust_calls(&source), vec![edge("run", "K", 1)], "{source}");
    }
    for source in [
        // Opaque callees.
        "fn a(x: X) { x.b() }\nfn b() {}",
        "fn a() { util::b() }\nfn b() {}",
        "fn a() { ::b() }\nfn b() {}",
        "fn a() { b::<u8>() }\nfn b() {}",
        "fn a() { b!() }\nfn b() {}",
        "fn a() { r#b() }\nfn b() {}",
        "fn a() { b() }\nfn r#b() {}",
        "fn a() { a() }",
        // Shadowing, or a header that can hide a binding.
        "fn a(b: fn()) { b() }\nfn b() {}",
        "fn run(K: fn()) { K() } fn K() {}",
        "fn run() { pick() } struct pick(u8); fn pick() {}",
        "fn run() { pick() } struct pick; fn pick() {}",
        "fn run() { pick() } const r#pick: fn() = run; fn pick() {}",
        "fn run() { pick() } static mut pick: fn() = run; fn pick() {}",
        "fn run() { pick() } use other::pick; fn pick() {}",
        "fn run() { pick() } use other::{self, inner::{callback as r#pick}}; fn pick() {}",
        "fn run() { K() } fn K() {} const K: fn() = run;",
        "fn run() { K() } fn K() {} use other::callback as K;",
        "fn a((x, b): (u8, fn())) { b() }\nfn b() {}",
        "fn a(bind!(): u8) { b() }\nfn b() {}",
        "fn a<const N: usize>() { b() }\nfn b() {}",
        "fn a() -> Foo<{ 1 }> { b() }\nfn b() {}",
        // Macro token trees and nested scopes.
        "macro_rules! m { () => { fn a() { b() } } }\nfn b() {}",
        "m! { fn b() {} }\nfn a() { b() }",
        "fn a() { b() }\nmod inner { fn b() {} }",
        "fn a() { b() }\nmod inner { pub fn c() {} }",
        "mod inner { fn a() { b() } }\nfn b() {}",
        "impl T { fn b() {} }\nfn a() { b() }",
        "extern \"C\" { fn b(); }\nfn a() { b() }",
        // Conditional or rewriting inner attributes still apply beyond later items.
        "#![cfg(any())] use std::io; fn a() { b() } fn b() {}",
        "#![forbid(unsafe_code)] #![cfg_attr(test, something)] fn a() { b() } fn b() {}",
        "#![rewrite] fn a() { b() } fn b() {}",
        // Conditional or duplicate declarations.
        "fn a() { b() }\n#[cfg(unix)]\nfn b() {}\n#[cfg(not(unix))]\nfn b() {}",
        "fn a() { b() }\n#[cfg(unix)]\nfn b() {}",
        "fn a() { b() }\n#[cfg(unix)]\n#[inline]\npub(crate) fn b() {}",
        "#[cfg_attr(unix, inline)]\nfn a() { b() }\nfn b() {}",
        "#[test]\nfn a() { b() }\nfn b() {}",
        "#[some::rewrite]\nfn a() { b() }\nfn b() {}",
        "fn a() { b() }\nfn b() {}\nfn b() {}",
        // Anything beyond one call with simple arguments.
        "fn a() { let x = 1; b() }\nfn b() {}",
        "fn a() { b(); b() }\nfn b() {}",
        "fn a() { b(c()) }\nfn b() {}",
        "fn a() { b(x.y) }\nfn b() {}",
        "fn a() { b(return) }\nfn b() {}",
        "fn a() { b(&&x) }\nfn b() {}",
        "fn a() { b(x y) }\nfn b() {}",
        "fn a() { b(,) }\nfn b() {}",
        "fn a() { b()? }\nfn b() {}",
        "fn a() { b().await }\nfn b() {}",
        "fn a() { unsafe { b() } }\nfn b() {}",
        // Incomplete or unbalanced files, and names with no local declaration.
        "fn a() { b() }\nfn b() {",
        "fn a() { b() }\nfn b() {}\n)",
        "fn a() { b() }\nfn b();",
        "use other::b;\nfn a() { b() }",
    ] {
        assert!(rust_calls(source).is_empty(), "{source}");
    }
    let long = format!("fn a() {{ b({}) }}\nfn b() {{}}", "x,".repeat(100_000));
    assert!(rust_calls(&long).is_empty());
    let header = format!("fn a({}) {{ b() }}\nfn b() {{}}", "x: u8,".repeat(100_000));
    assert!(rust_calls(&header).is_empty());
}
