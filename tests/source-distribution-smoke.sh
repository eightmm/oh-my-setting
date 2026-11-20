#!/usr/bin/env bash
set -euo pipefail

# Verify pure-Rust source distribution contracts, markdown reference integrity,
# workflow gate safety, and zero retired interpreter leakage — without Python.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"

DOCS_ONLY=0
PLANNING_ONLY=0
case "${1:-}" in
  "") ;;
  --docs-only) DOCS_ONLY=1 ;;
  --planning-only) PLANNING_ONLY=1 ;;
  *) echo "usage: source-distribution-smoke.sh [--docs-only|--planning-only]" >&2; exit 2 ;;
esac

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

check_affected_planning() (
  check_probe="$(mktemp -d "${TMPDIR:-/tmp}/oms-affected-plan.XXXXXX")"
  trap 'rm -rf "$check_probe"' EXIT HUP INT TERM
  repo="$check_probe/repo"
  mkdir -p "$repo/scripts/lib" "$check_probe/bin" "$check_probe/home" "$check_probe/tmp"
  cp "$ROOT/scripts/check.sh" "$repo/scripts/check.sh"
  cp "$ROOT/scripts/lib/critical-paths.sh" "$repo/scripts/lib/critical-paths.sh"
  # The planning contract must not enter maintenance or native setup at all.
  # Ordinary execution still has to reach that gate; its sentinel stops there.
  cat > "$repo/scripts/lib/check-maintenance.sh" <<'EOF'
printf 'maintenance-source\n' >> "$OMS_CHECK_FORBIDDEN_LOG"
oms_check_maintenance_begin() {
  printf 'maintenance-begin\n' >> "$OMS_CHECK_FORBIDDEN_LOG"
  return 97
}
EOF
  cat > "$repo/scripts/lib/platform.sh" <<'EOF'
printf 'platform-source\n' >> "$OMS_CHECK_FORBIDDEN_LOG"
oms_native_resolve() { return 1; }
oms_native_stage() { cargo --version; }
EOF
  ln -s "$BASH" "$check_probe/bin/bash"
  for blocked in cargo rustup curl wget codex claude agy opencode ntn; do
    cat > "$check_probe/bin/$blocked" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${0##*/}" >> "$OMS_CHECK_FORBIDDEN_LOG"
exit 99
EOF
    chmod +x "$check_probe/bin/$blocked"
  done
  invoke_check() {
    env -i HOME="$check_probe/home" PATH="$check_probe/bin:/usr/bin:/bin" \
      TMPDIR="$check_probe/tmp" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
      OMS_CHECK_FORBIDDEN_LOG="$check_probe/forbidden.log" OMS_NATIVE_NO_AUTO_STAGE=0 \
      bash "$repo/scripts/check.sh" "$@"
  }
  print_plan() { invoke_check --affected --print-affected-mode "$@"; }
  expected_ci_record() { printf 'mode=%s\nnative=%s\npython=false\noperator=\n' "$1" "$2"; }
  oms_smoke_make_repo "$repo"
  printf 'base\n' > "$repo/README.md"
  git -C "$repo" add .
  git -C "$repo" commit -qm base
  base="$(git -C "$repo" rev-parse HEAD)"
  printf 'documentation\n' >> "$repo/README.md"
  git -C "$repo" commit -qam documentation
  docs="$(git -C "$repo" rev-parse HEAD)"
  selected="$(print_plan --changed-from "$base" --changed-to "$docs")"
  [ "$selected" = affected ] || fail "documentation planning selected: $selected"
  print_plan --changed-from "$base" --changed-to "$docs" --ci-output "$check_probe/docs.out"
  expected_ci_record affected false > "$check_probe/expected.out"
  cmp -s "$check_probe/docs.out" "$check_probe/expected.out" || fail "documentation CI selection changed"

  mkdir -p "$repo/crates/oms/src"
  printf 'fn main() {}\n' > "$repo/crates/oms/src/main.rs"
  git -C "$repo" add .
  git -C "$repo" commit -qm native
  native="$(git -C "$repo" rev-parse HEAD)"
  selected="$(print_plan --changed-from "$docs" --changed-to "$native")"
  [ "$selected" = full ] || fail "native planning selected: $selected"
  print_plan --changed-from "$docs" --changed-to "$native" --ci-output "$check_probe/native.out"
  expected_ci_record full true > "$check_probe/expected.out"
  cmp -s "$check_probe/native.out" "$check_probe/expected.out" || fail "native CI selection changed"

  # The first critical path selects the reason, but later paths still contribute
  # native CI coverage even when an earlier contract file already selected full.
  printf 'security policy\n' > "$repo/SECURITY.md"
  printf 'fn helper() {}\n' >> "$repo/crates/oms/src/main.rs"
  git -C "$repo" add .
  git -C "$repo" commit -qm mixed-contract-native
  mixed="$(git -C "$repo" rev-parse HEAD)"
  print_plan --changed-from "$native" --changed-to "$mixed" --ci-output "$check_probe/mixed.out" \
    2> "$check_probe/mixed.err"
  cmp -s "$check_probe/mixed.out" "$check_probe/expected.out" || fail "earlier critical path hid later native changes"
  grep -Fxq 'affected: full test fallback (contract-boundary:SECURITY.md)' "$check_probe/mixed.err" \
    || fail "mixed planning did not preserve the first critical reason"

  git -C "$repo" checkout -q --detach "$docs"
  printf 'untracked\n' > "$repo/dirty"
  selected="$(print_plan --changed-from "$base" --changed-to "$docs")"
  [ "$selected" = full ] || fail "dirty-worktree planning no longer selects full verification"
  rm "$repo/dirty"

  status=0
  invoke_check --print-affected-mode > "$check_probe/invalid.out" 2>&1 || status=$?
  [ "$status" -eq 2 ] || fail "print mode without --affected must fail before setup"
  [ ! -s "$check_probe/forbidden.log" ] || fail "planning reached maintenance, build, or external tools"
  status=0
  invoke_check --affected --changed-from "$base" --changed-to "$docs" \
    > "$check_probe/execution.out" 2>&1 || status=$?
  [ "$status" -eq 97 ] || fail "actual affected verification bypassed the maintenance gate"
  grep -Fxq maintenance-begin "$check_probe/forbidden.log" || fail "execution omitted maintenance protection"
)

check_markdown_links() {
  local md dir raw clean resolved broken=0
  while IFS= read -r -d '' md; do
    [ -f "$ROOT/$md" ] || continue
    dir="${md%/*}"
    [ "$dir" != "$md" ] || dir=""
    while IFS= read -r raw; do
      clean="${raw%%#*}"
      clean="${clean%% *}"
      [ -n "$clean" ] || continue
      case "$clean" in
        http://*|https://*|mailto:*|tel:*|\#*|file://*) continue ;;
        crates/*/src/*|target/*) continue ;;
      esac
      if [ -n "$dir" ]; then
        resolved="$dir/$clean"
      else
        resolved="$clean"
      fi
      # Normalize relative ./ and ../ segments with awk
      resolved="$(awk -v p="$resolved" 'BEGIN {
        n = split(p, a, "/"); sp = 0
        for (i = 1; i <= n; i++) {
          if (a[i] == "" || a[i] == ".") continue
          if (a[i] == "..") { if (sp > 0) sp--; else { print "ESCAPE"; exit } }
          else st[++sp] = a[i]
        }
        out = ""
        for (i = 1; i <= sp; i++) out = (out == "" ? st[i] : out "/" st[i])
        print out
      }')"
      if [ "$resolved" = "ESCAPE" ] || { [ ! -e "$ROOT/$resolved" ]; }; then
        echo "broken markdown link in $md -> $clean ($resolved)" >&2
        broken=$((broken + 1))
      fi
    done < <(awk '
      {
        line = $0
        if (line ~ /^[[:space:]]*(```|~~~)/) { in_fence = !in_fence; next }
        if (in_fence) next
        while (match(line, /\]\([^()[:space:]<>]+\)/)) {
          link = substr(line, RSTART + 2, RLENGTH - 3)
          print link
          line = substr(line, RSTART + RLENGTH)
        }
      }
    ' "$ROOT/$md")
  done < <(git -C "$ROOT" ls-files -z -- '*.md')
  [ "$broken" -eq 0 ] || fail "found $broken broken relative markdown link(s)"
}

if [ "$PLANNING_ONLY" = 1 ]; then
  check_affected_planning
  echo "source-distribution-smoke (--planning-only): ok"
  exit 0
fi
check_markdown_links
if [ "$DOCS_ONLY" = 1 ]; then
  echo "source-distribution-smoke (--docs-only): ok"
  exit 0
fi
check_affected_planning

# 1. Zero retired .py files in scripts/, tests/, docs/
leftover_py="$(git -C "$ROOT" ls-files -- 'scripts/*.py' 'scripts/**/*.py' 'tests/*.py' 'tests/**/*.py' 'docs/*.py' 'docs/**/*.py')"
[ -z "$leftover_py" ] || fail "retired Python sources remain tracked: $leftover_py"

# 2. Shared namespace prefix and Korean coding-instructions invariants
for surface in custom-skills templates/project-skills; do
  [ -d "$ROOT/$surface" ] || continue
  for entry in "$ROOT/$surface"/*/; do
    [ -d "$entry" ] || continue
    case "$(basename "$entry")" in
      oms-*) : ;;
      *) fail "$surface entry lacks the oms- prefix: $(basename "$entry")" ;;
    esac
  done
done
for entry in "$ROOT/output-styles"/*.md; do
  [ -f "$entry" ] || continue
  case "$(basename "$entry")" in
    oms-*) : ;;
    *) fail "output style lacks the oms- prefix: $(basename "$entry")" ;;
  esac
done
while IFS= read -r skill_name; do
  case "$skill_name" in
    oms-*) ;;
    *) fail "skills.manifest.json name lacks oms- prefix: $skill_name" ;;
  esac
done < <(jq -r '.skills[].name' "$ROOT/skills.manifest.json")
while IFS= read -r skill_src; do
  case "${skill_src##*/}" in
    oms-*) ;;
    *) fail "skills.manifest.json source basename lacks oms- prefix: $skill_src" ;;
  esac
