#!/usr/bin/env python3
"""SQLite index for append-only agent memory Markdown logs."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import re
import sqlite3
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Optional


SCHEMA_VERSION = 3
HEADER_RE = re.compile(r"^## (\S+) (.*)$")
PIN_RE = re.compile(r"^- (\S+) \[([^\]]+)\] (.*)$")
TOKEN_RE = re.compile(r"[^\W_]+", re.UNICODE)
EVENT_ID_RE = re.compile(r"^[A-Za-z0-9._:-]{1,160}$")
METADATA_START = "<!-- oms-memory"
METADATA_END = "-->"
ENTRY_FIELDS = (
    "event_id",
    "source",
    "ordinal",
    "occurred_at",
    "agent",
    "kind",
    "task_id",
    "session_hash",
    "git_sha",
    "git_dirty",
    "git_state",
    "body",
    "rendered",
)


@dataclass(frozen=True)
class Entry:
    event_id: str
    source: str
    ordinal: int
    occurred_at: str
    agent: str
    kind: str
    task_id: str
    session_hash: str
    git_sha: str
    git_dirty: Optional[int]
    git_state: str
    body: str
    rendered: str


@dataclass(frozen=True)
class Snapshot:
    path: str
    size: int
    mtime_ns: int
    digest: str
    text: str


def stable_snapshot(path: str) -> Snapshot:
    """Read one stable source image while another agent may be appending."""
    if not os.path.exists(path):
        return Snapshot(path, -1, 0, "", "")
    for _ in range(4):
        before = os.stat(path)
        data = Path(path).read_bytes()
        after = os.stat(path)
        before_ns = getattr(before, "st_mtime_ns", int(before.st_mtime * 1_000_000_000))
        after_ns = getattr(after, "st_mtime_ns", int(after.st_mtime * 1_000_000_000))
        if (
            before.st_size == after.st_size == len(data)
            and before_ns == after_ns
        ):
            return Snapshot(
                os.path.realpath(path),
                len(data),
                after_ns,
                hashlib.sha256(data).hexdigest(),
                data.decode("utf-8", errors="replace"),
            )
    raise RuntimeError("memory source kept changing while it was being indexed: %s" % path)


def legacy_event_id(source: str, ordinal: int, rendered: str) -> str:
    digest = hashlib.sha256(rendered.encode("utf-8")).hexdigest()[:16]
    return "legacy:%s:%d:%s" % (source, ordinal, digest)


def make_entry(
    source: str,
    ordinal: int,
    occurred_at: str,
    agent: str,
    body: str,
    rendered: str,
    metadata: dict[str, str],
) -> Entry:
    event_id = metadata.get("event_id", "")
    if not EVENT_ID_RE.fullmatch(event_id):
        event_id = legacy_event_id(source, ordinal, rendered)
    kind = metadata.get("kind", "") or ("pin" if source == "pins" else "note")
    dirty_text = metadata.get("git_dirty", "")
    git_dirty = int(dirty_text) if dirty_text in ("0", "1") else None
    return Entry(
        event_id=event_id,
        source=source,
        ordinal=ordinal,
        occurred_at=occurred_at,
        agent=agent.strip(),
        kind=kind,
        task_id=metadata.get("task_id", ""),
        session_hash=metadata.get("session_hash", ""),
        git_sha=metadata.get("git_sha", ""),
        git_dirty=git_dirty,
        git_state=metadata.get("git_state", ""),
        body=body,
        rendered=rendered,
    )


def parse_metadata_line(line: str, metadata: dict[str, str]) -> None:
    key, separator, value = line.partition(":")
    if separator:
        metadata[key.strip()] = value.strip()


def parse_shared(text: str) -> list[Entry]:
    entries: list[Entry] = []
    occurred_at = ""
    agent = ""
    body_lines: list[str] = []
    entry_metadata: dict[str, str] = {}
    pending_metadata: dict[str, str] = {}
    metadata_block: Optional[dict[str, str]] = None

    def finish() -> None:
        nonlocal occurred_at, agent, body_lines, entry_metadata
        if not occurred_at:
            return
        body = "\n".join(body_lines).strip()
        rendered = "## %s %s" % (occurred_at, agent)
        if body:
            rendered += "\n\n" + body
        entries.append(
            make_entry(
                "shared",
                len(entries),
                occurred_at,
                agent,
                body,
                rendered,
                entry_metadata,
            )
        )
        occurred_at = ""
        agent = ""
        body_lines = []
        entry_metadata = {}

    for line in text.splitlines():
        if line.strip() == METADATA_START:
            finish()
            metadata_block = {}
            continue
        if metadata_block is not None:
            if line.strip() == METADATA_END:
                pending_metadata = metadata_block
                metadata_block = None
            else:
                parse_metadata_line(line, metadata_block)
            continue
        match = HEADER_RE.match(line)
        if match:
            finish()
            occurred_at = match.group(1)
            agent = match.group(2)
            body_lines = []
            entry_metadata = pending_metadata
            pending_metadata = {}
        elif occurred_at:
            body_lines.append(line)
    finish()
    return entries


def parse_pins(text: str) -> list[Entry]:
    entries: list[Entry] = []
    pending_metadata: dict[str, str] = {}
    metadata_block: Optional[dict[str, str]] = None
    for line in text.splitlines():
        if line.strip() == METADATA_START:
            metadata_block = {}
            continue
        if metadata_block is not None:
            if line.strip() == METADATA_END:
                pending_metadata = metadata_block
                metadata_block = None
            else:
                parse_metadata_line(line, metadata_block)
            continue
        match = PIN_RE.match(line)
        if not match:
            continue
        entries.append(
            make_entry(
                "pins",
                len(entries),
                match.group(1),
                match.group(2),
                match.group(3),
                line,
                pending_metadata,
            )
        )
        pending_metadata = {}
    return entries


def parse_failures(text: str) -> list[Entry]:
    """Index the failure ledger. Exact-fingerprint `check --cmd` answers "this
    same command failed"; indexing the rows makes the softer question — have we
    had trouble with this before, and what did it say — answerable too.

    Two passes, because a resolution is a separate row that carries only the
    fingerprint it clears. Recalling a fixed failure as though it still stood
    is worse than not recalling it: the reader treats a solved problem as an
    open one. The ledger is append-only and chronological, so the last event
    for a fingerprint is its current state and a later failure re-opens it.
    Field names here follow fail-ledger.sh, which writes exactly two events
    (`fail`, `resolved`) and stores the git state as `state_fingerprint`."""
    rows: list[dict] = []
    resolved: dict[str, bool] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if not isinstance(row, dict):
            continue
        fingerprint = str(row.get("fingerprint") or "")
        if str(row.get("event") or "") == "resolved":
            if fingerprint:
                resolved[fingerprint] = True
            continue
        if fingerprint:
            resolved[fingerprint] = False
        rows.append(row)

    entries: list[Entry] = []
    for row in rows:
        command = str(row.get("cmd") or "")
        summary = str(row.get("summary") or "")
        fingerprint = str(row.get("fingerprint") or "")
        exit_code = row.get("exit")
        parts = [part for part in (summary, command) if part]
        body = " | ".join(parts)
        if exit_code is not None:
            body = "%s (exit %s)" % (body, exit_code) if body else "exit %s" % exit_code
        if not body:
            continue
        # The ledger kind separates the project's own verification gate from an
        # arbitrary command, which is the difference between "the contract
        # broke" and "some command failed once".
        kind = str(row.get("kind") or "")
        if kind and kind != "cmd":
            body = "%s failure: %s" % (kind, body)
        is_resolved = resolved.get(fingerprint, False)
        if is_resolved:
            body = "resolved — %s" % body
        metadata = {
            "kind": "failure-resolved" if is_resolved else "failure",
            "task_id": str(row.get("task") or row.get("task_id") or ""),
            "git_state": str(row.get("state_fingerprint") or ""),
        }
        if EVENT_ID_RE.fullmatch("fail:%s:%d" % (fingerprint, len(entries))):
            metadata["event_id"] = "fail:%s:%d" % (fingerprint, len(entries))
        entries.append(
            make_entry(
                "failures",
                len(entries),
                str(row.get("ts") or row.get("timestamp") or ""),
                str(row.get("agent") or ""),
                body,
                "## %s %s\n\n%s" % (
                    row.get("ts") or "", row.get("agent") or "", body),
                metadata,
            )
        )
    return entries


def connect(path: str) -> sqlite3.Connection:
    parent = os.path.dirname(path)
    if parent:
        os.makedirs(parent, exist_ok=True)
    db = sqlite3.connect(path, timeout=10)
    db.execute("pragma busy_timeout = 10000")
    db.execute("pragma foreign_keys = on")
    return db


def migrate_schema_one(db: sqlite3.Connection) -> None:
    columns = {
        row[1] for row in db.execute("pragma table_info(memory_entries)")
    }
    additions = (
        ("event_id", "text not null default ''"),
        ("kind", "text not null default ''"),
        ("task_id", "text not null default ''"),
        ("session_hash", "text not null default ''"),
        ("git_sha", "text not null default ''"),
        ("git_dirty", "integer"),
        ("git_state", "text not null default ''"),
    )
    for name, declaration in additions:
        if name not in columns:
            db.execute(
                "alter table memory_entries add column %s %s" % (name, declaration)
            )
    rows = db.execute(
        "select id, source, ordinal, rendered, event_id, kind from memory_entries"
    ).fetchall()
    for row_id, source, ordinal, rendered, event_id, kind in rows:
        if not event_id:
            event_id = legacy_event_id(source, ordinal, rendered)
        if not kind:
            kind = "pin" if source == "pins" else "note"
        db.execute(
            "update memory_entries set event_id = ?, kind = ? where id = ?",
            (event_id, kind, row_id),
        )
    # A schema-one source snapshot may look current even though its rows have
    # only migration defaults. Force the next sync to reparse canonical
    # Markdown and recover any provenance comments already present there.
    db.execute("delete from memory_sources")
    db.execute("pragma user_version = 2")
    db.commit()


def migrate_to_three(db: sqlite3.Connection) -> None:
    """A CHECK constraint cannot be altered in place. Every row here is derived
    from the Markdown logs and the JSONL ledger, so dropping and re-deriving
    loses nothing that is not still on disk."""
    db.execute("drop trigger if exists memory_entries_ai")
    db.execute("drop trigger if exists memory_entries_ad")
    db.execute("drop trigger if exists memory_entries_au")
    db.execute("drop table if exists memory_fts")
    db.execute("drop table if exists memory_entries")
    db.execute("drop table if exists memory_sources")
    db.execute("pragma user_version = 3")
    db.commit()


def ensure_schema(db: sqlite3.Connection) -> None:
    version = db.execute("pragma user_version").fetchone()[0]
    migrated = version == 1
    if version == 2:
        migrate_to_three(db)
        version = 3
    if version not in (0, 1, SCHEMA_VERSION):
        raise RuntimeError(
            "unsupported memory database schema %d (expected %d)"
            % (version, SCHEMA_VERSION)
        )
    if version == 1:
        migrate_schema_one(db)
        migrate_to_three(db)
    db.executescript(
        """
        create table if not exists memory_sources (
          source text primary key check (source in ('shared', 'pins', 'failures')),
          path text not null,
          size integer not null,
          mtime_ns integer not null,
          digest text not null
        );
        create table if not exists memory_entries (
          id integer primary key,
          event_id text not null unique,
          source text not null check (source in ('shared', 'pins', 'failures')),
          ordinal integer not null,
          occurred_at text not null,
          agent text not null,
          kind text not null,
          task_id text not null,
          session_hash text not null,
          git_sha text not null,
          git_dirty integer check (git_dirty is null or git_dirty in (0, 1)),
          git_state text not null,
          body text not null,
          rendered text not null,
          unique (source, ordinal)
        );
        create index if not exists memory_entries_agent
          on memory_entries (agent);
        create index if not exists memory_entries_time
          on memory_entries (occurred_at);
        create unique index if not exists memory_entries_event
          on memory_entries (event_id);
        create index if not exists memory_entries_task
          on memory_entries (task_id);
        create index if not exists memory_entries_git_sha
          on memory_entries (git_sha);
        -- Access stats live OUTSIDE memory_entries: entry rows are re-derived
        -- from Markdown (and dropped wholesale on schema migration), while
        -- what was recalled and when must survive that. Keyed by the stable
        -- content-derived event_id. An explicit `rebuild` starts a fresh file
        -- and forfeits these stats; recall quality degrades gracefully.
        create table if not exists memory_access (
          event_id text primary key,
          last_accessed text not null,
          hit_count integer not null default 0
        );
        """
    )
    try:
        db.executescript(
            """
            create virtual table if not exists memory_fts using fts5(
              rendered,
              agent,
              content='memory_entries',
              content_rowid='id',
              tokenize='unicode61'
            );
            create trigger if not exists memory_entries_ai after insert
            on memory_entries begin
              insert into memory_fts(rowid, rendered, agent)
              values (new.id, new.rendered, new.agent);
            end;
            create trigger if not exists memory_entries_ad after delete
            on memory_entries begin
              insert into memory_fts(memory_fts, rowid, rendered, agent)
              values ('delete', old.id, old.rendered, old.agent);
            end;
            create trigger if not exists memory_entries_au after update
            on memory_entries begin
              insert into memory_fts(memory_fts, rowid, rendered, agent)
              values ('delete', old.id, old.rendered, old.agent);
              insert into memory_fts(rowid, rendered, agent)
              values (new.id, new.rendered, new.agent);
            end;
            """
        )
        if migrated:
            db.execute("insert into memory_fts(memory_fts) values ('rebuild')")
            db.commit()
    except sqlite3.OperationalError as error:
        if "fts5" not in str(error).lower():
            raise
    db.execute("pragma user_version = %d" % SCHEMA_VERSION)


def source_is_current(
    db: sqlite3.Connection,
    source: str,
    path: str,
    stat_result: Optional[os.stat_result],
) -> bool:
    row = db.execute(
        "select path, size, mtime_ns from memory_sources where source = ?", (source,)
    ).fetchone()
    if row is None:
        return False
    if stat_result is None:
        return row == (path, -1, 0)
    mtime_ns = getattr(
        stat_result,
        "st_mtime_ns",
        int(stat_result.st_mtime * 1_000_000_000),
    )
    return row == (os.path.realpath(path), stat_result.st_size, mtime_ns)


def replace_source(
    db: sqlite3.Connection, source: str, snapshot: Snapshot, entries: Iterable[Entry]
) -> None:
    db.execute("delete from memory_entries where source = ?", (source,))
    db.executemany(
        """
        insert into memory_entries
          (event_id, source, ordinal, occurred_at, agent, kind, task_id,
           session_hash, git_sha, git_dirty, git_state, body, rendered)
        values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        [
            (
                entry.event_id,
                entry.source,
                entry.ordinal,
                entry.occurred_at,
                entry.agent,
                entry.kind,
                entry.task_id,
                entry.session_hash,
                entry.git_sha,
                entry.git_dirty,
                entry.git_state,
                entry.body,
                entry.rendered,
            )
            for entry in entries
        ],
    )
    db.execute(
        """
        insert into memory_sources (source, path, size, mtime_ns, digest)
        values (?, ?, ?, ?, ?)
        on conflict(source) do update set
          path=excluded.path,
          size=excluded.size,
          mtime_ns=excluded.mtime_ns,
          digest=excluded.digest
        """,
        (
            source,
            snapshot.path,
            snapshot.size,
            snapshot.mtime_ns,
            snapshot.digest,
        ),
    )


