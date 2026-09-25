"""Shared read-only peer artifact sections and recent log windows."""

from __future__ import annotations

from pathlib import Path
import json
import math
import re
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


def debate_sections(answer: str) -> tuple[str, list[tuple[str, str]]]:
    """Ignore quoted headings, retaining the last response after prompt echoes."""
    lines = answer.splitlines()
    headers = {"Answer", "Findings", "Alternatives", "Counterargument", "Evidence", "Tradeoffs", "Risks", "Missing tests",
               "Recommendation", "Verification", "Changed from previous round", "Remaining disagreements"}
    entries = []
    fence = ""
    for i, line in enumerate(lines):
        match = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", line)
        if fence:
            if (match and match[1][0] == fence[0] and len(match[1]) >= len(fence)
                    and not match[2].strip()):
                fence = ""
            continue
        if match:
            fence = match[1]
            continue
        if line.expandtabs(4).startswith("    ") or line.lstrip().startswith(">"):
            continue
        value = line.strip()
        value, atx = re.subn(r"^#{1,6}\s+", "", value)
        if atx:
            value = re.sub(r"\s+#+$", "", value)
        bold = re.match(r"^(\*\*|__)(.+?)\1(.*)$", value)
        if bold:
            value = bold[2] + bold[3]
            # A heading's parenthetical qualification is metadata, not evidence
            # for an otherwise empty section (e.g. **Verification** (reported):).
            if bold[2] in headers:
                qualified = re.fullmatch(r"\s+\([^()\n]*\)\s*:(.*)", bold[3])
                if qualified:
                    value = bold[2] + ":" + qualified[1]
        heading, colon, rest = value.partition(":")
        if heading in headers and (colon or atx or bold):
            entries.append((i, heading, rest.strip()))
    starts = [i for i, (_, heading, _) in enumerate(entries) if heading in ("Answer", "Findings")]
    if starts:
        entries = entries[starts[-1]:]
    sections = []
    for j, (i, heading, rest) in enumerate(entries):
        end = entries[j + 1][0] if j + 1 < len(entries) else len(lines)
        sections.append((heading, "\n".join([rest] + lines[i + 1:end]).strip()))
    body = "\n".join(lines[entries[0][0]:]).strip() if entries else answer
    return body, sections


def valid_deliberation(answer: str) -> bool:
    """Check the response contract, not whether its reasoning is correct."""
    answer = re.split(r"(?m)^(?:model-result:|tokens used$|usage detail:|served model$|cost usd$)", answer)[0]
    _, sections = debate_sections(answer)
    required = ("Answer", "Alternatives", "Evidence", "Counterargument",
                "Risks", "Recommendation", "Verification", "Changed from previous round", "Remaining disagreements")
    for key in required:
        bodies = [body for heading, body in sections if heading == key]
        if len(bodies) != 1 or not bodies[0].strip():
            return False
        if key not in ("Risks", "Changed from previous round", "Remaining disagreements"):
            if bodies[0].strip().lower().rstrip(".! ") in ("none", "n/a", "unknown", "tbd"):
                return False
    return True


def debate_unchanged(answer: str) -> bool:
    _, sections = debate_sections(answer)
    changes = [body for key, body in sections if key == "Changed from previous round"]
    # Ambiguous/duplicate sections or any additional prose must not stop calls.
    return len(changes) == 1 and changes[0].lower().rstrip(".! \t\r\n") in ("none", "unchanged")


def bounded_answer(answer: str, limit: int) -> str:
    raw = answer.encode("utf-8")
    if len(raw) <= limit:
        return answer
    marker = "\n[TRUNCATED: read full answer]\n"
    room = max(0, limit - len(marker.encode()))
    if not room:
        return marker.encode()[:limit].decode("utf-8", errors="ignore")
    head = (room * 2 + 2) // 3
    tail = room - head
    return (raw[:head].decode("utf-8", errors="ignore") + marker
            + (raw[-tail:].decode("utf-8", errors="ignore") if tail else ""))


