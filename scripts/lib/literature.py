#!/usr/bin/env python3
"""Per-repo literature notes: .oms/literature.jsonl, id check, BibTeX export.

The file is append-only; for one id the last row wins. `verify` without
--online only checks identifier syntax. With --online it sends the identifier
(nothing else) to Crossref (DOI) or the arXiv API and compares title and year.
A failed lookup is `unverified`, never `verified`. Stdlib only.
"""
import argparse
import datetime
import difflib
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

DOI_RE = re.compile(r"^10\.\d{4,9}/[^\s]+$")
ARXIV_RE = re.compile(r"^(\d{4}\.\d{4,5}|[a-z\-]+(\.[A-Z]{2})?/\d{7})(v\d+)?$")
STATUSES = ("unverified", "verified", "mismatch", "not-found", "invalid-id")
CROSSREF = "https://api.crossref.org/works/"
ARXIV = "https://export.arxiv.org/api/query"
TIMEOUT = 15


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def die(message, code=2):
    print("error: %s" % message, file=sys.stderr)
    sys.exit(code)


def normalize_id(raw):
    """('doi'|'arxiv', canonical) or (None, reason)."""
    text = raw.strip()
    low = text.lower()
    for prefix in ("https://doi.org/", "http://doi.org/", "doi:"):
        if low.startswith(prefix):
            text = text[len(prefix):]
            low = text.lower()
            return normalize_doi(text)
    for prefix in ("arxiv:", "https://arxiv.org/abs/"):
        if low.startswith(prefix):
            return normalize_arxiv(text[len(prefix):])
    if low.startswith("10."):
        return normalize_doi(text)
    return normalize_arxiv(text)


def normalize_doi(text):
    text = text.strip()
    if not DOI_RE.match(text):
        return None, "not a DOI (expected 10.<registrant>/<suffix>)"
    return "doi", text.lower()


def normalize_arxiv(text):
    text = text.strip()
    if not ARXIV_RE.match(text):
        return None, "not an arXiv id (expected 2101.00001 or hep-th/9901001)"
    return "arxiv", text


def store_path(root):
    return os.environ.get("OMS_LITERATURE_FILE") or os.path.join(root, ".oms", "literature.jsonl")


def load(path):
    rows = {}
    order = []
    if not os.path.exists(path):
        return rows, order
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                die("%s line %d is not JSON" % (path, number))
            if not isinstance(row, dict) or not isinstance(row.get("id"), str):
                die("%s line %d has no id" % (path, number))
            if row["id"] not in rows:
                order.append(row["id"])
            rows[row["id"]] = row
    return rows, order


def append(path, row):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")


def norm_title(text):
    return re.sub(r"[^a-z0-9]+", " ", text.lower()).strip()


def titles_match(left, right):
    a, b = norm_title(left), norm_title(right)
    if not a or not b:
        return False
    return a == b or difflib.SequenceMatcher(None, a, b).ratio() >= 0.92


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "oms-cite/1 (stdlib)"})
    with urllib.request.urlopen(request, timeout=TIMEOUT) as response:  # noqa: S310 - explicit --online
        return response.read(2_000_000)


def lookup(kind, ident):
    """(title, year) from the registry, None when absent. Raises OSError on I/O."""
    if kind == "doi":
        base = os.environ.get("OMS_CITE_CROSSREF_URL", CROSSREF)
        try:
            body = fetch(base + urllib.parse.quote(ident, safe="/"))
        except urllib.error.HTTPError as err:
            if err.code == 404:
                return None
            raise
        message = json.loads(body.decode("utf-8", "replace")).get("message", {})
        titles = message.get("title") or [""]
        parts = ((message.get("issued") or {}).get("date-parts") or [[None]])[0]
        return titles[0], parts[0] if parts else None
    base = os.environ.get("OMS_CITE_ARXIV_URL", ARXIV)
    body = fetch(base + "?" + urllib.parse.urlencode({"id_list": ident}))
    root = ET.fromstring(body)
    ns = {"a": "http://www.w3.org/2005/Atom"}
    entry = root.find("a:entry", ns)
    if entry is None:
        return None
    title = (entry.findtext("a:title", default="", namespaces=ns) or "").strip()
    if not title or title.lower() == "error":
        return None
    published = entry.findtext("a:published", default="", namespaces=ns) or ""
    year = int(published[:4]) if published[:4].isdigit() else None
    return " ".join(title.split()), year


