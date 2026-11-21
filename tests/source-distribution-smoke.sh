#!/usr/bin/env bash
set -euo pipefail

# Verify pure-Rust source distribution contracts, markdown reference integrity,
# workflow gate safety, and zero retired interpreter leakage — without Python.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh disable=SC1091
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
  mkdir -p "$repo/scripts/lib" "$repo/tests/lib" "$check_probe/bin" "$check_probe/home" "$check_probe/tmp"
  cp "$ROOT/scripts/check.sh" "$repo/scripts/check.sh"
  cp "$ROOT/scripts/lib/critical-paths.sh" "$repo/scripts/lib/critical-paths.sh"
  cp "$ROOT/tests/source-distribution-smoke.sh" "$repo/tests/source-distribution-smoke.sh"
  cp "$ROOT/tests/lib/smoke-env.sh" "$repo/tests/lib/smoke-env.sh"
  # The planning contract must not enter maintenance or native setup at all.
  # Ordinary execution still has to reach that gate; its sentinel stops there
  # unless OMS_CHECK_MAINT_ALLOW=1 lets the actual docs-only check run.
  cat > "$repo/scripts/lib/check-maintenance.sh" <<'EOF'
printf 'maintenance-source\n' >> "$OMS_CHECK_FORBIDDEN_LOG"
oms_check_maintenance_begin() {
  printf 'maintenance-begin\n' >> "$OMS_CHECK_FORBIDDEN_LOG"
  [ "${OMS_CHECK_MAINT_ALLOW:-0}" = 1 ] && return 0
  return 97
}
oms_check_maintenance_end() {
  printf 'maintenance-end\n' >> "$OMS_CHECK_FORBIDDEN_LOG"
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
      OMS_CHECK_FORBIDDEN_LOG="${OMS_CHECK_FORBIDDEN_LOG:-$check_probe/forbidden.log}" \
      OMS_CHECK_MAINT_ALLOW="${OMS_CHECK_MAINT_ALLOW:-0}" OMS_NATIVE_NO_AUTO_STAGE=0 \
      bash "$repo/scripts/check.sh" "$@"
  }
  print_plan() { invoke_check --affected --print-affected-mode "$@"; }
  expected_ci_record() {
    printf 'mode=%s\nnative=%s\npython=false\noperator=\ndocs_only=%s\n' "$1" "$2" "$3"
  }
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
  expected_ci_record affected false true > "$check_probe/expected.out"
  cmp -s "$check_probe/docs.out" "$check_probe/expected.out" || fail "documentation CI selection changed"

  selected="$(invoke_check --affected --list-stages --changed-from "$base" --changed-to "$docs")"
  [ "$selected" = "$(printf 'doc-references\ncheck: ok')" ] || fail "documentation-only stages are not reference checks only: $selected"

  mkdir -p "$repo/crates/oms/src"
  printf 'fn main() {}\n' > "$repo/crates/oms/src/main.rs"
  git -C "$repo" add .
  git -C "$repo" commit -qm native
  native="$(git -C "$repo" rev-parse HEAD)"
  selected="$(print_plan --changed-from "$docs" --changed-to "$native")"
  [ "$selected" = full ] || fail "native planning selected: $selected"
  print_plan --changed-from "$docs" --changed-to "$native" --ci-output "$check_probe/native.out"
  expected_ci_record full true false > "$check_probe/expected.out"
  cmp -s "$check_probe/native.out" "$check_probe/expected.out" || fail "native CI selection changed"

  # Full --affected --ci-parity runs each unique required check once while
  # preserving --no-lint, --quick, --lint-only, and non-parity full affected.
  selected="$(invoke_check --affected --ci-parity --list-stages --changed-from "$docs" --changed-to "$native" 2>/dev/null)"
  expected_stages="$(printf 'shellcheck\nskill-manifest\ndoc-references\nrust-migration-check\ninvariants-source-distribution\ncheck: ok')"
  [ "$selected" = "$expected_stages" ] || fail "full --affected --ci-parity duplicated or changed stages: $selected"

  selected="$(invoke_check --affected --ci-parity --no-lint --list-stages --changed-from "$docs" --changed-to "$native" 2>/dev/null)"
  expected_stages="$(printf 'invariants-skill-manifest\ninvariants-doc-references\ninvariants-migration-check\ninvariants-source-distribution\ncheck: ok')"
  [ "$selected" = "$expected_stages" ] || fail "--affected --ci-parity --no-lint lost invariants: $selected"

  selected="$(invoke_check --quick --list-stages --changed-from "$docs" --changed-to "$native" 2>/dev/null)"
  [ "$selected" = "$expected_stages" ] || fail "--quick stages changed: $selected"

  selected="$(invoke_check --affected --list-stages --changed-from "$docs" --changed-to "$native" 2>/dev/null)"
  if ! grep -Fxq 'shellcheck' <<< "$selected" || ! grep -Fxq 'source-distribution' <<< "$selected" ||
    ! grep -Fxq 'rust-workspace-tests' <<< "$selected" || grep -Fxq 'invariants-source-distribution' <<< "$selected"; then
    fail "full --affected (without --ci-parity) stages changed: $selected"
  fi

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

  mkdir -p "$repo/roles"
  printf 'builder contract\n' > "$repo/roles/builder.md"
  git -C "$repo" add roles/builder.md
  git -C "$repo" commit -qm role
  role="$(git -C "$repo" rev-parse HEAD)"
  print_plan --changed-from "$mixed" --changed-to "$role" --ci-output "$check_probe/role.out"
  expected_ci_record affected false false > "$check_probe/expected.out"
  cmp -s "$check_probe/role.out" "$check_probe/expected.out" || fail "scoped role planning must retain Rust setup"

  git -C "$repo" checkout -q --detach "$docs"
  printf 'untracked\n' > "$repo/dirty"
  selected="$(print_plan --changed-from "$base" --changed-to "$docs")"
  [ "$selected" = full ] || fail "dirty-worktree planning no longer selects full verification"
  print_plan --changed-from "$base" --changed-to "$docs" --ci-output "$check_probe/dirty.out"
  expected_ci_record full true false > "$check_probe/expected.out"
  cmp -s "$check_probe/dirty.out" "$check_probe/expected.out" || fail "dirty planning allowed docs-only setup"
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

  # --ci-output without --print-affected-mode can execute real affected checks,
  # so it must not bypass maintenance protection.
  status=0
  OMS_CHECK_FORBIDDEN_LOG="$check_probe/ci-exec.log" \
    invoke_check --affected --changed-from "$base" --changed-to "$docs" --ci-output "$check_probe/ci-exec.out" \
    > "$check_probe/ci-exec.stdout" 2>&1 || status=$?
  [ "$status" -eq 97 ] || fail "--ci-output execution bypassed the maintenance gate (status=$status)"
  grep -Fxq maintenance-begin "$check_probe/ci-exec.log" || fail "--ci-output execution omitted maintenance protection"

  # Clean docs-only affected execution keeps maintenance protection and runs
  # markdown reference checks without resolving or building native binaries.
  status=0
  OMS_CHECK_MAINT_ALLOW=1 OMS_CHECK_FORBIDDEN_LOG="$check_probe/docs-exec.log" \
    invoke_check --affected --changed-from "$base" --changed-to "$docs" \
    > "$check_probe/docs-exec.out" 2>&1 || status=$?
  [ "$status" -eq 0 ] || fail "clean docs-only affected execution failed without Cargo (status=$status): $(cat "$check_probe/docs-exec.out")"
  grep -Fq 'ok: doc-references' "$check_probe/docs-exec.out" || fail "clean docs-only execution skipped doc-references"
  grep -Fxq 'check: ok' "$check_probe/docs-exec.out" || fail "clean docs-only execution did not finish cleanly"
  grep -Fxq maintenance-begin "$check_probe/docs-exec.log" || fail "clean docs-only execution omitted maintenance begin"
  grep -Fxq maintenance-end "$check_probe/docs-exec.log" || fail "clean docs-only execution omitted maintenance end"
  ! grep -Eq '^(platform-source|cargo|rustup)$' "$check_probe/docs-exec.log" ||
    fail "clean docs-only execution touched native staging or Cargo: $(cat "$check_probe/docs-exec.log")"

  # Dirty worktree must fall back to full verification (reaching native staging),
  # never taking the docs-only no-Cargo fast path.
  printf 'untracked\n' > "$repo/dirty"
  status=0
  OMS_CHECK_MAINT_ALLOW=1 OMS_CHECK_FORBIDDEN_LOG="$check_probe/dirty-exec.log" \
    invoke_check --affected --changed-from "$base" --changed-to "$docs" \
    > "$check_probe/dirty-exec.out" 2>&1 || status=$?
  rm -f "$repo/dirty"
  [ "$status" -ne 0 ] || fail "dirty worktree unexpectedly passed with blocked tools"
  grep -Fq 'affected: full test fallback (workspace-differs-from-head)' "$check_probe/dirty-exec.out" ||
    fail "dirty worktree did not fall back to full verification: $(cat "$check_probe/dirty-exec.out")"
  grep -Fxq maintenance-begin "$check_probe/dirty-exec.log" || fail "dirty fallback omitted maintenance protection"
  if ! grep -Fxq platform-source "$check_probe/dirty-exec.log" || ! grep -Fxq cargo "$check_probe/dirty-exec.log"; then
    fail "dirty fallback skipped native staging: $(cat "$check_probe/dirty-exec.log")"
  fi

  # Preflight readiness must match the lanes: lint runs Cargo (oms-migrate), so
  # ShellCheck alone (blocked rustup/cargo stubs above) leaves no lane runnable.
  printf '#!/usr/bin/env bash\nexit 0\n' > "$check_probe/bin/shellcheck"
  chmod +x "$check_probe/bin/shellcheck"
  selected="$(invoke_check --lint-only --list-stages 2>/dev/null)"
  expected_stages="$(printf 'shellcheck\nskill-manifest\ndoc-references\nrust-migration-check\ncheck: ok')"
  [ "$selected" = "$expected_stages" ] || fail "--lint-only stages changed: $selected"
  cp "$ROOT/rust-toolchain.toml" "$repo/rust-toolchain.toml"
  status=0
  invoke_check --preflight > "$check_probe/preflight.out" 2>&1 || status=$?
  [ "$status" -eq 1 ] || fail "preflight without the pinned toolchain must report missing prerequisites"
  grep -Fxq 'ok       shellcheck (shellcheck)' "$check_probe/preflight.out" || fail "preflight missed the ShellCheck stub"
  ! grep -q '^  yes ' "$check_probe/preflight.out" || fail "preflight claims a lane runnable without the pinned toolchain: $(cat "$check_probe/preflight.out")"
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
