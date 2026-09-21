#!/usr/bin/env bash
# Globals belong to the sourced harness; its writer is intentionally overridden
# only after the normal-path checks, to test late preparation failure.
# shellcheck disable=SC2034,SC2218
set -euo pipefail

# Fresh debate calls need bounded positions, evidence and deltas for every
# seat, with an on-disk reference to the full text. Stop early when every seat declares
# "none" under "Changed from previous round:". Extraction anchors on the
# LAST header occurrence because codex-style providers echo the entire
# prompt, section headers included, inside their output stream: a
# first-occurrence match would quote the instructions as the answer.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-debate-delta.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM HUP

# shellcheck source=../scripts/lib/peer-common.sh
. "$ROOT/scripts/lib/peer-common.sh"

fail() {
  echo "debate-delta-smoke: $*" >&2
  exit 1
}

assert_contains() {
  local file="$1"
  local text="$2"
  grep -Fq "$text" "$file" || fail "$(basename "$file") missing: $text"
}

assert_lacks() {
  local file="$1"
  local text="$2"
  if grep -Fq "$text" "$file"; then
    fail "$(basename "$file") must not contain: $text"
  fi
}

# A debate answer the way codex actually writes one: the prompt (with its
# section headers and the "none" instruction) echoed first, tool noise, then
# the real sections at the end.
write_echoing_artifact() {  # PATH MARKER CHANGED_TEXT
  local file="$1"
  local marker="$2"
  local changed="$3"
  {
    printf '# provider ask\n\n## Output\n\n'
    printf 'user\n'
    printf 'Return exactly these sections:\n'
    printf 'Answer:\nChanged from previous round:\nRemaining disagreements:\n'
    printf 'If nothing changed your position this round, write exactly "none" under "Changed from previous round:".\n'
    printf 'exec ls succeeded in 2ms\n'
    printf '%s-NOISE\n' "$marker"
    printf 'Answer:\n%s revised body line one\nline two with detail\n' "$marker"
    printf 'Changed from previous round:\n%s\n' "$changed"
    printf 'Remaining disagreements:\n%s-DISPUTE holds\n' "$marker"
    printf '\n## Exit\n\n0\n'
  } > "$file"
}

REPO="$TMP/repo"
mkdir -p "$REPO/.oms/artifacts/ask"

# Repeatable byte baseline; no model calls and no token-count claim.
long="$REPO/.oms/artifacts/ask/long.md"
{
  printf '# answer\n\n## Output\n\nAnswer:\n'
  for ((n=0; n<160; n++)); do printf 'Current claim: retain numerical precision and check the measured regression.\n'; done
  printf 'Evidence:\nsrc/kernel.py:42 and tests/test_kernel.py:18 support this claim.\n'
  printf 'Changed from previous round:\nKeep fp32 accumulation.\n'
  printf 'Remaining disagreements:\nThe throughput tradeoff is still unmeasured.\n\n## Exit\n\n0\n'
} > "$long"
PROMPT='Review precision versus throughput.'
MA_DEBATE_SECTIONS=$'Answer:\nEvidence:\nChanged from previous round:\nRemaining disagreements:'
write_debate_prompt "$TMP/measure.prompt" codex 2 "$long" "claude:$long" "antigravity:$long"
printf 'three-seat fixture prompt bytes per seat: %s\n' "$(wc -c < "$TMP/measure.prompt" | tr -d ' ')"
[ "$(wc -c < "$TMP/measure.prompt")" -lt 15000 ] || fail 'structured repetition was not compacted'
assert_contains "$TMP/measure.prompt" 'Current claim: retain numerical precision'
assert_contains "$TMP/measure.prompt" 'src/kernel.py:42'
assert_contains "$TMP/measure.prompt" 'Keep fp32 accumulation.'
assert_contains "$TMP/measure.prompt" 'throughput tradeoff is still unmeasured'
assert_contains "$TMP/measure.prompt" '[TRUNCATED: read full answer]'

no_sections="$REPO/.oms/artifacts/ask/agy-x-r2.md"
printf '# a\n\n## Output\n\nfreeform answer only\n\n## Exit\n\n0\n' > "$no_sections"

