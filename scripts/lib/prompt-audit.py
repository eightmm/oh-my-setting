#!/usr/bin/env python3
"""Read-only audit of the prompt text this harness ships.

Usage: prompt-audit.py SOURCE_ROOT [--json] [--strict]

Walks rules/, roles/, templates/*AGENTS.md, prompts/ and skill SKILL.md files under
SOURCE_ROOT and reports per file the words, bytes and whether it is injected every
session, on demand, or unknown. The class comes from the install code, not from the
file name: a class is "always" or "on-demand" only when the script that delivers it
still contains the marker string below, otherwise it is "unknown". Findings:

- phrases from config/prompt-audit-patterns.txt (over-emphasis written for older models)
- sentences of DUPLICATE_MIN_WORDS words or more that two or more always-injected files
  carry, compared after lowercasing and collapsing whitespace
- the always-injected word total per provider

Files are read as text; nothing runs and no model is called. Findings are advisory:
exit 0 unless --strict and a finding exists.
"""

import json
import os
import re
import sys

DUPLICATE_MIN_WORDS = 8
LIST_CAP = 10
MAX_FILE_BYTES = 1048576
PROVIDERS = (("claude", "$HOME/.claude/CLAUDE.md"), ("codex", "$HOME/.codex/AGENTS.md"),
             ("antigravity", "$HOME/.gemini/AGENTS.md"))

# class, script that delivers it, marker that must still be in that script
EVIDENCE = {
    "global": ("always", "scripts/link.sh", "rules/global-AGENTS.md"),
    "template": ("always", "scripts/apply-project-template.sh", "templates/project-"),
    "role": ("on-demand", "scripts/agent-role.sh", "injected into a delegated worker brief"),
    "skill": ("on-demand", "scripts/skill-doctor.sh", "body loads only when"),
}


def read(path):
    try:
        if os.path.getsize(path) > MAX_FILE_BYTES:
            return None
        with open(path, "rb") as handle:
            return handle.read().decode("utf-8", "replace")
    except OSError:
        return None


def contains(root, rel, marker):
    text = read(os.path.join(root, rel))
    return text is not None and marker in text


def listing(root):
    """(relative path, kind) for every audited file, sorted."""
    found = []

    def add_dir(rel, kind, suffix=".md", match=None):
        base = os.path.join(root, rel)
        if os.path.isdir(base):
            for name in sorted(os.listdir(base)):
                path = os.path.join(base, name)
                if os.path.isfile(path) and name.endswith(suffix) and (match is None or match in name):
                    found.append((rel + "/" + name, kind))

    add_dir("rules", "global")
    add_dir("roles", "role")
    add_dir("templates", "template", match="AGENTS")
    add_dir("prompts", "prompt")
    for rel in ("custom-skills", "templates/project-skills"):
        base = os.path.join(root, rel)
        if os.path.isdir(base):
            for name in sorted(os.listdir(base)):
                if os.path.isfile(os.path.join(base, name, "SKILL.md")):
                    found.append((rel + "/" + name + "/SKILL.md", "skill"))
    return found


def patterns(root):
    text = read(os.path.join(root, "config", "prompt-audit-patterns.txt")) or ""
    out = []
    for line in text.splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            try:
                out.append(re.compile(line, re.IGNORECASE))
            except re.error:
                continue
    return out


def sentences(text):
    """Normalised sentences outside code fences; list markers and emphasis marks are dropped."""
    out, fenced = [], False
    for line in text.splitlines():
        if line.lstrip().startswith(("```", "~~~")):
            fenced = not fenced
            continue
        if fenced or line.lstrip().startswith(("#", "|", "---")):
            continue
        line = re.sub(r"^\s*(?:[-*+]|\d+[.)])\s+", "", line).replace("`", "").replace("*", "")
        for part in re.split(r"(?<=[.!?:;])\s+", line):
            flat = " ".join(part.lower().split()).rstrip(".")
            if len(flat.split()) >= DUPLICATE_MIN_WORDS:
                out.append(flat)
    return out


def emphasis(text, compiled):
    hits = {}
    for line in text.splitlines():
        for rx in compiled:
            if rx.search(line):
                hits[rx.pattern] = hits.get(rx.pattern, 0) + 1
    return hits


