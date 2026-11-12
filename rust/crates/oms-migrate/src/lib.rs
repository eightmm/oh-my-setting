//! A lexical inventory of tracked legacy Python-bearing files, not a runtime proof.
//! Rust sources/build scripts, dynamic commands, and inert marker text require
//! separate review and execution on hosts without Python before migration completes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub const HEADER: &str = "path\tkind\tunits\n";
pub const BASELINE: &str = "migration/python-inventory.tsv";
pub type Inventory = BTreeMap<String, Entry>;
pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Source,
    Reference,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Source => "python-source",
            Self::Reference => "python-reference",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub units: usize,
}

fn validate_path(raw: &str) -> Result<()> {
    if raw.is_empty()
        || raw.chars().any(char::is_control)
        || raw.contains('\\')
        || Path::new(raw)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || raw.split('/').any(|part| part.is_empty() || part == ".")
    {
        return Err(format!("unsupported tracked path: {raw:?}"));
    }
    Ok(())
}

fn kind(path: &str) -> Option<Kind> {
    let extension = Path::new(path)
        .extension()
        .and_then(|part| part.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(extension.as_deref(), Some("py" | "pyw")) {
        return Some(Kind::Source);
    }
    // Rust (.rs), prose (.md), and inventory (.tsv) are outside this lexical
    // fence. Rust/build.rs review and clean-host execution remain independent.
    match extension.as_deref() {
        None | Some("") => Some(Kind::Reference),
        Some(
            "sh" | "bash" | "zsh" | "fish" | "js" | "mjs" | "cjs" | "json" | "yaml" | "yml"
            | "toml",
        ) => Some(Kind::Reference),
        _ => None,
    }
}

fn entry(path: &str, text: &str) -> Option<Entry> {
    let kind = kind(path)?;
    let units = match kind {
        Kind::Source => text.lines().count(),
        Kind::Reference => text
            .lines()
            .filter(|line| {
                line.as_bytes()
                    .windows(6)
                    .any(|word| word.eq_ignore_ascii_case(b"python"))
            })
            .count(),
    };
    (kind == Kind::Source || units > 0).then_some(Entry { kind, units })
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !output.status.success() {
        return Err(format!("git {args:?} failed with {}", output.status));
    }
    Ok(output.stdout)
}

pub fn repository_root(repo: &Path) -> Result<PathBuf> {
    let output = git(repo, &["rev-parse", "--show-toplevel"])?;
    let root = std::str::from_utf8(&output)
        .map_err(|_| "repository path is not UTF-8".to_string())?
        .trim_end_matches(['\r', '\n']);
    fs::canonicalize(root).map_err(|error| format!("cannot resolve repository root: {error}"))
}

pub fn inventory(repo: &Path) -> Result<Inventory> {
    let paths = git(repo, &["ls-files", "-z"])?;
    if !paths.is_empty() && paths.last() != Some(&0) {
        return Err("git returned an unterminated path list".to_string());
    }
    let mut result = Inventory::new();
    for raw in paths.split(|byte| *byte == 0).filter(|raw| !raw.is_empty()) {
        let path =
            std::str::from_utf8(raw).map_err(|_| "tracked filename is not UTF-8".to_string())?;
        validate_path(path)?;
        if kind(path).is_none() {
            continue;
        }
        let full = repo.join(path);
        let metadata = fs::symlink_metadata(&full)
            .map_err(|error| format!("cannot inspect {path:?}: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err(format!("scoped path must be a regular file: {path:?}"));
        }
        let text = fs::read_to_string(&full)
            .map_err(|error| format!("cannot read UTF-8 source {path:?}: {error}"))?;
        if text.contains('\0') {
            return Err(format!("scoped path contains binary data: {path:?}"));
        }
        if let Some(found) = entry(path, &text) {
            if result.insert(path.to_string(), found).is_some() {
                return Err(format!("duplicate tracked path: {path:?}"));
            }
        }
    }
    Ok(result)
}

pub fn render(inventory: &Inventory) -> String {
    let mut output = String::from(HEADER);
    for (path, found) in inventory {
        output.push_str(&format!("{path}\t{}\t{}\n", found.kind.name(), found.units));
    }
    output
}

pub fn parse_baseline(text: &str) -> Result<Inventory> {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER.trim_end()) {
        return Err("invalid inventory header; expected path/kind/units TSV".to_string());
    }
    let mut result = Inventory::new();
    let mut previous = "";
    for (index, line) in lines.enumerate() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 {
            return Err(format!("invalid inventory row {}", index + 2));
        }
        let path = fields[0];
        validate_path(path)?;
        if path <= previous {
            return Err(format!(
                "inventory paths must be unique and sorted: {path:?}"
            ));
        }
        let kind = match fields[1] {
            "python-source" => Kind::Source,
            "python-reference" => Kind::Reference,
            _ => return Err(format!("invalid inventory kind for {path:?}")),
        };
        if self::kind(path) != Some(kind) {
            return Err(format!(
                "inventory kind disagrees with scan scope: {path:?}"
            ));
        }
        let units = fields[2]
            .parse::<usize>()
            .map_err(|_| format!("invalid unit count for {path:?}"))?;
        if kind == Kind::Reference && units == 0 {
            return Err(format!("zero reference rows must be omitted: {path:?}"));
        }
        result.insert(path.to_string(), Entry { kind, units });
        previous = path;
    }
    Ok(result)
}

