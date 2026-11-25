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
  for lib in file-lock poll cargo-target-lock; do cp "$ROOT/scripts/lib/$lib.sh" "$repo/scripts/lib/$lib.sh"; done
  cp "$ROOT/verify" "$repo/verify"
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
      OMS_AFFECTED_FULL="${OMS_AFFECTED_FULL:-0}" OMS_CARGO_TARGET_WAIT="${OMS_CARGO_TARGET_WAIT:-3600}" \
      bash "$repo/scripts/check.sh" "$@"
  }
  print_plan() { invoke_check --affected --print-affected-mode "$@"; }
  expected_ci_record() {
    printf 'mode=%s\nnative=%s\ndocs_only=%s\n' "$1" "$2" "$3"
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

  # Direct non-critical crate README changes are ordinary docs, while mixed
  # code, nested READMEs, non-README crate docs, deletes, renames, critical
  # crates, and forced full stay strict.
  mkdir -p "$repo/crates/oms-json"
  printf '# oms-json\n' > "$repo/crates/oms-json/README.md"
  git -C "$repo" add crates/oms-json/README.md
  git -C "$repo" commit -qm crate-readme
  crate_readme="$(git -C "$repo" rev-parse HEAD)"
  selected="$(print_plan --changed-from "$docs" --changed-to "$crate_readme")"
  [ "$selected" = affected ] || fail "direct crate README planning selected: $selected"
  print_plan --changed-from "$docs" --changed-to "$crate_readme" --ci-output "$check_probe/crate-readme.out"
  expected_ci_record affected false true > "$check_probe/expected.out"
  cmp -s "$check_probe/crate-readme.out" "$check_probe/expected.out" || fail "direct crate README CI selection changed: $(cat "$check_probe/crate-readme.out")"
  selected="$(invoke_check --affected --list-stages --changed-from "$docs" --changed-to "$crate_readme")"
  [ "$selected" = "$(printf 'doc-references\ncheck: ok')" ] || fail "direct crate README stages are not reference checks only: $selected"

  OMS_AFFECTED_FULL=1 print_plan --changed-from "$docs" --changed-to "$crate_readme" --ci-output "$check_probe/crate-readme-forced.out" 2>/dev/null
  expected_ci_record full true false > "$check_probe/expected.out"
  cmp -s "$check_probe/crate-readme-forced.out" "$check_probe/expected.out" || fail "OMS_AFFECTED_FULL=1 did not override direct crate README"

  mkdir -p "$repo/crates/oms-json/sub"
  printf 'nested readme\n' > "$repo/crates/oms-json/sub/README.md"
  git -C "$repo" add crates/oms-json/sub/README.md
  git -C "$repo" commit -qm crate-nested-readme
  crate_nested="$(git -C "$repo" rev-parse HEAD)"
  print_plan --changed-from "$crate_readme" --changed-to "$crate_nested" --ci-output "$check_probe/crate-nested.out" 2>/dev/null
  expected_ci_record full true false > "$check_probe/expected.out"
  cmp -s "$check_probe/crate-nested.out" "$check_probe/expected.out" || fail "nested crate README must stay full/native"

  printf 'extra notes\n' > "$repo/crates/oms-json/NOTES.md"
  git -C "$repo" add crates/oms-json/NOTES.md
  git -C "$repo" commit -qm crate-non-readme
  crate_notes="$(git -C "$repo" rev-parse HEAD)"
  print_plan --changed-from "$crate_nested" --changed-to "$crate_notes" --ci-output "$check_probe/crate-notes.out" 2>/dev/null
  cmp -s "$check_probe/crate-notes.out" "$check_probe/expected.out" || fail "non-README crate markdown must stay full/native"

  mkdir -p "$repo/crates/oms-json/src"
  printf 'updated\n' >> "$repo/crates/oms-json/README.md"
  printf 'pub fn ok() {}\n' > "$repo/crates/oms-json/src/lib.rs"
  git -C "$repo" add crates/oms-json/README.md crates/oms-json/src/lib.rs
  git -C "$repo" commit -qm crate-mixed-code
  crate_mixed="$(git -C "$repo" rev-parse HEAD)"
  print_plan --changed-from "$crate_notes" --changed-to "$crate_mixed" --ci-output "$check_probe/crate-mixed.out" 2>/dev/null
  cmp -s "$check_probe/crate-mixed.out" "$check_probe/expected.out" || fail "mixed crate README + source must stay full/native"

  mkdir -p "$repo/crates/oms-other"
  git -C "$repo" mv crates/oms-json/README.md crates/oms-other/README.md
  git -C "$repo" commit -qm crate-readme-rename
  crate_rename="$(git -C "$repo" rev-parse HEAD)"
  print_plan --changed-from "$crate_mixed" --changed-to "$crate_rename" --ci-output "$check_probe/crate-rename.out" 2>/dev/null
  cmp -s "$check_probe/crate-rename.out" "$check_probe/expected.out" || fail "renamed crate README must stay full/native"

  git -C "$repo" rm -q crates/oms-other/README.md
  git -C "$repo" commit -qm crate-readme-delete
  crate_delete="$(git -C "$repo" rev-parse HEAD)"
  print_plan --changed-from "$crate_rename" --changed-to "$crate_delete" --ci-output "$check_probe/crate-delete.out" 2>/dev/null
  cmp -s "$check_probe/crate-delete.out" "$check_probe/expected.out" || fail "deleted crate README must stay full/native"

  mkdir -p "$repo/crates/oms-bootstrap"
  printf '# bootstrap\n' > "$repo/crates/oms-bootstrap/README.md"
  git -C "$repo" add crates/oms-bootstrap/README.md
  git -C "$repo" commit -qm critical-crate-readme
  critical_crate_readme="$(git -C "$repo" rev-parse HEAD)"
  print_plan --changed-from "$crate_delete" --changed-to "$critical_crate_readme" --ci-output "$check_probe/critical-crate-readme.out" 2>/dev/null
  cmp -s "$check_probe/critical-crate-readme.out" "$check_probe/expected.out" || fail "critical crate README must remain full/native"

  git -C "$repo" checkout -q --detach "$docs"

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
  rust_stages="$(grep -E '^rust-(fmt|clippy|workspace-tests)$' <<< "$selected" || true)"
  expected_rust_stages="$(printf 'rust-fmt\nrust-clippy\nrust-workspace-tests')"
  [ "$rust_stages" = "$expected_rust_stages" ] ||
    fail "full --affected Rust stages must run rust-fmt and rust-clippy before rust-workspace-tests: $selected"

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

  # Runs sharing one physical Cargo target take turns (#508): a waiter names the
  # holder and gives up at its bound before building, and a run nested under
  # the holder does not wait on it. The holder ends when its marker goes, so
  # removing the probe on failure also stops it.
  printf 'untracked\n' > "$repo/dirty"
  # shellcheck disable=SC2016 # the holder script expands its own arguments.
  env -i HOME="$check_probe/home" PATH=/usr/bin:/bin TMPDIR="$check_probe/tmp" "$BASH" -c '
    . "$1/scripts/lib/cargo-target-lock.sh"
    oms_cargo_target_hold "$1" holder || exit
    : > "$2/held"
    while [ -e "$2/held" ]; do sleep 0.1; done
    oms_cargo_target_release' _ "$repo" "$check_probe" &
  holder=$!
  until [ -e "$check_probe/held" ]; do
    kill -0 "$holder" 2>/dev/null || fail "Cargo target holder did not start"
    sleep 0.1
  done
  status=0
  OMS_CHECK_MAINT_ALLOW=1 OMS_CARGO_TARGET_WAIT=1 OMS_CHECK_FORBIDDEN_LOG="$check_probe/turn.log" \
    invoke_check --affected --changed-from "$base" --changed-to "$docs" \
    > "$check_probe/turn.out" 2>&1 || status=$?
  [ "$status" -eq 75 ] || fail "check did not wait for a held Cargo target (status=$status): $(cat "$check_probe/turn.out")"
  grep -Fq "in use by verification pid $holder" "$check_probe/turn.out" || fail "waiting check did not name the holder: $(cat "$check_probe/turn.out")"
  ! grep -Eq '^(platform-source|cargo|rustup)$' "$check_probe/turn.log" || fail "waiting check built before its turn: $(cat "$check_probe/turn.log")"
  # Lint-only runs Cargo too, and --parallel stages before forking lanes.
  printf '#!/usr/bin/env bash\nexit 0\n' > "$check_probe/bin/shellcheck"
  chmod +x "$check_probe/bin/shellcheck"
  for mode in --lint-only --parallel; do
    status=0
    OMS_CHECK_MAINT_ALLOW=1 OMS_CARGO_TARGET_WAIT=1 OMS_CHECK_FORBIDDEN_LOG="$check_probe/turn.log" \
      invoke_check "$mode" > "$check_probe/turn.out" 2>&1 || status=$?
    [ "$status" -eq 75 ] || fail "check $mode did not wait for a held Cargo target (status=$status): $(cat "$check_probe/turn.out")"
  done
  ! grep -Eq '^(platform-source|cargo|rustup|skill-manifest)$' "$check_probe/turn.log" || fail "a waiting check ran Cargo before its turn: $(cat "$check_probe/turn.log")"
  rm "$check_probe/bin/shellcheck"
  invoke_verify() {
    env -i HOME="$check_probe/home" PATH="$check_probe/bin:/usr/bin:/bin" TMPDIR="$check_probe/tmp" \
      OMS_CHECK_FORBIDDEN_LOG="$check_probe/verify-turn.log" OMS_CARGO_TARGET_WAIT=1 "$@" \
      bash "$repo/verify" > "$check_probe/verify-turn.out" 2>&1
  }
  status=0
  invoke_verify || status=$?
  [ "$status" -eq 75 ] || fail "verify did not wait for a held Cargo target (status=$status): $(cat "$check_probe/verify-turn.out")"
  [ ! -s "$check_probe/verify-turn.log" ] || fail "waiting verify ran Cargo before its turn"
  status=0
  invoke_verify OMS_CARGO_TARGET_HELD="$(cd "$repo/target" && pwd -P)" || status=$?
  if [ "$status" -ne 99 ] || ! grep -Fxq cargo "$check_probe/verify-turn.log"; then
    fail "a verify nested under the holder waited on it (status=$status): $(cat "$check_probe/verify-turn.out")"
  fi
  rm "$check_probe/held"
  wait "$holder" || fail "Cargo target holder failed"
  rmdir "$repo/target"

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
  ! compgen -G "$check_probe/home/.cache/oh-my-setting/locks/cargo-target.*" >/dev/null ||
    fail "check kept its Cargo target lock after exit"

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

  mkdir -p "$repo/plugins/oh-my-setting/scripts" "$repo/docs/examples" "$repo/templates"
  touch "$repo/install.sh" "$repo/scripts/oms" "$repo/plugins/oh-my-setting/scripts/fixture.sh" \
    "$repo/docs/examples/fixture.sh" "$repo/templates/fixture.sh"
  cat > "$check_probe/bin/cargo" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  '+1.99.1 run --quiet --manifest-path Cargo.toml -p oms-command-lifecycle --example skill_manifest --locked --offline')
    printf 'skill-manifest\n' >> "$OMS_CHECK_FORBIDDEN_LOG"
    grep -Fxq '{"skills":[]}' skills.manifest.json || { echo 'fixture: malformed skill manifest' >&2; exit 2; }
    ;;
  '+1.99.1 run --quiet --manifest-path Cargo.toml -p oms-migrate --locked --offline -- check --repo .'|\
  '+1.99.1 run --quiet --manifest-path Cargo.toml -p oms-migrate --locked --offline -- policy --repo .')
    printf 'rust-migration-check\n' >> "$OMS_CHECK_FORBIDDEN_LOG"
    ;;
  *) printf 'unexpected-cargo\n' >> "$OMS_CHECK_FORBIDDEN_LOG"; exit 99 ;;
