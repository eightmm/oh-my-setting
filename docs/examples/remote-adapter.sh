#!/usr/bin/env bash
set -euo pipefail

# Reference-only OMS remote adapter protocol.
#
# A production adapter must add real authentication, transport, isolation,
# cancellation, cleanup, and output collection. This example intentionally runs
# nothing; it validates the request and emits an attestation-shaped response.

req="$(cat)"
schema="$(printf '%s' "$req" | jq -r '.schema // empty' 2>/dev/null || true)"
is_cmd_list="$(printf '%s' "$req" | jq -r '(.command | type) == "array"' 2>/dev/null || echo false)"
if [ "$schema" != "1" ] || [ "$is_cmd_list" != "true" ]; then
  printf '{"error":"invalid request"}\n'
  exit 2
fi

op_id_json="$(printf '%s' "$req" | jq -c '.operation_id // null')"
jq -cn --argjson op "$op_id_json" '{
  schema: 1,
  operation_id: $op,
  accepted: false,
  reason: "reference adapter does not execute commands",
  attestation: {
    adapter: "reference-only",
    transport_authenticated: false,
    filesystem_isolated: false,
    network_policy_enforced: false,
    cleanup_confirmed: true
  }
}'
exit 3
