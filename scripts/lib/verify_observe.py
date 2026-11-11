"""Advisory observations about one verify run: did a test runner report zero
tests, and under which environment did the command run.

Nothing here decides anything. A verify receipt keeps its exit code as the only
verdict; these fields ride beside it so a reader can see an exit 0 that ran no
tests, or a pass recorded on a different interpreter than CI uses. Output a
recogniser does not understand yields no claim, never a guess, and a recognised
non-zero run adds nothing: a clean result is no stronger than the receipt was.
Stdlib only, no model, no network, bounded reads.

    verify_observe.py lines FILE [--repo DIR]   note lines for a task verification
    verify_observe.py scan FILE                 JSON: runners seen, runners with zero tests
    verify_observe.py env [--repo DIR] [--runner NAME ...]   JSON environment fingerprint
"""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys

READ_CAP = 16 * 1024 * 1024
LOCK_FILE_CAP = 8 * 1024 * 1024
LOCK_FILES_MAX = 64
PROBE_SECONDS = 5

ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")

# Runner family -> extra tools worth a version beside the always-recorded base.
RUNNER_TOOLS = {
    "pytest": ("pytest",),
    "unittest": (),
    "go": ("go",),
    "cargo": ("cargo", "rustc"),
    "jest": ("node",),
    "vitest": ("node",),
    "oms-smoke": (),
}
BASE_TOOLS = ("python3", "bash", "git")

LOCK_NAMES = (
    "requirements.txt", "constraints.txt", "uv.lock", "poetry.lock", "Pipfile.lock", "pdm.lock",
    "package-lock.json", "npm-shrinkwrap.json", "yarn.lock", "pnpm-lock.yaml", "bun.lock",
    "Cargo.lock", "go.mod", "go.sum", "Gemfile.lock", "composer.lock", "conda-lock.yml",
)

