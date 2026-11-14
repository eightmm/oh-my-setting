use crate::{
    Context, Error, Output, Result, V, cli::Args, fail, json, now, obj, out, peer, process,
    providers, safety, storage, text,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(crate) const GATE_INSTRUCTION: &str = "\nGate verdict:\nImmediately before the final line, emit CONFIDENCE: <value between 0.0 and 1.0>.\nEnd with exactly one final line: GATE: pass or GATE: fail. A pass requires no blocking bug, regression, unclear contract or unsafe operation.\nReport findings before confidence in one fenced block opened by ```findings, one JSON object per line with file (repository-relative), line (positive integer), severity (blocking or advisory), summary, quote (exact source text), and optional rule. Omit the block if there are no findings.\n";

pub(crate) fn run(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    if argv.first().is_some_and(|s| s == "verdicts") {
        return verdicts(&argv[1..], context);
    }
    if argv.first().is_some_and(|s| s == "findings") {
        return findings(&argv[1..], context);
    }
    if argv.first().is_some_and(|s| s == "advise") {
        return advise(&argv[1..], context);
    }
    let args = peer::parse(argv)?;
    if args.has("help") {
        return Ok(out(peer::help("peer-review").to_vec()));
    }
    for option in [
        "verify",
        "security-audit",
        "confirm",
        "covers",
        "require-dispositions",
    ] {
        if args.has(option) && !args.has("gate") {
            return fail(format!("--{option} requires --gate"));
        }
    }
    if args.has("covers") {
        return fail(
            "--covers needs the owner's current native criterion projection; no reviewer started",
        );
    }
    if args.has("confirm") {
        return fail(
            "confirmation requires provenance-bound singleton findings and independent family selection; no reviewer started",
        );
    }
    if args.has("candidate-patch") {
        return fail(
            "candidate review requires native indexed-patch admission and a pinned private checkout; no reviewer started",
        );
    }
    if args.has("gate")
        && !args.has("no-diff")
        && !peer::dry(&args, "peer-review")
        && !args.has("export-only")
    {
        let repo = storage::repository(context, &args.path("repo", context.cwd))?;
        let untracked = process::git(
            context,
            &repo,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        if untracked
            .split(|b| *b == 0)
            .any(|p| !p.is_empty() && !p.starts_with(b".oms/"))
        {
            return fail(
                "blocking gate cannot review untracked content; stage it or add intent-to-add first",
            );
        }
    }
    peer::council("peer-review", &args, context)
}
fn base(repo: &Path, value: &str, context: &Context<'_>) -> Result<String> {
    if value.starts_with('-') || value.is_empty() {
        return fail("invalid revision");
    }
    let bytes = process::git(
        context,
        repo,
        &["rev-parse", "--verify", &format!("{value}^{{commit}}")],
    )?;
    Ok(String::from_utf8_lossy(&bytes).trim().into())
}
pub(crate) fn scope(args: &Args, repo: &Path, context: &Context<'_>) -> Result<(V, Vec<u8>)> {
    let base = base(repo, args.value("base", "HEAD"), context)?;
    let head = base_ref(repo, "HEAD", context)?;
    let patch = process::git(
        context,
        repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            &base,
            "--",
        ],
    )?;
    let raw_paths = process::git(context, repo, &["diff", "--name-only", "-z", &base, "--"])?;
    let paths = raw_paths
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| V::Str(b.to_vec()))
        .collect();
    let dirty = process::git(
        context,
        repo,
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    Ok((
        obj([
            ("schema", 1usize.into()),
            ("kind", "review-scope".into()),
            ("base", base.into()),
            ("head", head.into()),
            ("diff_sha256", storage::hash(&patch).into()),
            ("diff_included", (!args.has("no-diff")).into()),
            ("tracked_clean", dirty.is_empty().into()),
            ("paths", V::Arr(paths)),
            ("ts", now().into()),
        ]),
        patch,
    ))
}
fn base_ref(repo: &Path, value: &str, context: &Context<'_>) -> Result<String> {
    base(repo, value, context)
}
pub(crate) fn prepare(
    value: &V,
    repo: &Path,
    dir: &Path,
    run: &str,
    context: &Context<'_>,
) -> Result<()> {
    storage::write(
        repo,
        &dir.join(format!("_scope-{run}.json")),
        json(value)?,
        false,
        context.interrupted,
    )
}
pub fn artifact_sections(raw: &str, require_exit: bool) -> (String, Option<i32>) {
    let mut start = None;
    let mut completed = None;
    let mut exit_start = None;
    let mut offset = 0usize;
    for line in raw.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content == "## Output" {
            start = Some(offset + line.len());
            if !require_exit {
                completed = Some((offset + line.len(), raw.len()));
                exit_start = None;
            }
        } else if content == "## Exit" {
            if let Some(start) = start {
                completed = Some((start, offset));
            }
            exit_start = Some(offset + line.len());
        }
        offset += line.len();
    }
    let answer = completed
        .map(|(a, b)| raw[a..b].trim().to_owned())
        .unwrap_or_default();
    let code = exit_start
        .and_then(|n| raw[n..].lines().map(str::trim).find(|s| !s.is_empty()))
        .and_then(|s| s.parse::<i32>().ok());
    (answer, code)
}
pub fn verdict(answer: &str, code: Option<i32>, complete: bool) -> (&'static str, Option<f64>) {
    if code != Some(0) || !complete || providers::answer_quality(answer) != "ok" {
        return ("incomplete", None);
    }
    let mut confidence = None;
    let mut state = "no-verdict";
    for line in answer.lines() {
        let line = line.trim_matches(|c: char| c.is_whitespace() || c == '*');
        if let Some(value) = line.strip_prefix("CONFIDENCE:") {
            confidence = value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v));
        }
        if line == "GATE: pass" {
            state = "pass";
        } else if line == "GATE: fail" {
            state = "fail";
        }
    }
    (state, confidence)
}
fn overall(states: &[&str]) -> i32 {
    if states.is_empty() || states.iter().any(|s| !["pass", "fail"].contains(s)) {
        2
    } else if states.contains(&"fail") {
        1
    } else {
        0
    }
}
fn verdicts(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(argv, &[], &["json"])?;
    if args.has("help") {
        return Ok(out(peer::help("peer-review").to_vec()));
    }
    args.exact_operands(1)?;
    let repo = storage::repository(context, context.cwd)?;
    let directory = if args.operand(0).is_empty() {
        PathBuf::from(".oms/artifacts/review")
    } else {
        let path = Path::new(args.operand(0));
        if path.is_absolute() {
            path.strip_prefix(&repo)
                .map_err(|_| Error::new(2, "artifact directory escaped repository"))?
                .to_owned()
        } else {
            path.into()
        }
    };
    let pattern = regex::Regex::new(r"-(\d{8}T\d{6}Z-\d+)(?:-r(\d+))?\.md\z").unwrap();
    let mut candidates = Vec::new();
    for name in storage::names(&repo, &directory, ".md")? {
        if name.starts_with('_') || name.ends_with(".export.md") || name.ends_with(".import.md") {
            continue;
        }
        let Some(caps) = pattern.captures(&name) else {
            continue;
        };
        let run = caps[1].to_owned();
        let round = caps
            .get(2)
            .and_then(|m| m.as_str().parse::<usize>().ok())
            .unwrap_or(0);
        let modified = std::fs::symlink_metadata(repo.join(&directory).join(&name))?.modified()?;
        candidates.push((modified, run, round, name));
    }
    let run = candidates
        .iter()
        .max_by_key(|v| v.0)
        .map(|v| v.1.clone())
        .ok_or_else(|| Error::new(2, "no review artifacts"))?;
    let mut seats = BTreeMap::new();
    for (_, group, round, name) in candidates {
        if group != run {
            continue;
        }
        let provider = name.split('-').next().unwrap_or("").to_owned();
        if seats.get(&provider).is_none_or(|(old, _)| round >= *old) {
            seats.insert(provider, (round, name));
        }
    }
    let mut values = Vec::new();
    let mut states = Vec::new();
    let mut output = format!("run: {run}\n");
    for (provider, (round, name)) in seats {
        let bytes = storage::read(&repo, &directory.join(&name), false)?;
        let (answer, code) = artifact_sections(&String::from_utf8_lossy(&bytes), true);
        let (state, confidence) = verdict(&answer, code, true);
        states.push(state);
        output.push_str(&format!("{provider}: {state}\n"));
        values.push(obj([
            ("provider", provider.into()),
            ("verdict", state.into()),
            (
                "confidence",
                confidence.map_or(V::Null, |v| V::Num(v.to_string())),
            ),
            ("round", V::Num(round.to_string())),
            ("artifact", name.into()),
        ]));
    }
    let code = overall(&states);
    let value = obj([
        ("run", run.into()),
        ("overall", V::Num(code.to_string())),
        ("seats", V::Arr(values)),
    ]);
    Ok((
        code,
        if args.has("json") {
            json(&value)?
        } else {
            output.into_bytes()
        },
        Vec::new(),
    ))
}
fn clean(raw: &str, limit: usize) -> String {
    raw.chars()
        .filter(|c| !c.is_control() || "\t\r\n".contains(*c))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}