pub fn regressions(baseline: &Inventory, current: &Inventory) -> Vec<String> {
    let mut failures = Vec::new();
    for (path, found) in current {
        match baseline.get(path) {
            None => failures.push(format!("new {} path: {path:?}", found.kind.name())),
            Some(old) if old.kind != found.kind => {
                failures.push(format!("changed inventory kind: {path:?}"));
            }
            Some(old) if found.units > old.units => failures.push(format!(
                "increased {} units: {path:?} ({} -> {})",
                found.kind.name(),
                old.units,
                found.units
            )),
            _ => {}
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline(rows: &str) -> Inventory {
        parse_baseline(&(HEADER.to_string() + rows)).unwrap()
    }

    #[test]
    fn detects_sources_and_markers_without_interpreting_comments() {
        assert_eq!(entry("tests/empty.py", "").unwrap().units, 0);
        assert_eq!(entry("templates/run.py", "a\r\nb\n").unwrap().units, 2);
        for path in ["tests/source.PY", "templates/source.PyW"] {
            assert_eq!(entry(path, "pass\n").unwrap().kind, Kind::Source);
        }
        let text = "PYTHON3=tool\n# python parser fixture\npython3 -c pass; python3 -V\n";
        for path in [
            "scripts/oms",
            "install.sh",
            "config/task.yaml",
            "tests/test.js",
        ] {
            assert_eq!(entry(path, text).unwrap().units, 3);
        }
        for path in ["RUN.SH", "config/task.TOML", "tool."] {
            assert_eq!(entry(path, text).unwrap().units, 3);
        }
        assert!(entry("scripts/no-python.sh", "printf done\n").is_none());
    }

    #[test]
    fn reference_scope_excludes_prose_and_rust_but_never_python_sources() {
        for path in ["docs/README.md", "rust/build.rs", BASELINE] {
            assert!(entry(path, "python3 -V\n").is_none());
        }
        assert!(entry("rust/accidental.py", "pass\n").is_some());
        assert!(entry("rust/build-helper.sh", "python3 -V\n").is_some());
        assert!(entry("rust/Cargo.toml", "# Python binding\n").is_some());
        assert!(entry("docs/assets/render.py", "pass\n").is_some());
        assert!(entry("templates/python-ci.yml", "uses: setup-python\n").is_some());
    }

    #[test]
    fn baseline_cannot_hide_duplicates_bad_counts_or_unsafe_paths() {
        for rows in [
            "a.py\tpython-source\t1\na.py\tpython-source\t99\n",
            "b.py\tpython-source\t1\na.py\tpython-source\t1\n",
            "../a.py\tpython-source\t1\n",
            "/a.py\tpython-source\t1\n",
            "a.py\tpython-source\t-1\n",
            "a.py\tpython-source\t1\textra\n",
            "a.sh\tpython-reference\t0\n",
            "a.py\tpython-reference\t1\n",
            "a.rs\tpython-source\t1\n",
        ] {
            assert!(parse_baseline(&(HEADER.to_string() + rows)).is_err());
        }
        for path in ["a//b.py", "a/./b.py", "a\\b.py", "a\tb.py", "a\nb.py"] {
            assert!(validate_path(path).is_err());
        }
        assert!(parse_baseline("wrong header\n").is_err());
    }

    #[test]
    fn regression_check_allows_retirement_but_blocks_new_or_growing_paths() {
        let old = baseline("a.py\tpython-source\t10\nb.sh\tpython-reference\t3\n");
        let reduced = baseline("a.py\tpython-source\t2\n");
        assert!(regressions(&old, &reduced).is_empty());
        assert!(regressions(&old, &Inventory::new()).is_empty());
        let grew = baseline("a.py\tpython-source\t11\nb.sh\tpython-reference\t4\n");
        assert_eq!(regressions(&old, &grew).len(), 2);
        let renamed = baseline("new.py\tpython-source\t0\n");
        assert_eq!(regressions(&old, &renamed).len(), 1);
        assert_eq!(parse_baseline(&render(&old)).unwrap(), old);
    }
}
