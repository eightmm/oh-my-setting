#!/usr/bin/env python3
"""Read-only report of whether this machine could sandbox, and whether a provider's sandbox setting can take effect.

Usage: doctor-sandbox.py [--json] [--probe]

OMS sandboxes nothing itself. This report states facts about the host: the
sandbox helper on PATH, the kernel switches that let an unprivileged process
make a namespace, Landlock, seccomp and (macOS) Seatbelt. Every item is one of
available | unavailable | restricted | unknown, with the file or command it was
read from. Nothing is changed, installed or contacted.

It then reuses the `--security` settings reader (doctor-security.py) and prints
a `warn:` for each provider whose sandbox setting is on while the mechanism it
relies on is not usable here. The provider-to-mechanism mapping is a statement
of public documentation, not something this report observed.

--probe is the only thing that starts a process: on Linux, when `bwrap` exists,
it runs `bwrap --ro-bind / / --unshare-all --die-with-parent true` once with a
short timeout. It never runs by default.

Test-only environment: OMS_SANDBOX_ROOT prefixes the /proc and /sys paths and
OMS_SANDBOX_OS replaces the detected operating system name.
"""
import importlib.util
import json
import os
import platform
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
STATES = ("available", "unavailable", "restricted", "unknown")
PROBE_ARGS = ["--ro-bind", "/", "/", "--unshare-all", "--die-with-parent", "true"]
PROBE_SECONDS = 5


def clean(value, limit=120):
    text = "".join(ch if ch.isprintable() else "?" for ch in str(value))
    return text if len(text) <= limit else text[:limit - 1] + "…"


def host_path(path):
    return os.environ.get("OMS_SANDBOX_ROOT", "") + path


def read_text(path, limit=65536):
    """(text, None) or (None, why) for the host file PATH."""
    real = host_path(path)
    try:
        with open(real, encoding="utf-8", errors="replace") as handle:
            return handle.read(limit), None
    except FileNotFoundError:
        return None, "absent"
    except (OSError, ValueError):
        return None, "unreadable"


def item(name, state, detail, evidence):
    return {"item": name, "state": state, "detail": clean(detail), "evidence": clean(evidence, 200)}


def integer_switch(name, path, restricted_when):
    """A kernel switch whose absence means the kernel has no such restriction."""
    text, why = read_text(path)
    if why == "absent":
        return item(name, "available", "switch not present on this kernel; no such restriction", path)
    if why:
        return item(name, "unknown", "could not be read", path)
    try:
        number = int(text.strip())
    except ValueError:
        return item(name, "unknown", "not an integer: %s" % clean(text.strip(), 20), path)
    state = "restricted" if restricted_when(number) else "available"
    return item(name, state, "value is %d" % number, path)


def linux_items(bwrap_path):
    items = []
    if bwrap_path:
        items.append(item("bwrap", "available", "found at %s (not executed)" % bwrap_path, "PATH"))
    else:
        items.append(item("bwrap", "unavailable", "not on PATH", "PATH"))
    items.append(integer_switch("userns_clone", "/proc/sys/kernel/unprivileged_userns_clone",
                                lambda value: value == 0))
    text, why = read_text("/proc/sys/user/max_user_namespaces")
    path = "/proc/sys/user/max_user_namespaces"
    if why:
        items.append(item("max_user_namespaces", "unknown", "file %s" % why, path))
    else:
        try:
            number = int(text.strip())
            items.append(item("max_user_namespaces", "unavailable" if number == 0 else "available",
                              "value is %d" % number, path))
        except ValueError:
            items.append(item("max_user_namespaces", "unknown", "not an integer", path))
    items.append(integer_switch("apparmor_restrict_userns",
                                "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
                                lambda value: value != 0))
    release, _ = read_text("/proc/sys/kernel/osrelease")
    release = release.strip() if release else "unknown"
    lsm, why = read_text("/sys/kernel/security/lsm")
    path = "/sys/kernel/security/lsm"
    if why:
        items.append(item("landlock", "unknown", "file %s; kernel %s; ABI unknown" % (why, clean(release, 40)), path))
    else:
        listed = "landlock" in [part.strip() for part in lsm.strip().split(",")]
        items.append(item("landlock", "available" if listed else "unavailable",
                          "%s in the LSM list; kernel %s; ABI unknown (needs a syscall to read)"
                          % ("listed" if listed else "not listed", clean(release, 40)), path))
    status, why = read_text("/proc/self/status")
    path = "/proc/self/status"
    if why:
        items.append(item("seccomp", "unknown", "file %s" % why, path))
    else:
        line = next((row for row in status.splitlines() if row.startswith("Seccomp:")), None)
        if line is None:
            items.append(item("seccomp", "unavailable", "no Seccomp line", path))
        else:
            items.append(item("seccomp", "available", "%s (this process)" % clean(" ".join(line.split()), 30), path))
    return items


def probe(bwrap_path):
    """The one optional live check; every outcome is a state, never an exception."""
    command = [bwrap_path] + PROBE_ARGS
    evidence = "ran " + " ".join(["bwrap"] + PROBE_ARGS)
    try:
        done = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                              stderr=subprocess.PIPE, timeout=PROBE_SECONDS, check=False)
    except subprocess.TimeoutExpired:
        return item("probe", "unknown", "did not finish in %d s" % PROBE_SECONDS, evidence)
    except OSError as error:
        return item("probe", "unknown", "could not start: %s" % clean(error, 60), evidence)
    if done.returncode == 0:
        return item("probe", "available", "works", evidence)
    lines = done.stderr.decode("utf-8", "replace").strip().splitlines()
    first = lines[0] if lines else "exit status %d, no stderr" % done.returncode
    return item("probe", "unavailable", "fails: %s" % clean(first), evidence)


