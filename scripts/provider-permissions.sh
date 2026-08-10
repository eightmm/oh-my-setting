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
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

MODE=check
PROFILE=consult
SETTINGS="${OMS_ANTIGRAVITY_SETTINGS:-$HOME/.gemini/antigravity-cli/settings.json}"
MANAGED=""
if [ -n "${OMS_DELEGATE_WORKTREE_ROOT:-}" ]; then
  WORKTREE_PARENT="$OMS_DELEGATE_WORKTREE_ROOT"
  WORKTREE_PARENT_EXPLICIT=1
else
  WORKTREE_PARENT="${XDG_CACHE_HOME:-$HOME/.cache}/oh-my-setting/worktrees"
  WORKTREE_PARENT_EXPLICIT=0
fi
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
Usage: provider-permissions.sh [--check | --print | --apply | --remove] [options]

Report or grant the standing permissions antigravity needs to act headlessly.
Codex and Claude Code carry their authority per invocation and are reported as
needing nothing.

Modes:
  --check          Report status; exit 1 when a required rule is missing
                   (default).
  --print          Print the missing rules, one per line, and change nothing.
  --apply          Add missing rules, keep a one-time .bak, and record only the
                   rules this command added so uninstall can revoke them.
  --remove         Revoke only rules recorded as added by this command. User
                   rules and settings changed after installation are kept.

Options:
  --profile NAME   consult (default): the peer reads the repository and
                   answers. Grants read_file(*) and command(*).
                   delegate: the above plus write access to the worktree
                   parent, so a peer-delegate worker can edit files.
  --allow-command C
                   Grant unsandboxed(C) — permission for command C to leave the
                   sandbox. C is one executable-name token, not a command line
                   or wildcard. Repeatable, delegate profile only. Needed by
                   tools that write outside the worktree, e.g. `uv` (its cache
                   lives in $HOME, so a cold `uv run` is denied without this).
  --allow-toolchains
                   Grant unsandboxed() for the package managers in the built-in
                   list that are installed on this machine (uv, npm, npx,
                   cargo, pnpm, yarn, go, poetry). Nothing is granted for a tool
                   that is absent. pip, conda, docker, make, node, and git are
                   deliberately not in the list — see the comment in the script
                   for why each one is out.
  --worktree-parent PATH
                   Absolute parent where peer-delegate builds worktrees. The
                   path is normalized before an exact rule is emitted. Default:
                   $XDG_CACHE_HOME/oh-my-setting/worktrees, else
                   ~/.cache/oh-my-setting/worktrees. The default is created
                   private (0700) by --apply. OMS_DELEGATE_WORKTREE_ROOT is the
                   environment override.
  --settings PATH  Antigravity settings file. Default:
                   ~/.gemini/antigravity-cli/settings.json
                   (OMS_ANTIGRAVITY_SETTINGS overrides).

Why these and not a list of commands: a `command` rule is matched against the
whole command line, so a curated list is denied by the first `cd x && rg y` a
peer runs. command(*) is not the write authority it looks like — this harness
always invokes agy with --sandbox, under which a granted command still cannot
write outside the workspace. Escaping needs `unsandboxed`, which is why that
one is per-command and never granted wholesale.

Antigravity currently has no stable rule that scopes MCP access to one server.
Its all-MCP wildcard authorizes every configured MCP server and tool, so the
harness never adds it as a standing permission. The OMS MCP remains available
to interactive sessions that can approve it explicitly.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) MODE=check; shift ;;
    --print) MODE=print; shift ;;
    --apply) MODE=apply; shift ;;
    --remove) MODE=remove; shift ;;
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
        # This value lands inside unsandboxed(...). Accept one executable-name
        # token only: no arguments and no rule-language metacharacters.
        [A-Za-z0-9]*) ;;
        *) fail "--allow-command needs one executable-name token" ;;
      esac
      case "$2" in
        *[!A-Za-z0-9._+-]*)
          fail "--allow-command needs one executable-name token" ;;
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
      WORKTREE_PARENT="$2"; WORKTREE_PARENT_EXPLICIT=1; shift 2 ;;
    --settings)
      [ "$#" -ge 2 ] || fail "--settings requires a path"
      SETTINGS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1 (try --help)" ;;
  esac
