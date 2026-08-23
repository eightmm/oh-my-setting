#!/usr/bin/env bash
set -euo pipefail

# Durable-writers boundary contract: every path that persists text into .oms
# state (or refuses to) is exercised with the same two sentinels — a
# secret-shaped value that must ALWAYS be refused or absent from the artifact,
# and an absolute home path that must either be normalized to ~ (repo-local
# git-ignored sinks) or refused (the strict shared-memory store). A new
# durable writer belongs in this table; a writer that bypasses the scrubber
# should fail here, not in production. goal-drive's own sinks (progress rows,
# commit subjects) are covered where cheap; its full delegate loop lives in
# tests/autonomy-plan-run-smoke.sh.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-durable-writers.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

export HOME="$TMP/home" TMPDIR="$TMP/tmp"
export NVM_DIR="$HOME/.nvm"
mkdir -p "$HOME" "$TMPDIR"
unset OMS_ARTIFACT_INDEX
export OMS_ARTIFACT_INDEX_KEEP=1000
export OMS_ARTIFACT_INDEX_HIGH_WATER=1200
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com

# Sentinels are assembled at runtime so this file itself stays scrubber-clean.
LEAK="AK""IAIOSFODNN7EXAMPLE"
MPATH="/ho""me/sentinel-user/proj/data.txt"

mk_repo() {
  local r="$1"
  mkdir -p "$r"
  git -C "$r" init -q
  git -C "$r" commit -q --allow-empty -m seed
}

# --- fail-ledger: secret refused, home path normalized --------------------
repo="$TMP/ledger"; mk_repo "$repo"
if ( cd "$repo" && bash "$ROOT/scripts/fail-ledger.sh" record \
    --cmd "deploy with $LEAK" --exit 1 >/dev/null 2>&1 ); then
  fail "fail-ledger must refuse a secret-bearing command"
fi
[ ! -s "$repo/.oms/failures.jsonl" ] || fail "refused ledger row must not persist"
( cd "$repo" && bash "$ROOT/scripts/fail-ledger.sh" record \
    --cmd "python $MPATH --epochs 1" --exit 1 >/dev/null 2>&1 ) ||
  fail "fail-ledger must accept a path-bearing command"
grep -Fq 'python ~/proj/data.txt' "$repo/.oms/failures.jsonl" ||
  fail "ledger row must normalize the home path"
if grep -Fq "$MPATH" "$repo/.oms/failures.jsonl"; then
  fail "raw home path leaked into the ledger"
fi

# --- session-handoff: secret refused, home path normalized ----------------
ch="$TMP/claude-home"
proj="$ch/projects/-proj-contract"
mkdir -p "$proj"
printf '{"type":"user","message":{"role":"user","content":"use %s"}}\n' "$LEAK" \
  > "$proj/s1.jsonl"
if OMS_CLAUDE_HOME="$ch" bash "$ROOT/scripts/session-handoff.sh" capture \
    --agent claude --cwd /proj/contract --out "$TMP/d1.md" >/dev/null 2>&1; then
  fail "handoff capture must refuse a secret-bearing transcript"
fi
[ ! -f "$TMP/d1.md" ] || fail "refused digest must not persist"
{ printf '{"type":"user","message":{"role":"user","content":"fix %s"}}\n' "$MPATH"
  printf '{"type":"user","message":{"role":"user","content":"then run the tests"}}\n'
} > "$proj/s1.jsonl"
OMS_CLAUDE_HOME="$ch" bash "$ROOT/scripts/session-handoff.sh" capture \
    --agent claude --cwd /proj/contract --out "$TMP/d2.md" >/dev/null 2>&1 ||
  fail "handoff capture must accept a path-bearing transcript"
grep -Fq 'fix ~/proj/data.txt' "$TMP/d2.md" || fail "digest must normalize the home path"
if grep -Fq "$MPATH" "$TMP/d2.md"; then
  fail "raw home path leaked into the digest body"
fi

# --- agent-memory append: the shared store stays strict on both tiers -----
repo="$TMP/memory"; mk_repo "$repo"
if ( cd "$repo" && bash "$ROOT/scripts/agent-memory.sh" append --agent t \
    --text "token is $LEAK" >/dev/null 2>&1 ); then
  fail "memory append must refuse a secret"
fi
if ( cd "$repo" && bash "$ROOT/scripts/agent-memory.sh" append --agent t \
    --text "see $MPATH" >/dev/null 2>&1 ); then
  fail "the shared memory store keeps the strict tier: home paths refuse"
fi
if [ -f "$repo/.oms/memory/shared.md" ] &&
  grep -Eq "$LEAK|sentinel-user" "$repo/.oms/memory/shared.md"; then
  fail "sentinel leaked into shared memory"
fi

# --- journal distill: a refused lesson skips, never crashes ---------------
repo="$TMP/distill"; mk_repo "$repo"
python3 - "$ROOT/scripts/lib" "$repo" "$LEAK" <<'PY'
import sys
sys.path.insert(0, sys.argv[1])
import work_journal as wj
store = wj.JournalStore(sys.argv[2], timezone_name="UTC")
for source_id, occurred_at in (
    ("b1", "2026-07-30T02:00:00Z"),
    ("b2", "2026-08-01T02:00:00Z"),
):
    store.record_event({
        "event_type": "agent_state",
        "occurred_at": occurred_at,
        "source": {"type": "fixture", "id": source_id},
        "verification_status": "not_verified",
        "blocker": "deploy blocked by %s rotation" % sys.argv[3],
    })
PY
out="$(bash "$ROOT/scripts/journal.sh" distill --repo "$repo" 2>&1)" ||
  fail "distill must not crash on a scrubber-refused lesson: $out"
printf '%s\n' "$out" | grep -Fq 'refused by the memory writer' ||
  fail "distill must report the skipped lesson: $out"
if [ -f "$repo/.oms/memory/shared.md" ] &&
  grep -Fq "$LEAK" "$repo/.oms/memory/shared.md"; then
  fail "secret leaked through distill into shared memory"
fi
second="$(bash "$ROOT/scripts/journal.sh" distill --repo "$repo" 2>&1)"
printf '%s\n' "$second" | grep -Fq 'nothing to promote' ||
  fail "distill marker must advance past the refused lesson: $second"

# --- agent-plan: secret in goal/accept text refuses; runtime acceptance ---
# output never lands raw in receipts (digest only).
repo="$TMP/accept"; mk_repo "$repo"
if ( cd "$repo" && bash "$ROOT/scripts/agent-plan.sh" init --goal g \
    --accept "curl -H 'x: $LEAK' https://x" >/dev/null 2>&1 ); then
  fail "init must refuse a secret-bearing acceptance command"
fi
[ ! -f "$repo/.oms/plan/tasks.json" ] || fail "refused plan must not persist"
# The runtime vector: the command text scans clean but PRINTS a secret.
( cd "$repo" && bash "$ROOT/scripts/agent-plan.sh" init --goal g \
    --accept "printf 'AK%s\n' IAIOSFODNN7EXAMPLE; false" >/dev/null )
if ( cd "$repo" && bash "$ROOT/scripts/agent-plan.sh" accept >/dev/null 2>&1 ); then
  fail "failing acceptance must exit nonzero"
fi
grep -Fq '"kind": "acceptance"' "$repo/.oms/plan/progress.jsonl" ||
  fail "acceptance receipt missing"
if grep -Fq "$LEAK" "$repo/.oms/plan/progress.jsonl"; then
  fail "acceptance output leaked raw into progress receipts"
fi

# --- artifact index: labeled durable append refuses a leaf symlink --------
artifact_repo="$TMP/artifact-index"; mk_repo "$artifact_repo"
mkdir -p "$artifact_repo/.oms/artifacts"
outside_index="$TMP/outside-index.jsonl"
printf 'outside untouched\n' > "$outside_index"
ln -s "$outside_index" "$artifact_repo/.oms/artifacts/index.jsonl"
if artifact_error="$({
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$artifact_repo" call codex 0 "" "" "" "" ""
} 2>&1)"; then
  fail "artifact index append must refuse a leaf symlink"
fi
printf '%s\n' "$artifact_error" | grep -Fq 'artifact index' ||
  fail "artifact index refusal must name its durable-writer label: $artifact_error"
grep -Fxq 'outside untouched' "$outside_index" ||
  fail "artifact index append followed a planted leaf symlink"
[ -L "$artifact_repo/.oms/artifacts/index.jsonl" ] ||
  fail "refused artifact index append must preserve the leaf symlink"

rm -f "$artifact_repo/.oms/artifacts/index.jsonl"
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$artifact_repo" call codex 0 "" "" "" "" ""
) || fail "artifact index must accept a normal durable append"
python3 - "$artifact_repo/.oms/artifacts/index.jsonl" <<'PY' ||
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    rows = [json.loads(line) for line in source]
assert len(rows) == 1, rows
assert rows[0]["kind"] == "call", rows
PY
  fail "one artifact index append must persist exactly one valid row"

# Retention is a store invariant, not an undocumented opt-out switch. Invalid
# bounds fail before mutation and leave an existing receipt byte-identical.
cp "$artifact_repo/.oms/artifacts/index.jsonl" "$TMP/invalid-retention-before.jsonl"
if (
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  OMS_ARTIFACT_INDEX_KEEP=0 OMS_ARTIFACT_INDEX_HIGH_WATER=1200 \
    ma_append_artifact_index "$artifact_repo" call claude 0 "" "" "" "" ""
) >/dev/null 2>&1; then
  fail "zero artifact retention keep must be rejected"