def sync_sources(
    db: sqlite3.Connection, shared: str, pins: str, failures: str = ""
) -> None:
    ensure_schema(db)
    db.execute("begin immediate")
    try:
        sources = [
            ("shared", shared, parse_shared),
            ("pins", pins, parse_pins),
        ]
        if failures:
            sources.append(("failures", failures, parse_failures))
        for source, path, parser in sources:
            try:
                stat_result = os.stat(path)
            except FileNotFoundError:
                stat_result = None
            if source_is_current(db, source, path, stat_result):
                continue
            snapshot = stable_snapshot(path)
            replace_source(db, source, snapshot, parser(snapshot.text))
        db.commit()
    except Exception:
        db.rollback()
        raise


def has_fts(db: sqlite3.Connection) -> bool:
    return (
        db.execute(
            "select 1 from sqlite_master where type = 'table' and name = 'memory_fts'"
        ).fetchone()
        is not None
    )


def failure_health(text: str) -> dict[str, int]:
    valid_rows = 0
    invalid_rows = 0
    states: dict[str, bool] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except ValueError:
            invalid_rows += 1
            continue
        if not isinstance(row, dict):
            invalid_rows += 1
            continue
        valid_rows += 1
        fingerprint = str(row.get("fingerprint") or "")
        event = str(row.get("event") or "")
        if fingerprint and event == "resolved":
            states[fingerprint] = True
        elif fingerprint and event == "fail":
            states[fingerprint] = False
    return {
        "valid_rows": valid_rows,
        "invalid_rows": invalid_rows,
        "open_fingerprints": sum(not resolved for resolved in states.values()),
        "resolved_fingerprints": sum(resolved for resolved in states.values()),
    }


