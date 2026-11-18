use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha512};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};
const MAX: usize = 1024 * 1024 * 1024;
pub(crate) const RELEASE_RECEIPT: &str = "tool-releases.json";

fn catalog() -> Value {
    json!({
        "schema": 2,
        "node": {"channel": "lts"},
        "uv": {},
        "gh": {},
        "antigravity": {},
        "npm": {
            "claude": {"package": "@anthropic-ai/claude-code", "binary": "claude"},
            "codex": {"package": "@openai/codex", "binary": "codex"},
            "ntn": {"package": "ntn", "binary": "ntn"}
        }
    })
}

fn release_policy(cwd: &Path, custom: Option<&Path>) -> Result<Value> {
    let table = match custom {
        Some(path) => r::value(&cwd.join(path))?,
        None => catalog(),
    };
    if table["schema"] != 2 {
        return r::fail("unsupported tool lock schema");
    }
    Ok(table)
}

fn installed(paths: &Paths, name: &str) -> Option<PathBuf> {
    r::find(name, &paths.cwd).or_else(|| {
        let path = paths.home.join(".local/bin").join(name);
        fs::metadata(&path)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .then_some(path)
    })
}

fn tool_row<'a>(table: &'a Value, name: &str) -> &'a Value {
    match name {
        "claude" | "codex" | "ntn" => &table["npm"][name],
        "agy" => &table["antigravity"],
        _ => &table[name],
    }
}

