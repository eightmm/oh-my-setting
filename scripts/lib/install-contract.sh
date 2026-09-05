#!/usr/bin/env bash
# shellcheck shell=bash

# Canonical install ownership shared by link, doctor, status, and plugin setup.

# Windows Python writes text streams with CRLF, so every value bash reads back
# from a helper arrives with a trailing carriage return: a path becomes a path
# that does not exist, and a state word matches no case branch. Command
# substitution strips the newline and leaves the CR, so this has to happen at the
# point of consumption. Confirmed on CI — `link.sh` built
# a `custom-skills/<skill>\r` source path from a manifest read and the copy failed
# on a source that was right there.
#
# Defined before the platform block and outside it: this is string handling with
# no platform dependency, and putting it behind the availability check made a
# transaction fixture that ships this file without platform.sh call an undefined
# function instead of running.
oms_strip_cr() {
  local value="$1"

  printf '%s\n' "${value//$'\r'/}"
}

OMS_INSTALL_CONTRACT_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
if [ -f "$OMS_INSTALL_CONTRACT_LIB_DIR/platform.sh" ]; then
  # shellcheck source=scripts/lib/platform.sh
  . "$OMS_INSTALL_CONTRACT_LIB_DIR/platform.sh"
else
  # Transaction fixtures and upgrades from pre-portability commits can briefly
  # have the contract without its new helper. Preserve the historical POSIX
  # symlink behavior until the complete checkout is present.
  oms_platform_is_windows() { return 1; }
  # Unrecognized rather than owned: without the helper this cannot prove the
  # shim is ours, and the safe answer to "may I delete this" is no.
  oms_install_python_shim_owned() { return 1; }
  oms_install_link_mode() {
    case "${OH_MY_SETTING_LINK_MODE:-auto}" in
      auto|symlink) printf 'symlink\n' ;;
      copy)
        echo "error: copy mode requires scripts/lib/platform.sh" >&2
        return 1
        ;;
      *) echo "error: invalid install link mode" >&2; return 2 ;;
    esac
  }
fi
OMS_MANAGED_TARGET_HELPER="$OMS_INSTALL_CONTRACT_LIB_DIR/managed-target.py"

oms_install_marker_path() {
  local target="$1"

  printf '%s/.%s.oh-my-setting-managed.json\n' \
    "$(dirname "$target")" "$(basename "$target")"
}

oms_install_managed_target() {
  local out

  if [ ! -f "$OMS_MANAGED_TARGET_HELPER" ]; then
    if [ "${1:-}" = copy ]; then
      echo "error: copy mode requires $OMS_MANAGED_TARGET_HELPER" >&2
    fi
    return 1
  fi
  # The state word is matched against a closed set and the source is used as a
  # path, so a Windows CR would turn both into values nothing recognizes.
  out="$(python3 "$OMS_MANAGED_TARGET_HELPER" "$@")" || return
  [ -z "$out" ] || oms_strip_cr "$out"
}

oms_install_target_state() {
  local source="$1"
  local target="$2"
  local state

  # Nothing there is its own answer. Reporting "foreign" would be a claim about
  # someone else's file, and answering it through the copy inspector spends a
  # python3 process (~25ms) per probe — doctor and status probe every managed
  # target on every run, most of which are absent on a partial install.
  if [ ! -L "$target" ] && [ ! -e "$target" ]; then
    printf 'missing\n'
    return 0
  fi

  if [ -L "$target" ]; then
    if [ "$(readlink "$target")" != "$source" ]; then
      printf 'foreign\n'
    elif [ -e "$target" ]; then
      printf 'symlink-current\n'
    else
      printf 'symlink-owned\n'
    fi
  else
    state="$(oms_install_managed_target inspect "$source" "$target")" || return
    case "$state" in
      current|owned|modified) printf 'copy-%s\n' "$state" ;;
      foreign) printf 'foreign\n' ;;
      *) echo "error: invalid managed target state: $state" >&2; return 1 ;;
    esac
  fi
}

