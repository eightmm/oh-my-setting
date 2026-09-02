#!/usr/bin/env bash
set -euo pipefail

# Focused suite for the graph layer: the unittest modules, the `oms graph`
# front door against a temporary fixture repository, and a dogfood build of
# this repository from a `git archive` copy. Nothing here writes into this
# checkout's own .oms (the check.sh state guard forbids it), so every build
# targets a temporary --repo.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PYTHONDONTWRITEBYTECODE=1
OMS="$ROOT/scripts/oms"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

PYTHONPATH="$ROOT/scripts/lib" python3 -m unittest discover -v -s "$ROOT/tests" -p "test_oms_graph_*.py"

# Three separate trees: the fixture repository stays pristine (captured output
# would otherwise become discovered source), the work directory holds every
# assertion artifact, and the dogfood copy holds this repository's own bytes.
tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-graph-fixture.XXXXXX")"
work="$(mktemp -d "${TMPDIR:-/tmp}/oms-graph-work.XXXXXX")"
dog="$(mktemp -d "${TMPDIR:-/tmp}/oms-graph-dogfood.XXXXXX")"
# Two more copies of the fixture: the auto-build assertions need a repository
# nobody has built a graph in, and the session-start hook needs one of its own
# because its build lands in the background.
auto="$(mktemp -d "${TMPDIR:-/tmp}/oms-graph-auto.XXXXXX")"
hook="$(mktemp -d "${TMPDIR:-/tmp}/oms-graph-hook.XXXXXX")"
trap 'rm -rf "$tmp" "$work" "$dog" "$auto" "$hook"' EXIT INT TERM HUP

mkdir -p "$tmp/scripts/lib" "$tmp/docs" "$tmp/tests"
git -C "$tmp" init -q -b main
git -C "$tmp" config user.email test@example.com
git -C "$tmp" config user.name test

cat > "$tmp/alpha.py" <<'EOF'
"""Alpha module."""


def alpha_entry(value):
    return value * 2
EOF
cat > "$tmp/beta.py" <<'EOF'
"""Beta module."""

from alpha import alpha_entry


class BetaRunner:
    def run(self, value):
        return alpha_entry(value)
EOF
cat > "$tmp/scripts/lib/common.sh" <<'EOF'
#!/usr/bin/env bash
say_hello() { echo hello; }
EOF
cat > "$tmp/scripts/run.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
. scripts/lib/common.sh
say_hello
EOF
# Path literals need a directory separator to be recognized as repo paths.
cat > "$tmp/docs/OVERVIEW.md" <<'EOF'
# Overview

The entrypoint is scripts/run.sh and the library is scripts/lib/common.sh.
EOF
cat > "$tmp/tests/smoke-test.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
bash scripts/run.sh
EOF

# Copy the six fixture files before anything builds a graph over them.
cp -R "$tmp/." "$auto/"

graph_rc=0
run_graph() {  # run_graph OUTPUT ARGS...
  out="$1"
  shift
  graph_rc=0
  "$OMS" graph --repo "$tmp" "$@" > "$out" 2>&1 || graph_rc=$?
}

run_graph "$work/build.out" project build
[ "$graph_rc" -eq 0 ] || fail "build exited $graph_rc: $(cat "$work/build.out")"
grep -Eq '^graph: built revision=[0-9a-f]{12} files=6 nodes=[0-9]+ edges=[0-9]+ cached=0 parsed=6 skipped=0$' \
  "$work/build.out" || fail "build summary is not the one-line contract: $(cat "$work/build.out")"

run_graph "$work/check-fresh.out" project check
[ "$graph_rc" -eq 0 ] || fail "fresh check exited $graph_rc: $(cat "$work/check-fresh.out")"
grep -Fqx 'fresh' "$work/check-fresh.out" || fail "fresh check did not say fresh: $(cat "$work/check-fresh.out")"

# Working-tree bytes decide freshness: an unstaged edit is stale, and the
# reader learns which path to rebuild.
printf '\n\ndef alpha_extra(value):\n    return value\n' >> "$tmp/alpha.py"
run_graph "$work/check-stale.out" project check
[ "$graph_rc" -eq 3 ] || fail "stale check exited $graph_rc, expected 3: $(cat "$work/check-stale.out")"
grep -Fq 'stale: alpha.py' "$work/check-stale.out" || fail "stale check did not name the path: $(cat "$work/check-stale.out")"
run_graph "$work/check-stale.json" project check --json
[ "$graph_rc" -eq 3 ] || fail "stale --json check exited $graph_rc, expected 3"
python3 - "$work/check-stale.json" <<'PY'
import json
import sys
row = json.load(open(sys.argv[1]))
assert row["present"] is True, row
assert row["fresh"] is False, row
assert row["stale"] == ["alpha.py"], row
PY