def connect_readonly(path: str) -> sqlite3.Connection:
    uri = Path(path).resolve().as_uri() + "?mode=ro"
    db = sqlite3.connect(uri, uri=True, timeout=10)
    db.execute("pragma busy_timeout = 10000")
    db.execute("pragma query_only = on")
    return db


def default_index_health(present: bool) -> dict:
    return {
        "present": present,
        "readable": False,
        "current": False,
        "schema_version": None,
        "expected_schema": SCHEMA_VERSION,
        "integrity": "not-run",
        "fts": False,
        "fts_entries": None,
        "fts_current": None,
        "entries": 0,
        "by_source": {"shared": 0, "pins": 0, "failures": 0},
        "provenance": {
            "total": 0,
            "modern": 0,
            "legacy": 0,
            "task_id": 0,
            "session_hash": 0,
            "git_sha": 0,
            "git_dirty": 0,
            "git_state": 0,
        },
        "problems": [],
    }


def memory_health(
    db_path: str, shared: str, pins: str, failures: str
) -> tuple[dict, int]:
    source_specs = (
        ("shared", shared, parse_shared),
        ("pins", pins, parse_pins),
        ("failures", failures, parse_failures),
    )
    snapshots: dict[str, Snapshot] = {}
    source_report: dict[str, dict[str, object]] = {}
    for source, path, parser in source_specs:
        snapshot = stable_snapshot(path)
        snapshots[source] = snapshot
        source_report[source] = {
            "present": snapshot.size >= 0,
            "bytes": max(snapshot.size, 0),
            "entries": len(parser(snapshot.text)),
        }

    failure_report = failure_health(snapshots["failures"].text)
    index_present = os.path.exists(db_path)
    index_report = default_index_health(index_present)
    total_source_entries = sum(
        int(report["entries"]) for report in source_report.values()
    )
    report = {
        "schema": 1,
        "action": "health",
        "assessment": "index-health-not-memory-quality",
        "status": "empty",
        "sources": source_report,
        "failures": failure_report,
        "index": index_report,
        "problems": [],
    }
    if failure_report["invalid_rows"]:
        report["problems"].append("failure-ledger-invalid")
    if not index_present:
        if report["problems"]:
            report["status"] = "degraded"
            return report, 1
        if total_source_entries or failure_report["valid_rows"]:
            report["status"] = "missing"
            index_report["problems"].append("index-missing")
            return report, 1
        return report, 0

    db: Optional[sqlite3.Connection] = None
    try:
        db = connect_readonly(db_path)
        index_report["readable"] = True
        index_report["integrity"] = str(
            db.execute("pragma integrity_check").fetchone()[0]
        )
        version = int(db.execute("pragma user_version").fetchone()[0])
        index_report["schema_version"] = version
        tables = {
            str(row[0])
            for row in db.execute(
                "select name from sqlite_master where type in ('table', 'view')"
            )
        }
        required_tables = {"memory_sources", "memory_entries"}
        missing_tables = sorted(required_tables - tables)
        if missing_tables:
            index_report["problems"].append("required-tables-missing")

        required_columns = {
            "event_id",
            "source",
            "ordinal",
            "occurred_at",
            "agent",
            "kind",
            "task_id",
            "session_hash",
            "git_sha",
            "git_dirty",
            "git_state",
            "body",
            "rendered",
        }
        columns: set[str] = set()
        if "memory_entries" in tables:
            columns = {
                str(row[1]) for row in db.execute("pragma table_info(memory_entries)")
            }
            if not required_columns.issubset(columns):
                index_report["problems"].append("required-columns-missing")

        source_rows: dict[str, tuple[object, ...]] = {}
        if "memory_sources" in tables:
            source_rows = {
                str(row[0]): tuple(row[1:])
                for row in db.execute(
                    "select source, path, size, mtime_ns, digest from memory_sources"
                )
            }
        source_current = True
        for source, snapshot in snapshots.items():
            expected = (
                snapshot.path,
                snapshot.size,
                snapshot.mtime_ns,
                snapshot.digest,
            )
            if source_rows.get(source) != expected:
                source_current = False
        index_report["current"] = source_current
        if not source_current:
            index_report["problems"].append("source-index-mismatch")

        if required_columns.issubset(columns):
            index_report["entries"] = int(
                db.execute("select count(*) from memory_entries").fetchone()[0]
            )
            by_source = {
                str(source): int(count)
                for source, count in db.execute(
                    "select source, count(*) from memory_entries group by source"
                )
            }
            index_report["by_source"] = {
                source: by_source.get(source, 0)
                for source in ("shared", "pins", "failures")
            }
            provenance_row = db.execute(
                """
                select
                  count(*),
                  sum(case when event_id like 'legacy:%' then 1 else 0 end),
                  sum(case when task_id != '' then 1 else 0 end),
                  sum(case when session_hash != '' then 1 else 0 end),
                  sum(case when git_sha != '' then 1 else 0 end),
                  sum(case when git_dirty is not null then 1 else 0 end),
                  sum(case when git_state != '' then 1 else 0 end)
                from memory_entries
                """
            ).fetchone()
            values = [int(value or 0) for value in provenance_row]
            (
                total,
                legacy,
                task_count,
                session_count,
                sha_count,
                dirty_count,
                state_count,
            ) = values
            index_report["provenance"] = {
                "total": total,
                "modern": total - legacy,
                "legacy": legacy,
                "task_id": task_count,
                "session_hash": session_count,
                "git_sha": sha_count,
                "git_dirty": dirty_count,
                "git_state": state_count,
            }

        index_report["fts"] = "memory_fts" in tables
        if index_report["fts"]:
            fts_entries = int(
                db.execute("select count(*) from memory_fts").fetchone()[0]
            )
            index_report["fts_entries"] = fts_entries
            index_report["fts_current"] = fts_entries == index_report["entries"]
            if not index_report["fts_current"]:
                index_report["problems"].append("fts-entry-mismatch")

        if version != SCHEMA_VERSION:
            index_report["problems"].append("schema-version-mismatch")
        if index_report["integrity"] != "ok":
            index_report["problems"].append("integrity-check-failed")
    except (OSError, sqlite3.DatabaseError):
        index_report["problems"].append("database-unreadable")
        report["status"] = "degraded"
        return report, 1
    finally:
        if db is not None:
            db.close()

    degrading = {
        "required-tables-missing",
        "required-columns-missing",
        "fts-entry-mismatch",
        "schema-version-mismatch",
        "integrity-check-failed",
    }
    if report["problems"] or degrading.intersection(index_report["problems"]):
        report["status"] = "degraded"
        return report, 1
    if not index_report["current"]:
        report["status"] = "stale"
        return report, 1
    report["status"] = "healthy"
    return report, 0


