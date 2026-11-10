#!/usr/bin/env python3
"""Mechanical triggers that name `oms advise` from the fail-ledger.

The ledger already names the advisor when one command has failed twice
unresolved (the repeat threshold, in fail-ledger.sh). The table below holds
the other triggers. Each is a plain fact the caller observed (a signal), never
a model judgement, and each is printed at most once per fingerprint: the row
that printed it carries the trigger id in `advised`, and later rows for the
same fingerprint stay quiet. Nothing here calls a model.

usage: advise_triggers.py LEDGER FINGERPRINT < signals
  signals: one per line, "conflict" or "contract:<reason>"
  stdout:  one "<id>\t<one-line suggestion>" per trigger that should speak
"""

import json
import sys

# id -> (signal prefix, reason). The reason states why; the suffix is shared.
TRIGGERS = (
    ("merge_conflict", "conflict",
     "a merge/rebase conflict is unresolved in the tree"),
    ("contract_surface", "contract",
     "the tree changes a contract or public-surface path ({detail})"),
)
SUFFIX = "optional outside read if authorized (oms advise --prompt \"...\")"


def already_advised(ledger, fingerprint):
    seen = set()
    try:
        handle = open(ledger, encoding="utf-8", errors="replace")
    except OSError:
        return seen
    with handle:
        for line in handle:
            if fingerprint not in line or "advised" not in line:
                continue
            try:
                row = json.loads(line)
            except Exception:
                continue
            if row.get("fingerprint") == fingerprint and isinstance(row.get("advised"), list):
                seen.update(str(item) for item in row["advised"])
    return seen


def main(argv):
    ledger, fingerprint = argv[1], argv[2]
    signals = {}
    for line in sys.stdin.read().splitlines():
        name, _sep, detail = line.partition(":")
        if name:
            signals.setdefault(name.strip(), detail.strip())
    if not signals:
        return 0
    seen = already_advised(ledger, fingerprint)
    for trigger_id, signal, reason in TRIGGERS:
        if signal in signals and trigger_id not in seen:
            print("%s\tfail-ledger: advise: %s; %s" % (
                trigger_id, reason.format(detail=signals[signal]), SUFFIX))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
