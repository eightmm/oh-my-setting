#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-tool-lock.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
fail() { echo "tool-lock-smoke: $*" >&2; exit 1; }

LOCK="$ROOT/tools.lock.json"
HELPER="$ROOT/scripts/lib/tool-lock.py"
[ -f "$LOCK" ] || fail "missing tools.lock.json"
[ -x "$HELPER" ] || fail "missing executable tool-lock helper"
python3 "$HELPER" --lock "$LOCK" validate >/dev/null || fail "tool lock is invalid"

# Integrity verification binds the package bytes that npm will install, not
# only registry metadata fetched in an earlier request.
printf 'locked package bytes\n' > "$TMP/package.tgz"
package_integrity="$(python3 - "$TMP/package.tgz" <<'PY'
import base64, hashlib, sys
print("sha512-" + base64.b64encode(hashlib.sha512(open(sys.argv[1], "rb").read()).digest()).decode("ascii"))
PY
)"
python3 "$HELPER" --lock "$LOCK" verify-sri \
  --integrity "$package_integrity" --file "$TMP/package.tgz" >/dev/null ||
  fail "matching npm package integrity was rejected"
printf 'changed\n' >> "$TMP/package.tgz"
if python3 "$HELPER" --lock "$LOCK" verify-sri \
    --integrity "$package_integrity" --file "$TMP/package.tgz" >/dev/null 2>&1; then
  fail "changed npm package bytes passed locked integrity"
fi

# A digest-authenticated archive is still data: extraction may not write
# outside its dedicated staging directory through member names.
python3 - "$TMP/traversal.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    data = b"outside\n"
    item = tarfile.TarInfo("../outside")
    item.size = len(data)
    archive.addfile(item, io.BytesIO(data))
PY
mkdir -p "$TMP/extracted"
if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
    --source "$TMP/traversal.tar.gz" --dest "$TMP/extracted" >/dev/null 2>&1; then
  fail "archive traversal member was accepted"
fi
[ ! -e "$TMP/outside" ] || fail "archive extraction escaped its staging directory"

python3 - "$TMP/safe.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    for name in ("node", "node/bin", "node/lib", "node/lib/npm"):
        item = tarfile.TarInfo(name)
        item.type = tarfile.DIRTYPE
        item.mode = 0o755
        archive.addfile(item)
    data = b"npm\n"
    item = tarfile.TarInfo("node/lib/npm/cli.js")
    item.size = len(data)
    item.mode = 0o644
    archive.addfile(item, io.BytesIO(data))
    link = tarfile.TarInfo("node/bin/npm")
    link.type = tarfile.SYMTYPE
    link.linkname = "../lib/npm/cli.js"
    archive.addfile(link)
PY
mkdir -p "$TMP/safe-extracted"
python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
  --source "$TMP/safe.tar.gz" --dest "$TMP/safe-extracted" >/dev/null ||
  fail "contained archive symlink was rejected"
[ "$(cat "$TMP/safe-extracted/node/bin/npm")" = npm ] ||
  fail "safe archive did not extract correctly"

# Windows treats C:payload as a drive-relative path even though it is not
# absolute. Both member names and link targets must reject that spelling
# before extraction on every host, so Linux CI protects native Git Bash too.
python3 - "$TMP/drive-relative.tar.gz" "$TMP/drive-link.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    data = b"drive\n"
    item = tarfile.TarInfo("C:payload")
    item.size = len(data)
    archive.addfile(item, io.BytesIO(data))
with tarfile.open(sys.argv[2], "w:gz") as archive:
    item = tarfile.TarInfo("safe/link")
    item.type = tarfile.SYMTYPE
    item.linkname = "C:payload"
    archive.addfile(item)
PY
for unsafe in drive-relative drive-link; do
  mkdir -p "$TMP/$unsafe-extracted"
  if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
      --source "$TMP/$unsafe.tar.gz" --dest "$TMP/$unsafe-extracted" \
      >/dev/null 2>&1; then
    fail "Windows drive-relative $unsafe archive path was accepted"
  fi