# Only the edited file is re-parsed; every other extraction comes from cache.
run_graph "$work/rebuild.out" project build
[ "$graph_rc" -eq 0 ] || fail "rebuild exited $graph_rc: $(cat "$work/rebuild.out")"
grep -Fq 'cached=5 parsed=1 ' "$work/rebuild.out" || fail "rebuild was not incremental: $(cat "$work/rebuild.out")"

run_graph "$work/map.json" project map --json
[ "$graph_rc" -eq 0 ] || fail "map --json exited $graph_rc: $(cat "$work/map.json")"
python3 - "$work/map.json" <<'PY'
import json
import sys
summary = json.load(open(sys.argv[1]))
for key in ("revision", "counts", "hubs", "groups"):
    assert key in summary, (key, sorted(summary))
assert summary["counts"]["kind"].get("function"), summary["counts"]
# Test files are dropped from the overview by default; --include-tests keeps them.
assert not summary["counts"]["kind"].get("test"), summary["counts"]
assert summary["hubs"] and set(summary["hubs"][0]) == {"id", "kind", "degree"}, summary["hubs"]
PY

run_graph "$work/analyze.json" project analyze --hubs 3 --cycles --communities --path 'file:beta.py' 'file:alpha.py' --json
[ "$graph_rc" -eq 0 ] || fail "analyze --json exited $graph_rc: $(cat "$work/analyze.json")"
python3 - "$work/analyze.json" <<'PY'
import json
import sys
row = json.load(open(sys.argv[1]))
assert 0 < len(row["hubs"]) <= 3, row["hubs"]
assert set(row["hubs"][0]) == {"id", "kind", "degree"}, row["hubs"][0]
assert row["components"] and row["components"][0]["size"] >= 2, row["components"]
assert isinstance(row["cycles"], list), row
assert row["communities"] and row["communities"][0]["members"], row["communities"]
assert row["path"] and row["path"][0] == "file:beta.py", row["path"]
PY

run_graph "$work/map-tests.json" project map --json --include-tests
[ "$graph_rc" -eq 0 ] || fail "map --include-tests exited $graph_rc: $(cat "$work/map-tests.json")"
python3 - "$work/map-tests.json" <<'PY'
import json
import sys
summary = json.load(open(sys.argv[1]))
assert summary["counts"]["kind"].get("test"), summary["counts"]
PY

run_graph "$work/find.out" project find alpha_entry --limit 5
[ "$graph_rc" -eq 0 ] || fail "find exited $graph_rc: $(cat "$work/find.out")"
grep -Fq 'symbol:alpha.py::alpha_entry  function  alpha.py  ' "$work/find.out" \
  || fail "find rows are not id/kind/path/score: $(cat "$work/find.out")"
run_graph "$work/find.json" project find alpha_entry --limit 5 --json
python3 - "$work/find.json" <<'PY'
import json
import sys
rows = json.load(open(sys.argv[1]))
assert isinstance(rows, list) and rows, rows
assert rows[0]["id"] == "symbol:alpha.py::alpha_entry", rows[0]
assert rows[0]["score"] > 0, rows[0]
PY

run_graph "$work/neighbors.out" project neighbors 'symbol:alpha.py::alpha_entry'
[ "$graph_rc" -eq 0 ] || fail "neighbors exited $graph_rc: $(cat "$work/neighbors.out")"
grep -Fqx 'in  calls  EXTRACTED  symbol:beta.py::BetaRunner.run' "$work/neighbors.out" \
  || fail "neighbors rows are not direction/relation/confidence/id: $(cat "$work/neighbors.out")"

run_graph "$work/trace.out" project trace 'file:beta.py' --depth 2
[ "$graph_rc" -eq 0 ] || fail "trace exited $graph_rc: $(cat "$work/trace.out")"
grep -Fqx 'file:beta.py' "$work/trace.out" || fail "trace omitted its root: $(cat "$work/trace.out")"
grep -Fqx '  module:alpha.py  via imports' "$work/trace.out" \
  || fail "trace did not indent by distance with via: $(cat "$work/trace.out")"