done

# WORKTREE_PARENT lands inside write_file(...). Normalize it as a POSIX path
# even under Windows Python (Git Bash passes POSIX paths to Antigravity), and
# reject every character that can terminate or widen the rule. NUL cannot be
# present in an argv value; all other control characters are rejected below.
command -v python3 >/dev/null 2>&1 ||
  fail "python3 is required to validate provider permission paths"
# On a Windows host, HOME/XDG/TMPDIR-derived defaults arrive in the native or
# mixed spelling (C:/...), which the absolute-path check below rightly
# rejects — so every --apply and --check died before touching the settings.
# Convert to the POSIX spelling the rule text wants before validating.
case "$(uname -s 2>/dev/null || true)" in
  MINGW*|MSYS*|CYGWIN*)
    if command -v cygpath >/dev/null 2>&1; then
      WORKTREE_PARENT="$(cygpath -u "$WORKTREE_PARENT" | tr -d '\r')"
    fi
    ;;
esac
# The value crosses to a possibly native Windows Python. Git-for-Windows
# converts environment values that look like POSIX paths when spawning
# native binaries, so /tmp/... arrives as C:\... and the absolute-path check
# below rejected every default on Windows. Base64 does not look like a path,
# so it crosses untouched; python's stdout crosses back unconverted.
worktree_parent_b64="$(printf '%s' "$WORKTREE_PARENT" | base64 | tr -d '\r\n')"
if normalized_worktree_parent="$(OMS_PP_WORKTREE_PARENT_B64="$worktree_parent_b64" python3 - <<'PY'
import base64
import os
import posixpath
import sys
import unicodedata

raw = base64.b64decode(
    os.environ["OMS_PP_WORKTREE_PARENT_B64"].encode("ascii")).decode("utf-8")
if not raw.startswith("/"):
    print("error: --worktree-parent must be an absolute path: %r" % raw,
          file=sys.stderr)
    sys.exit(2)
if any(ord(char) < 32 or ord(char) == 127 or
       unicodedata.category(char) in ("Zl", "Zp") for char in raw):
    print("error: --worktree-parent must be a single-line path", file=sys.stderr)
    sys.exit(2)
if any(char in raw for char in "()*"):
    print("error: --worktree-parent contains a permission-rule metacharacter",
          file=sys.stderr)
    sys.exit(2)
normalized = posixpath.normpath("/" + raw.lstrip("/"))
if not normalized.startswith("/"):
    print("error: --worktree-parent must be an absolute path", file=sys.stderr)
    sys.exit(2)
print(normalized)
PY
)"; then
  WORKTREE_PARENT="${normalized_worktree_parent//$'\r'/}"
else
  exit $?
fi

