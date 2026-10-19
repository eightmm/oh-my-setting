"""Bounded, sanitized changed paths/refs for a worker guard stop, from its preserved manifests."""
import os, re, sys

root, changed = sys.argv[1:]
surfaces = {part.strip() for part in changed.split(",")}
rows = []
def read(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            return set(line.rstrip("\r\n") for line in handle if line.strip())
    except OSError:
        return set()

def safe_name(value):
    value = value.replace("\\", "/")
    if value.startswith("/") or any(part == ".." for part in value.split("/")):
        return "[redacted path]"
    if not re.fullmatch(r"[A-Za-z0-9._/@+ -]{1,180}", value):
        return "[redacted path]"
    if re.search(r"(^|[._/-])(secret|token|credential|password|\.env)([._/-]|$)", value, re.I):
        return "[redacted path]"
    return value

def metadata_name(line):
    # Names keep their spaces: only a trailing digest is split off, as the manifest producer writes it.
    kind, _, value = line.rstrip("\r\n").partition(" ")
    if not kind:
        return ""
    if kind == "worktree-dir":
        return safe_name(value) if value else ""
    if kind in {"worktree", "worktree-link", "worktree-entry", "module"}:
        return safe_name(value.rsplit(" ", 1)[0]) if value else ""
    return safe_name(kind)

def ref_name(line):
    fields = line.split()
    if len(fields) > 1 and fields[1].startswith("refs/"):
        return "ref " + safe_name(fields[1])
    if len(fields) > 1 and fields[1] == "HEAD-identity":
        return "HEAD identity"
    if fields and re.fullmatch(r"[0-9a-fA-F]{40,64}", fields[0]):
        return "HEAD"
    return ""

directories = [(root, "")]
source = os.path.join(root, "source")
if os.path.isdir(source):
    directories.append((source, "source "))
for directory, prefix in directories:
    if "gitmeta" in surfaces:
        before = read(os.path.join(directory, "gitmeta"))
        now = set()
        for name in ("gitmeta-admin-now", "gitmeta-now", "guard-report-gitmeta-now"):
            now = read(os.path.join(directory, name))
            if now:
                break
        for line in sorted(before ^ now):
            name = metadata_name(line)
            if name:
                rows.append("gitmeta " + prefix + name)
    if "refs" in surfaces:
        before = read(os.path.join(directory, "refs"))
        now = read(os.path.join(directory, "refs-now")) or read(os.path.join(directory, "guard-report-refs-now"))
        for line in sorted(before ^ now):
            name = ref_name(line)
            if name:
                rows.append("refs " + prefix + name)

if "shared-state" in surfaces:
    # Producer lines read "<rel> <phrase>"; only the path and a plain phrase survive.
    for line in sorted(read(os.path.join(root, "state-detail"))):
        rel, _, rest = line.partition(" ")
        rows.append(("shared-state " + safe_name(rel) + " " + re.sub(r"[^A-Za-z0-9 ()>-]", "", rest)[:80]).rstrip())

unique = sorted(set(rows))
if not unique:
    print("no path/ref detail available in the preserved manifests")
else:
    for row in unique[:19 if len(unique) > 20 else 20]:
        print(row)
    if len(unique) > 20:
        print("additional changed paths/refs omitted")
