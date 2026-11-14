//! Read-only root navigation over the installed source's command headers.
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const USAGE: &[u8] = br#"Usage: oms <tool> [args...]
       oms                         # terminal control panel (interactive TTY)
       oms list [--all]
       oms help

Dispatch to an oh-my-setting harness tool by name. `list` prints the compact
subsystem entrypoints; `list --all` reveals core primitives. The command-routing
reference selects intent variants behind those entries. Docs/COMPONENTS.md
describes the existing subsystems and docs/OMS-RUNTIME.md explains the typed
runtime projection.

The terminal panel is the human entrypoint. The other tools are the agent's
hands; give your coding agent the goal instead of composing harness commands.
"#;

const PUBLIC: &str = "advise\nagent-call\nagent-events\nagent-memory\nagent-plan\nagent-role\nagent-supervisor\nagent-task\napply-project-template\napproval-inbox\nartifact-index\nartifact-view\nauto-update\nautopilot\ncandidate\nchange-guard\ncheckpoint\nci-status\ncite\ncleanup\nconnect-services\nconsult\ndashboard\ndata-manifest\ndeps\ndoctor\ndraft-pr\nexperiment-board\nexternal\nfail-ledger\ngc\ngoal-drive\ngraph\nharness\ninbox\ninit\ninput\ninstall-profile\nintent\njob-digest\njournal\nland\nlease\nmodel-doctor\nmodels\noutput-digest\npanel\npatch-admit\npatch-land\npeer-ask\npeer-delegate\npeer-review\nplan-from-spec\nplan-run\nproject-doctor\nproject-private\nprovider-permissions\nrecipe\nremove-project-template\nreport\nroom\nrun\nrun-ledger\nrun-reconcile\nruntime\nscratch-worktree\nsession\nsession-handoff\nskill-doctor\nskill-forge\nslurm-lint\nslurm-submit\nsnapshot\nstate\nstate-verify\nstatus\nsync-shared\nthread\ntick\ntsp-queue\nuninstall\nupdate";
const FRONTDOOR: &str = "# Setup and capabilities\ninit\ninstall-profile\nconnect-services\n# Goals and daily work\npanel\nagent-plan\nautopilot\nharness\ninbox\nstatus\ndashboard\nruntime\njournal\ntick\n# Collaboration and review\nroom\nconsult\npeer-delegate\npeer-review\n# Admission and delivery\npatch-admit\npatch-land\nland\nlease\n# Maintenance and removal\nreport\ndoctor\nupdate\nuninstall";

pub(super) fn owns(args: &[OsString]) -> bool {
    args.first()
        .is_some_and(|arg| matches!(arg.as_bytes(), b"help" | b"list" | b"-h" | b"--help"))
}

fn sentence(text: &[u8]) -> &[u8] {
    for (index, pair) in text.windows(2).enumerate() {
        if pair == b". " {
            let prefix = &text[..index + 1];
            if !prefix.ends_with(b"e.g.") && !prefix.ends_with(b"i.e.") {
                return prefix;
            }
        }
    }
    text
}

fn append_paragraph(target: &mut Vec<u8>, text: &[u8]) {
    if !target.is_empty() {
        target.push(b' ');
    }
    target.extend_from_slice(text);
}

fn describe(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let mut line = Vec::new();
    let (mut header, mut usage) = (Vec::new(), Vec::new());
    let (mut header_done, mut in_usage, mut number) = (false, false, 0);
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        number += 1;
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if number > 2 && !header_done {
            if line.starts_with(b"# shellcheck") {
                // Source annotations are not part of the user description.
            } else if line.starts_with(b"#") {
                let start = line.iter().take_while(|&&byte| byte == b'#').count();
                let text = &line[start..];
                append_paragraph(&mut header, text.strip_prefix(b" ").unwrap_or(text));
            } else if !header.is_empty() || !line.iter().all(u8::is_ascii_whitespace) {
                header_done = true;
            }
            if header_done && !header.is_empty() {
                break;
            }
        }
        if line.starts_with(b"Usage:") {
            in_usage = true;
            continue;
        }
        if in_usage && line == b"EOF" {
            break;
        }
        if in_usage
            && line.iter().any(|byte| !byte.is_ascii_whitespace())
            && line.first().is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            append_paragraph(&mut usage, &line);
            continue;
        }
        if in_usage && !usage.is_empty() {
            break;
        }
    }
    Ok(sentence(if header.is_empty() { &usage } else { &header }).to_vec())
}

pub(super) fn run(args: &[OsString], source: &Path) -> (i32, Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let selected = match args.first().map(|arg| arg.as_bytes()) {
        Some(b"-h" | b"--help") => return (0, USAGE.to_vec(), err),
        Some(b"help") => {
            out.extend_from_slice(USAGE);
            out.push(b'\n');
            FRONTDOOR
        }
        Some(b"list") => match args.get(1).map(|arg| arg.as_bytes()) {
            None | Some(b"") => FRONTDOOR,
            Some(b"--all") => PUBLIC,
            _ => return (2, out, b"error: oms list accepts --all\n".to_vec()),
        },
        _ => {
            return (
                2,
                out,
                b"error: invalid root navigation invocation\n".to_vec(),
            );
        }
    };
    for name in selected.lines() {
        if let Some(title) = name.strip_prefix("# ") {
            out.extend_from_slice(title.as_bytes());
            out.extend_from_slice(b":\n");
            continue;
        }
        let path = source.join("scripts").join(format!("{name}.sh"));
        if !path.is_file() {
            continue;
        }
        out.extend_from_slice(format!("{name:<26} ").as_bytes());
        match describe(&path) {
            Ok(description) => out.extend_from_slice(&description),
            Err(error) => {
                err.extend_from_slice(b"oms: cannot read tool description: ");
                err.extend_from_slice(path.as_os_str().as_bytes());
                err.extend_from_slice(format!(": {error}\n").as_bytes());
            }
        }
        out.push(b'\n');
    }
    (0, out, err)
}
