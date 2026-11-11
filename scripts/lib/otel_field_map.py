#!/usr/bin/env python3
"""Lint config/otel-genai-map.json: every OMS field it names must exist in its writer.

The map is documentation data for people who post-process .oms/ JSONL; nothing reads it at
runtime. A field "exists" when its last dotted segment appears as a quoted literal in the named
writer file. Attribute names marked confirmed must look like gen_ai.* / error.type names, and
every mapping must say whether the OTel name was confirmed.
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MAP_PATH = "config/otel-genai-map.json"
STATUSES = {"confirmed", "unconfirmed", "no-equivalent"}
FITS = {"exact", "approximate"}
KINDS = {"field", "event_type"}
OTEL_NAME_RE = re.compile(r"^(gen_ai(\.[a-z0-9_]+)+|error\.type)(=[a-z_]+)?$")


def check(root=ROOT):
    root = Path(root)
    try:
        data = json.loads((root / MAP_PATH).read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        return ["%s unreadable: %s" % (MAP_PATH, exc)]
    problems = []
    source = data.get("source") if isinstance(data, dict) else None
    if not isinstance(source, dict) or not source.get("checked") or not source.get("version"):
        problems.append("source needs checked and version")
    elif not any(str(u).startswith("https://") for u in source.get("urls", [])):
        problems.append("source needs at least one https url")
    if not isinstance(data.get("unconfirmed"), list):
        problems.append("unconfirmed must be a list")
    texts = {}
    seen = set()
    for index, entry in enumerate(data.get("entries", [])):
        field = entry.get("oms_field", "")
        where = "entry %d (%s)" % (index, field)
        writer = entry.get("writer", "")
        status = entry.get("status")
        if status not in STATUSES:
            problems.append("%s: bad status" % where)
        if entry.get("kind") not in KINDS:
            problems.append("%s: bad kind" % where)
        key = (writer, field)
        if key in seen:
            problems.append("%s: duplicate" % where)
        seen.add(key)
        otel = entry.get("otel")
        if status == "no-equivalent":
            if otel is not None:
                problems.append("%s: no-equivalent must have otel null" % where)
        elif not otel:
            problems.append("%s: %s needs an otel name" % (where, status))
        elif status == "confirmed":
            if not OTEL_NAME_RE.match(otel):
                problems.append("%s: confirmed name %r is not a gen_ai.* or error.type name" % (where, otel))
            if entry.get("fit") not in FITS:
                problems.append("%s: confirmed needs fit exact|approximate" % where)
        if writer not in texts:
            try:
                texts[writer] = (root / writer).read_text(encoding="utf-8")
            except OSError:
                texts[writer] = None
        text = texts[writer]
        if text is None:
            problems.append("%s: writer %s unreadable" % (where, writer))
            continue
        name = field.split(".")[-1] if entry.get("kind") == "field" else field
        if '"%s"' % name not in text and "'%s'" % name not in text:
            problems.append("%s: not found as a literal in %s" % (where, writer))
    return problems


def main(argv):
    root = Path(argv[1]) if len(argv) > 1 else ROOT
    problems = check(root)
    for line in problems:
        print("otel-field-map: " + line, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