fi
cmp -s "$TMP/invalid-retention-before.jsonl" \
  "$artifact_repo/.oms/artifacts/index.jsonl" ||
  fail "invalid zero retention changed the artifact index"
if (
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  OMS_ARTIFACT_INDEX_KEEP=10 OMS_ARTIFACT_INDEX_HIGH_WATER=9 \
    ma_append_artifact_index "$artifact_repo" call claude 0 "" "" "" "" ""
) >/dev/null 2>&1; then
  fail "artifact high-water below keep must be rejected"
fi
cmp -s "$TMP/invalid-retention-before.jsonl" \
  "$artifact_repo/.oms/artifacts/index.jsonl" ||
  fail "invalid inverted retention changed the artifact index"

# Validation and mutation must agree on structural corruption. A blank row or
# a valid-looking JSON object without its final newline is not a healthy JSONL
# ledger and must never be reported as one.
structural_repo="$TMP/artifact-structural-invalid"; mk_repo "$structural_repo"
mkdir -p "$structural_repo/.oms/artifacts"
printf '%s\n\n' \
  '{"schema":1,"event_id":"evt_valid","operation_id":"op_valid","artifact_id":"sha256:valid","ts":"2026-08-23T00:00:00Z","kind":"call","provider":"codex","exit":0}' \
  > "$structural_repo/.oms/artifacts/index.jsonl"
if bash "$ROOT/scripts/artifact-index.sh" --repo "$structural_repo" validate \
    >/dev/null 2>&1; then
  fail "validate accepted a blank artifact-index row"
fi
printf '%s' \
  '{"schema":1,"event_id":"evt_partial","operation_id":"op_partial","artifact_id":"sha256:partial","ts":"2026-08-23T00:00:00Z","kind":"call","provider":"codex","exit":0}' \
  > "$structural_repo/.oms/artifacts/index.jsonl"
if bash "$ROOT/scripts/artifact-index.sh" --repo "$structural_repo" validate \
    >/dev/null 2>&1; then
  fail "validate accepted a partial final artifact-index row"
fi
python3 - "$structural_repo/.oms/artifacts/index.jsonl" <<'PY'
import sys
with open(sys.argv[1], "wb") as handle:
    handle.write(
        b'{"schema":1,"event_id":"evt_utf8","operation_id":"op_utf8",'
        b'"artifact_id":"sha256:utf8","ts":"2026-08-23T00:00:00Z",'
        b'"kind":"call","provider":"codex","exit":0,"note":"\xff"}\n')
PY
if bash "$ROOT/scripts/artifact-index.sh" --repo "$structural_repo" validate \
    >/dev/null 2>&1; then
  fail "validate accepted invalid UTF-8 in the artifact index"
fi

# Invalid event ids are ordinary validation findings, not hash-table inputs
# that may raise a Python traceback. Cover unhashable, numeric, and empty ids.
python3 - "$structural_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
base = {
    "schema": 1, "operation_id": "op_invalid_event",
    "artifact_id": "sha256:" + "e" * 64,
    "ts": "2026-08-24T00:00:00Z", "kind": "call",
    "provider": "codex", "exit": 0,
}
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as handle:
    for event_id in ([], 7, ""):
        handle.write(json.dumps(dict(base, event_id=event_id)) + "\n")
PY
cp "$structural_repo/.oms/artifacts/index.jsonl" "$TMP/invalid-event-ids-before"
invalid_event_out="$(bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$structural_repo" validate 2>&1)" &&
  fail "validate accepted invalid event ids"
printf '%s\n' "$invalid_event_out" | grep -Fq 'Traceback' &&
  fail "validate raised a traceback for an invalid event id" || true
[ "$(printf '%s\n' "$invalid_event_out" | \
  grep -Fc 'invalid event_id')" -eq 3 ] ||
  fail "validate did not report every invalid event id: $invalid_event_out"
cmp -s "$TMP/invalid-event-ids-before" \
  "$structural_repo/.oms/artifacts/index.jsonl" ||
  fail "validate changed the invalid-event-id fixture"

# LF is the only JSONL row separator. CRLF is accepted as one line ending,
# but a bare CR cannot split two objects, and Python's non-standard NaN and
# Infinity tokens are not JSON. Every normal view/writer and validate must
# agree; salvage alone may drop the corrupt physical LF row with an exact count.
for corruption in bare-cr nonfinite-nan nonfinite-infinity oversize-row; do
  strict_repo="$TMP/artifact-strict-$corruption"; mk_repo "$strict_repo"
  mkdir -p "$strict_repo/.oms/artifacts"
  python3 - "$strict_repo/.oms/artifacts/index.jsonl" "$corruption" <<'PY'
import json, sys
path, corruption = sys.argv[1:]
base = {
    "schema": 1, "operation_id": "op_strict",
    "artifact_id": "sha256:" + "c" * 64,
    "ts": "2026-08-24T00:00:00Z", "kind": "call",
    "provider": "codex", "exit": 0,
}
valid = dict(base, event_id="evt_strict_valid")
bad_one = dict(base, event_id="evt_strict_bad_one")
bad_two = dict(base, event_id="evt_strict_bad_two")
with open(path, "wb") as handle:
    if corruption == "bare-cr":
        handle.write(json.dumps(bad_one, separators=(",", ":")).encode())
        handle.write(b"\r")
        handle.write(json.dumps(bad_two, separators=(",", ":")).encode())
        handle.write(b"\n")
    elif corruption.startswith("nonfinite-"):
        literal = b"NaN" if corruption == "nonfinite-nan" else b"Infinity"
        prefix = json.dumps(bad_one, separators=(",", ":"))[:-1].encode()
        handle.write(prefix + b',"metric":' + literal + b"}\n")
    else:
        # One physical LF row exceeds the store's 1 MiB row contract while
        # staying far below salvage's bounded 256 MiB raw-snapshot ceiling.
        handle.write(b'{"event_id":"evt_oversize","padding":"')
        handle.write(b"x" * (1024 * 1024))
        handle.write(b'"}\n')
    # A real CRLF row must survive recovery byte-for-byte.
    handle.write(json.dumps(valid, separators=(",", ":")).encode() + b"\r\n")
PY
  cp "$strict_repo/.oms/artifacts/index.jsonl" "$TMP/$corruption-raw"
  for strict_view in list telemetry validate; do
    if bash "$ROOT/scripts/artifact-index.sh" --repo "$strict_repo" \
        "$strict_view" >/dev/null 2>&1; then
      fail "$strict_view accepted $corruption artifact-index corruption"
    fi
    cmp -s "$TMP/$corruption-raw" \
      "$strict_repo/.oms/artifacts/index.jsonl" ||
      fail "refused $strict_view changed the $corruption index"
  done
  if (
    # shellcheck source=scripts/lib/peer-common.sh
    . "$ROOT/scripts/lib/peer-common.sh"
    ma_append_artifact_index "$strict_repo" call codex 0 "" "" "" "" ""
  ) >/dev/null 2>&1; then
    fail "artifact mutation accepted $corruption corruption"
  fi
  cmp -s "$TMP/$corruption-raw" \
    "$strict_repo/.oms/artifacts/index.jsonl" ||
    fail "refused mutation changed the $corruption index"

  strict_plan="$(bash "$ROOT/scripts/artifact-index.sh" \
    --repo "$strict_repo" salvage 2>&1)" ||
    fail "salvage could not plan $corruption recovery: $strict_plan"
  printf '%s\n' "$strict_plan" | grep -Fq 'recovered=1 dropped=1' ||
    fail "salvage miscounted $corruption recovery: $strict_plan"
  cmp -s "$TMP/$corruption-raw" \
    "$strict_repo/.oms/artifacts/index.jsonl" ||
    fail "salvage plan changed the $corruption index"
  bash "$ROOT/scripts/artifact-index.sh" --repo "$strict_repo" \
    salvage --apply >/dev/null ||
    fail "salvage could not apply $corruption recovery"
  python3 - "$strict_repo" "$TMP/$corruption-raw" <<'PY' ||
import hashlib, json, pathlib, sys
repo = pathlib.Path(sys.argv[1])
raw = pathlib.Path(sys.argv[2]).read_bytes()
digest = hashlib.sha256(raw).hexdigest()
quarantine = (repo / ".oms" / "artifacts" / "quarantine" /
              ("artifact-index-%s.raw" % digest))
assert quarantine.read_bytes() == raw
body = (repo / ".oms" / "artifacts" / "index.jsonl").read_bytes()
first, receipt_line = body.split(b"\n")[:2]
assert first.endswith(b"\r"), body[:200]
valid = json.loads(first[:-1].decode("utf-8"))
receipt = json.loads(receipt_line.decode("utf-8"))
assert valid["event_id"] == "evt_strict_valid", valid
assert receipt["kind"] == "artifact-index-salvage", receipt
assert receipt["recovered_rows"] == 1, receipt
assert receipt["dropped_rows"] == 1, receipt
PY
    fail "salvage did not loss-account $corruption recovery"
  bash "$ROOT/scripts/artifact-index.sh" --repo "$strict_repo" validate \
    >/dev/null || fail "repaired $corruption index did not validate"
done

