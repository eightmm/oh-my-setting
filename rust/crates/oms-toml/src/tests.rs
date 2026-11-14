use super::*;

#[test]
fn consumer_documents() {
    let config = parse_str(
        r#"
model = "example"
[profiles.local]
active = true
[servers."local.agent"]
command = 'worker'
args = ["--quiet", "--local",]
"#,
    )
    .unwrap();
    assert_eq!(config["model"].as_str(), Some("example"));
    assert_eq!(
        config["profiles"]
            .get("local")
            .unwrap()
            .get("active")
            .and_then(Value::as_bool),
        Some(true)
    );
    let pyproject = parse_str(
        r#"
[project]
name = "sample"
dependencies = [
  "example>=1", # multiline dependency list
]
[project.optional-dependencies]
dev = ["checker"]
[tool.example]
options = { a.b = 1, a.c = 2, empty = {} }
"#,
    )
    .unwrap();
    assert_eq!(
        pyproject["project"].get("name").and_then(Value::as_str),
        Some("sample")
    );
    assert_eq!(
        pyproject["project"]
            .get("dependencies")
            .and_then(Value::as_array)
            .unwrap()
            .len(),
        1
    );
    let cargo = parse_str(
        r#"
[package]
name = "sample"
version = "0.1.0"
[dependencies]
example = { version = "1", features = ["std"] }
[[bin]]
name = "first"
[[bin]]
name = "second"
"#,
    )
    .unwrap();
    assert_eq!(cargo["bin"].as_array().unwrap().len(), 2);
    assert!(cargo["dependencies"].as_table().is_some());
}

#[test]
fn keys_and_table_ownership() {
    let parsed = parse_str("a.b.\"c\" = 1\na.b.d = 2\n\"\" = 3\n''.'x.y' = 4\n");
    // The two spellings of the empty root key conflict.
    assert!(parsed.is_err());
    let parsed = parse_str("a.b.\"c\" = 1\na.b.d = 2\n''.'x.y' = 4\n").unwrap();
    assert_eq!(
        parsed["a"]
            .get("b")
            .unwrap()
            .get("c")
            .and_then(Value::as_integer),
        Some(1)
    );
    assert_eq!(parsed[""].get("x.y").and_then(Value::as_integer), Some(4));
    for text in [
        "[a.b]\nx=1\n[a]\ny=2",
        "a.b=1\n[a.c]\nx=2",
        "[a.b.c]\nx=1\n[a]\nb.d=2",
        "a.b=1\na.c=2",
        "[[a]]\n[a.b]\nx=1\n[[a]]\n[a.b]\nx=2",
        "[[a.b]]\nx=1\n[a]\ny=2",
        "a={b.c=1,b.d=2}",
        "[a.'']\n'1-2_A'=0",
    ] {
        assert!(parse_str(text).is_ok(), "{text:?}");
    }
    for text in [
        "a=1\na=2",
        "[a]\n[a]",
        "a.b=1\n[a]",
        "a.b.c=1\n[a.b]",
        "[a]\nb=1\n[a.b]",
        "a=[]\n[[a]]",
        "a={}\n[a.b]",
        "a={b=1}\na.c=2",
        "a={b={}}\na.b.c=2",
        "a={b={},b.c=1}",
        "a={b=[],b.c=1}",
        "a={b=1,b=2}",
        "a={b.c=1,b=2}",
        "[[a]]\n[a]",
        "[a]\n[[a]]",
        "[[a]]\na=1\n[a.a.b]",
        "[a.b]\nx=1\n[a]\nb.c=2",
        "a. = 1",
        ".a=1",
        "a..b=1",
        "é=1",
        "[a]junk",
        "[[a]",
        "a={b=1,}",
        "a={\nb=1}",
        "a={b=1,\nc=2}",
        "a={b=1 # comment\n}",
    ] {
        assert!(parse_str(text).is_err(), "{text:?}");
    }
}

#[test]
fn array_of_tables_instances_remain_independent() {
    let parsed = parse_str(
        r#"
[[fruit]]
name = "apple"
[fruit.physical]
color = "red"
[[fruit.variety]]
name = "gala"
[[fruit]]
name = "banana"
[[fruit.variety]]
name = "plantain"
"#,
    )
    .unwrap();
    let fruit = parsed["fruit"].as_array().unwrap();
    assert_eq!(
        fruit[0]
            .get("physical")
            .unwrap()
            .get("color")
            .and_then(Value::as_str),
        Some("red")
    );
    assert!(fruit[1].get("physical").is_none());
    let first = fruit[0].get("variety").unwrap().as_array().unwrap();
    let second = fruit[1].get("variety").unwrap().as_array().unwrap();
    assert_eq!(first[0].get("name").and_then(Value::as_str), Some("gala"));
    assert_eq!(
        second[0].get("name").and_then(Value::as_str),
        Some("plantain")
    );
}

