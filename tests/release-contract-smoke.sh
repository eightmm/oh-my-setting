#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

version="$(sed -n '1p' "$ROOT/VERSION")"
[ "$version" = "0.4.0" ] || fail "VERSION must be 0.4.0, got $version"

plugin_version="$(python3 - "$ROOT/plugins/oh-my-setting/.codex-plugin/plugin.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    print(json.load(handle)["version"])
PY
)"
[ "$plugin_version" = "$version" ] ||
  fail "plugin version $plugin_version does not match VERSION $version"

[ -f "$ROOT/docs/MIGRATION-0.4.md" ] || fail "docs/MIGRATION-0.4.md is missing"

help="$(bash "$ROOT/install.sh" --help)"
printf '%s\n' "$help" | grep -Fq -- '--ref REF' || fail "install help omits --ref REF"
printf '%s\n' "$help" | grep -Fq 'OH_MY_SETTING_REF' || fail "install help omits OH_MY_SETTING_REF"
grep -Fxq 'INSTALLER_DEFAULT_REF="edge"' "$ROOT/install.sh" ||
  fail "source installer must declare the edge placeholder exactly once"
[ "$(grep -Fxc 'INSTALLER_DEFAULT_REF="edge"' "$ROOT/install.sh")" = "1" ] ||
  fail "source installer ref placeholder must be unique"

# Exercise the same deterministic rewrite used by the release workflow. The
# published installer must carry the release tag and remain syntactically valid.
sed 's/^INSTALLER_DEFAULT_REF="edge"$/INSTALLER_DEFAULT_REF="v0.4.0"/' \
  "$ROOT/install.sh" > "$TMP/install.sh"
grep -Fxq 'INSTALLER_DEFAULT_REF="v0.4.0"' "$TMP/install.sh" ||
  fail "release installer was not pinned to the tag"
bash -n "$TMP/install.sh"

# A release-generated installer must preserve its embedded pin through the
# checkout re-exec. The same managed checkout can then opt back into edge.
upstream="$TMP/upstream"
mkdir -p "$upstream/scripts/lib"
cp "$ROOT/install.sh" "$upstream/install.sh"
cp "$ROOT/scripts/update.sh" "$upstream/scripts/update.sh"
cp "$ROOT/scripts/lib/install-contract.sh" "$upstream/scripts/lib/install-contract.sh"
for script in link.sh install-tools.sh install-autoupdate.sh \
  uninstall-autoupdate.sh install-claude-hooks.sh write-machine-snapshot.sh \
  generate-slurm-skill.sh; do
  cat > "$upstream/scripts/$script" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
  chmod +x "$upstream/scripts/$script"
done
cat > "$upstream/scripts/doctor.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n%s\n' "$OH_MY_SETTING_REF" "$OH_MY_SETTING_PROFILE" > "$OMS_CAPTURE"
EOF
chmod +x "$upstream/scripts/doctor.sh"

git -C "$upstream" init -q
git -C "$upstream" checkout -qb main
git -C "$upstream" config user.name test
git -C "$upstream" config user.email test@example.com
printf 'stable\n' > "$upstream/channel"
git -C "$upstream" add .
git -C "$upstream" commit -qm stable
git -C "$upstream" tag v0.4.0
printf 'edge\n' > "$upstream/channel"
git -C "$upstream" commit -qam edge
git -C "$upstream" branch release-line
git -C "$upstream" branch v0.4.0

home="$TMP/home"
dest="$home/.oh-my-setting"
capture="$TMP/install.env"
mkdir -p "$home"
bad_dest="$home/bad-ref"
if HOME="$home" OH_MY_SETTING_REPO_URL="$upstream" OH_MY_SETTING_DIR="$bad_dest" \
  bash "$ROOT/install.sh" --ref ../escape >/dev/null 2>&1; then
  fail "installer accepted an unsafe ref"
fi
[ ! -e "$bad_dest" ] || fail "unsafe ref mutated the install destination"

HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_CAPTURE="$capture" \
  OH_MY_SETTING_REPO_URL="$upstream" OH_MY_SETTING_DIR="$dest" \
  OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
  bash "$TMP/install.sh" >/dev/null
[ "$(cat "$dest/channel")" = stable ] || fail "release installer advanced past its tag"
if git -C "$dest" symbolic-ref -q HEAD >/dev/null; then
  fail "pinned install must use a detached checkout"
fi
[ "$(sed -n '1p' "$capture")" = v0.4.0 ] || fail "pinned ref was lost during re-exec"
[ "$(sed -n '2p' "$capture")" = minimal ] || fail "default profile must be minimal"

