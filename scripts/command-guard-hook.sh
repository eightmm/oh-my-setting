#!/usr/bin/env bash
[ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || { echo "error: oh-my-setting needs Bash 5 or newer (running ${BASH_VERSION:-unknown}); on macOS run: brew install bash, then rerun" >&2; exit 1; }

# PreToolUse hook (Bash): refuse a short list of commands that destroy work or
# a machine in one step, say why, and name the safer spelling. The list is
# deliberately narrow: every rule is a shape that is almost never what was
# meant (rm -rf of a root, home, the checkout or its .git; git reset --hard;
# git clean -fd/-x; a bare force push; git stash drop/clear; discarding the
# whole worktree; mkfs; dd onto a device; a download piped into a shell).
#
# This is a seatbelt against accidents, not a sandbox. It reads the command
# text only and fails open: an unparsable payload, a missing helper, a command
# spelled through a variable, eval, a subshell, a script file or a heredoc the
# tokenizer cannot see all exit 0. What a script does once it runs is not seen,
# so `oms` internals are never judged here.
#
# The switch is the environment of the session, never the command: the hook is
# started by the provider, so `OMS_COMMAND_GUARD=0 rm -rf .` typed by the agent
# changes nothing. A person who means it runs the command in their own shell,
# or starts the session with OMS_COMMAND_GUARD=0.
#
# A harness child (an OMS worker in its own worktree) skips the git rules: its
# worktree is disposable and patch-admit is the boundary for what it produced.
# The filesystem, device and download rules hold for every session.
#
# Every refusal leaves one row in a per-user log: time, rule, verdict, the
# command's first word and a digest of its text, never the text. A session
# whose guard is switched off still records the commands it would have refused,
# as `bypassed`. The log is a record of what the guard saw, not a boundary: any
# process of the same user, a worker included, can write or remove the file.
# `command-guard-hook.sh --audit [N]` summarizes the newest N rows.
#
# When a session's guard refuses the same rule again, the refusal says which
# time it is and suggests stopping to ask the owner (rows carry a digest of the
# session id, never the id). Nothing is blocked or ended by the count.
#
# `OMS_COMMAND_GUARD_NOTE=1` (off by default) also records, as `noted`, an
# allowed command that names a well-known secret path (~/.ssh, ~/.aws, ~/.gnupg,
# .netrc, .env) for a reading command. It prints nothing, never refuses, and
# keeps the same row shape (first word and digest, no text, no path). Commands
# the tokenizer cannot see, such as the inside of a script, are still unseen.
#
# Before refusing git reset --hard, git clean or a whole-worktree discard in a
# repository that already keeps OMS state, the guard takes a checkpoint of the
# tracked changes and the untracked files (`oms checkpoint create --untracked
# --reuse`) and names it in the refusal, so the work survives if the person
# then runs the command themselves. It has three seconds; past that the
# refusal stands and says no checkpoint was taken.

ROOT="${BASH_SOURCE[0]%/*}/.."
AUDIT_LOG="${XDG_STATE_HOME:-${HOME:-}/.local/state}/oh-my-setting/command-guard.jsonl"

if [ "${1:-}" = --audit ]; then
  case "${2:-50}" in
    ''|*[!0-9]*) echo "error: --audit takes a row count" >&2; exit 2 ;;
  esac
  [ "$#" -le 2 ] || { echo "error: --audit takes one optional row count" >&2; exit 2; }
  exec python3 -I - "$AUDIT_LOG" "${2:-50}" <<'PY'
import collections
import json
import sys

path, limit = sys.argv[1], max(1, min(int(sys.argv[2]), 1000))
rows = []
try:
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict):
                rows.append(row)
except OSError:
    pass
rows = rows[-limit:]
if not rows:
    print("command guard: nothing recorded")
    raise SystemExit(0)


def word(value, size):
    text = "".join(ch for ch in str(value or "?") if ch.isalnum() or ch in "._:-")
    return (text or "?")[:size]


