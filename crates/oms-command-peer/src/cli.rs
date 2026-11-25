use crate::{Result, fail};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

#[derive(Clone, Default, Debug)]
pub struct Args {
    values: BTreeMap<String, Vec<OsString>>,
    flags: BTreeSet<String>,
    pub positional: Vec<OsString>,
    pub command: Vec<OsString>,
}
impl Args {
    pub(crate) fn add_value(&mut self, key: &str, value: impl Into<OsString>) {
        self.values
            .entry(key.into())
            .or_default()
            .push(value.into());
    }
    pub(crate) fn set_value(&mut self, key: &str, value: impl Into<OsString>) {
        self.values.insert(key.into(), vec![value.into()]);
    }
    pub(crate) fn set_flag(&mut self, key: &str) {
        self.flags.insert(key.into());
    }
    pub(crate) fn remove(&mut self, key: &str) {
        self.flags.remove(key);
        self.values.remove(key);
    }
    /// Options do not abbreviate. The command following `--` is kept byte exact.
    pub fn parse(args: &[OsString], values: &[&str], flags: &[&str]) -> Result<Self> {
        let mut parsed = Self::default();
        let mut i = 0;
        while i < args.len() {
            let value = &args[i];
            if value == "--" {
                parsed.command.extend_from_slice(&args[i + 1..]);
                break;
            }
            let raw = value
                .to_str()
                .ok_or_else(|| crate::Error::new(2, "option names must be UTF-8"))?;
            let (name, inline) = raw
                .split_once('=')
                .map_or((raw, None), |(k, v)| (k, Some(v)));
            if name == "-h" || name == "--help" {
                parsed.flags.insert("help".into());
                i += 1;
                continue;
            }
            if let Some(key) = name.strip_prefix("--") {
                if values.contains(&key) {
                    let next = if let Some(v) = inline {
                        OsString::from(v)
                    } else {
                        i += 1;
                        args.get(i)
                            .filter(|v| !v.to_string_lossy().starts_with("--"))
                            .ok_or_else(|| {
                                crate::Error::new(2, format!("--{key} requires a value"))
                            })?
                            .clone()
                    };
                    parsed.values.entry(key.into()).or_default().push(next);
                } else if flags.contains(&key) && inline.is_none() {
                    parsed.flags.insert(key.into());
                } else {
                    return fail(format!("unknown option: {name}"));
                }
            } else if raw.starts_with('-') && raw != "-" {
                return fail(format!("unknown option: {raw}"));
            } else {
                parsed.positional.push(value.clone());
            }
            i += 1;
        }
        Ok(parsed)
    }
    pub fn has(&self, key: &str) -> bool {
        self.flags.contains(key) || self.values.contains_key(key)
    }
    pub fn os(&self, key: &str) -> Option<&OsStr> {
        self.values.get(key)?.last().map(OsString::as_os_str)
    }
    pub fn get(&self, key: &str) -> &str {
        self.os(key).and_then(OsStr::to_str).unwrap_or("")
    }
    pub fn value<'a>(&'a self, key: &str, fallback: &'a str) -> &'a str {
        if self.has(key) {
            self.get(key)
        } else {
            fallback
        }
    }
    pub fn all(&self, key: &str) -> Vec<String> {
        self.values
            .get(key)
            .into_iter()
            .flatten()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }
    pub fn required(&self, key: &str) -> Result<&str> {
        let v = self.get(key);
        if v.is_empty() {
            fail(format!("--{key} is required"))
        } else {
            Ok(v)
        }
    }
    pub fn limit(&self, key: &str, default: usize, low: usize, high: usize) -> Result<usize> {
        let n = if self.has(key) {
            self.get(key).parse::<usize>().ok()
        } else {
            Some(default)
        };
        n.filter(|n| (low..=high).contains(n))
            .ok_or_else(|| crate::Error::new(2, format!("--{key} must be {low}..{high}")))
    }
    pub fn path(&self, key: &str, cwd: &Path) -> PathBuf {
        cwd.join(self.os(key).unwrap_or_else(|| OsStr::new(".")))
    }
    pub fn action(&self) -> &str {
        self.positional
            .first()
            .and_then(|s| s.to_str())
            .unwrap_or("")
    }
    pub fn operand(&self, n: usize) -> &str {
        self.positional
            .get(n)
            .and_then(|s| s.to_str())
            .unwrap_or("")
    }
    pub fn exact_operands(&self, maximum: usize) -> Result<()> {
        if self.positional.len() > maximum || !self.command.is_empty() {
            fail("unexpected argument")
        } else {
            Ok(())
        }
    }
}
