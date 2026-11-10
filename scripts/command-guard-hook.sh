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
print("command guard: newest %d row(s): %d refused, %d bypassed (guard switched off for the session)"
      % (len(rows), refused, bypassed))
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
    *rm*|*git*|*mkfs*|*dd*|*curl*|*wget*) ;;
    *) exit 0 ;;
  esac
fi

parser=""
read -r -d '' parser <<'PY' || true
import datetime
import hashlib
import json
import os
import re
import subprocess
import sys

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
from hook_state import direct_shell_segments


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


def record(rule, outcome, name, text):
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


try:
    doc = json.load(sys.stdin)
    command = doc["tool_input"]["command"]
    cwd = doc.get("cwd")
    if not isinstance(cwd, str) or not cwd:
        cwd = os.getcwd()
    found = verdict(command, cwd) if isinstance(command, str) else None
    if found:
        rule, reason, instead, name, base = found
        record(rule, "bypassed" if mode == "bypass" else "refused", name, command)
        if mode != "bypass":
            print("%s. Instead: %s." % (reason, instead))
except Exception:
    pass
PY

child=0
[ "${OMS_HARNESS_CHILD:-}" != 1 ] || child=1
reason="$(printf '%s' "$payload" | python3 -I -c "$parser" "$ROOT/scripts/lib" "$child" "$mode" "$AUDIT_LOG" 2>/dev/null || true)"
[ -n "$reason" ] || exit 0
echo "command guard: ${reason%%$'\n'*} If a person means it, they run it in their own shell (or start the session with OMS_COMMAND_GUARD=0)." >&2
exit 2
