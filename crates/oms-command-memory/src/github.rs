//! Opt-in GitHub project mode: recall reuses reviewed pull-request and commit
//! knowledge, read only from the local repository (the project declaration,
//! commits reachable from HEAD and the snapshot cache). Nothing is fetched, and cached
//! GitHub text is untrusted display data that never enters provider context.
//! Like task and plan, the declared slug must name `origin` or the mode is off.
use crate::{
    store::{self, Store},
    *,
};
use oms_github::{PullRef, Slug, freshness};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    path::Path,
    sync::{LazyLock, atomic::AtomicBool},
};

/// Provisional: the pull-request template owns the final spelling.
pub(crate) const DECISION_HEADING: &str = "## Decision and evidence";
const CACHE_BYTES: usize = 4 << 20;
const LOG_BYTES: usize = 256 << 10;
const HITS: usize = 3;

/// The active project slug per `oms_github::declaration` (PROJECT.md and the
/// tracked `.github/oms-project.md` at HEAD, bound to `origin` by `active`),
/// else `None`. A declaration or origin error leaves the mode off with one
/// warning; no declaration, or a tracked one origin does not name (a fork),
/// stays silent.
pub(crate) fn mode(s: &Store, flag: &AtomicBool) -> Option<Slug> {
    use oms_github::declaration::{active, inspect};
    let repo = s.canonical_repo()?;
    let bound = inspect(repo).and_then(|declared| {
        let origin = declared.as_ref().and_then(|_| {
            let url = store::git_text(repo, &["remote", "get-url", "origin"], flag);
            Slug::from_remote(&url).ok()
        });
        active(declared.as_ref(), origin.as_ref())
    });
    bound.unwrap_or_else(|e| {
        let why = e.message;
        s.warning(&format!("warning: GitHub project mode is off: {why}"));
        None
    })
}

fn words(text: &str) -> HashSet<String> {
    static WORD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[^\W_]+").unwrap());
    let folded = fold(text);
    WORD.find_iter(&folded)
        .map(|m| m.as_str().to_owned())
        .collect()
}

fn overlap(query: &[String], text: &str) -> usize {
    let have = words(text);
    query.iter().filter(|w| have.contains(*w)).count()
}

/// Commits, then merged pull requests, each capped at three hits.
pub(crate) fn recall(
    s: &Store,
    query: &str,
    as_json: bool,
    flag: &AtomicBool,
) -> Result<(Vec<u8>, usize)> {
    let (Some(slug), Some(repo)) = (mode(s, flag), s.canonical_repo()) else {
        return Ok((Vec::new(), 0));
    };
    let query = db::tokens(query);
    if query.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let mut out = Vec::new();
    let commits = commits(repo, &query, flag);
    for (sha, when, subject, excerpt) in &commits {
        if as_json {
            out.extend(json_line(
                &json!({"schema":3,"event_id":format!("commit:{sha}"),"source":"commit","kind":"commit","ref":sha,"occurred_at":stamp(*when),"subject":subject,"excerpt":excerpt,"trust":"repository"}),
            )?);
        } else {
            out.extend(format!("commit {} {subject}\n", &sha[..12]).as_bytes());
        }
    }
    let pulls = pulls(s, repo, &slug, &query);
    for (reference, row, fresh) in &pulls {
        let withheld = !store::clean(&format!("{}\n{}", row.title, row.labels.join("\n")), true);
        let (title, labels) = if withheld {
            ("", &[][..])
        } else {
            (row.title.as_str(), &row.labels[..])
        };
        if as_json {
            out.extend(json_line(
                &json!({"schema":3,"event_id":format!("pull_request:{reference}"),"source":"pull_request","kind":"pull","ref":reference,"state":row.state,"title":title,"labels":labels,"freshness":fresh,"captured_at":row.captured_at,"trust":"untrusted","withheld":withheld}),
            )?);
        } else {
            let title = if withheld {
                "[withheld: sensitive-looking content]"
            } else {
                title
            };
            out.extend(format!("pr {reference} {title} ({fresh})\n").as_bytes());
        }
    }
    Ok((out, commits.len() + pulls.len()))
}