def collect(want_probe):
    system = os.environ.get("OMS_SANDBOX_OS") or platform.system()
    items = [item("os", "available", system, "platform")]
    if system == "Linux":
        bwrap_path = shutil.which("bwrap")
        items += linux_items(bwrap_path)
        if want_probe:
            if bwrap_path:
                items.append(probe(bwrap_path))
            else:
                items.append(item("probe", "unknown", "not run: bwrap is not on PATH", "PATH"))
    elif system == "Darwin":
        path = shutil.which("sandbox-exec")
        items.append(item("sandbox-exec", "available" if path else "unavailable",
                          "found at %s (not executed)" % path if path else "not on PATH", "PATH"))
        if want_probe:
            items.append(item("probe", "unknown", "not run: --probe is Linux and bwrap only", "platform"))
    else:
        items[0] = item("os", "unavailable", "unsupported operating system: %s" % clean(system, 30), "platform")
        if want_probe:
            items.append(item("probe", "unknown", "not run: unsupported operating system", "platform"))
    return system, items


def states(items):
    return {entry["item"]: entry["state"] for entry in items}


def mechanisms(system, items):
    """(bwrap_usable, any_usable, why_not): what the host facts allow, without running anything."""
    seen = states(items)
    if system == "Darwin":
        ok = seen.get("sandbox-exec") == "available"
        return ok, ok, "sandbox-exec is not on PATH"
    if system != "Linux":
        return False, False, "this operating system has no known sandbox mechanism here"
    blockers = []
    if seen.get("bwrap") != "available":
        blockers.append("bwrap is not on PATH")
    for name in ("userns_clone", "apparmor_restrict_userns"):
        if seen.get(name) == "restricted":
            blockers.append("%s is restricted" % name)
    if seen.get("max_user_namespaces") == "unavailable":
        blockers.append("max_user_namespaces is 0")
    if seen.get("probe") == "unavailable":
        blockers.append("the --probe run failed")
    landlock = seen.get("landlock") == "available"
    return not blockers, (not blockers) or landlock, "; ".join(blockers)


def load_security():
    spec = importlib.util.spec_from_file_location("oms_doctor_security", os.path.join(HERE, "doctor-security.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def cross_check(system, items):
    """warn rows for provider sandbox settings that are on while the host cannot honour them."""
    try:
        security = load_security()
        rows, facts = [], {}
        home = os.path.expanduser("~")
        security.claude(rows, facts, home)
        security.codex(rows, facts, home)
        security.antigravity(rows, facts, home)
    except Exception as error:  # a reader fault must not hide the host facts
        return [("info", "settings cross-check skipped: %s" % clean(error, 60))]
    bwrap_ok, any_ok, why = mechanisms(system, items)
    out = []
    claude = facts.get("claude")
    if isinstance(claude, dict) and claude.get("sandbox_enabled") and not bwrap_ok:
        out.append(("warn", "claude: sandbox.enabled is on but the sandbox helper is unusable here (%s); see oms doctor --security" % why))
    codex = facts.get("codex")
    if isinstance(codex, dict):
        modes = [codex.get("top", {}).get("sandbox_mode")] + [
            value.get("sandbox_mode") for key, value in codex.items() if key.startswith("profile:")]
        if any(mode in ("read-only", "workspace-write") for mode in modes) and not any_ok:
            out.append(("warn", "codex: a sandbox_mode is set but no sandbox mechanism is usable here (%s); see oms doctor --security" % why))
    antigravity = facts.get("antigravity")
    if isinstance(antigravity, dict) and antigravity.get("enableTerminalSandbox") is True and not any_ok:
        out.append(("warn", "antigravity: enableTerminalSandbox is on but no sandbox mechanism is usable here (%s); see oms doctor --security" % why))
    return out


def main(argv):
    as_json = want_probe = False
    for arg in argv:
        if arg == "--json":
            as_json = True
        elif arg == "--probe":
            want_probe = True
        else:
            print("error: unknown argument: %s" % clean(arg, 40), file=sys.stderr)
            return 2
    system, items = collect(want_probe)
    warnings = cross_check(system, items)
    counts = {state: sum(entry["state"] == state for entry in items) for state in STATES}
    if as_json:
        print(json.dumps({"schema": 1, "kind": "oms-doctor-sandbox", "os": system, "probe_requested": want_probe,
                          "items": items,
                          "warnings": [{"level": level, "text": text} for level, text in warnings],
                          "counts": counts}, sort_keys=True))
        return 0
    for entry in items:
        print("%s: %s: %s [%s]" % (entry["state"], entry["item"], entry["detail"], entry["evidence"]))
    for level, text in warnings:
        print("%s: %s" % (level, text))
    print("sandbox: %d available, %d unavailable, %d restricted, %d unknown, %d warning(s); facts only, nothing was changed"
          % (counts["available"], counts["unavailable"], counts["restricted"], counts["unknown"],
             sum(level == "warn" for level, _ in warnings)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
