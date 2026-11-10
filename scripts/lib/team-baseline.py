#!/usr/bin/env python3
"""Report-only check of a project's declared OMS baseline against this install.

Usage: team-baseline.py PROJECT_DIR OMS_ROOT RECEIPT_OR_EMPTY CAPABILITY_RECEIPT

The project declares what it expects in a tracked `.oms-team.json`:

  {"schema": 1,
   "min_oms_version": "0.7.0",
   "capabilities": ["core", "github"],
   "digests": {"custom_skills": "<sha256 from `oms status --fingerprint`>"}}

Every key but `schema` is optional. The check reads files only: it never runs a
provider CLI, never installs, never writes and never touches the network. An
unmet item prints the exact command a person could run; nothing is executed.
Exit 0 when every declared item holds, 1 when any is unmet, 2 when the
declaration cannot be read.
"""
import importlib.util
import json
import os
import re
import sys

NAME = ".oms-team.json"
KNOWN = {"schema", "min_oms_version", "capabilities", "digests"}


def version_tuple(text):
    if not isinstance(text, str) or not re.fullmatch(r"\d+(\.\d+){0,3}", text.strip()):
        raise ValueError("version must look like 1.2.3: %r" % (text,))
    return tuple(int(part) for part in text.strip().split("."))


def load_fingerprint_module(root):
    path = os.path.join(root, "scripts", "lib", "install-fingerprint.py")
    spec = importlib.util.spec_from_file_location("oms_install_fingerprint", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def installed_capabilities(path):
    try:
        with open(path, encoding="utf-8") as fh:
            row = json.load(fh)
        requested = row["plan"]["requested"]
        if row.get("schema") == 1 and isinstance(requested, list):
            return {item for item in requested if isinstance(item, str)}
    except (OSError, ValueError, KeyError, TypeError):
        pass
    return None


def main(argv):
    if len(argv) != 5:
        print(__doc__, file=sys.stderr)
        return 2
    project, root, receipt, cap_receipt = argv[1:]
    declaration = os.path.join(project, NAME)
    if not os.path.isfile(declaration):
        print("note: no %s in this project; nothing declared, nothing checked" % NAME)
        return 0
    try:
        with open(declaration, encoding="utf-8") as fh:
            want = json.load(fh)
        if not isinstance(want, dict) or want.get("schema") != 1:
            raise ValueError("schema must be 1")
        unknown = sorted(set(want) - KNOWN)
        if unknown:
            raise ValueError("unknown key(s): %s" % ", ".join(unknown))
        min_version = want.get("min_oms_version")
        if min_version is not None:
            version_tuple(min_version)
        caps = want.get("capabilities", [])
        if not isinstance(caps, list) or not all(isinstance(c, str) and c for c in caps):
            raise ValueError("capabilities must be a list of names")
        digests = want.get("digests", {})
        if not isinstance(digests, dict) or not all(
                isinstance(k, str) and isinstance(v, str) for k, v in digests.items()):
            raise ValueError("digests must map names to sha256 strings")
    except (OSError, ValueError) as exc:
        print("error: %s is not a valid baseline: %s" % (NAME, exc), file=sys.stderr)
        return 2

    unmet = 0
    fingerprint = load_fingerprint_module(root).build(root, receipt)

    if min_version is not None:
        have = fingerprint.get("oms_version")
        try:
            good = have is not None and version_tuple(have) >= version_tuple(min_version)
        except ValueError:
            good = False
        if good:
            print("ok: OMS %s >= required %s" % (have, min_version))
        else:
            unmet += 1
            print("drift: OMS %s is older than required %s; run: oms update"
                  % (have or "unknown", min_version))

    if caps:
        have_caps = installed_capabilities(cap_receipt)
        if have_caps is None:
            unmet += 1
            print("drift: capability receipt unreadable; run: oms install-profile --apply %s"
                  % " ".join("--profile " + c for c in caps))
        else:
            missing = [c for c in caps if c not in have_caps and "full" not in have_caps]
            if missing:
                unmet += 1
                print("drift: capabilities not installed: %s; run: oms install-profile --apply --add %s"
                      % (", ".join(missing), " ".join("--profile " + c for c in missing)))
            else:
                print("ok: capabilities present: %s" % ", ".join(caps))

    for name in sorted(digests):
        have = fingerprint["digests"].get(name)
        if have is None:
            unmet += 1
            print("drift: unknown digest name %s (see: oms status --fingerprint)" % name)
        elif have == digests[name]:
            print("ok: digest %s matches" % name)
        else:
            unmet += 1
            print("drift: digest %s differs (local %s, declared %s); run: oms update, or re-record "
                  "the value from a machine that has the intended version" % (name, have[:12], digests[name][:12]))

    if unmet:
        print("team-baseline: %d item(s) unmet" % unmet)
        return 1
    print("team-baseline: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