run_graph "$work/blast.out" project blast --path scripts/run.sh
[ "$graph_rc" -eq 0 ] || fail "blast exited $graph_rc: $(cat "$work/blast.out")"
grep -Fqx 'path: scripts/run.sh' "$work/blast.out" || fail "blast omitted its seed path: $(cat "$work/blast.out")"
grep -Fqx 'seed: file:scripts/run.sh' "$work/blast.out" || fail "blast omitted its seed node: $(cat "$work/blast.out")"
grep -Fq 'test:tests/smoke-test.sh' "$work/blast.out" || fail "blast omitted the dependent test: $(cat "$work/blast.out")"
grep -Fqx '  tests/smoke-test.sh' "$work/blast.out" || fail "blast omitted its tests section: $(cat "$work/blast.out")"
run_graph "$work/blast.json" project blast --path scripts/run.sh --json
python3 - "$work/blast.json" <<'PY'
import json
import sys
row = json.load(open(sys.argv[1]))
assert row["paths"] == ["scripts/run.sh"], row["paths"]
assert row["seeds"] == ["file:scripts/run.sh"], row["seeds"]
assert row["tests"] == ["tests/smoke-test.sh"], row["tests"]
assert set(row["changed_paths"]) == {"changed", "untracked"}, row["changed_paths"]
PY

run_graph "$work/context.json" project context --task "alpha entry helper for the beta runner" --max-files 3 --json
[ "$graph_rc" -eq 0 ] || fail "context exited $graph_rc: $(cat "$work/context.json")"
python3 - "$work/context.json" "$tmp" <<'PY'
import json
import os
import sys
pack = json.load(open(sys.argv[1]))
assert 0 < len(pack["files"]) <= 3, pack["files"]
assert "alpha.py" in pack["files"], pack["files"]
assert len(pack["evidence"]) == len(pack["files"]), pack["evidence"]
assert pack["pack_path"].startswith(".oms/project-graph/context/"), pack["pack_path"]
assert os.path.isfile(os.path.join(sys.argv[2], pack["pack_path"])), pack["pack_path"]
PY

# --- auto-build -------------------------------------------------------------
# A graph nobody built is not the reader's problem: every reader ensures one
# first. The summary goes to stderr so a --json stdout stays parsable.
auto_rc=0
"$OMS" graph --repo "$auto" project find alpha_entry \
  > "$work/auto-find.out" 2> "$work/auto-find.err" || auto_rc=$?
[ "$auto_rc" -eq 0 ] || fail "find without a graph exited $auto_rc: $(cat "$work/auto-find.err")"
grep -Eq '^graph: built revision=[0-9a-f]{12} files=6 ' "$work/auto-find.err" \
  || fail "the auto-build summary did not reach stderr: $(cat "$work/auto-find.err")"
grep -Fq 'symbol:alpha.py::alpha_entry' "$work/auto-find.out" \
  || fail "find did not answer from the graph it built: $(cat "$work/auto-find.out")"
if grep -Fq 'graph:' "$work/auto-find.out"; then
  fail "the auto-build summary polluted stdout: $(cat "$work/auto-find.out")"
fi

# An edit is refreshed through the cache, so a stale graph costs one re-parse.
printf '\n\ndef alpha_more(value):\n    return value\n' >> "$auto/alpha.py"
auto_rc=0
"$OMS" graph --repo "$auto" project map --json \
  > "$work/auto-map.json" 2> "$work/auto-map.err" || auto_rc=$?
[ "$auto_rc" -eq 0 ] || fail "map after an edit exited $auto_rc: $(cat "$work/auto-map.err")"
grep -Eq '^graph: refreshed revision=[0-9a-f]{12} .* cached=5 parsed=1 ' "$work/auto-map.err" \
  || fail "the reader did not refresh incrementally: $(cat "$work/auto-map.err")"
python3 - "$work/auto-map.json" <<'PY'
import json
import sys
summary = json.load(open(sys.argv[1]))
assert summary["counts"]["kind"].get("function"), summary["counts"]
PY

run_graph_auto() {  # run_graph_auto OUTPUT ARGS...
  out="$1"
  shift
  auto_rc=0
  "$OMS" graph --repo "$auto" "$@" > "$out" 2>&1 || auto_rc=$?
}

run_graph_auto "$work/auto-ensure.out" project ensure
[ "$auto_rc" -eq 0 ] || fail "ensure on a current graph exited $auto_rc: $(cat "$work/auto-ensure.out")"
grep -Eqx 'graph: fresh revision=[0-9a-f]{12}' "$work/auto-ensure.out" \
  || fail "ensure did not report a current graph: $(cat "$work/auto-ensure.out")"

# The opt-out restores the old contract: an absent graph is the reader's
# error, with the hint that names the verb to run.
auto_rc=0
OMS_GRAPH_AUTOBUILD=0 OMS_PROJECT_GRAPH_STATE="$work/no-autobuild" \
  "$OMS" graph --repo "$auto" project find alpha_entry > "$work/no-autobuild.out" 2>&1 || auto_rc=$?
