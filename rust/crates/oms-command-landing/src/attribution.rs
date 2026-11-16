//! Attribution is append-only observation, never permission to land or publish.
use crate::{
    Context, Error, Output, Result, V, cli::Options, digest, git::Git, now, obj, require, storage,
    string,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub fn record_landing(
    state: &Path,
    repo: &Path,
    landing: &str,
    base: &str,
    patch: &[u8],
    task: &str,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let scratch = crate::worktree::Sandbox::new(repo, base, interrupted)?;
    let git = Git::new(&scratch.path, interrupted);
    git.apply(patch, false, false, true)?;
    let tree = git.text(&["write-tree"])?;
    let files = git.run(&["diff", "--cached", "--name-only", "-z", "HEAD", "--"])?;
    let files: Vec<_> = files
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| V::Str(p.to_vec()))
        .collect();
    let count = files.len();
    let attempt = std::env::var("OMS_ATTEMPT_ID").unwrap_or_default();
    let (_, attempts) = oms_command_operations::load_projection(state)?;
    let record = attempts
        .iter()
        .find(|r| r.get("attempt_id").text() == Some(&attempt));
    let row = obj([
        ("schema", 1usize.into()),
        ("kind", "landing".into()),
        ("ts", now().into()),
        ("landing_id", landing.into()),
        ("base", base.into()),
        ("tree_after", tree.into()),
        ("files", V::Arr(files.into_iter().take(500).collect())),
        ("files_changed", count.into()),
        ("patch_sha256", digest(patch).into()),
        ("attempt", attempt.into()),
        ("attempt_recorded", record.is_some().into()),
        ("task", task.into()),
        (
            "provider",
            record
                .map(|r| r.get("provider").clone())
                .unwrap_or_else(|| "unknown".into()),
        ),
        (
            "model",
            record
                .map(|r| r.get("model").clone())
                .unwrap_or_else(|| "unknown".into()),
        ),
        (
            "room",
            std::env::var("OMS_ROOM_ID").unwrap_or_default().into(),
        ),
    ]);
    storage::append(
        state,
        Path::new(".oms/attribution.jsonl"),
        &row,
        interrupted,
    )
}
pub(crate) fn blame(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(
        o.words.len() == 2,
        "blame-session requires one commit or range",
    )?;
    let repo = c.repo(o)?;
    let state = c.state(&repo)?;
    let git = Git::new(&repo, c.interrupted);
    let spec = o
        .words
        .get(1)
        .ok_or_else(|| Error::new(2, "give a commit or a range such as BASE..HEAD"))?;
    require(!spec.starts_with('-'), "invalid commit specification")?;
    let max = if spec.contains("..") || o.has("file") {
        501
    } else {
        1
    };
    let mut args = vec![
        "rev-list",
        if max == 1 {
            "--max-count=1"
        } else {
            "--max-count=501"
        },
        spec,
        "--",
    ];
    if o.has("file") {
        args.push(o.get("file"));
    }
    let commits = git.text(&args)?;
    let commits: Vec<_> = commits.lines().collect();
    require(
        commits.len() <= 500,
        "attribution range exceeds 500 commits",
    )?;
    let rows = storage::rows(&state, Path::new(".oms/attribution.jsonl"))?;
    let mut by_commit = BTreeMap::new();
    for row in rows.iter().filter(|r| string(r, "kind") == "commit") {
        by_commit.insert(string(row, "commit"), row.clone());
    }
    let mut out = String::new();
    let mut projected = vec![];
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for sha in commits {
        let record = by_commit
            .get(sha)
            .cloned()
            .unwrap_or_else(|| obj([("commit", sha.into()), ("author_kind", "unrecorded".into())]));
        let kind = string(&record, "author_kind");
        *counts.entry(kind.clone()).or_default() += 1;
        out += &format!("{} {kind}\n", &sha[..7]);
        projected.push(record);
    }
    if o.has("export-notes") {
        for row in &projected {
            if string(row, "author_kind") == "unrecorded" {
                continue;
            }
            let body = row.json(false)?;
            let result = git.output(
                &[
                    "notes".as_ref(),
                    "--ref=refs/notes/oms-attribution".as_ref(),
                    "add".as_ref(),
                    "-f".as_ref(),
                    "-F".as_ref(),
                    "-".as_ref(),
                    string(row, "commit").as_ref(),
                ],
                Some(&body),
                &[],
            )?;
            require(result.code == 0, "cannot export attribution Git notes")?;
        }
    }
    if o.has("json") {
        let summary = V::Obj(
            counts
                .into_iter()
                .map(|(k, v)| (k.into_bytes(), v.into()))
                .collect(),
        );
        Output::json(&obj([
            ("schema", 1usize.into()),
            ("commits", V::Arr(projected)),
            ("summary", summary),
        ]))
    } else {
        out += &format!(
            "summary: {}\n",
            counts
                .iter()
                .map(|(k, n)| format!("{k}={n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(Output::text(out))
    }
}
pub fn author_kinds(rows: &[V]) -> BTreeSet<String> {
    rows.iter().map(|r| string(r, "author_kind")).collect()
}

pub fn record_commits(
    state: &Path,
    repo: &Path,
    range: &str,
    receipt: &str,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<usize> {
    require(
        !range.is_empty() && !range.starts_with('-'),
        "invalid attribution range",
    )?;
    let git = Git::new(repo, interrupted);
    git.safe()?;
    let path = Path::new(".oms/attribution.jsonl");
    let _lock = storage::lock(&state.join(path), interrupted)?;
    let rows = storage::rows(state, path)?;
    let recorded: BTreeSet<_> = rows
        .iter()
        .filter(|r| string(r, "kind") == "commit")
        .map(|r| string(r, "commit"))
        .collect();
    let used: BTreeSet<_> = rows
        .iter()
        .filter(|r| string(r, "kind") == "commit")
        .map(|r| string(r.get("evidence"), "landing_id"))
        .collect();
    let mut pending: Vec<_> = rows
        .iter()
        .filter(|r| string(r, "kind") == "landing" && !used.contains(&string(r, "landing_id")))
        .collect();
    let mut count = 0;
    for line in git
        .text(&["log", "--reverse", "--format=%H%x09%T%x09%P", range])?
        .lines()
        .take(500)
    {
        let fields: Vec<_> = line.split('\t').collect();
        require(fields.len() == 3, "unreadable attribution commit metadata")?;
        let (sha, tree) = (fields[0], fields[1]);
        if recorded.contains(sha) {
            continue;
        }
        let parents: Vec<_> = fields[2].split_whitespace().collect();
        let files = if let Some(parent) = parents.first() {
            git.run(&["diff", "--name-only", "-z", parent, sha, "--"])?
        } else {
            git.run(&[
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
                sha,
            ])?
        };
        let files: BTreeSet<_> = files
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| p.to_vec())
            .collect();
        let found = pending.iter().enumerate().find_map(|(i, landing)| {
            let kind = if string(landing, "tree_after") == tree {
                "ai"
            } else if parents.len() == 1
                && parents[0] == string(landing, "base")
                && landing
                    .get("files")
                    .array()
                    .iter()
                    .any(|v| v.bytes().is_some_and(|b| files.contains(b)))
            {
                "mixed"
            } else {
                return None;
            };
            Some((
                i,
                if landing.get("attempt_recorded").truth() {
                    kind
                } else {
                    "unknown"
                },
            ))
        });
        let Some((position, kind)) = found else {
            continue;
        };
        let landing = pending.remove(position);
        let diff = git.run(&["diff-tree", "-p", "--root", "--no-ext-diff", sha])?;
        let patch_id = git.output(
            &["patch-id".as_ref(), "--stable".as_ref()],
            Some(&diff),
            &[],
        )?;
        let patch_id = if patch_id.code == 0 {
            String::from_utf8_lossy(&patch_id.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_owned()
        } else {
            String::new()
        };
        let row = obj([
            ("schema", 1usize.into()),
            ("kind", "commit".into()),
            ("ts", now().into()),
            ("commit", sha.into()),
            ("tree", tree.into()),
            (
                "parents",
                V::Arr(parents.iter().map(|p| V::from(*p)).collect()),
            ),
            ("author_kind", kind.into()),
            ("provider", landing.get("provider").clone().or("unknown")),
            ("model", landing.get("model").clone().or("unknown")),
            ("attempt", landing.get("attempt").clone().or("")),
            ("task", landing.get("task").clone().or("")),
            ("room", landing.get("room").clone().or("")),
            ("files_changed", files.len().into()),
            ("patch_id", patch_id.into()),
            (
                "evidence",
                obj([
                    ("landing_id", landing.get("landing_id").clone()),
                    ("land_receipt", receipt.into()),
                ]),
            ),
        ]);
        storage::append_under_lock(state, path, &row)?;
        count += 1;
    }
    Ok(count)
}
