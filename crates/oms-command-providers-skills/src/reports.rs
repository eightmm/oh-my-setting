use crate::common::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

pub(crate) fn project(args: &Args, root: &Path, ctx: &Context<'_>) -> Result<Output> {
    if args.has("--budget") {
        return budget(args, root, ctx);
    }
    if args.has("--prompts") {
        return prompts(args, root);
    }
    if args.has("--team") {
        return team(root, ctx);
    }
    contract(args, root)
}
fn label(path: &Path, root: &Path, home: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.display().to_string())
        .or_else(|_| {
            path.strip_prefix(home)
                .map(|p| format!("~/{}", p.display()))
        })
        .unwrap_or_else(|_| {
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}
fn budget(args: &Args, root: &Path, ctx: &Context<'_>) -> Result<Output> {
    let home = home()?;
    let repo = ctx.repo(root.to_str())?;
    let mut chain = vec![root.to_path_buf()];
    let mut next = root;
    while next != repo {
        if let Some(parent) = next.parent() {
            if !parent.starts_with(&repo) {
                break;
            }
            chain.push(parent.to_path_buf());
            next = parent;
        } else {
            break;
        }
    }
    chain.reverse();
    let max_lines = std::env::var("OMS_RULES_MAX_LINES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 100000);
    let max_total = std::env::var("OMS_RULES_MAX_TOTAL_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(32768)
        .clamp(1, 16 * 1024 * 1024);
    let mut providers = vec![];
    let mut total_warnings = 0usize;
    for name in ["claude", "codex", "antigravity"] {
        let mut paths = VecDeque::new();
        let mut shadowed = vec![];
        match name {
            "codex" => {
                let base = env_path("CODEX_HOME", home.join(".codex"));
                for dir in std::iter::once(base).chain(chain.iter().cloned()) {
                    if let Some(path) = ["AGENTS.override.md", "AGENTS.md"]
                        .into_iter()
                        .map(|s| dir.join(s))
                        .find(|p| p.is_file())
                    {
                        paths.push_back((path, None::<String>, 0usize));
                    }
                }
            }
            "claude" => {
                paths.push_back((home.join(".claude/CLAUDE.md"), None, 0));
                let mut own = vec![];
                for dir in &chain {
                    for file in ["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"] {
                        let path = dir.join(file);
                        if path.is_file() {
                            own.push(path);
                        }
                    }
                }
                if own.is_empty() {
                    for dir in &chain {
                        paths.push_back((dir.join("AGENTS.md"), None, 0));
                    }
                } else {
                    for dir in &chain {
                        if dir.join("AGENTS.md").is_file() {
                            shadowed.push(json!({"path":label(&dir.join("AGENTS.md"),root,&home),"by":label(&own[0],root,&home)}));
                        }
                    }
                    for path in own {
                        paths.push_back((path, None, 0));
                    }
                }
                for dir in std::iter::once(home.join(".claude"))
                    .chain(chain.iter().map(|p| p.join(".claude")))
                {
                    if dir.join("rules").is_dir() {
                        for path in walk(&dir.join("rules"), 6, &[])? {
                            if path.extension().is_some_and(|s| s == "md") {
                                paths.push_back((path, None, 0));
                            }
                        }
                    }
                }
            }
            _ => {
                paths.push_back((home.join(".gemini/AGENTS.md"), None, 0));
                for dir in &chain {
                    for file in ["GEMINI.md", "AGENTS.md"] {
                        paths.push_back((dir.join(file), None, 0));
                    }
                }
            }
        }
        let mut seen = BTreeSet::new();
        let mut files = vec![];
        let mut warnings = vec![];
        let mut totals = [0usize; 3];
        let mut paragraphs: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
        while let Some((path, via, hop)) = paths.pop_front() {
            if !path.is_file() {
                continue;
            }
            let real = path.canonicalize()?;
            if !seen.insert(real.clone()) {
                continue;
            }
            if seen.len() > 512 {
                return Err(error("instruction source count exceeded budget"));
            }
            let display = label(&path, root, &home);
            let raw = read(&real, 1024 * 1024);
            if raw.is_err() {
                warnings.push(format!(
                    "{display} could not be read or is larger than 1048576 bytes; it is not counted"
                ));
                files.push(json!({"path":display,"via":via,"hop":hop,"symlink":std::fs::symlink_metadata(&path)?.file_type().is_symlink(),"bytes":null,"words":null,"lines":null,"unread":true}));
                continue;
            }
            let raw = raw?;
            let text = String::from_utf8_lossy(&raw);
            let words = text.split_whitespace().count();
            let lines = text.lines().count();
            totals[0] += raw.len();
            totals[1] += words;
            totals[2] += lines;
            if lines > max_lines {
                warnings.push(format!(
                    "{display} has {lines} lines, over the {max_lines}-line budget"
                ));
            }
            files.push(json!({"path":display,"via":via,"hop":hop,"symlink":std::fs::symlink_metadata(&path)?.file_type().is_symlink(),"bytes":raw.len(),"words":words,"lines":lines}));
            for paragraph in text.split("\n\n") {
                let normalized = paragraph
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                let count = normalized.split_whitespace().count();
                if count >= 12
                    && !paragraph.trim_start().starts_with("```")
                    && !paragraph.trim_start().starts_with("~~~")
                {
                    let entry = paragraphs
                        .entry(digest(normalized.as_bytes())[..12].to_owned())
                        .or_insert_with(|| (count, vec![]));
                    if !entry.1.contains(&display) {
                        entry.1.push(display.clone());
                    }
                }
            }
            if name == "claude" {
                let mut fenced = false;
                for line in text.lines() {
                    if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~")
                    {
                        fenced = !fenced;
                        continue;
                    }
                    if fenced {
                        continue;
                    }
                    let mut code = false;
                    let mut outside = String::new();
                    for c in line.chars() {
                        if c == '`' {
                            code = !code;
                        } else if !code {
                            outside.push(c);
                        }
                    }
                    for word in outside.split_whitespace() {
                        if let Some(reference) = word.strip_prefix('@') {
                            let reference = reference.trim_end_matches(['.', ',', ';', ':']);
                            let target = if let Some(relative) = reference.strip_prefix("~/") {
                                home.join(relative)
                            } else {
                                real.parent().unwrap_or(root).join(reference)
                            };
                            if target.is_file() {
                                if hop >= 4 {
                                    warnings.push(format!("{display} imports {} past 4 hops; Claude does not follow it",label(&target,root,&home)));
                                } else {
                                    paths.push_back((target, Some(display.clone()), hop + 1));
                                }
                            }
                        }
                    }
                }
            }
        }
        let duplicates = paragraphs
            .into_iter()
            .filter(|(_, (_, p))| p.len() > 1)
            .map(|(digest, (words, paths))| {
                warnings.push(format!(
                    "the same {words}-word paragraph is loaded from {}",
                    paths.join(" and ")
                ));
                json!({"digest":digest,"words":words,"start":"","files":paths})
            })
            .collect::<Vec<_>>();
        if totals[0] > max_total {
            warnings.push(format!(
                "{} bytes of always-loaded instructions, over the {max_total}-byte budget",
                totals[0]
            ));
        }
        for item in &shadowed {
            warnings.push(format!(
                "{} is not read: CLAUDE.md is present",
                item["path"].as_str().unwrap_or("AGENTS.md")
            ));
        }
        total_warnings += warnings.len();
        providers.push(json!({"provider":name,"basis":"local instruction files and installed provider conventions","files":files,"notes":[],"shadowed":shadowed,"total":{"files":files.len(),"bytes":totals[0],"words":totals[1],"lines":totals[2]},"duplicates":duplicates,"warnings":warnings}));
    }
    let result = json!({"schema":1,"kind":"oms-instruction-budget","project":root,"limits":{"max_lines_per_file":max_lines,"max_total_bytes":max_total,"import_hops":4},"providers":providers,"warnings":total_warnings});
    let mut output = if args.has("--json") {
        Output::json(result)
    } else {
        let mut text = String::new();
        for p in &providers {
            text.push_str(&format!(
                "{}: {} file(s), {} bytes, {} words, {} lines\n",
                p["provider"].as_str().unwrap_or(""),
                p["total"]["files"],
                p["total"]["bytes"],
                p["total"]["words"],
                p["total"]["lines"]
            ));
            for warning in p["warnings"].as_array().into_iter().flatten() {
                text.push_str(&format!("  warn: {}\n", warning.as_str().unwrap_or("")));
            }
        }
        text.push_str(&format!("budget: {total_warnings} warning(s)\n"));
        Output::text(text)
    };
    if args.has("--strict") && total_warnings > 0 {
        output.code = 1;
    }
    Ok(output)
}
fn prompts(args: &Args, root: &Path) -> Result<Output> {
    let raw = optional(&root.join("config/prompt-audit-patterns.txt"))?;
    let patterns = String::from_utf8_lossy(&raw)
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|s| {
            regex::RegexBuilder::new(s)
                .case_insensitive(true)
                .build()
                .ok()
        })
        .collect::<Vec<_>>();
    let mut files = vec![];
    let mut findings = 0usize;
    let mut global_words = 0usize;
    for (base, kind, depth) in [
        ("rules", "always", 1),
        ("roles", "on-demand", 1),
        ("templates", "always", 1),
        ("prompts", "unknown", 1),
        ("custom-skills", "on-demand", 2),
        ("templates/project-skills", "on-demand", 2),
    ] {
        let directory = root.join(base);
        if !directory.is_dir() {
            continue;
        }
        for path in walk(&directory, depth, &[])? {
            let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !filename.ends_with(".md")
                || (base == "templates" && !filename.contains("AGENTS"))
                || (depth == 2 && filename != "SKILL.md")
            {
                continue;
            }
            let raw = read(&path, 1024 * 1024)?;
            let body = String::from_utf8_lossy(&raw);
            let words = body.split_whitespace().count();
            let mut emphasis = BTreeMap::new();
            for pattern in &patterns {
                let hits = body.lines().filter(|l| pattern.is_match(l)).count();
                if hits > 0 {
                    emphasis.insert(pattern.as_str().to_owned(), hits);
                    findings += 1;
                }
            }
            if base == "rules" {
                global_words += words;
            }
            files.push(json!({"path":path.strip_prefix(root).map_err(|_|error("prompt outside root"))?,"injection":kind,"words":words,"bytes":raw.len(),"emphasis":emphasis}));
        }
    }
    let providers=["claude","codex","antigravity"].iter().map(|s|json!({"provider":s,"delivery":"link.sh","global_words":global_words,"with_largest_project_templates_words":null})).collect::<Vec<_>>();
    let row = json!({"schema":1,"kind":"oms-prompt-audit","root":root.file_name().map(|s|s.to_string_lossy()),"files":files,"duplicates":[],"providers":providers,"findings":findings});
    let mut output = if args.has("--json") {
        Output::json(row)
    } else {
        Output::text(format!(
            "prompt audit: {} files, {findings} findings\n",
            files.len()
        ))
    };
    if args.has("--strict") && findings > 0 {
        output.code = 1;
    }
    Ok(output)
}
fn contract(args: &Args, root: &Path) -> Result<Output> {
    let text = String::from_utf8(read(&root.join("PROJECT.md"), LIMIT)?)
        .map_err(|_| error("PROJECT.md must be UTF-8"))?;
    let mut warnings = vec![];
    let links = regex::Regex::new(r"\[[^\]]*\]\(([^)]+)\)").expect("static link matcher");
    for captures in links.captures_iter(&text) {
        let target = captures[1].split('#').next().unwrap_or("");
        if target.is_empty() || target.contains("://") || target.starts_with('#') {
            continue;
        }
        if safe_relative(target).is_err() || !root.join(target).exists() {
            warnings.push(format!(
                "warn: relative Markdown target is missing or outside project: {target}"
            ));
        }
    }
    let (sections, _) = oms_contract_markdown::sections(&text);
    let verification = oms_contract_markdown::section_text(&sections, &["Verification"]);
    if verification.trim().is_empty() {
        warnings.push("warn: PROJECT.md has no Verification section".into());
    }
    let bad = !warnings.is_empty();
    warnings.push(
        if bad {
            "project-doctor: contract ok (with warnings)"
        } else {
            "project-doctor: contract ok"
        }
        .into(),
    );
    let mut out = Output::text(format!("{}\n", warnings.join("\n")));
    if bad && args.has("--strict") {
        out.code = 1;
    }
    Ok(out)
}
fn team(root: &Path, ctx: &Context<'_>) -> Result<Output> {
    let raw = optional(&root.join(".oms-team.json"))?;
    if raw.is_empty() {
        return Ok(Output::text(
            "note: no .oms-team.json in this project; nothing declared, nothing checked\n",
        ));
    }
    let want = json(&raw)?;
    if want["schema"] != 1
        || want.as_object().is_none_or(|m| {
            m.keys().any(|k| {
                !["schema", "min_oms_version", "capabilities", "digests"].contains(&k.as_str())
            })
        })
    {
        return Err(error(".oms-team.json is not a valid baseline"));
    }
    let mut unmet = vec![];
    if let Some(version) = want.get("min_oms_version") {
        let parse = |v: &str| -> Result<Vec<u64>> {
            let parts = v
                .trim()
                .split('.')
                .map(|s| {
                    s.parse::<u64>()
                        .map_err(|_| error("version must look like 1.2.3"))
                })
                .collect::<Result<Vec<_>>>()?;
            if parts.is_empty() || parts.len() > 4 {
                return Err(error("invalid version"));
            }
            Ok(parts)
        };
        let minimum = parse(
            version
                .as_str()
                .ok_or_else(|| error("invalid minimum version"))?,
        )?;
        let installed = optional(&ctx.source.join("VERSION"))?;
        if !parse(String::from_utf8_lossy(&installed).trim()).is_ok_and(|v| v >= minimum) {
            unmet.push("drift: OMS version is older or unknown; run: oms update".into());
        }
    }
    if let Some(required) = want.get("capabilities") {
        let required = required
            .as_array()
            .filter(|a| a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty())))
            .ok_or_else(|| error("capabilities must be a list of names"))?;
        let path = env_path(
            "OMS_CAPABILITY_RECEIPT",
            env_path("XDG_CONFIG_HOME", home()?.join(".config"))
                .join("oh-my-setting/capabilities.json"),
        );
        let raw = optional(&path)?;
        let current = json(&raw).unwrap_or(Value::Null);
        let installed = current["plan"]["requested"].as_array();
        for name in required {
            if installed.is_none_or(|a| !a.contains(name) && !a.contains(&json!("full"))) {
                unmet.push(format!("drift: capability {} not installed; run: oms install-profile --apply --add --profile {}",name.as_str().unwrap_or("unknown"),name.as_str().unwrap_or("unknown")));
            }
        }
    }
    if want.get("digests").is_some_and(|v| !v.is_object()) {
        return Err(error("digests must map names to sha256 strings"));
    }
    for (name, expected) in want["digests"].as_object().into_iter().flatten() {
        if !expected.is_string() {
            return Err(error("digests must map names to sha256 strings"));
        }
        unmet.push(format!("drift: digest {name} requires native install fingerprint evidence; see: oms status --fingerprint"));
    }
    let failed = !unmet.is_empty();
    unmet.push(
        if failed {
            "team-baseline: item(s) unmet"
        } else {
            "team-baseline: ok"
        }
        .into(),
    );
    let mut out = Output::text(format!("{}\n", unmet.join("\n")));
    out.code = i32::from(failed);
    Ok(out)
}
