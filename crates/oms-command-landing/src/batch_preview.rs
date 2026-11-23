use crate::{
    Context, Error, Output, Result, V,
    cli::Options,
    obj, require,
    services::{BatchVerification, VerificationOutcome, VerifyRequest},
    storage,
    worktree::Sandbox,
};
use oms_command_plan::repository::{Git, GitOutput, commit, trim};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_REFS: usize = 8;
const MAX_REF_BYTES: usize = 200;
const MAX_CONFLICT_PATHS: usize = 20;
const DEADLINE: Duration = Duration::from_secs(30);
const VERIFY_WALL: Duration = Duration::from_secs(30 * 60);
const VERIFY_SCOPE: &str = "affected-ci-parity";

struct Pinned {
    name: String,
    oid: String,
}

struct Step {
    head: Pinned,
    depends_on: Vec<usize>,
    in_base: bool,
    status: &'static str,
    tree: String,
    cumulative_commit: String,
}

fn query(git: &Git<'_>, args: &[&str], limit: usize) -> Result<GitOutput> {
    let args = args.iter().map(OsStr::new).collect::<Vec<_>>();
    Ok(git.output(
        &args,
        None,
        &[
            ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
            ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
        ],
        limit,
    )?)
}

fn oid(raw: &[u8]) -> Result<String> {
    std::str::from_utf8(raw)
        .ok()
        .filter(|value| commit(value))
        .map(str::to_owned)
        .ok_or_else(|| Error::new(2, "Git did not return an exact object ID"))
}

fn resolve(git: &Git<'_>, name: &str) -> Result<Option<String>> {
    let out = query(git, &["show-ref", "--verify", "--hash", name], 128)?;
    if out.code == 0 {
        return oid(trim(&out.stdout)).map(Some);
    }
    Ok(None)
}

fn pin(git: &Git<'_>, name: &str, verify: bool) -> Result<Pinned> {
    require(
        name.len() <= MAX_REF_BYTES
            && !name.chars().any(char::is_control)
            && (name.starts_with("refs/heads/") || name.starts_with("refs/remotes/"))
            && query(git, &["check-ref-format", name], 128)?.code == 0,
        "batch-preview needs a fully qualified local or remote-tracking branch ref",
    )?;
    let oid = resolve(git, name)?.ok_or_else(|| {
        if verify {
            Error::new(
                3,
                format!("preview is stale; branch ref unavailable: {name}"),
            )
        } else {
            Error::new(2, format!("branch ref unavailable: {name}"))
        }
    })?;
    let object = query(git, &["cat-file", "-t", &oid], 128)?;
    require(
        object.code == 0 && trim(&object.stdout) == b"commit",
        format!("branch ref does not resolve to a local commit: {name}"),
    )?;
    Ok(Pinned {
        name: name.into(),
        oid,
    })
}

fn ancestor(git: &Git<'_>, earlier: &str, later: &str) -> Result<bool> {
    match query(git, &["merge-base", "--is-ancestor", earlier, later], 128)?.code {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Error::new(2, "cannot establish local commit ancestry")),
    }
}

fn record_dependencies(git: &Git<'_>, base: &Pinned, rows: &mut [Step]) -> Result<()> {
    // A later alias must not reorder a commit already named earlier.
    let mut seen = BTreeSet::new();
    let mut first_refs = Vec::new();
    for (index, row) in rows.iter_mut().enumerate() {
        row.in_base = ancestor(git, &row.head.oid, &base.oid)?;
        if !row.in_base && seen.insert(row.head.oid.as_str()) {
            first_refs.push(index);
        }
    }
    for index in 0..rows.len() {
        if rows[index].in_base {
            continue;
        }
        for &dependency in &first_refs {
            if rows[dependency].head.oid != rows[index].head.oid
                && ancestor(git, &rows[dependency].head.oid, &rows[index].head.oid)?
            {
                rows[index].depends_on.push(dependency);
            }
        }
    }
    Ok(())
}

fn pin_value(pin: &Pinned) -> V {
    obj([
        ("ref", pin.name.as_str().into()),
        ("oid", pin.oid.as_str().into()),
    ])
}

