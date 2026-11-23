//! Canonical same-repository GitHub references for the opt-in GitHub project
//! mode. Parsing only, except [`declaration`], which reads the local and
//! tracked declarations (bounded, through `git` for HEAD). Nothing here
//! contacts GitHub. A reference names shared work; it never grants acceptance.
#![forbid(unsafe_code)]

pub mod declaration;
pub mod freshness;

use std::fmt;

/// Fixed-text failure; callers map it to exit 2. Messages never echo raw
/// input except validated, capped slug spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
}
impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(Error::new(message)) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slug {
    pub owner: String,
    pub repo: String,
}
const SLUG: &str = "GitHub remote must name one owner/repository";
const HOSTS: &str = "only github.com HTTPS or SSH remotes are accepted";
impl Slug {
    fn checked(owner: &str, repo: &str, message: &str) -> Result<Self> {
        for p in [owner, repo] {
            require(
                !matches!(p, "" | "." | "..")
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
                message,
            )?;
        }
        Ok(Self {
            owner: owner.into(),
            repo: repo.into(),
        })
    }
    /// Exactly `owner/repo`; no `.git` stripping, URL or extra segments.
    pub fn parse(raw: &str) -> Result<Self> {
        const MESSAGE: &str = "GitHub slug must be owner/repo";
        let (owner, repo) = raw.split_once('/').ok_or_else(|| Error::new(MESSAGE))?;
        Self::checked(owner, repo, MESSAGE)
    }
    /// Mirrors oms-command-landing `report.rs` `github_remote` (github.com
    /// SSH, HTTPS and ssh:// forms; no query, fragment or userinfo in the path)
    /// and `github_slug` (strip one `.git`; `[A-Za-z0-9_.-]`, never `.`/`..`).
    pub fn from_remote(raw: &str) -> Result<Self> {
        require(
            !raw.is_empty() && !raw.chars().any(char::is_control),
            "remote URL is empty or malformed",
        )?;
        let path = if let Some(rest) = raw.strip_prefix("git@github.com:") {
            rest
        } else if let Some(rest) = raw.strip_prefix("https://") {
            let (authority, path) = rest.split_once('/').ok_or_else(|| Error::new(SLUG))?;
            require(
                ["github.com", "github.com:443"]
                    .iter()
                    .any(|h| authority.eq_ignore_ascii_case(h)),
                HOSTS,
            )?;
            path
        } else if let Some(rest) = raw.strip_prefix("ssh://") {
            let (authority, path) = rest
                .split_once('/')
                .ok_or_else(|| Error::new("malformed GitHub remote"))?;
            let authority = authority.strip_prefix("git@").unwrap_or(authority);
            require(
                ["github.com", "github.com:22"]
                    .iter()
                    .any(|h| authority.eq_ignore_ascii_case(h)),
                HOSTS,
            )?;
            path
        } else {
            return Err(Error::new(HOSTS));
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
        Self::checked(
            parts[0],
            parts[1].strip_suffix(".git").unwrap_or(parts[1]),
            SLUG,
        )
    }
    /// GitHub owner and repository names are case-insensitive.
    pub fn matches(&self, other: &Slug) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.repo.eq_ignore_ascii_case(&other.repo)
    }
    fn shown(&self) -> String {
        let cap = |s: &str| s.chars().take(100).collect::<String>();
        format!("{}/{}", cap(&self.owner), cap(&self.repo))
    }
}
impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// An issue or pull number: 1-10 ASCII digits, no leading zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Number(u64);
const NUMBER: &str = "GitHub number must be 1-10 ASCII digits without a leading zero";
impl Number {
    pub fn parse(raw: &str) -> Result<Self> {
        require(
            (1..=10).contains(&raw.len())
                && !raw.starts_with('0')
                && raw.bytes().all(|b| b.is_ascii_digit()),
            NUMBER,
        )?;
        raw.parse().map(Self).map_err(|_| Error::new(NUMBER))
    }
    pub fn get(self) -> u64 {
        self.0
    }
}
impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// `N`, `#N` or `owner/repo#N`, bound to `origin`'s spelling.
fn reference(input: &str, origin: &Slug, kind: &str) -> Result<(Slug, Number)> {
    let grammar = format!("{kind} reference must be N, #N or owner/repo#N");
    require(
        input.bytes().all(|b| b.is_ascii_graphic()),
        grammar.as_str(),
    )?;
    let (given, digits) = match input.split_once('#') {
        Some(("", digits)) => (None, digits),
        Some((slug, digits)) => (
            Some(Slug::parse(slug).map_err(|_| Error::new(grammar.as_str()))?),
            digits,
        ),
        None => (None, input),
    };
    let number = Number::parse(digits).map_err(|_| Error::new(grammar.as_str()))?;
    if let Some(given) = given
        && !given.matches(origin)
    {
        return Err(Error::new(format!(
            "{kind} reference names {}, but origin is {}; only same-repository references are accepted",
            given.shown(),
            origin.shown()
        )));
    }
    Ok((origin.clone(), number))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub slug: Slug,
    pub number: Number,
}
impl IssueRef {
    pub fn parse(input: &str, origin: &Slug) -> Result<Self> {
        let (slug, number) = reference(input, origin, "issue")?;
        Ok(Self { slug, number })
    }
}
impl fmt::Display for IssueRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.slug, self.number)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRef {
    pub slug: Slug,
    pub number: Number,
}
impl PullRef {
    pub fn parse(input: &str, origin: &Slug) -> Result<Self> {
        let (slug, number) = reference(input, origin, "pull")?;
        Ok(Self { slug, number })
    }
}
impl fmt::Display for PullRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.slug, self.number)
    }
}

