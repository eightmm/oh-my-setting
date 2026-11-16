use oms_migrate::{inventory, parse_baseline, regressions, render};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);

impl Repo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oms-migrate-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let repo = Self(root);
        repo.git(&["init", "--quiet"]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    fn write(&self, path: &str, content: &[u8]) {
        let full = self.0.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    fn check(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_oms-migrate"))
            .args(["check", "--repo"])
            .arg(&self.0)
            .args(["--baseline", "baseline.tsv"])
            .args(extra)
            .output()
            .unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn tracks_working_tree_changes_and_preserves_check_exit_codes() {
    let repo = Repo::new();
    repo.write("src/old.py", b"first\nsecond\n");
    repo.write("run.sh", b"exec python3 src/old.py\n");
    repo.write("README.md", b"python3 documentation\n");
    repo.write("untracked.py", b"not tracked\n");
    repo.git(&["add", "src/old.py", "run.sh", "README.md"]);
    let original = inventory(&repo.0).unwrap();
    assert_eq!(original.len(), 2);
    assert_eq!(original["src/old.py"].units, 2);
    repo.write("baseline.tsv", render(&original).as_bytes());
    assert_eq!(repo.check(&[]).status.code(), Some(0));
    assert_eq!(repo.check(&["--require-zero"]).status.code(), Some(1));

    repo.write("src/old.py", b"remaining\n");
    repo.write("run.sh", b"printf done\n");
    let reduced = inventory(&repo.0).unwrap();
    assert!(regressions(&original, &reduced).is_empty());
    assert_eq!(repo.check(&[]).status.code(), Some(0));

    repo.git(&["add", "untracked.py"]);
    assert_eq!(repo.check(&[]).status.code(), Some(1));
    repo.git(&["rm", "--quiet", "--force", "src/old.py", "untracked.py"]);
    assert!(inventory(&repo.0).unwrap().is_empty());
    assert_eq!(repo.check(&["--require-zero"]).status.code(), Some(0));
    let text = fs::read_to_string(repo.0.join("baseline.tsv")).unwrap();
    assert_eq!(parse_baseline(&text).unwrap(), original);

    repo.write("baseline.tsv", b"malformed\n");
    assert_eq!(repo.check(&[]).status.code(), Some(2));
}

#[test]
fn unreadable_scoped_files_do_not_become_a_clean_inventory() {
    let repo = Repo::new();
    repo.write("broken.py", &[0xff]);
    repo.git(&["add", "broken.py"]);
    assert!(inventory(&repo.0).unwrap_err().contains("UTF-8"));
    repo.write("broken.py", b"valid UTF-8 but binary\0\n");
    assert!(inventory(&repo.0).unwrap_err().contains("binary"));
    fs::remove_file(repo.0.join("broken.py")).unwrap();
    assert!(inventory(&repo.0).unwrap_err().contains("cannot inspect"));

    #[cfg(unix)]
    {
        repo.write("target.txt", b"pass\n");
        std::os::unix::fs::symlink("target.txt", repo.0.join("broken.py")).unwrap();
        assert!(inventory(&repo.0).unwrap_err().contains("regular file"));
    }
}
