#!/usr/bin/env bash
set -euo pipefail

# Check, and optionally grant, the standing permissions a provider CLI needs to
# work headlessly for this harness. Codex and Claude Code take their authority
# per invocation (--sandbox, --permission-mode), so they need nothing here.
# Antigravity does not: headless mode cannot prompt, so any tool outside
# permissions.allow is auto-denied and the CLI exits 0 having printed only its
# refusal — a peer that answers nothing, discovered only after a full call.
# This is the one setup step that used to live in a person's head.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"

MODE=check
PROFILE=consult
SETTINGS="${OMS_ANTIGRAVITY_SETTINGS:-$HOME/.gemini/antigravity-cli/settings.json}"
WORKTREE_PARENT="${TMPDIR:-/tmp}"
ALLOW_COMMANDS=""
ALLOW_FLAGS=""
TOOLCHAINS=0

# Package managers whose ordinary use writes outside the worktree — a registry
# cache under $HOME — so a worker hits the sandbox on a cold cache and stops.
# Deliberately short. Everything here can also run arbitrary code (`npm run`,
# `cargo test`), so each entry is a real grant, justified by the tool being
# unusable without it rather than by being convenient to have.
# Not here on purpose: pip and conda (this setup uses uv, and granting them
# would contradict that), docker (its socket is root on most machines), make
# and node (they write in-tree; no cache to reach for), git (a worker writing
# to the main repo's .git is the thing the worktree exists to prevent).
OMS_TOOLCHAIN_COMMANDS="uv npm npx cargo pnpm yarn go poetry"

fail() {
  echo "error: $*" >&2
  exit 2
}

usage() {
  cat <<'EOF'
Usage: provider-permissions.sh [--check | --print | --apply] [options]

Report or grant the standing permissions antigravity needs to act headlessly.
Codex and Claude Code carry their authority per invocation and are reported as
needing nothing.

Modes:
  --check          Report status; exit 1 when a required rule is missing
                   (default).
  --print          Print the missing rules, one per line, and change nothing.
  --apply          Add the missing rules to the settings file, after writing a
                   .bak copy beside it. Never removes or narrows a rule.

Options:
  --profile NAME   consult (default): the peer reads the repository and
                   answers. Grants read_file(*) and command(*).
                   delegate: the above plus write access to the worktree
                   parent, so a peer-delegate worker can edit files.
  --allow-command C
                   Grant unsandboxed(C) — permission for command C to leave the
                   sandbox. Repeatable, delegate profile only. Needed by tools
                   that write outside the worktree, e.g. `uv` (its cache lives
                   in $HOME, so a cold `uv run` is denied without this).
  --allow-toolchains
                   Grant unsandboxed() for the package managers in the built-in
                   list that are installed on this machine (uv, npm, npx,
                   cargo, pnpm, yarn, go, poetry). Nothing is granted for a tool
                   that is absent. pip, conda, docker, make, node, and git are
                   deliberately not in the list — see the comment in the script
                   for why each one is out.
  --worktree-parent PATH
                   Where peer-delegate builds worktrees. Default: $TMPDIR, else
                   /tmp.
  --settings PATH  Antigravity settings file. Default:
                   ~/.gemini/antigravity-cli/settings.json
                   (OMS_ANTIGRAVITY_SETTINGS overrides).

Why these and not a list of commands: a `command` rule is matched against the
whole command line, so a curated list is denied by the first `cd x && rg y` a
peer runs. command(*) is not the write authority it looks like — this harness
always invokes agy with --sandbox, under which a granted command still cannot
write outside the workspace. Escaping needs `unsandboxed`, which is why that
one is per-command and never granted wholesale.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) MODE=check; shift ;;
    --print) MODE=print; shift ;;
    --apply) MODE=apply; shift ;;
    --profile)
      [ "$#" -ge 2 ] || fail "--profile requires a value"
      case "$2" in
        consult|delegate) PROFILE="$2" ;;
        *) fail "--profile must be consult or delegate" ;;
      esac
      shift 2 ;;
    --allow-command)
      [ "$#" -ge 2 ] || fail "--allow-command requires a command"
      case "$2" in
        # A wildcard escape is the whole sandbox, granted forever, from a flag
        # that reads like a small thing. Name the command.
        "*"|"") fail "--allow-command needs a specific command, not a wildcard" ;;
      esac
      ALLOW_COMMANDS="${ALLOW_COMMANDS:+$ALLOW_COMMANDS
}$2"
      ALLOW_FLAGS="$ALLOW_FLAGS --allow-command $2"
      shift 2 ;;
    --allow-toolchains)
      TOOLCHAINS=1
      ALLOW_FLAGS="$ALLOW_FLAGS --allow-toolchains"
      shift ;;
    --worktree-parent)
      [ "$#" -ge 2 ] || fail "--worktree-parent requires a path"
      WORKTREE_PARENT="$2"; shift 2 ;;
    --settings)
      [ "$#" -ge 2 ] || fail "--settings requires a path"
      SETTINGS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1 (try --help)" ;;
  esac
