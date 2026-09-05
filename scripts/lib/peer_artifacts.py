"""Shared read-only peer artifact sections and recent log windows."""

from pathlib import Path
import sys


def tail_lines(path: Path, limit: int, max_bytes: int | None = None) -> tuple[list[str], bool]:
    """Read backwards; byte-capped display callers may receive a leading fragment."""
    if limit <= 0:
        return [], False
    with path.open("rb") as handle:
        handle.seek(0, 2)
        position = handle.tell()
        chunks = []
        size = newlines = 0
        while position and newlines <= limit:
            take = min(position, 8192)
            if max_bytes is not None:
                take = min(take, max_bytes - size)
            if take <= 0:
                break
            position -= take
            handle.seek(position)
            block = handle.read(take)
            chunks.append(block)
            size += len(block)
            newlines += block.count(b"\n")
        lines = b"".join(reversed(chunks)).decode("utf-8", errors="replace").splitlines()
    return lines[-limit:], bool(position and len(lines) <= limit)


def artifact_sections(path: Path, *, require_exit: bool = True) -> tuple[str, str]:
    """Select the last Output before the last Exit, excluding quoted templates.

    MCP exposes only completed sections; shell callers may still inspect a
    partial artifact. Read sequentially without retaining the composed prompt.
    """
    start = completed_start = completed_end = None
    exit_seen = partial = False
    exit_code = ""
    with path.open("rb") as handle:
        for raw in handle:
            line = raw.rstrip(b"\r\n")
            if exit_seen and not exit_code and line.strip():
                exit_code = line.decode("utf-8", errors="replace").strip()
            if line == b"## Output":
                start = handle.tell()
                partial = True
            elif line == b"## Exit":
                completed_start, completed_end = start, handle.tell() - len(raw)
                exit_seen = True
                partial = False
                exit_code = ""
        if partial and not require_exit:
            completed_start, completed_end = start, handle.tell()
            exit_code = ""
        if completed_start is None or completed_end is None:
            return "", exit_code
        handle.seek(completed_start)
        body = handle.read(completed_end - completed_start).decode("utf-8", errors="replace")
    return "\n".join(body.splitlines()).strip(), exit_code


if __name__ == "__main__":
    try:
        answer, _ = artifact_sections(Path(sys.argv[1]), require_exit=False)
        if answer:
            print(answer)
    except OSError as exc:
        print("error: %s" % exc, file=sys.stderr)
        raise SystemExit(1)
