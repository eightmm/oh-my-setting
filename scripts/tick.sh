#!/usr/bin/env bash
set -euo pipefail

# Unattended maintenance tick for registered repos. Each run syncs the work
# journal, reconciles dead attempts, closes idle threads and goal-less task
# packets, retires idle all-done plans, and refreshes a stale Codex plugin cache;
# gc stays opt-in (OMS_TICK_GC=1) because its retention floor is a per-repo
# decision. `install` wires an hourly user-level timer (systemd, cron fallback)
# so sessions start on a swept tree.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"

usage() {
  cat <<'EOF'
Usage: tick.sh run [--repo PATH ...] [--dry-run]
       tick.sh register|unregister [--repo PATH]
       tick.sh install [--method auto|systemd|cron] [--dry-run]
       tick.sh uninstall [--dry-run]
       tick.sh status

run        Sweep the given repos, or every registered one: journal sync,
           attempt reconcile, stale thread/task close and plan retirement,
           optional gc, and a Codex plugin cache refresh when the cache no
           longer matches the install.
           Each repo gets .oms/tick/last.json.
register   Add a repo (default: PWD, git-root anchored) to the registry;
           install registers PWD when it is an adopted repo.
install    Register the hourly trigger; uninstall removes only a unit this
           checkout owns.
Environment:
  OMS_TICK_REGISTRY            registry file (default: config dir/tick-repos.txt)
  OMS_TICK_THREAD_IDLE_DAYS    close open threads idle at least this long (7)
  OMS_TICK_TASK_IDLE_DAYS      close active goal-less tasks idle at least this long (7)
  OMS_TICK_PLAN_IDLE_DAYS      retire all-done plans idle at least this long (14)
  OMS_TICK_RETIRE=0            skip task and plan retirement (default on)
  OMS_TICK_GC=1                also run `oms gc --apply` per repo (default off)
  OMS_TICK_STEP_TIMEOUT        seconds per step when `timeout` exists (600)
EOF
}

CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/oh-my-setting"
REGISTRY="${OMS_TICK_REGISTRY:-$CONFIG_DIR/tick-repos.txt}"
SYSTEMD_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SERVICE_FILE="$SYSTEMD_DIR/oh-my-setting-tick.service"
TIMER_FILE="$SYSTEMD_DIR/oh-my-setting-tick.timer"
CRON_MARK="# oh-my-setting tick"
IDLE_DAYS="${OMS_TICK_THREAD_IDLE_DAYS:-7}"
TASK_IDLE_DAYS="${OMS_TICK_TASK_IDLE_DAYS:-7}"
PLAN_IDLE_DAYS="${OMS_TICK_PLAN_IDLE_DAYS:-14}"
RETIRE="${OMS_TICK_RETIRE:-1}"
STEP_TIMEOUT="${OMS_TICK_STEP_TIMEOUT:-600}"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
METHOD=auto
MODE="${1:-}"; [ "$#" -eq 0 ] || shift
REPOS=()
case "$MODE" in run|register|unregister|install|uninstall|status) ;; -h|--help|"") usage; exit 0 ;;
  *) echo "error: unknown mode: $MODE" >&2; usage >&2; exit 2 ;; esac
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) REPOS+=("$2"); shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --method) METHOD="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$IDLE_DAYS$TASK_IDLE_DAYS$PLAN_IDLE_DAYS$STEP_TIMEOUT" in *[!0-9]*) echo "error: OMS_TICK_* values must be integers" >&2; exit 2 ;; esac

repo_root() { git -C "${1:-$PWD}" rev-parse --show-toplevel 2>/dev/null || (cd "${1:-$PWD}" && pwd -P); }
bounded() { if command -v timeout >/dev/null 2>&1; then timeout "$STEP_TIMEOUT" "$@"; else "$@"; fi; }

registered() { [ -f "$REGISTRY" ] && grep -Fxq "$1" "$REGISTRY"; }
register_repo() {
  local root; root="$(repo_root "$1")"
  [ -d "$root/.oms" ] || { echo "error: not an adopted repo (no .oms): $root" >&2; return 2; }
  registered "$root" && { echo "already registered: $root"; return 0; }
  mkdir -p "$(dirname "$REGISTRY")"
  printf '%s\n' "$root" >> "$REGISTRY"
  echo "registered: $root"
}
unregister_repo() {
  local root tmp; root="$(repo_root "$1")"
  [ -f "$REGISTRY" ] || return 0
  tmp="$(mktemp "$REGISTRY.XXXXXX")"
  grep -Fxv "$root" "$REGISTRY" > "$tmp" || true
  mv "$tmp" "$REGISTRY"
  echo "unregistered: $root"
}

