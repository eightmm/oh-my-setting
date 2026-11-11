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

The container_socket item stats the Docker/Podman daemon sockets (and the
DOCKER_HOST socket path) and never connects. A socket this user can write to
is "restricted": whoever can talk to the daemon can start a container that
mounts the host, so it sits outside any provider sandbox. A warn is added when
a provider sandbox setting is on while such a socket is reachable.

The `info: guard coverage` rows say which tool classes each provider's
registered guards see. They are derived from the registration table in
install-claude-hooks.sh, the Codex plugin hooks.json and the installed
Antigravity hooks.json, so a changed registration changes the rows.

Test-only environment: OMS_SANDBOX_ROOT prefixes the /proc, /sys, /etc and
socket paths, OMS_SANDBOX_OS replaces the detected operating system name and
OMS_COVERAGE_ROOT replaces the checkout the guard registrations are read from.
"""
import importlib.util
import json
import os
import platform
import pwd
import re
import shutil
import stat
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


def socket_candidates(system):
    """(path, from_env) container daemon sockets worth a stat; never connected to."""
    uid = os.getuid()
    found = [("/var/run/docker.sock", False), ("/run/docker.sock", False),
             ("/run/podman/podman.sock", False),
             ("/run/user/%d/docker.sock" % uid, False), ("/run/user/%d/podman/podman.sock" % uid, False)]
    if system == "Darwin":
        found.append((os.path.join(os.path.expanduser("~"), ".docker/run/docker.sock"), False))
    return found


def docker_group_member():
    """True/False for membership of the docker group in /etc/group, None when unreadable or no such group."""
    text, why = read_text("/etc/group")
    if why:
        return None
    try:
        user = pwd.getpwuid(os.getuid()).pw_name
    except KeyError:
        user = ""
    for row in text.splitlines():
        parts = row.split(":")
        if len(parts) >= 4 and parts[0] == "docker":
            members = parts[3].split(",")
            gid = int(parts[2]) if parts[2].isdigit() else None
            return user in members or (gid is not None and gid in os.getgroups())
    return None


def container_socket(system):
    """One item: restricted when a daemon socket is writable by this user, available otherwise."""
    paths = socket_candidates(system)
    notes = []
    host = os.environ.get("DOCKER_HOST", "")
    if host.startswith("unix://"):
        paths.insert(0, (host[len("unix://"):], True))
    elif host:
        notes.append("DOCKER_HOST is %s (not contacted)" % clean(host.split("://")[0] + "://…", 30))
    reachable, present = [], []
    for path, _ in paths:
        try:
            mode = os.stat(host_path(path)).st_mode
        except OSError:
            continue
        if not stat.S_ISSOCK(mode):
            continue
        if os.access(host_path(path), os.W_OK):
            reachable.append(path)
        else:
            present.append(path)
    member = docker_group_member()
    if member is not None:
        notes.append("%s in the docker group" % ("user is" if member else "user is not"))
    evidence = "stat of " + ", ".join(path for path, _ in paths[:3]) + ", …"
    tail = ("; " + "; ".join(notes)) if notes else ""
    if reachable:
        return item("container_socket", "restricted",
                    "%s writable by this user: reaching it is outside the sandbox boundary%s" % (", ".join(reachable), tail),
                    evidence)
    if present:
        return item("container_socket", "available",
                    "%s exists but this user cannot write to it%s" % (", ".join(present), tail), evidence)
    return item("container_socket", "available", "no container daemon socket found%s" % tail, evidence)


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
    if system in ("Linux", "Darwin"):
        items.append(container_socket(system))
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
    on = [name for name, setting in (
        ("claude", isinstance(claude, dict) and claude.get("sandbox_enabled")),
        ("codex", isinstance(codex, dict) and any(mode in ("read-only", "workspace-write") for mode in modes)),
        ("antigravity", isinstance(antigravity, dict) and antigravity.get("enableTerminalSandbox") is True)) if setting]
    if on and states(items).get("container_socket") == "restricted":
        out.append(("warn", "%s: a sandbox setting is on but a container daemon socket is writable (see container_socket); "
                    "whoever reaches it can start a container that mounts the host, outside the sandbox" % ", ".join(on)))
    return out


def hook_label(command):
    """The script (and its dispatch argument for harness-hook.sh) named by a hook command."""
    match = re.search(r'([\w.-]+\.sh)"?(?:\s+([\w-]+))?', str(command))
    if not match:
        return clean(command, 40)
    return match.group(2) if match.group(1) == "harness-hook.sh" and match.group(2) else match.group(1)


def claude_registration(root):
    """{event: [(matcher or None, script)]} from the HOOK_SURFACES table of install-claude-hooks.sh, or None."""
    try:
        with open(os.path.join(root, "scripts", "install-claude-hooks.sh"), encoding="utf-8") as handle:
            block = re.search(r"^HOOK_SURFACES='\n(.*?)^'", handle.read(), re.S | re.M)
    except OSError:
        return None
    if not block:
        return None
    table = {}
    for line in block.group(1).splitlines():
        line = line.strip()
        parts = line.split("|")
        if not line or line.startswith("#") or len(parts) < 4:
            continue
        matcher = "|".join(part.strip() for part in parts[2:-1]) or None
        table.setdefault(parts[0].strip(), []).append((matcher, parts[1].strip()))
    return table


def json_registration(path):
    """{event: [(matcher or None, label)]} from a hooks.json (plain or plugin-keyed), or None."""
    try:
        with open(path, encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return None
    if isinstance(data, dict) and isinstance(data.get("hooks"), dict):
        data = data["hooks"]
    elif isinstance(data, dict) and data and all(isinstance(v, dict) for v in data.values()):
        data = {event: groups for plugin in data.values() for event, groups in plugin.items()}
    table = {}
    for event, groups in (data.items() if isinstance(data, dict) else ()):
        for group in groups if isinstance(groups, list) else ():
            if not isinstance(group, dict):
                continue
            inner = group.get("hooks") if isinstance(group.get("hooks"), list) else [group]
            for hook in inner:
                if isinstance(hook, dict) and hook.get("command"):
                    table.setdefault(event, []).append((group.get("matcher") or None, hook_label(hook["command"])))
    return table


# Representative tool names per class; the class taxonomy is fixed, the coverage is read from the registration.
TOOL_CLASSES = (("shell", "Bash"), ("file edit", "Edit"), ("file edit", "Write"), ("file edit", "MultiEdit"),
                ("MCP", "mcp__server__tool"), ("read/search/web", "Read"), ("read/search/web", "WebFetch"))


def covers(matcher, tool):
    if matcher in (None, "", "*"):
        return True
    try:
        return re.fullmatch(matcher, tool) is not None
    except re.error:
        return False


def classes_seen(rows):
    seen = []
    for name, tool in TOOL_CLASSES:
        if any(covers(matcher, tool) for matcher, _ in rows) and name not in seen:
            seen.append(name)
    return seen


def describe(rows):
    return "; ".join("%s%s" % (script, " [%s]" % matcher if matcher else "") for matcher, script in rows)


def guard_coverage():
    """info rows: which tool classes each provider's registered guards see, read from the registrations."""
    root = os.environ.get("OMS_COVERAGE_ROOT") or os.path.dirname(os.path.dirname(HERE))
    out = []
    claude = claude_registration(root)
    if claude is None:
        out.append(("info", "guard coverage claude: unknown; the registration table in install-claude-hooks.sh was not readable"))
    else:
        pre = claude.get("PreToolUse", [])
        seen = classes_seen(pre)
        unseen = [name for name in dict.fromkeys(n for n, _ in TOOL_CLASSES) if name not in seen]
        post = classes_seen(claude.get("PostToolUse", []) + claude.get("PostToolUseFailure", []))
        out.append(("info", "guard coverage claude: PreToolUse guards (%s) see %s; they do not see %s%s" % (
            describe(pre) or "none", ", ".join(seen) or "no tool class", ", ".join(unseen) or "nothing else",
            "; after-the-fact PostToolUse hooks only report on %s" % ", ".join(post) if post else "")))
    codex = json_registration(os.path.join(root, "plugins", "oh-my-setting", "hooks.json"))
    if codex is None:
        out.append(("info", "guard coverage codex: unknown; plugins/oh-my-setting/hooks.json was not readable"))
    else:
        pre = codex.get("PreToolUse", [])
        out.append(("info", "guard coverage codex: %s; PostToolUse reports only after the fact (%s); other events are lifecycle (%s)" % (
            "PreToolUse guards (%s) see only their matcher" % describe(pre) if pre else "no pre-tool hook is registered, so no tool call is checked before it runs",
            describe(codex.get("PostToolUse", [])) or "none",
            ", ".join(sorted(event for event in codex if not event.endswith("ToolUse"))) or "none")))
    agy_path = os.path.join(os.path.expanduser("~"), ".gemini", "config", "plugins", "oh-my-setting", "hooks.json")
    agy = json_registration(agy_path)
    if agy is None:
        out.append(("info", "guard coverage antigravity: no hooks.json installed at %s, so no hook sees any tool call" % clean(agy_path, 80)))
    else:
        tool_events = sorted(event for event in agy if "tool" in event.lower())
        out.append(("info", "guard coverage antigravity: installed hooks are %s; %s" % (
            ", ".join(sorted(agy)) or "none",
            "tool-level hooks: %s" % ", ".join(tool_events) if tool_events else "no tool-level hook, so no tool call is checked")))
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
    warnings = cross_check(system, items) + guard_coverage()
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