# --- artifact index salvage: explicit, loss-accounted, and reversible -----
# Normal readers and writers stay strict. The agent-only recovery command
# plans by default; --apply first quarantines the exact raw bytes, then
# publishes only complete JSON-object rows plus one typed recovery receipt.
salvage_repo="$TMP/artifact-salvage"; mk_repo "$salvage_repo"
mkdir -p "$salvage_repo/.oms/artifacts" "$salvage_repo/.oms/plan"
printf '{"progress":"untouched"}\n' > "$salvage_repo/.oms/plan/progress.jsonl"
python3 - "$salvage_repo/.oms/artifacts/index.jsonl" "$LEAK" <<'PY'
import json, sys
path, leak = sys.argv[1:]
good = {
    "schema": 1, "event_id": "evt_salvage_good", "operation_id": "op_good",
    "artifact_id": "sha256:" + "a" * 64, "ts": "2026-08-23T00:00:00Z",
    "kind": "call", "provider": "codex", "exit": 0,
}
final = dict(good, event_id="evt_salvage_final", operation_id="op_final")
legacy = {"schema": 0, "event_id": "evt_salvage_legacy", "kind": "legacy"}
oversized = b'{"padding":"' + b'x' * (1024 * 1024) + b'"}\n'
with open(path, "wb") as handle:
    handle.write((json.dumps(good, separators=(",", ":")) + "\n").encode())
    handle.write(b"\n")
    handle.write(b'{"payload":"' + leak.encode() + b'\xff"}\n')
    handle.write(b'["scalar"]\n')
    handle.write(b'{"nul":"x\x00y"}\n')
    handle.write((json.dumps(legacy, separators=(",", ":")) + "\n").encode())
    handle.write(oversized)
    handle.write(b"not-json\n")
    handle.write(json.dumps(final, separators=(",", ":")).encode())
PY
cp "$salvage_repo/.oms/artifacts/index.jsonl" "$TMP/salvage-raw-before"
cp "$salvage_repo/.oms/plan/progress.jsonl" "$TMP/salvage-progress-before"
if bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_repo" list \
    >/dev/null 2>&1; then
  fail "normal artifact-index views must refuse structural corruption"
fi
if bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_repo" telemetry \
    >/dev/null 2>&1; then
  fail "artifact telemetry must not derive a partial view from structural corruption"
fi
if (
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$salvage_repo" call codex 0 "" "" "" "" ""
) >/dev/null 2>&1; then
  fail "normal artifact-index mutation must refuse structural corruption"
fi
cmp -s "$TMP/salvage-raw-before" \
  "$salvage_repo/.oms/artifacts/index.jsonl" ||
  fail "a refused normal writer changed the corrupt index"

salvage_plan="$(bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$salvage_repo" salvage 2>&1)" ||
  fail "artifact salvage plan failed: $salvage_plan"
printf '%s\n' "$salvage_plan" | grep -Fq 'plan only' ||
  fail "salvage default did not identify itself as plan-only: $salvage_plan"
printf '%s\n' "$salvage_plan" | grep -Fq "$LEAK" &&
  fail "salvage plan printed corrupt secret-shaped content" || true
cmp -s "$TMP/salvage-raw-before" \
  "$salvage_repo/.oms/artifacts/index.jsonl" ||
  fail "salvage plan changed the index"
[ ! -e "$salvage_repo/.oms/artifacts/quarantine" ] ||
  fail "salvage plan created a quarantine directory"
cmp -s "$TMP/salvage-progress-before" \
  "$salvage_repo/.oms/plan/progress.jsonl" ||
  fail "salvage plan changed progress.jsonl"

salvage_digest="$(python3 - "$TMP/salvage-raw-before" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
salvage_rel=".oms/artifacts/quarantine/artifact-index-$salvage_digest.raw"
salvage_out="$(bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$salvage_repo" salvage --apply 2>&1)" ||
  fail "artifact salvage apply failed: $salvage_out"
printf '%s\n' "$salvage_out" | grep -Fq "$LEAK" &&
  fail "salvage apply printed corrupt secret-shaped content" || true
cmp -s "$TMP/salvage-raw-before" "$salvage_repo/$salvage_rel" ||
  fail "salvage quarantine is not an exact raw snapshot"
salvage_mode="$(stat -c '%a' "$salvage_repo/$salvage_rel" 2>/dev/null ||
  stat -f '%Lp' "$salvage_repo/$salvage_rel")"
[ "$salvage_mode" = "600" ] ||
  fail "salvage quarantine mode must be 0600, got $salvage_mode"
python3 - "$salvage_repo/.oms/artifacts/index.jsonl" \
  "$salvage_rel" "$salvage_digest" "$TMP/salvage-raw-before" <<'PY' ||
import json, pathlib, sys
index, relative, digest, raw_path = sys.argv[1:]
raw_size = len(pathlib.Path(raw_path).read_bytes())
body = pathlib.Path(index).read_bytes()
assert body.endswith(b"\n")
rows = [json.loads(line) for line in body.splitlines()]
assert [row.get("event_id") for row in rows[:-1]] == [
    "evt_salvage_good", "evt_salvage_legacy", "evt_salvage_final"
], rows
receipt = rows[-1]
assert receipt["schema"] == 1
assert receipt["kind"] == "artifact-index-salvage"
assert receipt["provider"] == "local" and receipt["exit"] == 0
assert receipt["quarantine"] == relative
assert receipt["quarantine_sha256"] == digest
assert receipt["artifact"] == relative
assert receipt["artifact_id"] == "sha256:" + digest
assert receipt["raw_bytes"] == raw_size
assert receipt["recovered_rows"] == 3
assert receipt["dropped_rows"] == 6
assert receipt["compacted_rows"] == 0
PY
  fail "salvage did not preserve recoverable rows and append one typed receipt"
cp "$salvage_repo/.oms/artifacts/index.jsonl" "$TMP/salvage-repaired-before"
salvage_repeat="$(bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$salvage_repo" salvage --apply 2>&1)" ||
  fail "idempotent salvage rerun failed: $salvage_repeat"
printf '%s\n' "$salvage_repeat" | grep -Fq 'already healthy' ||
  fail "idempotent salvage rerun was not reported as a no-op: $salvage_repeat"
cmp -s "$TMP/salvage-repaired-before" \
  "$salvage_repo/.oms/artifacts/index.jsonl" ||
  fail "salvage rerun changed a repaired index"
[ "$(find "$salvage_repo/.oms/artifacts/quarantine" -type f | wc -l | tr -d ' ')" -eq 1 ] ||
  fail "salvage rerun created another quarantine"
cmp -s "$TMP/salvage-progress-before" \
  "$salvage_repo/.oms/plan/progress.jsonl" ||
  fail "salvage apply changed progress.jsonl"

# A truncated final object is dropped, while the preceding complete row and a
# complete final object without LF (covered above) have distinct outcomes.
partial_salvage_repo="$TMP/artifact-salvage-partial"; mk_repo "$partial_salvage_repo"
mkdir -p "$partial_salvage_repo/.oms/artifacts"
printf '%s\n%s' \
  '{"schema":0,"event_id":"evt_complete","kind":"legacy"}' \
  '{"schema":1,"event_id":"evt_truncated"' \
  > "$partial_salvage_repo/.oms/artifacts/index.jsonl"
bash "$ROOT/scripts/artifact-index.sh" --repo "$partial_salvage_repo" \
  salvage --apply >/dev/null || fail "partial-final salvage failed"
python3 - "$partial_salvage_repo/.oms/artifacts/index.jsonl" <<'PY' ||
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
assert rows[0]["event_id"] == "evt_complete", rows
assert rows[-1]["kind"] == "artifact-index-salvage", rows
assert rows[-1]["recovered_rows"] == 1, rows
assert rows[-1]["dropped_rows"] == 1, rows
PY
  fail "partial-final salvage counts were wrong"

# Crash recovery may find the exact content-addressed quarantine already
# durable. Reuse it; a different file, symlink, hard link, or linked parent at
# that exact name is an integrity collision and must stop before index replace.
seed_salvage_fixture() {
  local repo="$1"
  mk_repo "$repo"
  mkdir -p "$repo/.oms/artifacts"
  printf '%s\n%s\n' '{"schema":0,"event_id":"evt_reuse","kind":"legacy"}' \
    'broken-row' > "$repo/.oms/artifacts/index.jsonl"
}
salvage_reuse_repo="$TMP/artifact-salvage-reuse"
seed_salvage_fixture "$salvage_reuse_repo"
reuse_digest="$(python3 - "$salvage_reuse_repo/.oms/artifacts/index.jsonl" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
reuse_q="$salvage_reuse_repo/.oms/artifacts/quarantine/artifact-index-$reuse_digest.raw"
mkdir -p "$(dirname "$reuse_q")"
cp "$salvage_reuse_repo/.oms/artifacts/index.jsonl" "$reuse_q"
chmod 600 "$reuse_q"
bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_reuse_repo" \
  salvage --apply >/dev/null || fail "exact quarantine crash recovery failed"
[ "$(find "$(dirname "$reuse_q")" -type f | wc -l | tr -d ' ')" -eq 1 ] ||
  fail "exact quarantine reuse created a second file"

for collision in different symlink hardlink parent-symlink; do
  collision_repo="$TMP/artifact-salvage-$collision"
  seed_salvage_fixture "$collision_repo"
  cp "$collision_repo/.oms/artifacts/index.jsonl" "$TMP/$collision-before"
  collision_digest="$(python3 - "$collision_repo/.oms/artifacts/index.jsonl" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
  collision_q="$collision_repo/.oms/artifacts/quarantine/artifact-index-$collision_digest.raw"
  collision_outside="$TMP/salvage-$collision-outside"
  printf 'outside untouched\n' > "$collision_outside"
  if [ "$collision" = parent-symlink ]; then
    ln -s "$collision_outside" "$collision_repo/.oms/artifacts/quarantine"
  else
    mkdir -p "$(dirname "$collision_q")"
    case "$collision" in
      different) printf 'different collision\n' > "$collision_q" ;;
      symlink) ln -s "$collision_outside" "$collision_q" ;;
      hardlink) ln "$collision_outside" "$collision_q" ;;
    esac
  fi
  if bash "$ROOT/scripts/artifact-index.sh" --repo "$collision_repo" \
      salvage --apply >/dev/null 2>&1; then
    fail "salvage accepted a $collision quarantine collision"
  fi
  cmp -s "$TMP/$collision-before" \
    "$collision_repo/.oms/artifacts/index.jsonl" ||
    fail "$collision quarantine collision changed the corrupt index"
  grep -Fxq 'outside untouched' "$collision_outside" ||
    fail "$collision quarantine collision changed outside bytes"
