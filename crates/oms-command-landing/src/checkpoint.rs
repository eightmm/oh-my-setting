//! Local staged/unstaged snapshots with a mandatory recovery copy before any
//! tracked restore. Untracked extraction creates only absent regular files.
use crate::{
    Context, Error, Output, Result, V, cli::Options, digest, git::Git, now, obj, require, storage,
    string, worktree::Sandbox,
};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path},
};
const ROOT: &str = ".oms/checkpoints";
fn id_valid(s: &str) -> bool {
    s.starts_with("cp-")
        && s.len() > 3
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn patch(git: &Git<'_>, cached: bool) -> Result<Vec<u8>> {
    let mut args = vec![
        "-c",
        "diff.noprefix=false",
        "diff",
        "--no-color",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--binary",
        "--full-index",
        "--no-ext-diff",
        "--no-textconv",
    ];
    if cached {
        args.extend(["--cached", "HEAD"]);
    }
    args.push("--");
    git.run(&args)
}
fn metadata(repo: &Path, id: &str) -> Result<(V, Vec<u8>, Vec<u8>)> {
    require(id_valid(id), "invalid checkpoint id")?;
    let dir = Path::new(ROOT).join(id);
    let row = V::parse(&storage::read(repo, &dir.join("meta.json"), false)?)?;
    require(
        row.get("schema") == &1usize.into() && row.get("id").text() == Some(id),
        "checkpoint metadata identity mismatch",
    )?;
    let head = string(&row, "head");
    require(
        crate::hex(&head, 40) || crate::hex(&head, 64),
        "invalid saved HEAD",
    )?;
    let index = storage::read(repo, &dir.join("index.patch"), false)?;
    let work = storage::read(repo, &dir.join("worktree.patch"), false)?;
    require(
        digest(&index) == string(&row, "index_sha256")
            && digest(&work) == string(&row, "worktree_sha256"),
        "checkpoint checksum mismatch",
    )?;
    let archive = storage::read(repo, &dir.join("untracked.tar"), true)?;
    if row.get("untracked_sha256").truth() || !archive.is_empty() {
        require(
            digest(&archive) == string(&row, "untracked_sha256"),
            "untracked checksum mismatch",
        )?;
    }
    Ok((row, index, work))
}
fn names(repo: &Path) -> Result<Vec<String>> {
    let dir = repo.join(ROOT);
    let mut names = vec![];
    if dir.exists() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if id_valid(&name) && e.file_type()?.is_dir() {
                names.push(name);
            }
        }
    }
    names.sort();
    names.reverse();
    Ok(names)
}
pub(crate) fn create(
    repo: &Path,
    label: &str,
    untracked: bool,
    reuse: bool,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<(String, V, bool)> {
    storage::ensure_ignore(repo, interrupted)?;
    let git = Git::new(repo, interrupted);
    git.safe()?;
    let head = git.commit("HEAD")?;
    let before = git.snapshot()?;
    let index = patch(&git, true)?;
    let work = patch(&git, false)?;
    let count = git
        .run(&["ls-files", "--others", "--exclude-standard", "-z"])?
        .split(|b| *b == 0)
        .filter(|v| !v.is_empty())
        .count();
    let (archive, capture) = if untracked {
        let (a, c) = capture(&git)?;
        (Some(a), c)
    } else {
        (None, V::Null)
    };
    require(
        git.snapshot()? == before,
        "tracked state changed during checkpoint capture",
    )?;
    if reuse {
        for name in names(repo)? {
            if let Ok((row, _, _)) = metadata(repo, &name)
                && string(&row, "label") == label
                && string(&row, "head") == head
                && string(&row, "index_sha256") == digest(&index)
                && string(&row, "worktree_sha256") == digest(&work)
                && row.get("untracked_fingerprint") == capture.get("untracked_fingerprint")
            {
                return Ok((name, row, true));
            }
        }
    }
    let id = format!(
        "cp-{}-{}-{}",
        now().replace(['-', ':'], ""),
        std::process::id(),
        oms_command_plan::storage::random_hex(4)?
    );
    let dir = Path::new(ROOT).join(&id);
    let mut row = obj([
        ("schema", 1usize.into()),
        ("id", id.clone().into()),
        ("created_at", now().into()),
        ("head", head.into()),
        ("label", label.into()),
        ("index_sha256", digest(&index).into()),
        ("worktree_sha256", digest(&work).into()),
        ("staged_bytes", index.len().into()),
        ("unstaged_bytes", work.len().into()),
        ("untracked_count", count.into()),
    ]);
    if let V::Obj(fields) = capture {
        for (k, v) in fields {
            row.set(
                std::str::from_utf8(&k).map_err(|_| Error::new(2, "invalid capture field"))?,
                v,
            );
        }
    }
    storage::replace(repo, &dir.join("index.patch"), None, &index)?;
    storage::replace(repo, &dir.join("worktree.patch"), None, &work)?;
    if let Some(a) = archive {
        storage::replace(repo, &dir.join("untracked.tar"), None, &a)?;
    }
    let mut bytes = row.json(false)?;
    bytes.push(b'\n');
    storage::replace(repo, &dir.join("meta.json"), None, &bytes)?;
    Ok((id, row, false))
}
fn verify(repo: &Path, id: &str, interrupted: &std::sync::atomic::AtomicBool) -> Result<V> {
    let (row, index, work) = metadata(repo, id)?;
    let scratch = Sandbox::new(repo, &string(&row, "head"), interrupted)?;
    let git = Git::new(&scratch.path, interrupted);
    if !index.is_empty() {
        git.apply(&index, false, false, true)?;
    }
    if !work.is_empty() {
        git.apply(&work, false, false, false)?;
    }
    require(
        patch(&git, true)? == index && patch(&git, false)? == work,
        "checkpoint patches cannot reproduce their saved tracked state",
    )?;
    Ok(row)
}
fn replace(
    repo: &Path,
    index: &[u8],
    work: &[u8],
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let git = Git::new(repo, interrupted);
    let current_index = patch(&git, true)?;
    let current_work = patch(&git, false)?;
    if !current_work.is_empty() {
        git.apply(&current_work, false, true, false)?;
    }
    if !current_index.is_empty() || !index.is_empty() {
        git.run(&["update-index", "--refresh"])?;
    }
    if !current_index.is_empty() {
        git.apply(&current_index, false, true, true)?;
    }
    if !index.is_empty() {
        git.apply(index, false, false, true)?;
    }
    if !work.is_empty() {
        git.apply(work, false, false, false)?;
    }
    require(
        patch(&git, true)? == index && patch(&git, false)? == work,
        "restored tracked state does not match checkpoint",
    )
}
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    let repo = c.repo(o)?;
    let action = o.action("");
    require(
        ["create", "list", "verify", "restore"].contains(&action),
        "checkpoint requires create, list, verify or restore",
    )?;
    require(
        !o.has("untracked") || action == "create",
        "--untracked applies to create",
    )?;
    require(
        !o.has("reuse") || action == "create",
        "--reuse applies to create",
    )?;
    require(
        !o.has("untracked-only") || action == "restore",
        "--untracked-only applies to restore",
    )?;
    require(
        !o.has("apply") || action == "restore",
        "--apply applies to restore",
    )?;
    let _guard = storage::lock(&repo.join(ROOT).join("state"), c.interrupted)?;
    if action == "list" {
        require(o.words.len() == 1, "list takes no id")?;
        let mut out = String::new();
        for id in names(&repo)? {
            if let Ok((row, _, _)) = metadata(&repo, &id) {
                if o.has("json") {
                    out += &String::from_utf8(row.json(false)?)
                        .map_err(|_| Error::new(2, "invalid checkpoint metadata"))?;
                    out.push('\n');
                } else {
                    out += &format!(
                        "{id}\t{}\t{}\t{}\n",
                        string(&row, "created_at"),
                        string(&row, "head"),
                        string(&row, "label")
                    );
                }
            }
        }
        return Ok(Output::text(out));
    }
    if action == "create" {
        require(o.words.len() == 1, "create takes no id")?;
        let (id, row, reused) = create(
            &repo,
            o.get("label"),
            o.has("untracked"),
            o.has("reuse"),
            c.interrupted,
        )?;
        return if o.has("json") {
            Output::json(&row)
        } else {
            Ok(Output::text(format!(
                "checkpoint: {} {id}\n",
                if reused { "reused" } else { "created" }
            )))
        };
    }
    require(
        o.words.len() == 2,
        "checkpoint verify/restore requires one id",
    )?;
    let id = &o.words[1];
    let (row, index, work) = metadata(&repo, id)?;
    if action == "verify" {
        verify(&repo, id, c.interrupted)?;
        return if o.has("json") {
            Output::json(&obj([
                ("schema", 1usize.into()),
                ("id", id.clone().into()),
                ("verified", true.into()),
            ]))
        } else {
            Ok(Output::text(format!("checkpoint: verified {id}\n")))
        };
    }
    let archive = storage::read(&repo, &Path::new(ROOT).join(id).join("untracked.tar"), true)?;
    if o.has("untracked-only") {
        require(
            !archive.is_empty(),
            "checkpoint captured no untracked files (create it with --untracked)",
        )?;
        let result = restore_untracked(&repo, &archive, o.has("apply"))?;
        return Output::json(&obj([
            ("schema", 1usize.into()),
            ("id", id.clone().into()),
            ("untracked_only", true.into()),
            ("untracked", result),
        ]));
    }
    let git = Git::new(&repo, c.interrupted);
    git.safe()?;
    require(
        git.commit("HEAD")? == string(&row, "head"),
        "checkpoint belongs to a different HEAD",
    )?;
    verify(&repo, id, c.interrupted)?;
    let mut result = obj([("schema", 1usize.into()), ("id", id.clone().into())]);
    if !o.has("apply") {
        result.set("dry_run", true);
        result.set("applicable", true);
        if !archive.is_empty() {
            result.set("untracked", restore_untracked(&repo, &archive, false)?);
        }
        return if o.has("json") {
            Output::json(&result)
        } else {
            Ok(Output::text(format!(
                "checkpoint: dry-run ok for {id} (pass --apply to restore)\n"
            )))
        };
    }
    let before = git.snapshot()?;
    let (backup, _, _) = create(
        &repo,
        &format!("auto-backup-before-{id}"),
        false,
        false,
        c.interrupted,
    )?;
    require(
        git.snapshot()? == before,
        "tracked state changed after recovery checkpoint",
    )?;
    if let Err(e) = replace(&repo, &index, &work, c.interrupted) {
        let (_, saved_index, saved_work) = metadata(&repo, &backup)?;
        let recovered = replace(&repo, &saved_index, &saved_work, c.interrupted).is_ok();
        return Err(Error::new(
            1,
            format!(
                "restore failed: {e}; original tracked state {} from {backup}",
                if recovered {
                    "recovered"
                } else {
                    "requires manual recovery"
                }
            ),
        ));
    }
    result.set("applied", true);
    result.set("backup_id", backup.clone());
    if !archive.is_empty() {
        result.set("untracked", restore_untracked(&repo, &archive, true)?);
    }
    if o.has("json") {
        Output::json(&result)
    } else {
        Ok(Output::text(format!(
            "checkpoint: restored {id}\ncheckpoint: recovery backup {backup}\n"
        )))
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\r', '\n', '\0'])
        && Path::new(name)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
        && !name.starts_with('/')
        && !name.split('/').any(|p| matches!(p, "" | "." | ".."))
        && !matches!(name.split('/').next(), Some(".git" | ".oms"))
}
fn octal(dst: &mut [u8], number: u64) -> Result<()> {
    let value = format!("{number:0width$o}\0", width = dst.len() - 1);
    require(value.len() == dst.len(), "tar numeric field overflow")?;
    dst.copy_from_slice(value.as_bytes());
    Ok(())
}
fn member(
    out: &mut Vec<u8>,
    name: &str,
    data: &[u8],
    mode: u32,
    mtime: u64,
    kind: u8,
) -> Result<()> {
    require(name.len() <= 100, "tar name exceeds header limit")?;
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], u64::from(mode & 0o777))?;
    octal(&mut h[108..116], 0)?;
    octal(&mut h[116..124], 0)?;
    octal(&mut h[124..136], data.len() as u64)?;
    octal(&mut h[136..148], mtime)?;
    h[148..156].fill(b' ');
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    out.extend(h);
    out.extend(data);
    out.resize(out.len().next_multiple_of(512), 0);
    Ok(())
}
fn pax(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let mut n = body.len() + 1;
    loop {
        let next = n.to_string().len() + body.len();
        if n == next {
            return format!("{n}{body}");
        }
        n = next;
    }
}
fn capture(git: &Git<'_>) -> Result<(Vec<u8>, V)> {
    let mut names: Vec<_> = git
        .run(&["ls-files", "--others", "--exclude-standard", "-z", "--"])?
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| {
            String::from_utf8(b.to_vec()).map_err(|_| Error::new(2, "untracked path is not UTF-8"))
        })
        .collect::<Result<_>>()?;
    names.sort();
    let mut out = vec![];
    let mut fingerprint = String::new();
    let (mut total, mut captured, mut omitted) = (0, 0, 0);
    for name in names {
        if !safe_name(&name) {
            continue;
        }
        let meta = match std::fs::symlink_metadata(git.repo.join(&name)) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let data = if meta.is_file()
            && meta.len() <= 8 * 1024 * 1024
            && total + meta.len() <= 64 * 1024 * 1024
            && captured < 5000
        {
            storage::read(git.repo, Path::new(&name), false)
                .ok()
                .filter(|v| {
                    v.len() <= 8 * 1024 * 1024 && total + v.len() as u64 <= 64 * 1024 * 1024
                })
        } else {
            None
        };
        let Some(data) = data else {
            omitted += 1;
            fingerprint += &format!("{name}\0omitted\n");
            continue;
        };
        if name.len() > 100 || !name.is_ascii() {
            member(
                &mut out,
                "././@PaxHeader",
                pax("path", &name).as_bytes(),
                0o600,
                0,
                b'x',
            )?;
        }
        member(
            &mut out,
            if name.len() > 100 { "./file" } else { &name },
            &data,
            meta.mode(),
            meta.mtime().max(0) as u64,
            b'0',
        )?;
        total += data.len() as u64;
        captured += 1;
        fingerprint += &format!("{name}\0{}\n", digest(&data));
    }
    out.extend([0; 1024]);
    let hash = digest(&out);
    Ok((
        out,
        obj([
            ("untracked_captured", V::Num(captured.to_string())),
            ("untracked_omitted", V::Num(omitted.to_string())),
            ("untracked_capture_bytes", V::Num(total.to_string())),
            ("untracked_sha256", hash.into()),
            (
                "untracked_fingerprint",
                digest(fingerprint.as_bytes()).into(),
            ),
        ]),
    ))
}
fn number(raw: &[u8]) -> Result<u64> {
    let s = std::str::from_utf8(raw)
        .map_err(|_| Error::new(1, "invalid tar number"))?
        .trim_matches(['\0', ' ']);
    u64::from_str_radix(if s.is_empty() { "0" } else { s }, 8)
        .map_err(|_| Error::new(1, "invalid tar number"))
}
pub fn restore_untracked(repo: &Path, archive: &[u8], apply: bool) -> Result<V> {
    require(
        archive.len() >= 1024 && archive.len().is_multiple_of(512),
        "truncated or malformed untracked archive",
    )?;
    let (mut offset, mut missing, mut present, mut written, mut refused) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut pax_path = None;
    let mut members = vec![];
    while offset.checked_add(512).is_some_and(|n| n <= archive.len()) {
        let h = &archive[offset..offset + 512];
        offset += 512;
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let expected = number(&h[148..156])?;
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(*b)
                }
            })
            .sum();
        require(expected == sum, "tar header checksum mismatch")?;
        let size = usize::try_from(number(&h[124..136])?)
            .map_err(|_| Error::new(1, "tar member too large"))?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| Error::new(1, "tar member size overflow"))?;
        require(end <= archive.len(), "truncated tar member")?;
        let data = &archive[offset..end];
        offset = end
            .checked_add(511)
            .ok_or_else(|| Error::new(1, "tar member size overflow"))?
            / 512
            * 512;
        require(offset <= archive.len(), "truncated tar member padding")?;
        if h[156] == b'x' {
            require(size <= 64 * 1024, "PAX header too large")?;
            let mut p = 0;
            while p < data.len() {
                let space = data[p..]
                    .iter()
                    .position(|b| *b == b' ')
                    .ok_or_else(|| Error::new(1, "invalid PAX field"))?
                    + p;
                let length = std::str::from_utf8(&data[p..space])
                    .map_err(|_| Error::new(1, "invalid PAX length"))?
                    .parse::<usize>()
                    .map_err(|_| Error::new(1, "invalid PAX length"))?;
                require(
                    length > space - p + 1
                        && p.checked_add(length).is_some_and(|n| n <= data.len()),
                    "invalid PAX field size",
                )?;
                let field = std::str::from_utf8(&data[space + 1..p + length - 1])
                    .map_err(|_| Error::new(1, "invalid PAX text"))?;
                if let Some(v) = field.strip_prefix("path=") {
                    pax_path = Some(v.to_owned());
                }
                p += length;
            }
            continue;
        }
        let name = String::from_utf8(
            h[..100]
                .split(|b| *b == 0)
                .next()
                .unwrap_or_default()
                .to_vec(),
        )
        .map_err(|_| Error::new(1, "invalid tar path"))?;
        let prefix = std::str::from_utf8(h[345..500].split(|b| *b == 0).next().unwrap_or_default())
            .map_err(|_| Error::new(1, "invalid tar prefix"))?;
        let name = pax_path.take().unwrap_or_else(|| {
            if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            }
        });
        if ![0, b'0'].contains(&h[156]) || !safe_name(&name) || size > 8 * 1024 * 1024 {
            refused += 1;
            continue;
        }
        members.push((
            name,
            data,
            number(&h[100..108])? as u32,
            number(&h[136..148])?,
        ));
        require(members.len() <= 5000, "too many untracked archive members")?;
    }
    require(
        members.iter().map(|(_, b, _, _)| b.len()).sum::<usize>() <= 64 * 1024 * 1024,
        "untracked archive exceeds total byte limit",
    )?;
    for (name, data, mode, mtime) in members {
        let path = repo.join(&name);
        if std::fs::symlink_metadata(&path).is_ok() {
            present += 1;
            continue;
        }
        if storage::read(repo, Path::new(&name), true).is_err() {
            refused += 1;
            continue;
        }
        missing += 1;
        if !apply {
            continue;
        }
        let file = match storage::create_exclusive(repo, Path::new(&name), data, 0o600, true) {
            Ok(file) => file,
            Err(_) => {
                refused += 1;
                continue;
            }
        };
        file.set_permissions(std::fs::Permissions::from_mode(if mode & 0o777 == 0 {
            0o600
        } else {
            mode & 0o777
        }))?;
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime);
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(modified)
                .set_accessed(modified),
        )?;
        written += 1;
    }
    Ok(obj([
        ("missing", missing.into()),
        ("present", present.into()),
        ("written", written.into()),
        ("refused", refused.into()),
        ("applied", apply.into()),
    ]))
}