counts = collections.Counter((word(row.get("verdict"), 12), word(row.get("rule"), 32)) for row in rows)
refused = sum(n for (verdict, _), n in counts.items() if verdict == "refused")
bypassed = sum(n for (verdict, _), n in counts.items() if verdict == "bypassed")
noted = sum(n for (verdict, _), n in counts.items() if verdict == "noted")
print("command guard: newest %d row(s): %d refused, %d bypassed (guard switched off for the session)%s"
      % (len(rows), refused, bypassed, ", %d noted (allowed, observed only)" % noted if noted else ""))
for (verdict, rule), n in sorted(counts.items(), key=lambda item: (-item[1], item[0])):
    print("  %4d  %-9s %s" % (n, verdict, rule))
print("last:")
for row in rows[-5:]:
    print("  %s  %-9s %-22s %s  sha256:%s" % (
        word(row.get("ts"), 20), word(row.get("verdict"), 12), word(row.get("rule"), 32),
        word(row.get("command"), 32), word(row.get("sha256"), 12)))
print("rows carry no command text; any process of this user can edit the file, so read it as a record, not proof")
PY
fi

# The guard switched off for the session still records what it would have refused.
mode=enforce
[ "${OMS_COMMAND_GUARD:-1}" != 0 ] || mode=bypass

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

# Shell-only precheck so an unrelated command never starts an interpreter. A
# payload this regex cannot read unambiguously falls through to the parser.
command_re='"command"[[:space:]]*:[[:space:]]*"(([^"\\]|\\.)*)"'
if [[ "$payload" =~ $command_re ]]; then
  command_text="${BASH_REMATCH[1]}"
  case "$command_text" in
    *'\u'*) ;;
    *rm*|*git*|*mkfs*|*dd*|*curl*|*wget*|*scp*|*sftp*|*rsync*) ;;
    *.ssh*|*.aws*|*.gnupg*|*.netrc*|*.env*) [ "${OMS_COMMAND_GUARD_NOTE:-0}" = 1 ] || exit 0 ;;
    *) exit 0 ;;
  esac
fi

parser=""
read -r -d '' parser <<'PY' || true
import datetime
import fcntl
import hashlib
import json
import os
import re
import signal
import stat
import subprocess
import sys
import time
from urllib.parse import urlsplit

OPTION_VALUE = frozenset(("-c", "--git-dir", "--work-tree", "--namespace",
                          "--super-prefix", "--config-env"))
WRAPPERS = frozenset(("command", "exec", "time", "nohup", "sudo", "env", "doas"))
SHELLS = frozenset(("sh", "bash", "zsh", "dash", "ksh"))
FETCHERS = frozenset(("curl", "wget"))
HOME_WORDS = frozenset(("~", "$HOME", "${HOME}"))
SAFE_DEVICES = ("/dev/null", "/dev/zero", "/dev/stdout", "/dev/stderr", "/dev/tty", "/dev/fd/")
AUDIT_ROWS = 1000
sys.path.insert(0, sys.argv[1])
child = sys.argv[2] == "1"
mode = sys.argv[3]
audit_log = sys.argv[4]
checkpoint_script = sys.argv[5]
# The refusals whose command would have discarded work a checkpoint can hold.
CHECKPOINTED = frozenset(("git-reset-hard", "git-clean", "git-discard-worktree"))
CHECKPOINT_SECONDS = 3
from hook_state import direct_shell_segments


