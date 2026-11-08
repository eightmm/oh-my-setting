#!/usr/bin/env bash
[ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || { echo "error: oh-my-setting needs Bash 5 or newer (running ${BASH_VERSION:-unknown}); on macOS run: brew install bash, then rerun" >&2; exit 2; }

# PreToolUse hook (Bash): a panel main must not move the canonical checkout's
# index or refs while OMS calls are live. The worker guard snapshots that index
# and stops every live worker the moment it changes; a `git merge`, `checkout`
# or `commit` run there by the main cost a 40-minute builder on 10-22, 10-26
# and 11-02. `oms sync-shared` (and `oms land --sync-shared`) defers with exit
# 75 until the calls finish, so the deny names that path.
#
# Acts only for a panel main (the fail-ledger hook's gate, in the acting
# direction); every other session exits 0 after one environment check. Liveness
# is the canonical-sync registry, the same one the worker guard and sync-shared
# read: peer-delegate (panel dispatch included) and peer-review register there
# for the whole call.
#
# This is a guardrail against accidents, not a security boundary. It fails open:
# an unparsable payload, a missing helper, an unresolved directory, a command
# spelled through a variable, a subshell or a heredoc the tokenizer cannot see
# all exit 0.

[ -n "${OMS_PANEL_MAIN_ATTEMPT:-}" ] || exit 0
[ "${OMS_HARNESS_CHILD:-}" != 1 ] || exit 0
case "${OMS_PANEL_SESSION:-}" in oms-*) ;; *) exit 0 ;; esac

ROOT="${BASH_SOURCE[0]%/*}/.."
payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

# Shell-only precheck so a non-git command never starts an interpreter. The
# payload's cwd often contains "git", so only the command string is tested; a
# payload this regex cannot read unambiguously falls through to the parser.
command_re='"command"[[:space:]]*:[[:space:]]*"(([^"\\]|\\.)*)"'
if [[ "$payload" =~ $command_re ]]; then
  command_text="${BASH_REMATCH[1]}"
  case "$command_text" in
    *'\u'*) ;;
    *git*) ;;
    *) exit 0 ;;
  esac
fi

parser=""
read -r -d '' parser <<'PY' || true
import json
import os
import re
import sys

MOVING = frozenset(("merge", "pull", "checkout", "switch", "reset", "commit",
                    "rebase", "cherry-pick", "am", "revert"))
OPTION_VALUE = frozenset(("-c", "--git-dir", "--work-tree", "--namespace",
                          "--super-prefix", "--config-env"))
WRAPPERS = frozenset(("command", "exec", "time", "nohup", "sudo", "env"))
sys.path.insert(0, sys.argv[1])
from hook_state import direct_shell_segments


def resolve(base, target):
    if base is None:
        return None
    target = os.path.expandvars(os.path.expanduser(target))
    if not target or target == "-" or "$" in target or "`" in target or "\n" in target:
        return None
    return os.path.join(base, target)


def moving(sub, rest):
    if sub in MOVING:
        return True
    if sub == "stash":
        return not (rest and rest[0] in ("list", "show"))
    if sub == "restore":
        return any(a == "--staged" or (a[:1] == "-" and a[:2] != "--" and "S" in a)
                   for a in rest)
    return False


def targets(text, cwd):
    found, cur = [], cwd
    for segment in direct_shell_segments(text):
        words = [w for w, is_op in segment if not is_op]
        i = 0
        while i < len(words) and (words[i] in WRAPPERS or
                                  re.match(r"[A-Za-z_][A-Za-z0-9_]*=", words[i])):
            i += 1
        if i >= len(words):
            continue
        word = words[i]
        if word in ("cd", "pushd"):
            i += 1
            while i < len(words) and words[i] in ("-P", "-L", "-e", "--"):
                i += 1
            cur = resolve(cur, words[i] if i < len(words) else "~")
            continue
        if os.path.basename(word) not in ("git", "git.exe"):
            continue
        base, i = cur, i + 1
        while i < len(words):
            arg = words[i]
            if arg == "-C" and i + 1 < len(words):
                base, i = resolve(base, words[i + 1]), i + 2
            elif arg in OPTION_VALUE:
                i += 2
            elif arg.startswith("-"):
                i += 1
            else:
                break
        if i < len(words) and base and moving(words[i], words[i + 1:]):
            found.append(base)
    return found


try:
    doc = json.load(sys.stdin)
    command = doc["tool_input"]["command"]
    cwd = doc.get("cwd")
    if not isinstance(cwd, str) or not cwd:
        cwd = os.getcwd()
    seen = []
    for path in targets(command if isinstance(command, str) else "", cwd):
        if path not in seen:
            seen.append(path)
            print(path)
except Exception:
    pass
PY

targets="$(printf '%s' "$payload" | python3 -I -c "$parser" "$ROOT/scripts/lib" 2>/dev/null || true)"
[ -n "$targets" ] || exit 0

# Past this point a moving git verb is present; resolve whether it lands in the
# canonical checkout. Any helper that is missing ends the hook open.
[ -f "$ROOT/scripts/lib/agent-memory-common.sh" ] || exit 0
[ -f "$ROOT/scripts/lib/canonical-sync.sh" ] || exit 0
# shellcheck source=lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh" >/dev/null 2>&1 || exit 0
# shellcheck source=lib/canonical-sync.sh
. "$ROOT/scripts/lib/canonical-sync.sh" >/dev/null 2>&1 || exit 0

while IFS= read -r target; do
  target="${target//$'\r'/}"
  [ -n "$target" ] && [ -d "$target" ] || continue
  top="$(git -C "$target" rev-parse --show-toplevel 2>/dev/null)" || continue
  top="${top//$'\r'/}"
  top="$(cd "$top" 2>/dev/null && pwd -P)" || continue
  [ -n "$top" ] || continue
  # Canonical means a non-linked worktree: its git dir is the common dir.
  git_dir="$(oms_harness_git_path_physical "$top" --git-dir 2>/dev/null)" || continue
  common_dir="$(oms_harness_git_path_physical "$top" --git-common-dir 2>/dev/null)" || continue
  [ -n "$git_dir" ] && [ "${git_dir//$'\r'/}" = "${common_dir//$'\r'/}" ] || continue
  state_root="$(oms_state_root "$top" 2>/dev/null)" || continue
  state_root="$(cd "${state_root//$'\r'/}" 2>/dev/null && pwd -P)" || continue
  [ "$state_root" = "$top" ] || continue
  live="$(oms_canonical_sync_live_entries "$top" 2>/dev/null | grep -c . || true)"
  case "$live" in ""|*[!0-9]*|0) continue ;; esac
  echo "canonical checkout has $live live OMS call(s); moving its index or refs stops them. Use oms sync-shared (defers until they finish) or commit in a linked worktree." >&2
  exit 2
done <<EOF
$targets
EOF
exit 0
