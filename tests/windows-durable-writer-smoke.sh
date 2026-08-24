#!/usr/bin/env bash
set -euo pipefail

# Native Windows Git Bash exercises the pathname branch that deliberately
# avoids opening/fsyncing directories. The same contract runs on POSIX so the
# helper remains covered by the local focused gate.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-windows-durable.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

case "${MSYSTEM:-}:${OSTYPE:-}:$(uname -s 2>/dev/null || printf unknown)" in
  *MINGW*|*MSYS*|*CYGWIN*|*:msys:*|*:cygwin:*)
    python3 -c 'import os; assert os.name == "nt", os.name' ||
      fail "Windows durable coverage requires native Windows Python"
    ;;
esac

export HOME="$TMP/home"
export OMS_LOCK_DIR="$TMP/locks"
mkdir -p "$HOME"
unset OMS_ARTIFACT_INDEX
export OMS_ARTIFACT_INDEX_KEEP=1000
export OMS_ARTIFACT_INDEX_HIGH_WATER=1200

repo="$TMP/repo"
mkdir -p "$repo/.oms/artifacts" "$repo/.oms/plan"

printf '{"kind":"progress"}\n' |
  python3 "$ROOT/scripts/lib/durable-jsonl.py" append \
    "$repo/.oms/plan/progress.jsonl" ||
  fail "durable append must work without a directory descriptor"
printf 'replacement body\n' |
  python3 "$ROOT/scripts/lib/durable-jsonl.py" --label body write \
    "$repo/.oms/plan/body.txt" ||
  fail "durable replace must work without a directory descriptor"
grep -Fxq 'replacement body' "$repo/.oms/plan/body.txt" ||
  fail "durable replacement body did not persist"

python3 - "$ROOT/scripts/lib/durable-jsonl.py" \
  "$ROOT/scripts/lib/artifact-index-store.py" "$repo" <<'PY'
import contextlib, io, json, os, runpy, stat, sys
from types import SimpleNamespace
durable_path, store_path, repo = sys.argv[1:]
durable = runpy.run_path(durable_path)
store = runpy.run_path(store_path)
index = repo + "/.oms/artifacts/index.jsonl"

# CPython 3.13 intentionally exposes different st_ctime_ns meanings for a
# Windows descriptor (metadata change time) and pathname stat (legacy birth
# time). A mixed-source CAS must compare the birth time available on both
# results while retaining inode, size, mtime, and link-count fences.
opened_stat = SimpleNamespace(
    st_dev=7, st_ino=11, st_size=13, st_mtime_ns=17, st_ctime_ns=19,
    st_birthtime_ns=23, st_nlink=1)
named_stat = SimpleNamespace(
    st_dev=7, st_ino=11, st_size=13, st_mtime_ns=17, st_ctime_ns=29,
    st_birthtime_ns=23, st_nlink=1)
saved_os_name = durable["os"].name
durable["os"].name = "nt"
try:
    assert not durable["_snapshot_matches"](opened_stat, named_stat)
    assert durable["_snapshot_matches"](
        opened_stat, named_stat, mixed_sources=True)
    changed_birth = SimpleNamespace(**vars(named_stat))
    changed_birth.st_birthtime_ns += 1
    assert not durable["_snapshot_matches"](
        opened_stat, changed_birth, mixed_sources=True)
    legacy_opened = SimpleNamespace(
        st_dev=7, st_ino=11, st_size=13, st_mtime_ns=17,
        st_ctime_ns=31, st_nlink=1)
    legacy_named = SimpleNamespace(**vars(legacy_opened))
    assert durable["_snapshot_matches"](
        legacy_opened, legacy_named, mixed_sources=True)
    legacy_named.st_ctime_ns += 1
    assert not durable["_snapshot_matches"](
        legacy_opened, legacy_named, mixed_sources=True)
    one_sided_birth = SimpleNamespace(**vars(legacy_opened))
    one_sided_birth.st_birthtime_ns = 23
    assert not durable["_snapshot_matches"](
        one_sided_birth, legacy_opened, mixed_sources=True)