receipt="$home/.config/oh-my-setting/install.json"
python3 - "$receipt" "$dest" "$(git -C "$dest" rev-parse HEAD)" <<'PY'
import json, os, sys
os.makedirs(os.path.dirname(sys.argv[1]), exist_ok=True)
json.dump({
    "schema": 2, "source_root": sys.argv[2], "commit": sys.argv[3],
    "channel": "detached", "dirty": False, "version": "0.4.0",
    "profile": "minimal", "ref": "v0.4.0", "previous_commit": "",
    "installed_at": "2026-07-12T00:00:00Z",
    "components": {"tools": False, "claude_hooks": False, "codex_plugin": False,
                   "auto_update": False, "machine_snapshot": False, "slurm_snapshot": False},
    "managed_targets": [],
    "plugin": {"name": "oh-my-setting", "version": "0.4.0", "sha256": "x" * 64},
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
  OMS_CAPTURE="$capture" PATH="/usr/bin:/bin" "$dest/scripts/update.sh" \
  --no-tools --no-doctor >/dev/null
[ "$(cat "$dest/channel")" = stable ] || fail "pinned update preferred a same-named branch over its tag"

# A fetched remote branch must beat a stale same-named local branch.
git -C "$dest" branch -f release-line refs/tags/v0.4.0
HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_CAPTURE="$capture" \
  OH_MY_SETTING_REPO_URL="$upstream" OH_MY_SETTING_DIR="$dest" \
  OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
  bash "$ROOT/install.sh" --ref release-line >/dev/null
[ "$(cat "$dest/channel")" = edge ] || fail "installer preferred a stale local branch over origin"

HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_CAPTURE="$capture" \
  OH_MY_SETTING_REF=v0.4.0 OH_MY_SETTING_REPO_URL="$upstream" \
  OH_MY_SETTING_DIR="$dest" OH_MY_SETTING_CLAUDE_HOOKS=0 \
  OH_MY_SETTING_CODEX_PLUGIN=0 bash "$ROOT/install.sh" --ref edge --tools >/dev/null
[ "$(cat "$dest/channel")" = edge ] || fail "--ref edge did not restore the default branch"
[ "$(sed -n '1p' "$capture")" = edge ] || fail "--ref did not override OH_MY_SETTING_REF"
[ "$(sed -n '2p' "$capture")" = custom ] || fail "component flags must select custom profile"

HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_CAPTURE="$capture" \
  OH_MY_SETTING_REPO_URL="$upstream" OH_MY_SETTING_DIR="$dest" \
  OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
  bash "$ROOT/install.sh" --ref edge --full >/dev/null
[ "$(sed -n '2p' "$capture")" = full ] || fail "--full must select full profile"

# Edge follows a changed remote default branch instead of cached origin/HEAD.
git -C "$upstream" checkout -qb trunk
printf 'trunk\n' > "$upstream/channel"
git -C "$upstream" commit -qam trunk
python3 - "$receipt" "$(git -C "$dest" rev-parse HEAD)" <<'PY'
import json, sys
path, commit = sys.argv[1:]
d = json.load(open(path, encoding="utf-8"))
d["commit"] = commit
d["ref"] = "edge"
json.dump(d, open(path, "w", encoding="utf-8"))
PY
HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
  OMS_CAPTURE="$capture" PATH="/usr/bin:/bin" "$dest/scripts/update.sh" \
  --no-tools --no-doctor >/dev/null
[ "$(cat "$dest/channel")" = trunk ] || fail "update edge ignored a changed remote default branch"
[ "$(git -C "$dest" symbolic-ref --short HEAD)" = trunk ] ||
  fail "update edge did not switch to the new remote default branch"

git -C "$upstream" checkout -q main
HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_CAPTURE="$capture" \
  OH_MY_SETTING_REPO_URL="$upstream" OH_MY_SETTING_DIR="$dest" \
  OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
  bash "$ROOT/install.sh" --ref edge >/dev/null
[ "$(cat "$dest/channel")" = edge ] || fail "installer edge ignored a changed remote default branch"
[ "$(git -C "$dest" symbolic-ref --short HEAD)" = main ] ||
  fail "installer edge did not switch to the changed remote default branch"

manifest="$TMP/SHA256SUMS"
"$ROOT/scripts/gen-checksums.sh" > "$manifest"
"$ROOT/scripts/gen-checksums.sh" --verify "$manifest" >/dev/null

bad_manifest="$TMP/SHA256SUMS.bad"
first_char="$(sed -n '1s/^\(.\).*/\1/p' "$manifest")"
replacement=0
[ "$first_char" = 0 ] && replacement=1
sed "1s/^./$replacement/" "$manifest" > "$bad_manifest"
if "$ROOT/scripts/gen-checksums.sh" --verify "$bad_manifest" >/dev/null 2>&1; then
  fail "checksum verification accepted a corrupted manifest"
fi

checksum_clone="$TMP/checksum-clone"
git clone -q "$ROOT" "$checksum_clone"
cp "$ROOT/scripts/gen-checksums.sh" "$checksum_clone/scripts/gen-checksums.sh"
"$checksum_clone/scripts/gen-checksums.sh" --strict >/dev/null
rm -f "$checksum_clone/install.sh"
if "$checksum_clone/scripts/gen-checksums.sh" --strict >/dev/null 2>&1; then
  fail "strict checksum generation accepted a missing tracked release file"
fi

workflow="$ROOT/.github/workflows/release.yml"
grep -Fq 'INSTALLER_DEFAULT_REF=\"$TAG\"' "$workflow" ||
  fail "release workflow does not assert the pinned installer tag"
grep -Fq 'scripts/gen-checksums.sh --strict' "$workflow" ||
  fail "release workflow does not require strict manifest generation"
grep -Fq 'scripts/gen-checksums.sh --verify' "$workflow" ||
  fail "release workflow does not verify the generated manifest"
grep -Fq '[ -s RELEASE_NOTES.md ]' "$workflow" ||
  fail "release workflow does not require changelog-backed notes"
grep -Fq 'CHANGELOG.md needs a dated section' "$workflow" ||
  fail "release workflow does not block an undated release candidate"
grep -Fq 're.escape(version)' "$workflow" ||
  fail "release workflow does not match changelog versions literally"
grep -Fq 'prefix = "## [" v "] - "' "$workflow" ||
  fail "release note extraction does not use a literal version heading"
python3 - <<'PY' || fail "release heading validator accepted a near-match"
import re
version = "0.4.0"
wanted = re.compile(r"^## \[" + re.escape(version) + r"\] - \d{4}-\d{2}-\d{2}$")
assert not wanted.fullmatch("## [0x4y0] - 2026-07-12")
assert wanted.fullmatch("## [0.4.0] - 2026-07-12")
PY

echo "release-contract-smoke: ok"