done

# A sequence of individually contained links must not redirect a later file
# write outside staging. Regular files are extracted without following archive
# links, then the complete link graph is checked before use.
python3 - "$TMP/link-chain.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    for name in ("p", "x"):
        item = tarfile.TarInfo(name)
        item.type = tarfile.DIRTYPE
        archive.addfile(item)
    first = tarfile.TarInfo("p/a")
    first.type = tarfile.SYMTYPE
    first.linkname = "../x"
    archive.addfile(first)
    second = tarfile.TarInfo("p/a/b")
    second.type = tarfile.SYMTYPE
    second.linkname = "../../outside"
    archive.addfile(second)
    data = b"escaped\n"
    payload = tarfile.TarInfo("p/a/b/payload")
    payload.size = len(data)
    archive.addfile(payload, io.BytesIO(data))
PY
mkdir -p "$TMP/chain-root/stage"
if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
    --source "$TMP/link-chain.tar.gz" --dest "$TMP/chain-root/stage" \
    >/dev/null 2>&1; then
  fail "archive symlink-chain pivot was accepted"
fi
[ ! -e "$TMP/chain-root/outside/payload" ] ||
  fail "archive symlink chain wrote outside staging before rejection"

python3 - "$TMP/ads.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    data = b"ads\n"
    item = tarfile.TarInfo("safe/payload:stream")
    item.size = len(data)
    archive.addfile(item, io.BytesIO(data))
PY
mkdir -p "$TMP/ads-extracted"
if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
    --source "$TMP/ads.tar.gz" --dest "$TMP/ads-extracted" >/dev/null 2>&1; then
  fail "Windows alternate-data-stream archive path was accepted"
fi

# Device, FIFO, and unknown tar entry types are never required by the locked
# tool payloads. Reject them before extraction instead of relying on each
# tarfile implementation's handling of special entries.
python3 - "$TMP/special.tar.gz" <<'PY'
import tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    item = tarfile.TarInfo("device")
    item.type = tarfile.CHRTYPE
    archive.addfile(item)
PY
mkdir -p "$TMP/special-extracted"
if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
    --source "$TMP/special.tar.gz" --dest "$TMP/special-extracted" \
    >/dev/null 2>&1; then
  fail "archive special file was accepted"
fi

python3 - "$TMP/duplicate.zip" <<'PY'
import warnings, zipfile, sys
with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    with zipfile.ZipFile(sys.argv[1], "w") as archive:
        archive.writestr("payload/file", b"first")
        archive.writestr("payload/file", b"second")
PY
mkdir -p "$TMP/duplicate-zip-extracted"
if python3 "$HELPER" --lock "$LOCK" extract --archive zip \
    --source "$TMP/duplicate.zip" --dest "$TMP/duplicate-zip-extracted" \
    >/dev/null 2>&1; then
  fail "zip archive with a duplicate member path was accepted"
fi

# Native npm packages are installed only after their embedded identity is
# checked, and the helper leaves a complete directory rather than a tarball or
# an interrupted backup.
python3 - "$TMP/native.tgz" <<'PY'
import io, json, tarfile, sys
manifest = json.dumps({"name": "@fixture/native", "version": "1.2.3"}).encode()
with tarfile.open(sys.argv[1], "w:gz") as archive:
    for name in ("package", "package/bin"):
        item = tarfile.TarInfo(name)
        item.type = tarfile.DIRTYPE
        item.mode = 0o755
        archive.addfile(item)
    item = tarfile.TarInfo("package/package.json")
    item.size = len(manifest)
    archive.addfile(item, io.BytesIO(manifest))
    binary = b"native\n"
    item = tarfile.TarInfo("package/bin/native")
    item.size = len(binary)
    item.mode = 0o755
    archive.addfile(item, io.BytesIO(binary))
PY
mkdir -p "$TMP/native-root"
python3 "$HELPER" --lock "$LOCK" install-npm-payload \
  --file "$TMP/native.tgz" --dest "$TMP/native-root/@fixture/native" \
  --root "$TMP/native-root" \
  --name '@fixture/native' --version 1.2.3 >/dev/null ||
  fail "verified native npm payload was not installed"
