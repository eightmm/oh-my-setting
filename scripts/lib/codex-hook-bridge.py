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

# Reuse the existing bounded, strict JSON and non-link file readers.
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "oms_skill_io", Path(__file__).with_name("skill-lifecycle.py"))
io = importlib.util.module_from_spec(spec)
spec.loader.exec_module(io)

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


def read_json(path):
    return io.strict_json_bytes(io.read_regular(path, path.name), path.name)


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
        # Tool-level telemetry was deliberately retired, not moved to native hooks.
        if event == "PostToolUse" and action == "telemetry-hook":
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
        print("codex-hook-bridge: preserved; enabled native replacement is unverified")
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
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--probe", action="store_true", help="read-only candidate check; 1 means absent")
    args = parser.parse_args()
    try:
        return migrate(args) or 0
    except (OSError, ValueError, TypeError, RecursionError, io.LifecycleError) as error:
        print("codex-hook-bridge: preserved; %s" % error, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