def stage_context(source: Path, root: Path, relative: str) -> None:
    """Copy only the run's bounded, sanitized evidence, never parent state."""
    rel = Path(relative)
    if not re.fullmatch(r"\.oms/artifacts/council-context\.[A-Za-z0-9]+", relative):
        raise ValueError("invalid council context destination")
    if any(path.is_symlink() for path in (source, *source.parents)):
        raise ValueError("symlinked council context")
    files = list(source.iterdir())
    if not 1 <= len(files) <= 16:
        raise ValueError("invalid council context size")
    for path in files:
        if (path.is_symlink() or not path.is_file() or path.stat().st_size > 131072
                or not re.fullmatch(r"request\.md|source\.txt|answer-[0-9]+\.md", path.name)):
            raise ValueError("invalid council context file")
    target = root.resolve()
    for part in rel.parts:
        target = target / part
        if target.is_symlink():
            raise ValueError("symlinked council context destination")
        target.mkdir(exist_ok=True)
    for path in files:
        dest = target / path.name
        if dest.exists() or dest.is_symlink():
            raise ValueError("council context destination already exists")
        with path.open("rb") as handle:
            data = handle.read(131073)
        if len(data) > 131072:
            raise ValueError("council context file grew beyond its limit")
        with dest.open("xb") as output:
            output.write(data)


def usage_footer(provider: str, usages: list[dict], cost=None) -> list[str]:
    """Provider-reported observation only; missing fields are not zero cost."""
    def total(key):
        values = [row.get(key) for row in usages]
        if not values or any(type(value) is not int or value < 0 for value in values):
            return None
        return sum(values)

    row = {"provider": provider, "input_tokens": total("input_tokens"),
           "output_tokens": total("output_tokens"),
           "cache_read_tokens": total("cached_input_tokens" if provider == "codex" else "cache_read_input_tokens"),
           "cache_write_tokens": total("cache_creation_input_tokens") if provider == "claude" else None,
           "cache_in_input": provider == "codex", "reported_cost_usd": None}
    if type(cost) in (int, float) and math.isfinite(cost) and cost >= 0:
        row["reported_cost_usd"] = cost
    # Retain the historical tokens-used field and its meaning for consumers.
    tokens = sum(row[key] or 0 for key in ("input_tokens", "output_tokens"))
    out = ["tokens used", str(tokens)] if tokens else []
    if row["reported_cost_usd"] is not None:
        out += ["cost usd", ("%.6f" % cost).rstrip("0").rstrip(".") or "0"]
    out.append("usage detail: " + json.dumps(row, sort_keys=True, allow_nan=False))
    return out


def debate_excerpt(answer: str, limit: int) -> str:
    """Keep current positions as well as deltas; fresh calls have no prior memory."""
    body, sections = debate_sections(answer)
    if not sections or sections[0][0] not in ("Answer", "Findings"):
        return bounded_answer(answer, limit)
    if len(body.encode("utf-8")) <= limit:
        return body
    marker = "\n[TRUNCATED: read full answer]"
    prefixes = [key + ":\n" for key, _ in sections]
    raw = [content.encode("utf-8") for _, content in sections]
    remaining = limit - sum(len(prefix.encode("utf-8")) + 1 for prefix in prefixes)
    marker_size = len(marker.encode("utf-8"))
    if remaining // len(sections) <= marker_size:
        return bounded_answer(body, limit)
    # Water-fill: small sections keep all their evidence; unused quota goes
    # to longer sections instead of discarding most of the available context.
    budgets = [0] * len(sections)
    pending = list(range(len(sections)))
    while pending:
        share = remaining // len(pending)
        short = [i for i in pending if len(raw[i]) <= share]
        if not short:
            for i in pending:
                budgets[i] = share
            break
        for i in short:
            budgets[i] = len(raw[i])
            remaining -= budgets[i]
            pending.remove(i)
    result = []
    for prefix, content_bytes, available in zip(prefixes, raw, budgets):
        content = content_bytes.decode("utf-8")
        if len(content_bytes) > available:
            content = content_bytes[:available - marker_size].decode("utf-8", errors="ignore") + marker
        result.append(prefix + content + "\n")
    return "".join(result).rstrip()


if __name__ == "__main__":
    try:
        if len(sys.argv) == 5 and sys.argv[1] == "--stage-context":
            stage_context(Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4])
            raise SystemExit(0)
        answer, _ = artifact_sections(Path(sys.argv[1]), require_exit=False)
        if len(sys.argv) == 4 and sys.argv[2] == "--debate-excerpt":
            answer = debate_excerpt(answer, min(4096, int(sys.argv[3])))
        elif len(sys.argv) == 3 and sys.argv[2] == "--deliberation-check":
            raise SystemExit(0 if valid_deliberation(answer) else 1)
        elif len(sys.argv) == 3 and sys.argv[2] == "--debate-unchanged":
            raise SystemExit(0 if debate_unchanged(answer) else 1)
        if answer:
            print(answer)
    except (OSError, ValueError) as exc:
        print("error: %s" % exc, file=sys.stderr)
        raise SystemExit(1)