def emit_health(report: dict, as_json: bool) -> None:
    if as_json:
        sys.stdout.write(json.dumps(report, ensure_ascii=False, sort_keys=True) + "\n")
        return
    sources = report["sources"]
    index = report["index"]
    provenance = index["provenance"]
    sys.stdout.write("memory health: %s\n" % report["status"])
    sys.stdout.write(
        "sources: shared=%d pins=%d failures=%d invalid-ledger-rows=%d\n"
        % (
            sources["shared"]["entries"],
            sources["pins"]["entries"],
            sources["failures"]["entries"],
            report["failures"]["invalid_rows"],
        )
    )
    if index["present"]:
        sys.stdout.write(
            "index: current=%s schema=%s/%d integrity=%s fts=%s entries=%d\n"
            % (
                "yes" if index["current"] else "no",
                index["schema_version"],
                index["expected_schema"],
                index["integrity"],
                "yes" if index["fts"] else "fallback",
                index["entries"],
            )
        )
        sys.stdout.write(
            "provenance: modern=%d legacy=%d task=%d session=%d git-sha=%d\n"
            % (
                provenance["modern"],
                provenance["legacy"],
                provenance["task_id"],
                provenance["session_hash"],
                provenance["git_sha"],
            )
        )
    else:
        sys.stdout.write("index: absent\n")
    problems = list(report["problems"]) + list(index["problems"])
    if problems:
        sys.stdout.write("problems: %s\n" % ", ".join(problems))
    sys.stdout.write("assessment: index health only; memory quality not evaluated\n")


