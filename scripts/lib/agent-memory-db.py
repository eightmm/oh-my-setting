#!/usr/bin/env python3
"""SQLite index for append-only agent memory Markdown logs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Optional


SCHEMA_VERSION = 2
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


def ensure_schema(db: sqlite3.Connection) -> None:
    version = db.execute("pragma user_version").fetchone()[0]
    migrated = version == 1
    if version not in (0, 1, SCHEMA_VERSION):
        raise RuntimeError(
            "unsupported memory database schema %d (expected %d)"
            % (version, SCHEMA_VERSION)
        )
    if version == 1:
        migrate_schema_one(db)
    db.executescript(
        """
        create table if not exists memory_sources (
          source text primary key check (source in ('shared', 'pins')),
          path text not null,
          size integer not null,
          mtime_ns integer not null,
          digest text not null
        );
        create table if not exists memory_entries (
          id integer primary key,
          event_id text not null unique,
          source text not null check (source in ('shared', 'pins')),
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


def sync_sources(db: sqlite3.Connection, shared: str, pins: str) -> None:
    ensure_schema(db)
    db.execute("begin immediate")
    try:
        for source, path, parser in (
            ("shared", shared, parse_shared),
            ("pins", pins, parse_pins),
        ):
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
            payload = {"schema": SCHEMA_VERSION}
            payload.update(values)
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


def ranked_recall(
    db: sqlite3.Connection, query: str, agent: str, limit: int
) -> list[tuple[object, ...]]:
    match = fts_query(query)
    if match and has_fts(db):
        return db.execute(
            """
            select %s
            from memory_fts
            join memory_entries as entries on entries.id = memory_fts.rowid
            where memory_fts match ? and (? = '' or entries.agent = ?)
            order by bm25(memory_fts), entries.occurred_at desc, entries.id desc
            limit ?
            """
            % select_fields("entries"),
            (match, agent, agent, limit),
        ).fetchall()

    tokens = [token.casefold() for token in TOKEN_RE.findall(query)]
    ranked = []
    occurred_index = ENTRY_FIELDS.index("occurred_at")
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
    return [row[3] for row in ranked[:limit]]


def remove_database_files(path: str) -> None:
    for candidate in (path, path + "-journal", path + "-wal", path + "-shm"):
        try:
            os.unlink(candidate)
        except FileNotFoundError:
            pass


def rebuild_database(path: str, shared: str, pins: str) -> None:
    """Build beside the old index and replace it only after a clean commit."""
    temporary = "%s.rebuild.%d" % (path, os.getpid())
    remove_database_files(temporary)
    db = connect(temporary)
    try:
        sync_sources(db, shared, pins)
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
    parser.add_argument("command", choices=("sync", "search", "recall", "rebuild"))
    parser.add_argument("--db", required=True)
    parser.add_argument("--shared", required=True)
    parser.add_argument("--pins", required=True)
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
        rebuild_database(args.db, args.shared, args.pins)
        return 0

    db = connect(args.db)
    result = 0
    try:
        sync_sources(db, args.shared, args.pins)
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
