#!/usr/bin/env bash
set -euo pipefail

REPO_URL="${OH_MY_SETTING_REPO_URL:-https://github.com/eightmm/oh-my-setting.git}"
DEST="${OH_MY_SETTING_DIR:-$HOME/.oh-my-setting}"
INSTALLER_DEFAULT_REF="edge"
REF="${OH_MY_SETTING_REF:-$INSTALLER_DEFAULT_REF}"
PROFILE="${OH_MY_SETTING_PROFILE:-minimal}"
GENERATE_SLURM="${OH_MY_SETTING_GENERATE_SLURM:-0}"
# Default auto: skills that plan GPU/Slurm/tsp work read local/machine.md, so
# every install writes the local-only snapshot unless explicitly opted out.
GENERATE_MACHINE="${OH_MY_SETTING_GENERATE_MACHINE:-auto}"
# Tool installation is capability-scoped: a fresh install provides the core
# runtime (Bash, Git, Python, one coding-agent provider) and records a private
# capability receipt; optional capabilities (council, github, notion, research,
# hpc, container, remote) install only when selected, and --full remains the
# explicit compatibility path with the historical all-tools footprint. Missing
# optional capabilities report as unavailable — never as success.
INSTALL_TOOLS="${OH_MY_SETTING_INSTALL_TOOLS:-1}"
CAPABILITY_PROFILES="${OH_MY_SETTING_CAPABILITY_PROFILES:-core}"
PRIMARY_PROVIDER="${OH_MY_SETTING_PRIMARY_PROVIDER:-auto}"
TOOLS_FULL=0
CONNECT_SERVICES="${OH_MY_SETTING_CONNECT_SERVICES:-auto}"
STAR_PROMPT="${OH_MY_SETTING_STAR_PROMPT:-0}"
# The apply-mode update trigger is a default, not an opt-in: a harness that
# silently goes stale stops matching the docs and skills its agents trust.
# Apply fast-forwards a clean checkout on the daily schedule; recording
# without touching stays available (OH_MY_SETTING_AUTO_UPDATE_MODE=check),
# and --no-auto-update opts out.
AUTO_UPDATE="${OH_MY_SETTING_AUTO_UPDATE:-1}"
CODEX_PLUGIN="${OH_MY_SETTING_CODEX_PLUGIN:-auto}"
PEER_PERMISSIONS="${OH_MY_SETTING_PEER_PERMISSIONS:-0}"
NOTION_DATA_SOURCE_ID="${OH_MY_SETTING_NOTION_DATA_SOURCE_ID:-${OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID:-}}"
# Re-exec protocol marker. A standalone source installer holds the lifecycle
# lock itself when the selected ref predates this protocol; current checkouts
# adopt the same lock across exec without opening a second mutation window.
# shellcheck disable=SC2034
OMS_INSTALL_LIFECYCLE_PROTOCOL=1

