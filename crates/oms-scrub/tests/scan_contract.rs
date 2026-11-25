use oms_scrub::{
    Error, MAX_ENV_BYTES, MAX_ENV_ENTRIES, MAX_HITS, MAX_INPUT_BYTES, Tier,
    first_unpaired_key_end_line, redact, scan,
};

#[test]
fn tiers_filters_and_binary() {
    let text = [
        b"persist-creden".as_slice(),
        b"tials: false\n",
        b"key = lookup(reference)\n",
        b"bad\0\xff ",
        b"api_to",
        b"ken=value\n",
        b"/ho",
        b"me/example\n",
    ]
    .concat();
    let hits = scan(&text, &[]).unwrap();
    assert!(
        hits.iter()
            .any(|h| h.line == 3 && h.tier == Tier::Secret && h.class == "token-assignment")
    );
    assert!(
        hits.iter()
            .any(|h| h.line == 4 && h.tier == Tier::Machine && h.class == "home-path")
    );
    assert_eq!(hits.len(), 2);
    assert!(scan(b"relative/home/example\n", &[]).unwrap().is_empty());
    assert!(scan(b"", &[]).unwrap().is_empty());
}

#[test]
fn environment_multiline_and_redaction() {
    let name = [b"FIXTURE_PASS".as_slice(), b"PHRASE"].concat();
    let value = b"first phrase\nsecond phrase";
    let env = [(name.as_slice(), value.as_slice())];
    let data = b"before\nfirst phrase\nsecond phrase\nafter\n";
    let hits = scan(data, &env).unwrap();
    assert_eq!(hits[0].line, 2);
    assert_eq!(hits[0].class, "env-secret");
    let result = redact(data, &env).unwrap();
    assert_eq!(
        result.bytes,
        b"before\n[oms-redacted:env-secret]\n[oms-redacted:env-secret]\nafter\n"
    );
    assert_eq!(result.count, 2);
    let short = [(name.as_slice(), b"short".as_slice())];
    assert!(scan(b"short", &short).unwrap().is_empty());
    let meta = [b"FIXTURE_API_".as_slice(), b"KEY_NAME"].concat();
    assert!(
        scan(
            b"ordinarylongvalue",
            &[(meta.as_slice(), b"ordinarylongvalue")]
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn private_blocks_and_precedence() {
    let input = [
        b"first\n".as_slice(),
        b"-----BE",
        b"GIN PRIVATE KEY-----\nbody\n-----END PRIVATE KEY-----\nlast\n",
    ]
    .concat();
    let out = redact(&input, &[]).unwrap();
    assert_eq!(out.bytes, b"first\n[oms-redacted:private-key]\nlast\n");
    assert_eq!(out.count, 1);
    let name = [b"API_".as_slice(), b"TOKEN"].concat();
    let val = b"example123456789";
    let data = [b"api_to".as_slice(), b"ken=example123456789\n"].concat();
    assert_eq!(
        redact(&data, &[(name.as_slice(), val)]).unwrap().bytes,
        b"[oms-redacted:token-assignment]\n"
    );

    // Lowercase and no-space markers close key blocks without swallowing trailing lines.
    let begin_l = concat!("-----", "begin");
    let begin_u = concat!("-----", "BEGIN");
    let end_l = concat!("-----", "end");
    let end_u = concat!("-----", "END");

    let lower =
        format!("first\n{begin_l} private key-----\nbody\n{end_l} private key-----\nlast\n");
    assert_eq!(
        redact(lower.as_bytes(), &[]).unwrap().bytes,
        b"first\n[oms-redacted:private-key]\nlast\n"
    );

    let no_space = format!("first\n{begin_u}-----\nbody\n{end_u}-----\nlast\n");
    assert_eq!(
        redact(no_space.as_bytes(), &[]).unwrap().bytes,
        b"first\n[oms-redacted:private-key]\nlast\n"
    );

    let single_line = format!("first\n{begin_u} KEY----- body {end_l} KEY-----\nlast\n");
    assert_eq!(
        redact(single_line.as_bytes(), &[]).unwrap().bytes,
        b"first\n[oms-redacted:private-key]\nlast\n"
    );

    let back_to_back = format!(
        "first\n{begin_u} KEY-----\nbody1\n{end_u} KEY----- {begin_u} KEY-----\nbody2\n{end_u} KEY-----\nlast\n"
    );
    assert_eq!(
        redact(back_to_back.as_bytes(), &[]).unwrap().bytes,
        b"first\n[oms-redacted:private-key]\nlast\n"
    );

    // first_unpaired_key_end_line reports only END markers with no preceding BEGIN.
    assert_eq!(first_unpaired_key_end_line(b""), None);
    assert_eq!(first_unpaired_key_end_line(b"ordinary\ntext\n"), None);
    assert_eq!(
        first_unpaired_key_end_line(format!("body\n{end_u} PRIVATE KEY-----\nafter").as_bytes()),
        Some(2)
    );
    assert_eq!(
        first_unpaired_key_end_line(format!("body\n{end_u}-----\nafter").as_bytes()),
        Some(2)
    );
    assert_eq!(
        first_unpaired_key_end_line(format!("body\n{end_l} private key-----\nafter").as_bytes()),
        Some(2)
    );
    assert_eq!(
        first_unpaired_key_end_line(
            format!("body\r\n{end_u} PRIVATE KEY-----\r\nafter").as_bytes()
        ),
        Some(2)
    );
    assert_eq!(first_unpaired_key_end_line(lower.as_bytes()), None);
    assert_eq!(first_unpaired_key_end_line(single_line.as_bytes()), None);
    assert_eq!(
        first_unpaired_key_end_line(
            format!("body\n{end_u} KEY----- {begin_u} KEY-----\nbody2\n{end_u} KEY-----")
                .as_bytes()
        ),
        Some(2)
    );
    assert_eq!(
        first_unpaired_key_end_line(format!("before\n{begin_u} PRIVATE KEY-----\nbody").as_bytes()),
        None
    );
    assert_eq!(
        first_unpaired_key_end_line(
            format!("cut\n{end_u} KEY-----\nmid\n{begin_u} KEY-----\nb2\n{end_u} KEY-----")
                .as_bytes()
        ),
        Some(2)
    );
}

#[test]
fn limits_fail_closed() {
    assert_eq!(
        scan(&vec![b'x'; MAX_INPUT_BYTES + 1], &[]),
        Err(Error::InputTooLarge)
    );
    assert!(scan(&vec![b'x'; MAX_INPUT_BYTES], &[]).is_ok());
    assert_eq!(
        scan(b"", &[(b"".as_slice(), b"x".as_slice())]),
        Err(Error::MalformedEnvironment)
    );
    assert_eq!(
        scan(b"", &[(b"A".as_slice(), b"\xff".as_slice())]),
        Ok(Vec::new())
    );
    let exact_env = vec![(b"A".as_slice(), b"".as_slice()); MAX_ENV_ENTRIES];
    assert!(scan(b"", &exact_env).is_ok());
    let long_value = vec![b'x'; MAX_ENV_BYTES];
    assert!(scan(b"", &[(b"A".as_slice(), &long_value[..MAX_ENV_BYTES - 1])]).is_ok());
    assert_eq!(
        scan(b"", &[(b"A".as_slice(), &long_value)]),
        Err(Error::EnvironmentTooLarge)
    );
    let env = vec![(b"A".as_slice(), b"".as_slice()); MAX_ENV_ENTRIES + 1];
    assert_eq!(scan(b"", &env), Err(Error::EnvironmentTooLarge));
    let line = [b"to".as_slice(), b"ken=x\n"].concat();
    assert!(scan(&line.repeat(MAX_HITS), &[]).is_ok());
    assert_eq!(
        scan(&line.repeat(MAX_HITS + 1), &[]),
        Err(Error::TooManyHits)
    );
}

#[test]
fn review_unicode_and_git_name() {
    let name = [b"FIXTURE_".as_slice(), b"TOKEN"].concat();
    let value = "\u{b}\u{1c}비밀값12345678\u{2003}";
    let target = "비밀값12345678".as_bytes();
    assert!(
        !scan(target, &[(name.as_slice(), value.as_bytes())])
            .unwrap()
            .is_empty()
    );
    assert!(
        scan(b"ordinary", &[("한글".as_bytes(), "안녕".as_bytes())])
            .unwrap()
            .is_empty()
    );
    assert!(
        scan(b"ordinary", &[(b"NOTE".as_slice(), b"\xff".as_slice())])
            .unwrap()
            .is_empty()
    );
    let raw = b"abcdefgh\xff";
    assert!(
        !scan(b"abcdefgh?", &[(name.as_slice(), raw.as_slice())])
            .unwrap()
            .is_empty()
    );
    let unicode_digits = "١٢٣٤٥٦٧٨".as_bytes();
    assert!(
        scan(unicode_digits, &[(name.as_slice(), unicode_digits)])
            .unwrap()
            .is_empty()
    );
    let db = concat!("scheme://u:", "한글암호12345678", "@example.test").as_bytes();
    assert!(
        !scan("한글암호12345678".as_bytes(), &[(b"URL".as_slice(), db)])
            .unwrap()
            .is_empty()
    );
    let upper = [b"GIT_CONFIG_".as_slice(), b"KEY_0"].concat();
    let lower = [b"git_config_".as_slice(), b"key_0"].concat();
    let sample = b"example-12345678";
    assert!(
        scan(sample, &[(upper.as_slice(), sample)])
            .unwrap()
            .is_empty()
    );
    assert!(
        !scan(sample, &[(lower.as_slice(), sample)])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn review_env_dedup_and_machine_budget() {
    let name = [b"FIXTURE_".as_slice(), b"TOKEN"].concat();
    let data = vec![b'-'; 102_400];
    let value = b"--------";
    let env = vec![(name.as_slice(), value.as_slice()); 100];
    assert_eq!(scan(&data, &env).unwrap().len(), 1);
    let machine = [b"/ho".as_slice(), b"me/person\n"]
        .concat()
        .repeat(MAX_HITS + 1);
    assert_eq!(redact(&machine, &[]).unwrap().count, 0);
    assert_eq!(scan(&machine, &[]), Err(Error::TooManyHits));
}

#[test]
fn review_redaction_tail_and_cap() {
    let line = [b"to".as_slice(), b"ken=x\n"].concat();
    let data = [line.as_slice(), b"tail"].concat();
    assert_eq!(
        redact(&data, &[]).unwrap().bytes,
        b"[oms-redacted:token-assignment]\ntail\n"
    );
    assert_eq!(redact(b"tail", &[]).unwrap().bytes, b"tail");
    let result = redact(&line.repeat(101), &[]).unwrap();
    assert!(result.bytes.ends_with(b"[oms-redacted:token-like]\n"));
    let block = [
        b"-----BE".as_slice(),
        b"GIN PRIVATE KEY-----\nbody\n-----END PRIVATE KEY-----\n",
    ]
    .concat();
    let late = [line.repeat(100), block].concat();
    let out = redact(&late, &[]).unwrap();
    assert!(out.bytes.ends_with(b"[oms-redacted:token-like]\n"));
    assert!(!out.bytes.windows(4).any(|part| part == b"body"));
}

#[test]
fn multiline_work_limit_is_fail_closed() {
    let name = [b"FIXTURE_".as_slice(), b"TOKEN"].concat();
    let values: Vec<Vec<u8>> = (8..28)
        .map(|n| [vec![b'-'; n], b"\n--------".to_vec()].concat())
        .collect();
    let env: Vec<(&[u8], &[u8])> = values
        .iter()
        .map(|v| (name.as_slice(), v.as_slice()))
        .collect();
    let row = [vec![b'-'; 27], b"\n--------\nx\n".to_vec()].concat();
    assert!(scan(&row.repeat(25_000), &env).is_ok());
    assert_eq!(scan(&row.repeat(25_001), &env), Err(Error::TooManyHits));
}

#[test]
fn secret_only_does_not_charge_machine_hits() {
    let row = [b"/ho".as_slice(), b"me/user\n"].concat();
    let input = row.repeat(MAX_HITS + 1);
    assert!(oms_scrub::scan_secret(&input, &[]).unwrap().is_empty());
    assert_eq!(scan(&input, &[]), Err(Error::TooManyHits));
}

/// Self-review gate: every regular tracked file that can enter diff or prompt
/// context passes the outbound scanner. Env files and `.github/` (CI
/// indirections, not literal secrets) are excluded as in the shell gate.
#[test]
fn scrubber_passes_tracked_repository_sources() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let listed = std::process::Command::new("git")
        .args(["ls-files", "-z", "--", "."])
        .args([":(exclude).env*", ":(exclude,glob).github/**"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(listed.status.success());
    let mut checked = 0;
    let mut failures = Vec::new();
    for name in listed.stdout.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let name = std::str::from_utf8(name).unwrap();
        let path = root.join(name);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        checked += 1;
        match scan(&std::fs::read(&path).unwrap(), &[]) {
            Ok(hits) if hits.is_empty() => {}
            result => failures.push(format!("{name}: {result:?}")),
        }
    }
    assert!(checked > 0);
    assert!(failures.is_empty(), "{failures:#?}");
}
