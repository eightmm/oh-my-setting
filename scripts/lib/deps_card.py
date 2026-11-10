#!/usr/bin/env python3
"""Which version of a third-party package this project actually uses, and where its documentation lives.

Reads lockfiles and the installed package's own metadata file. Nothing is
executed (no interpreter of the project is started, no package is imported)
and nothing is fetched: URLs are printed as the package declared them. The
answer is "what is pinned and what is installed here", not "what is latest".
"""

from __future__ import annotations

import argparse
import email.parser
import json
import os
import re
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple
from urllib.parse import urlsplit, urlunsplit

MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_ROWS = 40
NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
REQUIREMENT_RE = re.compile(r"^\s*([A-Za-z0-9][A-Za-z0-9._-]*)\s*(?:\[[^\]]*\])?\s*(==|===)\s*([A-Za-z0-9][A-Za-z0-9.!+_*-]*)")
DECLARED_RE = re.compile(r"^\s*[\"']?([A-Za-z0-9][A-Za-z0-9._-]*)\s*(?:\[[^\]]*\])?\s*((?:[<>=!~]=?|===?)[^\"',;#]*)")
CONDA_RE = re.compile(r"^\s*-\s*(?:[A-Za-z0-9_.-]+::)?([A-Za-z0-9][A-Za-z0-9._-]*)\s*={1,2}\s*([A-Za-z0-9][A-Za-z0-9.!+_*-]*)")


def canonical(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def _read(path: Path) -> Optional[str]:
    try:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_FILE_BYTES:
            return None
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


def _toml_packages(text: str) -> Optional[List[Tuple[str, str]]]:
    try:
        import tomllib
    except ImportError:  # Python older than 3.11: say so instead of guessing with a regex
        return None
    try:
        data = tomllib.loads(text)
    except (tomllib.TOMLDecodeError, RecursionError):
        return []
    rows = []
    for item in data.get("package", []) if isinstance(data.get("package"), list) else []:
        if isinstance(item, dict) and isinstance(item.get("name"), str) and isinstance(item.get("version"), str):
            rows.append((item["name"], item["version"]))
    return rows


def lock_rows(repo: Path, wanted: str) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]], List[str]]:
    """(pinned, declared, notes). Only files at the repository root are read."""
    pinned: List[Dict[str, Any]] = []
    declared: List[Dict[str, Any]] = []
    notes: List[str] = []
    try:
        names = sorted(entry.name for entry in repo.iterdir())
    except OSError:
        names = []
    for name in names:
        lower = name.lower()
        path = repo / name
        if re.fullmatch(r"requirements[A-Za-z0-9._-]*\.txt|constraints[A-Za-z0-9._-]*\.txt", lower):
            text = _read(path)
            for number, line in enumerate((text or "").splitlines(), 1):
                match = REQUIREMENT_RE.match(line)
                if match and canonical(match.group(1)) == wanted:
                    pinned.append({"file": name, "line": number, "version": match.group(3)})
                elif not match:
                    loose = DECLARED_RE.match(line)
                    if loose and canonical(loose.group(1)) == wanted:
                        declared.append({"file": name, "line": number, "spec": loose.group(2).strip()})
        elif lower in ("uv.lock", "poetry.lock", "cargo.lock", "pdm.lock"):
            text = _read(path)
            rows = _toml_packages(text) if text is not None else []
            if rows is None:
                notes.append("%s not read: this Python has no tomllib (3.11+)" % name)
                continue
            pinned.extend({"file": name, "version": version} for package, version in rows if canonical(package) == wanted)
        elif lower == "pipfile.lock":
            try:
                data = json.loads(_read(path) or "{}")
            except ValueError:
                data = {}
            for section in ("default", "develop"):
                for package, row in (data.get(section) or {}).items() if isinstance(data.get(section), dict) else []:
                    version = row.get("version") if isinstance(row, dict) else None
                    if canonical(str(package)) == wanted and isinstance(version, str):
                        pinned.append({"file": name, "section": section, "version": version.lstrip("=")})
        elif lower == "package-lock.json":
            try:
                data = json.loads(_read(path) or "{}")
            except ValueError:
                data = {}
            packages = data.get("packages") if isinstance(data.get("packages"), dict) else {}
            for key, row in packages.items():
                if isinstance(row, dict) and str(key).rsplit("node_modules/", 1)[-1].lower() == wanted \
                        and isinstance(row.get("version"), str):
                    pinned.append({"file": name, "path": str(key)[:200], "version": row["version"]})
        elif re.fullmatch(r"environment[A-Za-z0-9._-]*\.ya?ml", lower):
            for number, line in enumerate((_read(path) or "").splitlines(), 1):
                match = CONDA_RE.match(line)
                if match and canonical(match.group(1)) == wanted:
                    pinned.append({"file": name, "line": number, "version": match.group(2)})
        elif lower == "pyproject.toml":
            for number, line in enumerate((_read(path) or "").splitlines(), 1):
                match = DECLARED_RE.match(line)
                if match and canonical(match.group(1)) == wanted and not line.lstrip().startswith(("name", "version")):
                    declared.append({"file": name, "line": number, "spec": match.group(2).strip()})
    return pinned[:MAX_ROWS], declared[:MAX_ROWS], notes