done

# The canonical ledger lock makes concurrent recovery one transaction: one
# process repairs; the follower observes a healthy index and writes nothing.
salvage_race_repo="$TMP/artifact-salvage-race"
seed_salvage_fixture "$salvage_race_repo"
bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_race_repo" \
  salvage --apply >"$TMP/salvage-race-1" 2>&1 & salvage_pid_one=$!
bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_race_repo" \
  salvage --apply >"$TMP/salvage-race-2" 2>&1 & salvage_pid_two=$!
salvage_rc_one=0; wait "$salvage_pid_one" || salvage_rc_one=$?
salvage_rc_two=0; wait "$salvage_pid_two" || salvage_rc_two=$?
[ "$salvage_rc_one" -eq 0 ] && [ "$salvage_rc_two" -eq 0 ] ||
  fail "concurrent salvage failed: rc=$salvage_rc_one/$salvage_rc_two"
python3 - "$salvage_race_repo/.oms/artifacts/index.jsonl" <<'PY' ||
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
assert sum(row.get("kind") == "artifact-index-salvage" for row in rows) == 1, rows
PY
  fail "concurrent salvage wrote more than one receipt"
[ "$(find "$salvage_race_repo/.oms/artifacts/quarantine" -type f | wc -l | tr -d ' ')" -eq 1 ] ||
  fail "concurrent salvage wrote more than one quarantine"

# High-water compaction keeps a required salvage receipt and complete backward
# resolution lineage. It cannot retain a resolution while dropping its target.
salvage_lineage_repo="$TMP/artifact-salvage-lineage"; mk_repo "$salvage_lineage_repo"
mkdir -p "$salvage_lineage_repo/.oms/artifacts"
python3 - "$salvage_lineage_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
target = {"schema": 1, "event_id": "evt_salvage_target", "operation_id": "op_s",
          "artifact_id": "sha256:" + "b" * 64, "ts": "2026-08-23T00:00:00Z",
          "kind": "call", "provider": "codex", "exit": 1}
filler = dict(target, event_id="evt_salvage_filler", operation_id="op_f", exit=0)
resolution = dict(target, event_id="evt_salvage_resolution",
                  ts="2026-08-23T00:00:01Z", kind="artifact-resolution", exit=0,
                  parent_event_id="evt_salvage_target",
                  resolves_event_id="evt_salvage_target", resolution="resolved")
with open(sys.argv[1], "wb") as handle:
    for row in (target, filler, resolution):
        handle.write((json.dumps(row, separators=(",", ":")) + "\n").encode())
    handle.write(b"broken\n")
PY
OMS_ARTIFACT_INDEX_KEEP=3 OMS_ARTIFACT_INDEX_HIGH_WATER=3 \
  bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_lineage_repo" \
    salvage --apply >/dev/null || fail "lineage-aware salvage compaction failed"
python3 - "$salvage_lineage_repo/.oms/artifacts/index.jsonl" <<'PY' ||
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
assert [row["event_id"] for row in rows[:-1]] == [
    "evt_salvage_target", "evt_salvage_resolution"
], rows
assert rows[-1]["kind"] == "artifact-index-salvage", rows
assert rows[-1]["compacted_rows"] == 1, rows
PY
  fail "salvage compaction left incomplete resolution lineage"

# Byte pressure is a separate bound from row high-water. Seventeen legal
# near-1MiB objects recover from the 256MiB snapshot, but the repaired ledger
# is compacted below the 16MiB durable ceiling with its receipt retained.
salvage_bytes_repo="$TMP/artifact-salvage-bytes"; mk_repo "$salvage_bytes_repo"
mkdir -p "$salvage_bytes_repo/.oms/artifacts"
python3 - "$salvage_bytes_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
padding = "x" * (1024 * 1024 - 2048)
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as handle:
    for index in range(17):
        handle.write(json.dumps({
            "schema": 1, "event_id": "evt_salvage_byte_%d" % index,
            "operation_id": "op_salvage_byte_%d" % index,
            "artifact_id": "sha256:" + ("%064d" % index),
            "ts": "2026-08-23T00:00:00Z", "kind": "call",
            "provider": "codex", "exit": 0, "padding": padding,
        }, separators=(",", ":")) + "\n")
    handle.write("broken\n")
PY
bash "$ROOT/scripts/artifact-index.sh" --repo "$salvage_bytes_repo" \
  salvage --apply >/dev/null || fail "byte-bounded salvage failed"
python3 - "$salvage_bytes_repo/.oms/artifacts/index.jsonl" <<'PY' ||
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
body = path.read_bytes()
rows = [json.loads(line) for line in body.splitlines()]
assert len(body) <= 16 * 1024 * 1024, len(body)
assert rows[-1]["kind"] == "artifact-index-salvage", rows[-1]
assert rows[-1]["compacted_rows"] > 0, rows[-1]
PY
  fail "byte-bounded salvage did not retain its receipt"

# --- artifact index: an ancestry symlink is the host's layout, not an escape
real_ancestry="$TMP/real-ancestry"
mkdir -p "$real_ancestry"
ln -s "$real_ancestry" "$TMP/linked-ancestry"
ancestry_repo="$TMP/linked-ancestry/repo"
mk_repo "$ancestry_repo"
mkdir -p "$ancestry_repo/.oms/artifacts"
printf 'aliased artifact\n' > "$ancestry_repo/.oms/artifacts/aliased.md"
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$ancestry_repo" call codex 0 \
    "$ancestry_repo/.oms/artifacts/aliased.md" "" "" "" ""
) || fail "artifact index append must accept a repo path behind an ancestry symlink"
python3 - "$real_ancestry/repo/.oms/artifacts/index.jsonl" <<'PY' ||
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    rows = [json.loads(line) for line in source]
assert len(rows) == 1, rows
assert rows[0]["kind"] == "call", rows
assert rows[0]["provider"] == "codex", rows
assert rows[0]["artifact"] == ".oms/artifacts/aliased.md", rows
assert ".." not in rows[0]["artifact"].split("/"), rows
PY
  fail "the ancestry-symlink append must persist exactly one valid row in the physical ledger"

# --- artifact index: a hard-linked ledger inode refuses before mutation ---
hard_repo="$TMP/artifact-hardlink"; mk_repo "$hard_repo"
mkdir -p "$hard_repo/.oms/artifacts"
outside_hard="$TMP/outside-hardlink.jsonl"
printf 'outside hard untouched\n' > "$outside_hard"
ln "$outside_hard" "$hard_repo/.oms/artifacts/index.jsonl"
if hard_error="$({
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$hard_repo" call codex 0 "" "" "" "" ""
} 2>&1)"; then
  fail "artifact index append must refuse a hard-linked ledger"
fi
printf '%s\n' "$hard_error" | grep -Fq 'artifact index' ||
  fail "hard-link refusal must name its durable-writer label: $hard_error"
grep -Fxq 'outside hard untouched' "$outside_hard" ||
  fail "artifact index append mutated a hard-linked outside file"
[ "$(wc -l < "$outside_hard")" -eq 1 ] ||
  fail "hard-link refusal must leave the outside file byte-identical"

# A static component check is not enough: if artifacts/ becomes an
# intermediate symlink between canonicalization and open, a pathname reader
# can ingest an outside ledger and later quarantine it inside the repo. Force
# that exact boundary deterministically by swapping at the first open.
python3 - "$ROOT/scripts/lib/durable-jsonl.py" "$TMP" <<'PY' ||
import contextlib, io, os, runpy, shutil, sys
durable_path, temp = sys.argv[1:]
durable = runpy.run_path(durable_path)
repo = os.path.join(temp, "artifact-read-component-race")
parent = os.path.join(repo, ".oms", "artifacts")
parked = parent + ".parked"
outside = os.path.join(temp, "artifact-read-component-outside")
target = os.path.join(parent, "index.jsonl")
os.makedirs(parent)
os.makedirs(outside)
with open(target, "wb") as handle:
    handle.write(b"inside ledger\n")
outside_target = os.path.join(outside, "index.jsonl")
with open(outside_target, "wb") as handle:
    handle.write(b"outside secret must never be read\n")

real_open = os.open
fired = [False]
def racing_open(path, flags, *args, **kwargs):
    absolute = os.path.abspath(path) if isinstance(path, (str, bytes)) else path
    if (not fired[0] and kwargs.get("dir_fd") is None and
            absolute in (os.path.abspath(parent), os.path.abspath(target))):
        fired[0] = True
        os.rename(parent, parked)
        os.symlink(outside, parent)
    return real_open(path, flags, *args, **kwargs)

os.open = racing_open
try:
    try:
        with contextlib.redirect_stderr(io.StringIO()):
            durable["read_no_follow"](repo, target, "artifact race")
    except SystemExit:
        pass
    else:
        raise AssertionError("intermediate component swap escaped read_no_follow")
