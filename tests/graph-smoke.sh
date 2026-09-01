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
trap 'rm -rf "$tmp" "$work" "$dog"' EXIT INT TERM HUP

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
for key in ("kinds", "languages", "hubs", "modules"):
    assert key in summary, (key, sorted(summary))
assert summary["kinds"].get("function"), summary["kinds"]
assert summary["kinds"].get("test"), summary["kinds"]
assert summary["hubs"] and set(summary["hubs"][0]) == {"id", "degree"}, summary["hubs"]
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

# A delegated harness child may read the graph but never write one, and the
# refusal has to be actionable.
child_rc=0
OMS_HARNESS_CHILD=1 "$OMS" graph --repo "$tmp" project build > "$work/child-build.out" 2>&1 || child_rc=$?
[ "$child_rc" -eq 2 ] || fail "child build exited $child_rc, expected 2: $(cat "$work/child-build.out")"
grep -Fq 'a harness child may only use read-only graph actions' "$work/child-build.out" \
  || fail "child refusal was not actionable: $(cat "$work/child-build.out")"
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

echo "graph-smoke: ok"
