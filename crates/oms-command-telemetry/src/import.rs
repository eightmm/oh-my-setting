use super::*;
use std::os::unix::fs::PermissionsExt;
const USAGE: &str = "Usage: import-agent-result.sh --kind KIND --provider PROVIDER --file PATH [options]\n\nImport an externally run agent answer into the same artifact/index format used\nby the multi-agent harness. Use this after an --export-only prompt is answered\nby a registered or external agent outside the current policy boundary.\n\nOptions:\n  --kind KIND          ask, review, delegate, or call. Required.\n  --provider NAME      Registered agent transport. Required.\n  --file PATH          File containing the provider answer. Required.\n  --prompt-file PATH   Exported prompt artifact or prompt text file to include.\n  --repo PATH          Repo/directory for artifact index. Default: PWD.\n  --artifact-dir PATH  Destination artifact dir. Default: REPO/.oms/artifacts/KIND.\n  --exit N             Imported provider exit code. Default: 0.\n  -h, --help           Show help.\n";
pub(super) fn run(
    args: &[OsString],
    caller: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let result = execute(args, caller, interrupted);
    match result {
        Ok((stdout, stderr)) => (0, stdout.into_bytes(), stderr.into_bytes()),
        Err(e) => (
            e.code,
            Vec::new(),
            format!("error: {}\n", e.message).into_bytes(),
        ),
    }
}
fn execute(args: &[OsString], caller: &Path, interrupted: &AtomicBool) -> Result<(String, String)> {
    let mut values = BTreeMap::<String, OsString>::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let name = arg.to_string_lossy();
        if ["-h", "--help"].contains(&name.as_ref()) {
            return Ok((USAGE.into(), String::new()));
        }
        let suffix = match name.as_ref() {
            "--kind" | "--provider" => "value",
            "--file" | "--prompt-file" | "--repo" | "--artifact-dir" => "path",
            "--exit" => "number",
            _ => return Err(invalid(format!("unknown argument: {name}"))),
        };
        let value = iter
            .next()
            .ok_or_else(|| invalid(format!("{name} requires {suffix}")))?;
        values.insert(name.into_owned(), value.clone());
    }
    let get = |name: &str| {
        values
            .get(name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let kind = get("--kind");
    if kind.is_empty() {
        return Err(invalid("--kind is required"));
    }
    if !["ask", "review", "delegate", "call"].contains(&kind.as_str()) {
        return Err(invalid("--kind must be ask, review, delegate, or call"));
    }
    let provider = get("--provider");
    if provider.is_empty() {
        return Err(invalid("--provider is required"));
    }
    let provider = normalize_provider(&provider)?;
    let input = get("--file");
    if input.is_empty() {
        return Err(invalid("--file is required"));
    }
    let options = Options {
        repo: values.get("--repo").map(PathBuf::from),
        ..Options::default()
    };
    let (repo, _) = crate::repository(&options, caller)?;
    let root = Root::open(&repo).map_err(StoreError::from)?;
    let input = caller.join(values.get("--file").unwrap());
    let answer = producer::payload(&root, &repo, &input)
        .map_err(|_| invalid(format!("result file not found: {}", input.display())))?;
    if answer.is_empty() {
        return Err(invalid(format!(
            "result file is empty: {}",
            input.display()
        )));
    }
    let prompt_path = values
        .get("--prompt-file")
        .filter(|v| !v.is_empty())
        .map(|s| caller.join(s));
    let prompt = prompt_path
        .as_ref()
        .map(|p| {
            producer::payload(&root, &repo, p)
                .map_err(|_| invalid(format!("prompt file not found: {}", p.display())))
        })
        .transpose()?;
    let exit_raw = values
        .get("--exit")
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "0".into());
    let exit = exit_raw
        .parse::<i32>()
        .ok()
        .filter(|n| *n >= 0 && !exit_raw.is_empty() && exit_raw.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| invalid("--exit must be a non-negative integer"))?;
    cancelled(interrupted)?;
    let mut warnings = String::new();
    if sensitive(&String::from_utf8_lossy(&answer)) {
        warnings.push_str("warning: imported result contains sensitive-looking content; review before sharing or promoting to memory\n");
    }
    let directory = values
        .get("--artifact-dir")
        .filter(|s| !s.is_empty())
        .map(|p| caller.join(p))
        .unwrap_or_else(|| repo.join(format!(".oms/artifacts/{kind}")));
    let directory = paths::target(&root, &repo, &directory)?;
    let slug = prompt_path
        .as_ref()
        .map(|p| slug(&p.file_name().unwrap_or_default().to_string_lossy()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "imported-result".into());
    let now = producer::now_text()?;
    let stamp = format!("{}-{}", now.replace(['-', ':'], ""), std::process::id());
    let artifact = directory.join(format!("{provider}-{slug}-{stamp}.import.md"));
    let mut body = format!(
        "# {provider} {kind} import\n\n- imported: {now}\n- repo: {} (path omitted)\n",
        repo.file_name().unwrap_or_default().to_string_lossy()
    )
    .into_bytes();
    if let Some(path) = &prompt_path {
        let label = path
            .strip_prefix(&repo)
            .unwrap_or_else(|_| Path::new(path.file_name().unwrap_or_default()));
        body.extend_from_slice(format!("- source-prompt: {}\n", label.display()).as_bytes());
    }
    body.extend_from_slice(b"\n## Prompt\n\n");
    body.extend_from_slice(
        prompt
            .as_deref()
            .unwrap_or(b"Prompt not supplied at import time.\n"),
    );
    body.extend_from_slice(b"\n\n## Output\n\n");
    body.extend_from_slice(&answer);
    body.extend_from_slice(format!("\n\n## Exit\n\n{exit}\n").as_bytes());
    if body.len() > MAX_INPUT_BYTES {
        return Err(invalid("imported artifact exceeds byte limit"));
    }
    ensure_ignore(&repo, interrupted)?;
    let _guard = acquire(&artifact, interrupted)?;
    // Refuse a same-second name collision rather than overwriting a published
    // artifact referenced by an earlier import.
    paths::validate(&root, &artifact, true)?;
    root.atomic_mutate(
        &artifact,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: MAX_INPUT_BYTES,
            },
            create_parent: true,
            expected_exists: Some(false),
            max_output: MAX_INPUT_BYTES,
            ..MutationOptions::default()
        },
        |_| {
            cancelled(interrupted)?;
            Ok::<_, StoreError>(body)
        },
    )
    .map_err(StoreError::from)?;
    let mut request = AppendArtifactRequest {
        kind: format!("{kind}-import"),
        provider: provider.clone(),
        exit_code: exit,
        artifact: Some(artifact.clone()),
        prompt: prompt_path.clone(),
        source_artifact: prompt_path,
        ..AppendArtifactRequest::default()
    };
    if std::env::var("OMS_OPERATION_ID")
        .unwrap_or_default()
        .is_empty()
    {
        request
            .env_overrides
            .insert("OMS_OPERATION_ID".into(), format!("import-{stamp}"));
    }
    // The legacy importer leaves its artifact usable even if index accounting
    // fails. Surface the failure while preserving that recovery path.
    if let Err(e) = append_artifact_index(&repo, &request, interrupted) {
        writeln!(warnings, "warning: {e}").unwrap();
    }
    let quality = quality(&answer);
    if quality != "ok" {
        writeln!(warnings,"warning: imported result reads as a non-answer ({quality}); downstream consumers see only its recorded exit ({exit})").unwrap();
    }
    Ok((
        format!("imported: {provider} -> {}\n", artifact.display()),
        warnings,
    ))
}
fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if separator && !out.is_empty() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    out.truncate(out.len().min(48));
    out
}
fn normalize_provider(name: &str) -> Result<String> {
    let canonical = match name {
        "agy" => "antigravity",
        "cursor-agent" => "cursor",
        "grok-build" => "grok",
        "gemini-cli" => "gemini",
        "qwen-code" => "qwen",
        "opencode2" => "opencode",
        "dsh" | "deepseek-harness" => "deepseek",
        "mistral-vibe" => "vibe",
        "github-copilot" => "copilot",
        "factory-droid" => "droid",
        "kiro-cli" => "kiro",
        "cn" => "continue",
        s => s,
    };
    if [
        "codex",
        "claude",
        "antigravity",
        "cursor",
        "grok",
        "gemini",
        "qwen",
        "opencode",
        "deepseek",
        "vibe",
        "pi",
        "copilot",
        "droid",
        "aider",
        "muse",
        "kimi",
        "kiro",
        "continue",
        "goose",
        "cline",
        "kilo",
        "openhands",
        "auggie",
    ]
    .contains(&canonical)
    {
        return Ok(canonical.into());
    }
    if !canonical.is_empty()
        && canonical.len() <= 64
        && !canonical.contains("..")
        && canonical
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && canonical.as_bytes()[0].is_ascii_alphanumeric()
    {
        let config = std::env::var_os("OMS_PROVIDER_ADAPTER_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".config")))
                    .map(|v| v.join("oh-my-setting/provider-adapters"))
            });
        let mut dirs: Vec<_> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        if let Some(dir) = config {
            dirs.insert(0, dir);
        }
        if dirs.iter().any(|dir| {
            let p = dir.join(format!("oms-agent-adapter-{canonical}"));
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        }) {
            return Ok(canonical.into());
        }
    }
    Err(invalid(format!("unsupported provider: {name}")))
}
fn quality(raw: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(raw);
    let mut lines: Vec<_> = text.lines().collect();
    if let Some(pos) = lines.iter().rposition(|s| *s == "## Output") {
        lines = lines[pos + 1..].to_vec();
    }
    if lines.iter().any(|s| {
        s.trim().starts_with("[agy] print timeout after ")
            && s.trim()
                .ends_with(" with turn in progress; returning partial output")
    }) {
        return "truncated";
    }
    let stop = regex::Regex::new(
        r"^stop-reason: provider=\S+ reason=(\S+) subtype=\S+ is_error=([01])(?: category=\S+)?$",
    )
    .unwrap();
    for line in &lines {
        if let Some(c) = stop.captures(line.trim()) {
            if &c[2] == "1" || &c[1] == "refusal" {
                return "blocked";
            }
            if ["max_tokens", "stream_truncated"].contains(&&c[1]) {
                return "truncated";
            }
            break;
        }
    }
    let noise=regex::Regex::new(r"^\s*(model-route:|model-result:|usage detail:|tokens used|\[REDACTED|OpenAI Codex|\[untrusted |\[end untrusted |workdir:|model:|provider:|approval:|sandbox:|reasoning effort:|reasoning summaries:|session id:|-{3,}$|user$|DRY RUN)").unwrap();
    let refusal=regex::Regex::new(r"(?i)^(jetski: no output produced\b|add an allow-rule\b|alternatively, re-run with\b|.*\bcommand not found\b|(error|fatal)[: ].*\b(permission|not authenticated|not logged in|unauthorized|invalid api key|quota|rate limit)\b)").unwrap();
    let heading=regex::Regex::new(r"(?i)^(?:#{1,6}\s*)?(?:Answer|Alternatives|Tradeoffs|Evidence|Counterargument|Risks|Recommendation|Verification|Changed from previous round|Remaining disagreements):\s*$").unwrap();
    let mut selected = Vec::new();
    let mut skip = false;
    for line in lines {
        if skip {
            skip = false;
            continue;
        }
        if [
            "served model",
            "configured model",
            "tokens used",
            "cost usd",
        ]
        .contains(&line.trim())
        {
            skip = true;
            continue;
        }
        if !line.trim().is_empty() && !noise.is_match(line) && !stop.is_match(line.trim()) {
            selected.push(line.trim());
        }
    }
    if selected.is_empty() || selected.iter().all(|s| heading.is_match(s)) {
        "empty"
    } else if selected.iter().all(|s| refusal.is_match(s)) {
        "blocked"
    } else {
        "ok"
    }
}