/// Commits reachable from HEAD (at most 200, 256 KiB of log), ranked by query
/// term overlap and then recency; messages the scrubber flags are excluded.
fn commits(repo: &Path, query: &[String], flag: &AtomicBool) -> Vec<(String, i64, String, String)> {
    let args = [
        "log",
        "--no-show-signature",
        "--no-color",
        "--no-notes",
        "-z",
        "-n",
        "200",
        "--format=%H%n%ct%n%B",
        "HEAD",
        "--",
    ]
    .map(OsStr::new);
    let Some((raw, truncated)) = store::git_capped(repo, &args, LOG_BYTES, flag) else {
        return Vec::new();
    };
    let mut records: Vec<_> = raw.split(|b| *b == 0).collect();
    if truncated {
        records.pop();
    }
    let mut ranked = Vec::new();
    for (index, record) in records.into_iter().enumerate() {
        let record = String::from_utf8_lossy(record);
        let mut parts = record.trim_start_matches('\n').splitn(3, '\n');
        let (Some(sha), Some(when), Some(message)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(when), true) = (
            when.parse::<i64>(),
            matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        ) else {
            continue;
        };
        let score = overlap(query, message);
        if score > 0 {
            ranked.push((score, index, sha.to_owned(), when, message.to_owned()));
        }
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked
        .into_iter()
        .filter(|(.., message)| store::clean(message, true))
        .take(HITS)
        .map(|(_, _, sha, when, message)| {
            let subject = freshness::sanitize(message.lines().next().unwrap_or(""), 200);
            let excerpt = message
                .lines()
                .find(|line| overlap(query, line) > 0)
                .map(|line| freshness::sanitize(line, 300))
                .unwrap_or_default();
            (sha, when, subject, excerpt)
        })
        .collect()
}

/// Merged same-repository pulls from the snapshot cache, newest row per pull
/// (the `freshness::lookup` rule); only the sanitized title and labels match.
fn pulls(
    s: &Store,
    repo: &Path,
    slug: &Slug,
    query: &[String],
) -> Vec<(String, freshness::Cached, &'static str)> {
    let Ok(raw) = store::read_prefix(&repo.join(freshness::CACHE_REL), CACHE_BYTES + 1) else {
        return Vec::new();
    };
    if raw.len() > CACHE_BYTES {
        s.warning(
            "warning: GitHub snapshot cache is larger than 4 MiB; pull request recall skipped",
        );
        return Vec::new();
    }
    let now = timestamp(&now()).unwrap_or(0);
    let mut newest: HashMap<String, (freshness::Cached, freshness::Freshness)> = HashMap::new();
    for line in raw.split(|b| *b == b'\n') {
        let Some(reference) = parse_json(line).map(|v| text(&v, "reference")) else {
            continue;
        };
        let (Some(row), fresh) = freshness::lookup(line, &reference, now) else {
            continue;
        };
        let Ok(pull) = PullRef::parse(&row.reference, slug) else {
            continue;
        };
        let key = pull.to_string();
        if row.kind == "pull"
            && newest
                .get(&key)
                .is_none_or(|(n, _)| row.captured_at >= n.captured_at)
        {
            newest.insert(key, (row, fresh));
        }
    }
    let mut ranked: Vec<_> = newest
        .into_iter()
        .filter(|(_, (row, _))| row.state == "merged")
        .filter_map(|(key, (row, fresh))| {
            let score = overlap(query, &format!("{}\n{}", row.title, row.labels.join("\n")));
            (score > 0).then_some((score, key, row, fresh))
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.2.captured_at.cmp(&a.2.captured_at))
            .then(a.1.cmp(&b.1))
    });
    ranked
        .into_iter()
        .take(HITS)
        .map(|(_, key, row, fresh)| {
            let fresh = match fresh {
                freshness::Freshness::Fresh => "fresh",
                _ => "stale",
            };
            (key, row, fresh)
        })
        .collect()
}

/// A preview-only pull-request body section; the PR record owns the decision.
pub(crate) fn decision_section(line: &str, id: &str) -> String {
    format!("{DECISION_HEADING}\n\n- {line}\n- Local memory note: {id}\n")
}
