use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha512};
use std::{
    borrow::Cow,
    ffi::OsString,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};
const MAX: usize = 1024 * 1024 * 1024;
// npm 7.18 added the pack destination used to keep verified tarballs in scratch.
const NPM_PACK_MINIMUM: &[u64] = &[7, 18, 0];
// The Codex npm launcher declares Node 16 or later.
const PROVIDER_NODE_MAJOR: u64 = 16;
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

fn provider_node(paths: &Paths, flag: &AtomicBool) -> Result<Option<PathBuf>> {
    let local = paths.home.join(".local/bin/node");
    let ambient = r::find("node", &paths.cwd).filter(|path| *path != local);
    for binary in std::iter::once(local).chain(ambient) {
        if !fs::metadata(&binary).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            continue;
        }
        let out = r::command(
            &binary,
            &["--version".into()],
            &paths.cwd,
            None,
            flag,
            Duration::from_secs(10),
        );
        r::check(flag)?;
        if out.is_ok_and(|out| {
            out.0 == 0 && version_number(&out.1).is_some_and(|v| v[0] >= PROVIDER_NODE_MAJOR)
        }) {
            return Ok(Some(binary));
        }
    }
    Ok(None)
}

fn npm_runtime(
    paths: &Paths,
    flag: &AtomicBool,
    needs_pack: bool,
) -> Result<Option<(PathBuf, OsString, Vec<u64>)>> {
    let node = installed(paths, "node");
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        node.iter()
            .map(|binary| binary.parent().unwrap().to_path_buf())
            .chain(std::env::split_paths(&inherited)),
    )
    .map_err(|_| {
        r::Error(
            2,
            "Node runtime directory cannot be represented in PATH".into(),
        )
    })?;
    let local = paths.home.join(".local/bin/npm");
    let mut candidates = r::find("npm", &paths.cwd).into_iter().collect::<Vec<_>>();
    if !candidates.contains(&local) {
        candidates.push(local);
    }
    for binary in candidates {
        if !fs::metadata(&binary).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            continue;
        }
        let probe = r::command_env(
            &binary,
            &["--version".into()],
            &paths.cwd,
            None,
            flag,
            Duration::from_secs(10),
            &[("PATH", path.as_os_str())],
        );
        r::check(flag)?;
        if let Ok((0, output, _)) = probe
            && let Some(version) = version_number(&output)
            && (!needs_pack || version.as_slice() >= NPM_PACK_MINIMUM)
        {
            return Ok(Some((binary, path, version)));
        }
    }
    Ok(None)
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
    let (binary, path) = if name == "npm" {
        let needs_pack = args.first().is_some_and(|argument| argument == "pack");
        let (binary, path, _) = npm_runtime(paths, flag, needs_pack)?.ok_or_else(|| {
            r::Error(
                2,
                if needs_pack {
                    "npm 7.18.0 or later required for verified npm pack"
                } else {
                    "working npm required for verified tool installation"
                }
                .into(),
            )
        })?;
        (binary, Some(path))
    } else {
        let binary = installed(paths, name).ok_or_else(|| {
            r::Error(2, format!("{name} required for verified tool installation"))
        })?;
        (binary, None)
    };
    let environment = path
        .as_deref()
        .map(|value| ("PATH", value))
        .into_iter()
        .collect::<Vec<_>>();
    let out = r::command_env_limit(
        binary,
        args,
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(180),
        &environment,
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
pub(crate) fn extract(
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
fn ownership_marker(target: &Path) -> PathBuf {
    target.with_file_name(format!(
        "{}.oh-my-setting-managed",
        target.file_name().unwrap().to_string_lossy()
    ))
}
/// The second value identifies the marker formats eligible for vendor handoff
/// after a previous updater changed the binary without retiring its marker.
fn ownership_digest(raw: &[u8]) -> Option<(Cow<'_, str>, bool)> {
    let text = std::str::from_utf8(raw).ok()?;
    let (digest, canonical) = if let Some(legacy) = text.strip_prefix("sha256=") {
        (Cow::Borrowed(legacy.strip_suffix('\n')?), true)
    } else {
        let row = r::parse(raw).ok()?;
        if row["owner"] != "oh-my-setting" {
            return None;
        }
        (
            Cow::Owned(row["sha256"].as_str()?.to_owned()),
            row.as_object()?.len() == 2,
        )
    };
    (digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some((digest, canonical))
}

fn owns(paths: &Paths, target: &Path) -> Result<()> {
    if !r::exists(target) {
        return Ok(());
    }
    let marker = ownership_marker(target);
    let raw = r::read(&marker, 65536)
        .map_err(|_| r::Error(2, "refusing to replace an unmanaged tool".into()))?;
    let actual = r::hash(&r::read(target, MAX)?);
    if ownership_digest(&raw).is_none_or(|(digest, _)| digest != actual) {
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
    let marker = ownership_marker(target);
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
/// Stateful custody guard pinning an AGY vendor binary, its parent directory,
/// and any initial OMS ownership marker across pre-handoff `--version` probes.
struct AgyMarkerCustody {
    binary_before: fs::Metadata,
    directory: fs::File,
    parent_dev: u64,
    parent_ino: u64,
    name: OsString,
    initial: Option<AgyMarker>,
}

struct AgyMarker {
    stat: rustix::fs::Stat,
    raw: Vec<u8>,
    binary_digest: String,
    stale: bool,
}

impl AgyMarkerCustody {
    fn metadata_matches(a: &fs::Metadata, b: &fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.size() == b.size()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
            && a.mtime() == b.mtime()
            && a.mtime_nsec() == b.mtime_nsec()
    }

    fn stat_matches(a: &rustix::fs::Stat, b: &rustix::fs::Stat) -> bool {
        a.st_dev == b.st_dev
            && a.st_ino == b.st_ino
            && a.st_mode == b.st_mode
            && a.st_uid == b.st_uid
            && a.st_gid == b.st_gid
            && a.st_nlink == b.st_nlink
            && a.st_size == b.st_size
            && a.st_ctime == b.st_ctime
            && a.st_ctime_nsec == b.st_ctime_nsec
            && a.st_mtime == b.st_mtime
            && a.st_mtime_nsec == b.st_mtime_nsec
    }

    fn verify_parent(directory: &fs::File, parent: &Path, dev: u64, ino: u64) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let dir_meta = directory.metadata()?;
        let parent_meta = fs::symlink_metadata(parent)?;
        if !dir_meta.is_dir()
            || !parent_meta.is_dir()
            || parent_meta.file_type().is_symlink()
            || (dir_meta.dev(), dir_meta.ino()) != (dev, ino)
            || (parent_meta.dev(), parent_meta.ino()) != (dev, ino)
        {
            return r::fail("AGY ownership marker changed before vendor handoff");
        }
        Ok(())
    }

    fn verify_binary(before: &fs::Metadata, binary: &Path) -> Result<()> {
        let after = fs::symlink_metadata(binary)?;
        if !after.is_file()
            || after.file_type().is_symlink()
            || !Self::metadata_matches(before, &after)
        {
            return r::fail("AGY ownership marker changed before vendor handoff");
        }
        Ok(())
    }

    fn read_verified_marker(
        directory: &fs::File,
        name: &OsString,
        expected: &rustix::fs::Stat,
    ) -> Result<Vec<u8>> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        use std::io::Read;

        let fd = openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let opened = fstat(&fd).map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
            || opened.st_nlink != 1
            || !Self::stat_matches(expected, &opened)
        {
            return r::fail("AGY ownership marker changed before vendor handoff");
        }
        let expected_len = usize::try_from(expected.st_size)
            .map_err(|_| r::Error(2, "AGY ownership marker size out of range".into()))?;
        let mut file = fs::File::from(fd);
        let mut raw = Vec::with_capacity(expected_len);
        file.by_ref().take(4097).read_to_end(&mut raw)?;
        if raw.len() != expected_len {
            return r::fail("AGY ownership marker changed before vendor handoff");
        }
        Ok(raw)
    }

    fn capture(paths: &Paths, binary: &Path) -> Result<Self> {
        use rustix::fs::{AtFlags, FileType, Mode, OFlags, open, statat};
        use rustix::io::Errno;
        use std::os::unix::fs::MetadataExt;

        if !vendor(paths, "agy", binary) {
            return r::fail("AGY ownership handoff requires its recognized vendor path");
        }
        let binary_before = fs::symlink_metadata(binary)?;
        if !binary_before.is_file() || binary_before.file_type().is_symlink() {
            return r::fail("AGY ownership handoff requires its recognized vendor path");
        }
        let raw_marker = ownership_marker(binary);
        if let Some(raw_parent) = raw_marker.parent() {
            let raw_meta = fs::symlink_metadata(raw_parent)?;
            if !raw_meta.is_dir() || raw_meta.file_type().is_symlink() {
                return r::fail("AGY ownership marker changed before vendor handoff");
            }
        }
        let marker = r::leaf(&raw_marker)?;
        let parent = marker
            .parent()
            .ok_or_else(|| r::Error(2, "missing AGY ownership marker parent".into()))?;
        let name = marker
            .file_name()
            .ok_or_else(|| r::Error(2, "missing AGY ownership marker name".into()))?
            .to_os_string();
        let directory = fs::File::from(
            open(
                parent,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let dir_meta = directory.metadata()?;
        let (parent_dev, parent_ino) = (dir_meta.dev(), dir_meta.ino());
        if let Some(raw_parent) = raw_marker.parent()
            && raw_parent != parent
        {
            Self::verify_parent(&directory, raw_parent, parent_dev, parent_ino)?;
        }
        Self::verify_parent(&directory, parent, parent_dev, parent_ino)?;

        let initial = match statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => {
                Self::verify_parent(&directory, parent, parent_dev, parent_ino)?;
                Self::verify_binary(&binary_before, binary)?;
                None
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
            Ok(before) => {
                if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
                    || before.st_nlink != 1
                    || before.st_uid != rustix::process::getuid().as_raw()
                    || before.st_mode & 0o022 != 0
                    || before.st_size <= 0
                    || before.st_size > 4096
                {
                    return r::fail("AGY ownership marker changed before vendor handoff");
                }
                let raw = Self::read_verified_marker(&directory, &name, &before)?;
                let (digest, canonical) = ownership_digest(&raw).ok_or_else(|| {
                    r::Error(2, "invalid AGY ownership marker; refresh refused".into())
                })?;
                let binary_digest = r::hash(&r::read(binary, MAX)?);
                let stale = digest != binary_digest;
                if stale && !canonical {
                    return r::fail("invalid AGY ownership marker; refresh refused");
                }
                let after = statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(std::io::Error::from)?;
                if FileType::from_raw_mode(after.st_mode) != FileType::RegularFile
                    || after.st_nlink != 1
                    || !Self::stat_matches(&before, &after)
                {
                    return r::fail("AGY ownership marker changed before vendor handoff");
                }
                Self::verify_parent(&directory, parent, parent_dev, parent_ino)?;
                Self::verify_binary(&binary_before, binary)?;
                Some(AgyMarker {
                    stat: before,
                    raw,
                    binary_digest,
                    stale,
                })
            }
        };

        Ok(Self {
            binary_before,
            directory,
            parent_dev,
            parent_ino,
            name,
            initial,
        })
    }

    fn verify_continuity(&self, paths: &Paths, binary: &Path, retire: bool) -> Result<()> {
        use rustix::fs::{AtFlags, FileType, statat, unlinkat};
        use rustix::io::Errno;

        if !vendor(paths, "agy", binary) {
            return r::fail("AGY ownership handoff requires its recognized vendor path");
        }
        Self::verify_binary(&self.binary_before, binary)?;
        let raw_marker = ownership_marker(binary);
        let marker = r::leaf(&raw_marker)?;
        let parent = marker
            .parent()
            .ok_or_else(|| r::Error(2, "missing AGY ownership marker parent".into()))?;
        if marker.file_name() != Some(self.name.as_os_str()) {
            return r::fail("AGY ownership marker changed before vendor handoff");
        }
        if let Some(raw_parent) = raw_marker.parent()
            && raw_parent != parent
        {
            Self::verify_parent(
                &self.directory,
                raw_parent,
                self.parent_dev,
                self.parent_ino,
            )?;
        }
        Self::verify_parent(&self.directory, parent, self.parent_dev, self.parent_ino)?;

        match (
            &self.initial,
            statat(&self.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW),
        ) {
            (None, Err(Errno::NOENT)) => {
                Self::verify_parent(&self.directory, parent, self.parent_dev, self.parent_ino)?;
                Self::verify_binary(&self.binary_before, binary)?;
                Ok(())
            }
            (None, Ok(_)) | (Some(_), Err(Errno::NOENT)) => {
                r::fail("AGY ownership marker changed before vendor handoff")
            }
            (Some(before), Ok(after_stat)) => {
                if FileType::from_raw_mode(after_stat.st_mode) != FileType::RegularFile
                    || after_stat.st_nlink != 1
                    || !Self::stat_matches(&before.stat, &after_stat)
                {
                    return r::fail("AGY ownership marker changed before vendor handoff");
                }
                let raw = Self::read_verified_marker(&self.directory, &self.name, &before.stat)?;
                if raw != before.raw || r::hash(&r::read(binary, MAX)?) != before.binary_digest {
                    return r::fail("AGY ownership marker changed before vendor handoff");
                }
                let recheck = statat(&self.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(std::io::Error::from)?;
                if FileType::from_raw_mode(recheck.st_mode) != FileType::RegularFile
                    || recheck.st_nlink != 1
                    || !Self::stat_matches(&before.stat, &recheck)
                {
                    return r::fail("AGY ownership marker changed before vendor handoff");
                }
                Self::verify_parent(&self.directory, parent, self.parent_dev, self.parent_ino)?;
                Self::verify_binary(&self.binary_before, binary)?;
                if retire {
                    // Vendor bytes are outside OMS rollback. Restoring this marker on a later
                    // failure would attach an obsolete checksum, so retire it durably.
                    unlinkat(&self.directory, &self.name, AtFlags::empty())
                        .map_err(std::io::Error::from)?;
                    self.directory.sync_all()?;
                    if before.stale {
                        eprintln!("warning: retired stale AGY ownership marker for vendor update");
                    }
                }
                Ok(())
            }
            (_, Err(error)) => Err(std::io::Error::from(error).into()),
        }
    }
}

fn refresh_vendor(paths: &Paths, name: &str, binary: &Path, flag: &AtomicBool) -> Result<()> {
    refresh_vendor_with_custody(paths, name, binary, None, flag)
}

fn refresh_vendor_with_custody(
    paths: &Paths,
    name: &str,
    binary: &Path,
    custody: Option<AgyMarkerCustody>,
    flag: &AtomicBool,
) -> Result<()> {
    r::check(flag)?;
    let custody = if name == "agy" {
        Some(match custody {
            Some(custody) => custody,
            None => AgyMarkerCustody::capture(paths, binary)?,
        })
    } else {
        None
    };
    let before = r::command(
        binary,
        &["--version".into()],
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(10),
    )?;
    if before.0 != 0 || version_number(&before.1).is_none() {
        return r::fail("vendor provider version unavailable; refresh refused");
    }
    r::check(flag)?;
    if let Some(custody) = custody.as_ref() {
        custody.verify_continuity(paths, binary, true)?;
    }
    let out = r::command(
        binary,
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
    Ok(())
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
    let needs_install =
        |name: &str| upgrade || explicit_lock.is_some() || installed(paths, name).is_none();
    let mut npm_tools = selected
        .iter()
        .map(String::as_str)
        .filter(|name| ["claude", "codex", "ntn"].contains(name) && needs_install(name))
        .filter(|name| {
            explicit_lock.is_some()
                || !installed(paths, name).is_some_and(|binary| vendor(paths, name, &binary))
        })
        .peekable();
    let needs_pack = npm_tools.peek().is_some();
    let needs_provider_node =
        explicit_lock.is_none() && npm_tools.any(|name| matches!(name, "codex" | "ntn"));
    let node_ready = !selected.iter().any(|name| name == "node")
        || (npm_runtime(paths, flag, needs_pack)?.is_some()
            && (!needs_provider_node || provider_node(paths, flag)?.is_some()));
    let selected = selected
        .iter()
        .filter(|name| needs_install(name) || name.as_str() == "node" && !node_ready)
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
        let mut retain_node = false;
        if let Some(binary) = installed(paths, name) {
            let custody =
                if name == "agy" && explicit_lock.is_none() && vendor(paths, "agy", &binary) {
                    Some(AgyMarkerCustody::capture(paths, &binary)?)
                } else {
                    None
                };
            if let Some(expected) = tool_row(&lock, name)["version"].as_str() {
                let out = r::command(
                    &binary,
                    &["--version".into()],
                    &paths.cwd,
                    None,
                    flag,
                    Duration::from_secs(10),
                )?;
                r::check(flag)?;
                if explicit_lock.is_none() && (out.0 != 0 || version_number(&out.1).is_none()) {
                    return r::fail(format!("{name} version unavailable; refresh refused"));
                }
                if out.0 == 0 && satisfies_release(&out.1, expected, explicit_lock.is_none()) {
                    if name == "node" && !node_ready {
                        // Repair the bundled package tooling without replacing an
                        // already suitable (including newer) installed Node.
                        retain_node = true;
                    } else {
                        if let Some(custody) = custody.as_ref() {
                            custody.verify_continuity(paths, &binary, false)?;
                        }
                        continue;
                    }
                }
            }
            if custody.is_some()
                || ["claude", "codex", "agy"].contains(&name.as_str())
                    && explicit_lock.is_none()
                    && vendor(paths, name, &binary)
            {
                match custody {
                    Some(custody) => {
                        refresh_vendor_with_custody(paths, name, &binary, Some(custody), flag)?;
                    }
                    None => refresh_vendor(paths, name, &binary, flag)?,
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
            let node = if explicit_lock.is_none() {
                provider_node(paths, flag)?
                    .ok_or_else(|| r::Error(2, "Node 16 or later required for npm tool".into()))?
            } else if bin.join("node").is_file() {
                bin.join("node")
            } else {
                r::find("node", &paths.cwd)
                    .ok_or_else(|| r::Error(2, "Node runtime unavailable".into()))?
            };
            if name == "ntn" && target.join("install.cjs").is_file() {
                let checkpoint = tx.prepare_external(std::slice::from_ref(&target))?;
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
                    if command == "node" && retain_node {
                        continue;
                    }
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
                if npm_runtime(paths, flag, needs_pack)?.is_none() {
                    return r::fail("verified Node distribution did not provide working npm");
                }
                if needs_provider_node && provider_node(paths, flag)?.is_none() {
                    return r::fail(
                        "verified Node distribution did not provide a provider runtime",
                    );
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
    let npm_can_pack = npm_runtime(paths, flag, false)?
        .is_some_and(|(_, _, version)| version.as_slice() >= NPM_PACK_MINIMUM);
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
        if !npm_can_pack {
            return r::fail("npm 7.18.0 or later required to refresh this npm prefix");
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

    fn fixture_paths(root: &Path) -> Paths {
        Paths {
            home: root.join("home"),
            source: root.to_owned(),
            receipt: root.join("receipt.json"),
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            cwd: root.to_owned(),
        }
    }

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
    fn installed_node_requires_working_npm_and_preserves_newer_runtime() {
        if let Some(root) = std::env::var_os("OMS_SUPPLY_NODE_FIXTURE") {
            let paths = fixture_paths(Path::new(&root));
            let kind = std::env::var("OMS_SUPPLY_NODE_CASE").unwrap();
            if kind == "self-contained" {
                assert!(installed(&paths, "node").is_none());
                assert_eq!(
                    call(
                        &paths,
                        "npm",
                        &["--version".into()],
                        &AtomicBool::new(false),
                        false
                    )
                    .unwrap(),
                    b"11.1.0\n"
                );
                return;
            }
            let original = installed(&paths, "node").unwrap();
            let before = fs::read(&original).unwrap();
            let bundled_npm = original.with_file_name("npm");
            let npm_before = fs::read(&bundled_npm).ok();
            let legacy = matches!(kind.as_str(), "old-npm" | "old-node");
            let flag = AtomicBool::new(false);
            if legacy {
                assert_eq!(
                    npm_runtime(&paths, &flag, true).unwrap().is_none(),
                    kind == "old-npm"
                );
                assert_eq!(
                    provider_node(&paths, &flag).unwrap().is_none(),
                    kind == "old-node"
                );
            }
            let selected = if legacy {
                vec!["node".into(), "ntn".into()]
            } else {
                vec!["node".into()]
            };
            let _guard = r::lock(&paths.receipt, &paths.cwd).unwrap();
            let mut tx = Transaction::begin(&paths).unwrap();
            let result = install(&paths, &selected, false, false, &mut tx, &flag);
            if legacy {
                // The fixture stops at the provider's pack response. Node must
                // already have been provisioned, even outside the initial PATH.
                assert!(result.is_err());
                let requests = fs::read_to_string(paths.cwd.join("requests")).unwrap();
                assert!(
                    requests.contains("nodejs.org/dist/index.json"),
                    "{requests}"
                );
                assert!(
                    requests.contains("registry.npmjs.org/ntn/latest"),
                    "{requests}"
                );
                let local = paths.home.join(".local/bin");
                assert_eq!(
                    provider_node(&paths, &flag).unwrap(),
                    Some(local.join("node"))
                );
                let (npm, _, _) = npm_runtime(&paths, &flag, true).unwrap().unwrap();
                if kind == "old-npm" {
                    assert_eq!(npm, local.join("npm"));
                    assert!(!paths.cwd.join("old-npm-packed").exists());
                }
                drop(tx);
                assert!(!local.join("node").exists());
                assert_eq!(fs::read(&original).unwrap(), before);
                return;
            }
            if kind.starts_with("existing-") {
                assert_eq!(
                    result.unwrap_err().1,
                    "verified Node distribution did not provide working npm"
                );
                drop(tx);
                assert_eq!(fs::read(&original).unwrap(), before);
                assert_eq!(fs::read(&bundled_npm).ok(), npm_before);
                for command in ["node", "npm", "npx", "corepack"] {
                    let shim = paths.home.join(".local/bin").join(command);
                    assert!(!r::exists(&shim), "{kind} left a {command} shim");
                    assert!(!r::exists(&ownership_marker(&shim)));
                }
                return;
            }
            result.unwrap();
            let reused = matches!(kind.as_str(), "working" | "separate");
            assert_eq!(paths.cwd.join("requests").exists(), !reused, "{kind}");
            assert_eq!(fs::read(&original).unwrap(), before, "{kind} replaced Node");
            assert_eq!(installed(&paths, "node").unwrap(), original);
            let npm = call(
                &paths,
                "npm",
                &["--version".into()],
                &AtomicBool::new(false),
                false,
            )
            .unwrap();
            assert_eq!(npm, if reused { b"11.1.0\n" } else { b"11.2.0\n" });
            return;
        }
        let scratch = r::Scratch::new().unwrap();
        let paths = fixture_paths(&scratch.0);
        let name = "oms-supply-fixture-command";
        let executable = paths.home.join(".local/bin").join(name);
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        r::new_file(&executable, b"fixture only\n", 0o600).unwrap();
        assert!(installed(&paths, name).is_none());
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(installed(&paths, name), Some(executable));

        let distribution = scratch.0.join("distribution");
        fs::create_dir_all(distribution.join("bin")).unwrap();
        r::new_file(
            &distribution.join("bin/node"),
            b"#!/bin/sh\necho v26.1.0\n",
            0o755,
        )
        .unwrap();
        r::new_file(
            &distribution.join("bin/npm"),
            b"#!/bin/sh\nnode --version >/dev/null || exit 99\necho 11.2.0\n",
            0o755,
        )
        .unwrap();
        let archive = scratch.0.join("node.tar.gz");
        assert!(
            std::process::Command::new(r::find("tar", &scratch.0).unwrap())
                .args(["--format=ustar", "-czf"])
                .arg(&archive)
                .arg("-C")
                .arg(&scratch.0)
                .arg("distribution")
                .env("COPYFILE_DISABLE", "1")
                .status()
                .unwrap()
                .success()
        );
        let archive_digest = r::hash(&fs::read(&archive).unwrap());
        let host = platform().unwrap().replace("amd64", "x64");
        let archive_name = format!("node-v26.1.0-{host}.tar.gz");
        for kind in [
            "missing",
            "broken",
            "working",
            "separate",
            "newer",
            "managed-newer",
            "old-npm",
            "old-node",
            "self-contained",
            "existing-missing",
            "existing-broken",
        ] {
            let root = scratch.0.join(kind);
            let paths = fixture_paths(&root);
            let bin = root.join("bin");
            let elsewhere = root.join("elsewhere");
            fs::create_dir_all(&bin).unwrap();
            fs::create_dir_all(&elsewhere).unwrap();
            fs::create_dir_all(paths.home.join(".local/bin")).unwrap();
            let node = if kind == "managed-newer" {
                paths.home.join(".local/bin/node")
            } else if kind.starts_with("existing-") {
                paths.home.join(".nvm/versions/node/v26.1.0/bin/node")
            } else {
                bin.join("node")
            };
            fs::create_dir_all(node.parent().unwrap()).unwrap();
            let node_bytes = if kind == "old-node" {
                b"#!/bin/sh\necho v10.19.0\n".as_slice()
            } else if kind == "old-npm" {
                b"#!/bin/sh\necho v18.20.8\n".as_slice()
            } else if kind.ends_with("newer") {
                b"#!/bin/sh\necho v27.1.0\n".as_slice()
            } else {
                b"#!/bin/sh\necho v26.1.0\n".as_slice()
            };
            if kind != "self-contained" {
                r::new_file(&node, node_bytes, 0o755).unwrap();
            }
            if kind == "managed-newer" {
                r::new_file(
                    &ownership_marker(&node),
                    format!("sha256={}\n", r::hash(node_bytes)).as_bytes(),
                    0o600,
                )
                .unwrap();
            }
            if kind == "existing-broken" {
                r::new_file(&node.with_file_name("npm"), b"#!/bin/sh\nexit 7\n", 0o755).unwrap();
            }
            if !matches!(kind, "missing" | "existing-missing") {
                let npm = if kind == "separate" { &elsewhere } else { &bin }.join("npm");
                r::new_file(
                    &npm,
                    if kind == "self-contained" {
                        b"#!/bin/sh\necho 11.1.0\n"
                    } else if kind == "old-npm" {
                        b"#!/bin/sh\nif [ \"$1\" = --version ]; then echo 6.14.4; else : > old-npm-packed; exit 98; fi\n"
                    } else if matches!(kind, "working" | "separate" | "old-node") {
                        b"#!/bin/sh\nnode --version >/dev/null || exit 99\necho 11.1.0\n"
                    } else {
                        b"#!/bin/sh\nexit 7\n"
                    },
                    0o755,
                )
                .unwrap();
            }
            symlink(r::find("gzip", &scratch.0).unwrap(), bin.join("gzip")).unwrap();
            let curl = format!(
                "#!/bin/sh\ndestination=\nprevious=\nfor argument do\n  [ \"$previous\" != --output ] || destination=$argument\n  previous=$argument\ndone\nprintf '%s\\n' \"$argument\" >> {}\ncase \"$argument\" in\n  */index.json) printf '%s\\n200' '[{{\"version\":\"v26.1.0\",\"lts\":\"fixture\",\"files\":[\"linux-x64\",\"linux-arm64\",\"osx-x64-tar\",\"osx-arm64-tar\"]}}]' ;;\n  */SHASUMS256.txt) printf '{archive_digest}  {archive_name}\\n\\n200' ;;\n  */{archive_name}) /bin/cp {} \"$destination\" ;;\n  */ntn/latest) printf '%s\\n200' {} ;;\n  *) exit 99 ;;\nesac\n",
                r::shell(root.join("requests").as_os_str()).unwrap(),
                r::shell(archive.as_os_str()).unwrap(),
                r::shell(std::ffi::OsStr::new(
                    &npm_fixture("ntn", "0.9.1").to_string()
                ))
                .unwrap(),
            );
            r::new_file(&bin.join("curl"), curl.as_bytes(), 0o755).unwrap();
            let candidates = [
                node.parent().unwrap().to_path_buf(),
                paths.home.join(".local/bin"),
                bin,
                elsewhere,
            ];
            let path = std::env::join_paths(candidates.into_iter().filter(|directory| {
                !matches!(kind, "old-npm" | "old-node")
                    || *directory != paths.home.join(".local/bin")
            }))
            .unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "supply::tests::installed_node_requires_working_npm_and_preserves_newer_runtime", "--nocapture"])
                .env("OMS_SUPPLY_NODE_FIXTURE", &root)
                .env("OMS_SUPPLY_NODE_CASE", kind)
                .env("PATH", path)
                .env_remove("OH_MY_SETTING_TOOL_LOCK")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{kind}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn agy_vendor_handoff_survives_updater_failure_and_unrelated_rollback() {
        let flag = AtomicBool::new(false);
        for updater_exit in [0, 7] {
            for kind in ["legacy", "native", "absent", "stale-legacy", "stale-native"] {
                let scratch = r::Scratch::new().unwrap();
                let paths = fixture_paths(&scratch.0);
                let binary = paths.home.join(".local/bin/agy");
                fs::create_dir_all(binary.parent().unwrap()).unwrap();
                let marker = binary.with_file_name("agy.oh-my-setting-managed");
                let replacement = scratch.0.join("replacement");
                let updated = b"#!/bin/sh\necho 'antigravity 2.0.0'\n";
                r::new_file(&replacement, updated, 0o755).unwrap();
                let initial = format!(
                    "#!/bin/sh\ncase \"${{1:-}}\" in\n--version) echo 'antigravity 1.0.0' ;;\nupdate) /bin/mv {} \"$0\"; exit {updater_exit} ;;\n*) exit 99 ;;\nesac\n",
                    r::shell(replacement.as_os_str()).unwrap()
                );
                r::new_file(&binary, initial.as_bytes(), 0o755).unwrap();
                let stale = kind.starts_with("stale-");
                let digest = if stale {
                    r::hash(b"earlier AGY binary")
                } else {
                    r::hash(initial.as_bytes())
                };
                let raw = match kind {
                    "legacy" | "stale-legacy" => format!("sha256={digest}\n").into_bytes(),
                    "native" | "stale-native" => {
                        r::bytes(&json!({"owner":"oh-my-setting","sha256":digest})).unwrap()
                    }
                    _ => Vec::new(),
                };
                if !raw.is_empty() {
                    r::new_file(&marker, &raw, 0o600).unwrap();
                    assert_eq!(owns(&paths, &binary).is_err(), stale, "{kind}");
                }
                assert!(vendor(&paths, "agy", &binary));
                let unrelated = scratch.0.join("unrelated");
                r::new_file(&unrelated, b"before", 0o600).unwrap();
                let _guard = r::lock(&paths.receipt, &paths.cwd).unwrap();
                let mut tx = Transaction::begin(&paths).unwrap();
                tx.write(&unrelated, b"after", 0o600).unwrap();
                if stale {
                    let exact = publish(&paths, &mut tx, &replacement, &binary, "2.0.0", &flag);
                    assert_eq!(
                        exact.unwrap_err().1,
                        "managed tool changed; replacement refused"
                    );
                    assert_eq!(fs::read(&marker).unwrap(), raw);
                    assert_eq!(fs::read(&binary).unwrap(), initial.as_bytes());
                }
                let result = refresh_vendor(&paths, "agy", &binary, &flag);
                assert_eq!(result.is_ok(), updater_exit == 0, "{kind}: {result:?}");
                assert_eq!(r::read(&binary, MAX).unwrap(), updated);
                assert!(!r::exists(&marker), "{kind} marker survived vendor handoff");
                drop(tx);
                assert_eq!(r::read(&unrelated, 64).unwrap(), b"before");
                assert_eq!(r::read(&binary, MAX).unwrap(), updated);
                assert!(
                    !r::exists(&marker),
                    "rollback restored a stale {kind} marker"
                );
                assert!(
                    owns(&paths, &binary).is_err(),
                    "explicit replacement must still require OMS ownership"
                );
            }
        }
    }

    #[test]
    fn agy_vendor_handoff_rejects_changed_or_unsafe_markers_before_execution() {
        use std::os::unix::fs::MetadataExt;

        for kind in [
            "malformed-stale",
            "noncanonical-stale",
            "foreign",
            "symlink",
            "hardlink",
            "writable",
            "unrecognized",
            "cancelled",
            "probe-fails",
            "probe-fails-stale",
            "probe-unparseable",
            "probe-mutates",
            "probe-mutates-stale",
            "probe-removes-owned-fixture-marker",
            "probe-replaces-marker",
            "probe-creates-marker",
            "probe-replaces-parent",
            "probe-symlinks-parent",
            "probe-swaps-parent-back",
            "probe-replaces-binary",
            "probe-symlinks-binary",
        ] {
            let scratch = r::Scratch::new().unwrap();
            let paths = fixture_paths(&scratch.0);
            let binary = paths.home.join(if kind == "unrecognized" {
                ".local/bin/other-agy"
            } else {
                ".local/bin/agy"
            });
            let parent = binary.parent().unwrap();
            fs::create_dir_all(parent).unwrap();
            let invoked = scratch.0.join("invoked");
            let marker = ownership_marker(&binary);
            let marker_name = marker.file_name().unwrap();
            let updated = scratch.0.join("updated");
            let staged = scratch.0.join("staged-marker");
            let staged_binary = scratch.0.join("staged-binary");
            let backup = scratch.0.join("bin.old");
            let probe = match kind {
                "probe-unparseable" => "echo unavailable".into(),
                "probe-mutates" | "probe-mutates-stale" => format!(
                    "printf foreign > {}; echo 'antigravity 1.0.0'",
                    r::shell(marker.as_os_str()).unwrap()
                ),
                "probe-removes-owned-fixture-marker" => format!(
                    "/bin/rm -f {}; echo 'antigravity 1.0.0'",
                    r::shell(marker.as_os_str()).unwrap()
                ),
                "probe-replaces-marker" | "probe-creates-marker" => format!(
                    "/bin/mv {} {}; echo 'antigravity 1.0.0'",
                    r::shell(staged.as_os_str()).unwrap(),
                    r::shell(marker.as_os_str()).unwrap()
                ),
                "probe-replaces-parent" => format!(
                    "/bin/mv {} {} && /bin/mkdir -p {} && /bin/cp -p {} {} && /bin/cp -p {} {}; echo 'antigravity 1.0.0'",
                    r::shell(parent.as_os_str()).unwrap(),
                    r::shell(backup.as_os_str()).unwrap(),
                    r::shell(parent.as_os_str()).unwrap(),
                    r::shell(backup.join("agy").as_os_str()).unwrap(),
                    r::shell(binary.as_os_str()).unwrap(),
                    r::shell(backup.join(marker_name).as_os_str()).unwrap(),
                    r::shell(marker.as_os_str()).unwrap(),
                ),
                "probe-symlinks-parent" => format!(
                    "/bin/mv {} {} && /bin/ln -s {} {}; echo 'antigravity 1.0.0'",
                    r::shell(parent.as_os_str()).unwrap(),
                    r::shell(backup.as_os_str()).unwrap(),
                    r::shell(backup.as_os_str()).unwrap(),
                    r::shell(parent.as_os_str()).unwrap(),
                ),
                "probe-swaps-parent-back" => format!(
                    "/bin/mv {} {} && /bin/mv {} {} && /bin/mv {} {}; echo 'antigravity 1.0.0'",
                    r::shell(parent.as_os_str()).unwrap(),
                    r::shell(backup.as_os_str()).unwrap(),
                    r::shell(staged.as_os_str()).unwrap(),
                    r::shell(backup.join(marker_name).as_os_str()).unwrap(),
                    r::shell(backup.as_os_str()).unwrap(),
                    r::shell(parent.as_os_str()).unwrap(),
                ),
                "probe-replaces-binary" => format!(
                    "/bin/mv {} {}; echo 'antigravity 1.0.0'",
                    r::shell(staged_binary.as_os_str()).unwrap(),
                    r::shell(binary.as_os_str()).unwrap(),
                ),
                "probe-symlinks-binary" => format!(
                    "/bin/rm -f {} && /bin/ln -s {} {}; echo 'antigravity 1.0.0'",
                    r::shell(binary.as_os_str()).unwrap(),
                    r::shell(staged_binary.as_os_str()).unwrap(),
                    r::shell(binary.as_os_str()).unwrap(),
                ),
                _ => "exit 99".into(),
            };
            let raw = format!(
                "#!/bin/sh\ncase \"${{1:-}}\" in\n--version) printf invoked > {}; {probe} ;;\nupdate) printf updated > {}; exit 99 ;;\nesac\n",
                r::shell(invoked.as_os_str()).unwrap(),
                r::shell(updated.as_os_str()).unwrap(),
            );
            r::new_file(&binary, raw.as_bytes(), 0o755).unwrap();
            if matches!(kind, "probe-replaces-binary" | "probe-symlinks-binary") {
                r::new_file(&staged_binary, raw.as_bytes(), 0o755).unwrap();
            }
            let good = format!("sha256={}\n", r::hash(raw.as_bytes()));
            match kind {
                "malformed-stale" => r::new_file(
                    &marker,
                    format!("sha256={}\n", "g".repeat(64)).as_bytes(),
                    0o600,
                )
                .unwrap(),
                "noncanonical-stale" => r::new_file(
                    &marker,
                    &r::bytes(&json!({
                        "owner":"oh-my-setting","sha256":r::hash(b"earlier AGY binary"),
                        "extra":true
                    }))
                    .unwrap(),
                    0o600,
                )
                .unwrap(),
                "probe-fails-stale" | "probe-mutates-stale" => r::new_file(
                    &marker,
                    format!("sha256={}\n", r::hash(b"earlier AGY binary")).as_bytes(),
                    0o600,
                )
                .unwrap(),
                "writable" => {
                    r::new_file(&marker, good.as_bytes(), 0o600).unwrap();
                    fs::set_permissions(&marker, fs::Permissions::from_mode(0o662)).unwrap();
                }
                "foreign" => r::new_file(
                    &marker,
                    &r::bytes(&json!({"owner":"other","sha256":r::hash(raw.as_bytes())})).unwrap(),
                    0o600,
                )
                .unwrap(),
                "symlink" | "hardlink" => {
                    let source = scratch.0.join("shared-marker");
                    r::new_file(&source, good.as_bytes(), 0o600).unwrap();
                    if kind == "symlink" {
                        symlink(&source, &marker).unwrap();
                    } else {
                        fs::hard_link(&source, &marker).unwrap();
                    }
                }
                "probe-creates-marker" => {
                    r::new_file(&staged, good.as_bytes(), 0o600).unwrap();
                }
                "probe-replaces-marker" | "probe-swaps-parent-back" => {
                    r::new_file(&marker, good.as_bytes(), 0o600).unwrap();
                    r::new_file(&staged, good.as_bytes(), 0o600).unwrap();
                }
                _ => r::new_file(&marker, good.as_bytes(), 0o600).unwrap(),
            }
            let parent_before = fs::symlink_metadata(parent).unwrap();
            let binary_before = fs::symlink_metadata(&binary).unwrap();
            let marker_before = fs::symlink_metadata(&marker).ok();
            let before = fs::read(&marker).ok();
            assert!(
                refresh_vendor(
                    &paths,
                    "agy",
                    &binary,
                    &AtomicBool::new(kind == "cancelled")
                )
                .is_err(),
                "{kind}"
            );
            assert_eq!(invoked.exists(), kind.starts_with("probe-"), "{kind}");
            assert!(!updated.exists(), "{kind} reached the updater");
            match kind {
                "probe-removes-owned-fixture-marker" => {
                    assert!(!r::exists(&marker), "{kind} restored marker");
                }
                "probe-mutates" | "probe-mutates-stale" => {
                    assert_eq!(fs::read(&marker).unwrap(), b"foreign", "{kind}");
                }
                "probe-creates-marker" => {
                    assert_eq!(fs::read(&marker).unwrap(), good.as_bytes(), "{kind}");
                }
                "probe-replaces-marker" | "probe-swaps-parent-back" => {
                    assert_eq!(fs::read(&marker).unwrap(), good.as_bytes(), "{kind}");
                    assert_ne!(
                        fs::symlink_metadata(&marker).unwrap().ino(),
                        marker_before.unwrap().ino(),
                        "{kind}"
                    );
                }
                "probe-replaces-parent" => {
                    assert_eq!(fs::read(&marker).unwrap(), good.as_bytes(), "{kind}");
                    assert_ne!(
                        fs::symlink_metadata(parent).unwrap().ino(),
                        parent_before.ino(),
                        "{kind}"
                    );
                }
                "probe-symlinks-parent" => {
                    assert!(
                        fs::symlink_metadata(parent)
                            .unwrap()
                            .file_type()
                            .is_symlink(),
                        "{kind}"
                    );
                    assert_eq!(fs::read(&marker).unwrap(), good.as_bytes(), "{kind}");
                }
                _ => {
                    assert_eq!(fs::read(&marker).ok(), before, "{kind}");
                    let marker_after = fs::symlink_metadata(&marker).unwrap();
                    let marker_before = marker_before.unwrap();
                    assert_eq!(
                        (marker_after.dev(), marker_after.ino()),
                        (marker_before.dev(), marker_before.ino()),
                        "{kind}"
                    );
                }
            }
            match kind {
                "probe-replaces-binary" => {
                    assert_ne!(
                        fs::symlink_metadata(&binary).unwrap().ino(),
                        binary_before.ino(),
                        "{kind}"
                    );
                    assert_eq!(r::read(&binary, MAX).unwrap(), raw.as_bytes(), "{kind}");
                }
                "probe-symlinks-binary" => {
                    assert!(
                        fs::symlink_metadata(&binary)
                            .unwrap()
                            .file_type()
                            .is_symlink(),
                        "{kind}"
                    );
                    assert_eq!(fs::read(&binary).unwrap(), raw.as_bytes(), "{kind}");
                }
                _ => assert_eq!(r::read(&binary, MAX).unwrap(), raw.as_bytes(), "{kind}"),
            }
        }
    }

    #[test]
    fn agy_install_and_non_retiring_custody() {
        use std::os::unix::fs::MetadataExt;

        if let Some(root) = std::env::var_os("OMS_SUPPLY_AGY_FIXTURE") {
            let paths = fixture_paths(Path::new(&root));
            let kind = std::env::var("OMS_SUPPLY_AGY_CASE").unwrap();
            let _guard = r::lock(&paths.receipt, &paths.cwd).unwrap();
            let mut tx = Transaction::begin(&paths).unwrap();
            let result = install(
                &paths,
                &["agy".into()],
                true,
                false,
                &mut tx,
                &AtomicBool::new(false),
            );
            assert_eq!(
                result.is_ok(),
                matches!(kind.as_str(), "stale" | "valid"),
                "{kind}: {result:?}"
            );
            return;
        }

        for kind in ["valid", "stale", "absent"] {
            let present = kind != "absent";
            let scratch = r::Scratch::new().unwrap();
            let paths = fixture_paths(&scratch.0);
            let binary = paths.home.join(".local/bin/agy");
            fs::create_dir_all(binary.parent().unwrap()).unwrap();
            let raw = b"#!/bin/sh\necho 'antigravity 1.0.0'\n";
            r::new_file(&binary, raw, 0o755).unwrap();
            let marker = ownership_marker(&binary);
            let digest = r::hash(if kind == "stale" {
                b"earlier AGY binary"
            } else {
                raw
            });
            let good = format!("sha256={digest}\n");
            if present {
                r::new_file(&marker, good.as_bytes(), 0o600).unwrap();
            }
            let custody = AgyMarkerCustody::capture(&paths, &binary).unwrap();
            let probe = r::command(
                &binary,
                &["--version".into()],
                &paths.cwd,
                None,
                &AtomicBool::new(false),
                Duration::from_secs(10),
            )
            .unwrap();
            assert!(satisfies_release(&probe.1, "1.0.0", true));
            let before = fs::symlink_metadata(&marker).ok();
            custody.verify_continuity(&paths, &binary, false).unwrap();
            assert_eq!(r::exists(&marker), present);
            if let Some(before) = before {
                assert_eq!(fs::read(&marker).unwrap(), good.as_bytes());
                assert_eq!(fs::symlink_metadata(&marker).unwrap().ino(), before.ino());
            }
            if present {
                let staged = scratch.0.join("staged");
                r::new_file(&staged, good.as_bytes(), 0o600).unwrap();
                fs::rename(&staged, &marker).unwrap();
                assert!(custody.verify_continuity(&paths, &binary, false).is_err());
                assert_eq!(fs::read(&marker).unwrap(), good.as_bytes());
            } else {
                r::new_file(&marker, good.as_bytes(), 0o600).unwrap();
                assert!(custody.verify_continuity(&paths, &binary, false).is_err());
                assert_eq!(fs::read(&marker).unwrap(), good.as_bytes());
            }
        }

        for kind in ["stale", "malformed", "probe-removes-marker", "valid"] {
            let scratch = r::Scratch::new().unwrap();
            let paths = fixture_paths(&scratch.0);
            let binary = paths.home.join(".local/bin/agy");
            fs::create_dir_all(binary.parent().unwrap()).unwrap();
            let invoked = scratch.0.join("invoked");
            let updated = scratch.0.join("updated");
            let replacement = scratch.0.join("replacement");
            let updated_bytes = b"#!/bin/sh\necho 'antigravity 2.0.0'\n";
            r::new_file(&replacement, updated_bytes, 0o755).unwrap();
            let marker = ownership_marker(&binary);
            let probe = if kind == "probe-removes-marker" {
                format!(
                    "/bin/rm -f {}; echo 'antigravity 1.0.0'",
                    r::shell(marker.as_os_str()).unwrap()
                )
            } else {
                "echo 'antigravity 1.0.0'".into()
            };
            let raw = format!(
                "#!/bin/sh\ncase \"${{1:-}}\" in\n--version) printf invoked > {}; {probe} ;;\nupdate) printf updated > {}; /bin/mv {} \"$0\" ;;\n*) exit 99 ;;\nesac\n",
                r::shell(invoked.as_os_str()).unwrap(),
                r::shell(updated.as_os_str()).unwrap(),
                r::shell(replacement.as_os_str()).unwrap(),
            );
            r::new_file(&binary, raw.as_bytes(), 0o755).unwrap();
            let digest = match kind {
                "stale" => r::hash(b"earlier AGY binary"),
                "malformed" => "g".repeat(64),
                _ => r::hash(raw.as_bytes()),
            };
            let marker_bytes = format!("sha256={digest}\n");
            r::new_file(&marker, marker_bytes.as_bytes(), 0o600).unwrap();
            let marker_before = fs::symlink_metadata(&marker).unwrap();

            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "supply::tests::agy_install_and_non_retiring_custody",
                    "--nocapture",
                ])
                .env("OMS_SUPPLY_AGY_FIXTURE", &scratch.0)
                .env("OMS_SUPPLY_AGY_CASE", kind)
                .env("PATH", "")
                .env_remove("OH_MY_SETTING_TOOL_LOCK")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{kind}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );

            assert_eq!(invoked.exists(), kind != "malformed", "{kind}");
            assert_eq!(
                updated.exists(),
                matches!(kind, "stale" | "valid"),
                "{kind}"
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("retired stale AGY ownership marker"),
                kind == "stale",
                "{kind}"
            );
            match kind {
                "malformed" => {
                    assert_eq!(fs::read(&marker).unwrap(), marker_bytes.as_bytes());
                    assert_eq!(
                        fs::symlink_metadata(&marker).unwrap().ino(),
                        marker_before.ino()
                    );
                    assert_eq!(r::read(&binary, MAX).unwrap(), raw.as_bytes());
                }
                "probe-removes-marker" => {
                    assert!(!r::exists(&marker));
                    assert_eq!(r::read(&binary, MAX).unwrap(), raw.as_bytes());
                }
                _ => {
                    assert!(!r::exists(&marker));
                    assert_eq!(r::read(&binary, MAX).unwrap(), updated_bytes);
                }
            }
        }
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
