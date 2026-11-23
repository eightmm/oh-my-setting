use oms_migrate::policy::{dependency_violations, source_check, source_violations};
use std::fs;
use std::process::Command;

const RS: &str = "crates/x/src/y.rs";

#[test]
fn real_repository_passes_policy() {
    let output = Command::new(env!("CARGO_BIN_EXE_oms-migrate"))
        .args(["policy", "--repo", env!("CARGO_MANIFEST_DIR")])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "repository policy: PASS\n"
    );
}

#[test]
fn reverse_edges_unclassified_crates_and_incomplete_graphs_are_rejected() {
    let tree = concat!(
        "0oms v0.1.0 (/repo/crates/oms)\n",
        "1oms-panel-core v0.1.0 (/repo/crates/oms-panel-core)\n",
        "1serde_json v1.0.0\n",
        "0oms-json v0.1.0 (/repo/crates/oms-json)\n",
        "1oms-command-task v0.1.0 (/repo/crates/oms-command-task)\n",
        "0oms-command-task v0.1.0 (/repo/crates/oms-command-task)\n",
        "1oms-panel-core v0.1.0 (/repo/crates/oms-panel-core)\n",
        "1oms-json v0.1.0 (/repo/crates/oms-json)\n",
        "0oms-panel-core v0.1.0 (/repo/crates/oms-panel-core)\n",
        "1oms-command-task v0.1.0 (/repo/crates/oms-command-task)\n",
        "0oms-unlisted v0.1.0 (/repo/crates/oms-unlisted)\n",
        "1oms v0.1.0 (/repo/crates/oms)\n",
    );
    let members = [
        "oms",
        "oms-json",
        "oms-command-task",
        "oms-panel-core",
        "oms-unlisted",
    ];
    assert_eq!(
        dependency_violations(tree, &members).unwrap(),
        [
            "oms-command-task (command) depends on oms-panel-core (ui): command crates may depend only on foundation, service and command crates; move the shared code below the ui and entry layers",
            "oms-json (foundation) depends on oms-command-task (command): foundation crates may depend only on foundation crates; move the shared type into a foundation crate or drop the edge",
            "classify oms-unlisted in crates/oms-migrate/src/policy.rs LAYERS",
        ]
    );
    let entry = "0oms-lock v0.1.0 (/repo/crates/oms-lock)\n1oms v0.1.0 (/repo/crates/oms)\n0oms v0.1.0 (/repo/crates/oms)\n";
    let found = dependency_violations(entry, &["oms", "oms-lock"]).unwrap();
    assert!(found[0].contains("depends on oms (entry)"), "{found:?}");
    let tools = concat!(
        "0oms v0.1.0 (/repo/crates/oms)\n",
        "1oms-mcp-server v0.1.0 (/repo/crates/oms-mcp-server)\n",
        "0oms-mcp-server v0.1.0 (/repo/crates/oms-mcp-server)\n",
        "1oms v0.1.0 (/repo/crates/oms)\n",
        "0oms-migrate v0.1.0 (/repo/crates/oms-migrate)\n",
        "1oms-mcp-server v0.1.0 (/repo/crates/oms-mcp-server)\n",
    );
    let found = dependency_violations(tools, &["oms", "oms-mcp-server", "oms-migrate"]).unwrap();
    assert_eq!(found.len(), 2, "{found:#?}");
    assert!(found[0].starts_with("oms-mcp-server (tool) depends on oms (entry)"));
    assert!(found[1].starts_with("oms-migrate (tool) depends on oms-mcp-server (tool)"));
    assert!(dependency_violations("1oms v0.1.0 (/repo/crates/oms)\n", &["oms"]).is_err());
    let incomplete = |tree, members: &[&str]| dependency_violations(tree, members).unwrap_err();
    assert_eq!(
        incomplete("", &["oms"]),
        "dependency graph incomplete: 0 of 1 workspace members resolved; run cargo +1.99.1 tree --workspace --offline --locked to see the error"
    );
    assert!(
        incomplete(entry, &["oms", "oms-lock", "oms-json"])
            .starts_with("dependency graph incomplete: 2 of 3 ")
    );
    assert!(incomplete("", &[]).contains("0 of 0"));
}