finally:
    durable["os"].name = saved_os_name

# Native Windows reaches durable-jsonl's os.name == "nt" branch here; it is
# not a monkeypatch. Both capture modes must refuse before a near-ceiling
# progress ledger changes by even one byte.
ceiling = repo + "/.oms/plan/near-ceiling.jsonl"
ceiling_body = b"x" * (durable["MAX_FILE_BYTES"] - 1)
with open(ceiling, "wb") as handle:
    handle.write(ceiling_body)
for capture in (False, True):
    try:
        with contextlib.redirect_stderr(io.StringIO()):
            durable["append"](ceiling, b"{}\n", "near-ceiling", capture=capture)
    except SystemExit:
        pass
    else:
        raise AssertionError("near-ceiling append was accepted (capture=%r)" % capture)
    assert open(ceiling, "rb").read() == ceiling_body

target = {
    "schema": 1, "event_id": "evt_target", "operation_id": "op_target",
    "artifact_id": "sha256:" + "a" * 64,
    "ts": "2026-08-23T00:00:00Z", "kind": "call",
    "provider": "codex", "exit": 1,
}
resolution = {
    "schema": 1, "event_id": "evt_resolution", "operation_id": "op_target",
    "artifact_id": "sha256:" + "a" * 64,
    "ts": "2026-08-23T00:00:01Z", "kind": "artifact-resolution",
    "provider": "codex", "exit": 0, "parent_event_id": "evt_target",
    "resolves_event_id": "evt_target", "resolution": "resolved",
}
store["append_rows"](repo, index, [target])
if os.name == "nt":
    descriptor = os.open(index, os.O_RDONLY | getattr(os, "O_BINARY", 0))
    try:
        opened_index = os.fstat(descriptor)
        named_index = os.lstat(index)
    finally:
        os.close(descriptor)
    fields = (
        "st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns",
        "st_birthtime_ns", "st_nlink")
    assert durable["_snapshot_matches"](
        opened_index, named_index, mixed_sources=True), {
            field: (getattr(opened_index, field, None),
                    getattr(named_index, field, None))
            for field in fields
        }
store["append_rows"](repo, index, [resolution], keep=2, high=2)
rows = [json.loads(line) for line in store["read_index"](repo, index).splitlines()]
assert [row["event_id"] for row in rows] == ["evt_target", "evt_resolution"], rows

# Force byte pressure independently of the fixed 16MiB production ceiling:
# the required new resolution remains indivisible from its older target.
target_line = (json.dumps(target) + "\n").encode()
filler = dict(target, event_id="evt_filler", operation_id="op_filler",
              artifact_id="sha256:" + "b" * 64, exit=0)
filler_line = (json.dumps(filler) + "\n").encode()
resolution_line = (json.dumps(resolution) + "\n").encode()
bounded = store["_bounded"](
    [target_line, filler_line, resolution_line], 2,
    len(target_line) + len(resolution_line), ["evt_resolution"])
bounded_ids = [json.loads(line)["event_id"] for line in bounded.splitlines()]
assert bounded_ids == ["evt_target", "evt_resolution"], bounded_ids

# Even an invalid legacy chain where a resolution targets another resolution
# must never be compacted into dangling retained JSONL. Retention follows the
# complete backward target closure in both ordinary and mandatory-new-row
# selection.
nested_root = dict(target, event_id="evt_nested_root", operation_id="op_nested")
nested_inner = dict(
    resolution, event_id="evt_nested_inner", operation_id="op_nested",
    parent_event_id="evt_nested_root", resolves_event_id="evt_nested_root")
nested_outer = dict(
    resolution, event_id="evt_nested_outer", operation_id="op_nested",
    parent_event_id="evt_nested_inner", resolves_event_id="evt_nested_inner")
