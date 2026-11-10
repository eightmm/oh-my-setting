#!/usr/bin/env python3
"""The optional `## Roadmap` section of PROJECT.md: milestones as bundles of criterion ids.

    ## Roadmap
    - [ ] M1: loader reads the manifest (REQ-1, REQ-2)
    - [x] M0: project skeleton (REQ-0)

The section is a reading order written by the project's owner, not authority:
a checkbox is the document's own claim, never evidence that a milestone is
done, and nothing here reads or writes plan state. A milestone only narrows
which acceptance criteria one decomposition is asked to plan.

Usage: spec_roadmap.py list SPEC          print the milestones (exit 1 when there is no Roadmap)
       spec_roadmap.py block SPEC ID      print the planner block for one milestone (exit 2 on a bad id)
       spec_roadmap.py ids SPEC ID        print that milestone's criterion ids, one per line
"""
import os
import re
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

ITEM = re.compile(r"^\s*[-*+]\s+\[(?P<box>[ xX])\]\s*(?P<id>[A-Za-z0-9][A-Za-z0-9._-]{0,39})\s*:\s*(?P<rest>.+)$")
IDS = re.compile(r"\(([^()]*)\)\s*$")
ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
MAX_MILESTONES = 40
MAX_IDS = 30


def shown(value, limit=120):
    text = " ".join("".join(ch if ch.isprintable() else " " for ch in str(value)).split())
    return text if len(text) <= limit else text[:limit - 1] + "…"


def load(path):
    """(milestones, criteria): criteria maps every id the runtime projects to its statement."""
    from oms_runtime.markdown import (bullet_items, normalized_line, section_text, sections,
                                      stable_criterion_id, strip_criterion_marker)
    with open(path, encoding="utf-8") as handle:
        body, _ = sections(handle.read())
    criteria = {}
    for raw in bullet_items(section_text(body, ["acceptance", "done", "success", "completion", "완료", "성공", "수용"])):
        criteria.setdefault(stable_criterion_id("project", raw), strip_criterion_marker(normalized_line(raw)))
    milestones, seen = [], set()
    for line in section_text(body, ["roadmap", "로드맵"]).splitlines():
        match = ITEM.match(line)
        if not match or match.group("id") in seen or len(milestones) >= MAX_MILESTONES:
            continue
        seen.add(match.group("id"))
        rest = match.group("rest").strip()
        listed = IDS.search(rest)
        ids = []
        if listed:
            for item in listed.group(1).split(","):
                item = item.strip()
                if ID.match(item) and item not in ids:
                    ids.append(item)
            rest = rest[:listed.start()].strip()
        milestones.append({"id": match.group("id"), "checked": match.group("box") != " ",
                           "title": shown(rest), "criteria": ids[:MAX_IDS],
                           "unknown": [item for item in ids[:MAX_IDS] if item not in criteria]})
    return milestones, criteria


def main(argv):
    if len(argv) < 3 or argv[1] not in ("list", "block", "ids") or (argv[1] != "list" and len(argv) != 4):
        print(__doc__.strip().split("Usage:", 1)[1].strip(), file=sys.stderr)
        return 2
    try:
        milestones, criteria = load(argv[2])
    except (ImportError, OSError, UnicodeError) as exc:
        print("error: cannot read the contract's Roadmap: %s" % shown(exc), file=sys.stderr)
        return 2
    if argv[1] == "list":
        if not milestones:
            print("roadmap: PROJECT.md has no '## Roadmap' items (optional: '- [ ] M1: title (REQ-1, REQ-2)')")
            return 1
        print("roadmap: %d milestone(s); a checkbox is the document's own claim, not evidence" % len(milestones))
        for item in milestones:
            print("  [%s] %s: %s  (%s)" % ("x" if item["checked"] else " ", item["id"], item["title"],
                                         ", ".join(item["criteria"]) or "no criteria listed"))
            if item["unknown"]:
                print("      warning: not a criterion id in this contract: " + ", ".join(item["unknown"]))
        first = next((item for item in milestones if not item["checked"]), None)
        if first:
            print("next unchecked: %s  ->  oms plan-from-spec --milestone %s" % (first["id"], first["id"]))
        return 0
    wanted = next((item for item in milestones if item["id"] == argv[3]), None)
    if wanted is None:
        print("error: no milestone %s in PROJECT.md's Roadmap (have: %s)" % (
            shown(argv[3], 40), ", ".join(item["id"] for item in milestones) or "none"), file=sys.stderr)
        return 2
    if not wanted["criteria"]:
        print("error: milestone %s lists no criterion ids; write them as '(REQ-1, REQ-2)' at the end of its line"
              % wanted["id"], file=sys.stderr)
        return 2
    if wanted["unknown"]:
        print("error: milestone %s names ids this contract does not carry: %s" % (
            wanted["id"], ", ".join(wanted["unknown"])), file=sys.stderr)
        return 2
    if argv[1] == "ids":
        print("\n".join(wanted["criteria"]))
        return 0
    lines = ["", "Milestone for this decomposition: %s — %s" % (wanted["id"], wanted["title"]),
             "Plan only the work these acceptance criteria need; leave every other criterion for a later plan:"]
    lines += ["- %s | %s" % (item, criteria[item][:200]) for item in wanted["criteria"]]
    sys.stdout.write("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