[ "$auto_rc" -eq 2 ] || fail "opt-out find exited $auto_rc, expected 2: $(cat "$work/no-autobuild.out")"
grep -Fq 'project graph has not been built; run: oms graph project build' "$work/no-autobuild.out" \
  || fail "the opt-out refusal lost its hint: $(cat "$work/no-autobuild.out")"

# --no-refresh reads the graph as it stands and leaves it stale.
printf '\n\ndef alpha_again(value):\n    return value\n' >> "$auto/alpha.py"
auto_rc=0
"$OMS" graph --repo "$auto" project map --json --no-refresh \
  > "$work/no-refresh.json" 2> "$work/no-refresh.err" || auto_rc=$?
[ "$auto_rc" -eq 0 ] || fail "--no-refresh map exited $auto_rc: $(cat "$work/no-refresh.err")"
[ ! -s "$work/no-refresh.err" ] || fail "--no-refresh still refreshed: $(cat "$work/no-refresh.err")"
run_graph_auto "$work/no-refresh-check.out" project check
[ "$auto_rc" -eq 3 ] || fail "--no-refresh rebuilt the graph anyway: $(cat "$work/no-refresh-check.out")"
grep -Fq 'stale: alpha.py' "$work/no-refresh-check.out" \
  || fail "--no-refresh check lost the stale path: $(cat "$work/no-refresh-check.out")"

# The file bound belongs to the auto-build alone: the explicit `build` (which
# bounds bytes, not files) has none, and a refresh is never refused.
auto_rc=0
OMS_PROJECT_GRAPH_STATE="$work/bounded" "$OMS" graph --repo "$auto" project ensure --max-files 1 \
  > "$work/bounded.out" 2>&1 || auto_rc=$?
[ "$auto_rc" -eq 2 ] || fail "the bounded ensure exited $auto_rc, expected 2: $(cat "$work/bounded.out")"
grep -Fq '6 files exceed the 1-file bound' "$work/bounded.out" \
  || fail "the refusal did not name the bound: $(cat "$work/bounded.out")"
auto_rc=0
OMS_PROJECT_GRAPH_STATE="$work/bounded" "$OMS" graph --repo "$auto" project build \
  > "$work/bounded-build.out" 2>&1 || auto_rc=$?
[ "$auto_rc" -eq 0 ] || fail "the explicit build inherited the file bound: $(cat "$work/bounded-build.out")"
auto_rc=0
OMS_PROJECT_GRAPH_STATE="$work/bounded" "$OMS" graph --repo "$auto" project ensure --max-files 1 \
  > "$work/bounded-again.out" 2>&1 || auto_rc=$?
[ "$auto_rc" -eq 0 ] || fail "an existing graph was refused a refresh: $(cat "$work/bounded-again.out")"

# --- session-start hook -----------------------------------------------------
# The hook reports the graph and starts an absent one in the background; it
# never blocks and never fails session start.
hook_out="$(printf '{"session_id":"me","cwd":"%s"}' "$tmp" | "$ROOT/scripts/resume-hook.sh")" \
  || fail "the resume hook must exit 0"
printf '%s\n' "$hook_out" | grep -Eq '^- graph: fresh \([0-9a-f]{12}\)$' \
  || fail "the hook did not report a current graph: $hook_out"
hook_out="$(printf '{"session_id":"me","cwd":"%s"}' "$auto" |
  OMS_GRAPH_AUTOBUILD=0 "$ROOT/scripts/resume-hook.sh")" || fail "the resume hook must exit 0"
printf '%s\n' "$hook_out" | grep -Fq -- '- graph: absent (OMS_GRAPH_AUTOBUILD=0)' \
  || fail "the hook ignored the auto-build opt-out: $hook_out"
cp -R "$auto/." "$hook/"
rm -rf "$hook/.oms/project-graph"
hook_out="$(printf '{"session_id":"me","cwd":"%s"}' "$hook" | "$ROOT/scripts/resume-hook.sh")" \
  || fail "the resume hook must exit 0"
printf '%s\n' "$hook_out" | grep -Fq -- '- graph: building in the background (oms graph project ensure)' \
  || fail "the hook did not start a background build: $hook_out"
# Reap the detached build before the trap removes its repository. The line
# above is the contract; landing inside this window is not asserted.
for _ in 1 2 3 4 5 6 7 8 9 10; do
  if [ -f "$hook/.oms/project-graph/graph.json" ]; then break; fi
  sleep 1