#[test]
fn rust_suppressions_must_be_typed_outside_literals_and_fixture_directories() {
    let rust = concat!(
        "#![allow(dead_code)]\n",
        "#[allow(clippy::all)]\n",
        "#[cfg_attr(test, expect(unused_mut))]\n",
        "#[cfg_attr(test, allow(unused_variables))]\n",
        "#[allow(clippy::needless_return)]\n",
        "#[expect(clippy::needless_return, reason = \"kept for the wire format\")]\n",
        "#[allow(clippy::too_many_arguments)] #[allow(dead_code, unused_mut)]\n",
        "const S: &str = \"#![allow(warnings)]\";\n",
        "const R: &str = r#\"#[allow(warnings)] \" #[allow(unused)]\"#;\n",
        "// #[allow(warnings)]\n",
        "/* /* #![allow(unused)] */ #[allow(warnings)] */\n",
    );
    let found = source_violations(RS, rust);
    assert_eq!(
        found,
        [
            "crates/x/src/y.rs:1: #![allow(dead_code)] suppresses lints for the whole module; move dead_code gating to the item, or add the path to MODULE_ALLOW_PATHS with review",
            "crates/x/src/y.rs:2: #[allow(clippy::all)] silences the clippy::all lint group; name the specific lint in #[expect(lint, reason = \"...\")]",
            "crates/x/src/y.rs:3: #[cfg_attr(test,expect(unused_mut))] without a reason; add reason = \"...\"",
            "crates/x/src/y.rs:4: #[cfg_attr(test,allow(unused_variables))] allows unused_variables; add a reason-bearing #[expect(unused_variables, reason = \"...\")] or fix the code; extending the allowlist in policy.rs needs review",
            "crates/x/src/y.rs:5: #[allow(clippy::needless_return)] allows clippy::needless_return; add a reason-bearing #[expect(clippy::needless_return, reason = \"...\")] or fix the code; extending the allowlist in policy.rs needs review",
        ]
    );
    assert!(
        source_violations("crates/x/tests/support/mod.rs", "#![allow(dead_code)]\n").is_empty()
    );
    assert!(source_violations("crates/x/tests/fixtures/y.rs", "#![allow(warnings)]\n").is_empty());
    assert_eq!(
        source_violations("crates/x/src/fixture_util.rs", "#[allow(warnings)]\n").len(),
        1
    );

    let listed = "crates/oms-command-panel/src/agy_child_chat.rs";
    let module = "#![cfg_attr(not(test), allow(dead_code))]\n";
    assert!(source_violations(listed, module).is_empty());
    let twice = source_violations(listed, &format!("{module}{module}"));
    assert_eq!(twice.len(), 1);
    assert!(twice[0].starts_with(&format!(
        "{listed}:2: #![cfg_attr(not(test),allow(dead_code))] suppresses"
    )));
    let other = source_violations(listed, "#![allow(dead_code)]\n");
    assert_eq!(other.len(), 2, "{other:#?}");
    assert!(other[0].contains("suppresses lints for the whole module"));
    assert!(other[1].starts_with(&format!("remove {listed} from MODULE_ALLOW_PATHS")));
}