done

if [ -n "$ALLOW_COMMANDS" ] && [ "$PROFILE" != delegate ]; then
  fail "--allow-command applies to --profile delegate; a consult peer never writes"
fi
if [ "$TOOLCHAINS" = 1 ]; then
  [ "$PROFILE" = delegate ] ||
    fail "--allow-toolchains applies to --profile delegate; a consult peer never builds"
  # Only what this machine actually has: granting an escape to a tool that is
  # not installed buys nothing and outlives the reason it was added.
  for toolchain in $OMS_TOOLCHAIN_COMMANDS; do
    command -v "$toolchain" >/dev/null 2>&1 || continue
    case "
$ALLOW_COMMANDS
" in
      *"
$toolchain
"*) continue ;;
    esac
    ALLOW_COMMANDS="${ALLOW_COMMANDS:+$ALLOW_COMMANDS
}$toolchain"
  done
fi

if ! command -v agy >/dev/null 2>&1; then
  echo "ok: antigravity is not installed; nothing to grant"
  echo "note: codex and claude take authority per invocation and need no allow-list"
  exit 0
fi

required() {
  printf 'read_file(*)\n'
  printf 'command(*)\n'
  if [ "$PROFILE" = delegate ]; then
    printf 'write_file(%s)\n' "$WORKTREE_PARENT"
    printf '%s\n' "$ALLOW_COMMANDS" | while IFS= read -r cmd; do
      [ -n "$cmd" ] || continue
      printf 'unsandboxed(%s)\n' "$cmd"
    done
  fi
}

REQUIRED="$(required)"

missing="$(OMS_PP_SETTINGS="$SETTINGS" OMS_PP_REQUIRED="$REQUIRED" python3 - <<'PY'
import json, os, sys

path = os.environ["OMS_PP_SETTINGS"]
try:
    with open(path, encoding="utf-8") as handle:
        allow = json.load(handle).get("permissions", {}).get("allow", [])
except FileNotFoundError:
    allow = []
except (OSError, ValueError):
    print("UNREADABLE")
    sys.exit(0)
have = {rule for rule in allow if isinstance(rule, str)}
# A directory target grants write recursively, and a wildcard covers any
# target, so an existing broader rule already satisfies a narrower requirement.
def satisfied(rule):
    if rule in have:
        return True
    namespace, _, target = rule[:-1].partition("(")
    if "%s(*)" % namespace in have:
        return True
    if namespace in ("read_file", "write_file"):
        parts = target.rstrip("/").split("/")
        while len(parts) > 1:
            parts.pop()
            if "%s(%s)" % (namespace, "/".join(parts) or "/") in have:
                return True
    return False

for rule in os.environ["OMS_PP_REQUIRED"].split("\n"):
    rule = rule.strip()
    if rule and not satisfied(rule):
        print(rule)
PY
)"

if [ "$missing" = "UNREADABLE" ]; then
  echo "warn: antigravity settings $SETTINGS is not readable JSON"
  exit 1
fi

if [ -z "$missing" ]; then
  echo "ok: antigravity headless read permissions"
  [ "$PROFILE" != delegate ] || echo "ok: antigravity delegate write permissions"
  exit 0
fi

case "$MODE" in
  print)
    printf '%s\n' "$missing"
    exit 0
    ;;
  check)
    echo "warn: antigravity will be denied these headlessly: $(printf '%s' "$missing" | tr '\n' ' ')"
    echo "warn: add these under permissions.allow in $SETTINGS"
    # The hint carries the flags, or following it grants a different set than
    # the one just reported missing.
    echo "warn: or run: oms provider-permissions --apply --profile $PROFILE$ALLOW_FLAGS"
    exit 1
    ;;
esac

# --apply: widening what another program is allowed to do, so the previous
# state is kept where a person can find it.
mkdir -p "$(dirname "$SETTINGS")"
[ ! -f "$SETTINGS" ] || cp "$SETTINGS" "$SETTINGS.bak"
OMS_PP_SETTINGS="$SETTINGS" OMS_PP_MISSING="$missing" python3 - <<'PY'
import json, os

path = os.environ["OMS_PP_SETTINGS"]
try:
    with open(path, encoding="utf-8") as handle:
        cfg = json.load(handle)
except FileNotFoundError:
    cfg = {}
allow = cfg.setdefault("permissions", {}).setdefault("allow", [])
for rule in os.environ["OMS_PP_MISSING"].split("\n"):
    rule = rule.strip()
    if rule and rule not in allow:
        allow.append(rule)
with open(path, "w", encoding="utf-8") as handle:
    json.dump(cfg, handle, indent=2)
    handle.write("\n")
PY

printf '%s\n' "$missing" | while IFS= read -r rule; do
  [ -n "$rule" ] || continue
  echo "granted: $rule"
done
[ ! -f "$SETTINGS.bak" ] || echo "previous settings: $SETTINGS.bak"
echo "provider-permissions: ok ($PROFILE)"