# Canonical aliases of the same settings file must share both one transaction
# lock and one ownership sidecar. Resolve with POSIX paths and pwd -P rather
# than returning a native-Windows path from Python to Git Bash. Missing suffixes
# stay lexical below the nearest physical existing parent.
oms_pp_canonical_settings_path() {
  local raw="$1"
  local normalized=""
  local cursor=""
  local suffix=""
  local part=""
  local leaf=""
  local physical=""
  local candidate=""
  local target=""
  local links=0

  case "$raw" in
    /*) ;;
    *) raw="$PWD/$raw" ;;
  esac
  while :; do
    normalized="$(OMS_PP_PATH="$raw" python3 - <<'PY'
import os
import posixpath
print(posixpath.normpath("/" + os.environ["OMS_PP_PATH"].lstrip("/")))
PY
)" || return $?
    normalized="${normalized//$'\r'/}"
    cursor="$(dirname "$normalized")"
    leaf="$(basename "$normalized")"
    suffix=""
    while [ ! -d "$cursor" ]; do
      part="$(basename "$cursor")"
      suffix="/$part$suffix"
      [ "$cursor" != "$(dirname "$cursor")" ] || return 1
      cursor="$(dirname "$cursor")"
    done
    physical="$(cd "$cursor" && pwd -P)" || return 1
    physical="${physical//$'\r'/}"
    candidate="$(OMS_PP_PATH="$physical$suffix/$leaf" python3 - <<'PY'
import os
import posixpath
print(posixpath.normpath("/" + os.environ["OMS_PP_PATH"].lstrip("/")))
PY
)" || return $?
    candidate="${candidate//$'\r'/}"
    if [ ! -L "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
    links=$((links + 1))
    [ "$links" -le 40 ] || return 1
    target="$(readlink "$candidate")" || return 1
    target="${target//$'\r'/}"
    case "$target" in
      /*) raw="$target" ;;
      *) raw="$(dirname "$candidate")/$target" ;;
    esac
  done
}

if canonical_settings="$(oms_pp_canonical_settings_path "$SETTINGS")"; then
  SETTINGS="$canonical_settings"
else
  fail "could not canonicalize Antigravity settings path: $SETTINGS"
fi
# The canonical path is POSIX (/c/...) under Git Bash, and it reaches a
# possibly native Windows Python through the environment — where Git Bash
# converts argv but not env values. Unconverted, python resolves /c/...
# against the current drive and reads and writes a ghost settings file while
# the real one stays untouched. The mixed form (C:/...) names the same file
# to bash, MSYS python, and native python alike. After canonicalization on
# purpose: the canonicalizer's cd/pwd walk expects the POSIX spelling.
case "$(uname -s 2>/dev/null || true)" in
  MINGW*|MSYS*|CYGWIN*)
    if command -v cygpath >/dev/null 2>&1; then
      SETTINGS="$(cygpath -m "$SETTINGS" | tr -d '\r')"
    fi
    ;;
esac

# Keep ownership state beside the canonical settings path being widened. This is
# intentionally separate from the install receipt: provider-permissions can be
# used standalone, and uninstall must never restore a whole stale backup over
# user edits made after installation.
MANAGED="$SETTINGS.oh-my-setting-permissions.json"

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

if [ "$MODE" = apply ] && [ "$PROFILE" = delegate ] &&
    [ "$WORKTREE_PARENT_EXPLICIT" = 0 ]; then
  [ ! -L "$WORKTREE_PARENT" ] ||
    fail "default delegate worktree root must not be a symbolic link: $WORKTREE_PARENT"
  old_umask="$(umask)"
  umask 077
  mkdir -p "$WORKTREE_PARENT" ||
    fail "could not create default delegate worktree root: $WORKTREE_PARENT"
  chmod 700 "$WORKTREE_PARENT" ||
    fail "could not make default delegate worktree root private: $WORKTREE_PARENT"
  umask "$old_umask"
fi

remove_managed_permissions() {
  local removed status rule

  if [ ! -e "$MANAGED" ] && [ ! -L "$MANAGED" ]; then
    echo "ok: managed antigravity permissions already absent"
    return 0
  fi
  [ ! -L "$MANAGED" ] ||
    fail "managed permission state must not be a symbolic link: $MANAGED"
  command -v python3 >/dev/null 2>&1 ||
    fail "python3 is required to remove managed antigravity permissions"

  # The sidecar is the ownership boundary. Each recorded rule accounts for at
  # most one equal list entry, so a same-named grant appended later by the user
  # remains. A tiny removal journal makes a failed write/unlink retry-safe.
  if removed="$(OMS_PP_SETTINGS="$SETTINGS" OMS_PP_MANAGED="$MANAGED" python3 - <<'PY'
import hashlib
import json
import os
import stat
import sys
import tempfile

settings = os.environ["OMS_PP_SETTINGS"]
managed = os.environ["OMS_PP_MANAGED"]


def atomic_bytes(path, payload, follow_symlink=False):
    target = os.path.realpath(path) if follow_symlink and os.path.islink(path) else path
    parent = os.path.dirname(target) or "."
    os.makedirs(parent, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".%s." % os.path.basename(target), dir=parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        if os.path.exists(target):
            os.chmod(temporary, stat.S_IMODE(os.stat(target).st_mode))
        os.replace(temporary, target)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def json_bytes(value):
    return (json.dumps(value, indent=2) + "\n").encode("utf-8")


def digest(payload):
    return hashlib.sha256(payload).hexdigest()


def load_state():
    if os.path.islink(managed):
        raise ValueError("managed permission state %s must not be a symbolic link" % managed)
    try:
        with open(managed, encoding="utf-8") as handle:
            state = json.load(handle)
    except (OSError, ValueError) as exc:
        raise ValueError("managed permission state %s is unreadable: %s" % (managed, exc))
    if not isinstance(state, dict) or state.get("schema") != 1:
        raise ValueError("managed permission state %s has an unsupported schema" % managed)
    rules = state.get("rules")
    if not isinstance(rules, list) or any(
            not isinstance(rule, str) or not rule for rule in rules):
        raise ValueError("managed permission state %s has invalid rules" % managed)
    # Never let malformed duplicate ownership delete more than one occurrence.
    unique = []
    for rule in rules:
        if rule not in unique:
            unique.append(rule)
    journal = state.get("removal")
    if journal is not None:
        if not isinstance(journal, dict):
            raise ValueError("managed permission state %s has an invalid removal journal" % managed)
        for key in ("before_sha256", "after_sha256"):
            value = journal.get(key)
            if not isinstance(value, str) or len(value) != 64 or any(
                    char not in "0123456789abcdef" for char in value):
                raise ValueError("managed permission state %s has an invalid removal journal" % managed)
    return state, unique, journal


def load_settings():
    if os.path.islink(settings):
        raise ValueError("Antigravity settings changed during permission removal "
                         "(the settings target changed)")
    try:
        with open(settings, "rb") as handle:
            raw = handle.read()
    except FileNotFoundError:
        return None, None, None
    except OSError as exc:
        raise ValueError("Antigravity settings %s are unreadable: %s" % (settings, exc))
    try:
        cfg = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise ValueError("Antigravity settings %s are not valid JSON: %s" % (settings, exc))
    if not isinstance(cfg, dict):
        raise ValueError("Antigravity settings %s must contain a JSON object" % settings)
    permissions = cfg.get("permissions")
    if permissions is None:
        allow = []
    elif not isinstance(permissions, dict):
        raise ValueError("Antigravity settings %s have invalid permissions" % settings)
    else:
        allow = permissions.get("allow", [])
        if not isinstance(allow, list):
            raise ValueError("Antigravity settings %s have an invalid permissions.allow" % settings)
    return raw, cfg, allow


try:
    state, rules, journal = load_state()
    raw, cfg, allow = load_settings()

    if journal is not None:
        if raw is None:
            raise ValueError("managed permission removal cannot resume because settings are missing")
        current = digest(raw)
        if current == journal["after_sha256"]:
            os.unlink(managed)
            sys.exit(0)
        if current != journal["before_sha256"]:
            raise ValueError("managed permission removal cannot resume because settings changed")

    if cfg is None:
        os.unlink(managed)
        sys.exit(0)

    removed = []
    for rule in rules:
        try:
            index = allow.index(rule)
        except ValueError:
            continue
        del allow[index]
        removed.append(rule)

    if removed:
        after = json_bytes(cfg)
        if journal is None:
            state["removal"] = {
                "before_sha256": digest(raw),
                "after_sha256": digest(after),
            }
            atomic_bytes(managed, json_bytes(state))
        elif digest(after) != journal["after_sha256"]:
            raise ValueError("managed permission removal journal no longer matches settings")

        # Do not overwrite an edit that raced the ownership-journal write.
        if os.path.islink(settings):
            raise ValueError("Antigravity settings changed during permission removal "
                             "(the settings target changed)")
        with open(settings, "rb") as handle:
            if digest(handle.read()) != state["removal"]["before_sha256"]:
                raise ValueError("Antigravity settings changed during permission removal")
        atomic_bytes(settings, after)

    os.unlink(managed)
    for rule in removed:
        print(rule)
except (OSError, ValueError) as exc:
    print("error: managed permission state was not removed safely: %s" % exc, file=sys.stderr)
    sys.exit(1)
PY
)"; then
    :
  else
    status=$?
    return "$status"
  fi

  # Windows Python may return CRLF through command substitution.
  removed="${removed//$'\r'/}"
  while IFS= read -r rule; do
    [ -n "$rule" ] || continue
    echo "removed: $rule"
  done <<< "$removed"
  echo "provider-permissions: removed managed Antigravity grants"
}

if [ "$MODE" = remove ]; then
  oms_with_file_lock "$SETTINGS" remove_managed_permissions
  exit $?
fi

if ! command -v agy >/dev/null 2>&1; then
  echo "ok: antigravity is not installed; nothing to grant"
  echo "note: codex and claude take authority per invocation and need no allow-list"
  exit 0
fi
command -v python3 >/dev/null 2>&1 ||
  fail "python3 is required to inspect antigravity permissions"

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
        cfg = json.load(handle)
except FileNotFoundError:
    cfg = {}
except (OSError, ValueError):
    print("UNREADABLE")
    sys.exit(0)
if not isinstance(cfg, dict):
    print("UNREADABLE")
    sys.exit(0)
permissions = cfg.get("permissions", {})
if not isinstance(permissions, dict):
    print("UNREADABLE")
    sys.exit(0)
allow = permissions.get("allow", [])
if not isinstance(allow, list):
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
# Windows Python may return CRLF through command substitution.
missing="${missing//$'\r'/}"

if [ "$missing" = "UNREADABLE" ]; then
  echo "warn: antigravity settings $SETTINGS is not readable JSON"
  exit 1
fi

if [ -z "$missing" ] && [ "$MODE" != apply ]; then
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

# --apply: widen the provider's authority, but remember only the exact list
# entries this invocation owns. The backup is forensic/recovery context only;
# uninstall performs a surgical removal and never restores it over later edits.
apply_managed_permissions() {
  local added=""
  local status=0

  command -v python3 >/dev/null 2>&1 ||
    fail "python3 is required to grant managed antigravity permissions"
  if added="$(OMS_PP_SETTINGS="$SETTINGS" OMS_PP_MANAGED="$MANAGED" \
    OMS_PP_REQUIRED="$REQUIRED" python3 - <<'PY'
import io
import json
import os
import shutil
import stat
import sys
import tempfile

settings = os.environ["OMS_PP_SETTINGS"]
managed = os.environ["OMS_PP_MANAGED"]
required = [
    rule.strip() for rule in os.environ["OMS_PP_REQUIRED"].split("\n")
    if rule.strip()
]


def resolve_settings_target():
    # Bash already supplied one physical POSIX path. Replacing its final leaf
    # with a symlink during this transaction is a concurrent retarget.
    if os.path.islink(settings):
        raise ValueError("Antigravity settings changed during permission apply "
                         "(the settings target changed)")
    return settings


def read_settings_bytes(target):
    try:
        with open(target, "rb") as handle:
            return handle.read()
    except FileNotFoundError:
        return None


def assert_settings_unchanged(target, expected):
    if resolve_settings_target() != target:
        raise ValueError("Antigravity settings changed during permission apply "
                         "(the settings target changed)")
    try:
        current = read_settings_bytes(target)
    except OSError as exc:
        raise ValueError("Antigravity settings changed during permission apply: %s" % exc)
    if current != expected:
        raise ValueError("Antigravity settings changed during permission apply")


def atomic_json(path, value, follow_symlink=False):
    target = os.path.realpath(path) if follow_symlink and os.path.islink(path) else path
    parent = os.path.dirname(target) or "."
    os.makedirs(parent, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".%s." % os.path.basename(target), dir=parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
            json.dump(value, handle, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        if os.path.exists(target):
            os.chmod(temporary, stat.S_IMODE(os.stat(target).st_mode))
        os.replace(temporary, target)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def load_settings():
    target = resolve_settings_target()
    existed = os.path.exists(target)
    if not existed:
        cfg = {"permissions": {"allow": []}}
        return cfg, cfg["permissions"]["allow"], False, None, None, target
    try:
        with open(target, "rb") as handle:
            raw = handle.read()
        cfg = json.loads(raw.decode("utf-8"))
        source_mode = stat.S_IMODE(os.stat(target).st_mode)
    except (OSError, UnicodeDecodeError, ValueError) as exc:
        raise ValueError("Antigravity settings %s are not valid readable JSON: %s" %
                         (settings, exc))
    if not isinstance(cfg, dict):
        raise ValueError("Antigravity settings %s must contain a JSON object" % settings)
    if "permissions" not in cfg:
        cfg["permissions"] = {}
    elif not isinstance(cfg["permissions"], dict):
        raise ValueError("Antigravity settings %s have invalid permissions" % settings)
    permissions = cfg["permissions"]
    if "allow" not in permissions:
        permissions["allow"] = []
    elif not isinstance(permissions["allow"], list):
        raise ValueError("Antigravity settings %s have an invalid permissions.allow" % settings)
    return cfg, permissions["allow"], True, raw, source_mode, target


def load_state():
    if not os.path.lexists(managed):
        return {"schema": 1, "rules": []}, False
    if os.path.islink(managed):
        raise ValueError("managed permission state %s must not be a symbolic link" % managed)
    try:
        with open(managed, encoding="utf-8") as handle:
            state = json.load(handle)
    except (OSError, ValueError) as exc:
        raise ValueError("managed permission state %s is unreadable: %s" % (managed, exc))
    if not isinstance(state, dict) or state.get("schema") != 1:
        raise ValueError("managed permission state %s has an unsupported schema" % managed)
    rules = state.get("rules")
    if not isinstance(rules, list) or any(
            not isinstance(rule, str) or not rule for rule in rules):
        raise ValueError("managed permission state %s has invalid rules" % managed)
    if state.get("removal") is not None:
        raise ValueError("managed permission removal is incomplete; rerun --remove first")
    unique = []
    for rule in rules:
        if rule not in unique:
            unique.append(rule)
    state["rules"] = unique
    return state, True


def satisfied(rule, have):
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


try:
    cfg, allow, settings_existed, settings_raw, source_mode, settings_target = load_settings()
    state, state_existed = load_state()
    have = {rule for rule in allow if isinstance(rule, str)}
    added = []
    for rule in required:
        if satisfied(rule, have):
            continue
        allow.append(rule)
        have.add(rule)
        added.append(rule)

    if added:
        # Optimistic compare-and-swap: no stale settings snapshot may cross
        # the ownership or settings write boundary.
        assert_settings_unchanged(settings_target, settings_raw)
        backup = settings + ".bak"
        if settings_existed:
            try:
                backup_fd = os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                                    source_mode)
            except FileExistsError:
                pass
            else:
                try:
                    with io.BytesIO(settings_raw) as source, os.fdopen(backup_fd, "wb") as target:
                        shutil.copyfileobj(source, target)
                        target.flush()
                        os.fsync(target.fileno())
                    os.chmod(backup, source_mode)
                except Exception:
                    try:
                        os.close(backup_fd)
                    except OSError:
                        pass
                    try:
                        os.unlink(backup)
                    except OSError:
                        pass
                    raise

        assert_settings_unchanged(settings_target, settings_raw)

        owned = list(state["rules"])
        for rule in added:
            if rule not in owned:
                owned.append(rule)
        if not state_existed or owned != state["rules"]:
            state["rules"] = owned
            # Ownership lands first. If the settings write then fails, a retry
            # can safely finish adding the still-missing rule.
            atomic_json(managed, state)
        assert_settings_unchanged(settings_target, settings_raw)
        atomic_json(settings_target, cfg)

    for rule in added:
        print(rule)
except (OSError, ValueError) as exc:
    print("error: managed Antigravity permissions were not granted safely: %s" % exc,
          file=sys.stderr)
    sys.exit(1)
PY
)"; then
    :
  else
    status=$?
    return "$status"
  fi

  # Windows Python may return CRLF through command substitution.
  added="${added//$'\r'/}"
  printf '%s\n' "$added" | while IFS= read -r rule; do
    [ -n "$rule" ] || continue
    echo "granted: $rule"
  done
  [ ! -f "$SETTINGS.bak" ] || echo "previous settings: $SETTINGS.bak"
  if [ -z "$added" ]; then
    echo "ok: antigravity headless read permissions"
    [ "$PROFILE" != delegate ] || echo "ok: antigravity delegate write permissions"
  fi
  echo "provider-permissions: ok ($PROFILE)"
}

oms_with_file_lock "$SETTINGS" apply_managed_permissions
