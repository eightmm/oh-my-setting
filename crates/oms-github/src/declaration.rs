//! The one reader of a repository's GitHub project declaration, shared by
//! agent-task, agent-plan, agent-memory and `oms github`. Two sources use the
//! `project_mode` grammar: the per-checkout `PROJECT.md` and the reviewed,
//! tracked `.github/oms-project.md`, which is read from the HEAD commit so an
//! uncommitted edit never changes the mode. Callers still bind the result to
//! origin and fail closed on a mismatch.
use crate::{Error, Result, Slug, project_mode};
use std::{
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const TRACKED_REL: &str = ".github/oms-project.md";
const LOCAL_REL: &str = "PROJECT.md";
const LOCAL_MAX: usize = 1 << 20;
const TRACKED_MAX: usize = 64 << 10;
const NEEDLE: &[u8] = b"- GitHub project:";
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Local,
    Tracked,
    Both,
}
impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Local => "local",
            Source::Tracked => "tracked",
            Source::Both => "both",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub slug: Slug,
    pub source: Source,
}

/// The declared project for the Git toplevel `root`, or `None` when neither
/// source declares one.
pub fn load(root: &Path) -> Result<Option<Slug>> {
    Ok(inspect(root)?.map(|d| d.slug))
}

/// Like [`load`], also naming which source declared the project.
pub fn inspect(root: &Path) -> Result<Option<Declaration>> {
    combine(local(root)?, tracked(root)?)
}

/// The one origin guard: the project slug when the mode is active. A
/// tracked-only declaration that origin does not name belongs to the upstream
/// project, so a fork or other clone simply stays off; an explicit local
/// declaration that conflicts with origin fails closed.
pub fn active(decl: Option<&Declaration>, origin: Option<&Slug>) -> Result<Option<Slug>> {
    let Some(decl) = decl else {
        return Ok(None);
    };
    let who = match decl.source {
        Source::Tracked => "",
        Source::Local => "PROJECT.md declares",
        Source::Both => "PROJECT.md and .github/oms-project.md declare",
    };
    match origin {
        Some(origin) if decl.slug.matches(origin) => Ok(Some(decl.slug.clone())),
        _ if decl.source == Source::Tracked => Ok(None),
        Some(_) => Err(Error::new(format!(
            "{who} a different GitHub project than origin"
        ))),
        None => Err(Error::new(format!(
            "{who} a GitHub project, but origin is missing or not a github.com remote"
        ))),
    }
}

fn combine(local: Option<Slug>, tracked: Option<Slug>) -> Result<Option<Declaration>> {
    let declared = |slug, source| Some(Declaration { slug, source });
    Ok(match (local, tracked) {
        (None, None) => None,
        (Some(slug), None) => declared(slug, Source::Local),
        (None, Some(slug)) => declared(slug, Source::Tracked),
        (Some(l), Some(t)) if l.matches(&t) => declared(t, Source::Both),
        _ => {
            return Err(Error::new(
                "PROJECT.md and .github/oms-project.md declare different GitHub projects",
            ));
        }
    })
}

/// Oversized or non-UTF-8 bytes are an error only when they could carry a
/// declaration; otherwise the source simply declares nothing.
fn parse(raw: &[u8], name: &str, max: usize) -> Result<Option<Slug>> {
    match std::str::from_utf8(raw) {
        Ok(text) if raw.len() <= max => {
            project_mode(text).map_err(|e| Error::new(e.message.replacen("PROJECT.md", name, 1)))
        }
        _ if raw.windows(NEEDLE.len()).any(|w| w == NEEDLE) => Err(Error::new(format!(
            "{name} is larger than its bound or not UTF-8"
        ))),
        _ => Ok(None),
    }
}

