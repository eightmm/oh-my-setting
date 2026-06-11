#!/usr/bin/env bash
set -euo pipefail

ACTION=""
USER_NAME=""
REPO=""
QUERY=""
PATH_IN_REPO=""
TARGET=""
REF=""
LIMIT="20"
PROVENANCE=""
FORCE=0
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: github-source.sh [options] [profile|discover|fetch]

Use the authenticated GitHub CLI as a personal reusable-code source. This tool
never searches random code by default; scope discovery to a user or repo and
record provenance for fetched files.

Commands:
  profile                 Summarize a GitHub user's public profile and repos.
  discover                Search code under --user or --repo.
  fetch                   Fetch one file from --repo/--path to --target.

Options:
  --user USER             GitHub user. Defaults to gh api user login where used.
  --repo OWNER/REPO       Source repo for discover/fetch.
  --query TEXT            discover: code search text.
  --path PATH             fetch: file path inside repo.
  --target PATH           fetch: destination file.
  --ref REF               Branch, tag, or commit. Default: repo default branch.
  --limit N               profile/discover result limit. Default: 20.
  --provenance PATH       fetch record path. Default: .oms/code-sources.jsonl.
  --force                 Allow overwriting --target.
  --dry-run               Print planned action without writing.
  -h, --help              Show help.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

need_gh() {
  command -v gh >/dev/null 2>&1 || fail "gh is required; run: gh auth login"
}

urlencode() {
  python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe="/"))' "$1"
}

urlencode_query() {
  python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$1"
}

default_user() {
  gh api user | python3 -c 'import json,sys; print(json.load(sys.stdin).get("login", ""))'
}

json_text() {
  python3 -c 'import json,sys; print(json.load(sys.stdin).get(sys.argv[1]) or "")' "$1"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    profile|discover|fetch)
      ACTION="$1"
      shift
      ;;
    --user)
      [ "$#" -ge 2 ] || fail "--user requires value"
      USER_NAME="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires OWNER/REPO"
      REPO="$2"
      shift 2
      ;;
    --query)
      [ "$#" -ge 2 ] || fail "--query requires text"
      QUERY="$2"
      shift 2
      ;;
    --path)
      [ "$#" -ge 2 ] || fail "--path requires repo path"
      PATH_IN_REPO="$2"
      shift 2
      ;;
    --target)
      [ "$#" -ge 2 ] || fail "--target requires path"
      TARGET="$2"
      shift 2
      ;;
    --ref)
      [ "$#" -ge 2 ] || fail "--ref requires value"
      REF="$2"
      shift 2
      ;;
    --limit)
      [ "$#" -ge 2 ] || fail "--limit requires number"
      LIMIT="$2"
      shift 2
      ;;
    --provenance)
      [ "$#" -ge 2 ] || fail "--provenance requires path"
      PROVENANCE="$2"
      shift 2
      ;;
    --force)
      FORCE=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

ACTION="${ACTION:-profile}"
case "$LIMIT" in
  *[!0-9]*|"") fail "--limit must be a positive integer" ;;
esac
[ "$LIMIT" -gt 0 ] || fail "--limit must be a positive integer"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
need_gh

case "$ACTION" in
  profile)
    [ -n "$USER_NAME" ] || USER_NAME="$(default_user)"
    [ -n "$USER_NAME" ] || fail "could not determine GitHub user"
    profile_json="$(mktemp)"
    repos_json="$(mktemp)"
    trap 'rm -f "$profile_json" "$repos_json"' EXIT
    gh api "users/$USER_NAME" > "$profile_json"
    if ! gh repo list "$USER_NAME" --limit "$LIMIT" --json name,description,primaryLanguage,repositoryTopics,pushedAt,url > "$repos_json" 2>/dev/null; then
      printf '[]\n' > "$repos_json"
    fi
    python3 - "$profile_json" "$repos_json" <<'EOF'
import json, sys
profile = json.load(open(sys.argv[1]))
repos = json.load(open(sys.argv[2]))
print("# GitHub Source Profile")
for key in ("login", "name", "bio", "company", "location", "public_repos"):
    val = profile.get(key)
    if val not in (None, ""):
        print(f"- {key}: {val}")
langs, topics = {}, {}
print("\n## Recent Repos")
for r in repos:
    lang = (r.get("primaryLanguage") or {}).get("name") or ""
    if lang:
        langs[lang] = langs.get(lang, 0) + 1
    ts = r.get("repositoryTopics") or []
    names = []
    for t in ts:
        n = t.get("name") if isinstance(t, dict) else str(t)
        if n:
            names.append(n)
            topics[n] = topics.get(n, 0) + 1
    desc = (r.get("description") or "").replace("\n", " ")[:100]
    bits = [r.get("name", "")]
    if lang:
        bits.append(lang)
    if names:
        bits.append("topics=" + ",".join(names[:6]))
    if desc:
        bits.append(desc)
    print("- " + " | ".join(bits))