python3 "$HELPER" --lock "$LOCK" verify-installed-npm \
  --path "$TMP/native-root/@fixture/native" --name '@fixture/native' \
  --version 1.2.3 >/dev/null || fail "installed native npm payload lost its identity"
[ ! -e "$TMP/native-root/@fixture/native.oh-my-setting-backup" ] ||
  fail "native npm install leaked its recovery backup"
if python3 "$HELPER" --lock "$LOCK" verify-npm-package \
    --file "$TMP/native.tgz" --name '@fixture/wrong' --version 1.2.3 \
    >/dev/null 2>&1; then
  fail "native npm package with a mismatched identity was accepted"
fi

# Direct CLI payloads use a same-directory stage and recoverable swap. They
# refuse an unowned live target, restore an interrupted pre-rename update, and
# finish an interrupted post-rename update without leaving backup files.
(
  export HOME="$TMP/direct-home"
  mkdir -p "$HOME/.local/bin"
  # shellcheck source=scripts/install-tools.sh
  . "$ROOT/scripts/install-tools.sh"
  make_direct_fixture() {
    printf '#!/usr/bin/env bash\necho "fixture %s"\n' "$2" > "$1"
    chmod +x "$1"
  }
  source_file="$TMP/direct-source"
  target="$HOME/.local/bin/fixture"
  make_direct_fixture "$source_file" 2.0.0
  make_direct_fixture "$target" 1.0.0
  digest="$(sha256_file "$source_file")"
  if install_direct_binary "$source_file" "$target" sha256 "$digest" \
      2.0.0 fixture >/dev/null 2>&1; then
    fail "direct tool installer overwrote an unowned live target"
  fi
  [ "$($target --version 2>/dev/null || $target)" = 'fixture 1.0.0' ] ||
    fail "direct tool collision changed the user-owned target"
  printf 'sha256=%s\nextra' "$(sha256_file "$target")" > \
    "$target.oh-my-setting-managed"
  if install_direct_binary "$source_file" "$target" sha256 "$digest" \
      2.0.0 fixture >/dev/null 2>&1; then
    fail "malformed ownership sidecar authorized a direct tool overwrite"
  fi
  rm -f "$target.oh-my-setting-managed"

  rm -f "$target"
  install_direct_binary "$source_file" "$target" sha256 "$digest" \
    2.0.0 fixture >/dev/null || fail "direct tool first install failed"
  [ -f "$target.oh-my-setting-managed" ] ||
    fail "direct tool install did not record ownership"

  make_direct_fixture "$source_file" 3.0.0
  digest="$(sha256_file "$source_file")"
  install_direct_binary "$source_file" "$target" sha256 "$digest" \
    3.0.0 fixture >/dev/null || fail "owned direct tool update failed"

  printf '# same-version tamper\n' >> "$target"
  if direct_locked_command_is_current fixture "$target" 3.0.0; then
    fail "managed direct tool tamper was accepted by the fast path"
  fi
  install_direct_binary "$source_file" "$target" sha256 "$digest" \
    3.0.0 fixture >/dev/null || fail "managed direct tool tamper was not repaired"
  [ "$(sha256_file "$target")" = "$digest" ] ||
    fail "managed direct tool repair did not restore the locked payload"

  make_direct_fixture "$source_file" 4.0.0
  digest="$(sha256_file "$source_file")"
  cp "$source_file" "$target.oh-my-setting-stage"
  mv "$target" "$target.oh-my-setting-backup"
  recover_direct_install "$target" sha256 "$digest" 4.0.0 >/dev/null ||
    fail "interrupted direct tool update was not recovered"
  [ "$($target)" = 'fixture 3.0.0' ] &&
    [ ! -e "$target.oh-my-setting-stage" ] &&
    [ ! -e "$target.oh-my-setting-backup" ] ||
    fail "pre-rename direct tool recovery did not restore the previous target"

  mv "$target" "$target.oh-my-setting-backup"
  cp "$source_file" "$target"
  chmod +x "$target"
  recover_direct_install "$target" sha256 "$digest" 4.0.0 >/dev/null ||
    fail "post-rename direct tool update was not completed"
  [ "$($target)" = 'fixture 4.0.0' ] &&
    [ ! -e "$target.oh-my-setting-backup" ] ||
    fail "post-rename direct tool recovery left stale state"

  # uvx is a launcher that discovers an adjacent uv binary. Its staged name
  # therefore cannot be verified unless the matching uv stage is present, and
  # both collision checks must finish before either live command changes.
  pair_source="$TMP/uv-pair/source"
  pair_bin="$TMP/uv-pair/bin"
  mkdir -p "$pair_source" "$pair_bin"
  printf '%s\n' '#!/usr/bin/env bash' 'echo "uv 5.0.0"' > "$pair_source/uv"
  cat > "$pair_source/uvx" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd -P)"