finally:
    os.open = real_open
    if os.path.islink(parent):
        os.unlink(parent)
    if os.path.isdir(parked):
        os.rename(parked, parent)
assert fired[0], "race hook did not reach the open boundary"
assert open(outside_target, "rb").read() == b"outside secret must never be read\n"
PY
  fail "artifact raw reader followed a raced intermediate symlink"

# --- artifact index: every mutation shares one repo-bound store ------------
# A linked .oms component is more dangerous than a linked leaf: the old
# provider path created both .gitignore and the ledger in the outside tree
# before the durable leaf check got a chance to run.
component_repo="$TMP/artifact-component"; mk_repo "$component_repo"
outside_component="$TMP/outside-component"; mkdir -p "$outside_component"
ln -s "$outside_component" "$component_repo/.oms"
if component_error="$({
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$component_repo" call codex 0 "" "" "" "" ""
} 2>&1)"; then
  fail "artifact index append must refuse a symlinked .oms component"
fi
printf '%s\n' "$component_error" | grep -Fq 'artifact index' ||
  fail "component refusal must identify the artifact index: $component_error"
[ -z "$(ls -A "$outside_component" 2>/dev/null)" ] ||
  fail "artifact index setup escaped through a symlinked .oms component"

# Hand and mechanical resolvers used separate plain append paths. A planted
# leaf must be refused by all of them before the outside ledger changes.
for resolver in resolve resolve-superseded resolve-recovered; do
  resolver_repo="$TMP/artifact-$resolver"; mk_repo "$resolver_repo"
  mkdir -p "$resolver_repo/.oms/artifacts"
  resolver_outside="$TMP/outside-$resolver.jsonl"
  python3 - "$resolver_outside" "$resolver" <<'PY'
import json, sys
path, action = sys.argv[1:]
rows = [{
    "schema": 1,
    "event_id": "evt_failed",
    "operation_id": "op_fixture",
    "artifact_id": "sha256:" + "a" * 64,
    "ts": "2026-08-23T00:00:00Z",
    "kind": "call" if action == "resolve-recovered" else "patch-admit",
    "provider": "codex",
    "exit": 124 if action == "resolve-recovered" else 1,
    "patch_sha256": "b" * 64,
}]
if action in ("resolve-superseded", "resolve-recovered"):
    rows.append(dict(rows[0], event_id="evt_winner", exit=0,
                     ts="2026-08-23T00:00:01Z"))
with open(path, "w", encoding="utf-8", newline="\n") as handle:
    for row in rows:
        handle.write(json.dumps(row, sort_keys=True) + "\n")
PY
  resolver_before="$(python3 - "$resolver_outside" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
  ln -s "$resolver_outside" "$resolver_repo/.oms/artifacts/index.jsonl"
  set -- --repo "$resolver_repo" "$resolver"
  [ "$resolver" != resolve ] || set -- --repo "$resolver_repo" resolve --event-id evt_failed
  if bash "$ROOT/scripts/artifact-index.sh" "$@" >/dev/null 2>&1; then
    fail "$resolver must refuse a symlinked artifact index"
  fi
  resolver_after="$(python3 - "$resolver_outside" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
  [ "$resolver_before" = "$resolver_after" ] ||
    fail "$resolver mutated an outside symlink target"
done

# Whole-file rewrites must reject a hard-linked ledger even though replacing
# the in-repo name would leave the twin bytes intact. A shared inode is not an
# artifact-store-owned object and therefore is not a legal mutation target.
for rewriter in migrate prune prune-stale; do
  rewriter_repo="$TMP/artifact-$rewriter"; mk_repo "$rewriter_repo"
  mkdir -p "$rewriter_repo/.oms/artifacts"
  rewriter_outside="$TMP/outside-$rewriter.jsonl"
  printf '{"kind":"legacy","provider":"codex","exit":0,"ts":"2026-08-23T00:00:00Z"}\n' \
    > "$rewriter_outside"
  ln "$rewriter_outside" "$rewriter_repo/.oms/artifacts/index.jsonl"
  rewriter_before="$(stat -c '%d:%i:%h' "$rewriter_repo/.oms/artifacts/index.jsonl" 2>/dev/null ||
    stat -f '%d:%i:%l' "$rewriter_repo/.oms/artifacts/index.jsonl")"
  set -- --repo "$rewriter_repo" "$rewriter"
  [ "$rewriter" != prune ] || set -- --repo "$rewriter_repo" prune 1
  [ "$rewriter" != prune-stale ] || set -- --repo "$rewriter_repo" prune 1 --stale
  if bash "$ROOT/scripts/artifact-index.sh" "$@" >/dev/null 2>&1; then
    fail "$rewriter must refuse a hard-linked artifact index"
  fi
  rewriter_after="$(stat -c '%d:%i:%h' "$rewriter_repo/.oms/artifacts/index.jsonl" 2>/dev/null ||
    stat -f '%d:%i:%l' "$rewriter_repo/.oms/artifacts/index.jsonl")"
  [ "$rewriter_before" = "$rewriter_after" ] ||
    fail "$rewriter replaced a hard-linked artifact index"
done

# Orphan cleanup must skip planted child links while still reaching its real
# delete branch. A repo-bound reference outside .oms/artifacts is legitimate
# evidence but never becomes a cleanup candidate.
orphan_repo="$TMP/artifact-orphan-boundary"; mk_repo "$orphan_repo"
orphan_outside="$TMP/outside-orphan-root"; mkdir -p "$orphan_outside"
mkdir -p "$orphan_repo/.oms/artifacts/sub" "$orphan_repo/custom"
printf 'outside orphan must survive\n' > "$orphan_outside/orphan.md"
printf 'outside linked file survives\n' > "$orphan_outside/linked-file.md"
printf 'outside hard-linked file survives\n' > "$orphan_outside/hard-file.md"
printf 'owned orphan should go\n' > "$orphan_repo/.oms/artifacts/orphan.md"
printf 'normalized reference survives\n' > "$orphan_repo/.oms/artifacts/nested-body.md"
printf 'absolute inside reference survives\n' > "$orphan_repo/.oms/artifacts/absolute-inside.md"
printf 'custom evidence survives\n' > "$orphan_repo/custom/evidence.md"
absolute_ref="$TMP/outside-absolute-reference.md"
traversal_ref="$TMP/outside-traversal-reference.md"
printf 'absolute reference survives\n' > "$absolute_ref"
printf 'traversal reference survives\n' > "$traversal_ref"
touch -t 202001010000 "$orphan_outside/orphan.md" \
  "$orphan_repo/.oms/artifacts/orphan.md" 2>/dev/null || true
ln -s "$orphan_outside" "$orphan_repo/.oms/artifacts/linked"
ln -s "$orphan_outside/linked-file.md" \
  "$orphan_repo/.oms/artifacts/linked-file.md"
ln "$orphan_outside/hard-file.md" \
  "$orphan_repo/.oms/artifacts/hard-file.md"
printf '{"schema":1,"event_id":"evt_keep","operation_id":"op_keep","artifact_id":"sha256:%064d","ts":"2026-08-23T00:00:00Z","kind":"call","provider":"codex","exit":0,"artifact":"custom/evidence.md"}\n' 0 \
  > "$orphan_repo/index.jsonl"
python3 - "$orphan_repo/index.jsonl" "$absolute_ref" \
  "$orphan_repo/.oms/artifacts/absolute-inside.md" <<'PY'
import json, sys
index, absolute, absolute_inside = sys.argv[1:]
rows = [
    {
        "schema": 1, "event_id": "evt_absolute", "operation_id": "op_absolute",
        "artifact_id": "sha256:" + "a" * 64,
        "ts": "2026-08-23T00:00:00Z", "kind": "call",
        "provider": "codex", "exit": 0, "artifact": absolute,
    },
    {
        "schema": 1, "event_id": "evt_traversal", "operation_id": "op_traversal",
        "artifact_id": "sha256:" + "b" * 64,
        "ts": "2026-08-23T00:00:00Z", "kind": "call",
        "provider": "codex", "exit": 0,
        "artifact": "../outside-traversal-reference.md",
    },
    {
        "schema": 1, "event_id": "evt_normalized", "operation_id": "op_normalized",
        "artifact_id": "sha256:" + "c" * 64,
        "ts": "2026-08-23T00:00:00Z", "kind": "call",
        "provider": "codex", "exit": 0,
        "artifact": ".oms/artifacts/sub/../nested-body.md",
    },
    {
        "schema": 1, "event_id": "evt_absolute_inside",
        "operation_id": "op_absolute_inside",
        "artifact_id": "sha256:" + "d" * 64,
        "ts": "2026-08-23T00:00:00Z", "kind": "call",
        "provider": "codex", "exit": 0, "artifact": absolute_inside,
    },
]
with open(index, "a", encoding="utf-8", newline="\n") as handle:
    for row in rows:
        handle.write(json.dumps(row) + "\n")
PY
OMS_ARTIFACT_ORPHAN_GRACE=0 bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$orphan_repo" --file "$orphan_repo/index.jsonl" prune 10 --files \
  >/dev/null || fail "safe orphan cleanup must reach its owned delete branch"
[ ! -e "$orphan_repo/.oms/artifacts/orphan.md" ] ||
  fail "orphan cleanup did not delete an owned orphan"
grep -Fxq 'outside orphan must survive' "$orphan_outside/orphan.md" ||
  fail "orphan cleanup traversed a planted child symlink"
grep -Fxq 'outside linked file survives' "$orphan_outside/linked-file.md" ||
  fail "orphan cleanup followed a planted file symlink"
