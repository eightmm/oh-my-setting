use oms_contract_markdown::*;
#[test]
fn legacy_contract_examples() {
    assert_eq!(normalized_line(" - [X]  한\u{1c}글 - "), "한 글");
    let (s, m) = sections(
        "---\r\nName: Front\r\n---\r\n- name: fallback\n# Verify\n- first\n# Verification\nother\n# Verify\n- second\n",
    );
    assert_eq!(m["name"], "Front");
    assert_eq!(section_text_exact(&s, &["verify"]), "- first\n- second");
    assert_eq!(section_text(&s, &["verify"]), "- first\n- second");
    assert_eq!(
        bullet_items("- [x] -1\n  continuation\n1. --flag\nstop", false),
        ["1 continuation", "flag"]
    );
    assert_eq!(
        bullet_items("- [x] -1\n  continuation", true),
        ["[x] -1 continuation"]
    );
    assert_eq!(bounded_line("가나다", 2), "가…");
    assert_eq!(bounded_line("x", 0), "…");
    assert_eq!(first_nonempty("---\n  [x] hi", 500), "hi");
    assert_eq!(stable_criterion_id("s", "[ID:Ab-C] test"), "Ab-C");
    assert_eq!(strip_criterion_marker("A [criterion:Ab] B"), "A B");
    assert_eq!(
        parse_path_list("./a, ../bad, C:\\tmp, `b\\c`, /abs"),
        ["a", "b/c"]
    );
    let scope = parse_scope(["- allowed: b, a", "- forbidden: ../x, a"].iter().copied());
    assert_eq!(scope.allowed, ["a", "b"]);
    assert_eq!(scope.forbidden, ["a"]);
}

#[test]
fn python_unicode_and_edges() {
    for (text, expected) in [
        ("ΟΣ", "criterion-9eefee9656ab"),
        ("ΟΣΑ", "criterion-f2c69684030a"),
        ("İ Σ\u{301}", "criterion-377f6ce7dbdb"),
        ("\u{378}", "criterion-bc2a4e7457dc"),
        ("한글", "criterion-ca0c133a0e8d"),
        ("a\u{1c}b", "criterion-5eece3380e7d"),
    ] {
        assert_eq!(stable_criterion_id("src", text), expected);
    }
    let (s, _) = sections("# A\u{1c}body\u{85}# B\u{2028}end");
    assert_eq!(s[1].1, ["body"]);
    assert_eq!(s[2].1, ["end"]);
    let (s, m) = sections("---\nfoo: x");
    assert_eq!(s[0].1.len(), 0);
    assert_eq!(m["foo"], "x");
    assert_eq!(bullet_items("- a\n  \n- b", true), ["a ", "b"]);
    let marker = format!("[id:{}]", "x".repeat(81));
    assert_eq!(strip_criterion_marker(&marker), marker);
    assert_eq!(
        parse_path_list("//server/share, C:/x, ././ok, x/../y, C:relative"),
        ["C:relative", "ok"]
    );
    assert_eq!(bounded_line("", 0), "");
    assert_eq!(bounded_line("ab", 2), "ab");
}

#[test]
fn python_case_insensitive_marker() {
    assert_eq!(stable_criterion_id("s", "[ıd:X]"), "X");
}

#[test]
fn python_312_unicode_regressions() {
    assert_eq!(stable_criterion_id("s", "[İd:X]"), "X");
    assert_eq!(stable_criterion_id("s", "[crİterİon:Y]"), "Y");
    assert_eq!(
        stable_criterion_id("s", "\u{a7cb}"),
        "criterion-ac18ce4203b8"
    );
    assert_eq!(bullet_items("\u{10d40}. body", false), Vec::<String>::new());
    assert_eq!(parse_scope(["İnclude: a", "forbİdden: b"]).allowed, ["a"]);
    assert_eq!(parse_scope(["İnclude: a", "forbİdden: b"]).forbidden, ["b"]);
}

#[test]
fn python_contextual_sigma_and_headings() {
    for (input, id) in [
        ("Α\u{295}Σ", "criterion-ac721dacc0dc"),
        ("ΑΣ\u{295}", "criterion-700a8fa00241"),
        ("\u{295}Σ", "criterion-85ce6cd06682"),
        ("Α\u{a7cb}Σ", "criterion-3cbbdce504a5"),
        ("ΑΣ\u{a7cb}Α", "criterion-0bda2df3b901"),
    ] {
        assert_eq!(stable_criterion_id("s", input), id);
    }
    let (s, _) =
        sections("# Verify\nfirst\n# Verify Extra\nextra\n# Verify\nlast\n# Verification\nother");
    assert_eq!(section_text(&s, &["verify"]), "first\nlast\nextra");
    assert_eq!(section_text_exact(&s, &["verify"]), "first\nlast");
    assert_eq!(bullet_items("\u{660}. yes\n\u{10d40}. no", true), ["yes"]);
}