def _environments(repo: Path) -> List[Tuple[str, Path]]:
    found: List[Tuple[str, Path]] = []
    for label, base in (("project .venv", repo / ".venv"), ("project venv", repo / "venv"),
                        ("VIRTUAL_ENV", Path(os.environ["VIRTUAL_ENV"]) if os.environ.get("VIRTUAL_ENV") else None),
                        ("CONDA_PREFIX", Path(os.environ["CONDA_PREFIX"]) if os.environ.get("CONDA_PREFIX") else None)):
        if base is None:
            continue
        try:
            for site in sorted(base.glob("lib/python*/site-packages")):
                if site.is_dir() and all(site != other for _label, other in found):
                    found.append((label, site))
        except OSError:
            continue
    return found


def _clean_url(value: str) -> str:
    """A declared URL without credentials, query or fragment; "" when it is not http(s)."""
    try:
        parts = urlsplit(value.strip())
    except ValueError:
        return ""
    if parts.scheme not in ("http", "https") or not parts.hostname:
        return ""
    host = parts.hostname + (":%d" % parts.port if parts.port else "")
    return urlunsplit((parts.scheme, host, parts.path, "", ""))[:300]


def installed_rows(repo: Path, wanted: str) -> List[Dict[str, Any]]:
    rows: List[Dict[str, Any]] = []
    home = str(Path.home())
    for label, site in _environments(repo):
        try:
            candidates = sorted(entry for entry in site.iterdir() if entry.name.endswith(".dist-info"))
        except OSError:
            continue
        for entry in candidates:
            if canonical(entry.name[:-len(".dist-info")].rsplit("-", 1)[0]) != wanted:
                continue
            text = _read(entry / "METADATA")
            if text is None:
                continue
            meta = email.parser.HeaderParser().parsestr(text)
            urls = []
            for raw in [("Home-page, " + meta["Home-page"]) if meta.get("Home-page") else ""] + (meta.get_all("Project-URL") or []):
                kind, _, url = raw.partition(",")
                url = _clean_url(url)
                if url and (kind.strip()[:40], url) not in urls:
                    urls.append((kind.strip()[:40], url))
            location = str(site)
            if location.startswith(home + os.sep):
                location = "~" + location[len(home):]
            try:
                location = "./" + str(site.relative_to(repo))
            except ValueError:
                pass
            rows.append({"environment": label, "version": str(meta.get("Version", ""))[:80],
                         "summary": " ".join(str(meta.get("Summary", "")).split())[:200],
                         "requires_python": str(meta.get("Requires-Python", ""))[:80],
                         "location": location, "urls": [{"kind": kind, "url": url} for kind, url in urls[:12]]})
    return rows[:MAX_ROWS]


def card(repo: Path, name: str) -> Dict[str, Any]:
    wanted = canonical(name)
    pinned, declared, notes = lock_rows(repo, wanted)
    installed = installed_rows(repo, wanted)
    pins = sorted({row["version"] for row in pinned})
    have = sorted({row["version"] for row in installed if row["version"]})
    if len(pins) > 1:
        notes.append("lockfiles disagree: %s" % ", ".join(pins))
    if pins and have and not set(pins) & set(have):
        notes.append("installed %s is not a pinned version (%s)" % (", ".join(have), ", ".join(pins)))
    if not installed:
        notes.append("no installed copy found in ./.venv, ./venv, VIRTUAL_ENV or CONDA_PREFIX (Python packages only; nothing was imported)")
    return {"schema": 1, "kind": "oms-dependency-card", "name": name, "canonical": wanted,
            "pinned": pinned, "declared": declared, "installed": installed, "notes": notes,
            "fetched": False}


def card_text(value: Dict[str, Any]) -> str:
    lines = ["dependency %s: what this project pins and has installed (nothing fetched, nothing imported)" % value["name"]]
    for row in value["pinned"]:
        where = row["file"] + (":%d" % row["line"] if "line" in row else "")
        lines.append("  pinned     %s  (%s)" % (row["version"], where))
    for row in value["declared"]:
        lines.append("  declared   %s  (%s:%d)" % (row["spec"], row["file"], row["line"]))
    for row in value["installed"]:
        lines.append("  installed  %s  (%s, %s)" % (row["version"] or "?", row["environment"], row["location"]))
        if row["summary"]:
            lines.append("             %s" % row["summary"])
        for url in row["urls"]:
            lines.append("             %s: %s" % (url["kind"] or "url", url["url"]))
    if not (value["pinned"] or value["declared"] or value["installed"]):
        lines.append("  not found in the root lockfiles or an installed environment")
    lines.extend("  note: %s" % note for note in value["notes"])
    return "\n".join(lines) + "\n"


def main(argv: List[str]) -> int:
    parser = argparse.ArgumentParser(prog="oms deps", description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    show = sub.add_parser("show", help="pinned, declared and installed versions of one package, with its declared URLs")
    show.add_argument("name")
    show.add_argument("--repo", default=".")
    show.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if not NAME_RE.match(args.name):
        print("deps: package name must match [A-Za-z0-9][A-Za-z0-9._-]*", file=sys.stderr)
        return 2
    repo = Path(args.repo).resolve()
    if not repo.is_dir():
        print("deps: --repo is not a directory", file=sys.stderr)
        return 2
    value = card(repo, args.name)
    if args.json:
        print(json.dumps(value, ensure_ascii=False, sort_keys=True))
    else:
        sys.stdout.write(card_text(value))
    return 0 if value["pinned"] or value["declared"] or value["installed"] else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