codex_cache_stale() {  # doctor's predicate, so a tick repairs what doctor would flag
  local receipt cache version market expected marker_hash="" marker_root=""
  receipt="$(oms_install_receipt_path)"
  [ -f "$receipt" ] && command -v codex >/dev/null 2>&1 || return 1
  [ "$(oms_install_receipt_field components.codex_plugin "$receipt" 2>/dev/null || true)" = true ] || return 1
  version="$(oms_install_plugin_version "$ROOT")" || return 1
  market="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' \
    "$ROOT/.agents/plugins/marketplace.json" 2>/dev/null)" || return 1
  cache="${CODEX_HOME:-$HOME/.codex}/plugins/cache/$market/oh-my-setting/$version"
  [ -d "$cache" ] || return 0
  expected="$(oms_install_receipt_field plugin.sha256 "$receipt" 2>/dev/null || oms_install_plugin_hash "$ROOT")"
  [ ! -f "$cache/.oh-my-setting-source-sha256" ] || marker_hash="$(sed -n 1p "$cache/.oh-my-setting-source-sha256")"
  [ ! -f "$cache/.oh-my-setting-source-root" ] || marker_root="$(sed -n 1p "$cache/.oh-my-setting-source-root")"
  [ "$marker_root" != "$ROOT" ] || [ "$marker_hash" != "$expected" ] ||
    [ "$(oms_install_tree_hash "$cache")" != "$expected" ]
}

task_goal_empty() {
  awk '
    BEGIN { empty = 1 }
    $0 == "## Goal" { in_goal = 1; seen = 1; next }
    in_goal && /^## / { in_goal = 0; next }
    in_goal {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)
      if (line != "" && line != "(... entry clipped ...)") empty = 0
    }
    END { exit seen && empty ? 0 : 1 }
  ' "$1"
}

task_is_idle_active() {  # JSON from agent-task status --json on stdin
  python3 -c '
import datetime, json, sys
try:
    task = json.load(sys.stdin)
    then = datetime.datetime.strptime(
        task.get("last_activity") or "", "%Y-%m-%dT%H:%M:%SZ"
    ).replace(tzinfo=datetime.timezone.utc)
    days = int(sys.argv[1])
except Exception:
    raise SystemExit(1)
age = (datetime.datetime.now(datetime.timezone.utc) - then).total_seconds()
raise SystemExit(0 if task.get("present") is True and task.get("status") == "active"
                 and age >= days * 86400 else 1)
' "$TASK_IDLE_DAYS"
}

plan_tasks_are_idle() {  # PLAN JSON -> every nonempty done task is older than the idle threshold
  python3 - "$1" "$PLAN_IDLE_DAYS" <<'PY'
import datetime
import json
import os
import stat
import sys

maximum = 1024 * 1024
path = sys.argv[1]

def is_reparse(info):
    attribute = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(getattr(info, "st_file_attributes", 0) & attribute)

try:
    before = os.lstat(path)
    if (stat.S_ISLNK(before.st_mode) or is_reparse(before) or
            not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or
            before.st_size > maximum):
        raise ValueError
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (not stat.S_ISREG(opened.st_mode) or is_reparse(opened) or
                opened.st_nlink != 1 or
                (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)):
            raise ValueError
        chunks = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(maximum + 1 - total, 1024 * 1024))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > maximum:
                raise ValueError
    finally:
        os.close(descriptor)
    after = os.lstat(path)
    if (after.st_dev, after.st_ino, after.st_size) != (
            opened.st_dev, opened.st_ino, opened.st_size):
        raise ValueError
    plan = json.loads(b"".join(chunks).decode("utf-8"))
    tasks = plan.get("tasks") if isinstance(plan, dict) else None
    days = int(sys.argv[2])
    if not isinstance(tasks, dict) or not tasks:
        raise ValueError
    now = datetime.datetime.now(datetime.timezone.utc)
    for task in tasks.values():
        if not isinstance(task, dict) or task.get("state") != "done":
            raise ValueError
        updated = task.get("updated")
        if not isinstance(updated, str):
            raise ValueError
        then = datetime.datetime.strptime(updated, "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=datetime.timezone.utc
        )
        if (now - then).total_seconds() < days * 86400:
            raise ValueError