#[test]
fn bounded_legacy_oracle_corpus() {
    assert_eq!(normalized_line(""), "");
    assert_eq!(bounded_line("", 0), "");
    assert_eq!(bounded_line("", 1), "");
    assert_eq!(bounded_line("", 4), "");
    assert_eq!(normalized_line("   "), "");
    assert_eq!(bounded_line("   ", 0), "");
    assert_eq!(bounded_line("   ", 1), "");
    assert_eq!(bounded_line("   ", 4), "");
    assert_eq!(normalized_line("- [X] one\u{1c}둘 - "), "one 둘");
    assert_eq!(bounded_line("- [X] one\u{1c}둘 - ", 0), "…");
    assert_eq!(bounded_line("- [X] one\u{1c}둘 - ", 1), "…");
    assert_eq!(bounded_line("- [X] one\u{1c}둘 - ", 4), "- […");
    assert_eq!(normalized_line("[ ] [x] --flag"), "flag");
    assert_eq!(bounded_line("[ ] [x] --flag", 0), "…");
    assert_eq!(bounded_line("[ ] [x] --flag", 1), "…");
    assert_eq!(bounded_line("[ ] [x] --flag", 4), "[ ]…");
    assert_eq!(normalized_line("  -1"), "1");
    assert_eq!(bounded_line("  -1", 0), "…");
    assert_eq!(bounded_line("  -1", 1), "…");
    assert_eq!(bounded_line("  -1", 4), "-1");
    assert_eq!(normalized_line(" 한 글"), "한 글");
    assert_eq!(bounded_line(" 한 글", 0), "…");
    assert_eq!(bounded_line(" 한 글", 1), "…");
    assert_eq!(bounded_line(" 한 글", 4), "한 글");
    assert_eq!(normalized_line("άΣ"), "άΣ");
    assert_eq!(bounded_line("άΣ", 0), "…");
    assert_eq!(bounded_line("άΣ", 1), "…");
    assert_eq!(bounded_line("άΣ", 4), "άΣ");
    assert_eq!(normalized_line("͸"), "͸");
    assert_eq!(bounded_line("͸", 0), "…");
    assert_eq!(bounded_line("͸", 1), "͸");
    assert_eq!(bounded_line("͸", 4), "͸");
    {
        let (s, meta) = sections("");
        assert_eq!(s, vec![("__preamble__".to_owned(), vec![])]);
        assert_eq!(meta.len(), 0);
        assert_eq!(section_text(&s, &["verify", "a"]), "");
        assert_eq!(section_text_exact(&s, &["verify", "a"]), "");
        assert_eq!(bullet_items("", false), Vec::<String>::new());
        assert_eq!(bullet_items("", true), Vec::<String>::new());
        assert_eq!(first_nonempty("", 5), "");
    }
    {
        let (s, meta) = sections(
            "---\u{d}\u{a}Foo: one\u{d}\u{a}---\u{a}- foo: two\u{a}# Verify\u{a}- a\u{a}# Verify Extra\u{a}x\u{a}# Verify\u{a}- b",
        );
        assert_eq!(
            s,
            vec![
                ("__preamble__".to_owned(), vec!["- foo: two".to_owned()]),
                (
                    "Verify".to_owned(),
                    vec!["- a".to_owned(), "- b".to_owned()]
                ),
                ("Verify Extra".to_owned(), vec!["x".to_owned()])
            ]
        );
        assert_eq!(meta.len(), 1);
        assert_eq!(meta["foo"], "one");
        assert_eq!(
            section_text(&s, &["verify", "a"]),
            "- foo: two\u{a}- a\u{a}- b\u{a}x"
        );
        assert_eq!(section_text_exact(&s, &["verify", "a"]), "- a\u{a}- b");
        assert_eq!(
            bullet_items(
                "---\u{d}\u{a}Foo: one\u{d}\u{a}---\u{a}- foo: two\u{a}# Verify\u{a}- a\u{a}# Verify Extra\u{a}x\u{a}# Verify\u{a}- b",
                false
            ),
            ["foo: two", "a", "b"]
        );
        assert_eq!(
            bullet_items(
                "---\u{d}\u{a}Foo: one\u{d}\u{a}---\u{a}- foo: two\u{a}# Verify\u{a}- a\u{a}# Verify Extra\u{a}x\u{a}# Verify\u{a}- b",
                true
            ),
            ["foo: two", "a", "b"]
        );
        assert_eq!(
            first_nonempty(
                "---\u{d}\u{a}Foo: one\u{d}\u{a}---\u{a}- foo: two\u{a}# Verify\u{a}- a\u{a}# Verify Extra\u{a}x\u{a}# Verify\u{a}- b",
                5
            ),
            "Foo:…"
        );
    }
    {
        let (s, meta) = sections("---\u{a}foo: bar");
        assert_eq!(s, vec![("__preamble__".to_owned(), vec![])]);
        assert_eq!(meta.len(), 1);
        assert_eq!(meta["foo"], "bar");
        assert_eq!(section_text(&s, &["verify", "a"]), "");
        assert_eq!(section_text_exact(&s, &["verify", "a"]), "");
        assert_eq!(
            bullet_items("---\u{a}foo: bar", false),
            Vec::<String>::new()
        );
        assert_eq!(bullet_items("---\u{a}foo: bar", true), Vec::<String>::new());
        assert_eq!(first_nonempty("---\u{a}foo: bar", 5), "foo:…");
    }
    {
        let (s, meta) = sections("x\u{b}Y\u{c}Z\u{1c}1\u{1d}2\u{1e}34 5 6");
        assert_eq!(
            s,
            vec![(
                "__preamble__".to_owned(),
                vec![
                    "x".to_owned(),
                    "Y".to_owned(),
                    "Z".to_owned(),
                    "1".to_owned(),
                    "2".to_owned(),
                    "3".to_owned(),
                    "4".to_owned(),
                    "5".to_owned(),
                    "6".to_owned()
                ]
            )]
        );
        assert_eq!(meta.len(), 0);
        assert_eq!(
            section_text(&s, &["verify", "a"]),
            "x\u{a}Y\u{a}Z\u{a}1\u{a}2\u{a}3\u{a}4\u{a}5\u{a}6"
        );
        assert_eq!(section_text_exact(&s, &["verify", "a"]), "");
        assert_eq!(
            bullet_items("x\u{b}Y\u{c}Z\u{1c}1\u{1d}2\u{1e}34 5 6", false),
            Vec::<String>::new()
        );
        assert_eq!(
            bullet_items("x\u{b}Y\u{c}Z\u{1c}1\u{1d}2\u{1e}34 5 6", true),
            Vec::<String>::new()
        );
        assert_eq!(
            first_nonempty("x\u{b}Y\u{c}Z\u{1c}1\u{1d}2\u{1e}34 5 6", 5),
            "x"
        );
    }
    {
        let (s, meta) = sections("# A\u{a}- [x] a\u{a}  next\u{a}2) --flag\u{a}end");
        assert_eq!(
            s,
            vec![
                ("__preamble__".to_owned(), vec![]),
                (
                    "A".to_owned(),
                    vec![
                        "- [x] a".to_owned(),
                        "  next".to_owned(),
                        "2) --flag".to_owned(),
                        "end".to_owned()
                    ]
                )
            ]
        );
        assert_eq!(meta.len(), 0);
        assert_eq!(
            section_text(&s, &["verify", "a"]),
            "- [x] a\u{a}  next\u{a}2) --flag\u{a}end"
        );
        assert_eq!(
            section_text_exact(&s, &["verify", "a"]),
            "- [x] a\u{a}  next\u{a}2) --flag\u{a}end"
        );
        assert_eq!(
            bullet_items("# A\u{a}- [x] a\u{a}  next\u{a}2) --flag\u{a}end", false),
            ["a next", "flag"]
        );
        assert_eq!(
            bullet_items("# A\u{a}- [x] a\u{a}  next\u{a}2) --flag\u{a}end", true),
            ["[x] a next", "--flag"]
        );
        assert_eq!(
            first_nonempty("# A\u{a}- [x] a\u{a}  next\u{a}2) --flag\u{a}end", 5),
            "# A"
        );
    }
    {
        let (s, meta) = sections("# A\u{a}# B\u{a}# A\u{a}body");
        assert_eq!(
            s,
            vec![
                ("__preamble__".to_owned(), vec![]),
                ("A".to_owned(), vec!["body".to_owned()]),
                ("B".to_owned(), vec![])
            ]
        );
        assert_eq!(meta.len(), 0);
        assert_eq!(section_text(&s, &["verify", "a"]), "body");
        assert_eq!(section_text_exact(&s, &["verify", "a"]), "body");
        assert_eq!(
            bullet_items("# A\u{a}# B\u{a}# A\u{a}body", false),
            Vec::<String>::new()
        );
        assert_eq!(
            bullet_items("# A\u{a}# B\u{a}# A\u{a}body", true),
            Vec::<String>::new()
        );
        assert_eq!(first_nonempty("# A\u{a}# B\u{a}# A\u{a}body", 5), "# A");
    }
    assert_eq!(stable_criterion_id("src", "[İd:MiX]"), "MiX");
    assert_eq!(strip_criterion_marker("[İd:MiX]"), "");
    assert_eq!(
        stable_criterion_id(
            "src",
            "[id:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx]"
        ),
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    );
    assert_eq!(
        strip_criterion_marker(
            "[id:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx]"
        ),
        ""
    );
    assert_eq!(
        stable_criterion_id(
            "src",
            "[id:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx]"
        ),
        "criterion-d7b3aeff75a5"
    );
    assert_eq!(
        strip_criterion_marker(
            "[id:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx]"
        ),
        "[id:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx]"
    );
    assert_eq!(stable_criterion_id("src", "ΟΣ"), "criterion-9eefee9656ab");
    assert_eq!(strip_criterion_marker("ΟΣ"), "ΟΣ");
    assert_eq!(stable_criterion_id("src", "ΑΣʕ"), "criterion-7a8ebde4abc0");
    assert_eq!(strip_criterion_marker("ΑΣʕ"), "ΑΣʕ");
    assert_eq!(stable_criterion_id("src", "Ɤ"), "criterion-0017d397117a");
    assert_eq!(strip_criterion_marker("Ɤ"), "Ɤ");
    assert_eq!(
        stable_criterion_id(
            "src",
            "long xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        ),
        "criterion-eca8f32e9840"
    );
    assert_eq!(
        strip_criterion_marker(
            "long xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        ),
        "long xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    );
    assert_eq!(stable_criterion_id("src", "A [criterion:x] B"), "x");
    assert_eq!(strip_criterion_marker("A [criterion:x] B"), "A B");
    assert_eq!(parse_path_list(""), Vec::<String>::new());
    assert_eq!(
        parse_path_list("./a, ../b, C:\\tmp, //host/a, `x\\y`"),
        ["a", "x/y"]
    );
    assert_eq!(parse_path_list("././a, a, c:relative"), ["a", "c:relative"]);
    assert_eq!(parse_path_list("a\u{a}b, x/../z"), ["a", "b"]);
    let scope = parse_scope([
        "- allowed: b, ./a, ../x",
        "- forbİdden: x, /a",
        "include: c",
        "scope.deny: z, x",
    ]);
    assert_eq!(scope.allowed, ["a", "b", "c"]);
    assert_eq!(scope.forbidden, ["x", "z"]);
}

#[test]
fn unicode15_scalar_difference_regressions() {
    for (c, expected) in [
        ('\u{1c89}', "criterion-2266433eae15"),
        ('\u{a7cb}', "criterion-ac18ce4203b8"),
        ('\u{a7cc}', "criterion-4372650df356"),
        ('\u{a7ce}', "criterion-c0bbcfbdd7b7"),
        ('\u{a7d2}', "criterion-0ab3834481be"),
        ('\u{a7d4}', "criterion-192f084a6bd3"),
        ('\u{a7da}', "criterion-41bdbf496727"),
        ('\u{a7dc}', "criterion-46d4c2dd2509"),
        ('\u{10d50}', "criterion-ad1f31e47a2b"),
        ('\u{10d51}', "criterion-3e5da78d2edd"),
        ('\u{10d52}', "criterion-cf1a9df4e8b7"),
        ('\u{10d53}', "criterion-212e13699e9b"),
        ('\u{10d54}', "criterion-53172a4f89bd"),
        ('\u{10d55}', "criterion-a8059ba766ec"),
        ('\u{10d56}', "criterion-500d63d23d06"),
        ('\u{10d57}', "criterion-5827026f9890"),
        ('\u{10d58}', "criterion-ace31ec5c602"),
        ('\u{10d59}', "criterion-1ca1a513067b"),
        ('\u{10d5a}', "criterion-d5980297ad07"),
        ('\u{10d5b}', "criterion-ec4254c5b4af"),
        ('\u{10d5c}', "criterion-9aec8e7bd45a"),
        ('\u{10d5d}', "criterion-4cc63f446e9d"),
        ('\u{10d5e}', "criterion-9f91d42dbb07"),
        ('\u{10d5f}', "criterion-555c8e5c07ec"),
        ('\u{10d60}', "criterion-5673faea7940"),
        ('\u{10d61}', "criterion-208b3338ea56"),
        ('\u{10d62}', "criterion-772a18872c37"),
        ('\u{10d63}', "criterion-dda6ed968b3a"),
        ('\u{10d64}', "criterion-d51164b74ac8"),
        ('\u{10d65}', "criterion-832712e7bc1f"),
        ('\u{16ea0}', "criterion-bfa2fe6c11a7"),
        ('\u{16ea1}', "criterion-b464cce7bf64"),
        ('\u{16ea2}', "criterion-30659e1db07a"),
        ('\u{16ea3}', "criterion-c9e5d019145c"),
        ('\u{16ea4}', "criterion-7d46c34da3d9"),
        ('\u{16ea5}', "criterion-1426722d6a00"),
        ('\u{16ea6}', "criterion-6b6ea203e786"),
        ('\u{16ea7}', "criterion-34222bafb73a"),
        ('\u{16ea8}', "criterion-eb1983211345"),
        ('\u{16ea9}', "criterion-fcb49d3a22f2"),
        ('\u{16eaa}', "criterion-d368b9f2dc1c"),
        ('\u{16eab}', "criterion-9e54a359fd84"),
        ('\u{16eac}', "criterion-9b3420b4e656"),
        ('\u{16ead}', "criterion-faf56052831e"),
        ('\u{16eae}', "criterion-302eaa345a7f"),
        ('\u{16eaf}', "criterion-37d604c7cd58"),
        ('\u{16eb0}', "criterion-038b4d538859"),
        ('\u{16eb1}', "criterion-2b432495dc40"),
        ('\u{16eb2}', "criterion-2929991a6315"),
        ('\u{16eb3}', "criterion-2ca1770720a9"),
        ('\u{16eb4}', "criterion-96b1903b47af"),
        ('\u{16eb5}', "criterion-e350415948fb"),
        ('\u{16eb6}', "criterion-71e55f80293b"),
        ('\u{16eb7}', "criterion-85f4996a10c6"),
        ('\u{16eb8}', "criterion-a6380d1a730a"),
    ] {
        assert_eq!(stable_criterion_id("s", &c.to_string()), expected);
    }
    for c in [
        '\u{10d40}',
        '\u{10d41}',
        '\u{10d42}',
        '\u{10d43}',
        '\u{10d44}',
        '\u{10d45}',
        '\u{10d46}',
        '\u{10d47}',
        '\u{10d48}',
        '\u{10d49}',
        '\u{116d0}',
        '\u{116d1}',
        '\u{116d2}',
        '\u{116d3}',
        '\u{116d4}',
        '\u{116d5}',
        '\u{116d6}',
        '\u{116d7}',
        '\u{116d8}',
        '\u{116d9}',
        '\u{116da}',
        '\u{116db}',
        '\u{116dc}',
        '\u{116dd}',
        '\u{116de}',
        '\u{116df}',
        '\u{116e0}',
        '\u{116e1}',
        '\u{116e2}',
        '\u{116e3}',
        '\u{11bf0}',
        '\u{11bf1}',
        '\u{11bf2}',
        '\u{11bf3}',
        '\u{11bf4}',
        '\u{11bf5}',
        '\u{11bf6}',
        '\u{11bf7}',
        '\u{11bf8}',
        '\u{11bf9}',
        '\u{16130}',
        '\u{16131}',
        '\u{16132}',
        '\u{16133}',
        '\u{16134}',
        '\u{16135}',
        '\u{16136}',
        '\u{16137}',
        '\u{16138}',
        '\u{16139}',
        '\u{16d70}',
        '\u{16d71}',
        '\u{16d72}',
        '\u{16d73}',
        '\u{16d74}',
        '\u{16d75}',
        '\u{16d76}',
        '\u{16d77}',
        '\u{16d78}',
        '\u{16d79}',
        '\u{1ccf0}',
        '\u{1ccf1}',
        '\u{1ccf2}',
        '\u{1ccf3}',
        '\u{1ccf4}',
        '\u{1ccf5}',
        '\u{1ccf6}',
        '\u{1ccf7}',
        '\u{1ccf8}',
        '\u{1ccf9}',
        '\u{1e5f1}',
        '\u{1e5f2}',
        '\u{1e5f3}',
        '\u{1e5f4}',
        '\u{1e5f5}',
        '\u{1e5f6}',
        '\u{1e5f7}',
        '\u{1e5f8}',
        '\u{1e5f9}',
        '\u{1e5fa}',
    ] {
        assert!(bullet_items(&format!("{c}. body"), false).is_empty());
    }
}