def select_fields(alias: str = "") -> str:
    prefix = alias + "." if alias else ""
    return ", ".join(prefix + field for field in ENTRY_FIELDS)


def emit(entries: Iterable[tuple[object, ...]], as_json: bool = False) -> int:
    shown = 0
    for row in entries:
        values = dict(zip(ENTRY_FIELDS, row))
        rendered = str(values.pop("rendered"))
        if as_json:
            if values["git_dirty"] is not None:
                values["git_dirty"] = bool(values["git_dirty"])
            payload = {"schema": SCHEMA_VERSION, **values}
            sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
        else:
            sys.stdout.write(rendered.rstrip() + "\n\n")
        shown += 1
    return shown


def exact_search(
    db: sqlite3.Connection, query: str, agent: str, limit: int
) -> list[tuple[object, ...]]:
    rows = db.execute(
        """
        select %s
        from memory_entries as entries
        where (? = '' or agent = ?)
        order by case source when 'shared' then 0 else 1 end, ordinal
        """
        % select_fields("entries"),
        (agent, agent),
    ).fetchall()
    folded = query.casefold()
    rendered_index = ENTRY_FIELDS.index("rendered")
    matches = [row for row in rows if folded in str(row[rendered_index]).casefold()]
    return matches if limit == 0 else matches[:limit]