[ -L "$orphan_repo/.oms/artifacts/linked-file.md" ] ||
  fail "orphan cleanup removed a planted file symlink"
grep -Fxq 'outside hard-linked file survives' "$orphan_outside/hard-file.md" ||
  fail "orphan cleanup mutated a hard-link twin"
[ -f "$orphan_repo/.oms/artifacts/hard-file.md" ] ||
  fail "orphan cleanup removed a hard-linked candidate"
grep -Fxq 'custom evidence survives' "$orphan_repo/custom/evidence.md" ||
  fail "orphan cleanup touched a repo-bound reference outside its owned root"
grep -Fxq 'absolute reference survives' "$absolute_ref" ||
  fail "orphan cleanup touched an absolute external reference"
grep -Fxq 'traversal reference survives' "$traversal_ref" ||
  fail "orphan cleanup touched a traversal reference"
grep -Fxq 'normalized reference survives' \
  "$orphan_repo/.oms/artifacts/nested-body.md" ||
  fail "orphan cleanup deleted an in-root reference containing interior .."
grep -Fxq 'absolute inside reference survives' \
  "$orphan_repo/.oms/artifacts/absolute-inside.md" ||
  fail "orphan cleanup deleted a physical in-root absolute reference"

# A custom ledger does not imply ownership of the default artifact directory.
# If that directory has never existed, --files is a clean zero-orphan no-op.
orphan_empty_repo="$TMP/artifact-orphan-empty"; mk_repo "$orphan_empty_repo"
printf '{"schema":1,"event_id":"evt_keep","operation_id":"op_keep","artifact_id":"sha256:%064d","ts":"2026-08-23T00:00:00Z","kind":"call","provider":"codex","exit":0}\n' 0 \
  > "$orphan_empty_repo/index.jsonl"
bash "$ROOT/scripts/artifact-index.sh" --repo "$orphan_empty_repo" \
  --file "$orphan_empty_repo/index.jsonl" prune 10 --files >/dev/null ||
  fail "missing default artifact root must be a zero-orphan no-op"
[ ! -e "$orphan_empty_repo/.oms" ] ||
  fail "zero-orphan cleanup unexpectedly created the default artifact root"

# A within-keep prune without --files needs no temporary orphan-reference
# body. It remains usable on hosts whose ambient TMPDIR is unavailable.
no_temp_repo="$TMP/artifact-prune-no-temp"; mk_repo "$no_temp_repo"
mkdir -p "$no_temp_repo/.oms/artifacts"
printf '{"schema":1,"event_id":"evt_keep","operation_id":"op_keep","artifact_id":"sha256:%064d","ts":"2026-08-23T00:00:00Z","kind":"call","provider":"codex","exit":0}\n' 0 \
  > "$no_temp_repo/.oms/artifacts/index.jsonl"
TMPDIR="$TMP/does-not-exist" bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$no_temp_repo" prune 10 >/dev/null ||
  fail "within-keep prune without --files must not allocate a temp body"

# Byte retention is a store invariant as well as a row-count invariant. An
# already-over-ceiling but valid JSONL ledger must be compacted so the newest
# row can land; it must not enter a permanent refusal state.
byte_repo="$TMP/artifact-byte-retention"; mk_repo "$byte_repo"
mkdir -p "$byte_repo/.oms/artifacts"
python3 - "$byte_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
padding = "x" * (1024 * 1024 - 512)
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as handle:
    for index in range(17):
        row = {
            "schema": 1, "event_id": "evt_%02d" % index,
            "operation_id": "op_%02d" % index,
            "artifact_id": "sha256:" + ("%064d" % index),
            "ts": "2026-08-23T00:00:00Z", "kind": "call",
            "provider": "codex", "exit": 0, "padding": padding,
        }
        handle.write(json.dumps(row, separators=(",", ":")) + "\n")
PY
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  OMS_ARTIFACT_INDEX_KEEP=20 OMS_ARTIFACT_INDEX_HIGH_WATER=24 \
    ma_append_artifact_index "$byte_repo" call claude 0 "" "" "" "" ""
) || fail "an over-ceiling ledger must compact and accept the newest row"
[ "$(wc -c < "$byte_repo/.oms/artifacts/index.jsonl" | tr -d ' ')" -le 16777216 ] ||
  fail "artifact index exceeded the 16MiB store ceiling"
tail -n 1 "$byte_repo/.oms/artifacts/index.jsonl" | grep -Fq '"provider": "claude"' ||
  fail "byte compaction must preserve the newly appended row"

# A benign alias above the repository is accepted, but it is normalized before
# lock selection. Logical and physical callers therefore serialize on the
# same key and append to one physical ledger.
alias_parent="$TMP/artifact-alias-real"; mkdir -p "$alias_parent"
alias_repo_real="$alias_parent/repo"; mk_repo "$alias_repo_real"
ln -s "$alias_parent" "$TMP/artifact-alias-link"
alias_repo="$TMP/artifact-alias-link/repo"
alias_index="$alias_repo/.oms/artifacts/index.jsonl"
physical_index="$alias_repo_real/.oms/artifacts/index.jsonl"
mkdir -p "$alias_repo/.oms/artifacts"
printf 'alias artifact\n' > "$alias_repo/.oms/artifacts/body.md"
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  alias_key="$(python3 "$ROOT/scripts/lib/artifact-index-store.py" canonical \
    --repo "$alias_repo" --index "$alias_index")"
  physical_key="$(python3 "$ROOT/scripts/lib/artifact-index-store.py" canonical \
    --repo "$alias_repo_real" --index "$physical_index")"
  [ "$alias_key" = "$physical_key" ] ||
    fail "logical and physical index spellings must canonicalize identically"
  [ "$(oms_file_lock_path_for_file "$alias_key")" = \
    "$(oms_file_lock_path_for_file "$physical_key")" ] ||
    fail "logical and physical callers must share one artifact lock"
  ma_append_artifact_index "$alias_repo" call codex 0 \
    "$alias_repo/.oms/artifacts/body.md" "" "" "" ""
  ma_append_artifact_index "$alias_repo_real" call claude 0 "" "" "" "" ""
) || fail "artifact index must accept both benign repo spellings"
[ "$(wc -l < "$physical_index" | tr -d ' ')" -eq 2 ] ||
  fail "logical and physical callers must append to one ledger"
python3 - "$physical_index" <<'PY' ||
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    rows = [json.loads(line) for line in source]
assert rows[0]["artifact"] == ".oms/artifacts/body.md", rows
assert ".." not in rows[0]["artifact"].split("/"), rows
PY
  fail "alias append stored a logical path outside the physical repo"
bash "$ROOT/scripts/artifact-index.sh" --repo "$alias_repo" validate >/dev/null ||
  fail "artifact index written through an ancestry alias must validate"

# Migration follows the same physical relpath rule. A legacy absolute path
# supplied through the benign alias becomes one repo-relative spelling.
migrate_alias_real="$TMP/artifact-migrate-alias-real"; mk_repo "$migrate_alias_real"
ln -s "$migrate_alias_real" "$TMP/artifact-migrate-alias-link"
migrate_alias="$TMP/artifact-migrate-alias-link"
mkdir -p "$migrate_alias/.oms/artifacts"
printf 'legacy alias artifact\n' > "$migrate_alias/.oms/artifacts/body.md"
python3 - "$migrate_alias/.oms/artifacts/index.jsonl" \
  "$migrate_alias/.oms/artifacts/body.md" <<'PY'
import json, sys
index, artifact = sys.argv[1:]
row = {
    "ts": "2026-08-23T00:00:00Z", "kind": "call",
    "provider": "codex", "exit": 0, "artifact": artifact,
}
with open(index, "w", encoding="utf-8", newline="\n") as handle:
    handle.write(json.dumps(row) + "\n")
PY
bash "$ROOT/scripts/artifact-index.sh" --repo "$migrate_alias" migrate >/dev/null ||
  fail "migration through an ancestry alias failed"
python3 - "$migrate_alias_real/.oms/artifacts/index.jsonl" <<'PY' ||
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    row = json.loads(source.readline())
assert row["artifact"] == ".oms/artifacts/body.md", row
assert ".." not in row["artifact"].split("/"), row
PY
  fail "migration stored a logical alias escape path"
bash "$ROOT/scripts/artifact-index.sh" --repo "$migrate_alias_real" validate >/dev/null ||
  fail "artifact index migrated through an ancestry alias must validate"

# First-use setup is serialized by that same index lock. Neither concurrent
# provider may lose its receipt to an `.oms/.gitignore` creation race.
first_repo="$TMP/artifact-concurrent-first"; mk_repo "$first_repo"
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$first_repo" call codex 0 "" "" "" "" ""
) & first_pid=$!
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$first_repo" call claude 0 "" "" "" "" ""
) & second_pid=$!
wait "$first_pid" || fail "first concurrent artifact append failed"
wait "$second_pid" || fail "second concurrent artifact append failed"
[ "$(wc -l < "$first_repo/.oms/artifacts/index.jsonl" | tr -d ' ')" -eq 2 ] ||
  fail "concurrent first appends must preserve both rows"
grep -Fxq '*' "$first_repo/.oms/.gitignore" ||
  fail "concurrent first append must initialize the .oms ignore"

