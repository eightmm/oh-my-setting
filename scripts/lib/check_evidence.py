"""Bounded, scrubbed CI failure diagnostics; never verification receipts."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid

from output_digest import ANSI, CONTROL

LIB = Path(__file__).resolve().parent
LOG_BYTES = 256 * 1024
FILE_BYTES = 1024 * 1024
TOTAL_BYTES = 4 * 1024 * 1024
MAX_RECORDS = 16
RESERVE_BYTES = 8192
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_./:-]{0,199}\Z")
RECORD = re.compile(r"failure-[0-9a-f]{32}\.json\Z")
JSON_ESCAPE = re.compile(r'\\(?:u[dD][89aAbB][0-9a-fA-F]{2}\\u[dD][c-fC-F][0-9a-fA-F]{2}|["\\/bfnrt]|u[0-9a-fA-F]{4})')
SELECTOR_BASIS = "recognized failure headers only; absent means not recorded"
RUNTIME_BASIS = "check supervisor and evidence collector; fixture subprocesses may select other tools"
OMISSION_REASON = "aggregate byte or record limit"
IMAGE_FIELDS = {"ImageOS", "ImageVersion", "RUNNER_OS", "RUNNER_ARCH"}


class Refused(Exception):
    pass


@contextlib.contextmanager
def directory(path: Path, create: bool = False):
    """Walk with directory descriptors: no parent or leaf symlink is followed."""
    parts = Path(os.path.abspath(path)).parts
    fd = os.open(parts[0], os.O_RDONLY | os.O_DIRECTORY)
    try:
        for index, part in enumerate(parts[1:], 1):
            if create and index == len(parts) - 1:
                try:
                    os.mkdir(part, 0o700, dir_fd=fd)
                except FileExistsError:
                    pass
            next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = next_fd
        info = os.fstat(fd)
        if create and (info.st_uid != os.getuid() or info.st_mode & 0o022):
            raise Refused("unsafe-directory")
        yield fd
    finally:
        os.close(fd)


def identity(info):
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns


def read_at(parent: int, name: str, limit: int) -> bytes:
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise Refused("unsafe-file")
        if before.st_size > limit:
            raise Refused("size-limit")
        chunks, remaining = [], limit + 1
        while remaining:
            block = os.read(fd, min(65536, remaining))
            if not block:
                break
            chunks.append(block)
            remaining -= len(block)
        data = b"".join(chunks)
        if len(data) > limit:
            raise Refused("size-limit")
        if (identity(before) != identity(os.fstat(fd)) or
                identity(before) != identity(os.stat(name, dir_fd=parent, follow_symlinks=False))):
            raise Refused("changed-file")
        return data
    finally:
        os.close(fd)


def write_at(parent: int, name: str, data: bytes):
    fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
    except BaseException:
        os.unlink(name, dir_fd=parent)
        raise


def encoded(value) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def scrub(text: str, redact: bool = False) -> str:
    # The shared redactor scans all source bytes before path normalization.
    # No truncation or per-line clipping can hide a match from the final scan.
    with tempfile.TemporaryDirectory(prefix="oms-check-scan-") as scratch:
        path = Path(scratch) / "input.txt"
        path.write_text(text, encoding="utf-8")
        script = '''set -euo pipefail
. "$1"
if [ "$3" = redact ]; then
  rc=0
  agent_memory_redact_secret_lines "$2" >/dev/null || rc=$?
  [ "$rc" -le 1 ] || exit 2
  agent_memory_normalize_machine_paths < "$2" > "$2.clean"
  mv "$2.clean" "$2"
fi
rc=0
agent_memory_file_sensitive_status "$2" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ]
'''
        environment = dict(os.environ, OMS_LIB_TMPDIR=scratch)
        try:
            result = subprocess.run(["bash", "-c", script, "oms-check-scan",
                                     str(LIB / "agent-memory-common.sh"), str(path),
                                     "redact" if redact else "scan"], env=environment,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
        except (OSError, subprocess.SubprocessError):
            raise Refused("scan-unavailable")
        if result.returncode:
            raise Refused("scan-refused")
        with directory(path.parent) as parent:
            value = read_at(parent, path.name, TOTAL_BYTES).decode("utf-8")
        # Windows paths are not covered by the shared POSIX home-prefix rule.
        if re.search(r"[A-Za-z]:[\\/]|file://", value):
            raise Refused("private-path")
        return value


def coreutils_version() -> str:
    """Probe only stat's version, with a time and output bound, never its path."""
    try:
        child = subprocess.Popen(["stat", "--version"], stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
    except OSError:
        return "not recorded"
    data = b""
    try:
        with selectors.DefaultSelector() as ready:
            ready.register(child.stdout, selectors.EVENT_READ)
            deadline = time.monotonic() + 2
            while len(data) < 1024 and time.monotonic() < deadline:
                if not ready.select(max(0, deadline - time.monotonic())):
                    break
                block = os.read(child.stdout.fileno(), 1024 - len(data))
                if not block:
                    break
                data += block
                if b"\n" in data:
                    break
    finally:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)
        child.stdout.close()
    match = re.match(rb"stat \(GNU coreutils\) ([0-9][0-9.]*)\s", data)
    return match.group(1).decode("ascii") if match else "not recorded"