done < <(jq -r '.skills[].source' "$ROOT/skills.manifest.json")
grep -Fxq 'keep-coding-instructions: true' "$ROOT/output-styles/oms-korean.md" ||
  fail "language style must preserve native Claude coding instructions"

# 3. Install entrypoint rejects OH_MY_SETTING_INSTALL_TOOLS=0 before network
install_probe="$(mktemp -d "${TMPDIR:-/tmp}/oms-required-tools.XXXXXX")"
trap 'rm -rf "$install_probe"' EXIT HUP INT TERM
mkdir -p "$install_probe/bin" "$install_probe/home"
for blocked in git curl; do
  cat > "$install_probe/bin/$blocked" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$0 $*" >> "$OMS_TEST_NETWORK_LOG"
exit 99
EOF
  chmod +x "$install_probe/bin/$blocked"
done
install_status=0
install_out="$(HOME="$install_probe/home" PATH="$install_probe/bin:/usr/bin:/bin" \
  OMS_TEST_NETWORK_LOG="$install_probe/network.log" \
  OH_MY_SETTING_DIR="$install_probe/checkout" OH_MY_SETTING_INSTALL_TOOLS=0 \
  bash "$ROOT/install.sh" 2>&1)" || install_status=$?
[ "$install_status" -eq 2 ] ||
  fail "OH_MY_SETTING_INSTALL_TOOLS=0 must be rejected cleanly: $install_out"
