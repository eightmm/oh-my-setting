//! Local GitHub project-mode context for autonomy intake. Nothing here
//! contacts GitHub. Only the harness stamps a reviewed issue reference into a
//! proposal; planner output never supplies one.
use crate::storage::{Context, MAX_FILE};
use crate::{Error, Result, require, value};
use oms_github::{IssueRef, Number, Slug, declaration};
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;

pub(crate) const BLOCKED: &str = "github-issue";
pub(crate) const ADOPT_KIND: &str = "intent-adopt-receipt";

fn fail(e: oms_github::Error) -> Error {
    Error::new(2, format!("github issue: {}", e.message))
}

pub(crate) struct Mode {
    origin: Option<Slug>,
    /// The declared project when GitHub project mode is active.
    pub active: Option<Slug>,
}
impl Mode {
    /// A missing or non-github.com origin is `None`, as in the plan crate's
    /// `Issues::load`; a failed declaration or origin guard refuses.
    pub fn load(ctx: &Context<'_>) -> Result<Self> {
        let plan = ctx.plan()?;
        let out = plan.git().output(
            &["remote", "get-url", "origin"].map(OsStr::new),
            None,
            &[],
            64 * 1024,
        )?;
        let origin = (out.code == 0)
            .then(|| std::str::from_utf8(oms_command_plan::repository::trim(&out.stdout)).ok())
            .flatten()
            .and_then(|url| Slug::from_remote(url).ok());
        let decl = declaration::inspect(&ctx.repo).map_err(fail)?;
        let active = declaration::active(decl.as_ref(), origin.as_ref()).map_err(fail)?;
        Ok(Self { origin, active })
    }
    /// Canonical same-repository `owner/repo#N` in origin's spelling.
    pub fn canonical(&self, raw: &str) -> Result<String> {
        let origin = self
            .origin
            .as_ref()
            .ok_or_else(|| Error::new(2, "github issue: requires a github.com origin remote"))?;
        let issue = IssueRef::parse(raw, origin).map_err(fail)?;
        if let Some(active) = &self.active {
            require(
                issue.slug.matches(active),
                "github issue: names a different repository than the GitHub project",
            )?;
        }
        Ok(issue.to_string())
    }
    /// The issue bound by an adopted intent, only while the mode is active and
    /// the record's PROJECT.md digest is the current one. A foreign-repository
    /// record carries nothing.
    pub fn adopted(&self, ctx: &Context<'_>, project_sha256: &str) -> Result<Option<String>> {
        if self.active.is_none() {
            return Ok(None);
        }
        let mut found = BTreeSet::new();
        for name in ctx.source.names(".oms/intents", 4096, true)? {
            let Some(name) = name.to_str().filter(|n| n.ends_with(".adopt.json")) else {
                continue;
            };
            let row = value::parse(
                &ctx.source
                    .read(Path::new(".oms/intents").join(name), MAX_FILE)?,
            )?;
            if row["kind"] == ADOPT_KIND
                && value::text(&row, "project_sha256") == project_sha256
                && let Ok(issue) = self.canonical(value::text(&row, "issue"))
            {
                found.insert(issue);
            }
        }
        require(
            found.len() <= 1,
            "github issue: adopted intents bind different issues to PROJECT.md",
        )?;
        Ok(found.into_iter().next())
    }
}

/// `owner/repo#N` shape (no origin binding), for stored records.
pub(crate) fn shaped(raw: &str) -> bool {
    raw.split_once('#')
        .is_some_and(|(slug, n)| Slug::parse(slug).is_ok() && Number::parse(n).is_ok())
}

/// `owner/repo#N` from an exact `https://github.com/OWNER/REPO/issues/N`.
pub(crate) fn from_issue_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo, kind, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let slug = Slug::parse(&format!("{owner}/{repo}")).ok()?;
    let number = Number::parse(number).ok()?;
    (parts.next().is_none() && kind == "issues").then(|| format!("{slug}#{number}"))
}

/// Planner and provider rows never carry an issue: only the harness stamps one.
pub(crate) fn refuse_provider_issue(extracted: &Value) -> Result<()> {
    let tasks = extracted["tasks"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    require(
        extracted.get("issue").is_none() && tasks.iter().all(|t| t.get("issue").is_none()),
        "planner output must not carry an issue; the harness stamps the reviewed issue",
    )
}

/// Set the issue on the proposal and on every task.
pub(crate) fn stamp(proposal: &mut Value, issue: &str) {
    proposal["issue"] = issue.into();
    if let Some(tasks) = proposal["tasks"].as_array_mut() {
        for task in tasks {
            task["issue"] = issue.into();
        }
    }
}

/// The proposal's issue. When any is present, all must be present, identical
/// and canonical for this origin (and the active project).
pub(crate) fn proposal_issue(mode: &Mode, proposal: &Value) -> Result<Option<String>> {
    let tasks = proposal["tasks"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if proposal.get("issue").is_none() && tasks.iter().all(|t| t.get("issue").is_none()) {
        return Ok(None);
    }
    let issue = proposal["issue"].as_str().unwrap_or("");
    require(
        !issue.is_empty() && tasks.iter().all(|t| t["issue"].as_str() == Some(issue)),
        "proposal issue must be identical on the proposal and every task",
    )?;
    require(
        mode.canonical(issue)? == issue,
        "proposal issue must be canonical owner/repo#N for origin",
    )?;
    Ok(Some(issue.to_owned()))
}
