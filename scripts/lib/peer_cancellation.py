"""Cooperative cancellation receipts shared by the MCP reader and call owner."""
import json
import os
import stat
from pathlib import Path


def valid_receipt(directory: Path, name: str) -> bool:
    path = directory / (name + ".json")
    try:
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 4096:
            return False
        row = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    return isinstance(row, dict) and row.get("schema") == 1 and row.get("task_id") == directory.name and row.get("kind") == "mcp-task-" + name


def observed(directory: Path) -> None:
    path = directory / (".cancel-observed.%d.part" % os.getpid())
    row = {"schema": 1, "task_id": directory.name, "kind": "mcp-task-cancel-observed"}
    try:
        with path.open("x", encoding="utf-8") as handle:
            json.dump(row, handle)
        os.replace(path, directory / "cancel-observed.json")
    finally:
        if path.exists():
            path.unlink()
