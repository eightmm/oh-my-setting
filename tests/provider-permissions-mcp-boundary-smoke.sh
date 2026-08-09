#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-provider-mcp.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM

fail() {
  echo "provider-permissions-mcp-boundary-smoke: $*" >&2
  exit 1
}

mkdir -p "$TMP/bin"
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/agy"
chmod +x "$TMP/bin/agy"

out="$(PATH="$TMP/bin:$PATH" "$ROOT/scripts/provider-permissions.sh" \
  --print --profile consult --settings "$TMP/settings.json")" ||
  fail "consult permission projection failed"

printf '%s\n' "$out" | grep -Fxq 'read_file(*)' ||
  fail "consult profile lost repository reads"
printf '%s\n' "$out" | grep -Fxq 'command(*)' ||
  fail "consult profile lost sandboxed commands"
if printf '%s\n' "$out" | grep -Fq 'mcp(*)'; then
  fail "consult profile must not grant every MCP server and tool"
fi

help="$($ROOT/scripts/provider-permissions.sh --help)" ||
  fail "permission help failed"
if printf '%s\n' "$help" | grep -Fq 'mcp(*)'; then
  fail "permission consent must not advertise an all-MCP grant"
fi

assert_unsafe_input_rejected() {
  local label="$1"
  shift
  local out rc=0

  out="$(PATH="$TMP/bin:$PATH" "$ROOT/scripts/provider-permissions.sh" \
    --print --profile delegate --settings "$TMP/settings.json" "$@" 2>&1)" || rc=$?
  [ "$rc" = 2 ] ||
    fail "$label must be rejected before it can become a rule (status $rc): $out"
  if printf '%s\n' "$out" | grep -Eq '^(write_file|unsandboxed)\('; then
    fail "$label emitted an injected permission rule: $out"
  fi
}

# Both values are interpolated into Antigravity's rule language. They must not
# be able to close the current rule, add a wildcard, or add a second rule.
assert_unsafe_input_rejected "command rule terminator" \
  --allow-command 'uv)'
assert_unsafe_input_rejected "command wildcard" \
  --allow-command 'uv*'
assert_unsafe_input_rejected "command with arguments" \
  --allow-command 'uv run'
assert_unsafe_input_rejected "command newline" \
  --allow-command $'uv\nnpm'
assert_unsafe_input_rejected "command second-rule injection" \
  --allow-command $'uv)\nwrite_file(*)'
assert_unsafe_input_rejected "relative worktree parent" \
  --worktree-parent 'tmp/worktrees'
assert_unsafe_input_rejected "worktree rule terminator" \
  --worktree-parent '/tmp/worktrees)'
assert_unsafe_input_rejected "worktree wildcard" \
  --worktree-parent '/tmp/*'
assert_unsafe_input_rejected "worktree newline" \
  --worktree-parent $'/tmp/worktrees\n/tmp/delegates'
assert_unsafe_input_rejected "worktree second-rule injection" \
  --worktree-parent $'/tmp/worktrees)\nunsandboxed(*)'
assert_unsafe_input_rejected "worktree unicode newline injection" \
  --worktree-parent $'/tmp/worktrees\u2028unsandboxed(*)'

out="$(PATH="$TMP/bin:$PATH" "$ROOT/scripts/provider-permissions.sh" \
  --print --profile delegate --allow-command 'python3.12' \
  --worktree-parent '/tmp/worktrees/../delegates//' \
  --settings "$TMP/settings.json")" ||
  fail "safe exact permissions were rejected"
printf '%s\n' "$out" | grep -Fxq 'write_file(/tmp/delegates)' ||
  fail "worktree parent was not normalized to one exact rule: $out"
printf '%s\n' "$out" | grep -Fxq 'unsandboxed(python3.12)' ||
  fail "safe command token lost its exact grant: $out"
[ "$(printf '%s\n' "$out" | grep -Ec '^(write_file|unsandboxed)\(')" = 2 ] ||
  fail "safe inputs emitted more than their two exact rules: $out"

# The default delegate grant must not widen every user's shared temporary
# directory. It is one private, user-local OMS root; an explicit override
# remains exact and unchanged (covered above).
default_home="$TMP/default-home"
default_cache="$TMP/default-cache"
mkdir -p "$default_home" "$default_cache" "$TMP/untrusted-tmp"
out="$(HOME="$default_home" XDG_CACHE_HOME="$default_cache" \
  TMPDIR="$TMP/untrusted-tmp" PATH="$TMP/bin:$PATH" \
  "$ROOT/scripts/provider-permissions.sh" --print --profile delegate \
  --settings "$TMP/default-settings.json")" ||
  fail "default delegate permission projection failed"
default_root="$default_cache/oh-my-setting/worktrees"
printf '%s\n' "$out" | grep -Fxq "write_file($default_root)" ||
  fail "default delegate grant is not the dedicated OMS root: $out"
