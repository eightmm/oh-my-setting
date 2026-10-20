# Historical multi-main rooms remain readable; write their old join records explicitly.
def legacy_main_join(repo, ident, *args, **kwargs):
    with patch.object(room, "append"):
        event = room.join(repo, ident, *args, **kwargs)
    assert event["role"] == "main"
    rows = room.records(repo, ident)
    row = {"schema": 1, "ts": rows[-1]["ts"], "thread": ident, "seq": rows[-1]["seq"] + 1,
           "role": "note", "agent": "fixture", "text": "Joined: " + event["label"], "room_event": event}
    with (repo / ".oms/threads" / (ident + ".jsonl")).open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(row) + "\n")
    return event

project = temporary / "panel repo 'quoted'; dollar$"
project.mkdir()
home = temporary / "panel-home"
home.mkdir()
binary = temporary / "panel-bin"
binary.mkdir()
subprocess.run(["git", "init", "-q", str(project)], check=True)
for key, value in (("user.email", "test@example.com"), ("user.name", "test")):
    subprocess.run(["git", "-C", str(project), "config", key, value], check=True)
(project / "value.txt").write_text("old\n")
subprocess.run(["git", "-C", str(project), "add", "value.txt"], check=True)
subprocess.run(["git", "-C", str(project), "commit", "-qm", "init"], check=True)
brief = temporary / "panel-brief.md"
brief.write_text("Task: append new to value.txt. No primary edits or recursive workers.\n")
log = temporary / "panel-native-log.jsonl"
worker_log = temporary / "panel-worker-log.jsonl"
for provider in (*panel.PROVIDERS, "grok", "agy", "vibe", "oms-agent-adapter-muse"):
    executable = binary / provider
    executable.write_text('''#!/usr/bin/env bash
set -eu
case "${1:-}:${2:-}" in
  --version:|--help:|exec:--help|--no-auto-update:version|--no-auto-update:--help) printf 'fixture CLI 1.0 --effort low medium high --config\\n'; exit 0 ;;
esac
if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  python3 - "$@" <<'WORKER'
import json, os, sys, subprocess
with open(os.environ["PANEL_TEST_WORKER_LOG"], "a", encoding="utf-8") as f:
    f.write(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd(),
                        "git_common": subprocess.check_output(["git", "rev-parse", "--git-common-dir"], text=True).strip(),
                        "head": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                        "index": subprocess.check_output(["git", "ls-files", "--stage"], text=True),
                        "role": os.environ.get("OMS_PANEL_ROLE"),
                        "purpose": os.environ.get("OMS_PANEL_PURPOSE"),
                        "task_id": os.environ.get("OMS_TASK_ID"),
                        "parent": os.environ.get("OMS_PANEL_MAIN_ATTEMPT"),
                        "room": os.environ.get("OMS_ROOM_ID"),
                        "room_repo": os.environ.get("OMS_ROOM_REPO"),
                        "timeout": os.environ.get("OMS_PEER_TIMEOUT"),
                        "verify_timeout": os.environ.get("OMS_PEER_VERIFY_TIMEOUT"),
                        "state_repo": os.environ.get("OMS_STATE_REPO"),
                        "participant": os.environ.get("OMS_ROOM_PARTICIPANT")}) + "\\n")
WORKER
  if [ -n "${LEASE_FIXTURE_ENTERED:-}" ]; then
    # A failed precondition must fail the worker, not exit 0 without the marker.
    python3 - "$@" <<'LEASEWAIT' || exit 97
import json, os, sys, subprocess, time
from pathlib import Path
assert os.environ["OMS_AGENT"] == os.environ["LEASE_FIXTURE_PROVIDER"]
assert os.environ["LEASE_FIXTURE_MODEL"] in sys.argv[1:], sys.argv
if os.environ["OMS_AGENT"] == "claude":
    assert sys.argv[sys.argv.index("--effort") + 1] == os.environ["LEASE_FIXTURE_EFFORT"], sys.argv
else:
    assert any("model_reasoning_effort" in a and os.environ["LEASE_FIXTURE_EFFORT"] in a for a in sys.argv), sys.argv
for name in os.environ:
    assert name not in ("OMS_PLAN_LEASE_ID", "OMS_PANEL_EXECUTION_ROOT", "OBSERVED_BINDING", "OMS_INDEX_BASE_REPO", "DELEGATE_SOURCE_REPO") and not name.startswith(("OMS_DL_", "OMS_OBSERVER_", "OMS_OBSERVE_PLAN_")), name
entry = os.environ["OMS_PANEL_ENTRYPOINT"]
for args in (("agent-plan", "--repo", os.environ["OMS_ROOM_REPO"], "add", "--id", "child-denied", "--title", "Denied"),
             ("agent-plan", "--repo", os.environ["OMS_ROOM_REPO"], "touch", "--id", os.environ["OMS_TASK_ID"]),
             ("peer-delegate", "--to", "codex", "--prompt", "nested")):
    result = subprocess.run(["bash", entry, *args], capture_output=True, text=True)
    assert result.returncode != 0, (args, result.stdout, result.stderr)
Path(os.environ["LEASE_FIXTURE_ENTERED"]).write_text(json.dumps({"cwd": os.getcwd(),
    "head": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
    "value": Path("value.txt").read_text(), "participant": os.environ["OMS_ROOM_PARTICIPANT"],
    "parent": os.environ.get("OMS_PANEL_MAIN_ATTEMPT"), "role": os.environ.get("OMS_PANEL_ROLE"),
    "purpose": os.environ.get("OMS_PANEL_PURPOSE")}))
end = time.monotonic() + 60
while not Path(os.environ["LEASE_FIXTURE_RELEASE"]).exists():
    assert time.monotonic() < end, "worker fixture barrier exceeded"
    time.sleep(.05)
if os.environ.get("OMS_PANEL_PURPOSE") == "implement":
    with Path("value.txt").open("a", encoding="utf-8") as handle:
        handle.write("worker change\\n")
LEASEWAIT
  fi
  cat > "${PANEL_TEST_PROMPT:-/dev/null}"
  if [ -n "${PANEL_TEST_SESSION:-}" ]; then
    printf '{"type":"thread.started","thread_id":"%s"}\\n{"type":"item.completed","item":{"type":"agent_message","text":"Answer: inspected the bounded repository task."}}\\n{"type":"turn.completed","usage":{}}\\n' "$PANEL_TEST_SESSION"
    exit 0
  fi
  if [ "${PANEL_TEST_MODE:-}" = write ]; then
    printf 'new\\n' >> value.txt
    if bash "$OMS_PANEL_ENTRYPOINT" panel --launch codex >/dev/null 2>&1; then
      echo 'nested owner launch should have failed' >&2
      exit 1
    fi
  fi
  if [ "${PANEL_TEST_MODE:-}" = primary-write ]; then
    printf 'outside worker\\n' >> "$PANEL_TEST_PRIMARY"
  fi
  if [ "${OMS_PANEL_PURPOSE:-}" = advise ]; then
    printf 'Answer: inspected source.\nAlternatives: separate panels.\nEvidence: value.txt exists.\nCounterargument: retained history is bounded.\nRisks: delivery can fail.\nRecommendation: owner records evidence.\nVerification: inspect source.\nChanged from previous round: none.\nRemaining disagreements: none.\n'
    exit 0
  fi
  printf 'Answer: inspected the bounded repository task.\\nGATE: pass\\n'
else
  python3 - "$@" <<'INNER'
import json, os, sys
with open(os.environ["PANEL_TEST_LOG"], "a", encoding="utf-8") as f:
    f.write(json.dumps({"argv": sys.argv[1:], "agent": os.environ["OMS_AGENT"],
                        "repo": os.environ["OMS_PANEL_REPO"], "cwd": os.getcwd(),
                        "entry": os.environ["OMS_PANEL_ENTRYPOINT"]}) + "\\n")
print("NATIVE_FIXTURE_READY")
INNER
  if [ "${PANEL_TEST_NATIVE_HOLD:-0}" = 1 ]; then
    read -r reply || :
    case "$reply" in
      *SWAP_HANDOFF*)
        nonce="${reply#*SWAP_HANDOFF }"
        nonce="${nonce%%:*}"
        if [ "${PANEL_TEST_SWAP_HANDOFF:-1}" = 1 ]; then
          bash "$OMS_PANEL_ENTRYPOINT" room send --repo "$OMS_ROOM_REPO" --id "$OMS_ROOM_ID" \
            --participant "$OMS_ROOM_PARTICIPANT" --to all --kind handoff \
            --text "SWAP_HANDOFF $nonce: Goal fixture; decisions kept; workers continue; next inspect plan." >/dev/null
        fi
        while read -r reply; do [ "$reply" = /exit ] && break; done
        ;;
      *) printf 'NATIVE_CHAT_REPLY:%s\\n' "$reply" ;;
    esac
  fi
fi
''')
    executable.chmod(0o755)

