use oms_context::{Json, parse_json};
use oms_experiment::{Error, template, validate};

const BASE: &str = r#"{"schema":2,"question":" q ","hypothesis":"h","prediction":"p","baseline":{"x":1},"treatment":{"independent_change":"x"},"controlled_variables":{},"metrics":{"primary":{"name":"score","direction":"maximize"}},"seeds":[0,1,2],"success":{"min_improvement":0}}"#;
fn parse(s: &str) -> Json {
    parse_json(s.as_bytes()).unwrap()
}
fn with(key: &str, replacement: &str) -> Json {
    let mut v = parse(BASE);
    if let Json::Object(ref mut fields) = v {
        if let Some((_, value)) = fields.iter_mut().find(|(k, _)| k == key.as_bytes()) {
            *value = parse(replacement);
        } else {
            fields.push((key.as_bytes().to_vec(), parse(replacement)));
        }
    }
    v
}
fn error(key: &str, replacement: &str, expected: &str) {
    let e = match validate(&with(key, replacement)) {
        Ok(_) => panic!("unexpected success"),
        Err(e) => e,
    };
    assert_eq!(e, Error::Invalid(expected.into()));
}
#[test]
fn reference_contract() {
    let report = validate(&parse(BASE)).unwrap();
    assert_eq!(
        report.digest,
        "f88f9de258cd09f5a62804357be7b6a2033298cf9296bd99518dc93a17d6f671"
    );
    assert_eq!(
        String::from_utf8(report.source_bytes).unwrap(),
        r#"{"baseline":{"x":1},"controlled_variables":{},"hypothesis":"h","invariant_pack":[],"metrics":{"primary":{"direction":"maximize","name":"score"},"secondary":[]},"prediction":"p","question":" q ","schema":2,"seeds":[0,1,2],"success":{"min_improvement":0.0,"no_regression":{}},"treatment":{"independent_change":"x"}}"#
    );
    assert_eq!(template(), parse(include_str!("data/template.json")));
}
#[test]
fn invalid_branches() {
    for (k, v, e) in [
        ("schema", "1", "experiment contract schema must be 2"),
        (
            "question",
            "\"  \"",
            "experiment question must be one line of 1-2000 characters",
        ),
        (
            "baseline",
            "{}",
            "experiment baseline must be a nonempty object",
        ),
        (
            "treatment",
            "{}",
            "experiment treatment must be a nonempty object",
        ),
        (
            "controlled_variables",
            "null",
            "experiment controlled_variables must be an object",
        ),
        (
            "metrics",
            "{}",
            "experiment metrics.primary must be an object",
        ),
        (
            "metrics",
            r#"{"primary":{"name":"a","direction":"observe"}}"#,
            "metrics.primary.direction must be maximize or minimize",
        ),
        (
            "metrics",
            r#"{"primary":{"name":"a","direction":"minimize"},"secondary":["a"]}"#,
            "metric is declared more than once: a",
        ),
        ("seeds", "[]", "experiment seeds must be a nonempty list"),
        (
            "seeds",
            "[1,1,true]",
            "experiment seeds must contain integers",
        ),
        ("seeds", "[1,1]", "experiment seeds must be unique"),
        (
            "success",
            "{}",
            "experiment success must be a nonempty object",
        ),
        (
            "success",
            r#"{"min_improvement":true}"#,
            "success.min_improvement must be finite numeric",
        ),
        (
            "success",
            r#"{"no_regression":{"x":1}}"#,
            "no-regression metric is not declared: x",
        ),
        (
            "success",
            r#"{"noise_band":{"mode":"fixed","delta":-1}}"#,
            "success.noise_band.delta must be finite and non-negative",
        ),
        (
            "invariant_pack",
            r#"[{"name":"a","command":[]}]"#,
            "invariant_pack[0] must have name and nonempty command array",
        ),
        ("budget", "{}", "budget must be a nonempty object"),
        (
            "budget",
            r#"{"runs":0}"#,
            "budget.runs must be a positive integer",
        ),
    ] {
        error(k, v, e);
    }
}
#[test]
fn exceptions_and_preservation() {
    let e = match validate(&with("success", r#"{"noise_band":{"mode":[]}}"#)) {
        Err(e) => e,
        Ok(_) => panic!(),
    };
    assert_eq!(e, Error::Type("unhashable type: 'list'".into()));
    let huge = format!("{{\"min_improvement\":1{}}}", "0".repeat(400));
    let e = match validate(&with("success", &huge)) {
        Err(e) => e,
        Ok(_) => panic!(),
    };
    assert_eq!(
        e,
        Error::Overflow("int too large to convert to float".into())
    );
    let report = validate(&with("extra", r#"{"$serde_json::private::Number":"x"}"#)).unwrap();
    assert!(
        String::from_utf8(report.canonical_bytes().unwrap())
            .unwrap()
            .contains("$serde_json::private::Number")
    );
    let e = match validate(&with("extra", r#""\ud800""#)) {
        Err(e) => e,
        Ok(_) => panic!(),
    };
    assert_eq!(e, Error::Encoding(oms_json::Error::LoneSurrogate));
}

#[test]
fn differential_reference_corpus() {
    let mut count = 0;
    for line in include_str!("data/reference.tsv").lines() {
        let fields: Vec<_> = line.splitn(4, '\t').collect();
        let result = validate(&parse(fields[0]));
        if fields[1] == "ok" {
            let report = result.unwrap();
            assert_eq!(report.source_bytes, fields[2].as_bytes());
            assert_eq!(report.canonical_bytes().unwrap(), fields[3].as_bytes());
        } else {
            let err = match result {
                Ok(_) => panic!("unexpected success: {}", fields[0]),
                Err(e) => e,
            };
            let expected = match fields[1] {
                "CoreError" => Error::Invalid(fields[2].into()),
                "TypeError" => Error::Type(fields[2].into()),
                "OverflowError" => Error::Overflow(fields[2].into()),
                other => panic!("unexpected reference class: {other}"),
            };
            assert_eq!(err, expected, "input: {}", fields[0]);
        }
        count += 1;
    }
    assert_eq!(count, 102);
}

#[test]
fn repeated_and_parallel_results() {
    let expected = validate(&parse(BASE)).unwrap().canonical_bytes().unwrap();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let expected = &expected;
            scope.spawn(move || {
                for _ in 0..10 {
                    assert_eq!(
                        &validate(&parse(BASE)).unwrap().canonical_bytes().unwrap(),
                        expected
                    );
                }
            });
        }
    });
    error("seeds", "[-0,0]", "experiment seeds must be unique");
    assert!(validate(&with("seeds", "[-0]")).is_ok());
}

#[test]
fn malformed_typed_string_is_an_error() {
    let invalid = Json::String(vec![0xed]);
    assert!(matches!(
        validate(&invalid),
        Err(Error::Encoding(oms_json::Error::InvalidCodepoint))
    ));
}

#[test]
fn rejected_surrogate_key_is_lossless() {
    let err = match validate(&with("budget", r#"{"\ud800":1}"#)) {
        Err(e) => e,
        Ok(_) => panic!(),
    };
    let mut expected = b"budget has unknown key: ".to_vec();
    expected.extend_from_slice(&[0xed, 0xa0, 0x80]);
    assert_eq!(err, Error::InvalidWtf8(expected));
    assert!(!err.to_string().is_empty());
}

#[test]
fn derived_fields_are_removed_from_hash() {
    let plain = validate(&parse(BASE)).unwrap();
    let mut raw = parse(BASE);
    if let Json::Object(ref mut fields) = raw {
        for key in [
            "contract_digest",
            "experiment_id",
            "registered_at",
            "primary_metric",
            "metric_specs",
        ] {
            fields.push((key.as_bytes().to_vec(), Json::String(b"stale".to_vec())));
        }
    }
    let derived = validate(&raw).unwrap();
    assert_eq!(plain.source_bytes, derived.source_bytes);
    assert_eq!(plain.digest, derived.digest);
    assert_eq!(
        plain.canonical_bytes().unwrap(),
        derived.canonical_bytes().unwrap()
    );
}

#[test]
fn deep_input_is_process_safe() {
    const MARKER: &str = "OMS_EXPERIMENT_DEEP_CHILD";
    if std::env::var_os(MARKER).is_some() {
        let nested = format!("{}0{}", "[".repeat(1020), "]".repeat(1020));
        let raw = format!("{},\"extra\":{nested}}}", &BASE[..BASE.len() - 1]);
        let parsed = parse(&raw);
        assert!(validate(&parsed).is_ok());
        let mut deep = Json::Null;
        for _ in 0..20_000 {
            deep = Json::Array(vec![deep]);
        }
        let mut raw = parse(BASE);
        if let Json::Object(ref mut fields) = raw {
            fields.push((b"extra".to_vec(), deep));
        }
        assert!(matches!(
            validate(&raw),
            Err(Error::Encoding(oms_json::Error::DepthLimit))
        ));
    } else {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "deep_input_is_process_safe"])
            .env(MARKER, "1")
            .status()
            .unwrap();
        assert!(status.success(), "deep validation child failed: {status}");
    }
}

