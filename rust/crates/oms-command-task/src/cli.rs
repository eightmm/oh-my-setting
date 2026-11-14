use crate::{Error, Result, hex};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

pub const USAGE: &[u8] = include_bytes!("usage.txt");
pub const MODES: &[&str] = &[
    "path", "init", "show", "context", "status", "update", "append", "verify", "rotate", "close",
];
const OPTIONS: &[(&str, &str)] = &[
    ("dedupe-key", "hash"),
    ("repo", "path"),
    ("file", "path"),
    ("agent", "name"),
    ("goal", "text"),
    ("constraint", "text"),
    ("done", "text"),
    ("verify", "command"),
    ("loop-attempts", "number"),
    ("loop-max", "number"),
    ("diff-budget", "number"),
    ("verify-level", "text"),
    ("reason", "text"),
    ("replace-goal", "reason"),
    ("skip-verify-run", "reason"),
    ("last-failure", "text"),
    ("verification", "text"),
    ("hypothesis", "text"),
    ("result", "text"),
    ("decision", "text"),
    ("decision-reason", "text"),
    ("rejected", "text"),
    ("state", "text"),
    ("next", "text"),
    ("source-session", "hash"),
    ("text", "text"),
];
#[derive(Default)]
pub(crate) struct Options {
    pub action: Vec<u8>,
    pub values: BTreeMap<String, Vec<u8>>,
    pub stdin: bool,
    pub json: bool,
    pub help: bool,
}
impl Options {
    pub fn value(&self, key: &str) -> &[u8] {
        self.values.get(key).map(Vec::as_slice).unwrap_or_default()
    }
    pub fn has(&self, key: &str) -> bool {
        !self.value(key).is_empty()
    }
    pub fn mode(&self, mode: &str) -> bool {
        self.action == mode.as_bytes()
    }
    pub fn path(&self, key: &str, cwd: &Path) -> Option<PathBuf> {
        self.has(key).then(|| {
            let raw = Path::new(OsStr::from_bytes(self.value(key)));
            if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                cwd.join(raw)
            }
        })
    }
    pub fn agent(&self) -> Vec<u8> {
        if self.has("agent") {
            return self.value("agent").to_vec();
        }
        if let Some(v) = std::env::var_os("OMS_AGENT").filter(|s| !s.is_empty()) {
            return v.as_bytes().to_vec();
        }
        if ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"]
            .iter()
            .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
        {
            b"claude".to_vec()
        } else if std::env::var_os("CODEX_SANDBOX").is_some_and(|v| !v.is_empty()) {
            b"codex".to_vec()
        } else {
            b"agent".to_vec()
        }
    }
    pub fn arguments(&self) -> Vec<Vec<u8>> {
        self.values
            .iter()
            .flat_map(|(k, v)| [format!("--{k}").into_bytes(), v.clone()])
            .collect()
    }
    pub fn journal_update(&self) -> bool {
        ["result", "last-failure", "verification", "decision", "next"]
            .iter()
            .any(|k| self.has(k))
    }
}
pub(crate) fn parse(args: &[OsString]) -> Result<Options> {
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_bytes();
        if [b"-h".as_slice(), b"--help"].contains(&arg) {
            o.help = true;
            return Ok(o);
        }
        if let Some((name, label)) = OPTIONS
            .iter()
            .find(|(name, _)| arg.strip_prefix(b"--") == Some(name.as_bytes()))
        {
            if i + 1 == args.len() {
                return Err(Error::new(2, format!("--{name} requires {label}")));
            }
            o.values
                .insert((*name).into(), args[i + 1].as_bytes().to_vec());
            i += 2;
            continue;
        }
        match arg {
            b"--stdin" => o.stdin = true,
            b"--json" => o.json = true,
            _ if MODES.iter().any(|mode| mode.as_bytes() == arg) => o.action = arg.to_vec(),
            _ if o.action.is_empty() => o.action = arg.to_vec(),
            _ => {
                let mut v = o.value("text").to_vec();
                if !v.is_empty() {
                    v.push(b' ');
                }
                v.extend_from_slice(arg);
                o.values.insert("text".into(), v);
            }
        }
        i += 1;
    }
    if o.action.is_empty() {
        o.action = b"show".to_vec();
    }
    if o.has("dedupe-key") && (!o.mode("append") || !hex(o.value("dedupe-key"), 64, 64)) {
        return Err(Error::new(
            2,
            "--dedupe-key requires append and a 64-character lowercase hex hash",
        ));
    }
    if o.json && !o.mode("status") {
        return Err(Error::new(2, "--json is only supported by status"));
    }
    if !o.mode("append") && o.has("text") {
        return Err(Error::bytes(
            2,
            [b"unknown argument: ".as_slice(), o.value("text")].concat(),
        ));
    }
    if !o.has("decision") && (o.has("decision-reason") || o.has("rejected")) {
        return Err(Error::new(
            2,
            "--decision-reason and --rejected require --decision",
        ));
    }
    if o.has("decision") {
        fn part(raw: &[u8], limit: usize) -> Vec<u8> {
            let mut flattened = Vec::new();
            let mut space = false;
            for b in raw {
                let b = if b"\n\r\t".contains(b) { b' ' } else { *b };
                if b == b' ' {
                    space = !flattened.is_empty();
                } else {
                    if space {
                        flattened.push(b' ');
                    }
                    flattened.push(b);
                    space = false;
                }
            }
            // GNU cut's -c has the byte behavior used by the canonical writer.
            // Final note normalization drops an incomplete UTF-8 suffix.
            flattened.truncate(limit);
            flattened
        }
        let mut decision = part(o.value("decision"), 100);
        for (key, prefix, cap) in [
            ("decision-reason", b" | why: ".as_slice(), 110),
            ("rejected", b" | rejected: ".as_slice(), 60),
        ] {
            if o.has(key) {
                decision.extend_from_slice(prefix);
                decision.extend_from_slice(&part(o.value(key), cap));
            }
        }
        o.values.insert("decision".into(), decision);
    }
    for key in ["loop-attempts", "loop-max", "diff-budget"] {
        if o.value(key).iter().any(|b| !b.is_ascii_digit()) {
            return Err(Error::new(
                2,
                format!("--{key} must be a non-negative integer"),
            ));
        }
    }
    if o.has("source-session") && !hex(o.value("source-session"), 16, 64) {
        return Err(Error::new(
            2,
            "--source-session must be a 16-64 character lowercase hexadecimal hash",
        ));
    }
    if o.has("replace-goal") {
        if ![b"init".as_slice(), b"rotate", b"update"].contains(&o.action.as_slice()) {
            return Err(Error::new(
                2,
                "--replace-goal applies to init, rotate and update only",
            ));
        }
        if o.value("replace-goal").iter().any(|b| b"\r\n".contains(b)) {
            return Err(Error::new(
                2,
                "--replace-goal requires a single-line reason",
            ));
        }
    }
    Ok(o)
}
pub(crate) fn child() -> bool {
    std::env::var_os("OMS_HARNESS_CHILD").as_deref() == Some(OsStr::new("1"))
        || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH")
            .filter(|v| !v.is_empty())
            .is_some_and(|v| v != OsStr::new("0"))
}
pub(crate) fn parent_guard(o: &Options) -> Result<()> {
    if !child() {
        return Ok(());
    }
    if o.has("replace-goal") {
        return Err(Error::new(
            2,
            "--replace-goal is parent-only; a harness child cannot replace a goal",
        ));
    }
    if [b"init".as_slice(), b"rotate", b"verify"].contains(&o.action.as_slice()) {
        return Err(Error::bytes(
            2,
            [
                o.action.as_slice(),
                b" is parent-only; a harness child cannot create, replace or verify the goal",
            ]
            .concat(),
        ));
    }
    if o.mode("update") && o.has("goal") {
        return Err(Error::new(
            2,
            "update --goal is parent-only; a harness child cannot replace the goal",
        ));
    }
    Ok(())
}