fn preview_digest(base: &Pinned, rows: &[Step]) -> Result<String> {
    // Synthetic commit dates may differ across invocations. The copied trees,
    // canonical base and ordered local ref/commit pairs are the comparison.
    let steps = rows
        .iter()
        .map(|row| {
            obj([
                ("head", pin_value(&row.head)),
                ("tree_oid", row.tree.as_str().into()),
            ])
        })
        .collect();
    let payload = obj([
        ("kind", "oms-land-batch-preview-v1".into()),
        ("base", pin_value(base)),
        ("steps", V::Arr(steps)),
    ]);
    Ok(crate::digest(&payload.json(false)?))
}

fn moved_refs<'a>(git: &Git<'_>, base: &'a Pinned, rows: &'a [Step]) -> Result<Vec<&'a str>> {
    let mut moved = Vec::new();
    for pinned in std::iter::once(base).chain(rows.iter().map(|row| &row.head)) {
        if resolve(git, &pinned.name)?.as_deref() != Some(&pinned.oid) {
            moved.push(pinned.name.as_str());
        }
    }
    Ok(moved)
}

fn report(
    base: &Pinned,
    rows: &[Step],
    status: (&str, i32),
    tree: Option<&str>,
    conflicts: &[String],
    moved: &[&str],
    json: bool,
) -> Result<Output> {
    let digest = tree.map(|_| preview_digest(base, rows)).transpose()?;
    let mut output = if json {
        let steps = rows
            .iter()
            .map(|row| {
                let dependencies = row
                    .depends_on
                    .iter()
                    .map(|&index| rows[index].head.name.as_str().into())
                    .collect();
                obj([
                    ("ref", row.head.name.as_str().into()),
                    ("oid", row.head.oid.as_str().into()),
                    ("status", row.status.into()),
                    (
                        "tree_oid",
                        if row.tree.is_empty() {
                            V::Null
                        } else {
                            row.tree.as_str().into()
                        },
                    ),
                    ("depends_on", V::Arr(dependencies)),
                ])
            })
            .collect();
        Output::json(&obj([
            ("schema", 1usize.into()),
            ("preview_only", true.into()),
            ("status", status.0.into()),
            ("base", pin_value(base)),
            ("steps", V::Arr(steps)),
            ("tree_oid", tree.map_or(V::Null, V::from)),
            ("preview_sha256", digest.as_deref().map_or(V::Null, V::from)),
            (
                "conflict_paths",
                V::Arr(conflicts.iter().map(|s| s.as_str().into()).collect()),
            ),
            (
                "moved_refs",
                V::Arr(moved.iter().map(|s| (*s).into()).collect()),
            ),
        ]))?
    } else {
        use std::fmt::Write as _;
        let mut text = format!(
            "land batch-preview: {}\nbase {} {}\n",
            status.0, base.name, base.oid
        );
        for row in rows {
            let _ = write!(text, "{} {} {}", row.status, row.head.name, row.head.oid);
            for &index in &row.depends_on {
                let _ = write!(text, " needs {}", rows[index].head.name);
            }
            text.push('\n');
        }
        for path in conflicts {
            let _ = writeln!(text, "conflict path {path}");
        }
        for name in moved {
            let _ = writeln!(text, "ref changed {name}");
        }
        if let Some(tree) = tree {
            let _ = writeln!(text, "preview tree {tree}");
        }
        if let Some(digest) = digest {
            let _ = writeln!(text, "preview sha256 {digest}");
        }
        Output::text(text)
    };
    output.code = status.1;
    Ok(output)
}

