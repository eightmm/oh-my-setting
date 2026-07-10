#!/usr/bin/env bash
# shellcheck shell=bash

# Canonical install ownership shared by link, doctor, status, and plugin setup.

oms_install_receipt_path() {
  printf '%s\n' "${OMS_INSTALL_RECEIPT:-${XDG_CONFIG_HOME:-$HOME/.config}/oh-my-setting/install.json}"
}

oms_install_physical_root() {
  (cd "$1" 2>/dev/null && pwd -P)
}

oms_install_receipt_owner() {
  local receipt="${1:-$(oms_install_receipt_path)}"

  [ -f "$receipt" ] || return 1
  python3 - "$receipt" <<'PY'
import json
import os
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        row = json.load(fh)
    root = row.get("source_root", "")
    plugin = row.get("plugin")
    required = ("commit", "channel", "version", "installed_at")
    if row.get("schema") != 1 or not isinstance(root, str) or not os.path.isabs(root):
        raise ValueError("invalid receipt")
    if any(not isinstance(row.get(key), str) or not row.get(key) for key in required):
        raise ValueError("missing receipt metadata")
    if not isinstance(plugin, dict):
        raise ValueError("missing plugin metadata")
    if any(not isinstance(plugin.get(key), str) or not plugin.get(key) for key in ("name", "version", "sha256")):
        raise ValueError("invalid plugin metadata")
    print(os.path.realpath(root))
except Exception:
    sys.exit(1)
PY
}

oms_install_receipt_field() {
  local key="$1"
  local receipt="${2:-$(oms_install_receipt_path)}"

  [ -f "$receipt" ] || return 1
  python3 - "$receipt" "$key" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        value = json.load(fh)
    for part in sys.argv[2].split("."):
        value = value[part]
    if isinstance(value, bool):
        print("true" if value else "false")
    elif isinstance(value, (str, int, float)):
        print(value)
    else:
        raise ValueError("field is not scalar")
except Exception:
    sys.exit(1)
PY
}

oms_install_tree_hash() {
  local tree="$1"
  python3 - "$tree" <<'PY'
import hashlib
import os
import sys

root = os.path.realpath(sys.argv[1])
if not os.path.isdir(root):
    print("unknown")
    raise SystemExit(0)

h = hashlib.sha256()
for base, dirs, files in os.walk(root):
    dirs.sort()
    for name in sorted(files):
        if name in {".oh-my-setting-source-root", ".oh-my-setting-source-sha256"}:
            continue
        path = os.path.join(base, name)
        rel = os.path.relpath(path, root).replace(os.sep, "/")
        h.update(rel.encode("utf-8"))
        h.update(b"\0")
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1024 * 1024), b""):
                h.update(chunk)
        h.update(b"\0")
print(h.hexdigest())
PY
}

oms_install_plugin_hash() {
  oms_install_tree_hash "$1/plugins/oh-my-setting"
}

oms_install_plugin_version() {
  local root="$1"
  python3 - "$root/plugins/oh-my-setting/.codex-plugin/plugin.json" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        print(json.load(fh).get("version", "unknown"))
except Exception:
    print("unknown")
PY
}

oms_install_write_receipt() {
  local root="$1"
  local receipt="${2:-$(oms_install_receipt_path)}"
  local commit="unknown"
  local channel="unknown"
  local dirty="false"
  local version="unknown"
  local plugin_version
  local plugin_hash

  root="$(oms_install_physical_root "$root")" || return 1
  if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    commit="$(git -C "$root" rev-parse HEAD 2>/dev/null || printf unknown)"
    channel="$(git -C "$root" symbolic-ref --quiet --short HEAD 2>/dev/null || printf detached)"
    [ -z "$(git -C "$root" status --porcelain --untracked-files=normal 2>/dev/null)" ] || dirty="true"
  fi
  [ ! -f "$root/VERSION" ] || version="$(sed -n '1p' "$root/VERSION")"
  plugin_version="$(oms_install_plugin_version "$root")"
  plugin_hash="$(oms_install_plugin_hash "$root")"

  mkdir -p "$(dirname "$receipt")"
  OMS_INSTALL_DIRTY="$dirty" python3 - \
    "$receipt" "$root" "$commit" "$channel" "$version" \
    "$plugin_version" "$plugin_hash" <<'PY'
import datetime
import json
import os
import tempfile
import sys

receipt, root, commit, channel, version, plugin_version, plugin_hash = sys.argv[1:]
row = {
    "schema": 1,
    "source_root": root,
    "commit": commit,
    "channel": channel,
    "dirty": os.environ.get("OMS_INSTALL_DIRTY") == "true",
    "version": version,
    "installed_at": datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z"),
    "plugin": {
        "name": "oh-my-setting",
        "version": plugin_version,
        "sha256": plugin_hash,
    },
}
parent = os.path.dirname(receipt) or "."
fd, tmp = tempfile.mkstemp(prefix=".install.", suffix=".tmp", dir=parent)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(row, fh, ensure_ascii=False, indent=2, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, receipt)
    try:
        dirfd = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(dirfd)
        finally:
            os.close(dirfd)
    except OSError:
        pass
except Exception:
    try:
        os.unlink(tmp)
    except FileNotFoundError:
        pass
    raise
PY
}

oms_install_atomic_symlink() {
  local source="$1"
  local target="$2"
  local parent
  local temp

  parent="$(dirname "$target")"
  mkdir -p "$parent"
  temp="$parent/.${target##*/}.oms-link.$$.$RANDOM"
  rm -f "$temp"
  ln -s "$source" "$temp"
  if ! python3 - "$temp" "$target" <<'PY'
import os
import sys
os.replace(sys.argv[1], sys.argv[2])
PY
  then
    rm -f "$temp"
    return 1
  fi
}

oms_install_atomic_text() {
  local value="$1"
  local target="$2"

  mkdir -p "$(dirname "$target")"
  python3 - "$target" "$value" <<'PY'
import os
import tempfile
import sys

target, value = sys.argv[1:]
parent = os.path.dirname(target) or "."
fd, tmp = tempfile.mkstemp(prefix=".%s." % os.path.basename(target), dir=parent)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(value + "\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, target)
except Exception:
    try:
        os.unlink(tmp)
    except FileNotFoundError:
        pass
    raise
PY
}
