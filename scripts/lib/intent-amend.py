#!/usr/bin/env python3
"""Amend an adopted PROJECT.md: intent-amend.py draft|delta|publish ...

draft   PROJECT INTENT_DIR ID REASON   write a draft candidate bound to the contract's current bytes
delta   OLD NEW LIB [OUT_JSON]         print what the amendment changes; optionally write it as JSON
publish PROJECT STAGED INTENT_DIR ID EXPECTED_SHA DELTA_JSON
                                       keep the old bytes, replace the contract, append a receipt

The previous contract is never lost: publish stores its exact bytes under INTENT_DIR/archive/<sha256>.md
before the replace, and refuses (exit 17) when PROJECT.md no longer hashes to EXPECTED_SHA."""
import datetime
import hashlib
import json
import os
import re
import stat
import sys
import tempfile
import unicodedata

sys.dont_write_bytecode = True
LIMIT = 1024 * 1024
CRITERIA_HEADINGS = ["acceptance", "done", "success", "completion", "완료", "성공", "수용"]


def die(message, code=3):
    sys.stderr.write("error: intent amend: %s\n" % message)
    raise SystemExit(code)


def read_regular(path):
    """Bytes of a regular, non-symlink file of at most 1 MiB."""
    try:
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    except OSError as exc:
        die("cannot open %s: %s" % (os.path.basename(path), exc.strerror or exc))
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            die("%s is not a regular file" % os.path.basename(path))
        data = b""
        while len(data) <= LIMIT:
            chunk = os.read(descriptor, 65536)
            if not chunk:
                break
            data += chunk
    finally:
        os.close(descriptor)
    if len(data) > LIMIT:
        die("%s is larger than 1 MiB" % os.path.basename(path))
    return data


def write_new(path, data, mode=0o600):
    descriptor, temporary = tempfile.mkstemp(prefix=".intent-", dir=os.path.dirname(path))
    try:
        os.fchmod(descriptor, mode)
        offset = 0
        while offset < len(data):
            offset += os.write(descriptor, data[offset:])
        os.fsync(descriptor)
        os.link(temporary, path)
    finally:
        os.close(descriptor)
        os.unlink(temporary)


def clean_reason(reason):
    reason = " ".join(reason.split())
    if not reason:
        die("--reason is required: say why the contract changes", 2)
    if len(reason) > 500:
        die("--reason is longer than 500 characters; compact it", 2)
    if any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in reason):
        die("--reason contains a control or format character", 2)
    return reason


def shown(value, limit=90):
    value = "".join(ch if unicodedata.category(ch)[0] != "C" else " " for ch in str(value))
    value = " ".join(value.split())
    return value if len(value) <= limit else value[:limit - 3] + "..."