done

# A delegated harness child may build the cache it needs — a regenerable graph
# carries no authority and an isolated worktree has no other way to get one —
# but the context pack a parent's brief owns stays parent-only.
child_rc=0
OMS_HARNESS_CHILD=1 "$OMS" graph --repo "$tmp" project build > "$work/child-build.out" 2>&1 || child_rc=$?
[ "$child_rc" -eq 0 ] || fail "child build exited $child_rc: $(cat "$work/child-build.out")"
child_rc=0
OMS_HARNESS_CHILD=1 "$OMS" graph --repo "$tmp" project ensure > "$work/child-ensure.out" 2>&1 || child_rc=$?
[ "$child_rc" -eq 0 ] || fail "child ensure exited $child_rc: $(cat "$work/child-ensure.out")"
child_rc=0
OMS_HARNESS_CHILD=1 "$OMS" graph --repo "$tmp" project context --task alpha \
  > "$work/child-context.out" 2>&1 || child_rc=$?
[ "$child_rc" -eq 2 ] || fail "child context exited $child_rc, expected 2: $(cat "$work/child-context.out")"
grep -Fq 'a harness child may only use read-only graph actions' "$work/child-context.out" \
  || fail "child refusal was not actionable: $(cat "$work/child-context.out")"
child_rc=0
OMS_HARNESS_CHILD=1 "$OMS" graph --repo "$tmp" project map --json > "$work/child-map.json" 2>&1 || child_rc=$?
[ "$child_rc" -eq 0 ] || fail "child read exited $child_rc: $(cat "$work/child-map.json")"

# The state override keeps a cache out of the inspected tree; a relative value
# is refused rather than resolved against an unknown working directory.
state_rc=0
OMS_PROJECT_GRAPH_STATE="$work/altstate" "$OMS" graph --repo "$tmp" project build \
  > "$work/altstate.out" 2>&1 || state_rc=$?
[ "$state_rc" -eq 0 ] || fail "override build exited $state_rc: $(cat "$work/altstate.out")"
[ -f "$work/altstate/graph.json" ] || fail "override build did not write the named state directory"
state_rc=0
OMS_PROJECT_GRAPH_STATE=relative/state "$OMS" graph --repo "$tmp" project check \
  > "$work/relstate.out" 2>&1 || state_rc=$?
[ "$state_rc" -eq 2 ] || fail "relative override exited $state_rc, expected 2: $(cat "$work/relstate.out")"
grep -Fq 'OMS_PROJECT_GRAPH_STATE must be an absolute path' "$work/relstate.out" \
  || fail "relative override refusal was not actionable: $(cat "$work/relstate.out")"

# Dogfood this repository from an exported copy: never build inside $ROOT,
# whose .oms is guarded by check.sh.
git -C "$ROOT" archive HEAD > "$work/dogfood.tar"
tar -x -f "$work/dogfood.tar" -C "$dog"
dog_rc=0
"$OMS" graph --repo "$dog" project build > "$work/dog-build1.out" 2>&1 || dog_rc=$?
[ "$dog_rc" -eq 0 ] || fail "dogfood build exited $dog_rc: $(cat "$work/dog-build1.out")"
dog_files="$(sed -n 's/^graph: built .* files=\([0-9][0-9]*\) .*$/\1/p' "$work/dog-build1.out")"
[ -n "$dog_files" ] || fail "dogfood build summary had no file count: $(cat "$work/dog-build1.out")"
[ "$dog_files" -ge 100 ] || fail "dogfood build covered only $dog_files files"
dog_rc=0
"$OMS" graph --repo "$dog" project build > "$work/dog-build2.out" 2>&1 || dog_rc=$?
[ "$dog_rc" -eq 0 ] || fail "second dogfood build exited $dog_rc: $(cat "$work/dog-build2.out")"
dog_cached="$(sed -n 's/^graph: built .* cached=\([0-9][0-9]*\) .*$/\1/p' "$work/dog-build2.out")"
dog_files2="$(sed -n 's/^graph: built .* files=\([0-9][0-9]*\) .*$/\1/p' "$work/dog-build2.out")"
[ "$dog_cached" = "$dog_files2" ] || fail "second dogfood build re-parsed: cached=$dog_cached files=$dog_files2"

# --- execution graph --------------------------------------------------------
# A second repository with a real plan: the execution verbs read plan and
# receipt facts, so an empty tree would exercise nothing. No provider is
# needed here — `exec run` only ever appears as --dry-run.
exec_repo="$(mktemp -d "${TMPDIR:-/tmp}/oms-graph-exec.XXXXXX")"
trap 'rm -rf "$tmp" "$work" "$dog" "$auto" "$hook" "$exec_repo"' EXIT INT TERM HUP

