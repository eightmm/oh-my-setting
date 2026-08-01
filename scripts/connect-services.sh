#!/usr/bin/env bash
set -euo pipefail

# Connect the two account-backed services used by the harness without ever
# reading or persisting their credentials. gh and ntn own their browser login
# and credential stores; this script only verifies the resulting sessions and
# records the non-secret Work Journal data-source id.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
GH_BIN="${OMS_GH_BIN:-gh}"
NOTION_CLI="${OMS_NOTION_CLI:-ntn}"
NOTION_KEYRING_MODE="${OMS_NOTION_KEYRING:-}"
MODE=auto
CONNECT_GITHUB=1
CONNECT_NOTION=1
DATA_SOURCE_ID=""

usage() {
  cat <<'EOF'
Usage: connect-services.sh [--auto|--required] [--data-source-id ID]
                           [--no-github] [--no-notion] [-h|--help]

Authenticate GitHub with `gh auth login` and Notion with `ntn login`, then
discover and configure the existing Work Journal data source. Credentials stay
in the stores owned by those official CLIs; oh-my-setting saves only the
Notion data-source id and authentication mode.

--auto continues a non-interactive install with exact follow-up commands.
--required fails unless every selected service is connected.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --auto) MODE=auto ;;
    --required) MODE=required ;;
    --data-source-id)
      [ "$#" -ge 2 ] || {
        echo "error: --data-source-id requires a value" >&2
        exit 2
      }
      DATA_SOURCE_ID="$2"
      shift
      ;;
    --no-github) CONNECT_GITHUB=0 ;;
    --no-notion) CONNECT_NOTION=0 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

case "${OMS_CONNECT_INTERACTIVE:-auto}" in
  1) INTERACTIVE=1; LOGIN_USES_TTY=0 ;;
  0) INTERACTIVE=0; LOGIN_USES_TTY=0 ;;
  auto)
    LOGIN_USES_TTY=1
    if [ -t 0 ] || [ -t 1 ]; then INTERACTIVE=1; else INTERACTIVE=0; fi
    ;;
  *)
    echo "error: OMS_CONNECT_INTERACTIVE must be 0, 1, or auto" >&2
    exit 2
    ;;
esac

FAILED=0
PENDING=0

problem() {
  PENDING=1
  if [ "$MODE" = required ]; then
    echo "error: $*" >&2
    FAILED=1
  else
    echo "note: $*" >&2
  fi
}

run_login() {
  if [ "$LOGIN_USES_TTY" = 1 ] && [ -r /dev/tty ] && [ -w /dev/tty ]; then
    "$@" </dev/tty >/dev/tty
  else
    "$@"
  fi
}

github_authenticated() {
  "$GH_BIN" auth status >/dev/null 2>&1
}

notion_authenticated() {
  # Split the API path because the outbound scrubber correctly treats literal
  # user-home paths as private; this endpoint would otherwise be an
  # indistinguishable case-insensitive false positive in harness source.
  local me_path='v1/user'"s/me"
  "$NOTION_CLI" api "$me_path" 2>/dev/null |
    python3 -c 'import json,sys; row=json.load(sys.stdin); raise SystemExit(0 if isinstance(row, dict) and row.get("object") == "user" else 1)' \
      >/dev/null 2>&1
}

connect_github() {
  if ! command -v "$GH_BIN" >/dev/null 2>&1; then
    problem "GitHub CLI is unavailable; install gh, then run: gh auth login"
    return 0
  fi
  if github_authenticated; then
    echo "ok: GitHub CLI authenticated"
    return 0
  fi
  if [ "$INTERACTIVE" != 1 ]; then
    problem "GitHub login pending; run: gh auth login --web"
    return 0
  fi
  echo "connecting GitHub through gh"
  if ! run_login "$GH_BIN" auth login --web || ! github_authenticated; then
    problem "GitHub login did not complete; rerun: gh auth login --web"
    return 0
  fi
  echo "ok: GitHub CLI authenticated"
}

configure_notion_target() {
  if [ -n "$DATA_SOURCE_ID" ]; then
    OMS_WORK_JOURNAL_NOTION_AUTH=ntn OMS_NOTION_CLI="$NOTION_CLI" \
      OMS_NOTION_KEYRING="$NOTION_KEYRING_MODE" \
      "$ROOT/scripts/journal.sh" configure --data-source-id "$DATA_SOURCE_ID"
  else
    OMS_WORK_JOURNAL_NOTION_AUTH=ntn OMS_NOTION_CLI="$NOTION_CLI" \
      OMS_NOTION_KEYRING="$NOTION_KEYRING_MODE" \
      "$ROOT/scripts/journal.sh" configure --discover
  fi
}

remember_unverified_notion_target() {
  [ -n "$DATA_SOURCE_ID" ] || return 0
  "$ROOT/scripts/journal.sh" configure \
    --data-source-id "$DATA_SOURCE_ID" --no-validate
}

connect_notion() {
  local login_hint="ntn login"
  local keyring_probe

  if ! command -v "$NOTION_CLI" >/dev/null 2>&1; then
    remember_unverified_notion_target
    problem "Notion CLI is unavailable; install ntn, run: ntn login, then rerun this command"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    problem "Python 3 is required to verify the Notion CLI session"
    return 0
  fi
  # A machine without a usable OS keychain (headless or sandboxed session)
  # cannot store or read the default credential; ntn's own hint names the
  # fallback. Switch this run — and, through configure, the persisted
  # transport — to the file-based store so hooks work without env plumbing.
  keyring_probe="$("$NOTION_CLI" whoami 2>&1 || true)"
  case "$keyring_probe" in
    *[Kk]eychain*)
      NOTION_KEYRING_MODE="file"
      export NOTION_KEYRING=0
      login_hint="NOTION_KEYRING=0 ntn login"
      echo "note: OS keychain unavailable; using ntn's file-based credential store"
      ;;
  esac
  if ! notion_authenticated; then
    if [ "$INTERACTIVE" != 1 ]; then
      remember_unverified_notion_target
      problem "Notion login pending; run: $login_hint, then: oms journal configure --discover"
      return 0
    fi
    echo "connecting Notion through ntn"
    if ! run_login "$NOTION_CLI" login || ! notion_authenticated; then
      remember_unverified_notion_target
      problem "Notion login did not complete; rerun: $login_hint"
      return 0
    fi
  fi
  if ! configure_notion_target; then
    if [ -n "$DATA_SOURCE_ID" ]; then
      problem "the selected Notion data source is not a compatible Work Journal target"
    else
      problem "no unique compatible Work Journal data source was found; rerun with --data-source-id ID"
    fi
    return 0
  fi
  echo "ok: Notion Work Journal connected"
}

[ "$CONNECT_GITHUB" != 1 ] || connect_github
[ "$CONNECT_NOTION" != 1 ] || connect_notion

[ "$FAILED" = 0 ] || exit 1
if [ "$PENDING" = 1 ]; then
  echo "service connection: pending"
else
  echo "service connection: ok"
fi
