#!/usr/bin/env python3
"""Read-only report of the instruction files each provider loads for a project.

Usage: instruction-budget.py PROJECT_DIR [--json] [--strict]

For Claude Code, Codex and Antigravity it lists the always-loaded rule files a
session started in PROJECT_DIR reads (user-level rules plus the project chain),
their size, the imports Claude follows, paragraphs that reach one provider
twice, and the one shadowing rule a provider documents. Files are read as text:
nothing is executed, no provider command runs and no model is called. Findings
are warnings; --strict turns them into exit 1.

How each provider's list is known is printed with it. Claude's comes from its
memory documentation. Codex's and Antigravity's lists are what this harness
installs and what their AGENTS.md convention names; they were not confirmed
against the provider's own page, and the report says so instead of guessing
further.
"""

import hashlib
import json
import os
import re
import subprocess
import sys

MAX_FILE_BYTES = 1048576
IMPORT_HOPS = 4          # Claude follows imports this deep and no further
DUPLICATE_MIN_WORDS = 12
LIST_CAP = 20


def env_int(name, default, low, high):
    raw = os.environ.get(name, "")
    if not raw.isdigit() or not low <= int(raw) <= high:
        return default
    return int(raw)


MAX_LINES = env_int("OMS_RULES_MAX_LINES", 200, 1, 100000)
MAX_TOTAL_BYTES = env_int("OMS_RULES_MAX_TOTAL_BYTES", 32768, 1, 16777216)


def physical(path):
    try:
        return os.path.realpath(path)
    except (OSError, ValueError):
        return path


def toplevel(project):
    try:
        done = subprocess.run(["git", "-C", project, "rev-parse", "--show-toplevel"],
                              capture_output=True, text=True, timeout=5,
                              stdin=subprocess.DEVNULL, check=False)
    except (OSError, subprocess.SubprocessError):
        return None
    top = done.stdout.strip()
    return physical(top) if done.returncode == 0 and top else None


def chain(project):
    """PROJECT_DIR and its ancestors up to the Git top level, outermost first; the directory alone outside Git."""
    top = toplevel(project)
    dirs, cur = [project], project
    while top and cur != top:
        parent = os.path.dirname(cur)
        if parent == cur or not (parent == top or parent.startswith(top + os.sep)):
            break
        dirs.append(parent)
        cur = parent
    return list(reversed(dirs))


def read_text(path):
    try:
        if os.path.getsize(path) > MAX_FILE_BYTES:
            return None
        with open(path, "rb") as handle:
            return handle.read().decode("utf-8", "replace")
    except OSError:
        return None


def label(path, project, home):
    for base, prefix in ((project, ""), (home, "~/")):
        if base and (path == base or path.startswith(base + os.sep)):
            return prefix + os.path.relpath(path, base)
    return path


IMPORT_RE = re.compile(r"(?:^|(?<=\s))@((?:~/|\.{1,2}/|/)?[A-Za-z0-9._][A-Za-z0-9._/\-]*)")


def imports(text, owner, home):
    """Paths a Claude instruction file imports with @path, outside code fences and code spans."""
    found, fenced = [], False
    for line in text.splitlines():
        if line.lstrip().startswith(("```", "~~~")):
            fenced = not fenced
            continue
        if fenced:
            continue
        line = re.sub(r"`[^`]*`", " ", line)
        for match in IMPORT_RE.finditer(line):
            target = match.group(1).rstrip(".,;:")
            if target.startswith("~/"):
                target = os.path.join(home, target[2:])
            elif not os.path.isabs(target):
                target = os.path.join(os.path.dirname(owner), target)
            if os.path.isfile(target) and target not in found:
                found.append(target)
    return found


def rule_files(directory):
    out = []
    if os.path.isdir(directory):
        for root, dirs, names in os.walk(directory):
            dirs.sort()
            for name in sorted(names):
                if name.endswith(".md"):
                    out.append(os.path.join(root, name))
            if len(out) > 200:
                break
    return out


def first_existing(*paths):
    for path in paths:
        if os.path.isfile(path):
            return path
    return None