case "$(basename "$0")" in
  uvx.oh-my-setting-stage) companion="$here/uv.oh-my-setting-stage" ;;
  *) companion="$here/uv" ;;
esac
[ -x "$companion" ] || { echo "missing adjacent uv" >&2; exit 1; }
exec "$companion" --version
EOF
  chmod +x "$pair_source/uv" "$pair_source/uvx"
  uv_digest="$(sha256_file "$pair_source/uv")"
  uvx_digest="$(sha256_file "$pair_source/uvx")"
  printf '%s\n' '#!/usr/bin/env bash' 'echo "user uvx 5.0.0"' > "$pair_bin/uvx"
  chmod +x "$pair_bin/uvx"
  if install_uv_binaries "$pair_source/uv" "$pair_source/uvx" \
      "$pair_bin/uv" "$pair_bin/uvx" "$uv_digest" "$uvx_digest" \
      5.0.0 fixture >/dev/null 2>&1; then
    fail "uv pair installer overwrote an unowned uvx collision"
  fi
  [ ! -e "$pair_bin/uv" ] ||
    fail "uv pair installer changed uv before preflighting uvx"
  rm -f "$pair_bin/uvx"
  install_uv_binaries "$pair_source/uv" "$pair_source/uvx" \
    "$pair_bin/uv" "$pair_bin/uvx" "$uv_digest" "$uvx_digest" \
    5.0.0 fixture >/dev/null || fail "adjacent uv/uvx pair install failed"
  [ "$($pair_bin/uv --version)" = 'uv 5.0.0' ] &&
    [ "$($pair_bin/uvx --version)" = 'uv 5.0.0' ] &&
    [ -f "$pair_bin/uv.oh-my-setting-managed" ] &&
    [ -f "$pair_bin/uvx.oh-my-setting-managed" ] ||
    fail "uv pair install did not publish verified managed commands"

  npm_root="$TMP/npm-transaction/root"
  npm_bin="$TMP/npm-transaction/bin"
  npm_package="$npm_root/@fixture/tool"
  mkdir -p "$npm_package" "$npm_bin"
  printf 'old package\n' > "$npm_package/state"
  ln -s ../root/@fixture/tool/state "$npm_bin/fixture"
  npm_transaction_begin "$npm_package" "$npm_bin" fixture ||
    fail "npm rollback transaction could not start"
  mkdir -p "$npm_package"
  printf 'partial package\n' > "$npm_package/state"
  printf 'partial binary\n' > "$npm_bin/fixture"
  npm_transaction_restore "$npm_package" "$npm_bin" fixture ||
    fail "npm rollback transaction could not restore"
  [ "$(cat "$npm_package/state")" = 'old package' ] &&
    [ -L "$npm_bin/fixture" ] &&
    [ ! -e "$npm_package.oh-my-setting-transaction" ] &&
    [ ! -e "$npm_package.oh-my-setting-backup" ] ||
    fail "failed npm install did not restore its previous package and shim"
)

