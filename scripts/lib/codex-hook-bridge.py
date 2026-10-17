#!/usr/bin/env python3
"""Retire only the exact OMS user-hook bridge after native plugin verification."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import re
import shutil
import subprocess
import signal

# Reuse the existing bounded, strict JSON and non-link file readers.
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "oms_skill_io", Path(__file__).with_name("skill-lifecycle.py"))
io = importlib.util.module_from_spec(spec)
spec.loader.exec_module(io)

# Every bridge surface OMS ever wrote stays recognizable so an old bridge can be
# retired; RETIRED rows have no native replacement to verify.
SURFACES = {
    ("UserPromptSubmit", "", "skill-router"): {5},
    ("Stop", "", "turn-guard"): {12},
    ("PreCompact", "", "precompact-handoff codex"): {30},
    ("SessionStart", "", "resume-hook"): {10},
    ("SessionStart", "", "telemetry-hook"): {5},
    ("PostToolUse", "apply_patch", "syntax-guard-hook"): {5},
    ("PostToolUse", "", "telemetry-hook"): {5},
    ("SubagentStop", "", "telemetry-hook"): {5},
    ("SessionEnd", "", "telemetry-hook"): {3, 5},
}
# Tool-level and subagent-stop telemetry were deliberately retired, not moved.
RETIRED = {("PostToolUse", "", "telemetry-hook"), ("SubagentStop", "", "telemetry-hook")}


def read_json(path):
    return io.strict_json_bytes(io.read_regular(path, path.name), path.name)


def expected_hook_keys(native_root, plugin_id):
    hooks = read_json(native_root / "hooks.json").get("hooks", {})
    keys = {}
    for event, groups in hooks.items():
        snake = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", event).lower()
        for group_index, group in enumerate(groups):
            for hook_index, hook in enumerate(group.get("hooks", [])):
                if isinstance(hook, dict) and hook.get("type") == "command":
                    suffix = "hooks.json:%s:%d:%d" % (snake, group_index, hook_index)
                    command = hook.get("command", "")
                    marker = 'harness-hook.sh" '
                    label = command.split(marker, 1)[1] if marker in command else event
                    keys[plugin_id + ":" + suffix] = label
    return keys


def hooks_list_trust(native_root, plugin_id, binary=None):
    expected = expected_hook_keys(native_root, plugin_id)
    binary = binary or shutil.which("codex")
    if not binary:
        return "unknown", []
    env = dict(os.environ)
    for name in ("OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"):
        env.pop(name, None)
    requests = [
        {"id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "oms_hook_trust", "version": "1"}}},
        {"method": "initialized", "params": {}},
        {"id": 2, "method": "hooks/list", "params": {"cwds": [str(Path.cwd())]}},
    ]
    payload = "".join(json.dumps(row) + "\n" for row in requests).encode()
    # The probe owns its process group and reads at most 1 MiB of stdout, so a
    # broken or hostile CLI can neither hang the installer nor flood memory.
    group = ({"start_new_session": True} if os.name != "nt"
             else {"creationflags": getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)})
    limit = 1024 * 1024
    with tempfile.TemporaryFile() as captured:
        try:
            proc = subprocess.Popen([binary, "app-server", "--listen", "stdio://"],
                                    stdin=subprocess.PIPE, stdout=captured,
                                    stderr=subprocess.DEVNULL, env=env, **group)
        except OSError:
            return "unknown", []
        try:
            proc.communicate(payload, timeout=8)
            timed_out = False
        except subprocess.TimeoutExpired:
            timed_out = True
        finally:
            if os.name != "nt":
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except OSError:
                    pass
            elif proc.poll() is None:
                proc.kill()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
        if timed_out or proc.returncode != 0:
            return "unknown", []
        captured.seek(0)
        stdout = captured.read(limit + 1)
    if len(stdout) > limit:
        return "unknown", []
    requested = os.path.realpath(str(Path.cwd()))
    response = None
    try:
        for line in stdout.decode("utf-8").splitlines():
            try:
                row = json.loads(line)
            except (ValueError, TypeError):
                continue
            if isinstance(row, dict) and row.get("id") == 2:
                response = row
                break
    except (UnicodeError, ValueError):
        return "unknown", []
    if not isinstance(response, dict) or "error" in response:
        return "unknown", []
    result_data = response.get("result")
    if not isinstance(result_data, dict) or not isinstance(result_data.get("data"), list):
        return "unknown", []
    found = {}
    for entry in result_data["data"]:
        if (not isinstance(entry, dict) or not isinstance(entry.get("cwd"), str)
                or os.path.realpath(entry["cwd"]) != requested
                or not isinstance(entry.get("errors"), list)
                or not isinstance(entry.get("hooks"), list)
                or not isinstance(entry.get("warnings"), list)):
            return "unknown", []
        for hook in entry["hooks"]:
            if not isinstance(hook, dict):
                return "unknown", []
            if hook.get("pluginId") != plugin_id:
                continue
            key = hook.get("key")
            status = hook.get("trustStatus")
            if not isinstance(key, str) or key not in expected or status not in (
                    "trusted", "untrusted", "modified", "managed") or key in found:
                return "unknown", []
            # Trust and enablement are separate fields; only a positively
            # enabled hook runs, so a missing or malformed flag counts as disabled.
            found[key] = status if hook.get("enabled") is True else "disabled"
    if set(found) != set(expected):
        return "unknown", []
    untrusted = [expected[key] for key in expected if found[key] != "trusted"]
    return ("untrusted", untrusted) if untrusted else ("trusted", [])


def sync_directory(path):
    if os.name != "nt":
        descriptor = os.open(str(path), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


def native_verified(args):
    catalog = io.strict_json_bytes(sys.stdin.buffer.read(io.MAX_JSON_BYTES + 1), "plugin catalog")
    matches = [row for row in catalog.get("installed", [])
               if isinstance(row, dict) and row.get("pluginId") == args.plugin_id]
    if len(matches) != 1:
        return False
    row = matches[0]
    source = row.get("source")
    if (row.get("installed") is not True or row.get("enabled") is not True
            or not isinstance(source, dict)
            or source.get("path") != str(args.source_root / "plugins/oh-my-setting")):
        return False
    manifest = read_json(args.native_root / ".codex-plugin/plugin.json")
    if (manifest.get("name") != "oh-my-setting" or manifest.get("hooks") != "./hooks.json"
            or manifest.get("version") != row.get("version")):
        return False
    hooks = read_json(args.native_root / "hooks.json").get("hooks", {})
    for (event, matcher, action) in SURFACES:
        if (event, matcher, action) in RETIRED:
            continue
        expected = 'bash "${CLAUDE_PLUGIN_ROOT:-.}/scripts/harness-hook.sh" ' + action
        if not any(isinstance(group, dict) and group.get("matcher", "") == matcher
                   and any(isinstance(hook, dict) and hook.get("type") == "command"
                           and hook.get("command") == expected for hook in group.get("hooks", []))
                   for group in hooks.get(event, [])):
            return False
    return True


def known_hook(event, matcher, hook, source_root):
    if (not isinstance(hook, dict) or set(hook) - {"type", "command", "timeout"}
            or hook.get("type") != "command"):
        return False
    plugin = str(source_root / "plugins/oh-my-setting")
    prefix = 'CLAUDE_PLUGIN_ROOT="%s" bash "%s/scripts/harness-hook.sh" ' % (plugin, plugin)
    for (known_event, known_matcher, action), timeouts in SURFACES.items():
        if (event, matcher) != (known_event, known_matcher) or hook.get("timeout") not in timeouts:
            continue
        commands = {"env " + prefix + action}
        if action == "turn-guard":
            commands.add("env OMS_TURN_GUARD_MAX_BLOCKS_PER_TURN=0 " + prefix + action)
        if hook.get("command") in commands:
            return True
    return False


def migrate(args):
    path = args.hooks
    if not os.path.lexists(path):
        return 1 if args.probe else 0
    # A redirected config directory must not turn a migration into another user's write.
    if path.parent.is_symlink() or path.parent.resolve() != path.parent.absolute():
        raise ValueError("hook parent must be a canonical non-link directory")
    original = io.read_regular(path, "user hooks")
    before = path.lstat()
    data = io.strict_json_bytes(original, "user hooks")
    hooks = data.get("hooks")
    if not isinstance(hooks, dict):
        raise ValueError("user hooks must contain a hooks object")
    count = 0
    for event, groups in hooks.items():
        if not isinstance(groups, list):
            raise ValueError("hook event must contain a list")
        kept_groups = []
        for group in groups:
            if (not isinstance(group, dict) or set(group) - {"hooks", "matcher"}
                    or not isinstance(group.get("hooks"), list)):
                kept_groups.append(group)
                continue
            kept = [hook for hook in group["hooks"]
                    if not known_hook(event, group.get("matcher", ""), hook, args.source_root)]
            removed = len(group["hooks"]) - len(kept)
            count += removed
            if kept or not removed:
                kept_groups.append(dict(group, hooks=kept))
        hooks[event] = kept_groups
    if not count:
        return 1 if args.probe else 0
    if args.probe:
        return 0
    if not args.native_root or not native_verified(args):
        print("codex-hook-bridge: preserved; native plugin hooks are not verified")
        return
    state, untrusted = hooks_list_trust(args.native_root, args.plugin_id, args.codex)
    if state != "trusted":
        detail = "untrusted: " + ", ".join(untrusted) if state == "untrusted" else "unknown"
        print("codex-hook-bridge: preserved; native hook trust %s (open /hooks in Codex and trust the OMS hooks)" % detail)
        return
    if args.dry_run:
        print("codex-hook-bridge: would retire %d legacy commands with an exact-byte backup" % count)
        return
    backup = path.with_name(path.name + ".oms-bridge-" + hashlib.sha256(original).hexdigest() + ".bak")
    if os.path.lexists(backup):
        if io.read_regular(backup, "bridge backup") != original:
            raise ValueError("existing bridge backup differs")
    else:
        descriptor = os.open(str(backup), os.O_WRONLY | os.O_CREAT | os.O_EXCL
                             | getattr(os, "O_BINARY", 0), 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(original)
            handle.flush()
            os.fsync(handle.fileno())
    sync_directory(path.parent)
    encoded = (json.dumps(data, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode("utf-8")
    descriptor, temporary = tempfile.mkstemp(prefix=".oms-hook-bridge.", dir=str(path.parent))
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, stat.S_IMODE(before.st_mode))
        current = path.lstat()
        if ((current.st_dev, current.st_ino, current.st_mtime_ns, current.st_ctime_ns)
                != (before.st_dev, before.st_ino, before.st_mtime_ns, before.st_ctime_ns)
                or io.read_regular(path, "user hooks") != original):
            raise ValueError("user hooks changed during migration")
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    print("codex-hook-bridge: retired %d legacy commands; backup: %s" % (count, backup))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hooks", type=Path, required=True)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--native-root", type=Path)
    parser.add_argument("--plugin-id", default="oh-my-setting@oh-my-setting-local")
    parser.add_argument("--codex")
    parser.add_argument("--trust-report", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--probe", action="store_true", help="read-only candidate check; 1 means absent")
    args = parser.parse_args()
    try:
        if args.trust_report:
            if not args.native_root:
                raise ValueError("--native-root is required with --trust-report")
            state, untrusted = hooks_list_trust(args.native_root, args.plugin_id, args.codex)
            print(state + (": " + ", ".join(untrusted) if untrusted else ""))
            return 0
        return migrate(args) or 0
    except (OSError, ValueError, TypeError, RecursionError, io.LifecycleError) as error:
        print("codex-hook-bridge: preserved; %s" % error, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
