#!/usr/bin/env python3
"""Build or execute explicit launch plans for VS Code, Stably Orca, and Codex."""

import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple
from urllib.parse import quote


THREAD_ID = re.compile(r"^[A-Za-z0-9._:-]{1,160}$")


def fail(message: str) -> "NoReturn":
    print("error: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def positive(value: str) -> int:
    try:
        number = int(value)
    except ValueError:
        raise argparse.ArgumentTypeError("must be a positive integer")
    if number <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def repo_root(path: str) -> Path:
    try:
        result = subprocess.run(
            ["git", "-C", path, "rev-parse", "--show-toplevel"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        fail("cannot resolve --repo: %s" % exc)
    if result.returncode != 0 or not result.stdout.strip():
        fail("--repo is not a git worktree")
    return Path(result.stdout.strip().rstrip("\r")).resolve()


def executable(value: str) -> Optional[str]:
    if not value:
        return None
    found = shutil.which(value)
    return str(Path(found).resolve()) if found else None


def probe(command: List[str]) -> Tuple[int, str]:
    try:
        result = subprocess.run(
            command,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired):
        return 1, ""
    return result.returncode, (result.stdout + result.stderr).strip()


def find_vscode() -> Optional[str]:
    override = os.environ.get("OMS_VSCODE_BIN", "")
    candidates = [override] if override else ["code", "code-insiders"]
    for candidate in candidates:
        binary = executable(candidate)
        if binary is None:
            continue
        status, output = probe([binary, "--version"])
        if status == 0 and output:
            return binary
    return None


def vscode_supports_agents(binary: str) -> bool:
    status, output = probe([binary, "--help"])
    return status == 0 and re.search(r"(?:^|\s)--agents(?:\s|$)", output) is not None


def find_orca() -> Optional[str]:
    override = os.environ.get("OMS_ORCA_BIN", "")
    exported = os.environ.get("ORCA_CLI_COMMAND", "")
    if override:
        candidates = [override]
    elif exported:
        candidates = [exported]
    elif os.environ.get("ORCA_DEV_REPO_ROOT"):
        candidates = ["orca-dev"]
    elif sys.platform.startswith("linux"):
        # Outside Orca-managed terminals, bare `orca` is normally the GNOME
        # screen reader. The JSON probe is still mandatory for every fallback.
        candidates = ["orca-ide", "orca"]
    else:
        candidates = ["orca"]
    for candidate in candidates:
        binary = executable(candidate)
        if binary is None:
            continue
        status, output = probe([binary, "status", "--json"])
        if status != 0:
            continue
        try:
            identity = json.loads(output)
        except json.JSONDecodeError:
            continue
        if isinstance(identity, dict):
            return binary
    return None


def find_codex() -> Optional[str]:
    override = os.environ.get("OMS_CODEX_BIN", "")
    candidates = [override] if override else ["codex"]
    for candidate in candidates:
        binary = executable(candidate)
        if binary is None:
            continue
        status, output = probe([binary, "--version"])
        if status == 0 and "codex" in output.lower():
            return binary
    return None


def resolve_file(repo: Path, raw: Optional[str]) -> Tuple[Path, str]:
    if not raw:
        fail("--file is required for this target")
    candidate = Path(raw)
    if not candidate.is_absolute():
        candidate = repo / candidate
    try:
        resolved = candidate.resolve(strict=True)
        relative = resolved.relative_to(repo)
    except (OSError, ValueError):
        fail("--file must be an existing file inside --repo")
    if not resolved.is_file():
        fail("--file must name a regular file")
    return resolved, relative.as_posix()


def vscode_plan(
    repo: Path, raw_file: Optional[str], line: Optional[int], column: Optional[int]
) -> Dict[str, Any]:
    binary = find_vscode()
    if binary is None:
        fail("VS Code CLI was not found or failed its identity probe")
    path, _ = resolve_file(repo, raw_file)
    command = [binary, "--reuse-window"]
    suffix = ""
    if line is not None:
        suffix = ":%d:%d" % (line, column or 1)
        command.extend(["--goto", str(path) + suffix])
    else:
        command.append(str(path))
    scheme = (
        "vscode-insiders"
        if Path(binary).name.lower().startswith("code-insiders")
        else "vscode"
    )
    uri_path = quote(path.as_posix(), safe="/:")
    if not uri_path.startswith("/"):
        uri_path = "/" + uri_path
    uri = "%s://file%s%s" % (scheme, uri_path, suffix)
    return {
        "schema": 1,
        "action": "open-in",
        "target": "vscode",
        "method": "cli",
        "command": command,
        "uri": uri,
    }


def vscode_agents_plan(repo: Path) -> Dict[str, Any]:
    binary = find_vscode()
    if binary is None:
        fail("VS Code CLI was not found or failed its identity probe")
    if not vscode_supports_agents(binary):
        fail("this VS Code CLI does not advertise the preview --agents window")
    return {
        "schema": 1,
        "action": "open-in",
        "target": "vscode",
        "mode": "agents",
        "method": "cli",
        "repo": str(repo),
        "command": [binary, "--agents", str(repo)],
    }


def orca_plan(repo: Path, raw_file: Optional[str]) -> Dict[str, Any]:
    binary = find_orca()
    if binary is None:
        fail("Stably Orca was not found or failed its status --json identity probe")
    _, relative = resolve_file(repo, raw_file)
    # Herdr/Orca's file subcommand has no documented `--` separator. Prefix a
    # repository-relative path so a filename such as `--worktree` cannot be
    # reinterpreted as another option.
    relative_arg = "./" + relative
    return {
        "schema": 1,
        "action": "open-in",
        "target": "orca",
        "method": "cli",
        "command": [
            binary,
            "file",
            "open",
            relative_arg,
            "--worktree",
            "path:" + str(repo),
            "--json",
        ],
    }


def codex_plan(thread: Optional[str], last: bool) -> Dict[str, Any]:
    binary = find_codex()
    if binary is None:
        fail("Codex CLI was not found or failed its identity probe")
    if bool(thread) == bool(last):
        fail("Codex requires exactly one of --thread ID or --last")
    if thread and not THREAD_ID.fullmatch(thread):
        fail("--thread has an invalid identifier")
    command = [binary, "resume", "--last" if last else str(thread)]
    return {
        "schema": 1,
        "action": "open-in",
        "target": "codex",
        "method": "cli",
        "command": command,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Open a repository file or resume a Codex task through a verified CLI."
    )
    parser.add_argument("--repo", default=".", help="repository containing the target file")
    parser.add_argument(
        "--target",
        choices=("auto", "vscode", "orca", "codex"),
        default="auto",
        help="verified launch adapter (default: infer from the supplied target)",
    )
    parser.add_argument("--file", help="existing file inside --repo")
    parser.add_argument("--line", type=positive, help="VS Code line number")
    parser.add_argument("--column", type=positive, help="VS Code column number")
    parser.add_argument("--thread", help="exact Codex task/thread identifier")
    parser.add_argument("--last", action="store_true", help="resume the last Codex task")
    parser.add_argument(
        "--agents-window",
        action="store_true",
        help="open this repository in the probed VS Code Agents window",
    )
    parser.add_argument("--dry-run", action="store_true", help="print the launch command without executing it")
    parser.add_argument("--json", action="store_true", help="print the plan as JSON without executing it")
    args = parser.parse_args()

    if args.column is not None and args.line is None:
        fail("--column requires --line")
    if args.file and (args.thread or args.last):
        fail("file and Codex task targets cannot be combined")
    if args.agents_window and (args.file or args.thread or args.last or args.line or args.column):
        fail("--agents-window cannot be combined with a file, line, or Codex task")
    if args.agents_window and args.target not in ("auto", "vscode"):
        fail("--agents-window requires --target vscode or auto")
    if args.target in ("orca", "codex") and (args.line or args.column):
        fail("--line and --column are supported only for VS Code")

    repo = repo_root(args.repo)
    target = args.target
    if target == "auto":
        if args.agents_window:
            target = "vscode"
        elif args.thread or args.last:
            target = "codex"
        elif args.file:
            target = "orca" if find_orca() is not None else "vscode"
        else:
            fail("auto selection requires --file, --thread, or --last")

    if target == "vscode":
        if args.thread or args.last:
            fail("VS Code accepts a file, not a Codex task identifier")
        if args.agents_window:
            plan = vscode_agents_plan(repo)
        else:
            plan = vscode_plan(repo, args.file, args.line, args.column)
    elif target == "orca":
        if args.thread or args.last:
            fail("Orca accepts a file, not a Codex task identifier")
        plan = orca_plan(repo, args.file)
    else:
        if args.file:
            fail("Codex resume does not accept --file")
        plan = codex_plan(args.thread, args.last)

    # A launcher may navigate to a session or file, but it never grants the
    # external frontend OMS approval, admission, landing, commit, or push
    # authority. Keep that boundary visible to machine consumers.
    plan["frontend_authority"] = "none"

    if args.json:
        print(json.dumps(plan, ensure_ascii=False, sort_keys=True))
    elif args.dry_run:
        print(shlex.join(plan["command"]))
    if args.json or args.dry_run:
        return 0
    try:
        result = subprocess.run(plan["command"], check=False)
    except OSError as exc:
        fail("launch failed: %s" % exc)
    return int(result.returncode)


if __name__ == "__main__":
    raise SystemExit(main())