# Doctor must surface a damaged repository lock before an install/repair tries
# to consume it. The standalone check keeps this regression independent from
# whatever provider CLIs happen to be installed on the test host.
python3 - "$LOCK" "$TMP/invalid-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["nvm"]["sha256"] = "not-a-digest"
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if OH_MY_SETTING_TOOL_LOCK="$TMP/invalid-lock.json" \
    bash "$ROOT/scripts/doctor.sh" --tool-lock >"$TMP/doctor.out" 2>&1; then
  fail "doctor accepted a damaged tool lock"
fi
grep -Fq 'tool lock: invalid' "$TMP/doctor.out" || {
  sed -n '1,30p' "$TMP/doctor.out" >&2
  fail "doctor did not explain the damaged tool lock"
}

python3 - "$LOCK" "$TMP/invalid-native-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["npm"]["codex"]["native"]["linux-amd64"]["alias"] = "@openai/wrong"
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/invalid-native-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted a native payload bound to the wrong platform alias"
fi

python3 - "$LOCK" "$TMP/invalid-platform-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["gh"]["platforms"]["linux-amd64"]["archive"] = "zip"
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/invalid-platform-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted a payload archive type bound to the wrong platform"
fi

python3 - "$LOCK" "$TMP/swapped-platform-url-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
left = row["uv"]["platforms"]["linux-amd64"]["url"]
row["uv"]["platforms"]["linux-amd64"]["url"] = \
    row["uv"]["platforms"]["darwin-amd64"]["url"]
row["uv"]["platforms"]["darwin-amd64"]["url"] = left
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/swapped-platform-url-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted URLs swapped between platform rows"
fi

python3 - "$LOCK" "$TMP/wrong-host-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["uv"]["platforms"]["linux-amd64"]["url"] = \
    row["uv"]["platforms"]["linux-amd64"]["url"].replace(
        "https://github.com/", "https://example.test/"
    )
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/wrong-host-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted a platform payload from the wrong upstream host"
fi

python3 - "$LOCK" "$TMP/invalid-date-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["updated_at"] = "2026-02-31"
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/invalid-date-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted an impossible update date"
fi

python3 - "$LOCK" <<'PY' || fail "tool lock contains floating or incomplete sources"
import json, re, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema"] == 2
assert re.fullmatch(r"\d+\.\d+\.\d+", row["python"]["version"])
assert re.fullmatch(r"v?\d+\.\d+\.\d+", row["node"]["version"])
assert row["node"]["platforms"]
for platform, item in row["node"]["platforms"].items():
    assert platform in {"darwin-amd64", "darwin-arm64", "linux-amd64", "linux-arm64"}, item
    assert item["url"].startswith("https://nodejs.org/dist/"), item
    assert re.fullmatch(r"[0-9a-f]{64}", item["sha256"]), item
assert re.fullmatch(r"[0-9a-f]{64}", row["nvm"]["sha256"]), row["nvm"]
assert re.fullmatch(r"[0-9a-f]{64}", row["nvm"]["script_sha256"]), row["nvm"]
assert set(row["uv"]["platforms"]) == {
    "darwin-amd64", "darwin-arm64", "linux-amd64", "linux-arm64",
    "windows-amd64", "windows-arm64",
}
for platform, item in row["uv"]["platforms"].items():
    assert re.fullmatch(r"[0-9a-f]{64}", item["sha256"]), (platform, item)
for name, item in row["npm"].items():
    assert re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?", item["version"]), (name, item)
    assert item["integrity"].startswith("sha512-"), (name, item)
for name, expected in (("codex", 6), ("claude", 8)):
    native = row["npm"][name]["native"]
    assert len(native) == expected, (name, native)
    for platform, item in native.items():
        assert platform.startswith(("darwin-", "linux-", "windows-")), platform
        assert item["integrity"].startswith("sha512-"), (platform, item)
        assert item["alias"].startswith("@"), (platform, item)