def scan_log_escapes(text: str) -> None:
    """Screen nested JSON spellings without changing the retained log text."""
    views, size = [], 0
    for _ in range(8):
        decoded = JSON_ESCAPE.sub(lambda match: json.loads('"' + match[0] + '"'), text)
        if decoded == text:
            break
        size += len(decoded.encode("utf-8")) + 1
        if size > TOTAL_BYTES:
            raise Refused("scan-refused")
        views.append(decoded)
        text = decoded
    else:
        raise Refused("scan-refused")
    if views:
        scrub("\n".join(views))


def failure_selectors(text: str) -> list[str]:
    result = []
    for line in text.splitlines():
        match = (re.match(r"^(?:FAIL|ERROR): test_\w+ \(([A-Za-z0-9_.]+)\)", line) or
                 re.match(r"^check: ([A-Za-z0-9_./:-]+) (?:FAILED|INTERRUPTED)\b", line) or
                 re.match(r"^(?:FAILED|ERROR) ([A-Za-z0-9_./:-]+::[A-Za-z0-9_./:-]+)(?: |$)", line))
        if match and NAME.fullmatch(match.group(1)) and match.group(1) not in result:
            result.append(match.group(1))
    return result


def inventory(parent: int):
    with os.scandir(parent) as entries:
        names = []
        for entry in entries:
            names.append(entry.name)
            if len(names) > MAX_RECORDS + 1:
                raise Refused("file-count-limit")
    if any(not RECORD.fullmatch(name) and name != "omissions.json" for name in names):
        raise Refused("unexpected-entry")
    records, size = [], 0
    for name in sorted(names):
        data = read_at(parent, name, min(FILE_BYTES, TOTAL_BYTES - size))
        size += len(data)
        records.append((name, data))
    return records