# A pre-existing ignore is user content and remains a no-op even when it is
# larger than the helper's creation bound.
large_ignore_repo="$TMP/artifact-large-ignore"; mk_repo "$large_ignore_repo"
mkdir -p "$large_ignore_repo/.oms"
python3 - "$large_ignore_repo/.oms/.gitignore" <<'PY'
import pathlib, sys
pathlib.Path(sys.argv[1]).write_bytes(b"# existing\n" + b"x" * (1024 * 1024 + 1))
PY
large_ignore_before="$(python3 - "$large_ignore_repo/.oms/.gitignore" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$large_ignore_repo" call codex 0 "" "" "" "" ""
) || fail "an existing large .oms ignore must not block artifact append"
large_ignore_after="$(python3 - "$large_ignore_repo/.oms/.gitignore" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
[ "$large_ignore_before" = "$large_ignore_after" ] ||
  fail "artifact append rewrote an existing .oms ignore"

# The legacy setup helper treated existence, not contents, as ownership. An
# already-present empty ignore therefore remains user-owned and byte-empty.
empty_ignore_repo="$TMP/artifact-empty-ignore"; mk_repo "$empty_ignore_repo"
mkdir -p "$empty_ignore_repo/.oms"
: > "$empty_ignore_repo/.oms/.gitignore"
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$empty_ignore_repo" call codex 0 "" "" "" "" ""
) || fail "an existing empty .oms ignore must not block artifact append"
[ ! -s "$empty_ignore_repo/.oms/.gitignore" ] ||
  fail "artifact append rewrote an existing empty .oms ignore"

# The same central leaf policy covers resolver writes, not only provider rows.
resolve_hard_repo="$TMP/artifact-resolve-hard"; mk_repo "$resolve_hard_repo"
mkdir -p "$resolve_hard_repo/.oms/artifacts"
resolve_hard_outside="$TMP/outside-resolve-hard.jsonl"
printf '{"schema":1,"event_id":"evt_failed","operation_id":"op_fixture","artifact_id":"sha256:%064d","ts":"2026-08-23T00:00:00Z","kind":"call","provider":"codex","exit":1}\n' 0 \
  > "$resolve_hard_outside"
ln "$resolve_hard_outside" "$resolve_hard_repo/.oms/artifacts/index.jsonl"
if bash "$ROOT/scripts/artifact-index.sh" --repo "$resolve_hard_repo" resolve \
    --event-id evt_failed >/dev/null 2>&1; then
  fail "resolve must refuse a hard-linked artifact index"
fi
[ "$(wc -l < "$resolve_hard_outside" | tr -d ' ')" -eq 1 ] ||
  fail "resolve mutated the hard-link twin"

# Copy-on-write does not turn a read-only pending receipt into success merely
# because its parent directory allows rename.
readonly_repo="$TMP/artifact-readonly"; mk_repo "$readonly_repo"
(
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$readonly_repo" call codex 0 "" "" "" "" ""
) || fail "read-only fixture needs an initial artifact row"
readonly_index="$readonly_repo/.oms/artifacts/index.jsonl"
readonly_before="$(python3 - "$readonly_index" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
chmod 0444 "$readonly_index"
if (
  # shellcheck source=scripts/lib/peer-common.sh
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$readonly_repo" call claude 0 "" "" "" "" ""
) >/dev/null 2>&1; then
  fail "a read-only artifact index must refuse a pending receipt"
fi
readonly_after="$(python3 - "$readonly_index" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
[ "$readonly_before" = "$readonly_after" ] ||
  fail "read-only refusal changed the artifact index bytes"
chmod 0600 "$readonly_index"

# `migrate` upgrades rows; it is not an implicit count-prune. A healthy ledger
# may sit between keep and high-water, and every one of those rows must survive
# an idempotent migration while the hard byte ceiling remains enforced.
migrate_count_repo="$TMP/artifact-migrate-count"; mk_repo "$migrate_count_repo"
mkdir -p "$migrate_count_repo/.oms/artifacts"
python3 - "$migrate_count_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as handle:
    for index in range(1100):
        handle.write(json.dumps({
            "schema": 1, "event_id": "evt_live_%04d" % index,
            "operation_id": "op_live_%04d" % index,
            "artifact_id": "sha256:" + ("%064d" % index),
            "ts": "2026-08-23T00:00:00Z", "kind": "call",
            "provider": "codex", "exit": 0,
        }) + "\n")
PY
bash "$ROOT/scripts/artifact-index.sh" --repo "$migrate_count_repo" migrate \
  >/dev/null || fail "idempotent migration of a healthy ledger failed"
[ "$(wc -l < "$migrate_count_repo/.oms/artifacts/index.jsonl" | tr -d ' ')" -eq 1100 ] ||
  fail "migrate silently applied count retention below high-water"
head -n 1 "$migrate_count_repo/.oms/artifacts/index.jsonl" | grep -Fq 'evt_live_0000' ||
  fail "migrate dropped the oldest healthy row below high-water"
python3 - "$migrate_count_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
with open(sys.argv[1], "a", encoding="utf-8", newline="\n") as handle:
    for index in range(1100, 1201):
        handle.write(json.dumps({
            "schema": 1, "event_id": "evt_live_%04d" % index,
            "operation_id": "op_live_%04d" % index,
            "artifact_id": "sha256:" + ("%064d" % index),
            "ts": "2026-08-23T00:00:00Z", "kind": "call",
            "provider": "codex", "exit": 0,
        }) + "\n")
PY
bash "$ROOT/scripts/artifact-index.sh" --repo "$migrate_count_repo" migrate \
  >/dev/null || fail "migration above high-water failed"
[ "$(wc -l < "$migrate_count_repo/.oms/artifacts/index.jsonl" | tr -d ' ')" -eq 1000 ] ||
  fail "migrate did not apply count retention above high-water"
head -n 1 "$migrate_count_repo/.oms/artifacts/index.jsonl" | grep -Fq 'evt_live_0201' ||
  fail "migrate retained the wrong window above high-water"
tail -n 1 "$migrate_count_repo/.oms/artifacts/index.jsonl" | grep -Fq 'evt_live_1200' ||
  fail "migrate dropped the newest row above high-water"

# `prune --stale` is not a hidden count-prune. More than the default 1000 live
# rows survive when the one missing reference is removed.
stale_repo="$TMP/artifact-stale-count"; mk_repo "$stale_repo"
mkdir -p "$stale_repo/.oms/artifacts"
python3 - "$stale_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as handle:
    for index in range(1001):
        handle.write(json.dumps({
            "schema": 1, "event_id": "evt_live_%d" % index,
            "operation_id": "op_live_%d" % index,
            "artifact_id": "sha256:" + ("%064d" % index),
            "ts": "2026-08-23T00:00:00Z", "kind": "call",
            "provider": "codex", "exit": 0,
        }) + "\n")
    handle.write(json.dumps({
        "schema": 1, "event_id": "evt_stale", "operation_id": "op_stale",
        "artifact_id": "sha256:" + "f" * 64,
        "ts": "2026-08-23T00:00:00Z", "kind": "call",
        "provider": "codex", "exit": 0,
        "artifact": ".oms/artifacts/missing.md",
    }) + "\n")
PY
bash "$ROOT/scripts/artifact-index.sh" --repo "$stale_repo" prune --stale \
  >/dev/null || fail "stale repair must accept more than the default keep"
[ "$(wc -l < "$stale_repo/.oms/artifacts/index.jsonl" | tr -d ' ')" -eq 1001 ] ||
  fail "stale repair silently applied count retention to live rows"

# Lineage removal reaches a fixed point before retention accounting. If the
# missing root takes out an inner resolution, an outer resolution of that row
# is stale too; none of those dependency drops may be mislabeled byte pressure.
stale_chain_repo="$TMP/artifact-stale-chain"; mk_repo "$stale_chain_repo"
mkdir -p "$stale_chain_repo/.oms/artifacts"
python3 - "$stale_chain_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
root = {
    "schema": 1, "event_id": "evt_root", "operation_id": "op_chain",
    "artifact_id": "sha256:" + "c" * 64,
    "ts": "2026-08-23T00:00:00Z", "kind": "call",
    "provider": "codex", "exit": 1,
    "artifact": ".oms/artifacts/missing.md",
}
inner = {
    "schema": 1, "event_id": "evt_inner", "operation_id": "op_chain",
    "artifact_id": root["artifact_id"], "ts": "2026-08-23T00:00:01Z",
    "kind": "artifact-resolution", "provider": "codex", "exit": 0,
    "parent_event_id": "evt_root", "resolves_event_id": "evt_root",
    "resolution": "resolved",
}
outer = dict(
    inner, event_id="evt_outer", parent_event_id="evt_inner",
    resolves_event_id="evt_inner", ts="2026-08-23T00:00:02Z")
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as handle:
    for row in (root, inner, outer):
        handle.write(json.dumps(row) + "\n")
PY
stale_chain_dry="$(bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$stale_chain_repo" prune --stale --dry-run)" ||
  fail "nested stale-lineage dry-run failed"
printf '%s\n' "$stale_chain_dry" |
  grep -Fq 'would drop 3 stale row(s), 3 -> 0' ||
  fail "nested lineage drops were not counted as stale: $stale_chain_dry"
if printf '%s\n' "$stale_chain_dry" | grep -Fq 'byte retention'; then
  fail "nested lineage drops were mislabeled as byte retention: $stale_chain_dry"
fi
bash "$ROOT/scripts/artifact-index.sh" --repo "$stale_chain_repo" \
  prune --stale >/dev/null || fail "nested stale-lineage apply failed"
[ ! -s "$stale_chain_repo/.oms/artifacts/index.jsonl" ] ||
  fail "nested stale-lineage apply left a dangling resolution"

