#!/usr/bin/env bash
set -euo pipefail

# `artifact-index resolve-superseded` is the mechanical half of the unresolved
# queue's triage: a failure whose exact patch bytes the same provider later
# admitted or landed is answered without a human retyping the printed resolve
# command. These tests fence the bound — byte identity plus provider identity,
# and nothing else — and the idempotency the sweep needs to be safe to wire
# into a repeating repair pass.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-artifact-supersession.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

make_repo() {
  local repo="$1"

  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  printf 'base\n' > "$repo/file.txt"
  git -C "$repo" add file.txt
  git -C "$repo" commit -qm base
  mkdir -p "$repo/.oms/artifacts"
  printf '*\n' > "$repo/.oms/.gitignore"
}

unresolved_count() {
  "$ROOT/scripts/artifact-index.sh" --repo "$1" --json unresolved 100 |
    python3 -c 'import json,sys; print(len(json.load(sys.stdin)["rows"]))'
}

# The index rows are written by hand for the same reason the existing queue
# triage test writes them by hand: the join under test is a property of
# recorded rows, and driving patch-admit into a failure and then into a success
# over byte-identical patches — under two different providers — is far more
# machinery than the property needs. `validate` runs against the fixture below
# so these stay rows the real writers could have produced.
test_resolve_superseded_answers_exact_bytes_and_leaves_the_rest() {
  local repo="$TMP/sweep"
  local index out before after

  make_repo "$repo"
  index="$repo/.oms/artifacts/index.jsonl"
  printf 'diff --git a/file.txt b/file.txt\n' > "$repo/.oms/artifacts/work.patch"
  printf 'diff --git a/other.txt b/other.txt\n' > "$repo/.oms/artifacts/timeout.patch"

  # evt_admit_ok is the winner and comes last, because supersession only ever
  # looks forward from a failure. evt_other_provider shares its bytes but not
  # its provider, and evt_timeout_open shares neither.
  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_admit_fail","operation_id":"op_1","artifact_id":"sha256:a1","ts":"2026-08-06T01:00:00Z","kind":"patch-admit","provider":"codex","exit":1,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_other_provider","operation_id":"op_2","artifact_id":"sha256:a2","ts":"2026-08-06T01:01:00Z","kind":"patch-admit","provider":"gemini","exit":1,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_timeout_open","operation_id":"op_3","artifact_id":"sha256:t1","ts":"2026-08-06T01:02:00Z","kind":"delegate","provider":"codex","exit":124,"patch":".oms/artifacts/timeout.patch","patch_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}
{"schema":1,"event_id":"evt_timeout_landed","operation_id":"op_4","artifact_id":"sha256:t2","ts":"2026-08-06T01:03:00Z","kind":"delegate","provider":"codex","exit":124,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_admit_ok","operation_id":"op_5","artifact_id":"sha256:a3","ts":"2026-08-06T01:04:00Z","kind":"patch-admit","provider":"codex","exit":0,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
EOF

  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "the fixture is not a valid schema-1 index"
  before="$(unresolved_count "$repo")"
  [ "$before" = "4" ] || fail "expected 4 unresolved rows before the sweep, got $before"

  cp "$index" "$TMP/index-before"
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-superseded --dry-run)" ||
    fail "dry-run sweep failed"
  printf '%s\n' "$out" | grep -Fq 'would resolve evt_admit_fail (superseded by evt_admit_ok)' ||
    fail "dry run did not name the superseded admit failure: $out"
  printf '%s\n' "$out" | grep -Fq '2 superseded failure(s) would be resolved (dry run)' ||
    fail "dry run did not summarize its count: $out"
  cmp -s "$TMP/index-before" "$index" || fail "a dry run must not touch the index"

  out="$(cd "$repo" && OMS_AGENT=codex "$ROOT/scripts/artifact-index.sh" resolve-superseded)" ||
    fail "sweep failed"
  printf '%s\n' "$out" | grep -Fq 'resolved evt_admit_fail (superseded by evt_admit_ok)' ||
    fail "the sweep did not resolve the exact-byte admit failure: $out"
  # The bound is byte identity, not the exit code: a timeout whose patch the
  # same provider later admitted is as answered as any other superseded row.
  printf '%s\n' "$out" | grep -Fq 'resolved evt_timeout_landed (superseded by evt_admit_ok)' ||
    fail "a timeout whose exact bytes later landed should still resolve: $out"
  printf '%s\n' "$out" | grep -Fq '2 superseded failure(s) resolved' ||
    fail "the sweep did not summarize its count: $out"
  if printf '%s\n' "$out" | grep -Eq 'evt_other_provider|evt_timeout_open'; then
    fail "the sweep claimed a row outside the exact-byte, same-provider bound: $out"
  fi

  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "the sweep wrote invalid resolution lineage"
  after="$(unresolved_count "$repo")"
  [ "$after" = "2" ] || fail "expected 2 unresolved rows after the sweep, got $after"
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" unresolved 100)"
  printf '%s\n' "$out" | grep -Fq 'event=evt_other_provider' ||
    fail "a same-bytes failure from another provider must stay a human call"
  printf '%s\n' "$out" | grep -Fq 'event=evt_timeout_open' ||
    fail "a failure the bytes do not answer must stay in the queue"

  python3 - "$index" <<'PY' || fail "the appended resolutions are not the rows resolve would have written"
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
resolutions = [r for r in rows if r.get("kind") == "artifact-resolution"]
assert len(resolutions) == 2, resolutions
targets = {r["resolves_event_id"]: r for r in resolutions}
assert set(targets) == {"evt_admit_fail", "evt_timeout_landed"}, targets
for target, row in targets.items():
    assert row["schema"] == 1 and row["exit"] == 0, row
    assert row["parent_event_id"] == target and row["resolution"] == "resolved", row
    assert row["provider"] == "codex", row
    assert row["reason"] == "superseded by evt_admit_ok (same patch bytes later succeeded)", row