usage() {
  cat <<'EOF'
Usage: install.sh [--ref REF] [--profile NAME]... [--primary-provider NAME] [--full] [--connect-services] [--no-connect-services] [--no-auto-update] [--machine-snapshot] [--slurm-snapshot] [--notion-data-source ID] [--peer-permissions] [--star] [--help]

Options:
  --ref REF           Install edge, a tag, branch, or commit (default: installer channel).
  --profile NAME      Add a capability profile to the default core install:
                      council, github, notion, research, hpc, container,
                      remote, or full. Repeatable. Fresh default: core (Bash,
                      Git, Python, one coding-agent provider, no service
                      logins); everything else reports as unavailable until
                      selected.
  --primary-provider NAME  codex, claude, or agy for the core provider
                      (default auto: reuse an installed one, else codex).
  --full              Explicit compatibility install: the historical all-tools
                      council footprint, machine snapshot, and update timer.
  --tools             Install the full council toolset (Node, uv, provider
                      CLIs, gh, ntn) without the rest of --full.
  --connect-services  Require interactive gh and Notion login plus journal linking.
  --no-connect-services
                      Skip account login and automatic journal discovery.
  --auto-update       Install the apply-mode update timer (already the default).
  --no-auto-update    Skip the auto-update trigger.
  --machine-snapshot  Generate local machine metadata (already the default;
                      opt out with OH_MY_SETTING_GENERATE_MACHINE=0).
  --slurm-snapshot    Generate local Slurm cluster metadata when available.
  --notion-data-source ID
                      Select the Work Journal Notion mirror. Authentication is
                      owned by ntn and is never persisted by oh-my-setting.
  --peer-permissions  Grant Antigravity the standing consult permissions
                      (read_file(*), command(*)) in its user-global
                      settings so headless council calls are not auto-denied.
                      All-MCP wildcard access stays approval-gated.
                      Tracks only newly added rules for surgical uninstall and
                      keeps a one-time .bak. Without this flag, only reports.
  --star              Offer the optional GitHub star prompt.
  --no-star           Skip the star prompt (compatibility; default).
  --help              Show this help.

Environment:
  OH_MY_SETTING_STAR_PROMPT=1      Enable the GitHub star prompt.
  OH_MY_SETTING_REF=edge|REF       Track edge or pin an exact Git ref.
  OH_MY_SETTING_PROFILE=NAME       Receipt profile: minimal, full, or custom.
  OH_MY_SETTING_CLAUDE_HOOKS=0     Skip Claude Code hooks and usage HUD.
  OH_MY_SETTING_CODEX_PLUGIN=0|1|auto  Skip, require, or auto-detect Codex plugin setup.
  OH_MY_SETTING_PEER_PERMISSIONS=1 Grant Antigravity consult permissions.
  OH_MY_SETTING_GENERATE_MACHINE=0 Skip the machine snapshot (default: auto).
  OH_MY_SETTING_GENERATE_SLURM=1   Generate a Slurm snapshot.
  OH_MY_SETTING_CONNECT_SERVICES=auto|required|0
                                   Auto-connect in an interactive terminal,
                                   require connection, or skip it (default: auto).
  OH_MY_SETTING_AUTO_UPDATE=0      Skip the auto-update trigger (default: 1).
  OH_MY_SETTING_NOTION_DATA_SOURCE_ID=ID
                                   Configure the Work Journal Notion mirror.
  OH_MY_SETTING_AUTO_UPDATE_MODE=check  Only record available updates (default: apply).
  OH_MY_SETTING_REQUIRE_TOOLS=0    Let doctor treat a missing CLI as optional
                                   (default: 1 whenever the tools were installed).
  OH_MY_SETTING_LINK_MODE=MODE     auto, symlink, or copy. auto uses copies on
                                   Windows Git Bash and symlinks elsewhere.
  OH_MY_SETTING_DIR=/path/to/dir   Install location.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --ref)
      [ "$#" -ge 2 ] || { echo "error: --ref requires a value" >&2; exit 2; }
      REF="$2"
      shift
      ;;
    --full)
      PROFILE=full
      INSTALL_TOOLS=1
      TOOLS_FULL=1
      GENERATE_MACHINE=auto
      GENERATE_SLURM=auto
      AUTO_UPDATE=1
      ;;
    --tools)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      INSTALL_TOOLS=1
      TOOLS_FULL=1
      ;;
    --profile)
      [ "$#" -ge 2 ] || { echo "error: --profile requires a value" >&2; exit 2; }
      [ "$PROFILE" = "full" ] || PROFILE=custom
      case ",$CAPABILITY_PROFILES," in
        *",$2,"*) ;;
        *) CAPABILITY_PROFILES="$CAPABILITY_PROFILES,$2" ;;
      esac
      shift
      ;;
    --primary-provider)
      [ "$#" -ge 2 ] || { echo "error: --primary-provider requires a value" >&2; exit 2; }
      PRIMARY_PROVIDER="$2"
      shift
      ;;
    --connect-services)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      CONNECT_SERVICES=required
      ;;
    --no-connect-services)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      CONNECT_SERVICES=0
      ;;
    --auto-update)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      AUTO_UPDATE=1
      ;;
    --no-auto-update)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      AUTO_UPDATE=0
      ;;
    --machine-snapshot)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      GENERATE_MACHINE=1
      ;;
    --slurm-snapshot)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      GENERATE_SLURM=1
      ;;
    --notion-data-source)
      [ "$#" -ge 2 ] || {
        echo "error: --notion-data-source requires a value" >&2
        exit 2
      }
      [ "$PROFILE" = "full" ] || PROFILE=custom
      NOTION_DATA_SOURCE_ID="$2"
      shift
      ;;
    --peer-permissions)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      PEER_PERMISSIONS=1
      ;;
    --star)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      STAR_PROMPT=1
      ;;
    --no-star)
      STAR_PROMPT=0
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

case "$PROFILE" in
  minimal|full|custom) ;;
  *) echo "error: OH_MY_SETTING_PROFILE must be minimal, full, or custom" >&2; exit 2 ;;
esac
case "$INSTALL_TOOLS" in
  1) ;;
  0)
    echo "error: tool installation is required; OH_MY_SETTING_INSTALL_TOOLS=0 is no longer supported" >&2
    exit 2
    ;;
  *) echo "error: OH_MY_SETTING_INSTALL_TOOLS must be 1" >&2; exit 2 ;;
esac
case "$PEER_PERMISSIONS" in
  0|1) ;;
  *) echo "error: OH_MY_SETTING_PEER_PERMISSIONS must be 0 or 1" >&2; exit 2 ;;
esac
IFS=',' read -r -a _capability_list <<EOF_CAPS
$CAPABILITY_PROFILES
EOF_CAPS
for _capability in "${_capability_list[@]}"; do
  case "$_capability" in
    core|council|github|notion|research|hpc|container|remote|full) ;;
    *)
      echo "error: unknown capability profile: $_capability" >&2
      exit 2
      ;;
  esac
done
case "$PRIMARY_PROVIDER" in
  auto|codex|claude|agy) ;;
  *) echo "error: --primary-provider must be auto, codex, claude, or agy" >&2; exit 2 ;;
esac
# Default service connection is none: the installer never runs a service login
# the user did not select. --full keeps the historical auto behaviour, and an
# explicit --connect-services remains an explicit choice.
if [ "$TOOLS_FULL" -eq 0 ] && [ "$CONNECT_SERVICES" = auto ]; then
  CONNECT_SERVICES=0
fi
case "$CONNECT_SERVICES" in
  1) CONNECT_SERVICES=required ;;
  auto|required|0) ;;
  *)
    echo "error: OH_MY_SETTING_CONNECT_SERVICES must be auto, required, or 0" >&2
    exit 2
    ;;
esac
case "$GENERATE_MACHINE:$GENERATE_SLURM" in
  0:0|0:1|0:auto|1:0|1:1|1:auto|auto:0|auto:1|auto:auto) ;;
  *) echo "error: snapshot modes must be 0, 1, or auto" >&2; exit 2 ;;
