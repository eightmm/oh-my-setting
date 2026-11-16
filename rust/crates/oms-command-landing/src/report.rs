//! Allowlisted report metadata and privacy checks before preview/publication.
use crate::{
    Context, Error, Output, Result, V,
    cli::Options,
    git::{Git, process},
    require, safe, storage, string,
};
use std::{
    ffi::OsString,
    io::{IsTerminal, Read},
    net::IpAddr,
    time::Duration,
};

pub fn github_remote(raw: &str) -> Result<String> {
    require(
        !raw.is_empty() && !raw.chars().any(char::is_control),
        "remote URL is empty or malformed",
    )?;
    let path = if let Some(rest) = raw.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = raw.strip_prefix("https://") {
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| Error::new(2, "GitHub remote must name one owner/repository"))?;
        require(
            ["github.com", "github.com:443"]
                .iter()
                .any(|h| authority.eq_ignore_ascii_case(h)),
            "only github.com HTTPS or SSH remotes are accepted",
        )?;
        path
    } else if let Some(rest) = raw.strip_prefix("ssh://") {
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| Error::new(2, "malformed GitHub remote"))?;
        let authority = authority.strip_prefix("git@").unwrap_or(authority);
        require(
            ["github.com", "github.com:22"]
                .iter()
                .any(|h| authority.eq_ignore_ascii_case(h)),
            "only github.com HTTPS or SSH remotes are accepted",
        )?;
        path
    } else {
        return Err(Error::new(
            2,
            "only github.com HTTPS or SSH remotes are accepted",
        ));
    };
    require(
        !path.contains(['?', '#', '@']),
        "GitHub remote must not carry query or fragment data",
    )?;
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    require(
        parts.len() == 2,
        "GitHub remote must have exactly owner/repository",
    )?;
    github_slug(parts[0], parts[1])
}
pub fn github_slug(owner: &str, repo: &str) -> Result<String> {
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    for p in [owner, repo] {
        require(
            !matches!(p, "" | "." | "..")
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "GitHub remote must name one owner/repository",
        )?;
    }
    Ok(format!("{owner}/{repo}"))
}
fn gh(c: &Context<'_>, args: &[&str]) -> Result<crate::git::Execution> {
    let bin = std::env::var_os("OMS_GH_BIN").unwrap_or_else(|| OsString::from("gh"));
    let argv = std::iter::once(bin)
        .chain(args.iter().map(OsString::from))
        .collect();
    process(
        c.cwd,
        argv,
        None,
        Duration::from_secs(60),
        c.interrupted,
        &[],
    )
}
fn percent(text: &str) -> String {
    let mut s = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                s.push(b as char)
            }
            b' ' => s.push('+'),
            _ => s += &format!("%{b:02X}"),
        }
    }
    s
}
fn private_text(text: &str) -> Result<()> {
    safe(text.as_bytes())
        .map_err(|_| Error::new(2, "sensitive-content scanner refused the draft"))?;
    let absolute =
        regex::Regex::new(r"(?:^|[^\w/])(?:/(?:[^\s/]+/)+[^\s]*|[A-Za-z]:[\\/]|~/|\\\\[^\s]+)")
            .unwrap();
    require(!absolute.is_match(text), "absolute path in user text")?;
    let words = regex::Regex::new(r"[A-Za-z0-9_.:-]+").unwrap();
    let host = rustix::system::uname();
    let host = host.nodename().to_string_lossy().to_lowercase();
    let generic = [
        "oms",
        "os",
        "linux",
        "wsl",
        "macos",
        "bash",
        "python",
        "version",
        "commit",
        "fingerprint",
        "verb",
        "exit",
        "failure",
        "code",
        "repeat",
        "count",
        "unavailable",
        "bug",
        "report",
        "title",
    ];
    if host.len() >= 4 && !generic.contains(&host.as_str()) {
        require(
            !words.find_iter(text).any(|w| {
                w.as_str()
                    .trim_end_matches('.')
                    .trim_end_matches(':')
                    .eq_ignore_ascii_case(&host)
            }),
            "local hostname in user text",
        )?;
    }
    for word in words.find_iter(text) {
        let word = word.as_str().trim_end_matches('.');
        require(
            word == "::" || word.parse::<IpAddr>().is_err(),
            "IP address in user text",
        )?;
    }
    let host=regex::Regex::new(r"(?i)\b(?:[a-z0-9-]+\.)+(?:com|org|net|edu|gov|io|dev|local|internal|lan)\b|(?:https?://|\b(?:host(?:name)?|server)\s*[:=]\s*)(?:[a-z0-9-]+\.)+[a-z0-9-]+").unwrap();
    require(!host.is_match(text), "hostname in user text")?;
    require(
        !text.chars().any(|c| c < ' ' && c != '\n' && c != '\t'),
        "control character in user text",
    )?;
    Ok(())
}
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(
        o.words.len() == 1 && ["bug", "feature"].contains(&o.words[0].as_str()),
        "report requires bug or feature",
    )?;
    let kind = o.action("");
    require(
        kind != "feature" || !o.get("title").trim().is_empty(),
        "feature requires --title",
    )?;
    require(
        kind != "feature" || !o.has("from-failure"),
        "--from-failure requires bug",
    )?;
    if o.has("from-failure") {
        require(
            crate::hex(o.get("from-failure"), 16),
            "malformed failure fingerprint",
        )?;
    }
    let title = o.value("title", "OMS bug report").replace("\r\n", "\n");
    require(
        !title.trim().is_empty() && !title.contains(['\r', '\n']),
        "title must be a nonempty single line",
    )?;
    let message = if o.has("message") {
        o.get("message").to_owned()
    } else if std::io::stdin().is_terminal() {
        String::new()
    } else {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    };
    let message = message.replace("\r\n", "\n");
    private_text(&format!("{title}\n{message}"))?;
    let git = Git::new(c.source, c.interrupted);
    let slug = if let Ok(repo) = std::env::var("OMS_REPORT_REPO") {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| Error::new(2, "OMS_REPORT_REPO must be owner/name"))?;
        github_slug(owner, name)?
    } else {
        github_remote(&git.text(&["remote", "get-url", "origin"])?)?
    };
    let version = String::from_utf8(storage::read(
        c.source,
        std::path::Path::new("VERSION"),
        false,
    )?)
    .map_err(|_| Error::new(2, "invalid OMS version"))?
    .trim()
    .to_owned();
    require(
        regex::Regex::new(r"^[0-9]+(?:\.[0-9]+){1,2}(?:[-+][A-Za-z0-9.-]+)?$")
            .unwrap()
            .is_match(&version),
        "invalid OMS version",
    )?;
    let commit = git
        .text(&["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|_| "unavailable".into());
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("microsoft")
    {
        "wsl"
    } else {
        "linux"
    };
    let bash = process(
        c.cwd,
        vec![
            "bash".into(),
            "-c".into(),
            "printf '%s.%s' \"${BASH_VERSINFO[0]}\" \"${BASH_VERSINFO[1]}\"".into(),
        ],
        None,
        Duration::from_secs(10),
        c.interrupted,
        &[],
    )?;
    let bash = String::from_utf8_lossy(&bash.stdout);
    require(
        bash.split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .is_some_and(|v| v >= 5),
        "Bash 5.0 or newer is required",
    )?;
    let mut body = format!("OMS: {version} ({commit})\nOS: {os}\nBash: {bash}\nPython: retired\n");
    if o.has("from-failure") {
        let row = c.services.failure_report(c.cwd, o.get("from-failure"))?;
        for (label, key) in [
            ("Fingerprint", "fingerprint"),
            ("Verb", "verb"),
            ("Exit", "exit"),
            ("Failure code", "failure_code"),
            ("Repeat count", "count"),
        ] {
            body += &format!("{label}: {}\n", row.get(key).py());
        }
    }
    if !message.is_empty() {
        body += &format!("\n{message}\n");
    }
    safe(body.as_bytes())
        .map_err(|_| Error::new(2, "sensitive-content scanner refused the report metadata"))?;
    let mut out = Output::text(format!("Title: {title}\n\n{body}"));
    if o.has("dry-run") {
        return Ok(out);
    }
    let prefix = format!("https://github.com/{slug}/issues/new?");
    let url_title: String = title.chars().take(200).collect();
    let mut url_body: String = body.chars().take(8000).collect();
    let suffix = "\n[Draft shortened for URL; use --yes with gh for the full draft.]";
    require(
        prefix.len() + percent(&url_title).len() + percent(suffix).len() + 13 <= 8000,
        "destination exceeds URL capacity",
    )?;
    while prefix.len() + percent(&url_title).len() + percent(&url_body).len() + 13 > 8000 {
        url_body = url_body
            .chars()
            .take(url_body.chars().count().saturating_sub(100))
            .collect();
    }
    if url_body != body {
        while prefix.len()
            + percent(&url_title).len()
            + percent(&format!("{url_body}{suffix}")).len()
            + 13
            > 8000
        {
            url_body = url_body
                .chars()
                .take(url_body.chars().count().saturating_sub(100))
                .collect();
        }
        url_body += suffix;
    }
    let url = format!(
        "{prefix}title={}&body={}",
        percent(&url_title),
        percent(&url_body)
    );
    if !o.has("yes") {
        out.stdout.extend_from_slice(
            format!("Not published: explicit --yes or interactive y required.\n{url}\n").as_bytes(),
        );
        out.code = 1;
        return Ok(out);
    }
    if !gh(c, &["auth", "status"]).is_ok_and(|r| r.code == 0) {
        out.stdout.extend_from_slice(
            format!("Not published: gh unavailable or unauthenticated.\n{url}\n").as_bytes(),
        );
        return Ok(out);
    }
    let tmp = storage::Temp::new("oms-report")?;
    let draft = tmp.path.join("draft");
    storage::replace(&tmp.path, Path::new("draft"), None, body.as_bytes())?;
    let draft = draft.to_string_lossy();
    let mut command;
    let mut target = None;
    if o.has("from-failure") {
        let found = gh(
            c,
            &[
                "issue",
                "list",
                "--repo",
                &slug,
                "--state",
                "open",
                "--search",
                &format!("{} in:body", o.get("from-failure")),
                "--json",
                "number",
                "--limit",
                "1",
            ],
        )?;
        require(
            found.code == 0,
            "duplicate search failed; nothing published",
        )?;
        let rows = V::parse(&found.stdout)?;
        require(
            matches!(rows, V::Arr(_)) && rows.array().len() <= 1,
            "invalid duplicate search response; nothing published",
        )?;
        if let Some(row) = rows.array().first() {
            require(
                row.get("number").integer(true),
                "invalid duplicate search response; nothing published",
            )?;
            target = Some(row.get("number").py());
        }
    }
    if let Some(number) = target.as_deref() {
        command = vec![
            "issue",
            "comment",
            number,
            "--repo",
            &slug,
            "--body-file",
            &draft,
        ];
    } else {
        let label = if kind == "bug" { "bug" } else { "enhancement" };
        let labels = gh(
            c,
            &[
                "label", "list", "--repo", &slug, "--search", label, "--json", "name", "--limit",
                "100",
            ],
        )?;
        require(labels.code == 0, "label lookup failed; nothing published")?;
        let rows = V::parse(&labels.stdout)?;
        require(
            matches!(rows, V::Arr(_))
                && rows.array().iter().all(|r| r.get("name").text().is_some()),
            "invalid label response; nothing published",
        )?;
        command = vec![
            "issue",
            "create",
            "--repo",
            &slug,
            "--title",
            &title,
            "--body-file",
            &draft,
        ];
        if rows.array().iter().any(|v| string(v, "name") == label) {
            command.extend(["--label", label]);
        }
    }
    let posted = gh(c, &command)?;
    require(
        posted.code == 0,
        "publication failed or outcome unknown; inspect GitHub before retrying",
    )?;
    out.stdout.extend(posted.stdout);
    Ok(out)
}
use std::path::Path;