/// `root/PROJECT.md` as a regular file only. Without libc, std cannot open
/// with O_NOFOLLOW, so the checked metadata is compared with the opened
/// file's device and inode: a swap between the check and the open is refused
/// rather than followed.
fn local(root: &Path) -> Result<Option<Slug>> {
    let unreadable = || Error::new("PROJECT.md is unreadable");
    let path = root.join(LOCAL_REL);
    let checked = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unreadable()),
    };
    if !checked.file_type().is_file() {
        return Err(Error::new("PROJECT.md is not a regular file"));
    }
    let file = std::fs::File::open(&path).map_err(|_| unreadable())?;
    let opened = file.metadata().map_err(|_| unreadable())?;
    if !opened.is_file() || opened.dev() != checked.dev() || opened.ino() != checked.ino() {
        return Err(Error::new("PROJECT.md changed while it was read"));
    }
    let mut raw = Vec::new();
    file.take(LOCAL_MAX as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| unreadable())?;
    parse(&raw, LOCAL_REL, LOCAL_MAX)
}

/// One bounded Git query: exit code and at most `cap` bytes of stdout.
fn git(root: &Path, args: &[&str], cap: usize) -> Result<(Option<i32>, Vec<u8>)> {
    let fail = || Error::new(".github/oms-project.md could not be read from HEAD");
    let mut command = Command::new("git");
    // Inherited GIT_* state is dropped first; config isolation mirrors the
    // landing process runner.
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|k| k.starts_with("GIT_")) {
            command.env_remove(key);
        }
    }
    command
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-C"])
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "0")
        // A partial clone must not fetch a missing blob from the network.
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    let mut child = command.spawn().map_err(|_| fail())?;
    let stdout = child.stdout.take().ok_or_else(fail)?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let ok = stdout.take(cap as u64 + 1).read_to_end(&mut buf).is_ok();
        (ok, buf)
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail());
            }
        }
    };
    let (ok, buf) = reader.join().map_err(|_| fail())?;
    if !ok || buf.len() > cap {
        return Err(fail());
    }
    Ok((status.code(), buf))
}

