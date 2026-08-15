"""Anchored compatibility patchers for the OMS runtime overlay."""
from __future__ import annotations
import os
import shutil
import stat
from pathlib import Path
from typing import List, Optional
COPY_PATHS = ['.github/workflows/test.yml', 'README.md', 'README.ko.md', 'config/capability-profiles.json', 'config/update-channels.json', 'custom-skills/oms-agent-harness/SKILL.md', 'custom-skills/oms-agent-harness/references/runtime-core.md', 'docs/OMS-RUNTIME.md', 'docs/examples/experiment-contract-v2.json', 'docs/examples/remote-adapter.py', 'scripts/check-python.sh', 'scripts/install-profile.sh', 'scripts/oms', 'scripts/runtime-core.sh', 'scripts/lib/oms_core.py', 'scripts/lib/oms_runtime/__init__.py', 'scripts/lib/oms_runtime/benchmark.py', 'scripts/lib/oms_runtime/capsule.py', 'scripts/lib/oms_runtime/cli.py', 'scripts/lib/oms_runtime/cli_parser.py', 'scripts/lib/oms_runtime/common.py', 'scripts/lib/oms_runtime/context.py', 'scripts/lib/oms_runtime/evidence.py', 'scripts/lib/oms_runtime/execution.py', 'scripts/lib/oms_runtime/experiment.py', 'scripts/lib/oms_runtime/experiment_contract.py', 'scripts/lib/oms_runtime/experiment_run.py', 'scripts/lib/oms_runtime/failures.py', 'scripts/lib/oms_runtime/markdown.py', 'scripts/lib/oms_runtime/profiles.py', 'scripts/lib/oms_runtime/projection.py', 'scripts/lib/oms_runtime/release.py', 'tests/install-profile-smoke.sh', 'tests/runtime-core-smoke.sh', 'tests/runtime-core-integration-smoke.sh', 'tests/runtime_test_base.py', 'tests/test_oms_runtime_core.py', 'tests/test_oms_runtime_research.py']

PATCHED_PATHS = ['CHANGELOG.md', 'docs/COMPONENTS.md', 'install.sh', 'scripts/check.sh', 'scripts/update.sh', 'scripts/repo-state.sh', 'scripts/inbox.sh', 'scripts/state-verify.sh']

class ApplyError(RuntimeError):
    pass

def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise ApplyError('%s integration anchor matched %d times, expected exactly once' % (label, count))
    return text.replace(old, new, 1)

