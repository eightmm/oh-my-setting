#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="$PWD"
REGISTRY=""
GLOBAL=0
ACTION="list"
NAME=""
SRC_REPO=""
SRC_PATH=""
SRC_REF=""
TARGET=""
TAGS=""
LICENSE=""
NOTES=""
FORCE=0
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: code-source.sh [options] [path|list|show|add|fetch] [NAME]

Maintain a small registry of trusted reusable code sources and fetch them via
GitHub with provenance. Project registry defaults to .oms/code-sources.json;
--global uses ~/.oh-my-setting/local/code-sources.json.

Commands:
  path                 Print registry path.
  list                 List registered sources.
  show NAME            Show one source as JSON.
  add NAME             Add/update one source.
  fetch NAME           Fetch a registered source.

Add options:
  --repo OWNER/REPO    Source repo.
  --path PATH          Source file path in repo.
  --ref REF            Branch, tag, or commit. Default: repo default branch.
  --target PATH        Default target path.
  --tags CSV           Tags, e.g. ml,gnn,equivariant.
  --license TEXT       License/provenance note, e.g. own-code.
  --notes TEXT         Short note.

Fetch options:
  --target PATH        Override target path.
  --ref REF            Override ref.
  --force              Overwrite target.
  --dry-run            Print planned fetch.

Common options:
  --repo-dir PATH      Project directory. Default: PWD.
  --file PATH          Explicit registry path.
  --global             Use global registry.
  -h, --help           Show help.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

resolve_registry() {
  if [ -n "$REGISTRY" ]; then
    printf '%s\n' "$REGISTRY"
  elif [ "$GLOBAL" -eq 1 ]; then
    printf '%s/.oh-my-setting/local/code-sources.json\n' "$HOME"
  else
    REPO_DIR="$(cd "$REPO_DIR" && pwd)" || exit 2
    printf '%s/.oms/code-sources.json\n' "$REPO_DIR"
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    path|list|show|add|fetch)
      ACTION="$1"
      shift
      ;;
    --repo-dir)
      [ "$#" -ge 2 ] || fail "--repo-dir requires path"
      REPO_DIR="$2"
      shift 2
      ;;
    --file)
      [ "$#" -ge 2 ] || fail "--file requires path"
      REGISTRY="$2"
      shift 2
      ;;
    --global)
      GLOBAL=1
      shift
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires OWNER/REPO"
      SRC_REPO="$2"
      shift 2
      ;;
    --path)
      [ "$#" -ge 2 ] || fail "--path requires source path"
      SRC_PATH="$2"
      shift 2
      ;;
    --ref)
      [ "$#" -ge 2 ] || fail "--ref requires value"
      SRC_REF="$2"
      shift 2
      ;;
    --target)
      [ "$#" -ge 2 ] || fail "--target requires path"
      TARGET="$2"
      shift 2
      ;;
    --tags)
      [ "$#" -ge 2 ] || fail "--tags requires CSV"
      TAGS="$2"
      shift 2
      ;;
    --license)
      [ "$#" -ge 2 ] || fail "--license requires text"
      LICENSE="$2"
      shift 2
      ;;
    --notes)
      [ "$#" -ge 2 ] || fail "--notes requires text"
      NOTES="$2"
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
      if [ -z "$NAME" ]; then
        NAME="$1"
        shift
      else
        fail "unknown argument: $1"
      fi
      ;;
  esac
done

REGISTRY="$(resolve_registry)"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

case "$ACTION" in
  path)
    printf '%s\n' "$REGISTRY"
    ;;
  list)
    [ -f "$REGISTRY" ] || { echo "code-source: empty ($REGISTRY)"; exit 0; }
    python3 - "$REGISTRY" <<'EOF'
