#!/usr/bin/env python3
"""Validate the repository skill catalog and local skill resources."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


REFERENCE_RE = re.compile(r"\]\((references/[^)#]+)(?:#[^)]+)?\)")


def frontmatter(text: str) -> dict[str, str]:
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return {}
    try:
        end = lines.index("---", 1)
    except ValueError:
        return {}
    values: dict[str, str] = {}
    current = ""
    for line in lines[1:end]:
        match = re.match(r"^([A-Za-z0-9_-]+):\s*(.*)$", line)
        if match:
            current = match.group(1)
            raw = match.group(2).strip()
            values[current] = "" if raw in {">", "|", ">-", "|-"} else raw
        elif current and line.startswith((" ", "\t")):
            values[current] = (values[current] + " " + line.strip()).strip()
    return values


def main() -> int:
    root = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
    manifest_path = root / "skills.manifest.json"
    errors: list[str] = []
    if not manifest_path.is_file():
        print(f"error: missing {manifest_path}", file=sys.stderr)
        return 1
    try:
        entries = json.loads(manifest_path.read_text(encoding="utf-8")).get("skills", [])
    except (OSError, json.JSONDecodeError) as exc:
        print(f"error: invalid {manifest_path}: {exc}", file=sys.stderr)
        return 1

    names: set[str] = set()
    sources: set[str] = set()
    local_sources: set[str] = set()
    for entry in entries:
        name = str(entry.get("name", "")).strip()
        source = str(entry.get("source", "")).strip()
        if not name or not source:
            errors.append("manifest entry requires non-empty name and source")
            continue
        if name in names:
            errors.append(f"duplicate skill name: {name}")
        if source in sources:
            errors.append(f"duplicate skill source: {source}")
        names.add(name)
        sources.add(source)
        if source.startswith("custom-skills/"):
            local_sources.add(source)
        else:
            print(f"external: {name} -> {source}")

    custom_root = root / "custom-skills"
    actual_sources = {
        path.parent.relative_to(root).as_posix()
        for path in custom_root.glob("*/SKILL.md")
    } if custom_root.is_dir() else set()
    for source in sorted(actual_sources - local_sources):
        errors.append(f"unlisted custom skill: {source}")

    for entry in entries:
        name = str(entry.get("name", "")).strip()
        source = str(entry.get("source", "")).strip()
        if not source.startswith("custom-skills/"):
            continue
        skill_dir = root / source
        skill_path = skill_dir / "SKILL.md"
        if not skill_dir.is_dir():
            errors.append(f"missing: {name} -> {source}")
            continue
        if not skill_path.is_file():
            errors.append(f"missing SKILL.md: {name} -> {source}/SKILL.md")
            continue
        content = skill_path.read_text(encoding="utf-8")
        metadata = frontmatter(content)
        actual_name = metadata.get("name", "")
        if not actual_name:
            errors.append(f"missing skill name: {source}/SKILL.md")
        elif actual_name != name:
            errors.append(f"name mismatch: {name} -> {source}/SKILL.md has {actual_name}")
        if not metadata.get("description", "").strip():
            errors.append(f"missing skill description: {source}/SKILL.md")

        targets = {match.group(1) for match in REFERENCE_RE.finditer(content)}
        for target in sorted(targets):
            target_path = skill_dir / target
            if not target_path.is_file() and not target.endswith(".generated.md"):
                errors.append(f"missing local reference: {source}/{target}")
        refs_dir = skill_dir / "references"
        if refs_dir.is_dir():
            referenced = {Path(target).as_posix() for target in targets}
            for ref_path in sorted(refs_dir.rglob("*.md")):
                relative = ref_path.relative_to(skill_dir).as_posix()
                if relative.endswith(".generated.md"):
                    continue
                if relative not in referenced:
                    errors.append(f"orphan local reference: {source}/{relative}")

        openai_yaml = skill_dir / "agents" / "openai.yaml"
        if openai_yaml.exists():
            yaml_text = openai_yaml.read_text(encoding="utf-8")
            if "default_prompt:" not in yaml_text or f"${name}" not in yaml_text:
                errors.append(f"invalid agent metadata: {source}/agents/openai.yaml")
        print(f"ok: {name} -> {source}")

    for error in errors:
        print(error)
    if errors:
        print("install-skills: failed")
        return 1
    print("install-skills: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