assert "native" not in row["npm"]["ntn"]
for family in ("antigravity", "gh"):
    assert row[family]["platforms"], family
    for platform, item in row[family]["platforms"].items():
        key = "sha512" if family == "antigravity" else "sha256"
        assert re.fullmatch(r"[0-9a-f]{128}" if key == "sha512" else r"[0-9a-f]{64}", item[key]), (platform, item)
        assert item["url"].startswith("https://"), item
PY

# A clean but older user NVM is not a corrupt copy of the newly locked NVM.
# Preserve it, never source it, and activate the independently verified Node
# payload directly. This is the reinstall path that used to stop on every NVM
# lock bump before the receipt and auto-update wiring could be refreshed.
(
  export HOME="$TMP/older-nvm-home"
  export NVM_DIR="$HOME/.nvm"
  old_bin="$TMP/older-nvm-bin"
  mkdir -p "$NVM_DIR" "$old_bin"
  cat > "$NVM_DIR/nvm.sh" <<'EOF_NVM'
printf 'sourced\n' > "$HOME/nvm-was-sourced"
EOF_NVM
  printf '%s\n' '#!/usr/bin/env bash' 'echo v1.0.0' > "$old_bin/node"
  printf '%s\n' '#!/usr/bin/env bash' 'echo npm-old' > "$old_bin/npm"
  chmod +x "$old_bin/node" "$old_bin/npm"
  export PATH="$old_bin:$PATH"
  # shellcheck source=scripts/install-tools.sh
  . "$ROOT/scripts/install-tools.sh"
  install_locked_node() {
    local target="$NVM_DIR/versions/node/v$NODE_VERSION/bin"
    mkdir -p "$target"
    printf '%s\n' '#!/usr/bin/env bash' "echo v$NODE_VERSION" > "$target/node"
    printf '%s\n' '#!/usr/bin/env bash' 'echo npm-locked' > "$target/npm"
    chmod +x "$target/node" "$target/npm"
  }
  ensure_node >/dev/null || fail "an older NVM blocked locked Node activation"
  [ "$(command -v node)" = "$NVM_DIR/versions/node/v$NODE_VERSION/bin/node" ] ||
    fail "locked Node was not activated directly beside an older NVM"
  [ ! -e "$HOME/nvm-was-sourced" ] ||
    fail "an older NVM was sourced while activating locked Node"
)

# A fresh NVM install must bind both the archive and the extracted nvm.sh to
# their independent lock digests. Keep this behavioral: it catches an unset
# script digest before a downloaded shell file can be promoted.
(
  export HOME="$TMP/fresh-nvm-home"
  export NVM_DIR="$HOME/.nvm"
  mkdir -p "$HOME"
  # shellcheck source=scripts/install-tools.sh
  . "$ROOT/scripts/install-tools.sh"
  download_locked() { printf 'archive\n' > "$2"; }
  extract_locked() {
    mkdir -p "$3/nvm-${NVM_VERSION#v}"
    printf '#!/usr/bin/env bash\n' > "$3/nvm-${NVM_VERSION#v}/nvm.sh"
  }
  verify_digest() {
    [ -n "$2" ] || fail "fresh NVM install used an empty lock digest for $4"
  }
  install_nvm >/dev/null || fail "fresh locked NVM install failed"
  [ -f "$NVM_DIR/nvm.sh" ] || fail "fresh locked NVM was not promoted"
)

# A downloaded installer/archive whose bytes do not match the lock must never
# reach tar, bash, or another executable. This is the fail-closed regression
# for the old curl-pipe execution path.
mkdir -p "$TMP/bin" "$TMP/home"
python3 - "$LOCK" "$TMP/bad-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["nvm"]["sha256"] = "a" * 64
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
cat > "$TMP/bin/curl" <<'EOF'
#!/usr/bin/env bash
out=""
while [ "$#" -gt 0 ]; do
  case "$1" in -o) out="$2"; shift 2 ;; *) shift ;; esac
done
printf 'untrusted payload\n' > "$out"
EOF
cat > "$TMP/bin/tar" <<'EOF'
#!/usr/bin/env bash
printf 'tar executed\n' > "$TOOL_LOCK_TAR_RAN"
exit 99
EOF
chmod +x "$TMP/bin/curl" "$TMP/bin/tar"

