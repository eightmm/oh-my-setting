#!/usr/bin/env python3
"""Per-doctor external probe memo. The caller owns OMS_DOCTOR_PROBE_DIR."""

import contextlib
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def bounded_command(seconds, command, cache=None):
    if not seconds:
        return command
    for name in ("timeout", "gtimeout"):
        binary = shutil.which(name)
        if not binary:
            continue
        marker = None
        if cache:
            try:
                info = os.stat(binary)
                signature = [os.path.realpath(binary), info.st_dev, info.st_ino,
                             info.st_size, info.st_mtime_ns, info.st_ctime_ns]
                digest = hashlib.sha256(json.dumps(signature).encode("ascii")).hexdigest()
                marker = Path(cache) / (".timeout-" + digest)
            except OSError:
                pass
        valid = marker is not None and marker.is_file()
        if not valid:
            valid = subprocess.run([binary, "--version"], stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL).returncode == 0
            if valid and marker is not None:
                with contextlib.suppress(OSError):
                    marker.touch()
        if valid:
            return [binary, "--kill-after=1", str(seconds)] + command
    helper = Path(__file__).with_name("run-bounded.py")
    if shutil.which("python3") and helper.is_file():
        return [sys.executable, str(helper), "%ss" % seconds, "1s", "provider-probe"] + command
    return None


@contextlib.contextmanager
def probe_lock(cache, key, seconds):
    """Hold through replay so a tighter-bound replacement cannot remove it.

    The doctor's cache is disabled on Windows. Unsupported locks or a busy
    timed-out probe fall back to direct execution, without queuing N bounds.
    """
    try:
        import fcntl
        handle = open(Path(cache) / (key + ".lock"), "a+b")
    except (ImportError, OSError):
        yield False
        return
    acquired = False
    try:
        deadline = time.monotonic() + seconds
        while True:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                acquired = True
                break
            except BlockingIOError:
                if seconds and time.monotonic() >= deadline:
                    break
                time.sleep(0.02)
            except OSError:
                break
        yield acquired
    finally:
        if acquired:
            fcntl.flock(handle, fcntl.LOCK_UN)
        handle.close()


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


def execute(command, entry, merged):
    """Capture into files, never pipes: a descendant that keeps a pipe open
    would outlive the probe's bound. A merged capture shares one file."""
    start = time.monotonic()
    raw = Path(tempfile.mkdtemp(prefix=".raw-", dir=entry.parent))
    with open(raw / "stdout", "wb") as stdout, open(raw / "stderr", "wb") as stderr:
        try:
            result = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=stdout,
                                    stderr=stdout if merged else stderr).returncode
        except OSError as exc:
            (stdout if merged else stderr).write(("error: %s\n" % exc).encode("utf-8", "replace"))
            result = 127 if isinstance(exc, FileNotFoundError) else 126
    # A descendant may still hold the raw files: replay a snapshot it cannot
    # write to, so later hits never see bytes the probe did not produce. The
    # raw files live outside the entry because Windows refuses to delete a
    # file in use; the doctor's cache cleanup takes what is left.
    for name in ("stdout", "stderr"):
        shutil.copyfile(raw / name, entry / name)
    shutil.rmtree(raw, ignore_errors=True)
    return (128 - result if result < 0 else result), time.monotonic() - start


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


def cached(entry, seconds):
    try:
        with open(entry / "result.json", encoding="ascii") as handle:
            result = json.load(handle)
        if ((seconds == 0 or result["elapsed"] <= seconds) and result["code"] not in (124, 137)
                and (entry / "stdout").is_file() and (entry / "stderr").is_file()):
            return result["code"]
    except (OSError, KeyError, ValueError, TypeError):
        pass
    return None


def capture(runner, cache, entry, merged):
    """Run once; keep the result unless it timed out. Returns (code, dir, stage)."""
    stage = Path(tempfile.mkdtemp(prefix=".probe-", dir=cache))
    code, elapsed = execute(runner, stage, merged)
    if code in (124, 137):
        return code, stage, stage
    with open(stage / "result.json", "w", encoding="ascii") as handle:
        json.dump({"code": code, "elapsed": elapsed}, handle)
    try:
        # A fresh run may finish inside a tighter bound than the old one.
        if entry.is_dir():
            shutil.rmtree(entry)
        os.rename(stage, entry)
        return code, entry, None
    except OSError:
        return code, stage, stage


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
    key_parts = identity(command) if cache and os.path.isdir(cache) else None
    if key_parts is None:
        runner = bounded_command(seconds, command)
        if runner is None:
            print("error: bounded provider probe helper is unavailable", file=sys.stderr)
            return 127
        return direct(runner, mode == "merged")
    key = hashlib.sha256(json.dumps(key_parts, ensure_ascii=True).encode("utf-8")).hexdigest()
    with probe_lock(cache, key, seconds) as locked:
        runner = bounded_command(seconds, command, cache if locked else None)
        if runner is None:
            print("error: bounded provider probe helper is unavailable", file=sys.stderr)
            return 127
        if not locked:
            return direct(runner, mode == "merged")
        return replay(runner, cache, key, mode, seconds)


def replay(runner, cache, key, mode, seconds):
    stages = []
    try:
        # Every caller shares one split capture. A 2>&1 caller replays it
        # exactly while one stream is empty; only when both carry bytes is the
        # probe run once more into a single file, the only way to keep the
        # order of interleaved writes, and that capture is kept too.
        entry = Path(cache) / key
        code = cached(entry, seconds)
        if code is None:
            code, entry, stage = capture(runner, cache, entry, False)
            stages.append(stage)
        if mode == "split":
            emit(entry / "stdout", sys.stdout)
            emit(entry / "stderr", sys.stderr)
            return code
        if not (entry / "stdout").stat().st_size or not (entry / "stderr").stat().st_size:
            emit(entry / "stdout", sys.stdout)
            emit(entry / "stderr", sys.stdout)
            return code
        entry = Path(cache) / (key + "-merged")
        code = cached(entry, seconds)
        if code is None:
            code, entry, stage = capture(runner, cache, entry, True)
            stages.append(stage)
        emit(entry / "stdout", sys.stdout)
        return code
    finally:
        for stage in stages:
            if stage is not None:
                shutil.rmtree(stage, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
