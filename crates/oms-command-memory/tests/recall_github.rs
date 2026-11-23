#![forbid(unsafe_code)]
//! GitHub project mode: recall of reachable commits and cached merged pulls,
//! kept out of injected context, and graduate's pull-request preview.

use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("{prefix}-{}-{seq}-{nanos}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Re-runs `name` with hermetic HOME, global memory, locks and Git identity;
/// returns true inside that child.
fn isolated(name: &str) -> bool {
    if std::env::var_os("OMS_RECALL_GITHUB_TEST").is_some() {
        return true;
    }
    let home = TempDir::new("oms-recall-github-home");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", name, "--nocapture"]);
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.starts_with("OMS_") || key.starts_with("GIT_"))
        {
            command.env_remove(key);
        }
    }
    let result = command
        .env("OMS_RECALL_GITHUB_TEST", "1")
        .env("HOME", &home.0)
        .env("XDG_CONFIG_HOME", home.0.join("config"))
        .env("OH_MY_SETTING_GLOBAL_MEMORY", home.0.join("global.md"))
        .env("OMS_LOCK_DIR", home.0.join("locks"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Example Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "Example Author")
        .env("GIT_COMMITTER_EMAIL", "author@example.com")
        .output()
        .unwrap();
    assert!(
        result.status.success() && String::from_utf8_lossy(&result.stdout).contains("1 passed"),
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    false
}

const DECLARED: &str = "# P\n\n## Collaboration\n\n- GitHub project: example-org/example-repo\n";
const UNDECLARED: &str = "# P\n\n## Collaboration\n\n- Collaboration mode: careful\n";

struct Fixture {
    dir: TempDir,
    interrupted: AtomicBool,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: TempDir::new("oms-recall-github"),
            interrupted: AtomicBool::new(false),
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:example-org/example-repo.git",
        ]);
        let (code, _, err) = fixture.run(&["init"]);
        assert_eq!(code, 0, "{err}");
        let (code, _, err) = fixture.run(&["append", "--text", "widgetframe local memory note"]);
        assert_eq!(code, 0, "{err}");
        fixture
    }
    fn repo(&self) -> &Path {
        &self.dir.0
    }
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.repo())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn commit(&self, at: i64, message: &str) -> String {
        let date = format!("@{} +0000", 1_700_000_000 + at);
        let out = Command::new("git")
            .arg("-C")
            .arg(self.repo())
            .args(["commit", "-q", "--allow-empty", "-m", message])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }
    fn run(&self, items: &[&str]) -> (i32, String, String) {
        let mut args: Vec<OsString> = items.iter().map(OsString::from).collect();
        args.extend([OsString::from("--repo"), self.repo().into()]);
        let (code, out, err) =
            oms_command_memory::run_with_source(&args, self.repo(), self.repo(), &self.interrupted);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
    fn project(&self, body: &str) {
        std::fs::write(self.repo().join("PROJECT.md"), body).unwrap();
    }
    fn rows(&self, query: &str, source: &str) -> Vec<Value> {
        let (code, out, err) = self.run(&["recall", query, "--json"]);
        assert_eq!(code, 0, "{err}");
        out.lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["source"] == source)
            .collect()
    }
}

type Row<'a> = (&'a str, &'a str, &'a str, &'a str, &'a str, i64, u64);

/// A cache line; a reference starting with `#` belongs to the fixture repo.
fn pull((number, kind, state, title, labels, at, ttl): Row<'_>) -> String {
    let number = match number.strip_prefix('#') {
        Some(n) => format!("example-org/example-repo#{n}"),
        None => number.to_owned(),
    };
    format!(
        r#"{{"reference":"{number}","kind":"{kind}","state":"{state}","title":"{title}","labels":[{labels}],"body_sha256":"{}","captured_at":{at},"ttl_s":{ttl}}}"#,
        "a".repeat(64)
    )
}