def checkpoint(rule, base):
    """Save what the refused command would have discarded; a sentence for the refusal.

    Only in a repository that already keeps OMS state, and only inside the
    hook's time budget: a slow or failed capture is said, never waited for.
    The same state refused twice reuses the first checkpoint."""
    import signal
    top = toplevel(base) if base else None
    if not top or not os.path.isdir(os.path.join(top, ".oms")) or os.path.islink(os.path.join(top, ".oms")):
        return "No checkpoint was taken (this repository keeps no OMS state)."
    try:
        process = subprocess.Popen(
            ["bash", checkpoint_script, "--repo", top, "create", "--untracked", "--reuse",
             "--label", "pre-" + rule, "--json"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
            start_new_session=True, text=True)
    except OSError:
        return "No checkpoint was taken."
    try:
        out, _ = process.communicate(timeout=CHECKPOINT_SECONDS)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError:
            pass
        process.communicate()
        return "No checkpoint was taken (it did not finish in time)."
    try:
        ident = json.loads(out).get("id") if process.returncode == 0 else None
    except ValueError:
        ident = None
    if not isinstance(ident, str) or not re.fullmatch(r"cp-[A-Za-z0-9._-]+", ident):
        return "No checkpoint was taken."
    return ("The uncommitted and untracked work was saved first as checkpoint %s "
            "(oms checkpoint restore %s to see what it would bring back)." % (ident, ident))


def literal(text):
    return bool(text) and not any(ch in text for ch in "$`*?[\n")


def resolve(base, target):
    if base is None:
        return None
    if target in HOME_WORDS or target.startswith("~/"):
        target = os.path.expanduser("~") + target[1:] if target[:1] == "~" else os.path.expanduser("~")
    if not literal(target) or target == "-":
        return None
    return os.path.join(base, target)


def physical(path):
    try:
        return os.path.realpath(path)
    except (OSError, ValueError):
        return None


def toplevel(base, cache={}):
    if base not in cache:
        top = None
        try:
            done = subprocess.run(["git", "-C", base, "rev-parse", "--show-toplevel"],
                                  capture_output=True, text=True, timeout=2,
                                  stdin=subprocess.DEVNULL, check=False)
            if done.returncode == 0 and done.stdout.strip():
                top = physical(done.stdout.strip())
        except (OSError, subprocess.SubprocessError):
            top = None
        cache[base] = top
    return cache[base]


def short_flags(args):
    """Letters of the single-dash clusters before `--`, and the long options."""
    letters, longs = set(), set()
    for arg in args:
        if arg == "--":
            break
        if arg.startswith("--"):
            longs.add(arg.split("=", 1)[0])
        elif arg.startswith("-") and len(arg) > 1:
            letters.update(arg[1:])
    return letters, longs


def operands(args):
    found, plain = [], False
    for arg in args:
        if plain or not arg.startswith("-") or arg == "-":
            found.append(arg)
        elif arg == "--":
            plain = True
    return found


# Every rule answers (rule id, reason, safer spelling). The id is what the
# audit log keeps; it never carries a path or an argument.
def rm_rule(args, base):
    letters, longs = short_flags(args)
    if not (letters & {"r", "R"} or "--recursive" in longs):
        return None
    if not ("f" in letters or "--force" in longs):
        return None
    home = physical(os.path.expanduser("~"))
    for target in operands(args):
        bare = target.rstrip("/") or "/"
        if bare in ("/", "/*") or target == "/*":
            return ("rm-root", "rm -rf of the filesystem root", "name the directory you mean")
        if bare in HOME_WORDS or bare in ("~/*", "$HOME/*", "${HOME}/*"):
            return ("rm-home", "rm -rf of the home directory", "name the directory you mean")
        if bare in (".", ".."):
            return ("rm-cwd", "rm -rf of the current or parent directory", "name the path explicitly")
        path = resolve(base, target)
        path = physical(path) if path else None
        if not path:
            continue
        if path == "/":
            return ("rm-root", "rm -rf of the filesystem root", "name the directory you mean")
        if home and path == home:
            return ("rm-home", "rm -rf of the home directory", "name the directory you mean")
        top = toplevel(os.path.dirname(path) if os.path.basename(path) == ".git" else path) \
            if os.path.isdir(path) else None
        if top and path == top:
            return ("rm-checkout", "rm -rf of the whole checkout",
                    "name the path inside it, or remove a worktree with git worktree remove")
        if top and path == os.path.join(top, ".git"):
            return ("rm-git-dir", "rm -rf of the repository's .git",
                    "nothing recovers that; move it aside with mv if you mean it")
    return None


def git_rule(sub, rest, base):
    letters, longs = short_flags(rest)
    if sub == "reset" and "--hard" in longs:
        return ("git-reset-hard", "git reset --hard discards uncommitted work",
                "git stash push -u first, or git reset --keep")
    if sub == "clean":
        if "n" in letters or "--dry-run" in longs:
            return None
        if ("f" in letters or "--force" in longs) and letters & {"d", "x", "X"}:
            return ("git-clean", "git clean -fd/-x deletes untracked files for good",
                    "git clean -nd to list them, then remove named paths")
    if sub == "push":
        if "f" in letters or "--force" in longs:
            return ("git-push-force", "a bare force push can overwrite commits you have not seen",
                    "git push --force-with-lease")
    if sub == "stash" and rest and rest[0] in ("drop", "clear"):
        return ("git-stash-drop", "git stash %s destroys stashed work" % rest[0],
                "git stash list and git stash show first; apply what you need")
    if sub in ("checkout", "restore"):
        paths = operands(rest)
        if sub == "restore":
            staged_only = ("--staged" in longs or "S" in letters) and not ("--worktree" in longs or "W" in letters)
            if staged_only:
                return None
        elif "--" not in rest and len(paths) != 1:
            return None  # `git checkout BRANCH .` and friends: not judged
        everything = ":/" in paths
        if not everything and "." in paths and base:
            top = toplevel(base)
            everything = bool(top) and physical(base) == top
        if everything:
            return ("git-discard-worktree",
                    "git %s of the whole worktree discards every uncommitted change" % sub,
                    "git stash push first, or name the files")
    return None


def command_word(words):
    """Index of the command word after wrappers, their options and assignments."""
    i = 0
    while i < len(words) and (words[i] in WRAPPERS or words[i].startswith("-") or
                              re.match(r"[A-Za-z_][A-Za-z0-9_]*=", words[i])):
        i += 1
    return i


TRANSFER = ("transfer", "repeated remote copies should be bundled",
            "rsync -a --files-from=LIST SRC host:DST or tar -cf - FILES | ssh host 'tar -xf - -C DST'")


REMOTE = re.compile(r"^(?:(?P<user>[^/@:]+)@)?(?P<host>\[[^]]+\]|[^/:]+):")
# Per tool: (letters taking a value, boolean letters). A letter or long option
# outside these makes the shape uncertain, and the rule then leaves it alone.
TRANSFER_OPTIONS = {"scp": ("PiFoSJclD", "346ABCOpqRrTv"),
                    "sftp": ("bBFiJloPRSDcs", "46aCfNpqrv"),
                    "rsync": ("efTM", "aAbcdgHhikLlopqrtuvWxXz")}
RSYNC_VALUES = frozenset((
    "--rsh", "--port", "--files-from", "--exclude", "--include", "--exclude-from",
    "--include-from", "--filter", "--bwlimit", "--timeout", "--log-file", "--compare-dest",
    "--link-dest", "--copy-dest", "--rsync-path", "--chmod", "--chown", "--copy-as",
    "--usermap", "--groupmap", "--stop-at", "--out-format", "--iconv", "--address",
    "--partial-dir", "--temp-dir", "--backup-dir", "--suffix", "--max-size", "--min-size"))
RSYNC_FLAGS = frozenset(("--archive", "--recursive", "--verbose", "--compress", "--quiet",
                         "--partial", "--progress", "--delete", "--checksum", "--update"))
SHELL_KEYWORDS = frozenset(("do", "then", "else", "elif", "if", "{", "!"))
XARGS_VALUES = frozenset(("--arg-file", "--delimiter", "--max-args", "--max-procs",
                          "--max-chars", "--process-slot-var"))
XARGS_FLAGS = frozenset(("--null", "--eof", "--replace", "--max-lines", "--interactive",
                         "--no-run-if-empty", "--verbose", "--show-limits", "--exit", "--open-tty"))


def remote_host(endpoint):
    if endpoint.startswith(("scp://", "rsync://")):
        try:
            host = urlsplit(endpoint).hostname
            return host if host and literal(host) else None
        except ValueError:
            return None
    match = REMOTE.match(endpoint)
    if not match:
        return None
    host = match.group("host")
    if not match.group("user") and len(host) == 1:
        return None  # `C:foo` is a drive path
    host = host.strip("[]")
    return host if literal(host) else None


def literal_path(endpoint):
    """Whether the path part of an endpoint names exactly one file."""
    if endpoint.startswith(("scp://", "rsync://")):
        return False  # percent escapes can spell a glob
    match = REMOTE.match(endpoint)
    if match and remote_host(endpoint):
        endpoint = endpoint[match.end():]
        if any(ch.isspace() for ch in endpoint):
            return False  # the remote shell splits it into several names
    return literal(endpoint) and "{" not in endpoint  # brace expansion names several


def transfer_command(name, args):
    """(host, single) for a remote copy; None when it is local or its shape is uncertain.

    Only `scp` without -r and with one literal source is a single-file copy: it
    cannot copy a directory, so no filesystem lookup is needed. rsync and sftp
    batches count toward the in-command rule but never toward history.
    """
    if name not in TRANSFER_OPTIONS:
        return None
    values, booleans = TRANSFER_OPTIONS[name]
    endpoints, letters, i, plain = [], set(), 0, False
    while i < len(args):
        arg = args[i]
        if not plain and arg == "--":
            plain = True
        elif not plain and arg.startswith("--"):
            option, sep, _ = arg.partition("=")
            if name == "rsync" and option in RSYNC_VALUES:
                i += 0 if sep else 1
            elif name != "rsync" or option not in RSYNC_FLAGS or sep:
                return None
        elif not plain and arg.startswith("-") and arg != "-":
            for j, letter in enumerate(arg[1:]):
                if letter not in values and letter not in booleans:
                    return None
                letters.add(letter)
                if letter in values:
                    i += 1 if j == len(arg) - 2 else 0
                    break
        else:
            endpoints.append(arg)
        i += 1
    if i > len(args) or "D" in letters:
        return None  # a value option without its value; -D talks to a local server
    if name == "sftp":
        return ("sftp", False) if "b" in letters and endpoints else None
    hosts = [host for host in map(remote_host, endpoints) if host]
    if not hosts or len(endpoints) < 2:
        return None  # local, or an rsync listing
    single = name == "scp" and "r" not in letters and len(endpoints) == 2 and literal_path(endpoints[0])
    return hosts[-1], single


def xargs_command(words, i):
    """Index of the command xargs runs, or None when an option is not understood."""
    while i < len(words) and words[i].startswith("-"):
        option = words[i]
        i += 1
        if option == "--":
            break
        if option.startswith("--"):
            name, sep, _ = option.partition("=")
            if name in XARGS_VALUES:
                i += 0 if sep else 1
            elif name not in XARGS_FLAGS:
                return None
            continue
        for j, letter in enumerate(option[1:]):
            if letter in "adEILnPs":
                i += 1 if j == len(option) - 2 else 0
                break
            if letter in "eil":
                break  # optional value, attached only
            if letter not in "0prtxo":
                return None
    return i


def transfer_history(session, host):
    if not isinstance(session, str) or not session:
        return False
    directory = os.path.dirname(audit_log)
    if not directory or not os.path.isabs(directory):
        return False

    def expired(signum, frame):
        raise TimeoutError()

    previous = signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, 1.0)
    try:
        os.makedirs(directory, mode=0o700, exist_ok=True)
        path = os.path.join(directory, "command-guard-transfer.state")
        flags = os.O_RDWR | os.O_CREAT | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(path, flags, 0o600)
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            os.close(fd)
            return False
        with os.fdopen(fd, "r+", encoding="ascii") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            now = int(time.time())
            key = hashlib.sha256((session + "\0" + host.lower()).encode("utf-8", "surrogatepass")).hexdigest()
            rows = []
            for line in handle.read(100000).splitlines():
                parts = line.split()
                if len(parts) == 2 and re.fullmatch(r"[0-9a-f]{64}", parts[0]) and parts[1].isdigit():
                    stamp = int(parts[1])
                    if 0 <= now - stamp <= 600:
                        rows.append((parts[0], stamp))
            repeated = any(k == key for k, _ in rows)
            if not repeated:
                rows.append((key, now))
            handle.seek(0)
            handle.truncate()
            handle.writelines("%s %d\n" % row for row in rows[-1000:])
            return repeated
    except (OSError, ValueError, UnicodeError, TimeoutError):
        return False
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def transfer_rule(text, session):
    if re.search(r"\(\s*\)|(?:^|[\s;&|])function\s", text):
        return None  # a function body runs only when called
    if "=(" in text:
        return None  # array contents are words, not commands
    # `else`, `elif`, `||` and `case` arms are alternatives: two copies there need not both run.
    copies, depth, alternatives = [], 0, "||" in text or bool(re.search(r"(?:^|[\s;])case\s", text))
    guarded = "&&" in text  # a copy behind `&&` or `if` may never run

    for segment in direct_shell_segments(text):
        words, skip = [], False
        for word, is_op in segment:
            if is_op:
                skip = True  # a redirection operator and its target are not arguments
            elif skip:
                skip = False
            else:
                words.append(word)
        i = command_word(words)
        while i < len(words) and words[i] in SHELL_KEYWORDS:
            alternatives = alternatives or words[i] in ("else", "elif")
            guarded = guarded or words[i] == "if"
            i += 1 + command_word(words[i + 1:])
        if i >= len(words):
            continue
        if any(word.startswith("-") for word in words[:i]):
            continue  # a wrapper option may take a value (`sudo -u NAME`)
        name = os.path.basename(words[i])
        if name == "done":
            depth = max(0, depth - 1)
            continue
        if name in ("for", "while", "until"):
            depth += 1
            continue
        repeated = depth > 0
        if name == "find":
            positions = [j for j in range(i + 1, len(words)) if words[j] in ("-exec", "-execdir")]
            if not positions:
                continue
            i, repeated = positions[0] + 1, True
        elif name == "xargs":
            i, repeated = xargs_command(words, i + 1), True
            if i is None:
                continue
        if i >= len(words):
            continue
        name = os.path.basename(words[i])
        copy = transfer_command(name, words[i + 1:])
        if copy:
            copies.append((name, copy, repeated))
    if copies and (any(item[2] for item in copies) or (len(copies) >= 2 and not alternatives)):
        return TRANSFER + (copies[0][0], None)
    # A conditional copy may never run, so it does not enter history either.
    if len(copies) == 1 and not alternatives and not guarded and copies[0][1][1] and transfer_history(session, copies[0][1][0]):
        return TRANSFER + (copies[0][0], None)
    return None