print("\n## Signals")
if langs:
    print("- languages: " + ", ".join(f"{k}({v})" for k, v in sorted(langs.items(), key=lambda x: (-x[1], x[0]))[:8]))
if topics:
    print("- topics: " + ", ".join(f"{k}({v})" for k, v in sorted(topics.items(), key=lambda x: (-x[1], x[0]))[:12]))
print("\n## Guidance")
print("- Prefer fetching from this user's own repos or an explicit registered source.")
print("- Pin reusable code to a commit/tag when it becomes project-critical.")
print("- Record provenance and adapt imports/tests after copying code.")
EOF
    ;;
  discover)
    [ -n "$QUERY" ] || fail "--query is required for discover"
    if [ -n "$REPO" ]; then
      scope="repo:$REPO"
    else
      [ -n "$USER_NAME" ] || USER_NAME="$(default_user)"
      [ -n "$USER_NAME" ] || fail "--user or --repo is required for discover"
      scope="user:$USER_NAME"
    fi
    q="$(urlencode_query "$QUERY $scope")"
    gh api "search/code?q=$q&per_page=$LIMIT" |
      python3 -c '
import json, sys
items = json.load(sys.stdin).get("items", [])
print("# GitHub Source Discover")
for it in items:
    repo = (it.get("repository") or {}).get("full_name", "")
    print("- %s %s %s" % (repo, it.get("path", ""), it.get("html_url", "")))
'
    ;;
  fetch)
    [ -n "$REPO" ] || fail "--repo is required for fetch"
    [ -n "$PATH_IN_REPO" ] || fail "--path is required for fetch"
    [ -n "$TARGET" ] || fail "--target is required for fetch"
    # Refuse symlinks even with --force: open(wb) would follow the link and
    # write outside the intended path. -L catches broken symlinks that -e misses.
    [ ! -L "$TARGET" ] || fail "target is a symlink; refusing to write through it: $TARGET"
    [ "$FORCE" -eq 1 ] || [ ! -e "$TARGET" ] || fail "target exists (use --force): $TARGET"
    PROVENANCE="${PROVENANCE:-.oms/code-sources.jsonl}"
    repo_json="$(mktemp)"
    content_json="$(mktemp)"
    commit_json="$(mktemp)"
    trap 'rm -f "$repo_json" "$content_json" "$commit_json"' EXIT
    if [ -z "$REF" ]; then
      gh api "repos/$REPO" > "$repo_json"
      REF="$(json_text default_branch < "$repo_json")"
      [ -n "$REF" ] || REF="main"
    fi
    encoded_path="$(urlencode "$PATH_IN_REPO")"
    encoded_ref="$(urlencode_query "$REF")"
    if [ "$DRY_RUN" -eq 1 ]; then
      printf 'github-source: dry-run\n'
      printf -- '- repo: %s\n' "$REPO"
      printf -- '- ref: %s\n' "$REF"
      printf -- '- path: %s\n' "$PATH_IN_REPO"
      printf -- '- target: %s\n' "$TARGET"
      printf -- '- provenance: %s\n' "$PROVENANCE"
      exit 0
    fi
    gh api "repos/$REPO/contents/$encoded_path?ref=$encoded_ref" > "$content_json"
    commit=""
    if gh api "repos/$REPO/commits/$encoded_ref" > "$commit_json" 2>/dev/null; then
      commit="$(json_text sha < "$commit_json")"
    fi
    mkdir -p "$(dirname "$TARGET")" "$(dirname "$PROVENANCE")"
    python3 - "$content_json" "$TARGET" <<'EOF'
import base64, json, sys
src, target = sys.argv[1], sys.argv[2]
data = json.load(open(src))
if isinstance(data, list):
    raise SystemExit("error: --path is a directory, not a file; give a file path")
if not isinstance(data, dict) or data.get("encoding") != "base64" or "content" not in data:
    raise SystemExit("error: GitHub content response is not a base64 file")
content = base64.b64decode(data["content"].encode())
with open(target, "wb") as f:
    f.write(content)
EOF
    blob_sha="$(json_text sha < "$content_json")"
    html_url="$(json_text html_url < "$content_json")"
    python3 - "$PROVENANCE" "$REPO" "$PATH_IN_REPO" "$REF" "$commit" "$blob_sha" "$TARGET" "$html_url" <<'EOF'
import json, sys, time
prov, repo, path, ref, commit, blob, target, url = sys.argv[1:]
row = {
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "repo": repo,
    "path": path,
    "ref": ref,
    "target": target,
    "tool": "github-source",
}
if commit:
    row["commit"] = commit
if blob:
    row["blob_sha"] = blob
if url:
    row["url"] = url
with open(prov, "a", encoding="utf-8") as f:
    f.write(json.dumps(row, ensure_ascii=False, allow_nan=False) + "\n")
EOF
    printf 'github-source: fetched %s/%s@%s -> %s\n' "$REPO" "$PATH_IN_REPO" "$REF" "$TARGET"
    printf 'github-source: provenance %s\n' "$PROVENANCE"
    ;;
  *)
    fail "unknown command: $ACTION"
    ;;
esac