# --- unchanged detection: strict, punctuation-tolerant, echo-proof ---
unchanged="$REPO/.oms/artifacts/ask/codex-unchanged.md"
write_echoing_artifact "$unchanged" GAMMA "none"
ma_debate_seat_unchanged "$unchanged" || fail "explicit none must read as unchanged"
write_echoing_artifact "$unchanged" GAMMA "None."
ma_debate_seat_unchanged "$unchanged" || fail "None. must read as unchanged"
write_echoing_artifact "$unchanged" GAMMA $'none\nCorrection: the proposed fix introduces a race.'
if ma_debate_seat_unchanged "$unchanged"; then
  fail "a correction after none must not stop the debate"
fi
write_echoing_artifact "$unchanged" GAMMA "dropped my S4 objection"
if ma_debate_seat_unchanged "$unchanged"; then
  fail "prose under Changed must read as changed"
fi
if ma_debate_seat_unchanged "$no_sections"; then
  fail "a sectionless answer must read as changed"
fi
# An empty Changed section must not read the disagreements below it as its
# value: nothing stated is not "none" stated.
empty_changed="$REPO/.oms/artifacts/ask/codex-empty.md"
{
  printf '# a\n\n## Output\n\nAnswer:\nbody\n'
  printf 'Changed from previous round:\n'
  printf 'Remaining disagreements:\nnone\n'
  printf '\n## Exit\n\n0\n'
} > "$empty_changed"
if ma_debate_seat_unchanged "$empty_changed"; then
  fail "an empty Changed section must read as changed"
fi

# --- all rounds retain positions and deltas, without prompt echoes ---
PROMPT='Bound prior provider context.'
MA_DEBATE_ROLE=advisors
MA_DEBATE_TOPIC=question
MA_DEBATE_SECTIONS=$'Answer:\nChanged from previous round:\nRemaining disagreements:'

self="$REPO/.oms/artifacts/ask/codex-self.md"
other="$REPO/.oms/artifacts/ask/claude-other.md"
write_echoing_artifact "$self" SELF "self delta"
write_echoing_artifact "$other" OTHER "reworked the risk ranking"

r2="$TMP/round2.prompt"
write_debate_prompt "$r2" codex 2 "$self" "claude:$other"
assert_lacks "$r2" 'OTHER-NOISE'
assert_contains "$r2" 'OTHER revised body line one'
assert_contains "$r2" 'full answer on disk: .oms/artifacts/ask/claude-other.md'
assert_contains "$r2" 'write exactly "none" under "Changed from previous round:"'

r3="$TMP/round3.prompt"
write_debate_prompt "$r3" codex 3 "$self" "claude:$other"
assert_contains "$r3" 'reworked the risk ranking'
assert_contains "$r3" 'OTHER-DISPUTE holds'
assert_lacks "$r3" 'OTHER-NOISE'
assert_lacks "$r3" 'SELF-NOISE'
assert_contains "$r3" 'SELF revised body line one'
assert_contains "$r3" 'OTHER revised body line one'
assert_contains "$r3" 'full answer on disk: .oms/artifacts/ask/claude-other.md'

# Freeform answers retain the bounded full quote and an original reference.
r3f="$TMP/round3-fallback.prompt"
write_debate_prompt "$r3f" codex 3 "$self" "claude:$no_sections"
assert_contains "$r3f" 'freeform answer only'
assert_contains "$r3f" 'full answer on disk:'

