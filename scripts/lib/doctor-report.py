#!/usr/bin/env python3
"""Turn the existing doctor text contract into a bounded structured report.

The doctor remains the owner of every health decision.  This adapter only
classifies its already-rendered lines and extracts explicit remedies; it never
executes one and never upgrades a warning into mutation authority.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path
from typing import Dict, List, Optional


MAX_INPUT_BYTES = 2 * 1024 * 1024
LEVEL_PREFIXES = (
    ("ok: ", "ok"),
    ("note: ", "note"),
    ("optional missing: ", "note"),
    ("warn: ", "warn"),
    ("missing: ", "error"),
    ("broken: ", "error"),
    ("fail: ", "error"),
    ("hint: ", "hint"),
    ("displaced user config: ", "note"),
)


def bounded_input() -> str:
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise SystemExit("error: doctor output exceeds the 2 MiB report budget")
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise SystemExit("error: doctor output is not UTF-8") from exc


def level_and_message(line: str) -> Optional[tuple[str, str]]:
    for prefix, level in LEVEL_PREFIXES:
        if line.startswith(prefix):
            return level, line[len(prefix) :].strip()
    if line.startswith("doctor: "):
        return "summary", line[len("doctor: ") :].strip()
    return None


def extract_command(line: str) -> Optional[str]:
    candidates = (
        r"^hint:\s+run\s+(.+)$",
        r"\(run:\s*(.+?)\)$",
        r"\(run\s+(.+?)\)$",
        r"\(restore:\s*(.+?)\)$",
        r"\(log in once with:\s*(.+?)\)$",
    )
    for pattern in candidates:
        match = re.search(pattern, line)
        if match:
            command = match.group(1).strip()
            if command and len(command.encode("utf-8")) <= 4096:
                return command
    return None


def command_authority(command: str) -> str:
    lower = command.lower()
    if " log in" in " " + lower or lower.endswith(" login") or "auth login" in lower:
        return "operator_decision"
    if lower.startswith("oms uninstall") or lower.startswith("git push"):
        return "external_mutation"
    if any(
        token in lower
        for token in (
            "--apply",
            "--repair",
            " prune --files",
            " rebuild",
            "install-",
            "scripts/link.sh",
            "cleanup.sh",
        )
    ):
        return "local_mutation"
    return "read"


def parse_report(text: str, repo: str, exit_code: int) -> Dict[str, object]:
    section = "summary"
    findings: List[Dict[str, str]] = []
    remediations: List[Dict[str, str]] = []
    seen_commands = set()
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        if line.startswith("# "):
            section = line[2:].strip()[:160] or "summary"
            continue
        classified = level_and_message(line)
        if classified is not None:
            level, message = classified
            findings.append(
                {"section": section, "level": level, "message": message[:4096]}
            )
        command = extract_command(line)
        if command is None or command in seen_commands:
            continue
        seen_commands.add(command)
        remediations.append(
            {
                "command": command,
                "authority": command_authority(command),
                "source": line[:4096],
            }
        )
    counts: Dict[str, int] = {}
    for finding in findings:
        level = finding["level"]
        counts[level] = counts.get(level, 0) + 1
    return {
        "schema": 1,
        "repo": os.path.realpath(repo),
        "status": "ok" if exit_code == 0 else "failed",
        "exit": exit_code,
        "counts": dict(sorted(counts.items())),
        "findings": findings,
        "remediations": remediations,
    }


def render_plan(report: Dict[str, object]) -> str:
    actions = report["remediations"]
    assert isinstance(actions, list)
    lines = ["doctor remediation plan: %d item(s)" % len(actions)]
    if not actions:
        lines.append("no explicit remediation was reported")
        return "\n".join(lines) + "\n"
    for index, action in enumerate(actions, 1):
        assert isinstance(action, dict)
        lines.append(
            "%d. [%s] %s" % (index, action["authority"], action["command"])
        )
        lines.append("   source: %s" % action["source"])
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--exit", type=int, required=True, dest="exit_code")
    parser.add_argument("--plan", action="store_true")
    args = parser.parse_args()
    if args.exit_code < 0 or args.exit_code > 255:
        parser.error("--exit must be between 0 and 255")
    repo = Path(args.repo)
    if not repo.is_dir():
        parser.error("--repo must name a directory")
    report = parse_report(bounded_input(), str(repo), args.exit_code)
    if args.plan:
        sys.stdout.write(render_plan(report))
    else:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    return args.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
