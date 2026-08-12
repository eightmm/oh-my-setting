#!/usr/bin/env bash
set -euo pipefail

# `artifact-index resolve-recovered` is the seat-recovery half of the
# unresolved queue's mechanical triage: an exit-124 wall death whose provider
# and kind later produced a strictly newer success is recorded as recovered,
# the same principle the fail-ledger applies at the peer choke point. These
# tests fence the bound — exit 124 only, same provider, same kind, strictly
# later success — and the idempotency an explicitly repeated sweep needs.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-artifact-recovery.XXXXXX")"
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

# Rows are written by hand for the same reason the supersession smoke writes
# them by hand: the join under test is a property of recorded rows, and
# reproducing a live wall death plus a later healthy call is far more machinery
# than the property needs. `validate` runs against the fixture so these stay
# rows the real writers could have produced.
test_resolve_recovered_answers_wall_deaths_and_leaves_the_rest() {
  local repo="$TMP/sweep"
  local index out before after

  make_repo "$repo"
  index="$repo/.oms/artifacts/index.jsonl"

  # evt_wall_death is the only eligible row: exit 124, and evt_seat_back is a
  # strictly later codex ask success. evt_review_open (claude review) sees only
  # a later codex review success, so provider identity keeps it open;
  # evt_semantic_fail shares seat and kind but exits 1; evt_call_early_only's
  # only same-seat success is earlier; evt_delegate_open has no delegate
  # success at all.
  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_call_ok_early","operation_id":"op_0","artifact_id":"sha256:c0","ts":"2026-08-12T01:00:00Z","kind":"call","provider":"agy","exit":0}
{"schema":1,"event_id":"evt_wall_death","operation_id":"op_1","artifact_id":"sha256:a1","ts":"2026-08-12T01:01:00Z","kind":"ask","provider":"codex","exit":124}
{"schema":1,"event_id":"evt_review_open","operation_id":"op_2","artifact_id":"sha256:r1","ts":"2026-08-12T01:02:00Z","kind":"review","provider":"claude","exit":124}
{"schema":1,"event_id":"evt_semantic_fail","operation_id":"op_3","artifact_id":"sha256:s1","ts":"2026-08-12T01:03:00Z","kind":"ask","provider":"codex","exit":1}
{"schema":1,"event_id":"evt_call_early_only","operation_id":"op_4","artifact_id":"sha256:c1","ts":"2026-08-12T01:04:00Z","kind":"call","provider":"agy","exit":124}
{"schema":1,"event_id":"evt_delegate_open","operation_id":"op_5","artifact_id":"sha256:d1","ts":"2026-08-12T01:05:00Z","kind":"delegate","provider":"codex","exit":124}
{"schema":1,"event_id":"evt_seat_back","operation_id":"op_6","artifact_id":"sha256:a2","ts":"2026-08-12T01:06:00Z","kind":"ask","provider":"codex","exit":0}
{"schema":1,"event_id":"evt_review_ok_other","operation_id":"op_7","artifact_id":"sha256:r2","ts":"2026-08-12T01:07:00Z","kind":"review","provider":"codex","exit":0}
EOF

  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "the fixture is not a valid schema-1 index"
  before="$(unresolved_count "$repo")"
  [ "$before" = "5" ] || fail "expected 5 unresolved rows before the sweep, got $before"

  # The advisory annotation names the recovery for the eligible row and only
  # for the eligible row; nothing resolves without the explicit command.
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" unresolved 100)"
  printf '%s\n' "$out" | grep -Fq 'recovered-by: evt_seat_back' ||
    fail "the eligible wall death lost its recovered-by annotation: $out"
  [ "$(printf '%s\n' "$out" | grep -Fc 'recovered-by:')" = "1" ] ||
    fail "recovered-by annotated a row outside the eligible bound: $out"

  cp "$index" "$TMP/index-before"
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-recovered --dry-run)" ||
    fail "dry-run sweep failed"
  printf '%s\n' "$out" | grep -Fq 'would resolve evt_wall_death (recovered by evt_seat_back)' ||
    fail "dry run did not name the recovered wall death: $out"
  printf '%s\n' "$out" | grep -Fq '1 recovered failure(s) would be resolved (dry run)' ||
    fail "dry run did not summarize its count: $out"
  cmp -s "$TMP/index-before" "$index" || fail "a dry run must not touch the index"

  out="$(cd "$repo" && OMS_AGENT=codex "$ROOT/scripts/artifact-index.sh" resolve-recovered)" ||
    fail "sweep failed"
  printf '%s\n' "$out" | grep -Fq 'resolved evt_wall_death (recovered by evt_seat_back)' ||
    fail "the sweep did not resolve the recovered wall death: $out"
  printf '%s\n' "$out" | grep -Fq '1 recovered failure(s) resolved' ||
    fail "the sweep did not summarize its count: $out"
  if printf '%s\n' "$out" |
    grep -Eq 'evt_review_open|evt_semantic_fail|evt_call_early_only|evt_delegate_open'; then
    fail "the sweep claimed a row outside the exit-124 same-seat later-success bound: $out"
  fi

  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "the sweep wrote invalid resolution lineage"
  after="$(unresolved_count "$repo")"
  [ "$after" = "4" ] || fail "expected 4 unresolved rows after the sweep, got $after"
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" unresolved 100)"
  printf '%s\n' "$out" | grep -Fq 'event=evt_review_open' ||
    fail "a wall death answered only by another provider must stay a human call"
  printf '%s\n' "$out" | grep -Fq 'event=evt_semantic_fail' ||
    fail "a semantic failure must stay in the queue whatever the seat did later"
  printf '%s\n' "$out" | grep -Fq 'event=evt_call_early_only' ||
    fail "a success earlier than the wall death proves nothing and must not resolve it"
  printf '%s\n' "$out" | grep -Fq 'event=evt_delegate_open' ||
    fail "a wall death with no later same-kind success must stay in the queue"

  python3 - "$index" <<'PY' || fail "the appended resolution is not the row resolve would have written"
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
resolutions = [r for r in rows if r.get("kind") == "artifact-resolution"]
assert len(resolutions) == 1, resolutions
row = resolutions[0]
assert row["schema"] == 1 and row["exit"] == 0, row
assert row["resolves_event_id"] == "evt_wall_death", row
assert row["parent_event_id"] == "evt_wall_death", row
assert row["resolution"] == "resolved", row
assert row["provider"] == "codex", row
assert row["reason"] == "recovered evt_wall_death by evt_seat_back (same provider and kind later succeeded)", row
source = {r["event_id"]: r for r in rows if r.get("kind") != "artifact-resolution"}
assert row["operation_id"] == source["evt_wall_death"]["operation_id"], row
assert row["artifact_id"] == source["evt_wall_death"]["artifact_id"], row
PY

  cp "$index" "$TMP/index-settled"
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-recovered)" ||
    fail "second sweep failed"
  printf '%s\n' "$out" | grep -Fq '0 recovered failure(s) resolved' ||
    fail "a second sweep must find nothing left to resolve: $out"
  cmp -s "$TMP/index-settled" "$index" || fail "a second sweep appended a duplicate resolution"

  # The settled row keeps its annotation history without re-entering the queue.
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" unresolved 100)"
  if printf '%s\n' "$out" | grep -Fq 'event=evt_wall_death'; then
    fail "a resolved wall death re-entered the unresolved queue: $out"
  fi
}

test_resolve_recovered_argument_surface() {
  local repo="$TMP/surface"
  local out

  make_repo "$repo"
  # Explicit sweeps run wherever the harness runs, so a repo that has recorded
  # nothing is a no-op and not an error.
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-recovered)" ||
    fail "an empty index should be a no-op sweep"
  printf '%s\n' "$out" | grep -Fq '0 recovered failure(s) resolved' ||
    fail "the empty-index sweep did not report a count: $out"
  out="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-recovered --dry-run)" ||
    fail "an empty index should be a no-op dry run"
  printf '%s\n' "$out" | grep -Fq '0 recovered failure(s) would be resolved (dry run)' ||
    fail "the empty-index dry run did not report a count: $out"

  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-recovered 5 >/dev/null 2>&1; then
    fail "the sweep has no window, so a positional N must be refused"
  fi
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" --json resolve-recovered >/dev/null 2>&1; then
    fail "--json must be refused where it means nothing"
  fi
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve-recovered \
    --event-id evt_x >/dev/null 2>&1; then
    fail "--event-id belongs to resolve, not to the sweep"
  fi
}

test_resolve_recovered_answers_wall_deaths_and_leaves_the_rest
test_resolve_recovered_argument_surface

echo "artifact recovery smoke: ok"