def fts_query(text: str) -> str:
    tokens = []
    seen = set()
    for token in TOKEN_RE.findall(text):
        folded = token.casefold()
        if not folded or folded in seen:
            continue
        seen.add(folded)
        tokens.append('"%s"' % folded.replace('"', '""'))
    return " OR ".join(tokens)


ACCESS_BOOST_WINDOW_S = 7 * 86400
ACCESS_DORMANT_AGE_S = 45 * 86400


def parse_ts(text: object) -> Optional[float]:
    try:
        return datetime.datetime.strptime(
            str(text), "%Y-%m-%dT%H:%M:%SZ"
        ).replace(tzinfo=datetime.timezone.utc).timestamp()
    except (TypeError, ValueError):
        return None


def apply_access_decay(
    db: sqlite3.Connection,
    ranked: list[tuple[object, str, tuple[object, ...]]],
    limit: int,
) -> list[tuple[object, ...]]:
    """Re-rank match-ordered candidates by access history (mem0-style: the
    record never dies, its reach does). Recently recalled entries move up one
    quartile, never-recalled old entries move down one; the match order stays
    primary because bonuses are rank-ordinal, immune to bm25's sign. The
    entries handed back are touched in memory_access — best effort, a
    read-only database must not fail recall."""
    now = time.time()
    try:
        access = {
            row[0]: (row[1], row[2])
            for row in db.execute(
                "select event_id, last_accessed, hit_count from memory_access"
            )
        }
    except sqlite3.OperationalError:
        access = {}
    event_index = ENTRY_FIELDS.index("event_id")
    span = len(ranked)
    nudge = max(1.0, span / 4.0)
    adjusted = []
    for position, (_, occurred_at, entry) in enumerate(ranked):
        score = float(span - position)
        stats = access.get(entry[event_index])
        last_ts = parse_ts(stats[0]) if stats else None
        if last_ts is not None and now - last_ts <= ACCESS_BOOST_WINDOW_S:
            score += nudge
        occurred_ts = parse_ts(occurred_at)
        if (
            stats is None
            and occurred_ts is not None
            and now - occurred_ts >= ACCESS_DORMANT_AGE_S
        ):
            score -= nudge
        adjusted.append((score, occurred_at, position, entry))
    adjusted.sort(key=lambda item: (-item[0], str(item[1]), item[2]))
    chosen = [item[3] for item in adjusted[:limit]]
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    try:
        for entry in chosen:
            db.execute(
                """
                insert into memory_access (event_id, last_accessed, hit_count)
                values (?, ?, 1)
                on conflict (event_id) do update set
                  last_accessed = excluded.last_accessed,
                  hit_count = memory_access.hit_count + 1
                """,
                (entry[event_index], stamp),
            )
        db.commit()
    except sqlite3.OperationalError:
        pass
    return chosen