import json, sys
data = json.load(open(sys.argv[1]))
sources = data.get("sources", {})
for name in sorted(sources):
    s = sources[name]
    bits = [name, s.get("repo", ""), s.get("path", "")]
    if s.get("target"):
        bits.append("-> " + s["target"])
    if s.get("tags"):
        bits.append("tags=" + ",".join(s["tags"]))
    print("  ".join(b for b in bits if b))
EOF
    ;;
  show)
    [ -n "$NAME" ] || fail "show requires NAME"
    [ -f "$REGISTRY" ] || fail "registry not found: $REGISTRY"
    python3 - "$REGISTRY" "$NAME" <<'EOF'
import json, sys
data = json.load(open(sys.argv[1]))
s = data.get("sources", {}).get(sys.argv[2])
if not s:
    raise SystemExit("error: source not found: %s" % sys.argv[2])
print(json.dumps(s, indent=2, ensure_ascii=False, sort_keys=True))
EOF
    ;;
  add)
    [ -n "$NAME" ] || fail "add requires NAME"
    [ -n "$SRC_REPO" ] || fail "--repo is required for add"
    [ -n "$SRC_PATH" ] || fail "--path is required for add"
    [ -n "$TARGET" ] || fail "--target is required for add"
    mkdir -p "$(dirname "$REGISTRY")"
    python3 - "$REGISTRY" "$NAME" "$SRC_REPO" "$SRC_PATH" "$SRC_REF" "$TARGET" "$TAGS" "$LICENSE" "$NOTES" <<'EOF'
import json, os, sys, time
reg, name, repo, path, ref, target, tags, license_text, notes = sys.argv[1:]
if os.path.exists(reg):
    with open(reg) as f:
        data = json.load(f)
else:
    data = {"version": 1, "sources": {}}
data.setdefault("version", 1)
data.setdefault("sources", {})
entry = {"repo": repo, "path": path, "target": target, "updated": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
if ref:
    entry["ref"] = ref
if tags:
    entry["tags"] = [t.strip() for t in tags.split(",") if t.strip()]
if license_text:
    entry["license"] = license_text
if notes:
    entry["notes"] = notes
data["sources"][name] = entry
with open(reg, "w", encoding="utf-8") as f:
    json.dump(data, f, ensure_ascii=False, indent=2, sort_keys=True)
    f.write("\n")
EOF
    echo "code-source: added $NAME -> $SRC_REPO/$SRC_PATH"
    ;;
  fetch)
    [ -n "$NAME" ] || fail "fetch requires NAME"
    [ -f "$REGISTRY" ] || fail "registry not found: $REGISTRY"
    if ! spec="$(python3 - "$REGISTRY" "$NAME" "$TARGET" "$SRC_REF" <<'EOF'
import json, sys
reg, name, target_override, ref_override = sys.argv[1:]
data = json.load(open(reg))
s = data.get("sources", {}).get(name)
if not s:
    raise SystemExit("error: source not found: %s" % name)
repo = s.get("repo", "")
path = s.get("path", "")
target = target_override or s.get("target", "")
ref = ref_override or s.get("ref", "")
if not repo or not path or not target:
    raise SystemExit("error: source requires repo, path, and target")
print("\t".join([repo, path, target, ref]))
EOF
)"; then
      exit 2
    fi
    [ -n "$spec" ] || fail "could not resolve source: $NAME"
    IFS=$'\t' read -r SRC_REPO SRC_PATH TARGET SRC_REF <<EOF
$spec
EOF
    cmd=("$ROOT/scripts/github-source.sh" fetch --repo "$SRC_REPO" --path "$SRC_PATH" --target "$TARGET" --provenance "$REPO_DIR/.oms/code-sources.jsonl")
    [ -n "$SRC_REF" ] && cmd+=(--ref "$SRC_REF")
    [ "$FORCE" -eq 1 ] && cmd+=(--force)
    [ "$DRY_RUN" -eq 1 ] && cmd+=(--dry-run)
    "${cmd[@]}"
    ;;
  *)
    fail "unknown command: $ACTION"
    ;;
esac