nested_lines = [
    (json.dumps(row) + "\n").encode()
    for row in (nested_root, nested_inner, nested_outer)
]
for required in ([], ["evt_nested_outer"]):
    nested_bounded = store["_bounded"](
        nested_lines, 3, sum(map(len, nested_lines)), required)
    nested_ids = [
        json.loads(line)["event_id"] for line in nested_bounded.splitlines()
    ]
    assert nested_ids == [
        "evt_nested_root", "evt_nested_inner", "evt_nested_outer"
    ], (required, nested_ids)

# A copy-on-write replace must preserve the pending-receipt signal from a
# read-only ledger. Windows' read-only attribute and POSIX mode bits are both
# exercised by this fixture.
readonly = store["read_index"](repo, index)
os.chmod(index, stat.S_IREAD)
try:
    extra = dict(filler, event_id="evt_readonly", operation_id="op_readonly")
    try:
        with contextlib.redirect_stderr(io.StringIO()):
            store["append_rows"](repo, index, [extra])
    except SystemExit:
        pass
    else:
        raise AssertionError("read-only artifact index was replaced")
    assert open(index, "rb").read() == readonly
finally:
    os.chmod(index, stat.S_IREAD | stat.S_IWRITE)

# The native Windows job reaches delete_orphans' pathname unlink branch. It
# removes one owned aged orphan while treating a repo-bound custom artifact as
# valid evidence outside this cleanup primitive's ownership.
custom_dir = os.path.join(repo, "custom")
os.makedirs(custom_dir)
custom_path = os.path.join(custom_dir, "evidence.md")
with open(custom_path, "wb") as handle:
    handle.write(b"custom evidence survives\n")
custom = dict(
    filler, event_id="evt_custom", operation_id="op_custom",
    artifact="custom/evidence.md")
store["append_rows"](repo, index, [custom])
orphan = os.path.join(repo, ".oms", "artifacts", "orphan.md")
with open(orphan, "wb") as handle:
    handle.write(b"owned orphan\n")
os.utime(orphan, (0, 0))
kept = store["read_index"](repo, index)
changed, fresh = store["delete_orphans"](
    repo, index, kept, dry_run=False, grace=0)
assert ".oms/artifacts/orphan.md" in [item.replace("\\", "/") for item in changed]
assert not os.path.exists(orphan)
assert open(custom_path, "rb").read() == b"custom evidence survives\n"
assert fresh == 0

# A crash-truncated ledger is not silently repaired around; the transaction
# refuses before replace and keeps every byte intact for diagnosis.
partial = b'{"schema":1,"event_id":"evt_partial"}'
with open(index, "wb") as handle:
    handle.write(partial)
try:
    with contextlib.redirect_stderr(io.StringIO()):
        store["append_rows"](repo, index, [target])
except SystemExit:
    pass
else:
    raise AssertionError("partial final JSONL row was accepted")
assert open(index, "rb").read() == partial

# Hard links are available on normal NTFS even when symlink privileges are
# unavailable, so the Windows boundary always gets a non-skippable link test.
outside_hard = os.path.join(os.path.dirname(repo), "outside-hard.jsonl")
with open(outside_hard, "wb") as handle:
    handle.write(b"outside hard untouched\n")
os.unlink(index)
os.link(outside_hard, index)
try:
    try:
        with contextlib.redirect_stderr(io.StringIO()):
            store["append_rows"](repo, index, [target])
    except SystemExit:
        pass
    else:
        raise AssertionError("hard-linked artifact index was accepted")
    assert open(outside_hard, "rb").read() == b"outside hard untouched\n"
finally:
    os.unlink(index)
PY