def ranked_recall(
    db: sqlite3.Connection, query: str, agent: str, limit: int
) -> list[tuple[object, ...]]:
    occurred_index = ENTRY_FIELDS.index("occurred_at")
    candidates: list[tuple[object, str, tuple[object, ...]]] = []
    match = fts_query(query)
    if match and has_fts(db):
        for row in db.execute(
            """
            select %s
            from memory_fts
            join memory_entries as entries on entries.id = memory_fts.rowid
            where memory_fts match ? and (? = '' or entries.agent = ?)
            order by bm25(memory_fts), entries.occurred_at desc, entries.id desc
            limit ?
            """
            % select_fields("entries"),
            (match, agent, agent, max(limit * 3, limit)),
        ):
            candidates.append((None, row[occurred_index], tuple(row)))
        return apply_access_decay(db, candidates, limit)

    tokens = [token.casefold() for token in TOKEN_RE.findall(query)]
    ranked = []
    rendered_index = ENTRY_FIELDS.index("rendered")
    for row in db.execute(
        """
        select entries.id, %s
        from memory_entries as entries
        where (? = '' or agent = ?)
        """
        % select_fields("entries"),
        (agent, agent),
    ):
        row_id = row[0]
        entry = row[1:]
        occurred_at = entry[occurred_index]
        rendered = str(entry[rendered_index])
        folded = rendered.casefold()
        score = sum(folded.count(token) for token in tokens)
        if score:
            ranked.append((score, occurred_at, row_id, entry))
    ranked.sort(reverse=True)
    candidates = [
        (row[0], row[1], row[3]) for row in ranked[: max(limit * 3, limit)]
    ]
    return apply_access_decay(db, candidates, limit)