esac
EOF
  lint_log="$check_probe/lint-exec.log"
  expected_lint_log="$(printf 'maintenance-source\nmaintenance-begin\nskill-manifest\nrust-migration-check\nrust-migration-check\nmaintenance-end')"
  printf '{"skills":[]}\n' > "$repo/skills.manifest.json"
  OMS_CHECK_MAINT_ALLOW=1 OMS_CHECK_FORBIDDEN_LOG="$lint_log" invoke_check --lint-only \
    > "$check_probe/lint-exec.out" 2>&1 || fail "lint-only execution failed: $(cat "$check_probe/lint-exec.out")"
  selected="$(sed -E 's/^ok: (.*) \([0-9]+s\)$/\1/' "$check_probe/lint-exec.out")"
  [ "$selected" = "$expected_stages" ] || fail "lint-only execution changed stages: $selected"
  [ "$(cat "$lint_log")" = "$expected_lint_log" ] || fail "lint-only execution reached native staging or missed a Cargo check: $(cat "$lint_log")"

  bad_lint_log="$check_probe/lint-bad.log"
  printf '{"skills":null}\n' > "$repo/skills.manifest.json"
  status=0
  OMS_CHECK_MAINT_ALLOW=1 OMS_CHECK_FORBIDDEN_LOG="$bad_lint_log" invoke_check --lint-only \
    > "$check_probe/lint-bad.out" 2>&1 || status=$?
  [ "$status" -eq 2 ] || fail "malformed lint-only manifest had status $status: $(cat "$check_probe/lint-bad.out")"
  grep -Fq 'check: skill-manifest FAILED' "$check_probe/lint-bad.out" || fail 'malformed lint-only manifest did not fail its stage'
  ! grep -Eq '^ok: (skill-manifest|doc-references|rust-migration-check)|^check: ok$' "$check_probe/lint-bad.out" ||
    fail "lint-only continued past the failed skill-manifest stage: $(cat "$check_probe/lint-bad.out")"
  [ "$(cat "$bad_lint_log")" = "$(printf 'maintenance-source\nmaintenance-begin\nskill-manifest\nmaintenance-end')" ] ||
    fail "lint-only executed unexpected work after a manifest failure: $(cat "$bad_lint_log")"
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
  check_backtick_paths
}