pub(crate) fn execute(c: &Context<'_>, options: &Options) -> Result<Output> {
    let verify = options.action("") == "batch-check";
    require(
        !verify || cfg!(target_os = "linux"),
        "batch-check requires Linux",
    )?;
    let refs = &options.words[1..];
    require(
        (1..=MAX_REFS).contains(&refs.len()),
        "batch-preview needs 1 to 8 ordered branch refs",
    )?;
    require(
        refs.iter().collect::<BTreeSet<_>>().len() == refs.len(),
        "batch-preview branch refs must be distinct",
    )?;
    let deadline = Instant::now() + DEADLINE;
    let repo = c.repo(options)?;
    let source = Git {
        repo: &repo,
        interrupted: c.interrupted,
        group_resolver: storage::current_group_resolver(),
        deadline: Some(deadline),
        group_snapshot: None,
    };
    source.guard()?;
    let version = source.text(&["version"])?;
    let mut version = version.split_whitespace().nth(2).unwrap_or("").split('.');
    let major = version.next().and_then(|part| part.parse::<u32>().ok());
    let minor = version.next().and_then(|part| part.parse::<u32>().ok());
    require(
        major.zip(minor).is_some_and(|version| version >= (2, 38)),
        "batch-preview requires Git 2.38 or newer",
    )?;
    require(
        source.text(&["rev-parse", "--is-shallow-repository"])? == "false",
        "batch-preview needs complete local commit history",
    )?;
    let base = pin(&source, options.get("base"), verify)?;
    let mut rows = refs
        .iter()
        .map(|name| {
            Ok(Step {
                head: pin(&source, name, verify)?,
                depends_on: Vec::new(),
                in_base: false,
                status: "blocked",
                tree: String::new(),
                cumulative_commit: String::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // A bare repository with only a read-only object alternate keeps merge
    // objects and executable Git configuration out of the caller's checkout.
    let format = source.text(&["rev-parse", "--show-object-format"])?;
    require(
        ["sha1", "sha256"].contains(&format.as_str()),
        "unsupported Git object format",
    )?;
    let common = source.namespace_path()?;
    let objects = common.join("objects").canonicalize()?;
    let mut alternate = objects.as_os_str().as_bytes().to_vec();
    require(
        objects.is_dir() && !alternate.iter().any(|b| b"\n\r".contains(b)),
        "unsupported Git object directory",
    )?;
    alternate.push(b'\n');
    let scratch_root = std::env::temp_dir().canonicalize()?;
    require(
        !scratch_root.starts_with(&repo)
            && !scratch_root.starts_with(&common)
            && !scratch_root.starts_with(&objects),
        "batch-preview needs a temporary directory outside the source repository",
    )?;
    let temporary = storage::Temp::new_in("oms-batch-preview", &scratch_root)?;
    let git = Git {
        repo: &temporary.path,
        interrupted: c.interrupted,
        group_resolver: None,
        deadline: Some(deadline),
        group_snapshot: None,
    };
    git.run(&[
        "init",
        "--bare",
        "--quiet",
        "--template=",
        &format!("--object-format={format}"),
    ])?;
    std::fs::write(temporary.path.join("objects/info/alternates"), alternate)?;
    record_dependencies(&git, &base, &mut rows)?;

    let mut status = ("clean", 0);
    let mut incoming = base.oid.clone();
    let mut tree = oid(git
        .text(&["rev-parse", &format!("{incoming}^{{tree}}")])?
        .as_bytes())?;
    let mut conflict_paths = Vec::new();
    let last = rows.len() - 1;
    for (index, row) in rows.iter_mut().enumerate() {
        if row.in_base || ancestor(&git, &row.head.oid, &incoming)? {
            row.status = "already_present";
            row.tree.clone_from(&tree);
            row.cumulative_commit.clone_from(&incoming);
            continue;
        }
        if row.depends_on.iter().any(|&dependency| dependency > index) {
            row.status = "out_of_order";
            status = ("out_of_order", 3);
            break;
        }
        let merge = query(
            &git,
            &[
                "-c",
                "core.attributesFile=/dev/null",
                "merge-tree",
                "--write-tree",
                "--no-messages",
                "--name-only",
                "-z",
                &incoming,
                &row.head.oid,
            ],
            256 * 1024,
        )?;
        require(
            [0, 1].contains(&merge.code),
            "cannot preview cumulative Git merge",
        )?;
        let fields = merge
            .stdout
            .strip_suffix(&[0])
            .ok_or_else(|| Error::new(2, "unexpected Git merge result"))?;
        let mut fields = fields.split(|b| *b == 0);
        let candidate = oid(fields.next().unwrap_or_default())?;
        if merge.code == 1 {
            row.status = "conflict";
            status = ("conflict", 1);
            conflict_paths = fields
                .filter(|path| !path.is_empty())
                .take(MAX_CONFLICT_PATHS)
                .map(|path| {
                    let escaped = String::from_utf8_lossy(path).escape_debug().to_string();
                    oms_contract_markdown::bounded_line(&escaped, 200)
                })
                .collect();
            break;
        }
        require(fields.next().is_none(), "unexpected clean Git merge result")?;
        row.status = "clean";
        if index < last || verify {
            let synthetic = query(
                &git,
                &[
                    "-c",
                    "commit.gpgSign=false",
                    "-c",
                    "user.name=OMS",
                    "-c",
                    "user.email=oms@localhost",
                    "commit-tree",
                    &candidate,
                    "-p",
                    &incoming,
                    "-p",
                    &row.head.oid,
                    "-m",
                    "OMS batch preview",
                ],
                128,
            )?;
            require(synthetic.code == 0, "cannot build private Git merge prefix")?;
            incoming = oid(trim(&synthetic.stdout))?;
        }
        tree = candidate;
        row.tree.clone_from(&tree);
        row.cumulative_commit.clone_from(&incoming);
    }
    let moved = moved_refs(&source, &base, &rows)?;
    if !moved.is_empty() {
        status = ("stale", 3);
    }
    if verify {
        let batch = Batch {
            source: &repo,
            bare: &temporary.path,
            base: &base,
            rows: &rows,
        };
        return batch.check(c, options, status, moved);
    }
    report(
        &base,
        &rows,
        status,
        (status.1 == 0).then_some(tree.as_str()),
        &conflict_paths,
        &moved,
        options.has("json"),
    )
}

fn unavailable(c: &Context<'_>, error: Error, elapsed: Duration) -> BatchVerification {
    BatchVerification {
        outcome: if error.code == 130 || crate::check(c.interrupted).is_err() {
            VerificationOutcome::Cancelled
        } else {
            VerificationOutcome::Unavailable
        },
        verifier_exit: None,
        elapsed,
    }
}

struct Batch<'a> {
    source: &'a Path,
    bare: &'a Path,
    base: &'a Pinned,
    rows: &'a [Step],
}

impl<'a> Batch<'a> {
    fn moved(&self, c: &Context<'_>) -> Result<Vec<&'a str>> {
        let git = Git {
            repo: self.source,
            interrupted: c.interrupted,
            group_resolver: storage::current_group_resolver(),
            deadline: Some(Instant::now() + DEADLINE),
            group_snapshot: None,
        };
        moved_refs(&git, self.base, self.rows)
    }

    fn checkout(&self, c: &Context<'_>, row: &Step) -> Result<Sandbox> {
        let git = crate::git::Git::new(self.bare, c.interrupted);
        // Advertise exactly this candidate to the local clone. This ref and
        // the integration commits exist only inside the disposable bare repo.
        git.run(&[
            "update-ref",
            "refs/heads/batch-candidate",
            &row.cumulative_commit,
        ])?;
        let checkout = Sandbox::new(self.bare, &row.cumulative_commit, c.interrupted)?;
        let git = crate::git::Git::new(&checkout.path, c.interrupted);
        git.run(&["remote", "remove", "origin"])?;
        require(
            git.text(&["rev-parse", "HEAD^{tree}"])? == row.tree
                && git.commit("HEAD")? == row.cumulative_commit
                && !checkout
                    .path
                    .join(".git/objects/info/alternates")
                    .try_exists()?,
            "private candidate does not match the reviewed integration tree",
        )?;
        let scripts = checkout.path.join("scripts");
        require(
            std::fs::symlink_metadata(&scripts).is_ok_and(|meta| meta.file_type().is_dir())
                && std::fs::symlink_metadata(scripts.join("check.sh"))
                    .is_ok_and(|meta| meta.file_type().is_file()),
            "candidate has no regular scripts/check.sh verifier",
        )?;
        git.tracked_clean()?;
        Ok(checkout)
    }

    fn verify(&self, c: &Context<'_>, checkout: &Sandbox) -> BatchVerification {
        let measured = Instant::now();
        let request = VerifyRequest {
            argv: [
                "bash",
                "scripts/check.sh",
                "--affected",
                "--ci-parity",
                "--changed-from",
                &self.base.oid,
                "--changed-to",
                "HEAD",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
            cwd: checkout.path.clone(),
            wall: VERIFY_WALL,
            environment: Vec::new(),
        };
        c.services
            .supervise_batch(&request, c.interrupted)
            .unwrap_or_else(|error| unavailable(c, error, measured.elapsed()))
    }

    fn check(
        &self,
        c: &Context<'_>,
        options: &Options,
        preview_status: (&str, i32),
        moved: Vec<&str>,
    ) -> Result<Output> {
        let expected = options.get("expected-preview-sha256");
        let actual = (preview_status.1 == 0)
            .then(|| preview_digest(self.base, self.rows))
            .transpose()?;
        let mut status = ("stale", 3);
        let mut moved = moved;
        let mut outcomes = Vec::new();
        if actual.as_deref() == Some(expected) {
            let last = self.rows.len() - 1;
            let prefix = self.rows[..last]
                .iter()
                .rposition(|row| row.tree != self.rows[last].tree);
            for index in std::iter::once(last).chain(prefix) {
                let row = &self.rows[index];
                let prepared = Instant::now();
                let result = match self.checkout(c, row).and_then(|checkout| {
                    moved = self.moved(c)?;
                    Ok(checkout)
                }) {
                    Ok(checkout) if moved.is_empty() => self.verify(c, &checkout),
                    Ok(_) => {
                        status = ("stale", 3);
                        break;
                    }
                    Err(error) => unavailable(c, error, prepared.elapsed()),
                };
                let cancelled = result.outcome == VerificationOutcome::Cancelled
                    || crate::check(c.interrupted).is_err();
                let result_code = result
                    .verifier_exit
                    .map_or(V::Null, |code| V::Num(code.to_string()));
                outcomes.push(obj([
                    ("candidate_count", (index + 1).into()),
                    ("tree_oid", row.tree.as_str().into()),
                    ("outcome", result.outcome.status().into()),
                    ("verifier_exit", result_code),
                    (
                        "duration_ms",
                        V::Num(result.elapsed.as_millis().to_string()),
                    ),
                    ("scope", VERIFY_SCOPE.into()),
                ]));
                if cancelled {
                    status = ("cancelled", 130);
                    break;
                }
                match self.moved(c) {
                    Ok(changed) => moved = changed,
                    Err(error) => {
                        let outcome = unavailable(c, error, Duration::ZERO).outcome;
                        status = (outcome.status(), outcome.exit());
                        break;
                    }
                }
                if !moved.is_empty() {
                    status = ("stale", 3);
                    break;
                }
                if index == last {
                    status = (result.outcome.status(), result.outcome.exit());
                }
                if result.outcome != VerificationOutcome::Failed {
                    break;
                }
            }
        }
        let mut output = if options.has("json") {
            let candidates = self.rows.iter().map(|row| pin_value(&row.head)).collect();
            Output::json(&obj([
                ("schema", 1usize.into()),
                ("kind", "oms-local-batch-diagnostic".into()),
                ("hosted_ci_evidence", false.into()),
                ("status", status.0.into()),
                ("preview_status", preview_status.0.into()),
                ("base", pin_value(self.base)),
                ("candidates", V::Arr(candidates)),
                ("expected_preview_sha256", expected.into()),
                (
                    "observed_preview_sha256",
                    actual.as_deref().map_or(V::Null, V::from),
                ),
                (
                    "moved_refs",
                    V::Arr(moved.into_iter().map(V::from).collect()),
                ),
                ("checks", V::Arr(outcomes)),
            ]))?
        } else {
            use std::fmt::Write as _;
            let mut text = format!("land batch-check: {} (local diagnostic)\n", status.0);
            if status.0 == "stale" {
                text.push_str(
                    "Pinned preview no longer matches the current ordered refs and trees.\n",
                );
            } else if status.0 == "unavailable" {
                text.push_str("An isolated Linux verifier must be available.\n");
            }
            for name in moved {
                let _ = writeln!(text, "ref changed {name}");
            }
            for row in outcomes {
                let _ = writeln!(
                    text,
                    "first {} candidates · tree {} · {} · {} ms · {}",
                    row.get("candidate_count").py(),
                    row.get("tree_oid").py(),
                    row.get("outcome").py(),
                    row.get("duration_ms").py(),
                    row.get("scope").py(),
                );
            }
            Output::text(text)
        };
        output.code = status.1;
        Ok(output)
    }
}
