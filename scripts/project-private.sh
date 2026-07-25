#!/usr/bin/env bash
set -euo pipefail

# Keeps a project's agent-facing harness files out of git without touching the
# repo's own .gitignore. The loader files (AGENTS.md, CLAUDE.md), PROJECT.md,
# and any extra --path entries are listed in a managed block inside
# .git/info/exclude, which is per-clone and never committed: a public repo shows
# no harness residue while every agent still reads the files locally. Paths git
# already tracks are reported, never silently untracked (--untrack is explicit).
# The listing is preemptive, so a file created later is hidden the moment it
# appears. Local-only means local-only: `git clean -x` deletes these files and
# a fresh clone will not have them — re-run apply-project-template there.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

BEGIN_MARK="# oh-my-setting:private:begin"
END_MARK="# oh-my-setting:private:end"

REPO="$PWD"
ACTION=""
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
AS_JSON=0
CHECK=0
UNTRACK=0
EXTRA_PATHS=""

# Agent-facing files the harness writes into a project. Listed whether or not
# they exist yet, so apply once covers a file the loader adds later.
DEFAULT_PATHS="AGENTS.md CLAUDE.md GEMINI.md PROJECT.md"

usage() {
  cat <<'EOF'
Usage: project-private.sh [status|apply|remove] [--repo PATH] [--path REL]...
                          [--check] [--untrack] [--dry-run] [--json]

Hide the agent-facing harness files from git locally, so a public repo stays
clean. Entries live in a managed block in .git/info/exclude (per-clone, never
committed) instead of the project's own .gitignore.

Commands:
status  Report each agent path as hidden / tracked / exposed (default).
apply   Ensure the managed block lists the agent paths.
remove  Delete the managed block; the files themselves are never touched.

Options:
  --repo PATH   Project to act on (default: PWD, git-root anchored).
  --path REL    Extra repo-relative path to hide; repeatable. Added to the
                defaults (AGENTS.md, CLAUDE.md, GEMINI.md, PROJECT.md).
  --check       Exit 1 when an agent file exists untracked but is not hidden.
  --untrack     For an already-tracked agent file, stage its removal from the
                index (git rm --cached) and hide it. The file stays on disk;
                commit the removal yourself.
  --dry-run     Report what would change; write nothing.
  --json        Emit a schema-1 JSON object (status only).
  -h, --help    Show help.

A tracked agent file is a deliberate choice and is left alone (--check does not
fail on it). `git clean -x` removes local-only files, and a fresh clone starts
without them: re-run `oms apply-project-template` there.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    status|apply|remove)
      [ -z "$ACTION" ] || fail "only one command allowed"
      ACTION="$1"; shift ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --path)
      [ "$#" -ge 2 ] || fail "--path requires a value"
      case "$2" in
        ""|/*|*..*) fail "--path must be a relative path inside the repo: $2" ;;
      esac
      case "$2" in
        *[[:space:]]*) fail "--path must not contain whitespace: $2" ;;
      esac
      EXTRA_PATHS="$EXTRA_PATHS $2"; shift 2 ;;
    --check) CHECK=1; shift ;;
    --untrack) UNTRACK=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --json) AS_JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

ACTION="${ACTION:-status}"
[ "$AS_JSON" -eq 0 ] || [ "$ACTION" = "status" ] || fail "--json is only supported by status"
[ "$UNTRACK" -eq 0 ] || [ "$ACTION" = "apply" ] || fail "--untrack is only supported by apply"
[ -d "$REPO" ] || fail "not a directory: $REPO"

STATE_ROOT="$(oms_repo_root "$REPO")" || fail "bad --repo"

if ! git -C "$STATE_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  # Bootstrap runs the template before `git init`; that is not an error, there
  # is simply nothing to exclude yet.
  if [ "$AS_JSON" -eq 1 ]; then
    printf '{"schema": 1, "repo": "%s", "git": false, "entries": []}\n' "$STATE_ROOT"
  else
    echo "project-private: $STATE_ROOT is not a git repo yet; re-run after git init"
  fi
  exit 0
fi

# Linked worktrees share info/exclude with the main checkout: resolve the common
# dir, not the per-worktree .git dir. --git-common-dir may print a relative path
# and is absent before git 2.5.
git_common_dir() {
  local dir
  dir="$(git -C "$STATE_ROOT" rev-parse --git-common-dir 2>/dev/null || true)"
  [ -n "$dir" ] || dir="$(git -C "$STATE_ROOT" rev-parse --git-dir)"
  case "$dir" in
    /*) ;;
    *) dir="$STATE_ROOT/$dir" ;;
  esac
  (cd "$dir" && pwd)
}

GIT_DIR_PATH="$(git_common_dir)" || fail "cannot resolve git dir for $STATE_ROOT"
EXCLUDE="$GIT_DIR_PATH/info/exclude"

CANDIDATES="$DEFAULT_PATHS$EXTRA_PATHS"

is_tracked() {
  [ -n "$(git -C "$STATE_ROOT" ls-files -- "$1" 2>/dev/null)" ]
}

# Entries inside the managed block, one per line. Fails when the block opens
# and never closes: a half-written block must be repaired, not silently
# rewritten around.
block_entries() {
  [ -f "$EXCLUDE" ] || return 0
  awk -v begin="$BEGIN_MARK" -v end="$END_MARK" '
    $0 == begin { if (inb) { print "error: nested " begin > "/dev/stderr"; exit 2 } inb = 1; seen = 1; next }
    $0 == end { if (!inb) { print "error: unmatched " end > "/dev/stderr"; exit 2 } inb = 0; next }
    inb && NF && $0 !~ /^#/ { print }
    END { if (inb) { print "error: unterminated " begin > "/dev/stderr"; exit 2 } }
  ' "$EXCLUDE"
}

has_block() {
  [ -f "$EXCLUDE" ] && grep -Fqx "$BEGIN_MARK" "$EXCLUDE"
}

# Membership test over a newline-separated list, so an entry is never listed
# twice even when --path repeats a default.
in_list() {
  case "
$1
" in
    *"
$2
"*) return 0 ;;
  esac
  return 1
}

# hidden: excluded locally. tracked: committed, exclude is a no-op by design.
# exposed: on disk, untracked, and not excluded — it would show up in git
# status and land in a commit. reserved: not on disk but already excluded.
path_state() {
  local p="$1"
  if is_tracked "$p"; then
    printf 'tracked\n'
  elif in_list "$LISTED_ENTRIES" "$p"; then
    if [ -e "$STATE_ROOT/$p" ]; then printf 'hidden\n'; else printf 'reserved\n'; fi
  elif [ -e "$STATE_ROOT/$p" ]; then
    printf 'exposed\n'
  else
    printf 'absent\n'
  fi
}

LISTED_ENTRIES="$(block_entries)" || exit 1

write_exclude() {
  local entries="$1"
  local tmp
  local dir

  dir="$(dirname "$EXCLUDE")"
  mkdir -p "$dir"
  tmp="$(mktemp "$dir/.oms-exclude.XXXXXX")" || return 1
  if [ -f "$EXCLUDE" ]; then
    # Drop the old block, then trailing blank lines, so repeated
    # apply/remove cycles cannot grow the file with empty lines.
    awk -v begin="$BEGIN_MARK" -v end="$END_MARK" '
      $0 == begin { inb = 1; next }
      $0 == end { inb = 0; next }
      !inb { print }
    ' "$EXCLUDE" |
      awk '
        /^[[:space:]]*$/ { pending = pending "\n"; next }
        { printf "%s%s\n", pending, $0; pending = "" }
      ' > "$tmp" || { rm -f "$tmp"; return 1; }
  fi
  if [ -n "$entries" ]; then
    [ -s "$tmp" ] && printf '\n' >> "$tmp"
    {
      printf '%s\n' "$BEGIN_MARK"
      printf '# Local-only agent harness files (oms project-private). Not committed.\n'
      printf '%s\n' "$entries"
      printf '%s\n' "$END_MARK"
    } >> "$tmp" || { rm -f "$tmp"; return 1; }
  fi
  chmod 644 "$tmp"
  mv "$tmp" "$EXCLUDE"
}

cmd_status() {
  local p state exposed=0 rows="" json_rows="" block="absent"

  for p in $CANDIDATES; do
    state="$(path_state "$p")"
    [ "$state" = "exposed" ] && exposed=$((exposed + 1))
    rows="$rows$(printf '%-9s %s' "$state" "$p")
"
    json_rows="$json_rows{\"path\": \"$p\", \"state\": \"$state\"},"
  done
  has_block && block="present"

  if [ "$AS_JSON" -eq 1 ]; then
    printf '{"schema": 1, "repo": "%s", "git": true, "exclude_file": "%s", "block": "%s", "exposed": %d, "entries": [%s]}\n' \
      "$STATE_ROOT" "$EXCLUDE" "$block" "$exposed" "${json_rows%,}"
  else
    printf '%s' "$rows"
    if [ "$exposed" -gt 0 ]; then
      echo "project-private: $exposed agent file(s) visible to git; run: oms project-private apply"
    else
      echo "project-private: no agent files exposed"
    fi
  fi

  if [ "$CHECK" -eq 1 ] && [ "$exposed" -gt 0 ]; then
    return 1
  fi
  return 0
}

cmd_apply() {
  local p state entries="$LISTED_ENTRIES" added=0 tracked="" untracked_now=0

  for p in $CANDIDATES; do
    state="$(path_state "$p")"
    if [ "$state" = "tracked" ]; then
      if [ "$UNTRACK" -eq 1 ]; then
        if [ "$DRY_RUN" = "1" ]; then
          echo "would untrack (git rm --cached) $p"
        else
          git -C "$STATE_ROOT" rm --cached --quiet -- "$p" ||
            fail "git rm --cached failed for $p"
          echo "untracked $p (staged removal; commit it, the file stays on disk)"
        fi
        untracked_now=$((untracked_now + 1))
      else
        tracked="$tracked $p"
        continue
      fi
    fi
    in_list "$entries" "$p" && continue
    entries="${entries:+$entries
}$p"
    added=$((added + 1))
  done

  if [ -n "$tracked" ]; then
    echo "tracked (left alone;$tracked): committed deliberately? otherwise re-run with --untrack"
  fi

  if [ "$added" -eq 0 ] && [ "$untracked_now" -eq 0 ]; then
    echo "project-private: already hidden ($EXCLUDE)"
    return 0
  fi
  if [ "$DRY_RUN" = "1" ]; then
    echo "would add $added entry(ies) to $EXCLUDE"
    return 0
  fi
  write_exclude "$entries" || fail "cannot write $EXCLUDE"
  echo "project-private: hid $added entry(ies) in $EXCLUDE"
}

cmd_remove() {
  if ! has_block; then
    echo "project-private: no managed block in $EXCLUDE"
    return 0
  fi
  if [ "$DRY_RUN" = "1" ]; then
    echo "would remove the managed block from $EXCLUDE"
    return 0
  fi
  write_exclude "" || fail "cannot write $EXCLUDE"
  echo "project-private: removed the managed block from $EXCLUDE (files untouched)"
}

case "$ACTION" in
  status) cmd_status ;;
  apply) cmd_apply ;;
  remove) cmd_remove ;;
esac