def validate_record(row, omission=False):
    """Accept only writer-shaped records, then scan decoded string values."""
    def require(condition):
        if not condition:
            raise Refused("invalid-record-shape")

    def integer(value, minimum=0):
        return type(value) is int and value >= minimum

    def matches(value, pattern):
        return isinstance(value, str) and re.fullmatch(pattern, value) is not None

    common = {"schema", "diagnostic_only", "verification_evidence"}
    expected = (common | {"omitted_stages", "reason"} if omission else common |
                {"scope", "exit_code", "elapsed_s", "failure_selectors", "omitted_selectors",
                 "selector_basis", "runtime_basis", "runtime", "log"})
    require(isinstance(row, dict) and set(row) == expected)
    require(type(row["schema"]) is int and row["schema"] == 1 and
            row["diagnostic_only"] is True and row["verification_evidence"] is False)
    if omission:
        require(integer(row["omitted_stages"], 1) and row["omitted_stages"] <= 1_000_000)
        require(row["reason"] == OMISSION_REASON)
    else:
        scope, runtime, log = row["scope"], row["runtime"], row["log"]
        require(isinstance(scope, dict) and "stage" in scope and set(scope) <= {"stage", "mode", "lane", "shard"})
        require(all(matches(value, NAME) for value in scope.values()))
        require(integer(row["exit_code"], 1) and integer(row["elapsed_s"]))
        require(row["selector_basis"] == SELECTOR_BASIS and row["runtime_basis"] == RUNTIME_BASIS)
        required_runtime = {"python", "python_implementation", "bash", "coreutils_stat", "os"}
        require(isinstance(runtime, dict) and required_runtime <= set(runtime) <= required_runtime | IMAGE_FIELDS)
        require(matches(runtime["python"], r"[0-9][A-Za-z0-9.+-]{0,79}"))
        require(all(matches(runtime[key], r"[A-Za-z][A-Za-z0-9_.-]{0,79}")
                    for key in ("python_implementation", "os")))
        require(runtime["bash"] == "not recorded" or matches(runtime["bash"], r"[0-9][A-Za-z0-9.()+-]{0,79}"))
        require(runtime["coreutils_stat"] == "not recorded" or matches(runtime["coreutils_stat"], r"[0-9][0-9.]*"))
        require(all(matches(runtime[key], r"[A-Za-z0-9_.-]{1,100}") for key in set(runtime) & IMAGE_FIELDS))
        require(isinstance(log, dict))
        if log.get("status") == "retained":
            require(set(log) == {"status", "text", "source_bytes", "retained_bytes", "transformed"})
            require(isinstance(log["text"], str) and type(log["transformed"]) is bool)
            require(integer(log["source_bytes"]) and log["source_bytes"] <= LOG_BYTES)
            require(integer(log["retained_bytes"]) and log["retained_bytes"] <= FILE_BYTES and
                    log["retained_bytes"] == len(log["text"].encode("utf-8")))
            require(log["transformed"] or log["source_bytes"] == log["retained_bytes"])
            scan_log_escapes(log["text"])
            observed = failure_selectors(log["text"])
        else:
            require(log.get("status") == "omitted" and {"status", "reason"} <= set(log) <=
                    {"status", "reason", "source_bytes"})
            require(log["reason"] in {"unreadable-source", "unsafe-file", "size-limit", "changed-file",
                                      "scan-unavailable", "scan-refused", "private-path", "encoded-file-limit"})
            require("source_bytes" not in log or integer(log["source_bytes"]))
            observed = []
        require(isinstance(row["failure_selectors"], list) and row["failure_selectors"] == observed[:40])
        require(integer(row["omitted_selectors"]) and row["omitted_selectors"] == max(0, len(observed) - 40))

    def strings(value):
        if isinstance(value, str):
            yield value
        elif isinstance(value, dict):
            for item in value.values():
                yield from strings(item)
        elif isinstance(value, list):
            for item in value:
                yield from strings(item)

    # JSON escaping can hide assignment syntax from the outbound regex. Check
    # the decoded values together before also checking the serialized output.
    scrub("\n".join(strings(row)))


def note_omission(parent: int, records):
    old = next((data for name, data in records if name == "omissions.json"), b"{}")
    previous = json.loads(old)
    if not isinstance(previous, dict):
        raise Refused("invalid-omission-count")
    count = previous.get("omitted_stages", 0)
    if type(count) is not int or not 0 <= count < 1_000_000:
        raise Refused("invalid-omission-count")
    data = encoded({"schema": 1, "diagnostic_only": True, "verification_evidence": False, "omitted_stages": count + 1,
                    "reason": OMISSION_REASON})
    name = "failure-%s.json" % uuid.uuid4().hex
    write_at(parent, name, data)
    os.replace(name, "omissions.json", src_dir_fd=parent, dst_dir_fd=parent)