#[test]
fn typed_input_invariants() {
    let mut raw = parse(BASE);
    if let Json::Object(ref mut fields) = raw {
        fields.push((b"question".to_vec(), Json::String(b"override".to_vec())));
        fields.push((b"seeds".to_vec(), parse("[-0]")));
        fields.push((
            b"extra".to_vec(),
            Json::Object(vec![
                (b"x".to_vec(), parse("1")),
                (b"x".to_vec(), parse("2")),
            ]),
        ));
    }
    let mut report = validate(&raw).unwrap();
    let text = String::from_utf8(report.canonical_bytes().unwrap()).unwrap();
    assert!(text.contains("\"question\":\"override\""));
    assert!(text.contains("\"seeds\":[0]"));
    assert!(text.contains("\"extra\":{\"x\":2}"));
    if let Json::Object(ref fields) = report.normalized {
        let seeds = &fields.iter().find(|(k, _)| k == b"seeds").unwrap().1;
        assert_eq!(seeds, &parse("[0]"));
    } else {
        panic!();
    }
    report.normalized = Json::String(vec![0xed]);
    assert!(matches!(
        report.canonical_bytes(),
        Err(Error::Encoding(oms_json::Error::InvalidCodepoint))
    ));
    for spelling in ["inf", "+2.0", ".5", "01", "bad"] {
        let mut raw = parse(BASE);
        if let Json::Object(ref mut fields) = raw {
            fields.push((b"registered_at".to_vec(), Json::Number(spelling.into())));
        }
        assert!(validate(&raw).is_err(), "accepted {spelling}");
    }
}

