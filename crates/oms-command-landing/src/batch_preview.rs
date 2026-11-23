use crate::{Context, Error, Output, Result, V, cli::Options, obj, require, storage};
use oms_command_plan::repository::{Git, GitOutput, commit, trim};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant};

const MAX_REFS: usize = 8;
const MAX_REF_BYTES: usize = 200;
const MAX_CONFLICT_PATHS: usize = 20;
const DEADLINE: Duration = Duration::from_secs(30);

struct Pinned {
    name: String,
    oid: String,
}

struct Step {
    head: Pinned,
    depends_on: Vec<usize>,
    in_base: bool,
    status: &'static str,
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

fn pin(git: &Git<'_>, name: &str) -> Result<Pinned> {
    require(
        name.len() <= MAX_REF_BYTES
            && !name.chars().any(char::is_control)
            && (name.starts_with("refs/heads/") || name.starts_with("refs/remotes/"))
            && query(git, &["check-ref-format", name], 128)?.code == 0,
        "batch-preview needs a fully qualified local or remote-tracking branch ref",
    )?;
    let oid = resolve(git, name)?
        .ok_or_else(|| Error::new(2, format!("branch ref unavailable: {name}")))?;
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

fn report(
    base: &Pinned,
    rows: &[Step],
    status: (&str, i32),
    tree: Option<&str>,
    conflicts: &[String],
    moved: &[&str],
    json: bool,
) -> Result<Output> {
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
                    ("depends_on", V::Arr(dependencies)),
                ])
            })
            .collect();
        Output::json(&obj([
            ("schema", 1usize.into()),
            ("preview_only", true.into()),
            ("status", status.0.into()),
            (
                "base",
                obj([
                    ("ref", base.name.as_str().into()),
                    ("oid", base.oid.as_str().into()),
                ]),
            ),
            ("steps", V::Arr(steps)),
            ("tree_oid", tree.map_or(V::Null, V::from)),
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
        Output::text(text)
    };
    output.code = status.1;
    Ok(output)
}

pub(crate) fn execute(c: &Context<'_>, options: &Options) -> Result<Output> {
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
    let base = pin(&source, options.get("base"))?;
    let mut rows = refs
        .iter()
        .map(|name| {
            Ok(Step {
                head: pin(&source, name)?,
                depends_on: Vec::new(),
                in_base: false,
                status: "blocked",
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
    let mut tree = None;
    let mut conflict_paths = Vec::new();
    let last = rows.len() - 1;
    for (index, row) in rows.iter_mut().enumerate() {
        if row.in_base || ancestor(&git, &row.head.oid, &incoming)? {
            row.status = "already_present";
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
        if index < last {
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
        tree = Some(candidate);
    }
    let mut moved = Vec::new();
    for pinned in std::iter::once(&base).chain(rows.iter().map(|row| &row.head)) {
        if resolve(&source, &pinned.name)?.as_deref() != Some(&pinned.oid) {
            moved.push(pinned.name.as_str());
        }
    }
    if !moved.is_empty() {
        status = ("stale", 3);
    }
    if status.1 != 0 {
        tree = None;
    } else if tree.is_none() {
        tree = Some(oid(git
            .text(&["rev-parse", &format!("{}^{{tree}}", base.oid)])?
            .as_bytes())?);
    }
    report(
        &base,
        &rows,
        status,
        tree.as_deref(),
        &conflict_paths,
        &moved,
        options.has("json"),
    )
}