def retain(args) -> None:
    if not NAME.fullmatch(args.stage) or args.exit_code <= 0 or args.elapsed < 0:
        raise Refused("invalid-stage")
    log = {"status": "omitted", "reason": "unreadable-source"}
    try:
        with directory(args.log.parent) as parent:
            info = os.stat(args.log.name, dir_fd=parent, follow_symlinks=False)
            if stat.S_ISREG(info.st_mode) and info.st_nlink == 1:
                log["source_bytes"] = info.st_size
            raw = read_at(parent, args.log.name, LOG_BYTES).decode("utf-8")
        # Strip terminal controls without output_digest's 300-character line cap.
        clean = CONTROL.sub("", ANSI.sub("", raw))
        clean = scrub(clean, redact=True)
        scan_log_escapes(clean)
        log.update(status="retained", text=clean, retained_bytes=len(clean.encode("utf-8")),
                   transformed=clean != raw)
        log.pop("reason")
    except (OSError, UnicodeError, Refused) as error:
        log["reason"] = str(error) if isinstance(error, Refused) else "unreadable-source"
    runtime = {"python": platform.python_version(), "python_implementation": platform.python_implementation(),
               "bash": args.bash_version if re.fullmatch(r"[0-9][A-Za-z0-9.()+-]{0,79}", args.bash_version) else "not recorded",
               "coreutils_stat": coreutils_version(), "os": platform.system()}
    for name in sorted(IMAGE_FIELDS):
        value = os.environ.get(name, "")
        if re.fullmatch(r"[A-Za-z0-9_.-]{1,100}", value):
            runtime[name] = value
    scope = {"stage": args.stage}
    for name in ("mode", "lane", "shard"):
        value = getattr(args, name)
        if value and NAME.fullmatch(value):
            scope[name] = value
    observed = failure_selectors(log.get("text", ""))
    row = {"schema": 1, "diagnostic_only": True, "verification_evidence": False,
           "scope": scope, "exit_code": args.exit_code, "elapsed_s": args.elapsed,
           "failure_selectors": observed[:40], "omitted_selectors": max(0, len(observed) - 40),
           "selector_basis": SELECTOR_BASIS, "runtime_basis": RUNTIME_BASIS,
           "runtime": runtime, "log": log}
    data = encoded(row)
    if len(data) > FILE_BYTES:
        row["log"] = {"status": "omitted", "reason": "encoded-file-limit"}
        row["failure_selectors"], row["omitted_selectors"] = [], 0
        data = encoded(row)
    validate_record(row)
    scrub(data.decode("utf-8"))
    with directory(args.directory, create=True) as parent:
        fcntl.flock(parent, fcntl.LOCK_EX)
        records = inventory(parent)
        if (sum(name != "omissions.json" for name, _ in records) >= MAX_RECORDS or
                sum(len(body) for _, body in records) + len(data) > TOTAL_BYTES - RESERVE_BYTES):
            note_omission(parent, records)
            print("check: failure evidence omitted at aggregate limit (recorded)")
            return
        write_at(parent, "failure-%s.json" % uuid.uuid4().hex, data)
    print("check: failure evidence retained (diagnostic only; log %s)" % row["log"]["status"])


def publish(args) -> None:
    # Upload one newly created file, never the directory or an unchecked glob.
    with directory(args.directory) as parent:
        fcntl.flock(parent, fcntl.LOCK_EX)
        records = inventory(parent)
    if not records:
        raise Refused("no-evidence")
    rows = []
    for name, data in records:
        scrub(data.decode("utf-8"))
        row = json.loads(data)
        validate_record(row, omission=name == "omissions.json")
        rows.append(row)
    data = encoded({"schema": 1, "diagnostic_only": True, "verification_evidence": False,
                    "limits": {"source_log_bytes": LOG_BYTES, "record_bytes": FILE_BYTES,
                               "artifact_bytes": TOTAL_BYTES, "records": MAX_RECORDS}, "failures": rows})
    if len(data) > TOTAL_BYTES:
        raise Refused("artifact-size-limit")
    scrub(data.decode("utf-8"))
    with directory(args.output.parent) as parent:
        write_at(parent, args.output.name, data)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    capture = commands.add_parser("retain")
    capture.add_argument("--directory", type=Path, required=True)
    capture.add_argument("--log", type=Path, required=True)
    capture.add_argument("--stage", required=True)
    capture.add_argument("--exit-code", type=int, required=True)
    capture.add_argument("--elapsed", type=int, required=True)
    capture.add_argument("--bash-version", default="")
    for name in ("mode", "lane", "shard"):
        capture.add_argument("--" + name, default="")
    publication = commands.add_parser("publish")
    publication.add_argument("directory", type=Path)
    publication.add_argument("output", type=Path)
    args = parser.parse_args(argv)
    try:
        (retain if args.action == "retain" else publish)(args)
    except (OSError, ValueError, UnicodeError, RecursionError, Refused, subprocess.SubprocessError):
        print("check: failure evidence unavailable (missing, unsafe, oversized or unscreened input)", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