PYTEST_SUMMARY = re.compile(r"^=*\s*((?:\d+ [A-Za-z]+(?:, )?)+)\s+in [\d.]+s\b")
COUNT = re.compile(r"(\d+) ([A-Za-z]+)")
GO_ZERO = re.compile(r"^(?:\?\s+\S+\s+\[no test files\]|ok\s+\S+\s.*\[no tests to run\])")
GO_RAN = re.compile(r"^ok\s+\S+\s+(?:\(cached\)|[\d.]+s)(?:\s+\(cached\))?(?:\s+coverage:.*)?$")
CARGO = re.compile(r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;")
UNITTEST_RAN = re.compile(r"^Ran (\d+) tests? in [\d.]+s")
UNITTEST_SKIP = re.compile(r"^OK \(.*\bskipped=(\d+)")
SMOKE_OK = re.compile(r"^[a-z][a-z0-9-]*: ok \((?:shard \d+/\d+, )?(\d+) tests?\b")
SMOKE_SKIP = re.compile(r"^[a-z][a-z0-9-]*: skipped \(shard \d+/\d+, no selected tests\)")


def _counts(text):
    found = {}
    for number, word in COUNT.findall(text):
        found[word.lower()] = found.get(word.lower(), 0) + int(number)
    return found


def scan_text(text):
    """Return {runner: tests_that_ran} for every runner whose summary was recognised."""
    ran = {}

    def add(name, value):
        ran[name] = ran.get(name, 0) + value

    last_unittest = None
    for raw in text.split("\n"):
        line = ANSI.sub("", raw).rstrip()
        if not line:
            continue
        if re.match(r"^=*\s*no tests ran\b", line):
            add("pytest", 0)
            continue
        match = PYTEST_SUMMARY.match(line)
        if match:
            found = _counts(match.group(1))
            add("pytest", sum(found.get(key, 0) for key in
                              ("passed", "failed", "xfailed", "xpassed", "error", "errors")))
            continue
        match = UNITTEST_RAN.match(line)
        if match:
            add("unittest", int(match.group(1)))
            last_unittest = int(match.group(1))
            continue
        match = UNITTEST_SKIP.match(line)
        if match and last_unittest is not None:
            add("unittest", -min(int(match.group(1)), last_unittest))
            last_unittest = None
            continue
        if GO_ZERO.match(line):
            add("go", 0)
            continue
        if GO_RAN.match(line):
            add("go", 1)
            continue
        match = CARGO.match(line)
        if match:
            add("cargo", int(match.group(1)) + int(match.group(2)))
            continue
        match = re.match(r"^\s*Tests:\s+(.+)$", line)
        if match:
            found = _counts(match.group(1))
            if "total" in found:
                add("jest", found.get("passed", 0) + found.get("failed", 0))
            continue
        match = re.match(r"^\s*Tests\s+(?!:)(.+)$", line)
        if match:
            body = match.group(1)
            if re.match(r"no tests\b", body):
                add("vitest", 0)
            else:
                found = _counts(body)
                if found:
                    add("vitest", found.get("passed", 0) + found.get("failed", 0))
            continue
        if re.match(r"^No tests found, exiting with code 0\b", line):
            add("jest", 0)
            continue
        if re.match(r"^No test files found, exiting with code 0\b", line):
            add("vitest", 0)
            continue
        match = SMOKE_OK.match(line)
        if match:
            add("oms-smoke", int(match.group(1)))
            continue
        if SMOKE_SKIP.match(line):
            add("oms-smoke", 0)
    return ran


def verify_output(text):
    """The verify run's own output inside a known receipt artifact, else None.

    A peer-review gate artifact holds the whole run between "## Output" and the
    "## Exit" or "## Timeout" heading; a patch-admit report holds the tail under
    "## Verify output (tail)". Any other text is not claimed to be a verify run.
    """
    if text.startswith("# gate verify"):
        start = text.find("\n## Output\n")
        if start < 0:
            return None
        body = text[start + len("\n## Output\n"):]
        cut = [index for index in (body.find("\n\n## Exit\n"), body.find("\n\n## Timeout\n")) if index >= 0]
        return body[:min(cut)] if cut else body
    marker = "\n## Verify output (tail)\n"
    start = text.find(marker)
    if start >= 0:
        body = text[start + len(marker):]
        end = body.find("\n```\n", body.find("```") + 3)
        return body[:end] if end >= 0 else body
    return None


def read_text(path):
    size = os.path.getsize(path)
    with open(path, "rb") as handle:
        if size > READ_CAP:
            handle.seek(size - READ_CAP)
            handle.readline()
        return handle.read().decode("utf-8", "replace")


def scan(text):
    ran = scan_text(text)
    return {"runners": sorted(ran), "zero": sorted(name for name, value in ran.items() if value == 0)}


def _version(name):
    exe = shutil.which(name)
    if not exe:
        return None
    try:
        done = subprocess.run([exe, "--version"], capture_output=True, text=True, timeout=PROBE_SECONDS,
                              stdin=subprocess.DEVNULL, cwd="/")
    except Exception:
        return None
    first = ((done.stdout or "") + (done.stderr or "")).strip().split("\n")[0:1]
    match = re.search(r"\d+(?:\.\d+)+", first[0]) if first else None
    return match.group(0) if match else None


def _lock_digest(repo):
    names = []
    if repo and os.path.isdir(repo):
        candidates = list(LOCK_NAMES)
        for folder in ("", "requirements"):
            try:
                entries = sorted(os.listdir(os.path.join(repo, folder) if folder else repo))
            except OSError:
                continue
            for entry in entries:
                if re.fullmatch(r"requirements[A-Za-z0-9._-]*\.txt", entry) or (
                        folder and re.fullmatch(r"[A-Za-z0-9._-]+\.txt", entry)):
                    candidates.append("%s/%s" % (folder, entry) if folder else entry)
        for name in sorted(set(candidates)):
            try:
                info = os.lstat(os.path.join(repo, name))
            except OSError:
                continue
            if stat.S_ISREG(info.st_mode) and info.st_size <= LOCK_FILE_CAP:
                names.append(name)
    names = names[:LOCK_FILES_MAX]
    outer = hashlib.sha256()
    kept = []
    for name in names:
        inner = hashlib.sha256()
        try:
            with open(os.path.join(repo, name), "rb") as handle:
                for block in iter(lambda: handle.read(1 << 20), b""):
                    inner.update(block)
        except OSError:
            continue
        outer.update(("%s\0%s\n" % (name, inner.hexdigest())).encode("utf-8"))
        kept.append(name)
    return kept, outer.hexdigest() if kept else ""


def fingerprint(repo="", runners=()):
    """Names and versions only: no paths, no user, no environment variables."""
    wanted = list(BASE_TOOLS)
    for runner in runners:
        for tool in RUNNER_TOOLS.get(runner, ()):
            if tool not in wanted:
                wanted.append(tool)
    tools = {}
    for name in wanted:
        version = _version(name)
        if version:
            tools[name] = version
    locks, digest = _lock_digest(repo)
    result = {"os": "%s/%s" % (platform.system() or "unknown", platform.machine() or "unknown"), "tools": tools}
    if locks:
        result["locks"] = locks
        result["lock_sha256"] = digest
    return result


def note_lines(text, repo=""):
    """Advisory note lines for one verify run: always an env line, a warning when a runner ran zero tests."""
    found = scan(text)
    env = fingerprint(repo, found["runners"])
    parts = ["%s=%s" % (name, version) for name, version in env["tools"].items()]
    parts.append("os=%s" % env["os"])
    if env.get("locks"):
        parts.append("locks=%d" % len(env["locks"]))
        parts.append("lock_sha256=%s" % env["lock_sha256"][:12])
    lines = ["env: " + " ".join(parts)]
    if found["zero"]:
        lines.append("tests: %s reported no tests run" % ", ".join(found["zero"]))
    return lines, found["zero"]


def warning(zero):
    return ("warning: exit 0 but %s reported no tests run (advisory; the verify status is unchanged)"
            % ", ".join(zero))


def main(argv):
    parser = argparse.ArgumentParser(prog="verify_observe.py", description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    for name in ("lines", "scan"):
        item = sub.add_parser(name)
        item.add_argument("file")
        item.add_argument("--repo", default="")
    item = sub.add_parser("env")
    item.add_argument("--repo", default="")
    item.add_argument("--runner", action="append", default=[])
    args = parser.parse_args(argv)
    if args.cmd == "env":
        print(json.dumps(fingerprint(args.repo, args.runner), sort_keys=True))
        return 0
    try:
        text = read_text(args.file)
    except OSError as error:
        print("verify_observe: cannot read %s: %s" % (args.file, error.strerror or error), file=sys.stderr)
        return 2
    if args.cmd == "scan":
        print(json.dumps(scan(text), sort_keys=True))
        return 0
    lines, zero = note_lines(text, args.repo)
    print("\n".join(lines))
    if zero:
        print(warning(zero), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