fn set_tool(table: &mut Value, name: &str, row: Value) {
    match name {
        "claude" | "codex" | "ntn" => table["npm"][name] = row,
        "agy" => table["antigravity"] = row,
        _ => table[name] = row,
    }
}
fn scalar<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    v[name]
        .as_str()
        .filter(|s| !s.is_empty() && !s.contains(['\0', '\n', '\r']))
        .ok_or_else(|| r::Error(2, format!("invalid tool release field: {name}")))
}
fn version(value: &str) -> Result<()> {
    if !regex::Regex::new(r"^v?[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?$")
        .unwrap()
        .is_match(value)
    {
        return r::fail("tool version must be exact");
    }
    Ok(())
}
fn https(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| r::Error(2, "tool download must use HTTPS".into()))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty()
        || host.contains(['@', ':', '?', '#'])
        || url.contains(['\r', '\n', '\0', '?', '#', '*', ' ', '\\'])
        || url.contains("${")
        || url.contains("latest")
    {
        return r::fail("tool download must be an exact HTTPS URL");
    }
    Ok(())
}
pub(crate) fn platform() -> Result<String> {
    let kernel = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        _ => return r::fail("unsupported tool host"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return r::fail("unsupported tool architecture"),
    };
    Ok(format!("{kernel}-{arch}"))
}
fn call(
    paths: &Paths,
    name: &str,
    args: &[OsString],
    flag: &AtomicBool,
    large: bool,
) -> Result<Vec<u8>> {
    let binary = r::find(name, &paths.cwd)
        .or_else(|| {
            let p = paths.home.join(".local/bin").join(name);
            p.is_file().then_some(p)
        })
        .ok_or_else(|| r::Error(2, format!("{name} required for verified tool installation")))?;
    let out = r::command_env_limit(
        binary,
        args,
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(180),
        &[],
        if large { MAX } else { r::LIMIT },
    )?;
    if out.0 != 0 {
        return Err(r::Error(
            out.0,
            format!("{name} failed during verified tool installation; output withheld"),
        ));
    }
    Ok(out.1)
}
fn checksum(path: &Path, algorithm: &str, expected: &str) -> Result<()> {
    let raw = r::read(path, MAX)?;
    let actual = match algorithm {
        "sha256" => r::hash(&raw),
        "sha512" => format!("{:x}", Sha512::digest(&raw)),
        _ => return r::fail("unsupported checksum algorithm"),
    };
    if expected.len() != actual.len()
        || !expected
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || actual != expected
    {
        return r::fail("tool payload checksum mismatch");
    }
    Ok(())
}
fn download(paths: &Paths, row: &Value, target: &Path, flag: &AtomicBool) -> Result<()> {
    let url = scalar(row, "url")?;
    https(url)?;
    call(
        paths,
        "curl",
        &[
            "--proto".into(),
            "=https".into(),
            "--proto-redir".into(),
            "=https".into(),
            "--tlsv1.2".into(),
            "--fail".into(),
            "--silent".into(),
            "--show-error".into(),
            "--location".into(),
            "--max-time".into(),
            "120".into(),
            "--max-filesize".into(),
            MAX.to_string().into(),
            "--output".into(),
            target.as_os_str().into(),
            "--".into(),
            url.into(),
        ],
        flag,
        false,
    )?;
    let algorithm = if row.get("sha256").is_some() {
        "sha256"
    } else {
        "sha512"
    };
    checksum(target, algorithm, scalar(row, algorithm)?)
}
fn member(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains(['\0', '\r', '\n', '\\', ':']) {
        return r::fail("invalid archive member");
    }
    let name = name.trim_end_matches('/');
    let name = name.strip_prefix("./").unwrap_or(name);
    let path = PathBuf::from(name);
    if name.is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || name.split('/').any(|s| s.is_empty() || s == "..")
    {
        return r::fail("archive member escaped its destination");
    }
    Ok(path)
}
fn octal(raw: &[u8]) -> Result<usize> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| r::Error(2, "invalid tar number".into()))?
        .trim_matches(['\0', ' ']);
    if text.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(text, 8).map_err(|_| r::Error(2, "invalid tar number".into()))
}
fn field(raw: &[u8]) -> Result<String> {
    std::str::from_utf8(raw.split(|b| *b == 0).next().unwrap_or_default())
        .map(str::to_owned)
        .map_err(|_| r::Error(2, "archive names must be UTF-8".into()))
}
/// Parses the authenticated tar stream; links publish only after every regular
/// file, and no entry may traverse an earlier link or leave the private stage.
fn untar(raw: &[u8], dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    let mut pos = 0usize;
    let mut seen = std::collections::BTreeSet::new();
    let mut links = Vec::new();
    let mut long_name = None;
    let mut pax = std::collections::BTreeMap::<String, String>::new();
    while pos.checked_add(512).is_some_and(|end| end <= raw.len()) {
        let header = &raw[pos..pos + 512];
        pos += 512;
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let observed: usize = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    *b as usize
                }
            })
            .sum();
        if octal(&header[148..156])? != observed {
            return r::fail("tar header checksum mismatch");
        }
        let size = octal(&header[124..136])?;
        let end = pos
            .checked_add(size)
            .filter(|end| *end <= raw.len())
            .ok_or_else(|| r::Error(2, "truncated tar member".into()))?;
        let content = &raw[pos..end];
        pos = end
            .checked_add((512 - size % 512) % 512)
            .ok_or_else(|| r::Error(2, "tar member overflow".into()))?;
        match header[156] {
            b'x' | b'g' => {
                let mut offset = 0;
                while offset < content.len() {
                    let text = std::str::from_utf8(&content[offset..])
                        .map_err(|_| r::Error(2, "invalid PAX record".into()))?;
                    let (length, _) = text
                        .split_once(' ')
                        .ok_or_else(|| r::Error(2, "invalid PAX length".into()))?;
                    let length: usize = length
                        .parse()
                        .map_err(|_| r::Error(2, "invalid PAX length".into()))?;
                    let record = text
                        .get(length.to_string().len() + 1..length)
                        .filter(|s| length > 0 && s.ends_with('\n'))
                        .ok_or_else(|| r::Error(2, "truncated PAX record".into()))?;
                    if let Some((name, value)) = record.trim_end_matches('\n').split_once('=') {
                        pax.insert(name.into(), value.into());
                    }
                    offset += length;
                }
                continue;
            }
            b'L' => {
                long_name = Some(field(content)?);
                continue;
            }
            b'K' => {
                pax.insert("linkpath".into(), field(content)?);
                continue;
            }
            _ => (),
        }
        let prefix = field(&header[345..500])?;
        let basic = field(&header[..100])?;
        let name = pax.remove("path").or(long_name.take()).unwrap_or_else(|| {
            if prefix.is_empty() {
                basic.clone()
            } else {
                format!("{prefix}/{basic}")
            }
        });
        let relative = member(&name)?;
        if !seen.insert(relative.clone()) {
            return r::fail("duplicate archive destination");
        }
        let target = dest.join(&relative);
        let parent = target.parent().unwrap();
        fs::create_dir_all(parent)?;
        if !fs::canonicalize(parent)?.starts_with(dest) {
            return r::fail("archive parent escaped stage");
        }
        let mode = octal(&header[100..108])? as u32 & 0o777;
        match header[156] {
            0 | b'0' => r::new_file(
                &target,
                content,
                if mode & 0o111 != 0 { 0o755 } else { 0o644 },
            )?,
            b'5' => {
                fs::create_dir_all(&target)?;
            }
            b'1' | b'2' => {
                let link = pax.remove("linkpath").unwrap_or(field(&header[157..257])?);
                let path = PathBuf::from(&link);
                if path.is_absolute() {
                    return r::fail("absolute archive link");
                }
                let mut resolved = if header[156] == b'1' {
                    PathBuf::new()
                } else {
                    relative.parent().unwrap_or(Path::new("")).to_owned()
                };
                for part in path.components() {
                    match part {
                        Component::Normal(s) => resolved.push(s),
                        Component::CurDir => (),
                        Component::ParentDir if resolved.pop() => (),
                        _ => return r::fail("archive link escaped stage"),
                    }
                }
                links.push((target, path, dest.join(resolved), header[156] == b'1'));
            }
            _ => return r::fail("unsupported archive entry type"),
        }
    }
    for (target, link, resolved, hard) in links {
        if !fs::canonicalize(target.parent().unwrap())?.starts_with(dest) {
            return r::fail("archive link parent escaped stage");
        }
        if hard {
            if !fs::canonicalize(&resolved)?.starts_with(dest)
                || !fs::symlink_metadata(&resolved)?.is_file()
            {
                return r::fail("invalid archive hardlink");
            }
            fs::copy(resolved, target)?;
        } else {
            symlink(link, target)?;
        }
    }
    Ok(())
}
fn extract(
    paths: &Paths,
    archive: &Path,
    format: &str,
    dest: &Path,
    flag: &AtomicBool,
) -> Result<()> {
    if format != "tar.gz" {
        return r::fail("unsupported archive type for directory extraction");
    }
    let raw = call(
        paths,
        "gzip",
        &["-dc".into(), "--".into(), archive.as_os_str().into()],
        flag,
        true,
    )?;
    untar(&raw, dest)
}
fn find_file(root: &Path, name: &str) -> Result<PathBuf> {
    fn visit(root: &Path, name: &str, found: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
        if depth > 128 {
            return r::fail("tool payload nesting exceeds limit");
        }
        for row in fs::read_dir(root)? {
            let path = row?.path();
            let m = fs::symlink_metadata(&path)?;
            if m.is_dir() {
                visit(&path, name, found, depth + 1)?;
            } else if m.is_file() && path.file_name().is_some_and(|s| s == name) {
                found.push(path);
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    visit(root, name, &mut found, 0)?;
    if found.len() != 1 {
        return r::fail(format!("verified payload must contain exactly one {name}"));
    }
    Ok(found.remove(0))
}
fn owns(paths: &Paths, target: &Path) -> Result<()> {
    if !r::exists(target) {
        return Ok(());
    }
    let marker = target.with_file_name(format!(
        "{}.oh-my-setting-managed",
        target.file_name().unwrap().to_string_lossy()
    ));
    let raw = r::read(&marker, 65536)
        .map_err(|_| r::Error(2, "refusing to replace an unmanaged tool".into()))?;
    let actual = r::hash(&r::read(target, MAX)?);
    let text = String::from_utf8_lossy(&raw);
    let legacy = text
        .strip_prefix("sha256=")
        .is_some_and(|s| s.len() == 65 && s.ends_with('\n') && s.trim_end() == actual);
    let native =
        r::parse(&raw).is_ok_and(|row| row["owner"] == "oh-my-setting" && row["sha256"] == actual);
    if !legacy && !native {
        return r::fail("managed tool changed; replacement refused");
    }
    let _ = paths;
    Ok(())
}
fn publish(
    paths: &Paths,
    tx: &mut Transaction,
    source: &Path,
    target: &Path,
    release: &str,
    flag: &AtomicBool,
) -> Result<()> {
    owns(paths, target)?;
    let out = r::command(
        source,
        &["--version".into()],
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(15),
    )?;
    if out.0 != 0
        || !String::from_utf8_lossy(&out.1)
            .split_whitespace()
            .any(|v| v.trim_start_matches('v') == release.trim_start_matches('v'))
    {
        return r::fail("verified payload version mismatch");
    }
    let raw = r::read(source, MAX)?;
    tx.write(target, &raw, 0o755)?;
    let marker = target.with_file_name(format!(
        "{}.oh-my-setting-managed",
        target.file_name().unwrap().to_string_lossy()
    ));
    tx.write(
        &marker,
        format!("sha256={}\n", r::hash(&raw)).as_bytes(),
        0o600,
    )
}
fn sri(raw: &[u8], expected: &str) -> Result<()> {
    let encoded = expected
        .strip_prefix("sha512-")
        .ok_or_else(|| r::Error(2, "npm payload requires sha512 SRI".into()))?;
    if encoded.len() != 88 || !encoded.ends_with("==") {
        return r::fail("malformed SRI encoding");
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut decoded = Vec::new();
    let mut bits = 0u32;
    let mut n = 0;
    for byte in encoded.bytes().take_while(|b| *b != b'=') {
        let digit = alphabet
            .iter()
            .position(|b| *b == byte)
            .ok_or_else(|| r::Error(2, "malformed SRI encoding".into()))?
            as u32;
        bits = (bits << 6) | digit;
        n += 6;
        if n >= 8 {
            n -= 8;
            decoded.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    if bits != 0 || decoded != Sha512::digest(raw).to_vec() {
        return r::fail("npm payload checksum mismatch");
    }
    Ok(())
}
fn pack(paths: &Paths, row: &Value, dest: &Path, flag: &AtomicBool) -> Result<PathBuf> {
    let package = scalar(row, "package")?;
    let release = scalar(row, "version")?;
    version(release)?;
    if !regex::Regex::new(r"^(?:@[a-z0-9._-]+/)?[a-z0-9._-]+$")
        .unwrap()
        .is_match(package)
    {
        return r::fail("unsafe npm package name");
    }
    let raw = call(
        paths,
        "npm",
        &[
            "pack".into(),
            format!("{package}@{release}").into(),
            "--ignore-scripts".into(),
            "--json".into(),
            "--pack-destination".into(),
            dest.as_os_str().into(),
        ],
        flag,
        false,
    )?;
    let meta = r::parse(&raw)?;
    let rows = meta
        .as_array()
        .filter(|v| v.len() == 1)
        .ok_or_else(|| r::Error(2, "invalid npm pack response".into()))?;
    let file = member(scalar(&rows[0], "filename")?)?;
    if file.components().count() != 1 || rows[0]["integrity"] != row["integrity"] {
        return r::fail("npm pack identity mismatch");
    }
    let archive = dest.join(file);
    sri(&r::read(&archive, MAX)?, scalar(row, "integrity")?)?;
    Ok(archive)
}
pub(crate) fn install(
    paths: &Paths,
    selected: &[String],
    upgrade: bool,
    persist_path: bool,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<()> {
    if selected.is_empty() {
        return Ok(());
    }
    let explicit_lock = std::env::var_os("OH_MY_SETTING_TOOL_LOCK").map(PathBuf::from);
    let mut lock = release_policy(&paths.cwd, explicit_lock.as_deref())?;
    for name in selected {
        if !["node", "uv", "gh", "agy", "claude", "codex", "ntn"].contains(&name.as_str()) {
            return r::fail("unknown managed tool");
        }
    }
    let selected = selected
        .iter()
        .filter(|name| upgrade || explicit_lock.is_some() || installed(paths, name).is_none())
        .cloned()
        .collect::<Vec<_>>();
    let host = platform()?;
    if explicit_lock.is_none() {
        latest(paths, &mut lock, &selected, &host, tx, flag)?;
    }
    let scratch = r::Scratch::new()?;
    let bin = paths.home.join(".local/bin");
    for name in &selected {
        r::check(flag)?;
        if let Some(binary) = installed(paths, name) {
            if let Some(expected) = tool_row(&lock, name)["version"].as_str() {
                let out = r::command(
                    &binary,
                    &["--version".into()],
                    &paths.cwd,
                    None,
                    flag,
                    Duration::from_secs(10),
                )?;
                if explicit_lock.is_none() && (out.0 != 0 || version_number(&out.1).is_none()) {
                    return r::fail(format!("{name} version unavailable; refresh refused"));
                }
                if out.0 == 0 && satisfies_release(&out.1, expected, explicit_lock.is_none()) {
                    continue;
                }
            }
            if ["claude", "codex", "agy"].contains(&name.as_str())
                && explicit_lock.is_none()
                && vendor(paths, name, &binary)
            {
                let before = r::command(
                    &binary,
                    &["--version".into()],
                    &paths.cwd,
                    None,
                    flag,
                    Duration::from_secs(10),
                )?;
                if before.0 != 0 || version_number(&before.1).is_none() {
                    return r::fail("vendor provider version unavailable; refresh refused");
                }
                let out = r::command(
                    &binary,
                    &["update".into()],
                    &paths.cwd,
                    None,
                    flag,
                    Duration::from_secs(180),
                )?;
                if out.0 != 0 {
                    return r::fail("vendor provider refresh failed");
                }
                let after = r::command(
                    binary,
                    &["--version".into()],
                    &paths.cwd,
                    None,
                    flag,
                    Duration::from_secs(10),
                )?;
                if after.0 != 0
                    || version_number(&after.1)
                        .zip(version_number(&before.1))
                        .is_none_or(|(a, b)| a < b)
                {
                    return r::fail("vendor refresh could not prove a non-downgraded version");
                }
                continue;
            }
        }
        let work = scratch.0.join(name);
        fs::create_dir(&work)?;
        if ["claude", "codex", "ntn"].contains(&name.as_str()) {
            let row = &lock["npm"][name];
            let archive = pack(paths, row, &work, flag)?;
            let unpacked = work.join("unpacked");
            extract(paths, &archive, "tar.gz", &unpacked, flag)?;
            let package = unpacked.join("package");
            let meta = r::value(&package.join("package.json"))?;
            if meta["name"] != row["package"] || meta["version"] != row["version"] {
                return r::fail("npm archive package identity mismatch");
            }
            if let Some(native) = row["native"].get(&host) {
                let native_archive = pack(paths, native, &work, flag)?;
                let root = work.join("native");
                extract(paths, &native_archive, "tar.gz", &root, flag)?;
                let spec = r::value(&root.join("package/package.json"))?;
                if spec["name"] != native["package"] || spec["version"] != native["version"] {
                    return r::fail("npm native package identity mismatch");
                }
                if name == "claude" {
                    let executable = find_file(&root, name)?;
                    let release = scalar(row, "version")?;
                    let versioned = paths
                        .home
                        .join(".local/share/claude/versions")
                        .join(release);
                    if !r::exists(&versioned)
                        || r::hash(&r::read(&versioned, MAX)?)
                            != r::hash(&r::read(&executable, MAX)?)
                    {
                        publish(paths, tx, &executable, &versioned, release, flag)?;
                    }
                    let launcher = bin.join(name);
                    if r::exists(&launcher) && !vendor(paths, name, &launcher) {
                        owns(paths, &launcher)?;
                    }
                    tx.link(&launcher, &versioned)?;
                    continue;
                }
                let alias = member(scalar(native, "alias")?)?;
                let target = package.join("node_modules").join(alias);
                fs::create_dir_all(target.parent().unwrap())?;
                fs::rename(root.join("package"), target)?;
            }
            let package_name = member(scalar(row, "package")?)?;
            let target = paths
                .home
                .join(".local/lib/node_modules")
                .join(package_name);
            if target.exists() && !r::exists(&target.join(".oh-my-setting-native-tool")) {
                return r::fail("unmanaged npm package directory");
            }
            r::new_file(
                &package.join(".oh-my-setting-native-tool"),
                b"oh-my-setting\n",
                0o600,
            )?;
            tx.publish_external(&target, &package)?;
            if name == "ntn" && target.join("install.cjs").is_file() {
                let checkpoint = tx.prepare_external(std::slice::from_ref(&target))?;
                let node = if bin.join("node").is_file() {
                    bin.join("node")
                } else {
                    r::find("node", &paths.cwd)
                        .ok_or_else(|| r::Error(2, "Node runtime unavailable".into()))?
                };
                let result = r::command(
                    &node,
                    &[
                        target.join("install.cjs").into_os_string(),
                        "install".into(),
                    ],
                    &target,
                    None,
                    flag,
                    Duration::from_secs(180),
                );
                tx.finish_external(checkpoint)?;
                if result?.0 != 0 {
                    return r::fail("verified npm payload installer failed");
                }
            }
            let entry = if let Some(s) = meta["bin"].as_str() {
                s
            } else {
                scalar(&meta["bin"], name)?
            };
            let entry = member(entry)?;
            let executable = target.join(entry);
            if !r::physical(&executable)?.starts_with(&target) || !executable.is_file() {
                return r::fail("npm binary escaped its package");
            }
            owns(paths, &bin.join(name))?;
            let node = if bin.join("node").is_file() {
                bin.join("node")
            } else {
                r::find("node", &paths.cwd)
                    .ok_or_else(|| r::Error(2, "Node runtime unavailable".into()))?
            };
            let shim = format!(
                "#!/bin/sh\n# oh-my-setting managed npm tool\nexec {} {} \"$@\"\n",
                r::shell(node.as_os_str())?,
                r::shell(executable.as_os_str())?
            );
            tx.write(&bin.join(name), shim.as_bytes(), 0o755)?;
            tx.write(
                &bin.join(format!("{name}.oh-my-setting-managed")),
                format!("sha256={}\n", r::hash(shim.as_bytes())).as_bytes(),
                0o600,
            )?;
            let out = r::command(
                bin.join(name),
                &["--version".into()],
                &paths.cwd,
                None,
                flag,
                Duration::from_secs(15),
            )?;
            if out.0 != 0
                || !String::from_utf8_lossy(&out.1)
                    .split_whitespace()
                    .any(|s| s.trim_start_matches('v') == row["version"].as_str().unwrap_or(""))
            {
                return r::fail("installed npm command does not match its verified release");
            }
            continue;
        }
        let catalog = if name == "agy" {
            "antigravity"
        } else {
            name.as_str()
        };
        let release = scalar(&lock[catalog], "version")?;
        version(release)?;
        let row = &lock[catalog]["platforms"][&host];
        let archive = work.join("payload");
        download(paths, row, &archive, flag)?;
        let format = scalar(row, "archive")?;
        if format == "zip" && name == "gh" {
            let names = call(
                paths,
                "unzip",
                &["-Z1".into(), archive.as_os_str().into()],
                flag,
                false,
            )?;
            let text =
                String::from_utf8(names).map_err(|_| r::Error(2, "invalid ZIP listing".into()))?;
            let mut found = Vec::new();
            for name in text.lines() {
                let path = member(name)?;
                if path.file_name().is_some_and(|s| s == "gh") {
                    found.push(name);
                }
            }
            if found.len() != 1 {
                return r::fail("ZIP must contain exactly one gh binary");
            }
            let raw = call(
                paths,
                "unzip",
                &["-p".into(), archive.as_os_str().into(), found[0].into()],
                flag,
                true,
            )?;
            let source = work.join("gh");
            r::new_file(&source, &raw, 0o755)?;
            publish(paths, tx, &source, &bin.join("gh"), release, flag)?;
        } else {
            let root = work.join("extracted");
            extract(paths, &archive, format, &root, flag)?;
            if name == "node" {
                let executable = find_file(&root, "node")?;
                let distribution = executable
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| r::Error(2, "invalid Node distribution".into()))?;
                let target = paths
                    .home
                    .join(".nvm/versions/node")
                    .join(format!("v{release}"));
                let out = r::command(
                    &executable,
                    &["--version".into()],
                    &paths.cwd,
                    None,
                    flag,
                    Duration::from_secs(10),
                )?;
                if out.0 != 0
                    || String::from_utf8_lossy(&out.1)
                        .trim()
                        .trim_start_matches('v')
                        != release
                {
                    return r::fail("Node payload version mismatch");
                }
                if target.exists() {
                    let out = r::command(
                        target.join("bin/node"),
                        &["--version".into()],
                        &paths.cwd,
                        None,
                        flag,
                        Duration::from_secs(10),
                    )?;
                    if out.0 != 0
                        || String::from_utf8_lossy(&out.1)
                            .trim()
                            .trim_start_matches('v')
                            != release
                    {
                        return r::fail("unmanaged or damaged existing Node distribution");
                    }
                } else {
                    tx.publish_external(&target, distribution)?;
                }

                for command in ["node", "npm", "npx", "corepack"] {
                    let source = target.join("bin").join(command);
                    if source.exists() {
                        owns(paths, &bin.join(command))?;
                        let shim = format!(
                            "#!/bin/sh\n# oh-my-setting managed Node tool\nPATH={}:\"$PATH\"\nexport PATH\nexec {} \"$@\"\n",
                            r::shell(target.join("bin").as_os_str())?,
                            r::shell(source.as_os_str())?
                        );
                        tx.write(&bin.join(command), shim.as_bytes(), 0o755)?;
                        tx.write(
                            &bin.join(format!("{command}.oh-my-setting-managed")),
                            format!("sha256={}\n", r::hash(shim.as_bytes())).as_bytes(),
                            0o600,
                        )?;
                    }
                }
            } else {
                for command in if name == "uv" {
                    vec!["uv", "uvx"]
                } else if name == "agy" {
                    vec!["antigravity"]
                } else {
                    vec!["gh"]
                } {
                    let source = find_file(&root, command)?;
                    let target = if name == "uv" {
                        std::env::var_os("OMS_UV_BIN_DIR")
                            .map(PathBuf::from)
                            .unwrap_or(bin.clone())
                            .join(command)
                    } else {
                        bin.join(name)
                    };
                    publish(paths, tx, &source, &target, release, flag)?;
                }
            }
        }
    }
    if persist_path {
        let line = "export PATH=\"$HOME/.local/bin:$PATH\"";
        let login = if paths.home.join(".bash_profile").exists() {
            ".bash_profile"
        } else if paths.home.join(".bash_login").exists() {
            ".bash_login"
        } else {
            ".profile"
        };
        for name in [".bashrc", login, ".zshrc", ".zprofile"] {
            let path = r::physical(&paths.home.join(name))?;
            let mut raw = if r::exists(&path) {
                String::from_utf8(r::read(&path, r::LIMIT)?)
                    .map_err(|_| r::Error(2, "shell rc is not UTF-8".into()))?
            } else {
                String::new()
            };
            if !raw.lines().any(|s| s == line) {
                raw.push_str(&format!("\n{line}\n"));
                let mode = fs::metadata(&path)
                    .map(|m| m.permissions().mode() & 0o777)
                    .unwrap_or(0o600);
                tx.write(&path, raw.as_bytes(), mode)?;
            }
        }
    }
    Ok(())
}

fn satisfies_release(raw: &[u8], expected: &str, follow_upstream: bool) -> bool {
    String::from_utf8_lossy(raw)
        .split_whitespace()
        .any(|s| s.trim_start_matches('v') == expected.trim_start_matches('v'))
        || follow_upstream
            && version_number(raw)
                .zip(release_number(expected))
                .is_some_and(|(actual, desired)| actual > desired)
}

fn version_number(raw: &[u8]) -> Option<Vec<u64>> {
    String::from_utf8_lossy(raw)
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find_map(|s| {
            let parts = s
                .split('.')
                .map(str::parse::<u64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .ok()?;
            (parts.len() == 3).then_some(parts)
        })
}

fn vendor(paths: &Paths, name: &str, binary: &Path) -> bool {
    let Ok(path) = fs::canonicalize(binary) else {
        return false;
    };
    match name {
        "claude" => {
            path.parent() == Some(paths.home.join(".local/share/claude/versions").as_path())
        }
        "codex" => paths.codex_home().ok().is_some_and(|h| {
            path.strip_prefix(h.join("packages/standalone/releases"))
                .is_ok_and(|p| p.components().count() == 3 && p.ends_with("bin/codex"))
        }),
        "agy" => !binary.is_symlink() && path == paths.home.join(".local/bin/agy"),
        "pi" => {
            path == paths.home.join(".pi/agent/bin/pi")
                && paths
                    .home
                    .join(".pi/agent/install/current-version")
                    .is_file()
                && paths.home.join(".pi/agent/install/releases").is_dir()
        }
        _ => false,
    }
}
fn metadata_bytes(paths: &Paths, url: &str, flag: &AtomicBool) -> Result<Vec<u8>> {
    let host = url
        .strip_prefix("https://")
        .and_then(|s| s.split('/').next())
        .unwrap_or("");
    if ![
        "registry.npmjs.org",
        "antigravity-cli-auto-updater-974169037036.us-central1.run.app",
        "nodejs.org",
        "api.github.com",
    ]
    .contains(&host)
        || url.contains(['?', '#', '\r', '\n'])
    {
        return r::fail("unapproved tool metadata endpoint");
    }
    let program = r::find("curl", &paths.cwd)
        .ok_or_else(|| r::Error(2, "curl required for current tool metadata".into()))?;
    let out = r::command(
        program,
        &[
            "--proto".into(),
            "=https".into(),
            "--tlsv1.2".into(),
            "--silent".into(),
            "--show-error".into(),
            "--max-time".into(),
            "20".into(),
            "--max-filesize".into(),
            "2097152".into(),
            "--write-out".into(),
            "\n%{http_code}".into(),
            url.into(),
        ],
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(25),
    )?;
    if out.0 != 0 || out.1.len() > 2097152 + 4 {
        return r::fail("current tool metadata unavailable or oversized");
    }
    let split = out
        .1
        .iter()
        .rposition(|b| *b == b'\n')
        .ok_or_else(|| r::Error(2, "invalid metadata response".into()))?;
    if &out.1[split + 1..] == b"404" {
        return Err(r::Error(4, "current tool release is incomplete".into()));
    }
    if &out.1[split + 1..] != b"200" {
        return r::fail("tool metadata redirects and non-success responses are refused");
    }
    Ok(out.1[..split].to_vec())
}
type Fetch<'a> = dyn FnMut(&str) -> Result<Vec<u8>> + 'a;

fn npm_metadata(package: &str, release: &str, fetch: &mut Fetch<'_>) -> Result<Value> {
    if !regex::Regex::new(r"^(?:@[a-z0-9._-]+/)?[a-z0-9._-]+$")
        .unwrap()
        .is_match(package)
    {
        return r::fail("unsafe npm package name");
    }
    let encoded = package.replace('/', "%2F");
    let row = r::parse(&fetch(&format!(
        "https://registry.npmjs.org/{encoded}/{release}"
    ))?)?;
    let actual = scalar(&row, "version")?.to_owned();
    version(&actual)?;
    if row["name"] != package || release != "latest" && actual != release {
        return r::fail("npm release identity mismatch");
    }
    let url = scalar(&row["dist"], "tarball")?;
    https(url)?;
    let expected = format!(
        "https://registry.npmjs.org/{package}/-/{}-{actual}.tgz",
        package.rsplit('/').next().unwrap()
    );
    if url.replace("%2f", "/").replace("%2F", "/") != expected {
        return r::fail("npm release URL is not bound to its package and version");
    }
    let integrity = scalar(&row["dist"], "integrity")?.to_owned();
    if !regex::Regex::new(r"^sha512-[A-Za-z0-9+/]{86}==$")
        .unwrap()
        .is_match(&integrity)
    {
        return r::fail("invalid npm release SRI");
    }
    Ok(row)
}
fn release_number(value: &str) -> Option<Vec<u64>> {
    value
        .strip_prefix('v')
        .unwrap_or(value)
        .split('.')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()
        .filter(|v| v.len() == 3)
}
fn stable_release(value: &str) -> Result<()> {
    version(value)?;
    if release_number(value).is_none() {
        return r::fail("latest tool release must be a stable semantic version");
    }
    Ok(())
}

fn optional_upgrade(previous: &str, candidate: &str) -> Result<bool> {
    version(previous)?;
    stable_release(candidate)?;
    let base = previous.split(['-', '+']).next().unwrap_or(previous);
    let installed = release_number(base).ok_or_else(|| {
        r::Error(
            2,
            "installed optional provider version is uncomparable".into(),
        )
    })?;
    let desired = release_number(candidate).unwrap();
    let prerelease = previous.split('+').next().unwrap_or(previous).contains('-');
    Ok(desired > installed || desired == installed && prerelease)
}
fn digest(value: &str, length: usize) -> Result<String> {
    if value.len() != length || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return r::fail("invalid current tool payload digest");
    }
    Ok(value.to_ascii_lowercase())
}

fn checksum_entry(raw: &[u8], name: &str) -> Result<String> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| r::Error(2, "invalid release checksum manifest".into()))?;
    let mut found = None;
    for line in text.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.get(1) != Some(&name) {
            continue;
        }
        if fields.len() != 2 || found.is_some() {
            return r::fail("ambiguous release checksum entry");
        }
        found = Some(digest(fields[0], 64)?);
    }
    found.ok_or_else(|| r::Error(4, "current host payload has no published checksum".into()))
}

fn archive_row(
    release: &str,
    host: &str,
    url: &str,
    algorithm: &str,
    checksum: &str,
    format: &str,
) -> Value {
    let mut result = json!({"version": release, "platforms": {}});
    result["platforms"][host] = json!({"url": url, "archive": format});
    result["platforms"][host][algorithm] = json!(checksum);
    result
}

fn resolve_tool(name: &str, host: &str, fetch: &mut Fetch<'_>) -> Result<Value> {
    let (node_host, node_file, uv_host) = match host {
        "darwin-amd64" => ("darwin-x64", "osx-x64-tar", "x86_64-apple-darwin"),
        "darwin-arm64" => ("darwin-arm64", "osx-arm64-tar", "aarch64-apple-darwin"),
        "linux-amd64" => ("linux-x64", "linux-x64", "x86_64-unknown-linux-gnu"),
        "linux-arm64" => ("linux-arm64", "linux-arm64", "aarch64-unknown-linux-gnu"),
        _ => return r::fail("unsupported release host"),
    };
    if name == "node" {
        let index = r::parse(&fetch("https://nodejs.org/dist/index.json")?)?;
        let rows = index
            .as_array()
            .ok_or_else(|| r::Error(2, "invalid Node release index".into()))?;
        let current = rows
            .iter()
            .filter(|row| row["lts"].as_str().is_some_and(|s| !s.is_empty()))
            .filter_map(|row| Some((release_number(row["version"].as_str()?)?, row)))
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, row)| row)
            .ok_or_else(|| r::Error(4, "no stable Node LTS release is published".into()))?;
        let tag = scalar(current, "version")?;
        stable_release(tag)?;
        if !current["files"]
            .as_array()
            .is_some_and(|files| files.iter().any(|value| value == node_file))
        {
            return r::fail("current Node LTS release is unavailable for this host");
        }
        let release = tag.strip_prefix('v').unwrap_or(tag);
        let filename = format!("node-v{release}-{node_host}.tar.gz");
        let base = format!("https://nodejs.org/dist/v{release}");
        let checksum = checksum_entry(&fetch(&format!("{base}/SHASUMS256.txt"))?, &filename)?;
        let mut row = archive_row(
            release,
            host,
            &format!("{base}/{filename}"),
            "sha256",
            &checksum,
            "tar.gz",
        );
        row["channel"] = json!("lts");
        return Ok(row);
    }
    if matches!(name, "uv" | "gh") {
        let repo = if name == "uv" {
            "astral-sh/uv"
        } else {
            "cli/cli"
        };
        let response = r::parse(&fetch(&format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))?)?;
        if response["draft"] != false || response["prerelease"] != false {
            return r::fail("tool release must be published and stable");
        }
        let tag = scalar(&response, "tag_name")?;
        stable_release(tag)?;
        let release = tag.strip_prefix('v').unwrap_or(tag);
        let (filename, format) = if name == "uv" {
            (format!("uv-{uv_host}.tar.gz"), "tar.gz")
        } else {
            let platform = host
                .replace("darwin-", "macOS_")
                .replace("linux-", "linux_");
            let format = if host.starts_with("darwin-") {
                "zip"
            } else {
                "tar.gz"
            };
            (format!("gh_{release}_{platform}.{format}"), format)
        };
        let assets = response["assets"]
            .as_array()
            .ok_or_else(|| r::Error(2, "invalid release asset list".into()))?
            .iter()
            .filter(|asset| asset["name"] == filename)
            .collect::<Vec<_>>();
        if assets.len() != 1 {
            return r::fail("current release must contain exactly one host payload");
        }
        let asset = assets[0];
        if asset["state"] != "uploaded"
            || asset["size"]
                .as_u64()
                .is_none_or(|size| size == 0 || size > MAX as u64)
        {
            return r::fail("current host payload is incomplete or oversized");
        }
        let url = scalar(asset, "browser_download_url")?;
        https(url)?;
        if url != format!("https://github.com/{repo}/releases/download/{tag}/{filename}") {
            return r::fail("release payload URL is not bound to its repository and version");
        }
        let checksum = digest(
            scalar(asset, "digest")?
                .strip_prefix("sha256:")
                .ok_or_else(|| r::Error(2, "release payload requires a SHA256 digest".into()))?,
            64,
        )?;
        return Ok(archive_row(release, host, url, "sha256", &checksum, format));
    }
    if name == "agy" {
        let response = r::parse(&fetch(&format!(
            "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/{}.json",
            host.replace('-', "_")
        ))?)?;
        let release = scalar(&response, "version")?;
        stable_release(release)?;
        let url = scalar(&response, "url")?;
        https(url)?;
        if !url.starts_with(&format!(
            "https://storage.googleapis.com/antigravity-public/antigravity-cli/{release}-"
        )) || url.split('/').any(|part| matches!(part, "." | ".."))
        {
            return r::fail("provider release URL escaped its official versioned bucket");
        }
        let checksum = digest(scalar(&response, "sha512")?, 128)?;
        return Ok(archive_row(
            release, host, url, "sha512", &checksum, "tar.gz",
        ));
    }
    if matches!(name, "claude" | "codex" | "ntn") {
        let mut row = tool_row(&catalog(), name).clone();
        let main = npm_metadata(scalar(&row, "package")?, "latest", fetch)?;
        let release = scalar(&main, "version")?;
        stable_release(release)?;
        row["version"] = json!(release);
        row["integrity"] = main["dist"]["integrity"].clone();
        if name != "ntn" {
            let suffix = host
                .replace("darwin-amd64", "darwin-x64")
                .replace("linux-amd64", "linux-x64");
            let alias = if name == "claude" {
                format!("@anthropic-ai/claude-code-{suffix}")
            } else {
                format!("@openai/codex-{suffix}")
            };
            let package = if name == "codex" {
                "@openai/codex"
            } else {
                &alias
            };
            let expected = if name == "codex" {
                format!("{release}-{suffix}")
            } else {
                release.to_owned()
            };
            let native = npm_metadata(package, &expected, fetch)?;
            row["native"] = json!({});
            row["native"][host] = json!({
                "alias": alias, "package": package,
                "version": native["version"], "integrity": native["dist"]["integrity"]
            });
        }
        return Ok(row);
    }
    r::fail("unknown managed tool")
}