def verdict(text, cwd):
    """(rule id, reason, safer spelling, command name, git base) for the first refused segment."""
    cur, previous = cwd, None
    piped = "|" in text
    for segment in direct_shell_segments(text):
        words = [w for w, is_op in segment if not is_op]
        i = command_word(words)
        if i >= len(words):
            previous = None
            continue
        name, args = os.path.basename(words[i]), words[i + 1:]
        if name in ("cd", "pushd"):
            rest = [a for a in args if a not in ("-P", "-L", "-e", "--")]
            cur = resolve(cur, rest[0] if rest else "~")
            previous = None
            continue
        letters = short_flags(args)[0]
        if piped and previous in FETCHERS and name in SHELLS and "c" not in letters and \
                ("s" in letters or not [a for a in operands(args) if a != "-"]):
            return ("download-pipe-shell", "a download piped straight into a shell runs code nobody read",
                    "save it to a file, read it, then run the file", previous, None)
        previous = name
        found, base = None, None
        if name == "rm":
            found = rm_rule(args, cur)
        elif name.startswith("mkfs"):
            found = ("mkfs", "mkfs formats a filesystem", "a person runs that in their own shell")
        elif name == "dd":
            for arg in args:
                if arg.startswith("of=/dev/") and not arg[3:].startswith(SAFE_DEVICES):
                    found = ("dd-device", "dd onto a device overwrites it", "a person runs that in their own shell")
        elif name in ("git", "git.exe") and not child:
            base, j = cur, 0
            while j < len(args):
                arg = args[j]
                if arg == "-C" and j + 1 < len(args):
                    base, j = resolve(base, args[j + 1]), j + 2
                elif arg in OPTION_VALUE:
                    j += 2
                elif arg.startswith("-"):
                    j += 1
                else:
                    break
            if j < len(args):
                found = git_rule(args[j], args[j + 1:], base)
        if found:
            return found + (name, base)
    return None