# Dry-run and apply must predict the same hard byte-ceiling result. Seventeen
# individually valid rows fit below the recovery bound but not the 16MiB
# durable bound; removing one stale row still requires one byte-retention drop.
stale_byte_repo="$TMP/artifact-stale-bytes"; mk_repo "$stale_byte_repo"
mkdir -p "$stale_byte_repo/.oms/artifacts"
python3 - "$stale_byte_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
padding = "x" * (1024 * 1024 - 2048)
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as handle:
    for index in range(17):
        handle.write(json.dumps({
            "schema": 1, "event_id": "evt_live_%d" % index,
            "operation_id": "op_live_%d" % index,
            "artifact_id": "sha256:" + ("%064d" % index),
            "ts": "2026-08-23T00:00:00Z", "kind": "call",
            "provider": "codex", "exit": 0, "padding": padding,
        }, separators=(",", ":")) + "\n")
    handle.write(json.dumps({
        "schema": 1, "event_id": "evt_stale", "operation_id": "op_stale",
        "artifact_id": "sha256:" + "f" * 64,
        "ts": "2026-08-23T00:00:00Z", "kind": "call",
        "provider": "codex", "exit": 0,
        "artifact": ".oms/artifacts/missing.md",
    }) + "\n")
PY
stale_byte_dry="$(bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$stale_byte_repo" prune --stale --dry-run)" ||
  fail "byte-pressure stale dry-run failed"
printf '%s\n' "$stale_byte_dry" |
  grep -Fq 'would drop 1 stale row(s), 18 -> 16' ||
  fail "stale dry-run did not predict byte-bounded row count: $stale_byte_dry"
printf '%s\n' "$stale_byte_dry" |
  grep -Fq 'byte retention would compact 1 additional row(s)' ||
  fail "stale dry-run did not predict the additional byte drop: $stale_byte_dry"
stale_byte_apply="$(bash "$ROOT/scripts/artifact-index.sh" \
  --repo "$stale_byte_repo" prune --stale)" ||
  fail "byte-pressure stale apply failed"
printf '%s\n' "$stale_byte_apply" |
  grep -Fq 'dropped 1 stale row(s), 18 -> 16' ||
  fail "stale apply disagreed with dry-run: $stale_byte_apply"
printf '%s\n' "$stale_byte_apply" |
  grep -Fq 'byte retention compacted 1 additional row(s)' ||
  fail "stale apply did not report the additional byte drop: $stale_byte_apply"
[ "$(wc -l < "$stale_byte_repo/.oms/artifacts/index.jsonl" | tr -d ' ')" -eq 16 ] ||
  fail "stale apply persisted a different row count from its dry-run"

# --- draft-pr intent: verifier text is durable and therefore strict --------
repo="$TMP/draft-pr"; mk_repo "$repo"
git -C "$repo" branch -M main
printf '/.oms/\n' > "$repo/.gitignore"
git -C "$repo" add .gitignore
git -C "$repo" commit -qm ignore
bare="$TMP/draft-pr.git"
git init -q --bare "$bare"
git -C "$repo" remote add origin "$bare"
git -C "$repo" push -q -u origin main
git -C "$repo" checkout -qb codex/durable-writer
git -C "$repo" commit -q --allow-empty -m 'feat: fixture'
real_git="$(command -v git)"
mkdir -p "$TMP/draft-bin"
cat > "$TMP/draft-bin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *' remote get-url --push --all origin') echo 'git@github.com:eightmm/oh-my-setting.git'; exit 0 ;;
  *' remote get-url --all origin') echo 'git@github.com:eightmm/oh-my-setting.git'; exit 0 ;;
esac
translated=()
for arg in "\$@"; do
  [ "\$arg" != 'git@github.com:eightmm/oh-my-setting.git' ] || arg="$bare"
  translated+=("\$arg")
done
exec "$real_git" "\${translated[@]}"
EOF
cat > "$TMP/draft-bin/gh" <<'EOF'
#!/usr/bin/env bash
case "${1:-} ${2:-}" in
  'auth status') exit 0 ;;
  'repo view') printf '{"nameWithOwner":"eightmm/oh-my-setting","viewerPermission":"WRITE"}\n' ;;
  'api --hostname') printf 'fixture-user\n' ;;
  *) exit 9 ;;
esac
EOF
chmod +x "$TMP/draft-bin/git" "$TMP/draft-bin/gh"
if OMS_GIT_BIN="$TMP/draft-bin/git" OMS_GH_BIN="$TMP/draft-bin/gh" \
    bash "$ROOT/scripts/draft-pr.sh" --repo "$repo" prepare --base main \
      --verify "printf '%s' '$LEAK' >/dev/null" >/dev/null 2>&1; then
  fail "draft-pr must refuse a secret-bearing durable verifier"
fi
if OMS_GIT_BIN="$TMP/draft-bin/git" OMS_GH_BIN="$TMP/draft-bin/gh" \
    bash "$ROOT/scripts/draft-pr.sh" --repo "$repo" prepare --base main \
      --verify "printf '%s' '$MPATH' >/dev/null" >/dev/null 2>&1; then
  fail "draft-pr must refuse a machine-path-bearing durable verifier"
fi
if [ -d "$repo/.oms/publish" ] && grep -R -Eq "$LEAK|sentinel-user" "$repo/.oms/publish"; then
  fail "sentinel leaked into a Draft PR publication intent"
fi

# --- acceptance output body: normalized, redacted, symlink-refusing --------
# A non-pass acceptance persists its output under .oms/plan/acceptance keyed by
# the row's output digest. Repo-local git-ignored sink: machine paths
# normalize, secrets redact, and the writer must not follow symlinks (G3
# shape) at either the directory or the leaf.
PLAN="$ROOT/scripts/agent-plan.sh"

accept_repo="$TMP/accept-body"; mk_repo "$accept_repo"
"$PLAN" --repo "$accept_repo" init --goal body-capture \
  --accept "printf 'boom %s\n' '$MPATH'; false" >/dev/null
rc=0; "$PLAN" --repo "$accept_repo" accept >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 3 ] || fail "failing acceptance should exit 3, got $rc"
body_file="$(ls "$accept_repo/.oms/plan/acceptance/"*.log 2>/dev/null | head -n 1)"
[ -n "$body_file" ] || fail "a failing acceptance must persist its output body"
grep -q "boom" "$body_file" || fail "the body must carry the acceptance output"
grep -q "sentinel-user" "$body_file" &&
  fail "machine path must be normalized in the acceptance body" || true
key="$(basename "$body_file" .log)"
grep -q "\"output_sha256\": \"$key\"" "$accept_repo/.oms/plan/progress.jsonl" ||
  fail "the body filename must match the acceptance row's output digest"

accept_redact="$TMP/accept-redact"; mk_repo "$accept_redact"
# The plan guard refuses secret text in the command itself, so the secret must
# reach the writer the way it would in production: via the command's output.
printf 'key %s\n' "$LEAK" > "$accept_redact/leak.txt"
"$PLAN" --repo "$accept_redact" init --goal secret-capture \
  --accept "cat leak.txt; false" >/dev/null
"$PLAN" --repo "$accept_redact" accept >/dev/null 2>&1 || true
secret_body="$(ls "$accept_redact/.oms/plan/acceptance/"*.log 2>/dev/null | head -n 1)"
[ -n "$secret_body" ] || fail "a secret-bearing acceptance still persists a marker body"
grep -q "$LEAK" "$secret_body" && fail "secret leaked into the acceptance body" || true
grep -q "redacted:" "$secret_body" || fail "the secret body must be the redacted marker"

accept_dirlink="$TMP/accept-dirlink"; mk_repo "$accept_dirlink"
outside_dir="$TMP/outside-acceptance"; mkdir -p "$outside_dir"
mkdir -p "$accept_dirlink/.oms/plan"
ln -s "$outside_dir" "$accept_dirlink/.oms/plan/acceptance"
"$PLAN" --repo "$accept_dirlink" init --goal dirlink --accept "echo dirlink-out; false" >/dev/null
rc=0; "$PLAN" --repo "$accept_dirlink" accept >"$TMP/dirlink.out" 2>&1 || rc=$?
[ "$rc" -eq 3 ] || fail "a symlinked acceptance dir must not change the verdict, got $rc"
[ -z "$(ls -A "$outside_dir" 2>/dev/null)" ] ||
  fail "acceptance body write escaped through a symlinked directory"
grep -q "was not persisted" "$TMP/dirlink.out" ||
  fail "the refused body write must be surfaced as a warning"

accept_leaflink="$TMP/accept-leaflink"; mk_repo "$accept_leaflink"
"$PLAN" --repo "$accept_leaflink" init --goal leaflink --accept "echo leaflink-out; false" >/dev/null
"$PLAN" --repo "$accept_leaflink" accept >/dev/null 2>&1 || true
leaf="$(ls "$accept_leaflink/.oms/plan/acceptance/"*.log 2>/dev/null | head -n 1)"
[ -n "$leaf" ] || fail "leaflink fixture needs a first body file"
outside_leaf="$TMP/outside-leaf.log"
printf 'untouched\n' > "$outside_leaf"
rm -f "$leaf"
ln -s "$outside_leaf" "$leaf"
"$PLAN" --repo "$accept_leaflink" accept >/dev/null 2>&1 || true
grep -Fxq 'untouched' "$outside_leaf" ||
  fail "acceptance body write followed a planted leaf symlink"
[ ! -L "$leaf" ] || fail "the planted leaf symlink must be replaced by a regular file"
grep -q "leaflink-out" "$leaf" || fail "the replaced leaf must carry the real body"

echo "durable-writers-contract-smoke: ok"