#[test]
fn strings_and_line_endings() {
    let parsed = parse_str(concat!(
        "a = \"\\b\\t\\n\\f\\r\\\"\\\\\\u0041\\U0001F600\"\n",
        "b = '''\r\nliteral \\ text\r\n'''\n",
        "c = \"\"\"\nhello \\ \t\r\n  \n world\"\"\"\"\"\n",
        "d = '''one''two'''''\n",
        "e = \"\"\"a\"b\"\"c\"\"\"\n",
        "f = '\t'\n",
        "g = \"\"\"\\\n   continued\"\"\"",
    ))
    .unwrap();
    assert_eq!(parsed["a"].as_str(), Some("\u{8}\t\n\u{c}\r\"\\A😀"));
    assert_eq!(parsed["b"].as_str(), Some("literal \\ text\n"));
    assert_eq!(parsed["c"].as_str(), Some("hello world\"\""));
    assert_eq!(parsed["d"].as_str(), Some("one''two''"));
    assert_eq!(parsed["e"].as_str(), Some("a\"b\"\"c"));
    assert_eq!(parsed["g"].as_str(), Some("continued"));
    for text in [
        "a=\"\\x41\"",
        "a=\"\\uD800\"",
        "a=\"\\U00110000\"",
        "a=\"\\u12\"",
        "a=\"line\nline\"",
        "a='line\nline'",
        "a='unterminated",
        "a=\"unterminated",
        "a=\"\"\"unterminated",
        "a='''unterminated",
        "a=\"\u{7f}\"",
        "a='\r'",
        "a=\"\"\"abc\\  x\"\"\"",
        "a=\"\"\"abc\\  \"\"\"",
        "a=\"\\\n\"",
        "\"\"\"key\"\"\"=1",
        "a='''x''''''",
        "a=\"\"\"x\"\"\"\"\"\"",
        "a='\u{0}'",
        "#bad\u{1}",
        "#bad\u{7f}",
        "a=\"\u{b}\"",
    ] {
        assert!(parse_str(text).is_err(), "{text:?}");
    }
    assert!(parse_str("# unicode 😀\r\na='é'\r\n").is_ok());
    assert_eq!(
        parse_str("a=\"\"\"\r\n\r\nx\"\"\"").unwrap()["a"].as_str(),
        Some("\nx")
    );
}

#[test]
fn numeric_grammar_and_ranges() {
    for (text, expected) in [
        ("0", 0),
        ("+0", 0),
        ("-0", 0),
        ("1_234", 1234),
        ("-9_876", -9876),
        ("0x7fff_ffff_ffff_ffff", i64::MAX),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
        ("0b1_0", 2),
        ("0o7_7", 63),
    ] {
        assert_eq!(
            parse_str(&format!("n={text}")).unwrap()["n"].as_integer(),
            Some(expected)
        );
    }
    for (text, expected) in [
        ("0.0", 0.0),
        ("-0.0", -0.0),
        ("+1.0", 1.0),
        ("1e2", 100.0),
        ("1_0.2_5e+0_2", 1025.0),
        ("1E-2", 0.01),
        ("1e999", f64::INFINITY),
        ("+inf", f64::INFINITY),
        ("-inf", f64::NEG_INFINITY),
    ] {
        assert_eq!(
            parse_str(&format!("n={text}")).unwrap()["n"].as_float(),
            Some(expected)
        );
    }
    for text in ["nan", "+nan", "-nan"] {
        assert!(
            parse_str(&format!("n={text}")).unwrap()["n"]
                .as_float()
                .unwrap()
                .is_nan()
        );
    }
    for text in [
        "",
        "00",
        "01",
        "+01",
        "-01",
        "0_0",
        "_1",
        "1_",
        "1__0",
        "+_1",
        "0x",
        "0Xff",
        "-0xff",
        "+0b1",
        "0x_1",
        "0b2",
        "0o8",
        "0xff_",
        "9223372036854775808",
        "-9223372036854775809",
        "0x8000000000000000",
        "1.",
        ".1",
        "01.0",
        "00e1",
        "1e",
        "1e+",
        "1_e2",
        "1e_2",
        "1e2_",
        "1e2e3",
        "1._0",
        "1_.0",
        "1.0_",
        "True",
        "falsehood",
        "NaN",
        "Infinity",
    ] {
        assert!(parse_str(&format!("n={text}")).is_err(), "{text:?}");
    }
}

