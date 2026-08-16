#!/usr/bin/env python3
"""Reference-only OMS remote adapter protocol.

A production adapter must add real authentication, transport, isolation,
cancellation, cleanup, and output collection. This example intentionally runs
nothing; it validates the request and emits an attestation-shaped response.
"""

from __future__ import annotations

import json
import sys


def main() -> int:
    request = json.load(sys.stdin)
    if request.get("schema") != 1 or not isinstance(request.get("command"), list):
        print(json.dumps({"error": "invalid request"}))
        return 2
    response = {
        "schema": 1,
        "operation_id": request.get("operation_id"),
        "accepted": False,
        "reason": "reference adapter does not execute commands",
        "attestation": {
            "adapter": "reference-only",
            "transport_authenticated": False,
            "filesystem_isolated": False,
            "network_policy_enforced": False,
            "cleanup_confirmed": True,
        },
    }
    print(json.dumps(response, sort_keys=True))
    return 3


if __name__ == "__main__":
    raise SystemExit(main())