exec_rc=0
# The gate scrubs the invoking session's own harness identity: a run that
# inherits OMS_HARNESS_CHILD=1 loses agent-plan init/add and every exec writer.
run_exec() {  # run_exec OUTPUT ARGS...
  out="$1"
  shift
  exec_rc=0
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_ORIGIN OMS_HARNESS_PARENT_AGENT \
      OMS_HARNESS_CALL_ID OMS_STATE_REPO OMS_ATTEMPT_ID OMS_PLAN_LEASE_ID \
      OMS_LEASE_ID OMS_EXECUTOR_ID OMS_SOUL_SHA256 OMS_APPROVAL_ID \
      OMS_LANDING_ID OMS_WORKER_AUTHORITY_EXCLUSIVE
    OMS_LOCK_DIR="$work/locks" OMS_WORK_JOURNAL=0 \
      "$@"
  ) > "$out" 2>&1 || exec_rc=$?
}

run_graph_exec() {  # run_graph_exec OUTPUT ARGS...
  out="$1"
  shift
  run_exec "$out" "$OMS" graph --repo "$exec_repo" "$@"
}

git -C "$exec_repo" init -q -b main
git -C "$exec_repo" config user.email test@example.com
git -C "$exec_repo" config user.name test
mkdir -p "$exec_repo/src"
printf 'base\n' > "$exec_repo/README.md"
git -C "$exec_repo" add README.md
git -C "$exec_repo" commit -qm base

run_exec "$work/plan-init.out" bash "$ROOT/scripts/agent-plan.sh" --repo "$exec_repo" \
  init --goal "graph smoke" --accept true
[ "$exec_rc" -eq 0 ] || fail "plan init exited $exec_rc: $(cat "$work/plan-init.out")"
run_exec "$work/plan-add.out" bash "$ROOT/scripts/agent-plan.sh" --repo "$exec_repo" \
  add --id implement --title implement --allowed src/ --verify true
[ "$exec_rc" -eq 0 ] || fail "plan add exited $exec_rc: $(cat "$work/plan-add.out")"

for spec_name in coding-change goal-drive; do
  run_graph_exec "$work/validate-$spec_name.out" exec validate "$spec_name"
  [ "$exec_rc" -eq 0 ] || fail "validate $spec_name exited $exec_rc: $(cat "$work/validate-$spec_name.out")"
  grep -Fqx 'ok' "$work/validate-$spec_name.out" || fail "validate $spec_name did not say ok: $(cat "$work/validate-$spec_name.out")"
done

cat > "$work/broken.json" <<'EOF'
{"schema": 1, "id": "broken", "entry": "work",
 "budget": {"max_steps": 4, "max_repeats": 1},
 "nodes": {"work": {"kind": "tool", "command": "true"}, "done": {"kind": "terminal"}},
 "edges": [{"from": "work", "to": "ghost", "outcomes": ["completed"]}]}
EOF
run_graph_exec "$work/validate-broken.out" exec validate "$work/broken.json"
[ "$exec_rc" -eq 3 ] || fail "invalid spec exited $exec_rc, expected 3: $(cat "$work/validate-broken.out")"
grep -Fq 'unknown_endpoint' "$work/validate-broken.out" \
  || fail "invalid spec did not name unknown_endpoint: $(cat "$work/validate-broken.out")"

run_graph_exec "$work/render.out" exec render coding-change
[ "$exec_rc" -eq 0 ] || fail "render exited $exec_rc: $(cat "$work/render.out")"
grep -Fq 'inspect' "$work/render.out" || fail "render omitted the entry node: $(cat "$work/render.out")"
grep -Fq '↻' "$work/render.out" || fail "render did not mark a repeat edge: $(cat "$work/render.out")"
run_graph_exec "$work/render-mermaid.out" exec render coding-change --mermaid
[ "$exec_rc" -eq 0 ] || fail "render --mermaid exited $exec_rc: $(cat "$work/render-mermaid.out")"
head -n 1 "$work/render-mermaid.out" | grep -Fqx 'flowchart TD' \
  || fail "mermaid render is not a flowchart: $(head -n 1 "$work/render-mermaid.out")"

run_graph_exec "$work/route.out" exec route coding-change --outcomes '{"inspect":"completed"}'
[ "$exec_rc" -eq 0 ] || fail "route exited $exec_rc: $(cat "$work/route.out")"
grep -Fq 'implement' "$work/route.out" || fail "route did not name the next node: $(cat "$work/route.out")"

