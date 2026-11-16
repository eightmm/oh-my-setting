#!/usr/bin/env bash
set -euo pipefail

# Verify pure-Rust source distribution contracts, markdown reference integrity,
# workflow gate safety, and zero retired interpreter leakage — without Python.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"

DOCS_ONLY=0
case "${1:-}" in
  "") ;;
  --docs-only) DOCS_ONLY=1 ;;
  *) echo "usage: source-distribution-smoke.sh [--docs-only]" >&2; exit 2 ;;
esac

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

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
        rust/*/src/*|rust/target/*) continue ;;
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

check_markdown_links
if [ "$DOCS_ONLY" = 1 ]; then
  echo "source-distribution-smoke (--docs-only): ok"
  exit 0
fi

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
OMS_NATIVE_OUTCOME=absent bash "$ROOT/scripts/oms" output-digest -- \
  bash "$ROOT/scripts/install-skills.sh" --help >/dev/null ||
  fail "scripts/oms leaked OMS_NATIVE_OUTCOME into native child process"
if grep -Fq 'oms run-capsule' "$ROOT/scripts/init.sh"; then
  fail "oms init still recommends removed run-capsule front door"
fi
if grep -Fq -- '--effort' "$ROOT/custom-skills/oms-agent-harness/references/model-routing.md"; then
  fail "model routing skill advertises wrong public effort option"
fi

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
toolchain="$ROOT/rust/rust-toolchain.toml"
grep -Fq 'channel = "1.99.1"' "$toolchain" ||
  fail "rust-toolchain.toml must stay pinned to 1.99.1"

echo "source-distribution-smoke: ok"
