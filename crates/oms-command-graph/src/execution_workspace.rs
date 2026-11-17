//! A read cache is usable only when the whole dirty workspace is fingerprinted.
use super::*;
fn excluded(path: &str) -> bool {
    [".oms", ".git"]
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
}
fn git(repo: &Path, args: &[&str], cancel: &AtomicBool) -> Result<Vec<u8>> {
    let (code, raw, _) = util::command(repo, "git", args, cancel)?;
    if code != 0 {
        return Err(Error::new(format!(
            "workspace fingerprint could not run git {} in {} (exit {code})",
            args.first().copied().unwrap_or(""),
            repo.display()
        )));
    }
    Ok(raw)
}
pub fn fingerprint(
    repo: &Path,
    max_entries: usize,
    max_file_bytes: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    let raw = git(
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        cancel,
    )?;
    let head = util::git(repo, &["rev-parse", "HEAD"], cancel);
    let result = |digest: String, unsafe_: String, count: usize| {
        obj([
            ("schema", 1_i64.into()),
            ("head", head.clone().into()),
            ("digest", digest.into()),
            ("unsafe", unsafe_.into()),
            ("dirty_count", count.into()),
        ])
    };
    if head.is_empty() {
        return Ok(result(String::new(), "no-head".into(), 0));
    }
    let fields = raw.split(|b| *b == 0).collect::<Vec<_>>();
    let mut offset = 0;
    let mut dirty = vec![];
    while offset < fields.len() {
        let row = fields[offset];
        offset += 1;
        if row.len() < 4 {
            continue;
        }
        let status = String::from_utf8_lossy(&row[..2]).into_owned();
        let path = match std::str::from_utf8(&row[3..]) {
            Ok(s) => s.to_owned(),
            Err(_) => {
                return Ok(result(
                    String::new(),
                    "unreadable:[invalid-path]".into(),
                    dirty.len() + 1,
                ));
            }
        };
        if matches!(row[0], b'R' | b'C') {
            offset += 1;
        }
        if !excluded(&path) {
            dirty.push((status, path));
        }
    }
    dirty.sort_by(|a, b| a.1.cmp(&b.1));
    if dirty.len() > max_entries {
        return Ok(result(
            String::new(),
            format!("too-many-dirty-paths:{}", dirty.len()),
            dirty.len(),
        ));
    }
    let root =
        oms_safe_fs::Root::open(repo).map_err(|e| Error::new(format!("workspace root: {e:?}")))?;
    let mut entries = vec![];
    for (status, path) in &dirty {
        util::check(cancel)?;
        let content = if status.as_bytes().get(1) == Some(&b'D') || status == "D " {
            "deleted".into()
        } else {
            match root.read(path, max_file_bytes, oms_safe_fs::Policy::source()) {
                Ok(raw) => util::sha(raw),
                Err(error) => {
                    let reason = match error {
                        oms_safe_fs::Error::Oversize => "large-file",
                        _ if repo.join(path).is_symlink() => "symlink",
                        _ => "unreadable",
                    };
                    return Ok(result(
                        String::new(),
                        format!("{reason}:{path}"),
                        dirty.len(),
                    ));
                }
            }
        };
        entries.push(list([status.as_str(), path.as_str(), content.as_str()]));
    }
    let staged = git(repo, &["diff-index", "--cached", "HEAD", "--"], cancel)?;
    let staged = String::from_utf8_lossy(&staged)
        .lines()
        .filter(|line| !line.split('\t').skip(1).any(excluded))
        .collect::<Vec<_>>()
        .join("\n");
    let payload = obj([
        ("schema", 1_i64.into()),
        ("head", head.clone().into()),
        ("staged", staged.into()),
        ("entries", list(entries)),
    ]);
    Ok(result(
        util::sha(payload.json(false).map_err(Error::new)?),
        String::new(),
        dirty.len(),
    ))
}
pub fn cache_allowed(value: &Value) -> bool {
    value.get("digest").truth() && !value.get("unsafe").truth()
}
pub fn key(
    node: &Value,
    state: &Value,
    facts: &Value,
    id: &str,
    workspace: &str,
) -> Result<String> {
    if workspace.is_empty() {
        return Ok(String::new());
    }
    let upstream = object(
        map(state.get("nodes"))
            .into_iter()
            .filter(|(other, value)| other != id && st(value, "status") == "finished")
            .map(|(other, value)| (other, value.get("outcome").clone())),
    );
    let payload = obj([
        ("schema", 1_i64.into()),
        ("cache", 2_i64.into()),
        ("node", node.clone()),
        ("head", facts.get("git.head").clone()),
        ("workspace", workspace.into()),
        ("upstream", upstream),
    ]);
    Ok(util::sha(payload.json(false).map_err(Error::new)?))
}
