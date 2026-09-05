"""Bounded delivery over the existing append-only thread log; no new authority."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import stat
import time


ID = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9._-]{0,159}\Z")
MAX_ROW = 65536
MAX_FILE = 16 * 1024 * 1024
DURABLE = runpy.run_path(str(Path(__file__).with_name("durable-jsonl.py")))


def safe_path(repo, path, create_parent=False):
    try:
        return Path(DURABLE["canonical_repo_path"](str(repo), str(path), "thread", create_parent))
    except SystemExit:
        raise ValueError("unsafe thread path") from None


def thread_path(repo, thread):
    if not isinstance(thread, str) or not ID.fullmatch(thread):
        raise ValueError("invalid thread id")
    return safe_path(repo, Path(".oms") / "threads" / (thread + ".jsonl"))


def open_thread(repo, thread):
    path = thread_path(repo, thread)
    fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
    handle = os.fdopen(fd, "rb")
    info = os.fstat(fd)
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
            or (info.st_dev, info.st_ino) != (path.stat().st_dev, path.stat().st_ino)):
        handle.close()
        raise ValueError("thread identity changed")
    return handle


def create_thread(repo, thread):
    if not ID.fullmatch(thread):
        raise ValueError("invalid thread id")
    path = safe_path(repo, Path(".oms") / "threads" / (thread + ".jsonl"), True)
    try:
        fd = os.open(str(path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        thread_path(repo, thread)
        return "exists"
    os.close(fd)
    return "created"


def anchor(handle, offset):
    handle.seek(0)
    first = handle.read(min(offset, 256))
    handle.seek(max(0, offset - 256))
    return hashlib.sha256(first + handle.read(min(offset, 256))).hexdigest()


def cursor_for(handle, thread, offset):
    info = os.fstat(handle.fileno())
    value = [1, thread, info.st_dev, info.st_ino, offset, anchor(handle, offset)]
    return base64.urlsafe_b64encode(json.dumps(value, separators=(",", ":")).encode()).decode().rstrip("=")


def cursor_offset(handle, thread, cursor):
    if not cursor:
        return 0
    if not isinstance(cursor, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,1024}", cursor):
        raise ValueError("invalid thread cursor")
    try:
        value = json.loads(base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4)))
        version, tid, dev, ino, offset, digest = value
    except (ValueError, TypeError):
        raise ValueError("invalid thread cursor") from None
    info = os.fstat(handle.fileno())
    if (version != 1 or tid != thread or (dev, ino) != (info.st_dev, info.st_ino)
            or type(offset) is not int or offset < 0 or offset > info.st_size
            or digest != anchor(handle, offset)):
        raise ValueError("stale thread cursor; inspect the thread before restarting delivery")
    if offset:
        handle.seek(offset - 1)
        if handle.read(1) != b"\n":
            raise ValueError("cursor is not at a complete turn")
    return offset


def updates(repo, thread, after="", budget=6000, limit=12, wait=0):
    if (type(budget) is not int or not 1 <= budget <= MAX_ROW
            or type(limit) is not int or not 1 <= limit <= 200
            or type(wait) not in (int, float) or not 0 <= wait <= 30):
        raise ValueError("updates requires bytes 1..65536, turns 1..200, wait 0..30")
    deadline = time.monotonic() + wait
    while True:
        with open_thread(repo, thread) as handle:
            offset = cursor_offset(handle, thread, after)
            handle.seek(offset)
            rows, used = [], 0
            for _ in range(limit):
                line = handle.readline(MAX_ROW + 1)
                if len(line) > MAX_ROW:
                    raise ValueError("thread row exceeds 65536 bytes")
                if not line or not line.endswith(b"\n"):
                    break  # A concurrent writer's unfinished row is not delivered.
                row = json.loads(line)
                if not isinstance(row, dict) or row.get("thread") != thread:
                    raise ValueError("invalid thread row")
                if used + len(line) > budget:
                    if not rows:
                        raise ValueError("next complete turn exceeds byte budget; increase --max-bytes")
                    break
                rows.append(row)
                used += len(line)
                offset += len(line)
            cursor = cursor_for(handle, thread, offset)
            more = offset < os.fstat(handle.fileno()).st_size
        if rows or time.monotonic() >= deadline:
            return {"schema": 1, "thread": thread, "turns": rows,
                    "cursor": cursor, "has_more": more}
        # Bind an initially empty read before waiting, including file identity.
        after = cursor
        time.sleep(min(0.2, max(0, deadline - time.monotonic())))


def append_fields(repo, thread, after, consumer):
    if not ID.fullmatch(consumer):
        raise ValueError("consumer must be a bounded session identifier")
    with open_thread(repo, thread) as handle:
        if not after:
            raise ValueError("ack requires the cursor actually consumed")
        cursor_offset(handle, thread, after)
    return {"receipt": "ack", "consumer": consumer, "after": after}


def append_from_env():
    env = os.environ
    path = Path(env["OMS_TH_FILE"])
    repo, thread = path.parents[2], env["OMS_TH_ID"]
    fields = {}
    if env.get("OMS_TH_ACK"):
        fields = append_fields(repo, thread, env["OMS_TH_ACK"], env.get("OMS_TH_CONSUMER", ""))
    with open_thread(repo, thread) as handle:
        data = handle.read(MAX_FILE + 1)
    if len(data) > MAX_FILE or (data and not data.endswith(b"\n")):
        raise ValueError("thread is oversized or has an incomplete turn")
    rows = [json.loads(line) for line in data.splitlines() if line.strip()]
    if any(not isinstance(row, dict) for row in rows):
        raise ValueError("invalid thread row")
    if any(row.get("role") == "closed" for row in rows):
        raise ValueError("thread is closed")
    if fields and any(all(row.get(key) == val for key, val in fields.items()) for row in rows):
        return  # Replayed acknowledgment is idempotent under the existing lock.
    text_file = env.get("OMS_TH_TEXT_FILE")
    text = Path(text_file).read_text(encoding="utf-8", errors="replace").strip() if text_file else ""
    try:
        budget = max(1, min(32768, int(env.get("OMS_TH_MAX", "4000"))))
    except ValueError:
        budget = 4000
    if len(text.encode()) > budget:
        text = "[earlier output truncated: see artifact]\n" + text.encode()[-budget:].decode("utf-8", "ignore")
    row = {"schema": 1, "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
           "thread": thread, "seq": max((r.get("seq", 0) for r in rows), default=0) + 1,
           "role": env["OMS_TH_ROLE"], "agent": env.get("OMS_TH_AGENT") or "agent", "text": text}
    for key in ("provider", "model", "artifact", "quality"):
        if env.get("OMS_TH_" + key.upper()):
            row[key] = env["OMS_TH_" + key.upper()]
    if env.get("OMS_TH_LIVE") == "1":
        row["live"] = True
    row.update(fields)
    encoded = (json.dumps(row, ensure_ascii=False) + "\n").encode()
    if len(encoded) > MAX_ROW or len(data) + len(encoded) > MAX_FILE:
        raise ValueError("thread storage budget exceeded")
    thread_path(repo, thread)
    DURABLE["append"](str(path), encoded, "thread")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("updates", "append", "check", "create"))
    parser.add_argument("--repo", default=".")
    parser.add_argument("--id", default="")
    parser.add_argument("--after", default="")
    parser.add_argument("--max-bytes", type=int, default=6000)
    parser.add_argument("--turns", type=int, default=12)
    parser.add_argument("--wait", type=float, default=0)
    args = parser.parse_args()
    try:
        if args.action == "append":
            append_from_env()
        elif args.action == "check":
            thread_path(args.repo, args.id)
        elif args.action == "create":
            print(create_thread(args.repo, args.id))
        else:
            print(json.dumps(updates(args.repo, args.id, args.after, args.max_bytes, args.turns, args.wait), ensure_ascii=False))
    except (OSError, ValueError, TypeError, RecursionError) as exc:
        parser.exit(2, "error: thread: %s\n" % exc)


if __name__ == "__main__":
    main()