rc=0
env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:/usr/bin:/bin" \
  OH_MY_SETTING_TOOL_LOCK="$TMP/bad-lock.json" TOOL_LOCK_TAR_RAN="$TMP/tar-ran" \
  "$ROOT/scripts/install-tools.sh" >/dev/null 2>"$TMP/install.err" || rc=$?
[ "$rc" -ne 0 ] || fail "digest mismatch was accepted"
[ ! -e "$TMP/tar-ran" ] || fail "unverified nvm archive reached tar"
grep -Fq 'checksum mismatch' "$TMP/install.err" || {
  sed -n '1,20p' "$TMP/install.err" >&2
  fail "digest failure was not explained"
}

# Network-selected versions and pipe-to-shell execution are forbidden in the
# installer now that the repository owns an explicit lock.
if grep -En 'releases/latest|npm install -g "\$package"|curl[^|]*\|[[:space:]]*(sh|bash)' \
    "$ROOT/scripts/install-tools.sh" >/dev/null; then
  fail "install-tools still contains a floating or pipe-to-shell install"
fi
grep -Fq 'verify-sri' "$ROOT/scripts/install-tools.sh" ||
  fail "npm install bytes are not checked against locked integrity"
grep -Fq 'npm pack' "$ROOT/scripts/install-tools.sh" ||
  fail "npm packages are not staged before installation"
grep -Fq 'npm pack "$spec" --ignore-scripts' "$ROOT/scripts/install-tools.sh" ||
  fail "npm package staging can still run registry package lifecycle scripts"
if grep -Fq 'npm view "$spec" dist.integrity' "$ROOT/scripts/install-tools.sh"; then
  fail "npm integrity is still only a metadata preflight"
fi
grep -Fq 'node.platforms.$platform.sha256' "$ROOT/scripts/install-tools.sh" ||
  fail "Node payload is not bound to the repository lock"
grep -Fq 'nvm.script_sha256' "$ROOT/scripts/install-tools.sh" ||
  fail "an existing nvm.sh can be sourced without repository verification"
grep -Fq '.oh-my-setting-complete' "$ROOT/scripts/install-tools.sh" ||
  fail "interrupted Node/nvm staging has no completion marker for recovery"
if grep -Fq '. "$NVM_DIR/nvm.sh"' "$ROOT/install.sh"; then
  fail "the installer re-sources mutable nvm code after locked tools are installed"
fi
grep -Fq "get \"\$1\" | tr -d '\\r'" "$ROOT/scripts/install-tools.sh" ||
  fail "Python-to-Bash tool lock scalars are not normalized for native Windows"
grep -Fq 'uv.platforms.$platform.sha256' "$ROOT/scripts/install-tools.sh" ||
  fail "uv still executes a verified installer that selects an unverified payload"
if grep -En 'nvm[[:space:]]+install' "$ROOT/scripts/install-tools.sh" >/dev/null; then
  fail "Node still delegates payload selection to nvm instead of the lock"
fi
grep -Fq -- '--offline' "$ROOT/scripts/install-tools.sh" ||
  fail "verified npm tarballs can still trigger an unbounded registry fetch"
grep -Fq -- '--cache "$tmp/install-cache"' "$ROOT/scripts/install-tools.sh" ||
  fail "npm install can still reuse unrelated cached registry payloads"
grep -Fq 'npm.$name.native.$native_platform.integrity' \
  "$ROOT/scripts/install-tools.sh" ||
  fail "platform-native npm payloads are not selected from the lock"
grep -Fq 'install-npm-payload' "$ROOT/scripts/install-tools.sh" ||
  fail "verified native npm payloads are not installed through the safe helper"
shim_body="$(sed -n '/^write_npm_shim()/,/^}/p' "$ROOT/scripts/install-tools.sh")"
if printf '%s\n' "$shim_body" | grep -Fq 'nvm.sh'; then
  fail "provider shims source mutable nvm code without revalidating it"
fi

echo "tool-lock-smoke: ok"