def verify_row(row, online):
    kind, canon = normalize_id(row["id"])
    out = dict(row)
    out["checked_at"] = now()
    if kind is None:
        out.update(status="invalid-id", detail=canon)
        return out
    if not online:
        out.update(status=row.get("status", "unverified"), detail="syntax ok; not looked up (use --online)")
        out["checked_at"] = row.get("checked_at")
        return out
    try:
        found = lookup(kind, canon)
    except (OSError, ValueError, ET.ParseError) as err:
        out.update(status="unverified", detail="lookup failed: %s" % str(err)[:120])
        return out
    if found is None:
        out.update(status="not-found", detail="registry has no record for this id")
        return out
    title, year = found
    problems = []
    if row.get("title") and not titles_match(row["title"], title):
        problems.append("title differs (registry: %s)" % title[:100])
    if row.get("year") and year and int(row["year"]) != int(year):
        problems.append("year differs (registry: %s)" % year)
    if problems:
        out.update(status="mismatch", detail="; ".join(problems))
    elif not row.get("title"):
        out.update(status="unverified", detail="id exists but no title recorded to compare")
    else:
        out.update(status="verified", detail="id exists; title and year agree")
    return out


def cmd_add(args, path):
    kind, canon = normalize_id(args.id)
    rows, _ = load(path)
    row = {"id": "%s:%s" % (kind, canon) if kind else args.id.strip(),
           "title": args.title, "claim": args.claim or "", "locator": args.locator or "",
           "status": "unverified" if kind else "invalid-id", "added_at": now(), "checked_at": None}
    if args.year is not None:
        row["year"] = args.year
    if not kind:
        row["detail"] = canon
    old = rows.get(row["id"], {})
    for key in ("title", "claim", "locator"):
        if not row[key] and old.get(key):
            row[key] = old[key]
    if "year" not in row and old.get("year"):
        row["year"] = old["year"]
    append(path, row)
    print("%s: %s (%s)" % (row["id"], row["status"], row.get("detail", "recorded")))
    return 0 if kind else 1


def cmd_list(args, path):
    rows, order = load(path)
    if args.json:
        print(json.dumps([rows[i] for i in order], ensure_ascii=False, indent=2))
        return 0
    if not order:
        print("no literature notes (%s)" % path)
        return 0
    for ident in order:
        row = rows[ident]
        print("%-12s %s | %s | %s" % (row.get("status", "unverified"), ident,
                                       row.get("year", "-"), (row.get("title") or "")[:70]))
    return 0


def cmd_verify(args, path):
    rows, order = load(path)
    wanted = args.id
    if wanted and wanted not in rows:
        kind, canon = normalize_id(wanted)
        wanted = "%s:%s" % (kind, canon) if kind else wanted
        if wanted not in rows:
            die("no such id: %s" % args.id)
    ids = [wanted] if wanted else order
    bad = 0
    for ident in ids:
        result = verify_row(rows[ident], args.online)
        if args.online:
            append(path, result)
        print("%-10s %s: %s" % (result["status"], ident, result.get("detail", "")))
        if result["status"] != "verified":
            bad += 1
    if not args.online:
        print("offline: only identifier syntax was checked; nothing is verified without --online")
    return 1 if bad else 0


def bib_escape(text):
    return re.sub(r"([{}%&$#_\\])", lambda m: "\\" + m.group(1), text)


def cmd_export(args, path):
    rows, order = load(path)
    for ident in order:
        row = rows[ident]
        kind, canon = normalize_id(ident)
        key = re.sub(r"[^A-Za-z0-9]+", "_", ident).strip("_")
        fields = [("title", row.get("title") or ""), ("year", str(row.get("year") or ""))]
        if kind == "doi":
            fields.append(("doi", canon))
        elif kind == "arxiv":
            fields.extend([("eprint", canon), ("archiveprefix", "arXiv")])
        fields.append(("note", "OMS status: %s" % row.get("status", "unverified")))
        print("@misc{%s," % key)
        for name, value in fields:
            if value:
                print("  %s = {%s}," % (name, value if name in ("doi", "eprint", "archiveprefix") else bib_escape(value)))
        print("}\n")
    return 0


def main(argv):
    parser = argparse.ArgumentParser(prog="cite")
    parser.add_argument("--root", required=True)
    sub = parser.add_subparsers(dest="cmd", required=True)
    add = sub.add_parser("add")
    add.add_argument("--id", required=True)
    add.add_argument("--title", default="")
    add.add_argument("--year", type=int)
    add.add_argument("--claim")
    add.add_argument("--locator")
    lst = sub.add_parser("list")
    lst.add_argument("--json", action="store_true")
    ver = sub.add_parser("verify")
    ver.add_argument("--id")
    ver.add_argument("--online", action="store_true")
    exp = sub.add_parser("export")
    exp.add_argument("--bibtex", action="store_true", required=True)
    args = parser.parse_args(argv)
    path = store_path(args.root)
    return {"add": cmd_add, "list": cmd_list, "verify": cmd_verify, "export": cmd_export}[args.cmd](args, path)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