def remove_database_files(path: str) -> None:
    for candidate in (path, path + "-journal", path + "-wal", path + "-shm"):
        try:
            os.unlink(candidate)
        except FileNotFoundError:
            pass


def rebuild_database(path: str, shared: str, pins: str, failures: str = "") -> None:
    """Build beside the old index and replace it only after a clean commit."""
    temporary = "%s.rebuild.%d" % (path, os.getpid())
    remove_database_files(temporary)
    db = connect(temporary)
    try:
        sync_sources(db, shared, pins, failures)
        if db.execute("pragma integrity_check").fetchone()[0] != "ok":
            raise RuntimeError("rebuilt memory database failed its integrity check")
    except Exception:
        db.close()
        remove_database_files(temporary)
        raise
    db.close()
    os.chmod(temporary, 0o600)
    os.replace(temporary, path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "command", choices=("sync", "search", "recall", "health", "rebuild")
    )
    parser.add_argument("--db", required=True)
    parser.add_argument("--shared", required=True)
    parser.add_argument("--pins", required=True)
    # Optional: a repo that has never failed anything has no ledger to index.
    parser.add_argument("--failures", default="")
    parser.add_argument("--query", default="")
    parser.add_argument("--agent", default="")
    parser.add_argument("--limit", type=int, default=1000)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    if args.limit < 0:
        parser.error("--limit must not be negative")
    if args.command == "recall" and args.limit == 0:
        parser.error("recall --limit must be positive")
    if args.command in ("search", "recall") and not args.query:
        parser.error("--query is required")

    if args.command == "rebuild":
        rebuild_database(args.db, args.shared, args.pins, args.failures)
        return 0
    if args.command == "health":
        report, result = memory_health(
            args.db, args.shared, args.pins, args.failures
        )
        emit_health(report, args.json)
        return result

    db = connect(args.db)
    result = 0
    try:
        sync_sources(db, args.shared, args.pins, args.failures)
        if args.command == "search":
            entries = exact_search(db, args.query, args.agent, args.limit)
            shown = emit(entries, args.json)
            sys.stderr.write(
                'memory: %d match(es) for "%s"\n' % (shown, args.query)
            )
            result = 0 if shown else 1
        elif args.command == "recall":
            entries = ranked_recall(db, args.query, args.agent, args.limit)
            shown = emit(entries, args.json)
            sys.stderr.write(
                'memory: %d recalled entr%s for "%s"\n'
                % (shown, "y" if shown == 1 else "ies", args.query)
            )
            result = 0 if shown else 1
    finally:
        db.close()
    try:
        os.chmod(args.db, 0o600)
    except OSError:
        pass
    return result


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, sqlite3.DatabaseError) as error:
        sys.stderr.write("error: memory database: %s\n" % error)
        if isinstance(error, sqlite3.DatabaseError):
            sys.stderr.write(
                "hint: run `oms agent-memory --repo . rebuild` from the project root\n"
            )
        raise SystemExit(2)