def write_text(path: Path, text: str, mode: Optional[int]=None) -> None:
    if path.is_symlink():
        raise ApplyError('refusing to replace symbolic link: %s' % path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name('.%s.oms-runtime.tmp' % path.name)
    temporary.write_text(text, encoding='utf-8', newline='\n')
    if mode is not None:
        temporary.chmod(mode)
    os.replace(str(temporary), str(path))

def copy_overlay_file(source_root: Path, target_root: Path, relative: str) -> None:
    source = source_root / relative
    target = target_root / relative
    if not source.is_file() or source.is_symlink():
        raise ApplyError('overlay source is not a regular file: %s' % relative)
    if target.is_symlink():
        raise ApplyError('target path is a symbolic link: %s' % relative)
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary = target.with_name('.%s.oms-runtime.tmp' % target.name)
    shutil.copyfile(str(source), str(temporary))
    temporary.chmod(stat.S_IMODE(source.stat().st_mode))
    os.replace(str(temporary), str(target))

def patch_check(path: Path) -> None:
    text = path.read_text(encoding='utf-8')
    if 'stage runtime-core bash tests/runtime-core-smoke.sh' in text:
        return
    old = '  stage doctor-surfaces bash tests/doctor-surfaces-smoke.sh\n  stage artifact-supersession bash tests/artifact-supersession-smoke.sh\nfi\n'
    new = '  stage doctor-surfaces bash tests/doctor-surfaces-smoke.sh\n  stage artifact-supersession bash tests/artifact-supersession-smoke.sh\n  stage runtime-core bash tests/runtime-core-smoke.sh\n  stage runtime-core-integration bash tests/runtime-core-integration-smoke.sh\n  stage install-profile bash tests/install-profile-smoke.sh\nfi\n'
    write_text(path, replace_once(text, old, new, 'scripts/check.sh'), 493)

def patch_update(path: Path) -> None:
    text = path.read_text(encoding='utf-8')
    if 'install-profile.sh" --reapply --upgrade' in text:
        return
    old = 'if [ "$SKIP_TOOLS" != "1" ]; then\n  "$ROOT/scripts/install-tools.sh" --upgrade\nfi\n'
    new = 'if [ "$SKIP_TOOLS" != "1" ]; then\n  capability_receipt="${OMS_CAPABILITY_RECEIPT:-${XDG_CONFIG_HOME:-$HOME/.config}/oh-my-setting/capabilities.json}"\n  if [ -f "$capability_receipt" ] && [ ! -L "$capability_receipt" ]; then\n    "$ROOT/scripts/install-profile.sh" --reapply --upgrade --receipt "$capability_receipt"\n  else\n    # Legacy installs have no capability receipt and retain the old full-tool\n    # update contract until an agent explicitly selects a profile.\n    "$ROOT/scripts/install-tools.sh" --upgrade\n  fi\nfi\n'
    write_text(path, replace_once(text, old, new, 'scripts/update.sh'), 493)

def patch_install(path: Path) -> None:
    text = path.read_text(encoding='utf-8')
    if 'OH_MY_SETTING_CAPABILITIES' in text and 'install-profile.sh" "${profile_args' in text:
        return
    old = '# The provider CLIs, gh, and ntn are the executable half of the harness. An\n# initial install without them is an incomplete product, so setup fails rather\n# than recording a partial profile.\nINSTALL_TOOLS="${OH_MY_SETTING_INSTALL_TOOLS:-1}"\nCONNECT_SERVICES="${OH_MY_SETTING_CONNECT_SERVICES:-auto}"\n'
    new = '# Tool installation remains part of setup, but capability profiles decide the\n# minimum locked set. The default core profile installs one provider; `--full`\n# retains the historical all-provider/GitHub/Notion/research footprint.\nINSTALL_TOOLS="${OH_MY_SETTING_INSTALL_TOOLS:-1}"\nCAPABILITY_PROFILES="${OH_MY_SETTING_CAPABILITIES:-core}"\nPRIMARY_PROVIDER="${OH_MY_SETTING_PRIMARY_PROVIDER:-auto}"\nCONNECT_SERVICES="${OH_MY_SETTING_CONNECT_SERVICES:-0}"\n'
    text = replace_once(text, old, new, 'install.sh variables')
    text = replace_once(text, 'Usage: install.sh [--ref REF] [--full] [--connect-services] [--no-connect-services] [--no-auto-update] [--machine-snapshot] [--slurm-snapshot] [--notion-data-source ID] [--peer-permissions] [--star] [--help]\n', 'Usage: install.sh [--ref REF] [--full] [--capability PROFILE] [--primary-provider NAME] [--connect-services] [--no-connect-services] [--no-auto-update] [--machine-snapshot] [--slurm-snapshot] [--notion-data-source ID] [--peer-permissions] [--star] [--help]\n', 'install.sh usage')
    text = replace_once(text, '  --full              Install provider tools, machine snapshot, and update timer.\n  --tools             Install Node, uv, provider CLIs, gh, and ntn (already the default).\n  --connect-services  Require interactive gh and Notion login plus journal linking.\n', '  --full              Select the compatibility full capability profile, machine\n                      snapshot, and update timer.\n  --tools             Compatibility alias for the full capability profile.\n  --capability NAME   Add core, council, github, notion, research, hpc,\n                      container, remote, or full. Repeatable.\n  --primary-provider NAME\n                      auto, codex, claude, or agy for the core profile.\n  --connect-services  Add GitHub and Notion capabilities, then require login and\n                      journal linking.\n', 'install.sh options')
    text = replace_once(text, '  OH_MY_SETTING_PROFILE=NAME       Receipt profile: minimal, full, or custom.\n  OH_MY_SETTING_CLAUDE_HOOKS=0     Skip Claude Code hooks and usage HUD.\n', '  OH_MY_SETTING_PROFILE=NAME       Receipt profile: minimal, full, or custom.\n  OH_MY_SETTING_CAPABILITIES=LIST  Comma-separated capability profiles (default: core).\n  OH_MY_SETTING_PRIMARY_PROVIDER=NAME\n                                   auto, codex, claude, or agy (default: auto).\n  OH_MY_SETTING_CLAUDE_HOOKS=0     Skip Claude Code hooks and usage HUD.\n', 'install.sh environment')
    text = replace_once(text, '  OH_MY_SETTING_CONNECT_SERVICES=auto|required|0\n                                   Auto-connect in an interactive terminal,\n                                   require connection, or skip it (default: auto).\n', '  OH_MY_SETTING_CONNECT_SERVICES=auto|required|0\n                                   Auto-connect, require connection, or skip it\n                                   (default: 0; --connect-services uses required).\n', 'install.sh connect environment')
    text = replace_once(text, '    --full)\n      PROFILE=full\n      INSTALL_TOOLS=1\n      GENERATE_MACHINE=auto\n      GENERATE_SLURM=auto\n      AUTO_UPDATE=1\n      ;;\n    --tools)\n      [ "$PROFILE" = "full" ] || PROFILE=custom\n      INSTALL_TOOLS=1\n      ;;\n    --connect-services)\n', '    --full)\n      PROFILE=full\n      INSTALL_TOOLS=1\n      CAPABILITY_PROFILES=full\n      GENERATE_MACHINE=auto\n      GENERATE_SLURM=auto\n      AUTO_UPDATE=1\n      ;;\n    --tools)\n      [ "$PROFILE" = "full" ] || PROFILE=custom\n      INSTALL_TOOLS=1\n      CAPABILITY_PROFILES=full\n      ;;\n    --capability)\n      [ "$#" -ge 2 ] || { echo "error: --capability requires a value" >&2; exit 2; }\n      [ "$PROFILE" = "full" ] || PROFILE=custom\n      case "$2" in\n        core|council|github|notion|research|hpc|container|remote|full) ;;\n        *) echo "error: unsupported capability profile: $2" >&2; exit 2 ;;\n      esac\n      if [ "$CAPABILITY_PROFILES" = "core" ] && [ "$2" != "core" ]; then\n        CAPABILITY_PROFILES="core,$2"\n      else\n        CAPABILITY_PROFILES="${CAPABILITY_PROFILES:+$CAPABILITY_PROFILES,}$2"\n      fi\n      shift\n      ;;\n    --primary-provider)\n      [ "$#" -ge 2 ] || { echo "error: --primary-provider requires a value" >&2; exit 2; }\n      PRIMARY_PROVIDER="$2"\n      shift\n      ;;\n    --connect-services)\n', 'install.sh option parser')
    text = replace_once(text, 'case "$PEER_PERMISSIONS" in\n  0|1) ;;\n', 'case "$PRIMARY_PROVIDER" in\n  auto|codex|claude|agy) ;;\n  *) echo "error: OH_MY_SETTING_PRIMARY_PROVIDER must be auto, codex, claude, or agy" >&2; exit 2 ;;\nesac\ncase "$CAPABILITY_PROFILES" in\n  ""|,*|*,|*,,*|*[!A-Za-z0-9,_-]*)\n    echo "error: OH_MY_SETTING_CAPABILITIES must be a comma-separated profile list" >&2\n    exit 2\n    ;;\nesac\ncase "$PEER_PERMISSIONS" in\n  0|1) ;;\n', 'install.sh profile validation')
    text = replace_once(text, 'export OH_MY_SETTING_INSTALL_TOOLS="$INSTALL_TOOLS"\nexport OH_MY_SETTING_CONNECT_SERVICES="$CONNECT_SERVICES"\n', 'export OH_MY_SETTING_INSTALL_TOOLS="$INSTALL_TOOLS"\nexport OH_MY_SETTING_CAPABILITIES="$CAPABILITY_PROFILES"\nexport OH_MY_SETTING_PRIMARY_PROVIDER="$PRIMARY_PROVIDER"\nexport OH_MY_SETTING_CONNECT_SERVICES="$CONNECT_SERVICES"\n', 'install.sh exports')
    text = replace_once(text, '"$DEST/scripts/install-tools.sh"\nexport OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-1}"\nload_user_tool_paths\n', 'profile_args=(--apply --primary-provider "$PRIMARY_PROVIDER")\ncapability_list="$CAPABILITY_PROFILES"\nif [ "$PROFILE" = "full" ]; then\n  capability_list=full\nfi\nif [ "$CONNECT_SERVICES" = "required" ]; then\n  capability_list="${capability_list:+$capability_list,}github,notion"\nelif [ -n "$NOTION_DATA_SOURCE_ID" ]; then\n  capability_list="${capability_list:+$capability_list,}notion"\nfi\nold_ifs="$IFS"\nIFS=,\nfor capability in $capability_list; do\n  [ -n "$capability" ] && profile_args+=(--profile "$capability")\ndone\nIFS="$old_ifs"\n"$DEST/scripts/install-profile.sh" "${profile_args[@]}"\n# Requested profiles were checked by install-profile. Bare doctor must not\n# reinterpret unselected adapters as required full-profile tools.\nexport OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-0}"\nload_user_tool_paths\n', 'install.sh tool installer')
    write_text(path, text, 493)

def patch_changelog(path: Path) -> None:
    text = path.read_text(encoding='utf-8')
    marker = '- Compatibility-preserving typed runtime core'
    if marker in text:
        return
    old = '## [Unreleased]\n\n### Added\n'
    new = '## [Unreleased]\n\n### Added\n- Compatibility-preserving typed runtime core with effective TaskEnvelope projection, criterion-linked EvidenceCoverage, bounded ContextManifest artifacts, portable non-authoritative capsules, canonical failure recovery, and content-free effectiveness telemetry.\n- Optional `core`/`council`/`github`/`notion`/`research`/`hpc`/`container`/`remote`/`full` capability profiles backed by the existing locked installer; new installs default to `core`, while old installs without a capability receipt retain full-tool updates.\n- Honest `trusted-local`, container-isolated, and typed remote-adapter execution receipts plus ExperimentContract v2 with fixed seeds, fresh metric enforcement, no-regression checks, invariant packs, and comparable summaries.\n- Stable/edge update-channel manifest with digest-CAS promotion and sanitized cross-machine continuity capsules that carry no lease, approval, raw transcript, machine path, or publication authority.\n'
    write_text(path, replace_once(text, old, new, 'CHANGELOG.md'), 420)

def patch_components(path: Path) -> None:
    text = path.read_text(encoding='utf-8')
    if '## Typed runtime core' in text:
        return
    old = '## Project onboarding\n'
    new = '## Typed runtime core\n\n`oms runtime` is a standard-library-only semantic/query layer over the existing\nhardened execution plane. It projects the effective task contract and\ncriterion-level evidence, compiles bounded delegated context, records honest\nbackend capabilities, exports sanitized continuity capsules, selects optional\nhost capabilities, and evaluates comparable research experiments. It does not\nreplace `peer-delegate`, `patch-admit`, `patch-land`, leases, executor souls,\napprovals, commit intents, or Draft PR intents. See\n[`docs/OMS-RUNTIME.md`](OMS-RUNTIME.md).\n\n`oms list --frontdoor` shows the compact agent-facing surface; `oms list --all`\nkeeps every compatibility primitive available.\n\n## Project onboarding\n'
    write_text(path, replace_once(text, old, new, 'docs/COMPONENTS.md'), 420)
