"""Ownership, convergence and audit of OMS rows in Claude Code settings hooks.

install-claude-hooks.sh converges and removes with these functions and the
doctor audits with them, so a registration the doctor calls healthy is exactly
one the installer would leave unchanged.

A hook is OMS-owned only when its command is `bash <root>/scripts/<script>`
for a script OMS registers or once registered, and <root> is the current root
or a verified OMS checkout. A same-named script elsewhere, a wrapper, or any
other command stays user-owned: never rewritten, removed or counted.
"""

import os
import shlex

# Retired names stay listed so an old registration remains removable.
SCRIPTS = frozenset((
    "skill-router.sh", "turn-guard.sh", "fail-ledger-hook.sh",
    "syntax-guard-hook.sh", "tier-guard-hook.sh", "precompact-handoff.sh",
    "resume-hook.sh", "telemetry-hook.sh",
))


def hook_command(root, script):
    return "bash " + shlex.quote("%s/scripts/%s" % (root, script))


def _norm(path):
    return path.replace("\\", "/").rstrip("/")


def _script_path(command):
    try:
        argv = shlex.split(command)
    except ValueError:
        argv = []
    if len(argv) == 2 and argv[0] == "bash":
        return argv[1]
    # Earlier installers wrote the path unquoted, which splits a root that
    # contains spaces; that exact legacy form is still ours to converge.
    rest = command[5:].strip() if command.startswith("bash ") else ""
    if rest and not any(ch in rest for ch in "'\"\\$`;&|<>"):
        return rest
    return None


def is_oms_root(candidate, roots):
    if _norm(candidate) in {_norm(root) for root in roots}:
        return True
    try:
        real = os.path.realpath(candidate)
        if real in {os.path.realpath(root) for root in roots}:
            return True
    except (OSError, ValueError):
        return False
    # A previous checkout that still carries the OMS source markers.
    return (
        os.path.isfile(os.path.join(real, ".agents", "plugins", "marketplace.json"))
        and os.path.isfile(os.path.join(real, "scripts", "install-claude-hooks.sh"))
    )


def owned_script(hook, roots):
    if not isinstance(hook, dict) or hook.get("type", "command") != "command":
        return None
    command = hook.get("command")
    path = _script_path(command) if isinstance(command, str) else None
    if path is None:
        return None
    parts = _norm(path).rsplit("/", 2)
    if len(parts) != 3 or parts[1] != "scripts" or parts[2] not in SCRIPTS:
        return None
    root = parts[0]
    if not (root.startswith("/") or (len(root) > 2 and root[1:3] == ":/")):
        return None
    return parts[2] if is_oms_root(root, roots) else None


def matcher_ok(entry, matcher):
    current = entry.get("matcher")
    if matcher is None:
        return current in (None, "")
    return current == matcher


def _desired_hook(hook, surface, command_root):
    fixed = dict(hook, type="command",
                 command=hook_command(command_root, surface["script"]))
    if surface["timeout"] is None:
        fixed.pop("timeout", None)
    else:
        fixed["timeout"] = surface["timeout"]
    return fixed


def _entries(hooks):
    for event, entries in hooks.items():
        if not isinstance(entries, list):
            continue
        for entry in entries:
            if isinstance(entry, dict) and isinstance(entry.get("hooks"), list):
                yield event, entry


def converge(hooks, surfaces, command_root, roots):
    """Rewrite hooks in place to exactly one row per expected surface.

    The first owned registration of a surface is fixed where it stands;
    retired and duplicate owned rows are dropped. An owned hook whose entry
    matcher is wrong is split out, so user siblings keep their entry, its
    metadata and their relative order.
    """
    want = {(row["event"], row["script"]): row for row in surfaces}
    seen = set()
    keep = object()
    for event in list(hooks):
        entries = hooks[event]
        if not isinstance(entries, list):
            continue
        result = []
        for entry in entries:
            if not isinstance(entry, dict) or not isinstance(entry.get("hooks"), list):
                result.append(entry)
                continue
            segments = []
            changed = False
            for hook in entry["hooks"]:
                state, item = keep, hook
                script = owned_script(hook, roots)
                if script is not None:
                    surface = want.get((event, script))
                    if surface is None or (event, script) in seen:
                        changed = True
                        continue
                    seen.add((event, script))
                    item = _desired_hook(hook, surface, command_root)
                    changed = changed or item != hook
                    if not matcher_ok(entry, surface["matcher"]):
                        state = (surface["matcher"],)
                        changed = True
                if segments and segments[-1][0] == state:
                    segments[-1][1].append(item)
                else:
                    segments.append((state, [item]))
            if not changed:
                result.append(entry)
                continue
            for state, items in segments:
                split = dict(entry, hooks=items)
                if state is not keep:
                    split.pop("matcher", None)
                    if state[0] is not None:
                        split["matcher"] = state[0]
                result.append(split)
        if result:
            hooks[event] = result
        else:
            del hooks[event]
    for row in surfaces:
        if (row["event"], row["script"]) in seen:
            continue
        entry = {"hooks": [_desired_hook({}, row, command_root)]}
        if row["matcher"] is not None:
            entry["matcher"] = row["matcher"]
        entries = hooks.setdefault(row["event"], [])
        if not isinstance(entries, list):
            raise ValueError("hooks.%s is not a list" % row["event"])
        entries.append(entry)


def remove(hooks, roots):
    """Delete every owned hook; user siblings keep their entry and order."""
    for event in list(hooks):
        entries = hooks[event]
        if not isinstance(entries, list):
            continue
        kept = []
        for entry in entries:
            if not isinstance(entry, dict) or not isinstance(entry.get("hooks"), list):
                kept.append(entry)
                continue
            rest = [h for h in entry["hooks"] if owned_script(h, roots) is None]
            if len(rest) == len(entry["hooks"]):
                kept.append(entry)
            elif rest:
                kept.append(dict(entry, hooks=rest))
        if kept:
            hooks[event] = kept
        else:
            del hooks[event]


def audit(hooks, surfaces, command_root, roots):
    """Return {(event, script): [problem, ...]} for expected surfaces, plus
    the retired owned pairs still registered. A problem is (kind, detail),
    kind one of missing, duplicate, drift."""
    found = {}
    for event, entry in _entries(hooks):
        for hook in entry["hooks"]:
            script = owned_script(hook, roots)
            if script is not None:
                found.setdefault((event, script), []).append((entry, hook))
    problems = {}
    for row in surfaces:
        key = (row["event"], row["script"])
        rows = found.pop(key, [])
        issues = problems[key] = []
        if not rows:
            issues.append(("missing", ""))
            continue
        if len(rows) > 1:
            issues.append(("duplicate", "%d registrations" % len(rows)))
        entry, hook = rows[0]
        want = hook_command(command_root, row["script"])
        if hook.get("type") != "command":
            issues.append(("drift", "type absent, expected command"))
        if hook.get("command") != want:
            issues.append(("drift", "command %r, expected %r" % (hook.get("command"), want)))
        if hook.get("timeout") != row["timeout"]:
            issues.append(("drift", "timeout %s, expected %s" % (
                hook.get("timeout", "absent"),
                "absent" if row["timeout"] is None else row["timeout"])))
        if not matcher_ok(entry, row["matcher"]):
            issues.append(("drift", "matcher %r, expected %r" % (
                entry.get("matcher"), row["matcher"])))
    return problems, sorted(found)