source = {r["event_id"]: r for r in rows if r.get("kind") != "artifact-resolution"}
for target, row in targets.items():
    assert row["operation_id"] == source[target]["operation_id"], row
    assert row["artifact_id"] == source[target]["artifact_id"], row
PY

  cp "$index" "$TMP/index-settled"
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-superseded)" ||
    fail "second sweep failed"
  printf '%s\n' "$out" | grep -Fq '0 superseded failure(s) resolved' ||
    fail "a second sweep must find nothing left to resolve: $out"
  cmp -s "$TMP/index-settled" "$index" || fail "a second sweep appended a duplicate resolution"
}

# status() and resolve() disagree about what counts as already resolved, and
# the sweep has to obey the stricter of the two: anything resolve would refuse
# to write twice would otherwise be re-appended on every single run.
test_resolve_superseded_skips_rows_resolve_would_refuse() {
  local repo="$TMP/refused"
  local index out

  make_repo "$repo"
  index="$repo/.oms/artifacts/index.jsonl"
  printf 'diff --git a/file.txt b/file.txt\n' > "$repo/.oms/artifacts/work.patch"

  # evt_partial_resolution already carries a resolution whose lineage the queue
  # view rejects, so it still reads as unresolved; evt_duplicated is recorded
  # twice, which no resolution can ever clear.
  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_partial_resolution","operation_id":"op_1","artifact_id":"sha256:a1","ts":"2026-08-06T02:00:00Z","kind":"patch-admit","provider":"codex","exit":1,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_duplicated","operation_id":"op_2","artifact_id":"sha256:a2","ts":"2026-08-06T02:01:00Z","kind":"patch-admit","provider":"codex","exit":1,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_duplicated","operation_id":"op_2","artifact_id":"sha256:a2","ts":"2026-08-06T02:02:00Z","kind":"patch-admit","provider":"codex","exit":1,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_resolution_partial","operation_id":"op_1","artifact_id":"sha256:a1","ts":"2026-08-06T02:03:00Z","kind":"artifact-resolution","provider":"unknown","exit":0,"resolves_event_id":"evt_partial_resolution","resolution":"resolved"}
{"schema":1,"event_id":"evt_admit_ok","operation_id":"op_3","artifact_id":"sha256:a3","ts":"2026-08-06T02:04:00Z","kind":"patch-admit","provider":"codex","exit":0,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
EOF

  cp "$index" "$TMP/refused-before"
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-superseded)" ||
    fail "sweep over unrepairable rows failed"
  printf '%s\n' "$out" | grep -Fq '0 superseded failure(s) resolved' ||
    fail "the sweep answered a row resolve would refuse: $out"
  cmp -s "$TMP/refused-before" "$index" ||
    fail "the sweep wrote a resolution that could never settle"
}

test_resolve_superseded_argument_surface() {
  local repo="$TMP/surface"
  local out

  make_repo "$repo"
  # Wired into repeating repair passes, so a repo that has recorded nothing is
  # a no-op and not an error.
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-superseded)" ||
    fail "an empty index should be a no-op sweep"
  printf '%s\n' "$out" | grep -Fq '0 superseded failure(s) resolved' ||
    fail "the empty-index sweep did not report a count: $out"

  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-superseded 5 >/dev/null 2>&1; then
    fail "the sweep has no window, so a positional N must be refused"
  fi
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" --json resolve-superseded >/dev/null 2>&1; then
    fail "--json must be refused where it means nothing"
  fi
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-superseded \
    --event-id evt_x >/dev/null 2>&1; then
    fail "--event-id belongs to resolve, not to the sweep"
  fi
}

test_resolve_superseded_answers_exact_bytes_and_leaves_the_rest
test_resolve_superseded_skips_rows_resolve_would_refuse
test_resolve_superseded_argument_surface

echo "artifact supersession smoke: ok"