#[test]
fn datetime_validation_and_normalization() {
    for (text, expected) in [
        ("2000-02-29", "2000-02-29"),
        ("0001-01-01", "0001-01-01"),
        ("9999-12-31", "9999-12-31"),
        ("00:00:00", "00:00:00"),
        ("23:59:59.123456789", "23:59:59.123456"),
        ("12:34:56.1", "12:34:56.100000"),
        ("12:34:56.0000009", "12:34:56"),
        ("1979-05-27t07:32:00z", "1979-05-27T07:32:00+00:00"),
        (
            "1979-05-27 07:32:00.12-07:00",
            "1979-05-27T07:32:00.120000-07:00",
        ),
        ("1979-05-27T07:32:00-00:00", "1979-05-27T07:32:00+00:00"),
        ("1979-05-27T07:32:00+23:59", "1979-05-27T07:32:00+23:59"),
        ("1979-05-27T07:32:00", "1979-05-27T07:32:00"),
    ] {
        assert_eq!(
            parse_str(&format!("d={text}")).unwrap()["d"],
            Value::Datetime(expected.into())
        );
    }
    for text in [
        "0000-01-01",
        "1900-02-29",
        "2001-02-29",
        "2000-00-01",
        "2000-13-01",
        "2000-01-00",
        "2000-04-31",
        "2000-01-32",
        "24:00:00",
        "00:60:00",
        "00:00:60",
        "12:34",
        "1:23:45",
        "12:34:56.",
        "12:34:56Z",
        "12:34:56+01:00",
        "2000-01-01T00:00:00+24:00",
        "2000-01-01T00:00:00+00:60",
        "2000-01-01T00:00:00+0000",
        "2000-01-01T00:00:00Zx",
        "2000-01-01  00:00:00",
        "2000-01-01T",
        "2000-01-01T00:00:00._1",
    ] {
        assert!(parse_str(&format!("d={text}")).is_err(), "{text:?}");
    }
}

#[test]
fn arrays_and_inline_values() {
    let parsed = parse_str(
        "a=[\n # comment\n 1, true, 'x', [], {}, [1,],\n]\nb={x=[\n1,\n],y='''\na\nb'''}",
    )
    .unwrap();
    assert_eq!(parsed["a"].as_array().unwrap().len(), 6);
    for text in [
        "a=[", "a=[,]", "a=[1 2]", "a=[1,,2]", "a={", "a={,}", "a=[1}\n", "a={x=1]", "a=1 b=2",
    ] {
        assert!(parse_str(text).is_err(), "{text:?}");
    }
    assert!(parse_str("a=[]\nb={}\nc=''\nd=\"\"").is_ok());
}

#[test]
fn limits_utf8_and_error_positions() {
    assert!(parse_bounded(b"", 0).unwrap().is_empty());
    assert!(parse_bounded(b" ", 0).is_err());
    assert!(parse_bounded(b"x=0", 3).is_ok());
    assert!(parse_bounded(b"x=0", 2).is_err());
    assert!(parse_bounded(&[0xff], 1).is_err());
    assert!(parse_bytes(&[0xc3]).is_err());
    assert_eq!(parse_bytes(b"#\r\n\xff").unwrap_err().line, 2);
    let error = parse_str("a='é'\r\nb=\"\\q\"").unwrap_err();
    assert_eq!((error.line, error.column), (2, 5));
    assert_eq!(parse_str("é=0").unwrap_err().column, 1);
    assert_eq!(parse_str("'é'=0 x").unwrap_err().column, 7);
    let error = parse_bytes(&[b'#', b' ', 0xc3, 0xa9, 0xff]).unwrap_err();
    assert_eq!((error.line, error.column), (1, 4));
    assert!(error.to_string().contains("line 1, column 4"));
    let nested = |depth: usize| format!("x={}0{}", "[".repeat(depth), "]".repeat(depth));
    assert!(parse_str(&nested(0)).is_ok());
    assert!(parse_str(&nested(MAX_DEPTH - 1)).is_ok());
    assert!(parse_str(&nested(MAX_DEPTH)).is_err());
    assert!(parse_str(&format!("x={}", "[".repeat(MAX_DEPTH * 10))).is_err());
    for depth in [MAX_DEPTH, MAX_DEPTH + 1] {
        let key = vec!["a"; depth].join(".");
        assert_eq!(parse_str(&format!("{key}=0")).is_ok(), depth == MAX_DEPTH);
        assert_eq!(parse_str(&format!("[{key}]")).is_ok(), depth == MAX_DEPTH);
    }
    let inline = |depth: usize| format!("x={}0{}", "{x=".repeat(depth), "}".repeat(depth));
    assert!(parse_str(&inline(MAX_DEPTH - 1)).is_ok());
    assert!(parse_str(&inline(MAX_DEPTH)).is_err());
    assert!(parse_str("\u{feff}a=0").is_err());
    assert!(parse_str("a=0\rb=1").is_err());
}