#[test]
fn bash_shellcheck_disables_must_name_codes_outside_strings() {
    let bash = concat!(
        "#!/usr/bin/env bash\n",
        "# shellcheck disable=all\n",
        "  # shellcheck source=lib.sh disable=SC1091\n",
        "# shellcheck source=lib.sh disable=SC1091,all\n",
        "echo '# shellcheck disable=all' \"# shellcheck disable=all\"\n",
        "#\tshellcheck\tdisable=all\n",
        "echo \"x <<EOF y\"\n",
        "printf '%s' '<<X'\n",
        "# shellcheck disable=all\n",
        "X\n",
    );
    let reason = "name the specific SCxxxx codes and the reason";
    assert_eq!(
        source_violations("scripts/z.sh", bash),
        [
            format!("scripts/z.sh:2: # shellcheck disable=all; {reason}"),
            format!("scripts/z.sh:4: # shellcheck disable=SC1091,all; {reason}"),
            format!("scripts/z.sh:6: # shellcheck disable=all; {reason}"),
            format!("scripts/z.sh:9: # shellcheck disable=all; {reason}"),
        ]
    );
    let disable = "# shellcheck disable=all\n";
    let shell = |shebang: &str| source_violations("scripts/oms", &format!("{shebang}\n{disable}"));
    assert_eq!(shell("#!/usr/bin/env bash").len(), 1);
    assert!(shell("#!/usr/bin/env python3").is_empty());
}

#[test]
fn workaround_and_commented_out_code_comments_are_rejected_outside_docs() {
    let rust = concat!(
        "// FIXME: retry\n",
        "const A: &str = \"FIXME\";\n",
        "/// Keeps the todo list sorted; a HACK here is documentation.\n",
        "/// ```\n",
        "/// // FIXME in an example\n",
        "/// let x = 1;\n",
        "/// ```\n",
        "/* line\n",
        "   HACK */\n",
        "fn f() {} // for now\n",
        "// let value = compute();\n",
        "// use original names separately for collision identity.\n",
        "//! Crate HACK docs. /*! XXX */\n",
    );
    let found = source_violations(RS, rust);
    let marker = "comment; fix it now or track it as an issue/plan task and remove the marker";
    assert_eq!(
        found,
        [
            "crates/x/src/y.rs:11: commented-out code; delete it (Git keeps the history) or restore it".to_string(),
            format!("crates/x/src/y.rs:1: FIXME {marker}"),
            format!("crates/x/src/y.rs:9: HACK {marker}"),
            format!("crates/x/src/y.rs:10: \"for now\" {marker}"),
        ]
    );
    let bash = concat!(
        "#!/bin/sh XXX\n",
        "echo \"FIXME\" # HACK trailing notes are not prose\n",
        "   # Remove after the migration\n",
        "# temporary files live under TMPDIR\n",
        "# shellcheck disable=SC2086 # workaround in a directive\n",
        "cat <<-'EOF'\n",
        "# FIXME in heredoc\n",
        "\tEOF\n",
        "echo $(( 1 << n ))\n",
        "# FIXME after the terminator\n",
    );
    assert_eq!(
        source_violations("scripts/z.sh", bash),
        [
            format!("scripts/z.sh:3: \"remove after\" {marker}"),
            format!("scripts/z.sh:10: FIXME {marker}"),
        ]
    );
}

#[test]
fn source_check_scans_untracked_sources_and_the_root_verify_script() {
    let root = std::env::temp_dir().join(format!("oms-migrate-policy-{}", std::process::id()));
    let write = |path: &str, text: &str| {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let git = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["init", "--quiet"])
        .status();
    assert!(git.unwrap().success());
    write(".gitignore", "ignored.rs\n");
    write("src/new.rs", "#[allow(clippy::all)]\nfn f() {}\n");
    write("src/fixture_util.rs", "// FIXME\n");
    write("tests/fixtures/sample.rs", "// FIXME\n");
    write("ignored.rs", "// FIXME\n");
    write("verify", "#!/usr/bin/env bash\n# shellcheck disable=all\n");
    write(
        "tools/verify",
        "#!/usr/bin/env bash\n# shellcheck disable=all\n",
    );
    let found = source_check(&root).unwrap();
    let _ = fs::remove_dir_all(&root);
    let found = found
        .into_iter()
        .filter(|line| !line.contains("MODULE_ALLOW_PATHS"))
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 3, "{found:#?}");
    assert!(found[0].starts_with("src/fixture_util.rs:1: FIXME comment"));
    assert!(found[1].starts_with("src/new.rs:1: #[allow(clippy::all)] silences"));
    assert!(found[2].starts_with("verify:2: # shellcheck disable=all;"));
}