run_graph_exec "$work/fixtures.out" exec test "$ROOT/tests/fixtures/graph-routes"
[ "$exec_rc" -eq 0 ] || fail "fixture corpus exited $exec_rc: $(cat "$work/fixtures.out")"
grep -Fq 'PASS ' "$work/fixtures.out" || fail "fixture corpus reported no result: $(cat "$work/fixtures.out")"

run_graph_exec "$work/dry-run.json" exec run coding-change --worker codex --dry-run --json
[ "$exec_rc" -eq 0 ] || fail "dry run exited $exec_rc: $(cat "$work/dry-run.json")"
python3 - "$work/dry-run.json" <<'PY'
import json
import sys
row = json.load(open(sys.argv[1]))
assert row["dry_run"] is True, row
assert row["route"]["status"] == "actionable", row["route"]
assert "trace" not in row["route"], row["route"]
assert row["eligible"] == ["inspect"], row["eligible"]
PY

# Nothing has run yet: status is a reader that says so instead of inventing one.
run_graph_exec "$work/status-empty.out" exec status
[ "$exec_rc" -eq 3 ] || fail "empty status exited $exec_rc, expected 3: $(cat "$work/status-empty.out")"

run_graph_exec "$work/shadow.json" exec shadow --json
[ "$exec_rc" -eq 0 ] || fail "shadow exited $exec_rc: $(cat "$work/shadow.json")"
python3 - "$work/shadow.json" <<'PY'
import json
import sys
row = json.load(open(sys.argv[1]))
assert isinstance(row["agree"], bool), row
assert row["kind"] == "graph-route-shadow", row
assert set(row["control_plane"]) == {"action", "mapped"}, row["control_plane"]
PY
shadow_lines="$(wc -l < "$exec_repo/.oms/graph/shadow.jsonl" | tr -d ' ')"
[ "$shadow_lines" = "1" ] || fail "shadow ledger holds $shadow_lines rows, expected 1"
python3 - "$work/shadow.json" <<'PY'
import json
import sys
row = json.load(open(sys.argv[1]))
# Reconstruction against reality: the ready task is bound and the unproven
# check is assumed failed, so the frontier is the effectful work, not the entry.
assert row["route"]["primary"] == "implement", row["route"]
assert row["reconstructed"]["assumed_failed"] == ["acceptance"], row["reconstructed"]
assert row["reconstructed"]["bindings"] == {"work_item": "implement"}, row["reconstructed"]
assert row["basis"] in ("", "frontier", "successor", "blocked"), row
PY

# The session-start hook records one shadow row for a repository with a plan
# and reports the frontier on its own line; the ledger is ambient to the gate.
hook_out="$(printf '{"session_id":"me","cwd":"%s"}' "$exec_repo" |
  OMS_GRAPH_AUTOBUILD=0 OMS_LOCK_DIR="$work/locks" OMS_WORK_JOURNAL=0 "$ROOT/scripts/resume-hook.sh")" \
  || fail "the resume hook must exit 0 with a plan present"
printf '%s\n' "$hook_out" | grep -Eq -- '^- graph route: implement \((agrees|disagrees) with runtime next [a-z_-]+\)$' \
  || fail "the hook did not report the graph route: $hook_out"
shadow_lines="$(wc -l < "$exec_repo/.oms/graph/shadow.jsonl" | tr -d ' ')"
[ "$shadow_lines" = "2" ] || fail "the hook did not append exactly one shadow row (rows: $shadow_lines)"
hook_out="$(printf '{"session_id":"me","cwd":"%s"}' "$exec_repo" |
  OMS_GRAPH_AUTOBUILD=0 OMS_GRAPH_SHADOW=0 OMS_LOCK_DIR="$work/locks" OMS_WORK_JOURNAL=0 "$ROOT/scripts/resume-hook.sh")" \
  || fail "the resume hook must exit 0"
if printf '%s\n' "$hook_out" | grep -Fq -- '- graph route:'; then
  fail "OMS_GRAPH_SHADOW=0 must suppress the shadow line: $hook_out"
fi
python3 - "$ROOT/scripts/lib/oms-state-inventory.py" "$exec_repo/.oms" <<'PY'
import subprocess
import sys
listing = subprocess.run([sys.executable, sys.argv[1], sys.argv[2]], capture_output=True, text=True, check=True).stdout
assert "graph/shadow.jsonl" not in listing, listing
PY

# A delegated child may evaluate a route but never record a comparison.
child_rc=0
OMS_HARNESS_CHILD=1 "$OMS" graph --repo "$exec_repo" exec route coding-change \
  > "$work/child-route.out" 2>&1 || child_rc=$?