esac
case "$REF" in
  edge) ;;
  ""|-*|/*|*/|.*|*.|*..*|*//*|*/.*|*.lock|*.lock/*|*[!A-Za-z0-9._/-]*)
    echo "error: unsafe OH_MY_SETTING_REF: $REF" >&2
    exit 2
    ;;
esac

export OH_MY_SETTING_REF="$REF"
export OH_MY_SETTING_PROFILE="$PROFILE"
export OH_MY_SETTING_STAR_PROMPT="$STAR_PROMPT"
export OH_MY_SETTING_INSTALL_TOOLS="$INSTALL_TOOLS"
export OH_MY_SETTING_CONNECT_SERVICES="$CONNECT_SERVICES"
export OH_MY_SETTING_GENERATE_MACHINE="$GENERATE_MACHINE"
export OH_MY_SETTING_GENERATE_SLURM="$GENERATE_SLURM"
export OH_MY_SETTING_AUTO_UPDATE="$AUTO_UPDATE"
export OH_MY_SETTING_CODEX_PLUGIN="$CODEX_PLUGIN"
export OH_MY_SETTING_PEER_PERMISSIONS="$PEER_PERMISSIONS"
export OH_MY_SETTING_NOTION_DATA_SOURCE_ID="$NOTION_DATA_SOURCE_ID"

# A checkout invocation uses the shared helper. The documented curl/pipe form
# has only this file before the first clone, so it carries the same mkdir-lock
# protocol inline until the freshly selected checkout can adopt it.
INSTALL_ENTRY_DIR=""
INSTALL_ENTRY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd -P)" ||
  INSTALL_ENTRY_DIR=""
if [ -n "$INSTALL_ENTRY_DIR" ] &&
   [ -f "$INSTALL_ENTRY_DIR/scripts/lib/install-lifecycle-lock.sh" ]; then
  # shellcheck source=scripts/lib/install-lifecycle-lock.sh
  . "$INSTALL_ENTRY_DIR/scripts/lib/install-lifecycle-lock.sh"