environment = dict(os.environ, HOME=str(home), NVM_DIR=str(home / ".nvm"),
                   CODEX_HOME=str(home / ".codex"), CLAUDE_CONFIG_DIR=str(home / ".claude"),
                   PATH=str(binary) + os.pathsep + os.environ["PATH"],
                   XDG_STATE_HOME=str(temporary / "panel-state"),
                   OMS_PANEL_ENTRYPOINT=str(panel.ENTRY), PANEL_TEST_LOG=str(log),
                   PANEL_TEST_WORKER_LOG=str(worker_log), OMS_CAPABILITY_SKIP_MODELS="1",
                   OMS_PEER_TIMEOUT="30", OMS_ROLE_ROUTING="0")
os.environ["CODEX_HOME"] = str(home / ".codex")
for key in ("OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH", "OMS_PANEL_SESSION", "TMUX", "TMUX_TMPDIR",
            "OMS_PANEL_MAIN_ATTEMPT", "OMS_PANEL_DISPATCH", "OMS_ATTEMPT_ID",
            "OH_MY_SETTING_CALL_DRY_RUN", "OH_MY_SETTING_REVIEW_DRY_RUN"):
    environment.pop(key, None)

def call(command, env=None):
    started = time.monotonic()
    result = subprocess.run(command, cwd=str(project), env=env or environment,
                            capture_output=True, text=True,
                            timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
    if time.monotonic() - started > 30:
        print("slow call %.1fs: %s" % (time.monotonic() - started, command[:4]), file=sys.stderr)
    assert result.returncode == 0, (command, result.returncode, result.stdout[-6000:], result.stderr[-6000:])
    return result.stdout

# Routing checks stay on fixture catalogs even when provider CLIs are absent.
def fixture_selected_route(*args, **kwargs):
    target = args[5] if len(args) > 5 else kwargs.get("target")
    codex = {"provider": "codex", "binary": "codex", "installed": True,
             "models": ["gpt-6-sol", "gpt-6-luna"],
             "catalog_models": ["gpt-6-sol", "gpt-6-luna"], "access": ["read", "write"],
             "exact_model_override": True}
    claude = {"provider": "claude", "binary": "claude", "installed": True,
              "models": [], "catalog_models": [], "access": ["read", "write"],
              "exact_model_override": True}
    catalog = [row for row in (codex, claude) if target is None or row["provider"] == target]
    with patch.object(panel, "provider_catalog", return_value=catalog):
        return panel.selected_route(*args, **kwargs)

summary = temporary / "panel-result-summary.txt"
summary.write_text("결과 화면을 읽기 쉽게 정리했습니다.\n검증 근거는 value.txt에서 확인합니다.\n", encoding="utf-8")
final_args = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--finalize", "--owner", "codex",
              "--task-id", "panel-council-test", "--summary-file", str(summary), "--outcome", "accepted",
              "--verify", "test -f value.txt", "--evidence", "value.txt"]