# Reuse only identical artifact/quota pairs; bytes and sanitization stay intact.
(
  quote_cache_prefix="$TMP/quote-cache"
  quote_artifacts=()
  quote_budgets=()
  python3() {
    case "${2:-}:${3:-}" in
      *:--debate-excerpt) printf 'extract\n' >> "$TMP/extractions" ;;
    esac
    command python3 "$@"
  }
  for ((seat=0; seat<5; seat++)); do
    ma_debate_quote "$long" 1024 > "$TMP/cached-$seat"
    cmp "$TMP/cached-0" "$TMP/cached-$seat" || fail 'cache changed quote bytes'
  done
  [ "$(wc -l < "$TMP/extractions")" -eq 1 ] || fail 'identical quotes were re-extracted'
  ma_debate_quote "$long" 512 > "$TMP/smaller"
  ma_debate_quote "$self" 1024 > "$TMP/different"
  [ "$(wc -l < "$TMP/extractions")" -eq 3 ] || fail 'cache mixed quotas or artifacts'
  (
    unset quote_cache_prefix
    ma_debate_quote "$long" 1024 > "$TMP/uncached"
    ma_debate_quote "$long" 512 > "$TMP/uncached-smaller"
  )
  cmp "$TMP/cached-0" "$TMP/uncached" || fail 'cached quote differs from original'
  cmp "$TMP/smaller" "$TMP/uncached-smaller" || fail 'smaller quota differs'
  # A new round must not reuse the previous contents, even at the same path.
  quote_artifacts=()
  quote_budgets=()
  before="$(wc -l < "$TMP/extractions")"
  ma_debate_quote "$long" 1024 > "$TMP/refreshed"
  [ "$(wc -l < "$TMP/extractions")" -eq "$((before + 1))" ] || fail 'cache survived its round'
  python3() { return 1; }
  if ma_debate_quote "$long" 256 > "$TMP/failed-quote"; then
    fail 'failed extraction was cached as success'
  fi
  [ "${#quote_artifacts[@]}" -eq 1 ] || fail 'failed quote entered the cache'
)

# --- the loop stops when every seat declares none ---
DEBATE=3
DRY_RUN=1
slug=stable
timestamp=20260808T000000Z-1
ARTIFACT_DIR="$REPO/.oms/artifacts/ask"
debate_dir="$TMP/debate"
mkdir -p "$debate_dir"
provider_names=(codex claude)
alive=(1 1)
artifacts=("$self" "$other")
last_arts=("$self" "$other")
dropped=0
dropped_names=()

run_provider() {  # stubbed seat: answers, and never moves
  write_echoing_artifact "$3" STABLE none
}

ma_run_debate_rounds
[ "${debate_stable_round:-}" = 2 ] ||
  fail "unanimous none must stop the debate after round 2 (got: ${debate_stable_round:-unset})"