def claude_sources(project, home, dirs):
    roots = [os.path.join(home, ".claude", "CLAUDE.md")] + rule_files(os.path.join(home, ".claude", "rules"))
    own = []
    for directory in dirs:
        for name in ("CLAUDE.md", os.path.join(".claude", "CLAUDE.md"), "CLAUDE.local.md"):
            path = os.path.join(directory, name)
            if os.path.isfile(path):
                own.append(path)
        roots += rule_files(os.path.join(directory, ".claude", "rules"))
    agents = [os.path.join(d, "AGENTS.md") for d in dirs if os.path.isfile(os.path.join(d, "AGENTS.md"))]
    return roots, own, agents


def provider_report(name, project, home, dirs):
    files, notes, shadowed, order = [], [], [], []
    seen = {}

    def add(path, via=None, hop=0):
        real = physical(path)
        if real in seen:
            if via is None and seen[real]["via"]:
                seen[real]["via"] = None  # also loaded directly
            return
        text = read_text(path)
        entry = {"path": label(path, project, home), "real": real, "via": via, "hop": hop,
                 "symlink": os.path.islink(path)}
        if text is None:
            entry.update(bytes=None, words=None, lines=None, unread=True)
        else:
            entry.update(bytes=len(text.encode("utf-8")), words=len(text.split()),
                         lines=text.count("\n") + (0 if text.endswith("\n") or not text else 1), text=text)
        seen[real] = entry
        order.append(entry)
        if name == "claude" and text is not None:
            for target in imports(text, real, home):
                if hop >= IMPORT_HOPS:
                    notes.append("%s imports %s past %d hops; Claude does not follow it"
                                 % (entry["path"], label(target, project, home), IMPORT_HOPS))
                    continue
                add(target, via=entry["path"], hop=hop + 1)

    if name == "claude":
        basis = "Claude Code memory documentation"
        roots, own, agents = claude_sources(project, home, dirs)
        for path in roots:
            if os.path.isfile(path):
                add(path)
        for path in own:
            add(path)
        if own:
            for path in agents:
                if physical(path) not in seen:
                    shadowed.append({"path": label(path, project, home),
                                     "by": label(own[0], project, home)})
        else:
            for path in agents:
                add(path)
    elif name == "codex":
        basis = "what this harness installs plus the AGENTS.md convention; Codex's own page was not confirmed"
        codex_home = os.environ.get("CODEX_HOME") or os.path.join(home, ".codex")
        path = first_existing(os.path.join(codex_home, "AGENTS.override.md"), os.path.join(codex_home, "AGENTS.md"))
        if path:
            add(path)
        for directory in dirs:
            path = first_existing(os.path.join(directory, "AGENTS.override.md"), os.path.join(directory, "AGENTS.md"))
            if path:
                add(path)
    else:
        basis = ("what this harness installs plus GEMINI.md; whether a project AGENTS.md is read depends on "
                 "Antigravity settings this report does not read")
        path = os.path.join(home, ".gemini", "AGENTS.md")
        if os.path.isfile(path):
            add(path)
        for directory in dirs:
            for candidate in ("GEMINI.md", "AGENTS.md"):
                path = os.path.join(directory, candidate)
                if os.path.isfile(path):
                    add(path)
    return {"provider": name, "basis": basis, "files": order, "notes": notes, "shadowed": shadowed}


def normalized(paragraph):
    lines = [re.sub(r"^\s*(?:[-*+]|\d+[.)])\s+", "", line) for line in paragraph.splitlines()]
    return " ".join(" ".join(lines).lower().split())


def duplicates(files):
    """Paragraphs of at least DUPLICATE_MIN_WORDS words that two different loaded files both carry."""
    owners, fenced_skip = {}, re.compile(r"^\s*(```|~~~)")
    for entry in files:
        text = entry.get("text")
        if not text:
            continue
        marked = set()
        for paragraph in re.split(r"\n\s*\n", text):
            if fenced_skip.match(paragraph):
                continue
            flat = normalized(paragraph)
            if len(flat.split()) < DUPLICATE_MIN_WORDS:
                continue
            digest = hashlib.sha256(flat.encode("utf-8")).hexdigest()[:12]
            if digest in marked:
                continue
            marked.add(digest)
            owners.setdefault(digest, {"words": len(flat.split()), "start": " ".join(flat.split()[:6]),
                                       "files": []})["files"].append(entry["path"])
    return [dict(digest=d, **row) for d, row in sorted(owners.items()) if len(row["files"]) > 1]