def build(root):
    compiled = patterns(root)
    classes = {}
    for kind, (cls, script, marker) in EVIDENCE.items():
        classes[kind] = cls if contains(root, script, marker) else "unknown"
    classes["prompt"] = "unknown"
    files, owners = [], {}
    for rel, kind in listing(root):
        text = read(os.path.join(root, rel))
        if text is None:
            files.append({"path": rel, "injection": classes[kind], "words": None, "bytes": None, "emphasis": {}})
            continue
        row = {"path": rel, "injection": classes[kind], "words": len(text.split()),
               "bytes": len(text.encode("utf-8")), "emphasis": emphasis(text, compiled)}
        files.append(row)
        if row["injection"] == "always":
            for sentence in set(sentences(text)):
                owners.setdefault(sentence, []).append(rel)
    dups = [{"words": len(s.split()), "text": s, "files": f} for s, f in owners.items() if len(f) > 1]
    dups.sort(key=lambda d: (-len(d["files"]), -d["words"], d["text"]))
    always = [f for f in files if f["injection"] == "always" and f["words"] is not None]
    global_words = sum(f["words"] for f in always if f["path"].startswith("rules/"))
    tmpl = {f["path"]: f["words"] for f in always if f["path"].startswith("templates/")}
    slurm = sum(w for p, w in tmpl.items() if "slurm" in p)
    base = max([w for p, w in tmpl.items() if "slurm" not in p] or [0])
    delivered = {name: contains(root, "scripts/link.sh", target) for name, target in PROVIDERS}
    providers = []
    for name, _ in PROVIDERS:
        row = {"provider": name, "delivery": "link.sh" if delivered[name] else "unknown"}
        row["global_words"] = global_words if delivered[name] else None
        row["with_largest_project_templates_words"] = global_words + base + slurm if delivered[name] else None
        providers.append(row)
    findings = sum(len(f["emphasis"]) for f in files) + len(dups)
    return {"schema": 1, "kind": "oms-prompt-audit", "root": os.path.basename(root.rstrip(os.sep)),
            "files": files, "duplicates": dups, "providers": providers, "findings": findings}


def render(report):
    out = ["prompt audit (read as text; nothing was run, no model was called)"]
    for f in report["files"]:
        size = "unread" if f["words"] is None else "%5d w %6d B" % (f["words"], f["bytes"])
        out.append("  %-10s %s  %s" % (f["injection"], size, f["path"]))
    for f in report["files"]:
        for pat, count in sorted(f["emphasis"].items()):
            out.append("warn: %s: %d line(s) match emphasis pattern %s" % (f["path"], count, pat))
    for d in report["duplicates"][:LIST_CAP]:
        out.append("warn: %d-word sentence in %d always-injected files (%s): \"%s\"" %
                   (d["words"], len(d["files"]), ", ".join(d["files"]), d["text"][:90]))
    if len(report["duplicates"]) > LIST_CAP:
        out.append("warn: ... %d more duplicated sentence(s)" % (len(report["duplicates"]) - LIST_CAP))
    for p in report["providers"]:
        if p["global_words"] is None:
            out.append("%s: always-injected words unknown (scripts/link.sh no longer names its rules file)" % p["provider"])
        else:
            out.append("%s: %d always-injected words (global rules), up to %d with the largest project templates"
                       % (p["provider"], p["global_words"], p["with_largest_project_templates_words"]))
    out.append("prompt audit: %d finding(s); advisory, --strict fails on them; patterns in "
               "config/prompt-audit-patterns.txt" % report["findings"])
    return "\n".join(out)


def main(argv):
    args = [a for a in argv if not a.startswith("--")]
    flags = {a for a in argv if a.startswith("--")}
    if len(args) != 1 or flags - {"--json", "--strict"}:
        print("usage: prompt-audit.py SOURCE_ROOT [--json] [--strict]", file=sys.stderr)
        return 2
    root = os.path.realpath(args[0])
    if not os.path.isdir(root):
        print("error: not a directory: %s" % args[0], file=sys.stderr)
        return 2
    report = build(root)
    print(json.dumps(report, ensure_ascii=False, indent=2) if "--json" in flags else render(report))
    return 1 if "--strict" in flags and report["findings"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
