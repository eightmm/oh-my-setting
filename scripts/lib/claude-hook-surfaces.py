"""Ownership, convergence and audit of OMS rows in Claude Code settings hooks.

install-claude-hooks.sh converges and removes with these functions and the
doctor audits with them, so a registration the doctor calls healthy is exactly
one the installer would leave unchanged.

A hook is OMS-owned only when its command names a registered script under
the current root or a verified OMS checkout, either directly (legacy) or
through that same root's Python entry launcher. A launcher row whose root no
longer exists is still ours, so a moved or deleted checkout cannot strand a
blocking row. Arbitrary wrappers and same-named scripts elsewhere stay
user-owned: never rewritten or removed.
"""

import os
import shlex

# Retired names stay listed so an old registration remains removable.
SCRIPTS = frozenset((
    "skill-router.sh", "turn-guard.sh", "fail-ledger-hook.sh",
    "syntax-guard-hook.sh", "tier-guard-hook.sh", "precompact-handoff.sh",
    "resume-hook.sh", "telemetry-hook.sh", "git-move-guard-hook.sh",
    "panel-direct-work-hook.sh", "command-guard-hook.sh",
))


def entry_command(root, executable, path):
    return " ".join(shlex.quote(arg) for arg in (
        "bash", "%s/scripts/lib/python-entry.sh" % root, executable, path))


def hook_command(root, script):
    return entry_command(root, "bash", "%s/scripts/%s" % (root, script))


def entry_path(command, executable):
    try:
        argv = shlex.split(command)
    except ValueError:
        return None
    if len(argv) == 2 and argv[0] == executable:
        return argv[1]
    if len(argv) == 4 and argv[0] == "bash" and argv[2] == executable:
        root = _norm(argv[3]).rsplit("/", 2)[0]
        if _norm(argv[1]) == root + "/scripts/lib/python-entry.sh":
            return argv[3]
    return None


def _norm(path):
    return path.replace("\\", "/").rstrip("/")


def _script_path(command):
    path = entry_path(command, "bash")
    if path is not None:
        return path
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


def _dead_launcher(command, root):
    """True for the managed launcher form whose checkout is gone from disk."""
    try:
        argv = shlex.split(command)
    except ValueError:
        return False
    return (len(argv) == 4 and argv[0] == "bash" and argv[2] == "bash"
            and _norm(argv[1]) == root + "/scripts/lib/python-entry.sh"
            and not os.path.lexists(root))


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
    if is_oms_root(root, roots) or _dead_launcher(command, root):
        return parts[2]
    return None


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
    # Only a capable guard may keep an opt-in `block`; anything else is not
    # something the manifest asks for.
    if not (surface.get("fail_closed") and fixed.get("onFailure") == "block"):
        fixed.pop("onFailure", None)
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


def _guard_hooks(hooks, surfaces, roots):
    """The live owned hook of each surface the manifest marks fail-closed capable."""
    capable = {(row["event"], row["script"]) for row in surfaces if row.get("fail_closed")}
    for event, entry in _entries(hooks):
        for hook in entry["hooks"]:
            script = owned_script(hook, roots)
            if script is not None and (event, script) in capable:
                yield (event, script), hook


def apply_fail_closed(hooks, surfaces, roots, mode):
    """Set or clear Claude Code's `onFailure: "block"` on the capable guard rows.

    mode "on" writes it, "off" removes it and "keep" leaves each row as found,
    so an earlier opt-in survives an update that does not repeat the request.
    Only the fail-closed capable rows are touched; a user's own hook and every
    other OMS row keep whatever they carry. Returns the surfaces changed."""
    changed = []
    if mode not in ("on", "off"):
        return changed
    for key, hook in _guard_hooks(hooks, surfaces, roots):
        if mode == "on" and hook.get("onFailure") != "block":
            hook["onFailure"] = "block"
            changed.append(key)
        elif mode == "off" and "onFailure" in hook:
            del hook["onFailure"]
            changed.append(key)
    return changed


def fail_open(hooks, surfaces, roots):
    """Capable guard surfaces whose live registration does not block when the hook itself fails."""
    return sorted(key for key, hook in _guard_hooks(hooks, surfaces, roots)
                  if hook.get("onFailure") != "block")


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
        if "onFailure" in hook and not (row.get("fail_closed") and hook["onFailure"] == "block"):
            issues.append(("drift", "onFailure %r, expected absent%s" % (
                hook["onFailure"], " or 'block'" if row.get("fail_closed") else "")))
        path = entry_path(str(hook.get("command")), "bash")
        if path is not None and _dead_launcher(
                hook["command"], _norm(path).rsplit("/", 2)[0]):
            issues.append(("drift", "dead OMS hook: its checkout no longer exists"))
        if not matcher_ok(entry, row["matcher"]):
            issues.append(("drift", "matcher %r, expected %r" % (
                entry.get("matcher"), row["matcher"])))
    return problems, sorted(found)