def session_digest(session):
    if not isinstance(session, str) or not session:
        return None
    return hashlib.sha256(("command-guard-session\0" + session).encode("utf-8", "surrogatepass")).hexdigest()[:24]


def earlier_refusals(rule, digest):
    """How many times this session was already refused by this rule (the log is at most 1000 rows)."""
    if not digest:
        return 0
    count = 0
    try:
        with open(audit_log, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if isinstance(row, dict) and row.get("rule") == rule and row.get("verdict") == "refused" \
                        and row.get("session") == digest:
                    count += 1
    except OSError:
        pass
    return count


SECRET_PATH = re.compile(r"(?:^|/)\.(?:ssh|aws|gnupg)(?:/|$)|"
                         r"(?:^|/)\.(?:netrc|env(?:\.(?!example$|sample$|template$)[\w.-]+)?)$")
SECRET_READERS = frozenset(("cat", "less", "more", "head", "tail", "grep", "egrep", "fgrep", "rg", "awk", "sed",
                            "cp", "mv", "scp", "rsync", "tar", "zip", "base64", "xxd", "od", "strings", "bat",
                            "source", ".", "curl", "wget", "nc", "openssl", "ssh-add", "diff", "cut", "sort"))


def secret_read(text):
    """Name of the first reading command in `text` that names a well-known secret path, else None."""
    home = ("~", "$HOME", "${HOME}")
    for segment in direct_shell_segments(text):
        words = [w for w, is_op in segment if not is_op]
        i = command_word(words)
        if i >= len(words) or os.path.basename(words[i]) not in SECRET_READERS:
            continue
        for word in words[i + 1:]:
            for part in (word, word.split("=", 1)[-1], word.lstrip("@<")):
                for prefix in home:
                    if part.startswith(prefix + "/"):
                        part = part[len(prefix):]
                        break
                if SECRET_PATH.search(part):
                    return os.path.basename(words[i])
    return None


def record(rule, outcome, name, text, session=None):
    """One row per refusal in the per-user log: never the command text. Best effort."""
    try:
        directory = os.path.dirname(audit_log)
        if not directory or not os.path.isabs(directory):
            return
        os.makedirs(directory, mode=0o700, exist_ok=True)
        row = {
            "schema": 1,
            "ts": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "rule": rule,
            "verdict": outcome,
            "command": "".join(ch for ch in name if ch.isalnum() or ch in "._-")[:32] or "?",
            "sha256": hashlib.sha256(text.encode("utf-8", "surrogatepass")).hexdigest(),
            "child": child,
        }
        if session:
            row["session"] = session
        flags = os.O_WRONLY | os.O_APPEND | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(audit_log, flags, 0o600)
        try:
            os.write(fd, (json.dumps(row, sort_keys=True) + "\n").encode("utf-8"))
        finally:
            os.close(fd)
        with open(audit_log, encoding="utf-8", errors="replace") as handle:
            lines = handle.readlines()
        if len(lines) > AUDIT_ROWS:
            fresh = audit_log + ".%d.tmp" % os.getpid()
            fd = os.open(fresh, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.writelines(lines[-AUDIT_ROWS:])
            os.replace(fresh, audit_log)
    except (OSError, ValueError):
        pass


# Exit 3: the payload is not JSON, so the guard cannot tell what was asked. Exit
# 4: judging the command failed. The shell caller turns either into one stderr
# line; the command is still allowed. A payload without tool_input.command (a
# tool that is not Bash) is none of the guard's business and stays silent.
try:
    doc = json.load(sys.stdin)
except Exception:
    sys.exit(3)
if not isinstance(doc, dict):
    sys.exit(3)
try:
    command = doc["tool_input"]["command"]
except (KeyError, TypeError):
    sys.exit(0)
try:
    cwd = doc.get("cwd")
    if not isinstance(cwd, str) or not cwd:
        cwd = os.getcwd()
    session = doc.get("session_id") or doc.get("sessionId")
    found = (verdict(command, cwd) or transfer_rule(command, session)) if isinstance(command, str) else None
    if found:
        rule, reason, instead, name, base = found
        digest = session_digest(session)
        again = earlier_refusals(rule, digest) + 1 if mode != "bypass" else 1
        record(rule, "bypassed" if mode == "bypass" else "refused", name, command, digest)
        if mode != "bypass":
            line = "%s. Instead: %s." % (reason, instead)
            if again >= 2:
                line += (" This is refusal %d of this rule in this session: stop and ask the owner "
                         "instead of rewording the command." % again)
            print(line)
            if rule in CHECKPOINTED:
                print(checkpoint(rule, base))
    elif mode != "bypass" and os.environ.get("OMS_COMMAND_GUARD_NOTE") == "1" and isinstance(command, str):
        try:
            name = secret_read(command)
            if name:
                record("secret-path-read", "noted", name, command, session_digest(session))
        except Exception:
            pass
except Exception:
    sys.exit(4)
PY

child=0
[ "${OMS_HARNESS_CHILD:-}" != 1 ] || child=1
# Fail open, never silently: one stderr line says the command went unjudged.
# Shell builtins only, so it works when python3 or the helper library is gone.
guard_unevaluated() {
  printf 'command guard: cannot evaluate this command (%s); allowing it unchecked.\n' "$1" >&2
}
command -v python3 >/dev/null 2>&1 || { guard_unevaluated "python3 not found"; exit 0; }
rc=0
reason="$(printf '%s' "$payload" | python3 -I -c "$parser" "$ROOT/scripts/lib" "$child" "$mode" "$AUDIT_LOG" \
  "$ROOT/scripts/checkpoint.sh" 2>/dev/null)" || rc=$?
case "$rc" in
  0) ;;
  3) guard_unevaluated "hook payload is not valid JSON"; exit 0 ;;
  4) guard_unevaluated "internal error while judging it"; exit 0 ;;
  *) guard_unevaluated "python helper failed to run or its library is missing"; exit 0 ;;
esac
[ -n "$reason" ] || exit 0
saved=""
case "$reason" in
  *$'\n'*) saved="${reason#*$'\n'}"; saved=" ${saved%%$'\n'*}" ;;
esac
echo "command guard: ${reason%%$'\n'*}${saved} If a person means it, they run it in their own shell (or start the session with OMS_COMMAND_GUARD=0)." >&2
exit 2