except (OSError, TypeError, UnicodeError, ValueError):
    raise SystemExit(1)
raise SystemExit(0)
PY
}

plan_retire_check_sha() {  # JSON from agent-plan retire --check on stdin -> its exact CAS token
  python3 -c '
import json
import re
import sys

try:
    row = json.load(sys.stdin)
    plan_sha = row.get("plan_sha256")
    if (row.get("schema") != 1 or row.get("action") != "plan-retire-check" or
            row.get("active_task_blockers") != [] or row.get("would_write") is not False or
            not isinstance(plan_sha, str) or not re.fullmatch(r"[0-9a-f]{64}", plan_sha)):
        raise ValueError
except (TypeError, ValueError):
    raise SystemExit(1)
print(plan_sha)
'
}

sweep_repo() {  # sweep_repo ROOT -> one summary line; receipt in .oms/tick/last.json
  local root="$1" oms="$ROOT/scripts/oms" journal=0 reconcile=0 gc=skipped closed=() id task_file
  local tasks_closed=0 plans_retired=0 plan_file plan_check plan_sha plan_retire_rc=skipped
  [ -d "$root/.oms" ] || { echo "skip $root: no .oms"; return 0; }
  if [ "$DRY_RUN" = 1 ]; then echo "would sweep $root"; return 0; fi
  bounded "$oms" journal sync --repo "$root" >/dev/null 2>&1 || journal=$?
  bounded "$oms" agent-events --repo "$root" reconcile --apply >/dev/null 2>&1 || reconcile=$?
  while IFS= read -r id; do
    [ -n "$id" ] || continue
    bounded "$oms" thread close --repo "$root" --id "$id" \
      --summary "closed by oms tick: idle over ${IDLE_DAYS}d" >/dev/null 2>&1 && closed+=("$id")
  done < <(find "$root/.oms/threads" -maxdepth 1 -name '*.jsonl' -mtime "+$((IDLE_DAYS - 1))" 2>/dev/null |
    while IFS= read -r f; do id="$(basename "$f" .jsonl)"
      "$oms" thread list --json --repo "$root" 2>/dev/null | grep -Fq "\"$id\"" && echo "$id"; done)
  task_file="$root/.oms/task/current.md"
  if [ "$RETIRE" != 0 ] && [ -s "$task_file" ] && task_goal_empty "$task_file" &&
    bounded "$oms" agent-task --repo "$root" status --json 2>/dev/null | task_is_idle_active; then
    if bounded "$oms" agent-task --repo "$root" close \
      --reason "closed by oms tick: goal-less packet idle over ${TASK_IDLE_DAYS}d" >/dev/null 2>&1; then
      tasks_closed=$((tasks_closed + 1))
    fi
  fi
  plan_file="$root/.oms/plan/tasks.json"
  if [ "$RETIRE" != 0 ] && [ -s "$plan_file" ] && plan_tasks_are_idle "$plan_file"; then
    if plan_check="$(bounded "$oms" agent-plan --repo "$root" retire --check 2>/dev/null)"; then
      if plan_sha="$(printf '%s\n' "$plan_check" | plan_retire_check_sha | tr -d '\r')"; then
        if bounded "$oms" agent-plan --repo "$root" retire --apply \
          --expected-plan-sha256 "$plan_sha" --disposition superseded \
          --reason "retired by oms tick: all tasks idle over ${PLAN_IDLE_DAYS}d" >/dev/null 2>&1; then
          plans_retired=$((plans_retired + 1))
          plan_retire_rc=0
          work_journal_observe "$root" oms-run "$root/.oms/plan/retirements.jsonl" \
            --source-id "plan-retire:$plan_sha" --event-type phase_outcome \
            --outcome "Idle all-done plan retired" --outcome-status success \
            --verification-status not_verified
        else
          plan_retire_rc=$?
        fi
      else
        plan_retire_rc=1
      fi
    else
      plan_retire_rc=$?
    fi
  fi
  if [ "${OMS_TICK_GC:-0}" = 1 ]; then gc=0; bounded "$oms" gc --repo "$root" --apply >/dev/null 2>&1 || gc=$?; fi
  mkdir -p "$root/.oms/tick"
  python3 - "$root/.oms/tick/last.json" "$journal" "$reconcile" "$gc" "$tasks_closed" "$plans_retired" "$plan_retire_rc" "${closed[@]}" <<'PY'
import datetime, json, os, sys, tempfile
path, journal, reconcile, gc, tasks_closed, plans_retired, plan_retire_rc, *closed = sys.argv[1:]
row = {"schema": 1, "ran_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
       "journal_rc": int(journal), "reconcile_rc": int(reconcile),
       "gc": "skipped" if gc == "skipped" else int(gc), "threads_closed": closed,
       "tasks_closed": int(tasks_closed), "plans_retired": int(plans_retired),
       "plan_retire_rc": "skipped" if plan_retire_rc == "skipped" else int(plan_retire_rc)}
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".tick.")
with os.fdopen(fd, "w", encoding="utf-8") as fh:
    json.dump(row, fh, ensure_ascii=False, sort_keys=True); fh.write("\n")