oms_install_target_matches() {
  case "$(oms_install_target_state "$1" "$2")" in
    symlink-current|copy-current) return 0 ;;
    *) return 1 ;;
  esac
}

oms_install_target_owned() {
  case "$(oms_install_target_state "$1" "$2")" in
    symlink-current|symlink-owned|copy-current|copy-owned) return 0 ;;
    *) return 1 ;;
  esac
}

oms_install_target_owned_source_under() {
  local source_root="$1"
  local target="$2"
  local source

  if [ -L "$target" ]; then
    case "$(readlink "$target")" in
      "$source_root"/*) readlink "$target"; return 0 ;;
      *) return 1 ;;
    esac
  fi
  source="$(oms_install_managed_target source-under "$source_root" "$target")" ||
    return
  if oms_platform_is_windows && command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$source"
  else
    printf '%s\n' "$source"
  fi
}

oms_install_target_mode() {
  case "$(oms_install_target_state "$1" "$2")" in
    symlink-*) printf 'symlink\n' ;;
    copy-current|copy-owned) printf 'copy\n' ;;
    missing) printf 'missing\n' ;;
    *) printf 'foreign\n' ;;
  esac
}

oms_install_remove_marker() {
  [ -f "$OMS_MANAGED_TARGET_HELPER" ] || return 0
  oms_install_managed_target remove-marker "$1"
}

oms_install_remove_managed_target() {
  local target="$1"

  rm -rf "$target"
  oms_install_remove_marker "$target"
}

oms_install_materialize() {
  local source="$1"
  local target="$2"
  local mode

  mode="$(oms_install_link_mode)" || return
  case "$mode" in
    symlink) oms_install_atomic_symlink "$source" "$target" ;;
    copy) oms_install_managed_target copy "$source" "$target" ;;
  esac
}

oms_install_receipt_path() {
  printf '%s\n' "${OMS_INSTALL_RECEIPT:-${XDG_CONFIG_HOME:-$HOME/.config}/oh-my-setting/install.json}"
}

oms_install_physical_root() {
  (cd "$1" 2>/dev/null && pwd -P)
}

oms_install_receipt_owner() {
  local receipt="${1:-$(oms_install_receipt_path)}"
  local owner

  [ -f "$receipt" ] || return 1
  owner="$(python3 - "$receipt" <<'PY'
import json
import os
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        row = json.load(fh)
    root = row.get("source_root", "")
    plugin = row.get("plugin")
    required = ("commit", "channel", "version", "installed_at")
    schema = row.get("schema")
    if schema not in (1, 2) or not isinstance(root, str) or not os.path.isabs(root):
        raise ValueError("invalid receipt")
    if any(not isinstance(row.get(key), str) or not row.get(key) for key in required):
        raise ValueError("missing receipt metadata")
    if not isinstance(plugin, dict):
        raise ValueError("missing plugin metadata")
    if any(not isinstance(plugin.get(key), str) or not plugin.get(key) for key in ("name", "version", "sha256")):
        raise ValueError("invalid plugin metadata")
    if schema == 2:
        components = row.get("components")
        component_modes = row.get("component_modes")
        link_mode = row.get("link_mode")
        required_components = (
            "tools", "claude_hooks", "codex_plugin", "auto_update",
            "machine_snapshot", "slurm_snapshot",
        )
        if row.get("profile") not in ("minimal", "full", "custom"):
            raise ValueError("invalid install profile")
        if link_mode is not None and link_mode not in ("copy", "symlink"):
            raise ValueError("invalid install link mode")
        if not isinstance(row.get("ref"), str) or not row.get("ref"):
            raise ValueError("invalid install ref")
        if not isinstance(row.get("previous_commit"), str):
            raise ValueError("invalid previous commit")
        if not isinstance(components, dict) or any(
            not isinstance(components.get(key), bool) for key in required_components
        ):
            raise ValueError("invalid component profile")
        # A surgical component write may create component_modes holding only
        # the key it recorded; absent keys fall back to the components flags,
        # so presence is optional but a present value must be valid.
        if component_modes is not None and (
            not isinstance(component_modes, dict)
            or any(key in component_modes
                   and component_modes.get(key) not in ("0", "1", "auto")
                   for key in ("machine_snapshot", "slurm_snapshot"))
            or component_modes.get("auto_update") not in (None, "check", "apply")
        ):
            raise ValueError("invalid component modes")
        if not isinstance(row.get("managed_targets"), list) or any(
            not isinstance(value, str) or not value for value in row["managed_targets"]
        ):
            raise ValueError("invalid managed target inventory")
    print(os.path.realpath(root))
except Exception:
    sys.exit(1)
PY
)" || return
  owner="$(oms_strip_cr "$owner")"
  if oms_platform_is_windows && command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$owner"
  else
    printf '%s\n' "$owner"
  fi
}

oms_install_receipt_field() {
  local key="$1"
  local receipt="${2:-$(oms_install_receipt_path)}"

  [ -f "$receipt" ] || return 1
  local value
  value="$(python3 - "$receipt" "$key" <<'PY'
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
)" || return
  oms_strip_cr "$value"
}

oms_install_receipt_mode() {
  local key="$1"
  local fallback="${2:-0}"
  local receipt="${3:-$(oms_install_receipt_path)}"
  local value

  value="$(oms_install_receipt_field "component_modes.$key" "$receipt" 2>/dev/null || true)"
  case "$value" in
    0|1|auto) printf '%s\n' "$value"; return 0 ;;
  esac
  value="$(oms_install_receipt_field "components.$key" "$receipt" 2>/dev/null || true)"
  case "$value" in
    true) printf '1\n' ;;
    false) printf '0\n' ;;
    *) printf '%s\n' "$fallback" ;;
  esac
}

oms_install_require_owner() {
  local root="$1"
  local action="${2:-mutate the install}"
  local receipt="${3:-$(oms_install_receipt_path)}"
  local owner
  local physical_root

  [ -f "$receipt" ] || return 0
  owner="$(oms_install_receipt_owner "$receipt")" || {
    echo "error: invalid install receipt: $receipt" >&2
    return 1
  }
  physical_root="$(oms_install_physical_root "$root")" || {
    echo "error: install checkout is unavailable: $root" >&2
    return 1
  }
  if [ "$owner" != "$physical_root" ]; then
    echo "error: refusing to $action from non-canonical checkout: $physical_root" >&2
    echo "error: canonical install owner: $owner" >&2
    return 1
  fi
}

oms_install_tree_hash() {
  local tree="$1"
  local value
  value="$(python3 - "$tree" <<'PY'
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
)" || return
  oms_strip_cr "$value"
}

oms_install_plugin_hash() {
  oms_install_tree_hash "$1/plugins/oh-my-setting"
}

oms_install_plugin_version() {
  local root="$1"
  local value
  value="$(python3 - "$root/plugins/oh-my-setting/.codex-plugin/plugin.json" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        print(json.load(fh).get("version", "unknown"))
except Exception:
    print("unknown")
PY
)" || return
  oms_strip_cr "$value"
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
  local previous_commit="${OMS_INSTALL_PREVIOUS_COMMIT:-}"
  local profile="${OH_MY_SETTING_PROFILE:-custom}"
  local install_ref="${OH_MY_SETTING_REF:-}"
  local link_mode

  root="$(oms_install_physical_root "$root")" || return 1
  case "$profile" in minimal|full|custom) ;; *) echo "error: invalid install profile: $profile" >&2; return 2 ;; esac
  case "${OH_MY_SETTING_GENERATE_MACHINE:-0}:${OH_MY_SETTING_GENERATE_SLURM:-0}" in
    0:0|0:1|0:auto|1:0|1:1|1:auto|auto:0|auto:1|auto:auto) ;;
    *) echo "error: invalid snapshot component mode" >&2; return 2 ;;
  esac
  [ -n "$install_ref" ] || install_ref="unknown"
  link_mode="$(oms_install_link_mode)" || return
  case "$previous_commit" in
    "") ;;
    *[!0-9a-fA-F]*) echo "error: invalid previous install commit" >&2; return 2 ;;
  esac
  if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    commit="$(git -C "$root" rev-parse HEAD 2>/dev/null || printf unknown)"
    channel="$(git -C "$root" symbolic-ref --quiet --short HEAD 2>/dev/null || printf detached)"
    [ -z "$(git -C "$root" status --porcelain --untracked-files=normal 2>/dev/null)" ] || dirty="true"
  fi
  [ ! -f "$root/VERSION" ] || version="$(sed -n '1p' "$root/VERSION")"
  [ "$install_ref" != "unknown" ] || install_ref="$channel"
  plugin_version="$(oms_install_plugin_version "$root")"
  plugin_hash="$(oms_install_plugin_hash "$root")"

  mkdir -p "$(dirname "$receipt")"
  OMS_INSTALL_DIRTY="$dirty" \
    OMS_INSTALL_PROFILE="$profile" OMS_INSTALL_REF="$install_ref" \
    OMS_INSTALL_PREVIOUS="$previous_commit" OMS_INSTALL_LINK_MODE="$link_mode" python3 - \
    "$receipt" "$root" "$commit" "$channel" "$version" \
    "$plugin_version" "$plugin_hash" <<'PY'
import datetime
import json
import os
import tempfile
import sys

receipt, root, commit, channel, version, plugin_version, plugin_hash = sys.argv[1:]
row = {
    "schema": 2,
    "source_root": root,
    "commit": commit,
    "channel": channel,
    "dirty": os.environ.get("OMS_INSTALL_DIRTY") == "true",
    "version": version,
    "profile": os.environ.get("OMS_INSTALL_PROFILE", "custom"),
    "link_mode": os.environ["OMS_INSTALL_LINK_MODE"],
    "ref": os.environ.get("OMS_INSTALL_REF", channel),
    "previous_commit": os.environ.get("OMS_INSTALL_PREVIOUS", ""),
    "installed_at": datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z"),
    "components": {
        "tools": os.environ.get("OH_MY_SETTING_INSTALL_TOOLS", "0") == "1",
        "claude_hooks": os.environ.get("OH_MY_SETTING_CLAUDE_HOOKS", "1") == "1",
        "codex_plugin": os.environ.get("OH_MY_SETTING_CODEX_PLUGIN", "0") == "1",
        "auto_update": os.environ.get("OH_MY_SETTING_AUTO_UPDATE", "0") == "1",
        "machine_snapshot": os.environ.get("OH_MY_SETTING_GENERATE_MACHINE", "0") != "0",
        "slurm_snapshot": os.environ.get("OH_MY_SETTING_GENERATE_SLURM", "0") != "0",
    },
    "component_modes": {
        "auto_update": (
            "apply"
            if os.environ.get("OH_MY_SETTING_AUTO_UPDATE_MODE") == "apply"
            else "check"
        ),
        "machine_snapshot": os.environ.get("OH_MY_SETTING_GENERATE_MACHINE", "0"),
        "slurm_snapshot": os.environ.get("OH_MY_SETTING_GENERATE_SLURM", "0"),
    },
    "managed_targets": [
        ".codex/AGENTS.md",
        ".claude/CLAUDE.md",
        ".gemini/AGENTS.md",
        ".codex/skills",
        ".claude/skills",
        ".gemini/antigravity/skills",
        ".oh-my-setting-prompts",
        ".local/bin/oms",
    ],
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

# Persist one components.<key> flag in an existing schema-2 receipt without
# regenerating the rest of the row; a non-empty fourth argument also records
# component_modes.<key> in the same atomic write. Returns 1 when the receipt
# is missing or unreadable and 3 when the schema predates the component
# profile.
oms_install_receipt_set_component() {
  local key="$1"
  local value="$2"
  local receipt="${3:-$(oms_install_receipt_path)}"
  local mode="${4:-}"

  case "$value" in true|false) ;; *) return 2 ;; esac
  [ -f "$receipt" ] || return 1
  python3 - "$receipt" "$key" "$value" "$mode" <<'PY'
import json
import os
import sys
import tempfile

receipt, key, value, mode = sys.argv[1:]
try:
    with open(receipt, encoding="utf-8") as fh:
        row = json.load(fh)
except Exception:
    sys.exit(1)
if row.get("schema") != 2:
    sys.exit(3)
components = row.get("components")
if not isinstance(components, dict):
    components = {}
    row["components"] = components
components[key] = value == "true"
if mode:
    modes = row.get("component_modes")
    if not isinstance(modes, dict):
        modes = {}
        row["component_modes"] = modes
    modes[key] = mode
parent = os.path.dirname(receipt) or "."
fd, tmp = tempfile.mkstemp(prefix=".install.", suffix=".tmp", dir=parent)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(row, fh, ensure_ascii=False, indent=2, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, receipt)
except Exception:
    try:
        os.unlink(tmp)
    except FileNotFoundError:
        pass
    sys.exit(1)
PY
}

# Selection is shared; each trigger retains its own ownership and write path.
oms_install_scheduler_systemd_available() {
  command -v systemctl >/dev/null 2>&1 &&
    [ -n "${XDG_RUNTIME_DIR:-}" ] &&
    systemctl --user show-environment >/dev/null 2>&1
}

oms_install_scheduler_cron_available() {
  [ -n "${1:-}" ] || command -v crontab >/dev/null 2>&1
}

oms_install_scheduler_method() {
  local method="${1-auto}" cron_file="${2:-}"
  case "$method" in
    systemd|cron) printf '%s\n' "$method" ;;
    auto)
      if oms_install_scheduler_systemd_available; then
        printf 'systemd\n'
      elif oms_install_scheduler_cron_available "$cron_file"; then
        printf 'cron\n'
      else
        printf 'none\n'
      fi
      ;;
    *) echo "error: --method must be auto, systemd, or cron" >&2; return 2 ;;
  esac
}

# The cron marker pair is an ownership boundary, not just an installation
# hint. Every reader and writer must classify the same bytes the same way:
# absent has no marker, valid has exactly one ordered begin/end pair, and any
# orphan, reversed, nested, or duplicate marker is malformed. In particular,
# never stream-edit an unclassified crontab: an orphan begin marker would make
# the filter consume unrelated jobs through EOF.
OMS_INSTALL_AUTOUPDATE_CRON_MARK_BEGIN="# oh-my-setting autoupdate:begin"
OMS_INSTALL_AUTOUPDATE_CRON_MARK_END="# oh-my-setting autoupdate:end"

oms_install_autoupdate_cron_read() {
  local cron_file="${1:-}"

  if [ -n "$cron_file" ]; then
    if [ -f "$cron_file" ]; then
      cat "$cron_file" || return
    fi
    return 0
  fi
  command -v crontab >/dev/null 2>&1 || return 0
  crontab -l 2>/dev/null || true
}

oms_install_autoupdate_cron_write() {
  local cron_file="${1:-}"

  if [ -n "$cron_file" ]; then
    cat > "$cron_file"
  else
    crontab -
  fi
}

# Write the input with a valid managed block removed to OUTPUT and print its
# typed state. OUTPUT is scratch space for malformed input and must not be
# installed unless the returned state is valid or absent.
oms_install_autoupdate_cron_prepare() {
  local input="$1"
  local output="$2"
  local status=0

  awk -v begin="$OMS_INSTALL_AUTOUPDATE_CRON_MARK_BEGIN" \
      -v end="$OMS_INSTALL_AUTOUPDATE_CRON_MARK_END" '
    {
      probe = $0
      sub(/\r$/, "", probe)
      if (probe == begin) {
        if (inside || blocks > 0) malformed = 1
        inside = 1
        blocks++
        next
      }
      if (probe == end) {
        if (!inside) malformed = 1
        inside = 0
        next
      }
      if (!inside) print
    }
    END {
      if (inside) malformed = 1
      if (malformed) exit 2
      if (blocks == 0) exit 3
    }
  ' "$input" > "$output" || status="$?"

  case "$status" in
    0) printf 'valid\n' ;;
    3) printf 'absent\n' ;;
    2) printf 'malformed\n' ;;
    *) return "$status" ;;
  esac
}

oms_install_autoupdate_cron_state() {
  local cron_file="${1:-}"
  local contents
  local state

  if ! contents="$(oms_install_autoupdate_cron_read "$cron_file")"; then
    printf 'malformed\n'
    return 0
  fi
  state="$(printf '%s' "$contents" |
    oms_install_autoupdate_cron_prepare - /dev/null)" || return
  printf '%s\n' "$state"
}

# One reading of the systemd trigger for status and attention. A unit file
# alone is not a trigger: the manager fires what timers.target.wants links, so
# a written-but-never-enabled or later-disabled unit is `disabled`. Field
# incident: attention read the updater as wired for days on a unit file whose
# enablement a gate run had removed, while status called the same file
# "(disabled)".
oms_install_autoupdate_systemd_state() {
  local unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

  if [ ! -f "$unit_dir/oh-my-setting-autoupdate.timer" ]; then
    printf 'absent\n'
  elif [ -L "$unit_dir/timers.target.wants/oh-my-setting-autoupdate.timer" ]; then
    printf 'enabled\n'
  else
    printf 'disabled\n'
  fi
}

# The trigger scripts record their state here so an out-of-band
# `oms auto-update install`/`remove` survives the next update: update.sh
# reconciles the trigger from the receipt's component profile, so a timer that
# was installed but never recorded was silently uninstalled one update later.
# Only the receipt owner may record; other checkouts get a note instead, and
# schema-1 receipts are left alone because update.sh probes the scheduler
# directly for those.
oms_install_record_auto_update() {
  local flag="$1"
  local root="$2"
  local dry_run="${3:-0}"
  local mode="${4:-}"
  local receipt owner physical_root status

  case "$mode" in ""|check|apply) ;; *) return 2 ;; esac
  receipt="$(oms_install_receipt_path)"
  [ -f "$receipt" ] || return 0
  owner="$(oms_install_receipt_owner "$receipt" 2>/dev/null)" || return 0
  physical_root="$(oms_install_physical_root "$root")" || return 0
  if [ "$owner" != "$physical_root" ]; then
    echo "note: install receipt owner is $owner; auto_update=$flag not recorded from $physical_root" >&2
    return 0
  fi
  if [ "$dry_run" = "1" ]; then
    printf 'would record auto_update=%s%s in %s\n' "$flag" "${mode:+ ($mode)}" "$receipt"
    return 0
  fi
  status=0
  oms_install_receipt_set_component auto_update "$flag" "$receipt" "$mode" || status="$?"
  [ "$status" -ne 0 ] || return 0
  [ "$status" -ne 3 ] || return 0
  if [ "$flag" = "true" ]; then
    echo "warning: auto-update trigger installed but not recorded in $receipt; the next update will remove it" >&2
  else
    echo "warning: auto-update trigger removed but still recorded in $receipt; the next update will reinstall it" >&2
  fi
  return 0
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
  if [ ! -L "$temp" ]; then
    rm -rf "$temp"
    echo "error: this shell did not create a real symlink; use OH_MY_SETTING_LINK_MODE=copy" >&2
    return 1
  fi
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