def draft(project, intent_dir, ident, reason):
    reason = clean_reason(reason)
    data = read_regular(project)
    try:
        text = data.decode("utf-8", errors="strict")
    except UnicodeDecodeError:
        die("PROJECT.md is not UTF-8")
    lines = text.splitlines(True)
    in_status, matches = False, []
    for index, line in enumerate(lines):
        plain = line.rstrip("\r\n")
        if plain.startswith("## "):
            in_status = plain == "## Status"
            continue
        if in_status and re.match(r"^- State:[ \t]*\S", plain):
            matches.append(index)
    if len(matches) != 1:
        die("PROJECT.md needs exactly one '- State:' bullet in '## Status' to be amended")
    index = matches[0]
    state = lines[index].rstrip("\r\n").split(":", 1)[1].strip().lower()
    if state not in ("confirmed", "active"):
        die("only an adopted contract is amended (State: confirmed); this one says %s" % shown(state, 30), 2)
    ending = lines[index][len(lines[index].rstrip("\r\n")):]
    lines[index] = "- State: draft" + ending
    os.makedirs(intent_dir, exist_ok=True)
    candidate = os.path.join(intent_dir, ident + ".md")
    binding = os.path.join(intent_dir, ident + ".amend.json")
    base = hashlib.sha256(data).hexdigest()
    created = False
    try:
        write_new(candidate, "".join(lines).encode("utf-8"), 0o644)
        created = True
        write_new(binding, (json.dumps({
            "schema": 1, "kind": "intent-amendment", "id": ident, "base_project_sha256": base,
            "reason": reason, "ts": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        }, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8"))
    except OSError as exc:
        if created:
            os.unlink(candidate)
        if isinstance(exc, FileExistsError):
            die("an amendment candidate named %s already exists" % ident)
        die("cannot write the amendment candidate: %s" % (exc.strerror or exc))
    print(base)


def section_map(text):
    found, name = {}, "(preamble)"
    for line in text.splitlines():
        if line.startswith("## "):
            name = line[3:].strip()
            found.setdefault(name, [])
            continue
        found.setdefault(name, []).append(line.rstrip())
    return {key: "\n".join(value).strip() for key, value in found.items()}


def criteria_of(text, lib):
    sys.path.insert(0, lib)
    from oms_runtime.markdown import (bullet_items, normalized_line, section_text, sections,
                                      stable_criterion_id, strip_criterion_marker)
    body, _ = sections(text)
    found = {}
    for raw in bullet_items(section_text(body, CRITERIA_HEADINGS)):
        criterion_id = stable_criterion_id("project", raw)
        if criterion_id in found:
            die("duplicate criterion id %s" % shown(criterion_id, 80))
        found[criterion_id] = strip_criterion_marker(normalized_line(raw))
    return found


def state_neutral(text):
    return re.sub(r"(?m)^- State:[ \t]*\S.*$", "- State: -", text)


def delta(old_path, new_path, lib, out_json=None):
    old = read_regular(old_path).decode("utf-8", errors="replace")
    new = read_regular(new_path).decode("utf-8", errors="replace")
    before, after = section_map(state_neutral(old)), section_map(state_neutral(new))
    changed = [name for name in list(after) + [name for name in before if name not in after]
               if before.get(name) != after.get(name)]
    result = {"sections": changed[:40], "added": [], "modified": [], "removed": [], "criteria": "read"}
    try:
        was, now = criteria_of(old, lib), criteria_of(new, lib)
    except Exception:
        was, now, result["criteria"] = {}, {}, "unread"
    for criterion_id, statement in now.items():
        if criterion_id not in was:
            result["added"].append(criterion_id)
            print("ADDED %s: %s" % (shown(criterion_id, 40), shown(statement)))
        elif was[criterion_id] != statement:
            result["modified"].append(criterion_id)
            print("MODIFIED %s: %s -> %s" % (shown(criterion_id, 40), shown(was[criterion_id], 60), shown(statement, 60)))
    for criterion_id, statement in was.items():
        if criterion_id not in now:
            result["removed"].append(criterion_id)
            print("REMOVED %s: %s" % (shown(criterion_id, 40), shown(statement)))
    if result["criteria"] == "unread":
        print("criteria: could not be compared")
    elif not (result["added"] or result["modified"] or result["removed"]):
        print("criteria: unchanged")
    print("sections changed: %s" % (", ".join(shown(name, 40) for name in changed[:12]) or "none"))
    for key in ("added", "modified", "removed"):
        result[key] = result[key][:64]
    if out_json:
        with open(out_json, "w", encoding="utf-8") as handle:
            json.dump(result, handle, ensure_ascii=False, sort_keys=True)
    if not changed:
        raise SystemExit(4)


def publish(project, staged, intent_dir, ident, expected, delta_json):
    current = read_regular(project)
    previous = hashlib.sha256(current).hexdigest()
    if previous != expected:
        raise SystemExit(17)
    try:
        with open(os.path.join(intent_dir, ident + ".amend.json"), encoding="utf-8") as handle:
            binding = json.load(handle)
        with open(delta_json, encoding="utf-8") as handle:
            changes = json.load(handle)
    except (OSError, ValueError) as exc:
        die("cannot read the amendment binding: %s" % exc)
    if binding.get("base_project_sha256") != expected:
        raise SystemExit(17)
    archive_dir = os.path.join(intent_dir, "archive")
    os.makedirs(archive_dir, exist_ok=True)
    archive = os.path.join(archive_dir, previous + ".md")
    try:
        write_new(archive, current, 0o444)
    except FileExistsError:
        if hashlib.sha256(read_regular(archive)).hexdigest() != previous:
            die("the archive entry for the previous contract does not hold its bytes")
    except OSError as exc:
        die("cannot archive the previous contract: %s" % (exc.strerror or exc))
    replacement = hashlib.sha256(read_regular(staged)).hexdigest()
    receipt = {
        "schema": 1, "kind": "intent-amendment-receipt", "id": ident,
        "ts": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "reason": binding.get("reason", ""), "previous_sha256": previous, "project_sha256": replacement,
        "archive": "archive/%s.md" % previous,
        "sections": changes.get("sections", []), "added": changes.get("added", []),
        "modified": changes.get("modified", []), "removed": changes.get("removed", []),
    }
    try:
        descriptor = os.open(os.path.join(intent_dir, "amendments.jsonl"),
                             os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0), 0o600)
        try:
            if not stat.S_ISREG(os.fstat(descriptor).st_mode):
                die("receipt is not a regular file")
            data = (json.dumps(receipt, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8")
            original_size = os.fstat(descriptor).st_size
            try:
                offset = 0
                while offset < len(data):
                    offset += os.write(descriptor, data[offset:])
                os.fsync(descriptor)
            except OSError:
                os.ftruncate(descriptor, original_size)
                raise
        finally:
            os.close(descriptor)
    except OSError as exc:
        die("cannot write the amendment receipt: %s" % (exc.strerror or exc))
    try:
        os.chmod(staged, stat.S_IMODE(os.stat(project).st_mode))
        os.replace(staged, project)
    except OSError as exc:
        die("cannot replace PROJECT.md: %s" % (exc.strerror or exc))
    print(previous)


if __name__ == "__main__":
    action, rest = sys.argv[1], sys.argv[2:]
    if action == "draft" and len(rest) == 4:
        draft(*rest)
    elif action == "delta" and len(rest) in (3, 4):
        delta(*rest)
    elif action == "publish" and len(rest) == 6:
        publish(*rest)
    else:
        die("usage: draft|delta|publish", 2)
