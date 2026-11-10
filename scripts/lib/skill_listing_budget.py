#!/usr/bin/env python3
"""Advisory, read-only view of what the skill listing costs each CLI.

Every installed skill puts its name and description into the list an agent
sees before any skill is opened. This reads only SKILL.md frontmatter under
the skill roots OMS already checks, plus skills a Claude Code plugin shipped,
and reports:

- per CLI, the number of skills and the summed description length against an
  OMS budget (OMS_SKILL_LISTING_CHARS, default 8000 characters: the fallback
  Codex documents for its initial skill list when the window size is unknown);
- a Claude skill whose description plus when_to_use is longer than the 1,536
  characters Claude Code documents as the per-skill cut;
- a plugin-shipped skill that carries the same name as a user-level one;
- two differently named skills in one CLI whose descriptions nearly coincide.

Nothing is a failure and nothing is changed; no model is called.
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from typing import Dict, List, Optional, Tuple

MAX_SKILLS = 500
MAX_READ = 65536
MAX_PLUGIN_DIRS = 4000
CLAUDE_CUT = 1536
SIMILAR = 0.8
WORD_RE = re.compile(r"[a-z0-9]+")


def budget() -> int:
    try:
        value = int(os.environ.get("OMS_SKILL_LISTING_CHARS", "8000"))
    except ValueError:
        value = 8000
    return max(500, min(1000000, value))


def frontmatter(path: Path) -> Optional[Dict[str, str]]:
    try:
        if not path.is_file():
            return None
        with path.open("rb") as handle:
            text = handle.read(MAX_READ).decode("utf-8", errors="replace")
    except OSError:
        return None
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return None
    values: Dict[str, str] = {}
    key = ""
    for line in lines[1:]:
        if line.strip() == "---":
            break
        match = re.match(r"^([A-Za-z0-9_-]+):\s*(.*)$", line)
        if match:
            key = match.group(1)
            values.setdefault(key, match.group(2).strip().strip("\"'"))
            if values[key] in (">", "|", ">-", "|-"):
                values[key] = ""
        elif key and line[:1] in (" ", "\t") and key in ("description", "when_to_use"):
            values[key] = (values[key] + " " + line.strip()).strip()
    return values


def root_skills(root: Path) -> List[Tuple[str, int, int, str]]:
    """(name, description length, description + when_to_use length, description) per skill directly under root."""
    found: List[Tuple[str, int, int, str]] = []
    try:
        children = sorted(root.iterdir(), key=lambda item: item.name)
    except OSError:
        return found
    for child in children:
        if len(found) >= MAX_SKILLS or ".backup." in child.name:
            continue
        values = frontmatter(child / "SKILL.md")
        if not values or not values.get("name"):
            continue
        description = values.get("description", "")
        found.append((values["name"], len(description), len(description) + len(values.get("when_to_use", "")), description))
    return found


def plugin_skills(cache: Path) -> List[Tuple[str, str]]:
    """(skill name, plugin directory name) for SKILL.md files under a `skills` directory in the plugin cache."""
    found: List[Tuple[str, str]] = []
    seen = 0
    stack = [(cache, 0)]
    while stack and seen < MAX_PLUGIN_DIRS and len(found) < MAX_SKILLS:
        directory, depth = stack.pop()
        seen += 1
        try:
            children = [child for child in directory.iterdir() if child.is_dir() and not child.is_symlink()]
        except OSError:
            continue
        for child in sorted(children, key=lambda item: item.name):
            if child.name in (".git", "node_modules"):
                continue
            if directory.name == "skills":
                values = frontmatter(child / "SKILL.md")
                if values and values.get("name"):
                    try:
                        plugin = "/".join(directory.parent.relative_to(cache).parts[:3]) or directory.parent.name
                    except (ValueError, IndexError):
                        plugin = directory.parent.name
                    found.append((values["name"], plugin))
            elif depth < 6:
                stack.append((child, depth + 1))
    return found


def tokens(text: str) -> frozenset:
    return frozenset(word for word in WORD_RE.findall(text.lower()) if len(word) > 2)


def main() -> int:
    home = Path(os.environ.get("HOME", "")) if os.environ.get("HOME") else Path.home()
    limit = budget()
    groups = (
        ("Codex", (home / ".codex" / "skills", home / ".agents" / "skills")),
        ("Claude", (home / ".claude" / "skills",)),
        ("Antigravity", (home / ".gemini" / "config" / "skills",)),
    )
    notes = 0
    out: List[str] = []
    claude_names: Dict[str, bool] = {}
    for label, roots in groups:
        skills: Dict[str, Tuple[int, int, str]] = {}
        for root in roots:
            for name, length, full, description in root_skills(root):
                skills.setdefault(name, (length, full, description))
        if not skills:
            continue
        total = sum(value[0] for value in skills.values())
        line = "%s: %d skill(s), descriptions total %d characters" % (label, len(skills), total)
        if total > limit:
            notes += 1
            out.append("warn: %s, over the %d-character listing budget (OMS_SKILL_LISTING_CHARS); "
                       "a CLI may shorten or drop entries it cannot fit" % (line, limit))
        else:
            out.append("ok: %s (budget %d)" % (line, limit))
        if label == "Claude":
            claude_names = {name: True for name in skills}
            for name in sorted(skills):
                if skills[name][1] > CLAUDE_CUT:
                    notes += 1
                    out.append("warn: Claude skill %s lists %d characters of description and when_to_use; "
                               "Claude Code cuts the listing at %d" % (name, skills[name][1], CLAUDE_CUT))
        bags = [(name, tokens(skills[name][2])) for name in sorted(skills)]
        pairs = 0
        for index, (left, left_bag) in enumerate(bags):
            for right, right_bag in bags[index + 1:]:
                if len(left_bag) < 6 or len(right_bag) < 6 or pairs >= 10:
                    continue
                overlap = len(left_bag & right_bag) / float(len(left_bag | right_bag))
                if overlap >= SIMILAR:
                    pairs += 1
                    notes += 1
                    out.append("note: %s skills %s and %s have nearly the same description (word overlap %d%%); "
                               "an agent may not tell them apart" % (label, left, right, round(overlap * 100)))
    cache = home / ".claude" / "plugins" / "cache"
    if cache.is_dir() and not cache.is_symlink():
        shipped = plugin_skills(cache)
        clashes = sorted({(name, plugin) for name, plugin in shipped if name in claude_names})
        if shipped:
            out.append("ok: Claude plugins ship %d skill(s) (read from the plugin cache; not counted in the totals above)"
                       % len(shipped))
        for name, plugin in clashes[:20]:
            notes += 1
            out.append("note: skill %s is installed at user level and also shipped by a Claude plugin (cache: %s); "
                       "the plugin copy is addressed as plugin:skill, so both are listed" % (name, plugin))
    if not out:
        out.append("skip: no installed skills found")
    sys.stdout.write("\n".join(out) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