[ "$child_rc" -eq 0 ] || fail "child route exited $child_rc: $(cat "$work/child-route.out")"
child_rc=0
OMS_HARNESS_CHILD=1 "$OMS" graph --repo "$exec_repo" exec shadow \
  > "$work/child-shadow.out" 2>&1 || child_rc=$?
[ "$child_rc" -eq 2 ] || fail "child shadow exited $child_rc, expected 2: $(cat "$work/child-shadow.out")"
grep -Fq 'a harness child may only use read-only graph actions' "$work/child-shadow.out" \
  || fail "child shadow refusal was not actionable: $(cat "$work/child-shadow.out")"

# A run-less repository reaches the MCP reader as an error result, not a crash.
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"oms_execution_graph_status","arguments":{"repo":"%s"}}}\n' "$exec_repo"
} | python3 "$ROOT/scripts/oms-mcp-server.py" > "$work/mcp-status.out" 2>&1
python3 - "$work/mcp-status.out" <<'PY'
import json
import sys
text = open(sys.argv[1], encoding="utf-8").read()
assert "Traceback" not in text, text
rows = [json.loads(line) for line in text.splitlines() if line.strip()]
result = [row for row in rows if row.get("id") == 2][0]["result"]
assert result["isError"] is True, result
PY

# One recorded run through the front door, so the readers the MCP tools wrap
# are exercised against real events. Tool nodes only: no provider is involved.
cat > "$work/gate-spec.json" <<'EOF'
{"schema": 1, "id": "smoke-gate", "entry": "work",
 "budget": {"max_steps": 6, "max_repeats": 1},
 "nodes": {"work": {"kind": "tool", "effect": "read", "command": "echo work"},
           "review": {"kind": "gate", "authority": "parent", "decisions": ["approved", "changes_requested"]},
           "done": {"kind": "terminal"}},
 "edges": [{"from": "work", "to": "review", "outcomes": ["completed"]},
           {"from": "review", "to": "done", "outcomes": ["approved"]},
           {"from": "review", "to": "work", "outcomes": ["changes_requested"], "kind": "repeat"}]}
EOF
run_graph_exec "$work/run-gate.out" exec run "$work/gate-spec.json" --worker codex
[ "$exec_rc" -eq 3 ] || fail "gated run exited $exec_rc, expected 3: $(cat "$work/run-gate.out")"
grep -Fq 'status=gate primary=review' "$work/run-gate.out" \
  || fail "gated run did not stop at the gate: $(cat "$work/run-gate.out")"

run_graph_exec "$work/status-run.json" exec status --json
[ "$exec_rc" -eq 0 ] || fail "status exited $exec_rc: $(cat "$work/status-run.json")"
exec_run_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["run_id"])' "$work/status-run.json")"
[ -n "$exec_run_id" ] || fail "status reported no run id: $(cat "$work/status-run.json")"

run_graph_exec "$work/events.out" exec events --run "$exec_run_id" --limit 4
[ "$exec_rc" -eq 0 ] || fail "events exited $exec_rc: $(cat "$work/events.out")"
grep -Fq 'node_outcome work completed' "$work/events.out" \
  || fail "events did not record the tool outcome: $(cat "$work/events.out")"

run_graph_exec "$work/route-run.out" exec route --run "$exec_run_id"
[ "$exec_rc" -eq 0 ] || fail "route --run exited $exec_rc: $(cat "$work/route-run.out")"
grep -Fq 'gate review' "$work/route-run.out" || fail "route --run lost the gate: $(cat "$work/route-run.out")"

# Only a gate records a decision, and only one of its declared outcomes.
run_graph_exec "$work/decide-bad.out" exec decide --run "$exec_run_id" --node work --outcome approved
[ "$exec_rc" -eq 2 ] || fail "deciding a non-gate exited $exec_rc, expected 2: $(cat "$work/decide-bad.out")"
run_graph_exec "$work/decide.out" exec decide --run "$exec_run_id" --node review --outcome approved --note reviewed
[ "$exec_rc" -eq 0 ] || fail "decide exited $exec_rc: $(cat "$work/decide.out")"
run_graph_exec "$work/resume.out" exec resume --run "$exec_run_id" --worker codex
[ "$exec_rc" -eq 0 ] || fail "resume exited $exec_rc: $(cat "$work/resume.out")"
grep -Fq 'status=terminal' "$work/resume.out" || fail "resume did not reach the terminal: $(cat "$work/resume.out")"

echo "graph-smoke: ok"
