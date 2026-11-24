//! Public checkout metadata from one local porcelain v2 status, without file names.
use serde_json::{Value, json};

pub const STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain=v2",
    "--branch",
    "--ahead-behind",
    "--untracked-files=normal",
    "--ignore-submodules=none",
];

pub fn object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn ref_label(value: &str) -> Option<&str> {
    (!value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control))
        .then_some(value)
}

pub fn snapshot(status: &[u8]) -> Value {
    let mut head = None;
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = None;
    let mut behind = None;
    let mut dirty = false;
    let (mut saw_head, mut saw_branch) = (false, false);
    for line in status.split(|b| *b == b'\n') {
        if let Some(value) = line
            .strip_prefix(b"# branch.oid ")
            .and_then(|v| std::str::from_utf8(v).ok())
        {
            saw_head = object_id(value) || value == "(initial)";
            head = object_id(value).then_some(value);
        } else if let Some(value) = line.strip_prefix(b"# branch.head ") {
            saw_branch = !value.is_empty();
            branch = std::str::from_utf8(value)
                .ok()
                .and_then(ref_label)
                .filter(|v| *v != "(detached)");
        } else if let Some(value) = line.strip_prefix(b"# branch.upstream ") {
            upstream = std::str::from_utf8(value).ok().and_then(ref_label);
        } else if let Some((a, b)) = line
            .strip_prefix(b"# branch.ab +")
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|v| v.split_once(" -"))
        {
            ahead = a.parse::<u64>().ok();
            behind = b.parse::<u64>().ok();
        } else if [b"1 ", b"2 ", b"u ", b"? "]
            .iter()
            .any(|prefix| line.starts_with(*prefix))
        {
            dirty = true;
        }
    }
    let available = saw_branch && saw_head;
    json!({"available":available,"head":head,"branch":branch,"upstream":upstream,
           "ahead":ahead,"behind":behind,"dirty":available.then_some(dirty)})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn porcelain_exposes_metadata_without_paths_and_preserves_unknowns() {
        let head = "a".repeat(40);
        let raw = format!(
            "# branch.oid {head}\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -3\n? untracked\n"
        );
        assert_eq!(
            snapshot(raw.as_bytes()),
            json!({"available":true,"head":head,"branch":"main","upstream":"origin/main","ahead":2,"behind":3,"dirty":true})
        );
        let unborn = snapshot(b"# branch.oid (initial)\n# branch.head main\n");
        assert_eq!(unborn["dirty"], false);
        assert!(unborn["head"].is_null() && unborn["ahead"].is_null());
        let detached =
            snapshot(format!("# branch.oid {head}\n# branch.head (detached)\n").as_bytes());
        assert_eq!(detached["available"], true);
        assert!(detached["branch"].is_null());
        assert!(snapshot(b"")["dirty"].is_null());
    }
}
