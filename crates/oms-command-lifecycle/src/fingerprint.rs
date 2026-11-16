use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

pub(crate) fn tree(root: &Path, fingerprint: bool) -> Result<String> {
    if !root.is_dir() {
        return Ok("missing".into());
    }
    fn visit(
        base: &Path,
        path: &Path,
        h: &mut Sha256,
        fingerprint: bool,
        depth: usize,
    ) -> Result<()> {
        if depth > 128 {
            return r::fail("fingerprint tree exceeds nesting limit");
        }
        let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in &entries {
            let path = entry.path();
            let m = fs::symlink_metadata(&path)?;
            if m.is_dir() {
                continue;
            }
            let name = entry.file_name();
            if !fingerprint
                && matches!(
                    name.to_str(),
                    Some(".oh-my-setting-source-root" | ".oh-my-setting-source-sha256")
                )
            {
                continue;
            }
            h.update(path.strip_prefix(base).unwrap().as_os_str().as_bytes());
            h.update(b"\0");
            if m.file_type().is_symlink() && fingerprint {
                h.update(b"link:");
                h.update(fs::read_link(&path)?.as_os_str().as_bytes());
            } else {
                let source = if m.file_type().is_symlink() {
                    r::physical(&path)?
                } else {
                    path
                };
                h.update(if m.is_file() && m.nlink() > 1 {
                    crate::transaction::external_read(&source)?
                } else {
                    r::read(&source, 1024 * 1024 * 1024)?
                });
            }
            h.update(b"\0");
        }
        for e in &entries {
            if !e.file_type()?.is_dir() {
                continue;
            }
            if fingerprint && matches!(e.file_name().to_str(), Some(".git" | "__pycache__")) {
                continue;
            }
            visit(base, &e.path(), h, fingerprint, depth + 1)?;
        }
        Ok(())
    }
    let mut h = Sha256::new();
    visit(root, root, &mut h, fingerprint, 0)?;
    Ok(format!("{:x}", h.finalize()))
}
fn scalar(v: &Value) -> Value {
    if v.is_string() || v.is_boolean() || v.is_number() {
        v.clone()
    } else {
        Value::Null
    }
}
pub(crate) fn build(paths: &Paths) -> Result<Value> {
    let (state, row) = if !r::exists(&paths.receipt) {
        ("missing", json!({}))
    } else {
        match r::value(&paths.receipt) {
            Ok(v) if v.is_object() => ("valid", v),
            _ => ("invalid", json!({})),
        }
    };
    let version = r::read(&paths.source.join("VERSION"), 4096)
        .ok()
        .and_then(|raw| String::from_utf8(raw).ok())
        .and_then(|s| s.lines().next().map(str::trim).map(str::to_owned));
    let mut result =
        json!({"schema":1, "receipt":state, "oms_version":version.filter(|s| !s.is_empty())});
    for key in [
        "version",
        "commit",
        "channel",
        "ref",
        "profile",
        "link_mode",
        "dirty",
    ] {
        result[format!("receipt_{key}")] = scalar(&row[key]);
    }
    for key in ["components", "component_modes"] {
        let mut sub = serde_json::Map::new();
        if let Some(o) = row[key].as_object() {
            for (k, v) in o {
                let v = scalar(v);
                if !v.is_null() {
                    sub.insert(k.clone(), v);
                }
            }
        }
        result[key] = Value::Object(sub);
    }
    result["plugin"] = json!({"name":scalar(&row["plugin"]["name"]), "version":scalar(&row["plugin"]["version"]), "sha256":scalar(&row["plugin"]["sha256"])});
    let mut digests = serde_json::Map::new();
    for (key, file) in [
        ("rules_global_agents", "rules/global-AGENTS.md"),
        ("skills_manifest", "skills.manifest.json"),
        ("tools_lock", "tools.lock.json"),
    ] {
        let path = paths.source.join(file);
        digests.insert(
            key.into(),
            json!(if path.is_file() {
                r::hash(&r::read(&r::physical(&path)?, 1024 * 1024 * 1024)?)
            } else {
                "missing".into()
            }),
        );
    }
    digests.insert(
        "custom_skills".into(),
        json!(tree(&paths.source.join("custom-skills"), true)?),
    );
    result["digests"] = Value::Object(digests);
    let canonical = serde_json::to_string(&result)
        .map_err(|_| r::Error(2, "fingerprint serialization failed".into()))?;
    // Python's historical canonical form escapes non-ASCII before hashing.
    let mut ascii = String::new();
    for c in canonical.chars() {
        if c.is_ascii() {
            ascii.push(c);
        } else {
            let mut units = [0; 2];
            for u in c.encode_utf16(&mut units) {
                ascii.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    result["fingerprint"] = json!(r::hash(ascii.as_bytes()));
    Ok(result)
}
pub(crate) fn compare(paths: &Paths, path: &Path, as_json: bool) -> Result<crate::Output> {
    let other = r::value(path)?;
    if other["schema"] != 1 || !other["fingerprint"].is_string() {
        return r::fail("not an OMS fingerprint (schema 1)");
    }
    fn flatten(v: &Value, prefix: &str, out: &mut BTreeMap<String, Value>) {
        if let Some(o) = v.as_object() {
            for (k, v) in o {
                let name = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                if v.is_object() {
                    flatten(v, &name, out);
                } else {
                    out.insert(name, v.clone());
                }
            }
        }
    }
    let local = build(paths)?;
    let mut a = BTreeMap::new();
    let mut b = BTreeMap::new();
    flatten(&local, "", &mut a);
    flatten(&other, "", &mut b);
    let mut keys = a.keys().chain(b.keys()).cloned().collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    let rows = keys.iter().filter(|k| k.as_str() != "fingerprint" && a.get(*k) != b.get(*k)).map(|k| json!({"key":k,"local":a.get(k).cloned().unwrap_or(json!("<absent>")),"other":b.get(k).cloned().unwrap_or(json!("<absent>"))})).collect::<Vec<_>>();
    let mut output = if as_json {
        r::json(&json!({"same":rows.is_empty(),"differences":rows}))?
    } else if rows.is_empty() {
        r::text(format!(
            "same: configuration matches (fingerprint {})",
            &local["fingerprint"].as_str().unwrap()[..12]
        ))
    } else {
        r::text(format!(
            "differs: {} field(s)\n{}",
            rows.len(),
            rows.iter()
                .map(|r| format!(
                    "- {}: local={} file={}",
                    r["key"].as_str().unwrap(),
                    r["local"],
                    r["other"]
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    };
    output.0 = i32::from(!rows.is_empty());
    Ok(output)
}