# Convention: an inline `top-dir/.../file.ext` in a root or docs/ Markdown file
# names a repo-root-relative file and must exist. Out of pattern (skipped):
# CHANGELOG.md (history), templates/ docs (project-relative paths), code fences,
# non-crates .rs paths (crate-relative), dotted first segments (except `../`
# traversal with a checked extension), globs/placeholders/fragments (only
# [A-Za-z0-9_.-] segments), and other extensions. Any `..` path segment inside a
# repo-top-dir or `../...` checked-extension reference is rejected as traversal.
check_backtick_paths() {
  local tops md ref stale=0
  tops="$(git -C "$ROOT" ls-files | awk -F/ 'NF > 1 && $1 !~ /^\./ { print $1 }' | sort -u | tr '\n' ' ')"
  while IFS=$'\t' read -r md ref; do
    [ -n "$ref" ] || continue
    case "/$ref/" in
      */../*)
        echo "path traversal in backtick path in $md -> $ref" >&2
        stale=$((stale + 1))
        continue
        ;;
    esac
    [ -e "$ROOT/$ref" ] && continue
    echo "stale backtick path in $md -> $ref" >&2
    stale=$((stale + 1))
  done < <(git -C "$ROOT" ls-files -z -- '*.md' | while IFS= read -r -d '' md; do
    case "$md" in
      CHANGELOG.md) continue ;;
      */*) case "$md" in docs/*.md) ;; *) continue ;; esac ;;
    esac
    [ -f "$ROOT/$md" ] || continue
    awk -v f="$md" -v tops="$tops" '
      BEGIN { n = split(tops, a, " "); for (i = 1; i <= n; i++) top[a[i]] = 1 }
      /^[[:space:]]*(```|~~~)/ { in_fence = !in_fence; next }
      in_fence { next }
      {
        line = $0
        while (match(line, /`[^`]+`/)) {
          t = substr(line, RSTART + 1, RLENGTH - 2)
          line = substr(line, RSTART + RLENGTH)
          if (t !~ /^[A-Za-z0-9_.-]+(\/[A-Za-z0-9_.-]+)+$/) continue
          split(t, seg, "/")
          if (("/" t "/") ~ /\/\.\.\//) {
            if (seg[1] in top || t ~ /\.(md|sh|json|toml|ya?ml|rs)$/) print f "\t" t
            continue
          }
          if (!(seg[1] in top) || seg[1] == "templates") continue
          if (seg[1] == "crates") {
            if (t ~ /\.(md|sh|json|toml|ya?ml|rs)$/) print f "\t" t
          } else if (t ~ /\.(md|sh|json|toml|ya?ml)$/) {
            print f "\t" t
          }
        }
      }' "$ROOT/$md"
  done)
  [ "$stale" -eq 0 ] || fail "found $stale stale backtick repo path(s)"
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
# No OMS test or script runs the retired interpreter itself. The name is split so
# this check does not count as a reference to it.
retired='pyth''on'
interp_use="$(git -C "$ROOT" grep -n -E "#!/usr/bin/(env )?$retired|Command::new\\(\"(/usr/bin/)?${retired}3?\"\\)" -- \
  'crates/*.rs' 'crates/**/*.rs' 'scripts/*.sh' 'scripts/**/*.sh' 'tests/*.sh' 'tests/**/*.sh' || true)"