def build(project, home):
    dirs = chain(project)
    report = {"schema": 1, "kind": "oms-instruction-budget", "project": project,
              "limits": {"max_lines_per_file": MAX_LINES, "max_total_bytes": MAX_TOTAL_BYTES,
                         "import_hops": IMPORT_HOPS},
              "providers": [], "warnings": 0}
    for name in ("claude", "codex", "antigravity"):
        row = provider_report(name, project, home, dirs)
        known = [f for f in row["files"] if not f.get("unread")]
        row["total"] = {"files": len(row["files"]), "bytes": sum(f["bytes"] for f in known),
                        "words": sum(f["words"] for f in known), "lines": sum(f["lines"] for f in known)}
        row["duplicates"] = duplicates(row["files"])
        warnings = []
        for entry in row["files"]:
            if entry.get("unread"):
                warnings.append("%s could not be read or is larger than %d bytes; it is not counted"
                                % (entry["path"], MAX_FILE_BYTES))
            elif entry["lines"] > MAX_LINES:
                warnings.append("%s has %d lines, over the %d-line budget" % (entry["path"], entry["lines"], MAX_LINES))
        if row["total"]["bytes"] > MAX_TOTAL_BYTES:
            warnings.append("%d bytes of always-loaded instructions, over the %d-byte budget"
                            % (row["total"]["bytes"], MAX_TOTAL_BYTES))
        for dup in row["duplicates"]:
            warnings.append("the same %d-word paragraph is loaded from %s (\"%s…\")"
                            % (dup["words"], " and ".join(dup["files"][:3]), dup["start"]))
        for item in row["shadowed"]:
            warnings.append("%s is not read: Claude Code reads AGENTS.md only when no CLAUDE.md is present, and %s is"
                            % (item["path"], item["by"]))
        warnings.extend(row["notes"])
        row["warnings"] = warnings
        report["warnings"] += len(warnings)
        for entry in row["files"]:
            entry.pop("text", None)
            entry.pop("real", None)
        report["providers"].append(row)
    return report


def render(report):
    out = ["instruction budget for %s: always-loaded rule files per provider (read as text; nothing was run)"
           % os.path.basename(report["project"].rstrip(os.sep) or report["project"])]
    for row in report["providers"]:
        total = row["total"]
        out.append("%s: %d file(s), %d bytes, %d words, %d lines" %
                   (row["provider"], total["files"], total["bytes"], total["words"], total["lines"]))
        out.append("  basis: " + row["basis"])
        for entry in row["files"][:LIST_CAP]:
            size = "unread" if entry.get("unread") else "%6d B %5d w %4d l" % (entry["bytes"], entry["words"], entry["lines"])
            tail = ("  (imported by %s)" % entry["via"] if entry["via"] else "") + ("  (symlink)" if entry["symlink"] else "")
            out.append("  %s  %s%s" % (size, entry["path"], tail))
        if len(row["files"]) > LIST_CAP:
            out.append("  … %d more file(s) counted in the total" % (len(row["files"]) - LIST_CAP))
        if not row["files"]:
            out.append("  none found")
        for warning in row["warnings"][:LIST_CAP]:
            out.append("  warn: " + warning)
        if len(row["warnings"]) > LIST_CAP:
            out.append("  warn: … %d more" % (len(row["warnings"]) - LIST_CAP))
    out.append("budget: %d warning(s); limits %d lines per file (OMS_RULES_MAX_LINES), %d bytes per provider "
               "(OMS_RULES_MAX_TOTAL_BYTES); skills, hooks and path-scoped rules that load on demand are not counted"
               % (report["warnings"], report["limits"]["max_lines_per_file"], report["limits"]["max_total_bytes"]))
    return "\n".join(out)


def main(argv):
    args = [a for a in argv if not a.startswith("--")]
    flags = {a for a in argv if a.startswith("--")}
    if len(args) != 1 or flags - {"--json", "--strict"}:
        print("usage: instruction-budget.py PROJECT_DIR [--json] [--strict]", file=sys.stderr)
        return 2
    project = physical(args[0])
    if not os.path.isdir(project):
        print("error: not a directory: %s" % args[0], file=sys.stderr)
        return 2
    report = build(project, physical(os.path.expanduser("~")))
    print(json.dumps(report, ensure_ascii=False, indent=2) if "--json" in flags else render(report))
    return 1 if "--strict" in flags and report["warnings"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