printf '%s' "$install_out" | grep -Fq 'tool installation is required' ||
  fail "tool-install rejection did not explain the required contract"
[ ! -s "$install_probe/network.log" ] ||
  fail "tool-install rejection reached a network-facing command"

# 4. Every standalone smoke suite is reachable from scripts/check.sh or workflow
for suite in "$ROOT"/tests/*-smoke.sh; do
  base="${suite##*/}"
  grep -Fq "tests/$base" "$ROOT/scripts/check.sh" "$ROOT/.github/workflows/test.yml" ||
    fail "standalone smoke suite is not wired into check.sh or test.yml: tests/$base"
done

# 5. Effort option contract & doc guardrails
for command in agent-call peer-ask peer-review peer-delegate plan-run; do
  help="$(bash "$ROOT/scripts/$command.sh" --help 2>&1)" || fail "$command --help failed"
  grep -Fq 'xhigh' <<< "$help" || fail "$command help omits xhigh effort"
  grep -Fq 'max' <<< "$help" || fail "$command help omits max effort"
  grep -Fq 'ultra' <<< "$help" || fail "$command help omits ultra effort"
done
OMS_NATIVE_OUTCOME=absent bash "$ROOT/scripts/oms" output-digest --no-store -- \
  bash "$ROOT/scripts/install-skills.sh" --help >/dev/null ||
  fail "scripts/oms leaked OMS_NATIVE_OUTCOME into native child process"
if grep -Fq 'oms run-capsule' "$ROOT/scripts/init.sh"; then
  fail "oms init still recommends removed run-capsule front door"
fi
if grep -Fq -- '--effort' "$ROOT/custom-skills/oms-agent-harness/references/model-routing.md"; then
  fail "model routing skill advertises wrong public effort option"
fi
grep -Fq 'Spec stage first' "$ROOT/custom-skills/oms-agent-harness/references/review-gates.md" ||
  fail "review gates reference lost the spec-compliance stage before deep review"

# 6. CI workflow invariants (gate completeness, no setup-python, no windows-latest)
workflow="$ROOT/.github/workflows/test.yml"
if grep -Fq 'setup-python' "$workflow"; then
  fail "streamlined CI workflow must not install setup-python"
fi
if grep -Fq 'windows-latest' "$workflow"; then
  fail "native Windows is unsupported in CI"
fi
grep -Fq 'bsd-portability-smoke.sh' "$workflow" ||
  fail "macOS portability job must run bsd-portability-smoke.sh"
grep -Fq 'gate:' "$workflow" ||
  fail "workflow must expose a fixed gate job"
grep -Fq 'if: always()' "$workflow" ||
  fail "gate must evaluate all needs even on failure/cancellation"
grep -Fq 'tests/*.sh' "$ROOT/scripts/check.sh" ||
  fail "shellcheck lint must cover tests/*.sh"
toolchain="$ROOT/rust-toolchain.toml"
grep -Fq 'channel = "1.99.1"' "$toolchain" ||
  fail "rust-toolchain.toml must stay pinned to 1.99.1"

echo "source-distribution-smoke: ok"
