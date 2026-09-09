#!/usr/bin/env bash
set -euo pipefail

# Focused regressions for the optional, read-only Codex app-server transport.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-interoperability.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

repo="$TMP/repo"
mkdir -p "$repo/.oms"
git -C "$repo" init -q
printf '*\n' > "$repo/.oms/.gitignore"
printf 'fixture\n' > "$repo/README.md"
git -C "$repo" add README.md
git -C "$repo" -c user.name=test -c user.email=test@example.com commit -qm init

test_codex_app_server_adapter_is_explicit_read_only_and_fail_closed() {
  local fake="$TMP/fake-app-server.py"
  local log="$TMP/app-server-requests.jsonl"
  local prompt="$TMP/prompt.txt"
  local output="$TMP/adapter.out"

  cat > "$fake" <<'PY'
#!/usr/bin/env python3
import json, os, sys

log = os.environ["FAKE_APP_SERVER_LOG"]
mode = os.environ.get("FAKE_APP_SERVER_MODE", "success")
for line in sys.stdin:
    row = json.loads(line)
    with open(log, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, sort_keys=True) + "\n")
    method = row.get("method")
    msg_id = row.get("id")
    if method == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":msg_id,"result":{}}), flush=True)
    elif method == "thread/start":
        print(json.dumps({"jsonrpc":"2.0","id":msg_id,"result":{
            "thread":{"id":"thread-fixture"}}}), flush=True)
    elif method == "turn/start":
        print(json.dumps({"jsonrpc":"2.0","id":msg_id,"result":{
            "turn":{"id":"turn-fixture","status":"inProgress","items":[]}}}), flush=True)
        if mode == "approval":
            print(json.dumps({"jsonrpc":"2.0","id":91,
                "method":"item/commandExecution/requestApproval","params":{}}), flush=True)
        else:
            print(json.dumps({"jsonrpc":"2.0","method":"item/agentMessage/delta",
                "params":{"threadId":"thread-fixture","turnId":"turn-fixture",
                          "itemId":"item-fixture","delta":"ADAPTER-ANSWER"}}), flush=True)
            print(json.dumps({"jsonrpc":"2.0","method":"turn/completed",
                "params":{"threadId":"thread-fixture","turn":{
                    "id":"turn-fixture","status":"completed","items":[]}}}), flush=True)
PY
  chmod +x "$fake"
  printf 'review the repository without writing\n' > "$prompt"

  # Provider registry defaults do not change. The adapter is selected only by
  # the explicit Codex transport variable.
  (
    # shellcheck source=scripts/lib/provider-registry.sh
    . "$ROOT/scripts/lib/provider-registry.sh"
    [ "$(oms_provider_transport codex)" = cli-exec ]
    [ "$(OMS_CODEX_TRANSPORT=app-server oms_provider_transport codex)" = app-server ]
    ! OMS_CODEX_TRANSPORT=unknown oms_provider_transport codex >/dev/null 2>&1
    [ "$(OMS_CODEX_TRANSPORT=app-server oms_provider_transport claude)" = cli-exec ]
  ) || fail "provider transport registry contract failed"

  FAKE_APP_SERVER_LOG="$log" OMS_CODEX_APP_SERVER_COMMAND="$fake" \
    python3 "$ROOT/scripts/lib/codex-app-server-adapter.py" \
      --repo "$repo" --prompt-file "$prompt" --model provider-default \
      --effort medium --timeout 5 > "$output" 2>&1 ||
    fail "Codex app-server adapter failed: $(cat "$output")"
  [ "$(cat "$output")" = ADAPTER-ANSWER ] ||
    fail "adapter did not return only the agent message: $(cat "$output")"

  OMS_T_LOG="$log" OMS_T_REPO="$repo" OMS_T_PROMPT="$prompt" python3 - <<'PY' ||
import json, os
rows = [json.loads(line) for line in open(os.environ["OMS_T_LOG"], encoding="utf-8")]
by_method = {row.get("method"): row for row in rows}
assert list(by_method) == ["initialize", "initialized", "thread/start", "turn/start"], rows
thread = by_method["thread/start"]["params"]
assert thread["cwd"] == os.path.realpath(os.environ["OMS_T_REPO"]), thread
assert thread["sandbox"] == "read-only", thread
assert thread["approvalPolicy"] == "never", thread
turn = by_method["turn/start"]["params"]
assert turn["sandboxPolicy"] == {"type":"readOnly","networkAccess":False}, turn
assert turn["approvalPolicy"] == "never", turn
assert turn["input"] == [{"type":"text","text":"review the repository without writing\n"}], turn
assert turn["effort"] == "medium", turn
PY
    fail "app-server requests widened read authority"

  : > "$log"
  if FAKE_APP_SERVER_MODE=approval FAKE_APP_SERVER_LOG="$log" \
    OMS_CODEX_APP_SERVER_COMMAND="$fake" \
    python3 "$ROOT/scripts/lib/codex-app-server-adapter.py" \
      --repo "$repo" --prompt-file "$prompt" --model provider-default \
      --timeout 5 > "$output" 2>&1; then
    fail "adapter accepted an app-server approval request"
  fi
  grep -Fq 'approval request' "$output" ||
    fail "adapter refusal did not explain the approval boundary: $(cat "$output")"

  # The shared provider runner must select the adapter for read seats and must
  # refuse it for write seats. There is no silent fallback to `codex exec`.
  : > "$log"
  (
    # shellcheck source=scripts/lib/peer-common.sh
    . "$ROOT/scripts/lib/peer-common.sh"
    # The explicit adapter command is the transport authority. A host does not
    # need the Codex CLI when that command is independently available.
    oms_provider_cli_discovered() { return 1; }
    oms_provider_cli_available() { return 1; }
    OMS_CODEX_TRANSPORT=app-server OMS_CODEX_APP_SERVER_COMMAND="$fake" \
      FAKE_APP_SERVER_LOG="$log" OMS_PEER_TIMEOUT=10 \
      ma_provider_attempt codex read "$prompt" "$output" "$repo" \
        provider-default "" interoperability "$repo" call-fixture
  ) || fail "peer router did not use the explicit app-server adapter: $(cat "$output")"
  grep -Fq ADAPTER-ANSWER "$output" || fail "peer adapter answer was lost"
  if (
    . "$ROOT/scripts/lib/peer-common.sh"
    OMS_CODEX_TRANSPORT=app-server OMS_CODEX_APP_SERVER_COMMAND="$fake" \
      FAKE_APP_SERVER_LOG="$log" ma_provider_attempt codex write "$prompt" \
        "$output" "$repo" provider-default "" interoperability "$repo" call-write
  ); then
    fail "write delegation crossed the read-only app-server adapter"
  fi
}

test_codex_app_server_adapter_is_explicit_read_only_and_fail_closed

echo "interoperability-smoke: ok"