else
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=0
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH=""
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER=""
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID=""
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START=""
  OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_PATH=""
  OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_OWNER=""

  oms_install_lifecycle_inline_claim_release() {
    local recorded=""
    local claim="$OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_PATH"
    [ -n "$claim" ] || return 0
    if [ -d "$claim" ] && [ ! -L "$claim" ]; then
      recorded="$(oms_install_lifecycle_lock_read \
        "$claim/owner")"
      if [ "$recorded" = "$OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_OWNER" ]; then
        rm -f "$claim/pid" "$claim/pid-start" "$claim/started" "$claim/owner"
        rmdir "$claim" 2>/dev/null || return 75
        rmdir "$(dirname "$claim")" 2>/dev/null || true
      fi
    fi
    OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_PATH=""
    OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_OWNER=""
  }

  oms_install_lifecycle_lock_process_start() {
    local pid="$1"
    local line=""
    local rest=""
    local value=""

    case "$(uname -s 2>/dev/null || true)" in
      MINGW*|MSYS*|CYGWIN*) return 0 ;;
    esac

    if [ -r "/proc/$pid/stat" ]; then
      IFS= read -r line < "/proc/$pid/stat" || line=""
      rest="${line##*) }"
      if [ "$rest" != "$line" ]; then
        value="$(printf '%s\n' "$rest" | awk 'NF >= 20 { print $20; exit }')"
        case "$value" in
          *[!0-9]*|"") ;;
          *) printf 'proc:%s\n' "$value"; return 0 ;;
        esac
      fi
    fi
    value="$(LC_ALL=C ps -p "$pid" -o lstart= 2>/dev/null || true)"
    value="$(printf '%s\n' "$value" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/[[:space:]][[:space:]]*/ /g')"
    [ -n "$value" ] && printf 'ps:%s\n' "$value"
  }

  oms_install_lifecycle_lock_current_identity() {
    local pid=""
    local process_start=""

    if [ "${BASH_VERSINFO[0]:-0}" -ge 4 ] 2>/dev/null &&
       [ -n "${BASHPID:-}" ]; then
      pid="$BASHPID"
    else
      # exec makes the probe child the substitution fork itself, so its PPID
      # is this shell. Stock Bash 3.2 otherwise hands sh an ephemeral
      # intermediate fork, and the recorded pid is dead before adopt or
      # release ever compares it (the 2026-08-10 macOS e2e lock leak).
      pid="$(exec sh -c 'printf "%s\n" "$PPID"' 2>/dev/null)" || pid=""
      pid="${pid//$'\r'/}"
      [ -n "$pid" ] || return 75
    fi
    case "$pid" in *[!0-9]*|"") return 75 ;; esac
    process_start="$(oms_install_lifecycle_lock_process_start "$pid")"
    process_start="${process_start//$'\r'/}"
    OMS_INSTALL_LIFECYCLE_CURRENT_PID="$pid"
    OMS_INSTALL_LIFECYCLE_CURRENT_PID_START="$process_start"
  }

  oms_install_lifecycle_lock_path() {
    local raw="${OMS_INSTALL_LIFECYCLE_LOCK:-${OMS_LOCK_DIR:-$HOME/.cache/oh-my-setting/locks}/install-lifecycle.lock.d}"
    local parent
    local name

    case "$raw" in /*) ;; *) raw="$PWD/$raw" ;; esac
    parent="$(dirname "$raw")"
    name="$(basename "$raw")"
    if [ "$name" != "install-lifecycle.lock.d" ]; then
      echo "error: invalid install lifecycle lock path: $raw" >&2
      return 75
    fi
    mkdir -p "$parent" || return
    parent="$(cd "$parent" && pwd -P)" || return
    printf '%s/%s\n' "$parent" "$name"
  }

  oms_install_lifecycle_lock_timeout() {
    local timeout="${OMS_INSTALL_LIFECYCLE_LOCK_TIMEOUT:-${OMS_LOCK_TIMEOUT:-300}}"

    case "$timeout" in *[!0-9]*|"") timeout=300 ;; esac
    [ "$timeout" -gt 0 ] || timeout=300
    printf '%s\n' "$timeout"
  }

  oms_install_lifecycle_lock_read() {
    local value=""

    [ -f "$1" ] && value="$(sed -n '1p' "$1" 2>/dev/null || true)"
    printf '%s\n' "${value//$'\r'/}"
  }

  oms_install_lifecycle_inline_claim_stale() {
    local claim="$1" timeout="$2" now="$3"
    local pid pid_start actual_start started
    pid="$(oms_install_lifecycle_lock_read "$claim/pid")"
    pid_start="$(oms_install_lifecycle_lock_read "$claim/pid-start")"
    started="$(oms_install_lifecycle_lock_read "$claim/started")"
    case "$pid" in
      *[!0-9]*|"")
        case "$started" in
          *[!0-9]*|"")
            started="$(basename "$claim")"
            started="${started%%.*}"
            case "$started" in *[!0-9]*|"") return 1 ;; esac
            [ $((now - started)) -ge "$timeout" ]
            ;;
          *) [ $((now - started)) -ge "$timeout" ] ;;
        esac
        ;;
      *)
        if ! kill -0 "$pid" 2>/dev/null; then return 0; fi
        if [ -n "$pid_start" ]; then
          actual_start="$(oms_install_lifecycle_lock_process_start "$pid")"
          actual_start="${actual_start//$'\r'/}"
          [ -n "$actual_start" ] && [ "$actual_start" != "$pid_start" ]
          return $?
        fi
        return 1
        ;;
    esac
  }

  oms_install_lifecycle_inline_claim_acquire() {
    local path="$1" timeout="$2" owner="$3" start="$4"
    local root="$1.recovery-claims.d" name claim candidate candidate_name
    local now elapsed first tick_owner
    local -a names=()

    [ ! -L "$root" ] || return 75
    mkdir -p "$root" || return 75
    [ -d "$root" ] && [ ! -L "$root" ] || return 75
    name="$(date +%s).$owner"
    claim="$root/$name"
    mkdir "$claim" 2>/dev/null || return 75
    if ! printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" > "$claim/pid" ||
       ! printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" > "$claim/pid-start" ||
       ! printf '%s\n' "$(date +%s)" > "$claim/started" ||
       ! printf '%s\n' "$owner" > "$claim/owner"; then
      rm -f "$claim/pid" "$claim/pid-start" "$claim/started" "$claim/owner"
      rmdir "$claim" 2>/dev/null || true
      return 75
    fi
    OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_PATH="$claim"
    OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_OWNER="$owner"

    while :; do
      now="$(date +%s)"
      elapsed=$((now - start))
      names=()
      for candidate in "$root"/*; do
        [ -e "$candidate" ] || continue
        [ -d "$candidate" ] && [ ! -L "$candidate" ] || {
          oms_install_lifecycle_inline_claim_release || true
          return 75
        }
        if [ "$candidate" != "$claim" ] &&
           oms_install_lifecycle_inline_claim_stale "$candidate" "$timeout" "$now"; then
          tick_owner="$(oms_install_lifecycle_lock_read "$candidate/owner")"
          rm -f "$candidate/pid" "$candidate/pid-start" \
            "$candidate/started" "$candidate/owner"
          rmdir "$candidate" 2>/dev/null || true
          [ ! -e "$candidate" ] || [ "$(oms_install_lifecycle_lock_read \
            "$candidate/owner")" != "$tick_owner" ] || return 75
        fi
        [ -d "$candidate" ] || continue
        candidate_name="$(basename "$candidate")"
        names+=("$candidate_name")
      done
      first="$(printf '%s\n' "${names[@]}" | LC_ALL=C sort | sed -n '1p')"
      if [ "$first" = "$name" ] && [ -d "$claim" ] &&
         [ "$(oms_install_lifecycle_lock_read "$claim/owner")" = "$owner" ]; then
        return 0
      fi
      [ "$elapsed" -lt "$timeout" ] || {
        oms_install_lifecycle_inline_claim_release || true
        return 75
      }
      sleep 1
    done
  }

  oms_install_lifecycle_lock_adopt() {
    local path="$1"
    local recorded_owner
    local recorded_pid
    local recorded_pid_start

    [ "${OMS_INSTALL_LIFECYCLE_LOCK_HELD:-0}" = 1 ] || return 1
    [ "${OMS_INSTALL_LIFECYCLE_LOCK_PATH:-}" = "$path" ] || return 1
    [ -d "$path" ] && [ ! -L "$path" ] || return 1
    recorded_owner="$(oms_install_lifecycle_lock_read "$path/owner")"
    recorded_pid="$(oms_install_lifecycle_lock_read "$path/pid")"
    recorded_pid_start="$(oms_install_lifecycle_lock_read "$path/pid-start")"
    [ -n "${OMS_INSTALL_LIFECYCLE_LOCK_OWNER:-}" ] || return 1
    [ "$recorded_owner" = "$OMS_INSTALL_LIFECYCLE_LOCK_OWNER" ] || return 1
    oms_install_lifecycle_lock_current_identity || return 1
    [ "$recorded_pid" = "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" ] || return 1
    if [ -n "$recorded_pid_start" ]; then
      [ -n "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" ] || return 1
      [ "$recorded_pid_start" = "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" ] || return 1
    fi
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=1
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH="$path"
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER="$recorded_owner"
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID="$recorded_pid"
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START="$recorded_pid_start"
  }

  oms_install_lifecycle_lock_acquire() {
    local action="${1:-mutate the install}"
    local path
    local timeout
    local owner
    local start
    local now
    local elapsed
    local pid
    local pid_start
    local actual_pid_start
    local recorded_started
    local stale
    local stale_path
    local recorded_owner
    local snapshot
    local current_snapshot

    oms_install_lifecycle_inline_initialize() {
      if ! {
        printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" > "$path/pid" &&
        printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" > "$path/pid-start" &&
        printf '%s\n' "$(date +%s)" > "$path/started" &&
        printf '%s\n' "$owner" > "$path/owner"
      }; then
        rm -rf "$path"
        return 75
      fi
    }

    path="$(oms_install_lifecycle_lock_path)" || return 75
    if oms_install_lifecycle_lock_adopt "$path"; then
      return 0
    fi
    timeout="$(oms_install_lifecycle_lock_timeout)"
    oms_install_lifecycle_lock_current_identity || {
      echo "error: could not determine install lifecycle process identity" >&2
      return 75
    }
    owner="$OMS_INSTALL_LIFECYCLE_CURRENT_PID.$(date +%s).${RANDOM:-0}"
    start="$(date +%s)"
    oms_install_lifecycle_inline_claim_acquire "$path" "$timeout" "$owner" "$start" ||
      return 75
    while :; do
      now="$(date +%s)"
      elapsed=$((now - start))
      if mkdir "$path" 2>/dev/null; then
        if ! oms_install_lifecycle_inline_initialize; then
          oms_install_lifecycle_inline_claim_release || true
          echo "error: could not initialize install lifecycle lock: $path" >&2
          return 75
        fi
        oms_install_lifecycle_inline_claim_release || return 75
        OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=1
        OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH="$path"
        OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER="$owner"
        OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID="$OMS_INSTALL_LIFECYCLE_CURRENT_PID"
        OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START="$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START"
        OMS_INSTALL_LIFECYCLE_LOCK_HELD=1
        OMS_INSTALL_LIFECYCLE_LOCK_PATH="$path"
        OMS_INSTALL_LIFECYCLE_LOCK_OWNER="$owner"
        export OMS_INSTALL_LIFECYCLE_LOCK_HELD OMS_INSTALL_LIFECYCLE_LOCK_PATH \
          OMS_INSTALL_LIFECYCLE_LOCK_OWNER
        return 0
      fi

      if [ -L "$path" ] || [ ! -d "$path" ]; then
        oms_install_lifecycle_inline_claim_release || true
        echo "error: install lifecycle lock is not a directory: $path" >&2
        return 75
      fi

      stale=0
      pid="$(oms_install_lifecycle_lock_read "$path/pid")"
      recorded_owner="$(oms_install_lifecycle_lock_read "$path/owner")"
      pid_start="$(oms_install_lifecycle_lock_read "$path/pid-start")"
      recorded_started="$(oms_install_lifecycle_lock_read "$path/started")"
      snapshot="$recorded_owner|$pid|$pid_start|$recorded_started"
      case "$pid" in
        *[!0-9]*|"")
          case "$recorded_started" in
            *[!0-9]*|"")
              [ "$elapsed" -ge "$timeout" ] && stale=1
              ;;
            *)
              if [ "$elapsed" -ge "$timeout" ] ||
                 [ $((now - recorded_started)) -ge "$timeout" ]; then
                stale=1
              fi
              ;;
          esac
          ;;
        *)
          if ! kill -0 "$pid" 2>/dev/null; then
            stale=1
          else
            if [ -n "$pid_start" ]; then
              actual_pid_start="$(oms_install_lifecycle_lock_process_start "$pid")"
              actual_pid_start="${actual_pid_start//$'\r'/}"
              if [ -n "$actual_pid_start" ] &&
                 [ "$pid_start" != "$actual_pid_start" ]; then
                stale=1
              fi
            fi
          fi
          ;;
      esac
      if [ "$stale" = 1 ]; then
        if [ -n "${OMS_TEST_INSTALL_LIFECYCLE_STALE_BARRIER:-}" ]; then
          printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" > \
            "$OMS_TEST_INSTALL_LIFECYCLE_STALE_BARRIER.ready"
          while [ ! -e "$OMS_TEST_INSTALL_LIFECYCLE_STALE_BARRIER.release" ]; do
            sleep 1
          done
        fi
        current_snapshot="$(
          printf '%s|%s|%s|%s\n' \
            "$(oms_install_lifecycle_lock_read "$path/owner")" \
            "$(oms_install_lifecycle_lock_read "$path/pid")" \
            "$(oms_install_lifecycle_lock_read "$path/pid-start")" \
            "$(oms_install_lifecycle_lock_read "$path/started")"
        )"
        stale_path="$path.stale.$OMS_INSTALL_LIFECYCLE_CURRENT_PID.$now.$RANDOM"
        if [ "$current_snapshot" = "$snapshot" ] &&
           mv "$path" "$stale_path" 2>/dev/null; then
          rm -rf "$stale_path"
          if mkdir "$path" 2>/dev/null && oms_install_lifecycle_inline_initialize; then
            oms_install_lifecycle_inline_claim_release || return 75
            OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=1
            OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH="$path"
            OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER="$owner"
            OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID="$OMS_INSTALL_LIFECYCLE_CURRENT_PID"
            OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START="$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START"
            OMS_INSTALL_LIFECYCLE_LOCK_HELD=1
            OMS_INSTALL_LIFECYCLE_LOCK_PATH="$path"
            OMS_INSTALL_LIFECYCLE_LOCK_OWNER="$owner"
            export OMS_INSTALL_LIFECYCLE_LOCK_HELD OMS_INSTALL_LIFECYCLE_LOCK_PATH \
              OMS_INSTALL_LIFECYCLE_LOCK_OWNER
            return 0
          fi
        fi
      fi
      if [ "$elapsed" -ge "$timeout" ]; then
        oms_install_lifecycle_inline_claim_release || true
        echo "error: could not acquire lock for install lifecycle ($action) after ${timeout}s: $path" >&2
        echo "error: install lifecycle lock is busy: $path" >&2
        return 75
      fi
      sleep 1
    done
  }

  oms_install_lifecycle_lock_release() {
    local recorded_owner
    local recorded_pid
    local recorded_pid_start

    oms_install_lifecycle_inline_claim_release || true
    [ "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL" = 1 ] || return 0
    oms_install_lifecycle_lock_current_identity || return 0
    [ "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" = "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID" ] ||
      return 0
    if [ -n "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START" ]; then
      [ "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" = \
        "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START" ] || return 0
    fi
    if [ -d "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH" ] &&
       [ ! -L "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH" ]; then
      recorded_owner="$(oms_install_lifecycle_lock_read \
        "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/owner")"
      recorded_pid="$(oms_install_lifecycle_lock_read \
        "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/pid")"
      recorded_pid_start="$(oms_install_lifecycle_lock_read \
        "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/pid-start")"
      if [ "$recorded_owner" = "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER" ] &&
         [ "$recorded_pid" = "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID" ] &&
         [ "$recorded_pid_start" = "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START" ]; then
        rm -f "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/pid" \
          "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/pid-start" \
          "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/started" \
          "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/owner"
        rmdir "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH" 2>/dev/null ||
          echo "warning: install lifecycle lock directory was not empty: $OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH" >&2
      fi
    fi
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=0
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH=""
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER=""
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID=""
    OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START=""
    unset OMS_INSTALL_LIFECYCLE_LOCK_HELD OMS_INSTALL_LIFECYCLE_LOCK_PATH \
      OMS_INSTALL_LIFECYCLE_LOCK_OWNER
  }
fi

install_lifecycle_exit() {
  local code=$?

  trap - EXIT HUP INT TERM
  oms_install_lifecycle_lock_release
  exit "$code"
}
trap install_lifecycle_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
oms_install_lifecycle_lock_acquire install || exit $?

run_as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    echo "error: need root privileges for: $*" >&2
    exit 1
  fi
}

install_git_if_missing() {
  if command -v git >/dev/null 2>&1; then
    return 0
  fi

  echo "git missing; attempting install"

  if command -v apt-get >/dev/null 2>&1; then
    run_as_root apt-get update
    run_as_root apt-get install -y git
  elif command -v dnf >/dev/null 2>&1; then
    run_as_root dnf install -y git
  elif command -v yum >/dev/null 2>&1; then
    run_as_root yum install -y git
  elif command -v pacman >/dev/null 2>&1; then
    run_as_root pacman -Sy --noconfirm git
  elif command -v brew >/dev/null 2>&1; then
    brew install git
  else
    echo "error: git is required; install it manually and rerun" >&2
    exit 1
  fi
}

load_user_tool_paths() {
  local locked_node=""
  local managed_node_bin=""

  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ -x "$DEST/scripts/lib/tool-lock.py" ] && [ -f "$DEST/tools.lock.json" ]; then
    locked_node="$(python3 "$DEST/scripts/lib/tool-lock.py" \
      --lock "$DEST/tools.lock.json" get node.version | tr -d '\r')"
    managed_node_bin="$NVM_DIR/versions/node/v$locked_node/bin"
    if [ -x "$managed_node_bin/node" ] &&
       [ "$("$managed_node_bin/node" --version 2>/dev/null | tr -d '\r')" = "v$locked_node" ]; then
      export PATH="$managed_node_bin:$PATH"
    fi
  fi
}

ensure_python3() {
  local candidate=""
  local uv_interp=""
  local shim="$HOME/.local/bin/python3"

  export PATH="$HOME/.local/bin:$PATH"
  if command -v python3 >/dev/null 2>&1 &&
     python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
    return 0
  fi
  if oms_platform_is_windows; then
    if command -v python >/dev/null 2>&1 &&
       python -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
      candidate=python
    elif command -v py >/dev/null 2>&1 &&
         py -3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
      candidate=py
    fi
  fi
  # uv-only machines (fresh workstations, cluster accounts) carry a managed
  # CPython without exposing any python3 command. Adopt it through a managed
  # shim that resolves `uv python find` at call time, so the shim survives uv
  # relocating or upgrading its interpreters.
  if [ -z "$candidate" ] && command -v uv >/dev/null 2>&1; then
    uv_interp="$(uv python find 2>/dev/null || true)"
    if [ -z "$uv_interp" ]; then
      uv python install >/dev/null 2>&1 || true
      uv_interp="$(uv python find 2>/dev/null || true)"
    fi
    if [ -n "$uv_interp" ] &&
       "$uv_interp" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
      candidate=uv
    fi
  fi

  if [ -z "$candidate" ]; then
    if oms_platform_is_windows; then
      echo "error: Python 3.9+ is required; install Python and rerun" >&2
    else
      echo "error: a Python 3.9+ 'python3' command is required (installing uv also satisfies this)" >&2
    fi
    exit 1
  fi

  # A shim we wrote whose interpreter has since moved is repairable, and
  # refusing it would mean no reinstall can ever fix what an install created.
  # Anything else at that path is the user's launcher and stays untouched.
  if [ -e "$shim" ] || [ -L "$shim" ]; then
    if oms_install_python_shim_owned "$shim"; then
      rm -f "$shim"
      echo "replacing stale managed python3 shim: $shim"
    else
      echo "error: refusing to replace existing Python launcher: $shim" >&2
      exit 1
    fi
  fi
  mkdir -p "$HOME/.local/bin"
  case "$candidate" in
    py)
      printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
        'exec py -3 "$@"' > "$shim"
      ;;
    uv)
      printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
        'exec "$(uv python find)" "$@"' > "$shim"
      ;;
    *)
      printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
        'exec python "$@"' > "$shim"
      ;;
  esac
  chmod +x "$shim"
  echo "python3 shim: $shim -> $candidate"
}

install_git_if_missing

# Reinstall is also an update path. Refuse before fetch/checkout so a local
# edit or untracked file in the managed checkout can never be hidden, collided
# with, or later purged by an apparently routine setup rerun.
if [ -d "$DEST/.git" ]; then
  install_dirty="$(git -C "$DEST" status --porcelain --untracked-files=all 2>/dev/null)" || {
    echo "error: cannot inspect existing checkout before reinstall: $DEST" >&2
    exit 1
  }
  if [ -n "$install_dirty" ]; then
    echo "error: refusing to reinstall over a dirty managed checkout: $DEST" >&2
    printf '%s\n' "$install_dirty" | sed -n '1,20p' >&2
    echo "error: commit, stash, or move these files, then retry" >&2
    exit 1
  fi
fi

if [ "$REF" = "edge" ]; then
  if [ -d "$DEST/.git" ]; then
    git -C "$DEST" fetch --prune origin
    git -C "$DEST" remote set-head origin -a >/dev/null
    remote_head="$(git -C "$DEST" symbolic-ref --quiet --short refs/remotes/origin/HEAD || true)"
    [ -n "$remote_head" ] || { echo "error: cannot resolve origin default branch" >&2; exit 1; }
    edge_branch="${remote_head#origin/}"
    if git -C "$DEST" show-ref --verify --quiet "refs/heads/$edge_branch"; then
      git -C "$DEST" checkout "$edge_branch"
    else
      git -C "$DEST" checkout -b "$edge_branch" --track "$remote_head"
    fi
    git -C "$DEST" pull --ff-only origin "$edge_branch"
  else
    mkdir -p "$(dirname "$DEST")"
    git clone "$REPO_URL" "$DEST"
  fi
else
  if [ ! -d "$DEST/.git" ]; then
    mkdir -p "$(dirname "$DEST")"
    git clone --no-checkout "$REPO_URL" "$DEST"
  fi
  git -C "$DEST" fetch --prune --tags origin
  target="$(git -C "$DEST" rev-parse --verify --quiet "refs/tags/${REF}^{commit}" || true)"
  if [ -z "$target" ]; then
    target="$(git -C "$DEST" rev-parse --verify --quiet "origin/${REF}^{commit}" || true)"
  fi
  if [ -z "$target" ]; then
    target="$(git -C "$DEST" rev-parse --verify --quiet "${REF}^{commit}" || true)"
  fi
  [ -n "$target" ] || { echo "error: cannot resolve install ref: $REF" >&2; exit 1; }
  git -C "$DEST" checkout --detach "$target"
fi

# Continue from the updated checkout so old piped/local installers do not run stale logic.
if [ "${OH_MY_SETTING_REEXECED:-0}" != "1" ] && [ -f "$DEST/install.sh" ]; then
  export OH_MY_SETTING_REEXECED=1
  if grep -Fq 'OMS_INSTALL_LIFECYCLE_PROTOCOL=1' "$DEST/install.sh"; then
    exec bash "$DEST/install.sh"
  fi
  # An older selected ref cannot adopt the lock. Keep it in this parent while
  # the old installer runs as a child, then release it on our EXIT trap.
  bash "$DEST/install.sh"
  exit $?
fi

# One failure past this point can leave a partial install: tools present but
# targets unlinked, or hooks half-registered. The installer is idempotent and
# doctor relinks from the receipt, so a crash should name that recovery path
# instead of ending with only the failing tool's error.
install_failed() {
  local code="$1"

  [ "$code" -ne 0 ] || return 0
  {
    echo "error: install failed (exit $code)"
    echo "recover: rerun this installer, or run $DEST/scripts/doctor.sh --repair to fix links from the receipt"
  } >&2
}
install_lifecycle_exit() {
  local code=$?

  trap - EXIT HUP INT TERM
  install_failed "$code"
  oms_install_lifecycle_lock_release
  exit "$code"
}
trap install_lifecycle_exit EXIT

# shellcheck disable=SC1091
. "$DEST/scripts/lib/platform.sh"
ensure_python3

if [ "$TOOLS_FULL" -eq 1 ]; then
  # Explicit compatibility: the historical all-tools council footprint.
  "$DEST/scripts/install-tools.sh"
  export OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-1}"
else
  install_profile_args=(--apply --primary-provider "$PRIMARY_PROVIDER")
  for _capability in "${_capability_list[@]}"; do
    install_profile_args+=(--profile "$_capability")
  done
  "$DEST/scripts/install-profile.sh" "${install_profile_args[@]}"
  # A core install treats absent optional CLIs as unavailable capabilities,
  # not as a broken install; the capability receipt records what was chosen.
  export OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-0}"
fi
load_user_tool_paths

case "$CODEX_PLUGIN" in
  auto)
    if command -v codex >/dev/null 2>&1; then
      CODEX_PLUGIN=1
    else
      CODEX_PLUGIN=0
    fi
    ;;
  0|1) ;;
  *)
    echo "error: OH_MY_SETTING_CODEX_PLUGIN must be 0, 1, or auto" >&2
    exit 2
    ;;
esac
export OH_MY_SETTING_CODEX_PLUGIN="$CODEX_PLUGIN"

"$DEST/scripts/link.sh"

case "$CONNECT_SERVICES" in
  auto|required)
    connect_args=("--$CONNECT_SERVICES")
    if [ -n "$NOTION_DATA_SOURCE_ID" ]; then
      connect_args+=(--data-source-id "$NOTION_DATA_SOURCE_ID")
    fi
    "$DEST/scripts/connect-services.sh" "${connect_args[@]}"
    ;;
  0)
    if [ -n "$NOTION_DATA_SOURCE_ID" ]; then
      "$DEST/scripts/journal.sh" configure \
        --data-source-id "$NOTION_DATA_SOURCE_ID" --no-validate
    fi
    echo "skipping GitHub/Notion connection: OH_MY_SETTING_CONNECT_SERVICES=0"
    ;;
esac

# Claude Code skill-router/turn-guard hooks and main/subagent HUDs. Additive
# settings.json merge; Claude-only, non-fatal on failure.
if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = "1" ]; then
  "$DEST/scripts/install-claude-hooks.sh" ||
    echo "warning: claude settings registration failed (install continues)" >&2
else
  echo "skipping claude hooks/HUD: OH_MY_SETTING_CLAUDE_HOOKS=0"
fi

if [ "$CODEX_PLUGIN" = "1" ]; then
  "$DEST/scripts/install-codex-plugin.sh"
else
  echo "skipping codex plugin registration: OH_MY_SETTING_CODEX_PLUGIN=0"
fi

# Harness-state MCP server: claude and codex register it directly,
# antigravity receives it through the agy plugin. Non-fatal on failure; a
# missing CLI is a note, not an error.
"$DEST/scripts/install-mcp.sh" ||
  echo "warning: MCP server registration failed (install continues)" >&2
"$DEST/scripts/install-agy-plugin.sh" ||
  echo "warning: antigravity plugin import failed (install continues)" >&2

# Antigravity is the one provider whose headless calls are auto-denied without
# standing permissions; codex and claude carry authority per invocation. The
# grant widens what another program may do, so it stays behind an explicit
# flag — a default install only reports what a headless peer would be denied,
# instead of leaving that to be discovered after the first silent council.
if command -v agy >/dev/null 2>&1; then
  if [ "$PEER_PERMISSIONS" = "1" ]; then
    echo "granting Antigravity user-global consult permissions: read_file(*), command(*) (all-MCP access remains approval-gated)"
    "$DEST/scripts/provider-permissions.sh" --apply --profile consult ||
      echo "warning: peer permission grant failed (install continues)" >&2
  elif ! "$DEST/scripts/provider-permissions.sh" --check; then
    echo "note: peer permissions not granted; rerun with --peer-permissions to allow headless antigravity councils"
  fi
fi

if [ "$GENERATE_SLURM" = "1" ] || { [ "$GENERATE_SLURM" = "auto" ] && command -v sinfo >/dev/null 2>&1; }; then
  "$DEST/scripts/generate-slurm-reference.sh"
fi

if [ "$GENERATE_MACHINE" != "0" ]; then
  "$DEST/scripts/write-machine-snapshot.sh"
fi

"$DEST/scripts/doctor.sh"

if [ "$AUTO_UPDATE" = "1" ]; then
  "$DEST/scripts/install-autoupdate.sh"
elif [ "$AUTO_UPDATE" = "0" ]; then
  echo "skipping auto-update trigger: OH_MY_SETTING_AUTO_UPDATE=0"
else
  echo "error: OH_MY_SETTING_AUTO_UPDATE must be 0 or 1" >&2
  exit 2
fi

prompt_star_repo() {
  if [ "$STAR_PROMPT" = "0" ]; then
    return 0
  fi

  cat <<'EOF'

If oh-my-setting helped, please consider starring the repo:
  gh api --method PUT /user/starred/eightmm/oh-my-setting
EOF

  if ! command -v gh >/dev/null 2>&1; then
    return 0
  fi

  if ! gh auth status >/dev/null 2>&1; then
    echo "note: gh not authenticated; run 'gh auth login' to star with one command"
    return 0
  fi

  if [ ! -r /dev/tty ]; then
    return 0
  fi

  printf 'Star it now with gh? [y/N] ' >/dev/tty
  IFS= read -r answer </dev/tty || return 0

  case "$answer" in
    y|Y|yes|YES|Yes)
      if gh api --method PUT /user/starred/eightmm/oh-my-setting >/dev/null; then
        echo "ok: starred eightmm/oh-my-setting"
      else
        echo "warning: failed to star repo with gh api; you can run it manually later" >&2
      fi
      ;;
  esac
}

prompt_star_repo