# The recovery path uses the same native pathname-safe CoW branch. Its default
# is read-only; --apply quarantines the exact corrupt bytes, salvages complete
# JSON objects, appends one receipt, and is then an idempotent no-op.
salvage_repo="$TMP/windows-salvage"
mkdir -p "$salvage_repo/.oms/artifacts" "$salvage_repo/.oms/plan"
printf '{"progress":"untouched"}\n' > "$salvage_repo/.oms/plan/progress.jsonl"
python3 - "$salvage_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
row = {"schema": 0, "event_id": "evt_windows_salvage", "kind": "legacy"}
with open(sys.argv[1], "wb") as handle:
    handle.write(json.dumps(row).encode() + b"\r\n")
    handle.write(b'{"invalid":"\xff"}\n')
    handle.write(b"broken\n")
    handle.write(b'{"event_id":"evt_bare_one"}\r{"event_id":"evt_bare_two"}\n')
    handle.write(b'{"metric":NaN}\n')
    handle.write(b'{"metric":Infinity}\n')
PY
cp "$salvage_repo/.oms/artifacts/index.jsonl" "$TMP/windows-salvage-raw"
cp "$salvage_repo/.oms/plan/progress.jsonl" "$TMP/windows-salvage-progress"
if bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_repo" list \
    >/dev/null 2>&1; then
  fail "native normal view accepted non-LF or non-finite JSON"
fi
if bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_repo" validate \
    >/dev/null 2>&1; then
  fail "native validate accepted non-LF or non-finite JSON"
fi
bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_repo" salvage \
  >/dev/null || fail "native salvage plan failed"
cmp -s "$TMP/windows-salvage-raw" \
  "$salvage_repo/.oms/artifacts/index.jsonl" ||
  fail "native salvage plan changed the index"
[ ! -e "$salvage_repo/.oms/artifacts/quarantine" ] ||
  fail "native salvage plan created quarantine state"
bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_repo" salvage --apply \
  >/dev/null || fail "native salvage apply failed"
python3 - "$salvage_repo" "$TMP/windows-salvage-raw" <<'PY' ||
import hashlib, json, pathlib, sys
repo = pathlib.Path(sys.argv[1])
raw = pathlib.Path(sys.argv[2]).read_bytes()
digest = hashlib.sha256(raw).hexdigest()
quarantine = repo / ".oms" / "artifacts" / "quarantine" / (
    "artifact-index-%s.raw" % digest)
assert quarantine.read_bytes() == raw
rows = [json.loads(line) for line in (
    repo / ".oms" / "artifacts" / "index.jsonl").read_bytes().splitlines()]
assert rows[0]["event_id"] == "evt_windows_salvage", rows
assert rows[-1]["kind"] == "artifact-index-salvage", rows
assert rows[-1]["recovered_rows"] == 1, rows
assert rows[-1]["dropped_rows"] == 5, rows
assert rows[-1]["quarantine_sha256"] == digest, rows
PY
  fail "native salvage result was incomplete"
cp "$salvage_repo/.oms/artifacts/index.jsonl" "$TMP/windows-salvage-repaired"
bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_repo" salvage --apply \
  >/dev/null || fail "native salvage rerun failed"
cmp -s "$TMP/windows-salvage-repaired" \
  "$salvage_repo/.oms/artifacts/index.jsonl" ||
  fail "native salvage rerun changed a healthy index"
cmp -s "$TMP/windows-salvage-progress" \
  "$salvage_repo/.oms/plan/progress.jsonl" ||
  fail "native salvage changed progress.jsonl"

# NTFS hard links need no developer-mode symlink permission. A planted
# content-addressed quarantine name must be refused before either the corrupt
# index or its outside twin changes.
salvage_hard_repo="$TMP/windows-salvage-hard"
mkdir -p "$salvage_hard_repo/.oms/artifacts/quarantine"
printf '%s\n%s\n' '{"schema":0,"event_id":"evt_hard","kind":"legacy"}' \
  'broken' > "$salvage_hard_repo/.oms/artifacts/index.jsonl"