if printf '%s\n' "$out" | grep -Fxq "write_file($TMP/untrusted-tmp)"; then
  fail "default delegate grant still widens the shared temporary directory"
fi

HOME="$default_home" XDG_CACHE_HOME="$default_cache" \
  TMPDIR="$TMP/untrusted-tmp" PATH="$TMP/bin:$PATH" \
  "$ROOT/scripts/provider-permissions.sh" --apply --profile delegate \
  --settings "$TMP/default-settings.json" >/dev/null ||
  fail "default delegate permission apply failed"
[ -d "$default_root" ] || fail "default delegate root was not created"
mode="$(python3 - "$default_root" <<'PY'
import os
import stat
import sys
print("%o" % stat.S_IMODE(os.stat(sys.argv[1]).st_mode))
PY
)"
[ "$mode" = 700 ] || fail "default delegate root mode is $mode, expected 700"

# Two OMS writers targeting the same canonical settings file must serialize
# the settings and ownership-sidecar transaction. Pause writer A after its
# read; writer B must wait, then merge its disjoint exact grant.
race="$TMP/concurrent"
settings="$race/settings.json"
settings_b="$settings"
pythonpath="$race/pythonpath"
ready="$race/ready"
continue_file="$race/continue"
mkdir -p "$pythonpath"
printf '%s\n' '{"permissions":{"allow":[]}}' > "$settings"
if ln -s "$(basename "$settings")" "$race/settings-alias.json" 2>/dev/null; then
  settings_b="$race/settings-alias.json"
fi
cat > "$pythonpath/shutil.py" <<'PY'
import os
import time


def rmtree(path, ignore_errors=False, onerror=None):
    raise AssertionError("unexpected shutil.rmtree call: %s" % path)


def copyfileobj(source, target, length=1024 * 1024):
    ready = os.environ["OMS_TEST_PP_READY"]
    proceed = os.environ["OMS_TEST_PP_CONTINUE"]
    with open(ready, "w", encoding="utf-8") as handle:
        handle.write("ready\n")
    deadline = time.time() + 10
    while not os.path.exists(proceed):
        if time.time() >= deadline:
            raise RuntimeError("timed out waiting for the second writer")
        time.sleep(0.01)
    while True:
        chunk = source.read(length)
        if not chunk:
            return
        target.write(chunk)
PY

PYTHONPATH="$pythonpath" OMS_TEST_PP_READY="$ready" \
  OMS_TEST_PP_CONTINUE="$continue_file" PATH="$TMP/bin:$PATH" \
  "$ROOT/scripts/provider-permissions.sh" --apply --profile delegate \
  --allow-command uv --worktree-parent "$race/worktrees" \
  --settings "$settings" >"$race/a.out" 2>&1 &
pid_a=$!
i=0
while [ ! -e "$ready" ] && kill -0 "$pid_a" 2>/dev/null; do
  i=$((i + 1))
  [ "$i" -lt 500 ] || break
  sleep 0.01
done
[ -e "$ready" ] || {
  : > "$continue_file"
  wait "$pid_a" || true
  fail "writer A did not reach its post-read boundary: $(cat "$race/a.out")"
}

PATH="$TMP/bin:$PATH" "$ROOT/scripts/provider-permissions.sh" \
  --apply --profile delegate --allow-command npm \
  --worktree-parent "$race/worktrees" --settings "$settings_b" \
  >"$race/b.out" 2>&1 &
pid_b=$!
sleep 0.20
b_finished=0
kill -0 "$pid_b" 2>/dev/null || b_finished=1
: > "$continue_file"
rc_a=0
rc_b=0
wait "$pid_a" || rc_a=$?
wait "$pid_b" || rc_b=$?
[ "$b_finished" = 0 ] ||
  fail "writer B crossed writer A's settings/sidecar transaction"
[ "$rc_a" = 0 ] || fail "serialized writer A failed: $(cat "$race/a.out")"
[ "$rc_b" = 0 ] || fail "serialized writer B failed: $(cat "$race/b.out")"
if ! python3 - "$settings" "$settings.oh-my-setting-permissions.json" <<'PY'
import json
import sys

settings, managed = sys.argv[1:]
allow = json.load(open(settings, encoding="utf-8"))["permissions"]["allow"]
owned = json.load(open(managed, encoding="utf-8"))["rules"]
for rule in ("read_file(*)", "command(*)", "write_file(%s/worktrees)" %
             settings.rsplit("/", 1)[0], "unsandboxed(uv)", "unsandboxed(npm)"):
    assert rule in allow, (rule, allow)
for rule in ("read_file(*)", "command(*)", "write_file(%s/worktrees)" %
             settings.rsplit("/", 1)[0], "unsandboxed(uv)", "unsandboxed(npm)"):
    assert rule in owned, (rule, owned)
PY
then
  fail "serialized disjoint grants or ownership were lost"
fi

echo "provider-permissions-mcp-boundary-smoke: ok"
