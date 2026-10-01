#!/usr/bin/env python3
"""Per-doctor external probe memo. The caller owns OMS_DOCTOR_PROBE_DIR."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time


def bounded_command(seconds, command):
    if not seconds:
        return command
    for name in ("timeout", "gtimeout"):
        binary = shutil.which(name)
        if binary and subprocess.run([binary, "--version"], stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL).returncode == 0:
            return [binary, "--kill-after=1", str(seconds)] + command
    helper = Path(__file__).with_name("run-bounded.py")
    if shutil.which("python3") and helper.is_file():
        return [sys.executable, str(helper), "%ss" % seconds, "1s", "provider-probe"] + command
    return None


def identity(command):
    binary = (command[0] if "/" in command[0] or "\\" in command[0]
              else shutil.which(command[0]))
    if binary is None:
        # Preserve the normal command-not-found result; do not memoize it.
        return None
    return [os.path.realpath(os.path.abspath(binary))] + command[1:]


def emit(path, stream):
    with open(path, "rb") as handle:
        shutil.copyfileobj(handle, stream.buffer)


def execute(command, entry):
    """Run once, keeping each stream and the order its chunks arrived in, so a
    2>&1 caller can be replayed without running the probe a second time."""
    start = time.monotonic()
    chunks = []
    lock = threading.Lock()
    try:
        proc = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError as exc:
        chunks.append((1, ("error: %s\n" % exc).encode("utf-8", "replace")))
        result = 127 if isinstance(exc, FileNotFoundError) else 126
    else:
        def pump(pipe, index):
            # Pipe threads, not select: native Windows Python selects sockets only.
            for block in iter(lambda: os.read(pipe.fileno(), 65536), b""):
                with lock:
                    chunks.append((index, block))
        threads = [threading.Thread(target=pump, args=(proc.stdout, 0)),
                   threading.Thread(target=pump, args=(proc.stderr, 1))]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        proc.stdout.close()
        proc.stderr.close()
        result = proc.wait()
    for index, name in enumerate(("stdout", "stderr")):
        with open(entry / name, "wb") as handle:
            handle.write(b"".join(block for i, block in chunks if i == index))
    with open(entry / "order.json", "w", encoding="ascii") as handle:
        json.dump([[index, len(block)] for index, block in chunks], handle)
    return (128 - result if result < 0 else result), time.monotonic() - start


def replay(entry, mode):
    if mode == "split":
        emit(entry / "stdout", sys.stdout)
        emit(entry / "stderr", sys.stderr)
        return
    with open(entry / "order.json", encoding="ascii") as handle:
        order = json.load(handle)
    streams = [open(entry / "stdout", "rb"), open(entry / "stderr", "rb")]
    try:
        for index, size in order:
            sys.stdout.buffer.write(streams[index].read(size))
    finally:
        for stream in streams:
            stream.close()


def direct(command, merged):
    try:
        code = subprocess.run(command, stdin=subprocess.DEVNULL,
                              stderr=subprocess.STDOUT if merged else None).returncode
        return 128 - code if code < 0 else code
    except FileNotFoundError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 127
    except OSError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 126


def main():
    if len(sys.argv) < 4 or sys.argv[2] not in ("split", "merged"):
        print("usage: doctor-probe-memo.py SECONDS split|merged COMMAND...", file=sys.stderr)
        return 2
    try:
        seconds = int(sys.argv[1])
        if seconds < 0:
            raise ValueError
    except ValueError:
        print("error: probe bound must be a nonnegative integer", file=sys.stderr)
        return 2
    mode, command = sys.argv[2], sys.argv[3:]
    cache = os.environ.get("OMS_DOCTOR_PROBE_DIR", "")
    runner = bounded_command(seconds, command)
    if runner is None:
        print("error: bounded provider probe helper is unavailable", file=sys.stderr)
        return 127
    if not cache or not os.path.isdir(cache):
        return direct(runner, mode == "merged")
    key_parts = identity(command)
    if key_parts is None:
        return direct(runner, mode == "merged")
    key = hashlib.sha256(json.dumps(key_parts, ensure_ascii=True).encode("utf-8")).hexdigest()
    entry = Path(cache) / key
    try:
        with open(entry / "result.json", encoding="ascii") as handle:
            result = json.load(handle)
        reusable = (seconds == 0 or result["elapsed"] <= seconds) and result["code"] not in (124, 137)
        if reusable and all((entry / name).is_file() for name in ("stdout", "stderr", "order.json")):
            replay(entry, mode)
            return result["code"]
    except (OSError, KeyError, ValueError, TypeError):
        pass
    stage = Path(tempfile.mkdtemp(prefix=".probe-", dir=cache))
    try:
        code, elapsed = execute(runner, stage)
        if code not in (124, 137):
            with open(stage / "result.json", "w", encoding="ascii") as handle:
                json.dump({"code": code, "elapsed": elapsed}, handle)
            try:
                # A fresh run may finish inside a tighter bound than the old one.
                if entry.is_dir():
                    shutil.rmtree(entry)
                os.rename(stage, entry)
                source = entry
            except OSError:
                source = stage
        else:
            source = stage
        replay(source, mode)
        return code
    finally:
        shutil.rmtree(stage, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