fn normalized(raw: &str) -> String {
    raw.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn parse_findings(answer: &str) -> (Vec<V>, usize) {
    let mut inside = false;
    let mut found = Vec::new();
    let mut rejected = 0;
    for line in answer.lines() {
        let line = line.trim();
        if !inside {
            if line.trim_start_matches('`').trim() == "findings" && line.starts_with("```") {
                inside = true;
            }
            continue;
        }
        if line.starts_with("```") {
            inside = false;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let Ok(mut row) = V::parse(line.as_bytes()) else {
            rejected += 1;
            continue;
        };
        let path = text(&row, "file")
            .strip_prefix("./")
            .unwrap_or(text(&row, "file"))
            .to_owned();
        let valid = !path.is_empty()
            && path.len() <= 1024
            && storage::relative(Path::new(&path)).is_ok()
            && clean(&path, 1024) == path
            && row.get("line").integer(true)
            && !clean(text(&row, "summary"), 500).is_empty()
            && ["blocking", "advisory"].contains(&row.get("severity").text().unwrap_or("blocking"))
            && (!row.has("quote") || row.get("quote").text().is_some());
        if !valid || found.len() >= 50 {
            rejected += 1;
            continue;
        }
        row.set("file", path);
        row.set("summary", clean(text(&row, "summary"), 500));
        row.set("quote", clean(text(&row, "quote"), 500));
        if !row.has("severity") {
            row.set("severity", "blocking");
        }
        found.push(row);
    }
    (found, rejected)
}
type Lines = BTreeMap<String, (BTreeSet<usize>, BTreeSet<usize>)>;
fn diff_lines(patch: &[u8]) -> Lines {
    let mut files = Lines::new();
    let mut current = None;
    let mut line = 0usize;
    let hunk = regex::Regex::new(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@").unwrap();
    for raw in String::from_utf8_lossy(patch).lines() {
        if raw.starts_with("diff --git ") {
            current = None;
        } else if let Some(name) = raw.strip_prefix("+++ b/") {
            let name = name.split('\t').next().unwrap_or("").to_owned();
            files.entry(name.clone()).or_default();
            current = Some(name);
        } else if raw.starts_with("--- ") {
            continue;
        } else if raw.starts_with("@@") {
            line = hunk
                .captures(raw)
                .and_then(|c| c[1].parse().ok())
                .unwrap_or(0);
        } else if let Some(path) = &current
            && line > 0
        {
            if raw.starts_with('+') {
                files.get_mut(path).unwrap().0.insert(line);
                line += 1;
            } else if raw.starts_with(' ') {
                files.get_mut(path).unwrap().1.insert(line);
                line += 1;
            }
        }
    }
    files
}
fn classify(row: &V, repo: &Path, diff: Option<&Lines>) -> (&'static str, V) {
    let Ok(bytes) = storage::read(repo, Path::new(text(row, "file")), false) else {
        return (
            "anchor-not-found",
            if text(row, "quote").is_empty() {
                V::Null
            } else {
                false.into()
            },
        );
    };
    let body = String::from_utf8_lossy(&bytes);
    let lines = body.lines().collect::<Vec<_>>();
    let number = crate::number(row.get("line")).unwrap_or(0.0) as usize;
    if number == 0 || number > lines.len() {
        return (
            "anchor-not-found",
            if text(row, "quote").is_empty() {
                V::Null
            } else {
                false.into()
            },
        );
    }
    let quote = normalized(text(row, "quote"));
    let found = if quote.is_empty() {
        V::Null
    } else {
        let found = lines[number.saturating_sub(4)..(number + 3).min(lines.len())]
            .iter()
            .any(|s| normalized(s).contains(&quote));
        if !found {
            return ("anchor-not-found", false.into());
        }
        true.into()
    };
    let Some(diff) = diff else {
        return ("unclassified", found);
    };
    let anchor = match diff.get(text(row, "file")) {
        Some((added, _)) if added.contains(&number) => "in-diff",
        Some((_, context)) if context.contains(&number) => "context",
        _ => "outside-scope",
    };
    (anchor, found)
}
pub(crate) fn finish(
    args: &Args,
    repo: &Path,
    dir: &Path,
    run: &str,
    seats: &[peer::Seat],
    mut output: String,
    context: &Context<'_>,
) -> Result<Output> {
    if !args.has("gate") {
        return Ok((
            i32::from(
                !seats
                    .iter()
                    .any(|s| s.complete && s.code == 0 && s.quality == "ok"),
            ),
            output.into_bytes(),
            Vec::new(),
        ));
    }
    let original = V::parse(&storage::read(
        repo,
        &dir.join(format!("_scope-{run}.json")),
        false,
    )?)?;
    let (current, patch) = scope(args, repo, context)?;
    let diff = diff_lines(&patch);
    let mut states = Vec::new();
    let mut seat_rows = Vec::new();
    for seat in seats {
        let (state, confidence) = verdict(&seat.answer, Some(seat.code), seat.complete);
        states.push(state);
        output.push_str(&format!("{}: {state}\n", seat.target.provider));
        seat_rows.push(obj([
            ("provider", seat.target.provider.clone().into()),
            ("verdict", state.into()),
            (
                "confidence",
                confidence.map_or(V::Null, |v| V::Num(v.to_string())),
            ),
            ("round", V::Num(seat.round.to_string())),
            (
                "artifact",
                seat.artifact
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
                    .into(),
            ),
        ]));
        let (findings, _) = if matches!(state, "pass" | "fail") {
            parse_findings(&seat.answer)
        } else {
            (Vec::new(), 0)
        };
        let mut seen = BTreeSet::new();
        for mut finding in findings {
            let ident = format!(
                "f-{}",
                &storage::hash(
                    format!(
                        "{}\0{}\0{}",
                        text(&finding, "file"),
                        normalized(text(&finding, "summary")),
                        normalized(text(&finding, "quote"))
                    )
                    .as_bytes()
                )[..12]
            );
            if !seen.insert(ident.clone()) {
                continue;
            }
            let (anchor, quote) = classify(
                &finding,
                repo,
                if args.has("no-diff") {
                    None
                } else {
                    Some(&diff)
                },
            );
            for (key, value) in [
                ("schema", 1usize.into()),
                ("kind", "review-finding".into()),
                ("id", ident.into()),
                ("run", run.into()),
                ("seat", seat.target.provider.clone().into()),
                ("anchor", anchor.into()),
                ("quote_found", quote),
                ("scope", format!("_scope-{run}.json").into()),
                ("diff_sha256", original.get("diff_sha256").clone()),
                ("ts", now().into()),
            ] {
                finding.set(key, value);
            }
            storage::append(
                repo,
                &dir.join(format!("_findings-{run}.jsonl")),
                &finding,
                context.interrupted,
            )?;
        }
    }
    let mut code = overall(&states);
    if original.get("diff_sha256") != current.get("diff_sha256")
        || original.get("head") != current.get("head")
    {
        code = 2;
        output.push_str("review scope changed during provider calls; gate incomplete\n");
    }
    if let Some(command) = crate::delegate::verification(args, repo, false)? {
        let checked = crate::delegate::verify(&command, repo, context)?;
        let mut exit = if checked.complete { checked.code } else { 125 };
        let after = scope(args, repo, context)?.0;
        if after.get("diff_sha256") != current.get("diff_sha256")
            || after.get("head") != current.get("head")
        {
            code = 2;
            exit = 125;
        }
        let artifact = dir.join(format!("_verify-{run}.md"));
        storage::write(
            repo,
            &artifact,
            format!(
                "# Gate verification\n\n## Output\n\n{}\n\n## Exit\n\n{exit}\n",
                String::from_utf8_lossy(&safety::redact(&checked.stdout)?)
            )
            .into_bytes(),
            false,
            context.interrupted,
        )?;
        let scope_path = dir.join(format!("_verify-scope-{run}.json"));
        let bytes = json(&current)?;
        let digest = storage::hash(&bytes);
        storage::write(repo, &scope_path, bytes, false, context.interrupted)?;
        peer::index(
            repo,
            &obj([
                ("schema", 1usize.into()),
                ("kind", "review-verify".into()),
                ("ts", now().into()),
                ("artifact_id", crate::id("art_")?.into()),
                ("artifact", artifact.to_string_lossy().into_owned().into()),
                ("source", scope_path.to_string_lossy().into_owned().into()),
                ("source_sha256", digest.into()),
                ("exit", V::Num(exit.to_string())),
            ]),
            context,
        )?;
        output.push_str(&format!(
            "verify: {} (exit {exit})\n",
            if exit == 0 { "pass" } else { "fail" }
        ));
        if exit != 0 && code != 2 {
            code = 1;
        }
    }
    if args.has("require-dispositions")
        || std::env::var_os("OMS_REVIEW_REQUIRE_DISPOSITIONS").is_some_and(|v| v == "1")
    {
        if load_findings(repo, dir)?
            .iter()
            .any(|r| text(r, "status") == "open")
        {
            code = 2;
            output.push_str("open findings remain; gate incomplete\n");
        }
    }
    for (option, kind) in [("security-audit", "security"), ("confirm", "confirmation")] {
        if args.has(option) {
            let paths = current
                .get("paths")
                .array()
                .iter()
                .filter_map(V::text)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if option == "security-audit" && risk_paths(&paths, &patch).is_empty() {
                continue;
            }
            let target = providers::Target::parse(args.get(option))?;
            let requested = providers::route(context, &target, "", "auto", "security-auditor")?;
            if args.has("writer")
                && providers::family(&requested.provider, &requested.model)
                    == providers::family(args.get("writer"), "")
            {
                return fail("security auditor must use a different model family from the writer");
            }
            let body = format!(
                "Independent {kind} of the review below. Labels are advisory and do not change the gate.\n{}",
                seats
                    .iter()
                    .map(|s| safety::untrusted(
                        &s.target.provider,
                        "review claims",
                        s.answer.as_bytes()
                    ))
                    .collect::<Result<Vec<_>>>()?
                    .join("\n")
            );
            let mut options = args.clone();
            options.remove("model");
            options.remove("fallback-model");
            options.remove("gate");
            options.set_value("role", "security-auditor");
            let body = format!(
                "{body}\n{}",
                safety::untrusted("repository", "git diff", &patch)?
            );
            let body = peer::prompt("agent-call", &options, repo, &body, None, context)?;
            let seat = peer::call(
                "agent-call",
                &options,
                repo,
                dir,
                &format!("{run}-{kind}"),
                &target,
                &body,
                0,
                context,
            )?;
            output.push_str(&format!(
                "{kind}: {}\n",
                if seat.complete && seat.code == 0 {
                    "recorded (advisory)"
                } else {
                    "incomplete (advisory)"
                }
            ));
        }
    }
    let verdicts = obj([
        ("run", run.into()),
        ("overall", V::Num(code.to_string())),
        ("seats", V::Arr(seat_rows)),
    ]);
    storage::write(
        repo,
        &dir.join(format!("_verdicts-{run}.json")),
        json(&verdicts)?,
        false,
        context.interrupted,
    )?;
    Ok((code, output.into_bytes(), Vec::new()))
}
fn load_findings(repo: &Path, dir: &Path) -> Result<Vec<V>> {
    let mut entries: BTreeMap<String, V> = BTreeMap::new();
    let mut sightings: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in storage::names(repo, dir, ".jsonl")?
        .into_iter()
        .filter(|s| s.starts_with("_findings-"))
    {
        for row in storage::load(repo, &dir.join(name))? {
            if text(&row, "kind") != "review-finding" {
                continue;
            }
            let ident = text(&row, "id").to_owned();
            sightings
                .entry(ident.clone())
                .or_default()
                .insert(text(&row, "run").into());
            entries.insert(ident, row);
        }
    }
    let events = storage::load(repo, &dir.join("_finding-events.jsonl"))?;
    for (ident, entry) in &mut entries {
        let history = events
            .iter()
            .filter(|r| text(r, "id") == ident)
            .cloned()
            .collect::<Vec<_>>();
        let last = history.last();
        let mut status = last.map_or("open", |r| text(r, "state"));
        if ["fixed", "verified"].contains(&status)
            && sightings[ident].iter().any(|run| {
                !last
                    .unwrap()
                    .get("runs")
                    .array()
                    .iter()
                    .any(|v| v.text() == Some(run))
            })
        {
            status = "open";
        }
        entry.set("status", status);
        entry.set(
            "runs",
            V::Arr(sightings[ident].iter().map(|s| s.as_str().into()).collect()),
        );
        entry.set("history", V::Arr(history));
    }
    Ok(entries.into_values().collect())
}
fn findings(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &[
            "repo",
            "artifact-dir",
            "status",
            "note",
            "reason",
            "commit",
            "run",
            "min-samples",
        ],
        &["json"],
    )?;
    if args.has("help") {
        return Ok(out(peer::help("peer-review").to_vec()));
    }
    args.exact_operands(2)?;
    if args.action() == "collect" {
        return fail("findings collect runs inside a gate review, not by hand");
    }
    let repo = storage::repository(context, &args.path("repo", context.cwd))?;
    let dir = peer::directory(&args, &repo, "review")?;
    let entries = load_findings(&repo, &dir)?;
    if args.action() == "list" {
        let entries = entries
            .into_iter()
            .filter(|r| !args.has("status") || text(r, "status") == args.get("status"))
            .collect::<Vec<_>>();
        return Ok(out(if args.has("json") {
            json(&V::Arr(entries))?
        } else {
            entries
                .iter()
                .map(|r| {
                    format!(
                        "{} {:12} {}:{} {}\n",
                        text(r, "id"),
                        text(r, "status"),
                        text(r, "file"),
                        r.get("line").py(),
                        text(r, "summary")
                    )
                })
                .collect::<String>()
                .into_bytes()
        }));
    }
    if ["rules", "suggest-rules"].contains(&args.action()) {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in &entries {
            if !text(row, "rule").is_empty() {
                *counts.entry(text(row, "rule").into()).or_default() += 1;
            }
        }
        let minimum = args.limit("min-samples", 3, 1, 10000)?;
        let values = counts
            .into_iter()
            .filter(|(_, n)| *n >= minimum)
            .map(|(rule, n)| {
                obj([
                    ("rule", rule.into()),
                    ("samples", V::Num(n.to_string())),
                    ("advisory", true.into()),
                ])
            })
            .collect();
        return Ok(out(json(&V::Arr(values))?));
    }
    let matches = entries
        .iter()
        .filter(|r| {
            text(r, "id") == args.operand(1)
                || args.operand(1).len() >= 6 && text(r, "id").starts_with(args.operand(1))
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return fail("unknown or ambiguous finding id");
    }
    let entry = matches[0];
    if args.action() == "show" {
        return Ok(out(json(entry)?));
    }
    let status = text(entry, "status");
    let last = entry.get("history").array().last();
    let mut event = obj([
        ("schema", 1usize.into()),
        ("kind", "review-finding-event".into()),
        ("id", entry.get("id").clone()),
        ("runs", entry.get("runs").clone()),
        ("ts", now().into()),
    ]);
    let state = match args.action() {
        "fix" => {
            if status != "open" {
                return fail("only an open finding can be marked fixed");
            }
            if args.has("commit") {
                event.set("commit", base(&repo, args.get("commit"), context)?);
            }
            event.set("note", args.get("note"));
            "fixed"
        }
        "dismiss" | "out-of-scope" => {
            safety::parent()?;
            if !["open", "fixed"].contains(&status) {
                return fail("finding has no open disposition");
            }
            event.set("reason", args.required("reason")?);
            if args.action() == "dismiss" {
                "dismissed"
            } else {
                "out-of-scope"
            }
        }
        "reopen" => {
            if status == "open" {
                return fail("finding is already open");
            }
            event.set("reason", args.required("reason")?);
            "open"
        }
        "verify" => {
            if status != "fixed" {
                return fail("mark finding fixed before verifying");
            }
            let fixed = last.ok_or_else(|| Error::new(2, "missing fix event"))?;
            let wanted = args.required("run")?;
            let receipts = storage::load(&repo, Path::new(".oms/artifacts/index.jsonl"))?;
            let receipt = receipts
                .iter()
                .rev()
                .find(|r| {
                    text(r, "kind") == "review-verify"
                        && (text(r, "artifact_id") == wanted
                            || text(r, "artifact") == wanted
                            || Path::new(text(r, "artifact"))
                                .file_name()
                                .is_some_and(|s| s == wanted))
                })
                .ok_or_else(|| Error::new(2, "no recorded review-verify receipt named by --run"))?;
            if receipt.get("exit") != &0usize.into() || text(receipt, "ts") <= text(fixed, "ts") {
                return fail("verification must pass after the fix record");
            }
            let bytes = storage::read(&repo, Path::new(text(receipt, "source")), false)?;
            if storage::hash(&bytes) != text(receipt, "source_sha256") {
                return fail("verification scope digest differs from the indexed receipt");
            }
            let scope = V::parse(&bytes)?;
            let original = V::parse(&storage::read(
                &repo,
                &dir.join(text(entry, "scope")),
                false,
            )?)?;
            if scope.get("tracked_clean") != &true.into()
                || scope.get("diff_included") != &true.into()
                || scope.get("base") != original.get("base")
                || original.get("diff_sha256") != entry.get("diff_sha256")
                || text(fixed, "commit").is_empty()
                || scope.get("head") != fixed.get("commit")
                || !scope
                    .get("paths")
                    .array()
                    .iter()
                    .any(|v| v == entry.get("file"))
            {
                return fail(
                    "verification scope does not bind the clean fixed commit, original base and cited file",
                );
            }
            event.set("verify_artifact_id", receipt.get("artifact_id").clone());
            event.set("verify_artifact", receipt.get("artifact").clone());
            event.set("commit", fixed.get("commit").clone());
            "verified"
        }
        _ => return fail("unknown findings action"),
    };
    event.set("state", state);
    storage::log(
        &repo,
        &dir.join("_finding-events.jsonl"),
        context.interrupted,
        |old, _| {
            let actual = storage::rows(old)?
                .into_iter()
                .filter(|r| r.get("id") == entry.get("id"))
                .collect::<Vec<_>>();
            if actual.last() != last {
                return fail("finding changed during transition; inspect it again");
            }
            let mut bytes = event.json(false)?;
            bytes.push(b'\n');
            Ok((bytes, ()))
        },
    )?;
    Ok(out(format!("{}: {state}\n", text(entry, "id")).into_bytes()))
}
fn glob(pattern: &str, value: &str) -> bool {
    let mut regex = String::from("\\A");
    for c in pattern.chars() {
        match c {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            c => regex.push_str(&regex::escape(&c.to_string())),
        }
    }
    regex.push_str("\\z");
    regex::Regex::new(&regex).is_ok_and(|r| r.is_match(value))
}
fn risk_paths(paths: &[String], patch: &[u8]) -> Vec<String> {
    const PATTERNS: &[&str] = &[
        "plugins/*",
        "hooks/*",
        "install.sh",
        "uninstall.sh",
        "scripts/install*",
        "scripts/uninstall*",
        "scripts/*hook*",
        "scripts/lib/*hook*",
        "scripts/lib/pi-oms-extension.mjs",
        "scripts/land.sh",
        "scripts/patch-land.sh",
        "scripts/patch-admit.sh",
        "scripts/lib/landing-capture.py",
        "scripts/provider-permissions.sh",
        ".github/workflows/*",
        "rules/*",
        "*permission*",
        "*settings*",
        "*policy*",
        "*secret*",
        "*scanner*",
        "*credential*",
    ];
    const ON_OPEN: &[&str] = &[
        ".vscode/tasks.json",
        ".envrc",
        ".husky/*",
        ".githooks/*",
        ".lefthook/*",
        "lefthook.y*ml",
        ".lefthook.y*ml",
        "lefthook-local.y*ml",
        ".lefthook-local.y*ml",
        ".overcommit.yml",
        ".pre-commit-config.yaml",
        ".devcontainer.json",
        ".devcontainer/*",
        ".gitlab-ci.yml",
        ".circleci/*",
        ".buildkite/*",
        ".azure-pipelines/*",
        "azure-pipelines.y*ml",
        "Jenkinsfile",
        ".github/actions/*",
        ".npmrc",
        ".yarnrc",
        ".yarnrc.yml",
        ".pnpmfile.cjs",
    ];
    let mut selected = paths
        .iter()
        .filter(|p| {
            PATTERNS.iter().any(|g| glob(g, p))
                || ON_OPEN
                    .iter()
                    .any(|g| glob(g, p) || glob(&format!("*/{g}"), p))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    let deletion=regex::Regex::new(r"\bos\.(?:remove|unlink)\s*\(|\bshutil\.rmtree\s*\(|\.unlink\s*\(|\brm\s+-[^\s]*[rf][^\s]*(?:\s|$)|\bfind\b[^\n]*\s-delete\b").unwrap();
    let lifecycle=regex::Regex::new(r#""(?:preinstall|install|postinstall|prepare|prepack|postpack|prepublish|prepublishOnly|simple-git-hooks|husky)"\s*:"#).unwrap();
    let mut current = String::new();
    let mut deleted = false;
    for line in String::from_utf8_lossy(patch).lines() {
        if line.starts_with("diff --git ") {
            current.clear();
            deleted = false;
        } else if line.starts_with("deleted file mode ") {
            deleted = true;
        } else if let Some(path) = line.strip_prefix("--- a/") {
            current = path.split('\t').next().unwrap_or("").into();
            if deleted
                && !current.ends_with(".md")
                && !current.ends_with(".rst")
                && !current.starts_with("docs/")
            {
                selected.insert(current.clone());
            }
        } else if let Some(path) = line.strip_prefix("+++ b/") {
            current = path.split('\t').next().unwrap_or("").into();
        } else if line.starts_with('+') && !current.is_empty() {
            let doc = current.ends_with(".md")
                || current.ends_with(".rst")
                || current.starts_with("docs/");
            if !doc && deletion.is_match(&line[1..])
                || current.rsplit('/').next() == Some("package.json")
                    && lifecycle.is_match(&line[1..])
            {
                selected.insert(current.clone());
            }
        }
    }
    selected.into_iter().collect()
}
fn advise(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &["repo", "base", "red-check", "red-timeout"],
        &["json"],
    )?;
    args.exact_operands(0)?;
    if args.has("red-check") {
        return fail("red-check requires native test-patch admission in a scratch worktree");
    }
    let repo = storage::repository(context, &args.path("repo", context.cwd))?;
    let base = base(&repo, args.value("base", "HEAD"), context)?;
    let bytes = process::git(
        context,
        &repo,
        &["diff", "--numstat", "--no-renames", &base, "--"],
    )?;
    let mut paths = Vec::new();
    let mut lines = 0usize;
    for line in String::from_utf8_lossy(&bytes).lines() {
        let cells = line.splitn(3, '\t').collect::<Vec<_>>();
        if cells.len() == 3 {
            lines += cells[..2]
                .iter()
                .filter_map(|s| s.parse::<usize>().ok())
                .sum::<usize>();
            paths.push(cells[2].to_owned());
        }
    }
    let config = providers::config(context, "review-depth.json")?;
    let contracts = paths
        .iter()
        .filter(|p| {
            config
                .get("contract_paths")
                .array()
                .iter()
                .filter_map(V::text)
                .any(|g| glob(g, p))
        })
        .cloned()
        .collect::<Vec<_>>();
    let patch = process::git(
        context,
        &repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            &base,
            "--",
        ],
    )?;
    let risks = risk_paths(&paths, &patch);
    let limit = |key: &str| crate::number(config.get(key)).unwrap_or(0.0) as usize;
    let mut depth = "none";
    let mut reasons = Vec::new();
    if !paths.is_empty()
        && (!risks.is_empty() || !contracts.is_empty() || lines > limit("none_lines"))
    {
        depth = "single";
        reasons.push(format!(
            "single: {lines} line(s) in {} file(s)",
            paths.len()
        ));
    }
    if !risks.is_empty()
        || !contracts.is_empty()
        || lines >= limit("gate_lines")
        || paths.len() >= limit("gate_files")
    {
        depth = "gate+security";
        reasons.push("gate+security: risk, contract or size threshold".into());
    }
    if !risks.is_empty() && !contracts.is_empty()
        || lines >= limit("council_lines")
        || paths.len() >= limit("council_files")
    {
        depth = "council";
        reasons.push("council: combined risk and contract or size threshold".into());
    }
    if reasons.is_empty() {
        reasons.push("none: no risk or contract path above the size floor".into());
    }
    let value = obj([
        ("schema", 1usize.into()),
        ("advisory", true.into()),
        ("depth", depth.into()),
        (
            "reasons",
            V::Arr(reasons.iter().map(|s| s.as_str().into()).collect()),
        ),
        (
            "risk_paths",
            V::Arr(risks.into_iter().map(V::from).collect()),
        ),
        (
            "contract_paths",
            V::Arr(contracts.into_iter().map(V::from).collect()),
        ),
        ("lines", V::Num(lines.to_string())),
        ("files", V::Num(paths.len().to_string())),
    ]);
    Ok(out(if args.has("json") {
        json(&value)?
    } else {
        format!("review depth: {depth}\n{}\nadvice only: this starts no review and changes no gate verdict\n",reasons.join("\n")).into_bytes()
    }))
}