#[test]
fn duplicate_fields_use_last_value_at_every_level() {
    let expected = r#"{"schema":2,"question":"q","hypothesis":"h","prediction":"p","baseline":{"x":1},"treatment":{"independent_change":"x"},"controlled_variables":{},"metrics":{"primary":{"name":"score","direction":"maximize"},"secondary":[]},"seeds":[1],"success":{"min_improvement":0,"no_regression":{"score":{"direction":"maximize","max_regression":1}}},"budget":{"runs":1},"invariant_pack":[{"name":"check","command":["true"],"required":true}],"extra":{"x":2}}"#;
    fn prepend(object: &mut Json, key: &str, bad: &str) {
        if let Json::Object(fields) = object {
            fields.insert(0, (key.as_bytes().to_vec(), parse(bad)));
        } else {
            panic!()
        }
    }
    fn field<'a>(object: &'a mut Json, key: &str) -> &'a mut Json {
        if let Json::Object(fields) = object {
            &mut fields
                .iter_mut()
                .find(|(k, _)| k == key.as_bytes())
                .unwrap()
                .1
        } else {
            panic!()
        }
    }
    let mut input = parse(expected);
    prepend(&mut input, "schema", "1");
    prepend(field(&mut input, "baseline"), "x", "0");
    let primary = field(field(&mut input, "metrics"), "primary");
    prepend(primary, "name", r#""old""#);
    prepend(primary, "direction", r#""minimize""#);
    let success = field(&mut input, "success");
    prepend(success, "min_improvement", "false");
    let regression = field(field(success, "no_regression"), "score");
    prepend(regression, "direction", r#""observe""#);
    prepend(field(&mut input, "budget"), "runs", "0");
    if let Json::Array(items) = field(&mut input, "invariant_pack") {
        prepend(&mut items[0], "name", r#""bad name""#);
        prepend(&mut items[0], "command", "[]");
        prepend(&mut items[0], "required", "false");
    } else {
        panic!()
    }
    prepend(field(&mut input, "extra"), "x", "0");
    let actual = validate(&input).unwrap();
    let reference = validate(&parse(expected)).unwrap();
    assert_eq!(actual.source_bytes, reference.source_bytes);
    assert_eq!(
        actual.canonical_bytes().unwrap(),
        reference.canonical_bytes().unwrap()
    );
}

#[test]
fn number_grammar_and_nonfinite_constants() {
    for spelling in [
        "", "-", "inf", "+2.0", ".5", "01", "-01", "1.", "1e", "1e+", "bad",
    ] {
        let mut raw = parse(BASE);
        if let Json::Object(ref mut fields) = raw {
            fields.push((b"registered_at".to_vec(), Json::Number(spelling.into())));
        }
        assert!(
            matches!(validate(&raw), Err(Error::MalformedNumber)),
            "{spelling}"
        );
    }
    for spelling in ["0", "-0", "1.0", "-1.25e+2", "NaN", "Infinity", "-Infinity"] {
        let mut raw = parse(BASE);
        if let Json::Object(ref mut fields) = raw {
            fields.push((b"registered_at".to_vec(), Json::Number(spelling.into())));
        }
        assert!(validate(&raw).is_ok(), "{spelling}");
    }
    for spelling in ["NaN", "Infinity", "-Infinity"] {
        let mut raw = parse(BASE);
        if let Json::Object(ref mut fields) = raw {
            fields.push((b"extra".to_vec(), Json::Number(spelling.into())));
        }
        assert!(
            matches!(
                validate(&raw),
                Err(Error::Encoding(oms_json::Error::NonFiniteFloat))
            ),
            "{spelling}"
        );
    }
    let mut report = validate(&parse(BASE)).unwrap();
    for bytes in [vec![0xed], vec![0xc0, 0x80], vec![0xf4, 0x90, 0x80, 0x80]] {
        report.normalized = Json::String(bytes);
        assert!(matches!(
            report.canonical_bytes(),
            Err(Error::Encoding(oms_json::Error::InvalidCodepoint))
        ));
    }
    report.normalized = Json::Number("+2.0".into());
    assert!(matches!(
        report.canonical_bytes(),
        Err(Error::MalformedNumber)
    ));
}