hard_digest="$(python3 - "$salvage_hard_repo/.oms/artifacts/index.jsonl" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
hard_outside="$TMP/windows-salvage-hard-outside"
printf 'outside survives\n' > "$hard_outside"
ln "$hard_outside" \
  "$salvage_hard_repo/.oms/artifacts/quarantine/artifact-index-$hard_digest.raw"
cp "$salvage_hard_repo/.oms/artifacts/index.jsonl" \
  "$TMP/windows-salvage-hard-before"
if bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_hard_repo" \
    salvage --apply >/dev/null 2>&1; then
  fail "native salvage accepted a hard-linked quarantine"
fi
cmp -s "$TMP/windows-salvage-hard-before" \
  "$salvage_hard_repo/.oms/artifacts/index.jsonl" ||
  fail "native hard-link refusal changed the index"
grep -Fxq 'outside survives' "$hard_outside" ||
  fail "native hard-link refusal changed outside bytes"

# Native Python emits a drive or UNC spelling while Git Bash's file-lock
# helper requires one leading '/'. Normalize before keying the lock, so the
# same physical ledger cannot acquire a different lock merely because the
# caller's working directory differs.
shell_repo="$TMP/shell-repo"
mkdir -p "$shell_repo/sub"
test_native_path() {
  local value="$1"
  case "${MSYSTEM:-}:${OSTYPE:-}:$(uname -s 2>/dev/null || printf unknown)" in
    *MINGW*|*MSYS*|*CYGWIN*|*:msys:*|*:cygwin:*)
      value="$(cygpath -m "$value")" || return $?
      value="${value//$'\r'/}"
      ;;
  esac
  printf '%s\n' "$value"
}
native_repo="$(test_native_path "$shell_repo")"
native_store="$(test_native_path "$ROOT/scripts/lib/artifact-index-store.py")"
native_index_input="$(test_native_path \
  "$shell_repo/.oms/artifacts/index.jsonl")"
native_index="$(MSYS2_ARG_CONV_EXCL='*' python3 "$native_store" canonical \
  --repo "$native_repo" --index "$native_index_input")"
native_index="${native_index//$'\r'/}"

# The public shell front door owns the Git-Bash/native-Python conversion. An
# explicit missing index is a normal empty view, not a false containment error.
missing_rc=0
missing_out="$(MSYS2_ARG_CONV_EXCL="$shell_repo/.oms" \
  bash "$ROOT/scripts/artifact-index.sh" --repo "$shell_repo" \
    --file "$shell_repo/.oms/artifacts/index.jsonl" list 2>&1)" || missing_rc=$?
[ "$missing_rc" = 2 ] ||
  fail "native explicit missing index returned $missing_rc instead of 2"
printf '%s\n' "$missing_out" | grep -Fq 'no artifact index at' ||
  fail "native explicit index failed before the empty-index view: $missing_out"

