#!/usr/bin/env python3
"""Content-free install fingerprint and two-machine comparison.

Usage:
  install-fingerprint.py print ROOT RECEIPT
  install-fingerprint.py compare ROOT RECEIPT OTHER.json [--json]

Reads the install receipt and a few managed files; never runs a provider CLI,
never opens the network and never writes. Only version strings, flags and
sha256 digests are emitted: no path, user name, timestamp or file content, so
the output can be committed or copied between machines. Moving the file is the
user's job.
"""
import hashlib
import json
import os
import sys

SCHEMA = 1
# Receipt fields that describe this machine rather than the configuration.
RECEIPT_SCALARS = ("version", "commit", "channel", "ref", "profile", "link_mode", "dirty")
SINGLE_FILES = {
    "rules_global_agents": "rules/global-AGENTS.md",
    "skills_manifest": "skills.manifest.json",
    "tools_lock": "tools.lock.json",
}
TREES = {"custom_skills": "custom-skills"}


def file_digest(path):
    if not os.path.isfile(path):
        return "missing"
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def tree_digest(root):
    if not os.path.isdir(root):
        return "missing"
    h = hashlib.sha256()
    for base, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in (".git", "__pycache__"))
        for name in sorted(files):
            path = os.path.join(base, name)
            rel = os.path.relpath(path, root).replace(os.sep, "/")
            h.update(rel.encode("utf-8") + b"\0")
            if os.path.islink(path):
                h.update(b"link:" + os.readlink(path).encode("utf-8", "replace"))
            else:
                with open(path, "rb") as fh:
                    for chunk in iter(lambda: fh.read(1024 * 1024), b""):
                        h.update(chunk)
            h.update(b"\0")
    return h.hexdigest()


def load_receipt(path):
    if not path or not os.path.isfile(path):
        return "missing", {}
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return "invalid", {}
    return ("valid", data) if isinstance(data, dict) else ("invalid", {})


def scalar(value):
    return value if isinstance(value, (str, int, float, bool)) else None


def build(root, receipt_path):
    state, receipt = load_receipt(receipt_path)
    version = ""
    try:
        with open(os.path.join(root, "VERSION"), encoding="utf-8") as fh:
            version = fh.readline().strip()
    except OSError:
        pass
    body = {"schema": SCHEMA, "receipt": state}
    body["oms_version"] = version or None
    for key in RECEIPT_SCALARS:
        body["receipt_" + key] = scalar(receipt.get(key))
    for key in ("components", "component_modes"):
        sub = receipt.get(key)
        sub = sub if isinstance(sub, dict) else {}
        body[key] = {k: scalar(sub[k]) for k in sorted(sub) if scalar(sub[k]) is not None}
    plugin = receipt.get("plugin") if isinstance(receipt.get("plugin"), dict) else {}
    body["plugin"] = {k: scalar(plugin.get(k)) for k in ("name", "version", "sha256")}
    digests = {name: file_digest(os.path.join(root, rel)) for name, rel in SINGLE_FILES.items()}
    digests.update({name: tree_digest(os.path.join(root, rel)) for name, rel in TREES.items()})
    body["digests"] = digests
    canonical = json.dumps(body, sort_keys=True, separators=(",", ":"))
    body["fingerprint"] = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
    return body


def flatten(obj, prefix=""):
    out = {}
    for key, value in obj.items():
        name = prefix + key
        if isinstance(value, dict):
            out.update(flatten(value, name + "."))
        else:
            out[name] = value
    return out


def compare(local, other):
    a, b = flatten(local), flatten(other)
    rows = []
    for key in sorted(set(a) | set(b)):
        if key == "fingerprint":
            continue
        if a.get(key, "<absent>") != b.get(key, "<absent>"):
            rows.append({"key": key, "local": a.get(key, "<absent>"), "other": b.get(key, "<absent>")})
    return rows


def main(argv):
    if len(argv) >= 4 and argv[1] == "print":
        print(json.dumps(build(argv[2], argv[3]), indent=2, sort_keys=True))
        return 0
    if len(argv) >= 5 and argv[1] == "compare":
        try:
            with open(argv[4], encoding="utf-8") as fh:
                other = json.load(fh)
            if not isinstance(other, dict) or other.get("schema") != SCHEMA or "fingerprint" not in other:
                raise ValueError("not an OMS fingerprint (schema %d)" % SCHEMA)
        except (OSError, ValueError) as exc:
            print("error: cannot read fingerprint file: %s" % exc, file=sys.stderr)
            return 2
        local = build(argv[2], argv[3])
        rows = compare(local, other)
        if "--json" in argv[5:]:
            print(json.dumps({"same": not rows, "differences": rows}, indent=2, sort_keys=True))
        elif not rows:
            print("same: configuration matches (fingerprint %s)" % local["fingerprint"][:12])
        else:
            print("differs: %d field(s); local fingerprint %s, file %s"
                  % (len(rows), local["fingerprint"][:12], str(other["fingerprint"])[:12]))
            for row in rows:
                print("- %s: local=%s file=%s" % (row["key"], row["local"], row["other"]))
        return 1 if rows else 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
