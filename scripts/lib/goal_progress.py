#!/usr/bin/env python3
"""Mechanical per-cycle progress facts for goal-drive (advisory, no model).

Usage: goal_progress.py CYCLE STALLED THRESHOLD CUR_HEAD CUR_TREE CUR_ACCEPT
           CUR_CLOSED CUR_RECEIPTS PREV_HEAD PREV_TREE PREV_ACCEPT PREV_CLOSED
           PREV_RECEIPTS

Compares the state at the start of this cycle with the state at the start of
the previous one. Prints three lines: the new stalled_cycles count, a re-plan
suggestion (empty unless the count exceeds THRESHOLD), and a JSON object of the
facts that goal-drive merges into its existing cycle row.

A cycle stalls when the tree did not change and no task closed. The digest of
the acceptance output and the receipt count are recorded but do not count as
progress: a failing check that words itself differently each run is not
movement, and the existing "same tree and same failing acceptance twice" park
keeps its own rule.
"""
import json
import sys


def count(text):
    try:
        return max(int(text), 0)
    except ValueError:
        return 0


def main(argv):
    if len(argv) != 14:
        print("goal_progress: expected 13 arguments", file=sys.stderr)
        return 2
    cycle, stalled, threshold = count(argv[1]), count(argv[2]), count(argv[3])
    head, tree, accept, closed, receipts = argv[4:9]
    p_head, p_tree, p_accept, p_closed, p_receipts = argv[9:14]
    if cycle <= 1 or not p_tree:
        facts = {"head_changed": False, "tree_changed": False,
                 "accept_changed": False, "tasks_closed": 0,
                 "receipts_new": 0, "stalled_cycles": 0}
        print(0)
        print()
        print(json.dumps(facts, sort_keys=True))
        return 0
    tree_changed = tree != p_tree
    tasks_closed = max(count(closed) - count(p_closed), 0)
    stalled = 0 if (tree_changed or tasks_closed > 0) else stalled + 1
    facts = {
        "head_changed": head != p_head,
        "tree_changed": tree_changed,
        "accept_changed": accept != p_accept,
        "tasks_closed": tasks_closed,
        "receipts_new": max(count(receipts) - count(p_receipts), 0),
        "stalled_cycles": stalled,
    }
    advice = ""
    if stalled > threshold:
        advice = ("goal-drive: stalled cycles=%d (threshold %d): the tree did not "
                  "change and no task closed; consider a re-plan: add tasks "
                  "(agent-plan add) or decompose the remaining gap"
                  % (stalled, threshold))
    print(stalled)
    print(advice)
    print(json.dumps(facts, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