# The local gate fixes the converter contract even without Windows: CRLF is
# stripped, relative artifact paths stay relative, and missing cygpath fails.
# shellcheck source=scripts/lib/peer-common.sh
. "$ROOT/scripts/lib/peer-common.sh"
fake_cygpath_bin="$TMP/fake-cygpath-bin"
fake_cygpath_log="$TMP/fake-cygpath.log"
mkdir -p "$fake_cygpath_bin"
cat > "$fake_cygpath_bin/cygpath" <<'SH'
#!/usr/bin/env bash
[ "$1" = -m ] || exit 9
printf '%s\n' "$*" >> "$OMS_TEST_CYGPATH_LOG"
value="$2"
case "$value" in /c/*) value="C:/${value#/c/}" ;; esac
printf '%s\r\n' "$value"
SH
chmod +x "$fake_cygpath_bin/cygpath"
converted="$(MSYSTEM=MINGW64 OMS_TEST_CYGPATH_LOG="$fake_cygpath_log" \
  PATH="$fake_cygpath_bin:$PATH" \
  ma_artifact_index_python_path /c/fixture/repo/.oms/artifacts/index.jsonl)"
[ "$converted" = 'C:/fixture/repo/.oms/artifacts/index.jsonl' ] ||
  fail "artifact Python path conversion kept CRLF or changed the path: $converted"
relative="$(MSYSTEM=MINGW64 OMS_TEST_CYGPATH_LOG="$fake_cygpath_log" \
  PATH="$fake_cygpath_bin:$PATH" ma_artifact_index_python_path answer.md)"
[ "$relative" = answer.md ] || fail "relative artifact path was made absolute"
[ "$(wc -l < "$fake_cygpath_log" | tr -d ' ')" -eq 1 ] ||
  fail "relative artifact path unexpectedly invoked cygpath"
missing_cygpath_rc=0
(
  MSYSTEM=MINGW64 PATH="$TMP/empty-path" \
    ma_artifact_index_python_path /c/fixture/index.jsonl >/dev/null 2>&1
) || missing_cygpath_rc=$?
[ "$missing_cygpath_rc" -ne 0 ] ||
  fail "Windows artifact path conversion ignored missing cygpath"
root_lock="$(
  cd "$shell_repo"
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  normalized="$(ma_artifact_index_shell_path "$native_index")"
  oms_file_lock_path_for_file "$normalized"
)"
sub_lock="$(
  cd "$shell_repo/sub"
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  normalized="$(ma_artifact_index_shell_path "$native_index")"
  oms_file_lock_path_for_file "$normalized"
)"
[ "$root_lock" = "$sub_lock" ] ||
  fail "native canonical path produced CWD-dependent artifact locks"
printf 'root artifact\n' > "$shell_repo/answer-root.md"
printf 'sub artifact\n' > "$shell_repo/sub/answer-sub.md"

(
  cd "$shell_repo"
  export MSYS2_ARG_CONV_EXCL="$shell_repo/.oms"
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index . call codex 0 "answer-root.md" "" "" "" ""
) & root_pid=$!
(
  cd "$shell_repo/sub"
  export MSYS2_ARG_CONV_EXCL="$shell_repo/.oms"
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index .. call claude 0 "answer-sub.md" "" "" "" ""
) & sub_pid=$!
wait "$root_pid" || fail "root-CWD shell artifact append failed"
wait "$sub_pid" || fail "sub-CWD shell artifact append failed"
[ "$(wc -l < "$shell_repo/.oms/artifacts/index.jsonl" | tr -d ' ')" -eq 2 ] ||
  fail "different-CWD shell appends did not serialize into one ledger"
if ! MSYS2_ARG_CONV_EXCL='*' python3 - "$native_index" <<'PY'
import json, sys

with open(sys.argv[1], encoding="utf-8") as source:
    rows = [json.loads(line) for line in source]
assert sorted(row.get("artifact") for row in rows) == [
    "answer-root.md", "sub/answer-sub.md"
], rows
assert all(len(row.get("artifact_sha256", "")) == 64 for row in rows), rows
PY
then
  fail "relative artifact paths changed at the Git-Bash/Python boundary"
fi
(
  cd "$shell_repo/sub"
  MSYS2_ARG_CONV_EXCL="$shell_repo/.oms" \
    bash "$ROOT/scripts/artifact-index.sh" --repo .. validate >/dev/null
) || fail "sub-CWD artifact-index CLI path did not round-trip through native Python"

# Directory junctions are reparse points and require no developer-mode
# symlink privilege. The native Windows job therefore proves an intermediate
# component is rejected before the outside directory is touched.
python3 - "$ROOT/scripts/lib/artifact-index-store.py" "$TMP" <<'PY'
import contextlib, io, os, runpy, stat, sys
store_path, temp = sys.argv[1:]
if os.name != "nt":
    raise SystemExit(0)
import _winapi

store = runpy.run_path(store_path)

def create_junction(target, link, label):
    # Call the same Win32 reparse primitive used by CPython instead of passing
    # a built-in command through cmd.exe's second quoting/parser layer. The
    # helper enables the required restore privilege, creates the link entry,
    # and raises the exact Win32 error if the fixture cannot be established.
    target = os.path.normpath(os.path.abspath(target))
    link = os.path.normpath(os.path.abspath(link))
    try:
        _winapi.CreateJunction(target, link)
    except OSError as exc:
        try:
            os.rmdir(link)
        except OSError:
            pass
        raise AssertionError("%s: %s" % (label, exc))
    info = os.lstat(link)
    assert info.st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT, label

repo = os.path.join(temp, "junction-repo")
outside = os.path.join(temp, "junction-outside")
assert os.path.isabs(repo) and os.path.isabs(outside), (repo, outside)
os.makedirs(repo)
os.makedirs(outside)
marker = os.path.join(outside, "sentinel.txt")
with open(marker, "wb") as handle:
    handle.write(b"junction target survives\n")
link = os.path.join(repo, ".oms")
create_junction(outside, link, "native Windows fixture could not create a directory junction")
try:
    try:
        with contextlib.redirect_stderr(io.StringIO()):
            store["canonical_index"](
                repo, os.path.join(link, "artifacts", "index.jsonl"))
    except SystemExit:
        pass
    else:
        raise AssertionError("intermediate directory junction was accepted")
    assert open(marker, "rb").read() == b"junction target survives\n"
    assert os.listdir(outside) == ["sentinel.txt"], os.listdir(outside)
finally:
    os.rmdir(link)

# The salvage-specific child directory is independently protected. A junction
# planted at quarantine cannot redirect the exact raw snapshot outside.
salvage_repo = os.path.join(temp, "junction-salvage-repo")
salvage_outside = os.path.join(temp, "junction-salvage-outside")
os.makedirs(os.path.join(salvage_repo, ".oms", "artifacts"))
os.makedirs(salvage_outside)
salvage_marker = os.path.join(salvage_outside, "sentinel.txt")
with open(salvage_marker, "wb") as handle:
    handle.write(b"salvage junction target survives\n")
salvage_index = os.path.join(salvage_repo, ".oms", "artifacts", "index.jsonl")
with open(salvage_index, "wb") as handle:
    handle.write(b'{"schema":0,"event_id":"evt_junction","kind":"legacy"}\n')
    handle.write(b"broken\n")
salvage_link = os.path.join(salvage_repo, ".oms", "artifacts", "quarantine")
create_junction(
    salvage_outside, salvage_link,
    "native Windows fixture could not create salvage junction")
before = open(salvage_index, "rb").read()
try:
    try:
        with contextlib.redirect_stderr(io.StringIO()):
            store["salvage_index"](
                salvage_repo, salvage_index, apply=True)
    except SystemExit:
        pass
    else:
        raise AssertionError("salvage quarantine junction was accepted")
    assert open(salvage_index, "rb").read() == before
    assert open(salvage_marker, "rb").read() == b"salvage junction target survives\n"
    assert os.listdir(salvage_outside) == ["sentinel.txt"], os.listdir(salvage_outside)
finally:
    os.rmdir(salvage_link)
PY

outside="$TMP/outside.jsonl"
printf 'outside untouched\n' > "$outside"
rm -f "$repo/.oms/artifacts/index.jsonl"
if ln -s "$outside" "$repo/.oms/artifacts/index.jsonl" 2>/dev/null; then
  if python3 "$ROOT/scripts/lib/artifact-index-store.py" canonical \
      --repo "$repo" --index "$repo/.oms/artifacts/index.jsonl" >/dev/null 2>&1; then
    fail "artifact store must refuse a leaf symlink"
  fi
  grep -Fxq 'outside untouched' "$outside" ||
    fail "artifact store followed a leaf symlink"
fi

echo "windows-durable-writer-smoke: ok"
