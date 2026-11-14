//! Read-only pairwise preview. Merge objects live only in private temporary
//! storage; pairwise cleanliness never proves a combined landing is safe.
use crate::{Error, Result, require, value};
use oms_command_plan::repository::Git;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(value::random("oms-conflicts-")?);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| Error::new(2, "cannot create private merge object directory"))?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn merge(git: &Git<'_>, left: &str, right: &str) -> Result<Value> {
    let scratch = Scratch::new()?;
    let objects = git.namespace_path()?.join("objects");
    let quoted =
        oms_command_plan::json::canonical(&oms_command_plan::json::os(objects.as_os_str()))?;
    use std::os::unix::ffi::OsStringExt;
    let env = [
        (
            "GIT_OBJECT_DIRECTORY".into(),
            scratch.0.as_os_str().to_owned(),
        ),
        (
            "GIT_ALTERNATE_OBJECT_DIRECTORIES".into(),
            OsString::from_vec(quoted),
        ),
        ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
    ];
    let output = git.output(
        &[
            "merge-tree".as_ref(),
            "--write-tree".as_ref(),
            "--name-only".as_ref(),
            "--no-messages".as_ref(),
            "-z".as_ref(),
            left.as_ref(),
            right.as_ref(),
        ],
        None,
        &env,
        1024 * 1024,
    )?;
    let fields = output.stdout.split(|b| *b == 0).collect::<Vec<_>>();
    let valid = fields
        .first()
        .and_then(|s| std::str::from_utf8(s).ok())
        .is_some_and(value::commit);
    if !valid || ![0, 1].contains(&output.code) {
        return Ok(
            json!({"state": "error", "files": [], "reason": format!("git merge-tree failed (exit {})", output.code)}),
        );
    }
    let files = fields
        .iter()
        .skip(1)
        .filter(|s| !s.is_empty())
        .map(|s| oms_contract_markdown::bounded_line(&String::from_utf8_lossy(s), 200))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(200)
        .collect::<Vec<_>>();
    Ok(
        json!({"state": if output.code == 0 { "clean" } else { "conflicts" }, "files": files, "reason": ""}),
    )
}
pub fn pending(
    repo: &Path,
    base: &str,
    branches: &[String],
    limit: usize,
    interrupted: &AtomicBool,
) -> Result<Value> {
    require(
        (2..=24).contains(&limit),
        "pending conflict limit must be 2..24",
    )?;
    let repo =
        std::fs::canonicalize(repo).map_err(|_| Error::new(2, "cannot resolve repository"))?;
    let git = Git {
        repo: &repo,
        interrupted,
        group_resolver: None,
    };
    git.text(&["rev-parse", "--show-toplevel"])?;
    let mut doc = json!({"schema": 1, "mode": "pending-conflicts", "status": "ok", "readonly": true, "limit": {"max": limit, "hard_max": 24}});
    let version = git.text(&["--version"])?;
    let old = regex::Regex::new(r"([0-9]+)\.([0-9]+)")
        .unwrap()
        .captures(&version)
        .and_then(|c| Some((c[1].parse::<u64>().ok()?, c[2].parse::<u64>().ok()?)))
        .is_none_or(|v| v < (2, 38));
    if old {
        doc["status"] = "unavailable".into();
        doc["reason"] = "git-too-old".into();
        return Ok(doc);
    }
    let drivers = git.output(
        &[
            "config".as_ref(),
            "--no-includes".as_ref(),
            "--name-only".as_ref(),
            "--get-regexp".as_ref(),
            r"^merge\..*\.driver$".as_ref(),
        ],
        None,
        &[],
        65536,
    )?;
    if git.guard().is_err() || drivers.code != 1 {
        doc["status"] = "unavailable".into();
        doc["reason"] = "unsafe-config".into();
        return Ok(doc);
    }
    require(
        base.is_empty() || !base.starts_with('-'),
        "base cannot start with '-' ",
    )?;
    let defaults = ["origin/main", "origin/master", "main", "master"];
    let choices = if base.is_empty() {
        defaults.to_vec()
    } else {
        vec![base]
    };
    let (base_name, base_sha) = choices
        .iter()
        .find_map(|name| {
            git.text(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ])
            .ok()
            .filter(|sha| value::commit(sha))
            .map(|sha| ((*name).to_owned(), sha))
        })
        .ok_or_else(|| Error::new(2, "no base found; provide a commit ref"))?;
    doc["base"] = json!({"ref": base_name, "sha": base_sha});
    let mut found = vec![];
    let refs = if branches.is_empty() {
        vec!["refs/heads/oms/".to_owned()]
    } else {
        branches
            .iter()
            .map(|name| {
                require(
                    !name.starts_with('-') && !name.contains('\0'),
                    "invalid local branch name",
                )?;
                Ok(format!("refs/heads/{name}"))
            })
            .collect::<Result<Vec<_>>>()?
    };
    for reference in refs {
        let output = git.text(&[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)%00%(committerdate:unix)",
            &reference,
        ])?;
        for line in output.lines() {
            let fields = line.split('\0').collect::<Vec<_>>();
            if fields.len() != 3 || !value::commit(fields[1]) {
                continue;
            }
            if !branches.is_empty() && !branches.iter().any(|name| name == fields[0]) {
                continue;
            }
            let when = fields[2].parse::<i64>().unwrap_or(0);
            found.push((fields[0].to_owned(), fields[1].to_owned(), when));
        }
    }
    if !branches.is_empty() {
        for branch in branches {
            require(
                found.iter().any(|(name, _, _)| name == branch),
                "a requested local branch does not exist",
            )?;
        }
    }
    found.sort();
    found.dedup();
    let mut pending = vec![];
    let mut skipped = vec![];
    for (name, sha, committed) in found {
        match git
            .text(&["rev-list", "--count", &format!("{base_sha}..{sha}")])
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            Some(ahead) if ahead > 0 => pending
                .push(json!({"name": name, "sha": sha, "ahead": ahead, "committed": committed})),
            Some(_) => skipped.push(json!({"name": name, "reason": "already in the base"})),
            None => skipped.push(json!({"name": name, "reason": "cannot compare with the base"})),
        }
    }
    pending.sort_by(|a, b| {
        b["committed"]
            .as_i64()
            .cmp(&a["committed"].as_i64())
            .then(value::text(a, "name").cmp(value::text(b, "name")))
    });
    let considered = pending.len();
    for row in pending.iter().skip(limit) {
        skipped
            .push(json!({"name": row["name"], "reason": "over the limit (oldest first dropped)"}));
    }
    pending.truncate(limit);
    pending.sort_by(|a, b| {
        a["committed"]
            .as_i64()
            .cmp(&b["committed"].as_i64())
            .then(value::text(a, "name").cmp(value::text(b, "name")))
    });
    let mut base_states = BTreeMap::new();
    for row in &mut pending {
        row["vs_base"] = merge(&git, &base_sha, value::text(row, "sha"))?;
        base_states.insert(
            value::text(row, "name").to_owned(),
            value::text(&row["vs_base"], "state").to_owned(),
        );
    }
    let mut pairs = vec![];
    let mut states = BTreeMap::new();
    for (index, left) in pending.iter().enumerate() {
        for right in &pending[index + 1..] {
            let result = merge(&git, value::text(left, "sha"), value::text(right, "sha"))?;
            let a = value::text(left, "name");
            let b = value::text(right, "name");
            let state = value::text(&result, "state").to_owned();
            states.insert((a.to_owned(), b.to_owned()), state.clone());
            states.insert((b.to_owned(), a.to_owned()), state);
            let mut result = result;
            result["a"] = a.into();
            result["b"] = b.into();
            pairs.push(result);
        }
    }
    let mut remaining = pending
        .iter()
        .map(|r| value::text(r, "name").to_owned())
        .collect::<Vec<_>>();
    let mut order = vec![];
    while !remaining.is_empty() {
        let best = remaining
            .iter()
            .min_by_key(|name| {
                let conflicts = remaining
                    .iter()
                    .filter(|other| {
                        *other != *name
                            && states
                                .get(&((*name).clone(), (*other).clone()))
                                .is_none_or(|s| s != "clean")
                    })
                    .count();
                let committed = pending
                    .iter()
                    .find(|r| value::text(r, "name") == name.as_str())
                    .and_then(|r| r["committed"].as_i64())
                    .unwrap_or(0);
                (
                    base_states.get(*name).is_none_or(|s| s != "clean"),
                    conflicts,
                    committed,
                    (*name).clone(),
                )
            })
            .unwrap()
            .clone();
        remaining.retain(|name| name != &best);
        order.push(best);
    }
    doc["limit"] = json!({"max": limit, "hard_max": 24, "considered": considered, "checked": pending.len(), "truncated": considered > pending.len()});
    doc["branches"] = pending.into();
    doc["skipped"] = skipped.into();
    doc["pairs"] = pairs.into();
    doc["suggested_order"] =
        json!({"label": "suggestion", "basis": "pairwise results only", "branches": order});
    Ok(doc)
}