/// Six commits (one carrying a machine path) and a cache with every row the
/// pull source must skip next to four merged matches.
fn history(fixture: &Fixture) -> Vec<String> {
    let machine = ["/ho", "me/person/privatedir"].concat();
    let shas = [
        "chore: unrelated start",
        "feat: widgetframe cache rows\n\nThe widgetframe body line stays bounded.",
        "fix: widgetframe ordering stable",
        "docs: describe widgetframe",
        &format!("chore: widgetframe moved to {machine}"),
        "test: widgetframe ordering coverage",
    ]
    .iter()
    .enumerate()
    .map(|(at, message)| fixture.commit(at as i64, message))
    .collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let private = format!("privatetitle {machine}");
    #[rustfmt::skip]
    let rows: [Row<'_>; 9] = [
        ("#11", "pull", "merged", "widgetframe alphamerge", r#""memory""#, now, 86_400),
        ("#12", "pull", "open", "widgetframe openpull", "", 300, 60),
        ("#14", "pull", "open", "widgetframe ordering latermerge", "", 100, 60),
        ("#14", "pull", "merged", "widgetframe ordering latermerge", "", 200, 60),
        ("#15", "issue", "closed", "widgetframe issuerow", "", 300, 60),
        ("other-org/example-repo#16", "pull", "merged", "widgetframe foreignrow", "", 300, 60),
        ("#17", "pull", "merged", "widgetframe gammamerge", r#""ordering""#, 300, 60),
        ("#18", "pull", "merged", "widgetframe deltamerge", "", 50, 60),
        ("#19", "pull", "merged", private.as_str(), "", 50, 60),
    ];
    let mut cache: Vec<String> = rows.into_iter().map(pull).collect();
    #[rustfmt::skip]
    let malformed = ("#13", "pull", "merged", "widgetframe malformedrow", "", 300, 60);
    cache.insert(1, pull(malformed).replace(&"a".repeat(64), "A"));
    cache.insert(1, "not json".into());
    let cache = cache.join("\n");
    std::fs::create_dir_all(fixture.repo().join(".oms/github")).unwrap();
    std::fs::write(fixture.repo().join(".oms/github/cache.jsonl"), cache).unwrap();
    shas
}

#[test]
fn project_mode_recalls_commits_and_merged_pulls_outside_injected_context() {
    if !isolated("project_mode_recalls_commits_and_merged_pulls_outside_injected_context") {
        return;
    }
    let fixture = Fixture::new();
    let shas = history(&fixture);
    let query = "widgetframe ordering";

    // Mode off, with or without PROJECT.md, leaves recall as it was.
    let off = fixture.run(&["recall", query]);
    assert_eq!(off.0, 0, "{}", off.2);
    assert!(
        !off.1
            .lines()
            .any(|l| l.starts_with("commit ") || l.starts_with("pr ")),
        "{}",
        off.1
    );
    assert!(!off.2.contains("warning"), "{}", off.2);
    let off_json = fixture.run(&["recall", query, "--json"]).1;
    fixture.project(UNDECLARED);
    assert_eq!(fixture.run(&["recall", query]), off);
    assert_eq!(fixture.run(&["recall", query, "--json"]).1, off_json);

    fixture.project(DECLARED);
    let (code, on, err) = fixture.run(&["recall", query]);
    assert_eq!(code, 0, "{err}");
    assert!(on.starts_with(&off.1), "{on}");
    assert!(!err.contains("warning"), "{err}");

    // Overlap first, then recency; the machine-path commit is excluded.
    let commits = fixture.rows(query, "commit");
    let subjects: Vec<_> = commits
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert_eq!(
        subjects,
        [
            "test: widgetframe ordering coverage",
            "fix: widgetframe ordering stable",
            "docs: describe widgetframe"
        ]
    );
    assert!(commits.iter().all(|c| c["trust"] == "repository"));
    assert_eq!(commits[0]["ref"], shas[5].as_str());
    assert_eq!(commits[0]["excerpt"], "test: widgetframe ordering coverage");
    assert!(!on.contains("privatedir"), "{on}");
    let body = &fixture.rows("bounded", "commit")[0];
    assert_eq!(body["ref"], shas[1].as_str());
    assert_eq!(body["excerpt"], "The widgetframe body line stays bounded.");

    // Merged, same-repository, well-formed rows only; newest row per pull wins.
    let pulls = fixture.rows(query, "pull_request");
    let refs: Vec<_> = pulls.iter().map(|p| p["ref"].as_str().unwrap()).collect();
    assert_eq!(
        refs,
        [
            "example-org/example-repo#17",
            "example-org/example-repo#14",
            "example-org/example-repo#11"
        ]
    );
    let shown = |p: &Value| p["trust"] == "untrusted" && p["state"] == "merged";
    assert!(pulls.iter().all(shown));
    let fresh: Vec<_> = pulls.iter().map(|p| p["freshness"].as_str()).collect();
    assert_eq!(fresh, [Some("stale"), Some("stale"), Some("fresh")]);
    assert_eq!(pulls[0]["labels"], serde_json::json!(["ordering"]));
    assert!(pulls.iter().all(|p| p.get("body").is_none()));
    let line = format!(
        "commit {} test: widgetframe ordering coverage\n",
        &shas[5][..12]
    );
    assert!(on.contains(&line), "{on}");
    let line = "pr example-org/example-repo#17 widgetframe gammamerge (stale)\n";
    assert!(on.contains(line), "{on}");
    let withheld = fixture.rows("privatetitle", "pull_request");
    assert_eq!(
        (&withheld[0]["withheld"], &withheld[0]["title"]),
        (&Value::Bool(true), &Value::from(""))
    );

    // Neither source reaches injected provider context or the database adapter.
    let interrupted = AtomicBool::new(false);
    let (_, context, _) = oms_command_memory::provider_context(fixture.repo(), query, &interrupted);
    let context = String::from_utf8(context).unwrap();
    let note = "widgetframe local memory note";
    assert!(context.contains(note), "{context}");
    let memory = fixture.repo().join(".oms/memory");
    let args: Vec<OsString> = [
        "recall".into(),
        "--repo".into(),
        fixture.repo().as_os_str().to_owned(),
        "--db".into(),
        memory.join("memory.sqlite3").into(),
        "--shared".into(),
        memory.join("shared.md").into(),
        "--pins".into(),
        memory.join("pins.md").into(),
        "--query".into(),
        query.into(),
    ]
    .to_vec();
    let (_, database, _) = oms_command_memory::run_database(&args, fixture.repo(), &interrupted);
    let database = String::from_utf8(database).unwrap();
    assert!(database.contains(note), "{database}");
    let leaks = ["gammamerge", "alphamerge", "example-org/example-repo#"];
    for view in [&context, &database] {
        for leaked in leaks.into_iter().chain([&shas[5][..12]]) {
            assert!(!view.contains(leaked), "{leaked}: {view}");
        }
    }
}

#[test]
fn graduate_previews_pull_request_section_in_project_mode() {
    if !isolated("graduate_previews_pull_request_section_in_project_mode") {
        return;
    }
    let fixture = Fixture::new();
    std::fs::write(fixture.repo().join("AGENTS.md"), "# Rules\n").unwrap();
    let (_, found, _) = fixture.run(&["search", "widgetframe", "--json"]);
    let id = serde_json::from_str::<Value>(found.lines().next().unwrap()).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let graduate = |to: &str| fixture.run(&["graduate", "--id", &id, "--to", to]);

    fixture.project(UNDECLARED);
    let (code, diff, _) = graduate("PROJECT.md");
    assert_eq!(code, 0);
    assert!(
        diff.starts_with("--- a/PROJECT.md\n+++ b/PROJECT.md\n")
            && diff.contains("+## Decisions\n")
            && diff.contains("+- widgetframe local memory note\n"),
        "{diff}"
    );

    fixture.project(DECLARED);
    let (code, preview, err) = graduate("PROJECT.md");
    assert_eq!(code, 0, "{err}");
    assert!(
        preview.starts_with(&format!(
            "## Decision and evidence\n\n- widgetframe local memory note\n- Local memory note: {id}\n\n"
        )) && preview.contains("Put this section in the pull request that lands the change")
            && !preview.contains("--- a/"),
        "{preview}"
    );
    let project = std::fs::read_to_string(fixture.repo().join("PROJECT.md")).unwrap();
    assert_eq!(project, DECLARED);
    let (_, json, _) = fixture.run(&["graduate", "--id", &id, "--to", "PROJECT.md", "--json"]);
    let json: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["applied"], false);
    assert_eq!(json["diff"], serde_json::json!([]));
    assert_eq!(json["github_project"], "example-org/example-repo");
    let (_, agents, _) = graduate("AGENTS.md");
    let item = "+- widgetframe local memory note\n";
    assert!(
        agents.starts_with("--- a/AGENTS.md\n") && agents.contains(item),
        "{agents}"
    );
}

#[test]
fn declaration_errors_warn_once_and_tracked_forks_stay_silent() {
    if !isolated("declaration_errors_warn_once_and_tracked_forks_stay_silent") {
        return;
    }
    let fixture = Fixture::new();
    history(&fixture);
    let off = fixture.run(&["recall", "widgetframe ordering"]);
    // `None` expects a silent off: a tracked declaration origin does not name.
    let off_with = |reason: Option<&str>| {
        let (code, out, err) = fixture.run(&["recall", "widgetframe ordering"]);
        assert_eq!((code, out.as_str()), (off.0, off.1.as_str()));
        let warnings: Vec<_> = err.lines().filter(|l| l.starts_with("warning:")).collect();
        let expected = reason.map(|r| format!("warning: GitHub project mode is off: {r}"));
        assert_eq!(warnings, expected.as_slice(), "{err}");
    };
    let url = "https://github.com/example-org/example-repo";
    fixture.project(&DECLARED.replace("example-org/example-repo", url));
    off_with(Some(
        "PROJECT.md '- GitHub project:' must be one owner/repo slug",
    ));
    let fork = DECLARED.replace("example-org", "fork-org");
    fixture.project(&fork);
    let mismatch = "PROJECT.md declares a different GitHub project than origin";
    off_with(Some(mismatch));
    // The tracked declaration at HEAD must agree with PROJECT.md.
    std::fs::create_dir_all(fixture.repo().join(".github")).unwrap();
    std::fs::write(fixture.repo().join(".github/oms-project.md"), DECLARED).unwrap();
    fixture.git(&["add", ".github/oms-project.md"]);
    fixture.commit(10, "chore: track project declaration");
    off_with(Some(
        "PROJECT.md and .github/oms-project.md declare different GitHub projects",
    ));
    std::fs::remove_file(fixture.repo().join("PROJECT.md")).unwrap();
    std::fs::create_dir(fixture.repo().join("PROJECT.md")).unwrap();
    off_with(Some("PROJECT.md is not a regular file"));
    std::fs::remove_dir(fixture.repo().join("PROJECT.md")).unwrap();
    // Tracked only: a clone whose origin is elsewhere simply stays off.
    let gitlab = "https://gitlab.com/example-org/example-repo.git";
    fixture.git(&["remote", "set-url", "origin", gitlab]);
    off_with(None);
    fixture.git(&["remote", "remove", "origin"]);
    off_with(None);
    // An explicit local declaration without a GitHub origin fails closed.
    fixture.project(DECLARED);
    off_with(Some(
        "PROJECT.md and .github/oms-project.md declare a GitHub project, but origin is missing or not a github.com remote",
    ));
}

#[test]
fn tracked_declaration_alone_turns_project_mode_on() {
    if !isolated("tracked_declaration_alone_turns_project_mode_on") {
        return;
    }
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.repo().join(".github")).unwrap();
    std::fs::write(fixture.repo().join(".github/oms-project.md"), DECLARED).unwrap();
    fixture.git(&["add", ".github/oms-project.md"]);
    let sha = fixture.commit(0, "feat: widgetframe tracked declaration");
    assert!(!fixture.repo().join("PROJECT.md").exists());
    let tracked = fixture.git(&["status", "--porcelain", "--", ".github"]);
    assert_eq!(tracked, "");
    let commits = fixture.rows("widgetframe", "commit");
    assert_eq!(commits.len(), 1, "{commits:?}");
    assert_eq!(commits[0]["ref"], sha.as_str());
    // Graduation previews the pull request section without a local PROJECT.md.
    let (_, found, _) = fixture.run(&["search", "widgetframe", "--json"]);
    let id = serde_json::from_str::<Value>(found.lines().next().unwrap()).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (code, preview, err) = fixture.run(&["graduate", "--id", &id, "--to", "PROJECT.md"]);
    assert_eq!(code, 0, "{err}");
    let heading = "## Decision and evidence\n";
    assert!(preview.starts_with(heading), "{preview}");
}