os.replace(tmp, path)
PY
  echo "swept $root: journal=$journal reconcile=$reconcile gc=$gc threads_closed=${#closed[@]} tasks_closed=$tasks_closed plans_retired=$plans_retired"
}

run_tick() {
  local root
  if [ "${#REPOS[@]}" -eq 0 ] && [ -f "$REGISTRY" ]; then
    while IFS= read -r root; do [ -n "$root" ] && REPOS+=("$root"); done < "$REGISTRY"
  fi
  [ "${#REPOS[@]}" -gt 0 ] || { echo "nothing registered: oms tick register --repo PATH"; return 0; }
  for root in "${REPOS[@]}"; do
    [ -d "$root" ] || { echo "skip $root: missing"; continue; }
    sweep_repo "$(repo_root "$root")"
  done
  if codex_cache_stale; then
    if [ "$DRY_RUN" = 1 ]; then echo "would refresh the codex plugin cache"
    else bounded "$ROOT/scripts/install-codex-plugin.sh" >/dev/null 2>&1 && echo "codex plugin cache refreshed" ||
      echo "codex plugin cache refresh failed (run: $ROOT/scripts/install-codex-plugin.sh)" >&2; fi
  fi
}

systemd_usable() { command -v systemctl >/dev/null 2>&1 && [ -n "${XDG_RUNTIME_DIR:-}" ]; }
systemd_owns_unit() {  # the unit systemd knows must be this checkout's file, not a namesake
  local fragment
  systemd_usable || return 1
  fragment="$(systemctl --user show -p FragmentPath --value oh-my-setting-tick.timer 2>/dev/null)" || return 1
  fragment="${fragment#FragmentPath=}"
  [ -n "$fragment" ] && [ "$(cd "$(dirname "$fragment")" 2>/dev/null && pwd -P)/$(basename "$fragment")" = \
    "$(cd "$(dirname "$TIMER_FILE")" 2>/dev/null && pwd -P)/$(basename "$TIMER_FILE")" ]
}
record_component() {  # best effort; only the receipt owner records
  local receipt; receipt="$(oms_install_receipt_path)"
  [ -f "$receipt" ] && [ "$(oms_install_receipt_owner "$receipt" 2>/dev/null)" = "$(oms_install_physical_root "$ROOT")" ] || return 0
  oms_install_receipt_set_component tick "$1" "$receipt" >/dev/null 2>&1 || true
}
tool_path() {  # PATH for the trigger: user managers and cron do not see nvm or ~/.local/bin
  local t d out=""
  for t in ntn codex gh claude agy; do
    d="$(command -v "$t" 2>/dev/null)" || continue
    d="$(dirname "$d")"
    case ":$out:" in *":$d:"*) ;; *) out="${out:+$out:}$d" ;; esac
  done
  printf '%s' "${out:+$out:}$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"
}
pick_method() {
  case "$METHOD" in
    systemd|cron) printf '%s\n' "$METHOD" ;;
    auto) if systemd_usable; then echo systemd; elif command -v crontab >/dev/null 2>&1; then echo cron; else echo none; fi ;;
    *) echo "error: --method must be auto, systemd, or cron" >&2; return 2 ;;
  esac
}
install_tick() {
  local method; method="$(pick_method)" || exit 2
  [ "$DRY_RUN" = 1 ] || [ ! -d "$PWD/.oms" ] || register_repo "$PWD" >/dev/null || true
  case "$method" in
    systemd)
      if [ "$DRY_RUN" = 1 ]; then echo "would install systemd user timer: $TIMER_FILE (hourly)"; return 0; fi
      mkdir -p "$SYSTEMD_DIR"
      printf '[Unit]\nDescription=oh-my-setting tick\n[Service]\nType=oneshot\nEnvironment=PATH=%s\nExecStart="%s/scripts/tick.sh" run\n' "$(tool_path)" "$ROOT" > "$SERVICE_FILE"
      printf '[Unit]\nDescription=Run oh-my-setting tick hourly\n[Timer]\nOnCalendar=hourly\nPersistent=true\nRandomizedDelaySec=10m\n[Install]\nWantedBy=timers.target\n' > "$TIMER_FILE"
      systemctl --user daemon-reload
      systemctl --user enable --now oh-my-setting-tick.timer >/dev/null
      echo "tick trigger: systemd timer installed (hourly)" ;;
    cron)
      local line
      line="23 * * * * PATH=$(tool_path) \"$ROOT/scripts/tick.sh\" run >/dev/null 2>&1 $CRON_MARK"
      if [ "$DRY_RUN" = 1 ]; then echo "would install cron line: $line"; return 0; fi
      { crontab -l 2>/dev/null | grep -Fv "$CRON_MARK" || true; printf '%s\n' "$line"; } | crontab -
      echo "tick trigger: cron line installed (hourly)" ;;
    none) echo "error: neither systemd --user nor crontab is available" >&2; exit 1 ;;
  esac
  record_component true
}
uninstall_tick() {
  if [ "$DRY_RUN" = 1 ]; then echo "would remove the tick timer and cron line"; return 0; fi
  if [ -e "$TIMER_FILE" ] || [ -e "$SERVICE_FILE" ]; then
    if systemd_owns_unit; then
      systemctl --user disable --now oh-my-setting-tick.timer >/dev/null 2>&1 || true
      rm -f "$TIMER_FILE" "$SERVICE_FILE"
      systemctl --user daemon-reload >/dev/null 2>&1 || true
    else
      rm -f "$TIMER_FILE" "$SERVICE_FILE"
    fi
  fi
  if command -v crontab >/dev/null 2>&1 && crontab -l 2>/dev/null | grep -Fq "$CRON_MARK"; then
    crontab -l 2>/dev/null | grep -Fv "$CRON_MARK" | crontab - || true
  fi
  record_component false
  echo "tick trigger removed"
}
status_tick() {
  local root
  if [ -f "$TIMER_FILE" ]; then
    if systemd_owns_unit; then echo "timer: systemd (owned): $TIMER_FILE"; else echo "timer: unit file present, not wired: $TIMER_FILE"; fi
  elif command -v crontab >/dev/null 2>&1 && crontab -l 2>/dev/null | grep -Fq "$CRON_MARK"; then echo "timer: cron"
  else echo "timer: none (run: oms tick install)"; fi
  [ -f "$REGISTRY" ] || { echo "registry: empty"; return 0; }
  while IFS= read -r root; do
    [ -n "$root" ] || continue
    if [ -f "$root/.oms/tick/last.json" ]; then
      printf '%s  last: %s\n' "$root" "$(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r["ran_at"], "journal_rc=%s threads_closed=%d tasks_closed=%d plans_retired=%d" % (r["journal_rc"], len(r["threads_closed"]), int(r.get("tasks_closed", 0)), int(r.get("plans_retired", 0))))' "$root/.oms/tick/last.json" 2>/dev/null | tr -d '\r' || echo '?')"
    else printf '%s  never swept\n' "$root"; fi
  done < "$REGISTRY"
}

case "$MODE" in
  run) run_tick ;;
  register) register_repo "${REPOS[0]:-$PWD}" ;;
  unregister) unregister_repo "${REPOS[0]:-$PWD}" ;;
  install) install_tick ;;
  uninstall) uninstall_tick ;;
  status) status_tick ;;
esac
