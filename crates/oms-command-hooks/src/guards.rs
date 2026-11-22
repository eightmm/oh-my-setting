use crate::{Output, common::*, shell};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

const GIT_VALUES: &[&str] = &[
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--config-env",
];
#[derive(Clone)]
struct Verdict {
    rule: &'static str,
    reason: String,
    instead: &'static str,
    name: String,
    base: Option<PathBuf>,
}
impl Verdict {
    fn new(rule: &'static str, reason: impl Into<String>, instead: &'static str) -> Self {
        Self {
            rule,
            reason: reason.into(),
            instead,
            name: String::new(),
            base: None,
        }
    }
}
fn flags(args: &[String]) -> (BTreeSet<char>, BTreeSet<&str>) {
    let mut letters = BTreeSet::new();
    let mut longs = BTreeSet::new();
    for arg in args {
        if arg == "--" {
            break;
        }
        if arg.starts_with("--") {
            longs.insert(arg.split('=').next().unwrap());
        } else if arg.starts_with('-') {
            letters.extend(arg.chars().skip(1));
        }
    }
    (letters, longs)
}
fn operands(args: &[String]) -> Vec<&str> {
    let mut plain = false;
    args.iter()
        .filter_map(|s| {
            if plain || !s.starts_with('-') || s == "-" {
                Some(s.as_str())
            } else {
                if s == "--" {
                    plain = true;
                }
                None
            }
        })
        .collect()
}
fn literal(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| "$`*?[\n".contains(c))
}
fn resolve(base: Option<&Path>, target: &str) -> Option<PathBuf> {
    let base = base?;
    let h = home();
    let target = if ["~", "$HOME", "${HOME}"].contains(&target) {
        h.to_string_lossy().into_owned()
    } else if let Some(rest) = target.strip_prefix("~/") {
        h.join(rest).to_string_lossy().into_owned()
    } else {
        target.into()
    };
    (literal(&target) && target != "-").then(|| absolute(base, Path::new(&target)))
}
fn physical(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    let mut rest = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        if let Some(name) = current.file_name() {
            rest.push(name);
        }
        current = parent;
        if let Ok(mut p) = current.canonicalize() {
            for name in rest.into_iter().rev() {
                p.push(name);
            }
            return normalize(&p);
        }
    }
    normalize(path)
}
fn rm_rule(args: &[String], base: Option<&Path>) -> Option<Verdict> {
    let (letters, longs) = flags(args);
    if !(letters.contains(&'r') || letters.contains(&'R') || longs.contains("--recursive"))
        || !(letters.contains(&'f') || longs.contains("--force"))
    {
        return None;
    }
    let home = physical(&home());
    for target in operands(args) {
        let bare = target.trim_end_matches('/');
        let rule = if bare.is_empty() || bare == "/*" {
            Some((
                "rm-root",
                "rm -rf of the filesystem root",
                "name the directory you mean",
            ))
        } else if ["~", "$HOME", "${HOME}", "~/*", "$HOME/*", "${HOME}/*"].contains(&bare) {
            Some((
                "rm-home",
                "rm -rf of the home directory",
                "name the directory you mean",
            ))
        } else if [".", ".."].contains(&bare) {
            Some((
                "rm-cwd",
                "rm -rf of the current or parent directory",
                "name the path explicitly",
            ))
        } else {
            None
        };
        if let Some((rule, why, safer)) = rule {
            return Some(Verdict::new(rule, why, safer));
        }
        let Some(p) = resolve(base, target).map(|p| physical(&p)) else {
            continue;
        };
        if p == Path::new("/") {
            return Some(Verdict::new(
                "rm-root",
                "rm -rf of the filesystem root",
                "name the directory you mean",
            ));
        }
        if p == home {
            return Some(Verdict::new(
                "rm-home",
                "rm -rf of the home directory",
                "name the directory you mean",
            ));
        }
        if !p.is_dir() {
            continue;
        }
        let base = if p.file_name().is_some_and(|n| n == ".git") {
            p.parent().unwrap_or(&p)
        } else {
            &p
        };
        if let Some(top) = root(base, base)
            && top.join(".git").exists()
        {
            if p == top {
                return Some(Verdict::new(
                    "rm-checkout",
                    "rm -rf of the whole checkout",
                    "name the path inside it, or remove a worktree with git worktree remove",
                ));
            }
            if p == top.join(".git") {
                return Some(Verdict::new(
                    "rm-git-dir",
                    "rm -rf of the repository's .git",
                    "nothing recovers that; move it aside with mv if you mean it",
                ));
            }
        }
    }
    None
}
fn git_args(args: &[String], cur: Option<&Path>) -> (usize, Option<PathBuf>) {
    let mut base = cur.map(Path::to_path_buf);
    let mut j = 0;
    while j < args.len() {
        if args[j] == "-C" && j + 1 < args.len() {
            base = resolve(base.as_deref(), &args[j + 1]);
            j += 2;
        } else if GIT_VALUES.contains(&args[j].as_str()) {
            j += 2;
        } else if args[j].starts_with('-') {
            j += 1;
        } else {
            break;
        }
    }
    (j, base)
}
fn git_rule(sub: &str, rest: &[String], base: Option<&Path>, land_active: bool) -> Option<Verdict> {
    let (letters, longs) = flags(rest);
    let ff_only = longs.contains("--ff-only") && !longs.contains("--no-ff");
    if !land_active
        && !ff_only
        && (sub == "pull"
            || sub == "merge"
                && !["--abort", "--quit", "--continue"]
                    .iter()
                    .any(|f| longs.contains(f)))
        && base.is_some_and(on_protected_branch)
    {
        return Some(Verdict::new(
            "git-merge-protected-branch",
            format!("git {sub} would change main/master outside the landing gate"),
            "oms land --pr or oms land --local-merge --from <branch>",
        ));
    }
    // Default-branch work goes through a pull request (`oms land`); only the
    // landing run itself, which pushes with its own exact-head checks, may
    // write main/master directly. A repository with no commit or no remote
    // yet, and the first publish of a default branch, have no PR to target.
    let repo = base.and_then(|b| root(b, b));
    let common = repo.as_deref().and_then(git_directories).map(|(_, c)| c);
    let branch = match repo.as_deref().and_then(head_ref) {
        Some(HeadRef::Branch(name)) => name,
        _ => String::new(),
    };
    let protected = |name: &str| {
        let name = name.strip_prefix("refs/heads/").unwrap_or(name);
        ["main", "master"].contains(&name).then(|| name.to_owned())
    };
    let known = |name: &str| {
        common.as_ref().is_some_and(|c| {
            c.join(name).is_file()
                || read(&c.join("packed-refs"), INPUT_MAX).is_ok_and(|p| {
                    String::from_utf8_lossy(&p)
                        .lines()
                        .any(|l| l.split_once(' ').is_some_and(|(_, r)| r == name))
                })
        })
    };
    let instead = "commit on a work branch in `oms scratch-worktree`, then `oms land` (opens a PR); `oms land --direct` only for a consented docs-only change of at most 2 lines";
    if !land_active
        && sub == "commit"
        && !longs.contains("--dry-run")
        && protected(&branch).is_some()
        && repo.as_deref().and_then(head).is_some()
        && common.as_ref().is_some_and(|c| {
            read(&c.join("config"), INPUT_MAX)
                .is_ok_and(|raw| String::from_utf8_lossy(&raw).contains("[remote \""))
        })
    {
        return Some(Verdict::new(
            "git-commit-protected-branch",
            "git commit on main/master bypasses the pull-request workflow",
            instead,
        ));
    }
    if !land_active && sub == "push" && !longs.contains("--dry-run") && !letters.contains(&'n') {
        let operands = operands(rest);
        let remote = operands.first().copied().unwrap_or("origin");
        let specs = operands.iter().skip(1).copied().collect::<Vec<_>>();
        let destinations: Vec<String> = if longs.contains("--all") || longs.contains("--mirror") {
            ["main", "master"]
                .into_iter()
                .filter(|b| known(&format!("refs/heads/{b}")))
                .map(str::to_owned)
                .collect()
        } else if specs.is_empty() {
            protected(&branch)
                .filter(|_| !longs.contains("--tags"))
                .into_iter()
                .collect()
        } else {
            specs
                .iter()
                .filter_map(|spec| {
                    let spec = spec.trim_start_matches('+');
                    match spec.split_once(':') {
                        Some((_, destination)) => protected(destination),
                        None if spec == "HEAD" => protected(&branch),
                        None => protected(spec),
                    }
                })
                .collect()
        };
        // Only a URL or a remote whose default branch was fetched before can
        // be overwritten; an absent tracking ref is the initial publish.
        let published = |b: &String| {
            remote.contains(['/', ':']) || known(&format!("refs/remotes/{remote}/{b}"))
        };
        if destinations.iter().any(published) {
            return Some(Verdict::new(
                "git-push-protected-branch",
                "git push to main/master bypasses the pull-request workflow",
                instead,
            ));
        }
    }
    if sub == "reset" && longs.contains("--hard") {
        return Some(Verdict::new(
            "git-reset-hard",
            "git reset --hard discards uncommitted work",
            "git stash push -u first, or git reset --keep",
        ));
    }
    if sub == "clean"
        && !letters.contains(&'n')
        && !longs.contains("--dry-run")
        && (letters.contains(&'f') || longs.contains("--force"))
        && letters.iter().any(|c| "dxX".contains(*c))
    {
        return Some(Verdict::new(
            "git-clean",
            "git clean -fd/-x deletes untracked files for good",
            "git clean -nd to list them, then remove named paths",
        ));
    }
    if sub == "push" && (letters.contains(&'f') || longs.contains("--force")) {
        return Some(Verdict::new(
            "git-push-force",
            "a bare force push can overwrite commits you have not seen",
            "git push --force-with-lease",
        ));
    }
    if sub == "stash"
        && rest
            .first()
            .is_some_and(|a| ["drop", "clear"].contains(&a.as_str()))
    {
        return Some(Verdict::new(
            "git-stash-drop",
            format!("git stash {} destroys stashed work", rest[0]),
            "git stash list and git stash show first; apply what you need",
        ));
    }
    if ["checkout", "restore"].contains(&sub) {
        let paths = operands(rest);
        if sub == "restore" {
            if (longs.contains("--staged") || letters.contains(&'S'))
                && !(longs.contains("--worktree") || letters.contains(&'W'))
            {
                return None;
            }
        } else if !rest.iter().any(|a| a == "--") && paths.len() != 1 {
            return None;
        }
        let all = paths.contains(&":/")
            || paths.contains(&".")
                && base.is_some_and(|b| {
                    root(b, b).is_some_and(|r| r.join(".git").exists() && physical(b) == r)
                });
        if all {
            return Some(Verdict::new(
                "git-discard-worktree",
                format!("git {sub} of the whole worktree discards every uncommitted change"),
                "git stash push first, or name the files",
            ));
        }
    }
    None
}
fn verdict(command: &str, cwd: &Path) -> Result<Option<Verdict>, &'static str> {
    let land_active = std::env::var("OMS_LAND_ACTIVE").as_deref() == Ok("1");
    let mut cur = Some(cwd.to_path_buf());
    let mut previous = String::new();
    for segment in shell::segments(command)? {
        let words = shell::words(&segment, false);
        let i = shell::command_index(&words);
        if i >= words.len() {
            previous.clear();
            continue;
        }
        let name = Path::new(&words[i])
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let args = &words[i + 1..];
        if ["cd", "pushd"].contains(&name.as_str()) {
            let target = args
                .iter()
                .find(|s| !["-P", "-L", "-e", "--"].contains(&s.as_str()))
                .map_or("~", String::as_str);
            cur = resolve(cur.as_deref(), target);
            previous.clear();
            continue;
        }
        let (letters, _) = flags(args);
        if command.contains('|')
            && ["curl", "wget"].contains(&previous.as_str())
            && ["sh", "bash", "zsh", "dash", "ksh"].contains(&name.as_str())
            && !letters.contains(&'c')
            && (letters.contains(&'s') || operands(args).iter().all(|a| *a == "-"))
        {
            let mut v = Verdict::new(
                "download-pipe-shell",
                "a download piped straight into a shell runs code nobody read",
                "save it to a file, read it, then run the file",
            );
            v.name = previous;
            return Ok(Some(v));
        }
        previous = name.clone();
        let mut base = None;
        let found = if name == "rm" {
            rm_rule(args, cur.as_deref())
        } else if name.starts_with("mkfs") {
            Some(Verdict::new(
                "mkfs",
                "mkfs formats a filesystem",
                "a person runs that in their own shell",
            ))
        } else if name == "dd"
            && args.iter().any(|a| {
                a.starts_with("of=/dev/")
                    && ![
                        "/dev/null",
                        "/dev/zero",
                        "/dev/stdout",
                        "/dev/stderr",
                        "/dev/tty",
                        "/dev/fd/",
                    ]
                    .iter()
                    .any(|s| a[3..].starts_with(s))
            })
        {
            Some(Verdict::new(
                "dd-device",
                "dd onto a device overwrites it",
                "a person runs that in their own shell",
            ))
        } else if ["git", "git.exe"].contains(&name.as_str()) && !child() {
            let (j, b) = git_args(args, cur.as_deref());
            base = b;
            args.get(j)
                .and_then(|sub| git_rule(sub, &args[j + 1..], base.as_deref(), land_active))
        } else {
            None
        };
        if let Some(mut found) = found {
            found.name = name;
            found.base = base;
            return Ok(Some(found));
        }
    }
    Ok(None)
}
fn audit_path() -> PathBuf {
    env_path("XDG_STATE_HOME")
        .unwrap_or_else(|| home().join(".local/state"))
        .join("oh-my-setting/command-guard.jsonl")
}
fn audit_rows() -> Vec<Value> {
    read(&audit_path(), 8 * INPUT_MAX)
        .unwrap_or_default()
        .split(|b| *b == b'\n')
        .filter_map(document)
        .collect()
}
fn audit(args: &[OsString]) -> Output {
    if args.len() > 2 {
        return fail("--audit takes one optional row count");
    }
    let n = args
        .get(1)
        .map(|s| s.to_string_lossy())
        .unwrap_or_else(|| "50".into());
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return fail("--audit takes a row count");
    }
    let n = n.parse::<usize>().unwrap_or(1000).clamp(1, 1000);
    let rows = audit_rows();
    let rows = &rows[rows.len().saturating_sub(n)..];
    if rows.is_empty() {
        return out("command guard: nothing recorded\n");
    }
    let word = |v: &Value, k: &str, limit: usize| {
        let s = text(v, k)
            .chars()
            .filter(|c| c.is_alphanumeric() || "._:-".contains(*c))
            .take(limit)
            .collect::<String>();
        if s.is_empty() { "?".into() } else { s }
    };
    let mut counts = BTreeMap::<(String, String), usize>::new();
    for row in rows {
        *counts
            .entry((word(row, "verdict", 12), word(row, "rule", 32)))
            .or_default() += 1;
    }
    let total = |v: &str| {
        counts
            .iter()
            .filter(|((a, _), _)| a == v)
            .map(|(_, n)| *n)
            .sum::<usize>()
    };
    let noted = total("noted");
    let mut output = format!(
        "command guard: newest {} row(s): {} refused, {} bypassed (guard switched off for the session){}\n",
        rows.len(),
        total("refused"),
        total("bypassed"),
        if noted > 0 {
            format!(", {noted} noted (allowed, observed only)")
        } else {
            String::new()
        }
    );
    let mut sorted: Vec<_> = counts.iter().collect();
    sorted.sort_by_key(|(k, n)| (std::cmp::Reverse(**n), (*k).clone()));
    for ((verdict, rule), n) in sorted {
        output.push_str(&format!("  {n:4}  {verdict:9} {rule}\n"));
    }
    output.push_str("last:\n");
    for row in &rows[rows.len().saturating_sub(5)..] {
        output.push_str(&format!(
            "  {}  {:9} {:22} {}  sha256:{}\n",
            word(row, "ts", 20),
            word(row, "verdict", 12),
            word(row, "rule", 32),
            word(row, "command", 32),
            word(row, "sha256", 12)
        ));
    }
    output.push_str("rows carry no command text; any process of this user can edit the file, so read it as a record, not proof\n");
    out(output)
}
fn record(found: &Verdict, command: &str, sid: &str, bypass: bool) -> usize {
    let path = audit_path();
    if !path.is_absolute() {
        return 1;
    }
    let digest = if sid.is_empty() {
        String::new()
    } else {
        hash(format!("command-guard-session\0{sid}"))[..24].into()
    };
    mutate(&path, 8*INPUT_MAX,false,|raw| {
        let mut rows:Vec<Value>=raw.split(|b| *b==b'\n').filter_map(document).collect();
        let again=1+rows.iter().filter(|v| text(v,"rule")==found.rule && text(v,"verdict")=="refused" && !digest.is_empty() && text(v,"session")==digest).count();
        let mut row=json!({"schema":1,"ts":now(),"rule":found.rule,"verdict":if bypass {"bypassed"} else {"refused"},"command":found.name.chars().filter(|c| c.is_alphanumeric() || "._-".contains(*c)).take(32).collect::<String>(),"sha256":hash(command),"child":child()});
        if !digest.is_empty() { row["session"]=json!(digest); } rows.push(row);
        let bytes=rows.iter().skip(rows.len().saturating_sub(1000)).flat_map(json_line).collect(); Ok((bytes,again))
    }).unwrap_or(1)
}
fn checkpoint(found: &Verdict, ctx: &Context<'_>) -> String {
    let Some(repo) = found
        .base
        .as_deref()
        .and_then(|b| root(b, ctx.cwd))
        .filter(|r| adopted(r))
    else {
        return "No checkpoint was taken (this repository keeps no OMS state).".into();
    };
    let args: Vec<OsString> = [
        "create",
        "--untracked",
        "--reuse",
        "--label",
        &format!("pre-{}", found.rule),
        "--json",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    let (result, timed_out) = budgeted(ctx, std::time::Duration::from_secs(3), |flag| {
        oms_command_landing::run_with_source("checkpoint", &args, &repo, ctx.source, flag)
    });
    if timed_out {
        return "No checkpoint was taken (it did not finish in time).".into();
    }
    if result.0 == 0
        && let Some(row) = document(&result.1)
    {
        let id = text(&row, "id");
        if id.starts_with("cp-") && identifier(id, 160) {
            return format!(
                "The uncommitted and untracked work was saved first as checkpoint {id} (oms checkpoint restore {id} to see what it would bring back)."
            );
        }
    }
    "No checkpoint was taken.".into()
}
pub fn command(args: &[OsString], raw: &[u8], ctx: &Context<'_>) -> Output {
    if args.first().is_some_and(|a| a == "--audit") {
        return audit(args);
    }
    if raw.is_empty() {
        return empty();
    }
    let Some(v) = document(raw) else {
        return (0,vec![],b"command guard: cannot evaluate this command (hook payload is not valid JSON); allowing it unchecked.\n".to_vec());
    };
    let command = text(&v["tool_input"], "command");
    if command.is_empty() {
        return empty();
    }
    let cwd = absolute(ctx.cwd, payload_cwd(&v, ctx.cwd));
    let found=match verdict(command,&cwd) { Ok(v)=>v, Err(_)=>return (0,vec![],b"command guard: cannot evaluate this command (internal error while judging it); allowing it unchecked.\n".to_vec()) };
    let found = found.or_else(|| transfer_rule(command, session(&v), ctx));
    let Some(found) = found else {
        if env("OMS_COMMAND_GUARD_NOTE") == "1" && env("OMS_COMMAND_GUARD") != "0" {
            note(command, &v);
        }
        return empty();
    };
    let bypass = env("OMS_COMMAND_GUARD") == "0";
    let again = record(&found, command, session(&v), bypass);
    if bypass {
        return empty();
    }
    let mut line = format!(
        "command guard: {}. Instead: {}.",
        found.reason, found.instead
    );
    if again >= 2 {
        line.push_str(&format!(" This is refusal {again} of this rule in this session: stop and ask the owner instead of rewording the command."));
    }
    if ["git-reset-hard", "git-clean", "git-discard-worktree"].contains(&found.rule) {
        line.push(' ');
        line.push_str(&checkpoint(&found, ctx));
    }
    line.push_str(" If a person means it, they run it in their own shell (or start the session with OMS_COMMAND_GUARD=0).\n");
    (2, vec![], line.into_bytes())
}
fn note(command: &str, v: &Value) {
    let re =
        cached_regex!(r"(?:^|/)\.(?:ssh|aws|gnupg)(?:/|$)|(?:^|/)\.(?:netrc|env(?:\.[\w.-]+)?)$",);
    for segment in shell::segments(command).unwrap_or_default() {
        let words = shell::words(&segment, false);
        let i = shell::command_index(&words);
        let Some(name) = words.get(i) else {
            continue;
        };
        let name = Path::new(name)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        if ![
            "cat", "less", "more", "head", "tail", "grep", "egrep", "fgrep", "rg", "awk", "sed",
            "cp", "mv", "scp", "rsync", "tar", "zip", "base64", "xxd", "od", "strings", "bat",
            "source", ".", "curl", "wget", "nc", "openssl", "ssh-add", "diff", "cut", "sort",
        ]
        .contains(&name.as_ref())
        {
            continue;
        }
        if words[i + 1..].iter().any(|s| {
            ![".env.example", ".env.sample", ".env.template"]
                .iter()
                .any(|suffix| s.ends_with(suffix))
                && re.is_match(s)
        }) {
            let path = audit_path();
            let digest = hash(format!("command-guard-session\0{}", session(v)));
            let row = json!({
                "schema":1,"ts":now(),"rule":"secret-path-read","verdict":"noted",
                "command":name,"sha256":hash(command),"child":child(),"session":&digest[..24]
            });
            let _ = append(&path, &row, 8 * INPUT_MAX);
            break;
        }
    }
}
fn remote_host(endpoint: &str) -> Option<String> {
    if let Some(rest) = endpoint
        .strip_prefix("scp://")
        .or_else(|| endpoint.strip_prefix("rsync://"))
    {
        let host = rest
            .split('/')
            .next()?
            .rsplit('@')
            .next()?
            .trim_start_matches('[')
            .split(']')
            .next()?
            .split(':')
            .next()?;
        return literal(host).then(|| host.into());
    }
    let (_, tail) = endpoint.rsplit_once('@').unwrap_or(("", endpoint));
    let host = if tail.starts_with('[') {
        tail.split_once("]:")?.0.trim_start_matches('[')
    } else {
        tail.split_once(':')?.0
    };
    if host.contains('/') || host.is_empty() || host.len() == 1 && !endpoint.contains('@') {
        return None;
    }
    literal(host).then(|| host.into())
}
fn transfer_command(name: &str, args: &[String]) -> Option<(String, bool)> {
    let (values, booleans) = match name {
        "scp" => ("PiFoSJclD", "346ABCOpqRrTv"),
        "sftp" => ("bBFiJloPRSDcs", "46aCfNpqrv"),
        "rsync" => ("efTM", "aAbcdgHhikLlopqrtuvWxXz"),
        _ => return None,
    };
    let long_values = [
        "--rsh",
        "--port",
        "--files-from",
        "--exclude",
        "--include",
        "--exclude-from",
        "--include-from",
        "--filter",
        "--bwlimit",
        "--timeout",
        "--log-file",
        "--compare-dest",
        "--link-dest",
        "--copy-dest",
        "--rsync-path",
        "--chmod",
        "--chown",
        "--copy-as",
        "--usermap",
        "--groupmap",
        "--stop-at",
        "--out-format",
        "--iconv",
        "--address",
        "--partial-dir",
        "--temp-dir",
        "--backup-dir",
        "--suffix",
        "--max-size",
        "--min-size",
    ];
    let long_flags = [
        "--archive",
        "--recursive",
        "--verbose",
        "--compress",
        "--quiet",
        "--partial",
        "--progress",
        "--delete",
        "--checksum",
        "--update",
    ];
    let mut ends = Vec::new();
    let mut letters = BTreeSet::new();
    let mut i = 0;
    let mut plain = false;
    while i < args.len() {
        let arg = &args[i];
        if !plain && arg == "--" {
            plain = true;
        } else if !plain && arg.starts_with("--") {
            let (option, value) = arg
                .split_once('=')
                .map_or((arg.as_str(), false), |(k, _)| (k, true));
            if name == "rsync" && long_values.contains(&option) {
                if !value {
                    i += 1;
                }
            } else if name != "rsync" || !long_flags.contains(&option) || value {
                return None;
            }
        } else if !plain && arg.starts_with('-') && arg != "-" {
            for (j, c) in arg.chars().skip(1).enumerate() {
                if !values.contains(c) && !booleans.contains(c) {
                    return None;
                }
                letters.insert(c);
                if values.contains(c) {
                    if j == arg.len() - 2 {
                        i += 1;
                    }
                    break;
                }
            }
        } else {
            ends.push(arg.as_str());
        }
        i += 1;
    }
    if i > args.len() || letters.contains(&'D') {
        return None;
    }
    if name == "sftp" {
        return (letters.contains(&'b') && !ends.is_empty()).then(|| ("sftp".into(), false));
    }
    let host = ends.iter().filter_map(|s| remote_host(s)).next_back()?;
    if ends.len() < 2 {
        return None;
    }
    let source = ends[0];
    let local = source.rsplit_once(':').map_or(source, |(_, p)| p);
    let single = name == "scp"
        && !letters.contains(&'r')
        && ends.len() == 2
        && !source.contains("://")
        && literal(local)
        && !local.contains('{')
        && !(remote_host(source).is_some() && local.chars().any(char::is_whitespace));
    Some((host, single))
}
fn transfer_rule(command: &str, sid: &str, _ctx: &Context<'_>) -> Option<Verdict> {
    if cached_regex!(r"\(\s*\)|(?:^|[\s;&|])function\s").is_match(command) || command.contains("=(")
    {
        return None;
    }
    let mut copies = Vec::new();
    let mut depth = 0usize;
    let mut alternatives =
        command.contains("||") || cached_regex!(r"(?:^|[\s;])case\s").is_match(command);
    let mut guarded = command.contains("&&");
    for segment in shell::segments(command).ok()? {
        let words = shell::words(&segment, true);
        let mut i = shell::command_index(&words);
        while i < words.len()
            && ["do", "then", "else", "elif", "if", "{", "!"].contains(&words[i].as_str())
        {
            alternatives |= ["else", "elif"].contains(&words[i].as_str());
            guarded |= words[i] == "if";
            i += 1;
            i += shell::command_index(&words[i..]);
        }
        if i >= words.len() || words[..i].iter().any(|a| a.starts_with('-')) {
            continue;
        }
        let name = Path::new(&words[i])
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        if name == "done" {
            depth = depth.saturating_sub(1);
            continue;
        }
        if ["for", "while", "until"].contains(&name.as_ref()) {
            depth += 1;
            continue;
        }
        let mut repeated = depth > 0;
        if name == "find" {
            let Some(j) = words[i + 1..]
                .iter()
                .position(|w| ["-exec", "-execdir"].contains(&w.as_str()))
            else {
                continue;
            };
            i += j + 2;
            repeated = true;
        } else if name == "xargs" {
            i += 1;
            repeated = true;
            while i < words.len() && words[i].starts_with('-') {
                let option = &words[i];
                i += 1;
                if option == "--" {
                    break;
                }
                if option.starts_with("--") {
                    let (name, attached) = option
                        .split_once('=')
                        .map_or((option.as_str(), false), |(k, _)| (k, true));
                    if [
                        "--arg-file",
                        "--delimiter",
                        "--max-args",
                        "--max-procs",
                        "--max-chars",
                        "--process-slot-var",
                    ]
                    .contains(&name)
                    {
                        if !attached {
                            i += 1;
                        }
                    } else if ![
                        "--null",
                        "--eof",
                        "--replace",
                        "--max-lines",
                        "--interactive",
                        "--no-run-if-empty",
                        "--verbose",
                        "--show-limits",
                        "--exit",
                        "--open-tty",
                    ]
                    .contains(&name)
                    {
                        i = words.len();
                    }
                } else {
                    for (j, c) in option.chars().skip(1).enumerate() {
                        if "adEILnPs".contains(c) {
                            if j == option.len() - 2 {
                                i += 1;
                            }
                            break;
                        }
                        if "eil".contains(c) {
                            break;
                        }
                        if !"0prtxo".contains(c) {
                            i = words.len();
                            break;
                        }
                    }
                }
            }
        }
        if i >= words.len() {
            continue;
        }
        let name = Path::new(&words[i])
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if let Some(copy) = transfer_command(&name, &words[i + 1..]) {
            copies.push((name, copy, repeated));
        }
    }
    let in_command =
        !copies.is_empty() && (copies.iter().any(|c| c.2) || copies.len() >= 2 && !alternatives);
    let mut history = false;
    if copies.len() == 1 && !alternatives && !guarded && copies[0].1.1 && !sid.is_empty() {
        let path = audit_path().with_file_name("command-guard-transfer.state");
        let key = hash(format!("{sid}\0{}", copies[0].1.0.to_lowercase()));
        history = mutate(&path, 100_000, false, |raw| {
            let now = seconds() as u64;
            let mut rows: Vec<(String, u64)> = String::from_utf8_lossy(raw)
                .lines()
                .filter_map(|l| {
                    let (k, t) = l.split_once(' ')?;
                    let t = t.parse::<u64>().ok()?;
                    (k.len() == 64
                        && k.bytes().all(|b| b.is_ascii_hexdigit())
                        && now >= t
                        && now - t <= 600)
                        .then(|| (k.into(), t))
                })
                .collect();
            let repeated = rows.iter().any(|(k, _)| k == &key);
            if !repeated {
                rows.push((key, now));
            }
            let bytes = rows
                .iter()
                .skip(rows.len().saturating_sub(1000))
                .map(|(k, t)| format!("{k} {t}\n"))
                .collect::<String>()
                .into_bytes();
            Ok((bytes, repeated))
        })
        .unwrap_or(false);
    }
    if in_command || history {
        let mut v = Verdict::new(
            "transfer",
            "repeated remote copies should be bundled",
            "rsync -a --files-from=LIST SRC host:DST or tar -cf - FILES | ssh host 'tar -xf - -C DST'",
        );
        v.name = copies[0].0.clone();
        Some(v)
    } else {
        None
    }
}
pub fn git_move(raw: &[u8], ctx: &Context<'_>) -> Output {
    if !panel() || raw.is_empty() {
        return empty();
    }
    let Some(v) = document(raw) else {
        return (0,vec![],b"git move guard: cannot evaluate this command (hook payload is not valid JSON); allowing it unchecked.\n".to_vec());
    };
    let command = text(&v["tool_input"], "command");
    if command.is_empty() {
        return empty();
    }
    let Ok(segments) = shell::segments(command) else {
        return (0,vec![],b"git move guard: cannot evaluate this command (internal error while resolving it); allowing it unchecked.\n".to_vec());
    };
    let mut cur = Some(absolute(ctx.cwd, payload_cwd(&v, ctx.cwd)));
    for segment in segments {
        let words = shell::words(&segment, false);
        let mut i = 0;
        while i < words.len()
            && (["command", "exec", "time", "nohup", "sudo", "env"].contains(&words[i].as_str())
                || shell::assignment(&words[i]))
        {
            i += 1;
        }
        if i >= words.len() {
            continue;
        }
        if ["cd", "pushd"].contains(&words[i].as_str()) {
            i += 1;
            while i < words.len() && ["-P", "-L", "-e", "--"].contains(&words[i].as_str()) {
                i += 1;
            }
            cur = resolve(cur.as_deref(), words.get(i).map_or("~", String::as_str));
            continue;
        }
        if !Path::new(&words[i])
            .file_name()
            .is_some_and(|n| n == "git" || n == "git.exe")
        {
            continue;
        }
        let (j, base) = git_args(&words[i + 1..], cur.as_deref());
        let rest = &words[i + 1..];
        let Some(sub) = rest.get(j) else {
            continue;
        };
        let args = &rest[j + 1..];
        let moving = [
            "merge",
            "pull",
            "checkout",
            "switch",
            "reset",
            "commit",
            "rebase",
            "cherry-pick",
            "am",
            "revert",
        ]
        .contains(&sub.as_str())
            || sub == "stash"
                && !args
                    .first()
                    .is_some_and(|a| ["list", "show"].contains(&a.as_str()))
            || sub == "restore"
                && args.iter().any(|a| {
                    a == "--staged" || a.starts_with('-') && !a.starts_with("--") && a.contains('S')
                });
        if !moving {
            continue;
        }
        let Some(top) = base.as_deref().and_then(|b| root(b, ctx.cwd)) else {
            continue;
        };
        if !fs::symlink_metadata(top.join(".git")).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if let Some(state) = env_path("OMS_STATE_REPO")
            && root(&state, ctx.cwd).as_ref() != Some(&top)
        {
            continue;
        }
        let mut live = 0;
        for entry in fs::read_dir(top.join(".oms/locks/canonical-sync.d"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let pid = read(&entry.path().join("pid"), 128)
                .ok()
                .and_then(|r| String::from_utf8(r).ok())
                .and_then(|s| s.trim().parse::<i32>().ok())
                .filter(|p| *p > 0);
            let saved = read(&entry.path().join("process-start"), 1024)
                .ok()
                .map(|r| String::from_utf8_lossy(&r).trim().to_owned())
                .unwrap_or_default();
            if let Some(pid) = pid
                && !saved.is_empty()
            {
                if !oms_command_operations::lock::pid_alive(pid as i64) {
                    continue;
                }
                if let Some(current) = oms_command_operations::lock::process_start(pid as u32)
                    && current != saved
                {
                    continue;
                }
            }
            live += 1;
        }
        if live > 0 {
            return (2,vec![],format!("canonical checkout has {live} live OMS call(s); moving its index or refs stops them. Use oms sync-shared (defers until they finish) or commit in a linked worktree.\n").into_bytes());
        }
    }
    empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(head: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("oms-guard-{}-{n}", std::process::id()));
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/HEAD"), head).unwrap();
        dir
    }
    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn merge_and_pull_on_a_protected_branch_need_the_landing_gate() {
        for head in ["ref: refs/heads/main\n", "ref: refs/heads/master\n"] {
            let dir = checkout(head);
            for sub in ["merge", "pull"] {
                let found = git_rule(sub, &words(&["feat"]), Some(&dir), false).unwrap();
                assert_eq!(found.rule, "git-merge-protected-branch");
                assert!(found.instead.contains("oms land"));
                assert!(git_rule(sub, &words(&["feat"]), Some(&dir), true).is_none());
            }
            for flag in ["--abort", "--quit", "--continue"] {
                assert!(git_rule("merge", &words(&[flag]), Some(&dir), false).is_none());
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn commit_and_push_to_a_protected_branch_need_the_pull_request_workflow() {
        let oid = "1".repeat(40);
        let published = |head: &str| {
            let dir = checkout(head);
            for name in [
                "refs/heads/main",
                "refs/heads/feat/x",
                "refs/remotes/origin/main",
            ] {
                fs::create_dir_all(dir.join(".git").join(name).parent().unwrap()).unwrap();
                fs::write(dir.join(".git").join(name), format!("{oid}\n")).unwrap();
            }
            fs::write(dir.join(".git/config"), "[remote \"origin\"]\n").unwrap();
            dir
        };
        let main = published("ref: refs/heads/main\n");
        let feat = published("ref: refs/heads/feat/x\n");
        let fresh = checkout("ref: refs/heads/main\n");
        let rule = |sub: &str, args: &[&str], dir: &PathBuf, land: bool| {
            git_rule(sub, &words(args), Some(dir), land).map(|v| v.rule)
        };
        let commit = Some("git-commit-protected-branch");
        let push = Some("git-push-protected-branch");
        assert_eq!(rule("commit", &["-m", "x"], &main, false), commit);
        assert_eq!(rule("commit", &["-m", "x"], &main, true), None);
        assert_eq!(rule("commit", &["--dry-run"], &main, false), None);
        assert_eq!(rule("commit", &["-m", "x"], &feat, false), None);
        // an unborn or remote-less repository has no pull request to open yet
        assert_eq!(rule("commit", &["-m", "x"], &fresh, false), None);
        fs::write(fresh.join(".git/config"), "[core]\n").unwrap();
        fs::create_dir_all(fresh.join(".git/refs/heads")).unwrap();
        fs::write(fresh.join(".git/refs/heads/main"), format!("{oid}\n")).unwrap();
        assert_eq!(rule("commit", &["-m", "x"], &fresh, false), None);
        for args in [
            &[][..],
            &["origin"],
            &["origin", "HEAD"],
            &["-u", "origin", "main"],
            &["origin", "+feat/x:main"],
            &["--all", "origin"],
            &["git@example.invalid:o/r.git", "HEAD:refs/heads/master"],
        ] {
            assert_eq!(rule("push", args, &main, false), push, "{args:?}");
            assert_eq!(rule("push", args, &main, true), None, "{args:?}");
        }
        for args in [
            &["origin", "HEAD"][..],
            &["-u", "origin", "feat/x"],
            &["origin", "main:feat/x"],
        ] {
            assert_eq!(rule("push", args, &feat, false), None, "{args:?}");
        }
        assert_eq!(rule("push", &["origin", "main"], &feat, false), push);
        assert_eq!(rule("push", &["--tags", "origin"], &main, false), None);
        assert_eq!(rule("push", &["-n", "origin", "main"], &main, false), None);
        // the first publish of a default branch has no remote branch to target
        assert_eq!(rule("push", &["-u", "origin", "main"], &fresh, false), None);
        assert_eq!(rule("push", &["origin", "HEAD:master"], &main, false), None);
        let found = git_rule("push", &words(&["origin", "main"]), Some(&main), false).unwrap();
        assert!(found.instead.contains("oms land"));
        for dir in [main, feat, fresh] {
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn ff_only_pull_and_merge_pass_on_protected_branches_but_not_no_ff() {
        let dir = checkout("ref: refs/heads/main\n");
        for sub in ["merge", "pull"] {
            assert!(git_rule(sub, &words(&["--ff-only", "feat"]), Some(&dir), false).is_none());
            for args in [&["--ff-only", "--no-ff", "feat"][..], &["--no-ff", "feat"]] {
                assert!(git_rule(sub, &words(args), Some(&dir), false).is_some());
            }
            assert!(git_rule(sub, &words(&["feat"]), Some(&dir), false).is_some());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn merge_and_pull_on_feature_or_unknown_checkouts_stay_allowed() {
        let dir = checkout("ref: refs/heads/feature/x\n");
        for sub in ["merge", "pull"] {
            assert!(git_rule(sub, &words(&["main"]), Some(&dir), false).is_none());
            assert!(git_rule(sub, &[], None, false).is_none());
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
