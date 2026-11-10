#!/usr/bin/env python3
"""Read-only check of what PROJECT.md points at: contract-check.py PROJECT_DIR LIB_DIR.

Prints ok:/warn:/note: lines for project-doctor --contract. A warn is something the contract names that is
not there (a required check file, a referenced document, a criterion a plan task says it covers). Evidence
per criterion is a summary, never a verdict: an unproven criterion is a note, because work in progress is
not drift. Nothing is written, no model is called, and the exit code is always 0; project-doctor decides
what --strict makes of the warnings."""
import json
import os
import re
import sys
import unicodedata

sys.dont_write_bytecode = True
project, lib = sys.argv[1:3]
sys.path.insert(0, lib)

CRITERIA_HEADINGS = ["acceptance", "done", "success", "completion", "완료", "성공", "수용"]
MAX_LISTED = 12


def shown(value, limit=70):
    value = "".join(ch if unicodedata.category(ch)[0] != "C" else " " for ch in str(value))
    value = " ".join(value.split())
    return value if len(value) <= limit else value[:limit - 3] + "..."


def say(kind, message):
    print("%s: %s" % (kind, message))


spec = os.path.join(project, "PROJECT.md")
try:
    with open(spec, encoding="utf-8") as handle:
        text = handle.read()
except (OSError, UnicodeError):
    say("warn", "contract: PROJECT.md is missing or unreadable, so there is no contract to check")
    sys.exit(0)
lines = text.splitlines()


def field(section_name, label):
    """First `- label:` value in `## section_name`; an empty value takes the indented list under it."""
    inside = False
    pattern = re.compile(r"^- " + re.escape(label) + r":[ \t]*(.*)$")
    for number, line in enumerate(lines):
        if line.startswith("## "):
            inside = line.rstrip() == "## " + section_name
            continue
        match = pattern.match(line) if inside else None
        if match:
            value = match.group(1).strip()
            if value:
                return value
            nested = []
            for following in lines[number + 1:]:
                item = re.match(r"^[ \t]+[-*+][ \t]+(.*)$", following)
                if not item:
                    break
                nested.append(item.group(1).strip())
            return ", ".join(nested)
    return None


def inside_project(relative):
    """The path under the project, or None when it is absolute, escapes, or is not a plain path."""
    relative = relative.strip().strip("`'\"").split("#", 1)[0]
    if (not relative or relative.startswith(("/", "~")) or "://" in relative or relative.startswith("mailto:")
            or any(ch in relative for ch in "*?<>|$\n")):
        return None
    full = os.path.normpath(os.path.join(project, relative))
    if full != project and not full.startswith(project.rstrip("/") + "/"):
        return None
    return relative, full


def report_paths(label, values, want_file):
    missing, present = [], 0
    for value in values:
        resolved = inside_project(value)
        if resolved is None:
            continue
        relative, full = resolved
        if (os.path.isfile(full) if want_file else os.path.exists(full)):
            present += 1
        else:
            missing.append(relative)
    for relative in missing[:MAX_LISTED]:
        say("warn", "contract: %s names %s, which does not exist" % (label, shown(relative)))
    if len(missing) > MAX_LISTED:
        say("warn", "contract: %s names %d more missing path(s)" % (label, len(missing) - MAX_LISTED))
    if present and not missing:
        say("ok", "contract: %s: %d path(s) exist" % (label, present))
    return missing


raw_files = field("Verification", "Required check files") or ""
report_paths("Required check files", [part for part in re.split(r"[ \t,]+", raw_files) if part], True)

references = field("Decisions", "References") or field("Decisions", "References (repo-relative paths/sections)") or ""
candidates = [part for part in re.split(r"[,;]+", references)
              if part.strip() and " " not in part.strip().strip("`") and re.search(r"[/.]", part)]
report_paths("References", candidates, False)

links = [target for target in re.findall(r"\]\(([^)\s]+)\)", text)]
report_paths("a Markdown link", sorted(set(links)), False)

try:
    from oms_runtime.markdown import (bullet_items, normalized_line, section_text, sections,
                                      stable_criterion_id, strip_criterion_marker)
    body, _ = sections(text)
    criteria = []
    for raw in bullet_items(section_text(body, CRITERIA_HEADINGS)):
        criterion_id = stable_criterion_id("project", raw)
        if criterion_id in [item[0] for item in criteria]:
            say("warn", "contract: duplicate criterion id %s" % shown(criterion_id, 80))
        else:
            criteria.append((criterion_id, strip_criterion_marker(normalized_line(raw))))
except Exception:  # the runtime helpers are optional here: without them only the path checks run
    criteria = None

covered = {}
plan_path = os.path.join(project, ".oms", "plan", "tasks.json")
if criteria is not None and os.path.isfile(plan_path) and not os.path.islink(plan_path):
    try:
        with open(plan_path, encoding="utf-8") as handle:
            tasks = json.load(handle).get("tasks") or {}
        known = {item[0] for item in criteria}
        for task_id in sorted(tasks):
            task = tasks[task_id] if isinstance(tasks[task_id], dict) else {}
            for criterion_id in task.get("covers") or []:
                if criterion_id in known:
                    covered.setdefault(str(criterion_id), []).append(str(task_id))
                else:
                    say("warn", "contract: plan task %s covers %s, which the spec does not carry"
                        % (shown(task_id, 40), shown(criterion_id, 40)))
    except (OSError, ValueError, AttributeError, TypeError):
        say("note", "contract: the plan could not be read, so task coverage is not shown")

if criteria:
    status = {}
    try:
        from pathlib import Path
        from oms_runtime.evidence import build_coverage
        from oms_runtime.projection import build_base_envelope
        base = build_base_envelope(Path(project))
        for item in build_coverage(Path(project), base, use_cache=False).get("criteria", []):
            status[str(item.get("id"))] = (str(item.get("status")), len(item.get("evidence") or []))
    except Exception as exc:
        say("note", "contract: evidence is not shown (%s)" % shown(exc, 90))
    for criterion_id, statement in criteria[:40]:
        parts = []
        if criterion_id in status:
            parts.append("evidence %s (%d record(s))" % status[criterion_id])
        elif status:
            parts.append("evidence unknown")
        if covered.get(criterion_id):
            parts.append("plan task " + ", ".join(covered[criterion_id][:4]))
        say("note", "criterion %s: %s — \"%s\"" % (shown(criterion_id, 40), "; ".join(parts) or "no record",
                                                 shown(statement, 60)))
    if len(criteria) > 40:
        say("note", "contract: %d more criteria not listed" % (len(criteria) - 40))
elif criteria is not None:
    say("note", "contract: PROJECT.md lists no acceptance criteria, so there is no per-criterion evidence to show")