fn is_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The declaration in the HEAD commit's blob. A root that is not a Git work
/// tree toplevel, no HEAD or no such path is `None`, so callers outside Git
/// still read PROJECT.md; any other Git failure inside a work tree (including
/// a timeout) fails closed.
fn tracked(root: &Path) -> Result<Option<Slug>> {
    let fail = || Error::new(".github/oms-project.md could not be read from HEAD");
    // No `.git` entry: not a toplevel, and no process is spawned.
    match std::fs::symlink_metadata(root.join(".git")) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(fail()),
    }
    // Git exits nonzero (128) when the directory is not a repository at all.
    // With stderr nulled and global config ignored, a checkout Git refuses
    // as "dubious ownership" (safe.directory) looks the same: accepted as
    // the tracked source being absent (mode off), not an error.
    match git(root, &["rev-parse", "--is-inside-work-tree"], 64)? {
        (Some(0), out) if out == b"true\n" => {}
        (Some(_), _) => return Ok(None),
        (None, _) => return Err(fail()),
    }
    let head_oid = match git(
        root,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        128,
    )? {
        (Some(0), out) => {
            let oid =
                std::str::from_utf8(out.strip_suffix(b"\n").unwrap_or(&out)).map_err(|_| fail())?;
            if !is_oid(oid) {
                return Err(fail());
            }
            oid.to_owned()
        }
        (Some(1), _) => return Ok(None),
        _ => return Err(fail()),
    };
    let (code, listing) = git(root, &["ls-tree", "-z", &head_oid, "--", TRACKED_REL], 4096)?;
    if code != Some(0) {
        return Err(fail());
    }
    if listing.is_empty() {
        return Ok(None);
    }
    let raw_entry = listing.strip_suffix(b"\0").ok_or_else(fail)?;
    if raw_entry.contains(&0) {
        return Err(fail());
    }
    let entry = std::str::from_utf8(raw_entry).map_err(|_| fail())?;
    let (meta, path) = entry.split_once('\t').ok_or_else(fail)?;
    let fields: Vec<_> = meta.split(' ').collect();
    let [mode, kind, oid] = fields[..] else {
        return Err(fail());
    };
    if path != TRACKED_REL || kind != "blob" || mode != "100644" || !is_oid(oid) {
        return Err(Error::new(
            ".github/oms-project.md must be a regular file in HEAD",
        ));
    }
    // Over the cap, the reader closes the pipe and `git` fails closed.
    let (code, blob) = git(root, &["cat-file", "blob", oid], TRACKED_MAX)?;
    if code != Some(0) {
        return Err(fail());
    }
    parse(&blob, TRACKED_REL, TRACKED_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slug(raw: &str) -> Option<Slug> {
        Some(Slug::parse(raw).unwrap())
    }

    #[test]
    fn combine_prefers_tracked_spelling_and_refuses_disagreement() {
        assert_eq!(combine(None, None), Ok(None));
        let local = combine(slug("example-org/example-repo"), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            (local.slug.to_string(), local.source),
            ("example-org/example-repo".into(), Source::Local)
        );
        let tracked = combine(None, slug("example-org/example-repo"))
            .unwrap()
            .unwrap();
        assert_eq!(tracked.source, Source::Tracked);
        let both = combine(
            slug("example-org/example-repo"),
            slug("Example-Org/Example-Repo"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (both.slug.to_string(), both.source.name()),
            ("Example-Org/Example-Repo".into(), "both")
        );
        let err = combine(
            slug("example-org/example-repo"),
            slug("other-org/example-repo"),
        )
        .unwrap_err();
        assert!(
            err.message.contains("declare different GitHub projects"),
            "{err}"
        );
        assert!(
            parse(b"## Collaboration\n- GitHub project: x\n", TRACKED_REL, 64)
                .unwrap_err()
                .message
                .starts_with(".github/oms-project.md ")
        );
        assert_eq!(parse(&[0xff, b'x'], LOCAL_REL, 64), Ok(None));
        assert!(parse(b"\xff- GitHub project: a/b", LOCAL_REL, 64).is_err());
    }

    #[test]
    fn active_lets_forks_ignore_a_tracked_declaration_only() {
        let upstream = Slug::parse("example-org/example-repo").unwrap();
        let fork = Slug::parse("fork-org/example-repo").unwrap();
        let decl = |source| Declaration {
            slug: upstream.clone(),
            source,
        };
        assert_eq!(active(None, Some(&fork)), Ok(None));
        let same = Slug::parse("Example-Org/Example-Repo").unwrap();
        for source in [Source::Local, Source::Tracked, Source::Both] {
            assert_eq!(
                active(Some(&decl(source)), Some(&same)),
                Ok(Some(upstream.clone()))
            );
        }
        assert_eq!(active(Some(&decl(Source::Tracked)), Some(&fork)), Ok(None));
        assert_eq!(active(Some(&decl(Source::Tracked)), None), Ok(None));
        for (source, origin, needle) in [
            (
                Source::Local,
                Some(&fork),
                "PROJECT.md declares a different",
            ),
            (
                Source::Both,
                Some(&fork),
                "and .github/oms-project.md declare a different",
            ),
            (Source::Local, None, "origin is missing"),
            (Source::Both, None, "origin is missing"),
        ] {
            let err = active(Some(&decl(source)), origin).unwrap_err();
            assert!(err.message.contains(needle), "{source:?}: {err}");
        }
    }

    #[test]
    fn a_directory_outside_git_reads_only_project_md() {
        let dir =
            std::env::temp_dir().join(format!("oms-github-declaration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = inspect(&dir);
        let written = std::fs::write(
            dir.join(LOCAL_REL),
            "## Collaboration\n\n- GitHub project: example-org/example-repo\n",
        );
        let declared = inspect(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        written.unwrap();
        assert_eq!(empty, Ok(None));
        let local = declared.unwrap().unwrap();
        assert_eq!(
            (local.slug.to_string(), local.source),
            ("example-org/example-repo".into(), Source::Local)
        );
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn temp_repo(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oms-github-declaration-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git_in(&dir, &["init", "-q"]);
        std::fs::write(dir.join("README.md"), "# test\n").unwrap();
        git_in(&dir, &["add", "README.md"]);
        git_in(&dir, &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn tracked_declaration_reads_exact_committed_blob_and_ignores_uncommitted_edits() {
        let dir = temp_repo("committed-blob");
        let tracked_path = dir.join(TRACKED_REL);
        std::fs::create_dir_all(tracked_path.parent().unwrap()).unwrap();

        // Untracked working-tree file is ignored.
        std::fs::write(
            &tracked_path,
            "## Collaboration\n\n- GitHub project: example-org/example-repo\n",
        )
        .unwrap();
        let untracked = inspect(&dir);

        // Staged-only file (not committed in HEAD) is ignored.
        git_in(&dir, &["add", TRACKED_REL]);
        let staged_only = inspect(&dir);

        // Committed 100644 blob loads cleanly as Source::Tracked.
        git_in(&dir, &["commit", "-qm", "declare project"]);
        let committed = inspect(&dir);

        // Dirty working-tree modification is ignored in favor of HEAD blob.
        std::fs::write(
            &tracked_path,
            "## Collaboration\n\n- GitHub project: other-org/other-repo\n",
        )
        .unwrap();
        let dirty_worktree = inspect(&dir);

        // Staged modification is also ignored in favor of HEAD blob.
        git_in(&dir, &["add", TRACKED_REL]);
        let dirty_index = inspect(&dir);

        // Deleting the working-tree file does not hide the committed HEAD blob.
        std::fs::remove_file(&tracked_path).unwrap();
        let deleted_worktree = inspect(&dir);

        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(untracked, Ok(None));
        assert_eq!(staged_only, Ok(None));
        let expected = Declaration {
            slug: Slug::parse("example-org/example-repo").unwrap(),
            source: Source::Tracked,
        };
        assert_eq!(committed, Ok(Some(expected.clone())));
        assert_eq!(dirty_worktree, Ok(Some(expected.clone())));
        assert_eq!(dirty_index, Ok(Some(expected.clone())));
        assert_eq!(deleted_worktree, Ok(Some(expected)));
    }

    #[test]
    fn tracked_declaration_rejects_symlink_and_non_100644_git_objects() {
        let dir = temp_repo("symlink-blob");
        std::fs::write(
            dir.join("target.md"),
            "## Collaboration\n\n- GitHub project: example-org/example-repo\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join(".github")).unwrap();
        std::os::unix::fs::symlink("../target.md", dir.join(TRACKED_REL)).unwrap();
        git_in(&dir, &["add", "target.md", TRACKED_REL]);
        git_in(&dir, &["commit", "-qm", "commit symlink declaration"]);
        let symlink_err = inspect(&dir).unwrap_err();

        // Replace symlink with executable 100755 blob in HEAD: also refused.
        std::fs::remove_file(dir.join(TRACKED_REL)).unwrap();
        std::fs::write(
            dir.join(TRACKED_REL),
            "## Collaboration\n\n- GitHub project: example-org/example-repo\n",
        )
        .unwrap();
        git_in(&dir, &["add", TRACKED_REL]);
        git_in(&dir, &["update-index", "--chmod=+x", TRACKED_REL]);
        git_in(&dir, &["commit", "-qm", "commit executable declaration"]);
        let exec_err = inspect(&dir).unwrap_err();

        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            symlink_err
                .message
                .contains("must be a regular file in HEAD"),
            "{symlink_err}"
        );
        assert!(
            exec_err.message.contains("must be a regular file in HEAD"),
            "{exec_err}"
        );
    }
}