/// Reads the opt-in declaration `- GitHub project: owner/repo` under
/// `## Collaboration` (same section rule as the autonomy `- Acceptance reuse:`
/// declaration). Absent is `None`; duplicates or a malformed slug fail closed.
pub fn project_mode(project_md: &str) -> Result<Option<Slug>> {
    let mut inside = false;
    let mut found = Vec::new();
    for line in project_md.lines() {
        if line.starts_with("## ") {
            inside = line.trim_end() == "## Collaboration";
            continue;
        }
        if inside && let Some(value) = line.strip_prefix("- GitHub project:") {
            found.push(value.trim());
        }
    }
    match found[..] {
        [] => Ok(None),
        [value] => Slug::parse(value)
            .map(Some)
            .map_err(|_| Error::new("PROJECT.md '- GitHub project:' must be one owner/repo slug")),
        _ => Err(Error::new(
            "PROJECT.md has multiple '- GitHub project:' declarations",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Slug {
        Slug::from_remote("git@github.com:Example-Org/Example-Repo.git").unwrap()
    }

    #[test]
    fn remote_forms_mirror_landing() {
        // Reference: oms-command-landing report::github_remote/github_slug.
        for ok in [
            "git@github.com:example-org/example-repo.git",
            "https://github.com/example-org/example-repo",
            "https://GitHub.com:443/example-org/example-repo.git",
            "ssh://git@github.com:22/example-org/example-repo.git",
            "ssh://github.com/example-org//example-repo",
        ] {
            assert_eq!(
                Slug::from_remote(ok).unwrap().to_string(),
                "example-org/example-repo",
                "{ok}"
            );
        }
        for bad in [
            "",
            "https://gitlab.com/example-org/example-repo",
            "https://git@github.com/example-org/example-repo",
            "https://github.com/example-org/example-repo?x=1",
            "https://github.com/example-org/example-repo#frag",
            "https://github.com/example-org/example-repo/extra",
            "git@github.com:example-org/..",
            "git@github.com:example org/example-repo",
            "git@github.com:example-org/example-repo\n",
            "file:///srv/example-repo",
            "https://github.com",
        ] {
            assert!(Slug::from_remote(bad).is_err(), "{bad:?}");
        }
        assert!(origin().matches(&Slug::parse("example-org/EXAMPLE-REPO").unwrap()));
        assert!(!origin().matches(&Slug::parse("example-org/other").unwrap()));
        for bad in ["example-org", "a/b/c", "/r", "o/", "o/..", "o/r.git#1"] {
            assert!(Slug::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reference_grammar_is_bounded_and_same_repo() {
        for (input, number) in [
            ("7", 7),
            ("#42", 42),
            ("example-org/example-repo#9999999999", 9_999_999_999),
        ] {
            let issue = IssueRef::parse(input, &origin()).unwrap();
            assert_eq!(issue.number.get(), number);
            assert_eq!(
                issue.to_string(),
                format!("Example-Org/Example-Repo#{number}")
            );
        }
        assert_eq!(
            PullRef::parse("#3", &origin()).unwrap().to_string(),
            "Example-Org/Example-Repo#3"
        );
        for bad in [
            "",
            "0",
            "#0",
            "07",
            "12345678901",
            "1a",
            "#",
            "##5",
            "5#",
            "#5#6",
            "# 5",
            "5 ",
            "5\n",
            "owner#5",
            "o/r/x#5",
            "/r#5",
            "example-org/example-repo#5?x",
            "https://github.com/example-org/example-repo/issues/5",
            "-5",
            "+5",
        ] {
            assert!(IssueRef::parse(bad, &origin()).is_err(), "{bad:?}");
        }
        let wrong = IssueRef::parse("other-org/example-repo#5", &origin()).unwrap_err();
        assert!(wrong.message.contains("same-repository"), "{wrong}");
    }

    #[test]
    fn project_mode_declaration_fails_closed() {
        let doc = |body: &str| {
            format!("# P\n\n## Collaboration\n\n{body}\n## Other\n\n- GitHub project: x/y\n")
        };
        assert_eq!(project_mode("# P\n").unwrap(), None);
        assert_eq!(
            project_mode(&doc("- Collaboration mode: careful\n")).unwrap(),
            None
        );
        assert_eq!(
            project_mode(&doc("- GitHub project: example-org/example-repo\n"))
                .unwrap()
                .unwrap()
                .to_string(),
            "example-org/example-repo"
        );
        for body in [
            "- GitHub project: example-org/example-repo\n- GitHub project: example-org/example-repo\n",
            "- GitHub project:\n",
            "- GitHub project: https://github.com/example-org/example-repo\n",
            "- GitHub project: example-org/example-repo extra\n",
        ] {
            assert!(project_mode(&doc(body)).is_err(), "{body}");
        }
    }
}
