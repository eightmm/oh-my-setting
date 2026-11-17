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
fn scalar<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    v[name]
        .as_str()
        .filter(|s| !s.is_empty() && !s.contains(['\0', '\n', '\r']))
        .ok_or_else(|| r::Error(2, format!("invalid tool lock field: {name}")))
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
        .ok_or_else(|| r::Error(2, "locked download must use HTTPS".into()))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty()
        || host.contains(['@', ':', '?', '#'])
        || url.contains(['\r', '\n', '\0', '?', '#', '*', ' ', '\\'])
        || url.contains("${")
        || url.contains("latest")
    {
        return r::fail("locked download must be an exact HTTPS URL");
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
        return r::fail("locked tool checksum mismatch");
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
    let lock_file = std::env::var_os("OH_MY_SETTING_TOOL_LOCK")
        .map(PathBuf::from)
        .unwrap_or(paths.source.join("tools.lock.json"));
    let mut lock = r::value(&lock_file)?;
    if lock["schema"] != 2 {
        return r::fail("unsupported tool lock schema");
    }
    latest(paths, &mut lock, selected, tx, flag)?;
    let host = platform()?;
    let scratch = r::Scratch::new()?;
    let bin = paths.home.join(".local/bin");
    for name in selected {
        if !["node", "uv", "gh", "agy", "claude", "codex", "ntn"].contains(&name.as_str()) {
            return r::fail("unknown managed tool");
        }
        r::check(flag)?;
        if let Some(binary) = r::find(name, &paths.cwd).or_else(|| {
            let p = bin.join(name);
            p.is_file().then_some(p)
        }) {
            if !upgrade && ["claude", "codex", "agy", "ntn"].contains(&name.as_str()) {
                continue;
            }
            let catalog = if name == "agy" {
                "antigravity"
            } else {
                name.as_str()
            };
            if !["claude", "codex", "agy", "ntn"].contains(&name.as_str()) {
                let out = r::command(
                    &binary,
                    &["--version".into()],
                    &paths.cwd,
                    None,
                    flag,
                    Duration::from_secs(10),
                )?;
                if out.0 == 0
                    && String::from_utf8_lossy(&out.1).split_whitespace().any(|s| {
                        s.trim_start_matches('v') == lock[catalog]["version"].as_str().unwrap_or("")
                    })
                {
                    continue;
                }
            }
            if ["claude", "codex", "agy"].contains(&name.as_str())
                && std::env::var_os("OH_MY_SETTING_TOOL_LOCK").is_none()
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
                if before.0 != 0
                    || after.0 != 0
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
fn metadata(paths: &Paths, url: &str, flag: &AtomicBool) -> Result<Value> {
    let host = url
        .strip_prefix("https://")
        .and_then(|s| s.split('/').next())
        .unwrap_or("");
    if ![
        "registry.npmjs.org",
        "antigravity-cli-auto-updater-974169037036.us-central1.run.app",
    ]
    .contains(&host)
        || url.contains(['?', '#', '\r', '\n'])
    {
        return r::fail("unapproved provider metadata endpoint");
    }
    let program = r::find("curl", &paths.cwd)
        .ok_or_else(|| r::Error(2, "curl required for provider metadata".into()))?;
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
        return r::fail("provider metadata unavailable or oversized");
    }
    let split = out
        .1
        .iter()
        .rposition(|b| *b == b'\n')
        .ok_or_else(|| r::Error(2, "invalid metadata response".into()))?;
    if &out.1[split + 1..] == b"404" {
        return Err(r::Error(4, "provider release incomplete".into()));
    }
    if &out.1[split + 1..] != b"200" {
        return r::fail("provider metadata redirects and non-success responses are refused");
    }
    r::parse(&out.1[..split])
}
fn npm_release(
    paths: &Paths,
    package: &str,
    release: &str,
    flag: &AtomicBool,
) -> Result<(String, String)> {
    let encoded = package.replace('/', "%2F");
    let row = metadata(
        paths,
        &format!("https://registry.npmjs.org/{encoded}/{release}"),
        flag,
    )?;
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
    Ok((actual, integrity))
}
fn stable(actual: &str, previous: &str) -> Result<()> {
    let parse = |s: &str| {
        s.trim_start_matches('v')
            .split('.')
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .ok()
            .filter(|v| v.len() == 3)
    };
    if parse(actual)
        .zip(parse(previous))
        .is_none_or(|(a, b)| a < b)
    {
        return r::fail("provider release is not a non-downgraded stable version");
    }
    Ok(())
}
fn latest(
    paths: &Paths,
    lock: &mut Value,
    selected: &[String],
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<()> {
    if std::env::var_os("OH_MY_SETTING_TOOL_LOCK").is_some() {
        return Ok(());
    }
    let file = paths.data.join("provider-tools.lock.json");
    if r::exists(&file) {
        let old = r::value(&file)?;
        if old["schema"] != 2 {
            return r::fail("invalid previous provider snapshot");
        }
        for name in ["claude", "codex"] {
            lock["npm"][name] = old["npm"][name].clone();
        }
        lock["antigravity"] = old["antigravity"].clone();
    }
    let mut changed = false;
    for name in selected
        .iter()
        .filter(|n| ["claude", "codex", "agy"].contains(&n.as_str()))
    {
        if r::find(name, &paths.cwd).is_some_and(|p| vendor(paths, name, &p)) {
            continue;
        }
        let mut candidate = lock.clone();
        let resolved = (|| -> Result<()> {
            if name == "agy" {
                let previous = scalar(&candidate["antigravity"], "version")?.to_owned();
                let mut releases = std::collections::BTreeSet::new();
                let platforms = candidate["antigravity"]["platforms"]
                    .as_object()
                    .ok_or_else(|| r::Error(2, "invalid provider platforms".into()))?
                    .keys()
                    .filter(|s| !s.starts_with("windows-"))
                    .cloned()
                    .collect::<Vec<_>>();
                for host in platforms {
                    let row = metadata(
                        paths,
                        &format!(
                            "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/{}.json",
                            host.replace('-', "_")
                        ),
                        flag,
                    )?;
                    let release = scalar(&row, "version")?;
                    stable(release, &previous)?;
                    releases.insert(release.to_owned());
                    let url = scalar(&row, "url")?;
                    https(url)?;
                    if !url.starts_with(&format!("https://storage.googleapis.com/antigravity-public/antigravity-cli/{release}-")) || url.split('/').any(|p|p==".."||p=="."){return r::fail("provider release URL escaped its official bucket");}
                    let digest = scalar(&row, "sha512")?;
                    if digest.len() != 128 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return r::fail("invalid provider release checksum");
                    }
                    candidate["antigravity"]["platforms"][&host]["url"] = json!(url);
                    candidate["antigravity"]["platforms"][&host]["sha512"] = json!(digest);
                }
                if releases.len() != 1 {
                    return Err(r::Error(
                        4,
                        "provider release incomplete across supported platforms".into(),
                    ));
                }
                candidate["antigravity"]["version"] = json!(releases.into_iter().next().unwrap());
            } else {
                let row = &mut candidate["npm"][name];
                let (release, integrity) =
                    npm_release(paths, scalar(row, "package")?, "latest", flag)?;
                stable(&release, scalar(row, "version")?)?;
                row["version"] = json!(release);
                row["integrity"] = json!(integrity);
                for (host, native) in row["native"]
                    .as_object_mut()
                    .ok_or_else(|| r::Error(2, "invalid npm native platforms".into()))?
                {
                    if host.starts_with("windows-") {
                        continue;
                    }
                    let suffix = host
                        .replace("darwin-amd64", "darwin-x64")
                        .replace("linux-amd64", "linux-x64");
                    let expected = if name == "codex" {
                        format!("{release}-{suffix}")
                    } else {
                        release.clone()
                    };
                    let (actual, integrity) =
                        npm_release(paths, scalar(native, "package")?, &expected, flag)?;
                    native["version"] = json!(actual);
                    native["integrity"] = json!(integrity);
                }
            }
            Ok(())
        })();
        match resolved {
            Ok(()) => {
                *lock = candidate;
                changed = true;
            }
            Err(error) if error.0 == 4 => {
                tx.warn("provider release incomplete; retaining the previous verified snapshot")
            }
            Err(error) => return Err(error),
        }
    }
    if changed {
        tx.write(&file, &r::bytes(lock)?, 0o600)?;
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
    let now = chrono::DateTime::parse_from_rfc3339(&r::now())
        .map_err(|_| r::Error(2, "clock unavailable".into()))?;
    let cutoff = now - chrono::Duration::days(14);
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
        let history = match metadata(
            paths,
            &format!("https://registry.npmjs.org/{}", name.replace('/', "%2F")),
            flag,
        ) {
            Ok(row) => row,
            Err(_) => {
                r::check(flag)?;
                tx.warn("optional provider metadata unavailable; keeping the installed package");
                continue;
            }
        };
        let mut eligible = Vec::new();
        let parse = |s: &str| {
            s.split('+')
                .next()
                .unwrap_or(s)
                .split('.')
                .map(str::parse::<u64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .ok()
                .filter(|p| p.len() == 3)
        };
        let base = previous.split('-').next().unwrap_or(previous);
        let Some(installed) = parse(base) else {
            continue;
        };
        for (release, data) in history["versions"]
            .as_object()
            .ok_or_else(|| r::Error(2, "invalid optional release catalog".into()))?
        {
            let Some(number) = parse(release) else {
                continue;
            };
            let published = history["time"][release]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
            if published.is_some_and(|date| date <= cutoff)
                && (number > installed || number == installed && previous.contains('-'))
            {
                eligible.push((number, release.clone(), data.clone()));
            }
        }
        eligible.sort_by(|a, b| a.0.cmp(&b.0));
        let Some((_, release, candidate)) = eligible.pop() else {
            continue;
        };
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
        let (_, integrity) = npm_release(paths, name, &release, flag)?;
        let archive = pack(
            paths,
            &json!({"package":name,"version":release,"integrity":integrity}),
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
                format!("--before={}", cutoff.format("%Y-%m-%dT%H:%M:%SZ")).into(),
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