fn latest(
    paths: &Paths,
    lock: &mut Value,
    selected: &[String],
    host: &str,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<()> {
    let file = paths.data.join(RELEASE_RECEIPT);
    let mut observed = json!({"schema": 2, "npm": {}});
    let previous = if r::exists(&file) {
        Some((file.clone(), false))
    } else {
        let legacy = paths.data.join("provider-tools.lock.json");
        r::exists(&legacy).then_some((legacy, true))
    };
    if let Some((previous, legacy)) = previous {
        match r::value(&previous) {
            Ok(old) if old["schema"] == 2 => {
                for name in ["node", "uv", "gh", "ntn", "claude", "codex", "agy"] {
                    if legacy && !matches!(name, "claude" | "codex" | "agy") {
                        continue;
                    }
                    let row = tool_row(&old, name);
                    if row["version"].is_string() {
                        set_tool(&mut observed, name, row.clone());
                    }
                }
            }
            _ => tx.warn("previous release observations unreadable; recording fresh resolutions"),
        }
    }
    let mut changed = false;
    for name in selected {
        if installed(paths, name).is_some_and(|p| vendor(paths, name, &p)) {
            continue;
        }
        // Receipts describe observations only. Neither an old observation nor a
        // checked-in version may choose or restrict the next upstream resolution.
        let mut row = resolve_tool(name, host, &mut |url| metadata_bytes(paths, url, flag))
            .map_err(|error| {
                r::Error(
                    error.0,
                    format!("cannot resolve current {name} release: {}", error.1),
                )
            })?;
        row["resolved_at"] = json!(r::now());
        set_tool(lock, name, row.clone());
        set_tool(&mut observed, name, row);
        changed = true;
    }
    if changed {
        tx.write(&file, &r::bytes(&observed)?, 0o600)?;
    }
    Ok(())
}
pub(crate) fn refresh_optional(
    paths: &Paths,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<()> {
    if let Some(program) = r::find("pi", &paths.cwd).filter(|p| vendor(paths, "pi", p))
        && std::env::var_os("OH_MY_SETTING_TOOL_LOCK").is_none()
    {
        let before = r::command(
            &program,
            &["--version".into()],
            &paths.cwd,
            None,
            flag,
            Duration::from_secs(10),
        )?;
        if before.0 != 0 || version_number(&before.1).is_none() {
            return r::fail("Pi version unavailable; refresh refused");
        }
        let out = r::command(
            &program,
            &["update".into(), "self".into()],
            &paths.cwd,
            None,
            flag,
            Duration::from_secs(180),
        )?;
        if out.0 != 0 {
            return r::fail("Pi vendor refresh failed; its installer retains recovery state");
        }
        let after = r::command(
            &program,
            &["--version".into()],
            &paths.cwd,
            None,
            flag,
            Duration::from_secs(10),
        )?;
        if after.0 != 0
            || version_number(&after.1)
                .zip(version_number(&before.1))
                .is_none_or(|(a, b)| a < b)
        {
            return r::fail("Pi refresh could not prove a non-downgraded version");
        }
    }
    if std::env::var_os("OH_MY_SETTING_TOOL_LOCK").is_some()
        || rustix::process::geteuid().as_raw() == 0
        || r::find("npm", &paths.cwd).is_none()
    {
        return Ok(());
    }
    let root = PathBuf::from(
        String::from_utf8(call(
            paths,
            "npm",
            &["root".into(), "-g".into()],
            flag,
            false,
        )?)
        .map_err(|_| r::Error(2, "invalid npm root".into()))?
        .trim(),
    );
    let prefix = PathBuf::from(
        String::from_utf8(call(
            paths,
            "npm",
            &["prefix".into(), "-g".into()],
            flag,
            false,
        )?)
        .map_err(|_| r::Error(2, "invalid npm prefix".into()))?
        .trim(),
    );
    let root = fs::canonicalize(root)?;
    let bin = prefix.join("bin");
    let scratch = r::Scratch::new()?;
    for binary in [
        "cursor-agent",
        "grok",
        "gemini",
        "qwen",
        "opencode",
        "opencode2",
        "dsh",
        "vibe",
        "pi",
        "copilot",
        "droid",
        "aider",
        "muse",
        "kimi",
        "kiro-cli",
        "cn",
        "goose",
        "cline",
        "kilo",
        "openhands",
        "auggie",
    ] {
        let Some(program) = r::find(binary, &paths.cwd) else {
            continue;
        };
        if binary == "pi" && vendor(paths, "pi", &program) {
            continue;
        }
        let resolved = fs::canonicalize(&program)?;
        let Ok(relative) = resolved.strip_prefix(&root) else {
            continue;
        };
        let components = relative.components().collect::<Vec<_>>();
        let count = if components
            .first()
            .is_some_and(|s| s.as_os_str().to_string_lossy().starts_with('@'))
        {
            2
        } else {
            1
        };
        if components.len() <= count {
            continue;
        }
        let package = components[..count].iter().collect::<PathBuf>();
        let directory = root.join(&package);
        if fs::canonicalize(&directory)? != directory {
            continue;
        }
        let before = r::value(&directory.join("package.json"))?;
        let name = scalar(&before, "name")?;
        let previous = scalar(&before, "version")?;
        if package.to_str() != Some(name) {
            continue;
        }
        let bins = if let Some(path) = before["bin"].as_str() {
            json!({name.rsplit('/').next().unwrap():path})
        } else {
            before["bin"].clone()
        };
        let Some(declared) = bins[binary].as_str() else {
            continue;
        };
        let declared = member(declared)?;
        if fs::canonicalize(directory.join(declared))? != resolved {
            continue;
        }
        let candidate = npm_metadata(name, "latest", &mut |url| metadata_bytes(paths, url, flag))?;
        let release = scalar(&candidate, "version")?.to_owned();
        if !optional_upgrade(previous, &release)? {
            continue;
        }
        let new_bins = if let Some(path) = candidate["bin"].as_str() {
            json!({name.rsplit('/').next().unwrap():path})
        } else {
            candidate["bin"].clone()
        };
        let mut targets = std::collections::BTreeSet::from([directory.clone()]);
        for table in [&bins, &new_bins] {
            for (command, _) in table
                .as_object()
                .ok_or_else(|| r::Error(2, "invalid optional binary declarations".into()))?
            {
                if !r::safe_name(command) || matches!(command.as_str(), "." | "..") {
                    return r::fail("unsafe optional binary name");
                }
                let path = bin.join(command);
                if r::exists(&path) && !fs::canonicalize(&path)?.starts_with(&directory) {
                    return r::fail("optional provider bin is owned by another package");
                }
                targets.insert(path);
            }
        }
        let work = scratch.0.join(binary);
        fs::create_dir(&work)?;
        let archive = pack(
            paths,
            &json!({"package":name,"version":release,"integrity":candidate["dist"]["integrity"]}),
            &work,
            flag,
        )?;
        let checkpoint = tx.prepare_external(&targets.into_iter().collect::<Vec<_>>())?;
        let result = call(
            paths,
            "npm",
            &[
                "install".into(),
                "-g".into(),
                "--no-fund".into(),
                "--no-audit".into(),
                "--".into(),
                archive.into_os_string(),
            ],
            flag,
            false,
        );
        tx.finish_external(checkpoint)?;
        result?;
        let actual = r::value(&directory.join("package.json"))?;
        if actual["name"] != name || actual["version"] != release {
            return r::fail("optional provider installed an unexpected identity");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn response(url: &str, value: Value) -> (String, Vec<u8>) {
        (url.into(), serde_json::to_vec(&value).unwrap())
    }

    fn resolve_fixture(name: &str, host: &str, responses: Vec<(String, Vec<u8>)>) -> Result<Value> {
        let mut responses = VecDeque::from(responses);
        let result = resolve_tool(name, host, &mut |url| {
            let (expected, raw) = responses.pop_front().expect("unexpected metadata request");
            assert_eq!(url, expected);
            Ok(raw)
        });
        if result.is_ok() {
            assert!(responses.is_empty(), "release resolution stopped early");
        }
        result
    }

    fn npm_fixture(package: &str, release: &str) -> Value {
        json!({
            "name": package,
            "version": release,
            "dist": {
                "tarball": format!(
                    "https://registry.npmjs.org/{package}/-/{}-{release}.tgz",
                    package.rsplit('/').next().unwrap()
                ),
                "integrity": format!("sha512-{}==", "A".repeat(86))
            }
        })
    }

    fn github_fixture(repo: &str, tag: &str, filename: &str) -> Value {
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "assets": [{
                "name": filename,
                "state": "uploaded",
                "size": 1024,
                "digest": format!("sha256:{}", "a".repeat(64)),
                "browser_download_url": format!(
                    "https://github.com/{repo}/releases/download/{tag}/{filename}"
                )
            }]
        })
    }

    #[test]
    fn default_catalog_has_no_pins_and_only_explicit_relative_override_is_loaded() {
        let scratch = r::Scratch::new().unwrap();
        let pinned = json!({"schema": 2, "node": {"version": "18.20.8"}});
        r::new_file(
            &scratch.0.join("tools.lock.json"),
            b"not a default policy",
            0o600,
        )
        .unwrap();
        r::new_file(
            &scratch.0.join("custom.json"),
            &r::bytes(&pinned).unwrap(),
            0o600,
        )
        .unwrap();
        let current = release_policy(&scratch.0, None).unwrap();
        for name in ["node", "uv", "gh", "ntn", "claude", "codex", "agy"] {
            assert!(tool_row(&current, name).get("version").is_none());
            assert!(tool_row(&current, name).get("platforms").is_none());
        }
        assert_eq!(current["node"]["channel"], "lts");
        assert_eq!(
            release_policy(&scratch.0, Some(Path::new("custom.json"))).unwrap(),
            pinned
        );
        assert!(release_policy(&scratch.0, Some(Path::new("absent.json"))).is_err());
        assert!(release_policy(&scratch.0, Some(Path::new("tools.lock.json"))).is_err());
    }

    #[test]
    fn non_executable_local_placeholder_is_not_an_installed_tool() {
        let scratch = r::Scratch::new().unwrap();
        let paths = Paths {
            home: scratch.0.join("home"),
            source: scratch.0.clone(),
            receipt: scratch.0.join("receipt.json"),
            config: scratch.0.join("config"),
            state: scratch.0.join("state"),
            data: scratch.0.join("data"),
            cwd: scratch.0.clone(),
        };
        let name = "oms-supply-fixture-command";
        let executable = paths.home.join(".local/bin").join(name);
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        r::new_file(&executable, b"fixture only\n", 0o600).unwrap();
        assert!(installed(&paths, name).is_none());
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(installed(&paths, name), Some(executable));
    }

    #[test]
    fn node_selects_highest_lts_by_version_and_ignores_order_and_major_parity() {
        let row = resolve_fixture(
            "node",
            "linux-amd64",
            vec![
                response(
                    "https://nodejs.org/dist/index.json",
                    json!([
                        {"version": "v26.0.0", "lts": false, "files": ["linux-x64"]},
                        {"version": "v24.10.0", "lts": "Older", "files": ["linux-x64"]},
                        {"version": "v27.0.0-alpha.1", "lts": "Preview", "files": ["linux-x64"]},
                        {"version": "v25.3.1", "lts": "Current", "files": ["linux-x64"]},
                        {"version": "v25.2.9", "lts": "Current", "files": ["linux-x64"]}
                    ]),
                ),
                (
                    "https://nodejs.org/dist/v25.3.1/SHASUMS256.txt".into(),
                    format!("{}  node-v25.3.1-linux-x64.tar.gz\n", "b".repeat(64)).into_bytes(),
                ),
            ],
        )
        .unwrap();
        assert_eq!(row["version"], "25.3.1");
        assert_eq!(row["channel"], "lts");
        assert_eq!(row["platforms"].as_object().unwrap().len(), 1);
        assert_eq!(row["platforms"]["linux-amd64"]["sha256"], "b".repeat(64));
    }

    #[test]
    fn node_resolves_each_host_without_requiring_other_platforms() {
        for (host, index_file, filename) in [
            (
                "darwin-amd64",
                "osx-x64-tar",
                "node-v26.1.0-darwin-x64.tar.gz",
            ),
            (
                "darwin-arm64",
                "osx-arm64-tar",
                "node-v26.1.0-darwin-arm64.tar.gz",
            ),
            ("linux-amd64", "linux-x64", "node-v26.1.0-linux-x64.tar.gz"),
            (
                "linux-arm64",
                "linux-arm64",
                "node-v26.1.0-linux-arm64.tar.gz",
            ),
        ] {
            let row = resolve_fixture(
                "node",
                host,
                vec![
                    response(
                        "https://nodejs.org/dist/index.json",
                        json!([{"version": "v26.1.0", "lts": "Fixture", "files": [index_file]}]),
                    ),
                    (
                        "https://nodejs.org/dist/v26.1.0/SHASUMS256.txt".into(),
                        format!("{}  {filename}\n", "a".repeat(64)).into_bytes(),
                    ),
                ],
            )
            .unwrap();
            assert_eq!(
                row["platforms"][host]["url"],
                format!("https://nodejs.org/dist/v26.1.0/{filename}")
            );
            assert_eq!(row["platforms"].as_object().unwrap().len(), 1);
        }
    }

    #[test]
    fn node_rejects_incomplete_current_release_and_missing_or_ambiguous_checksum() {
        let result = resolve_fixture(
            "node",
            "linux-amd64",
            vec![response(
                "https://nodejs.org/dist/index.json",
                json!([
                    {"version": "v26.2.0", "lts": "Fixture", "files": ["linux-arm64"]},
                    {"version": "v26.1.0", "lts": "Fixture", "files": ["linux-x64"]}
                ]),
            )],
        );
        assert!(result.is_err());
        let line = format!("{}  host.tar.gz\n", "a".repeat(64));
        assert!(checksum_entry(line.as_bytes(), "other.tar.gz").is_err());
        assert!(checksum_entry(format!("{line}{line}").as_bytes(), "host.tar.gz").is_err());
        assert!(checksum_entry(b"invalid  host.tar.gz\n", "host.tar.gz").is_err());
        assert!(
            checksum_entry(
                format!("{}  host.tar.gz extra\n", "a".repeat(64)).as_bytes(),
                "host.tar.gz"
            )
            .is_err()
        );
    }

    #[test]
    fn github_releases_resolve_exact_verified_host_assets() {
        for (name, host, repo, tag, filename, format) in [
            (
                "uv",
                "darwin-amd64",
                "astral-sh/uv",
                "0.13.9",
                "uv-x86_64-apple-darwin.tar.gz",
                "tar.gz",
            ),
            (
                "uv",
                "darwin-arm64",
                "astral-sh/uv",
                "0.13.9",
                "uv-aarch64-apple-darwin.tar.gz",
                "tar.gz",
            ),
            (
                "uv",
                "linux-amd64",
                "astral-sh/uv",
                "0.13.9",
                "uv-x86_64-unknown-linux-gnu.tar.gz",
                "tar.gz",
            ),
            (
                "uv",
                "linux-arm64",
                "astral-sh/uv",
                "0.13.9",
                "uv-aarch64-unknown-linux-gnu.tar.gz",
                "tar.gz",
            ),
            (
                "gh",
                "darwin-amd64",
                "cli/cli",
                "v2.105.0",
                "gh_2.105.0_macOS_amd64.zip",
                "zip",
            ),
            (
                "gh",
                "darwin-arm64",
                "cli/cli",
                "v2.105.0",
                "gh_2.105.0_macOS_arm64.zip",
                "zip",
            ),
            (
                "gh",
                "linux-amd64",
                "cli/cli",
                "v2.105.0",
                "gh_2.105.0_linux_amd64.tar.gz",
                "tar.gz",
            ),
            (
                "gh",
                "linux-arm64",
                "cli/cli",
                "v2.105.0",
                "gh_2.105.0_linux_arm64.tar.gz",
                "tar.gz",
            ),
        ] {
            let row = resolve_fixture(
                name,
                host,
                vec![response(
                    &format!("https://api.github.com/repos/{repo}/releases/latest"),
                    github_fixture(repo, tag, filename),
                )],
            )
            .unwrap();
            assert_eq!(row["version"], tag.trim_start_matches('v'));
            assert_eq!(row["platforms"][host]["archive"], format);
            assert_eq!(row["platforms"][host]["sha256"], "a".repeat(64));
            assert_eq!(row["platforms"].as_object().unwrap().len(), 1);
        }
    }

    #[test]
    fn github_rejects_unpublished_misbound_and_unverified_assets() {
        let base = github_fixture("cli/cli", "v2.105.0", "gh_2.105.0_linux_amd64.tar.gz");
        for (pointer, bad) in [
            ("/draft", json!(true)),
            ("/prerelease", json!(true)),
            ("/tag_name", json!("v2.106.0-rc.1")),
            ("/assets/0/digest", Value::Null),
            (
                "/assets/0/digest",
                json!(format!("sha512:{}", "a".repeat(128))),
            ),
            ("/assets/0/digest", json!("sha256:invalid")),
            ("/assets/0/state", json!("new")),
            ("/assets/0/size", json!(0)),
            ("/assets/0/size", json!(MAX as u64 + 1)),
            (
                "/assets/0/browser_download_url",
                json!(
                    "https://github.com/other/cli/releases/download/v2.105.0/gh_2.105.0_linux_amd64.tar.gz"
                ),
            ),
        ] {
            let mut bad_release = base.clone();
            *bad_release.pointer_mut(pointer).unwrap() = bad;
            assert!(
                resolve_fixture(
                    "gh",
                    "linux-amd64",
                    vec![response(
                        "https://api.github.com/repos/cli/cli/releases/latest",
                        bad_release,
                    )],
                )
                .is_err(),
                "accepted {pointer}"
            );
        }
        let mut duplicate = base.clone();
        duplicate["assets"]
            .as_array_mut()
            .unwrap()
            .push(base["assets"][0].clone());
        assert!(
            resolve_fixture(
                "gh",
                "linux-amd64",
                vec![response(
                    "https://api.github.com/repos/cli/cli/releases/latest",
                    duplicate,
                )],
            )
            .is_err()
        );
    }

    #[test]
    fn npm_latest_freezes_native_versions_for_only_the_current_host() {
        for (host, suffix) in [
            ("darwin-amd64", "darwin-x64"),
            ("darwin-arm64", "darwin-arm64"),
            ("linux-amd64", "linux-x64"),
            ("linux-arm64", "linux-arm64"),
        ] {
            for (name, package, release) in [
                ("claude", "@anthropic-ai/claude-code", "2.1.20"),
                ("codex", "@openai/codex", "0.150.0"),
            ] {
                let alias = format!("{package}-{suffix}");
                let native_package = if name == "codex" { package } else { &alias };
                let native_version = if name == "codex" {
                    format!("{release}-{suffix}")
                } else {
                    release.into()
                };
                let row = resolve_fixture(
                    name,
                    host,
                    vec![
                        response(
                            &format!(
                                "https://registry.npmjs.org/{}/latest",
                                package.replace('/', "%2F")
                            ),
                            npm_fixture(package, release),
                        ),
                        response(
                            &format!(
                                "https://registry.npmjs.org/{}/{native_version}",
                                native_package.replace('/', "%2F")
                            ),
                            npm_fixture(native_package, &native_version),
                        ),
                    ],
                )
                .unwrap();
                assert_eq!(row["version"], release);
                assert_eq!(row["native"].as_object().unwrap().len(), 1);
                assert_eq!(row["native"][host]["version"], native_version);
                assert_eq!(row["native"][host]["alias"], alias);
            }
        }
        let ntn = resolve_fixture(
            "ntn",
            "linux-amd64",
            vec![response(
                "https://registry.npmjs.org/ntn/latest",
                npm_fixture("ntn", "0.9.1"),
            )],
        )
        .unwrap();
        assert_eq!(ntn["version"], "0.9.1");
        assert!(ntn.get("native").is_none());
    }

    #[test]
    fn npm_rejects_identity_tarball_and_integrity_mismatches() {
        for (pointer, bad) in [
            ("/name", json!("other-package")),
            ("/version", json!("0.9.2")),
            (
                "/dist/tarball",
                json!("https://registry.npmjs.org/ntn/-/ntn-0.9.0.tgz"),
            ),
            (
                "/dist/tarball",
                json!("https://example.invalid/ntn-0.9.1.tgz"),
            ),
            ("/dist/integrity", json!("sha1-invalid")),
        ] {
            let mut bad_release = npm_fixture("ntn", "0.9.1");
            *bad_release.pointer_mut(pointer).unwrap() = bad;
            assert!(
                npm_metadata("ntn", "0.9.1", &mut |_| Ok(r::bytes(&bad_release).unwrap())).is_err()
            );
        }
        assert!(
            resolve_fixture(
                "ntn",
                "linux-amd64",
                vec![response(
                    "https://registry.npmjs.org/ntn/latest",
                    npm_fixture("ntn", "1.0.0-beta.1"),
                )],
            )
            .is_err()
        );
    }

    #[test]
    fn agy_uses_only_current_host_manifest_and_requires_versioned_bucket_and_digest() {
        for host in ["darwin-amd64", "darwin-arm64", "linux-amd64", "linux-arm64"] {
            let url = format!(
                "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/{}.json",
                host.replace('-', "_")
            );
            let manifest = json!({
                "version": "1.5.0",
                "url": format!("https://storage.googleapis.com/antigravity-public/antigravity-cli/1.5.0-build/{host}/payload.tar.gz"),
                "sha512": "a".repeat(128)
            });
            let row = resolve_fixture("agy", host, vec![response(&url, manifest.clone())]).unwrap();
            assert_eq!(row["version"], "1.5.0");
            assert_eq!(row["platforms"].as_object().unwrap().len(), 1);
            for (key, bad) in [
                ("sha512", json!("invalid")),
                (
                    "url",
                    json!("https://storage.googleapis.com/another-bucket/1.5.0/payload.tar.gz"),
                ),
                (
                    "url",
                    json!(
                        "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.4.0-build/payload.tar.gz"
                    ),
                ),
                (
                    "url",
                    json!(
                        "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.5.0-build/../payload.tar.gz"
                    ),
                ),
            ] {
                let mut bad_manifest = manifest.clone();
                bad_manifest[key] = bad;
                assert!(resolve_fixture("agy", host, vec![response(&url, bad_manifest)]).is_err());
            }
        }
    }

    #[test]
    fn unavailable_upstream_never_resolves_to_an_observed_or_built_in_version() {
        for name in ["node", "uv", "gh", "ntn", "claude", "codex", "agy"] {
            let error = resolve_tool(name, "linux-amd64", &mut |_| {
                Err(r::Error(4, "fixture upstream unavailable".into()))
            })
            .unwrap_err();
            assert_eq!(error.0, 4);
        }
    }

    #[test]
    fn newer_installed_versions_are_retained_unless_the_user_explicitly_pins() {
        assert!(satisfies_release(b"uv 0.14.0\n", "0.13.9", true));
        assert!(!satisfies_release(b"uv 0.14.0\n", "0.13.9", false));
        assert!(satisfies_release(b"v26.1.0\n", "26.1.0", false));
        assert!(!satisfies_release(b"gh version 2.100.0\n", "2.105.0", true));
        assert!(!satisfies_release(b"unrecognized version\n", "1.0.0", true));
        assert!(optional_upgrade("1.9.9", "2.0.0").unwrap());
        assert!(optional_upgrade("2.0.0-beta.1", "2.0.0").unwrap());
        assert!(!optional_upgrade("2.0.0", "2.0.0").unwrap());
        assert!(!optional_upgrade("2.1.0", "2.0.0").unwrap());
        assert!(!optional_upgrade("2.0.0+build.1", "2.0.0").unwrap());
        assert!(!optional_upgrade("2.0.0+build-1", "2.0.0").unwrap());
        assert!(optional_upgrade("invalid", "2.0.0").is_err());
        assert!(optional_upgrade("2.0.0", "2.1.0-beta.1").is_err());
    }
}