[ -f "$ARTIFACT_DIR/codex-stable-$timestamp-r2.md" ] || fail "round 2 must have run"
if ls "$ARTIFACT_DIR"/*-r3.md >/dev/null 2>&1; then
  fail "round 3 must not run after a stable round 2"
fi

# One moving seat keeps the debate alive to its budget.
rm -f "$ARTIFACT_DIR"/*-r2.md "$ARTIFACT_DIR"/*-r3.md "$ARTIFACT_DIR"/*-r4.md
alive=(1 1)
last_arts=("$self" "$other")
dropped=0
dropped_names=()
run_provider() {
  case "$1" in
    codex) write_echoing_artifact "$3" MOVER "sharpened the S11 objection" ;;
    *) write_echoing_artifact "$3" HOLDER none ;;
  esac
}
ma_run_debate_rounds
[ -z "${debate_stable_round:-}" ] ||
  fail "a moving seat must keep the debate running (stable=$debate_stable_round)"
[ -f "$ARTIFACT_DIR/codex-stable-$timestamp-r4.md" ] || fail "budgeted rounds must all run"

# A missing participant is not evidence of stability; retain its last good
# answer, but label that stale contribution in the synthesis itself.
alive=(1 1)
last_arts=("$self" "$other")
dropped=0
dropped_names=()
run_provider() {
  if [ "$1" = claude ] && [[ "$3" == *-r3.md ]]; then return 1; fi
  if [[ "$3" == *-r2.md ]]; then
    write_echoing_artifact "$3" MOVED 'updated the risk ranking'
  else
    write_echoing_artifact "$3" HOLDER none
  fi
}
ma_run_debate_rounds
[ -z "${debate_stable_round:-}" ] || fail 'dropout must not be reported as unanimous stability'
[ "$dropped" -eq 1 ] || fail 'dropout must be counted'
MA_KIND=ask
ok=2 total=2
seat_exit=(0 0)
seat_quality=('' '')
prompt_file="$r2"
ma_write_synthesis "$TMP/dropout-synthesis.md"
assert_contains "$TMP/dropout-synthesis.md" '_last successful answer; provider dropped during debate'
assert_contains "$TMP/dropout-synthesis.md" 'claude-stable-'

# --- a model-pinned seat stays quotable and its files stay portable ----------
# The raw target (provider:model=NAME) once entered the "name:artifact" pair
# and the round file names: the pair split at the wrong colon, mangling the
# peer's artifact path into an absolute-path leak the outbound scrubber
# blocked, and the colon is not a legal NTFS file-name byte. The seat label
# is the only form that may cross into encodings and names.
rm -f "$ARTIFACT_DIR"/*-r2.md "$ARTIFACT_DIR"/*-r3.md "$ARTIFACT_DIR"/*-r4.md
DEBATE=1
provider_names=(codex "claude:model=sonnet-x")
alive=(1 1)
last_arts=("$self" "$other")
dropped=0
dropped_names=()
run_provider() { write_echoing_artifact "$3" PINNED none; }
ma_run_debate_rounds
[ -f "$ARTIFACT_DIR/claude-sonnet-x-stable-$timestamp-r2.md" ] ||
  fail "the pinned seat's round artifact must use its colon-free label"
for f in "$ARTIFACT_DIR"/*; do
  case "$(basename "$f")" in
    *:*) fail "no artifact file name may carry a colon (NTFS-illegal): $f" ;;
  esac
done
pinned_prompt="$debate_dir/prompt-r2-codex"
[ -f "$pinned_prompt" ] || fail "codex round-2 prompt missing"
assert_contains "$pinned_prompt" "OTHER revised body line one"
assert_contains "$pinned_prompt" "full answer on disk: .oms/artifacts/ask/"
assert_lacks "$pinned_prompt" "model=sonnet-x:"
[ -f "$debate_dir/prompt-r2-claude-sonnet-x" ] ||
  fail "the pinned seat's prompt file must use its colon-free label"

# --- a debate-dropped seat still counts toward answered families -------------
# Its last good answer rides the synthesis, so a two-family debate that lost
# one seat in the final round must not report "1 family: replication".
provider_names=(codex antigravity)
alive=(1 0)
dropped_names=(antigravity)
last_arts=("$self" "$other")
fams="$(ma_answered_families)"
[ "$fams" = 2 ] || fail "a dropped seat's family must still count (got $fams)"
# A seat that never answered (round-1 non-answer/failure) still counts nothing.
dropped_names=()
fams="$(ma_answered_families)"
[ "$fams" = 1 ] || fail "a never-answered seat must not count (got $fams)"

# Parallel seats publish immediately, but all see the same round-start notes.
# No provider CLI is invoked; the barrier rejects accidentally serial dispatch.
(
  MA_KIND=ask DEBATE=2 DRY_RUN=1
  THREAD_ID=council-test
  bash "$ROOT/scripts/thread.sh" --repo "$REPO" new --id "$THREAD_ID" --live --topic council >/dev/null
  bash "$ROOT/scripts/thread.sh" --repo "$REPO" --id "$THREAD_ID" append --role note \
    --text 'ROUND-START-NOTE compare evidence' >/dev/null
  provider_names=(codex 'claude:model=opus' 'codex:model=sol' 'codex:model=astra' 'codex:model=terra')
  alive=(1 1 1 1 1)
  last_arts=("$self" "$other" "$self" "$other" "$self")
  dropped=0 dropped_names=()
  unset OMS_MODEL_SELECTED
  mkdir "$TMP/parallel-seats"
  run_provider() {
    local tries=0
    OMS_MODEL_SELECTED='seat-route-must-stay-local'
    assert_contains "$2" 'ROUND-START-NOTE'
    case "$2" in
      *prompt-r2-*)
        touch "$TMP/parallel-seats/$(ma_target_label "$1" "$(ma_target_model "$1")")"
        until [ "$(find "$TMP/parallel-seats" -type f | wc -l)" -eq 5 ]; do
          tries=$((tries + 1))
          [ "$tries" -lt 50 ] || return 9
          sleep 0.1
        done
        assert_lacks "$2" 'COORDINATOR-CORRECTION'
        assert_lacks "$2" 'PARALLEL-codex revised body'
        if [ "$1" = codex ]; then
          bash "$ROOT/scripts/thread.sh" --repo "$REPO" --id "$THREAD_ID" append --role note \
            --text 'COORDINATOR-CORRECTION check the filter first' >/dev/null
        fi
        ;;
      *prompt-r3-*)
        assert_contains "$2" 'PARALLEL-codex revised body'
        assert_contains "$2" 'COORDINATOR-CORRECTION'
        ;;
    esac
    write_echoing_artifact "$3" "PARALLEL-$(printf '%s' "$1" | tr ':=' '--')" 'revised using latest evidence'
  }
  ma_run_debate_rounds
  [ -z "${OMS_MODEL_SELECTED:-}" ] || fail 'speaker routing leaked into owner/synthesis'
  [ "$dropped" = 0 ] || fail 'parallel council dropped a seat'
  [ "$(grep -c '"role": "answer"' "$REPO/.oms/threads/$THREAD_ID.jsonl")" = 10 ] || fail 'council must publish every reply once'

  previous="${last_arts[1]}"
  DEBATE=1 timestamp=council-failure
  run_provider() {
    [ "$1" != 'claude:model=opus' ] || return 7
    write_echoing_artifact "$3" RETAIN none
  }
  ma_run_debate_rounds
  [ "$dropped" = 1 ] && [ "${alive[1]}" = 0 ] || fail 'council failure was counted as success'
  [ "${last_arts[1]}" = "$previous" ] || fail 'council failure replaced the last good answer'
  assert_contains "$REPO/.oms/threads/$THREAD_ID.jsonl" 'no answer (exit 7)'

  # Completion-order publication cannot wait for the first (slow) seat.
  run_provider() {
    if [ "$1" = slow ]; then
      local tries=0
      until grep -q 'FAST-PUBLISHED' "$REPO/.oms/threads/$THREAD_ID.jsonl"; do
        tries=$((tries + 1))
        [ "$tries" -lt 50 ] || return 9
        sleep 0.1
      done
    fi
    write_echoing_artifact "$3" FAST-PUBLISHED none
  }
  ma_council_call slow "$self" "$ARTIFACT_DIR/council-slow.md" &
  slow_pid=$!
  ma_council_call fast "$self" "$ARTIFACT_DIR/council-fast.md"
  wait "$slow_pid" || fail 'a slow seat blocked publication of the fast seat'
)

# All three seats share one round budget; oversized questions launch nobody.
DEBATE=1
provider_names=(codex claude antigravity)
alive=(1 1 1)
last_arts=("$long" "$long" "$long")
OMS_DEBATE_ROUND_BYTES=12288
PROMPT='Preserve three independent views.'
run_provider() {
  printf '%s\n' "$1" >> "$TMP/calls"
  write_echoing_artifact "$3" BOUNDED none
}
ma_run_debate_rounds
[ "$(wc -l < "$TMP/calls")" -eq 3 ] || fail 'budget must not remove a seat'
round_size=0
for seat in codex claude antigravity; do
  file="$debate_dir/prompt-r2-$seat"
  round_size=$((round_size + $(wc -c < "$file")))
  assert_contains "$file" 'src/kernel.py:42'
  assert_contains "$file" 'throughput tradeoff is still unmeasured'
done
[ "$round_size" -le "$OMS_DEBATE_ROUND_BYTES" ] || fail 'whole round exceeded its budget'
printf 'bounded three-seat round bytes: %s/%s\n' "$round_size" "$OMS_DEBATE_ROUND_BYTES"
before="$(wc -l < "$TMP/calls")"
PROMPT="$(printf '%14000s' x)"
if ma_run_debate_rounds > "$TMP/oversize.out" 2>&1; then
  fail 'oversized operator question must refuse, not truncate or partially launch'
fi
[ "$(wc -l < "$TMP/calls")" -eq "$before" ] || fail 'budget refusal launched a provider'
assert_contains "$TMP/oversize.out" 'insufficient evidence space'
unset OMS_DEBATE_ROUND_BYTES

# The shared source identity must not silently mix concurrent edits into a
# later round. The context pack contains only question/source/answer data.
git -C "$REPO" init -q
printf 'tracked source\n' > "$REPO/source.txt"
git -C "$REPO" add source.txt
git -C "$REPO" -c user.name=Test -c user.email=test@example.com commit -qm initial
INCLUDE_DIFF=1
ma_prepare_council_context
context_dir="$MA_COUNCIL_CONTEXT_DIR"
assert_contains "$context_dir/source.txt" 'Tracked base:'
assert_contains "$context_dir/request.md" 'Original question:'
printf 'changed source\n' > "$REPO/source.txt"
if ma_run_debate_rounds > "$TMP/changed-source.out" 2>&1; then
  fail 'changed tracked source must stop the next round'
fi
assert_contains "$TMP/changed-source.out" 'tracked source changed'
[ "$(wc -l < "$TMP/calls")" -eq "$before" ] || fail 'source mismatch spent a seat'
printf 'tracked source\n' > "$REPO/source.txt"
git -C "$REPO" -c user.name=Test -c user.email=test@example.com commit --allow-empty -qm new-base
if ma_run_debate_rounds > "$TMP/changed-base.out" 2>&1; then
  fail 'a new HEAD with identical files must not reuse the old source identity'
fi
assert_contains "$TMP/changed-base.out" 'tracked source changed'
ma_cleanup_council_context
[ ! -d "$context_dir" ] || fail 'shared context cleanup failed'
unset INCLUDE_DIFF
MA_KIND=review
NO_DIFF=0
BASE_REF=HEAD~1
ma_prepare_council_context
assert_contains "$MA_COUNCIL_CONTEXT_DIR/source.txt" 'Tracked base:'
assert_contains "$MA_COUNCIL_CONTEXT_DIR/source.txt" "Diff base tree: $(git -C "$REPO" rev-parse 'HEAD~1^{tree}')"
ma_cleanup_council_context
unset MA_KIND NO_DIFF BASE_REF

# No partially launched round even when a later seat's preparation fails.
eval "$(declare -f write_debate_prompt | sed '1s/write_debate_prompt/real_write_debate_prompt/')"
write_debate_prompt() {
  [ "$2" != claude ] || return 2
  real_write_debate_prompt "$@"
}
PROMPT='Valid short question.'
if ma_run_debate_rounds >/dev/null 2>&1; then
  fail 'late prompt-preparation failure must fail the round'
fi
[ "$(wc -l < "$TMP/calls")" -eq "$before" ] || fail 'late preparation failure spent an earlier seat'

# Structured clipping preserves section coverage, Unicode and explicit debt.
PYTHONPATH="$ROOT/scripts/lib${PYTHONPATH:+:$PYTHONPATH}" python3 - <<'PY'
from peer_artifacts import debate_excerpt, debate_unchanged, stage_context, usage_footer
import json
import tempfile
from pathlib import Path

text = ('echoed prompt\nAnswer:\nnoise\nFindings:\n' + '정밀도 위험 ' * 1000
        + '\nEvidence:\nsrc/kernel.py:42\nRisks:\noverflow remains unverified\n'
        + 'Missing tests:\nlarge input\nRecommendation:\nmeasure first\n'
        + 'Changed from previous round:\nnone\nRemaining disagreements:\nlatency unknown')
clipped = debate_excerpt(text, 1024)
assert len(clipped.encode('utf-8')) <= 1024
assert 'echoed prompt' not in clipped and 'noise' not in clipped
for value in ('정밀도', 'src/kernel.py:42', 'overflow remains unverified', 'large input',
              'measure first', 'Changed from previous round:', 'latency unknown', '[TRUNCATED:'):
    assert value in clipped, (value, clipped)
assert debate_excerpt('freeform answer', 1024) == 'freeform answer'
freeform = 'MAIN CLAIM\n' + '한글 ' * 1000 + '\nUNRESOLVED RISK'
for limit in (35, 128, 1024, 4096):
    clipped = debate_excerpt(freeform, limit)
    assert len(clipped.encode()) <= limit
    if limit >= 128:
        assert 'MAIN CLAIM' in clipped and 'UNRESOLVED RISK' in clipped
status = 'Answer:\n' + 'body ' * 1000 + '\nVerification:\nF1 refuted: source guard\nRemaining disagreements:\nF2 unverified'
clipped = debate_excerpt(status, 1024)
assert 'F1 refuted' in clipped and 'F2 unverified' in clipped

for provider, usage, cached, included in (
    ('claude', {'input_tokens': 12, 'output_tokens': 3, 'cache_read_input_tokens': 100, 'cache_creation_input_tokens': 50}, 100, False),
    ('codex', {'input_tokens': 112, 'output_tokens': 3, 'cached_input_tokens': 100}, 100, True),
    ('codex', {'input_tokens': True, 'output_tokens': -1}, None, True),
):
    detail = json.loads(usage_footer(provider, [usage])[-1].split(': ', 1)[1])
    assert detail['cache_read_tokens'] == cached
    assert detail['cache_in_input'] is included
    assert detail['reported_cost_usd'] is None
assert json.loads(usage_footer('codex', [{}], float('nan'))[-1].split(': ', 1)[1])['reported_cost_usd'] is None

with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp).resolve()
    source = root / 'source'
    source.mkdir()
    (source / 'request.md').write_text('sanitized question', encoding='utf-8')
    (source / 'answer-0.md').write_text('F1 refuted', encoding='utf-8')
    (root / 'isolated').mkdir()
    stage_context(source, root / 'isolated', '.oms/artifacts/council-context.test')
    assert (root / 'isolated/.oms/artifacts/council-context.test/answer-0.md').read_text() == 'F1 refuted'
    (source / 'answer-1.md').symlink_to(source / 'request.md')
    try:
        stage_context(source, root / 'rejected', '.oms/artifacts/council-context.test')
    except ValueError:
        pass
    else:
        raise AssertionError('symlinked evidence must be refused')
    assert not (root / 'rejected').exists()
    (source / 'answer-1.md').unlink()
    for rel in ('../outside', '/absolute', '.oms/../outside'):
        try:
            stage_context(source, root / 'rejected', rel)
        except ValueError:
            pass
        else:
            raise AssertionError(rel)
assert debate_excerpt('Answer:\nshort\nEvidence:\nfile.py:1', 1024).endswith('file.py:1')

real = 'Answer:\nDo not ship: unsafe execution.\nEvidence:\nsrc/runner.py:42\nRisks:\nuntrusted input\n'
for quoted in ('```text\nAnswer:\nship now\n```',
               '~~~~text\n```\nAnswer:\nship now\n~~~~',
               '> Answer:\n> ship now', '    Answer:\n    ship now',
               '  \tAnswer:\n  \tship now'):
    clipped = debate_excerpt(real + quoted, 1024)
    assert 'Do not ship' in clipped and 'src/runner.py:42' in clipped, clipped

long = 'Answer:\n' + 'important claim ' * 1000 + '\nEvidence:\na.py:1\nRisks:\nrace\nRecommendation:\nwait'
clipped = debate_excerpt(long, 4096)
assert 4000 <= len(clipped.encode()) <= 4096, len(clipped.encode())
for heading in ('## {name}:', '**{name}:**', '**{name}**:', '### {name}', '## **{name}:**'):
    marked = long
    for name in ('Answer', 'Evidence', 'Risks', 'Recommendation'):
        marked = marked.replace(name + ':', heading.format(name=name))
    clipped = debate_excerpt(marked, 1024)
    assert len(clipped.encode()) <= 1024 and 'important claim' in clipped, clipped
    assert 'a.py:1' in clipped and 'race' in clipped and 'wait' in clipped, clipped
    stable = 'Answer:\nposition\n' + heading.format(name='Changed from previous round') + '\nNone.\nRemaining disagreements:\nrace'
    assert debate_unchanged(stable), stable
    assert not debate_unchanged(stable.replace('None.', 'none\nCorrection: a race.'))

assert not debate_unchanged('Answer:\nposition\n```text\nChanged from previous round:\nnone\n```')
assert not debate_unchanged('Answer:\nposition\nChanged from previous round:\nnone\nChanged from previous round:\nunchanged')
assert not debate_unchanged('Answer:\nposition\nChanged from previous round:\nnone\nOther correction:\nrace')
assert not debate_unchanged('Answer:\nposition\nChanged from previous round:\nnone\n```text\nRemaining disagreements:\nrace\n```')
PY

echo "debate-delta-smoke: ok"