[ -z "$interp_use" ] || fail "OMS sources run the retired interpreter: $interp_use"

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

# 6. CI workflow invariants (Linux-only CI, gate completeness, no setup-python, no windows/macOS runners)
workflow="$ROOT/.github/workflows/test.yml"
if grep -Fq 'setup-python' "$workflow"; then
  fail "streamlined CI workflow must not install setup-python"
fi
if grep -Fq 'windows-latest' "$workflow"; then
  fail "native Windows is unsupported in CI"
fi
if grep -Fq 'macos-latest' "$workflow" || grep -Fq "runner.os == 'macOS'" "$workflow"; then
  fail "CI workflow must stay Linux-only (no macOS jobs or steps)"
fi
if grep -Fq 'inputs.macos' "$workflow"; then
  fail "CI workflow must not keep schedule/dispatch macOS hooks"
fi
grep -Fq 'lint:' "$workflow" ||
  fail "workflow must keep the lint job"
grep -Fq 'name: rust (ubuntu-24.04)' "$workflow" ||
  fail "workflow must keep the Linux rust (ubuntu-24.04) job"
grep -Fq 'focused:' "$workflow" ||
  fail "workflow must keep the focused job"
grep -Fq 'name: native (ubuntu-latest, copy)' "$workflow" ||
  fail "workflow must keep the Linux native (ubuntu-latest, copy) job"
grep -Fq 'gate:' "$workflow" ||
  fail "workflow must expose a fixed gate job"
grep -Fq 'if: always()' "$workflow" ||
  fail "gate must evaluate all needs even on failure/cancellation"
grep -Fq "rust: \${{ steps.plan.outputs.docs_only == 'true' && 'false' || steps.rust_scope.outputs.required }}" "$workflow" ||
  fail "affected_plan rust output must suppress Rust only when docs_only is proven true"
grep -Fq 'tests/*.sh' "$ROOT/scripts/check.sh" ||
  fail "shellcheck lint must cover tests/*.sh"
toolchain="$ROOT/rust-toolchain.toml"
grep -Fq 'channel = "1.99.1"' "$toolchain" ||
  fail "rust-toolchain.toml must stay pinned to 1.99.1"

echo "source-distribution-smoke: ok"
