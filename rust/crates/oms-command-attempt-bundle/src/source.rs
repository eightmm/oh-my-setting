use crate::value::V;
use oms_safe_fs::{Error, Policy, Root};
use std::path::{Path, PathBuf};

fn policy(single_link: bool) -> Policy {
    Policy {
        single_link,
        owner: None,
        max_components: None,
    }
}
pub(crate) fn read(
    root: &Root,
    path: &Path,
    limit: usize,
    single_link: bool,
) -> Result<Vec<u8>, String> {
    let result = if path.is_absolute() {
        let parent = path.parent().ok_or("invalid input path")?;
        let name = path.file_name().ok_or("invalid input path")?;
        Root::open(parent).and_then(|r| r.read_os(name, limit, policy(single_link)))
    } else {
        root.read_path(path, limit, policy(single_link))
    };
    match result {
        Ok(b) => Ok(b),
        Err(Error::Missing) => Ok(vec![]),
        Err(Error::Unsafe) => Err("ledger must be a regular non-linked file".into()),
        Err(e) => Err(format!("cannot read ledger: {e:?}")),
    }
}
pub(crate) fn rows(root: &Root, path: &Path, lifecycle: bool) -> Result<Vec<V>, String> {
    let bytes = read(
        root,
        path,
        if lifecycle {
            usize::MAX
        } else {
            32 * 1024 * 1024
        },
        lifecycle,
    )?;
    let mut out = Vec::new();
    if lifecycle {
        let text = String::from_utf8_lossy(&bytes)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        // TextIO retains each universal newline in the string passed to json.loads.
        for (index, line) in text.split_inclusive('\n').enumerate() {
            if lifecycle_blank(line) {
                continue;
            }
            out.push(row(root, path, line, index + 1)?);
        }
    } else {
        // common.read_jsonl splits and strips bytes before strict UTF-8 decoding.
        for (index, line) in byte_lines(&bytes).enumerate() {
            if state_blank(line) {
                continue;
            }
            if line.len() > 1024 * 1024 || out.len() >= 250_000 {
                return Err("append-only state exceeds the supported row limit".into());
            }
            let text = std::str::from_utf8(line).map_err(|_| "invalid UTF-8 JSONL")?;
            out.push(row(root, path, text, index + 1)?);
        }
    }
    Ok(out)
}
fn lifecycle_blank(line: &str) -> bool {
    line.chars()
        .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}
fn state_blank(line: &[u8]) -> bool {
    line.iter().all(|b| matches!(b, b'\t'..=b'\r' | b' '))
}
fn byte_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start == bytes.len() {
            return None;
        }
        let end = bytes[start..]
            .iter()
            .position(|b| matches!(b, b'\r' | b'\n'))
            .map_or(bytes.len(), |i| start + i);
        let line = &bytes[start..end];
        start = if bytes.get(end) == Some(&b'\r') && bytes.get(end + 1) == Some(&b'\n') {
            end + 2
        } else if end < bytes.len() {
            end + 1
        } else {
            end
        };
        Some(line)
    })
}
fn row(root: &Root, path: &Path, line: &str, number: usize) -> Result<V, String> {
    let display = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.physical_root().join(path)
    };
    let value = V::parse(line.as_bytes()).map_err(|_| {
        format!(
            "{}:{number} is not valid JSON: {}",
            display.display(),
            crate::diagnostic::json(line)
        )
    })?;
    if !matches!(value, V::Obj(_)) {
        return Err(format!(
            "{}:{number} is not a JSON object",
            display.display()
        ));
    }
    Ok(value)
}

pub(crate) fn expand(path: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let b = path.as_os_str().as_bytes();
    if (b == b"~" || b.starts_with(b"~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let mut p = PathBuf::from(home);
        if b.len() > 2 {
            p.push(std::ffi::OsStr::from_bytes(&b[2..]));
        }
        p
    } else {
        path.to_path_buf()
    }
}
pub(crate) fn root(path: &Path, cwd: &Path) -> Result<Root, String> {
    let path = expand(path);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let physical = std::fs::canonicalize(&path)
        .map_err(|_| format!("repository path does not exist: {}", path.display()))?;
    let repo = physical
        .ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(&physical);
    Root::open(repo).map_err(|_| "cannot open repository".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blank_predicates_match_text_and_bytes() {
        for c in [
            '\t', '\n', '\u{b}', '\u{c}', '\r', ' ', '\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}',
            '\u{85}', '\u{a0}',
        ] {
            assert!(lifecycle_blank(&c.to_string()));
        }
        for b in [9, 10, 11, 12, 13, 32] {
            assert!(state_blank(&[b]));
        }
        for line in [b"\x1c".as_slice(), b"\xc2\xa0", b"\xc2\x85", b"x", b"\xff"] {
            assert!(!state_blank(line));
        }
        assert!(lifecycle_blank(""));
        assert!(state_blank(b""));
        assert!(!lifecycle_blank("\0"));
        assert!(!lifecycle_blank("x"));
        let bytes = b"\r\n\xc2\x85\n\x1c\r{}\n";
        assert_eq!(
            byte_lines(bytes).collect::<Vec<_>>(),
            vec![b"".as_slice(), b"\xc2\x85", b"\x1c", b"{}"]
        );
    }
}
